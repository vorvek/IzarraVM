// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn group11_mov_rm_imm_with_nonzero_reg_faults_without_consuming_the_immediate() {
    // C6 /1 is an undefined group-11 encoding (only reg=000 is MOV r/m,imm). This is the one
    // data-move path the goldens can't cover: decode DEFERS parsing the operand/immediate when
    // reg != 0 and the executor re-raises the error. Drive it through the split (which returns
    // the raw fault without eip rewind) and assert two things:
    //   1. the fault is a deliverable #UD (vector 6, no error code), and
    //   2. eip advanced to exactly 2 (opcode + ModRM) — proving decode did NOT over-consume the
    //      trailing imm8 (0x55) on the fault path, so the bytes charged match the fused handler.
    let (mut cpu, memory) = real_mode_cpu(&[0xc6, 0xc9, 0x55], 0x20);
    let mut bus = TestBus::with_memory(memory);

    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            fault,
            InternalFault::Exception {
                vector: 6,
                error_code: None
            }
        ),
        "{fault:?}"
    );
    assert_eq!(
        cpu.registers.eip, 2,
        "decode must stop after the ModRM on the fault path (imm8 not consumed)"
    );
}

#[test]
fn decode_then_execute_matches_golden_for_add_rm_reg() {
    // 01 D8 = ADD AX, BX (ALU form 1, op=0, modrm mode=3 rm=0 reg=3). The decode +
    // execute_decoded path must produce the architectural ADD result. (Once the ALU block was
    // fully converted to the split, the former fused executor was deleted, so this asserts the
    // known-correct end-state directly rather than diffing against a removed reference path.)
    let code = [0x01, 0xd8];

    let (mut split, mem) = real_mode_cpu(&code, 0x10);
    split.write_reg16(Reg16::Ax, 0x1234);
    split.write_reg16(Reg16::Bx, 0x1111);
    let mut split_bus = TestBus::with_memory(mem);
    split.begin_instruction();
    let insn = split.decode(&mut split_bus).unwrap();
    assert_eq!(insn.opcode, 0x01);
    assert_eq!(insn.operand, Some(DecodedOperand::Reg(0))); // r/m = AX
    let split_outcome = split.execute_decoded(&insn, &mut split_bus).unwrap();

    // 0x1234 + 0x1111 = 0x2345: no carry/zero/sign/overflow/aux, low byte 0x45 has odd parity
    // (PF clear), so only the always-set reserved bit 1 remains.
    assert_eq!(split.read_reg16(Reg16::Ax), 0x2345);
    assert_eq!(split.read_reg16(Reg16::Bx), 0x1111); // source untouched
    assert_eq!(split.eflags(), 0x02);
    assert_eq!(split.registers.eip, 0x02);
    assert_eq!(split_outcome.core_clocks, 2);
}

#[test]
fn decoded_add_rm_reg_recomputes_ea_from_live_registers() {
    // 01 07 = ADD [BX], AX (modrm mode=0 rm=7 -> [BX]). Decode once, then change BX before
    // executing: the addressing-mode descriptor must resolve against the *new* BX, proving
    // the decoded form stores a descriptor and not a baked-in offset.
    let code = [0x01, 0x07];
    let (mut cpu, mut mem) = real_mode_cpu(&code, 0x40);
    // Seed both candidate target words.
    mem[0x20..0x22].copy_from_slice(&0x0001u16.to_le_bytes());
    mem[0x30..0x32].copy_from_slice(&0x0001u16.to_le_bytes());
    let mut bus = TestBus::with_memory(mem);
    cpu.write_reg16(Reg16::Ax, 0x0010);
    cpu.write_reg16(Reg16::Bx, 0x0020);

    cpu.begin_instruction();
    let insn = cpu.decode(&mut bus).unwrap();
    // The descriptor must name BX (register 3) as its base, not a resolved offset.
    match insn.operand {
        Some(DecodedOperand::Mem(addr)) => {
            assert_eq!(addr.base, Some(3));
            assert_eq!(addr.index, None);
            assert_eq!(addr.disp, 0);
        }
        other => panic!("expected a memory operand, got {other:?}"),
    }

    // Move the pointer before executing.
    cpu.write_reg16(Reg16::Bx, 0x0030);
    cpu.execute_decoded(&insn, &mut bus).unwrap();

    assert_eq!(bus.memory[0x20], 0x01, "old target must be untouched");
    assert_eq!(bus.memory[0x30], 0x11, "new target (BX=0x30) gets AX added");
}

#[test]
fn alu_split_recomputes_effective_address() {
    // 00 07 = ADD [BX], AL (ALU form 0, op=0). Decode once, then execute against two different
    // BX values: each execution must resolve [BX] against the *current* BX and update the byte
    // there, proving the generalized ALU split recomputes the effective address every run.
    let code = [0x00, 0x07];
    let (mut cpu, mut mem) = real_mode_cpu(&code, 0x60);
    mem[0x40] = 0x01;
    mem[0x50] = 0x02;
    let mut bus = TestBus::with_memory(mem);
    cpu.write_reg16(Reg16::Ax, 0x0010); // AL = 0x10, AH = 0

    cpu.begin_instruction();
    let insn = cpu.decode(&mut bus).unwrap();

    // First run with BX = 0x40: the byte at [0x40] gains AL.
    cpu.write_reg16(Reg16::Bx, 0x0040);
    cpu.execute_decoded(&insn, &mut bus).unwrap();
    assert_eq!(bus.memory[0x40], 0x11, "[BX=0x40] must get AL added");
    assert_eq!(bus.memory[0x50], 0x02, "[0x50] untouched on the first run");

    // Re-execute the SAME decoded instruction with BX = 0x50: the EA must follow BX.
    cpu.write_reg16(Reg16::Bx, 0x0050);
    cpu.execute_decoded(&insn, &mut bus).unwrap();
    assert_eq!(bus.memory[0x40], 0x11, "[0x40] untouched on the second run");
    assert_eq!(bus.memory[0x50], 0x12, "[BX=0x50] must get AL added");
}

#[test]
fn self_modified_opcode_beyond_prefetch_window_is_seen() {
    let mut code = vec![0x90; 0x40]; // nop sled
    code[0..5].copy_from_slice(&[0xc6, 0x06, 0x21, 0x00, 0xf4]); // mov byte [0021h],hlt
    code[0x21] = 0x90; // replaced before execution reaches it
    code[0x22..0x24].copy_from_slice(&[0xeb, 0xfe]); // stale path would loop here
    let (mut cpu, memory) = real_mode_cpu(&code, 0x40);
    let mut bus = TestBus::with_memory(memory);

    let mut halted = false;
    for _ in 0..40 {
        halted = cpu.cycle(&mut bus).unwrap().halted;
        if halted {
            break;
        }
    }

    assert!(halted, "modified HLT at 0021h must execute");
    assert_eq!(cpu.registers.eip, 0x22);
}

#[test]
fn int3_traps_to_vector_3() {
    // 0xCC. IVT[3] (linear 12) -> CS:IP = 0000:0100.
    let (mut cpu, mut memory) = real_mode_cpu(&[0xcc], 0x200);
    memory[12..14].copy_from_slice(&0x0100u16.to_le_bytes());
    memory[14..16].copy_from_slice(&0x0000u16.to_le_bytes());
    cpu.write_reg16(Reg16::Sp, 0x0200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x0100);
    assert_eq!(cpu.registers.cs().selector, 0);
    // flags, CS, return-IP(=1) were pushed: SP fell by 6, return IP word is 1.
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01fa);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x1fa], bus.memory[0x1fb]]),
        1
    );
}

#[test]
fn into_traps_only_when_overflow_set() {
    // 0xCE with OF=1 traps to vector 4 (IVT[4] linear 16 -> 0000:0200).
    let (mut cpu, mut memory) = real_mode_cpu(&[0xce], 0x300);
    memory[16..18].copy_from_slice(&0x0200u16.to_le_bytes());
    cpu.write_reg16(Reg16::Sp, 0x0280);
    cpu.set_flag(FLAG_OF, true);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eip, 0x0200, "OF set: INTO must trap");

    // OF=0: INTO is a no-op, just advances past the one byte.
    let (mut cpu, memory) = real_mode_cpu(&[0xce], 0x40);
    cpu.set_flag(FLAG_OF, false);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eip, 1, "OF clear: INTO must fall through");
}

#[test]
fn word_in_out_use_word_width() {
    // IN AX, DX (0xED): word port read lands in AX (TestBus returns 0).
    let (mut cpu, memory) = real_mode_cpu(&[0xed], 0x10);
    cpu.write_reg16(Reg16::Ax, 0xffff);
    cpu.write_reg16(Reg16::Dx, 0x03f8);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0000);
    assert!(
        bus.trace
            .cycles()
            .iter()
            .any(|c| c.kind == BusAccessKind::IoRead
                && c.width == BusWidth::Word
                && c.address == 0x03f8)
    );

    // OUT DX, AX (0xEF): word port write at DX.
    let (mut cpu, memory) = real_mode_cpu(&[0xef], 0x10);
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Dx, 0x03f8);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(
        bus.trace
            .cycles()
            .iter()
            .any(|c| c.kind == BusAccessKind::IoWrite
                && c.width == BusWidth::Word
                && c.address == 0x03f8)
    );
}

#[test]
fn push_imm8_sign_extends_to_word() {
    // 0x6A 0x80 -> push 0xFF80 onto a 16-bit stack.
    let (mut cpu, memory) = real_mode_cpu(&[0x6a, 0x80], 0x120);
    cpu.write_reg16(Reg16::Sp, 0x0100);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x00fe);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
        0xff80
    );
}

#[test]
fn imul_imm8_sign_extended() {
    // IMUL AX, AX, -1  (0x6B 0xC0 0xFF): 2 * -1 = -2, fits, CF/OF clear.
    let (mut cpu, memory) = real_mode_cpu(&[0x6b, 0xc0, 0xff], 0x10);
    cpu.write_reg16(Reg16::Ax, 0x0002);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xfffe);
    assert!(!cpu.flag(FLAG_CF) && !cpu.flag(FLAG_OF));
}

#[test]
fn imul_imm16_overflow_sets_carry_and_overflow() {
    // IMUL AX, AX, 0x0004 (0x69 0xC0 0x04 0x00) with AX=0x4000 -> 0x10000, truncates.
    let (mut cpu, memory) = real_mode_cpu(&[0x69, 0xc0, 0x04, 0x00], 0x10);
    cpu.write_reg16(Reg16::Ax, 0x4000);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0000);
    assert!(cpu.flag(FLAG_CF) && cpu.flag(FLAG_OF));
}

#[test]
fn enter_level_zero_builds_frame() {
    // ENTER 4, 0 (0xC8 0x04 0x00 0x00): push BP, BP=SP, SP-=4.
    let (mut cpu, memory) = real_mode_cpu(&[0xc8, 0x04, 0x00, 0x00], 0x120);
    cpu.write_reg16(Reg16::Bp, 0xbbbb);
    cpu.write_reg16(Reg16::Sp, 0x0100);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.read_reg16(Reg16::Bp),
        0x00fe,
        "BP = frame after PUSH BP"
    );
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x00fa, "SP -= alloc");
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
        0xbbbb
    );
}

#[test]
fn salc_sets_al_from_carry() {
    // 0xD6 with CF=1 -> AL=0xFF (AH preserved).
    let (mut cpu, memory) = real_mode_cpu(&[0xd6], 0x10);
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x12ff);

    // CF=0 -> AL=0x00.
    let (mut cpu, memory) = real_mode_cpu(&[0xd6], 0x10);
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1200);
}

