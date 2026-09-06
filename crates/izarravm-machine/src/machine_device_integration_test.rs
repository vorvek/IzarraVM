// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn timed_packet(machine: &mut Machine, cdb: [u8; 12]) {
    with_bus(machine, |bus| {
        bus.write_io(0x177, BusWidth::Byte, 0xa0, false).unwrap();
    });
    let accept = machine.ide.ticks_until_completion().unwrap();
    machine.advance_devices_ticks(accept);
    with_bus(machine, |bus| {
        let _ = bus.read_io(0x177, BusWidth::Byte, 0, false).unwrap();
        for byte in cdb {
            bus.write_io(0x170, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
    });
    while let Some(ticks) = machine.ide.ticks_until_completion() {
        machine.advance_devices_ticks(ticks);
    }
}

fn timed_packet_data_out(machine: &mut Machine, cdb: [u8; 12], data: &[u8]) {
    timed_packet(machine, cdb);
    with_bus(machine, |bus| {
        for &byte in data {
            bus.write_io(0x170, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
    });
    while let Some(ticks) = machine.ide.ticks_until_completion() {
        machine.advance_devices_ticks(ticks);
    }
}

#[test]
fn v86spike_enters_v86_and_signals() {
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        boot_image_with(izarravm_firmware::V86SPIKE_BIN),
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(50_000_000).unwrap();
    assert_eq!(
        reason,
        StopReason::TestExit { code: 0xA5 },
        "V86 spike did not reach the V86 stub and signal (stop={reason:?})"
    );
}

#[test]
fn hlt_wakes_on_pit_timer_tick() {
    // Boot code: init the PIC, unmask IRQ0, program channel 0 (mode 3, count 1000),
    // install IVT[8] -> a handler that bumps [0x0500] and EOIs, sti, hlt, then
    // cli, hlt. The run loop must fast-forward to the IRQ0 edge and wake the CPU.
    // The count is large enough that the handler finishes long before the next
    // tick, so the cli after hlt runs and the program reaches a genuine halt.
    let code: &[u8] = &[
        0xb0, 0x11, 0xe6, 0x20, 0xb0, 0x08, 0xe6, 0x21, 0xb0, 0x04, 0xe6, 0x21, 0xb0, 0x01, 0xe6,
        0x21, 0xb0, 0xfe, 0xe6, 0x21, 0xb0, 0x36, 0xe6, 0x43, 0xb0, 0xe8, 0xe6, 0x40, 0xb0, 0x03,
        0xe6, 0x40, 0xc7, 0x06, 0x20, 0x00, 0x30, 0x7c, 0xc7, 0x06, 0x22, 0x00, 0x00, 0x00, 0xfb,
        0xf4, 0xfa, 0xf4, 0xff, 0x06, 0x00, 0x05, 0xb0, 0x20, 0xe6, 0x20, 0xcf,
    ];
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        boot_image_with(code),
    )
    .unwrap();

    let halted_before = machine.halted_ticks();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    let tick = u16::from_le_bytes([
        machine.memory().as_slice()[0x0500],
        machine.memory().as_slice()[0x0501],
    ]);
    assert_eq!(tick, 1, "the IRQ0 handler should have run once");
    // One tick is about 1000 PIT clocks, near 18000 CPU clocks at 22 MHz, so a
    // real fast-forward clears this slack floor while a no-op halt would not.
    assert!(
        machine.elapsed_clocks() > 10_000,
        "the fast-forward should have advanced emulated time across the tick interval"
    );
    assert!(
        machine.halted_ticks() > halted_before,
        "the HLT wait must be visible to host speed reporting"
    );
}

#[test]
fn tsc_keeps_running_while_hlt_waits_for_irq0() {
    let mut code = vec![
        0xb0, 0x11, 0xe6, 0x20, 0xb0, 0x08, 0xe6, 0x21, 0xb0, 0x04, 0xe6, 0x21, 0xb0, 0x01, 0xe6,
        0x21, 0xb0, 0xfe, 0xe6, 0x21, 0xb0, 0x36, 0xe6, 0x43, 0xb0, 0xe8, 0xe6, 0x40, 0xb0, 0x03,
        0xe6, 0x40, 0xc7, 0x06, 0x20, 0x00, 0x00, 0x00, 0xc7, 0x06, 0x22, 0x00, 0x00, 0x00, 0x0f,
        0x31, 0x66, 0xa3, 0x00, 0x05, 0xfb, 0xf4, 0x0f, 0x31, 0x66, 0x2b, 0x06, 0x00, 0x05, 0x66,
        0xa3, 0x04, 0x05, 0xfa, 0xf4,
    ];
    let handler = (0x7c00 + code.len()) as u16;
    code[36..38].copy_from_slice(&handler.to_le_bytes());
    code.extend_from_slice(&[0xb0, 0x20, 0xe6, 0x20, 0xcf]);

    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let mut machine = Machine::new_boot_image(profile, boot_image_with(&code)).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    let bytes: [u8; 4] = machine.memory().as_slice()[0x0504..0x0508]
        .try_into()
        .unwrap();
    let halted_tsc_clocks = u32::from_le_bytes(bytes);
    assert!(
        u64::from(halted_tsc_clocks) > GswMode::Gsw586.clock_hz() / 2_000,
        "TSC advanced only {halted_tsc_clocks} clocks across the PIT wait"
    );
}

#[test]
fn given_the_boot_suite_image_when_it_halts_at_586_then_every_named_probe_passes() {
    // Given: the Lotura boot-suite image at Gsw586, using the same half-second
    // cycle budget `--headless-boot-suite` uses.
    // When: it runs to halt.
    // Then: every probe the old per-record / per-mode cargo tests asserted is
    // PASS, and elapsed_clocks shows real PIT time rather than an instant spin.
    // 386/486/386-slow loops only re-checked the same record names; the CLI
    // step still fails the job on any FAIL record at this same 586 profile.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let budget = profile.cpu.clock_rate().clocks_for_fraction_floor(1, 2);
    let mut machine =
        Machine::new_boot_image(profile, izarravm_firmware::X86_BOOT_TEST_IMAGE).unwrap();
    let reason = machine.run_until_halt_or_cycles(budget).unwrap();
    assert_eq!(reason, StopReason::Halted);

    let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
    for name in [
        "timer.irq0",
        "sound.sb_dsp_reset",
        "sound.opl3",
        "sound.opl2",
        "sound.sb_8bit_dma",
        "sound.sb_16bit_dma",
    ] {
        assert!(
            results.records.iter().any(|record| {
                record.status == izarravm_firmware::SuiteRecordStatus::Pass && record.name == name
            }),
            "boot suite should report PASS {name}; remaining={}, playing={}",
            machine.sb16.test_block_remaining(),
            machine.sb16.test_is_playing()
        );
    }
    // Clock-relative floor: 1_500_000 clocks was ~68 ms at 22 MHz and only ~9 ms
    // at 166 MHz, so it stopped meaning "the timer idle actually waited."
    // One-twentieth of a second is still far below the ten PIT ticks the probe
    // waits, and still fails a guest that spun instantly.
    let pit_floor = GswMode::Gsw586
        .clock_rate()
        .clocks_for_fraction_floor(1, 20);
    assert!(
        machine.elapsed_clocks() > pit_floor,
        "timer idle must advance guest time, not spin instantly (elapsed={}, floor={pit_floor})",
        machine.elapsed_clocks()
    );
}

#[test]
fn sb_dma_irq5_wakes_a_halted_cpu_via_fast_forward() {
    // A guest arms 8-bit single-cycle DMA + IRQ5, then `sti;hlt`. The run loop
    // must fast-forward across the DSP sample window (the new IRQ5 wake) and
    // deliver the block-completion IRQ5, so the handler runs and real emulated time
    // advances -- not a genuine no-wake halt. Setup mirrors the 8-bit probe.
    // Pinned to IRQ5 EXPLICITLY: the default is now IRQ7, and this fixture's
    // guest code hooks vector 0x0D / unmasks master bit 5 by hand. Following the
    // default would leave it asserting nothing while still passing.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.sound_blaster.irq = izarravm_core::SbIrq::I5;
    let mut machine = Machine::new(
        profile,
        // mov ax,0; mov ds,ax; sti; hlt; cli; hlt
        rom_with_code(&[0xb8, 0x00, 0x00, 0x8e, 0xd8, 0xfb, 0xf4, 0xfa, 0xf4]),
    )
    .unwrap();
    // 16-byte unsigned ramp at 0x01_0000 (DMA page 0x01, byte addr 0).
    for (i, b) in (0..16u8).map(|i| i * 16).enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    // IRQ5 handler at 0x0700: inc word [0x0610]; mov al,0x20; out 0x20,al; iret.
    let handler: [u8; 9] = [0xff, 0x06, 0x10, 0x06, 0xb0, 0x20, 0xe6, 0x20, 0xcf];
    for (i, &b) in handler.iter().enumerate() {
        machine.write_physical_u8(0x0700 + i as u32, b);
    }
    // IVT[0x0D] -> 0x0000:0x0700; clear the tick counter.
    machine.write_physical_u8(0x34, 0x00);
    machine.write_physical_u8(0x35, 0x07);
    machine.write_physical_u8(0x36, 0x00);
    machine.write_physical_u8(0x37, 0x00);
    machine.write_physical_u8(0x0610, 0x00);
    machine.write_physical_u8(0x0611, 0x00);
    with_bus(&mut machine, |bus| {
        // PIC base 0x08 (ICW1..ICW4) so IRQ5 -> vector 0x0D; all IRQs unmasked.
        bus.write_io(0x20, BusWidth::Byte, 0x11, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x08, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x01, false).unwrap();
        // DMA ch1: page 0x01, byte addr 0, count 15 (16 bytes), single read.
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        // DSP: 11025 Hz, block 16, single-cycle 8-bit DMA output.
        for &b in &[0x41u8, 0x2B, 0x11, 0x14, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    let master_before = machine.master_ticks();
    let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
    // The handler ran (after the cli the second hlt is genuine).
    assert_eq!(reason, StopReason::Halted);
    let ticks = u16::from(machine.read_physical_u8(0x0610))
        | (u16::from(machine.read_physical_u8(0x0611)) << 8);
    assert!(ticks >= 1, "the IRQ5 handler should have run");
    // The fast-forward crossed the full 16-sample block (about 32k CPU clocks
    // at 22 MHz), not a no-op halt.
    assert!(
        machine.elapsed_clocks() > 30_000,
        "the fast-forward should advance emulated time across the DSP sample window"
    );
    assert!(machine.master_ticks() > master_before);
}

#[test]
fn sb16_creative_adpcm_decodes_over_dma_and_raises_its_irq() {
    // End-to-end SB16 Creative ADPCM: a guest arms 4-bit ADPCM-with-reference
    // (DSP command 0x75) over 8-bit DMA channel 1, the clock-driven producer
    // pulls the encoded bytes, decodes them through the DSP, and raises the
    // 8-bit IRQ (IRQ7, the mixer default) at programmed block completion. Exercises the
    // real DMA -> DSP decode -> PIC path, not just the codec in isolation.
    let mut machine = test_machine();
    // 16 encoded DMA bytes at 0x01_0000 (page 0x01): a reference seed (0x80)
    // followed by 15 non-zero code bytes so the decoded output is audible.
    machine.write_physical_u8(0x1_0000, 0x80);
    for i in 1..16u32 {
        machine.write_physical_u8(0x1_0000 + i, 0x50);
    }
    with_bus(&mut machine, |bus| {
        // DMA ch1: page 0x01, byte addr 0, count 15 (16 bytes), single read.
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        // DSP: 11025 Hz, then 0x75 (4-bit ADPCM + reference), length 0x000F ->
        // 16 encoded bytes (the block counts DMA bytes, reference included).
        for &b in &[0x41u8, 0x2B, 0x11, 0x75, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    let rate = u64::from(machine.sb16.test_output_frame_rate());
    for _ in 0..30 {
        let clocks = machine
            .timeline
            .cpu_clocks_until(timeline::DeviceClock::Dsp, 1, rate)
            .unwrap();
        machine.advance_devices_clocks(clocks);
    }
    // The programmed-block IRQ latched on the SB16's default line (IRQ7).
    assert!(
        machine.pic.irr_bit(7),
        "Creative ADPCM block raised the 8-bit IRQ at block completion"
    );
    // Single-cycle playback stopped at the end of the block.
    assert!(
        !machine.sb16.test_is_playing(),
        "single-cycle ADPCM halted at TC"
    );
    // The decoder produced audible (non-silent) frames on the DSP ring: the
    // reference byte seeded 0x80 and the 0x50 code bytes moved it off center.
    let decoded: Vec<_> = std::iter::from_fn(|| machine.sb16.test_drain_frame()).collect();
    assert_eq!(decoded.len(), 30, "15 packed bytes produce 30 frames");
    assert!(
        decoded.iter().any(|&(left, _)| left != 0),
        "decoded ADPCM is audible, not flat silence"
    );
}

#[test]
fn cli_hlt_is_a_genuine_halt() {
    // With interrupts off, HLT must still halt immediately, not spin.
    // Pinned to IRQ5 EXPLICITLY: the default is now IRQ7, and this fixture's
    // guest code hooks vector 0x0D / unmasks master bit 5 by hand. Following the
    // default would leave it asserting nothing while still passing.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.sound_blaster.irq = izarravm_core::SbIrq::I5;
    let mut machine = Machine::new(
        profile,
        rom_with_code(&[0xfa, 0xf4]), // cli; hlt
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.halted_ticks(),
        0,
        "a terminal HLT has no hidden fast-forward"
    );
}

#[test]
fn pit_channel0_raises_irq0_while_running() {
    // cli; jmp $ keeps the CPU spinning with interrupts off, so advance_devices
    // ticks the PIT but the raised IRQ0 stays pending (never acknowledged).
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        rom_with_code(&[0xfa, 0xeb, 0xfe]),
    )
    .unwrap();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0x36, false).unwrap(); // counter 0, mode 3
        bus.write_io(0x40, BusWidth::Byte, 0x04, false).unwrap(); // count low
        bus.write_io(0x40, BusWidth::Byte, 0x00, false).unwrap(); // count high -> 4
    });
    machine.run_cycles(4000).unwrap();
    let pending = with_bus(&mut machine, |bus| bus.interrupt_pending());
    assert!(
        pending,
        "channel 0 should have raised IRQ0 over 4000 cycles"
    );
}

#[test]
#[ignore]
fn batch_throughput() {
    // cli; jmp $ — a tight interrupt-free loop with no port I/O, the case the
    // batch fully amortizes (one bus build + device fan-out per ~thousands of
    // instructions instead of per instruction).
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        rom_with_code(&[0xfa, 0xeb, 0xfe]),
    )
    .unwrap();
    let budget = 2_000_000_000u64;
    let t = std::time::Instant::now();
    machine.run_cycles(budget).unwrap();
    let secs = t.elapsed().as_secs_f64();
    println!(
        "batch_throughput: {budget} guest clocks in {secs:.3}s = {:.1} M guest-clocks/s",
        budget as f64 / secs / 1.0e6
    );
}

#[test]
fn fdc_read_data_streams_a_sector_into_memory_over_dma_channel_2() {
    // A guest that programs the FDC directly: arm DMA channel 2 for a
    // device->memory write, then issue READ DATA. The sector bytes must land
    // in the guest buffer through the channel-2 datapath, and the controller
    // must raise IRQ6 and present its result phase.
    let mut machine = test_machine();

    // 720 KB image (9 sectors/track, 2 heads). Seed CHS(2,0,3) with a marker.
    // LBA = (cyl*heads + head)*spt + (sector-1) = (2*2+0)*9 + 2 = 38.
    let mut img = vec![0u8; 737_280];
    // LBA for CHS(2,0,3) on a 9-spt, 2-head disk: (2*2 + 0)*9 + (3-1) = 38.
    let lba = 38usize;
    let off = lba * 512;
    for (i, slot) in img[off..off + 512].iter_mut().enumerate() {
        *slot = (0xA0 + (i & 0x0F)) as u8;
    }
    machine.mount_floppy(img).unwrap();

    // Guest DMA target buffer at physical 0x2000 (512 bytes).
    const BUF: u16 = 0x2000;

    with_bus(&mut machine, |bus| {
        // --- Program DMA channel 2: device->memory (write), single, count 512.
        bus.write_io(0x0B, BusWidth::Byte, 0x46, false).unwrap(); // mode ch2: single, write
        bus.write_io(0x0C, BusWidth::Byte, 0x00, false).unwrap(); // clear the flip-flop
        bus.write_io(0x04, BusWidth::Byte, u32::from(BUF & 0xFF), false)
            .unwrap();
        bus.write_io(0x04, BusWidth::Byte, u32::from(BUF >> 8), false)
            .unwrap();
        bus.write_io(0x81, BusWidth::Byte, 0x00, false).unwrap(); // page (A16-A23) = 0
        bus.write_io(0x05, BusWidth::Byte, 0xFF, false).unwrap(); // count low (511)
        bus.write_io(0x05, BusWidth::Byte, 0x01, false).unwrap(); // count high -> 0x01FF
        bus.write_io(0x0A, BusWidth::Byte, 0x02, false).unwrap(); // unmask channel 2

        // --- Drive the FDC.
        bus.write_io(0x3F2, BusWidth::Byte, 0x1C, false).unwrap(); // DOR: motor A, gate, out of reset, drive 0
        bus.write_io(0x3F5, BusWidth::Byte, 0x08, false).unwrap(); // SENSE INT (clear power-up irq)
        while bus.read_io(0x3F4, BusWidth::Byte, 0, false).unwrap() & 0x40 != 0 {
            bus.read_io(0x3F5, BusWidth::Byte, 0, false).unwrap();
        }
        // READ DATA: HDS+DS=0, C=2, H=0, R=3, N=2(512), EOT=3, GPL=0x1B, DTL=0xFF.
        for &b in &[0xE6u8, 0x00, 0x02, 0x00, 0x03, 0x02, 0x03, 0x1B, 0xFF] {
            bus.write_io(0x3F5, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });

    // The port write only scheduled the command. Advance from one fixed-timeline
    // deadline to the next so spin-up, rotation, and all 512 DMA cycles happen at
    // their own guest times.
    while machine.fdc.read_port(0x3F4).unwrap() & 0x40 == 0 {
        let ticks = machine
            .fdc
            .ticks_until_event(machine.master_ticks())
            .expect("the active FDC command has another deadline");
        machine.advance_devices_ticks(ticks);
    }

    // The sector landed in the guest buffer over channel 2.
    for i in 0..512usize {
        let got = machine.read_physical_u8(u32::from(BUF) + i as u32);
        let want = (0xA0 + (i & 0x0F)) as u8;
        assert_eq!(got, want, "byte {i} of the sector in memory");
    }

    assert_eq!(
        machine.dma.master.channels[2].transfer_cycles, 512,
        "one channel cycle per sector byte"
    );

    // The final DMA deadline has already forwarded IRQ6 to the PIC.
    let pending = with_bus(&mut machine, |bus| bus.interrupt_pending());
    assert!(pending, "FDC completion raised IRQ6");

    // The result phase is seven status bytes ending at sector 3.
    let result = with_bus(&mut machine, |bus| {
        let mut out = Vec::new();
        while bus.read_io(0x3F4, BusWidth::Byte, 0, false).unwrap() & 0x40 != 0 {
            out.push(bus.read_io(0x3F5, BusWidth::Byte, 0, false).unwrap() as u8);
        }
        out
    });
    assert_eq!(result.len(), 7, "ST0..N result phase");
    assert_eq!(result[0] & 0xC0, 0x00, "normal termination");
    assert_eq!(result[3], 2, "ending cylinder 2");
    assert_eq!(result[5], 3, "ending sector 3");
}

#[test]
fn pic_command_and_data_ports_route_to_the_master() {
    let mut machine = test_machine();
    let mask = with_bus(&mut machine, |bus| {
        // ICW1..ICW4 init, then OCW1 sets the mask to a recognizable value.
        for (port, value) in [
            (0x20u16, 0x11u32),
            (0x21, 0x08),
            (0x21, 0x04),
            (0x21, 0x01),
            (0x21, 0xab),
        ] {
            bus.write_io(port, BusWidth::Byte, value, false).unwrap();
        }
        // The data port reads back the mask, not the passive 0xff stub.
        bus.read_io(0x21, BusWidth::Byte, 0, false).unwrap()
    });
    assert_eq!(mask, 0xab);
}

#[test]
fn machine_bus_acknowledges_a_pic_interrupt() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        for (port, value) in [(0x20u16, 0x11u32), (0x21, 0x08), (0x21, 0x04), (0x21, 0x01)] {
            bus.write_io(port, BusWidth::Byte, value, false).unwrap();
        }
    });
    machine.request_irq(0);

    let (pending, vector) = with_bus(&mut machine, |bus| {
        (bus.interrupt_pending(), bus.acknowledge_interrupt())
    });
    assert!(pending);
    assert_eq!(vector, Some(0x08));
}

#[test]
fn opl_sounds_through_the_adlib_ports() {
    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        with_bus(&mut machine, |bus| program_tone(bus, 0x0388, 0x0389));
        let pcm = machine.render_audio(2000);
        assert!(
            pcm.iter().any(|&(l, _)| l != 0),
            "the OPL should produce audio via the AdLib ports in {mode:?}"
        );
    }
}

#[test]
fn opl_sounds_through_the_sound_blaster_aliases() {
    // 0x220/0x221 mirror the OPL3 primary-bank address/data ports.
    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        with_bus(&mut machine, |bus| program_tone(bus, 0x0220, 0x0221));
        let pcm = machine.render_audio(2000);
        assert!(
            pcm.iter().any(|&(l, _)| l != 0),
            "the OPL should produce audio via the SB base aliases in {mode:?}"
        );
    }
}

