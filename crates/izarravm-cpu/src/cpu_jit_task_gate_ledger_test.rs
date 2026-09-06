// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Native task-gate rows for the REP-E return ledger. These enter an installed
//! Direct block and let the real INT or MOV-Sreg helper perform the task switch.

use super::sixteen_bit::{arm_native_sixteen_bit, sixteen_bit_bus, warm_sixteen_bit};
use super::*;

const ENTRY: u32 = 0x100;
const NEXT_TASK_ENTRY: u32 = 0x90;
const GDT_BASE: u32 = 0x1200;
const IDT_BASE: u32 = 0x1400;
const OLD_TSS: u32 = 0x300;
const NEW_TSS: u32 = 0x380;
const SEL_CODE: u16 = 0x08;
const SEL_DATA: u16 = 0x10;
const SEL_NEW_TSS: u16 = 0x40;
const SEL_OLD_TSS: u16 = 0x48;
const SEL_TINY_SS: u16 = 0x50;
const MARKER: u32 = 0x1357_9bdf;

fn descriptor(low: u32, high: u32) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&low.to_le_bytes());
    bytes[4..].copy_from_slice(&high.to_le_bytes());
    bytes
}

fn put_descriptor(memory: &mut [u8], address: u32, descriptor: [u8; 8]) {
    let address = address as usize;
    memory[address..address + 8].copy_from_slice(&descriptor);
}

fn put32(memory: &mut [u8], address: u32, value: u32) {
    let address = address as usize;
    memory[address..address + 4].copy_from_slice(&value.to_le_bytes());
}

fn put16(memory: &mut [u8], address: u32, value: u16) {
    let address = address as usize;
    memory[address..address + 2].copy_from_slice(&value.to_le_bytes());
}

/// Keep the INT row enabled only for the fixture that proves its terminal path.
#[must_use]
struct IntRowsGuard;

impl Drop for IntRowsGuard {
    fn drop(&mut self) {
        jit::direct::set_int_imm8_rows_for_test(None);
    }
}

fn enable_int_rows() -> IntRowsGuard {
    jit::direct::set_int_imm8_rows_for_test(Some(true));
    assert!(jit::direct::int_imm8_rows_armed());
    IntRowsGuard
}

struct NativeTaskGateFixture {
    cpu: CpuGsw,
    bus: TestBus,
    block: jit::direct::CompiledBlock,
}

