// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

// ---- Real-mode integer opcode coverage ----

/// Drive `run_straight_line` repeatedly the way the machine batch loop does (without devices or
/// interrupts): keep starting a fresh run from the current eip until one halts or a generous step
/// budget is exhausted. Returns the number of runs the executor produced, so a test can assert a
/// hot loop actually collapsed into multi-instruction runs rather than one-instruction stutters.
fn drive_straight_line_runs(cpu: &mut CpuGsw, bus: &mut TestBus) -> usize {
    let mut runs = 0;
    for _ in 0..10_000 {
        runs += 1;
        let outcome = cpu.run_straight_line(bus, u64::MAX).unwrap();
        if outcome.halted {
            return runs;
        }
    }
    panic!("straight-line driver never halted");
}

#[test]
fn budgeted_run_matches_the_compatibility_wrapper() {
    let (mut budgeted_cpu, memory) = real_mode_cpu(&[0x90, 0xf4], 32);
    let mut legacy_cpu = budgeted_cpu.clone();
    let mut budgeted_bus = TestBus::with_memory(memory.clone());
    let mut legacy_bus = TestBus::with_memory(memory);

    for cap in [1, u64::MAX] {
        let budgeted = budgeted_cpu.run_budgeted(&mut budgeted_bus, cap).unwrap();
        let legacy = legacy_cpu.run_straight_line(&mut legacy_bus, cap).unwrap();
        assert_eq!(budgeted.consumed_core_clocks, legacy.core_clocks);
        assert_eq!(budgeted.halted, legacy.halted);
    }
    assert_eq!(budgeted_cpu, legacy_cpu);
    assert_eq!(budgeted_bus.memory, legacy_bus.memory);
}

#[test]
fn straight_line_hot_loop_matches_per_instruction_result() {
    // MOV CX,5 ; loop: INC AX ; INC AX ; LOOP loop ; HLT. Once the loop body is cached, the
    // relative LOOP can run as a continuation too, so one hot run can chain several iterations.
    let code = [
        0xb9, 0x05, 0x00, // MOV CX, 5
        0x40, // INC AX            (loop target, 0x03)
        0x40, // INC AX            (0x04)
        0xe2, 0xfc, // LOOP -4 -> 0x03  (0x05)
        0xf4, // HLT               (0x07)
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    let runs = drive_straight_line_runs(&mut cpu, &mut bus);
    // Two warming INCs (cache cold) plus four loop iterations of two INCs each = 10.
    assert_eq!(cpu.read_reg16(Reg16::Ax), 10);
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0);
    // 17 instructions retire: MOV CX, 2 warming INCs, then 5 LOOP iterations (the body's 2 INCs
    // run on the four jumping iterations, the fifth LOOP falls through), and HLT =
    // 1 + 2 + (5 LOOP + 4*2 INC) + 1 = 17. A one-instruction-per-run executor would produce 17
    // runs; with cached branch continuations this cold-start case reaches HLT in five runner
    // entries: MOV miss, first INC miss, second INC miss, the hot chained loop, then HLT.
    let retired = 1 + 2 + (5 + 4 * 2) + 1;
    assert!(
        runs < retired,
        "the hot loop must collapse into multi-instruction runs: {runs} runs for {retired} \
             instructions"
    );
    assert_eq!(runs, 5, "cached LOOP should stay inside the hot run");
}

#[test]
fn straight_line_run_executes_hot_register_cached_forms() {
    // MOV AX,1 ; TEST AX,AX ; JNZ target ; MOV BX,dead ; target: MOV CX,2 ; MOV BX,AX ;
    // MOV AX,BX ; DEC CX ; HLT. The warm second run exercises cached continuation fast paths
    // for TEST reg/reg, JNZ, MOV reg,imm, both MOV reg/reg directions, and DEC reg. The skipped
    // MOV proves the branch target, not the contiguous bytes, drives the next continuation.
    let code = [
        0xb8, 0x01, 0x00, // MOV AX, 1
        0x85, 0xc0, // TEST AX, AX
        0x75, 0x03, // JNZ +3 -> 0x0A
        0xbb, 0xad, 0xde, // MOV BX, 0xDEAD (skipped)
        0xb9, 0x02, 0x00, // MOV CX, 2
        0x89, 0xc3, // MOV BX, AX
        0x8b, 0xc3, // MOV AX, BX
        0x49, // DEC CX
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    cpu.registers.set_ebx(0);
    cpu.registers.set_ecx(0);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted, "HLT remains a terminator");
    assert_eq!(cpu.registers.eip, 0x12, "run stopped at HLT");
    assert_eq!(cpu.read_reg16(Reg16::Ax), 1);
    assert_eq!(cpu.read_reg16(Reg16::Bx), 1, "taken JNZ skipped dead MOV");
    assert_eq!(cpu.read_reg16(Reg16::Cx), 1, "MOV imm then DEC ran");
    assert!(!cpu.flag(FLAG_ZF), "DEC CX from 2 to 1 leaves ZF clear");
}

