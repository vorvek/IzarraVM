// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_cpu::DescriptorTable;

const DOS_LOAD_BASE: u32 = 0x2000;

fn rep_port_machine(opcode: u8, port: u16) -> Machine {
    let mut machine = Machine::new_raw_program(
        MachineProfile::gsw_386(32, VideoCard::Vega),
        &[0xf3, opcode, 0xf4],
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw486);
    machine.cpu.registers.eflags &= !0x200;
    machine.cpu.registers.set_edx(u32::from(port));
    machine.cpu.registers.set_ecx(2);
    machine.cpu.registers.set_esi(0x0200);
    machine.cpu.registers.set_edi(0x0300);
    machine.write_physical_u8(DOS_LOAD_BASE + 0x0200, 0x11);
    machine.write_physical_u8(DOS_LOAD_BASE + 0x0201, 0x22);
    machine
}

fn arm_fast_channel2(machine: &mut Machine) {
    {
        let mut bus = machine.make_construction_bus();
        bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap();
        bus.write_io(0x42, BusWidth::Byte, 0x40, false).unwrap();
        bus.write_io(0x42, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x61, BusWidth::Byte, 0x01, false).unwrap();
    }
}

fn write_descriptor(machine: &mut Machine, base: u32, selector: u16, low: u32, high: u32) {
    let offset = base + u32::from(selector);
    machine.write_physical_u32(offset, low);
    machine.write_physical_u32(offset + 4, high);
}

fn write_u16(machine: &mut Machine, address: u32, value: u16) {
    for (offset, byte) in value.to_le_bytes().into_iter().enumerate() {
        machine.write_physical_u8(address + offset as u32, byte);
    }
}

fn write_u32(machine: &mut Machine, address: u32, value: u32) {
    machine.write_physical_u32(address, value);
}

fn read_u32(machine: &mut Machine, address: u32) -> u32 {
    u32::from_le_bytes([
        machine.read_physical_u8(address),
        machine.read_physical_u8(address + 1),
        machine.read_physical_u8(address + 2),
        machine.read_physical_u8(address + 3),
    ])
}

fn task_gate_terminal_machine() -> Machine {
    const GDT: u32 = 0x1000;
    const IDT: u32 = 0x2000;
    const OLD_TSS: u32 = 0x3000;
    const NEW_TSS: u32 = 0x3800;
    // mov ebx,0x1111; mov eax,0x30; mov ds,ax. The invalid selector raises
    // #GP after the state-changing prefix through the ordinary CPU path.
    const PROG: &[u8] = &[
        0xbb, 0x11, 0x11, 0x00, 0x00, 0xb8, 0x30, 0x00, 0x00, 0x00, 0x8e, 0xd8,
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(32, VideoCard::Vega), PROG).unwrap();
    machine.set_mode(GswMode::Gsw486);
    write_descriptor(&mut machine, GDT, 0x08, 0x0000_ffff, 0x00cf_9b00);
    write_descriptor(&mut machine, GDT, 0x10, 0x0000_ffff, 0x00cf_9300);
    write_descriptor(&mut machine, GDT, 0x18, 0x3800_0067, 0x0000_8900);
    write_descriptor(&mut machine, GDT, 0x20, 0x3000_0067, 0x0000_8b00);
    write_descriptor(&mut machine, GDT, 0x28, 0x0000_00ff, 0x0040_9300);
    write_u32(&mut machine, IDT + 13 * 8, 0x0018_0000);
    write_u32(&mut machine, IDT + 13 * 8 + 4, 0x0000_8500);
    write_u32(&mut machine, NEW_TSS + 32, 0x90);
    write_u32(&mut machine, NEW_TSS + 36, 0x0000_0002);
    write_u32(&mut machine, NEW_TSS + 56, 0);
    write_u16(&mut machine, NEW_TSS + 72, 0x10);
    write_u16(&mut machine, NEW_TSS + 76, 0x08);
    write_u16(&mut machine, NEW_TSS + 80, 0x28);
    write_u16(&mut machine, NEW_TSS + 84, 0x10);
    // The resumed incoming task deliberately reaches a distinct real fatal
    // port path, which makes an old task-switch charge visible if it repeats.
    for (offset, byte) in [0xba, 0x10, 0x20, 0x00, 0x00, 0xec].into_iter().enumerate() {
        machine.write_physical_u8(0x90 + offset as u32, byte);
    }
    for (offset, byte) in PROG.iter().copied().enumerate() {
        machine.write_physical_u8(0x100 + offset as u32, byte);
    }
    assert_eq!(machine.read_physical_u8(0x100), 0xbb);
    assert_eq!(machine.read_physical_u8(0x10b), 0xd8);
    machine.cpu.gdtr = DescriptorTable {
        base: GDT,
        limit: 0xff,
    };
    machine.cpu.idtr = DescriptorTable {
        base: IDT,
        limit: 0xff,
    };
    machine.cpu.tr = SegmentRegister {
        selector: 0x20,
        base: OLD_TSS,
        limit: 0x67,
        access: 0x8b,
        default_size_32: false,
    };
    let code = SegmentRegister::flat(0x08, 0x9b);
    let data = SegmentRegister::flat(0x10, 0x93);
    machine.cpu.registers.set_segment(SegmentIndex::Cs, code);
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Es,
        SegmentIndex::Ss,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        machine.cpu.registers.set_segment(segment, data);
    }
    machine.cpu.control.cr0 |= 1;
    machine.cpu.registers.set_esp(0x1234);
    machine.cpu.registers.eflags &= !0x200;
    machine.set_fatal_ports(&[0x2010]);
    machine
}