#[test]
fn opl_aliases_remain_live_when_the_sb16_path_is_disabled() {
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.sound_blaster.enabled = false;
    let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();

    with_bus(&mut machine, |bus| program_tone(bus, 0x0220, 0x0221));
    let pcm = machine.render_audio(2_000);

    assert!(pcm.iter().any(|&(left, _)| left != 0));
}

#[test]
fn play_audio_mixes_cd_audio_into_render_audio() {
    let mut machine = test_machine();
    machine.mount_cd(audio_cd(20));
    // Open the CD volume to full (5-bit registers 0x36/0x37) via the mixer.
    with_bus(&mut machine, |bus| {
        // The 5-bit level lives in D7-D3, so full volume is 31 << 3.
        for (index, value) in [(0x36u32, 0xF8u32), (0x37, 0xF8)] {
            bus.write_io(0x224, BusWidth::Byte, index, false).unwrap();
            bus.write_io(0x225, BusWidth::Byte, value, false).unwrap();
        }
    });
    // Issue PLAY AUDIO(10) over the secondary-channel ATAPI ports: PACKET
    // command, then the 12-byte CDB. Play from LBA 1 (audio start) for 16
    // frames.
    let mut cdb = [0u8; 12];
    cdb[0] = 0x45; // PLAY AUDIO(10)
    cdb[5] = 1; // starting LBA 1
    cdb[8] = 16; // 16 frames
    timed_packet(&mut machine, cdb);
    assert!(machine.cd_loaded());
    let pcm = machine.render_audio(2000);
    assert!(
        pcm.iter().any(|&(l, r)| l != 0 || r != 0),
        "PLAY AUDIO should mix nonzero CD audio into the DAC output"
    );
}

#[test]
fn mode_select_volume_scales_signed_cd_audio_channels() {
    let mut machine = test_machine();
    machine.mount_cd(audio_cd(20));
    with_bus(&mut machine, |bus| {
        // The 5-bit level lives in D7-D3, so full volume is 31 << 3.
        for (index, value) in [(0x36u32, 0xF8u32), (0x37, 0xF8)] {
            bus.write_io(0x224, BusWidth::Byte, index, false).unwrap();
            bus.write_io(0x225, BusWidth::Byte, value, false).unwrap();
        }
    });

    let mut params = vec![0u8; 24];
    params[8] = 0x0E;
    params[9] = 14;
    params[16] = 0x01;
    params[17] = 0xFF;
    params[18] = 0x02;
    params[19] = 0x80;
    let mut select = [0u8; 12];
    select[0] = 0x55;
    select[1] = 0x10;
    select[7..9].copy_from_slice(&(params.len() as u16).to_be_bytes());
    timed_packet_data_out(&mut machine, select, &params);

    let mut play = [0u8; 12];
    play[0] = 0x45;
    play[5] = 1;
    play[8] = 16;
    timed_packet(&mut machine, play);
    let pcm = machine.render_audio(2000);

    assert!(!pcm.is_empty());
    // The CD leg is at full volume, so these are the Red Book samples after the
    // drive's own per-channel scaling and the summing node's MIX_HEADROOM
    // (-6 dB), which every card source takes alike.
    assert!(
        pcm.iter()
            .all(|&(left, right)| (left, right) == (4000, -2007)),
        "drive volumes must scale positive and negative Red Book samples per channel"
    );
}

