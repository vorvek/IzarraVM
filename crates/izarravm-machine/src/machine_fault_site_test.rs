// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::tests::with_bus;
use izarravm_cpu::DescriptorTable;

fn fatal_sb8_dma_machine() -> Machine {
    // mov dx,2010h; in al,dx; jmp short back to the IN. The failed IN is
    // real device work: its settlement must still advance the armed SB8 DMA.
    const PROG: &[u8] = &[0xba, 0x10, 0x20, 0xec, 0xeb, 0xfd];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    machine.set_mode(GswMode::Gsw486);
    machine.set_timing_epoch_for_test(2);
    machine.cpu.registers.eflags &= !0x200;
    machine.set_fatal_ports(&[0x2010]);
    for index in 0..16u32 {
        machine.write_physical_u8(0x1_0000 + index, if index == 0 { 0x40 } else { 0x10 });
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0b, BusWidth::Byte, 0x49, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0f, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0a, BusWidth::Byte, 0x01, false).unwrap();
        for byte in [0x41u8, 0x2b, 0x11, 0xc0, 0x00, 0x0f, 0x00] {
            bus.write_io(0x22c, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
    });
    machine.port_bus_batch_clocks = 0;
    machine
}

#[test]
fn a_fatal_settlement_advances_one_due_sb8_dma_sample_once() {
    // This is not the synchronous 8237 software-copy path. SB8 consumption is
    // clock-driven by advance_devices, so the fatal return has to own it.
    const FATAL_STEP: u64 = 57; // MOV DX (1) + failed byte IN raw (4) + ISA (52)
    let mut candidate = fatal_sb8_dma_machine();
    let mut reference = fatal_sb8_dma_machine();
    let first_sample = candidate
        .timeline
        .cpu_clocks_until(
            crate::timeline::DeviceClock::Dsp,
            1,
            u64::from(candidate.sb16.test_output_frame_rate()),
        )
        .unwrap();
    assert!(first_sample > FATAL_STEP);
    for machine in [&mut candidate, &mut reference] {
        assert_eq!(machine.dma.master.channels[1].transfer_cycles, 0);
        assert_eq!(machine.dma.master.channels[1].cur_addr, 0);
        assert_eq!(machine.dma.master.channels[1].cur_count, 15);
        machine.advance_devices(first_sample - FATAL_STEP);
    }

    let batch = candidate.test_batch_observations.len();
    let stop = candidate.run_until_halt_or_cycles(1_000_000).unwrap();
    assert!(matches!(stop, StopReason::CpuError(_)));
    let observation = &candidate.test_batch_observations[batch];
    assert!(observation.fatal);
    assert_eq!(observation.core_clocks, 1);
    assert_eq!(observation.isa_clocks, 52);
    assert_eq!(observation.step, FATAL_STEP);

    reference.advance_cpu_work(FATAL_STEP, 1);
    assert_eq!(
        candidate.dma.master.channels[1], reference.dma.master.channels[1],
        "fatal settlement and the independent device reference must own the same DMA cycle"
    );
    assert_eq!(candidate.dma.master.channels[1].transfer_cycles, 1);
    assert_eq!(candidate.dma.master.channels[1].cur_addr, 1);
    assert_eq!(candidate.dma.master.channels[1].cur_count, 14);
    let candidate_frame = candidate.sb16.test_drain_frame();
    let reference_frame = reference.sb16.test_drain_frame();
    assert!(candidate_frame.is_some());
    assert_eq!(candidate_frame, reference_frame);

    let transfers = candidate.dma.master.channels[1].transfer_cycles;
    let resumed = candidate.run_until_halt_or_cycles(1_000_000).unwrap();
    assert!(matches!(resumed, StopReason::CpuError(_)));
    assert_eq!(candidate.dma.master.channels[1].transfer_cycles, transfers);
}