fn task_gate_terminal_pit_machine() -> Machine {
    let mut machine = task_gate_terminal_machine();
    arm_fast_channel2(&mut machine);
    machine.port_bus_batch_clocks = 0;
    machine
}

fn run_one_capped_batch(machine: &mut Machine, cap: u64) -> usize {
    let batch = machine.test_batch_core_totals.len();
    let cap_observation = machine.test_effective_batch_caps.len();
    machine.test_next_batch_cap = Some(cap);
    let stop = machine.run_until_halt_or_cycles(cap).unwrap();
    assert!(
        matches!(stop, StopReason::CycleLimit { .. }),
        "the selected cap must end at the requested deadline, not halt or fault"
    );
    assert_eq!(
        machine.test_next_batch_cap, None,
        "cap must be consumed once"
    );
    assert_eq!(
        machine.test_effective_batch_caps[cap_observation], cap,
        "no nearer real device edge may make the selected reservation vacuous"
    );
    assert_eq!(
        machine.test_batch_core_totals.len(),
        batch + 1,
        "the deadline must leave exactly the selected batch to inspect"
    );
    batch
}

fn assert_rep_port_batch(
    machine: &Machine,
    observation: &TestBatchObservation,
    expected: (u64, u64, u64, u64, u64, u64, u64),
) {
    let quantum = match machine.active_mode {
        GswMode::Gsw386Slow => 747,
        GswMode::Gsw386 => 249,
        GswMode::Gsw486 => 83,
        GswMode::Gsw586 => 33,
    };
    assert_eq!(
        (
            observation.core_clocks,
            observation.raw_bus_clocks,
            observation.scaled_bus_clocks,
            observation.isa_clocks,
            observation.step,
            observation.bus_rem_at_entry,
            observation.bus_rem_at_exit,
        ),
        expected
    );
    assert_eq!(
        observation.elapsed_at_exit - observation.elapsed_at_entry,
        observation.step
    );
    assert_eq!(
        observation.timeline_ticks_at_exit - observation.timeline_ticks_at_entry,
        observation.core_clocks * quantum
            + (observation.raw_bus_clocks + observation.isa_clocks) * 33
    );
}