#[test]
fn opcode_82_aliases_80_add() {
    // ADD AL, 5 encoded with the undocumented 0x82 group-1 opcode.
    let (mut cpu, memory) = real_mode_cpu(&[0x82, 0xc0, 0x05], 0x10);
    cpu.write_reg16(Reg16::Ax, 0x0010);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x15);
}

#[test]
fn wait_is_a_nop_without_a_pending_x87_exception() {
    let (mut cpu, memory) = real_mode_cpu(&[0x9b], 0x10);
    let flags_before = cpu.registers.eflags;
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eip, 1);
    assert_eq!(cpu.registers.eflags, flags_before);
}

// ---- x87 FPU foundation ----

#[test]
fn fninit_then_fld1_pushes_one() {
    // FNINIT (DB E3) then FLD1 (D9 E8).
    let (mut cpu, memory) = real_mode_cpu(&[0xdb, 0xe3, 0xd9, 0xe8], 0x20);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 1.0);
    assert_eq!(cpu.fpu.top(), 7);
}

#[test]
fn fld_fadd_fstp_round_trips_m64() {
    // FLD m64 [0x100]; FADD m64 [0x108]; FSTP m64 [0x110]. 2.5 + 1.25 = 3.75.
    let code = [
        0xdd, 0x06, 0x00, 0x01, 0xdc, 0x06, 0x08, 0x01, 0xdd, 0x1e, 0x10, 0x01,
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x108].copy_from_slice(&2.5f64.to_le_bytes());
    memory[0x108..0x110].copy_from_slice(&1.25f64.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    let stored = f64::from_le_bytes(bus.memory[0x110..0x118].try_into().unwrap());
    assert_eq!(stored, 3.75);
}

#[test]
fn fxch_swaps_st0_and_st1() {
    // FLD1 (D9 E8); FLDZ (D9 EE); FXCH ST(1) (D9 C9). ST0 ends as 1.0.
    let (mut cpu, memory) = real_mode_cpu(&[0xd9, 0xe8, 0xd9, 0xee, 0xd9, 0xc9], 0x20);
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..3 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.fpu.get(0), 1.0);
    assert_eq!(cpu.fpu.get(1), 0.0);
}

#[test]
fn fnstsw_ax_reports_top_in_status() {
    // FLD1 (D9 E8) then FNSTSW AX (DF E0): TOP=7 lands in AX bits 11-13.
    let (mut cpu, memory) = real_mode_cpu(&[0xd9, 0xe8, 0xdf, 0xe0], 0x20);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    assert_eq!((cpu.read_reg16(Reg16::Ax) >> 11) & 0x7, 7);
}

#[test]
fn fild_fmulp_fistp_integer_path() {
    // FILD m32 [0x100]=5; FILD m32 [0x104]=3; FMULP ST1,ST0 (DE C9); FISTP m32 [0x108].
    let code = [
        0xdb, 0x06, 0x00, 0x01, 0xdb, 0x06, 0x04, 0x01, 0xde, 0xc9, 0xdb, 0x1e, 0x08, 0x01,
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x104].copy_from_slice(&5i32.to_le_bytes());
    memory[0x104..0x108].copy_from_slice(&3i32.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..4 {
        cpu.cycle(&mut bus).unwrap();
    }
    let stored = i32::from_le_bytes(bus.memory[0x108..0x10c].try_into().unwrap());
    assert_eq!(stored, 15);
}

#[test]
fn fsub_reverse_forms_differ() {
    // D8 /5 FSUBR ST0,ST(i): ST0 = ST(i) - ST0. Start ST0=2, ST1=10 -> 8.
    // FLD m64 [0x100]=10; FLD m64 [0x108]=2; FSUBR ST0,ST1 (D8 E9).
    let code = [0xdd, 0x06, 0x00, 0x01, 0xdd, 0x06, 0x08, 0x01, 0xd8, 0xe9];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x108].copy_from_slice(&10.0f64.to_le_bytes());
    memory[0x108..0x110].copy_from_slice(&2.0f64.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..3 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.fpu.get(0), 8.0);
}

// ---- x87 transcendentals ----

#[test]
fn f2xm1_of_one_is_one() {
    // FLD1 (D9 E8); F2XM1 (D9 F0): 2^1 - 1 = 1.
    let (mut cpu, memory) = real_mode_cpu(&[0xd9, 0xe8, 0xd9, 0xf0], 0x20);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 1.0);
}

#[test]
fn fyl2x_computes_y_times_log2_x() {
    // FLD1 (ST1=1); FLD m64 [0x100]=2 (ST0=2); FYL2X (D9 F1): 1 * log2(2) = 1.
    let code = [0xd9, 0xe8, 0xdd, 0x06, 0x00, 0x01, 0xd9, 0xf1];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x108].copy_from_slice(&2.0f64.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..3 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.fpu.get(0), 1.0);
}

#[test]
fn fscale_scales_by_power_of_two() {
    // FLD m64 [0x100]=2 (ST1); FLD m64 [0x108]=3 (ST0); FSCALE (D9 FD): 3 * 2^2 = 12.
    let code = [0xdd, 0x06, 0x00, 0x01, 0xdd, 0x06, 0x08, 0x01, 0xd9, 0xfd];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x108].copy_from_slice(&2.0f64.to_le_bytes());
    memory[0x108..0x110].copy_from_slice(&3.0f64.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..3 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.fpu.get(0), 12.0);
}

#[test]
fn fptan_replaces_st0_and_pushes_one() {
    // FLDZ (D9 EE); FPTAN (D9 F2): tan(0)=0 in ST1, 1.0 pushed into ST0.
    let (mut cpu, memory) = real_mode_cpu(&[0xd9, 0xee, 0xd9, 0xf2], 0x20);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 1.0);
    assert_eq!(cpu.fpu.get(1), 0.0);
}

// ---- Integer-operand arithmetic and 80-bit extended precision ----

#[test]
fn fidiv_divides_by_an_integer_operand() {
    // FILD m32 [0x100]=20; FIDIV m32 [0x104]=4 (DA /6). 20 / 4 = 5.
    let code = [0xdb, 0x06, 0x00, 0x01, 0xda, 0x36, 0x04, 0x01];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x104].copy_from_slice(&20i32.to_le_bytes());
    memory[0x104..0x108].copy_from_slice(&4i32.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 5.0);
}

#[test]
fn extended80_round_trips_through_memory() {
    // FLD m64 [0x100]=3.5; FSTP m80 [0x108] (DB /7); FLD m80 [0x108] (DB /5).
    let code = [
        0xdd, 0x06, 0x00, 0x01, 0xdb, 0x3e, 0x08, 0x01, 0xdb, 0x2e, 0x08, 0x01,
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x108].copy_from_slice(&3.5f64.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..3 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.fpu.get(0), 3.5);
}

// ---- BCD, environment, state save/restore, and FUCOMPP ----

#[test]
fn fbld_fbstp_round_trips_packed_bcd() {
    // FILD m32 [0x100]=12345; FBSTP m80 [0x108] (DF /6); FBLD m80 [0x108] (DF /4).
    let code = [
        0xdb, 0x06, 0x00, 0x01, 0xdf, 0x36, 0x08, 0x01, 0xdf, 0x26, 0x08, 0x01,
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x104].copy_from_slice(&12345i32.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..3 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.fpu.get(0), 12345.0);
}

#[test]
fn fnsave_frstor_round_trips_registers() {
    // FLD1; FLD m64 [0x180]=2.5; FNSAVE [0x100] (DD /6); FRSTOR [0x100] (DD /4).
    let code = [
        0xd9, 0xe8, 0xdd, 0x06, 0x80, 0x01, 0xdd, 0x36, 0x00, 0x01, 0xdd, 0x26, 0x00, 0x01,
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x180..0x188].copy_from_slice(&2.5f64.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..4 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.fpu.get(0), 2.5);
    assert_eq!(cpu.fpu.get(1), 1.0);
}

#[test]
fn fnstenv_fldenv_round_trips_top() {
    // FLD1 (TOP=7); FNSTENV [0x100] (D9 /6); FNINIT (TOP=0); FLDENV [0x100] (D9 /4).
    let code = [
        0xd9, 0xe8, 0xd9, 0x36, 0x00, 0x01, 0xdb, 0xe3, 0xd9, 0x26, 0x00, 0x01,
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 0x200);
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..4 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.fpu.top(), 7);
}

#[test]
fn fucompp_sets_equal_condition() {
    // FLD1; FLD1; FUCOMPP (DA E9): equal -> C3 set, both popped.
    let (mut cpu, memory) = real_mode_cpu(&[0xd9, 0xe8, 0xd9, 0xe8, 0xda, 0xe9], 0x20);
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..3 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!((cpu.fpu.status >> 14) & 1, 1, "C3 set on equal");
    assert_eq!(cpu.fpu.top(), 0, "both operands popped");
}

// ---- Protected-mode system instructions ----

#[test]
fn smsw_stores_machine_status_word() {
    // SMSW eax (0F 01 E0).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x01, 0xe0], 0x20);
    cpu.control.cr0 = CR0_TS | CR0_MP; // 0x0A
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax() & 0xffff, 0x000a);
}

#[test]
fn lmsw_sets_protection_enable() {
    // LMSW ax (0F 01 F0) with AX bit 0 set turns on CR0.PE.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x01, 0xf0], 0x20);
    cpu.write_reg16(Reg16::Ax, 0x0001);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_ne!(cpu.control.cr0 & CR0_PE, 0);
}

#[test]
fn clts_clears_task_switched() {
    // CLTS (0F 06).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x06], 0x20);
    cpu.control.cr0 |= CR0_TS;
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.cr0 & CR0_TS, 0);
}

#[test]
fn sgdt_stores_the_gdtr() {
    // SGDT [0x100] (0F 01 06 00 01): limit word then base dword.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x01, 0x06, 0x00, 0x01], 0x200);
    cpu.gdtr = DescriptorTable {
        base: 0x1234_5678,
        limit: 0x0abc,
    };
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x100], bus.memory[0x101]]),
        0x0abc
    );
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x102..0x106].try_into().unwrap()),
        0x1234_5678
    );
}

#[test]
fn sldt_stores_the_ldtr_selector() {
    // SLDT ax (0F 00 C0), protected mode only.
    let (mut cpu, memory) = protected_cpu(&[0x0f, 0x00, 0xc0], 0, 0);
    cpu.ldtr.selector = 0x0028;
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0028);
}

#[test]
fn sldt_is_invalid_in_real_mode() {
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x00, 0xc0], 0x20);
    let mut bus = TestBus::with_memory(memory);
    // SLDT (0F 00 /0) is converted to the decode/execute split (task A12); the whole 0F 00
    // group is #UD outside protected mode, raised in `execute_system_seg_decoded`.
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn arpl_register_form_raises_only_a_less_restrictive_rpl() {
    let (mut cpu, memory) = protected_cpu(&[0x63, 0xc8], 0, 0); // ARPL AX,CX
    cpu.write_reg16(Reg16::Ax, 0x0029);
    cpu.write_reg16(Reg16::Cx, 0x0003);
    cpu.registers.eflags = FLAG_CF | FLAG_SF | FLAG_OF | 0x2;
    let mut bus = TestBus::with_memory(memory);

    exec_one_split(&mut cpu, &mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x002b);
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0003);
    assert_eq!(
        cpu.eflags() & (FLAG_CF | FLAG_ZF | FLAG_SF | FLAG_OF),
        FLAG_CF | FLAG_ZF | FLAG_SF | FLAG_OF
    );

    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x002b);
    cpu.write_reg16(Reg16::Cx, 0x0001);
    exec_one_split(&mut cpu, &mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x002b);
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn arpl_memory_form_updates_the_selector_word() {
    let (mut cpu, mut memory) = protected_cpu(&[0x63, 0x0f], 0, 0); // ARPL [BX],CX
    cpu.write_reg16(Reg16::Bx, 0x0040);
    cpu.write_reg16(Reg16::Cx, 0x0002);
    memory[0x40..0x42].copy_from_slice(&0x0030_u16.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);

    exec_one_split(&mut cpu, &mut bus).unwrap();

    assert_eq!(
        u16::from_le_bytes(bus.memory[0x40..0x42].try_into().unwrap()),
        0x0032
    );
    assert!(cpu.flag(FLAG_ZF));
}

