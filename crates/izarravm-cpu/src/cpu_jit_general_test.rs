// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Real mode with a 32-bit code segment (flat, 64 KB limit), at the 586 level so the FP
/// timing classes are non-identity and `fp_rem` actually carries.
fn fresh() -> CpuGsw {
    fresh_for(GswMode::Gsw586)
}

fn fresh_for(mode: GswMode) -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(mode);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    let mut cs = cpu.registers.cs();
    cs.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    cpu
}

fn drive_to_halt(cpu: &mut CpuGsw, bus: &mut TestBus) {
    for _ in 0..10_000 {
        if cpu.run_straight_line(bus, u64::MAX).unwrap().halted {
            return;
        }
    }
    panic!("guest never halted");
}

// ---- 1. Four-accumulator identity on an x87-containing loop ----

const X87_START: u32 = 0x100;
const X87_LOOP: u32 = 0x101;
const X87_COUNT: usize = 0x400;

/// NOP starter, then a self-loop mixing an ALU op, a memory store, two x87 memory ops (the
/// IntConvert32 class, x34 at 586, so `fp_rem` carries hard) and an FNINIT (Register class,
/// x0.25) so the block spans two FP classes, a memory-counter DEC, and the rel8 back-edge.
/// FNINIT each iteration keeps the x87 stack balanced regardless of the reset FPU state.
fn x87_program() -> Vec<u8> {
    let mut m = vec![0u8; 0x1000];
    m[X87_START as usize] = 0x90; // nop starter, so X87_LOOP is reached as a continuation
    let body: [u8; 18] = [
        0xdb, 0xe3, // fninit                 (Register class)
        0xdb, 0x06, // fild dword [esi]       (IntConvert32)
        0xdb, 0x1f, // fistp dword [edi]      (IntConvert32)
        0x83, 0xc3, 0x03, // add ebx,3        (ALU)
        0x89, 0x5f, 0x04, // mov [edi+4],ebx  (memory store)
        0xff, 0x0d, 0x00, 0x04, 0x00, 0x00, // dec dword [X87_COUNT] (memory RMW)
    ];
    m[X87_LOOP as usize..X87_LOOP as usize + body.len()].copy_from_slice(&body);
    let jnz_at = X87_LOOP as usize + body.len();
    let rel = (X87_LOOP as i32 - (jnz_at as i32 + 2)) as i8;
    m[jnz_at] = 0x75; // jnz X87_LOOP
    m[jnz_at + 1] = rel as u8;
    m[jnz_at + 2] = 0xf4; // hlt at the loop fall-through
    m[0x300..0x304].copy_from_slice(&1234u32.to_le_bytes()); // the int fild reads
    m
}

fn x87_arm(cpu: &mut CpuGsw, bus: &mut TestBus, count: u32) {
    cpu.registers.eip = X87_START;
    cpu.registers.set_esp(0x0700);
    cpu.registers.set_eax(0);
    cpu.registers.set_ebx(0);
    cpu.registers.set_esi(0x300);
    cpu.registers.set_edi(0x310);
    bus.memory[X87_COUNT..X87_COUNT + 4].copy_from_slice(&count.to_le_bytes());
}

fn count_of(bus: &TestBus) -> u32 {
    u32::from_le_bytes(bus.memory[X87_COUNT..X87_COUNT + 4].try_into().unwrap())
}

/// Drive until the memory counter hits zero (the loop fall-through), which is BEFORE the
/// trailing HLT is executed as a fresh run's first instruction (that would reset
/// `core_clocks_so_far`). This is the point at which the four accumulators are meaningfully
/// compared.
fn drive_until_count_zero(cpu: &mut CpuGsw, bus: &mut TestBus) {
    for _ in 0..10_000 {
        let out = cpu.run_straight_line(bus, u64::MAX).unwrap();
        if count_of(bus) == 0 || out.halted {
            return;
        }
    }
    panic!("counter never reached zero");
}

#[test]
fn general_block_four_accumulator_identity() {
    let mut interp = fresh();
    let mut jit_cpu = fresh();
    let mut bus_i = TestBus::with_memory(x87_program());
    let mut bus_j = TestBus::with_memory(x87_program());

    // Warm both identically (fills the decode cache), then admit the loop on the jit CPU.
    x87_arm(&mut interp, &mut bus_i, 2);
    x87_arm(&mut jit_cpu, &mut bus_j, 2);
    drive_to_halt(&mut interp, &mut bus_i);
    drive_to_halt(&mut jit_cpu, &mut bus_j);
    assert_eq!(interp, jit_cpu, "warm phases must match before admission");
    assert_eq!(interp.fp_rem, jit_cpu.fp_rem, "warm fp_rem must match");

    let idx = jit::block::try_admit(&mut jit_cpu, X87_LOOP, true).expect("the x87 loop must build");
    {
        let region = jit_cpu.jit_regions.get_mut(idx).unwrap();
        assert!(region.is_loop, "the x87 block is a self-loop");
        assert_eq!(
            region.ctx.slots.len(),
            7,
            "fninit+fild+fistp+add+mov+dec+jnz"
        );
    }
    jit_cpu.decode_cache.stamp_region(X87_LOOP, true, idx);

    // Measured run: eight iterations, driven to the loop fall-through.
    x87_arm(&mut interp, &mut bus_i, 8);
    x87_arm(&mut jit_cpu, &mut bus_j, 8);
    drive_until_count_zero(&mut interp, &mut bus_i);
    drive_until_count_zero(&mut jit_cpu, &mut bus_j);

    // THE gate: all four accumulators byte-identical, region vs interpreter.
    assert_eq!(
        interp.elapsed_clocks, jit_cpu.elapsed_clocks,
        "elapsed_clocks diverged"
    );
    assert_eq!(interp.timing_rem, jit_cpu.timing_rem, "timing_rem diverged");
    assert_eq!(
        interp.fp_rem, jit_cpu.fp_rem,
        "fp_rem diverged (x87 batching)"
    );
    assert_eq!(
        interp.core_clocks_so_far, jit_cpu.core_clocks_so_far,
        "core_clocks_so_far diverged"
    );
    // And the full architectural state + guest memory.
    assert_eq!(interp, jit_cpu, "architectural state diverged");
    assert_eq!(bus_i.memory, bus_j.memory, "guest memory diverged");

    // The test is only meaningful if the region actually ran and the FP path carried a
    // remainder (a non-identity FP class was exercised).
    assert!(
        jit_cpu.perf_counters().jit_region_entries > 0,
        "the region never executed"
    );
    assert_eq!(interp.perf_counters().jit_region_entries, 0);
    assert!(
        interp.fp_rem != 0,
        "the FP timing remainder must be exercised (else the fp_rem check is vacuous)"
    );
}

// ---- 2. Self-loop livelock guard (jmp $) ----

#[test]
fn self_loop_advances_the_clock_and_stops_at_the_cap() {
    let mut cpu = fresh();
    let mut mem = vec![0u8; 0x1000];
    mem[0x100] = 0x90; // nop starter
    mem[0x101] = 0xeb; // jmp $ (rel8 -2 -> 0x101)
    mem[0x102] = 0xfe;
    let mut bus = TestBus::with_memory(mem);

    // Warm 0x100 and 0x101 (jmp $ never halts, so warm with a bounded finite-cap drive).
    cpu.registers.eip = 0x100;
    for _ in 0..8 {
        let _ = cpu.run_straight_line(&mut bus, 50);
    }

    let idx =
        jit::block::try_admit(&mut cpu, 0x101, true).expect("jmp $ must build a 1-slot self-loop");
    {
        let region = cpu.jit_regions.get_mut(idx).unwrap();
        assert!(region.is_loop);
        assert_eq!(region.ctx.slots.len(), 1);
    }
    cpu.decode_cache.stamp_region(0x101, true, idx);

    cpu.registers.eip = 0x101;
    let before = cpu.elapsed_clocks;
    let entries_before = cpu.perf_counters().jit_region_entries;
    let out = cpu.run_straight_line(&mut bus, 1000).unwrap();

    assert!(!out.halted, "jmp $ never halts");
    assert!(
        cpu.elapsed_clocks > before,
        "the self-loop must advance the clock (no net-zero livelock)"
    );
    assert!(
        cpu.perf_counters().jit_region_entries > entries_before,
        "the region must have run"
    );
    assert_eq!(cpu.registers.eip, 0x101, "still looping at jmp $");
}

// ---- 3. A linear (non-loop) block runs identically to the interpreter ----

const LIN_START: u32 = 0x100;
const LIN_BODY: u32 = 0x101;

fn linear_program() -> Vec<u8> {
    let mut m = vec![0u8; 0x1000];
    m[LIN_START as usize] = 0x90; // nop starter -> LIN_BODY is a continuation
    let body: [u8; 11] = [
        0x83, 0xc0, 0x05, // add eax,5
        0x83, 0xc3, 0x07, // add ebx,7
        0x89, 0x07, // mov [edi],eax
        0x83, 0xc1, 0x01, // add ecx,1
    ];
    m[LIN_BODY as usize..LIN_BODY as usize + body.len()].copy_from_slice(&body);
    m[LIN_BODY as usize + body.len()] = 0xf4; // hlt terminates the block
    m
}

fn lin_arm(cpu: &mut CpuGsw) {
    cpu.registers.eip = LIN_START;
    cpu.registers.set_esp(0x0700);
    cpu.registers.set_eax(0x1111);
    cpu.registers.set_ebx(0x2222);
    cpu.registers.set_ecx(0);
    cpu.registers.set_edi(0x310);
}

#[test]
fn linear_block_matches_the_interpreter() {
    let mut interp = fresh();
    let mut jit_cpu = fresh();
    let mut bus_i = TestBus::with_memory(linear_program());
    let mut bus_j = TestBus::with_memory(linear_program());

    lin_arm(&mut interp);
    lin_arm(&mut jit_cpu);
    drive_to_halt(&mut interp, &mut bus_i);
    drive_to_halt(&mut jit_cpu, &mut bus_j);
    assert_eq!(interp, jit_cpu, "warm phases must match before admission");

    let idx =
        jit::block::try_admit(&mut jit_cpu, LIN_BODY, true).expect("the linear block must build");
    {
        let region = jit_cpu.jit_regions.get_mut(idx).unwrap();
        assert!(!region.is_loop, "a straight-line block is not a self-loop");
        assert_eq!(
            region.ctx.slots.len(),
            4,
            "add,add,mov,add (hlt is the terminator)"
        );
    }
    jit_cpu.decode_cache.stamp_region(LIN_BODY, true, idx);

    lin_arm(&mut interp);
    lin_arm(&mut jit_cpu);
    drive_to_halt(&mut interp, &mut bus_i);
    drive_to_halt(&mut jit_cpu, &mut bus_j);

    assert_eq!(
        interp.elapsed_clocks, jit_cpu.elapsed_clocks,
        "elapsed_clocks"
    );
    assert_eq!(interp.timing_rem, jit_cpu.timing_rem, "timing_rem");
    assert_eq!(interp, jit_cpu, "architectural state diverged");
    assert_eq!(bus_i.memory, bus_j.memory, "guest memory diverged");
    assert!(
        jit_cpu.perf_counters().jit_region_entries > 0,
        "the linear block region never ran"
    );
}

// ---- 4. The behavioral terminator predicate (§2.9) ----

fn decode_one(bytes: &[u8]) -> DecodedInsn {
    let mut cpu = fresh();
    let mut mem = vec![0u8; 0x100];
    mem[..bytes.len()].copy_from_slice(bytes);
    let mut bus = TestBus::with_memory(mem);
    cpu.registers.eip = 0;
    cpu.decode(&mut bus).expect("opcode decodes")
}