#[test]
fn machinebus_second_rep_port_observation_crosses_a_real_pit_edge() {
    // MOV BX,0x1111 is the real preceding CPU run. REP INSB begins the next
    // run in the same Machine batch, so the two reads must publish its one
    // settled clock as prior work and their own coordinates 0 then 3.
    const PROG: &[u8] = &[0xbb, 0x11, 0x11, 0xf3, 0x6c, 0xf4];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(32, VideoCard::Vega), PROG).unwrap();
    machine.set_mode(GswMode::Gsw486);
    machine.cpu.registers.set_edx(0x61);
    machine.cpu.registers.set_ecx(2);
    machine.cpu.registers.set_edi(0x0300);
    machine.test_string_port_observations = Some(Vec::new());
    arm_fast_channel2(&mut machine);
    assert!(machine.pit.out_after(1, 0).is_some());
    assert!(machine.pit.out_after(2, 0).is_some());
    machine.port_bus_batch_clocks = 0;
    let _ = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(machine.cpu.registers.ecx() as u16, 0);
    assert_eq!(machine.cpu.registers.edi() as u16, 0x0302);
    assert_eq!(
        machine.read_physical_u8(0x2300),
        machine.test_string_port_observations.as_ref().unwrap()[0].value as u8
    );
    assert_eq!(
        machine.read_physical_u8(0x2301),
        machine.test_string_port_observations.as_ref().unwrap()[1].value as u8
    );
    let observations = machine.test_string_port_observations.as_ref().unwrap();
    assert_eq!(
        observations.len(),
        2,
        "the actual REP must issue two reads in one run"
    );
    assert!(
        observations
            .iter()
            .all(|observation| !observation.write && observation.success)
    );
    assert!(
        observations
            .iter()
            .all(|observation| observation.port == 0x61)
    );
    assert!(
        observations
            .iter()
            .all(|observation| observation.width == BusWidth::Byte)
    );
    assert_eq!(observations[0].prior_runs_core_clocks, 1);
    assert_eq!(observations[1].prior_runs_core_clocks, 1);
    assert_eq!(observations[0].core_clocks_so_far, 0);
    assert_eq!(observations[1].core_clocks_so_far, 3);
    assert_eq!(observations[0].bus_num_at_batch_start, 33);
    assert_eq!(observations[0].bus_den_at_batch_start, 83);
    assert_eq!(observations[0].bus_rem_at_batch_start, 0);
    assert_eq!(observations[0].isa_io_clocks, 156);
    assert_eq!(observations[1].isa_io_clocks, 312);
    let raw_first = observations[0].trace_elapsed - observations[0].trace_elapsed_at_batch_start;
    let raw_second = observations[1].trace_elapsed - observations[1].trace_elapsed_at_batch_start;
    assert!(raw_second > raw_first);
    let calibration = &machine.test_batch_observations;
    assert_eq!(
        calibration.len(),
        1,
        "the calibrated guest settles in one batch"
    );
    let calibration = &calibration[0];
    assert_eq!(calibration.core_clocks, 22);
    assert_eq!(calibration.isa_clocks, 312);
    assert_eq!(calibration.scaled_bus_clocks, 127);
    assert_eq!(calibration.bus_rem_at_entry, 0);
    assert_eq!(calibration.bus_rem_at_exit, 0);
    assert_eq!(
        calibration.step, 149,
        "full settlement includes the second destination access and HLT"
    );
    assert!(
        machine.test_effective_batch_caps[0] > calibration.step,
        "the real event cap must leave the calibrated guest unsplit"
    );
    assert_eq!((raw_first, raw_second), (4, 8));
    let stale = 83 + (raw_second + 312) * 33;
    let corrected = stale + 3 * 83;
    assert_eq!((stale, corrected), (10_643, 10_892));
    let make_reference = || {
        let mut reference =
            Machine::new_raw_program(MachineProfile::gsw_386(32, VideoCard::Vega), PROG).unwrap();
        reference.set_mode(GswMode::Gsw486);
        arm_fast_channel2(&mut reference);
        reference.port_bus_batch_clocks = 0;
        reference
    };
    let mut stale_reference = make_reference();
    let mut corrected_reference = make_reference();
    stale_reference.advance_devices_ticks(stale);
    corrected_reference.advance_devices_ticks(corrected);
    let phase_limit =
        (64 * stale_reference.active_mode.clock_hz()).div_ceil(u64::from(PIT_INPUT_HZ));
    let mut phase = 0;
    while !(stale_reference.pit.channel_out(2) && !corrected_reference.pit.channel_out(2)) {
        assert!(
            phase < phase_limit,
            "a reload-64 mode-3 falling edge must exist within one PIT period"
        );
        stale_reference.advance_devices(1);
        corrected_reference.advance_devices(1);
        phase += 1;
    }

    let mut candidate = make_reference();
    candidate.cpu.registers.set_edx(0x61);
    candidate.cpu.registers.set_ecx(2);
    candidate.cpu.registers.set_edi(0x0300);
    candidate.test_string_port_observations = Some(Vec::new());
    candidate.advance_devices(phase);
    let candidate_start_ticks = candidate.master_ticks();
    let _ = candidate.run_until_halt_or_cycles(1_000_000).unwrap();
    let candidate_observations = candidate.test_string_port_observations.as_ref().unwrap();
    assert_eq!(candidate_observations.len(), 2);
    assert_eq!(
        candidate_observations[0].master_ticks_at_batch_start,
        candidate_start_ticks
    );
    assert_eq!(
        candidate_observations[1].master_ticks_at_batch_start,
        candidate_start_ticks
    );
    assert_eq!(
        candidate_observations[0].master_ticks_at_batch_start,
        candidate_observations[1].master_ticks_at_batch_start
    );
    let candidate_second_value = candidate_observations[1].value as u8;
    assert_eq!(
        candidate_observations[0].trace_elapsed
            - candidate_observations[0].trace_elapsed_at_batch_start,
        raw_first
    );
    assert_eq!(
        candidate_observations[1].trace_elapsed
            - candidate_observations[1].trace_elapsed_at_batch_start,
        raw_second
    );
    let candidate_calibration = &candidate.test_batch_observations;
    assert_eq!(candidate_calibration.len(), 1);
    let candidate_calibration = &candidate_calibration[0];
    assert_eq!(
        candidate_calibration.raw_bus_clocks,
        calibration.raw_bus_clocks
    );
    assert_eq!(
        candidate_calibration.scaled_bus_clocks,
        calibration.scaled_bus_clocks
    );
    assert_eq!(candidate_calibration.isa_clocks, 312);
    assert_eq!(candidate_calibration.core_clocks, 22);
    assert_eq!(candidate_calibration.bus_rem_at_entry, 0);
    assert_eq!(candidate_calibration.bus_rem_at_exit, 0);
    assert!(candidate.test_effective_batch_caps[0] > candidate_calibration.step);
    assert_eq!(candidate.read_physical_u8(0x2301), candidate_second_value);
    let observed = (candidate.read_physical_u8(0x2301) >> 5) & 1;
    let corrected_out = u8::from(corrected_reference.pit.channel_out(2));
    let stale_out = u8::from(stale_reference.pit.channel_out(2));
    assert_eq!(observed, corrected_out);
    assert_ne!(observed, stale_out);
    assert_eq!(PROG, &[0xbb, 0x11, 0x11, 0xf3, 0x6c, 0xf4]);
}