#[test]
fn cd_audio_is_silent_with_the_volume_muted() {
    // The CD line powers on at 0 dB, not muted (DOSBox-X `CTMIXER_Reset` sets
    // cda = 31; 86Box resets 0x36/0x37 to 0xF8). A guest has to ASK for the
    // mute, and this proves both halves: the untouched default passes audio, and
    // level 0 on 0x36/0x37 stops it.
    let start_playing = |machine: &mut Machine| {
        machine.mount_cd(audio_cd(20));
        let mut cdb = [0u8; 12];
        cdb[0] = 0x45;
        cdb[5] = 1;
        cdb[8] = 16;
        timed_packet(machine, cdb);
    };

    let mut default_volume = test_machine();
    start_playing(&mut default_volume);
    let pcm = default_volume.render_audio(2000);
    assert!(
        pcm.iter().any(|&(l, r)| l != 0 || r != 0),
        "the power-on CD level is 0 dB, so a playing disc is audible"
    );

    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        for index in [0x36u32, 0x37] {
            bus.write_io(0x224, BusWidth::Byte, index, false).unwrap();
            bus.write_io(0x225, BusWidth::Byte, 0x00, false).unwrap();
        }
    });
    start_playing(&mut machine);
    let pcm = machine.render_audio(2000);
    assert!(
        pcm.iter().all(|&(l, r)| l == 0 && r == 0),
        "a muted CD volume yields silence even while playing"
    );
}

#[test]
fn disabled_sb16_silences_ct1745_cd_input_without_stopping_transport() {
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.sound_blaster.enabled = false;
    let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();
    machine.mount_cd(audio_cd(20));
    machine.set_cd_linked_level(31);

    let mut cdb = [0u8; 12];
    cdb[0] = 0x45;
    cdb[5] = 1;
    cdb[8] = 16;
    timed_packet(&mut machine, cdb);
    assert!(machine.ide.device().mixer_audio_active());
    let pcm = machine.render_audio(20_000);

    assert!(machine.cd_audio_state().playing);
    assert_eq!(machine.cd_audio_state().left_level, 0);
    assert_eq!(machine.cd_audio_state().right_level, 0);
    assert!(!machine.ide.device().mixer_audio_active());
    assert!(pcm.iter().all(|&(left, right)| left == 0 && right == 0));
}

#[test]
fn front_panel_cd_controls_preserve_transport_sense_and_time() {
    let mut machine = test_machine();
    machine.mount_cd(audio_cd(20));
    machine.set_cd_linked_level(19);

    let transport = machine.ide.transport_state_snapshot();
    let ticks = machine.master_ticks();
    machine.cd_front_panel_play();
    assert!(machine.cd_audio_state().playing);
    assert_eq!(machine.cd_audio_state().left_level, 19);
    assert_eq!(machine.cd_audio_state().right_level, 19);
    assert_eq!(machine.ide.transport_state_snapshot(), transport);
    assert_eq!(machine.master_ticks(), ticks);

    let transport = machine.ide.transport_state_snapshot();
    machine.cd_front_panel_stop();
    assert!(!machine.cd_audio_state().playing);
    assert_eq!(machine.ide.transport_state_snapshot(), transport);
    assert_eq!(machine.master_ticks(), ticks);
}

#[test]
fn pending_guest_play_completed_after_host_stop_wins() {
    let mut machine = test_machine();
    machine.mount_cd(audio_cd(20));
    machine.cd_front_panel_play();

    with_bus(&mut machine, |bus| {
        bus.write_io(0x177, BusWidth::Byte, 0xa0, false).unwrap();
    });
    let accept = machine.ide.ticks_until_completion().unwrap();
    machine.advance_devices_ticks(accept);
    let mut play = [0u8; 12];
    play[0] = 0x45;
    play[5] = 2;
    play[8] = 8;
    with_bus(&mut machine, |bus| {
        for byte in play {
            bus.write_io(0x170, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
    });
    assert!(machine.ide.ticks_until_completion().is_some());
    machine.cd_front_panel_stop();
    assert!(!machine.cd_audio_state().playing);
    while let Some(ticks) = machine.ide.ticks_until_completion() {
        machine.advance_devices_ticks(ticks);
    }
    assert!(machine.cd_audio_state().playing);
    assert_eq!(machine.ide.device().playback().current_lba, 2);
}

#[test]
fn guest_eject_is_visible_in_live_cd_state() {
    let mut machine = test_machine();
    machine.mount_cd(audio_cd(20));
    assert!(machine.cd_audio_state().media_present);

    let mut eject = [0u8; 12];
    eject[0] = 0x1B;
    eject[4] = 0x02;
    timed_packet(&mut machine, eject);

    assert!(!machine.cd_audio_state().media_present);
    assert!(!machine.cd_audio_state().audio_capable);
}

#[test]
fn cd_mixer_cursor_resets_on_epoch_but_not_pause_resume() {
    let mut machine = test_machine();
    machine.mount_cd(audio_cd(20));
    machine.set_cd_linked_level(31);
    machine.cd_front_panel_play();
    let _ = machine.render_audio(100);
    let before_pause = machine.cd_audio_sample;
    assert!(before_pause > 0);

    let mut pause = [0u8; 12];
    pause[0] = 0x4B;
    timed_packet(&mut machine, pause);
    assert!(machine.cd_audio_state().paused);
    let paused_epoch = machine.ide.device().playback_epoch();
    let _ = machine.render_audio(100);
    assert_eq!(machine.cd_audio_sample, before_pause);

    machine.cd_front_panel_play();
    assert_eq!(machine.ide.device().playback_epoch(), paused_epoch);
    let _ = machine.render_audio(100);
    assert!(machine.cd_audio_sample > before_pause);

    machine.cd_front_panel_stop();
    assert_ne!(machine.ide.device().playback_epoch(), paused_epoch);
    let _ = machine.render_audio(1);
    assert_eq!(machine.cd_audio_sample, 0);
}

#[test]
fn icdex_install_check_reports_installed() {
    let mut machine = test_machine();
    // The probe pushes DADAh, then the INT pushed IP, CS, FLAGS over it, so
    // the marker sits at SS:SP+6. Stand in for that frame here.
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Ss, SegmentRegister::real(0x9000));
    machine.cpu.registers.set_esp(0x0100);
    let marker_addr = 0x9000 * 16 + (0x0100 + 6);
    machine.memory.write_u16(marker_addr, 0xDADA).unwrap();
    machine.cpu.registers.set_eax(0x1100);
    assert!(machine.handle_int2f());
    // AL = FFh means installed.
    assert_eq!(machine.cpu.registers.eax() as u8, 0xFF);
    // The pushed marker is rewritten to ADADh so a strict probe sees the
    // word change (RBIL INTERRUP.K, INT 2F/AX=1100h).
    assert_eq!(machine.memory.read_u16(marker_addr).unwrap(), 0xADAD);
}

#[test]
fn int2f_redirector_install_check_reports_not_installed_without_dada_marker() {
    let mut machine = test_machine();
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Ss, SegmentRegister::real(0x9000));
    machine.cpu.registers.set_esp(0x0100);
    let marker_addr = 0x9000 * 16 + (0x0100 + 6);
    // A pushed word other than DADAh is the plain redirector install check.
    // It must not claim IZCDEX installed or touch the stack word.
    machine.memory.write_u16(marker_addr, 0x1234).unwrap();
    machine.cpu.registers.set_eax(0xCAFE_1100);
    assert!(machine.handle_int2f());
    assert_eq!(machine.cpu.registers.eax(), 0xCAFE_1100);
    assert_eq!(machine.memory.read_u16(marker_addr).unwrap(), 0x1234);
}

#[test]
fn icdex_drive_check_reports_the_cd_drive() {
    let mut machine = test_machine();
    machine.mount_cd(audio_cd(4));
    // AX=1500: BX = drive count, CX = first drive letter (D: = 3).
    machine.cpu.registers.set_eax(0x1500);
    assert!(machine.handle_int2f());
    assert_eq!(machine.cpu.registers.ebx() as u16, 1);
    assert_eq!(
        machine.cpu.registers.ecx() as u16,
        u16::from(CD_DRIVE_NUMBER)
    );
    // AX=150B drive check for D:: BX = ADADh, AX nonzero (supported).
    machine.cpu.registers.set_eax(0x150B);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    assert!(machine.handle_int2f());
    assert_eq!(machine.cpu.registers.ebx() as u16, 0xADAD);
    assert_ne!(machine.cpu.registers.eax() as u16, 0);
}

#[test]
fn icdex_direct_calls_cover_metadata_vtoc_absolute_read_and_preferences() {
    let mut machine = test_machine();
    let mut bytes = vec![0u8; 20 * cdimage::DATA_SECTOR];
    bytes[0] = 0x42; // LBA 0 marker for AX=1508h.
    let pvd = 16 * cdimage::DATA_SECTOR;
    bytes[pvd] = 0x01; // primary volume descriptor
    bytes[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
    let root = iso_dir_record(18, cdimage::DATA_SECTOR as u32, 0x02, &[0]);
    bytes[pvd + 156..pvd + 156 + root.len()].copy_from_slice(&root);
    let file = iso_dir_record(19, 1234, 0x00, b"README.TXT;1");
    let root_sector = 18 * cdimage::DATA_SECTOR;
    bytes[root_sector..root_sector + root.len()].copy_from_slice(&root);
    bytes[root_sector + root.len()..root_sector + root.len() + file.len()].copy_from_slice(&file);
    let term = 17 * cdimage::DATA_SECTOR;
    bytes[term] = 0xff; // volume descriptor set terminator
    machine.mount_cd(CdImage::from_iso(bytes).unwrap());

    machine.write_guest_block(0x5000, &[0xaa; 38]);
    prime_dos_int_frame(&mut machine);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0));
    machine.cpu.registers.set_ebx(0x5000);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_eax(0x1502);
    assert!(machine.handle_int2f(), "AX=1502h handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "metadata clears CF");
    assert_eq!(machine.read_guest_block(0x5000, 38), vec![0; 38]);

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0x6000);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_edx(0);
    machine.cpu.registers.set_eax(0x1505);
    assert!(machine.handle_int2f(), "AX=1505h handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "VTOC clears CF");
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0001);
    assert_eq!(machine.read_physical_u8(0x6000), 0x01);
    assert_eq!(&machine.read_guest_block(0x6001, 5), b"CD001");

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0x7000);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_esi(0);
    machine.cpu.registers.set_edi(0);
    machine.cpu.registers.set_edx(1);
    machine.cpu.registers.set_eax(0x1508);
    assert!(machine.handle_int2f(), "AX=1508h handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "absolute read clears CF");
    assert_eq!(machine.read_physical_u8(0x7000), 0x42);

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_edx(0);
    machine.cpu.registers.set_eax(0x150E);
    assert!(machine.handle_int2f(), "AX=150Eh get handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "preference get clears CF");
    assert_eq!(machine.cpu.registers.edx() as u16, 0x0100);

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(1);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_edx(0x0201);
    machine.cpu.registers.set_eax(0x150E);
    assert!(machine.handle_int2f(), "AX=150Eh set handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "preference set clears CF");

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_edx(0);
    machine.cpu.registers.set_eax(0x150E);
    assert!(machine.handle_int2f(), "AX=150Eh get-after-set handled");
    assert_eq!(machine.cpu.registers.edx() as u16, 0x0201);

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_eax(0x150A);
    assert!(machine.handle_int2f(), "AX=150Ah handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "reserved call clears CF");

    machine.write_guest_block(0x5100, b"\\README.TXT\0");
    prime_dos_int_frame(&mut machine);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0));
    machine.cpu.registers.set_ebx(0x5100);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_esi(0x3000);
    machine.cpu.registers.set_edi(0x0100);
    machine.cpu.registers.set_eax(0x150F);
    assert!(machine.handle_int2f(), "AX=150Fh direct handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "directory entry clears CF");
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0001);
    let direct = machine.read_guest_block((0x3000u32 << 4) + 0x0100, file.len());
    assert_eq!(direct[0], file.len() as u8);
    assert_eq!(&direct[2..6], &19u32.to_le_bytes());
    assert_eq!(&direct[10..14], &1234u32.to_le_bytes());
    assert_eq!(&direct[33..45], b"README.TXT;1");

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0x5100);
    machine
        .cpu
        .registers
        .set_ecx((1u32 << 8) | u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_esi(0x3100);
    machine.cpu.registers.set_edi(0);
    machine.cpu.registers.set_eax(0x150F);
    assert!(machine.handle_int2f(), "AX=150Fh canonical handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "canonical entry clears CF");
    let canonical = machine.read_guest_block(0x3100u32 << 4, 0x41);
    assert_eq!(&canonical[1..5], &19u32.to_le_bytes());
    assert_eq!(&canonical[5..7], &1u16.to_le_bytes());
    assert_eq!(&canonical[7..11], &1234u32.to_le_bytes());
    assert_eq!(canonical[0x17], 10);
    assert_eq!(&canonical[0x18..0x22], b"README.TXT");
    assert_eq!(&canonical[0x3e..0x40], &1u16.to_le_bytes());

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_eax(0x1509);
    assert!(machine.handle_int2f(), "AX=1509h handled");
    assert_ne!(dos_int_flags(&machine) & 1, 0, "write sets CF");
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0001);

    prime_dos_int_frame(&mut machine);
    machine
        .cpu
        .registers
        .set_ecx(u32::from(CD_DRIVE_NUMBER) + 1);
    machine.cpu.registers.set_eax(0x1508);
    assert!(machine.handle_int2f(), "AX=1508h invalid drive handled");
    assert_ne!(dos_int_flags(&machine) & 1, 0, "invalid drive sets CF");
    assert_eq!(machine.cpu.registers.eax() as u16, 0x000f);

    let mut empty = test_machine();
    prime_dos_int_frame(&mut empty);
    empty
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0));
    empty.cpu.registers.set_ebx(0x8000);
    empty.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    empty.cpu.registers.set_edx(0);
    empty.cpu.registers.set_eax(0x1505);
    assert!(empty.handle_int2f(), "AX=1505h no-media handled");
    assert_ne!(dos_int_flags(&empty) & 1, 0, "no media sets CF");
    assert_eq!(empty.cpu.registers.eax() as u16, 0x0015);
}