#[test]
fn terminator_predicate_covers_clock_device_and_interrupt_ops() {
    // Interior-eligible ops: fall through, no interrupt-visibility change, continuable.
    for (bytes, name) in [
        (&[0x83, 0xc1, 0x01][..], "add ecx,1"),
        (&[0x89, 0xd8][..], "mov eax,ebx"),
        (&[0x8e, 0xc0][..], "mov es,ax"),
        (
            &[0xec][..],
            "in al,dx (Approximate: runtime step-break, interior)",
        ),
        (&[0xd9, 0xe8][..], "fld1 (x87)"),
    ] {
        let insn = decode_one(bytes);
        assert!(
            jit::block::is_interior_eligible(&insn),
            "{name} must be an interior slot"
        );
    }

    for (bytes, name) in [(&[0x1f][..], "pop ds"), (&[0x8e, 0xd8][..], "mov ds,ax")] {
        let insn = decode_one(bytes);
        assert!(insn.continuable, "{name} is continuable");
        assert!(jit::block::changes_native_memory_context(&insn));
        assert!(
            !jit::block::is_interior_eligible(&insn),
            "{name} must end a native-memory block"
        );
    }

    // Hard terminators: not continuable at all (build_block stops before them).
    for (bytes, name) in [
        (&[0xf4][..], "hlt"),
        (&[0xee][..], "out dx,al"),
        (&[0xe6, 0x00][..], "out imm8,al"),
        (&[0x6c][..], "insb"),
        (&[0x6e][..], "outsb"),
        (&[0x0f, 0x31][..], "rdtsc (reads elapsed_clocks)"),
        (&[0x0f, 0x30][..], "wrmsr"),
        (&[0x0f, 0x22, 0xc0][..], "mov cr0,eax"),
        (&[0x0f, 0x01, 0x10][..], "lgdt [eax]"),
        (&[0xcd, 0x21][..], "int 21h"),
        (&[0xcf][..], "iret"),
    ] {
        let insn = decode_one(bytes);
        assert!(
            !insn.continuable,
            "{name} must be a non-continuable hard terminator"
        );
        assert!(
            !jit::block::is_interior_eligible(&insn),
            "{name} must not be an interior slot"
        );
    }

    // The load-bearing gap: IF/shadow changers are `continuable` (the interpreter runs
    // them inline with a per-instruction interrupt check) but MUST be excluded from
    // interior slots, because the region defers that check to the boundary.
    for (bytes, name) in [
        (&[0xfb][..], "sti"),
        (&[0xfa][..], "cli"),
        (&[0x9d][..], "popf"),
        (&[0x17][..], "pop ss"),
        (&[0x8e, 0xd0][..], "mov ss,ax"),
    ] {
        let insn = decode_one(bytes);
        assert!(
            insn.continuable,
            "{name} is continuable (the whole point of the gap)"
        );
        assert!(
            jit::block::changes_interrupt_visibility(&insn),
            "{name} must be flagged as an interrupt-visibility change"
        );
        assert!(
            !jit::block::is_interior_eligible(&insn),
            "{name} must not be an interior slot"
        );
    }

    // Control transfers are continuable but end the block as the terminal slot.
    for (bytes, name) in [
        (&[0xc3][..], "ret near"),
        (&[0x75, 0x00][..], "jnz rel8"),
        (&[0xeb, 0x00][..], "jmp rel8"),
    ] {
        let insn = decode_one(bytes);
        assert!(insn.continuable, "{name} is continuable");
        assert!(
            jit::block::is_control_transfer(&insn),
            "{name} must be flagged as a control transfer"
        );
        assert!(
            !jit::block::is_interior_eligible(&insn),
            "{name} must not be an interior slot"
        );
    }
}

#[test]
fn jit_prefix_and_interpreter_out_terminators_agree_on_the_bus_offset() {
    fn run(opcode: u8) {
        let mut program = vec![0u8; 0x1000];
        program[0x100..0x106].copy_from_slice(&[
            0x90, // starter
            0x40, // inc eax, JIT loop body
            0xe2, 0xfd,   // loop 0x101
            opcode, // out dx,al or outsb
            0xf4,   // hlt
        ]);
        program[0x300] = 0x5a;

        let mut interp = fresh();
        let mut jit_cpu = fresh();
        let mut bus_i = TestBus::with_memory(program.clone());
        let mut bus_j = TestBus::with_memory(program);
        let arm = |cpu: &mut CpuGsw| {
            cpu.registers.eip = 0x100;
            cpu.registers.set_ecx(3);
            cpu.registers.set_edx(0x300);
            cpu.registers.set_esi(0x300);
        };
        let drive = |cpu: &mut CpuGsw, bus: &mut TestBus| {
            for _ in 0..32 {
                bus.io_touched = false;
                if cpu.run_straight_line(bus, u64::MAX).unwrap().halted {
                    return;
                }
            }
            panic!("guest never halted");
        };

        arm(&mut interp);
        arm(&mut jit_cpu);
        drive(&mut interp, &mut bus_i);
        drive(&mut jit_cpu, &mut bus_j);
        let idx = jit::block::try_admit(&mut jit_cpu, 0x101, true).unwrap();
        jit_cpu.decode_cache.stamp_region(0x101, true, idx);

        arm(&mut interp);
        arm(&mut jit_cpu);
        drive(&mut interp, &mut bus_i);
        drive(&mut jit_cpu, &mut bus_j);

        assert_eq!(interp, jit_cpu);
        assert_eq!(bus_i.memory, bus_j.memory);
        assert_eq!(
            bus_i.last_write_io_core_clocks_so_far,
            bus_j.last_write_io_core_clocks_so_far
        );
        assert_eq!(bus_i.last_write_io_core_clocks_so_far, Some(0));
        assert!(jit_cpu.perf_counters().jit_region_entries > 0);
    }

    run(0xee);
    run(0x6e);
}

// ---- 5. 16-bit register ops must not be inlined as 32-bit templates ----

/// Real mode with a 16-bit code segment (the default DOS-game target): CS.D is clear, so
/// the unprefixed mov/add/shr register forms are 16-bit ops.
fn fresh16() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.load_segment_real(SegmentIndex::Cs, 0); // default_size_32 = false
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu
}

#[test]
fn sixteen_bit_register_ops_are_not_inlined_as_wrong_width() {
    // Regression for the operand-size gap: in a 16-bit segment the inline-able opcodes
    // (0x8B mov r,r; 0x81 /0 add r,imm; 0xC1 /5 shr r,imm) are 16-bit, so they must run
    // through the full trampoline step (correct width), NOT the 32-bit inline template
    // (which would clobber the upper 16 bits and compute 32-bit flags).
    let program = || {
        let mut m = vec![0u8; 0x1000];
        m[0x100] = 0x90; // nop starter, so 0x101 is reached as a continuation
        let body: [u8; 15] = [
            0x8b, 0xc3, // mov ax,bx           (16-bit: keeps EAX[31:16])
            0x81, 0xc0, 0x34, 0x12, // add ax,0x1234       (16-bit add + 16-bit flags)
            0xc1, 0xe8, 0x01, // shr ax,1            (16-bit shr)
            0xff, 0x0e, 0x00, 0x04, // dec word [0x400]    (16-bit memory RMW)
            0x75, 0x00, // jnz (rel patched below)
        ];
        m[0x101..0x101 + body.len()].copy_from_slice(&body);
        m[0x10f] = ((0x101i32 - 0x110i32) as i8) as u8; // jnz -> 0x101
        m[0x110] = 0xf4; // hlt
        m
    };
    let arm = |cpu: &mut CpuGsw, bus: &mut TestBus, count: u16| {
        cpu.registers.eip = 0x100;
        cpu.registers.set_esp(0x0700);
        cpu.registers.set_eax(0xAAAA_0000); // distinct upper half
        cpu.registers.set_ebx(0xBBBB_2222);
        bus.memory[0x400..0x402].copy_from_slice(&count.to_le_bytes());
    };

    let mut interp = fresh16();
    let mut jit_cpu = fresh16();
    let mut bus_i = TestBus::with_memory(program());
    let mut bus_j = TestBus::with_memory(program());

    arm(&mut interp, &mut bus_i, 2);
    arm(&mut jit_cpu, &mut bus_j, 2);
    drive_to_halt(&mut interp, &mut bus_i);
    drive_to_halt(&mut jit_cpu, &mut bus_j);
    assert_eq!(interp, jit_cpu, "warm phases must match before admission");

    // d = false (16-bit segment).
    let idx =
        jit::block::try_admit(&mut jit_cpu, 0x101, false).expect("the 16-bit loop must build");
    {
        let region = jit_cpu.jit_regions.get_mut(idx).unwrap();
        assert!(region.is_loop);
        assert_eq!(region.ctx.slots.len(), 5);
        // The fix in the flesh: no 16-bit slot is an inline 32-bit template.
        for (i, s) in region.ctx.slots.iter().enumerate() {
            assert!(
                matches!(
                    s.kind,
                    jit::step::SlotKind::Memory | jit::step::SlotKind::BackEdge
                ),
                "16-bit slot {i} must run through the full step, got {:?}",
                s.kind
            );
        }
    }
    jit_cpu.decode_cache.stamp_region(0x101, false, idx);

    arm(&mut interp, &mut bus_i, 5);
    arm(&mut jit_cpu, &mut bus_j, 5);
    drive_to_halt(&mut interp, &mut bus_i);
    drive_to_halt(&mut jit_cpu, &mut bus_j);

    assert_eq!(
        interp, jit_cpu,
        "16-bit register ops diverged (wrong-width inline?)"
    );
    assert_eq!(bus_i.memory, bus_j.memory, "guest memory diverged");
    // The 16-bit ops must have preserved the upper half of EAX in both paths.
    assert_eq!(
        jit_cpu.registers.eax() & 0xFFFF_0000,
        0xAAAA_0000,
        "the JIT clobbered EAX[31:16] with a 32-bit op"
    );
    assert!(jit_cpu.perf_counters().jit_region_entries > 0);
}

#[test]
fn region_callbacks_keep_host_stack_aligned_in_a_string_loop() {
    // DOS/4GW uses this strlen/copy shape while starting a protected-mode
    // program. It was the first real workload whose callback needed the host
    // ABI's stack-alignment guarantee, exposing a region prologue that counted
    // five saved registers while actually pushing six.
    let program = || {
        let mut memory = vec![0u8; 0x1000];
        memory[0x100] = 0x90; // starter NOP warms the loop as a continuation
        memory[0x101..0x108].copy_from_slice(&[
            0xac, // lodsb
            0xaa, // stosb
            0x84, 0xc0, // test al,al
            0x75, 0xfa, // jnz 0x101
            0xf4, // hlt
        ]);
        memory[0x300..0x307].copy_from_slice(b"DOS4GW\0");
        memory
    };
    let arm = |cpu: &mut CpuGsw, bus: &mut TestBus| {
        cpu.registers.eip = 0x100;
        cpu.registers.set_esp(0x700);
        cpu.registers.set_esi(0x300);
        cpu.registers.set_edi(0x400);
        bus.memory[0x400..0x407].fill(0);
    };

    let mut interp = fresh16();
    let mut jit_cpu = fresh16();
    let mut bus_i = TestBus::with_memory(program());
    let mut bus_j = TestBus::with_memory(program());

    arm(&mut interp, &mut bus_i);
    arm(&mut jit_cpu, &mut bus_j);
    drive_to_halt(&mut interp, &mut bus_i);
    drive_to_halt(&mut jit_cpu, &mut bus_j);
    assert_eq!(interp, jit_cpu, "warm phases must match before admission");

    let idx = jit::block::try_admit(&mut jit_cpu, 0x101, false)
        .expect("the DOS/4GW string loop must build");
    jit_cpu.decode_cache.stamp_region(0x101, false, idx);

    arm(&mut interp, &mut bus_i);
    arm(&mut jit_cpu, &mut bus_j);
    drive_to_halt(&mut interp, &mut bus_i);
    drive_to_halt(&mut jit_cpu, &mut bus_j);

    assert_eq!(interp, jit_cpu, "string-loop CPU state diverged");
    assert_eq!(bus_i.memory, bus_j.memory, "string-loop memory diverged");
    assert_eq!(&bus_j.memory[0x400..0x407], b"DOS4GW\0");
    assert!(jit_cpu.perf_counters().jit_region_entries > 0);
}

// ---- Per-operation native-template differential checks ----
//
// For each templated op, admit it as a single INTERIOR inline slot and run the region vs
// the interpreter across flag-corner operands, asserting byte-identical guest state,
// materialized eflags, all four accumulators, and guest memory. This is the gate every
// native template must pass (a divergence in a width/wrap/undefined-flag corner fails
// here). Each template adds a row. The op's flags must survive to the comparison,
// so the loop back-edge is LOOP (0xE2), which decrements ECX and branches WITHOUT touching
// the flags a `dec`/`jnz` counter would clobber.

/// nop starter at 0x100, then `<op>` (the interior slot under test) at 0x101, then
/// `loop 0x101` (the terminal back-edge, flag-neutral), then hlt at the fall-through.
fn template_diff_program(op: &[u8]) -> Vec<u8> {
    let mut m = vec![0u8; 0x1000];
    m[0x100] = 0x90; // nop starter -> 0x101 is reached as a continuation
    let entry = 0x101usize;
    let mut p = entry;
    m[p..p + op.len()].copy_from_slice(op);
    p += op.len();
    let loop_at = p; // loop 0x101 (E2 rel8): ECX -= 1, branch if ECX != 0, sets NO flags
    m[p] = 0xe2;
    m[p + 1] = ((entry as i32) - (loop_at as i32 + 2)) as i8 as u8;
    p += 2;
    m[p] = 0xf4; // hlt
    m
}

/// Drive to the loop fall-through (ECX == 0), i.e. BEFORE the trailing HLT is executed as a
/// fresh run's first instruction (which would reset core_clocks_so_far).
fn drive_until_ecx_zero(cpu: &mut CpuGsw, bus: &mut TestBus) {
    for _ in 0..10_000 {
        let out = cpu.run_straight_line(bus, u64::MAX).unwrap();
        if cpu.read_gpr32(1) == 0 || out.halted {
            return;
        }
    }
    panic!("ECX never reached zero");
}

