// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn native_cached_fetch_batch_charges_the_exact_warm_ram_cost() {
    const FETCHES: u64 = 25_000;
    const FETCH_LENS: &[u8] = &[1, 3, 2, 4];
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486);

    with_bus(&mut machine, |bus| {
        let clocks_before = bus.trace.elapsed_clocks();
        let fetch_cost = bus.jit_fetch_cost_clocks();
        assert!(bus.charge_native_cached_fetches(0xF_4000, 0x100, FETCH_LENS, FETCHES));
        assert_eq!(
            bus.trace.elapsed_clocks() - clocks_before,
            fetch_cost * FETCHES * FETCH_LENS.len() as u64
        );
    });
}

#[test]
fn native_deadline_bound_uses_the_same_bus_scale_as_batch_accounting() {
    const RAW_CLOCKS: u64 = 301;
    let mut machine = test_machine();

    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        machine.set_mode(mode);
        let (num, den) = bus_timing(mode.persona());
        let expected = RAW_CLOCKS
            .saturating_mul(u64::from(num))
            .saturating_add(u64::from(den) - 1)
            / u64::from(den);
        with_bus(&mut machine, |bus| {
            assert_eq!(bus.jit_scale_bus_cost_upper(RAW_CLOCKS), expected);
        });
    }
}

#[test]
fn rep_page_walk_bound_covers_four_scaled_page_table_cycles() {
    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        for address in [0x3000, LOW_BIOS_BASE, 0xA_1000] {
            let mut machine = test_machine();
            machine.set_mode(mode);
            assert!(machine.set_vga_mode(0x13));
            with_bus(&mut machine, |bus| {
                let bound = bus
                    .rep_page_walk_cost_upper()
                    .expect("MachineBus supplies a cold page-walk bound");
                let before = bus.in_batch_scaled_bus_clocks();
                bus.read_memory(address, BusWidth::Dword, BusAccessKind::PageWalkRead)
                    .unwrap();
                bus.write_memory(address, BusWidth::Dword, 0, BusAccessKind::PageWalkWrite)
                    .unwrap();
                bus.read_memory(address, BusWidth::Dword, BusAccessKind::PageWalkRead)
                    .unwrap();
                bus.write_memory(address, BusWidth::Dword, 0, BusAccessKind::PageWalkWrite)
                    .unwrap();
                let growth = bus.in_batch_scaled_bus_clocks() - before;
                assert!(
                    growth <= bound,
                    "{mode:?} address {address:#x}: growth {growth}, bound {bound}"
                );
            });
        }
    }
}

#[test]
fn native_cached_fetch_batch_observes_the_linear_stub_address() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486);
    machine.last_int_vector = Some(0x10);

    with_bus(&mut machine, |bus| {
        assert!(bus.charge_native_cached_fetches(BIOS_LEGACY_IRET_LINEAR, 0x5000, &[1], 4,));
    });

    assert_eq!(machine.pending_soft_int, Some(0x10));
    assert_eq!(machine.last_int_vector, None);
}

const NATIVE_FETCH_LINEAR: u32 = 0xF_4000;
const NATIVE_FETCH_PHYSICAL: u32 = 0x5000;

fn arm_native_fetch_loop(cpu: &mut CpuGsw) {
    cpu.halted = false;
    cpu.registers.eip = NATIVE_FETCH_LINEAR;
    cpu.registers.set_eax(0);
    cpu.registers.set_ecx(0);
    cpu.registers.set_edx(0);
    cpu.registers.set_ebx(0);
    cpu.registers.set_esp(0);
    cpu.registers.set_ebp(0);
    cpu.registers.set_esi(0);
    cpu.registers.set_edi(0);
    cpu.registers.eflags = 0x203;
    let mut cs = cpu.registers.cs();
    cs.default_size_32 = true;
    cs.limit = u32::MAX;
    cs.access = 0x9b;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
}

fn drive_native_fetch_loop(cpu: &mut CpuGsw, machine: &mut Machine) -> Vec<CycleOutcome> {
    let mut outcomes = Vec::new();
    for _ in 0..64 {
        let outcome = with_bus(machine, |bus| cpu.run_straight_line(bus, u64::MAX).unwrap());
        outcomes.push(outcome);
        if outcome.halted {
            return outcomes;
        }
    }
    panic!("native fetch loop did not halt");
}