#[test]
fn icdex_send_request_read_long_loads_a_sector() {
    let mut machine = test_machine();
    // A small data ISO with a marker per sector.
    let mut bytes = vec![0u8; 4 * cdimage::DATA_SECTOR];
    bytes[2 * cdimage::DATA_SECTOR] = 0x99; // LBA 2 marker
    machine.mount_cd(CdImage::from_iso(bytes).unwrap());

    // Build a READ LONG (0x80) device request header at linear 0x2000, with a
    // transfer buffer at 0x4000. ES:BX -> header via ES base 0, BX = 0x2000.
    let header = 0x2000u32;
    let xfer = 0x4000u32;
    machine.write_physical_u8(header, 27); // declared length (gated >= 13)
    machine.write_physical_u8(header + 2, 0x80); // command READ LONG
    machine.write_physical_u8(header + 0x0D, 0x00); // HSG addressing
    // transfer address dword at 0x0E
    for (i, b) in xfer.to_le_bytes().iter().enumerate() {
        machine.write_physical_u8(header + 0x0E + i as u32, *b);
    }
    // sector count (1) at 0x12
    machine.write_physical_u8(header + 0x12, 1);
    machine.write_physical_u8(header + 0x13, 0);
    // starting sector (LBA 2) dword at 0x14
    for (i, b) in 2u32.to_le_bytes().iter().enumerate() {
        machine.write_physical_u8(header + 0x14 + i as u32, *b);
    }

    machine.cpu.registers.set_eax(0x1510);
    machine.cpu.registers.set_ebx(header); // ES base 0, BX = header
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    assert!(machine.handle_int2f());

    // The sector landed at the transfer address.
    assert_eq!(machine.read_physical_u8(xfer), 0x99);
    // Status word (offset 3) has the done bit set, no error.
    let status = machine.read_guest_word(header + 3);
    assert_eq!(status & 0x8000, 0, "no error bit");
    assert_ne!(status & 0x0100, 0, "done bit set");
}

/// A data ISO whose LBA 2 is marked at byte 0 and at byte 0x200, so a 2048-byte
/// transfer can be checked on both sides of a page boundary 0x200 bytes in.
fn marked_data_cd() -> CdImage {
    let mut bytes = vec![0u8; 8 * cdimage::DATA_SECTOR];
    bytes[2 * cdimage::DATA_SECTOR] = 0x99;
    bytes[2 * cdimage::DATA_SECTOR + 0x200] = 0x9a;
    bytes[3 * cdimage::DATA_SECTOR] = 0x9b;
    CdImage::from_iso(bytes).unwrap()
}

/// Plant a READ LONG (0x80) device-driver request at guest linear `header`,
/// through the caller's own mapping. The transfer field carries the MSCDEX
/// far-pointer form (offset word, then segment word); `xfer` arrives linear
/// and is encoded here.
fn plant_read_long_request(machine: &mut Machine, header: u32, xfer: u32, lba: u32, count: u16) {
    let mut request = [0u8; 0x18];
    request[0] = 27; // declared request length (the driver gates on >= 13)
    request[2] = 0x80; // READ LONG
    request[0x0D] = 0x00; // HSG addressing
    let xfer_far = ((xfer >> 4) << 16) | (xfer & 0xF);
    request[0x0E..0x12].copy_from_slice(&xfer_far.to_le_bytes());
    request[0x12..0x14].copy_from_slice(&count.to_le_bytes());
    request[0x14..0x18].copy_from_slice(&lba.to_le_bytes());
    machine.write_guest_linear_block(header, &request);
}

/// AX=1510h sends a CD-ROM device-driver request whose header is at ES:BX and
/// whose transfer address is a field INSIDE that header. Both were read and
/// written as physical addresses: the header fields, the sector data, and the
/// status word the caller polls.
#[test]
fn icdex_device_request_uses_the_non_identity_mapped_header_and_buffer() {
    let mut machine = test_machine();
    machine.mount_cd(marked_data_cd());
    super::margo::install_umb_paging(&mut machine);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0xc8c6));

    // Header at guest linear C8C60h, transfer buffer at C8E00h: the 2048-byte
    // sector runs 0x200 bytes into the first page and the rest into the second.
    plant_read_long_request(&mut machine, 0x000c_8c60, 0x000c_8e00, 2, 1);

    machine.cpu.registers.set_eax(0x1510);
    machine.cpu.registers.set_ebx(0);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    assert!(machine.handle_int2f());

    assert_eq!(
        machine.read_physical_u8(super::margo::UMB_FRAME_LOW + 0x0e00),
        0x99,
        "the head of the sector must follow the first page's mapping"
    );
    assert_eq!(
        machine.read_physical_u8(super::margo::UMB_FRAME_HIGH),
        0x9a,
        "the tail of the sector must follow the second page's mapping"
    );
    let status = u16::from_le_bytes([
        machine.read_physical_u8(super::margo::UMB_BUFFER_PHYSICAL + 3),
        machine.read_physical_u8(super::margo::UMB_BUFFER_PHYSICAL + 4),
    ]);
    assert_eq!(status & 0x8000, 0, "no error bit");
    assert_ne!(status & 0x0100, 0, "done bit, written through the mapping");
}

/// The request header's count field is what a driver returns the transferred
/// count in, so it owes the caller the same truth the EDD packet does.
#[test]
fn icdex_device_request_counts_only_the_sectors_that_reached_the_caller() {
    let mut machine = test_machine();
    machine.mount_cd(marked_data_cd());
    super::margo::install_umb_paging(&mut machine);
    super::margo::unmap_guest_page(&mut machine, 0xc9);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0xc8c6));

    // Two sectors from C8800h: the first ends exactly on the C9000h boundary
    // and the second falls entirely in the page this fixture unmaps.
    plant_read_long_request(&mut machine, 0x000c_8c60, 0x000c_8800, 2, 2);

    machine.cpu.registers.set_eax(0x1510);
    machine.cpu.registers.set_ebx(0);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    assert!(machine.handle_int2f());

    assert_eq!(
        machine.read_physical_u8(super::margo::UMB_FRAME_LOW + 0x800),
        0x99,
        "the one reachable sector must still be delivered"
    );
    assert_eq!(
        machine.read_physical_u8(super::margo::UMB_BUFFER_PHYSICAL + 0x12),
        1,
        "the header must report the sectors that LANDED"
    );
    let status = u16::from_le_bytes([
        machine.read_physical_u8(super::margo::UMB_BUFFER_PHYSICAL + 3),
        machine.read_physical_u8(super::margo::UMB_BUFFER_PHYSICAL + 4),
    ]);
    assert_ne!(status & 0x8000, 0, "a short transfer is an error");
}

#[test]
fn atapi_read10_on_a_folder_mount_returns_a_known_files_bytes() {
    // Mount a small host folder as a CD (the ISO9660 metadata is
    // synthesized in memory, file bytes stay on the host), then drive a
    // real READ(10) through the ATAPI PACKET port handshake exactly like
    // ide.rs's packet_read10 helper, proving the lazy folder backing works
    // through the same device path a real driver uses.
    let dir = tempfile::tempdir().unwrap();
    let content = b"hello from a folder-mounted CD-ROM, izarra style";
    std::fs::write(dir.path().join("HELLO.TXT"), content).unwrap();

    let built = crate::iso9660::build(dir.path()).unwrap();
    let image = CdImage::from_folder(built).unwrap();
    let mut machine = test_machine();
    machine.mount_cd(image);

    // Walk the root directory (through read_data_sector, the same path
    // the ATAPI device serves) to find HELLO.TXT;1's extent LBA.
    let pvd = machine
        .ide
        .device()
        .image()
        .unwrap()
        .read_data_sector(16)
        .unwrap();
    let root_lba = u32::from_le_bytes(pvd[156 + 2..156 + 6].try_into().unwrap());
    let root_sector = machine
        .ide
        .device()
        .image()
        .unwrap()
        .read_data_sector(root_lba)
        .unwrap();
    let mut offset = 0usize;
    let mut file_lba = None;
    while offset < root_sector.len() {
        let len = usize::from(root_sector[offset]);
        if len == 0 {
            break;
        }
        let name_len = usize::from(root_sector[offset + 32]);
        let name = &root_sector[offset + 33..offset + 33 + name_len];
        if name == b"HELLO.TXT;1" {
            file_lba = Some(u32::from_le_bytes(
                root_sector[offset + 2..offset + 6].try_into().unwrap(),
            ));
        }
        offset += len;
    }
    let file_lba = file_lba.expect("HELLO.TXT;1 must be in the root directory");

    // Clear the post-mount unit attention with a TEST UNIT READY packet.
    timed_packet(&mut machine, [0u8; 12]);

    // READ(10) one sector at the file's LBA over the real ATAPI packet
    // ports, then drain the data-in phase.
    let mut cdb = [0u8; 12];
    cdb[0] = 0x28; // READ(10)
    cdb[2..6].copy_from_slice(&file_lba.to_be_bytes());
    cdb[8] = 1; // one sector
    timed_packet(&mut machine, cdb);
    let sector = with_bus(&mut machine, |bus| {
        let mut out = Vec::with_capacity(cdimage::DATA_SECTOR);
        for _ in 0..cdimage::DATA_SECTOR {
            out.push(bus.read_io(0x170, BusWidth::Byte, 0, false).unwrap() as u8);
        }
        out
    });

    assert_eq!(&sector[..content.len()], &content[..]);
    assert!(
        sector[content.len()..].iter().all(|&b| b == 0),
        "the rest of the sector is zero-padded"
    );
}

#[test]
fn icdex_send_request_play_audio_starts_playback() {
    let mut machine = test_machine();
    machine.mount_cd(audio_cd(40));
    let header = 0x2000u32;
    machine.write_physical_u8(header, 22); // declared length (gated >= 13)
    machine.write_physical_u8(header + 2, 0x84); // PLAY AUDIO
    machine.write_physical_u8(header + 0x0D, 0x00); // HSG
    // start sector (LBA 1, the audio track) dword at 0x0E
    for (i, b) in 1u32.to_le_bytes().iter().enumerate() {
        machine.write_physical_u8(header + 0x0E + i as u32, *b);
    }
    // play count (8 frames) dword at 0x12
    for (i, b) in 8u32.to_le_bytes().iter().enumerate() {
        machine.write_physical_u8(header + 0x12 + i as u32, *b);
    }
    machine.cpu.registers.set_eax(0x1510);
    machine.cpu.registers.set_ebx(header);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    assert!(machine.handle_int2f());
    assert!(machine.ide.device().playback().playing);
}

#[test]
fn render_audio_outputs_at_the_dac_rate() {
    let mut machine = test_machine();
    let pcm = machine.render_audio(OPL_NATIVE_HZ as usize); // one second of OPL time
    assert!(
        (pcm.len() as i32 - DAC_HZ as i32).abs() < 50,
        "expected ~{DAC_HZ} frames, got {}",
        pcm.len()
    );
}

#[test]
fn render_audio_passes_through_when_the_dsp_is_idle() {
    // No DMA playback armed: the DSP produces nothing, so render_audio must
    // return the OPL-only output at the DAC rate (the existing contract).
    let mut machine = test_machine();
    let pcm = machine.render_audio(OPL_NATIVE_HZ as usize);
    assert!(
        (pcm.len() as i32 - DAC_HZ as i32).abs() < 50,
        "idle DSP must not truncate the OPL stream, got {} frames",
        pcm.len()
    );
}