#[test]
fn arpl_is_undefined_in_real_and_v86_modes() {
    let (mut real, memory) = real_mode_cpu(&[0x63, 0xc0], 0x40);
    let mut real_bus = TestBus::with_memory(memory);
    assert!(matches!(
        exec_one_split(&mut real, &mut real_bus),
        Err(InternalFault::Exception { vector: 6, .. })
    ));

    let (mut v86, memory) = protected_cpu(&[0x63, 0xc0], 0, 0);
    v86.registers.eflags |= FLAG_VM;
    v86.cpl = 3;
    let mut v86_bus = TestBus::with_memory(memory);
    assert!(matches!(
        exec_one_split(&mut v86, &mut v86_bus),
        Err(InternalFault::Exception { vector: 6, .. })
    ));
}

#[test]
fn arpl_uses_the_386_and_486_class_cycle_counts() {
    for (mode, expected) in [
        (GswMode::Gsw386Slow, 20),
        (GswMode::Gsw386, 20),
        (GswMode::Gsw486, 9),
        (GswMode::Gsw586, 7),
    ] {
        let (mut cpu, memory) = protected_cpu(&[0x63, 0xc8], 0, 0);
        cpu.set_mode(mode);
        let mut bus = TestBus::with_memory(memory);
        assert_eq!(
            exec_one_split(&mut cpu, &mut bus).unwrap().core_clocks,
            expected,
            "{mode:?}"
        );
    }
}

// ---- PUSH/POP FS/GS (0F A0/A1/A8/A9) ----

/// Real-mode CPU with SP parked at 0x1f0 (mirrors `stack_seed`) and 0x200 bytes of memory,
/// so PUSH/POP FS/GS have room on the stack. Used for the real-mode + 16/32-bit-operand-size
/// arms of the new opcodes; the protected-mode descriptor-load arms use `protected_cpu` below.
fn fs_gs_stack_cpu(code: &[u8]) -> (CpuGsw, Vec<u8>) {
    let mut memory = vec![0u8; 0x200];
    memory[..code.len()].copy_from_slice(code);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x01f0);
    (cpu, memory)
}

#[test]
fn push_fs_pushes_the_selector_in_real_mode() {
    // PUSH FS (0F A0). Mirrors PUSH DS (0x1e): pushes the 16-bit selector, SP -= 2.
    let (mut cpu, memory) = fs_gs_stack_cpu(&[0x0f, 0xa0]);
    cpu.registers
        .set_segment(SegmentIndex::Fs, SegmentRegister::real(0x1234));
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01ee, "SP must decrement by 2");
    assert_eq!(
        u16::from_le_bytes(bus.memory[0x1ee..0x1f0].try_into().unwrap()),
        0x1234,
        "FS selector must land on the stack"
    );
}

#[test]
fn push_gs_pushes_the_selector_in_real_mode() {
    // PUSH GS (0F A8). Mirrors PUSH DS (0x1e).
    let (mut cpu, memory) = fs_gs_stack_cpu(&[0x0f, 0xa8]);
    cpu.registers
        .set_segment(SegmentIndex::Gs, SegmentRegister::real(0x5678));
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01ee, "SP must decrement by 2");
    assert_eq!(
        u16::from_le_bytes(bus.memory[0x1ee..0x1f0].try_into().unwrap()),
        0x5678,
        "GS selector must land on the stack"
    );
}

#[test]
fn push_fs_gs_zero_extend_under_the_32_bit_operand_size_prefix() {
    // 66 0F A0 / 66 0F A8: 386 PRM -- PUSH sreg with a 32-bit operand size decrements
    // ESP by 4 and writes the 16-bit selector zero-extended to a dword (the SDM PUSH
    // operation note). Same rule as the one-byte PUSH ES/CS/SS/DS arms.
    for (code, segment, value) in [
        ([0x66u8, 0x0f, 0xa0].as_slice(), SegmentIndex::Fs, 0x1234u16),
        ([0x66, 0x0f, 0xa8].as_slice(), SegmentIndex::Gs, 0x5678u16),
    ] {
        let (mut cpu, memory) = fs_gs_stack_cpu(code);
        cpu.registers
            .set_segment(segment, SegmentRegister::real(value));
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.read_reg16(Reg16::Sp),
            0x01ec,
            "SP must move by 4 with a 32-bit operand-size prefix"
        );
        assert_eq!(
            u32::from_le_bytes(bus.memory[0x01ec..0x01f0].try_into().unwrap()),
            u32::from(value),
            "the pushed dword must be the 16-bit selector zero-extended"
        );
    }
}

#[test]
fn pop_fs_gs_discard_the_upper_word_under_the_32_bit_operand_size_prefix() {
    // 66 0F A1 / 66 0F A9: 386 PRM -- POP sreg with a 32-bit operand size pops a full
    // dword, loads the low 16 bits into the segment register, and discards the upper 16.
    // Same rule as the one-byte POP ES/SS/DS arms.
    for (code, segment) in [
        ([0x66u8, 0x0f, 0xa1].as_slice(), SegmentIndex::Fs),
        ([0x66, 0x0f, 0xa9].as_slice(), SegmentIndex::Gs),
    ] {
        let (mut cpu, memory) = fs_gs_stack_cpu(code);
        let mut bus = TestBus::with_memory(memory);
        bus.memory[0x1f0..0x1f4].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01f4, "SP must advance by 4");
        assert_eq!(
            cpu.registers.segment(segment).selector,
            0xbeef,
            "{segment:?} must load only the low 16 bits, discarding 0xdead"
        );
    }
}

#[test]
fn push_pop_one_byte_sreg_zero_extend_under_the_32_bit_operand_size_prefix() {
    // 66 06 / 66 0E / 66 16 / 66 1E (PUSH ES/CS/SS/DS) and 66 07 / 66 1F (POP ES/DS):
    // the one-byte segment-register push/pop opcodes follow the identical 386 PRM
    // operand-size rule as PUSH/POP FS/GS above. POP SS (66 17) is covered separately
    // below because it arms the MOV-SS interrupt shadow.
    for (push_code, pop_code, segment, value) in [
        (
            [0x66u8, 0x06].as_slice(),
            [0x66u8, 0x07].as_slice(),
            SegmentIndex::Es,
            0x1111u16,
        ),
        (
            [0x66, 0x1e].as_slice(),
            [0x66, 0x1f].as_slice(),
            SegmentIndex::Ds,
            0x2222,
        ),
    ] {
        // PUSH: selector zero-extended to a dword, ESP -= 4.
        let (mut cpu, memory) = fs_gs_stack_cpu(push_code);
        cpu.registers
            .set_segment(segment, SegmentRegister::real(value));
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.read_reg16(Reg16::Sp),
            0x01ec,
            "{push_code:02x?}: SP must move by 4 with a 32-bit operand-size prefix"
        );
        assert_eq!(
            u32::from_le_bytes(bus.memory[0x01ec..0x01f0].try_into().unwrap()),
            u32::from(value),
            "{push_code:02x?}: the pushed dword must be the selector zero-extended"
        );

        // POP: full dword popped, only the low 16 bits loaded.
        let (mut cpu, memory) = fs_gs_stack_cpu(pop_code);
        let mut bus = TestBus::with_memory(memory);
        bus.memory[0x1f0..0x1f4].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.read_reg16(Reg16::Sp),
            0x01f4,
            "{pop_code:02x?}: SP must advance by 4"
        );
        assert_eq!(
            cpu.registers.segment(segment).selector,
            0xbeef,
            "{pop_code:02x?}: {segment:?} must load only the low 16 bits"
        );
    }
}

#[test]
fn push_pop_ss_zero_extends_under_the_32_bit_operand_size_prefix() {
    // 66 16 (PUSH SS) / 66 17 (POP SS): same 386 PRM operand-size rule, but POP SS also
    // arms the one-instruction interrupt shadow (`load_segment_arming_ss_shadow`), so it
    // gets its own test rather than folding into the ES/DS table above. Unlike PUSH
    // FS/ES/DS, PUSH SS cannot push an arbitrary probe value into SS without also
    // relocating the stack it is about to push onto, so this asserts against
    // `fs_gs_stack_cpu`'s real-mode SS selector (0) instead.
    let (mut cpu, memory) = fs_gs_stack_cpu(&[0x66, 0x16]);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.read_reg16(Reg16::Sp),
        0x01ec,
        "PUSH SS must move SP by 4"
    );
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x01ec..0x01f0].try_into().unwrap()),
        0x0000,
        "PUSH SS must zero-extend the selector to a dword"
    );

    let (mut cpu, memory) = fs_gs_stack_cpu(&[0x66, 0x17]);
    let mut bus = TestBus::with_memory(memory);
    bus.memory[0x1f0..0x1f4].copy_from_slice(&0xdead_beefu32.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.read_reg16(Reg16::Sp),
        0x01f4,
        "POP SS must advance SP by 4"
    );
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ss).selector,
        0xbeef,
        "POP SS must load only the low 16 bits"
    );
}

#[test]
fn push_pop_one_byte_sreg_unchanged_at_16_bit_operand_size() {
    // Without a 66h prefix, PUSH/POP ES/CS/SS/DS (and FS/GS) stay the classic 2-byte
    // real-mode DOS behavior -- this is the frozen-class-sensitivity check: no bench or
    // real-mode DOS code observes a behavior change from the operand_size fix.
    for code in [
        [0x06u8, 0x90], // PUSH ES; NOP pad
        [0x0e, 0x90],   // PUSH CS; NOP pad
        [0x16, 0x90],   // PUSH SS; NOP pad
        [0x1e, 0x90],   // PUSH DS; NOP pad
    ] {
        let (mut cpu, memory) = fs_gs_stack_cpu(&code);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.read_reg16(Reg16::Sp),
            0x01ee,
            "{code:02x?}: 16-bit-operand-size PUSH sreg must still only move SP by 2"
        );
    }
    for code in [
        [0x07u8, 0x90], // POP ES; NOP pad
        [0x1f, 0x90],   // POP DS; NOP pad
    ] {
        let (mut cpu, memory) = fs_gs_stack_cpu(&code);
        let mut bus = TestBus::with_memory(memory);
        bus.memory[0x1f0..0x1f2].copy_from_slice(&0xbeefu16.to_le_bytes());
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.read_reg16(Reg16::Sp),
            0x01f2,
            "{code:02x?}: 16-bit-operand-size POP sreg must still only move SP by 2"
        );
    }
}

