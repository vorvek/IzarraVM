// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Fast-persona instruction prices contain cached RAM fetch and L1 data costs.
//! ROM and device apertures retain their separate bus charges.

use super::*;

const RAM_ADDRESS: u32 = 0x0002_0000;
const APERTURE_ADDRESS: u32 = 0x000A_1000;
const ROM_ADDRESS: u32 = 0x000F_0000;

fn fold_machine(mode: GswMode) -> Machine {
    let mut machine = test_machine();
    machine.set_mode(mode);
    machine
}

fn fetch_run_clocks(machine: &mut Machine, address: u32, len: u32) -> u64 {
    let before = machine.trace.elapsed_clocks();
    with_bus(machine, |bus| {
        bus.charge_instruction_fetch_run(address, len)
            .expect("instruction fetch run");
    });
    machine.trace.elapsed_clocks() - before
}

fn data_read_clocks(machine: &mut Machine, address: u32) -> u64 {
    let before = machine.trace.elapsed_clocks();
    with_bus(machine, |bus| {
        bus.read_memory(address, BusWidth::Dword, BusAccessKind::DataRead)
            .unwrap();
    });
    machine.trace.elapsed_clocks() - before
}

#[test]
fn cached_ram_fetches_are_included_in_fast_persona_instruction_prices() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = fold_machine(mode);
        for length in [1, 3, 15] {
            assert_eq!(
                fetch_run_clocks(&mut machine, RAM_ADDRESS, length),
                0,
                "{mode:?}"
            );
        }
    }
}

#[test]
fn rom_and_device_window_fetches_keep_their_bus_charge() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = fold_machine(mode);
        for address in [ROM_ADDRESS, APERTURE_ADDRESS] {
            assert!(
                fetch_run_clocks(&mut machine, address, 3) > 0,
                "{mode:?} {address:#x}"
            );
        }
    }
}

#[test]
fn l1_data_reads_are_included_in_fast_persona_instruction_prices() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = fold_machine(mode);
        data_read_clocks(&mut machine, RAM_ADDRESS);
        assert_eq!(data_read_clocks(&mut machine, RAM_ADDRESS), 0, "{mode:?}");
    }
}

#[test]
fn aperture_data_reads_keep_their_bus_charge() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = fold_machine(mode);
        assert!(
            data_read_clocks(&mut machine, APERTURE_ADDRESS) > 0,
            "{mode:?}"
        );
    }
}

#[test]
fn the_386_keeps_separate_fetch_and_data_charges() {
    for mode in [GswMode::Gsw386Slow, GswMode::Gsw386] {
        let mut machine = fold_machine(mode);
        assert!(fetch_run_clocks(&mut machine, RAM_ADDRESS, 3) > 0);
        data_read_clocks(&mut machine, RAM_ADDRESS);
        assert!(data_read_clocks(&mut machine, RAM_ADDRESS) > 0);
    }
}

#[test]
fn bus_ratios_convert_one_fixed_board_clock_to_each_cpu_quantum() {
    for (mode, quantum) in [
        (GswMode::Gsw386Slow, 747),
        (GswMode::Gsw386, 249),
        (GswMode::Gsw486, 83),
        (GswMode::Gsw586, 33),
    ] {
        assert_eq!(bus_timing(mode), (33, quantum));
    }
}

#[test]
fn cache_tier_costs_are_fixed_reference_bus_wait_states() {
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        let cost = crate::cache_config::tier_cost(mode);
        assert_eq!([cost.l1, cost.l2, cost.ram], [0, 12, 30]);
    }
}

#[test]
fn cache_lines_follow_the_persona() {
    use crate::cache_config::cache_line_bytes;
    assert_eq!(cache_line_bytes(GswMode::Gsw586), 32);
    assert_eq!(cache_line_bytes(GswMode::Gsw486), 16);
    assert_eq!(cache_line_bytes(GswMode::Gsw386), 64);
    assert_eq!(cache_line_bytes(GswMode::Gsw386Slow), 64);
}