#[test]
fn machinebus_rep_port_setup_pause_resume_and_forced_progress_keep_each_batch_owned() {
    // These byte programs go through the real MachineBus and the normal batch
    // settlement. The exact setup boundaries are 11 for INSB and 13 for
    // OUTSB; a nonzero entry U is legitimate overshoot, while zero allowance
    // prevents a first invocation from forcing an element.
    for (opcode, port, cap, setup) in [(0x6c, 0x40, 11, 11), (0x6e, 0x80, 13, 13)] {
        let mut normal = rep_port_machine(opcode, port);
        assert_eq!(normal.active_mode(), GswMode::Gsw486);
        assert_eq!(normal.timing_epoch(), 2);
        assert_eq!(normal.bus_rem, 0);
        assert_eq!(normal.cpu.registers.eflags & 0x200, 0);
        normal.test_string_port_observations = Some(Vec::new());
        let paused = run_one_capped_batch(&mut normal, cap);
        assert_eq!(DOS_LOAD_BASE + 0x0200, 0x2200);
        assert_eq!(DOS_LOAD_BASE + 0x0300, 0x2300);
        assert_eq!(normal.cpu.registers.ecx() as u16, 2);
        assert_eq!(normal.cpu.registers.esi() as u16, 0x0200);
        assert_eq!(normal.cpu.registers.edi() as u16, 0x0300);
        assert_eq!(normal.cpu.registers.eip, 0x100);
        assert_rep_port_batch(
            &normal,
            &normal.test_batch_observations[paused],
            (setup, 0, 0, 0, setup, 0, 0),
        );
        #[cfg(feature = "jit")]
        assert_eq!(normal.cpu.poll_skip_timing_remainder(), 0);
        assert!(
            normal
                .test_string_port_observations
                .as_ref()
                .unwrap()
                .is_empty(),
            "the setup pause must not perform a string port access"
        );

        let normal_start = normal.test_batch_core_totals.len();
        let scaled_before = normal.scaled_bus_clocks;
        assert_eq!(
            normal.run_until_halt_or_cycles(1_000_000).unwrap(),
            StopReason::Halted
        );
        assert_eq!(normal.cpu.registers.ecx() as u16, 0);
        if opcode == 0x6c {
            assert_eq!(normal.cpu.registers.edi() as u16, 0x0302);
        } else {
            assert_eq!(normal.cpu.registers.esi() as u16, 0x0202);
        }
        let normal_batches = normal.test_batch_observations[normal_start..].to_vec();
        if opcode == 0x6c {
            assert_eq!(normal_batches.len(), 1);
            assert_rep_port_batch(&normal, &normal_batches[0], (10, 8, 127, 312, 137, 0, 0));
        } else {
            assert_eq!(normal_batches.len(), 3);
            assert_rep_port_batch(&normal, &normal_batches[0], (4, 4, 63, 156, 67, 0, 0));
            assert_rep_port_batch(&normal, &normal_batches[1], (4, 4, 64, 156, 68, 0, 0));
            assert_rep_port_batch(&normal, &normal_batches[2], (4, 0, 0, 0, 4, 0, 0));
        }
        assert_eq!(
            normal.scaled_bus_clocks - scaled_before,
            127,
            "the two string elements own the normal path's complete scaled bus total"
        );
        #[cfg(feature = "jit")]
        assert_eq!(normal.cpu.poll_skip_timing_remainder(), 0);
        let normal_ports = normal
            .test_string_port_observations
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(normal_ports.len(), 2);
        for (index, observation) in normal_ports.iter().enumerate() {
            let batch_index = if opcode == 0x6c { 0 } else { index };
            assert_eq!(
                observation.master_ticks_at_batch_start,
                normal_batches[batch_index].timeline_ticks_at_entry
            );
            assert_eq!(
                (
                    observation.write,
                    observation.success,
                    observation.port,
                    observation.width,
                ),
                (opcode == 0x6e, true, port, BusWidth::Byte)
            );
            if opcode == 0x6c {
                assert_eq!(
                    normal.read_physical_u8(DOS_LOAD_BASE + 0x0300 + index as u32),
                    observation.value as u8
                );
            } else {
                assert_eq!(observation.value as u8, [0x11, 0x22][index]);
            }
        }

        let mut forced = rep_port_machine(opcode, port);
        assert_eq!(forced.active_mode(), GswMode::Gsw486);
        assert_eq!(forced.timing_epoch(), 2);
        assert_eq!(forced.bus_rem, 0);
        assert_eq!(forced.cpu.registers.eflags & 0x200, 0);
        forced.test_string_port_observations = Some(Vec::new());
        let forced_paused = run_one_capped_batch(&mut forced, cap);
        assert_rep_port_batch(
            &forced,
            &forced.test_batch_observations[forced_paused],
            (setup, 0, 0, 0, setup, 0, 0),
        );
        #[cfg(feature = "jit")]
        assert_eq!(forced.cpu.poll_skip_timing_remainder(), 0);
        assert!(
            forced
                .test_string_port_observations
                .as_ref()
                .unwrap()
                .is_empty()
        );
        let first_forced_scaled = forced.scaled_bus_clocks;
        let first_forced_ports = forced.test_string_port_observations.as_ref().unwrap().len();
        let forced_batch = run_one_capped_batch(&mut forced, 1);
        assert_eq!(forced.scaled_bus_clocks - first_forced_scaled, 63);
        #[cfg(feature = "jit")]
        assert_eq!(forced.cpu.poll_skip_timing_remainder(), 0);
        assert_eq!(
            forced.test_string_port_observations.as_ref().unwrap().len(),
            first_forced_ports + 1
        );
        assert_eq!(forced.cpu.registers.ecx() as u16, 1);
        assert_eq!(forced.cpu.registers.eip, 0x100);
        if opcode == 0x6c {
            assert_eq!(forced.cpu.registers.edi() as u16, 0x0301);
        } else {
            assert_eq!(forced.cpu.registers.esi() as u16, 0x0201);
        }
        assert_rep_port_batch(
            &forced,
            &forced.test_batch_observations[forced_batch],
            if opcode == 0x6c {
                (3, 4, 63, 156, 66, 0, 0)
            } else {
                (4, 4, 63, 156, 67, 0, 0)
            },
        );
        let second_forced_scaled = forced.scaled_bus_clocks;
        let second_forced_ports = forced.test_string_port_observations.as_ref().unwrap().len();
        let second_forced_batch = run_one_capped_batch(&mut forced, 1);
        assert_eq!(forced.scaled_bus_clocks - second_forced_scaled, 64);
        #[cfg(feature = "jit")]
        assert_eq!(forced.cpu.poll_skip_timing_remainder(), 0);
        assert_eq!(
            forced.test_string_port_observations.as_ref().unwrap().len(),
            second_forced_ports + 1
        );
        assert_eq!(forced.cpu.registers.ecx() as u16, 0);
        assert_eq!(forced.cpu.registers.eip, 0x102);
        if opcode == 0x6c {
            assert_eq!(forced.cpu.registers.edi() as u16, 0x0302);
        } else {
            assert_eq!(forced.cpu.registers.esi() as u16, 0x0202);
        }
        assert_rep_port_batch(
            &forced,
            &forced.test_batch_observations[second_forced_batch],
            if opcode == 0x6c {
                (3, 4, 64, 156, 67, 0, 0)
            } else {
                (4, 4, 64, 156, 68, 0, 0)
            },
        );
        let forced_ports = forced
            .test_string_port_observations
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(
            forced_ports.len(),
            2,
            "each forced return performs one ordered string port access"
        );
        for (index, observation) in forced_ports.iter().enumerate() {
            let batch_index = [forced_batch, second_forced_batch][index];
            assert_eq!(
                observation.master_ticks_at_batch_start,
                forced.test_batch_observations[batch_index].timeline_ticks_at_entry
            );
            assert_eq!(
                (
                    observation.write,
                    observation.success,
                    observation.port,
                    observation.width,
                ),
                (opcode == 0x6e, true, port, BusWidth::Byte)
            );
            if opcode == 0x6c {
                assert_eq!(
                    forced.read_physical_u8(DOS_LOAD_BASE + 0x0300 + index as u32),
                    observation.value as u8
                );
            } else {
                assert_eq!(observation.value as u8, [0x11, 0x22][index]);
            }
        }
        let halted_batch = forced.test_batch_observations.len();
        let halted_scaled = forced.scaled_bus_clocks;
        assert_eq!(
            forced.run_until_halt_or_cycles(1_000_000).unwrap(),
            StopReason::Halted
        );
        assert_eq!(forced.test_batch_observations.len(), halted_batch + 1);
        assert_rep_port_batch(
            &forced,
            &forced.test_batch_observations[halted_batch],
            (4, 0, 0, 0, 4, 0, 0),
        );
        assert_eq!(forced.scaled_bus_clocks - halted_scaled, 0);
        #[cfg(feature = "jit")]
        assert_eq!(forced.cpu.poll_skip_timing_remainder(), 0);
    }
}