/// Run one templated op through the region and the interpreter under `arm` (which sets the
/// op's input registers, not ECX), and assert full identity. `expect_kind` pins that the
/// op was actually inlined as the intended template (not a Memory fallback).
fn assert_template_identity(
    op: &[u8],
    expect_kind: jit::step::SlotKind,
    arm: &dyn Fn(&mut CpuGsw),
) {
    let entry = 0x101u32;
    let mut interp = fresh();
    let mut jit_cpu = fresh();
    let mut bus_i = TestBus::with_memory(template_diff_program(op));
    let mut bus_j = TestBus::with_memory(template_diff_program(op));

    let prep = |cpu: &mut CpuGsw, ecx: u32| {
        cpu.registers.eip = 0x100;
        cpu.registers.set_esp(0x0700);
        cpu.registers.set_ecx(ecx); // LOOP counter (address-size 32 -> ECX)
        arm(cpu);
        // Seed a non-trivial incoming arithmetic-flag pattern so the PRESERVING templates
        // are tested against non-zero flags, not the default 0: a MOV that wrongly touched
        // any flag, or a SHR that clobbered its preserved AF or forced OF on a multi-bit
        // shift (both architecturally preserved / undefined), would otherwise be masked by
        // an all-zero incoming state. ZF is left clear so ZF-preservation is also observable.
        cpu.materialize_flags();
        const ARITH: u32 = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF;
        let seed = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_SF | FLAG_OF;
        cpu.registers.eflags = (cpu.registers.eflags & !ARITH) | seed;
    };
    // Warm both (two iterations fill the decode cache), then admit on the jit CPU.
    prep(&mut interp, 2);
    prep(&mut jit_cpu, 2);
    drive_until_ecx_zero(&mut interp, &mut bus_i);
    drive_until_ecx_zero(&mut jit_cpu, &mut bus_j);
    let idx = jit::block::try_admit(&mut jit_cpu, entry, true)
        .unwrap_or_else(|| panic!("op {op:02x?} must build a self-loop"));
    assert_eq!(
        jit_cpu.jit_regions.get_mut(idx).unwrap().ctx.slots[0].kind,
        expect_kind,
        "op {op:02x?}: slot 0 must be the intended inline template"
    );
    jit_cpu.decode_cache.stamp_region(entry, true, idx);

    // Measured: one iteration under the swept operand.
    prep(&mut interp, 1);
    prep(&mut jit_cpu, 1);
    drive_until_ecx_zero(&mut interp, &mut bus_i);
    drive_until_ecx_zero(&mut jit_cpu, &mut bus_j);

    assert_eq!(interp, jit_cpu, "op {op:02x?}: guest state diverged");
    assert_eq!(
        interp.eflags(),
        jit_cpu.eflags(),
        "op {op:02x?}: materialized eflags diverged"
    );
    assert_eq!(
        interp.elapsed_clocks, jit_cpu.elapsed_clocks,
        "op {op:02x?}: elapsed_clocks diverged"
    );
    assert_eq!(
        interp.timing_rem, jit_cpu.timing_rem,
        "op {op:02x?}: timing_rem diverged"
    );
    assert_eq!(
        interp.fp_rem, jit_cpu.fp_rem,
        "op {op:02x?}: fp_rem diverged"
    );
    assert_eq!(
        interp.core_clocks_so_far, jit_cpu.core_clocks_so_far,
        "op {op:02x?}: core_clocks_so_far diverged"
    );
    assert_eq!(
        bus_i.memory, bus_j.memory,
        "op {op:02x?}: guest memory diverged"
    );
    assert!(
        jit_cpu.perf_counters().jit_region_entries > 0,
        "op {op:02x?}: region did not run"
    );
}

#[test]
fn template_diff_add_r32_imm_across_flag_corners() {
    // add eax, imm32 (81 /0, the RegAddImm template). Sweep eax and imm across the carry
    // (0xffffffff+1), overflow (0x7fffffff+1, 0x80000000+0x80000000), sign, zero, and
    // parity corners; region and interpreter must agree on state, eflags, and all four
    // accumulators for every corner.
    let corners: [u32; 5] = [0, 1, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff];
    for &eax in &corners {
        for &imm in &corners {
            let mut op = vec![0x81u8, 0xc0]; // add eax, imm32
            op.extend_from_slice(&imm.to_le_bytes());
            assert_template_identity(
                &op,
                jit::step::SlotKind::RegAddImm { dst: 0, imm },
                &|cpu: &mut CpuGsw| cpu.registers.set_eax(eax),
            );
        }
    }
    // Register-addressing coverage: the emit addresses gpr[i] as [R14 + 4*i], so every
    // inline-eligible destination index must be exercised (skip ECX=1, the LOOP counter,
    // and ESP=4, the stack). A wrong displacement for a high index would pass an EAX-only
    // sweep.
    for &dst in &[0u8, 2, 3, 5, 6, 7] {
        let imm = 0x8000_0001u32; // carry + overflow + sign in one operand
        let mut op = vec![0x81u8, 0xc0 + dst]; // add <dst>, imm32
        op.extend_from_slice(&imm.to_le_bytes());
        assert_template_identity(
            &op,
            jit::step::SlotKind::RegAddImm { dst, imm },
            &|cpu: &mut CpuGsw| cpu.write_gpr32(dst, 0x8000_0001),
        );
    }
}

#[test]
fn template_diff_shr_r32_imm_across_shift_corners() {
    // shr eax, imm8 (C1 /5, the RegShrImm template). Sweep the value and the count across
    // the CF-from-last-bit-out, OF (count 1), sign, zero, and parity corners.
    let vals: [u32; 6] = [0, 1, 0x8000_0001, 0xffff_ffff, 0x7fff_fffe, 0x0000_00ff];
    let counts: [u8; 5] = [1, 2, 7, 25, 31];
    for &eax in &vals {
        for &count in &counts {
            let op = vec![0xc1u8, 0xe8, count]; // shr eax, count
            assert_template_identity(
                &op,
                jit::step::SlotKind::RegShrImm { dst: 0, count },
                &|cpu: &mut CpuGsw| cpu.registers.set_eax(eax),
            );
        }
    }
    // Register-addressing coverage across all inline-eligible destinations (with the
    // incoming AF/OF seeded by `prep`, this also pins the preserved-flag path per index).
    for &dst in &[0u8, 2, 3, 5, 6, 7] {
        let count = 7u8; // multi-bit shift: OF falls back to live, AF is preserved
        let op = vec![0xc1u8, 0xe8 + dst, count]; // shr <dst>, count
        assert_template_identity(
            &op,
            jit::step::SlotKind::RegShrImm { dst, count },
            &|cpu: &mut CpuGsw| cpu.write_gpr32(dst, 0x8000_0001),
        );
    }
}

#[test]
fn template_diff_mov_r32_r32_preserves_state() {
    // mov eax, ebx (8B /r, the RegMov template). No flags; sweep the source value and
    // confirm the destination is a faithful full-32-bit copy with flags untouched.
    let vals: [u32; 5] = [0, 0xdead_beef, 0xffff_ffff, 0x8000_0000, 0x1234_5678];
    for &ebx in &vals {
        let op = vec![0x8bu8, 0xc3]; // mov eax, ebx
        assert_template_identity(
            &op,
            jit::step::SlotKind::RegMov { dst: 0, src: 3 },
            &|cpu: &mut CpuGsw| {
                cpu.registers.set_eax(0xaaaa_5555);
                cpu.registers.set_ebx(ebx);
            },
        );
    }
    // Register-addressing coverage: distinct dst/src index pairs (never ECX=1), each dst !=
    // src so the copy is observable, catching a wrong displacement or a dst/src swap that
    // happens to work for the EAX<-EBX case. With `prep`'s seeded flags, this also confirms
    // MOV touches no flag for every index.
    for &(dst, src) in &[(0u8, 7u8), (2, 5), (3, 6), (5, 3), (6, 2), (7, 0)] {
        let op = vec![0x8bu8, 0xc0 | (dst << 3) | src]; // mov <dst>, <src>
        assert_template_identity(
            &op,
            jit::step::SlotKind::RegMov { dst, src },
            &|cpu: &mut CpuGsw| {
                cpu.write_gpr32(dst, 0xaaaa_5555);
                cpu.write_gpr32(src, 0x1234_5678);
            },
        );
    }
}

// ---- Round 3 GATING HARNESS: general multi-iteration + fault-injection + SMC differential ----
//
// The one-iteration `assert_template_identity` gate above is structurally blind to the bug
// classes Round 3's native memory templates introduce: cross-iteration flag/carry propagation
// over the back-edge, register spill on a mid-loop fault, and self-modifying-code refetch. This
// harness runs a block SHAPE to completion on the interpreter and the JIT (hotness auto-admit)
// and asserts full state + memory identity at the halt boundary, with variants that inject a
// mid-loop fault and a self-store. Every memory template that lands must pass it per shape.
// It began as a trampoline oracle and now also pins the exact native-memory path.

const H_ENTRY: u32 = 0x101;
const H_COUNT: usize = 0x400;
const H_GP_HANDLER: u32 = 0x0b00;

/// A self-loop `mov al,[esi] ; mov [edi],al ; inc esi ; inc edi ; dec [count] ; jnz` plus a
/// HLT at the fall-through, a #GP (vector 13) IVT entry to a HLT handler, and `handler` bytes.
/// A byte-copy loop with a memory load, a memory store, and a memory RMW counter - the exact
/// operand shapes Round 3 templates target.
fn h_copy_program() -> Vec<u8> {
    let mut m = vec![0u8; 0x1_0000];
    m[0x100] = 0x90; // nop starter -> H_ENTRY reached as a continuation
    let body: [u8; 13] = [
        0x8a, 0x06, // mov al,[esi]
        0x88, 0x07, // mov [edi],al
        0x46, // inc esi
        0x47, // inc edi
        0xff, 0x0d, 0x00, 0x04, 0x00, 0x00, // dec dword [H_COUNT]
        0x75, // jnz rel8 (rel filled below)
    ];
    m[H_ENTRY as usize..H_ENTRY as usize + body.len()].copy_from_slice(&body);
    let rel_at = H_ENTRY as usize + body.len(); // the rel8 byte
    m[rel_at] = ((H_ENTRY as i32) - (rel_at as i32 + 1)) as i8 as u8;
    m[rel_at + 1] = 0xf4; // hlt at the loop fall-through
    // #GP (vector 13) -> 0:H_GP_HANDLER, a HLT (the fault-injection landing).
    m[13 * 4..13 * 4 + 2].copy_from_slice(&(H_GP_HANDLER as u16).to_le_bytes());
    m[H_GP_HANDLER as usize] = 0xf4;
    m
}

/// Run `prog` to a halt on both an interpreter CPU and a hotness-auto-admitting JIT CPU under
/// `arm`, asserting full guest identity + memory + timing at the halt boundary. `expect_region`
/// pins that the JIT actually compiled and ran a region (drop it for shapes whose SMC churn may
/// keep the region cold). Returns the final interpreter CPU so a caller can assert its shape
/// actually exercised its scenario (the fault fired, SMC churned). Panics on any divergence.
fn assert_shape_identical(prog: Vec<u8>, arm: &dyn Fn(&mut CpuGsw), expect_region: bool) -> CpuGsw {
    let mut interp = fresh();
    let mut jit_cpu = fresh();
    jit_cpu.set_legacy_region_auto_admit(true);
    let mut bus_i = TestBus::with_memory(prog.clone());
    let mut bus_j = TestBus::with_memory(prog);
    arm(&mut interp);
    arm(&mut jit_cpu);
    drive_to_halt(&mut interp, &mut bus_i);
    drive_to_halt(&mut jit_cpu, &mut bus_j);
    assert_state_identical(&interp, &jit_cpu);
    assert_eq!(
        interp.eflags(),
        jit_cpu.eflags(),
        "materialized eflags diverged"
    );
    // Assert every timing accumulator separately so a divergence names the field.
    assert_eq!(
        interp.elapsed_clocks, jit_cpu.elapsed_clocks,
        "elapsed_clocks diverged"
    );
    assert_eq!(
        interp.core_clocks_so_far, jit_cpu.core_clocks_so_far,
        "core_clocks_so_far diverged"
    );
    assert_eq!(interp.timing_rem, jit_cpu.timing_rem, "timing_rem diverged");
    assert_eq!(interp.fp_rem, jit_cpu.fp_rem, "fp_rem diverged");
    assert_eq!(bus_i.memory, bus_j.memory, "guest memory diverged");
    assert_eq!(
        interp.perf_counters().jit_region_entries,
        0,
        "the interpreter CPU must never compile"
    );
    if expect_region {
        assert!(
            jit_cpu.perf_counters().jit_region_entries > 0,
            "the JIT must have compiled and run a region"
        );
    }
    interp
}

/// Assert two CPUs are STATE-identical, ignoring the four timing accumulators
/// (`elapsed_clocks`, `core_clocks_so_far`, `timing_rem`, `fp_rem`).
///
/// A compiled JIT block leaves guest architectural state byte-identical to the interpreter. This
/// helper zeros the four timing fields so callers can report architecture and timing failures
/// separately without maintaining a manual field list.
fn assert_state_identical(interp: &CpuGsw, jit: &CpuGsw) {
    assert!(
        state_eq(interp, jit),
        "architectural state diverged (timing fields ignored)"
    );
}

/// Bool core of [`assert_state_identical`], for tests that want to check both
/// directions without catching a panic.
fn state_eq(interp: &CpuGsw, jit: &CpuGsw) -> bool {
    let mut a = interp.clone();
    let mut b = jit.clone();
    for c in [&mut a, &mut b] {
        c.elapsed_clocks = 0;
        c.core_clocks_so_far = 0;
        c.timing_rem = 0;
        c.fp_rem = 0;
    }
    a == b
}