#[test]
fn motherboard_port_duration_is_independent_of_cpu_persona() {
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        for (port, clocks) in [(0x80, 160), (0x3da, 56), (0x1234, 56)] {
            let mut machine = fold_machine(mode);
            let start = machine.timeline.now_ticks();
            with_bus(&mut machine, |bus| {
                bus.read_io(port, BusWidth::Byte, 0, false).unwrap();
                assert_eq!(
                    bus.guest_tick_now() - start,
                    clocks * 33,
                    "{mode:?} {port:#x}"
                );
            });
        }
    }
}

#[test]
fn motherboard_l2_contents_and_geometry_survive_cpu_mode_changes() {
    let mut cache = CacheModel::new(GswMode::Gsw586);
    assert_eq!(cache.data_tier(GswMode::Gsw586, 0x20000), Tier::Ram);
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        cache.set_mode(mode);
        assert_eq!(cache.data_tier(mode, 0x20010), Tier::L2, "{mode:?}");
        assert_eq!(cache_geometry(mode).l2_bytes, 512 * 1024);
    }
}

#[test]
fn motherboard_wide_io_charges_only_physical_transactions() {
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        for (port, width, clocks) in [
            (0x80, BusWidth::Word, 320),
            (0x1234, BusWidth::Dword, 224),
            (0xcf8, BusWidth::Dword, 56),
            (0x1f0, BusWidth::Byte, 20),
            (0x1f0, BusWidth::Word, 20),
            (0x1f0, BusWidth::Dword, 40),
        ] {
            for write in [false, true] {
                let mut machine = fold_machine(mode);
                let start = machine.timeline.now_ticks();
                with_bus(&mut machine, |bus| {
                    if write {
                        CpuBus::write_io(bus, port, width, 0, 0, false).unwrap();
                    } else {
                        bus.read_io(port, width, 0, false).unwrap();
                    }
                    assert_eq!(
                        bus.guest_tick_now() - start,
                        clocks * 33,
                        "{mode:?} {port:#x} {width:?} write={write}"
                    );
                });
            }
        }
    }
}

#[test]
fn native_io_budget_covers_the_whole_board_transaction() {
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        let mut machine = fold_machine(mode);
        with_bus(&mut machine, |bus| {
            let upper = bus.jit_scale_bus_cost_upper(bus.jit_io_cost_clocks(BusWidth::Byte));
            let before = bus.in_batch_scaled_bus_clocks();
            bus.read_io(0x80, BusWidth::Byte, 0, false).unwrap();
            assert!(
                bus.in_batch_scaled_bus_clocks() - before <= upper,
                "{mode:?}"
            );
        });
    }
}

#[test]
fn wide_port_accesses_wrap_without_claiming_pci_configuration() {
    let mut machine = fold_machine(GswMode::Gsw586);
    with_bus(&mut machine, |bus| {
        let start = bus.guest_tick_now();
        bus.read_io(0xffff, BusWidth::Dword, 0, false).unwrap();
        assert_eq!(bus.guest_tick_now() - start, 536 * 33);
    });
}

#[test]
fn opl_timer_start_does_not_consume_the_prewrite_prefix() {
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        let mut machine = fold_machine(mode);
        {
            let mut bus = machine.make_construction_bus();
            for (port, value) in [(0x388, 2), (0x389, 0xff), (0x388, 4)] {
                CpuBus::write_io(&mut bus, port, BusWidth::Byte, value, 0, false).unwrap();
            }
        }
        with_bus(&mut machine, |bus| {
            bus.prior_runs_core_clocks = 50_000;
            CpuBus::write_io(bus, 0x389, BusWidth::Byte, 1, 17, false).unwrap();
            assert_eq!(bus.predicted_opl_status().0 & 0xc0, 0, "{mode:?}");
        });
    }
}