#[test]
fn pop_fs_loads_the_selector_in_real_mode() {
    // POP FS (0F A1). Mirrors POP DS (0x1f): pops a 16-bit selector and loads it, SP += 2.
    let (mut cpu, memory) = fs_gs_stack_cpu(&[0x0f, 0xa1]);
    let mut bus = TestBus::with_memory(memory);
    bus.memory[0x1f0..0x1f2].copy_from_slice(&0xbeefu16.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01f2, "SP must increment by 2");
    assert_eq!(cpu.registers.segment(SegmentIndex::Fs).selector, 0xbeef);
}

#[test]
fn pop_gs_loads_the_selector_in_real_mode() {
    // POP GS (0F A9). Mirrors POP DS (0x1f).
    let (mut cpu, memory) = fs_gs_stack_cpu(&[0x0f, 0xa9]);
    let mut bus = TestBus::with_memory(memory);
    bus.memory[0x1f0..0x1f2].copy_from_slice(&0xbeefu16.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01f2, "SP must increment by 2");
    assert_eq!(cpu.registers.segment(SegmentIndex::Gs).selector, 0xbeef);
}

#[test]
fn pop_fs_gs_load_a_valid_descriptor_in_protected_mode() {
    // Data segment access 0x92 (present, data, writable), byte-granular limit 0xffff,
    // base 0 -- the same descriptor shape `verr_sets_zf_for_a_readable_segment` and
    // `lar_and_lsl_read_descriptor_fields` use. Selector 0x0008 (GDT index 1, RPL 0).
    for (code, segment) in [
        ([0x0fu8, 0xa1].as_slice(), SegmentIndex::Fs), // POP FS
        ([0x0f, 0xa9].as_slice(), SegmentIndex::Gs),   // POP GS
    ] {
        let (mut cpu, memory) = protected_cpu(code, 0x0000_ffff, 0x0000_9200);
        let mut bus = TestBus::with_memory(memory);
        cpu.write_reg16(Reg16::Sp, 0x01f0);
        bus.memory[0x1f0..0x1f2].copy_from_slice(&0x0008u16.to_le_bytes());
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.registers.segment(segment).selector,
            0x0008,
            "{segment:?} selector must load"
        );
        assert_eq!(
            cpu.registers.segment(segment).base,
            0,
            "{segment:?} base must come from the descriptor"
        );
        assert_eq!(
            cpu.registers.segment(segment).limit,
            0xffff,
            "{segment:?} limit must come from the descriptor"
        );
        assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01f2, "SP must advance by 2");
    }
}

#[test]
fn pop_fs_gs_fault_on_a_bad_selector_in_protected_mode() {
    // Selector 0x0028 (index 5, byte offset 40) is past the GDT limit of 0x1f (31), which
    // only covers offsets 0 (null) and 8 (the one installed descriptor), so the descriptor
    // load must #GP -- the same fault a bad POP DS selector raises.
    for (code, name) in [
        ([0x0fu8, 0xa1].as_slice(), "POP FS"),
        ([0x0f, 0xa9].as_slice(), "POP GS"),
    ] {
        let (mut cpu, memory) = protected_cpu(code, 0x0000_ffff, 0x0000_9200);
        let mut bus = TestBus::with_memory(memory);
        cpu.write_reg16(Reg16::Sp, 0x01f0);
        bus.memory[0x1f0..0x1f2].copy_from_slice(&0x0028u16.to_le_bytes());
        let err = exec_one_split(&mut cpu, &mut bus).unwrap_err();
        assert!(
            matches!(
                err,
                InternalFault::Exception {
                    vector: 13,
                    error_code: Some(40)
                }
            ),
            "{name} with an out-of-limit selector must #GP(0x28), got {err:?}"
        );
    }
}

#[test]
fn lldt_loads_the_descriptor() {
    // LDT descriptor at selector 0x08: base 0x0004_0000, limit 0x0fff, access 0x82.
    let low = 0x0000_0fff; // limit low, base low 16 = 0
    let high = 0x0000_8204; // base[23:16]=0x04, access=0x82
    let (mut cpu, memory) = protected_cpu(&[0x0f, 0x00, 0xd0], low, high);
    cpu.write_reg16(Reg16::Ax, 0x0008);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.ldtr.selector, 0x0008);
    assert_eq!(cpu.ldtr.base, 0x0004_0000);
    assert_eq!(cpu.ldtr.limit, 0x0fff);
}

#[test]
fn null_selector_loads_into_data_segments_without_fault() {
    // MOV DS/ES/FS/GS, AX with AX = 0 (a null selector: index 0, TI 0). The 386 lets a
    // null selector load into a data segment with no fault; only a later memory access
    // through it #GPs. Descriptor bytes are irrelevant here (never read for a null load).
    for (opcode_reg, segment) in [
        (0xc0u8, SegmentIndex::Es), // MOV ES, AX (8E C0)
        (0xd8, SegmentIndex::Ds),   // MOV DS, AX (8E D8)
        (0xe0, SegmentIndex::Fs),   // MOV FS, AX (8E E0)
        (0xe8, SegmentIndex::Gs),   // MOV GS, AX (8E E8)
    ] {
        let (mut cpu, memory) = protected_cpu(&[0x8e, opcode_reg], 0x0000_ffff, 0x0000_9200);
        cpu.write_reg16(Reg16::Ax, 0x0000);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.registers.segment(segment).selector,
            0x0000,
            "{segment:?} must load the null selector"
        );
        assert_eq!(
            cpu.registers.segment(segment).access & 0x80,
            0,
            "{segment:?} must install a not-present/unusable segment"
        );
    }
}

#[test]
fn access_through_a_null_data_segment_faults() {
    // MOV DS, AX (8E D8) with AX = 0 loads DS as null (no fault); a following memory
    // access through DS (MOV AL, [SI], opcode 8A 04) must then #GP -- the null segment's
    // base=0/limit=0 default fails the segment-limit check for any nonzero offset.
    let (mut cpu, memory) = protected_cpu(&[0x8e, 0xd8, 0x8a, 0x04], 0x0000_ffff, 0x0000_9200);
    cpu.write_reg16(Reg16::Ax, 0x0000);
    cpu.write_reg16(Reg16::Si, 0x0010);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap(); // MOV DS, AX: loads null, no fault.
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            fault,
            InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            }
        ),
        "access through a null DS must fault, got {fault:?}"
    );
}

#[test]
fn null_selector_into_ss_still_faults() {
    // MOV SS, AX (8E D0) with AX = 0. Unlike the data segments, a null selector loaded
    // into SS must still #GP -- the stack segment can never be null.
    let (mut cpu, memory) = protected_cpu(&[0x8e, 0xd0], 0x0000_ffff, 0x0000_9200);
    cpu.write_reg16(Reg16::Ax, 0x0000);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            fault,
            InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            }
        ),
        "a null selector into SS must #GP, got {fault:?}"
    );
}

#[test]
fn ldt_selector_resolves_against_the_ldt_not_the_gdt() {
    // Install an LDT (via LLDT) whose own descriptor lives at GDT selector 0x08, then load
    // DS from an LDT selector (TI=1, index 1: selector 0x000c) whose descriptor lives at
    // LDT offset 8. The GDT selector 0x08 descriptor is deliberately a system (LDT)
    // descriptor, not a data segment: if a test regression accidentally indexed the GDT
    // instead of the LDT for the DS load, it would read this LDT-type descriptor and the
    // base/limit assertions below would fail.
    let mut memory = vec![0u8; 0x400];
    // GDT at 0x100 (base/limit set by protected_cpu below): selector 0x08 is the LDT
    // system descriptor (base 0x0000_0200, limit 0x0f, access 0x82 = present, LDT type).
    let ldt_desc_low = 0x0200_000f; // limit low = 0x0f, base[15:0] = 0x0200
    let ldt_desc_high = 0x0000_8200; // base[31:24]=0, base[23:16]=0, access = 0x82 (present LDT)
    let (mut cpu, mut code) =
        protected_cpu(&[0x0f, 0x00, 0xd0, 0x8e, 0xd9], ldt_desc_low, ldt_desc_high);
    code.resize(0x400, 0);
    // LDT lives at 0x200 (matches the descriptor base above). LDT selector 0x000c is
    // index 1 (byte offset 8) inside the LDT: a data segment, base 0x0005_0000, limit
    // 0x00ff, access 0x92 (present, data, writable).
    let ldt_base = 0x200usize;
    code[ldt_base + 8..ldt_base + 12].copy_from_slice(&0x0000_00ffu32.to_le_bytes());
    code[ldt_base + 12..ldt_base + 16].copy_from_slice(&0x0000_9205u32.to_le_bytes());
    memory[..code.len()].copy_from_slice(&code);
    cpu.write_reg16(Reg16::Ax, 0x0008); // LLDT AX: load LDTR from GDT selector 0x08.
    cpu.write_reg16(Reg16::Cx, 0x000c); // MOV DS, CX: load DS from LDT selector 0x000c.
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap(); // LLDT AX
    assert_eq!(cpu.ldtr.base, 0x0000_0200);
    cpu.cycle(&mut bus).unwrap(); // MOV DS, CX
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).selector, 0x000c);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ds).base,
        0x0005_0000,
        "DS must resolve against the LDT descriptor, not the GDT"
    );
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).limit, 0x00ff);
}

#[test]
fn gdt_selector_still_loads_after_the_ldt_fix() {
    // A plain GDT selector (TI=0) must still resolve against the GDT: regression guard for
    // the TI-bit fix in `load_protected_segment`.
    let (mut cpu, memory) = protected_cpu(&[0x8e, 0xd8], 0x0000_ffff, 0x0000_9200);
    cpu.write_reg16(Reg16::Ax, 0x0008);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).selector, 0x0008);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).base, 0);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).limit, 0xffff);
}

#[test]
fn out_of_limit_selector_still_faults() {
    // Selector 0x0028 (index 5) is past the GDT limit of 0x1f installed by `protected_cpu`
    // (which only covers offsets 0 and 8): a genuinely invalid, non-null selector must
    // still #GP, unaffected by the null-selector and LDT fixes.
    let (mut cpu, memory) = protected_cpu(&[0x8e, 0xd8], 0x0000_ffff, 0x0000_9200);
    cpu.write_reg16(Reg16::Ax, 0x0028);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            fault,
            InternalFault::Exception {
                vector: 13,
                error_code: Some(40)
            }
        ),
        "an out-of-limit selector must #GP(0x28), got {fault:?}"
    );
}

#[test]
fn retf_popping_a_null_selector_into_cs_faults() {
    // RETF (0xcb) in protected mode with the stacked far pointer's selector word set to
    // 0x0000 (null, index 0, TI 0). Unlike a data segment, CS must never be null: this
    // exercises load_segment(..., SegmentIndex::Cs, ...) through the real RETF path
    // (return_far -> load_segment -> load_protected_segment), not a synthetic direct call,
    // so it also confirms IRET/interrupt-gate delivery's CS reload would fault the same way.
    let (mut cpu, mut memory) = protected_cpu(&[0xcb], 0x0000_ffff, 0x0000_9200);
    memory.resize(0x200, 0);
    cpu.registers.set_esp(0x0100);
    // Stacked far pointer at ss:0x0100: offset 0x1234, then selector 0x0000 (null).
    memory[0x100..0x102].copy_from_slice(&0x1234u16.to_le_bytes());
    memory[0x102..0x104].copy_from_slice(&0x0000u16.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            fault,
            InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            }
        ),
        "RETF popping a null selector into CS must #GP, got {fault:?}"
    );
}