#[test]
fn machine_task_gate_terminal_fault_settles_switch_work_once_before_resume() {
    let mut calibration = task_gate_terminal_pit_machine();
    let calibration_batch = calibration.test_batch_observations.len();
    let calibration_scaled = calibration.scaled_bus_clocks;
    let calibration_elapsed = calibration.elapsed_clocks;
    let calibration_cpu_elapsed = calibration.cpu.elapsed_clocks;
    let calibration_acks = calibration.inta_diag.acknowledge_count;
    let stop = calibration.run_until_halt_or_cycles(1_000_000).unwrap();
    assert!(
        matches!(&stop, StopReason::CpuError(text) if text.contains("vector 12 raised after a task switch had committed")),
        "the task gate must commit before its error-code push faults: {stop:?}"
    );
    assert_eq!(
        calibration.test_batch_observations.len(),
        calibration_batch + 1
    );
    let calibration_observation = &calibration.test_batch_observations[calibration_batch];
    let raw_first = calibration_observation.raw_bus_clocks;
    let full_first_ticks = 201 * 83 + raw_first * 33;
    let full_first_step = full_first_ticks / 83;
    let first_scaled_charge = full_first_step - 201;
    let stale_first_ticks = 2 * 83 + raw_first * 33;
    assert_eq!(calibration.active_mode, GswMode::Gsw486);
    assert_eq!(calibration.timing_epoch(), 2);
    assert!(calibration_observation.fatal);
    assert_eq!(
        (
            calibration_observation.core_clocks,
            calibration_observation.raw_bus_clocks,
            calibration_observation.scaled_bus_clocks,
            calibration_observation.isa_clocks,
            calibration_observation.step,
            calibration_observation.bus_rem_at_entry,
            calibration_observation.bus_rem_at_exit,
        ),
        (
            201,
            raw_first,
            first_scaled_charge,
            0,
            full_first_step,
            0,
            0
        )
    );
    assert_eq!(
        calibration.elapsed_clocks - calibration_elapsed,
        full_first_step
    );
    assert_eq!(
        calibration.scaled_bus_clocks - calibration_scaled,
        first_scaled_charge
    );
    assert_eq!(
        calibration.cpu.elapsed_clocks - calibration_cpu_elapsed,
        201
    );
    assert_eq!(calibration.inta_diag.acknowledge_count, calibration_acks);

    let mut stale_reference = task_gate_terminal_pit_machine();
    let mut corrected_reference = task_gate_terminal_pit_machine();
    stale_reference.advance_cpu_work(stale_first_ticks, 2);
    corrected_reference.advance_cpu_work(full_first_ticks, 201);
    let phase_limit =
        (64 * stale_reference.active_mode.clock_hz()).div_ceil(u64::from(PIT_INPUT_HZ));
    let mut phase = 0;
    while !(stale_reference.pit.channel_out(2) && !corrected_reference.pit.channel_out(2)) {
        assert!(
            phase < phase_limit,
            "a reload-64 mode-3 falling edge must exist within one PIT period"
        );
        stale_reference.advance_devices(1);
        corrected_reference.advance_devices(1);
        phase += 1;
    }

    let mut machine = task_gate_terminal_pit_machine();
    machine.advance_devices(phase);
    let first = machine.test_batch_observations.len();
    let acks = machine.inta_diag.acknowledge_count;
    let first_scaled = machine.scaled_bus_clocks;
    let first_elapsed = machine.elapsed_clocks;
    let first_cpu_elapsed = machine.cpu.elapsed_clocks;
    let stop = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert!(
        matches!(&stop, StopReason::CpuError(text) if text.contains("vector 12 raised after a task switch had committed")),
        "the task gate must commit before its error-code push faults: {stop:?}"
    );
    assert_eq!(machine.test_batch_observations.len(), first + 1);
    let first_observation = &machine.test_batch_observations[first];
    assert!(first_observation.fatal);
    assert_eq!(first_observation.raw_bus_clocks, raw_first);
    assert!(
        first_observation.effective_cap > full_first_step,
        "the selected phase must leave the terminal task-gate fault in one real batch"
    );
    assert_eq!(
        (
            first_observation.core_clocks,
            first_observation.raw_bus_clocks,
            first_observation.scaled_bus_clocks,
            first_observation.isa_clocks,
            first_observation.step,
            first_observation.bus_rem_at_entry,
            first_observation.bus_rem_at_exit,
        ),
        (
            201,
            raw_first,
            first_scaled_charge,
            0,
            full_first_step,
            0,
            0
        )
    );
    assert_eq!(machine.cpu.tr.selector, 0x18);
    assert_eq!(machine.cpu.registers.eip, 0x90);
    assert_eq!(machine.cpu.fault_site().unwrap().eip, 0x10a);
    assert_eq!(read_u32(&mut machine, 0x3000 + 52), 0x1111);
    assert_eq!(machine.elapsed_clocks - first_elapsed, full_first_step);
    assert_eq!(
        machine.scaled_bus_clocks - first_scaled,
        first_scaled_charge
    );
    assert_eq!(machine.cpu.elapsed_clocks - first_cpu_elapsed, 201);
    assert_eq!(machine.inta_diag.acknowledge_count, acks);
    assert_eq!(machine.pit, corrected_reference.pit);
    assert_ne!(machine.pit, stale_reference.pit);
    assert_eq!(machine.timeline, corrected_reference.timeline);
    assert_ne!(machine.timeline, stale_reference.timeline);

    let resumed = machine.test_batch_observations.len();
    let resumed_scaled = machine.scaled_bus_clocks;
    let resumed_elapsed = machine.elapsed_clocks;
    let resumed_cpu_elapsed = machine.cpu.elapsed_clocks;
    let second = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert!(matches!(second, StopReason::CpuError(_)));
    assert_eq!(machine.test_batch_observations.len(), resumed + 1);
    let resumed_observation = &machine.test_batch_observations[resumed];
    let raw_second = resumed_observation.raw_bus_clocks;
    let resumed_ticks = 83 + (raw_second + 52) * 33;
    let resumed_step = (full_first_ticks % 83 + resumed_ticks) / 83;
    let resumed_scaled_charge = resumed_step - 1;
    assert!(resumed_observation.fatal);
    assert_eq!(
        (
            resumed_observation.core_clocks,
            resumed_observation.raw_bus_clocks,
            resumed_observation.scaled_bus_clocks,
            resumed_observation.isa_clocks,
            resumed_observation.step,
            resumed_observation.bus_rem_at_entry,
            resumed_observation.bus_rem_at_exit,
        ),
        (1, raw_second, resumed_scaled_charge, 52, resumed_step, 0, 0)
    );
    assert_eq!(machine.cpu.registers.eip, 0x96);
    assert_eq!(machine.cpu.fault_site().unwrap().eip, 0x95);
    assert_eq!(machine.elapsed_clocks - resumed_elapsed, resumed_step);
    assert_eq!(
        machine.scaled_bus_clocks - resumed_scaled,
        resumed_scaled_charge
    );
    assert_eq!(machine.cpu.elapsed_clocks - resumed_cpu_elapsed, 1);
    assert_eq!(machine.inta_diag.acknowledge_count, acks);
    corrected_reference.advance_cpu_work(resumed_ticks, 1);
    assert_eq!(machine.pit, corrected_reference.pit);
    assert_eq!(machine.timeline, corrected_reference.timeline);
}