#[test]
fn render_audio_mixes_the_dsp_dc_level_with_the_opl() {
    let mut machine = test_machine();
    // A constant 256-byte DMA buffer; 0x40 maps to sample_u8(0x40) = -16384.
    // Voice (0x32/0x33) and master (0x30/0x31) both power on at level 31
    // (0 dB), so the CT1745 passes the DC level unattenuated and the only
    // scaling left is the summing node's MIX_HEADROOM (-6 dB).
    const BYTE: u8 = 0x40;
    let expected: i32 = -8192;
    for i in 0..256u32 {
        machine.write_physical_u8(0x1_0000 + i, BYTE);
    }
    with_bus(&mut machine, |bus| {
        // DMA ch1: page 0x01, address 0, count 255, auto-init read.
        bus.write_io(0x0B, BusWidth::Byte, 0x59, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0xFF, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        // DSP: 11025 Hz, block 256, auto-init 8-bit output.
        for &b in &[0x41u8, 0x2B, 0x11, 0x48, 0xFF, 0x00, 0x1C] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    // The OPL is silent (no voices keyed), so the steady output is the DSP DC
    // level after the resampler warmup. Playback is clock-driven now, so
    // advance CPU time to let the per-clock producer fill the ring, then
    // render plenty of OPL-native time for the host drainer + resampler.
    machine.advance_devices_clocks(2_500_000);
    let out = machine.render_audio(4_000);
    assert!(!out.is_empty());
    let mid = &out[out.len() / 3..out.len() * 2 / 3];
    let (min_l, max_l) = mid
        .iter()
        .map(|f| f.0)
        .fold((i16::MAX, i16::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let center = (i32::from(min_l) + i32::from(max_l)) / 2;
    assert!(
        (center - expected).abs() < 400,
        "DSP DC center {center}, expected ~{expected}"
    );
    // Mono is duplicated to both channels.
    assert!(mid.iter().all(|f| f.0 == f.1), "DSP mono duplicated L/R");
}

#[test]
fn sb_mixer_voice_and_master_volume_attenuate_output() {
    let mut machine = test_machine();
    // Constant 256-byte DC buffer: sample_u8(0x40) = -16384, auto-init.
    for i in 0..256u32 {
        machine.write_physical_u8(0x1_0000 + i, 0x40);
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0B, BusWidth::Byte, 0x59, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0xFF, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        for &b in &[0x41u8, 0x2B, 0x11, 0x48, 0xFF, 0x00, 0x1C] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });

    fn set_reg(machine: &mut Machine, index: u8, value: u8) {
        with_bus(machine, |bus| {
            bus.write_io(0x224, BusWidth::Byte, u32::from(index), false)
                .unwrap();
            bus.write_io(0x225, BusWidth::Byte, u32::from(value), false)
                .unwrap();
        });
    }
    // Refill the clock-driven ring, then render a window of mixed output.
    fn render(machine: &mut Machine) -> Vec<(i16, i16)> {
        machine.advance_devices_clocks(2_500_000);
        machine.render_audio(4_000)
    }
    fn mid_quiet(out: &[(i16, i16)]) -> bool {
        let mid = &out[out.len() / 3..out.len() * 2 / 3];
        mid.iter().all(|&(l, r)| l.abs() <= 50 && r.abs() <= 50)
    }

    // Voice mute (0x32/0x33 = 0) silences the DSP path regardless of master.
    set_reg(&mut machine, 0x32, 0x00);
    set_reg(&mut machine, 0x33, 0x00);
    assert!(
        mid_quiet(&render(&mut machine)),
        "voice mute silences the DSP output"
    );

    // Master mute (0x30/0x31 = 0) silences the whole mix even at full voice.
    set_reg(&mut machine, 0x32, 0x1F << 3);
    set_reg(&mut machine, 0x33, 0x1F << 3);
    set_reg(&mut machine, 0x30, 0x00);
    set_reg(&mut machine, 0x31, 0x00);
    assert!(
        mid_quiet(&render(&mut machine)),
        "master mute silences the summed output"
    );

    // Level 24 (the Guide's -14 dB step) on both legs returns the attenuated DC
    // level. Written left-aligned: the 5-bit field is D7-D3.
    for (idx, val) in [
        (0x30u8, 24u8 << 3),
        (0x31, 24 << 3),
        (0x32, 24 << 3),
        (0x33, 24 << 3),
    ] {
        set_reg(&mut machine, idx, val);
    }
    let restored = render(&mut machine);
    let mid = &restored[restored.len() / 3..restored.len() * 2 / 3];
    let (min_l, max_l) = mid
        .iter()
        .map(|f| f.0)
        .fold((i16::MAX, i16::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let center = (i32::from(min_l) + i32::from(max_l)) / 2;
    // Voice -14 dB, master -14 dB, then the summing node's MIX_HEADROOM.
    let expected =
        (-16384.0f32 * 10f32.powf(-14.0 / 20.0) * 10f32.powf(-14.0 / 20.0) * MIX_HEADROOM) as i32;
    assert!(
        (center - expected).abs() < 200,
        "restored DC center {center}, expected ~{expected}"
    );
}

#[test]
fn opl_timers_advance_with_machine_clocks() {
    // AdLib detection: arm timer 1 to overflow in one 80us step, let machine
    // time pass, and confirm the status port reports the overflow + IRQ.
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        let mut write = |reg: u8, value: u8| {
            bus.write_io(0x0388, BusWidth::Byte, u32::from(reg), false)
                .unwrap();
            bus.write_io(0x0389, BusWidth::Byte, u32::from(value), false)
                .unwrap();
        };
        write(0x04, 0x60); // mask both timers
        write(0x04, 0x80); // reset the overflow flags
        write(0x02, 0xff); // timer 1 preset: overflow in one step
        write(0x04, 0x21); // start timer 1 (unmasked), mask timer 2
    });

    // 100 us of CPU time (clock_hz/10000 clocks) covers the 80 us timer step.
    machine.advance_devices(
        machine
            .active_mode()
            .clock_rate()
            .clocks_for_fraction_floor(1, 10_000),
    );

    let status = with_bus(&mut machine, |bus| {
        bus.read_io(0x0388, BusWidth::Byte, 0, false).unwrap()
    });
    assert_eq!(
        status & 0xe0,
        0xc0,
        "timer 1 overflow raises IRQ + timer-1 flag"
    );
}

/// Arm 8-bit auto-init DMA playback of a constant DC buffer at 22050 Hz on the
/// 0x40 time constant (the rate most DOS digital-sound engines program).
fn arm_dc_dma_playback(machine: &mut Machine, byte: u8) {
    for i in 0..256u32 {
        machine.write_physical_u8(0x1_0000 + i, byte);
    }
    with_bus(machine, |bus| {
        bus.write_io(0x0B, BusWidth::Byte, 0x59, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0xFF, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        // 22050 Hz via 0x41 (set sample rate), block 256, auto-init 8-bit out.
        for &b in &[0x41u8, 0x56, 0x22, 0x48, 0xFF, 0x00, 0x1C] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
}

/// Pump audio the way the GUI emulation loop does: advance guest time by a
/// slice, then render the OPL-native samples that slice of WALL time is worth.
/// `speed_ratio` is guest time per wall second (1.0 = the guest keeps up).
/// The wall slice length wobbles per pass the way the real loop's `dt` does, so
/// the guest-clocked and wall-clocked windows disagree in BOTH directions and
/// not just by the average shortfall.
fn pump_slices(
    machine: &mut Machine,
    slices: usize,
    wall_slice_secs: f64,
    speed_ratio: f64,
) -> Vec<(i16, i16)> {
    let mut out = Vec::new();
    for slice in 0..slices {
        let wobble = 1.0 + 0.4 * f64::from(slice as u32 % 5) - 0.8;
        let wall = wall_slice_secs * wobble;
        let guest_ticks = (wall * speed_ratio * izarravm_core::MASTER_CLOCK_HZ as f64) as u64;
        machine.advance_devices_ticks(guest_ticks);
        out.extend(machine.render_audio((wall * f64::from(OPL_NATIVE_HZ)) as usize));
    }
    out
}

/// Count samples that fall far from the median: with a DC source and a silent
/// OPL, every output frame should hold the same level. A notch toward zero is a
/// full-scale impulse in the guest's ear.
fn dropout_count(out: &[(i16, i16)], skip: usize) -> (usize, i16) {
    let mut sorted: Vec<i16> = out[skip..].iter().map(|f| f.0).collect();
    sorted.sort_unstable();
    let median = sorted[sorted.len() / 2];
    let tolerance = (i32::from(median).abs() / 4).max(64);
    let count = out[skip..]
        .iter()
        .filter(|f| (i32::from(f.0) - i32::from(median)).abs() > tolerance)
        .count();
    (count, median)
}

#[test]
fn dsp_dc_playback_has_no_per_pump_dropouts_at_full_speed() {
    let mut machine = test_machine();
    arm_dc_dma_playback(&mut machine, 0x40);
    // 1 ms pumps, the GUI's cadence, with the guest exactly at real time.
    let out = pump_slices(&mut machine, 400, 0.001, 1.0);
    let (dropouts, median) = dropout_count(&out, 2_000);
    assert_eq!(
        dropouts,
        0,
        "DC playback dropped to silence {dropouts} times (median {median}, {} frames)",
        out.len()
    );
}

#[test]
fn dsp_dc_playback_has_no_per_pump_dropouts_slightly_behind_real_time() {
    let mut machine = test_machine();
    arm_dc_dma_playback(&mut machine, 0x40);
    // A 486 persona a few percent short of real time: the guest-clocked DSP
    // stream is shorter than the wall-clocked OPL window every pump.
    let out = pump_slices(&mut machine, 400, 0.001, 0.96);
    let (dropouts, median) = dropout_count(&out, 2_000);
    assert_eq!(
        dropouts,
        0,
        "DC playback dropped to silence {dropouts} times (median {median}, {} frames)",
        out.len()
    );
}

/// The MSCDEX blocks IZCDEX returns at ES:BX have the same address defect the
/// VBE information blocks had: the segment base plus BX went onto the bus as a
/// physical address. `run.rs` dispatches INT 2Fh for any caller outside ring-0
/// protected mode, so a program running in V86 under TOKAEMM reaches this with
/// a buffer its page tables put outside the identity map -- and a DJGPP program
/// calls MSCDEX through DPMI with a transfer buffer DOS took from upper memory.
///
/// The fixture is the VBE one: guest pages C8h and C9h mapped to two
/// non-adjacent frames, ES = C8C6h. A volume descriptor is 2048 bytes, so it
/// crosses the page boundary and the per-page split has to hold it together.
#[test]
fn icdex_blocks_use_the_non_identity_mapped_caller_buffer() {
    let mut machine = test_machine();
    let mut bytes = vec![0u8; 20 * cdimage::DATA_SECTOR];
    let pvd = 16 * cdimage::DATA_SECTOR;
    bytes[pvd] = 0x01;
    bytes[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
    bytes[pvd + 0x400] = 0x7e; // a marker past the caller's first page
    machine.mount_cd(CdImage::from_iso(bytes).unwrap());
    super::margo::install_umb_paging(&mut machine);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0xc8c6));

    // AX=1502h writes 38 zero bytes. Fill the mapped frame with a marker first,
    // so "the block arrived" is distinguishable from "nothing happened".
    machine.write_guest_block(super::margo::UMB_BUFFER_PHYSICAL, &[0xee; 38]);
    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_eax(0x1502);
    assert!(machine.handle_int2f(), "AX=1502h handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "metadata clears CF");
    assert_eq!(
        machine.read_guest_block(super::margo::UMB_BUFFER_PHYSICAL, 38),
        vec![0; 38],
        "the metadata block must reach the frame the caller's page is mapped to"
    );

    // AX=1505h reads a 2048-byte volume descriptor at ES:BX.
    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_edx(0);
    machine.cpu.registers.set_eax(0x1505);
    assert!(machine.handle_int2f(), "AX=1505h handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "VTOC clears CF");
    assert_eq!(
        &machine.read_guest_block(super::margo::UMB_BUFFER_PHYSICAL + 1, 5),
        b"CD001",
        "the descriptor head must follow the first page's mapping"
    );
    // Guest linear C8C60h + 400h is C9060h, in the second page.
    assert_eq!(
        machine.read_physical_u8(super::margo::UMB_FRAME_HIGH + 0x60),
        0x7e,
        "the descriptor tail must follow the second page's mapping"
    );
}

/// An ISO whose root directory holds one file, and the file's own directory
/// record, for the AX=150Fh fixtures.
fn icdex_directory_iso() -> (CdImage, Vec<u8>) {
    let mut bytes = vec![0u8; 20 * cdimage::DATA_SECTOR];
    let pvd = 16 * cdimage::DATA_SECTOR;
    bytes[pvd] = 0x01;
    bytes[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
    let root = iso_dir_record(18, cdimage::DATA_SECTOR as u32, 0x02, &[0]);
    bytes[pvd + 156..pvd + 156 + root.len()].copy_from_slice(&root);
    let file = iso_dir_record(19, 1234, 0x00, b"README.TXT;1");
    let root_sector = 18 * cdimage::DATA_SECTOR;
    bytes[root_sector..root_sector + root.len()].copy_from_slice(&root);
    bytes[root_sector + root.len()..root_sector + root.len() + file.len()].copy_from_slice(&file);
    (CdImage::from_iso(bytes).unwrap(), file)
}

/// AX=1501h, the drive device list at ES:BX. It was one of three ES:BX blocks
/// 8edf30d0 left on the physical path beside the ones it converted.
#[test]
fn icdex_drive_device_list_uses_the_non_identity_mapped_caller_buffer() {
    let mut machine = test_machine();
    machine.mount_cd(audio_cd(4));
    super::margo::install_umb_paging(&mut machine);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0xc8c6));
    machine.write_guest_block(super::margo::UMB_BUFFER_PHYSICAL, &[0xee; 5]);

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0);
    machine.cpu.registers.set_eax(0x1501);
    assert!(machine.handle_int2f(), "AX=1501h handled");

    // Subunit 0, then the IzarraCD ROM device header far pointer.
    assert_eq!(
        machine.read_guest_block(super::margo::UMB_BUFFER_PHYSICAL, 5),
        vec![0x00, 0x20, 0x04, 0x00, 0xFF],
        "the device-list entry must reach the frame the caller's page is mapped to"
    );
}

/// AX=150Dh, the CD drive-letter list at ES:BX.
#[test]
fn icdex_drive_letters_use_the_non_identity_mapped_caller_buffer() {
    let mut machine = test_machine();
    machine.mount_cd(audio_cd(4));
    super::margo::install_umb_paging(&mut machine);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0xc8c6));
    machine.write_guest_block(super::margo::UMB_BUFFER_PHYSICAL, &[0xee]);

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0);
    machine.cpu.registers.set_eax(0x150D);
    assert!(machine.handle_int2f(), "AX=150Dh handled");

    assert_eq!(
        machine.read_physical_u8(super::margo::UMB_BUFFER_PHYSICAL),
        CD_DRIVE_NUMBER,
        "the drive letter must reach the frame the caller's page is mapped to"
    );
}

/// AX=150Fh carries TWO caller pointers with two different conventions: the
/// ASCIZ path comes IN at ES:BX, and the directory record goes OUT at SI:DI --
/// a real-mode segment:offset pair in registers that are not segment registers,
/// so it is built by shifting SI, not from a descriptor base. Each is a
/// separate address and each needs its own proof; this one puts the path in the
/// mapped page and leaves the destination on an identity address, so only the
/// input side can be responsible for the result.
#[test]
fn icdex_directory_entry_reads_a_non_identity_mapped_path() {
    let (image, file) = icdex_directory_iso();
    let mut machine = test_machine();
    machine.mount_cd(image);
    super::margo::install_umb_paging(&mut machine);
    machine.write_guest_block(super::margo::UMB_BUFFER_PHYSICAL, b"\\README.TXT\0");
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0xc8c6));

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_esi(0x3000);
    machine.cpu.registers.set_edi(0x0100);
    machine.cpu.registers.set_eax(0x150F);
    assert!(machine.handle_int2f(), "AX=150Fh handled");

    assert_eq!(
        dos_int_flags(&machine) & 1,
        0,
        "the path must be read through the caller's mapping, or the lookup \
         fails on bytes that are not the caller's"
    );
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0001);
    assert_eq!(
        machine.read_guest_block((0x3000u32 << 4) + 0x0100, file.len()),
        file,
        "the record found must be the file the caller named"
    );
}