fn ring0_software_dma_copy_machine(mem_to_mem_enabled: bool) -> Machine {
    // Ring-0 I486 leaves this DMA request in the exempt I/O lane, so the
    // synchronous copy and the fatal IN share one Machine batch.
    const PROG: &[u8] = &[
        0xb0, 0x04, // mov al,4: software DREQ on channel 0
        0x66, 0xba, 0x09, 0x00, // mov dx,9
        0xee, // out dx,al
        0x66, 0xba, 0x10, 0x20, // mov dx,2010h
        0xec, // in al,dx
    ];
    const SRC: u32 = 0x1000;
    const DST: u32 = 0x1100;
    const GDT: u32 = 0x3000;
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    machine.set_mode(GswMode::Gsw486);
    machine.set_timing_epoch_for_test(2);
    machine.write_physical_u32(GDT + 0x08, 0x0000_ffff);
    machine.write_physical_u32(GDT + 0x0c, 0x00cf_9b00);
    machine.write_physical_u32(GDT + 0x10, 0x0000_ffff);
    machine.write_physical_u32(GDT + 0x14, 0x00cf_9300);
    machine.cpu.gdtr = DescriptorTable {
        base: GDT,
        limit: 0x17,
    };
    machine.cpu.control.cr0 |= 1;
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
    for (offset, byte) in PROG.iter().copied().enumerate() {
        machine.write_physical_u8(0x100 + offset as u32, byte);
    }
    machine.cpu.registers.eip = 0x100;
    machine.cpu.registers.eflags &= !0x200;
    machine.set_fatal_ports(&[0x2010]);
    for (offset, byte) in [0xb0, 0xa5, 0xf4, 0x90].into_iter().enumerate() {
        machine.write_physical_u8(DST + offset as u32, byte);
    }
    let source = [0xb0, 0x5a, 0xf4, 0x90];
    for (offset, byte) in source.into_iter().enumerate() {
        machine.write_physical_u8(SRC + offset as u32, byte);
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x10, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x11, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x03, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x87, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x0a, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x08, BusWidth::Byte, mem_to_mem_enabled as u32, false)
            .unwrap();
    });
    machine.port_bus_batch_clocks = 0;
    machine
}

#[test]
fn a_ring0_software_dma_copy_is_reconciled_by_the_same_fatal_return() {
    const DST: u32 = 0x1100;
    for mem_to_mem_enabled in [true, false] {
        let mut machine = ring0_software_dma_copy_machine(mem_to_mem_enabled);
        machine.cpu.registers.eip = DST;
        let warm = machine.run_until_halt_or_cycles(1).unwrap();
        assert!(matches!(warm, StopReason::CycleLimit { requested: 1 }));
        assert_eq!(machine.cpu.registers.eip, DST + 2);
        assert_eq!(machine.cpu.registers.eax() as u8, 0xa5);

        // Repositioning EIP preserves the warmed destination's decode state. The
        // guest's MOV AL,4 restores the request value before its OUT 09.
        machine.cpu.registers.eip = 0x100;
        let resets = machine.cpu.perf_counters().device_write_coarse_resets;
        let batch = machine.test_batch_observations.len();
        let stop = machine.run_until_halt_or_cycles(1_000_000).unwrap();
        assert!(matches!(stop, StopReason::CpuError(_)));
        let observation = &machine.test_batch_observations[batch];
        assert!(observation.fatal);
        assert_eq!(
            observation.device_wrote_memory_before_reconcile,
            mem_to_mem_enabled
        );
        assert!(!machine.device_wrote_memory);
        assert_eq!(
            machine.cpu.perf_counters().device_write_coarse_resets,
            resets + if mem_to_mem_enabled { 1 } else { 0 }
        );
        for (offset, byte) in [0xb0, 0x5a, 0xf4, 0x90].into_iter().enumerate() {
            assert_eq!(
                machine.read_physical_u8(DST + offset as u32),
                if mem_to_mem_enabled {
                    byte
                } else {
                    [0xb0, 0xa5, 0xf4, 0x90][offset]
                }
            );
        }

        machine.cpu.registers.eip = DST;
        assert_eq!(
            machine.run_until_halt_or_cycles(1_000_000).unwrap(),
            StopReason::Halted
        );
        assert_eq!(
            machine.cpu.registers.eax() as u8,
            if mem_to_mem_enabled { 0x5a } else { 0xa5 }
        );
    }
}