/// The state comparator must ignore ONLY the four timing accumulators and still
/// catch a real architectural divergence. If it silently ignored a state field,
/// every downstream template differential test would be compromised.
#[test]
fn state_comparator_ignores_timing_but_catches_state() {
    let base = fresh();
    let mut timing_only = base.clone();
    timing_only.elapsed_clocks = 12_345;
    timing_only.core_clocks_so_far = 999;
    timing_only.timing_rem = 7;
    timing_only.fp_rem = 3;
    assert!(
        state_eq(&base, &timing_only),
        "a timing-only difference must compare state-identical"
    );
    let mut gpr_diff = base.clone();
    gpr_diff.write_gpr32(0, 0xdead_beef);
    assert!(
        !state_eq(&base, &gpr_diff),
        "a GPR difference must be caught"
    );
}

#[test]
fn p55c_mode_admitted_xadd_loop_matches_the_interpreter() {
    let mut program = vec![0u8; 0x1_0000];
    program[0x100..0x108].copy_from_slice(&[
        0x90, // NOP starter
        0x0f, 0xc1, 0xd8, // XADD EAX,EBX
        0x49, // DEC ECX
        0x75, 0xfa, // JNZ 0x101
        0xf4, // HLT
    ]);
    let arm = |cpu: &mut CpuGsw| {
        cpu.registers.eip = 0x100;
        cpu.registers.set_esp(0x700);
        cpu.registers.set_eax(1);
        cpu.registers.set_ebx(1);
        cpu.registers.set_ecx(200);
    };
    assert_shape_identical(program, &arm, true);
}

/// Sets eip/esp/esi/edi and a non-trivial incoming flag pattern. The loop count lives in the
/// program image (at `H_COUNT`), not here.
fn h_arm(esi: u32, edi: u32) -> impl Fn(&mut CpuGsw) {
    move |cpu: &mut CpuGsw| {
        cpu.registers.eip = 0x100;
        cpu.registers.set_esp(0x0700);
        cpu.write_gpr32(6, esi); // esi
        cpu.write_gpr32(7, edi); // edi
        // Seed non-trivial flags so a template that wrongly clobbers a preserved flag shows.
        cpu.materialize_flags();
        const ARITH: u32 = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF;
        cpu.registers.eflags =
            (cpu.registers.eflags & !ARITH) | FLAG_CF | FLAG_PF | FLAG_AF | FLAG_SF;
    }
}

/// Baseline: a long byte-copy loop (load + store + RMW counter) runs many iterations to halt
/// identically, and the JIT auto-admits and runs a region. Catches per-iteration divergence
/// that accumulates over the run (invisible to a single-iteration gate).
#[test]
fn harness_multi_iteration_copy_loop_is_identical() {
    let build = |count: u32| -> Vec<u8> {
        let mut m = h_copy_program();
        m[H_COUNT..H_COUNT + 4].copy_from_slice(&count.to_le_bytes());
        m
    };
    // 200 iterations, esi/edi in low RAM well inside the flat 64 KB segment limit.
    assert_shape_identical(build(200), &h_arm(0x2000, 0x3000), true);
}

/// Fault injection: a mid-loop memory access runs off the DS limit and #GPs, delivering to the
/// IVT handler FROM INSIDE THE LIVE REGION. The interpreter and the JIT must fault at the SAME
/// instruction with identical pushed state - the register file must be committed (not stale) at
/// the fault, the trap the re-plan's spill-on-every-fault-exit rule guards for the eventual
/// native templates. The fault MUST land after hotness admission (JIT_HOTNESS_THRESHOLD = 32
/// iterations) so the JIT's own fault-delivery path is what runs - not the interpreter during
/// warm-up. `expect_region: true` pins that the region actually admitted and ran before faulting.
#[test]
fn harness_mid_loop_fault_delivers_identically() {
    // DS base 0, limit 0x2000. esi=0x1000 (the LOAD stays well inside the limit for the whole
    // run). edi=0x1FC0 advances 1/iteration, so the STORE `mov [edi],al` #GPs when edi first
    // exceeds 0x2000 - at iteration ~66, comfortably past the 32-iteration admission threshold,
    // so the fault is delivered by the running region. count=100 so the loop cannot finish first.
    let prog = {
        let mut m = h_copy_program();
        m[H_COUNT..H_COUNT + 4].copy_from_slice(&100u32.to_le_bytes());
        m
    };
    let arm = move |cpu: &mut CpuGsw| {
        h_arm(0x1000, 0x1fc0)(cpu);
        let mut ds = cpu.registers.segment(SegmentIndex::Ds);
        ds.limit = 0x2000;
        cpu.registers.set_segment(SegmentIndex::Ds, ds);
    };
    let interp = assert_shape_identical(prog, &arm, true);
    // Confirm the shape ACTUALLY faulted (else it just ran to the loop-end HLT and tested
    // nothing): the guest must have halted in the #GP handler, far above the loop code.
    assert!(
        interp.registers.eip >= H_GP_HANDLER,
        "the memory access must have #GP'd into the handler, eip={:#x}",
        interp.registers.eip
    );
}

/// Self-modifying store: `edi` points at the loop's own first opcode byte and `al` is loaded to
/// equal that byte, so every iteration stores the SAME value into live code - firing the SMC
/// watch and forcing a re-decode / region re-admit each iteration without changing behavior.
/// State must stay identical across the write-then-refetch churn (and it stresses the Round 1
/// unstamp-reprimes-hotness re-admit fix). The region may stay cold under the churn, so it is
/// not required to run.
#[test]
fn harness_self_modifying_store_stays_identical() {
    let prog = {
        let mut m = h_copy_program();
        m[H_COUNT..H_COUNT + 4].copy_from_slice(&40u32.to_le_bytes());
        m
    };
    // esi points at H_ENTRY (whose byte is 0x8a, the loop's first opcode), so al = 0x8a each
    // iteration; edi ALSO points at H_ENTRY, so the store rewrites that byte with its own value.
    // Both esi and edi advance by 1/iteration (inc), so after the store the pointers move on -
    // only the FIRST iteration self-writes, but the SMC epoch/generation churn it triggers must
    // still leave both CPUs identical. (A fixed-pointer variant lands with the store template.)
    let arm = h_arm(H_ENTRY, H_ENTRY);
    let interp = assert_shape_identical(prog, &arm, false);
    // Confirm the self-store ACTUALLY hit live code and triggered SMC handling (else it wrote
    // only data and tested nothing): some SMC narrow-kill or global-flush must have fired.
    let pc = interp.perf_counters();
    assert!(
        pc.smc_narrow_kills > 0 || pc.decode_inval_smc > 0,
        "the self-store must have triggered the SMC watch (narrow={}, global={})",
        pc.smc_narrow_kills,
        pc.decode_inval_smc
    );
}

// ---- Round 3 PAGED differential harness ----
//
// The real-mode harness above runs with paging OFF (linear == physical). The Round 3 native
// memory probe's #1 correctness trap (re-plan trap #1) is that the direct-page cache is
// PHYSICAL-keyed while the guest address is LINEAR, so in paged mode a probe that indexes the
// cache with the linear address reads the WRONG physical frame. A harness with an IDENTITY map
// cannot catch that. This one runs the same byte-copy self-loop in 32-bit protected mode with
// paging ON and a deliberately NON-IDENTITY linear->physical map, so a linear-indexed probe
// would diverge. The native path must remain bit-identical, including timing. The Doom/Quake
// anchors run paged, so this is the mode the helper must handle.
//
// Physical image (256 KiB): page directory at 0x1000, page table at 0x2000 (PDE[0], covers
// linear 0..4 MiB), the code frame at 0x8000, the data frame at 0x9000. Linear 0x10000 maps to
// phys 0x8000 (page index 0x10 vs frame 0x8) and linear 0x30000 to phys 0x9000 (0x30 vs 0x9) -
// the indices differ, so the map is genuinely non-identity.
const PG_CODE_LIN: u32 = 0x10000;
const PG_CODE_PHYS: usize = 0x8000;
const PG_DATA_LIN: u32 = 0x30000;
const PG_DATA_PHYS: usize = 0x9000;
const PG_ENTRY_LIN: u32 = PG_CODE_LIN + 1; // loop head, after the nop starter
const PG_SRC_LIN: u32 = PG_DATA_LIN; // esi
const PG_DST_LIN: u32 = PG_DATA_LIN + 0x800; // edi
const PG_COUNT_LIN: u32 = PG_DATA_LIN + 0x400; // dec dword [PG_COUNT_LIN]

/// The `h_copy_program` byte-copy self-loop, assembled to run at `PG_CODE_LIN` in 32-bit
/// protected paged mode with the non-identity map above. `count` seeds the loop counter.
fn paged_copy_program(count: u32) -> Vec<u8> {
    let mut m = vec![0u8; 0x40000];
    // PDE[0] -> PT at phys 0x2000 (present + rw + user).
    m[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes());
    // PTE[linear>>12] for the code and data pages (frame + present + rw + user).
    let code_pte = 0x2000 + (PG_CODE_LIN as usize >> 12) * 4;
    m[code_pte..code_pte + 4].copy_from_slice(&((PG_CODE_PHYS as u32) | 0x007).to_le_bytes());
    let data_pte = 0x2000 + (PG_DATA_LIN as usize >> 12) * 4;
    m[data_pte..data_pte + 4].copy_from_slice(&((PG_DATA_PHYS as u32) | 0x007).to_le_bytes());
    // Code at phys 0x8000 (= linear 0x10000): nop starter, then the loop body.
    m[PG_CODE_PHYS] = 0x90; // nop -> PG_ENTRY_LIN reached as a continuation
    let body: [u8; 13] = [
        0x8a, 0x06, // mov al,[esi]
        0x88, 0x07, // mov [edi],al
        0x46, // inc esi
        0x47, // inc edi
        0xff, 0x0d, 0x00, 0x00, 0x00, 0x00, // dec dword [disp32] (disp filled below)
        0x75, // jnz rel8 (rel filled below)
    ];
    let body_at = PG_CODE_PHYS + 1;
    m[body_at..body_at + body.len()].copy_from_slice(&body);
    m[body_at + 8..body_at + 12].copy_from_slice(&PG_COUNT_LIN.to_le_bytes());
    let rel_at = body_at + body.len(); // the rel8 byte
    let after = PG_CODE_LIN as i32 + (rel_at as i32 - PG_CODE_PHYS as i32) + 1;
    m[rel_at] = (PG_ENTRY_LIN as i32 - after) as i8 as u8;
    m[rel_at + 1] = 0xf4; // hlt at the loop fall-through
    let count_phys = PG_DATA_PHYS + (PG_COUNT_LIN - PG_DATA_LIN) as usize;
    m[count_phys..count_phys + 4].copy_from_slice(&count.to_le_bytes());
    m
}

/// Arm a CPU for `paged_copy_program`: flat 32-bit protected mode, paging on, CPL 0, esi/edi
/// at the given linear addresses, and the same non-trivial incoming flags as `h_arm`.
fn pg_arm(esi: u32, edi: u32) -> impl Fn(&mut CpuGsw) {
    move |cpu: &mut CpuGsw| {
        let flat = |access: u8| SegmentRegister {
            selector: 0x08,
            base: 0,
            limit: 0xffff_ffff,
            access,
            default_size_32: true,
        };
        cpu.registers.set_segment(SegmentIndex::Cs, flat(0x9b)); // code, exec/read
        cpu.registers.set_segment(SegmentIndex::Ds, flat(0x93)); // data, r/w
        cpu.registers.set_segment(SegmentIndex::Ss, flat(0x93));
        cpu.registers.set_segment(SegmentIndex::Es, flat(0x93));
        cpu.cpl = 0;
        cpu.control.cr3 = 0x1000;
        cpu.control.cr0 |= CR0_PE | CR0_PG;
        cpu.registers.eip = PG_CODE_LIN;
        cpu.registers.set_esp(PG_DATA_LIN + 0xf00); // mapped; the loop never touches it
        cpu.write_gpr32(6, esi); // esi
        cpu.write_gpr32(7, edi); // edi
        cpu.materialize_flags();
        const ARITH: u32 = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF;
        cpu.registers.eflags =
            (cpu.registers.eflags & !ARITH) | FLAG_CF | FLAG_PF | FLAG_AF | FLAG_SF;
    }
}

/// A 200-iteration byte copy under NON-IDENTITY paging stays byte-identical (state + timing)
/// between the interpreter and the auto-admitting JIT, and the JIT runs a real paged region.
/// This is the gating harness for the Round 3 paged memory probe: a probe that indexes the
/// physical page cache with the linear address (trap #1) would read the wrong frame here and
/// diverge, because the linear page and physical frame indices differ.
#[test]
fn harness_paged_copy_loop_is_identical() {
    let interp = assert_shape_identical(
        paged_copy_program(200),
        &pg_arm(PG_SRC_LIN, PG_DST_LIN),
        true,
    );
    assert!(
        interp.is_paging_enabled(),
        "the harness must run with paging enabled"
    );
}

/// Self-modifying store under paging: `edi` == `esi` == the loop's own first opcode (linear
/// PG_ENTRY_LIN), so each iteration reads a code byte and writes it back to the SAME linear
/// address through the page tables - firing the physical-keyed SMC watch. The re-decode /
/// region re-admit churn must leave both CPUs identical. The region may stay cold under the
/// churn, so it is not required to run.
#[test]
fn harness_paged_self_modifying_store_stays_identical() {
    let interp = assert_shape_identical(
        paged_copy_program(40),
        &pg_arm(PG_ENTRY_LIN, PG_ENTRY_LIN),
        false,
    );
    let pc = interp.perf_counters();
    assert!(
        pc.smc_narrow_kills > 0 || pc.decode_inval_smc > 0,
        "the self-store must have triggered the SMC watch (narrow={}, global={})",
        pc.smc_narrow_kills,
        pc.decode_inval_smc
    );
}