/// The other half of AX=150Fh: the SI:DI destination. The path stays on an
/// identity address and the record goes to guest linear C8FF0h, which straddles
/// the two mapped frames, so only the output side can be responsible.
#[test]
fn icdex_directory_entry_writes_a_non_identity_mapped_destination() {
    let (image, file) = icdex_directory_iso();
    let mut machine = test_machine();
    machine.mount_cd(image);
    super::margo::install_umb_paging(&mut machine);
    machine.write_guest_block(0x5100, b"\\README.TXT\0");
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0));

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0x5100);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_esi(0xc8ff); // C8FF:0000 = guest linear C8FF0h
    machine.cpu.registers.set_edi(0);
    machine.cpu.registers.set_eax(0x150F);
    assert!(machine.handle_int2f(), "AX=150Fh direct handled");

    assert_eq!(dos_int_flags(&machine) & 1, 0, "directory entry clears CF");
    assert_eq!(
        machine.read_guest_block(super::margo::UMB_FRAME_LOW + 0xff0, 16),
        file[..16],
        "the head of the record must follow the first page's mapping"
    );
    assert_eq!(
        machine.read_guest_block(super::margo::UMB_FRAME_HIGH, file.len() - 16),
        file[16..],
        "the tail of the record must follow the second page's mapping"
    );

    // CH bit 0 selects MSCDEX's canonical structure, which is built and
    // deposited by a different function against the same pointer.
    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0x5100);
    machine
        .cpu
        .registers
        .set_ecx((1u32 << 8) | u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_esi(0xc8ff);
    machine.cpu.registers.set_edi(0);
    machine.cpu.registers.set_eax(0x150F);
    assert!(machine.handle_int2f(), "AX=150Fh canonical handled");

    assert_eq!(
        machine.read_guest_block(super::margo::UMB_FRAME_LOW + 0xff1, 4),
        19u32.to_le_bytes(),
        "the canonical record's LBA must follow the first page's mapping"
    );
    // Guest linear C8FF0h + 18h is C9008h, in the second page.
    assert_eq!(
        machine.read_guest_block(super::margo::UMB_FRAME_HIGH + 8, 10),
        b"README.TXT",
        "the canonical record's name must follow the second page's mapping"
    );
}

// ---------------------------------------------------------------------------
// IzarraCD host redirector (cdredir.rs): the INT 2Fh AH=11h surface served
// from the host ISO index. Each test lays the kernel structures out by hand
// at REDIR_DS (the DOS data segment the redirector is armed with) and calls
// handle_int2f directly, the way the icdex_* tests above do for AH=15h.

const REDIR_DS: u16 = 0x0800;
const REDIR_SFT_SEG: u16 = 0x8800;
const REDIR_DTA_SEG: u16 = 0x8C00;

fn redir_lin(offset: u32) -> u32 {
    (u32::from(REDIR_DS) << 4) + offset
}

fn redir_sft_lin() -> u32 {
    u32::from(REDIR_SFT_SEG) << 4
}

fn redir_dta_lin() -> u32 {
    u32::from(REDIR_DTA_SEG) << 4
}

/// A folder disc plus an armed machine. The TempDir must stay alive: file
/// extents read lazily from the host folder.
fn redirector_machine() -> (tempfile::TempDir, Machine) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("readme.txt"), b"root file").unwrap();
    let game = dir.path().join("game");
    std::fs::create_dir(&game).unwrap();
    std::fs::write(
        game.join("data.bin"),
        (0..5000u32).map(|i| i as u8).collect::<Vec<u8>>(),
    )
    .unwrap();
    let levels = game.join("levels");
    std::fs::create_dir(&levels).unwrap();
    std::fs::write(levels.join("e1m1.map"), b"level bytes").unwrap();
    let built = crate::iso9660::build(dir.path()).unwrap();

    let mut machine = test_machine();
    machine.mount_cd(CdImage::from_folder(built).unwrap());
    machine.arm_cd_redirector(REDIR_DS);
    // The SDA DTA field points at the transfer area unless a test moves it.
    machine.write_guest_linear_block(
        redir_lin(0x32C),
        &[0, 0, REDIR_DTA_SEG as u8, (REDIR_DTA_SEG >> 8) as u8],
    );
    (dir, machine)
}

fn redir_set_path(machine: &mut Machine, path: &str) {
    let mut bytes = path.as_bytes().to_vec();
    bytes.push(0);
    machine.write_guest_linear_block(redir_lin(0x3BE), &bytes);
}

/// Point ES:DI at the scratch SFT and zero it.
fn redir_prime_sft(machine: &mut Machine) {
    machine.write_guest_linear_block(redir_sft_lin(), &[0u8; 0x40]);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(REDIR_SFT_SEG));
    machine.cpu.registers.set_edi(0);
}

/// The open-mode word the kernel pushes before INT 2Fh, above the frame that
/// prime_dos_int_frame builds: SS:SP+6.
fn redir_set_open_mode(machine: &mut Machine, mode: u16) {
    machine
        .memory
        .write_u16(0x9000 * 16 + 0x0106, mode)
        .unwrap();
}

fn redir_call(machine: &mut Machine, ax: u16) {
    prime_dos_int_frame(machine);
    machine.cpu.registers.set_eax(u32::from(ax));
    assert!(machine.handle_int2f(), "AX={ax:04X}h handled");
}

#[test]
fn cd_redirector_open_read_close_roundtrip() {
    let (_disc, mut machine) = redirector_machine();
    redir_set_path(&mut machine, "D:\\GAME\\DATA.BIN");
    redir_prime_sft(&mut machine);
    redir_set_open_mode(&mut machine, 0x0000);
    redir_call(&mut machine, 0x1116);
    assert_eq!(dos_int_flags(&machine) & 1, 0, "open succeeds");

    let sft = redir_sft_lin();
    assert_eq!(
        machine.read_guest_linear_block(sft + 0x20, 11),
        b"DATA    BIN"
    );
    assert_eq!(machine.read_guest_linear_block(sft + 0x04, 1)[0], 0x01);
    let flags = machine.read_guest_word(sft + 0x05);
    assert_eq!(flags, 0x8040 | u16::from(CD_DRIVE_NUMBER));
    assert_eq!(machine.read_guest_dword(sft + 0x11), 5000, "size");
    assert_eq!(machine.read_guest_dword(sft + 0x15), 0, "position");
    let lba = machine.read_guest_dword(sft + 0x19);
    assert_ne!(lba, 0, "extent LBA recorded");

    // Read 100 bytes, then a read that crosses EOF.
    machine.cpu.registers.set_ecx(100);
    redir_call(&mut machine, 0x1108);
    assert_eq!(dos_int_flags(&machine) & 1, 0, "read succeeds");
    assert_eq!(machine.cpu.registers.ecx() as u16, 100);
    let data = machine.read_guest_linear_block(redir_dta_lin(), 100);
    assert_eq!(data[0], 0);
    assert_eq!(data[99], 99);
    assert_eq!(machine.read_guest_dword(sft + 0x15), 100);

    machine.write_guest_linear_block(sft + 0x15, &4990u32.to_le_bytes());
    machine.cpu.registers.set_ecx(100);
    redir_call(&mut machine, 0x1108);
    assert_eq!(machine.cpu.registers.ecx() as u16, 10, "clamped at EOF");
    assert_eq!(
        machine.read_guest_linear_block(redir_dta_lin(), 10),
        (4990..5000u32).map(|i| i as u8).collect::<Vec<u8>>()
    );
    assert_eq!(machine.cd_redirector_read_bytes(), 110);

    // Close drops the reference count and clamps at zero.
    machine.write_guest_linear_block(sft, &1u16.to_le_bytes());
    redir_call(&mut machine, 0x1106);
    assert_eq!(machine.read_guest_word(sft), 0);
    redir_call(&mut machine, 0x1106);
    assert_eq!(machine.read_guest_word(sft), 0, "clamped");
}

#[test]
fn cd_redirector_read_spans_sector_boundaries() {
    let (_disc, mut machine) = redirector_machine();
    redir_set_path(&mut machine, "D:\\GAME\\DATA.BIN");
    redir_prime_sft(&mut machine);
    redir_set_open_mode(&mut machine, 0x0002); // read-write passes, as through IZCDEX
    redir_call(&mut machine, 0x1116);
    let sft = redir_sft_lin();
    // Start 10 bytes before the first sector boundary and read 30.
    machine.write_guest_linear_block(sft + 0x15, &2038u32.to_le_bytes());
    machine.cpu.registers.set_ecx(30);
    redir_call(&mut machine, 0x1108);
    assert_eq!(machine.cpu.registers.ecx() as u16, 30);
    let data = machine.read_guest_linear_block(redir_dta_lin(), 30);
    let expect: Vec<u8> = (2038..2068u32).map(|i| i as u8).collect();
    assert_eq!(data, expect, "bytes on both sides of the boundary");
}