#[test]
fn a_split_vga_word_out_reconciles_its_data_map_before_a_later_fatal() {
    // The word OUT changes GC6 through 03cf and its passive 03d0 neighbour in
    // one ordinary batch. The later IN is independently fatal, proving it
    // cannot re-consume the completed data-map invalidation.
    const PROG: &[u8] = &[
        0xbb, 0x11, 0x11, // mov bx,1111h
        0xba, 0xcf, 0x03, // mov dx,03cfh
        0xb8, 0x00, 0x00, // mov ax,0000h
        0xef, // out dx,ax
        0xba, 0x10, 0x20, // mov dx,2010h
        0xec, // in al,dx
        0xb0, 0x5a, // mov al,5ah
        0xf4, // hlt
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    machine.set_mode(GswMode::Gsw486);
    machine.set_timing_epoch_for_test(2);
    assert!(machine.set_vga_mode(0x13));
    assert_eq!(machine.vega.direct_write_token(), 1);
    with_bus(&mut machine, |bus| {
        bus.write_io(0x3ce, BusWidth::Byte, 0x06, false).unwrap();
        *bus.direct_map_changed = false;
        *bus.direct_data_map_changed = false;
        *bus.io_touched = false;
    });
    machine.set_fatal_ports(&[0x2010]);
    let code_generation = machine.cpu.decode_cache_generation();
    let audit_before = machine.cpu.fast_map_audit_counters();
    let batch = machine.test_batch_observations.len();
    let stop = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert!(
        matches!(&stop, StopReason::CpuError(text) if text.contains("0x2010")),
        "the separate IN must reach the configured fatal open port: {stop:?}"
    );
    let observations = &machine.test_batch_observations[batch..];
    let remap_index = observations
        .iter()
        .position(|observation| observation.direct_data_map_changed_before_reconcile)
        .expect("the real GC6 data write must reach ordinary reconciliation");
    let remap = &observations[remap_index];
    assert!(!remap.fatal);
    assert!(!remap.direct_map_changed_before_reconcile);
    let fatal_index = observations
        .iter()
        .position(|observation| observation.fatal)
        .expect("the later IN must have its own fatal batch");
    assert!(
        remap_index < fatal_index,
        "the ordinary GC6 remap must settle before the separate fatal IN"
    );
    let fatal = &observations[fatal_index];
    assert!(!fatal.direct_map_changed_before_reconcile);
    assert!(!fatal.direct_data_map_changed_before_reconcile);
    assert_eq!(machine.vega.direct_write_token(), 0);
    assert!(!machine.direct_map_changed);
    assert!(!machine.direct_data_map_changed);
    assert_eq!(machine.cpu.decode_cache_generation(), code_generation);
    let audit_after = machine.cpu.fast_map_audit_counters();
    assert_eq!(audit_after.wipes_direct_map, audit_before.wipes_direct_map);
    assert_eq!(
        audit_after.wipes_direct_data_map,
        audit_before.wipes_direct_data_map + 1
    );

    let resumed = machine.test_batch_core_totals.len();
    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(
        machine.test_batch_core_totals[resumed..]
            .iter()
            .sum::<u64>(),
        1,
        "the later MOV AL,5ah must retire its one-clock work without fatal debt"
    );
    assert_eq!(machine.cpu.registers.eax() as u8, 0x5a);
    assert_eq!(machine.vega.direct_write_token(), 0);
    assert!(!machine.direct_map_changed);
    assert!(!machine.direct_data_map_changed);
    let audit_after_resume = machine.cpu.fast_map_audit_counters();
    assert_eq!(
        audit_after_resume.wipes_direct_map,
        audit_after.wipes_direct_map
    );
    assert_eq!(
        audit_after_resume.wipes_direct_data_map,
        audit_after.wipes_direct_data_map
    );
}

/// An access to an undecoded port USED TO BE fatal by default. It is not any
/// more -- real hardware floats an unclaimed read and swallows an unclaimed
/// write, and stopping on the first one hid every later probe (see
/// `bus::OpenBusPorts`). The fatal path survives as an opt-in, which is exactly
/// what these tests need and what they arm with `set_fatal_ports`: it is the
/// only path that records a `fault_site`, and chasing which instruction probes
/// one specific port is still worth doing.
///
/// When a port IS fatal, the stop reports the whole diagnosis, and until this
/// test it named the wrong
/// instruction: EIP advances at fetch, and the fatal path did not rewind it, so
/// the report pointed one instruction PAST the IN or OUT. Prince of Persia was
/// investigated for hours off a CS:IP that was a return address.
///
/// The address and the port are both load-bearing. Checking the address alone
/// would also pass if the IN never ran at all (a decode refusal, a segment
/// fault, any earlier stop), so the port is checked too: the fixture must not
/// be able to pass by never reaching the instruction it is about. The third
/// assertion, on cs_moved, is documentation rather than a guard: this fixture
/// runs in real mode, where nothing here can move CS.
#[test]
fn a_fatal_port_fault_names_the_faulting_instruction_not_the_next_one() {
    // 0x100: BA 10 20  mov dx, 0x2010    <- 0x2010 is decoded by nothing
    // 0x103: EC        in  al, dx        <- the faulting instruction
    // 0x104: CD 20     int 20h
    const PROG: &[u8] = &[0xBA, 0x10, 0x20, 0xEC, 0xCD, 0x20];
    const IN_AL_DX: u32 = 0x103;

    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    // Opt 0x2010 back onto the fatal path; open bus does not stop, and a stop is
    // what carries the fault site this test is about.
    machine.set_fatal_ports(&[0x2010]);
    let elapsed_before = machine.elapsed_clocks();
    let ticks_before = machine.master_ticks();
    let raw_bus_before = machine.raw_bus_clocks();
    let scaled_bus_before = machine.scaled_bus_clocks();
    let batch_before = machine.test_batch_core_totals.len();
    let stop = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert!(
        matches!(&stop, StopReason::CpuError(text) if text.contains("0x2010")),
        "expected the run to stop on the undecoded port, got {stop:?}"
    );
    let site = machine
        .cpu()
        .fault_site()
        .expect("a fatal CpuError must record where it was raised");
    assert_eq!(
        site.eip, IN_AL_DX,
        "the recorded site must be the IN itself, not the instruction after it"
    );
    assert!(
        !site.cs_moved,
        "IN cannot change CS, so the recorded segment must be trustworthy"
    );
    assert!(machine.elapsed_clocks() > elapsed_before);
    assert!(machine.master_ticks() > ticks_before);
    assert!(machine.raw_bus_clocks() > raw_bus_before);
    assert!(machine.scaled_bus_clocks() > scaled_bus_before);
    let fatal_core = *machine
        .test_batch_core_totals
        .get(batch_before)
        .expect("the fatal batch must expose its settled core once");
    assert_eq!(
        fatal_core, 0,
        "I386's first raw-2 MOV leaves carry 4, not a clock"
    );
}

#[test]
fn a_fatal_settlement_advances_an_armed_pit_without_acknowledging_it() {
    // Keep IF clear. The PIT edge raised while the fatal batch settles must be
    // left for a later batch, not acknowledged by fatal handling.
    const PROG: &[u8] = &[
        0xb8, 0x01, 0x00, 0xbb, 0x02, 0x00, 0xb9, 0x03, 0x00, 0xba, 0x10, 0x20, 0xec, 0xeb, 0xfd,
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    machine.set_fatal_ports(&[0x2010]);
    machine.cpu.registers.eflags &= !0x200;
    with_bus(&mut machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0x34, false).unwrap();
        bus.write_io(0x40, BusWidth::Byte, 0x02, false).unwrap();
        bus.write_io(0x40, BusWidth::Byte, 0x00, false).unwrap();
    });
    let mut reference =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    reference.cpu.registers.eflags &= !0x200;
    with_bus(&mut reference, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0x34, false).unwrap();
        bus.write_io(0x40, BusWidth::Byte, 0x02, false).unwrap();
        bus.write_io(0x40, BusWidth::Byte, 0x00, false).unwrap();
    });
    // This is a valid I386 scaler remainder. The expected bus lane below is
    // calculated from the actual raw trace, never from the settled step.
    machine.bus_rem = 7;
    reference.bus_rem = 7;
    const FATAL_INTERVAL: u64 = 32;
    let next_irq_deadline = machine.event_batch_cap(u64::MAX);
    assert!(next_irq_deadline >= FATAL_INTERVAL);
    reference.advance_devices(next_irq_deadline - FATAL_INTERVAL);
    machine.advance_devices(next_irq_deadline - FATAL_INTERVAL);
    // Raw-program construction has already seeded channel 0. Clear that
    // setup-era edge before arming the measured interval; this direct PIC
    // normalization is outside a Machine run and precedes the INTA baseline.
    for candidate in [&mut machine, &mut reference] {
        with_bus(candidate, |bus| {
            bus.write_io(0x20, BusWidth::Byte, 0x11, false).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x08, false).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x04, false).unwrap();
            bus.write_io(0x21, BusWidth::Byte, 0x01, false).unwrap();
        });
    }
    assert!(
        !machine.pic.irr_bit(0),
        "the short PIT interval starts unraised"
    );
    assert_eq!(machine.event_batch_cap(u64::MAX), FATAL_INTERVAL);
    let acknowledges = machine.inta_diag.acknowledge_count;
    let elapsed = machine.elapsed_clocks();
    let raw_bus = machine.raw_bus_clocks();
    let scaled_bus = machine.scaled_bus_clocks();
    let batches = machine.test_batch_core_totals.len();

    let stop = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert!(matches!(stop, StopReason::CpuError(_)));
    assert!(machine.elapsed_clocks() > elapsed);
    let settled_core = machine.test_batch_core_totals[batches];
    let observation = &machine.test_batch_observations[batches];
    let settled_step = machine.test_batch_steps[batches];
    let settled_isa = machine.test_batch_isa_clocks[batches];
    let settled_scaled_bus = machine.scaled_bus_clocks() - scaled_bus;
    assert_eq!(machine.raw_bus_clocks() - raw_bus, 40);
    assert_eq!(observation.raw_bus_clocks, 40);
    assert_eq!(observation.bus_rem_at_entry, 7);
    assert!(observation.fatal);
    assert_eq!(observation.isa_clocks, 0, "I386 epoch one has no ISA lane");
    let scaled_from_raw = ((u128::from(observation.raw_bus_clocks) * 23 + 7) / 31) as u64;
    let remainder_from_raw = ((u128::from(observation.raw_bus_clocks) * 23 + 7) % 31) as u64;
    let independent_step = 3 + scaled_from_raw;
    assert_eq!(settled_core, 3);
    #[cfg(feature = "jit")]
    assert_eq!(machine.cpu.poll_skip_timing_remainder(), 1);
    assert_eq!(settled_scaled_bus, 29);
    assert_eq!(settled_isa, 0);
    assert_eq!(settled_step, FATAL_INTERVAL);
    assert_eq!(independent_step, FATAL_INTERVAL);
    assert_eq!(observation.step, FATAL_INTERVAL);
    assert_eq!(remainder_from_raw, 28);
    assert_eq!(machine.bus_rem, 28);
    assert_eq!(machine.elapsed_clocks() - elapsed, FATAL_INTERVAL);
    reference.advance_cpu_work(FATAL_INTERVAL, 3);
    assert_eq!(machine.pit, reference.pit);
    assert_eq!(machine.timeline, reference.timeline);
    assert!(
        machine.pic.irr_bit(0),
        "the fatal settlement must advance the armed PIT through an IRQ0 edge; step={} raw={} core={}",
        independent_step,
        observation.raw_bus_clocks,
        settled_core
    );
    assert_eq!(machine.pic.irr_bit(0), reference.pic.irr_bit(0));
    assert_eq!(
        machine.inta_diag.acknowledge_count, acknowledges,
        "fatal settlement must not service or acknowledge the newly pending IRQ0"
    );
    assert_eq!(
        settled_core, 3,
        "four state-changing raw-2 MOVs scale to three I386 clocks with carry 0"
    );

    assert_eq!(
        machine.device_edge_cache,
        crate::timing::DeviceEdgeCache::Stale
    );
    let cached_cap = machine.event_batch_cap_cached(u64::MAX);
    let fresh_cap = machine.event_batch_cap(u64::MAX);
    assert_eq!(cached_cap, fresh_cap);

    let resumed_elapsed = machine.elapsed_clocks();
    let resumed_raw_bus = machine.raw_bus_clocks();
    let resumed_scaled_bus = machine.scaled_bus_clocks();
    let resumed_batches = machine.test_batch_core_totals.len();
    let resumed = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert!(matches!(resumed, StopReason::CpuError(_)));
    assert!(machine.elapsed_clocks() > resumed_elapsed);
    assert!(machine.raw_bus_clocks() > resumed_raw_bus);
    let resumed_observation = &machine.test_batch_observations[resumed_batches];
    let resumed_raw = machine.raw_bus_clocks() - resumed_raw_bus;
    let resumed_scaled = ((u128::from(resumed_observation.raw_bus_clocks) * 23
        + u128::from(resumed_observation.bus_rem_at_entry))
        / 31) as u64;
    let resumed_remainder = ((u128::from(resumed_observation.raw_bus_clocks) * 23
        + u128::from(resumed_observation.bus_rem_at_entry))
        % 31) as u64;
    assert!(resumed_observation.fatal);
    assert_eq!(resumed_observation.raw_bus_clocks, resumed_raw);
    assert_eq!(resumed_observation.bus_rem_at_entry, 28);
    assert_eq!(machine.test_batch_core_totals[resumed_batches], 3);
    #[cfg(feature = "jit")]
    assert_eq!(machine.cpu.poll_skip_timing_remainder(), 0);
    assert_eq!(resumed_observation.scaled_bus_clocks, resumed_scaled);
    assert_eq!(
        machine.scaled_bus_clocks() - resumed_scaled_bus,
        resumed_scaled
    );
    let resumed_step = 3 + resumed_scaled;
    assert_eq!(resumed_observation.step, resumed_step);
    assert_eq!(machine.bus_rem, resumed_remainder);
    assert_eq!(machine.elapsed_clocks() - resumed_elapsed, resumed_step);
    reference.advance_cpu_work(resumed_step, 3);
    assert_eq!(machine.pit, reference.pit);
    assert_eq!(machine.timeline, reference.timeline);
    assert_eq!(
        machine.inta_diag.acknowledge_count, acknowledges,
        "the IF-clear GUI-style resume must not pay the prior stop by servicing IRQ0"
    );
    assert_eq!(
        machine.device_edge_cache,
        crate::timing::DeviceEdgeCache::Stale
    );
    let cached_cap = machine.event_batch_cap_cached(u64::MAX);
    let fresh_cap = machine.event_batch_cap(u64::MAX);
    assert_eq!(cached_cap, fresh_cap);
}