#[test]
fn peripheral_start_credits_survive_a_real_fdc_split() {
    use izarravm_core::MASTER_CLOCK_HZ;
    fn prepared() -> Machine {
        let mut machine = fold_machine(GswMode::Gsw586);
        machine.advance_devices_ticks(13);
        let now = machine.master_ticks();
        machine.fdc.write_port_at(0x3f2, 0x0c, now);
        machine.fdc.write_port_at(0x3f5, 0x08, now);
        while machine.fdc.read_port(0x3f4).unwrap() & 0x40 != 0 {
            machine.fdc.read_port(0x3f5);
        }
        for value in [0x03, 0xf0, 0, 0x0f, 0, 1] {
            machine.fdc.write_port_at(0x3f5, value, now);
        }
        {
            let mut bus = machine.make_construction_bus();
            for (port, value) in [
                (0x388, 2),
                (0x389, 0xff),
                (0x388, 4),
                (0x3fb, 3),
                (0x378, 0x41),
            ] {
                CpuBus::write_io(&mut bus, port, BusWidth::Byte, value, 0, false).unwrap();
            }
        }
        machine
    }
    let mut candidate = prepared();
    let mut reference = prepared();
    let start = candidate.master_ticks();
    let fdc = candidate.fdc.ticks_until_event(start).unwrap();
    const CORE: u64 = 200_000;
    let writes = [(0x389, 1), (0x3f8, 0x41), (0x37a, 0x11)];
    let timestamps = with_bus(&mut candidate, |bus| {
        bus.prior_runs_core_clocks = CORE - 17;
        writes.map(|(port, value)| {
            CpuBus::write_io(bus, port, BusWidth::Byte, value, 17, false).unwrap();
            bus.in_batch_master_ticks()
        })
    });
    for (index, &timestamp) in timestamps.iter().enumerate() {
        assert_eq!(timestamp, CORE * 33 + (index as u64 + 1) * 160 * 33);
    }
    assert!(0 < fdc && fdc < timestamps[0]);
    assert!(candidate.opl_timer_advance_credit_us > 0);
    assert_eq!(candidate.serial.advance_credit_ticks(), timestamps[1]);
    assert_eq!(candidate.lpt.advance_credit_ticks(), timestamps[2]);
    assert_eq!(
        std::mem::take(&mut candidate.port_bus_batch_clocks),
        3 * 156
    );
    for ((port, value), timestamp) in writes.into_iter().zip(timestamps) {
        reference.advance_devices_ticks(start + timestamp - reference.master_ticks());
        let mut bus = reference.make_construction_bus();
        CpuBus::write_io(&mut bus, port, BusWidth::Byte, value, 0, false).unwrap();
    }
    let suffix = 5 * MASTER_CLOCK_HZ / 1_000_000;
    candidate.advance_devices_ticks(timestamps[2] + suffix);
    reference.advance_devices_ticks(suffix);
    for machine in [&mut candidate, &mut reference] {
        assert_eq!(machine.fdc.ticks_until_event(machine.master_ticks()), None);
        assert_eq!(machine.opl_timer_advance_credit_us, 0);
        assert_eq!(machine.serial.advance_credit_ticks(), 0);
        assert_eq!(machine.lpt.advance_credit_ticks(), 0);
        assert!(machine.serial.output().is_empty());
        assert!(machine.lpt.output().is_empty());
        let mut bus = machine.make_construction_bus();
        assert_eq!(
            bus.read_io(0x388, BusWidth::Byte, 0, false).unwrap() & 0xc0,
            0
        );
    }
    assert_eq!(candidate.timeline, reference.timeline);
    assert_eq!(candidate.pic, reference.pic);
    let completion = (MASTER_CLOCK_HZ * 10).div_ceil(115_200);
    assert!(completion > 80 * MASTER_CLOCK_HZ / 1_000_000);
    for machine in [&mut candidate, &mut reference] {
        machine.advance_devices_ticks(completion);
        assert_eq!(machine.serial.output(), b"A");
        assert_eq!(machine.lpt.output(), b"A");
        let mut bus = machine.make_construction_bus();
        assert_eq!(
            bus.read_io(0x388, BusWidth::Byte, 0, false).unwrap() & 0xc0,
            0xc0
        );
    }
    assert_eq!(candidate.timeline, reference.timeline);
    assert_eq!(candidate.pic, reference.pic);
}