#[test]
fn machine_fatal_keeps_staged_success_only_commands_pending() {
    // The repeated real fatal leaves all deferred work untouched. A successful
    // batch is the only owner permitted to consume these commands.
    const PROG: &[u8] = &[0xba, 0x10, 0x20, 0xec, 0xeb, 0xfd];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    machine.set_fatal_ports(&[0x2010]);
    with_bus(&mut machine, |bus| {
        bus.write_io(0xe1, BusWidth::Byte, 1, false).unwrap();
        bus.write_io(0xe3, BusWidth::Byte, 1, false).unwrap();
        bus.write_io(0xe8, BusWidth::Byte, 0x5a, false).unwrap();
        bus.write_io(0xe6, BusWidth::Byte, 1, false).unwrap();
    });
    for _ in 0..2 {
        let stop = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert!(matches!(stop, StopReason::CpuError(_)));
        assert_eq!(machine.pending_mode, Some(GswMode::Gsw486));
        assert_eq!(machine.pending_toka_service, Some(1));
        assert_eq!(machine.pending_cd_doorbell, Some(0x5a));
        assert_eq!(machine.cd_doorbell_status, 1);
        assert_eq!(machine.unittester.pending_command(), Some(1));
        assert_eq!(machine.active_mode, GswMode::Gsw386);
    }
}