// ---- Native byte-memory address probe (isolation test of the emitted assembly) ----

fn run_address_probe(cpu: &mut CpuGsw, base: u8, index: Option<u8>, disp: i32) -> Option<u32> {
    use jit::encoder::{Encoder, Reg};
    let regs_off = std::mem::offset_of!(CpuGsw, registers) as u32;
    let mut e = Encoder::new();
    e.push(Reg::RBX);
    e.push(Reg::RBP);
    e.push(Reg::R12);
    e.push(Reg::R13);
    e.push(Reg::R14);
    e.push(Reg::R15);
    #[cfg(windows)]
    e.mov_r64_r64(Reg::R12, Reg::RCX); // win64 arg0 = cpu
    #[cfg(not(windows))]
    e.mov_r64_r64(Reg::R12, Reg::RDI); // sysv arg0 = cpu
    e.mov_r64_r64(Reg::RBP, Reg::R12);
    if regs_off != 0 {
        e.add_r64_imm32(Reg::RBP, regs_off);
    }
    e.load_r32_disp8(Reg::RBX, Reg::RBP, 20);
    e.load_r32_disp8(Reg::R14, Reg::RBP, 28);
    let miss = e.label();
    let done = e.label();
    jit::block::emit_u8_address(&mut e, base, index, disp, false, miss, false);
    e.jmp(done);
    e.place(miss);
    e.mov_r32_imm32(Reg::RAX, u32::MAX);
    e.place(done);
    e.pop(Reg::R15);
    e.pop(Reg::R14);
    e.pop(Reg::R13);
    e.pop(Reg::R12);
    e.pop(Reg::RBP);
    e.pop(Reg::RBX);
    e.ret();
    let bytes = e.finish();
    let buf = jit::exec_mem::ExecutableBuffer::new(&bytes).expect("W^X alloc must succeed");
    let f: extern "C" fn(*mut CpuGsw) -> u32 = unsafe { std::mem::transmute(buf.entry_ptr()) };
    match f(cpu as *mut CpuGsw) {
        u32::MAX => None,
        physical => Some(physical),
    }
}

/// The emitter computes all currently admitted effective-address shapes and leaves guest state alone.
#[test]
fn native_address_probe_returns_the_physical_address() {
    let mut cpu = fresh();
    cpu.write_gpr32(0, 0x5000);
    cpu.write_gpr32(1, 0x0d);
    let before = cpu.registers.clone();
    assert_eq!(run_address_probe(&mut cpu, 0, None, 3), Some(0x5003));
    assert_eq!(run_address_probe(&mut cpu, 0, Some(1), 3), Some(0x5010));
    cpu.write_gpr32(0, 0x9003);
    assert_eq!(run_address_probe(&mut cpu, 0, None, 0), Some(0x9003));
    cpu.write_gpr32(0, 0x5000);
    assert_eq!(cpu.registers, before);
}

// ---- Round 3 byte-LOAD template (stage 1: dispatch removal) ----

/// The classifier must actually route the loop's `mov al,[esi]` (opcode 0x8A, memory operand)
/// to `SlotKind::MemLoadU8`, so the specialized `jit_execute_load_u8` runs instead of the full
/// dispatch. Without this assertion the harness tests would pass even if the classifier never
/// tagged the load (the trampoline is bit-identical either way), so the template would be dead.
#[test]
fn byte_load_slot_is_classified_memloadu8() {
    let mut cpu = fresh();
    let mut bus = TestBus::with_memory({
        let mut m = h_copy_program();
        m[H_COUNT..H_COUNT + 4].copy_from_slice(&2u32.to_le_bytes());
        m
    });
    h_arm(0x2000, 0x3000)(&mut cpu);
    drive_to_halt(&mut cpu, &mut bus); // warm the loop's decode lines
    let idx = jit::block::try_admit(&mut cpu, H_ENTRY, true).expect("admit the copy loop");
    let region = cpu.jit_regions.get_mut(idx).unwrap();
    let load_slots = region
        .ctx
        .slots
        .iter()
        .filter(|s| s.kind == jit::step::SlotKind::MemLoadU8)
        .count();
    assert_eq!(
        load_slots, 1,
        "the `mov al,[esi]` slot must classify as MemLoadU8 (got {load_slots})"
    );
}

/// Fault injection ON THE BYTE LOAD itself: `esi` runs off the DS limit so `mov al,[esi]`
/// #GPs mid-region (before the store), delivering identically on the interpreter and the JIT.
/// The store-fault variant (`harness_mid_loop_fault_delivers_identically`) never exercises the
/// LOAD's fault path, which the MemLoadU8 executor now owns. The fault must land after the
/// 32-iteration admission threshold so the running region delivers it.
#[test]
fn byte_load_mid_loop_fault_delivers_identically() {
    let prog = {
        let mut m = h_copy_program();
        m[H_COUNT..H_COUNT + 4].copy_from_slice(&100u32.to_le_bytes());
        m
    };
    // esi=0x1FC0 advances 1/iteration, so the LOAD `mov al,[esi]` #GPs when esi first exceeds
    // 0x2000 (iteration ~65, past the 32 admission threshold). edi=0x1000 keeps the store in
    // limit, so the load is the faulting access. count=100 so the loop cannot finish first.
    let arm = move |cpu: &mut CpuGsw| {
        h_arm(0x1fc0, 0x1000)(cpu);
        let mut ds = cpu.registers.segment(SegmentIndex::Ds);
        ds.limit = 0x2000;
        cpu.registers.set_segment(SegmentIndex::Ds, ds);
    };
    let interp = assert_shape_identical(prog, &arm, true);
    assert!(
        interp.registers.eip >= H_GP_HANDLER,
        "the byte load must have #GP'd into the handler, eip={:#x}",
        interp.registers.eip
    );
}

// ---- Round 3 byte-STORE template (stage 1: dispatch removal) ----

/// The classifier must route the loop's `mov [edi],al` (opcode 0x88, memory operand) to
/// `SlotKind::MemStoreU8`, so `jit_execute_store_u8` runs instead of the full dispatch. This
/// pins the routing itself is live. The store's FAULT path is exercised in-region by
/// `harness_mid_loop_fault_delivers_identically` (edi runs off the DS limit, so the faulting
/// access is the store, past the admission threshold). The SMC (note_code_write) behavior is
/// covered dynamically by the same-value and changed-value native SMC tests below.
#[test]
fn byte_store_slot_is_classified_memstoreu8() {
    let mut cpu = fresh();
    let mut bus = TestBus::with_memory({
        let mut m = h_copy_program();
        m[H_COUNT..H_COUNT + 4].copy_from_slice(&2u32.to_le_bytes());
        m
    });
    h_arm(0x2000, 0x3000)(&mut cpu);
    drive_to_halt(&mut cpu, &mut bus); // warm the loop's decode lines
    let idx = jit::block::try_admit(&mut cpu, H_ENTRY, true).expect("admit the copy loop");
    let region = cpu.jit_regions.get_mut(idx).unwrap();
    let store_slots = region
        .ctx
        .slots
        .iter()
        .filter(|s| s.kind == jit::step::SlotKind::MemStoreU8)
        .count();
    assert_eq!(
        store_slots, 1,
        "the `mov [edi],al` slot must classify as MemStoreU8 (got {store_slots})"
    );
}

// ---- Round 3 sized (word/dword) mem-move template (stage 1: dispatch removal) ----

/// A dword-copy loop `mov eax,[esi]; mov [edi],eax; add esi,4; add edi,4; dec [cnt]; jnz`, in a
/// 64 KB image. `H_SIZED_CNT` holds the iteration count.
fn sized_copy_program(count: u32) -> Vec<u8> {
    const H_SIZED_CNT: usize = 0x400;
    let mut m = vec![0u8; 0x1_0000];
    m[0x100] = 0x90; // nop starter -> 0x101 reached as a continuation
    let body: [u8; 16] = [
        0x8b, 0x06, // mov eax,[esi]   (MemLoadSized)
        0x89, 0x07, // mov [edi],eax   (MemStoreSized)
        0x83, 0xc6, 0x04, // add esi,4
        0x83, 0xc7, 0x04, // add edi,4
        0xff, 0x0d, 0x00, 0x04, 0x00, 0x00, // dec dword [0x400]
    ];
    m[0x101..0x101 + body.len()].copy_from_slice(&body);
    let rel_at = 0x101 + body.len();
    m[rel_at] = 0x75; // jnz 0x101
    m[rel_at + 1] = ((0x101_i32) - (rel_at as i32 + 2)) as i8 as u8;
    m[rel_at + 2] = 0xf4; // hlt at the fall-through
    m[H_SIZED_CNT..H_SIZED_CNT + 4].copy_from_slice(&count.to_le_bytes());
    m
}

fn sized_copy_arm(cpu: &mut CpuGsw) {
    cpu.registers.eip = 0x100;
    cpu.registers.set_esp(0x0700);
    cpu.write_gpr32(6, 0x2000); // esi
    cpu.write_gpr32(7, 0x3000); // edi
}

/// The classifier must route `mov eax,[esi]` (0x8B mem) to `MemLoadSized` and `mov [edi],eax`
/// (0x89 mem) to `MemStoreSized` so the specialized sized executors run; the register forms
/// (0x8B/0x89 mode 3) carry a Reg operand and stay off this path.
#[test]
fn sized_mem_moves_are_classified() {
    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(sized_copy_program(2));
    sized_copy_arm(&mut cpu);
    drive_to_halt(&mut cpu, &mut bus); // warm the loop's decode lines
    let idx = jit::block::try_admit(&mut cpu, 0x101, true).expect("admit the dword loop");
    let region = cpu.jit_regions.get_mut(idx).unwrap();
    let kinds: Vec<_> = region.ctx.slots.iter().map(|s| s.kind).collect();
    assert!(
        kinds.contains(&jit::step::SlotKind::MemLoadSized),
        "`mov eax,[esi]` must classify as MemLoadSized: {kinds:?}"
    );
    assert!(
        kinds.contains(&jit::step::SlotKind::MemStoreSized),
        "`mov [edi],eax` must classify as MemStoreSized: {kinds:?}"
    );
}

/// The dword-copy loop runs many iterations to halt bit-identically with the sized executors
/// (dword load + dword store), and the JIT auto-admits and runs the region. Pins that the sized
/// templates reproduce the interpreter's `read_memory_sized`/`write_memory_sized` (dword width,
/// the alignment/page-cross/SMC behavior) exactly across the run.
#[test]
fn sized_mem_moves_run_identically() {
    assert_shape_identical(sized_copy_program(200), &sized_copy_arm, true);
}

#[test]
fn block_builder_stops_at_the_entry_code_page() {
    let mut cpu = fresh();
    let mut memory = vec![0u8; 0x2000];
    for offset in (0..16).step_by(2) {
        memory[0x0ff8 + offset] = 0x8b;
        memory[0x0ff9 + offset] = 0xc3; // mov eax,ebx
    }
    memory[0x1008] = 0xf4;
    let mut bus = TestBus::with_memory(memory);
    cpu.registers.eip = 0x0ff8;
    drive_to_halt(&mut cpu, &mut bus);

    let (slots, is_loop) = jit::block::build_block(&cpu, 0x0ff8, true).expect("warm block");
    assert!(!is_loop);
    assert_eq!(slots.len(), 4);
    assert!(slots.iter().all(|slot| slot.lin < 0x1000));
}

// ---- Linear-block auto-admission gate ----

