// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn int11_returns_equipment_word() {
    // Stub: INT 11h then halt. AX must hold the seeded BDA equipment word.
    // The BIOS service vectors return through the ROM IRET at offset 0xF000
    // that rom_with_code supplies, matching the real izarra BIOS.
    let rom = rom_with_code(&[
        0xCD, 0x11, // int 11h
        0xF4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let ax = machine.cpu().registers.eax() as u16;
    assert_eq!(ax, BIOS_EQUIPMENT_WORD);
    // Bits 11-9 = 010b: two serial ports advertised (COM1 + COM2).
    assert_eq!((ax >> 9) & 0x07, 2, "two serial ports advertised");
    // Bits 15-14 = 10b: two parallel printer ports advertised (LPT1 + LPT2).
    assert_eq!((ax >> 14) & 0x03, 2, "two parallel ports advertised");
    // Bit 1 (80x87 coprocessor) stays clear: the Izarra 3000 has no FPU.
    assert_eq!(ax & 0x0002, 0, "no coprocessor advertised");
}

#[test]
fn int12_returns_conventional_memory_kib() {
    // Stub: INT 12h then halt. AX must hold the conventional memory size. The
    // 1 KB EBDA reserved at POST drops the reported size from 640 to 639 KB.
    let rom = rom_with_code(&[
        0xCD, 0x12, // int 12h
        0xF4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let ax = machine.cpu().registers.eax() as u16;
    assert_eq!(ax, BIOS_BASE_MEMORY_KIB - 1);
    assert_eq!(ax, 639);
}

#[test]
fn int1a_ah00_reads_bda_tick() {
    // Seed the BDA tick to 0x00012345, then INT 1Ah AH=00h returns CX:DX.
    let rom = rom_with_code(&[
        0xB4, 0x00, // mov ah, 0
        0xCD, 0x1A, // int 1Ah
        0xF4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.write_physical_u8(0x46c, 0x45);
    machine.write_physical_u8(0x46d, 0x23);
    machine.write_physical_u8(0x46e, 0x01);
    machine.write_physical_u8(0x46f, 0x00);
    machine.write_physical_u8(0x470, 0x00); // no rollover
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let cx = machine.cpu().registers.ecx() as u16;
    let dx = machine.cpu().registers.edx() as u16;
    assert_eq!(cx, 0x0001, "CX = high word of tick");
    assert_eq!(dx, 0x2345, "DX = low word of tick");
    assert_eq!(
        machine.cpu().registers.eax() as u8,
        0x00,
        "AL = rollover count"
    );
}

#[test]
fn int1a_ah02_ah04_return_bcd_clock() {
    // AH=04h clobbers CX/DX, so the AH=02h time result must be stashed to
    // memory before the date call overwrites it. Set DS=0, run AH=02h, store
    // CX/DX into BIOS scratch at 0:0500h, then run AH=04h and HLT. The date
    // result stays live in CX/DX; the time result is read back from scratch.
    let rom = rom_with_code(&[
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xD8, // mov ds, ax (DS = 0)
        0xB4, 0x02, 0xCD, 0x1A, // int 1Ah AH=02h (time)
        0x89, 0x0E, 0x00, 0x05, // mov [0500h], cx
        0x89, 0x16, 0x02, 0x05, // mov [0502h], dx
        0xB4, 0x04, 0xCD, 0x1A, // int 1Ah AH=04h (date)
        0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.seed_rtc(2026, 6, 21, 1, 13, 45, 30); // helper forwards to rtc.seed
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // After AH=04h: CH=century 0x20, CL=year 0x26, DH=month 0x06, DL=day 0x21.
    let cx = machine.cpu().registers.ecx() as u16;
    let dx = machine.cpu().registers.edx() as u16;
    assert_eq!(cx, 0x2026);
    assert_eq!(dx, 0x0621);
    // AH=02h stashed time: CH=hour 0x13, CL=minute 0x45, DH=second 0x30, DL=0.
    let time_cx = u16::from(machine.read_physical_u8(0x0500))
        | (u16::from(machine.read_physical_u8(0x0501)) << 8);
    let time_dx = u16::from(machine.read_physical_u8(0x0502))
        | (u16::from(machine.read_physical_u8(0x0503)) << 8);
    assert_eq!(time_cx, 0x1345, "CH=hour BCD, CL=minute BCD");
    assert_eq!(time_dx, 0x3000, "DH=second BCD, DL=0");
}

#[test]
fn int15_ah87_block_move_across_1mb() {
    // Build a GDT in low RAM with source = 0x20000, dest = 0x30000, move 4 words.
    let rom = rom_with_code(&[
        0xB4, 0x87, // mov ah,87h
        0xB9, 0x04, 0x00, // mov cx,4 (words)
        0xBE, 0x00, 0x10, // mov si,1000h (GDT offset)
        0xCD, 0x15, 0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    // ES = 0 so the GDT sits at linear 0x1000. Descriptors at +0x10 (src), +0x18 (dst).
    let gdt = 0x1000u32;
    let write_desc = |m: &mut Machine, at: u32, base: u32| {
        m.write_physical_u8(at, 0xFF); // limit low
        m.write_physical_u8(at + 1, 0xFF);
        m.write_physical_u8(at + 2, base as u8); // base 0..7
        m.write_physical_u8(at + 3, (base >> 8) as u8); // base 8..15
        m.write_physical_u8(at + 4, (base >> 16) as u8); // base 16..23
        m.write_physical_u8(at + 5, 0x93); // access
        m.write_physical_u8(at + 6, 0x00);
        m.write_physical_u8(at + 7, (base >> 24) as u8); // base 24..31
    };
    write_desc(&mut machine, gdt + 0x10, 0x20000);
    write_desc(&mut machine, gdt + 0x18, 0x30000);
    for i in 0..8u32 {
        machine.write_physical_u8(0x20000 + i, 0xA0 + i as u8);
    }
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    for i in 0..8u32 {
        assert_eq!(machine.read_physical_u8(0x30000 + i), 0xA0 + i as u8);
    }
    assert_eq!(
        (machine.cpu().registers.eax() as u16 >> 8) as u8,
        0x00,
        "AH=0 success"
    );
}

#[test]
fn int15_ah86_wait_advances_guest_clock() {
    let rom = rom_with_code(&[
        0xB4, 0x86, 0xB9, 0x00, 0x00, // CX=0
        0xBA, 0x40, 0x42, // DX=0x4240 -> with CX=0 that is 16960 us
        0xCD, 0x15, 0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    let before = machine.master_ticks();
    let reason = machine.run_until_halt_or_cycles(10_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let expected_stall = 16_960 * (izarravm_core::MASTER_CLOCK_HZ / 1_000_000);
    let advanced = machine.master_ticks() - before;
    assert!(
        advanced >= expected_stall / 2,
        "AH=86h stall too small: advanced {advanced} ticks, expected ~{expected_stall}"
    );
    let flags = machine.cpu().registers.eflags;
    assert_eq!(flags & 0x0001, 0, "CF clear after WAIT");
}

#[test]
fn microsecond_stalls_map_exactly_to_master_ticks() {
    let mut machine = test_machine();
    let before = machine.master_ticks();
    machine.stall_for_micros(16_960);
    assert_eq!(
        machine.master_ticks() - before,
        16_960 * (izarravm_core::MASTER_CLOCK_HZ / 1_000_000)
    );
}

#[test]
fn live_mode_switches_keep_one_master_deadline_and_device_phase() {
    let rom = rom_with_code(&[
        0xB0, 0x01, 0xE6, 0xE1, // 486
        0xB0, 0x00, 0xE6, 0xE1, // 386
        0xB0, 0x03, 0xE6, 0xE1, // 386-slow
        0xFA, // cli
        0xEB, 0xFE, // jmp $
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.set_mode(GswMode::Gsw586);
    let requested = 200_000;
    let expected_ticks = machine.timeline.master_ticks_for_cpu_clocks(requested);
    let before = machine.master_ticks();

    let reason = machine.run_cycles(requested).unwrap();

    assert_eq!(reason, StopReason::CycleLimit { requested });
    assert_eq!(machine.active_mode(), GswMode::Gsw386Slow);
    let actual_ticks = machine.master_ticks() - before;
    assert!(actual_ticks >= expected_ticks);
    assert!(
        actual_ticks - expected_ticks < 100 * machine.timeline.ticks_per_cpu_clock(),
        "run deadline overshot by {} ticks (expected {expected_ticks}, actual {actual_ticks})",
        actual_ticks - expected_ticks
    );

    // Advancing a fresh slow-mode machine by the same fixed duration must land
    // every rational device phase at the same point. The three set_mode calls
    // above therefore changed only future CPU quanta.
    let mut baseline = test_machine();
    baseline.set_mode(GswMode::Gsw386Slow);
    baseline.advance_devices_ticks(actual_ticks);
    assert_eq!(machine.timeline, baseline.timeline);
    assert_eq!(machine.video.beam_dots(), baseline.video.beam_dots());
    assert_eq!(machine.pit.channel_out(0), baseline.pit.channel_out(0));
    assert_eq!(machine.pit.channel_out(2), baseline.pit.channel_out(2));
}

#[test]
fn device_only_advance_moves_global_time_not_cpu_work() {
    let mut machine = test_machine();
    let work_before = machine.elapsed_clocks();
    let ticks_before = machine.master_ticks();
    machine.advance_devices_clocks(1000);
    assert_eq!(machine.elapsed_clocks(), work_before);
    assert_eq!(
        machine.master_ticks(),
        ticks_before + machine.timeline.master_ticks_for_cpu_clocks(1000)
    );
}

#[test]
fn storage_stall_advances_pit_audio_video_and_the_timeline() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap();
        bus.write_io(0x42, BusWidth::Byte, 0x20, false).unwrap();
        bus.write_io(0x42, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x61, BusWidth::Byte, 0x03, false).unwrap();
    });
    let ticks_before = machine.master_ticks();
    let beam_before = machine.video.beam_dots();
    let pit_before = machine.pit.channel_out(2);

    machine.stall_for(0.010);

    assert_eq!(
        machine.master_ticks() - ticks_before,
        izarravm_core::MASTER_CLOCK_HZ / 100
    );
    assert_ne!(machine.video.beam_dots(), beam_before);
    assert_ne!(machine.pit.channel_out(2), pit_before);
    assert!(
        machine.speaker.drain(441).iter().any(|&sample| sample != 0),
        "speaker samples must continue through a storage stall"
    );
}

#[test]
fn splitting_a_time_advance_preserves_device_state_and_irq_order() {
    fn configured() -> Machine {
        let mut machine = test_machine();
        with_bus(&mut machine, |bus| {
            bus.write_io(0x43, BusWidth::Byte, 0x34, false).unwrap();
            bus.write_io(0x40, BusWidth::Byte, 100, false).unwrap();
            bus.write_io(0x40, BusWidth::Byte, 0, false).unwrap();
            bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap();
            bus.write_io(0x42, BusWidth::Byte, 32, false).unwrap();
            bus.write_io(0x42, BusWidth::Byte, 0, false).unwrap();
            bus.write_io(0x61, BusWidth::Byte, 0x03, false).unwrap();
        });
        machine
    }

    let mut whole = configured();
    let mut split = configured();
    whole.advance_devices_clocks(12_345);
    for clocks in [1, 17, 337, 5_000, 6_990] {
        split.advance_devices_clocks(clocks);
    }

    assert_eq!(split.timeline, whole.timeline);
    assert_eq!(split.pit, whole.pit);
    assert_eq!(split.pic, whole.pic);
    assert_eq!(split.video.beam_dots(), whole.video.beam_dots());
    assert_eq!(split.speaker.drain(100), whole.speaker.drain(100));
}

#[test]
fn wall_shortfall_advances_devices_and_master_clock_together() {
    // The GUI's Approximate-class wall-clock top-up relies on this: when the
    // host could not execute the full budget, the unrun remainder must move
    // both device time and the fixed timeline, without claiming CPU work.
    let mut machine = test_machine();
    fn latched_count(m: &mut Machine) -> u16 {
        let mut bus = m.make_bus();
        bus.write_io(0x43, BusWidth::Byte, 0x00, false).unwrap(); // latch counter 0
        let lo = bus.read_io(0x40, BusWidth::Byte, 0, false).unwrap() as u16;
        let hi = bus.read_io(0x40, BusWidth::Byte, 0, false).unwrap() as u16;
        lo | (hi << 8)
    }
    {
        // Program PIT counter 0 (mode 3, reload 0 = 65536) so it counts; the
        // test ROM machine never ran the POST timer setup.
        let mut bus = machine.make_bus();
        bus.write_io(0x43, BusWidth::Byte, 0x36, false).unwrap();
        bus.write_io(0x40, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x40, BusWidth::Byte, 0x00, false).unwrap();
    }
    let before = machine.elapsed_clocks();
    let pit_before = latched_count(&mut machine);
    // 100_000 clocks at the 386 mode rate is ~4.5ms, thousands of PIT ticks,
    // and well short of the first vretrace start edge (the boot text-mode
    // beam sits at dot 0; the edge is ~289k clocks away), so the span has no
    // edge and must be consumed in full.
    let consumed = machine.advance_wall_shortfall(100_000);
    assert_eq!(
        consumed, 100_000,
        "a span with no intervening vretrace edge is consumed in full"
    );
    assert_eq!(
        machine.master_ticks(),
        before + machine.timeline.master_ticks_for_cpu_clocks(consumed),
        "advance_wall_shortfall must advance global guest time"
    );
    assert_ne!(
        latched_count(&mut machine),
        pit_before,
        "advance_wall_shortfall must advance device time (PIT counter 0 moved)"
    );
}

#[test]
fn wall_shortfall_stops_at_a_vretrace_start_edge_and_then_makes_progress() {
    // The P4d clamp: a top-up spanning a vretrace start edge must stop AT the
    // edge (vretrace bit 3 already readable) and report the shorter consume,
    // so the GUI can grant a polling guest an execution quantum there instead
    // of sweeping the whole window past it unobserved.
    let mut machine = test_machine();
    let clock_hz = machine.active_mode().clock_hz();
    let before = machine.master_ticks();
    // A full guest second: dozens of frames, so an edge is guaranteed inside.
    let consumed = machine.advance_wall_shortfall(clock_hz);
    assert!(
        consumed < clock_hz,
        "a span crossing a vretrace start edge must stop early (consumed {consumed})"
    );
    assert!(consumed > 0, "the stop must still make progress");
    assert_eq!(
        machine.master_ticks(),
        before + machine.timeline.master_ticks_for_cpu_clocks(consumed),
        "the timeline advances by the consumed active-mode clocks"
    );
    assert_ne!(
        machine.video_mut().read_status1() & 0x08,
        0,
        "the beam must land inside the vretrace window (bit 3 set at the stop)"
    );

    // Termination pin: with the beam ON the edge (inside the window), the
    // next call must not return 0. A short span still inside the window has
    // no NEXT start edge within it (that edge is a full frame ahead), so it
    // is consumed in full.
    let consumed_inside = machine.advance_wall_shortfall(10);
    assert_eq!(
        consumed_inside, 10,
        "on-edge/inside-window spans consume fully instead of stalling"
    );

    // And a long span from inside the window stops at the NEXT frame's edge,
    // roughly one frame period away, never zero.
    let consumed_next = machine.advance_wall_shortfall(clock_hz);
    assert!(consumed_next > 0 && consumed_next < clock_hz);
    assert_ne!(
        machine.video_mut().read_status1() & 0x08,
        0,
        "each stop lands inside the vretrace window"
    );
}

#[test]
fn paced_wall_topup_lets_a_polling_guest_catch_vretrace_windows() {
    // Permanent port of the P4d investigation repro. A mode-13h guest
    // double-polling port 0x3DA (wait for vretrace to clear, then wait for
    // it to set) is driven with the GUI's Approximate-class pacing pattern
    // at a 1/8 execution share: run 1/8 of each ~1ms quantum, then top the
    // remainder up wall-style. Unfixed (single unclamped top-up per
    // quantum), the guest caught 12.8-18.9 percent of the vretrace windows,
    // because a top-up sweeps the whole 2-scanline window past it with zero
    // instructions executing. With the edge clamp + peek it must catch
    // nearly all of them. Window count derives from beam geometry (frames
    // completed; each frame crosses exactly one vretrace start edge), so
    // the test is host-speed-independent and deterministic.
    let code = [
        0xB8, 0x13, 0x00, // mov ax, 0x0013 (mode 13h)
        0xCD, 0x10, // int 0x10
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x00, 0x70, 0x00, 0x00, // mov word [0x7000], 0 (catch counter)
        0xBA, 0xDA, 0x03, // mov dx, 0x03DA
        // wait_clear (0x12): spin while the vretrace bit is set
        0xEC, // in al, dx
        0xA8, 0x08, // test al, 0x08
        0x75, 0xFB, // jnz wait_clear
        // wait_set (0x17): spin until the vretrace bit sets
        0xEC, // in al, dx
        0xA8, 0x08, // test al, 0x08
        0x74, 0xFB, // jz wait_set
        0xFF, 0x06, 0x00, 0x70, // inc word [0x7000] (window caught)
        0xEB, 0xF0, // jmp wait_clear
    ];
    let mut machine = Machine::new(
        MachineProfile::gsw_386(4, VideoCard::Et4000Ax),
        rom_with_code(&code),
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw486); // Approximate class, 66 MHz
    let clock_hz = machine.active_mode().clock_hz();
    let quantum = clock_hz / 1000; // the GUI's ~1ms sub-slice

    // Warm up at full speed until the guest set mode 13h and is inside the
    // poll loop, then baseline the counters.
    machine.run_cycles(quantum).unwrap();
    let counter = |m: &Machine| m.memory.read_u16(0x7000).unwrap();
    let counter_base = u64::from(counter(&machine));
    let frames_base = machine.video().frames_completed();

    // One guest second of the paced pattern: ~70 mode-13h frames, plenty of
    // statistical power for a 90 percent threshold against a 13-19 percent
    // unfixed baseline, at half the runtime of a two-second run.
    for _ in 0..1000 {
        let before = machine.elapsed_clocks();
        machine.run_cycles(quantum / 8).unwrap();
        let ran = machine.elapsed_clocks().saturating_sub(before);
        let mut remaining = quantum.saturating_sub(ran);
        let mut stops = 0u32;
        while remaining > 0 {
            let consumed = machine.advance_wall_shortfall(remaining);
            assert!(consumed > 0, "termination: every call must make progress");
            remaining = remaining.saturating_sub(consumed);
            if remaining == 0 {
                break;
            }
            // Stopped at a vretrace start edge: grant the peek so the
            // polling guest observes the window, exactly like the GUI.
            stops += 1;
            assert!(
                stops <= 4,
                "termination: at most one edge fits in a 1ms quantum (plus slack)"
            );
            machine.run_cycles(VRETRACE_PEEK_CLOCKS).unwrap();
        }
    }

    let windows_opened = machine.video().frames_completed() - frames_base;
    let caught = u64::from(counter(&machine)) - counter_base;
    assert!(
        windows_opened >= 60,
        "geometry sanity: expected ~70 frames in 1 guest second, saw {windows_opened}"
    );
    assert!(
        caught <= windows_opened + 1,
        "sanity: cannot catch more windows than opened ({caught} vs {windows_opened})"
    );
    assert!(
        caught * 10 >= windows_opened * 9,
        "guest caught {caught} of {windows_opened} vretrace windows (< 90 percent); \
             unfixed baseline was 12.8-18.9 percent"
    );
}