#[test]
fn cd_redirector_denies_writes_and_reports_lookup_errors() {
    let (_disc, mut machine) = redirector_machine();

    // Open for write.
    redir_set_path(&mut machine, "D:\\GAME\\DATA.BIN");
    redir_prime_sft(&mut machine);
    redir_set_open_mode(&mut machine, 0x0001);
    redir_call(&mut machine, 0x1116);
    assert_eq!(dos_int_flags(&machine) & 1, 1);
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0005);

    // Extended open with write mode (SDA+2E1h bit 0).
    redir_prime_sft(&mut machine);
    machine.write_guest_linear_block(redir_lin(0x601), &[0x01]);
    machine.write_guest_linear_block(redir_lin(0x5FD), &[0x00]);
    redir_call(&mut machine, 0x112E);
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0005);

    // Extended open that would truncate (action bit 1).
    machine.write_guest_linear_block(redir_lin(0x601), &[0x00]);
    machine.write_guest_linear_block(redir_lin(0x5FD), &[0x02]);
    redir_call(&mut machine, 0x112E);
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0005);

    // A read-only extended open succeeds.
    machine.write_guest_linear_block(redir_lin(0x5FD), &[0x01]);
    redir_prime_sft(&mut machine);
    redir_call(&mut machine, 0x112E);
    assert_eq!(dos_int_flags(&machine) & 1, 0, "extended open succeeds");

    // Missing file in an existing directory: 02h. Missing directory: 03h.
    redir_set_path(&mut machine, "D:\\GAME\\NOPE.BIN");
    redir_prime_sft(&mut machine);
    redir_set_open_mode(&mut machine, 0x0000);
    redir_call(&mut machine, 0x1116);
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0002);
    redir_set_path(&mut machine, "D:\\NODIR\\NOPE.BIN");
    redir_prime_sft(&mut machine);
    redir_call(&mut machine, 0x1116);
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0003);
}

#[test]
fn cd_redirector_getattr_chdir_getspace_and_seek() {
    let (_disc, mut machine) = redirector_machine();

    // GetAttr: AX = attributes, BX:DI = size, CX = time, DX = date.
    redir_set_path(&mut machine, "D:\\GAME\\DATA.BIN");
    redir_call(&mut machine, 0x110F);
    assert_eq!(dos_int_flags(&machine) & 1, 0);
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0001, "read-only file");
    let size = (u32::from(machine.cpu.registers.ebx() as u16) << 16)
        | u32::from(machine.cpu.registers.edi() as u16);
    assert_eq!(size, 5000);
    assert_ne!(machine.cpu.registers.edx() as u16, 0, "date stamp present");

    redir_set_path(&mut machine, "D:\\GAME");
    redir_call(&mut machine, 0x110F);
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0010, "directory");

    // ChDir validates directories only.
    redir_set_path(&mut machine, "D:\\GAME\\LEVELS");
    redir_call(&mut machine, 0x1105);
    assert_eq!(dos_int_flags(&machine) & 1, 0);
    redir_set_path(&mut machine, "D:\\GAME\\DATA.BIN");
    redir_call(&mut machine, 0x1105);
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0003);

    // GetSpace against the CDS: AL=1, AH=0, CX=2048, DX=0.
    let cds = 0x7800u32 << 4;
    machine.write_guest_linear_block(cds, b"D:\\\0");
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x7800));
    machine.cpu.registers.set_edi(0);
    redir_call(&mut machine, 0x110C);
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0001);
    assert_eq!(machine.cpu.registers.ecx() as u16, 2048);
    assert_eq!(machine.cpu.registers.edx() as u16, 0);
    assert_ne!(machine.cpu.registers.ebx() as u16, 0, "volume sectors");

    // Seek from end: DX:AX = size + signed CX:DX offset.
    redir_set_path(&mut machine, "D:\\GAME\\DATA.BIN");
    redir_prime_sft(&mut machine);
    redir_set_open_mode(&mut machine, 0x0000);
    redir_call(&mut machine, 0x1116);
    machine.cpu.registers.set_ecx(0xFFFF); // CX:DX = -1000
    machine.cpu.registers.set_edx((-1000i32 as u32) & 0xFFFF);
    redir_call(&mut machine, 0x1121);
    let position = (u32::from(machine.cpu.registers.edx() as u16) << 16)
        | u32::from(machine.cpu.registers.eax() as u16);
    assert_eq!(position, 4000);
    assert_eq!(
        machine.read_guest_dword(redir_sft_lin() + 0x15),
        0,
        "seek must not move the stored position"
    );
}

#[test]
fn cd_redirector_findfirst_findnext_walks_a_directory() {
    let (_disc, mut machine) = redirector_machine();
    // Search D:\GAME\*.* for files and directories.
    redir_set_path(&mut machine, "D:\\GAME\\*.*");
    machine.write_guest_linear_block(redir_lin(0x56D), &[0x10]);
    redir_call(&mut machine, 0x111B);
    assert_eq!(dos_int_flags(&machine) & 1, 0, "first match");

    let fdb = redir_dta_lin() + 0x15;
    let mut names = vec![machine.read_guest_linear_block(fdb, 11)];
    loop {
        redir_call(&mut machine, 0x111C);
        if dos_int_flags(&machine) & 1 != 0 {
            assert_eq!(machine.cpu.registers.eax() as u16, 0x0012);
            break;
        }
        names.push(machine.read_guest_linear_block(fdb, 11));
    }
    // A subdirectory lists `.` and `..` first, the way DOS and the guest
    // redirector did; then its children in disc order.
    assert_eq!(names.len(), 4, "{names:?}");
    assert_eq!(names[0], b".          ".to_vec());
    assert_eq!(names[1], b"..         ".to_vec());
    assert!(names.contains(&b"DATA    BIN".to_vec()));
    assert!(names.contains(&b"LEVELS     ".to_vec()));

    // The directory entry carries the subdirectory attribute in the FDB.
    redir_set_path(&mut machine, "D:\\GAME\\LEVELS");
    redir_call(&mut machine, 0x111B);
    assert_eq!(machine.read_guest_linear_block(fdb + 11, 1)[0], 0x10);

    // A search with attributes 08h exactly returns the volume label.
    redir_set_path(&mut machine, "D:\\*.*");
    machine.write_guest_linear_block(redir_lin(0x56D), &[0x08]);
    redir_call(&mut machine, 0x111B);
    assert_eq!(dos_int_flags(&machine) & 1, 0, "label search succeeds");
    assert_eq!(machine.read_guest_linear_block(fdb + 11, 1)[0], 0x08);
    redir_call(&mut machine, 0x111C);
    assert_eq!(
        machine.cpu.registers.eax() as u16,
        0x0012,
        "label ends search"
    );

    // A pattern with no match fails FindFirst with no-more-files.
    redir_set_path(&mut machine, "D:\\GAME\\*.XYZ");
    machine.write_guest_linear_block(redir_lin(0x56D), &[0x10]);
    redir_call(&mut machine, 0x111B);
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0012);
}

#[test]
fn cd_redirector_ignores_other_drives_and_disarmed_machines() {
    let (_disc, mut machine) = redirector_machine();
    // A path on C: falls back to the absent-redirector refusal.
    redir_set_path(&mut machine, "C:\\ANYTHING");
    redir_call(&mut machine, 0x110F);
    assert_eq!(dos_int_flags(&machine) & 1, 1);
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0001);

    // Disarmed, even a D: path is refused the old way.
    machine.disarm_cd_redirector();
    redir_set_path(&mut machine, "D:\\GAME");
    redir_call(&mut machine, 0x110F);
    assert_eq!(dos_int_flags(&machine) & 1, 1);
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0001);
}

// ---------------------------------------------------------------------------
// The IzarraCD ROM device header and the Lotura doorbell (port 0xE8).

#[test]
fn izarracd_rom_header_is_returned_by_1501_and_carries_the_stubs() {
    let mut machine = test_machine();
    machine.mount_cd(audio_cd(4));
    // AX=1501h writes subunit 0 plus the ROM header far pointer.
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x9000));
    machine.cpu.registers.set_ebx(0x0200);
    machine.cpu.registers.set_eax(0x1501);
    assert!(machine.handle_int2f());
    let entry = machine.read_guest_linear_block(0x9000 * 16 + 0x0200, 5);
    assert_eq!(entry[0], 0, "subunit");
    let off = u16::from_le_bytes([entry[1], entry[2]]);
    let seg = u16::from_le_bytes([entry[3], entry[4]]);
    assert_eq!((seg, off), (0xFF00, 0x0420));

    // The header bytes live in the ROM: name, strategy/interrupt offsets.
    let header = machine.read_guest_linear_block(0xFF420, 22);
    assert_eq!(&header[10..18], b"TOKACD01");
    let strategy = u16::from_le_bytes([header[6], header[7]]);
    let interrupt = u16::from_le_bytes([header[8], header[9]]);
    // Both entries hold real code ending in RETF (CBh).
    let strategy_code = machine.read_guest_linear_block(0xFF000 + u32::from(strategy), 17);
    assert_eq!(*strategy_code.last().unwrap(), 0xCB);
    let interrupt_code = machine.read_guest_linear_block(0xFF000 + u32::from(interrupt), 16);
    assert_eq!(*interrupt_code.last().unwrap(), 0xCB);
}

#[test]
fn izarracd_doorbell_executes_the_mailbox_request() {
    let mut machine = test_machine();
    machine.mount_cd(marked_data_cd());

    // The strategy stub would have stored the request pointer here: header at
    // 0200:0060 (linear 0x2060).
    machine.memory.write_u16(0x063C, 0x0060).unwrap();
    machine.memory.write_u16(0x063E, 0x0200).unwrap();
    plant_read_long_request(&mut machine, 0x2060, 0x4000, 2, 1);

    // Ring the doorbell the way the ROM interrupt stub does.
    with_bus(&mut machine, |bus| {
        bus.write_io(0x00E8, BusWidth::Byte, 1, false).unwrap();
        assert_eq!(
            bus.read_io(0x00E8, BusWidth::Byte, 0, false).unwrap(),
            1,
            "busy until serviced"
        );
    });
    assert!(machine.pending_cd_doorbell.is_some());
    machine.pending_cd_doorbell.take();
    machine.perform_cd_doorbell(1);

    with_bus(&mut machine, |bus| {
        assert_eq!(
            bus.read_io(0x00E8, BusWidth::Byte, 0, false).unwrap(),
            0,
            "idle after service"
        );
    });
    assert_eq!(machine.read_physical_u8(0x4000), 0x99, "sector delivered");
    let status = machine.read_guest_word(0x2060 + 3);
    assert_ne!(status & 0x0100, 0, "done bit");
    assert_eq!(status & 0x8000, 0, "no error");
}

/// A stray OUT to 0xE8 (wrong command, or no request stored) must stay inert:
/// the port was open bus before the doorbell existed, so garbage rings must
/// never decode low memory as a device request.
#[test]
fn izarracd_doorbell_refuses_stray_rings() {
    let mut machine = test_machine();
    machine.mount_cd(marked_data_cd());

    // Unknown command: status parks at 0xFF, nothing executes.
    machine.memory.write_u16(0x063C, 0x0060).unwrap();
    machine.memory.write_u16(0x063E, 0x0200).unwrap();
    plant_read_long_request(&mut machine, 0x2060, 0x4000, 2, 1);
    machine.pending_cd_doorbell = Some(0x77);
    machine.pending_cd_doorbell.take();
    machine.perform_cd_doorbell(0x77);
    assert_eq!(machine.read_physical_u8(0x4000), 0, "no transfer happened");
    assert_eq!(
        machine.read_guest_word(0x2060 + 3),
        0,
        "status word untouched"
    );

    // Command 1 with a null mailbox: refused, and the INT 0 vector's segment
    // word (linear 0x0003, where a request-at-0 would land its status) stays.
    machine.memory.write_u16(0x063C, 0).unwrap();
    machine.memory.write_u16(0x063E, 0).unwrap();
    let int0_seg = machine.read_guest_word(0x0002);
    machine.perform_cd_doorbell(1);
    assert_eq!(machine.read_guest_word(0x0002), int0_seg);
}

// ---------------------------------------------------------------------------
// The B3 FAT-position hypercall (Lotura doorbell commands 3/4). Design:
// dev_docs/tier-b-b3-a2-design-2026-08-28.md sections 2.2-2.4.

use crate::dos::{HDD_MAP_RESULT, HDD_MAP_START, HDD_MAP_STEPS, HDD_MAP_UNIT};