#[test]
fn ti_bit_set_index_zero_selector_resolves_against_the_ldt_not_treated_as_null() {
    // Selector 0x0004: index 0, TI=1. This is NOT a null selector (only index 0 AND TI 0
    // is null) -- it must resolve against LDT offset 0, not short-circuit into the
    // null/unusable path. Install an LDT (via LLDT, GDT selector 0x08) whose first entry
    // (offset 0) is a normal data descriptor, then load DS from selector 0x0004 and check
    // the resulting base/limit came from that LDT descriptor.
    let mut memory = vec![0u8; 0x400];
    // GDT selector 0x08: LDT system descriptor, base 0x0000_0300, limit 0x0f, access 0x82.
    let ldt_desc_low = 0x0300_000f;
    let ldt_desc_high = 0x0000_8200;
    let (mut cpu, mut code) =
        protected_cpu(&[0x0f, 0x00, 0xd0, 0x8e, 0xd9], ldt_desc_low, ldt_desc_high);
    code.resize(0x400, 0);
    // LDT at 0x300 (matches the descriptor base above). LDT offset 0 (selector 0x0004,
    // index 0, TI 1): data segment, base 0x0006_0000, limit 0x00aa, access 0x92.
    let ldt_base = 0x300usize;
    code[ldt_base..ldt_base + 4].copy_from_slice(&0x0000_00aau32.to_le_bytes());
    code[ldt_base + 4..ldt_base + 8].copy_from_slice(&0x0000_9206u32.to_le_bytes());
    memory[..code.len()].copy_from_slice(&code);
    cpu.write_reg16(Reg16::Ax, 0x0008); // LLDT AX.
    cpu.write_reg16(Reg16::Cx, 0x0004); // MOV DS, CX: selector 0x0004 (index 0, TI 1).
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap(); // LLDT AX
    assert_eq!(cpu.ldtr.base, 0x0000_0300);
    cpu.cycle(&mut bus).unwrap(); // MOV DS, CX
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).selector, 0x0004);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ds).base,
        0x0006_0000,
        "index-0/TI-1 selector 0x0004 must resolve against LDT[0], not be treated as null"
    );
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).limit, 0x00aa);
    assert_ne!(
        cpu.registers.segment(SegmentIndex::Ds).access & 0x80,
        0,
        "a resolved LDT descriptor load must install a present segment, not the null/unusable default"
    );
}

#[test]
fn verr_sets_zf_for_a_readable_segment() {
    // Readable data segment: access 0x92 (P, S, data, writable -> readable).
    let (mut cpu, memory) = protected_cpu(&[0x0f, 0x00, 0xe0], 0x0000_ffff, 0x0000_9200);
    cpu.write_reg16(Reg16::Ax, 0x0008);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(
        cpu.flag(FLAG_ZF),
        "VERR should set ZF for a readable segment"
    );
}

#[test]
fn lar_and_lsl_read_descriptor_fields() {
    // Data segment access 0x92, byte-granular limit 0xffff.
    // LAR ax, cx (0F 02 C1); CX holds the selector.
    let (mut cpu, memory) = protected_cpu(&[0x0f, 0x02, 0xc1], 0x0000_ffff, 0x0000_9200);
    cpu.write_reg16(Reg16::Cx, 0x0008);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x9200);

    // LSL ax, cx (0F 03 C1) -> the byte-granular limit.
    let (mut cpu, memory) = protected_cpu(&[0x0f, 0x03, 0xc1], 0x0000_ffff, 0x0000_9200);
    cpu.write_reg16(Reg16::Cx, 0x0008);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xffff);
}

// ---- Exception error codes and FPU #MF ----

#[test]
fn error_code_vectors_are_classified() {
    for v in [8u8, 10, 11, 12, 13, 14, 17] {
        assert!(
            vector_pushes_error_code(v),
            "vector {v} should carry a code"
        );
    }
    for v in [0u8, 1, 3, 4, 5, 6, 7, 9, 16, 18, 19] {
        assert!(!vector_pushes_error_code(v), "vector {v} carries no code");
    }
}

/// FLDZ; FLD1; FDIV ST0,ST1 (divide 1 by 0); FWAIT.
const DIV_BY_ZERO_THEN_WAIT: [u8; 7] = [0xd9, 0xee, 0xd9, 0xe8, 0xd8, 0xf1, 0x9b];

#[test]
fn unmasked_divide_by_zero_traps_mf_on_fwait() {
    let (mut cpu, memory) = real_mode_cpu(&DIV_BY_ZERO_THEN_WAIT, 0x40);
    cpu.fpu.control = 0x037b; // default mask with ZM (bit 2) cleared
    cpu.control.cr0 |= CR0_NE;
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..3 {
        cpu.cycle(&mut bus).unwrap(); // FLDZ, FLD1, FDIV
    }
    assert_ne!(cpu.fpu.status & 0x04, 0, "ZE flag set");
    // FWAIT (0x9b) is now on the decode/execute split (its fused arm is gone), so drive it
    // through `exec_one_split` rather than the legacy fused entry, which would #UD it.
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 16, .. }));
}

#[test]
fn masked_divide_by_zero_does_not_trap() {
    let (mut cpu, memory) = real_mode_cpu(&DIV_BY_ZERO_THEN_WAIT, 0x40);
    // Default control 0x037F masks every exception.
    cpu.control.cr0 |= CR0_NE;
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..4 {
        cpu.cycle(&mut bus).unwrap(); // FWAIT retires normally
    }
    assert_ne!(cpu.fpu.status & 0x04, 0, "ZE flag still latched");
}

#[test]
fn mf_is_suppressed_when_ne_is_clear() {
    // Unmasked exception but CR0.NE clear: the PC's FERR/IRQ13 path applies, so no
    // internal #MF. FWAIT retires.
    let (mut cpu, memory) = real_mode_cpu(&DIV_BY_ZERO_THEN_WAIT, 0x40);
    cpu.fpu.control = 0x037b;
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..4 {
        cpu.cycle(&mut bus).unwrap();
    }
}

// ---- Call gates and privilege-level stack switching ----

/// Protected-mode CPU with a GDT at 0x100 holding the given (selector, low, high)
/// descriptors. CS/SS default to ring 0 (real-mode shells, base 0); SP at 0x80.
fn protected_cpu_with_gdt(code: &[u8], descriptors: &[(u16, u32, u32)]) -> (CpuGsw, Vec<u8>) {
    let mut memory = vec![0u8; 0x400];
    memory[..code.len()].copy_from_slice(code);
    for &(sel, low, high) in descriptors {
        let off = 0x100 + (sel & !0x7) as usize;
        memory[off..off + 4].copy_from_slice(&low.to_le_bytes());
        memory[off + 4..off + 8].copy_from_slice(&high.to_le_bytes());
    }
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x80);
    cpu.gdtr = DescriptorTable {
        base: 0x100,
        limit: 0xff,
    };
    cpu.control.cr0 |= CR0_PE;
    (cpu, memory)
}

// Flat ring-0 code at 0x08, and a 386 call gate at 0x10 -> 0x08:0x40.
const RING0_CODE: (u16, u32, u32) = (0x08, 0x0000_ffff, 0x00cf_9b00);
const CALL_GATE_DPL0: (u16, u32, u32) = (0x10, 0x0008_0040, 0x0000_8c00);

#[test]
fn call_gate_same_privilege_transfers() {
    // CALL FAR 0x10:0 -> through the gate to 0x08:0x40, return pushed.
    let (mut cpu, memory) = protected_cpu_with_gdt(
        &[0x9a, 0x00, 0x00, 0x10, 0x00],
        &[RING0_CODE, CALL_GATE_DPL0],
    );
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.cs().selector, 0x08);
    assert_eq!(cpu.registers.eip, 0x40);
    // The gate is a 386 (32-bit) gate, so the return CS:EIP is two dwords.
    assert_eq!(
        cpu.registers.esp(),
        0x80 - 8,
        "return offset+selector pushed"
    );
}

#[test]
fn jmp_gate_transfers_without_pushing_return() {
    // JMP FAR 0x10:0 -> same target, no return frame.
    let (mut cpu, memory) = protected_cpu_with_gdt(
        &[0xea, 0x00, 0x00, 0x10, 0x00],
        &[RING0_CODE, CALL_GATE_DPL0],
    );
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.cs().selector, 0x08);
    assert_eq!(cpu.registers.eip, 0x40);
    assert_eq!(cpu.registers.esp(), 0x80, "JMP pushes nothing");
}

#[test]
fn call_gate_inter_privilege_switches_stack() {
    // Ring-3 caller through a DPL-3 gate into ring-0 code, copying two dword params.
    let ring0_data = (0x10u16, 0x0000_ffff, 0x00cf_9300);
    let gate_dpl3 = (0x30u16, 0x0008_0040, 0x0000_ec02); // DPL3 386 gate, 2 params
    let (mut cpu, mut memory) = protected_cpu_with_gdt(
        &[0x9a, 0x00, 0x00, 0x30, 0x00],
        &[RING0_CODE, ring0_data, gate_dpl3],
    );
    // Run at CPL 3 with a ring-3 CS and SS (set the cached registers directly).
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x1b,
            base: 0,
            limit: 0xf_ffff,
            access: 0xfb,
            default_size_32: false,
        },
    );
    cpu.registers.set_segment(
        SegmentIndex::Ss,
        SegmentRegister {
            selector: 0x23,
            base: 0,
            limit: 0xf_ffff,
            access: 0xf3,
            default_size_32: false,
        },
    );
    cpu.registers.set_esp(0xc0);
    cpu.cpl = 3; // this test sets CS/SS directly, so seed the cached CPL to match
    // Two parameters on the outer stack.
    memory[0xc0..0xc4].copy_from_slice(&0x1111u32.to_le_bytes());
    memory[0xc4..0xc8].copy_from_slice(&0x2222u32.to_le_bytes());
    // TSS at 0x300 with the ring-0 stack: ESP0 at +4, SS0 at +8.
    cpu.tr.base = 0x300;
    memory[0x304..0x308].copy_from_slice(&0x00f0u32.to_le_bytes());
    memory[0x308..0x30a].copy_from_slice(&0x0010u16.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x08, "entered ring-0 code");
    assert_eq!(cpu.registers.eip, 0x40);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ss).selector,
        0x10,
        "switched to SS0"
    );
    // Frame on the new stack: 6 dwords pushed below ESP0 = 0xF0.
    assert_eq!(cpu.registers.esp(), 0xf0 - 24);
    // Return EIP (5, past the CALL) at the top; param0 above the return frame.
    assert_eq!(
        u32::from_le_bytes(bus.memory[0xd8..0xdc].try_into().unwrap()),
        5
    );
    assert_eq!(
        u32::from_le_bytes(bus.memory[0xe0..0xe4].try_into().unwrap()),
        0x1111
    );
}