/// Build a 32-bit protected Direct entry with an old busy TSS and a task-gate
/// target TSS. The native block itself has no synthetic switch owner.
fn native_task_gate_fixture(
    mode: GswMode,
    code: &[u8],
    starts: &[u32],
    expected_instructions: u8,
    expected_int_slots: u8,
    expected_interpret_one_slots: u8,
    tiny_incoming_stack: bool,
) -> NativeTaskGateFixture {
    let mut memory = vec![0u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);
    memory[NEXT_TASK_ENTRY as usize..NEXT_TASK_ENTRY as usize + 2].copy_from_slice(&[0xb0, 0x5a]);

    put_descriptor(
        &mut memory,
        GDT_BASE + u32::from(SEL_CODE),
        descriptor(0x0000_ffff, 0x00cf_9b00),
    );
    put_descriptor(
        &mut memory,
        GDT_BASE + u32::from(SEL_DATA),
        descriptor(0x0000_ffff, 0x00cf_9300),
    );
    put_descriptor(
        &mut memory,
        GDT_BASE + u32::from(SEL_NEW_TSS),
        descriptor(0x0380_0067, 0x0000_8900),
    );
    put_descriptor(
        &mut memory,
        GDT_BASE + u32::from(SEL_OLD_TSS),
        descriptor(0x0300_0067, 0x0000_8b00),
    );
    if tiny_incoming_stack {
        put_descriptor(
            &mut memory,
            GDT_BASE + u32::from(SEL_TINY_SS),
            descriptor(0x0000_00ff, 0x0040_9300),
        );
    }

    // IDT vector 14 is a present task gate whose ignored offset bits stay zero.
    put_descriptor(
        &mut memory,
        IDT_BASE + 14 * 8,
        descriptor(u32::from(SEL_NEW_TSS) << 16, 0x0000_8500),
    );
    // The MOV-Sreg rows raise #GP(SEL_BAD); route that delivery through the
    // same actual task gate rather than invoking delivery directly.
    put_descriptor(
        &mut memory,
        IDT_BASE + 13 * 8,
        descriptor(u32::from(SEL_NEW_TSS) << 16, 0x0000_8500),
    );

    put32(&mut memory, NEW_TSS + 32, NEXT_TASK_ENTRY);
    put32(&mut memory, NEW_TSS + 36, 0x0000_0002);
    put32(&mut memory, NEW_TSS + 40, 0);
    put32(
        &mut memory,
        NEW_TSS + 56,
        if tiny_incoming_stack { 0 } else { 0xf0 },
    );
    put16(&mut memory, NEW_TSS + 72, SEL_DATA);
    put16(&mut memory, NEW_TSS + 76, SEL_CODE);
    put16(
        &mut memory,
        NEW_TSS + 80,
        if tiny_incoming_stack {
            SEL_TINY_SS
        } else {
            SEL_DATA
        },
    );
    put16(&mut memory, NEW_TSS + 84, SEL_DATA);

    let mut cpu = CpuGsw::default();
    cpu.set_mode(mode);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(SEL_CODE, 0x9b));
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(SEL_DATA, 0x93));
    }
    cpu.gdtr = DescriptorTable {
        base: GDT_BASE,
        limit: if tiny_incoming_stack { 0x57 } else { 0x4f },
    };
    cpu.idtr = DescriptorTable {
        base: IDT_BASE,
        limit: 0xff,
    };
    cpu.tr = SegmentRegister {
        selector: SEL_OLD_TSS,
        base: OLD_TSS,
        limit: 0x67,
        access: 0x8b,
        default_size_32: false,
    };
    cpu.registers.set_esp(0x0f0);
    cpu.registers.eflags = 0x2;
    cpu.set_eip(ENTRY);

    let mut bus = sixteen_bit_bus(memory);
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0, 0x1000]);
    warm_sixteen_bit(
        &mut cpu,
        &mut bus,
        &starts
            .iter()
            .map(|offset| ENTRY + offset)
            .collect::<Vec<_>>(),
    );
    let compilation = jit::direct::compile(&mut cpu, ENTRY, true)
        .expect("the protected task-gate block must compile");
    assert_eq!(compilation.span.instructions, expected_instructions);
    assert_eq!(compilation.callout_int_imm8_slots, expected_int_slots);
    assert_eq!(
        compilation.callout_interpret_one_slots,
        expected_interpret_one_slots
    );
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("a protected key");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("install the task-gate block");
    let block = cpu.jit_direct.block(id).expect("the block is live");

    NativeTaskGateFixture { cpu, bus, block }
}