#[test]
fn an_i486_fatal_port_keeps_its_attempted_isa_lane_once() {
    // Four MOVs retire before the failed IN. Epoch two prices the actual
    // unclaimed byte port attempt on ISA even though the IN contributes no
    // completed core clocks.
    const PROG: &[u8] = &[
        0xb8, 0x01, 0x00, 0xbb, 0x02, 0x00, 0xb9, 0x03, 0x00, 0xba, 0x10, 0x20, 0xec,
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    machine.set_mode(GswMode::Gsw486);
    machine.set_timing_epoch_for_test(2);
    machine.set_fatal_ports(&[0x2010]);
    let batch = machine.test_batch_observations.len();
    let stop = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert!(matches!(stop, StopReason::CpuError(_)));
    let observation = &machine.test_batch_observations[batch];
    assert!(observation.fatal);
    assert_eq!(observation.core_clocks, 4);
    assert_eq!(observation.isa_clocks, 52);
    assert_eq!(observation.bus_rem_at_entry, 0);
    assert_eq!(observation.bus_rem_at_exit, 0);
    assert_eq!(
        observation.step,
        observation.core_clocks + observation.scaled_bus_clocks + observation.isa_clocks
    );
}

/// The byte dump used to hand a LINEAR address to `read_physical_u8`, which
/// does no page walk, so under paging it printed whatever happened to sit at
/// that physical address. It never said so; it printed plausible hex either
/// way, which is the failure mode that makes a diagnostic worse than useless.
///
/// The fixture puts the faulting code in a page whose linear address is NOT its
/// physical one, and plants a decoy at the physical address. The decoy is what
/// makes this test able to fail: without it, an unfixed dump reading the
/// physical address would find zeros, and "not the instruction" and "zeros"
/// would be indistinguishable from a correct read of an unmapped page. The
/// precondition assertion pins that the decoy really is where the broken path
/// would look.
#[test]
fn the_fault_dump_reads_code_through_the_guest_page_tables() {
    const PD: u32 = 0x1000;
    const PT: u32 = 0x2000;
    // Linear page 5 is mapped to a frame at 0x9000, so linear != physical for
    // the code the dump has to find.
    const CODE_LINEAR: u32 = 0x5000;
    const CODE_FRAME: u32 = 0x9000;
    // mov edx,0x2010; in al,dx; hlt. CS here is a 32-bit descriptor, so the
    // immediate is four bytes: the 16-bit encoding would swallow the IN as part
    // of it and the fixture would run off into unmapped memory.
    const PROG: [u8; 7] = [0xBA, 0x10, 0x20, 0x00, 0x00, 0xEC, 0xF4];
    const DECOY: u8 = 0xA5;

    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &[0xf4]).unwrap();
    machine.set_fatal_ports(&[0x2010]);
    machine.write_physical_u32(PD, PT | 7);
    // Identity-map the whole first 4 MB, so nothing in the fixture can take a
    // page fault for an unrelated reason, and override exactly one page.
    for page in 0u32..1024 {
        let pte = if page == CODE_LINEAR >> 12 {
            CODE_FRAME | 7
        } else {
            (page << 12) | 7
        };
        machine.write_physical_u32(PT + page * 4, pte);
    }
    for (offset, byte) in PROG.iter().enumerate() {
        machine.write_physical_u8(CODE_FRAME + offset as u32, *byte);
    }
    // The decoy sits where the unfixed, identity-assuming dump would read.
    for offset in 0..PROG.len() as u32 {
        machine.write_physical_u8(CODE_LINEAR + offset, DECOY);
    }
    assert_eq!(
        machine.read_physical_u8(CODE_LINEAR),
        DECOY,
        "precondition: the physical address must hold the decoy, or this test \
         cannot tell a paging-aware read from an identity-assuming one"
    );

    machine.cpu.control.cr3 = PD;
    machine.cpu.control.cr0 |= 0x8000_0001;
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x08, 0x9b));
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        machine
            .cpu
            .registers
            .set_segment(segment, SegmentRegister::flat(0x10, 0x93));
    }
    machine.cpu.registers.eip = CODE_LINEAR;
    // No IDT is set up here, so a timer IRQ arriving mid-fixture would stop the
    // run on a nested delivery fault before the IN is ever reached. Mask it;
    // this fixture is about the dump, not about interrupt delivery.
    machine.cpu.registers.eflags &= !0x200;

    let stop = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert!(
        matches!(&stop, StopReason::CpuError(text) if text.contains("0x2010")),
        "expected the undecoded-port stop, got {stop:?}"
    );

    let error = CpuError::Bus(izarravm_bus::BusError::UnsupportedPort { port: 0x2010 });
    let report = machine.fault_trace_report(&error);
    let at_eip = report
        .lines()
        .find(|line| line.contains("bytes at/after EIP"))
        .expect("the report must carry the bytes at EIP");
    let before_eip = report
        .lines()
        .find(|line| line.contains("bytes before EIP"))
        .expect("the report must carry the bytes before EIP");

    // The window opens ON the IN, so at/after starts with the IN and the HLT
    // behind it, and the MOV that set up DX is in the window before. All of it
    // lives in the mapped frame, so none of it is reachable without the walk.
    assert!(
        at_eip.contains("ec f4"),
        "the dump must walk the page tables to the real instruction, got: {at_eip}"
    );
    assert!(
        before_eip.contains("ba 10 20 00 00"),
        "the preceding window must walk too, got: {before_eip}"
    );
    assert!(
        !at_eip.contains("a5") && !before_eip.contains("a5"),
        "the dump read the physical address instead of translating:\n{before_eip}\n{at_eip}"
    );
}