#[test]
fn straight_line_cached_zf_branches_keep_test_flags_lazy() {
    for (opcode, ax) in [(0x74, 0u8), (0x75, 1u8)] {
        // MOV AX,ax ; TEST AX,AX ; JZ/JNZ target ; MOV BX,dead ; target: MOV BX,1234 ; HLT.
        let code = [
            0xb8, ax, 0x00, // MOV AX, ax
            0x85, 0xc0, // TEST AX, AX
            opcode, 0x03, // JZ/JNZ +3 -> target
            0xbb, 0xad, 0xde, // MOV BX, 0xDEAD (skipped)
            0xbb, 0x34, 0x12, // target: MOV BX, 0x1234
            0xf4, // HLT
        ];
        let (mut cpu, memory) = real_mode_cpu(&code, 1024);
        let mut bus = TestBus::with_memory(memory);
        drive_straight_line_runs(&mut cpu, &mut bus);

        cpu.registers.eip = 0;
        cpu.registers.set_eax(0);
        cpu.registers.set_ebx(0);
        cpu.pending_flags = PendingFlags::default();
        cpu.halted = false;
        cpu.reset_perf_counters();

        let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
        assert!(!outcome.halted, "HLT remains the run terminator");
        assert_eq!(cpu.registers.eip, 0x0d, "run stopped at HLT");
        assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234, "branch was taken");
        assert!(
            cpu.pending_flags.tag & (1u32 << 31) != 0,
            "TEST flags should remain deferred after JZ/JNZ reads ZF"
        );
        assert_eq!(
            cpu.perf.flag_materializations, 0,
            "JZ/JNZ should read pending ZF without materializing"
        );
    }
}

#[test]
fn straight_line_run_executes_hot_alu_group_cached_forms() {
    // MOV AX,10 ; MOV BX,3 ; ADD AX,BX ; SUB AX,1 ; CMP AX,12 ; JNZ dead ;
    // OR AL,1 ; XOR AL,1 ; AND AX,0x00ff ; SHL AX,1 ; SHR AX,1 ; HLT. The warm second run
    // exercises cached ALU reg/reg, accumulator immediate, group-1 and group-2 register forms,
    // CMP no-writeback, and flags.
    let code = [
        0xb8, 0x0a, 0x00, // MOV AX, 10
        0xbb, 0x03, 0x00, // MOV BX, 3
        0x01, 0xd8, // ADD AX, BX
        0x83, 0xe8, 0x01, // SUB AX, 1
        0x83, 0xf8, 0x0c, // CMP AX, 12
        0x75, 0x07, // JNZ dead (not taken)
        0x0c, 0x01, // OR AL, 1
        0x34, 0x01, // XOR AL, 1
        0x25, 0xff, 0x00, // AND AX, 0x00ff
        0xd1, 0xe0, // SHL AX, 1
        0xd1, 0xe8, // SHR AX, 1
        0xf4, // HLT
        0xb8, 0xad, 0xde, // dead: MOV AX, 0xDEAD
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    cpu.registers.set_ebx(0);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.eip, 0x1b, "run stopped at HLT");
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x000c);
    assert_eq!(cpu.read_reg16(Reg16::Bx), 3);
    assert!(!cpu.flag(FLAG_ZF), "final AND leaves a nonzero result");
    assert!(!cpu.flag(FLAG_SF));
}

#[test]
fn straight_line_run_executes_hot_memory_alu_group_cached_forms() {
    // MOV SI,0x40 ; MOV AX,4 ; MOV BX,3 ; MOV CL,0x7f ; MOV [SI],AX ; MOV [SI+2],CL ;
    // ADD [SI],BX ; SUB BX,[SI] ; ADD byte [SI+2],1 ; ADD DL,[SI+2] ;
    // ADD word [SI],5 ; CMP word [SI],12 ; JNZ dead ; MOV AX,[SI] ; HLT.
    let code = [
        0xbe, 0x40, 0x00, // MOV SI, 0x40
        0xb8, 0x04, 0x00, // MOV AX, 4
        0xbb, 0x03, 0x00, // MOV BX, 3
        0xb1, 0x7f, // MOV CL, 0x7f
        0x89, 0x04, // MOV [SI], AX
        0x88, 0x4c, 0x02, // MOV [SI+2], CL
        0x01, 0x1c, // ADD [SI], BX
        0x2b, 0x1c, // SUB BX, [SI]
        0x80, 0x44, 0x02, 0x01, // ADD byte [SI+2], 1
        0x02, 0x54, 0x02, // ADD DL, [SI+2]
        0x83, 0x04, 0x05, // ADD word [SI], 5
        0x83, 0x3c, 0x0c, // CMP word [SI], 12
        0x75, 0x03, // JNZ dead (not taken)
        0x8b, 0x04, // MOV AX, [SI]
        0xf4, // HLT
        0xb8, 0xad, 0xde, // dead: MOV AX, 0xDEAD
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    cpu.registers.set_ebx(0);
    cpu.registers.set_ecx(0);
    cpu.registers.set_edx(0);
    cpu.write_reg16(Reg16::Si, 0);
    bus.memory[0x40..0x43].fill(0);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.eip, 0x25, "run stopped at HLT");
    assert_eq!(u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]), 12);
    assert_eq!(bus.memory[0x42], 0x80);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 12);
    assert_eq!(cpu.read_reg16(Reg16::Bx), 0xfffc);
    assert_eq!(cpu.read_reg16(Reg16::Dx), 0x0080);
    assert!(cpu.flag(FLAG_ZF), "CMP equal keeps the dead branch untaken");
}

#[test]
fn straight_line_run_executes_hot_datamove_cached_forms() {
    // MOV AX,0x00fe ; MOV BX,0x1234 ; MOV DI,4 ; MOVSX CX,AL ; MOVZX DX,BL ;
    // XCHG AX,BX ; LEA SI,[BX+DI+5] ; HLT.
    let code = [
        0xb8, 0xfe, 0x00, // MOV AX, 0x00fe
        0xbb, 0x34, 0x12, // MOV BX, 0x1234
        0xbf, 0x04, 0x00, // MOV DI, 4
        0x0f, 0xbe, 0xc8, // MOVSX CX, AL
        0x0f, 0xb6, 0xd3, // MOVZX DX, BL
        0x93, // XCHG AX, BX
        0x8d, 0x71, 0x05, // LEA SI, [BX+DI+5]
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    cpu.registers.set_ebx(0);
    cpu.registers.set_ecx(0);
    cpu.registers.set_edx(0);
    cpu.write_reg16(Reg16::Si, 0);
    cpu.write_reg16(Reg16::Di, 0);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.eip, 0x13, "run stopped at HLT");
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1234);
    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x00fe);
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0xfffe);
    assert_eq!(cpu.read_reg16(Reg16::Dx), 0x0034);
    assert_eq!(cpu.read_reg16(Reg16::Si), 0x0107);
}