/// Fill a 16-byte B3 request block (design §2.3) at `block`. The host-written
/// result dword at `HDD_MAP_RESULT` is zeroed here so a test can tell a real
/// answer from a stale one.
fn write_map_request(machine: &mut Machine, block: u32, unit: u8, start: u32, steps: u32) {
    machine.write_physical_u8(block, 0);
    machine.write_physical_u8(block + HDD_MAP_UNIT, unit);
    machine.write_physical_u16(block + 2, 0);
    machine.write_physical_u32(block + HDD_MAP_START, start);
    machine.write_physical_u32(block + HDD_MAP_STEPS, steps);
    machine.write_physical_u32(block + HDD_MAP_RESULT, 0);
}

/// Ring command 3 the way the kernel's boot probe/register does: the far
/// pointer's offset word then segment word in the CD mailbox (command 1's
/// layout), `block` split so `(seg << 4) + off == block`.
fn register_map_block(machine: &mut Machine, block: u32) {
    machine.write_physical_u16(0x063C, (block & 0xF) as u16);
    machine.write_physical_u16(0x063E, (block >> 4) as u16);
    machine.perform_cd_doorbell(0x03);
}

/// The host-written result dword at `HDD_MAP_RESULT`, composed byte-wise
/// instead of through `read_physical_u32` so the test independently checks
/// the little-endian placement `write_physical_u32` is supposed to produce.
fn read_map_result(machine: &mut Machine, block: u32) -> u32 {
    let at = block + HDD_MAP_RESULT;
    u32::from(machine.read_physical_u8(at))
        | (u32::from(machine.read_physical_u8(at + 1)) << 8)
        | (u32::from(machine.read_physical_u8(at + 2)) << 16)
        | (u32::from(machine.read_physical_u8(at + 3)) << 24)
}

/// Mount `dir` as C: through Katea, the way `machine_storage_test.rs`'s
/// `machine_with_hdd_folder` does — a fresh machine plus a host-folder mount,
/// so `map_fat_chain` has a real Katea volume to answer from.
fn hdd_map_machine(dir: &std::path::Path) -> Machine {
    let mut machine = test_machine();
    machine.mount_hdd_folder(dir).unwrap();
    machine
}

/// A scratch host folder for the B3 hypercall tests, emptied first so a
/// previous run cannot seed it.
fn hdd_map_scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("izarra_hdd_map_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// Free conventional RAM for the tests' request block: below
// `BIOS_BOOT_CHOICE_ADDR` (0x0537) and clear of the firmware scratch block
// (0x600-0x63F, which the CD mailbox itself sits inside) -- nothing else in
// a test machine claims this range.
const HDD_MAP_TEST_BLOCK: u32 = 0x0520;

#[test]
fn hdd_map_probe_with_null_mailbox_parks_fe_and_registers_nothing() {
    let mut machine = test_machine();
    machine.write_physical_u16(0x063C, 0);
    machine.write_physical_u16(0x063E, 0);
    machine.perform_cd_doorbell(0x03);
    assert_eq!(machine.cd_doorbell_status, 0xFE, "probe parks FE");

    // Nothing was registered, so a lookup answers unregistered too. Unit 2
    // on purpose: with the correct unit, only the registration guard can
    // produce this 0xFE.
    write_map_request(&mut machine, HDD_MAP_TEST_BLOCK, 2, 2, 0);
    machine.perform_cd_doorbell(0x04);
    assert_eq!(machine.cd_doorbell_status, 0xFE);
}

/// Exercises all three doorbell-4 outcomes against a real Katea host-folder
/// volume: the zero-step success at the root cluster, a step count past the
/// chain cap (Refused), and a step count comfortably under the cap but past
/// the root directory's own (tiny, empty-folder) chain length -- the only
/// case here that proves `HDD_MAP_STEPS` is actually read rather than
/// ignored, since 0 and u32::MAX cannot tell "read" from "not read".
///
/// A real multi-cluster file's chain (mirroring
/// `katea_tree_test.rs::map_chain_matches_a_real_chain_walk`'s ~3 MiB
/// fixture) is not used here: reaching a file's start cluster and chain
/// length from `Machine`/`AtaDisk` needs accessors that walk the volume the
/// way `katea_tree_test.rs` does internally (`chain_via_walk`,
/// `first_cluster_of`), and none of the existing Machine-level plumbing
/// (`katea_file_lba` et al.) exposes clusters, only LBAs. The volume-level
/// tests in `katea_tree_test.rs` already pin the multi-cluster success and
/// end-of-chain cases directly against `map_chain`; this test's job is only
/// to prove the doorbell wiring reaches that logic and reads the right
/// fields.
#[test]
fn hdd_map_lookup_maps_root_refuses_past_cap_and_reports_end_of_chain() {
    let dir = hdd_map_scratch("lookup");
    let mut machine = hdd_map_machine(&dir);

    register_map_block(&mut machine, HDD_MAP_TEST_BLOCK);
    assert_eq!(machine.cd_doorbell_status, 0, "registration ok");

    // Cluster 2 is the volume root, always allocated: zero steps just
    // confirms the walk starts where it is told to.
    write_map_request(&mut machine, HDD_MAP_TEST_BLOCK, 2, 2, 0);
    machine.perform_cd_doorbell(0x04);
    assert_eq!(machine.cd_doorbell_status, 0);
    assert_eq!(read_map_result(&mut machine, HDD_MAP_TEST_BLOCK), 2);

    // A step count past the chain cap is refused (design §2.4's Refused
    // path — the guest falls back to its native walk). The host writes
    // nothing on a refusal; the stale zero `write_map_request` planted
    // stays.
    write_map_request(&mut machine, HDD_MAP_TEST_BLOCK, 2, 2, u32::MAX);
    machine.perform_cd_doorbell(0x04);
    assert_eq!(machine.cd_doorbell_status, 0xFE);
    assert_eq!(read_map_result(&mut machine, HDD_MAP_TEST_BLOCK), 0);

    // 4096 is comfortably under `max_chain()` (tens of thousands of
    // clusters, sized from the volume's floor partition size) but far past
    // the root directory's own chain -- a handful of system files fit in
    // its first cluster -- so this deterministically walks off the end and
    // proves the field at HDD_MAP_STEPS drove the walk. The host writes
    // nothing here either.
    write_map_request(&mut machine, HDD_MAP_TEST_BLOCK, 2, 2, 4096);
    machine.perform_cd_doorbell(0x04);
    assert_eq!(machine.cd_doorbell_status, 2, "end of chain (DE_SEEK)");
    assert_eq!(read_map_result(&mut machine, HDD_MAP_TEST_BLOCK), 0);
}

#[test]
fn hdd_map_lookup_without_katea_hdd_refuses() {
    let mut machine = test_machine(); // no ATA disk mounted
    register_map_block(&mut machine, HDD_MAP_TEST_BLOCK);
    assert_eq!(
        machine.cd_doorbell_status, 0,
        "registration alone never touches ata"
    );

    // Unit 2 on purpose: this test must reach the missing-disk refusal, not
    // stop earlier at the wrong-unit check.
    write_map_request(&mut machine, HDD_MAP_TEST_BLOCK, 2, 2, 1);
    machine.perform_cd_doorbell(0x04);
    assert_eq!(machine.cd_doorbell_status, 0xFE);
}

/// The host half of the two-sided drive guard: a request whose unit byte is
/// not the Katea HDD (DOS unit 2) refuses before any FAT walk. The kernel
/// half is `BootDrive >= 3` in fatfs.c's fast-path conjunction.
#[test]
fn hdd_map_lookup_refuses_a_wrong_unit() {
    let scratch = hdd_map_scratch("wrong_unit");
    let mut machine = hdd_map_machine(&scratch);
    register_map_block(&mut machine, HDD_MAP_TEST_BLOCK);

    write_map_request(&mut machine, HDD_MAP_TEST_BLOCK, 0, 2, 0);
    machine.perform_cd_doorbell(0x04);
    assert_eq!(machine.cd_doorbell_status, 0xFE);
    assert_eq!(read_map_result(&mut machine, HDD_MAP_TEST_BLOCK), 0);
}

#[test]
fn unknown_doorbell_commands_still_park_ff() {
    let mut machine = test_machine();
    machine.perform_cd_doorbell(0x05);
    assert_eq!(machine.cd_doorbell_status, 0xFF);
}

#[test]
fn int19_disarms_the_hdd_map_registration() {
    let dir = hdd_map_scratch("disarm");
    let mut machine = hdd_map_machine(&dir);

    register_map_block(&mut machine, HDD_MAP_TEST_BLOCK);
    assert_eq!(machine.cd_doorbell_status, 0);

    machine.disarm_hdd_map();

    write_map_request(&mut machine, HDD_MAP_TEST_BLOCK, 2, 2, 0);
    machine.perform_cd_doorbell(0x04);
    assert_eq!(
        machine.cd_doorbell_status, 0xFE,
        "disarm forgets the registration, same as a never-registered kernel"
    );
}

#[test]
fn deferred_cd_audio_commands_conserve_the_span_after_completion() {
    use izarravm_core::MASTER_CLOCK_HZ;
    fn pending(mode: GswMode, operation: u8, early_fdc: bool) -> (Machine, u64) {
        let mut machine = test_machine();
        machine.set_mode(mode);
        machine.mount_cd(audio_cd(40));
        machine.ide.device_mut().execute(&[0; 12]);
        let mut play = [0; 12];
        play[0] = 0x45;
        play[5] = 1;
        play[8] = 30;
        if operation != 0x45 {
            machine.ide.device_mut().execute(&play);
            machine.advance_devices_ticks(MASTER_CLOCK_HZ / 225);
            if operation == 1 {
                let mut pause = [0; 12];
                pause[0] = 0x4b;
                machine.ide.device_mut().execute(&pause);
            }
        }
        let now = machine.master_ticks();
        machine.fdc.write_port_at(0x3f2, 0x0c, now);
        machine.fdc.write_port_at(0x3f5, 0x08, now);
        while machine.fdc.read_port(0x3f4).unwrap() & 0x40 != 0 {
            machine.fdc.read_port(0x3f5);
        }
        for value in [0x03, 0xf0, 0, 0x0f, 0, 1] {
            machine.fdc.write_port_at(0x3f5, value, now);
        }
        if early_fdc {
            machine.advance_devices_ticks(MASTER_CLOCK_HZ * 9 / 10_000);
        }
        machine.ide.write_port(0x177, 0xa0);
        machine.advance_devices_ticks(MASTER_CLOCK_HZ / 20_000);
        machine.ide.read_port(0x177);
        let mut command = play;
        if operation != 0x45 {
            command = [0; 12];
            command[0] = if operation == 1 { 0x4b } else { operation };
            command[8] = u8::from(operation == 1);
        }
        let prefix = with_bus(&mut machine, |bus| {
            bus.prior_runs_core_clocks = 5_000;
            for byte in command {
                CpuBus::write_io(bus, 0x170, BusWidth::Byte, u32::from(byte), 17, false).unwrap();
            }
            bus.in_batch_master_ticks()
        });
        let deadline = prefix + MASTER_CLOCK_HZ / 10_000;
        assert_eq!(machine.ide.ticks_until_completion(), Some(deadline));
        let fdc = machine
            .fdc
            .ticks_until_event(machine.master_ticks())
            .unwrap();
        assert_eq!(fdc < deadline, early_fdc);
        (machine, deadline)
    }
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        for operation in [0x45, 0x4b, 1, 0x4e] {
            for early_fdc in [false, true] {
                let (mut whole, deadline) = pending(mode, operation, early_fdc);
                let (mut split, other_deadline) = pending(mode, operation, early_fdc);
                assert_eq!(deadline, other_deadline);
                let suffix = MASTER_CLOCK_HZ * 2 / 75;
                whole.advance_devices_ticks(deadline + suffix);
                split.advance_devices_ticks(deadline);
                split.advance_devices_ticks(suffix);
                assert_eq!(
                    whole.ide.device().playback(),
                    split.ide.device().playback(),
                    "{mode:?} op={operation:x} early_fdc={early_fdc}"
                );
                assert_eq!(
                    whole.ide.device().playback().current_lba,
                    if operation == 0x45 || operation == 1 {
                        3
                    } else {
                        1
                    }
                );
                assert_eq!(whole.timeline, split.timeline);
                assert_eq!(whole.pic, split.pic);
                assert_eq!(whole.ide.ticks_until_completion(), None);
                assert_eq!(whole.fdc.ticks_until_event(whole.master_ticks()), None);
                assert_eq!(whole.ide.read_port(0x177), split.ide.read_port(0x177));
            }
        }
    }
}