/// The diagnosis has to arrive without anyone having set anything. That is the
/// whole of T3, and it is a claim about the CALL SITE being unconditional, not
/// about a formatter, so this drives a real machine to a real stop rather than
/// calling the formatter and watching it format.
///
/// No test here touches IZARRAVM_FAULT_TRACE. The CPU-side gate latches in a
/// OnceLock, so the first reader in a test binary fixes it process-wide, and
/// mutating process env from a threaded harness is racy anyway.
#[test]
fn a_fatal_port_fault_reports_itself_without_any_env_var() {
    const PROG: &[u8] = &[0xBA, 0x10, 0x20, 0xEC, 0xCD, 0x20];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    // Opt 0x2010 back onto the fatal path; open bus does not stop, and a stop is
    // what carries the fault site this test is about.
    machine.set_fatal_ports(&[0x2010]);
    machine.run_until_halt_or_cycles(1_000_000).unwrap();

    let line = machine
        .last_fault_line()
        .expect("a fatal stop must report itself with no env var set");
    assert!(line.contains("0x2010"), "must name the port: {line}");
    assert!(
        line.contains("0x00000103"),
        "must name the faulting instruction, not the one after it: {line}"
    );
    // The window opens ON the faulting instruction, so the first byte is its
    // opcode: 0xEC is IN AL,DX. That byte is the datum the old report lacked,
    // and having it is the difference between "port 0x2010 came from
    // somewhere" and "an IN AL,DX did this".
    assert!(
        line.contains("bytes=[ec cd 20"),
        "must carry the faulting instruction's own bytes: {line}"
    );
}