#[test]
fn straight_line_run_executes_hot_datamove_memory_cached_forms() {
    // MOV SI,0x40 ; MOV AX,0x1234 ; MOV [SI],AX ; MOV BX,[SI] ;
    // MOV CL,0x7f ; MOV [SI+2],CL ; MOV DL,[SI+2] ; HLT.
    let code = [
        0xbe, 0x40, 0x00, // MOV SI, 0x40
        0xb8, 0x34, 0x12, // MOV AX, 0x1234
        0x89, 0x04, // MOV [SI], AX
        0x8b, 0x1c, // MOV BX, [SI]
        0xb1, 0x7f, // MOV CL, 0x7f
        0x88, 0x4c, 0x02, // MOV [SI+2], CL
        0x8a, 0x54, 0x02, // MOV DL, [SI+2]
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    cpu.registers.set_ebx(0);
    cpu.registers.set_ecx(0);
    cpu.registers.set_edx(0);
    cpu.write_reg16(Reg16::Si, 0);
    bus.memory[0x40..0x43].fill(0);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.eip, 0x12, "run stopped at HLT");
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]),
        0x1234
    );
    assert_eq!(bus.memory[0x42], 0x7f);
    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x007f);
    assert_eq!(cpu.read_reg16(Reg16::Dx), 0x007f);
}

#[test]
fn straight_line_run_executes_hot_flags_misc_cached_forms() {
    // MOV SI,0x40 ; MOV AX,0x8001 ; MOV [SI],AX ; TEST [SI],AX ;
    // MOV AL,0x80 ; CBW ; CWD ; CLC ; STC ; CMC ; CLD ; STD ;
    // MOV AH,0xd7 ; SAHF ; LAHF ; HLT.
    let code = [
        0xbe, 0x40, 0x00, // MOV SI, 0x40
        0xb8, 0x01, 0x80, // MOV AX, 0x8001
        0x89, 0x04, // MOV [SI], AX
        0x85, 0x04, // TEST [SI], AX
        0xb0, 0x80, // MOV AL, 0x80
        0x98, // CBW
        0x99, // CWD
        0xf8, // CLC
        0xf9, // STC
        0xf5, // CMC
        0xfc, // CLD
        0xfd, // STD
        0xb4, 0xd7, // MOV AH, 0xd7
        0x9e, // SAHF
        0x9f, // LAHF
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    cpu.registers.set_edx(0);
    cpu.write_reg16(Reg16::Si, 0);
    cpu.registers.eflags = 0x02;
    cpu.pending_flags = PendingFlags::default();
    bus.memory[0x40..0x42].fill(0);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.eip, 0x17, "run stopped at HLT");
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]),
        0x8001
    );
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xd780);
    assert_eq!(cpu.read_reg16(Reg16::Dx), 0xffff);
    assert_eq!(cpu.eflags(), 0x4d7);
}

#[test]
fn straight_line_run_executes_hot_stack_cached_forms() {
    // MOV AX,0x1234 ; PUSH AX ; POP BX ; PUSH 0x55aa ; POP CX ; PUSH -1 ; POP DX ; HLT.
    // The warm second run keeps stack memory access on the existing push/pop helpers while
    // skipping the decoded stack dispatch for register and immediate forms.
    let code = [
        0xb8, 0x34, 0x12, // MOV AX, 0x1234
        0x50, // PUSH AX
        0x5b, // POP BX
        0x68, 0xaa, 0x55, // PUSH 0x55aa
        0x59, // POP CX
        0x6a, 0xff, // PUSH -1
        0x5a, // POP DX
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    cpu.write_reg16(Reg16::Sp, 0x0200);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    cpu.registers.set_ebx(0);
    cpu.registers.set_ecx(0);
    cpu.registers.set_edx(0);
    cpu.write_reg16(Reg16::Sp, 0x0200);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.eip, 0x0c, "run stopped at HLT");
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1234);
    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x55aa);
    assert_eq!(cpu.read_reg16(Reg16::Dx), 0xffff);
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x0200);
}