/// Auto-admission refuses short or mixed linear blocks, admits a linear block with four native
/// interior slots, and keeps the existing self-loop path. This avoids the measured broad-linear
/// Doom regression while permitting blocks that can amortize one entry and terminal helper.
#[test]
fn auto_admit_gate_requires_a_native_linear_body_and_keeps_loops() {
    // A linear block: nop; mov eax,ebx; hlt -> build_block yields a 2-slot non-loop.
    let mut lin = fresh();
    let mut m = vec![0u8; 0x1000];
    m[0x200] = 0x90; // nop
    m[0x201] = 0x8b;
    m[0x202] = 0xc3; // mov eax,ebx
    m[0x203] = 0xf4; // hlt
    let mut bus_l = TestBus::with_memory(m);
    lin.registers.eip = 0x200;
    drive_to_halt(&mut lin, &mut bus_l); // warm 0x200..0x203
    assert!(
        jit::block::try_admit_gated(&mut lin, 0x200, true, true).is_none(),
        "auto-admission must refuse a linear block"
    );
    assert!(
        jit::block::try_admit_gated(&mut lin, 0x200, true, false).is_some(),
        "the forced/test path still admits the same linear block"
    );

    // Five register moves followed by HLT produce four native interior slots and one precise
    // terminal slot. This is the minimum linear shape that repays one region entry.
    let mut native_interp = fresh();
    let mut native_lin = fresh();
    let mut m = vec![0u8; 0x1000];
    m[0x1ff] = 0x90; // starter so the region entry is a continuation
    for offset in (0..10).step_by(2) {
        m[0x200 + offset] = 0x8b;
        m[0x201 + offset] = 0xc3; // mov eax,ebx
    }
    m[0x20a] = 0xf4;
    let mut native_interp_bus = TestBus::with_memory(m.clone());
    let mut native_bus = TestBus::with_memory(m);
    for cpu in [&mut native_interp, &mut native_lin] {
        cpu.registers.eip = 0x1ff;
        cpu.registers.set_eax(0);
        cpu.registers.set_ebx(0x1234_5678);
    }
    drive_to_halt(&mut native_interp, &mut native_interp_bus);
    drive_to_halt(&mut native_lin, &mut native_bus);
    assert_eq!(native_interp, native_lin, "warm phases must match");
    let native_region = jit::block::try_admit_gated(&mut native_lin, 0x200, true, true)
        .expect("four native interior slots should admit a hot linear block");
    {
        let region = native_lin.jit_regions.get_mut(native_region).unwrap();
        assert!(!region.is_loop);
        assert_eq!(region.ctx.slots.len(), 5);
    }
    native_lin
        .decode_cache
        .stamp_region(0x200, true, native_region);
    for cpu in [&mut native_interp, &mut native_lin] {
        cpu.registers.eip = 0x1ff;
        cpu.registers.set_eax(0);
        cpu.registers.set_ebx(0x89ab_cdef);
    }
    let entries_before = native_lin.perf_counters().jit_region_entries;
    drive_to_halt(&mut native_interp, &mut native_interp_bus);
    drive_to_halt(&mut native_lin, &mut native_bus);
    assert_eq!(
        native_interp, native_lin,
        "native linear execution diverged"
    );
    assert_eq!(native_interp_bus.memory, native_bus.memory);
    assert!(native_lin.perf_counters().jit_region_entries > entries_before);
    assert!(native_lin.perf_counters().jit_native_insns >= 4);

    // A self-loop (the byte-copy loop) still admits with the gate on.
    let mut lp = fresh();
    let mut bus_p = TestBus::with_memory({
        let mut mm = h_copy_program();
        mm[H_COUNT..H_COUNT + 4].copy_from_slice(&2u32.to_le_bytes());
        mm
    });
    h_arm(0x2000, 0x3000)(&mut lp);
    drive_to_halt(&mut lp, &mut bus_p);
    let region = jit::block::try_admit_gated(&mut lp, H_ENTRY, true, true);
    assert!(
        region.is_some(),
        "a self-loop must still admit under the linear-block gate"
    );
    assert!(
        lp.jit_regions.get_mut(region.unwrap()).unwrap().is_loop,
        "the admitted region is a self-loop"
    );

    // Production unpaged ring-0 protected mode is the V86 monitor. Auto-admission stays out,
    // while the forced/test path remains available for the protected-mode differential suites.
    lp.control.cr0 |= CR0_PE;
    lp.cpl = 0;
    assert!(lp.is_ring0_protected());
    assert!(jit::block::try_admit_gated(&mut lp, H_ENTRY, true, true).is_none());
    assert!(jit::block::try_admit_gated(&mut lp, H_ENTRY, true, false).is_some());
}

// ---- Exact native byte-memory differential ----

/// `h_arm` plus a flat DS, matching the unreal-mode setup used by DOS extenders.
fn flat_ds_arm(esi: u32, edi: u32) -> impl Fn(&mut CpuGsw) {
    move |cpu: &mut CpuGsw| {
        h_arm(esi, edi)(cpu);
        let mut ds = cpu.registers.segment(SegmentIndex::Ds);
        ds.limit = u32::MAX;
        cpu.registers.set_segment(SegmentIndex::Ds, ds);
    }
}

#[test]
fn native_memory_linear_block_passes_the_admission_gate() {
    let mut program = vec![0u8; 0x1000];
    program[0x1ff] = 0x90;
    program[0x200..0x20d].copy_from_slice(&[
        0x8a, 0x06, // mov al,[esi]
        0x88, 0x07, // mov [edi],al
        0x8a, 0x4e, 0x01, // mov cl,[esi+1]
        0x88, 0x4f, 0x01, // mov [edi+1],cl
        0x8a, 0x56, 0x02, // mov dl,[esi+2]
    ]);
    program[0x20d] = 0xf4;
    program[0x300..0x303].copy_from_slice(&[0x12, 0x34, 0x56]);

    let mut nonflat = fresh();
    let mut nonflat_bus = TestBus::with_memory(program.clone());
    nonflat.registers.eip = 0x1ff;
    nonflat.write_gpr32(6, 0x300);
    nonflat.write_gpr32(7, 0x400);
    drive_to_halt(&mut nonflat, &mut nonflat_bus);
    assert!(
        jit::block::try_admit_gated(&mut nonflat, 0x200, true, true).is_none(),
        "a linear memory block must not admit when DS disables its native slots"
    );

    let arm = |cpu: &mut CpuGsw| {
        flat_ds_arm(0x300, 0x400)(cpu);
        cpu.registers.eip = 0x1ff;
        cpu.halted = false;
    };
    let mut interp = fresh();
    let mut jit_cpu = fresh();
    let mut bus_i = TestBus::with_memory(program.clone());
    let mut bus_j = TestBus::with_memory(program);
    bus_i.direct_pages_enabled = true;
    bus_j.direct_pages_enabled = true;
    arm(&mut interp);
    arm(&mut jit_cpu);
    drive_to_halt(&mut interp, &mut bus_i);
    drive_to_halt(&mut jit_cpu, &mut bus_j);
    let idx = jit::block::try_admit_gated(&mut jit_cpu, 0x200, true, true)
        .expect("four native memory interiors must admit");
    jit_cpu.decode_cache.stamp_region(0x200, true, idx);
    arm(&mut interp);
    arm(&mut jit_cpu);
    drive_to_halt(&mut interp, &mut bus_i);
    drive_to_halt(&mut jit_cpu, &mut bus_j);
    assert_state_identical(&interp, &jit_cpu);
    assert_eq!(interp.elapsed_clocks, jit_cpu.elapsed_clocks);
    assert_eq!(bus_i.memory, bus_j.memory);
    assert!(jit_cpu.perf_counters().jit_native_load_hits > 0);
    assert!(jit_cpu.perf_counters().jit_native_store_hits > 0);
}

/// Run one direct-page workload through the interpreter and JIT and compare architecture, timing,
/// and memory exactly. Both final CPUs are returned for counter and invalidation assertions.
fn assert_native_memory_identical(
    mode: GswMode,
    prog: Vec<u8>,
    arm: &dyn Fn(&mut CpuGsw),
    expect_native_hits: Option<bool>,
    expect_store_hits: Option<bool>,
    expect_region: bool,
) -> (CpuGsw, CpuGsw) {
    let mut interp = fresh_for(mode);
    let mut jit_cpu = fresh_for(mode);
    jit_cpu.set_legacy_region_auto_admit(true);
    let mut bus_i = TestBus::with_memory(prog.clone());
    let mut bus_j = TestBus::with_memory(prog);
    bus_i.direct_pages_enabled = true;
    bus_j.direct_pages_enabled = true;
    arm(&mut interp);
    arm(&mut jit_cpu);
    drive_to_halt(&mut interp, &mut bus_i);
    drive_to_halt(&mut jit_cpu, &mut bus_j);
    assert_state_identical(&interp, &jit_cpu);
    assert_eq!(
        interp.eflags(),
        jit_cpu.eflags(),
        "materialized eflags diverged"
    );
    assert_eq!(interp.elapsed_clocks, jit_cpu.elapsed_clocks);
    assert_eq!(interp.core_clocks_so_far, jit_cpu.core_clocks_so_far);
    assert_eq!(interp.timing_rem, jit_cpu.timing_rem);
    assert_eq!(interp.fp_rem, jit_cpu.fp_rem);
    assert_eq!(bus_i.memory, bus_j.memory, "guest memory diverged");
    assert_eq!(
        interp.perf_counters().jit_region_entries,
        0,
        "the interpreter CPU must never compile"
    );
    assert_eq!(
        interp.perf_counters().jit_native_load_hits,
        0,
        "the interpreter must never run a native load slot"
    );
    assert_eq!(
        interp.perf_counters().jit_native_store_hits,
        0,
        "the interpreter must never run a native store slot"
    );
    if expect_region {
        assert!(
            jit_cpu.perf_counters().jit_region_entries > 0,
            "the JIT must have compiled and run a region"
        );
    }
    let check = |actual: u64, expect: Option<bool>, what: &str| match expect {
        Some(true) => assert!(actual > 0, "the native {what} path never ran"),
        Some(false) => {
            assert_eq!(actual, 0, "the native {what} path must be gated off here")
        }
        None => {}
    };
    check(
        jit_cpu.perf_counters().jit_native_load_hits,
        expect_native_hits,
        "LOAD",
    );
    check(
        jit_cpu.perf_counters().jit_native_store_hits,
        expect_store_hits,
        "STORE",
    );
    assert_eq!(
        jit_cpu.perf_counters().jit_native_memory_helpers,
        jit_cpu.perf_counters().jit_native_load_hits
            + jit_cpu.perf_counters().jit_native_store_hits,
        "the direct-page fixture must account for every byte-memory helper call"
    );
    (interp, jit_cpu)
}

/// Real-mode, flat DS, unpaged byte-copy loop through the native direct-page path.
#[test]
fn native_real_mode_copy_loop_is_exact() {
    let build = |count: u32| -> Vec<u8> {
        let mut m = h_copy_program();
        m[H_COUNT..H_COUNT + 4].copy_from_slice(&count.to_le_bytes());
        m
    };
    assert_native_memory_identical(
        GswMode::Gsw586,
        build(200),
        &flat_ds_arm(0x2000, 0x3000),
        Some(true),
        Some(true),
        true,
    );
}

/// Base+INDEX load (`mov al,[esi+ecx]`, scale 1) lowered natively and kept exact. The copy
/// loop above has no index; the real R_DrawColumn loads are `[esi+ecx]`/`[esi+edx]`, so this
/// exercises the probe's index-EA path (`add_r32_r32`) in an integrated multi-iteration loop, not
/// just the isolation test. `ecx` is a fixed in-page offset; `esi` walks within the cached page.
#[test]
fn native_index_load_is_exact() {
    let prog = {
        let mut m = vec![0u8; 0x1_0000];
        m[0x100] = 0x90; // nop starter -> 0x101 reached as a continuation
        let body: [u8; 14] = [
            0x8a, 0x04, 0x0e, // mov al,[esi+ecx]  (base esi, index ecx, scale 1)
            0x88, 0x07, // mov [edi],al
            0x46, // inc esi
            0x47, // inc edi
            0xff, 0x0d, 0x00, 0x04, 0x00, 0x00, // dec dword [H_COUNT]
            0x75, // jnz rel8
        ];
        m[0x101..0x101 + body.len()].copy_from_slice(&body);
        let rel_at = 0x101 + body.len();
        m[rel_at] = ((0x101i32) - (rel_at as i32 + 1)) as i8 as u8;
        m[rel_at + 1] = 0xf4; // hlt at the fall-through
        m[H_COUNT..H_COUNT + 4].copy_from_slice(&200u32.to_le_bytes());
        m
    };
    let arm = move |cpu: &mut CpuGsw| {
        flat_ds_arm(0x2000, 0x3000)(cpu);
        cpu.write_gpr32(1, 0x40); // ecx = a fixed in-page index offset; load reads [esi+0x40]
    };
    assert_native_memory_identical(GswMode::Gsw586, prog, &arm, Some(true), Some(true), true);
}

/// Native register groups and a direct byte load retain exact state and timing together.
#[test]
fn native_alu_and_memory_slots_are_exact() {
    let prog = {
        let mut m = vec![0u8; 0x1_0000];
        m[0x100] = 0x90; // nop starter -> 0x101 reached as a continuation
        let mut body: Vec<u8> = vec![
            0x8b, 0xc8, // mov ecx,eax        (RegMov)
            0x81, 0xc1, 0x11, 0x22, 0x00, 0x00, // add ecx,0x2211  (RegAddImm)
            0xc1, 0xe9, 0x01, // shr ecx,1          (RegShrImm)
            0x8a, 0x06, // mov al,[esi]       (MemLoadU8)
            0xff, 0x0d, // dec dword [disp32] ...
        ];
        body.extend_from_slice(&(H_COUNT as u32).to_le_bytes()); // dec dword [H_COUNT]
        body.push(0x75); // jnz rel8
        let jnz_at = 0x101 + body.len() - 1; // linear addr of the jnz opcode
        let after = jnz_at + 2;
        body.push((0x101i32 - after as i32) as i8 as u8);
        m[0x101..0x101 + body.len()].copy_from_slice(&body);
        m[0x101 + body.len()] = 0xf4; // hlt at the loop fall-through
        m[H_COUNT..H_COUNT + 4].copy_from_slice(&200u32.to_le_bytes());
        m
    };
    let arm = move |cpu: &mut CpuGsw| {
        flat_ds_arm(0x2000, 0x3000)(cpu);
        cpu.write_gpr32(0, 0x1357); // eax feeds the mov/add/shr chain
    };
    assert_native_memory_identical(GswMode::Gsw586, prog, &arm, Some(true), None, true);
}