/// The report is latched on the SITE. Without a latch it floods, because a
/// fatal error leaves the machine resumable and callers do resume it; with a
/// plain "print once" it would hide every later fault, and the interesting one
/// is often not the first. Neither failure mode is visible by running the
/// emulator once, so it is pinned here.
#[test]
fn the_fault_report_latches_per_site_not_per_run() {
    // Both halves are driven for real, using the property that motivates the
    // latch in the first place: a fatal error leaves the machine resumable, so
    // calling run again continues the guest from where it stopped.

    // Spinning on ONE bad port: mov dx,0x2010; in al,dx; jmp back to the IN.
    // The second run re-enters at the JMP and faults at the same address.
    const SPIN: &[u8] = &[0xBA, 0x10, 0x20, 0xEC, 0xEB, 0xFD];
    let mut spinner =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), SPIN).unwrap();
    spinner.set_fatal_ports(&[0x2010]);
    let first_elapsed = spinner.elapsed_clocks();
    let first_ticks = spinner.master_ticks();
    let first_raw_bus = spinner.raw_bus_clocks();
    let first_scaled_bus = spinner.scaled_bus_clocks();
    let first_batch = spinner.test_batch_core_totals.len();
    spinner.run_until_halt_or_cycles(1_000_000).unwrap();
    let first_settled_core = spinner.test_batch_core_totals[first_batch];
    let first_elapsed = spinner.elapsed_clocks() - first_elapsed;
    let first_ticks = spinner.master_ticks() - first_ticks;
    let first_raw_bus = spinner.raw_bus_clocks() - first_raw_bus;
    let first_scaled_bus = spinner.scaled_bus_clocks() - first_scaled_bus;
    assert_eq!(first_settled_core, 0);
    assert!(first_elapsed > 0 && first_ticks > 0);
    assert!(first_raw_bus > 0 && first_scaled_bus > 0);
    let first = spinner
        .last_fault_line()
        .expect("first fault reports")
        .to_string();
    assert!(first.contains("0x00000103"));
    // Clear the record before the second run. Asserting the line is UNCHANGED
    // would not test the latch at all: a re-report at the same site rebuilds a
    // byte-identical string, so the assertion passes whether the latch runs or
    // not (verified: disabling the latch left that version green). Clearing
    // first makes a re-report visible, and also catches a fixture whose second
    // run silently never faulted.
    spinner.last_fault_line = None;
    let second_elapsed = spinner.elapsed_clocks();
    let second_ticks = spinner.master_ticks();
    let second_raw_bus = spinner.raw_bus_clocks();
    let second_scaled_bus = spinner.scaled_bus_clocks();
    let second_batch = spinner.test_batch_core_totals.len();
    spinner.run_until_halt_or_cycles(1_000_000).unwrap();
    let second_settled_core = spinner.test_batch_core_totals[second_batch];
    let second_elapsed = spinner.elapsed_clocks() - second_elapsed;
    let second_ticks = spinner.master_ticks() - second_ticks;
    let second_raw_bus = spinner.raw_bus_clocks() - second_raw_bus;
    let second_scaled_bus = spinner.scaled_bus_clocks() - second_scaled_bus;
    assert_eq!(
        spinner.last_fault_line(),
        None,
        "a repeat at the same site must not re-report, or a spinning guest \
         floods stderr for as long as it runs"
    );
    assert_eq!(second_settled_core, 3);
    assert!(second_elapsed > 0 && second_ticks > 0);
    assert!(second_raw_bus > 0 && second_scaled_bus > 0);
    assert_eq!(
        second_settled_core, 3,
        "the resumed JMP consumes the first call's carry but no prior return debt"
    );

    // TWO bad ports back to back. The second run faults one byte further on,
    // which is a different site and must get through: hiding it would bury the
    // fault that matters behind whichever one happened to come first.
    const TWO: &[u8] = &[0xBA, 0x10, 0x20, 0xEC, 0xEC, 0xCD, 0x20];
    let mut two =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), TWO).unwrap();
    two.set_fatal_ports(&[0x2010]);
    two.run_until_halt_or_cycles(1_000_000).unwrap();
    let one = two
        .last_fault_line()
        .expect("first fault reports")
        .to_string();
    assert!(one.contains("0x00000103"));
    two.run_until_halt_or_cycles(1_000_000).unwrap();
    let other = two.last_fault_line().expect("second site reports");
    assert!(
        other.contains("0x00000104"),
        "a fault at a different site must be reported, not swallowed: {other}"
    );
}