#[test]
fn straight_line_run_sees_self_modified_later_instruction() {
    // The key correctness property of the lean executor: a guest write that modifies a later,
    // already-cached instruction must make the NEXT continuation re-decode the new bytes, never
    // replay the stale cached opcode. We loop so the body is cached, then on the second iteration
    // an early store overwrites a later instruction's opcode in place.
    //
    // Layout (DS = CS = 0):
    //   0x00: B9 02 00        MOV CX, 2
    //   loop (0x03):
    //   0x03: C6 06 0A 00 48  MOV byte [0x0A], 0x48   ; patch the op at 0x0A to DEC AX (0x48)
    //   0x08: 40              INC AX
    //   0x09: 40              INC AX                  ; cached as INC AX on pass 1, runs DEC on 2
    //   0x0A: 40              INC AX  <- patched to 0x48 (DEC AX) by the store at 0x03
    //   0x0B: E2 F6           LOOP -10 -> 0x03
    //   0x0D: F4              HLT
    //
    // Pass 1 (cache cold): the store writes 0x48 over [0x0A] BEFORE 0x0A is ever decoded, so 0x0A
    //   decodes fresh as DEC AX. AX: +1 (0x08) +1 (0x09) -1 (0x0A) = +1.
    // Pass 2 (body cached): 0x0A is now cached as DEC AX from pass 1. The store rewrites the same
    //   0x48, hitting a cached code byte -> generation bump -> the 0x0A continuation re-decodes
    //   (still DEC AX). AX: +1 +1 -1 = +1. Total AX = 2, CX = 0.
    // If the executor replayed the stale cache without honoring the SMC bump, 0x0A would run as
    //   the original INC and AX would be wrong.
    let code = [
        0xb9, 0x02, 0x00, // MOV CX, 2
        0xc6, 0x06, 0x0a, 0x00, 0x48, // MOV byte [0x000A], 0x48   (0x03)
        0x40, // INC AX                                  (0x08)
        0x40, // INC AX                                  (0x09)
        0x40, // INC AX  (patched to DEC AX at runtime)  (0x0A)
        0xe2, 0xf6, // LOOP -10 -> 0x03                  (0x0B)
        0xf4, // HLT                                     (0x0D)
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);
    // Each of the two iterations nets +1 because the patched op ran as DEC AX, not the stale INC.
    assert_eq!(cpu.read_reg16(Reg16::Ax), 2);
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0);
    // The byte at 0x0A is the patched DEC AX opcode.
    assert_eq!(bus.memory[0x0a], 0x48);
}

#[test]
fn straight_line_run_discards_cached_opcode_overwritten_to_a_different_op() {
    // The strongly discriminating SMC case: a later instruction is cached as one opcode (INC AX,
    // a +1), an earlier guest store then overwrites it with a DIFFERENT opcode (DEC AX, a -1), and
    // the executor must re-decode the new opcode rather than replay the stale cached form. Because
    // the cached form (INC, +1) and the rewritten form (DEC, -1) have OPPOSITE effects, a stale
    // snapshot replay produces the wrong sign and the assertion fails - unlike a rewrite to the
    // already-cached value, which a stale replay would pass.
    //
    // Layout (DS = CS = SS = 0):
    //   0x00: C6 06 05 00 48   MOV byte [0x05], 0x48   ; store: patch P (0x05) from 0x40 to 0x48
    //   P = 0x05: 40           INC AX                  ; cached as INC AX first; 0x48 = DEC AX after
    //   0x06: F4               HLT
    let code = [
        0xc6, 0x06, 0x05, 0x00, 0x48, // MOV byte [0x0005], 0x48
        0x40, // INC AX  (P, patched to 0x48 = DEC AX at runtime)
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);

    // Cache P as INC AX BEFORE any rewrite: run a single instruction starting at P so it decodes
    // and caches as INC AX (0x40). This is the cached form a stale replay would later wrongly use.
    cpu.registers.eip = 0x05;
    cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(
        cpu.decode_cache.get(0x05, false).is_some(),
        "P must be cached as INC AX before the rewrite"
    );
    assert_eq!(cpu.read_reg16(Reg16::Ax), 1, "the warm INC ran once");

    // Now run from the top: the store overwrites P's opcode byte (0x40 -> 0x48). That write hits a
    // cached code byte, bumping the decode-cache generation, so when control reaches P it
    // re-decodes the NEW byte (DEC AX) instead of replaying the cached INC.
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    drive_straight_line_runs(&mut cpu, &mut bus);
    // DEC AX ran (AX = 0xFFFF). A stale-snapshot executor that replayed the cached INC AX would
    // leave AX = 0x0001, failing this assertion - that is the discrimination the L1 test lacked.
    assert_eq!(
        cpu.read_reg16(Reg16::Ax),
        0xffff,
        "the rewritten DEC AX must run, not the stale cached INC AX"
    );
    assert_eq!(
        bus.memory[0x05], 0x48,
        "P's opcode byte was patched to DEC AX"
    );
}