/// A non-identity paged map exercises the same native implementation in every GSW mode.
#[test]
fn native_paged_copy_loop_is_exact_in_all_modes() {
    for mode in [
        GswMode::Gsw586,
        GswMode::Gsw486,
        GswMode::Gsw386,
        GswMode::Gsw386Slow,
    ] {
        let (interp, _) = assert_native_memory_identical(
            mode,
            paged_copy_program(200),
            &pg_arm(PG_SRC_LIN, PG_DST_LIN),
            Some(true),
            Some(true),
            true,
        );
        assert!(
            interp.is_paging_enabled(),
            "{mode:?} must keep paging enabled"
        );
    }
}

#[test]
fn native_memory_preflights_finite_caps_in_all_modes() {
    let drive = |cpu: &mut CpuGsw, bus: &mut TestBus, cap: u64| {
        let mut calls = Vec::new();
        for _ in 0..10_000 {
            let outcome = cpu.run_straight_line(bus, cap).unwrap();
            calls.push((outcome.core_clocks, cpu.registers.eip));
            if outcome.halted {
                return calls;
            }
        }
        panic!("guest never halted");
    };
    for mode in [
        GswMode::Gsw586,
        GswMode::Gsw486,
        GswMode::Gsw386,
        GswMode::Gsw386Slow,
    ] {
        for (cap, direct_bound) in [(3, None), (7, None), (13, None), (13, Some(64))] {
            let mut program = h_copy_program();
            program[H_COUNT..H_COUNT + 4].copy_from_slice(&2u32.to_le_bytes());
            let mut interp = fresh_for(mode);
            let mut jit_cpu = fresh_for(mode);
            let mut bus_i = TestBus::with_memory(program.clone());
            let mut bus_j = TestBus::with_memory(program);
            bus_i.direct_pages_enabled = true;
            bus_j.direct_pages_enabled = true;
            if cap == 13 {
                bus_j.project_additional_bus_clocks = true;
            }
            if let Some(bound) = direct_bound {
                // Model a direct device page whose access cost exceeds the remaining event cap.
                // The region must execute the memory slot through its precise helper instead.
                bus_j.direct_memory_max_clock_override = Some(bound);
            }
            flat_ds_arm(0x2000, 0x3000)(&mut interp);
            flat_ds_arm(0x2000, 0x3000)(&mut jit_cpu);
            drive_to_halt(&mut interp, &mut bus_i);
            drive_to_halt(&mut jit_cpu, &mut bus_j);
            let idx =
                jit::block::try_admit(&mut jit_cpu, H_ENTRY, true).expect("admit byte-copy loop");
            jit_cpu.decode_cache.stamp_region(H_ENTRY, true, idx);

            for (cpu, bus) in [(&mut interp, &mut bus_i), (&mut jit_cpu, &mut bus_j)] {
                flat_ds_arm(0x2000, 0x3000)(cpu);
                cpu.halted = false;
                bus.memory[H_COUNT..H_COUNT + 4].copy_from_slice(&12u32.to_le_bytes());
            }
            interp.reset_perf_counters();
            jit_cpu.reset_perf_counters();
            let calls_i = drive(&mut interp, &mut bus_i, cap);
            let calls_j = drive(&mut jit_cpu, &mut bus_j, cap);
            assert_eq!(
                calls_i, calls_j,
                "{mode:?}, cap {cap}, bound {direct_bound:?}"
            );
            assert_state_identical(&interp, &jit_cpu);
            assert_eq!(interp.elapsed_clocks, jit_cpu.elapsed_clocks);
            assert_eq!(bus_i.memory, bus_j.memory);
            let perf = jit_cpu.perf_counters();
            assert!(perf.jit_native_memory_helpers > 0, "{mode:?}, cap {cap}");
            if cap == 3 || direct_bound.is_some() {
                assert!(
                    perf.jit_native_memory_helpers
                        > perf.jit_native_load_hits + perf.jit_native_store_hits,
                    "{mode:?}, cap {cap}, bound {direct_bound:?}: no cap fallback"
                );
            }
            if cap == 13 && direct_bound.is_none() {
                assert!(
                    perf.jit_native_load_hits + perf.jit_native_store_hits > 0,
                    "{mode:?}: cap-13 control recorded no native memory hits"
                );
            }
            if direct_bound.is_some() {
                assert_eq!(
                    perf.jit_native_load_hits + perf.jit_native_store_hits,
                    0,
                    "{mode:?}: high-latency device access ran natively"
                );
            }
        }
    }
}

#[test]
fn native_paged_permissions_side_exit_in_all_modes() {
    let modes = [
        GswMode::Gsw586,
        GswMode::Gsw486,
        GswMode::Gsw386,
        GswMode::Gsw386Slow,
    ];
    for mode in modes {
        for (writable, user, expected_cr2, load_hits) in
            [(true, false, PG_SRC_LIN, 0), (false, true, PG_DST_LIN, 1)]
        {
            let mut cpu = fresh_for(mode);
            let mut bus = TestBus::with_memory(paged_copy_program(2));
            bus.direct_pages_enabled = true;
            pg_arm(PG_SRC_LIN, PG_DST_LIN)(&mut cpu);
            drive_to_halt(&mut cpu, &mut bus);
            let idx =
                jit::block::try_admit(&mut cpu, PG_ENTRY_LIN, true).expect("admit paged copy loop");
            cpu.decode_cache.stamp_region(PG_ENTRY_LIN, true, idx);

            pg_arm(PG_SRC_LIN, PG_DST_LIN)(&mut cpu);
            cpu.halted = false;
            cpu.cpl = 3;
            cpu.idtr.limit = 0;
            let count_phys = PG_DATA_PHYS + (PG_COUNT_LIN - PG_DATA_LIN) as usize;
            bus.memory[count_phys..count_phys + 4].copy_from_slice(&1u32.to_le_bytes());
            cpu.tlb
                .insert(PG_DATA_LIN >> 12, PG_DATA_PHYS as u32, writable, user, true);
            #[cfg(all(
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            cpu.jit_fast_map.invalidate_all();
            cpu.reset_perf_counters();
            assert!(
                cpu.run_straight_line(&mut bus, u64::MAX).is_err(),
                "{mode:?} must deliver the page fault"
            );
            assert_eq!(cpu.control.cr2, expected_cr2, "{mode:?} CR2");
            assert!(cpu.perf_counters().jit_region_entries > 0, "{mode:?}");
            assert_eq!(
                cpu.perf_counters().jit_native_load_hits,
                load_hits,
                "{mode:?} load fast-path count"
            );
            assert_eq!(cpu.perf_counters().jit_native_store_hits, 0, "{mode:?}");
        }
    }
}

#[test]
fn native_paged_store_falls_back_to_set_the_dirty_bit_in_all_modes() {
    let data_pte = 0x2000 + (PG_DATA_LIN as usize >> 12) * 4;
    let count_phys = PG_DATA_PHYS + (PG_COUNT_LIN - PG_DATA_LIN) as usize;
    let dst_phys = PG_DATA_PHYS + (PG_DST_LIN - PG_DATA_LIN) as usize;
    for mode in [
        GswMode::Gsw586,
        GswMode::Gsw486,
        GswMode::Gsw386,
        GswMode::Gsw386Slow,
    ] {
        let mut program = paged_copy_program(2);
        program[PG_DATA_PHYS] = 0x5a;
        let mut interp = fresh_for(mode);
        let mut jit_cpu = fresh_for(mode);
        let mut bus_i = TestBus::with_memory(program.clone());
        let mut bus_j = TestBus::with_memory(program);
        bus_i.direct_pages_enabled = true;
        bus_j.direct_pages_enabled = true;
        pg_arm(PG_SRC_LIN, PG_DST_LIN)(&mut interp);
        pg_arm(PG_SRC_LIN, PG_DST_LIN)(&mut jit_cpu);
        drive_to_halt(&mut interp, &mut bus_i);
        drive_to_halt(&mut jit_cpu, &mut bus_j);
        let idx =
            jit::block::try_admit(&mut jit_cpu, PG_ENTRY_LIN, true).expect("admit paged copy loop");
        jit_cpu.decode_cache.stamp_region(PG_ENTRY_LIN, true, idx);

        for (cpu, bus) in [(&mut interp, &mut bus_i), (&mut jit_cpu, &mut bus_j)] {
            pg_arm(PG_SRC_LIN, PG_DST_LIN)(cpu);
            cpu.halted = false;
            cpu.tlb
                .insert(PG_DATA_LIN >> 12, PG_DATA_PHYS as u32, true, true, false);
            #[cfg(all(
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            cpu.jit_fast_map.invalidate_all();
            bus.memory[data_pte..data_pte + 4]
                .copy_from_slice(&((PG_DATA_PHYS as u32) | 0x007).to_le_bytes());
            bus.memory[count_phys..count_phys + 4].copy_from_slice(&1u32.to_le_bytes());
            bus.memory[dst_phys] = 0;
        }
        interp.reset_perf_counters();
        jit_cpu.reset_perf_counters();
        drive_to_halt(&mut interp, &mut bus_i);
        drive_to_halt(&mut jit_cpu, &mut bus_j);
        assert_state_identical(&interp, &jit_cpu);
        assert_eq!(interp.elapsed_clocks, jit_cpu.elapsed_clocks, "{mode:?}");
        assert_eq!(bus_i.memory, bus_j.memory, "{mode:?}");
        let pte = u32::from_le_bytes(bus_j.memory[data_pte..data_pte + 4].try_into().unwrap());
        assert_ne!(pte & 0x40, 0, "{mode:?}: the store must set PTE.D");
        assert_eq!(bus_j.memory[dst_phys], 0x5a, "{mode:?}");
        assert!(jit_cpu.perf_counters().jit_native_load_hits > 0, "{mode:?}");
        assert_eq!(jit_cpu.perf_counters().jit_native_store_hits, 0, "{mode:?}");
    }
}

#[test]
fn native_paged_supervisor_store_obeys_cr0_wp() {
    let data_pte = 0x2000 + (PG_DATA_LIN as usize >> 12) * 4;
    let count_phys = PG_DATA_PHYS + (PG_COUNT_LIN - PG_DATA_LIN) as usize;
    let dst_phys = PG_DATA_PHYS + (PG_DST_LIN - PG_DATA_LIN) as usize;
    for mode in [GswMode::Gsw586, GswMode::Gsw486] {
        for wp in [false, true] {
            let mut cpu = fresh_for(mode);
            let mut bus = TestBus::with_memory(paged_copy_program(2));
            bus.direct_pages_enabled = true;
            pg_arm(PG_SRC_LIN, PG_DST_LIN)(&mut cpu);
            drive_to_halt(&mut cpu, &mut bus);
            let idx =
                jit::block::try_admit(&mut cpu, PG_ENTRY_LIN, true).expect("admit paged copy loop");
            cpu.decode_cache.stamp_region(PG_ENTRY_LIN, true, idx);

            pg_arm(PG_SRC_LIN, PG_DST_LIN)(&mut cpu);
            cpu.halted = false;
            cpu.idtr.limit = 0;
            cpu.control.cr0 = if wp {
                cpu.control.cr0 | CR0_WP
            } else {
                cpu.control.cr0 & !CR0_WP
            };
            cpu.tlb
                .insert(PG_DATA_LIN >> 12, PG_DATA_PHYS as u32, false, true, true);
            #[cfg(all(
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            cpu.jit_fast_map.invalidate_all();
            bus.memory[data_pte..data_pte + 4]
                .copy_from_slice(&((PG_DATA_PHYS as u32) | 0x065).to_le_bytes());
            bus.memory[count_phys..count_phys + 4].copy_from_slice(&1u32.to_le_bytes());
            bus.memory[PG_DATA_PHYS] = 0x5a;
            bus.memory[dst_phys] = 0;
            cpu.reset_perf_counters();

            if wp {
                assert!(
                    cpu.run_straight_line(&mut bus, u64::MAX).is_err(),
                    "{mode:?}"
                );
                assert_eq!(cpu.control.cr2, PG_DST_LIN, "{mode:?}");
                assert_eq!(bus.memory[dst_phys], 0, "{mode:?}");
                assert_eq!(cpu.perf_counters().jit_native_store_hits, 0, "{mode:?}");
            } else {
                drive_to_halt(&mut cpu, &mut bus);
                assert_eq!(bus.memory[dst_phys], 0x5a, "{mode:?}");
                assert!(cpu.perf_counters().jit_native_store_hits > 0, "{mode:?}");
            }
            assert!(cpu.perf_counters().jit_region_entries > 0, "{mode:?}");
            assert!(cpu.perf_counters().jit_native_load_hits > 0, "{mode:?}");
            let pte = u32::from_le_bytes(bus.memory[data_pte..data_pte + 4].try_into().unwrap());
            assert_eq!(pte & 0x60, 0x60, "{mode:?}: PTE A/D state");
        }
    }
}

/// A same-value store into live code must not invalidate the compiled block.
#[test]
fn native_self_modifying_store_stays_exact() {
    let prog = {
        let mut m = h_copy_program();
        m[H_COUNT..H_COUNT + 4].copy_from_slice(&40u32.to_le_bytes());
        m
    };
    let (interp, jit_cpu) = assert_native_memory_identical(
        GswMode::Gsw586,
        prog,
        &flat_ds_arm(H_ENTRY, H_ENTRY),
        Some(true),
        Some(true),
        true,
    );
    for (name, pc) in [
        ("interpreter", interp.perf_counters()),
        ("JIT", jit_cpu.perf_counters()),
    ] {
        assert_eq!(pc.smc_narrow_kills, 0, "{name} narrow invalidation");
        assert_eq!(pc.decode_inval_smc, 0, "{name} global invalidation");
    }
    assert_eq!(
        interp.perf_counters().code_invalidations,
        jit_cpu.perf_counters().code_invalidations
    );
}