#[test]
fn native_terminal_int_task_gate_settles_the_public_switch_and_stops_at_the_int() {
    let _int_rows = enable_int_rows();
    // MOV EBX,marker; INT 0E; MOV EAX,suffix. The terminal INT excludes the suffix.
    let code = [
        0xbb, 0xdf, 0x9b, 0x57, 0x13, 0xcd, 0x0e, 0xb8, 0xe0, 0xac, 0x68, 0x24,
    ];

    for (mode, expected) in [(GswMode::Gsw486, 269), (GswMode::Gsw586, 214)] {
        let mut fixture = native_task_gate_fixture(mode, &code, &[0, 5], 2, 1, 0, false);
        let entries = fixture.cpu.perf_counters().jit_direct_entries;
        let retired = fixture.cpu.perf_counters().jit_direct_insns;
        let resync = fixture
            .cpu
            .direct_stall_snapshot()
            .callout_interpret_one_resync;
        let elapsed = fixture.cpu.elapsed_clocks;

        let outcome = fixture
            .cpu
            .run_direct_block_accounted_for_test(&mut fixture.bus, fixture.block, u64::MAX)
            .expect("the task-gate INT must not stop the CPU")
            .expect("the installed terminal INT block must enter natively");

        assert_eq!(outcome.core_clocks, expected, "{mode:?} exact switch total");
        assert_eq!(fixture.cpu.elapsed_clocks - elapsed, expected);
        assert_eq!(fixture.cpu.timing_rem, 0);
        assert_eq!(fixture.cpu.perf_counters().jit_direct_entries - entries, 1);
        assert_eq!(fixture.cpu.perf_counters().jit_direct_insns - retired, 2);
        assert_eq!(
            fixture
                .cpu
                .direct_stall_snapshot()
                .callout_interpret_one_resync
                - resync,
            1,
            "the terminal IntImm8 helper must resync exactly once"
        );
        assert_eq!(
            u32::from_le_bytes(
                fixture.bus.memory[(OLD_TSS + 52) as usize..(OLD_TSS + 56) as usize]
                    .try_into()
                    .unwrap()
            ),
            MARKER,
            "the actual task save contains the native prefix marker"
        );
        assert_eq!(fixture.cpu.tr.selector, SEL_NEW_TSS);
        assert_eq!(fixture.cpu.registers.eip, NEXT_TASK_ENTRY);
        assert_eq!(
            fixture.cpu.registers.eax(),
            0,
            "the suffix must remain unexecuted"
        );
        assert!(fixture.cpu.direct_runtime.callout_error.is_none());
        assert_eq!(fixture.cpu.jit_direct.native_successful_helper_core, 0);
        assert_eq!(fixture.cpu.jit_direct.native_fatal_helper_core, 0);
        assert!(
            fixture
                .cpu
                .jit_direct
                .take_callout_retire_pending()
                .is_none()
        );
        assert_eq!(fixture.cpu.cycle(&mut fixture.bus).unwrap().core_clocks, 1);
        assert_eq!(fixture.cpu.registers.eax() & 0xff, 0x5a);
    }
}

#[test]
fn native_delivered_mov_sreg_fault_settles_the_task_gate_and_leaves_no_old_debt() {
    // MOV EBX,marker; MOV ES,DX; MOV EAX,suffix. The third slot makes the
    // nonterminal helper shape executable, but delivery exits at the helper.
    let code = [
        0xbb, 0xdf, 0x9b, 0x57, 0x13, 0x8e, 0xc2, 0xb8, 0xe0, 0xac, 0x68, 0x24,
    ];

    for (mode, expected) in [(GswMode::Gsw486, 269), (GswMode::Gsw586, 214)] {
        let mut fixture = native_task_gate_fixture(mode, &code, &[0, 5, 7], 3, 0, 1, false);
        fixture.cpu.registers.set_edx(0x38);
        let entries = fixture.cpu.perf_counters().jit_direct_entries;
        let retired = fixture.cpu.perf_counters().jit_direct_insns;
        let resync_fault = fixture
            .cpu
            .direct_stall_snapshot()
            .callout_interpret_one_resync_fault;
        let elapsed = fixture.cpu.elapsed_clocks;

        let outcome = fixture
            .cpu
            .run_direct_block_accounted_for_test(&mut fixture.bus, fixture.block, u64::MAX)
            .expect("the delivered #GP task gate must not stop the CPU")
            .expect("the installed helper block must enter natively");

        assert_eq!(
            outcome.core_clocks, expected,
            "{mode:?} exact delivery total"
        );
        assert_eq!(fixture.cpu.elapsed_clocks - elapsed, expected);
        assert_eq!(fixture.cpu.timing_rem, 0);
        assert_eq!(fixture.cpu.perf_counters().jit_direct_entries - entries, 1);
        assert_eq!(fixture.cpu.perf_counters().jit_direct_insns - retired, 1);
        assert_eq!(
            fixture
                .cpu
                .direct_stall_snapshot()
                .callout_interpret_one_resync_fault
                - resync_fault,
            1
        );
        assert_eq!(fixture.cpu.tr.selector, SEL_NEW_TSS);
        assert_eq!(fixture.cpu.registers.eip, NEXT_TASK_ENTRY);
        assert_eq!(fixture.cpu.registers.esp(), 0xec);
        assert_eq!(
            u32::from_le_bytes(fixture.bus.memory[0xec..0xf0].try_into().unwrap()),
            0x38
        );
        assert_eq!(
            u32::from_le_bytes(
                fixture.bus.memory[(OLD_TSS + 52) as usize..(OLD_TSS + 56) as usize]
                    .try_into()
                    .unwrap()
            ),
            MARKER
        );
        assert_eq!(
            fixture.cpu.registers.eax(),
            0,
            "the suffix must remain unexecuted"
        );
        assert!(fixture.cpu.direct_runtime.callout_error.is_none());
        assert_eq!(fixture.cpu.jit_direct.native_successful_helper_core, 0);
        assert_eq!(fixture.cpu.jit_direct.native_fatal_helper_core, 0);
        assert!(
            fixture
                .cpu
                .jit_direct
                .take_callout_retire_pending()
                .is_none()
        );
        assert_eq!(fixture.cpu.cycle(&mut fixture.bus).unwrap().core_clocks, 1);
        assert_eq!(fixture.cpu.registers.eax() & 0xff, 0x5a);
    }
}