#[test]
fn straight_line_run_stops_at_a_page_crossing_instruction() {
    // The continuation rule `(lin & 0xfff) + len <= 0x1000` keeps a run from executing a cached
    // instruction that would straddle a 4 KB page boundary; that instruction must run through the
    // normal path instead. This exercises BOTH sides of the `<= 0x1000` bound:
    //   - an instruction ENDING exactly at 0x1000 is allowed and runs as a continuation;
    //   - an instruction CROSSING 0x1000 ends the run and runs afterward via the normal path.
    // Real-mode flat layout (CS base 0, so lin == eip). The probe instruction is MOV AL, 7
    // (0xB0 0x07), a 2-byte straight-line DataMove with an observable effect (AL = 7).

    // Case A: the probe begins at 0xFFE and ends at 0x1000 (0xFFE + 2 == 0x1000) -> ALLOWED.
    //   0xFFD: 40         INC AX
    //   0xFFE: B0 07       MOV AL, 7   (ends exactly at 0x1000)
    //   0x1000: F4         HLT
    {
        let mut memory = vec![0u8; 0x2000];
        memory[0xffd] = 0x40; // INC AX
        memory[0xffe] = 0xb0; // MOV AL,
        memory[0xfff] = 0x07; //   7
        memory[0x1000] = 0xf4; // HLT
        let mut cpu = CpuGsw::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        let mut bus = TestBus::with_memory(memory);

        // Warm the decode cache for all three instructions so the only thing gating a continuation
        // is the page check, not a cache miss.
        cpu.registers.eip = 0xffd;
        drive_straight_line_runs(&mut cpu, &mut bus);

        // Run once from 0xFFD: INC is the run's first instruction, then the cached MOV AL,7 (ending
        // exactly at 0x1000) runs as a continuation in the SAME run.
        cpu.registers.eip = 0xffd;
        cpu.registers.set_eax(0);
        let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
        assert!(!outcome.halted);
        // The MOV ran as a continuation: AL == 7 and the run advanced past it (eip past 0x1000 is
        // the HLT, which ends the run as a non-straight-line group, leaving eip at 0x1000).
        assert_eq!(
            cpu.read_reg16(Reg16::Ax) & 0xff,
            7,
            "MOV AL,7 ending at 0x1000 must run"
        );
        assert_eq!(
            cpu.registers.eip, 0x1000,
            "the run reached the HLT after the MOV"
        );
    }

    // Case B: the probe begins at 0xFFF and ends at 0x1001 (0xFFF + 2 > 0x1000) -> CROSSES.
    //   0xFFE: 40         INC AX
    //   0xFFF: B0 07       MOV AL, 7   (straddles the page boundary)
    //   0x1001: F4         HLT
    {
        let mut memory = vec![0u8; 0x2000];
        memory[0xffd] = 0x40; // INC AX (warm anchor)
        memory[0xffe] = 0x40; // INC AX
        memory[0xfff] = 0xb0; // MOV AL,
        memory[0x1000] = 0x07; //   7  (this byte is on the next page)
        memory[0x1001] = 0xf4; // HLT
        let mut cpu = CpuGsw::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        let mut bus = TestBus::with_memory(memory);

        // Warm the page-local instructions. The crossing MOV must remain uncached because its
        // second linear page could map to a noncontiguous physical page under paging.
        cpu.registers.eip = 0xffd;
        drive_straight_line_runs(&mut cpu, &mut bus);
        assert!(
            cpu.decode_cache.get(0xfff, false).is_none(),
            "the page-crossing MOV must not enter the decode cache"
        );
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        {
            // The page-local instructions are cached, so their bytes are watched outright.
            assert!(cpu.decode_cache.native_code_watch.is_watched(0x0ffd));
            assert!(cpu.decode_cache.native_code_watch.is_watched(0x0ffe));
            assert!(cpu.decode_cache.native_code_watch.is_watched(0x1001));

            // The crossing MOV's own two bytes are a different matter: nothing caches them, so
            // they are watched only when a granule wider than one byte SPILLS onto them from a
            // cached neighbour. This used to assert both unconditionally, which passed on the
            // accident of a 16-byte granule and would have kept passing for the wrong reason.
            // Derive the expectation from the constant instead.
            let granule = 1u32 << crate::jit::code_watch::NATIVE_CHUNK_SHIFT;
            let granule_base = |physical: u32| physical & !(granule - 1);
            assert_eq!(
                cpu.decode_cache.native_code_watch.is_watched(0x0fff),
                granule_base(0x0fff) == granule_base(0x0ffe),
                "the MOV's opcode byte is watched exactly when it shares a granule with the INC"
            );
            assert_eq!(
                cpu.decode_cache.native_code_watch.is_watched(0x1000),
                granule_base(0x1000) == granule_base(0x1001),
                "the MOV's immediate is watched exactly when it shares a granule with the HLT"
            );
        }

        // Run once from 0xFFD: INC (0xFFD, first) + INC (0xFFE, continuation) run, then the MOV
        // misses because it is page-straddling and the run stops there.
        cpu.registers.eip = 0xffd;
        cpu.registers.set_eax(0);
        let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
        assert!(!outcome.halted);
        assert_eq!(cpu.read_reg16(Reg16::Ax), 2, "the two INCs ran");
        assert_eq!(
            cpu.read_reg16(Reg16::Ax) & 0xff,
            2,
            "the page-crossing MOV did NOT run in this call (AL is not 7)"
        );
        assert_eq!(
            cpu.registers.eip, 0xfff,
            "the run stopped at the page-crossing MOV"
        );

        // The crossing MOV runs correctly afterward through the normal path (first instruction of
        // the next run, not subject to the continuation page check).
        let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
        assert!(!outcome.halted);
        assert_eq!(
            cpu.read_reg16(Reg16::Ax) & 0xff,
            7,
            "MOV AL,7 ran via the normal path"
        );
        assert_eq!(
            cpu.registers.eip, 0x1001,
            "eip advanced past the crossing MOV"
        );
        assert!(cpu.decode_cache.get(0xfff, false).is_none());

        bus.memory[0x1000] = 9;
        cpu.registers.eip = 0xfff;
        cpu.registers.set_eax(0);
        let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
        assert!(!outcome.halted);
        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 9);
        assert!(cpu.decode_cache.get(0xfff, false).is_none());
    }
}