fn peripheral_start_does_not_consume_the_prewrite_prefix(port: u16) {
    let uart_duration = (izarravm_core::MASTER_CLOCK_HZ * 10).div_ceil(115_200);
    let lpt_duration = izarravm_core::MASTER_CLOCK_HZ / 100_000;
    let keyboard_duration = izarravm_core::MASTER_CLOCK_HZ / 50_000;
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        let mut machine = fold_machine(mode);
        {
            let mut bus = machine.make_construction_bus();
            if port == 0x3f8 || port == 0x2f8 {
                CpuBus::write_io(&mut bus, port + 3, BusWidth::Byte, 3, 0, false).unwrap();
            } else if port != 0x64 {
                CpuBus::write_io(&mut bus, port, BusWidth::Byte, 0x41, 0, false).unwrap();
            }
        }
        with_bus(&mut machine, |bus| {
            bus.prior_runs_core_clocks = 50_000;
            let (target, value) = match port {
                0x378 | 0x278 => (port + 2, 0x11),
                0x64 => (port, 0xaa),
                _ => (port, 0x41),
            };
            CpuBus::write_io(bus, target, BusWidth::Byte, value, 17, false).unwrap();
            let prefix = bus.in_batch_master_ticks();
            match port {
                0x3f8 | 0x2f8 => {
                    let chip = if port == 0x3f8 {
                        &mut bus.serial
                    } else {
                        &mut bus.serial2
                    };
                    chip.advance_master_ticks(prefix + uart_duration - 1);
                    assert!(chip.output().is_empty(), "{mode:?} {port:#x}");
                    chip.advance_master_ticks(1);
                    assert_eq!(chip.output(), b"A");
                    chip.advance_master_ticks(uart_duration);
                    assert_eq!(chip.output(), b"A");
                }
                0x378 | 0x278 => {
                    let chip = if port == 0x378 {
                        &mut bus.lpt
                    } else {
                        &mut bus.lpt2
                    };
                    chip.advance_master_ticks(prefix + lpt_duration - 1);
                    assert!(chip.output().is_empty(), "{mode:?} {port:#x}");
                    assert!(!chip.take_irq());
                    chip.advance_master_ticks(1);
                    assert_eq!(chip.output(), b"A");
                    assert!(chip.take_irq());
                }
                _ => {
                    bus.keyboard
                        .advance_master_ticks(prefix + keyboard_duration - 1);
                    assert_eq!(bus.keyboard.read_port(0x64).unwrap() & 3, 2, "{mode:?}");
                    bus.keyboard.advance_master_ticks(1);
                    assert_eq!(bus.keyboard.read_port(0x64).unwrap() & 3, 1);
                    assert_eq!(bus.keyboard.read_port(0x60), Some(0x55));
                }
            }
        });
    }
}

#[test]
fn uart_start_does_not_consume_the_prewrite_prefix() {
    peripheral_start_does_not_consume_the_prewrite_prefix(0x3f8);
    peripheral_start_does_not_consume_the_prewrite_prefix(0x2f8);
}

#[test]
fn lpt_start_does_not_consume_the_prewrite_prefix() {
    peripheral_start_does_not_consume_the_prewrite_prefix(0x378);
    peripheral_start_does_not_consume_the_prewrite_prefix(0x278);
}

#[test]
fn keyboard_start_does_not_consume_the_prewrite_prefix() {
    peripheral_start_does_not_consume_the_prewrite_prefix(0x64);
}