#[test]
fn machinebus_rep_price_pause_and_resume_conserve_ram_work_and_pit() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for opcode in [0xa4, 0xaa] {
            let fixture = || {
                let mut machine = rep_port_machine(opcode, 0);
                machine.set_mode(mode);
                machine.cpu.registers.set_eax(0x11);
                arm_fast_channel2(&mut machine);
                machine.port_bus_batch_clocks = 0;
                machine
            };
            let mut paused = fixture();
            let mut normal = fixture();
            let batch = run_one_capped_batch(&mut paused, 1);
            let startup = match (mode, opcode) {
                (GswMode::Gsw486, 0xa4) => 12,
                (GswMode::Gsw486, _) => 7,
                (_, 0xa4) => 13,
                _ => 9,
            };
            assert_rep_port_batch(
                &paused,
                &paused.test_batch_observations[batch],
                (startup, 0, 0, 0, startup, 0, 0),
            );
            assert_eq!(paused.cpu.registers.ecx() as u16, 2);
            assert_eq!(paused.cpu.registers.eip, 0x100);
            assert_eq!(paused.read_physical_u8(0x2300), 0);
            assert_eq!(
                paused.run_until_halt_or_cycles(1_000_000).unwrap(),
                StopReason::Halted
            );
            assert_eq!(
                normal.run_until_halt_or_cycles(1_000_000).unwrap(),
                StopReason::Halted
            );
            assert_eq!(paused.cpu.registers.ecx() as u16, 0);
            assert_eq!(paused.read_physical_u8(0x2300), 0x11);
            assert_eq!(
                paused.read_physical_u8(0x2301),
                if opcode == 0xa4 { 0x22 } else { 0x11 }
            );
            assert_eq!(paused.cpu.registers, normal.cpu.registers);
            assert_eq!(paused.elapsed_clocks, normal.elapsed_clocks);
            assert_eq!(paused.cpu.elapsed_clocks, normal.cpu.elapsed_clocks);
            assert_eq!(paused.timeline.now_ticks(), normal.timeline.now_ticks());
            assert_eq!(paused.pit, normal.pit);
            assert_eq!(paused.bus_rem, normal.bus_rem);
            for address in [0x2300, 0x2301] {
                assert_eq!(
                    paused.read_physical_u8(address),
                    normal.read_physical_u8(address)
                );
            }
            for observation in &paused.test_batch_observations {
                assert_eq!(
                    observation.step,
                    observation.core_clocks
                        + observation.scaled_bus_clocks
                        + observation.isa_clocks
                );
                assert_eq!(
                    observation.elapsed_at_exit - observation.elapsed_at_entry,
                    observation.step
                );
                assert_eq!(observation.isa_clocks, 0);
                assert!(!observation.fatal);
            }
        }
    }
}