#[test]
fn straight_line_run_faults_on_cached_continuation_keeping_earlier_effects() {
    // A fault raised by a CACHED straight-line instruction running as a continuation
    // (run_one_cached) must route through the SAME tail the per-instruction path uses: a
    // delivered #DE (divide-by-zero) retargets CS:IP at the guest's own IVT handler, and the
    // earlier straight-line instruction's effects are kept. DIV is data-dependent, so it can
    // be cached with a good divisor and then fault on a later run with a zero divisor -
    // exactly the case where the faulting instruction is a valid cache hit (a delivered IVT
    // exception reloads CS and flushes the cache, so a register-input fault -- not a decode
    // change -- is the way to reach the cached-continuation path).
    //
    //   0x10: 40           INC AX     ; straight-line, runs before the DIV in the same run
    //   0x11: F6 F3        DIV BL     ; AX / BL ; #DE when BL = 0
    //   0x13: F4           HLT
    //
    // Code starts at 0x10 (not 0x00) so it does not overlap the real-mode IVT's vector-0 slot
    // (bytes 0..4), which this test populates with a trap handler address.
    const ORIGIN: usize = 0x10;
    let code = [
        0x40, // INC AX
        0xf6, 0xf3, // DIV BL
        0xf4, // HLT
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&[], 0x1_0000);
    memory[ORIGIN..ORIGIN + code.len()].copy_from_slice(&code);
    memory[0..2].copy_from_slice(&DE_TRAP_IP.to_le_bytes());
    memory[2..4].copy_from_slice(&DE_TRAP_CS.to_le_bytes());
    cpu.registers.eip = ORIGIN as u32;
    cpu.registers.set_esp(0x2000);
    let mut bus = TestBus::with_memory(memory);

    // Warming pass with a good divisor: AX = 11, BL = 2. This caches both INC and DIV in the live
    // generation WITHOUT any fault (no CS reload, so the decode cache stays valid).
    cpu.registers.set_eax(11);
    cpu.write_reg16(Reg16::Bx, 0x0002);
    drive_straight_line_runs(&mut cpu, &mut bus);
    assert!(
        cpu.decode_cache.get(ORIGIN as u32 + 1, false).is_some(),
        "DIV must be cached after the warming pass"
    );

    // Now poke the divisor to 0 and run from the top: INC is the run's first instruction, then the
    // CACHED DIV runs as a straight-line continuation and delivers #DE.
    cpu.registers.eip = ORIGIN as u32;
    cpu.registers.set_eax(10);
    cpu.write_reg16(Reg16::Bx, 0x0000);
    let outcome = cpu
        .run_straight_line(&mut bus, u64::MAX)
        .expect("the cached DIV continuation must deliver #DE, not error the run");
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.cs().selector, DE_TRAP_CS);
    assert_eq!(cpu.registers.eip, u32::from(DE_TRAP_IP));
    // INC AX ran before the fault and its effect is kept (AX = 11).
    assert_eq!(cpu.read_reg16(Reg16::Ax), 11);
}

#[test]
fn straight_line_run_executes_cached_relative_jump_continuation() {
    // A cached relative JMP is safe to run as a continuation: it only changes EIP, and the next
    // continuation lookup uses that live target rather than falling through into skipped bytes.
    //
    //   0x00: 40           INC AX
    //   0x01: 40           INC AX
    //   0x02: EB 02        JMP +2 -> 0x06
    //   0x04: 40           INC AX   (skipped by the jump)
    //   0x05: 40           INC AX   (skipped)
    //   0x06: F4           HLT
    let code = [
        0x40, // INC AX
        0x40, // INC AX
        0xeb, 0x02, // JMP +2 -> 0x06
        0x40, // INC AX (skipped)
        0x40, // INC AX (skipped)
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted, "the HLT target is still a terminator");
    assert_eq!(cpu.read_reg16(Reg16::Ax), 2);
    assert_eq!(
        cpu.registers.eip, 0x06,
        "the cached JMP ran and skipped to the HLT target"
    );
}