#[test]
fn real_guest_peripheral_writes_settle_the_prefix_once_on_a_fatal_exit() {
    use izarravm_cpu::{SegmentIndex, SegmentRegister};
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        for exempt in [false, true] {
            if exempt && !mode.uses_approximate_timing() {
                continue;
            }
            for port in [0x3f8u16, 0x378] {
                let mut program = vec![0x90; 20_000];
                let target = if port == 0x378 { port + 2 } else { port };
                program.push(0xba);
                program.extend_from_slice(&target.to_le_bytes());
                program.extend_from_slice(&[0xb0, if port == 0x378 { 0x11 } else { b'A' }, 0xee]);
                if exempt {
                    program.extend(std::iter::repeat_n(0x90, 20_000));
                    if port == 0x378 {
                        program.extend_from_slice(&[0xb0, 0x10, 0xee]);
                        program.push(0xba);
                        program.extend_from_slice(&port.to_le_bytes());
                        program
                            .extend_from_slice(&[0xb0, b'B', 0xee, 0x42, 0x42, 0xb0, 0x11, 0xee]);
                    } else {
                        program.extend_from_slice(&[0xb0, b'B', 0xee]);
                    }
                }
                program.extend_from_slice(&[0xba, 0x10, 0x20, 0xec]);
                let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
                profile.cpu = mode;
                let mut machine = Machine::new_raw_program(profile, &program).unwrap();
                machine.cpu.registers.eflags &= !0x200;
                if exempt {
                    let mut cs = SegmentRegister::flat(8, 0x9b);
                    cs.base = machine.cpu.registers.cs().base;
                    cs.default_size_32 = false;
                    machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
                    machine.cpu.control.cr0 |= 1;
                }
                {
                    let mut bus = machine.make_construction_bus();
                    if port == 0x378 {
                        CpuBus::write_io(&mut bus, port, BusWidth::Byte, u32::from(b'A'), 0, false)
                            .unwrap();
                    } else {
                        CpuBus::write_io(&mut bus, port + 3, BusWidth::Byte, 3, 0, false).unwrap();
                    }
                }
                machine.set_fatal_ports(&[0x2010]);
                machine.test_next_batch_cap = Some(1_000_000);
                let start = machine.master_ticks();
                let stop = machine.run_until_halt_or_cycles(1_000_000).unwrap();
                assert!(
                    matches!(&stop, StopReason::CpuError(text) if text.contains("unsupported I/O port 0x2010")),
                    "{mode:?} exempt={exempt} {port:x}: {stop:?}"
                );
                let expected: &[u8] = if exempt { b"A" } else { b"" };
                let output = if port == 0x378 {
                    machine.lpt.output()
                } else {
                    machine.serial.output()
                };
                assert_eq!(output, expected, "{mode:?} exempt={exempt} {port:x}");
                assert_eq!(machine.serial.advance_credit_ticks(), 0);
                assert_eq!(machine.lpt.advance_credit_ticks(), 0);
                let physical: u64 = machine
                    .test_batch_observations
                    .iter()
                    .map(|batch| {
                        let quantum = u64::from(bus_timing(mode).1);
                        batch.core_clocks * quantum + (batch.raw_bus_clocks + batch.isa_clocks) * 33
                    })
                    .sum();
                assert_eq!(machine.master_ticks() - start, physical);
                assert!(machine.test_batch_observations.last().unwrap().fatal);
                if exempt {
                    assert_eq!(machine.test_batch_observations.len(), 1);
                }
                let duration = if port == 0x378 {
                    izarravm_core::MASTER_CLOCK_HZ / 100_000
                } else {
                    (izarravm_core::MASTER_CLOCK_HZ * 10).div_ceil(115_200)
                };
                assert!(physical > duration);
                machine.advance_devices_ticks(duration);
                let output = if port == 0x378 {
                    machine.lpt.output()
                } else {
                    machine.serial.output()
                };
                assert_eq!(output, if exempt { &b"AB"[..] } else { &b"A"[..] });
            }
        }
    }
}