/// A clean run must leave nothing behind for a reporter to pick up. Nothing
/// clears the field, because a fatal CpuError leaves the machine resumable and
/// callers that ignore the stop reason go on running it, so the real guarantee
/// is on the READ side: only the fatal arm consults it.
///
/// Honest scope: this is a WEAK test, and the reason is worth keeping. Deleting
/// all three record calls leaves it green, because nothing in the fixture ever
/// writes the field and the assertion then just reads `FaultSite::default()`.
/// What it does catch is a `record_fault_site` call that wandered onto a
/// non-fault path. The stop-reason assertion is load-bearing for a different
/// reason: without it the test also passes when the program never ran at all
/// and the machine merely hit the cycle limit.
#[test]
fn a_run_that_did_not_fault_records_no_fault_site() {
    // mov ax,0x4c00; int 21h -- exits cleanly, touches no port.
    const PROG: &[u8] = &[0xB8, 0x00, 0x4C, 0xCD, 0x21];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    // Opt 0x2010 back onto the fatal path; open bus does not stop, and a stop is
    // what carries the fault site this test is about.
    machine.set_fatal_ports(&[0x2010]);
    let stop = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert!(
        matches!(stop, StopReason::DosExit { .. }),
        "the program must actually have run and exited, got {stop:?}"
    );
    assert!(machine.cpu().fault_site().is_none());
    assert!(machine.last_fault_line().is_none());
}