#[test]
fn straight_line_run_executes_cached_near_call_continuation() {
    // CALL near is a relative branch plus a normal stack push, and near RET is now a
    // continuable near transfer too, so the warm run chains CALL -> body -> RET -> return
    // site all the way to the HLT.
    //
    //   0x00: B8 01 00     MOV AX, 1
    //   0x03: E8 03 00     CALL 0x09        ; return address 0x06
    //   0x06: 40           INC AX           ; return site, reached through the chained RET
    //   0x07: F4           HLT
    //   0x08: 90           NOP
    //   0x09: 40           INC AX           ; subroutine
    //   0x0A: C3           RET              ; chained continuation
    let code = [
        0xb8, 0x01, 0x00, // MOV AX, 1
        0xe8, 0x03, 0x00, // CALL +3 -> 0x09
        0x40, // INC AX (return site)
        0xf4, // HLT
        0x90, // NOP
        0x40, // INC AX (subroutine)
        0xc3, // RET
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    cpu.write_reg16(Reg16::Sp, 0x0200);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    bus.memory[0x01fe..0x0200].fill(0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.write_reg16(Reg16::Sp, 0x0200);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted, "the run stops AT the HLT terminator");
    assert_eq!(
        cpu.read_reg16(Reg16::Ax),
        3,
        "subroutine and return site both ran"
    );
    assert_eq!(
        cpu.read_reg16(Reg16::Sp),
        0x0200,
        "RET released the return address"
    );
    assert_eq!(
        cpu.registers.eip, 0x07,
        "cached CALL chained through the RET back to the return site up to the HLT"
    );
}

#[test]
fn straight_line_run_chains_a_near_ret_procedure_in_one_run() {
    // A warm CALL rel16 -> body -> near RET procedure executes as
    // ONE run (no brk[branch] break at the RET), proven via the run/break perf counters.
    //
    //   0x00: B9 02 00     MOV CX, 2
    //   0x03: E8 05 00     CALL 0x0B        ; return address 0x06
    //   0x06: 49           DEC CX
    //   0x07: 75 FA        JNZ 0x03         ; call again
    //   0x09: F4           HLT
    //   0x0A: 90           NOP
    //   0x0B: 40           INC AX           ; body
    //   0x0C: C3           RET
    let code = [
        0xb9, 0x02, 0x00, // MOV CX, 2
        0xe8, 0x05, 0x00, // CALL +5 -> 0x0B
        0x49, // DEC CX
        0x75, 0xfa, // JNZ -6 -> 0x03
        0xf4, // HLT
        0x90, // NOP
        0x40, // INC AX (body)
        0xc3, // RET
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    cpu.write_reg16(Reg16::Sp, 0x0200);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.write_reg16(Reg16::Cx, 0);
    cpu.write_reg16(Reg16::Sp, 0x0200);
    cpu.halted = false;
    cpu.reset_perf_counters();
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    // The run chains both CALL -> body -> RET round trips and stops only at the HLT
    // (HLT is Misc, still a terminator that runs on the next runner entry).
    assert!(
        !outcome.halted,
        "the run stops AT the HLT, which runs next entry"
    );
    assert_eq!(cpu.read_reg16(Reg16::Ax), 2, "the body ran on both calls");
    assert_eq!(
        cpu.read_reg16(Reg16::Sp),
        0x0200,
        "both RETs released their frames"
    );
    assert_eq!(cpu.registers.eip, 0x09, "one run reached the HLT");
    let p = cpu.perf_counters();
    assert_eq!(
        p.straight_line_runs, 1,
        "one runner entry covered the whole procedure"
    );
    assert_eq!(
        p.brk_decode_or_branch, 1,
        "only the HLT terminator broke the run; the near RETs chained (eip pins that \
             the break was at 0x09, past both RETs)"
    );
    assert_eq!(p.brk_halt, 0, "HLT was not executed inside the run");
}

#[test]
fn straight_line_run_still_breaks_at_far_ret() {
    // The contrast case: far RET loads CS, so it stays a run terminator even warm.
    //
    //   0x00: 40           INC AX
    //   0x01: CB           RETF       ; stack target 0000:0006
    //   0x02: 40 40 40 90  (skipped)
    //   0x06: F4           HLT
    let code = [
        0x40, // INC AX
        0xcb, // RETF
        0x40, 0x40, 0x40, 0x90, // skipped
        0xf4, // HLT
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 1024);
    memory[0x100..0x104].copy_from_slice(&[0x06, 0x00, 0x00, 0x00]); // 0000:0006
    cpu.write_reg16(Reg16::Sp, 0x0100);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    bus.memory[0x100..0x104].copy_from_slice(&[0x06, 0x00, 0x00, 0x00]);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.write_reg16(Reg16::Sp, 0x0100);
    cpu.halted = false;
    cpu.reset_perf_counters();
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 1);
    assert_eq!(
        cpu.registers.eip, 0x01,
        "far RET must not run as a continuation"
    );
    assert_eq!(
        cpu.perf_counters().brk_decode_or_branch,
        1,
        "the warm run broke at the cached RETF"
    );
}

#[test]
fn straight_line_run_chains_rep_movs_mid_run() {
    // A REP MOVS mid-block runs as a continuation: the whole warm block (setup, the
    // atomic REP, and the instruction after it) is ONE runner entry ending at the HLT.
    //
    //   0x00: B9 03 00     MOV CX, 3
    //   0x03: BE 40 00     MOV SI, 0x40
    //   0x06: BF 60 00     MOV DI, 0x60
    //   0x09: F3 A4        REP MOVSB
    //   0x0B: 40           INC AX          ; still inside the same run
    //   0x0C: F4           HLT
    let code = [
        0xb9, 0x03, 0x00, // MOV CX, 3
        0xbe, 0x40, 0x00, // MOV SI, 0x40
        0xbf, 0x60, 0x00, // MOV DI, 0x60
        0xf3, 0xa4, // REP MOVSB
        0x40, // INC AX
        0xf4, // HLT
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 1024);
    memory[0x40..0x43].copy_from_slice(b"abc");
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    bus.memory[0x60..0x63].fill(0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.write_reg16(Reg16::Cx, 0);
    cpu.halted = false;
    cpu.reset_perf_counters();
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted, "the run stops AT the HLT terminator");
    assert_eq!(&bus.memory[0x60..0x63], b"abc", "the REP MOVS copied");
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0, "the repeat ran to exhaustion");
    assert_eq!(cpu.read_reg16(Reg16::Ax), 1, "the post-REP INC chained");
    assert_eq!(cpu.registers.eip, 0x0c, "one run reached the HLT");
    let p = cpu.perf_counters();
    assert_eq!(
        p.straight_line_runs, 1,
        "one runner entry covered the block"
    );
    assert_eq!(
        p.brk_decode_or_branch, 1,
        "only the HLT terminator broke the run; the REP MOVS chained"
    );
}

#[test]
fn ignored_rep_prefix_uses_the_non_yielding_cached_path() {
    let code = [
        0xb8, 0x00, 0x00, // MOV AX, 0
        0xf3, 0x40, // REP INC AX; REP is ignored
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0xffff);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();

    assert!(!outcome.halted);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 1);
    assert_eq!(cpu.registers.eip, 5);
    assert!(!cpu.rep_resume_active);
    assert!(cpu.rep_execution.resume.is_none());
}

#[test]
fn straight_line_run_still_breaks_at_string_port_io() {
    // OUTSB (0x6E, Misc group) touches a port, so it must never run as a continuation
    // even warm: the run breaks at the gate and OUTSB runs on the next runner entry.
    //
    //   0x00: 40           INC AX
    //   0x01: 6E           OUTSB      ; must not run as a continuation
    //   0x02: F4           HLT
    let code = [
        0x40, // INC AX
        0x6e, // OUTSB
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    bus.io_touched = false; // clear the warm drive's port-touch step-break latch
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.halted = false;
    cpu.reset_perf_counters();
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 1);
    assert_eq!(
        cpu.registers.eip, 0x01,
        "OUTSB must not run as a continuation"
    );
    assert_eq!(
        cpu.perf_counters().brk_decode_or_branch,
        1,
        "the warm run broke at the cached OUTSB"
    );
}