#[test]
fn dsp_reset_and_pause_origins_apply_in_every_cpu_mode() {
    let ticks_per_us = izarravm_core::MASTER_CLOCK_HZ / 1_000_000;
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        for phase in [3, 10_007] {
            for reset in [true, false] {
                let mut machine = fold_machine(mode);
                machine.advance_devices_ticks(phase);
                {
                    let mut bus = machine.make_construction_bus();
                    if reset {
                        CpuBus::write_io(&mut bus, 0x226, BusWidth::Byte, 1, 0, false).unwrap();
                    } else {
                        for value in [0x41, 0x27, 0x10, 0x80, 0] {
                            CpuBus::write_io(&mut bus, 0x22c, BusWidth::Byte, value, 0, false)
                                .unwrap();
                        }
                    }
                }
                let prefix = with_bus(&mut machine, |bus| {
                    bus.prior_runs_core_clocks = 50_000;
                    CpuBus::write_io(
                        bus,
                        if reset { 0x226 } else { 0x22c },
                        BusWidth::Byte,
                        0,
                        17,
                        false,
                    )
                    .unwrap();
                    bus.in_batch_master_ticks()
                });
                if !reset {
                    assert!(
                        machine
                            .sb16
                            .pause_irq_deadline_micros()
                            .is_some_and(|(_, us)| us >= 100)
                    );
                }
                machine.advance_devices_ticks(prefix);
                if reset {
                    assert_eq!(
                        machine.sb16.read_port(0x22e).unwrap() & 0x80,
                        0,
                        "{mode:?} phase={phase}"
                    );
                } else {
                    assert_eq!(
                        machine.sb16.pause_irq_deadline_micros().unwrap().1,
                        100,
                        "{mode:?} phase={phase}"
                    );
                }
                let delay = if reset { 20 } else { 100 };
                let remaining = delay * ticks_per_us - machine.master_ticks() % ticks_per_us;
                machine.advance_devices_ticks(remaining - 1);
                if reset {
                    assert_eq!(machine.sb16.read_port(0x22e).unwrap() & 0x80, 0);
                } else {
                    assert_eq!(machine.sb16.pause_irq_deadline_micros().unwrap().1, 1);
                }
                machine.advance_devices_ticks(1);
                if reset {
                    assert_ne!(machine.sb16.read_port(0x22e).unwrap() & 0x80, 0);
                    assert_eq!(machine.sb16.read_port(0x22a), Some(0xaa));
                    assert_eq!(machine.sb16.read_port(0x22e).unwrap() & 0x80, 0);
                } else {
                    assert_eq!(machine.sb16.pause_irq_deadline_micros(), None);
                    machine.sb16.write_port(0x224, 0x82);
                    assert_eq!(machine.sb16.read_port(0x225).unwrap() & 1, 1);
                    machine.sb16.read_port(0x22e);
                    machine.advance_devices_ticks(delay * ticks_per_us);
                    assert_eq!(machine.sb16.read_port(0x225).unwrap() & 1, 0);
                }
            }
        }
    }
}