#[test]
fn call_gate_inter_privilege_reads_params_from_a_16bit_outer_stack_with_esp_high_garbage() {
    // The DOS4GW/VCPI scenario: the outer (caller's) stack is SS.B=0 with garbage
    // in ESP's high word. Per PRM 17-42 the old stack's top is SS:SP -- the param
    // read must use the wrapped 16-bit SP, not outer_esp + k*psize on the full
    // (garbage-laden) ESP, which would read from a bogus linear address entirely
    // outside the intended stack page.
    let ring0_data = (0x10u16, 0x0000_ffff, 0x00cf_9300);
    let gate_dpl1 = (0x30u16, 0x0008_0040, 0x0000_ec01); // DPL3 386 gate, 1 param
    let (mut cpu, mut memory) = protected_cpu_with_gdt(
        &[0x9a, 0x00, 0x00, 0x30, 0x00],
        &[RING0_CODE, ring0_data, gate_dpl1],
    );
    memory.resize(0x1_0004, 0);
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x1b,
            base: 0,
            limit: 0xf_ffff,
            access: 0xfb,
            default_size_32: false,
        },
    );
    cpu.registers.set_segment(
        SegmentIndex::Ss,
        SegmentRegister {
            selector: 0x23,
            base: 0,
            limit: 0xf_ffff,
            access: 0xf3,
            default_size_32: false, // SS.B=0: the outer stack is 16-bit.
        },
    );
    // SP = 0xfffe, but ESP's high word carries garbage that a full-ESP add would
    // corrupt into a wrong linear address entirely -- the wrapped SP is the only
    // correct read point. The single param sits at SP=0xfffe, well clear of the
    // 5-byte CALL instruction at offset 0.
    cpu.registers.set_esp(0xbeef_fffe);
    cpu.cpl = 3;
    memory[0xfffe..0x1_0002].copy_from_slice(&0x1111u32.to_le_bytes());
    cpu.tr.base = 0x300;
    memory[0x304..0x308].copy_from_slice(&0x00f0u32.to_le_bytes());
    memory[0x308..0x30a].copy_from_slice(&0x0010u16.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x08, "entered ring-0 code");
    assert_eq!(cpu.registers.eip, 0x40);
    // Frame on the new stack: return CS:EIP + old SS:ESP + 1 param = 5 dwords.
    assert_eq!(cpu.registers.esp(), 0xf0 - 20);
    // The param, pushed just above the return frame, must be the value at the
    // wrapped SP 0xfffe, not whatever garbage a full-ESP read would have hit.
    assert_eq!(
        u32::from_le_bytes(bus.memory[0xe4..0xe8].try_into().unwrap()),
        0x1111,
        "param read from the wrapped SP, not full-ESP garbage"
    );
}

// ---- CPL transition unit tests (the `cpl` field, one per PRM transition-point
// class named in the VCPI substrate fix). Each drives a real transfer through
// `cycle`/`deliver_exception`/`iret` and asserts `current_privilege_level()`
// lands where the PRM says, not merely that a CS selector's low bits look right.

#[test]
fn cpl_transition_call_gate_inter_privilege_call_lowers_cpl_to_target_dpl() {
    // Reuses the exact fixture from `call_gate_inter_privilege_switches_stack`:
    // a ring-3 caller through a DPL-3 gate into ring-0 code. The cached CPL must
    // read 0 once inside the gate's target, not just the CS selector's RPL.
    let ring0_data = (0x10u16, 0x0000_ffff, 0x00cf_9300);
    let gate_dpl3 = (0x30u16, 0x0008_0040, 0x0000_ec02);
    let (mut cpu, mut memory) = protected_cpu_with_gdt(
        &[0x9a, 0x00, 0x00, 0x30, 0x00],
        &[RING0_CODE, ring0_data, gate_dpl3],
    );
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x1b,
            base: 0,
            limit: 0xf_ffff,
            access: 0xfb,
            default_size_32: false,
        },
    );
    cpu.registers.set_segment(
        SegmentIndex::Ss,
        SegmentRegister {
            selector: 0x23,
            base: 0,
            limit: 0xf_ffff,
            access: 0xf3,
            default_size_32: false,
        },
    );
    cpu.registers.set_esp(0xc0);
    cpu.cpl = 3;
    cpu.tr.base = 0x300;
    memory[0x304..0x308].copy_from_slice(&0x00f0u32.to_le_bytes());
    memory[0x308..0x30a].copy_from_slice(&0x0010u16.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);

    assert_eq!(cpu.current_privilege_level(), 3, "starts at CPL 3");
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.current_privilege_level(),
        0,
        "the call gate's target DPL (0) is the new CPL"
    );
}

#[test]
fn cpl_transition_far_jmp_direct_tracks_the_loaded_cs_rpl() {
    // A direct (non-gate) far JMP to a flat code segment: no privilege check is
    // enforced on this path today, but the cached CPL must still track whatever
    // CS RPL the jump landed on (same live-formula answer as before, just cached).
    let target = (0x20u16, 0x0000_ffff, 0x00cf_fb00); // DPL 3 code segment
    let (mut cpu, memory) = protected_cpu_with_gdt(
        &[0xea, 0x00, 0x00, 0x23, 0x00], // JMP FAR 0x23:0 (RPL 3)
        &[RING0_CODE, target],
    );
    let mut bus = TestBus::with_memory(memory);
    assert_eq!(cpu.current_privilege_level(), 0, "starts at CPL 0");
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.cs().selector & 3, 3);
    assert_eq!(
        cpu.current_privilege_level(),
        3,
        "direct far JMP's cached CPL follows the loaded CS RPL"
    );
}

#[test]
fn cpl_transition_iret_into_v86_forces_cpl_3() {
    // A ring-0 IRET whose popped EFLAGS carries VM=1 always lands at CPL 3,
    // regardless of the popped V86 CS's arbitrary selector bits.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    cpu.registers.eflags = 0x2; // ring 0, no VM
    cpu.cpl = 0;
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    // 0x9000 is unused by `v86_world`'s memory map (PD/PT/GDT/IDT/TSS/ESP0/monitor
    // code all sit below it, guest code at 0xA000 above); avoids clobbering the
    // paging structures the way writing at 0x1000 (the PD itself) would.
    cpu.registers.set_esp(0x9000);
    // Build the V86-return IRET frame by hand: EIP, CS(0xFFFF, arbitrary low
    // bits), EFLAGS(VM=1), ESP, SS, ES, DS, FS, GS.
    // Lay out the frame in ascending-address (pop) order: IRET pops EIP, CS,
    // EFLAGS, then (VM=1 detected) ESP, SS, ES, DS, FS, GS.
    let mut write = |offset: u32, v: u32| {
        put32(&mut bus.memory, 0x9000 + offset, v);
    };
    write(0, 0x10); // EIP
    write(4, 0xffff); // CS (RPL bits arbitrary/irrelevant)
    write(8, FLAG_VM | 0x2); // EFLAGS
    write(12, 0x2000); // ESP
    write(16, 0x0900); // SS
    write(20, 0x1111); // ES
    write(24, 0x2222); // DS
    write(28, 0x3333); // FS
    write(32, 0x4444); // GS

    cpu.iret(&mut bus, OperandSize::Dword).unwrap();

    assert!(cpu.is_v86_mode(), "returned into V86");
    assert_eq!(
        cpu.current_privilege_level(),
        3,
        "IRET-into-V86 always forces CPL 3"
    );
}

#[test]
fn cpl_transition_pe_clear_resets_cpl_to_zero() {
    // MOV CR0, EAX clearing PE (require_cpl0-gated, so CPL was already 0): the
    // cache must stay 0 across the real-mode transition, matching real mode's
    // fixed CPL 0.
    let mut memory = vec![0u8; 16];
    memory[..3].copy_from_slice(&[0x0f, 0x22, 0xc0]); // MOV CR0, EAX
    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0); // clears PE
    let mut bus = TestBus::with_memory(memory);

    assert_eq!(cpu.current_privilege_level(), 0);
    cpu.cycle(&mut bus).unwrap();
    assert!(!cpu.is_protected_mode(), "PE cleared");
    assert_eq!(
        cpu.current_privilege_level(),
        0,
        "real mode is always CPL 0"
    );
}

// ---- Hardware task switch ----

#[test]
fn jmp_to_tss_performs_a_task_switch() {
    // New 386 TSS at 0x380 (selector 0x18), old busy TSS at 0x300 (selector 0x20).
    let new_tss = (0x18u16, 0x0380_0067, 0x0000_8900);
    let old_tss = (0x20u16, 0x0300_0067, 0x0000_8b00);
    let ring0_data = (0x10u16, 0x0000_ffff, 0x00cf_9300);
    let (mut cpu, mut memory) = protected_cpu_with_gdt(
        &[0xea, 0x00, 0x00, 0x18, 0x00],
        &[RING0_CODE, ring0_data, new_tss, old_tss],
    );
    cpu.tr = SegmentRegister {
        selector: 0x20,
        base: 0x300,
        limit: 0x67,
        access: 0x8b,
        default_size_32: false,
    };
    let put32 =
        |m: &mut [u8], off: usize, v: u32| m[off..off + 4].copy_from_slice(&v.to_le_bytes());
    let put16 =
        |m: &mut [u8], off: usize, v: u16| m[off..off + 2].copy_from_slice(&v.to_le_bytes());
    put32(&mut memory, 0x380 + 32, 0x200); // EIP
    put32(&mut memory, 0x380 + 36, 0x0000_0002); // EFLAGS
    put32(&mut memory, 0x380 + 40, 0xaaaa); // EAX
    put32(&mut memory, 0x380 + 56, 0x00f0); // ESP
    put16(&mut memory, 0x380 + 72, 0x10); // ES
    put16(&mut memory, 0x380 + 76, 0x08); // CS
    put16(&mut memory, 0x380 + 80, 0x10); // SS
    put16(&mut memory, 0x380 + 84, 0x10); // DS
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x08, "loaded new task CS");
    assert_eq!(cpu.registers.eip, 0x200);
    assert_eq!(cpu.registers.eax(), 0xaaaa);
    assert_eq!(cpu.registers.esp(), 0x00f0);
    assert_eq!(cpu.tr.selector, 0x18, "task register points at the new TSS");
    assert_ne!(cpu.control.cr0 & CR0_TS, 0, "TS set on a task switch");
    // The outgoing task's EIP (past the 5-byte JMP) was saved into the old TSS.
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x320..0x324].try_into().unwrap()),
        5
    );
    // JMP clears the old TSS busy bit in its GDT descriptor (0x8b -> 0x89).
    assert_eq!(bus.memory[0x100 + 0x20 + 5], 0x89);
}

// ---- BOUND and INS/OUTS ----

#[test]
fn bound_passes_when_in_range() {
    // BOUND AX, [0x100] (62 06 00 01); bounds [10, 20]; AX = 15.
    let (mut cpu, mut memory) = real_mode_cpu(&[0x62, 0x06, 0x00, 0x01], 0x200);
    memory[0x100..0x102].copy_from_slice(&10u16.to_le_bytes());
    memory[0x102..0x104].copy_from_slice(&20u16.to_le_bytes());
    cpu.write_reg16(Reg16::Ax, 15);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eip, 4);
}

#[test]
fn bound_raises_br_out_of_range() {
    let (mut cpu, mut memory) = real_mode_cpu(&[0x62, 0x06, 0x00, 0x01], 0x200);
    memory[0x100..0x102].copy_from_slice(&10u16.to_le_bytes());
    memory[0x102..0x104].copy_from_slice(&20u16.to_le_bytes());
    cpu.write_reg16(Reg16::Ax, 25);
    let mut bus = TestBus::with_memory(memory);
    // BOUND (0x62) is converted to the decode/execute split (task A12); the #BR (vector 5) is
    // raised in `execute_system_seg_decoded`, so run it through the split.
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 5, .. }));
}

#[test]
fn insb_stores_port_byte_to_es_di() {
    // INSB (0x6C): [ES:DI] <- port[DX]. TestBus returns 0, so the 0xFF clears.
    let (mut cpu, mut memory) = real_mode_cpu(&[0x6c], 0x200);
    memory[0x100] = 0xff;
    cpu.write_reg16(Reg16::Dx, 0x03f8);
    cpu.write_reg16(Reg16::Di, 0x0100);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(bus.memory[0x100], 0x00);
    assert_eq!(cpu.read_reg16(Reg16::Di), 0x0101);
    assert!(
        bus.trace
            .cycles()
            .iter()
            .any(|c| c.kind == BusAccessKind::IoRead && c.address == 0x03f8)
    );
}