#[test]
fn native_changed_store_exits_before_a_stale_register_group() {
    const ENTRY: u32 = 0x101;
    const PATCH: u32 = 0x107;
    let mut program = vec![0u8; 0x1000];
    program[0x100] = 0x90;
    program[0x101..0x10f].copy_from_slice(&[
        0x88, 0x07, // mov [edi],al
        0x8b, 0xdb, // mov ebx,ebx
        0x81, 0xc1, 0x01, 0x00, 0x00, 0x00, // add ecx,1
        0x8b, 0xd2, // mov edx,edx
        0x8b, 0xf6, // mov esi,esi (terminal helper)
    ]);
    program[0x10f] = 0xf4;

    let arm = |cpu: &mut CpuGsw, patch: u8| {
        flat_ds_arm(0, PATCH)(cpu);
        cpu.registers.eip = 0x100;
        cpu.registers.set_eax(u32::from(patch));
        cpu.registers.set_ecx(0);
        cpu.halted = false;
    };
    let mut interp = fresh();
    let mut jit_cpu = fresh();
    let mut bus_i = TestBus::with_memory(program.clone());
    let mut bus_j = TestBus::with_memory(program);
    bus_i.direct_pages_enabled = true;
    bus_j.direct_pages_enabled = true;
    arm(&mut interp, 1);
    arm(&mut jit_cpu, 1);
    drive_to_halt(&mut interp, &mut bus_i);
    drive_to_halt(&mut jit_cpu, &mut bus_j);
    let idx = jit::block::try_admit(&mut jit_cpu, ENTRY, true).expect("admit SMC block");
    jit_cpu.decode_cache.stamp_region(ENTRY, true, idx);

    arm(&mut interp, 2);
    arm(&mut jit_cpu, 2);
    drive_to_halt(&mut interp, &mut bus_i);
    drive_to_halt(&mut jit_cpu, &mut bus_j);
    assert_eq!(bus_i.memory[PATCH as usize], 2);
    assert_eq!(interp.registers.ecx(), 2);
    assert_state_identical(&interp, &jit_cpu);
    assert_eq!(interp.elapsed_clocks, jit_cpu.elapsed_clocks);
    assert_eq!(bus_i.memory, bus_j.memory);
    assert!(jit_cpu.perf_counters().jit_native_store_hits > 0);
    assert!(jit_cpu.perf_counters().smc_narrow_kills > 0);
}

/// Native entry gates use the same descriptor matrix as the interpreter and are rechecked after a
/// region has already been compiled.
#[test]
fn native_ds_access_gates_match_protected_mode() {
    let flat = |access: u8| SegmentRegister {
        selector: 0x08,
        base: 0,
        limit: 0xffff_ffff,
        access,
        default_size_32: true,
    };
    let setup = |cpu: &mut CpuGsw, ds_access: u8| {
        cpu.registers.set_segment(SegmentIndex::Cs, flat(0x9b)); // exec/read
        cpu.registers.set_segment(SegmentIndex::Ds, flat(ds_access));
        cpu.registers.set_segment(SegmentIndex::Ss, flat(0x93)); // r/w stack
        cpu.registers.set_segment(SegmentIndex::Es, flat(0x93));
        cpu.cpl = 0;
        cpu.control.cr0 |= CR0_PE; // protected, UNPAGED (PG stays clear)
        h_arm(0x2000, 0x3000)(cpu); // eip/esp/esi/edi + flags
    };
    for (access, readable, writable) in [
        (0x90, true, false),
        (0x92, true, true),
        (0x98, false, false),
        (0x9a, true, false),
    ] {
        let mut cpu = fresh();
        setup(&mut cpu, access);
        assert_eq!(cpu.jit_segment_readable(SegmentIndex::Ds), readable);
        assert_eq!(cpu.jit_segment_writable(SegmentIndex::Ds), writable);
    }

    for (forbidden_access, load_hits) in [(0x90, 1), (0x98, 0)] {
        let mut prog = h_copy_program();
        prog[H_COUNT..H_COUNT + 4].copy_from_slice(&8u32.to_le_bytes());
        let mut cpu = fresh();
        let mut bus = TestBus::with_memory(prog);
        bus.direct_pages_enabled = true;
        setup(&mut cpu, 0x92);
        drive_to_halt(&mut cpu, &mut bus);
        let idx = jit::block::try_admit(&mut cpu, H_ENTRY, true).expect("admit copy loop");
        {
            let region = cpu.jit_regions.get_mut(idx).unwrap();
            assert!(region.has_native_load);
            assert!(region.has_native_store);
        }
        cpu.decode_cache.stamp_region(H_ENTRY, true, idx);
        setup(&mut cpu, forbidden_access);
        cpu.halted = false;
        bus.memory[H_COUNT..H_COUNT + 4].copy_from_slice(&8u32.to_le_bytes());
        cpu.reset_perf_counters();
        assert!(
            cpu.run_straight_line(&mut bus, u64::MAX).is_err(),
            "the forbidden DS access must take the interpreter fault path"
        );
        assert!(cpu.perf_counters().jit_region_entries > 0);
        assert_eq!(cpu.perf_counters().jit_native_load_hits, load_hits);
        assert_eq!(cpu.perf_counters().jit_native_store_hits, 0);
    }
}

// ---- Unit-simulator feed from the interpreter (Track C, Task 2) ----
//
// These tests drive small guest programs through the plain interpreter (no JIT admission, so
// `run_budgeted`'s native continuation path stays off and every retired instruction flows through
// an observe site) and check the trace the simulator reconstructs. The simulator never influences
// execution, so a sim-on run must match a sim-off run bit-for-bit.

/// The unit-sim probe loop base. A NOP starter so the loop head is reached as a continuation,
/// then a small ALU body decrementing ECX (all `0x83`-form, so continuable and fall-through), and
/// a `jnz` rel8 self-loop back-edge; a trailing HLT at the fall-through halts the machine.
const USIM_START: u32 = 0x200;
const USIM_LOOP: u32 = 0x201;

/// Build the probe program with `body_adds` leading `add r32,1` instructions before the
/// `sub ecx,1` / `jnz` back-edge. `body_adds = 3` gives the "few ALU ops + a near branch" shape;
/// `body_adds = 0` gives the tight two-instruction self-loop.
fn usim_program(body_adds: usize) -> Vec<u8> {
    let mut m = vec![0u8; 0x1000];
    m[USIM_START as usize] = 0x90; // nop starter
    let mut body: Vec<u8> = Vec::new();
    // add eax,1 / add ebx,1 / add edx,1 (0x83 /0, ModRM mode 3): continuable ALU, no transfer.
    for &rm in [0xc0u8, 0xc3, 0xc2].iter().take(body_adds) {
        body.extend_from_slice(&[0x83, rm, 0x01]);
    }
    body.extend_from_slice(&[0x83, 0xe9, 0x01]); // sub ecx,1 (0x83 /5): sets ZF at ecx==0
    let jnz_at = USIM_LOOP as usize + body.len();
    let rel = (USIM_LOOP as i32 - (jnz_at as i32 + 2)) as i8;
    body.push(0x75); // jnz rel8
    body.push(rel as u8);
    m[USIM_LOOP as usize..USIM_LOOP as usize + body.len()].copy_from_slice(&body);
    m[USIM_LOOP as usize + body.len()] = 0xf4; // hlt at the loop fall-through
    m
}

fn usim_arm(cpu: &mut CpuGsw, count: u32) {
    cpu.registers.eip = USIM_START;
    cpu.registers.set_esp(0x0700);
    cpu.registers.set_eax(0);
    cpu.registers.set_ebx(0);
    cpu.registers.set_edx(0);
    cpu.registers.set_ecx(count);
}

/// Enabling the simulator must not perturb architectural state (it is excluded from `CpuGsw`
/// equality), and a sim-on run must produce a non-empty trace.
#[test]
fn unit_sim_feed_is_state_neutral() {
    let mut sim_off = fresh();
    let mut sim_on = fresh();
    let mut bus_off = TestBus::with_memory(usim_program(3));
    let mut bus_on = TestBus::with_memory(usim_program(3));

    usim_arm(&mut sim_off, 6);
    usim_arm(&mut sim_on, 6);
    sim_on.set_unit_sim_enabled(true);

    drive_to_halt(&mut sim_off, &mut bus_off);
    drive_to_halt(&mut sim_on, &mut bus_on);

    // The sim slot is excluded from equality, so the two CPUs must compare equal despite one
    // carrying an enabled simulator. (If the field ever leaked into the derived PartialEq this
    // assertion would fail, which is exactly the signal the plan calls for.)
    assert_eq!(sim_off, sim_on, "enabling the unit sim changed CPU state");
    assert_eq!(bus_off.memory, bus_on.memory, "guest memory diverged");

    let reports = sim_on
        .take_unit_sim_report()
        .expect("the sim was enabled, so a report exists");
    // The wired ladder fans out to the measurement set {L0, L4, L5, L6}; every rung sees the same
    // non-empty trace.
    assert_eq!(
        reports.len(),
        4,
        "the ladder must report the four measurement rungs"
    );
    for (cfg, report, histogram) in &reports {
        assert!(report.entries > 0, "rung {cfg} recorded no entries");
        assert!(
            report.retired_in_units > 0,
            "rung {cfg} accrued no retired instructions"
        );
        assert!(
            !histogram.is_empty(),
            "rung {cfg} should show at least one unit in the histogram"
        );
    }
    // Taking the report disables the sim.
    assert!(
        sim_on.take_unit_sim_report().is_none(),
        "the report was already taken; the sim is disabled"
    );
}

/// Coverage invariant (review finding B2): every retired interpreter instruction is observed
/// exactly once, so `retired_in_units` equals the run's `perf.instructions` delta. Each `observe`
/// accrues exactly once, so `retired_in_units` is the total observed count.
///
/// The ladder fans one stream to every wired rung, and each observed instruction accrues to exactly
/// one open entry in every rung (a deferred L2/L4/L5 check keeps the entry open across the closing
/// transfer, but that transfer already accrued, and the next instruction accrues to the switched
/// unit or a fresh entry either way; the L6 io call-out likewise accrues and keeps the entry open).
/// So `retired_in_units` is config-independent: EVERY rung must report the same total, equal to the
/// perf delta. A rung that differs is a ladder bug, not a test artifact.
#[test]
fn unit_sim_observes_every_retired_instruction() {
    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(usim_program(3));
    usim_arm(&mut cpu, 9);
    cpu.set_unit_sim_enabled(true);
    cpu.reset_perf_counters();

    drive_to_halt(&mut cpu, &mut bus);

    let retired = cpu.perf_counters().instructions;
    let reports = cpu.take_unit_sim_report().expect("sim enabled");
    assert!(retired > 0, "the program retired no instructions");
    assert_eq!(
        reports.len(),
        4,
        "the ladder must report the four measurement rungs"
    );
    for (cfg, report, _) in &reports {
        assert_eq!(
            report.retired_in_units, retired,
            "rung {cfg} must observe every retired instruction exactly once \
             (retired_in_units {} vs perf.instructions {retired})",
            report.retired_in_units,
        );
    }
}

/// A tight two-instruction self-loop: the `jnz` back-edge is a `DirectNear` transfer to the loop
/// head, so the open entry stays open across it and the fall-through body accrues to the same
/// entry. `entries` is therefore bounded by the batch count (one open entry per batch), never by
/// the far larger retired-instruction count.
#[test]
fn unit_sim_self_loop_keeps_entry_open() {
    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(usim_program(0));
    usim_arm(&mut cpu, 60);
    cpu.set_unit_sim_enabled(true);
    cpu.reset_perf_counters();

    drive_to_halt(&mut cpu, &mut bus);

    let batches = cpu.perf_counters().straight_line_runs;
    let reports = cpu.take_unit_sim_report().expect("sim enabled");
    // Assert on the L0 rung (v1 parity): its `DirectNear` back-edge keeps the entry open.
    let (cfg, report, _) = &reports[0];
    assert_eq!(*cfg, "L0", "the first ladder rung is L0");

    // The loop runs 60 iterations of `sub ecx,1; jnz`, so retired is large.
    assert!(
        report.retired_in_units >= 100,
        "expected a long retired stream, got {}",
        report.retired_in_units
    );
    // Exactly one entry opens per batch: the `jnz` back-edge is `DirectNear`, so it keeps the open
    // entry alive and the fall-through `sub` accrues to it rather than opening a second entry. A
    // broken back-edge (treating `jnz` as `Indirect`) would close after the branch and reopen on
    // the body, pushing entries past the batch count. So `entries <= batches` is the discriminating
    // proof, and entries stays strictly below the far larger retired stream.
    assert!(
        report.entries <= batches,
        "entries {} exceeded the batch count {} (back-edge did not keep the entry open)",
        report.entries,
        batches
    );
    assert!(
        report.entries < report.retired_in_units,
        "entries {} should stay below the retired stream {}",
        report.entries,
        report.retired_in_units
    );
}