#[test]
fn wide_port_costs_follow_the_live_decode_before_and_after_pci_writes() {
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        for (port, width, expected, wss_base, bmide_base) in [
            (0x170, BusWidth::Word, 320, 0x170, 0xf000),
            (0xcfb, BusWidth::Word, 112, 0x530, 0xf000),
            (0x170, BusWidth::Dword, 640, 0x170, 0xf000),
            (0xf001, BusWidth::Dword, 112, 0x530, 0xf000),
            (0xffff, BusWidth::Dword, 536, 0x530, 0xfff0),
        ] {
            for write in [false, true] {
                let mut machine = fold_machine(mode);
                machine.wss_base = wss_base;
                assert!(machine.wss_enabled);
                {
                    let mut bus = machine.make_construction_bus();
                    CpuBus::write_io(&mut bus, 0xcf8, BusWidth::Dword, 0x8000_3920, 0, false)
                        .unwrap();
                    CpuBus::write_io(&mut bus, 0xcfc, BusWidth::Dword, bmide_base | 1, 0, false)
                        .unwrap();
                }
                assert_eq!(
                    machine.pci.ide_bus_master_io_base(),
                    Some(bmide_base as u16)
                );
                assert_eq!(machine.port_bus_batch_clocks, 0);
                with_bus(&mut machine, |bus| {
                    let raw = bus.trace.elapsed_clocks();
                    let census = *bus.port_accesses_by_class;
                    let upper = bus.rep_io_cost_upper(port, width);
                    assert_eq!(bus.trace.elapsed_clocks(), raw);
                    assert_eq!(*bus.isa_io_clocks, 0);
                    if write {
                        CpuBus::write_io(bus, port, width, 0, 0, false).unwrap();
                    } else {
                        bus.read_io(port, width, 0, false).unwrap();
                    }
                    assert_eq!(
                        bus.in_batch_reference_bus_clocks(),
                        expected,
                        "{mode:?} {port:#x} {width:?} write={write}"
                    );
                    let (records, classes) = match (port, width) {
                        (0x170, BusWidth::Word) => (2, [2, 0, 0, 0, 0]),
                        (0x170, BusWidth::Dword) => (4, [4, 0, 0, 0, 0]),
                        (0xf001, _) => (1, [0, 0, 2, 0, 0]),
                        (0xffff, _) => (4, [3, 0, 1, 0, 0]),
                        (0xcfb, _) => (2, [0, 0, 2, 0, 0]),
                        _ => unreachable!(),
                    };
                    assert_eq!(bus.trace.elapsed_clocks() - raw, records * 4);
                    assert_eq!(
                        std::array::from_fn::<_, 5, _>(
                            |i| bus.port_accesses_by_class[i] - census[i]
                        ),
                        classes
                    );
                    assert!(upper * u64::from(bus.bus_den_at_batch_start) >= expected * 33);
                });
            }
        }
        let mut machine = fold_machine(mode);
        machine.wss_base = 0xd00;
        {
            let mut bus = machine.make_construction_bus();
            CpuBus::write_io(&mut bus, 0xcf8, BusWidth::Dword, 0x8000_3920, 0, false).unwrap();
            CpuBus::write_io(&mut bus, 0xcfc, BusWidth::Dword, 0xd01, 0, false).unwrap();
        }
        assert_eq!(machine.pci.ide_bus_master_io_base(), Some(0xd00));
        assert_eq!(machine.port_bus_batch_clocks, 0);
        with_bus(&mut machine, |bus| {
            let census = *bus.port_accesses_by_class;
            let raw = bus.trace.elapsed_clocks();
            let upper = bus.rep_io_cost_upper(0xcff, BusWidth::Dword);
            assert_eq!(bus.pci.ide_bus_master_io_base(), Some(0xd00));
            assert_eq!(bus.in_batch_reference_bus_clocks(), 0);
            assert_eq!(
                upper,
                (640u64 * 33).div_ceil(u64::from(bus.bus_den_at_batch_start))
            );
            CpuBus::write_io(bus, 0xcff, BusWidth::Dword, 1, 0, false).unwrap();
            assert_eq!(bus.pci.ide_bus_master_io_base(), None);
            assert_eq!(bus.in_batch_reference_bus_clocks(), 56 + 3 * 160);
            assert_eq!(bus.trace.elapsed_clocks() - raw, 16);
            assert_eq!(
                std::array::from_fn::<_, 5, _>(|i| bus.port_accesses_by_class[i] - census[i]),
                [3, 0, 1, 0, 0]
            );
            assert!(bus.in_batch_scaled_bus_clocks() <= upper);
        });
    }
}
