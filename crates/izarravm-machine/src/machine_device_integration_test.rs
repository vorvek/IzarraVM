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
fn boot_suite_reports_timer_irq0_pass() {
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::X86_BOOT_TEST_IMAGE,
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(11_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);

    let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
    assert!(
        results.records.iter().any(|record| {
            record.status == izarravm_firmware::SuiteRecordStatus::Pass
                && record.name == "timer.irq0"
        }),
        "boot suite should report PASS timer.irq0"
    );
    // The timer idle genuinely advanced emulated time (ten ticks of ~11932
    // input clocks each), not spun instantly.
    assert!(machine.elapsed_clocks() > 1_500_000);
}

#[test]
fn boot_suite_reports_sb_dsp_reset_pass() {
    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = Machine::new_boot_image(
            MachineProfile::gsw_386(16, VideoCard::Vega),
            izarravm_firmware::X86_BOOT_TEST_IMAGE,
        )
        .unwrap();
        machine.set_mode(mode);
        let reason = machine
            .run_until_halt_or_cycles(mode.clock_hz() / 2)
            .unwrap();
        assert_eq!(reason, StopReason::Halted);
        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        assert!(
            results.records.iter().any(|record| {
                record.status == izarravm_firmware::SuiteRecordStatus::Pass
                    && record.name == "sound.sb_dsp_reset"
            }),
            "boot suite should report PASS sound.sb_dsp_reset in {mode:?}"
        );
    }
}

#[test]
fn boot_suite_reports_opl3_pass() {
    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = Machine::new_boot_image(
            MachineProfile::gsw_386(16, VideoCard::Vega),
            izarravm_firmware::X86_BOOT_TEST_IMAGE,
        )
        .unwrap();
        machine.set_mode(mode);
        let reason = machine
            .run_until_halt_or_cycles(mode.clock_hz() / 2)
            .unwrap();
        assert_eq!(reason, StopReason::Halted);
        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        assert!(
            results.records.iter().any(|record| {
                record.status == izarravm_firmware::SuiteRecordStatus::Pass
                    && record.name == "sound.opl3"
            }),
            "boot suite should report PASS sound.opl3 in {mode:?}"
        );
    }
}

#[test]
fn boot_suite_reports_opl2_pass() {
    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = Machine::new_boot_image(
            MachineProfile::gsw_386(16, VideoCard::Vega),
            izarravm_firmware::X86_BOOT_TEST_IMAGE,
        )
        .unwrap();
        machine.set_mode(mode);
        let reason = machine
            .run_until_halt_or_cycles(mode.clock_hz() / 2)
            .unwrap();
        assert_eq!(reason, StopReason::Halted);
        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        assert!(
            results.records.iter().any(|record| {
                record.status == izarravm_firmware::SuiteRecordStatus::Pass
                    && record.name == "sound.opl2"
            }),
            "boot suite should report PASS sound.opl2 in {mode:?}"
        );
    }
}

#[test]
fn boot_suite_reports_sb_8bit_dma_pass() {
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        let mut machine = Machine::new_boot_image(
            MachineProfile::gsw_386(16, VideoCard::Vega),
            izarravm_firmware::X86_BOOT_TEST_IMAGE,
        )
        .unwrap();
        machine.set_mode(mode);
        let reason = machine
            .run_until_halt_or_cycles(mode.clock_hz() / 2)
            .unwrap();
        assert_eq!(reason, StopReason::Halted);
        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        assert!(
            results.records.iter().any(|record| {
                record.status == izarravm_firmware::SuiteRecordStatus::Pass
                    && record.name == "sound.sb_8bit_dma"
            }),
            "boot suite should report PASS sound.sb_8bit_dma in {mode:?}; remaining={}, playing={}",
            machine.sb16.test_block_remaining(),
            machine.sb16.test_is_playing()
        );
    }
}

#[test]
fn boot_suite_reports_sb_16bit_dma_pass() {
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::X86_BOOT_TEST_IMAGE,
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(11_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
    assert!(
        results.records.iter().any(|record| {
            record.status == izarravm_firmware::SuiteRecordStatus::Pass
                && record.name == "sound.sb_16bit_dma"
        }),
        "boot suite should report PASS sound.sb_16bit_dma (clock-driven auto-init DMA + IRQ5)"
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