#[test]
fn direct_large_self_loop_bulk_fetch_uses_physical_paging_alias_timing() {
    const ITERATIONS: u32 = 1_000;
    const PROGRAM: [u8; 16] = [
        0xb9, 0xe8, 0x03, 0x00, 0x00, // mov ecx,1000
        0x83, 0xc0, 0x03, // add eax,3
        0x89, 0xc2, // mov edx,eax
        0x83, 0xe9, 0x01, // sub ecx,1
        0x75, 0xf6, // jnz to the loop body
        0xf4,
    ];
    let mut interp_machine = test_machine();
    let mut native_machine = test_machine();
    interp_machine.set_mode(GswMode::Gsw586);
    native_machine.set_mode(GswMode::Gsw586);
    for machine in [&mut interp_machine, &mut native_machine] {
        machine.write_physical_u32(0x1000, 0x2007);
        machine.write_physical_u32(
            0x2000 + ((NATIVE_FETCH_LINEAR >> 12) & 0x3FF) * 4,
            NATIVE_FETCH_PHYSICAL | 7,
        );
    }
    for (offset, byte) in PROGRAM.into_iter().enumerate() {
        interp_machine.write_physical_u8(NATIVE_FETCH_PHYSICAL + offset as u32, byte);
        native_machine.write_physical_u8(NATIVE_FETCH_PHYSICAL + offset as u32, byte);
    }
    let mut interp_cpu = interp_machine.cpu.clone();
    let mut native_cpu = native_machine.cpu.clone();
    for cpu in [&mut interp_cpu, &mut native_cpu] {
        cpu.control.cr0 |= 0x8000_0001;
        cpu.control.cr3 = 0x1000;
        cpu.registers.set_segment(
            SegmentIndex::Cs,
            SegmentRegister {
                selector: 0x08,
                base: 0,
                limit: u32::MAX,
                access: 0x9b,
                default_size_32: true,
            },
        );
    }
    interp_cpu.set_jit_auto_admit(false);
    native_cpu.set_jit_auto_admit(true);

    for _ in 0..4 {
        arm_native_fetch_loop(&mut interp_cpu);
        arm_native_fetch_loop(&mut native_cpu);
        drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
        drive_native_fetch_loop(&mut native_cpu, &mut native_machine);
    }
    interp_machine.trace = BusTrace::default();
    native_machine.trace = BusTrace::default();
    arm_native_fetch_loop(&mut interp_cpu);
    arm_native_fetch_loop(&mut native_cpu);
    let traced_direct_insns = native_cpu.perf_counters().jit_direct_insns;
    let traced_direct_entries = native_cpu.perf_counters().jit_direct_entries;

    let interp_traced_outcomes = drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
    let native_traced_outcomes = drive_native_fetch_loop(&mut native_cpu, &mut native_machine);

    assert_eq!(native_traced_outcomes, interp_traced_outcomes);
    assert_eq!(native_machine.trace, interp_machine.trace);
    assert_eq!(
        native_cpu.perf_counters().jit_direct_insns,
        traced_direct_insns
    );
    assert_eq!(
        native_cpu.perf_counters().jit_direct_entries,
        traced_direct_entries
    );

    interp_machine.trace = BusTrace::default();
    interp_machine.trace.set_tracing_mode(TracingMode::Off);
    native_machine.trace = BusTrace::default();
    native_machine.trace.set_tracing_mode(TracingMode::Off);
    arm_native_fetch_loop(&mut interp_cpu);
    arm_native_fetch_loop(&mut native_cpu);
    let direct_insns = native_cpu.perf_counters().jit_direct_insns;
    let direct_entries = native_cpu.perf_counters().jit_direct_entries;

    let interp_outcomes = drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
    let native_outcomes = drive_native_fetch_loop(&mut native_cpu, &mut native_machine);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native_cpu, interp_cpu);
    assert_eq!(
        native_machine.trace.elapsed_clocks(),
        interp_machine.trace.elapsed_clocks()
    );
    assert_eq!(native_cpu.registers.eax(), ITERATIONS * 3);
    assert_eq!(
        native_cpu.perf_counters().jit_direct_insns - direct_insns,
        u64::from(ITERATIONS) * 4
    );
    assert_eq!(
        native_cpu.perf_counters().jit_direct_entries - direct_entries,
        1
    );
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn paged_fast_map_tlb_collision_keeps_interpreter_and_native_timing_equal() {
    const PAGE_DIRECTORY: u32 = 0x1000;
    const PAGE_TABLE: u32 = 0x2000;
    const WARM_CODE_LINEAR: u32 = 0x000f_4000;
    const WARM_CODE_PHYSICAL: u32 = 0x5000;
    const MEASURE_CODE_LINEAR: u32 = 0x000f_5000;
    const MEASURE_CODE_PHYSICAL: u32 = 0x8000;
    const LINEAR_A: u32 = 0x3000;
    const LINEAR_B: u32 = LINEAR_A + 64 * 0x1000;
    const FRAME_A: u32 = 0x6000;
    const FRAME_B: u32 = 0x7000;
    const VALUE_A: u32 = 0x1020_3040;
    const VALUE_B: u32 = 0x5566_7788;
    const PTE_A: u32 = PAGE_TABLE + ((LINEAR_A >> 12) & 0x3ff) * 4;
    const PTE_B: u32 = PAGE_TABLE + ((LINEAR_B >> 12) & 0x3ff) * 4;

    assert_eq!((LINEAR_A >> 12) & 63, (LINEAR_B >> 12) & 63);

    let mut warm_program = vec![0xa1];
    warm_program.extend_from_slice(&LINEAR_A.to_le_bytes());
    warm_program.push(0xa1);
    warm_program.extend_from_slice(&LINEAR_B.to_le_bytes());
    warm_program.push(0xf4);

    let mut program = vec![0x90, 0xa1];
    program.extend_from_slice(&LINEAR_A.to_le_bytes());
    program.extend_from_slice(&[
        0x85, 0xc0, // test eax,eax
        0x74, 0xf7, // jz back to the entry, not taken for VALUE_A
        0xf4, // hlt
    ]);

    let make_fixture = || {
        let mut machine = test_machine();
        machine.set_mode(GswMode::Gsw486);
        machine.write_physical_u32(PAGE_DIRECTORY, PAGE_TABLE | 7);
        machine.write_physical_u32(
            PAGE_TABLE + ((WARM_CODE_LINEAR >> 12) & 0x3ff) * 4,
            WARM_CODE_PHYSICAL | 7,
        );
        machine.write_physical_u32(
            PAGE_TABLE + ((MEASURE_CODE_LINEAR >> 12) & 0x3ff) * 4,
            MEASURE_CODE_PHYSICAL | 7,
        );
        machine.write_physical_u32(PTE_A, FRAME_A | 7);
        machine.write_physical_u32(PTE_B, FRAME_B | 7);
        machine.write_physical_u32(FRAME_A, VALUE_A);
        machine.write_physical_u32(FRAME_B, VALUE_B);
        for (offset, byte) in warm_program.iter().copied().enumerate() {
            machine.write_physical_u8(WARM_CODE_PHYSICAL + offset as u32, byte);
        }
        for (offset, byte) in program.iter().copied().enumerate() {
            machine.write_physical_u8(MEASURE_CODE_PHYSICAL + offset as u32, byte);
        }
        machine.trace = BusTrace::default();
        machine.trace.set_tracing_mode(TracingMode::Full);
        machine
    };
    let mut interp_machine = make_fixture();
    let mut native_machine = make_fixture();

    let configure_cpu = |machine: &Machine| {
        let mut cpu = machine.cpu.clone();
        cpu.control.cr0 |= 0x8000_0001;
        cpu.control.cr3 = PAGE_DIRECTORY;
        cpu.registers
            .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x08, 0x9b));
        for segment in [
            SegmentIndex::Ds,
            SegmentIndex::Ss,
            SegmentIndex::Es,
            SegmentIndex::Fs,
            SegmentIndex::Gs,
        ] {
            cpu.registers
                .set_segment(segment, SegmentRegister::flat(0x10, 0x93));
        }
        cpu.set_jit_auto_admit(false);
        cpu
    };
    let mut interp_cpu = configure_cpu(&interp_machine);
    let mut native_cpu = configure_cpu(&native_machine);
    let arm = |cpu: &mut CpuGsw, eip: u32| {
        cpu.halted = false;
        cpu.registers.eip = eip;
        cpu.registers.set_eax(0);
        cpu.registers.set_ecx(0);
        cpu.registers.set_edx(0);
        cpu.registers.set_ebx(0);
        cpu.registers.set_esp(0);
        cpu.registers.set_ebp(0);
        cpu.registers.set_esi(0);
        cpu.registers.set_edi(0);
        cpu.registers.eflags = 0x202;
    };

    for (cpu, machine) in [
        (&mut interp_cpu, &mut interp_machine),
        (&mut native_cpu, &mut native_machine),
    ] {
        arm(cpu, WARM_CODE_LINEAR);
        let outcomes = drive_native_fetch_loop(cpu, machine);
        assert!(outcomes.last().is_some_and(|outcome| outcome.halted));
        for pte in [PTE_A, PTE_B] {
            assert!(
                machine.trace.cycles().iter().any(|cycle| {
                    cycle.kind == BusAccessKind::PageWalkRead && cycle.address == pte
                }),
                "the cold warmup must walk PTE {pte:#x}"
            );
        }
    }

    interp_machine.trace = BusTrace::default();
    interp_machine.trace.set_tracing_mode(TracingMode::Off);
    native_machine.trace = BusTrace::default();
    native_machine.trace.set_tracing_mode(TracingMode::Off);
    native_cpu.set_jit_auto_admit(true);
    for _ in 0..12 {
        interp_machine.write_physical_u32(FRAME_A, VALUE_A);
        native_machine.write_physical_u32(FRAME_A, VALUE_A);
        arm(&mut interp_cpu, MEASURE_CODE_LINEAR);
        arm(&mut native_cpu, MEASURE_CODE_LINEAR);
        let interp_outcomes = drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
        let native_outcomes = drive_native_fetch_loop(&mut native_cpu, &mut native_machine);
        assert_eq!(native_outcomes, interp_outcomes);
    }
    assert!(
        native_cpu.perf_counters().jit_direct_insns >= 3,
        "{:?}",
        native_cpu.perf_counters()
    );

    interp_machine.write_physical_u32(FRAME_A, VALUE_A);
    native_machine.write_physical_u32(FRAME_A, VALUE_A);
    interp_machine.trace = BusTrace::default();
    interp_machine.trace.set_tracing_mode(TracingMode::Off);
    native_machine.trace = BusTrace::default();
    native_machine.trace.set_tracing_mode(TracingMode::Off);
    arm(&mut interp_cpu, MEASURE_CODE_LINEAR);
    arm(&mut native_cpu, MEASURE_CODE_LINEAR);
    interp_cpu.elapsed_clocks = 0;
    native_cpu.elapsed_clocks = 0;
    let direct_insns = native_cpu.perf_counters().jit_direct_insns;
    let direct_loads = native_cpu.perf_counters().jit_native_load_hits;

    let interp_outcomes = drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
    let native_outcomes = drive_native_fetch_loop(&mut native_cpu, &mut native_machine);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(native_cpu, interp_cpu);
    assert_eq!(
        native_machine.trace.elapsed_clocks(),
        interp_machine.trace.elapsed_clocks(),
        "production aggregate accounting must preserve raw bus clocks"
    );
    assert_eq!(
        native_cpu.perf_counters().jit_direct_insns - direct_insns,
        3
    );
    assert_eq!(
        native_cpu.perf_counters().jit_native_load_hits - direct_loads,
        1,
        "the evicted first alias must be read by native code"
    );
    assert_eq!(
        native_machine.memory.as_slice(),
        interp_machine.memory.as_slice()
    );
    assert_eq!(
        interp_machine.memory.read_u32(FRAME_A as usize).unwrap(),
        VALUE_A
    );
    assert_eq!(
        interp_machine.memory.read_u32(FRAME_B as usize).unwrap(),
        VALUE_B
    );

    interp_machine.write_physical_u32(FRAME_A, VALUE_A);
    interp_machine.trace = BusTrace::default();
    interp_machine.trace.set_tracing_mode(TracingMode::Full);
    arm(&mut interp_cpu, MEASURE_CODE_LINEAR);
    drive_native_fetch_loop(&mut interp_cpu, &mut interp_machine);
    assert!(
        interp_machine.trace.cycles().iter().all(|cycle| !matches!(
            cycle.kind,
            BusAccessKind::PageWalkRead | BusAccessKind::PageWalkWrite
        )),
        "the shared FastMap must survive the old 64-entry TLB collision"
    );
}

#[test]
fn ram_lookup_does_not_expose_partial_final_pages_as_full_pages() {
    let vega = Vega::default();
    let lookup = RamPageLookup::new(RAM_LOOKUP_PAGE_SIZE + 17, &vega);
    assert!(lookup.direct_bytes(0, RAM_LOOKUP_PAGE_SIZE).is_some());
    assert!(
        lookup
            .direct_bytes(RAM_LOOKUP_PAGE_SIZE as u32, RAM_LOOKUP_PAGE_SIZE)
            .is_none(),
        "a final partial page cannot back a full direct-page pointer"
    );
}