#[test]
fn rep_outsw_writes_words_from_ds_si() {
    // REP OUTSW (F3 6F): write CX words from [DS:SI] to port[DX].
    let (mut cpu, memory) = real_mode_cpu(&[0xf3, 0x6f], 0x200);
    cpu.write_reg16(Reg16::Cx, 2);
    cpu.write_reg16(Reg16::Si, 0x0100);
    cpu.write_reg16(Reg16::Dx, 0x03f8);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    let writes = bus
        .trace
        .cycles()
        .iter()
        .filter(|c| {
            c.kind == BusAccessKind::IoWrite && c.width == BusWidth::Word && c.address == 0x03f8
        })
        .count();
    assert_eq!(writes, 2);
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0);
    assert_eq!(cpu.read_reg16(Reg16::Si), 0x0104);
}

// ---- Virtual-8086 mode ----

#[test]
fn v86_segment_load_uses_real_mode_base() {
    // MOV DS, AX (8E D8) in a V86 task: DS base = selector << 4.
    let (mut cpu, memory) = real_mode_cpu(&[0x8e, 0xd8], 0x40);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags |= FLAG_VM;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).base, 0x1_2340);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).selector, 0x1234);
}

#[test]
fn v86_far_call_uses_real_mode_segments() {
    // CALL FAR 0x8FA9:0x1234 (9A off16 seg16) in a V86 task must be an 8086-style
    // far call (CS = 0x8FA9, base 0x8FA90), never a GDT descriptor lookup — 0x8FA9
    // is not a valid selector and would #GP. Regression for the V86 boot:
    // real FreeDOS makes far calls to high segments while virtualized.
    let (mut cpu, memory) = real_mode_cpu(&[0x9a, 0x34, 0x12, 0xa9, 0x8f], 0x200);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x2 | FLAG_VM | 0x3000; // IOPL 3
    cpu.cpl = 3;
    cpu.registers.set_esp(0x100);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.cs().selector, 0x8fa9);
    assert_eq!(cpu.registers.cs().base, 0x8_fa90);
    assert_eq!(cpu.registers.eip & 0xffff, 0x1234);
}

#[test]
fn cli_faults_in_v86_below_iopl3() {
    // CLI (0xFA) in a V86 task with IOPL 0 traps to the monitor with #GP(0).
    // CLI is converted to DecodeGroup::FlagsMisc, so drive it through the split (exec_one_split)
    // rather than execute_instruction_legacy, which no longer carries the 0xFA arm.
    let (mut cpu, memory) = real_mode_cpu(&[0xfa], 0x40);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x2 | FLAG_VM; // IOPL 0
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 13, .. }));
}

#[test]
fn cli_runs_in_v86_at_iopl3() {
    // With IOPL 3 the V86 task may touch IF directly.
    let (mut cpu, memory) = real_mode_cpu(&[0xfa], 0x40);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x2 | FLAG_VM | 0x3000 | FLAG_IF; // IOPL 3, IF set
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(!cpu.flag(FLAG_IF), "CLI cleared IF");
}

#[test]
fn iret_faults_in_v86_below_iopl3() {
    // IRET (0xCF) in a V86 task with IOPL 0 traps to the monitor with #GP(0), exactly
    // like CLI/STI/PUSHF/POPF. This is the TOKAEMM root-cause fix: a V86 guest's IRET
    // must be IOPL-gated so the monitor can virtualize the flags pop (VIF), instead of
    // popping a monitor-stamped IF=0 image straight into real EFLAGS.
    let (mut cpu, memory) = real_mode_cpu(&[0xcf], 0x40);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x2 | FLAG_VM; // IOPL 0
    let esp_before = cpu.registers.esp();
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 13, .. }));
    assert_eq!(
        cpu.registers.esp(),
        esp_before,
        "faulted IRET must not pop the stack"
    );
}

#[test]
fn iret_runs_in_v86_at_iopl3() {
    // With IOPL 3 the V86 task may execute a native 8086-style IRET directly: pop
    // IP/CS/FLAGS from the stack with no monitor round-trip.
    let (mut cpu, memory) = real_mode_cpu(&[0xcf], 0x40);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x2 | FLAG_VM | 0x3000; // IOPL 3
    cpu.cpl = 3; // V86 is always CPL 3; load_flags reads the cached cpl.
    cpu.registers.set_esp(0x20);
    let mut bus = TestBus::with_memory(memory);
    // 16-bit IRET frame at SS:0x20 (IP, CS, FLAGS), popped low-to-high.
    bus.memory[0x20..0x22].copy_from_slice(&0x1234u16.to_le_bytes());
    bus.memory[0x22..0x24].copy_from_slice(&0x0050u16.to_le_bytes());
    let popped_flags = (0x2 | FLAG_VM | 0x3000 | FLAG_IF) as u16;
    bus.memory[0x24..0x26].copy_from_slice(&popped_flags.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.is_v86_mode(), "IRET at IOPL 3 must stay in V86");
    assert_eq!(cpu.registers.eip, 0x1234);
    assert_eq!(cpu.registers.cs().selector, 0x0050);
    assert_eq!(cpu.registers.esp(), 0x26, "IRET must pop all three words");
    assert!(
        cpu.flag(FLAG_IF),
        "native IRET loads IF straight from the popped image"
    );
}

#[test]
fn iret_in_v86_at_iopl3_cannot_drop_iopl() {
    // The JEMMEX/TOKAEMM root cause: a V86 client is deliberately run at IOPL 3 so its
    // own native (same-privilege, CPL 3) IRET never traps to the monitor. Per the 386
    // PRM (section 9.7.1.2), "The IOPL field ... is restored only if the CPL is 0" -- at
    // CPL 3 a stale/zeroed IOPL field in the popped image must never reach real EFLAGS.
    let (mut cpu, memory) = real_mode_cpu(&[0xcf], 0x40);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x2 | FLAG_VM | 0x3000; // IOPL 3
    cpu.cpl = 3; // V86 is always CPL 3; load_flags reads the cached cpl.
    cpu.registers.set_esp(0x20);
    let mut bus = TestBus::with_memory(memory);
    bus.memory[0x20..0x22].copy_from_slice(&0x1234u16.to_le_bytes());
    bus.memory[0x22..0x24].copy_from_slice(&0x0050u16.to_le_bytes());
    // Popped image carries IOPL=0 (bits 12-13 clear) -- exactly the stale flags word
    // traced in the field: a JEMM-internal in-V86 IRET popping 0x200.
    let popped_flags = (0x2 | FLAG_VM | FLAG_IF) as u16;
    bus.memory[0x24..0x26].copy_from_slice(&popped_flags.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.is_v86_mode());
    assert_eq!(
        cpu.registers.eflags & FLAG_IOPL,
        FLAG_IOPL,
        "IRET at CPL 3 must not lower live IOPL from the popped image"
    );
    assert!(
        cpu.flag(FLAG_IF),
        "CPL 3 <= (unchanged) IOPL 3, so IF still loads from the popped image"
    );
}

#[test]
fn popf_in_v86_at_iopl3_cannot_drop_iopl() {
    // Same PRM rule (POPF/POPFD, p.17-136), driven through POPF instead of IRET.
    let (mut cpu, memory) = real_mode_cpu(&[0x9d], 0x40);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x2 | FLAG_VM | 0x3000; // IOPL 3
    cpu.cpl = 3; // V86 is always CPL 3; load_flags reads the cached cpl.
    cpu.registers.set_esp(0x20);
    let mut bus = TestBus::with_memory(memory);
    let popped_flags = (0x2 | FLAG_IF) as u16; // IOPL 0 in the popped image
    bus.memory[0x20..0x22].copy_from_slice(&popped_flags.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eflags & FLAG_IOPL,
        FLAG_IOPL,
        "POPF at CPL 3 must not lower live IOPL from the popped image"
    );
    assert!(cpu.is_v86_mode(), "POPF must never clear VM");
}

#[test]
fn pmode_ring3_popf_below_iopl_preserves_if_and_iopl() {
    // Non-V86 ring-3 POPF with IOPL < 3 reaches native load_flags directly (no V86 trap
    // upstream) and per the PRM must leave both IF and IOPL untouched. Built like
    // `cpl3_code`, but with a matching flat CPL-3 SS so POPF can pop the stack.
    let mut memory = vec![0u8; 256];
    memory[0] = 0x9d; // POPF
    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: true,
        },
    );
    cpu.registers.set_segment(
        SegmentIndex::Ss,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x93,
            default_size_32: true,
        },
    );
    cpu.registers.eip = 0;
    cpu.cpl = 3;
    cpu.registers.eflags = 0x2 | FLAG_IF; // IOPL 0, IF set, CPL 3
    cpu.registers.set_esp(0x80);
    let mut bus = TestBus::with_memory(memory);
    // CS.default_size_32 makes plain 9D a POPFD (32-bit pop). Popped image tries to
    // clear IF and raise IOPL to 3.
    let popped_flags = 0x2u32 | 0x3000;
    bus.memory[0x80..0x84].copy_from_slice(&popped_flags.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert!(
        cpu.flag(FLAG_IF),
        "CPL 3 > IOPL 0: IF must keep its live value, not the popped clear"
    );
    assert_eq!(
        cpu.registers.eflags & FLAG_IOPL,
        0,
        "CPL 3 != 0: IOPL must keep its live value, not the popped raise"
    );
}

#[test]
fn cpl0_popfd_still_loads_iopl_and_if_fully() {
    // CPL 0 native POPFD is the one case the PRM lets change IOPL, and IF always loads
    // there too (CPL 0 <= any IOPL). Existing full-load behavior must be unchanged.
    let (mut cpu, memory) = real_mode_cpu(&[0x66, 0x9d], 0x40); // POPFD
    cpu.registers.set_esp(0x20);
    let mut bus = TestBus::with_memory(memory);
    let popped_flags = 0x2u32 | 0x3000 | FLAG_IF;
    bus.memory[0x20..0x24].copy_from_slice(&popped_flags.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eflags & FLAG_IOPL, FLAG_IOPL);
    assert!(cpu.flag(FLAG_IF));
}

#[test]
fn popfd_can_never_set_vm_at_any_cpl() {
    // POPF/POPFD can never alter VM (bit 17) at any CPL -- real mode here is CPL 0,
    // the most permissive case, and even it must not let VM turn on via a flags pop.
    let (mut cpu, memory) = real_mode_cpu(&[0x66, 0x9d], 0x40); // POPFD
    cpu.registers.set_esp(0x20);
    let mut bus = TestBus::with_memory(memory);
    let popped_flags = 0x2u32 | FLAG_VM;
    bus.memory[0x20..0x24].copy_from_slice(&popped_flags.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eflags & FLAG_VM, 0, "POPFD must never set VM");
}

// ---- Unreal / flat-real mode: the real-mode segment load preserves the cached limit ----

/// A 4 GB flat data descriptor at GDT selector 0x10 (base 0, limit 0xFFFFF with G=1,
/// access 0x93), the shape SpeedSys/DOS4GW-style loaders install before dropping back to
/// real mode. `memory` is sized past 1 MB so a >64 KB access has somewhere to land.
fn unreal_world(code: &[u8]) -> (CpuGsw, Vec<u8>) {
    let mut memory = vec![0u8; 0x11_0010];
    memory[..code.len()].copy_from_slice(code);
    let flat = 0x00cf_9300u32;
    memory[0x110..0x114].copy_from_slice(&0x0000_ffffu32.to_le_bytes());
    memory[0x114..0x118].copy_from_slice(&flat.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x80);
    cpu.gdtr = DescriptorTable {
        base: 0x100,
        limit: 0xff,
    };
    (cpu, memory)
}