#[test]
fn native_post_push_task_gate_fatal_returns_the_prefix_and_switch_only() {
    let code = [
        0xbb, 0xdf, 0x9b, 0x57, 0x13, 0x8e, 0xc2, 0xb8, 0xe0, 0xac, 0x68, 0x24,
    ];

    for (mode, expected) in [(GswMode::Gsw486, 200), (GswMode::Gsw586, 174)] {
        let mut fixture = native_task_gate_fixture(mode, &code, &[0, 5, 7], 3, 0, 1, true);
        fixture.cpu.registers.set_edx(0x38);
        let entries = fixture.cpu.perf_counters().jit_direct_entries;
        let retired = fixture.cpu.perf_counters().jit_direct_insns;
        let resync_fault = fixture
            .cpu
            .direct_stall_snapshot()
            .callout_interpret_one_resync_fault;
        let elapsed = fixture.cpu.elapsed_clocks;

        let error = fixture
            .cpu
            .run_direct_block_accounted_for_test(&mut fixture.bus, fixture.block, u64::MAX)
            .expect_err("the incoming tiny stack must make the error-code push fatal");

        assert!(matches!(
            error.error,
            CpuError::FaultAfterTaskSwitchCommit { nested_vector: 12 }
        ));
        assert_eq!(
            error.consumed_core_clocks, expected,
            "{mode:?} exact fatal total"
        );
        assert_eq!(fixture.cpu.elapsed_clocks - elapsed, expected);
        assert_eq!(fixture.cpu.timing_rem, 0);
        assert_eq!(fixture.cpu.perf_counters().jit_direct_entries - entries, 1);
        assert_eq!(fixture.cpu.perf_counters().jit_direct_insns - retired, 1);
        assert_eq!(
            fixture
                .cpu
                .direct_stall_snapshot()
                .callout_interpret_one_resync_fault
                - resync_fault,
            1
        );
        assert_eq!(fixture.cpu.tr.selector, SEL_NEW_TSS);
        assert_eq!(fixture.cpu.registers.eip, NEXT_TASK_ENTRY);
        assert_eq!(
            u32::from_le_bytes(
                fixture.bus.memory[(OLD_TSS + 52) as usize..(OLD_TSS + 56) as usize]
                    .try_into()
                    .unwrap()
            ),
            MARKER,
            "the fatal task save retains the native prefix marker"
        );
        assert_eq!(
            fixture.cpu.registers.eax(),
            0,
            "the suffix must remain unexecuted"
        );
        assert!(fixture.cpu.direct_runtime.callout_error.is_none());
        assert_eq!(fixture.cpu.jit_direct.native_successful_helper_core, 0);
        assert_eq!(fixture.cpu.jit_direct.native_fatal_helper_core, 0);
        assert!(
            fixture
                .cpu
                .jit_direct
                .take_callout_retire_pending()
                .is_none(),
            "the fatal entry must not leave a retirement latch for the next entry"
        );
        let later_elapsed = fixture.cpu.elapsed_clocks;
        assert_eq!(fixture.cpu.cycle(&mut fixture.bus).unwrap().core_clocks, 1);
        assert_eq!(fixture.cpu.elapsed_clocks - later_elapsed, 1);
        assert_eq!(fixture.cpu.timing_rem, 0);
        assert_eq!(fixture.cpu.registers.eax() & 0xff, 0x5a);
        assert_eq!(fixture.cpu.registers.eip, NEXT_TASK_ENTRY + 2);
    }
}