#[test]
fn straight_line_run_continues_push_rm_but_breaks_far_indirect() {
    // The 0xFF split: /6 PUSH r/m is a plain fall-through form and chains; /3 far
    // indirect CALL loads CS and stays a terminator.
    //
    //   0x00: 40           INC AX
    //   0x01: FF 36 40 00  PUSH word [0x0040]
    //   0x05: 40           INC AX
    //   0x06: F4           HLT
    let push_code = [
        0x40, // INC AX
        0xff, 0x36, 0x40, 0x00, // PUSH word [0x0040]
        0x40, // INC AX
        0xf4, // HLT
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&push_code, 1024);
    memory[0x40..0x42].copy_from_slice(&0xbeefu16.to_le_bytes());
    cpu.write_reg16(Reg16::Sp, 0x0200);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.write_reg16(Reg16::Sp, 0x0200);
    cpu.halted = false;
    cpu.reset_perf_counters();
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(
        cpu.read_reg16(Reg16::Ax),
        2,
        "both INCs chained past the PUSH"
    );
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01fe);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x01fe], bus.memory[0x01ff]]),
        0xbeef,
        "PUSH r/m ran as a continuation"
    );
    assert_eq!(cpu.registers.eip, 0x06, "one run reached the HLT");
    assert_eq!(cpu.perf_counters().straight_line_runs, 1);
    assert_eq!(
        cpu.perf_counters().brk_decode_or_branch,
        1,
        "only the HLT broke"
    );

    //   0x00: 40           INC AX
    //   0x01: FF 1E 40 00  CALL FAR [0x0040]   ; m16:16 -> 0000:0008
    //   0x05: 40 40 90     (not reached in the warm run)
    //   0x08: F4           HLT
    let far_code = [
        0x40, // INC AX
        0xff, 0x1e, 0x40, 0x00, // CALL FAR [0x0040]
        0x40, 0x40, 0x90, // filler
        0xf4, // HLT
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&far_code, 1024);
    memory[0x40..0x44].copy_from_slice(&[0x08, 0x00, 0x00, 0x00]); // 0000:0008
    cpu.write_reg16(Reg16::Sp, 0x0200);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.write_reg16(Reg16::Sp, 0x0200);
    cpu.halted = false;
    cpu.reset_perf_counters();
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 1);
    assert_eq!(
        cpu.registers.eip, 0x01,
        "far indirect CALL must not run as a continuation"
    );
    assert_eq!(
        cpu.perf_counters().brk_decode_or_branch,
        1,
        "the warm run broke at the cached 0xFF /3"
    );
}

#[test]
fn straight_line_run_never_executes_an_int_after_a_taken_branch() {
    // Regression guard against the "recompiler executes non-executed code" claim: a
    // side-effecting instruction (INT 0x13) sitting in the contiguous bytes AFTER a taken
    // branch must NEVER be dispatched, even after the decode cache is warm. The cached JMP here
    // makes EIP skip the INT entirely, so the executed-INT trace must stay empty for vector 0x13.
    //
    //   0x00: 40           INC AX
    //   0x01: 40           INC AX
    //   0x02: EB 02        JMP +2 -> 0x06        (taken branch over the INT)
    //   0x04: CD 13        INT 0x13              (contiguous bytes; must NEVER run)
    //   0x06: F4           HLT
    let code = [
        0x40, // INC AX
        0x40, // INC AX
        0xeb, 0x02, // JMP +2 -> 0x06 (skips the INT 0x13)
        0xcd, 0x13, // INT 0x13 (must never execute)
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);

    // First drive: warms the decode cache (the INC/INC/JMP block becomes a cached run) and runs
    // to the HLT. A warm cache is exactly the condition under which an over-read of trailing
    // bytes would surface, so this is the case the claim must be tested against.
    drive_straight_line_runs(&mut cpu, &mut bus);
    assert_eq!(
        cpu.read_reg16(Reg16::Ax),
        2,
        "only the two pre-JMP INCs ran"
    );
    assert_eq!(cpu.registers.eip, 0x07, "control reached the HLT at 0x06");

    // Re-arm and drive again from the top with the cache now hot, to be sure a cached relative
    // branch continuation still targets the HLT rather than over-reading into the INT bytes.
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.halted = false;
    drive_straight_line_runs(&mut cpu, &mut bus);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 2);

    // The decisive assertion: across both drives, NO software interrupt was ever acknowledged
    // for vector 0x13. `software_interrupt` is the single dispatch point for an executed INT n,
    // and it always calls `bus.interrupt_acknowledge(vector, ..)`, which the TestBus records as
    // an InterruptAcknowledge cycle. An empty result proves the post-branch INT bytes are inert.
    let executed_int13 = bus
        .trace
        .cycles()
        .iter()
        .filter(|c| c.kind == BusAccessKind::InterruptAcknowledge && c.address == 0x13)
        .count();
    assert_eq!(
        executed_int13, 0,
        "the straight-line executor dispatched INT 0x13 from bytes after a taken branch; \
             this would be a genuine over-read of non-executed code"
    );
}

#[test]
fn straight_line_run_ends_on_port_io_step_break() {
    // A port access (OUT) touches time-dependent device state, so the old per-instruction machine
    // loop ended the step immediately after it (io_touched). The executor must do the same via
    // bus.requires_step_break(): an OUT as the run's FIRST instruction ends the run after that one
    // instruction, so the following straight-line INC does NOT run in the same call. Without the
    // step-break the executor would keep going and the device boundary would drift.
    //
    //   0x00: E6 80   OUT 0x80, AL   ; PortIo -> sets io_touched
    //   0x02: 40      INC AX         ; must NOT run in the same run as the OUT
    //   0x03: F4      HLT
    let code = [
        0xe6, 0x80, // OUT 0x80, AL
        0x40, // INC AX
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    // eip advanced past only the OUT (2 bytes); the run broke before the INC.
    assert_eq!(cpu.registers.eip, 0x02);
    // The INC did not run in this call, so AX is unchanged.
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0);
    assert!(bus.io_touched, "the OUT must have touched device I/O");
}