#[test]
fn real_mode_segment_load_preserves_a_protected_mode_limit() {
    // The whole unreal-mode sequence, driven as instructions: enter protected mode with a
    // 4 GB data descriptor in DS, clear CR0.PE, reload DS in real mode, then read a dword
    // 1 MB up. The final load is the one that used to stamp `limit = 0xFFFF` and turn every
    // subsequent >64 KB access into #GP (SpeedSys 4.78 hung in exactly this shape).
    let (mut cpu, memory) = unreal_world(&[
        0xb8, 0x10, 0x00, // mov ax, 0x10
        0x8e, 0xd8, // mov ds, ax        -- protected-mode load, limit 4 GB
        0x0f, 0x20, 0xc0, // mov eax, cr0
        0x24, 0xfe, // and al, 0xfe
        0x0f, 0x22, 0xc0, // mov cr0, eax     -- PE 1 -> 0
        0xb8, 0x00, 0x00, // mov ax, 0
        0x8e, 0xd8, // mov ds, ax        -- REAL-mode load
        0x67, 0x66, 0x8b, 0x05, 0x00, 0x00, 0x11, 0x00, // mov eax, [dword 0x110000]
    ]);
    let mut bus = TestBus::with_memory(memory);
    bus.memory[0x11_0000..0x11_0004].copy_from_slice(&0xdead_beefu32.to_le_bytes());
    cpu.control.cr0 |= CR0_PE;

    for _ in 0..5 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert!(
        !cpu.is_protected_mode(),
        "PE must be clear before the reload"
    );
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ds).limit,
        0xffff_ffff,
        "clearing CR0.PE must not touch a descriptor cache"
    );

    cpu.cycle(&mut bus).unwrap(); // mov ax, 0
    cpu.cycle(&mut bus).unwrap(); // mov ds, ax  -- the real-mode load under test
    let ds = cpu.registers.segment(SegmentIndex::Ds);
    assert_eq!(ds.selector, 0, "the selector IS recomputed");
    assert_eq!(ds.base, 0, "base = selector << 4 IS recomputed");
    assert_eq!(ds.access, 0x93, "access IS recomputed");
    assert!(!ds.default_size_32, "B is re-stamped false, not preserved");
    assert_eq!(
        ds.limit, 0xffff_ffff,
        "the cached limit survives a real-mode load -- this IS unreal mode"
    );

    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eax(),
        0xdead_beef,
        "a 1 MB-up read through the unreal DS must complete, not #GP"
    );
}

#[test]
fn real_mode_cs_load_recanonicalizes_the_limit() {
    // CS is the documented exception: a real-mode CS load rebuilds the whole descriptor,
    // so a far jump out of protected mode really does give back a 64 KB code segment.
    // DS, loaded in the same breath, keeps its big limit -- that split is the hardware's.
    let (mut cpu, memory) = unreal_world(&[]);
    let mut bus = TestBus::with_memory(memory);
    let big = SegmentRegister::flat(0x10, 0x93);
    cpu.registers.set_segment(SegmentIndex::Cs, big);
    cpu.registers.set_segment(SegmentIndex::Ds, big);

    cpu.load_segment(&mut bus, SegmentIndex::Cs, 0x1234)
        .unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ds, 0x1234)
        .unwrap();

    let cs = cpu.registers.cs();
    assert_eq!(cs.limit, 0xffff, "a real-mode CS load re-canonicalizes");
    assert_eq!(cs.base, 0x1_2340);
    assert!(!cs.default_size_32);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ds).limit,
        0xffff_ffff,
        "a data segment loaded at the same moment keeps its limit"
    );
}

#[test]
fn v86_segment_load_forces_the_64k_limit() {
    // V86 is not unreal mode: a V86 task addresses memory like the 8086, 64 KB, no
    // exceptions (386 PRM 26.3.1). A stale big limit must not survive into it.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);
    cpu.registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x10, 0x93));

    cpu.load_segment(&mut bus, SegmentIndex::Ds, 0x0a00)
        .unwrap();

    let ds = cpu.registers.segment(SegmentIndex::Ds);
    assert_eq!(ds.limit, 0xffff, "V86 forces the 64 KB limit");
    assert_eq!(ds.base, 0xa000);
    assert!(!ds.default_size_32);
}

#[test]
fn clearing_cr0_pe_leaves_every_segment_cache_untouched() {
    // The PE 1 -> 0 transition itself rebuilds nothing. Both prior-art checkouts agree
    // (86Box's CR0 write and DOSBox-X's CPU_SET_CRX touch no segment field), and it is
    // what makes the sequence above reach real mode with the big limit still live.
    let (mut cpu, memory) = unreal_world(&[0x0f, 0x22, 0xc0]); // mov cr0, eax
    let mut bus = TestBus::with_memory(memory);
    cpu.control.cr0 |= CR0_PE;
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
        SegmentIndex::Ss,
    ] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(0x10, 0x93));
    }
    let before = cpu.registers.segments;
    cpu.registers.set_eax(0); // clears PE

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.is_protected_mode());
    assert_eq!(
        cpu.registers.segments, before,
        "clearing PE must not rewrite any descriptor cache"
    );
}

#[test]
fn a_real_mode_only_guest_never_sees_a_limit_other_than_0xffff() {
    // Boot-normal invariance: with no protected-mode excursion there is no big limit to
    // preserve, so every segment load still yields the canonical real-mode descriptor.
    // This is what keeps the fix invisible to guests that never enter unreal mode.
    let (mut cpu, memory) = unreal_world(&[
        0xb8, 0x00, 0x20, // mov ax, 0x2000
        0x8e, 0xd8, // mov ds, ax
        0x8e, 0xc0, // mov es, ax
        0x8e, 0xd0, // mov ss, ax
        0xea, 0x00, 0x00, 0x00, 0x30, // jmp far 0x3000:0
    ]);
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..5 {
        cpu.cycle(&mut bus).unwrap();
    }
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
        SegmentIndex::Ss,
    ] {
        let descriptor = cpu.registers.segment(segment);
        assert_eq!(descriptor.limit, 0xffff, "{segment:?} limit");
        assert_eq!(descriptor.access, 0x93, "{segment:?} access");
        assert!(!descriptor.default_size_32, "{segment:?} B bit");
    }
}

/// A hardware task switch INTO a V86 task with a null DS selector.
///
/// Selector 0 in a V86 task's DS is not the protected-mode null descriptor -- it is an
/// ordinary 8086 segment at base 0, and the task-switch restore loop must build it the way
/// every other V86 segment load does: base 0, limit 0xFFFF, the real-mode access byte. The
/// loop's null-selector short-circuit used to install `Default::default()` (limit 0, access
/// 0) for it, which is only right in protected mode.
///
/// This became reachable-and-visible with the unreal-mode fix. Before it, the JIT's
/// `LoadSegReal` lowering re-stamped 0xFFFF over the bad descriptor on the next
/// `MOV DS, r16` and silently repaired the state; now neither role writes the limit, so a
/// natively-executed segment load in such a task would have diverged from the interpreter.
#[test]
fn task_switch_into_v86_builds_a_null_data_selector_as_a_real_mode_segment() {
    let new_tss = (0x18u16, 0x0380_0067, 0x0000_8900);
    let old_tss = (0x20u16, 0x0300_0067, 0x0000_8b00);
    let ring0_data = (0x10u16, 0x0000_ffff, 0x00cf_9300);
    let (mut cpu, mut memory) = protected_cpu_with_gdt(
        &[0xea, 0x00, 0x00, 0x18, 0x00],
        &[RING0_CODE, ring0_data, new_tss, old_tss],
    );
    cpu.tr = SegmentRegister {
        selector: 0x20,
        base: 0x300,
        limit: 0x67,
        access: 0x8b,
        default_size_32: false,
    };
    let put32 =
        |m: &mut [u8], off: usize, v: u32| m[off..off + 4].copy_from_slice(&v.to_le_bytes());
    let put16 =
        |m: &mut [u8], off: usize, v: u16| m[off..off + 2].copy_from_slice(&v.to_le_bytes());
    put32(&mut memory, 0x380 + 32, 0x200); // EIP
    put32(&mut memory, 0x380 + 36, 0x0002_0002); // EFLAGS: VM set
    put32(&mut memory, 0x380 + 56, 0x00f0); // ESP
    put16(&mut memory, 0x380 + 72, 0x0a00); // ES
    put16(&mut memory, 0x380 + 76, 0x0a00); // CS
    put16(&mut memory, 0x380 + 80, 0x0900); // SS
    put16(&mut memory, 0x380 + 84, 0x0000); // DS -- null, and perfectly ordinary in V86
    put16(&mut memory, 0x380 + 88, 0x0000); // FS
    put16(&mut memory, 0x380 + 92, 0x0000); // GS
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.is_v86_mode(), "the incoming task's EFLAGS carried VM");
    assert_eq!(cpu.current_privilege_level(), 3, "a V86 task runs at CPL 3");
    for segment in [SegmentIndex::Ds, SegmentIndex::Fs, SegmentIndex::Gs] {
        assert_eq!(
            cpu.registers.segment(segment),
            SegmentRegister::real(0),
            "{segment:?} must be an 8086 segment, not a null descriptor"
        );
    }
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Es),
        SegmentRegister::real(0x0a00),
        "the non-null V86 segments take the same rule"
    );
}

#[test]
fn task_switch_into_protected_mode_still_builds_a_null_data_selector_as_unusable() {
    // The companion to the case above, and the reason the repair must be VM-gated: outside
    // V86 a null selector really is the null descriptor -- loadable without fault, base 0,
    // limit 0, and #GP on the first access through it (386 PRM 6.3.3).
    let new_tss = (0x18u16, 0x0380_0067, 0x0000_8900);
    let old_tss = (0x20u16, 0x0300_0067, 0x0000_8b00);
    let ring0_data = (0x10u16, 0x0000_ffff, 0x00cf_9300);
    let (mut cpu, mut memory) = protected_cpu_with_gdt(
        &[0xea, 0x00, 0x00, 0x18, 0x00],
        &[RING0_CODE, ring0_data, new_tss, old_tss],
    );
    cpu.tr = SegmentRegister {
        selector: 0x20,
        base: 0x300,
        limit: 0x67,
        access: 0x8b,
        default_size_32: false,
    };
    let put32 =
        |m: &mut [u8], off: usize, v: u32| m[off..off + 4].copy_from_slice(&v.to_le_bytes());
    let put16 =
        |m: &mut [u8], off: usize, v: u16| m[off..off + 2].copy_from_slice(&v.to_le_bytes());
    put32(&mut memory, 0x380 + 32, 0x200); // EIP
    put32(&mut memory, 0x380 + 36, 0x0000_0002); // EFLAGS: VM clear
    put32(&mut memory, 0x380 + 56, 0x00f0); // ESP
    put16(&mut memory, 0x380 + 72, 0x10); // ES
    put16(&mut memory, 0x380 + 76, 0x08); // CS
    put16(&mut memory, 0x380 + 80, 0x10); // SS
    put16(&mut memory, 0x380 + 84, 0x0000); // DS -- the null descriptor
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.is_v86_mode());
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ds),
        SegmentRegister::default(),
        "a protected-mode null selector stays the unusable null descriptor"
    );
}