#[test]
fn machinebus_rep_prices_fold_aligned_fetch_and_ram_once() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for (opcode, width, operand32) in [
            (0xa4, 1, false),
            (0xa5, 2, false),
            (0xa5, 4, true),
            (0xaa, 1, false),
            (0xab, 2, false),
            (0xab, 4, true),
        ] {
            for address32 in [false, true] {
                for backwards in [false, true] {
                    for count in [0u32, 1, 2, 4097] {
                        let mut code = vec![0xf3];
                        if operand32 {
                            code.push(0x66);
                        }
                        if address32 {
                            code.push(0x67);
                        }
                        code.extend_from_slice(&[opcode, 0xf4]);
                        let mut machine = Machine::new_raw_program(
                            MachineProfile::gsw_386(2, VideoCard::Vega),
                            &code,
                        )
                        .unwrap();
                        machine.set_mode(mode);
                        machine.cpu.registers.eflags &= !0x200;
                        if backwards {
                            machine.cpu.registers.eflags |= 0x400;
                        }
                        machine.cpu.registers.set_ecx(count);
                        machine.cpu.registers.set_eax(0x5a5a5a5a);
                        let offset = if backwards && count > 0 {
                            (count - 1) * width
                        } else {
                            0
                        };
                        machine.cpu.registers.set_esi(0x2000 + offset);
                        machine.cpu.registers.set_edi(0x8000 + offset);
                        for i in 0..count * width {
                            machine.write_physical_u8(DOS_LOAD_BASE + 0x2000 + i, 0x5a);
                        }
                        let mut cpu = machine.cpu.clone();
                        let before = cpu.elapsed_clocks;
                        let (result, raw_bus, scaled_bus, isa) = with_bus(&mut machine, |bus| {
                            let raw_before = bus.trace.elapsed_clocks();
                            let result = cpu.cycle(bus).unwrap();
                            (
                                result,
                                bus.trace.elapsed_clocks() - raw_before,
                                bus.in_batch_scaled_bus_clocks(),
                                *bus.isa_io_clocks,
                            )
                        });
                        let stos = opcode == 0xaa || opcode == 0xab;
                        let expected = match (mode, stos, count) {
                            (GswMode::Gsw486, _, 0) => 5,
                            (GswMode::Gsw586, _, 0) => 6,
                            (_, false, 1) => 13,
                            (GswMode::Gsw486, false, _) => 12 + 3 * u64::from(count),
                            (GswMode::Gsw586, false, _) => 13 + u64::from(count),
                            (GswMode::Gsw486, true, _) => 7 + 4 * u64::from(count),
                            (GswMode::Gsw586, true, _) => 9 + u64::from(count),
                            _ => unreachable!(),
                        };
                        assert_eq!(
                            result.core_clocks, expected,
                            "{mode:?} {opcode:x} width={width} address32={address32} DF={backwards} C={count}"
                        );
                        assert_eq!(cpu.elapsed_clocks - before, expected);
                        assert_eq!((raw_bus, scaled_bus, isa), (0, 0, 0));
                        assert_eq!(cpu.registers.ecx(), 0);
                        assert_eq!(cpu.perf_counters().rep_string_iterations, u64::from(count));
                        if count > 0 {
                            if backwards {
                                assert_eq!(cpu.perf_counters().rep_string_fast_iterations, 0);
                            } else {
                                assert!(cpu.perf_counters().rep_string_fast_iterations > 0);
                            }
                        }
                        for i in 0..count * width {
                            assert_eq!(machine.read_physical_u8(DOS_LOAD_BASE + 0x8000 + i), 0x5a);
                        }
                    }
                }
            }
        }
    }
}
