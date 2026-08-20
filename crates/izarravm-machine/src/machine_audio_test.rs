// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_core::{
    CanonicalSectionId, CanonicalSectionRequirement, CanonicalSectionVersion, CanonicalStateView,
    CanonicalStateWriter,
};

fn canonical_dma_records(machine: &Machine) -> (Vec<u8>, Vec<u8>) {
    fn finish(writer: CanonicalStateWriter) -> Vec<u8> {
        let bytes = writer.finish().unwrap();
        CanonicalStateView::parse(&bytes).unwrap().sections()[0]
            .payload()
            .to_vec()
    }

    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_0006).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| machine.dma.canonical_projection().write_payload(out),
        )
        .unwrap();
    let mut totals = CanonicalStateWriter::new().unwrap();
    totals
        .section(
            CanonicalSectionId::new(0x7ffe_0001).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| machine.dma.canonical_event_totals_v1().write_payload(out),
        )
        .unwrap();
    (finish(state), finish(totals))
}

fn prepare_sb8_dma_canonical_proof() -> Machine {
    let mut machine = test_machine();
    for (index, byte) in (0..16u8).map(|value| value.wrapping_mul(16)).enumerate() {
        machine.write_physical_u8(0x1_0000 + index as u32, byte);
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
    machine
}

fn prepare_sb16_dma_canonical_proof() -> Machine {
    let mut machine = test_machine();
    for index in 0..16u32 {
        let bytes = (index as u16).wrapping_mul(0x111).to_le_bytes();
        machine.write_physical_u8(0x2_0000 + index * 2, bytes[0]);
        machine.write_physical_u8(0x2_0001 + index * 2, bytes[1]);
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0xd6, BusWidth::Byte, 0x49, false).unwrap();
        bus.write_io(0xc4, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0xc4, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0xc6, BusWidth::Byte, 0x0f, false).unwrap();
        bus.write_io(0xc6, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x8b, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0xd4, BusWidth::Byte, 0x01, false).unwrap();
        for byte in [0x41u8, 0x56, 0x22, 0xb0, 0x30, 0x0f, 0x00] {
            bus.write_io(0x22c, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
    });
    machine
}

fn prepare_wss_dma_canonical_proof() -> Machine {
    let mut machine = test_machine();
    let frame = [0x01u8, 0x00, 0xfe, 0xff];
    for index in 0..8u32 {
        for (offset, byte) in frame.into_iter().enumerate() {
            machine.write_physical_u8(0x1_0000 + index * 4 + offset as u32, byte);
        }
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0b, BusWidth::Byte, 0x48, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x1f, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0a, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x48, false)
            .unwrap();
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x5c, false).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
            .unwrap();
        wss_write_indirect(bus, 10, 0x02);
        wss_write_indirect(bus, 15, 0x07);
        wss_write_indirect(bus, 14, 0x00);
        wss_write_indirect(bus, 9, 0x09);
    });
    machine
}

#[test]
fn sb_dma_canonical_records_are_invariant_to_advance_batch_size() {
    for prepare in [
        prepare_sb8_dma_canonical_proof as fn() -> Machine,
        prepare_sb16_dma_canonical_proof,
    ] {
        let mut whole = prepare();
        let mut split = prepare();
        whole.advance_devices_clocks(200_000);
        for _ in 0..200 {
            split.advance_devices_clocks(1_000);
        }
        assert_eq!(canonical_dma_records(&whole), canonical_dma_records(&split));
    }

    let mut byte = prepare_sb8_dma_canonical_proof();
    byte.advance_devices_clocks(200_000);
    let byte_records = canonical_dma_records(&byte);
    assert_eq!(byte_records.0.len(), 152);
    assert_eq!(byte_records.1.len(), 64);
    assert_eq!(
        u64::from_le_bytes(byte_records.1[8..16].try_into().unwrap()),
        16
    );

    let mut word = prepare_sb16_dma_canonical_proof();
    word.advance_devices_clocks(200_000);
    let word_records = canonical_dma_records(&word);
    assert_eq!(
        u64::from_le_bytes(word_records.1[40..48].try_into().unwrap()),
        16,
        "sixteen 16-bit transfers are sixteen cycles"
    );
}

#[test]
fn wss_dma_canonical_records_are_invariant_to_advance_batch_size() {
    let mut whole = prepare_wss_dma_canonical_proof();
    let mut split = prepare_wss_dma_canonical_proof();
    whole.advance_devices_clocks(200_000);
    for _ in 0..200 {
        split.advance_devices_clocks(1_000);
    }

    let whole_records = canonical_dma_records(&whole);
    assert_eq!(whole_records, canonical_dma_records(&split));
    assert_eq!(whole_records.0.len(), 152);
    assert_eq!(whole_records.1.len(), 64);
    assert_eq!(
        u64::from_le_bytes(whole_records.1[0..8].try_into().unwrap()),
        32
    );
}

#[test]
fn passive_target_ports_allow_capability_probes_to_fail_cleanly() {
    // 0x226 is the SB DSP reset port: still an unimplemented passive port
    // (0x224/0x225 are now the CT1745 mixer, 0x388 the OPL chip).
    let mut machine = test_machine();
    let value = with_bus(&mut machine, |bus| {
        bus.read_io(0x0226, BusWidth::Byte, 0, false).unwrap()
    });

    assert_eq!(value, 0xff);
    assert!(
        machine
            .bus_trace()
            .cycles()
            .iter()
            .any(|cycle| cycle.kind == BusAccessKind::IoRead && cycle.address == 0x0226)
    );
}

#[test]
fn mixer_index_port_decodes_instead_of_falling_through_passive() {
    // 0x224 used to read 0xFF as a passive port; it is now the CT1745 mixer
    // index register, whose read returns the latched index (0 at reset).
    let mut machine = test_machine();
    let index_read = with_bus(&mut machine, |bus| {
        bus.read_io(0x0224, BusWidth::Byte, 0, false).unwrap()
    });
    assert_eq!(index_read, 0x00, "0x224 returns the latched mixer index");
    // Programming register 0x80 (IRQ7) round-trips through 0x225.
    with_bus(&mut machine, |bus| {
        bus.write_io(0x224, BusWidth::Byte, 0x80, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x04, false).unwrap();
    });
    let routed = with_bus(&mut machine, |bus| {
        bus.write_io(0x224, BusWidth::Byte, 0x80, false).unwrap();
        bus.read_io(0x225, BusWidth::Byte, 0, false).unwrap()
    });
    assert_eq!(routed, 0x04, "IRQ7 latched in mixer register 0x80");
}

#[test]
fn dma_channel_one_transfers_from_memory_through_the_bus() {
    let mut machine = test_machine();
    // Seed memory at physical 0x01_0010 (page 0x01, offset 0x0010).
    machine.write_physical_u8(0x0001_0010, 0x77);
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap(); // mode ch1: single, read
        bus.write_io(0x02, BusWidth::Byte, 0x10, false).unwrap(); // address LSB
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap(); // address MSB -> 0x0010
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap(); // count LSB
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap(); // count MSB -> 0 (1 transfer)
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap(); // page -> 0x01_0010
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap(); // unmask channel 1
    });
    let byte = machine.dma_read_byte(1).expect("a byte from channel 1");
    assert_eq!(byte, 0x77);
}

#[test]
fn sb_dsp_reset_handshake_through_the_bus() {
    let mut machine = test_machine();
    // Reset: write 1, then 0 to the DSP reset port 0x226.
    with_bus(&mut machine, |bus| {
        bus.write_io(0x226, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x226, BusWidth::Byte, 0x00, false).unwrap();
    });
    // Advance emulated time past the ~100us DSP settle window.
    machine.advance_devices_ticks(izarravm_core::MASTER_CLOCK_HZ / 5_000);
    let status = with_bus(&mut machine, |bus| {
        u8::try_from(bus.read_io(0x22E, BusWidth::Byte, 0, false).unwrap()).unwrap()
    });
    assert_eq!(status & 0x80, 0x80, "data available after reset");
    let ack = with_bus(&mut machine, |bus| {
        u8::try_from(bus.read_io(0x22A, BusWidth::Byte, 0, false).unwrap()).unwrap()
    });
    assert_eq!(ack, 0xAA);
}

#[test]
fn sb_dsp_status_read_sets_io_touched_in_every_cpu_mode() {
    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        for port in [0x22Eu16, 0x22F] {
            with_bus(&mut machine, |bus| {
                *bus.io_touched = false;
                let _ = bus.read_io(port, BusWidth::Byte, 0, false).unwrap();
                assert!(
                    *bus.io_touched,
                    "{mode:?}: DSP status port {port:#06X} must end the batch"
                );
            });
        }
    }
}

#[test]
fn sb_dsp_version_and_status_route_through_the_bus() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x22C, BusWidth::Byte, 0xE1, false).unwrap(); // read version
    });
    let hi = with_bus(&mut machine, |bus| {
        u8::try_from(bus.read_io(0x22A, BusWidth::Byte, 0, false).unwrap()).unwrap()
    });
    let lo = with_bus(&mut machine, |bus| {
        u8::try_from(bus.read_io(0x22A, BusWidth::Byte, 0, false).unwrap()).unwrap()
    });
    assert_eq!([hi, lo], [4, 5]);
}

#[test]
fn sb_dsp_write_status_does_not_read_as_open_bus() {
    let mut machine = test_machine();
    let status = with_bus(&mut machine, |bus| {
        u8::try_from(bus.read_io(0x22C, BusWidth::Byte, 0, false).unwrap()).unwrap()
    });
    assert_eq!(
        status & 0x80,
        0x00,
        "DSP write-status bit 7 clear means ready for commands"
    );
}

#[test]
fn idle_f2_publishes_8bit_status_and_22f_cross_acknowledges_it() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x22C, BusWidth::Byte, 0xF2, false).unwrap();
    });
    assert!(!machine.pic.irr_bit(7));

    machine.advance_devices_ticks(0);

    assert!(machine.pic.irr_bit(7));
    let (status, cleared) = with_bus(&mut machine, |bus| {
        bus.write_io(0x224, BusWidth::Byte, 0x82, false).unwrap();
        let status = bus.read_io(0x225, BusWidth::Byte, 0, false).unwrap();
        bus.read_io(0x22F, BusWidth::Byte, 0, false).unwrap();
        bus.write_io(0x224, BusWidth::Byte, 0x82, false).unwrap();
        let cleared = bus.read_io(0x225, BusWidth::Byte, 0, false).unwrap();
        (status, cleared)
    });
    assert_eq!(status, 0x01);
    assert_eq!(cleared, 0x00);
    assert!(
        machine.pic.irr_bit(7),
        "the device ack does not clear the PIC"
    );
}

#[test]
fn f2_after_16bit_arm_publishes_16bit_status_and_22e_cross_acknowledges_it() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        for byte in [0xB0u8, 0x00, 0x00, 0x00, 0xF2] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
    });

    machine.advance_devices_ticks(0);

    assert!(machine.pic.irr_bit(7));
    let (status, cleared) = with_bus(&mut machine, |bus| {
        bus.write_io(0x224, BusWidth::Byte, 0x82, false).unwrap();
        let status = bus.read_io(0x225, BusWidth::Byte, 0, false).unwrap();
        bus.read_io(0x22E, BusWidth::Byte, 0, false).unwrap();
        bus.write_io(0x224, BusWidth::Byte, 0x82, false).unwrap();
        let cleared = bus.read_io(0x225, BusWidth::Byte, 0, false).unwrap();
        (status, cleared)
    });
    assert_eq!(status, 0x02);
    assert_eq!(cleared, 0x00);
    assert!(
        machine.pic.irr_bit(7),
        "the device ack does not clear the PIC"
    );
}

#[test]
fn sb_dma_irq5_fires_from_the_cpu_clock_without_host_audio_pull() {
    let mut machine = test_machine();
    // 8-bit ramp at 0x01_0000; arm DMA ch1 + DSP exactly like the playback golden.
    for (i, b) in (0..16u8).map(|i| i * 16).enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        for &b in &[0x41u8, 0x2B, 0x11, 0x14, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    let before = with_bus(&mut machine, |bus| bus.interrupt_pending());
    assert!(!before, "no IRQ pending before time advances");
    // Advance CPU time for well over the 16-sample block (single-cycle -> end IRQ).
    machine.advance_devices_clocks(200_000);
    let after = with_bus(&mut machine, |bus| bus.interrupt_pending());
    assert!(
        after,
        "IRQ5 must be raised by the per-clock sample advance, not the host render path"
    );
}

#[test]
fn sb16_8bit_dma_command_c0_plays_and_raises_irq5() {
    let mut machine = test_machine();
    for (i, b) in (0..16u8).map(|i| i * 16).enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        // SB16 8-bit single-cycle output: command 0xC0, unsigned mono,
        // count 15 -> 16 DMA bytes. Doom-style SB16 drivers may use this
        // command family after detecting a DSP 4.x card.
        for &b in &[0x41u8, 0x2B, 0x11, 0xC0, 0x00, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    machine.advance_devices_clocks(200_000);
    let after = with_bus(&mut machine, |bus| bus.interrupt_pending());
    assert!(after, "SB16 0xC0 DMA playback must raise IRQ5");
    let out = machine.sb16.test_render_dsp_audio(16);
    assert_eq!(out.len(), 16, "SB16 0xC0 playback drained DMA bytes");
}

#[test]
fn sb_mixer_selects_irq7_and_routes_the_dma_irq() {
    let mut machine = test_machine();
    // 8-bit ramp at 0x01_0000 (DMA ch1, the mixer's default 8-bit channel).
    for (i, b) in (0..16u8).map(|i| i * 16).enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        // Route the DSP IRQ on IRQ7 (mixer register 0x80 = 0x04).
        bus.write_io(0x224, BusWidth::Byte, 0x80, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x04, false).unwrap();
        // PIC base 0x08 so IRQ7 -> vector 0x0F; mask everything except IR7.
        bus.write_io(0x20, BusWidth::Byte, 0x11, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x08, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x7F, false).unwrap();
        // DMA ch1 + DSP 8-bit single-cycle, exactly like the IRQ5 golden.
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        for &b in &[0x41u8, 0x2B, 0x11, 0x14, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    machine.advance_devices_clocks(200_000);
    let vector = with_bus(&mut machine, |bus| bus.acknowledge_interrupt());
    assert_eq!(vector, Some(0x0F), "the DMA IRQ must land on line 7, not 5");
}

#[test]
fn sb_mixer_selects_dma_channel_3() {
    let mut machine = test_machine();
    let bytes: Vec<u8> = (0..16).map(|i| (i * 16) as u8).collect();
    for (i, &b) in bytes.iter().enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        // Route the 8-bit DMA through DMA3 (mixer register 0x81 = 0x08).
        bus.write_io(0x224, BusWidth::Byte, 0x81, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x08, false).unwrap();
        // DMA ch3: page 0x82, byte addr 0, count 15 (16 bytes), single read.
        bus.write_io(0x0B, BusWidth::Byte, 0x4B, false).unwrap();
        bus.write_io(0x06, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x06, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x07, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x07, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x82, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x03, false).unwrap();
        // DSP: 11025 Hz, block 16, single-cycle 8-bit DMA output.
        for &b in &[0x41u8, 0x2B, 0x11, 0x14, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    let out = {
        machine.advance_devices_clocks(200_000);
        machine.sb16.test_render_dsp_audio(16)
    };
    assert_eq!(out.len(), 16, "buffer drained via DMA channel 3");
    assert!(out.iter().any(|&(l, _)| l < 0), "expected negative samples");
    assert!(
        out.iter().all(|&(l, r)| l == r),
        "8-bit mono duplicated L/R"
    );
    // Single mode masks channel 3 at terminal count, proving the producer
    // drew from channel 3 (channel 1 stayed masked and untouched).
    assert_eq!(machine.dma_read_byte(3), None, "ch3 reached TC");
}

#[test]
fn sb_mixer_reset_restores_irq5_default() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        // Route the IRQ on IRQ7, then reset the mixer (any value to 0x00).
        bus.write_io(0x224, BusWidth::Byte, 0x80, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x224, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x01, false).unwrap();
        // A guest reset restores the hardware IRQ5 default, not the host config.
        bus.write_io(0x224, BusWidth::Byte, 0x80, false).unwrap();
        let byte = bus.read_io(0x225, BusWidth::Byte, 0, false).unwrap();
        assert_eq!(byte, 0x02);
    });
}

#[test]
fn machine_applies_host_sound_blaster_config_at_boot() {
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.sound_blaster = SoundBlasterConfig {
        enabled: true,
        irq: SbIrq::I7,
        dma: SbDma8::D3,
        high_dma: SbDma16::D6,
    };
    let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();
    // The mixer boots on the configured routing, not the hardware IRQ5/DMA1/DMA5.
    let (irq_byte, dma_byte) = with_bus(&mut machine, |bus| {
        bus.write_io(0x224, BusWidth::Byte, 0x80, false).unwrap();
        let irq = u8::try_from(bus.read_io(0x225, BusWidth::Byte, 0, false).unwrap()).unwrap();
        bus.write_io(0x224, BusWidth::Byte, 0x81, false).unwrap();
        let dma = u8::try_from(bus.read_io(0x225, BusWidth::Byte, 0, false).unwrap()).unwrap();
        (irq, dma)
    });
    assert_eq!(irq_byte, 0x04, "register 0x80 boots on IRQ7");
    assert_eq!(dma_byte, 0x48, "register 0x81 boots on DMA3 | DMA6");
}

#[test]
fn disabled_sb16_leaves_ports_dma_irq_and_voice_inert() {
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.sound_blaster.enabled = false;
    let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();
    machine.write_physical_u8(0x1_0000, 0x55);

    with_bus(&mut machine, |bus| {
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x224, BusWidth::Byte, 0x82, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x22C, BusWidth::Byte, 0xF2, false).unwrap();
    });

    machine.advance_devices_clocks(200_000);

    assert!(!machine.pic.irr_bit(7));
    assert!(machine.sb16.irq_deadline().is_none());
    assert_eq!(machine.dma_read_byte(1), Some(0x55));
    let (index, data, dsp_data) = with_bus(&mut machine, |bus| {
        (
            bus.read_io(0x224, BusWidth::Byte, 0, false).unwrap(),
            bus.read_io(0x225, BusWidth::Byte, 0, false).unwrap(),
            bus.read_io(0x22A, BusWidth::Byte, 0, false).unwrap(),
        )
    });
    assert_eq!((index, data, dsp_data), (0x82, 0x04, 0xFF));
    assert!(
        machine
            .render_audio(128)
            .iter()
            .all(|&(left, right)| left == 0 && right == 0)
    );
}

#[test]
fn sb_8bit_dma_plays_a_buffer_through_the_dsp() {
    let mut machine = test_machine();
    // A 16-byte unsigned ramp in conventional memory at 0x01_0000.
    let bytes: Vec<u8> = (0..16).map(|i| (i * 16) as u8).collect();
    for (i, &b) in bytes.iter().enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        // DMA ch1: address 0x0000, page 0x01, count 15, single read.
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap(); // mode ch1
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap(); // page -> 0x01_0000
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap(); // unmask ch1
        bus.write_io(0x224, BusWidth::Byte, 0x32, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0xF8, false).unwrap(); // level 31 << 3
        bus.write_io(0x224, BusWidth::Byte, 0x33, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0xF8, false).unwrap();
        // DSP: 11025 Hz, block 16, single 8-bit DMA output.
        for &b in &[0x41u8, 0x2B, 0x11, 0x14, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    let out = {
        // Playback is now clock-driven: advance CPU time for well over the
        // 16-sample block (single-cycle -> end IRQ), then drain the ring.
        machine.advance_devices_clocks(200_000);
        machine.sb16.test_render_dsp_audio(16)
    };
    assert_eq!(
        out,
        vec![
            (-32768, -32768),
            (-28672, -28672),
            (-24576, -24576),
            (-20480, -20480),
            (-16384, -16384),
            (-12288, -12288),
            (-8192, -8192),
            (-4096, -4096),
            (0, 0),
            (4096, 4096),
            (8192, 8192),
            (12288, 12288),
            (16384, 16384),
            (20480, 20480),
            (24576, 24576),
            (28672, 28672),
        ]
    );
    // Single mode masks channel 1 at terminal count.
    assert_eq!(machine.dma_read_byte(1), None);
}

#[test]
fn sb_pro_8bit_stereo_deinterleaves_two_bytes_per_frame_at_the_halved_rate() {
    let mut machine = test_machine();
    // A 16-byte unsigned interleaved L/R pattern in conventional memory:
    // bytes 0,16,32,... so each frame's left byte differs from its right.
    let bytes: Vec<u8> = (0..16).map(|i| (i * 16) as u8).collect();
    for (i, &b) in bytes.iter().enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        // DMA ch1: address 0x0000, page 0x01, count 15, single read.
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap(); // mode ch1
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap(); // page -> 0x01_0000
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap(); // unmask ch1
        // Mixer register 0x0E bit1: SB Pro stereo.
        bus.write_io(0x224, BusWidth::Byte, 0x0E, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x02, false).unwrap();
        // Voice volume to unity so the decoded L/R samples survive the mixer.
        bus.write_io(0x224, BusWidth::Byte, 0x32, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0xF8, false).unwrap(); // level 31 << 3
        bus.write_io(0x224, BusWidth::Byte, 0x33, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0xF8, false).unwrap();
        // DSP: set the interleaved byte rate via the 0x40 TIME CONSTANT
        // (tc 0xD3 -> 1_000_000/45 = 22_222 byte/s; SB Pro stereo halves it
        // to the per-channel frame rate), block 16, single 8-bit DMA output.
        for &b in &[0x40u8, 0xD3, 0x14, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    // Advance well past the 16-byte block (8 stereo frames at 2 bytes/frame).
    machine.advance_devices_clocks(200_000);
    // Drain the raw producer ring: each frame must carry DISTINCT L/R pulled
    // from two interleaved DMA bytes (left = even byte, right = odd byte).
    let raw = machine.sb16.test_render_dsp_audio(8);
    assert_eq!(raw.len(), 8, "8 stereo frames from a 16-byte block");
    // Frame 0: left byte 0 (= -32768), right byte 16 (= -28672); distinct.
    assert_ne!(raw[0].0, raw[0].1, "frame 0 de-interleaves distinct L/R");
    assert!(
        raw.iter().any(|&(l, r)| l != r),
        "stereo de-interleave yields a per-channel L != R through the DMA path"
    );
    // Single mode masks channel 1 at terminal count.
    assert_eq!(machine.dma_read_byte(1), None);
    // And the resampler runs at the HALVED per-channel rate: byte rate 22_222
    // (1_000_000/45) -> 11_111 Hz.
    let out = machine.render_audio(OPL_NATIVE_HZ as usize / 50);
    assert!(!out.is_empty(), "SB Pro stereo produces output");
    assert_eq!(
        machine.sb16.test_resampler_rate_hz(),
        11_111,
        "DSP resampler configured at the halved per-channel rate"
    );
}

#[test]
fn sb_pro_stereo_auto_init_keeps_every_frame_in_a_large_batch() {
    let mut machine = test_machine();
    for (i, byte) in [0x00u8, 0x40, 0x80, 0xC0].into_iter().enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, byte);
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0B, BusWidth::Byte, 0x59, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x03, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x224, BusWidth::Byte, 0x0E, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x02, false).unwrap();
        for &byte in &[0x41u8, 0x2B, 0x11, 0x48, 0x03, 0x00, 0x1C] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
    });
    let rate = u64::from(machine.sb16.test_output_frame_rate());
    let clocks = machine
        .timeline
        .cpu_clocks_until(timeline::DeviceClock::Dsp, 8, rate)
        .unwrap();

    machine.advance_devices_clocks(clocks);
    let out = machine.sb16.test_render_dsp_audio(8);

    assert_eq!(out.len(), 8, "four auto-init blocks produce eight frames");
    assert!(out.iter().all(|(left, right)| left != right));
}

#[test]
fn sb_16bit_dma_plays_a_signed_stereo_buffer_through_the_dsp() {
    let mut machine = test_machine();
    // 8 signed-LE stereo frames (32 bytes). The slave 8237A (channel 5)
    // word-addresses its transfers: the page supplies A23-A17 from its bits
    // 7-1, so page 0x02 at word addr 0 drives byte base 0x2_0000 (A0 tied
    // low). Each frame is L = -1 (0xFFFF) then R = +1 (0x0001).
    let frame: [u8; 4] = [0xFF, 0xFF, 0x01, 0x00];
    for i in 0..8 {
        for (j, &b) in frame.iter().enumerate() {
            machine.write_physical_u8(0x2_0000 + (i * 4 + j) as u32, b);
        }
    }
    with_bus(&mut machine, |bus| {
        // Slave ch5 (local ch1): word addr 0, page 0x8B=0x02, count 15 (16
        // words), auto-init read.
        bus.write_io(0xD6, BusWidth::Byte, 0x59, false).unwrap(); // slave ch1 mode: auto-init, read
        bus.write_io(0xC4, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0xC4, BusWidth::Byte, 0x00, false).unwrap(); // word addr 0
        bus.write_io(0xC6, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0xC6, BusWidth::Byte, 0x00, false).unwrap(); // count 15 -> 16 words
        bus.write_io(0x8B, BusWidth::Byte, 0x02, false).unwrap(); // page -> byte base 0x2_0000
        bus.write_io(0xD4, BusWidth::Byte, 0x01, false).unwrap(); // unmask slave ch1
        // Voice volume to unity (0 dB) so the exact -1/+1 samples survive the
        // CT1745 voice attenuation and the test stays about 16-bit decoding.
        bus.write_io(0x224, BusWidth::Byte, 0x32, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0xF8, false).unwrap(); // level 31 << 3
        bus.write_io(0x224, BusWidth::Byte, 0x33, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0xF8, false).unwrap();
        // DSP: 22050 Hz, 16-bit auto-init output, signed, stereo, count 15.
        for &b in &[0x41u8, 0x56, 0x22, 0xB6, 0x30, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    let rate = u64::from(machine.sb16.test_output_frame_rate());
    let clocks = machine
        .timeline
        .cpu_clocks_until(timeline::DeviceClock::Dsp, 32, rate)
        .unwrap();
    machine.advance_devices_clocks(clocks);
    let out = machine.sb16.test_render_dsp_audio(32);
    assert_eq!(out.len(), 32, "a large batch keeps every stereo frame");
    assert_eq!(out[0].0, -1, "left channel is signed -1");
    assert_eq!(out[0].1, 1, "right channel is signed +1");
    assert!(
        out.iter().all(|&(l, r)| l == -1 && r == 1),
        "every stereo frame decodes L=-1, R=+1"
    );
    // Auto-init: channel 5 (the mixer's default 16-bit channel) still feeds.
    assert!(
        machine.dma_read_word(5).is_some(),
        "auto-init keeps feeding"
    );
}

#[test]
fn sb_16bit_dma_waits_for_the_first_sample_deadline_before_reading() {
    let mut machine = test_machine();
    for i in 0..16u32 {
        let word = (0x1000u16 + i as u16).to_le_bytes();
        machine.write_physical_u8(0x2_0000 + i * 2, word[0]);
        machine.write_physical_u8(0x2_0001 + i * 2, word[1]);
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0xD6, BusWidth::Byte, 0x59, false).unwrap();
        bus.write_io(0xC4, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0xC4, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0xC6, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0xC6, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x8B, BusWidth::Byte, 0x02, false).unwrap();
        bus.write_io(0xD4, BusWidth::Byte, 0x01, false).unwrap();
        for &byte in &[0x41u8, 0x56, 0x22, 0xB6, 0x00, 0x03, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
    });
    let rate = u64::from(machine.sb16.test_output_frame_rate());
    let first_frame = machine
        .timeline
        .cpu_clocks_until(timeline::DeviceClock::Dsp, 1, rate)
        .unwrap();

    machine.advance_devices_clocks(first_frame - 1);

    assert_eq!(machine.sb16.test_drain_frame(), None);
    assert_eq!(
        machine.dma_read_word(5),
        Some(0x1000),
        "DMA remains at the first word until its sample deadline"
    );
}

#[test]
fn short_16bit_dma_does_not_fabricate_silent_words() {
    let mut machine = test_machine();
    for (i, word) in [0x1000u16, 0x2000, 0x3000].into_iter().enumerate() {
        let bytes = word.to_le_bytes();
        machine.write_physical_u8(0x2_0000 + i as u32 * 2, bytes[0]);
        machine.write_physical_u8(0x2_0001 + i as u32 * 2, bytes[1]);
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0xD6, BusWidth::Byte, 0x49, false).unwrap();
        bus.write_io(0xC4, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0xC4, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0xC6, BusWidth::Byte, 0x02, false).unwrap();
        bus.write_io(0xC6, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x8B, BusWidth::Byte, 0x02, false).unwrap();
        bus.write_io(0xD4, BusWidth::Byte, 0x01, false).unwrap();
        for &byte in &[0x41u8, 0x56, 0x22, 0xB0, 0x00, 0x07, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
    });
    let rate = u64::from(machine.sb16.test_output_frame_rate());
    let clocks = machine
        .timeline
        .cpu_clocks_until(timeline::DeviceClock::Dsp, 8, rate)
        .unwrap();

    machine.advance_devices_clocks(clocks);
    let out = machine.sb16.test_render_dsp_audio(8);

    assert_eq!(out.len(), 3, "only the three DMA words become samples");
    assert_eq!(machine.sb16.test_block_remaining(), 5);
    assert!(
        !machine.pic.irr_bit(7),
        "an incomplete DSP block has no IRQ"
    );
}

#[test]
fn wss_16bit_stereo_dma_plays_and_irqs_through_the_machine() {
    let mut machine = test_machine();
    // 8 signed-LE 16-bit stereo frames (32 bytes) at byte base 0x01_0000 over
    // the WSS byte-wide DMA channel 0. Each frame is asymmetric: L = +1
    // (0x0001), R = -2 (0xFFFE), so a real de-interleave yields L != R and the
    // codec's left-before-right ordering is observable.
    let frame: [u8; 4] = [0x01, 0x00, 0xFE, 0xFF];
    for i in 0..8u32 {
        for (j, &b) in frame.iter().enumerate() {
            machine.write_physical_u8(0x1_0000 + i * 4 + j as u32, b);
        }
    }
    with_bus(&mut machine, |bus| {
        // DMA ch0 (the WSS default): byte addr 0x0000, page 0x01 -> 0x01_0000,
        // count 31 (32 bytes), single read. Channel 0 ports: addr 0x00,
        // count 0x01, mode 0x0B, page 0x87, single-mask 0x0A.
        bus.write_io(0x0B, BusWidth::Byte, 0x48, false).unwrap(); // mode ch0: single, read
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap(); // addr 0x0000
        bus.write_io(0x01, BusWidth::Byte, 0x1F, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x00, false).unwrap(); // count 31 -> 32 bytes
        bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap(); // ch0 page -> 0x01_0000
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap(); // unmask ch0

        // Program the codec for 16-bit signed stereo at 48000 Hz (XTAL1 CFS6).
        // I8 = FMT(0x40) | S/M(0x10) | CFS6(0x0C) -> 0x5C, MCE-gated.
        bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08), false)
            .unwrap(); // R0: MCE | index 8
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x5C, false).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
            .unwrap(); // clear MCE
        // Enable the external INT pin (I10 IEN, bit1) so terminal count forwards.
        wss_write_indirect(bus, 10, 0x02);
        // Base count 7 -> underflow at frame 8 (N+1 cadence).
        wss_write_indirect(bus, 15, 0x07); // I15 lower count
        wss_write_indirect(bus, 14, 0x00); // I14 upper count (loads current)
        // Arm playback: I9 PEN (bit0) + ACAL (bit3).
        wss_write_indirect(bus, 9, 0x09);
        // Unmute both DACs at 0 dB so the decoded samples pass through.
        wss_write_indirect(bus, 6, 0x00);
        wss_write_indirect(bus, 7, 0x00);
    });

    // Drain the codec ring directly to prove de-interleave (render_audio mixes
    // OPL in, which would mask the exact values).
    machine.advance_devices_clocks(200_000);
    let mut frames = Vec::new();
    while let Some(f) = machine.wss.drain_frame() {
        frames.push(f);
    }
    assert!(!frames.is_empty(), "WSS produced rendered frames");
    assert_eq!(
        frames[0],
        (1, -2),
        "16-bit LE de-interleave, left before right"
    );
    assert!(
        frames.iter().any(|&(l, r)| l != r),
        "asymmetric L/R proves a real stereo de-interleave (not a mono dup)"
    );

    // The terminal-count interrupt reached the PIC on the WSS line (IRQ7).
    assert!(
        machine.pic.irr_bit(11),
        "WSS terminal count raised its IRQ on the configured line"
    );

    // render_audio still produces a full mix here (this drained the WSS ring
    // above, so the WSS contribution is proven through render_audio separately
    // in wss_stream_reaches_the_mixed_render_output_through_render_audio; this
    // only checks the OPL/DSP/speaker mix is not truncated by the WSS path).
    let mixed = machine.render_audio(64);
    assert!(
        !mixed.is_empty(),
        "render_audio still mixes the other streams after the WSS ring is drained"
    );
}

/// The exact port sequence HMI's MSS driver (`ms8m`, shipped with Tomb Raider)
/// emits to start playback, replayed on a machine whose profile says IRQ 11.
///
/// The driver's very first write is the board config register, and it derives
/// the PIC vector and mask it hooks from the same IRQ number it encoded there.
/// So the terminal-count interrupt has to land on the line the config byte
/// named, not on the profile's. Dropping that write -- which is what we used to
/// do -- left the codec interrupting IRQ 11 while the guest waited on IRQ 7,
/// and the setup utility's card test spun forever.
#[test]
fn wss_hmi_start_sequence_selects_irq7_and_interrupts_on_that_line() {
    let mut machine = test_machine();
    assert_eq!(
        machine.wss_routing(),
        Some((11, 0)),
        "the profile default is IRQ 11; the guest is about to move it"
    );

    // 8 unsigned-8-bit mono samples at physical 0x01_0000.
    for i in 0..8u32 {
        machine.write_physical_u8(0x1_0000 + i, 0x80);
    }

    with_bus(&mut machine, |bus| {
        // 1. The board config register, first, exactly as the driver's encoder
        //    builds it: IRQ 7 -> 0x08, DMA 0 -> |= 0x01.
        bus.write_io(0x530, BusWidth::Byte, 0x09, false).unwrap();

        // 2. I6/I7 = 0: both DACs unmuted at 0 dB.
        wss_write_indirect(bus, 6, 0x00);
        wss_write_indirect(bus, 7, 0x00);
        // 3. Format, programmed under MCE: 8-bit unsigned mono, 48000 Hz.
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x48, false)
            .unwrap(); // R0 = MCE | index 8
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x0C, false).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
            .unwrap(); // R0 = index 8, MCE cleared
        // 4. R0 = 0x49 (MCE | index 9), I9 = SDC | PEN.
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x49, false)
            .unwrap();
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x05, false).unwrap();
        // 5. R0 = 0x4A (MCE | index 10), I10 = IEN.
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x4A, false)
            .unwrap();
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x02, false).unwrap();
        // 6. R0 = 0x0F (MCE CLEARED, index 15), I15 = count low; then I14 high.
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x0F, false)
            .unwrap();
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x07, false).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x0E, false)
            .unwrap();
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x00, false).unwrap();
        // 7. The 8237: channel 0, single + auto-init + read(mem->I/O) = 0x58.
        bus.write_io(0x0B, BusWidth::Byte, 0x58, false).unwrap();
        bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap(); // page -> 0x01_0000
        bus.write_io(0x0C, BusWidth::Byte, 0x00, false).unwrap(); // clear flip-flop
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x0C, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x07, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x00, false).unwrap();
        // 8. Unmask the channel.
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap();
    });

    assert_eq!(
        machine.wss_routing(),
        Some((7, 0)),
        "the config write must move the codec to the line the guest hooked"
    );

    machine.advance_devices_clocks(200_000);

    assert!(
        machine.pic.irr_bit(7),
        "terminal count must interrupt on IRQ 7 -- the line the guest's own \
         config byte selected and whose vector it hooked"
    );
    assert!(
        !machine.pic.irr_bit(11),
        "and NOT on the profile's IRQ 11, which nothing is listening to"
    );
}

/// HMI's MSS detect probe, driven as guest port accesses through the bus. This
/// is why Microsoft Sound System never appeared in the setup's autodetect list:
/// the probe gives up at the `base+3` signature before it ever reaches the
/// codec.
#[test]
fn wss_detect_probe_finds_the_card_at_0x530() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        let r0 = bus.read_io(0x534, BusWidth::Byte, 0, false).unwrap();
        assert_eq!(r0 & 0x80, 0, "R0 INIT must read clear");
        let sig = bus.read_io(0x533, BusWidth::Byte, 0, false).unwrap();
        assert_eq!(sig & 0x3F, 0x04, "MSS signature at base+3");
        bus.write_io(0x533, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x534, BusWidth::Byte, 0x06, false).unwrap();
        bus.write_io(0x535, BusWidth::Byte, 0xAA, false).unwrap();
        bus.write_io(0x534, BusWidth::Byte, 0x06, false).unwrap();
        assert_eq!(
            bus.read_io(0x535, BusWidth::Byte, 0, false).unwrap(),
            0xAA,
            "I6 write/read-back closes the probe"
        );
    });
}

/// The 8237's current count is a guest-visible register, and a WSS sound engine
/// reads it to follow the play position. Tomb Raider's HMI engine polls
/// channel 0's count exactly this way.
///
/// The HLE block prefetch used to pull the codec's whole remaining count in one
/// gulp, which ran the 8237 all the way around inside a single advance: the
/// guest saw the reloaded value on every poll, the position never moved, and
/// the game hung at its first FMV while the codec was playing and interrupting
/// perfectly. The prefetch is now bounded by the frames the advance actually
/// plays, so the counter the guest reads tracks what it hears.
#[test]
fn wss_dma_position_advances_with_the_frames_the_guest_hears() {
    let mut machine = test_machine();
    for i in 0..256u32 {
        machine.write_physical_u8(0x1_0000 + i, 0x80);
    }
    with_bus(&mut machine, |bus| {
        // 8-bit mono, 8000 Hz (I8 CFS0/CSS0), base count 255 -> 256 frames.
        bus.write_io(0x0B, BusWidth::Byte, 0x58, false).unwrap();
        bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0C, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x0C, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0xFF, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap();

        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x48, false)
            .unwrap();
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
            .unwrap();
        wss_write_indirect(bus, 10, 0x02);
        wss_write_indirect(bus, 15, 0xFF);
        wss_write_indirect(bus, 14, 0x00);
        wss_write_indirect(bus, 9, 0x09);
        wss_write_indirect(bus, 6, 0x00);
        wss_write_indirect(bus, 7, 0x00);
    });

    // Read the channel-0 current count the way a guest does: clear the
    // flip-flop, then two byte reads.
    fn guest_dma_count(machine: &mut Machine) -> u16 {
        let mut count = 0;
        with_bus(machine, |bus| {
            bus.write_io(0x0C, BusWidth::Byte, 0x00, false).unwrap();
            let lo = bus.read_io(0x01, BusWidth::Byte, 0, false).unwrap();
            let hi = bus.read_io(0x01, BusWidth::Byte, 0, false).unwrap();
            count = (hi as u16) << 8 | lo as u16;
        });
        count
    }

    let start = guest_dma_count(&mut machine);
    assert_eq!(
        start, 255,
        "the channel starts at the count the guest wrote"
    );

    // Advance a fraction of the buffer and watch the counter move with it. At
    // 8000 Hz, 10 guest ms is 80 frames -- well short of the 256-frame buffer,
    // so the count must land strictly inside it rather than back at the reload.
    let clocks = GswMode::Gsw386
        .clock_rate()
        .clocks_for_fraction_floor(10, 1_000);
    machine.advance_devices_clocks(clocks);
    let mid = guest_dma_count(&mut machine);
    assert!(
        mid < start,
        "the guest-visible DMA position must advance as frames play (start {start}, now {mid})"
    );
    assert!(
        mid > 8,
        "and it must NOT have run the whole buffer around inside one advance \
         (start {start}, now {mid})"
    );

    // A second advance moves it again, monotonically within the buffer.
    machine.advance_devices_clocks(clocks);
    let later = guest_dma_count(&mut machine);
    assert!(
        later < mid,
        "the position keeps advancing (was {mid}, now {later})"
    );
}

/// One answer to "what IRQ is the codec on". The audio crate's device-level
/// default and the core profile default used to disagree (7 vs 11), so every
/// unit test in the audio crate asserted a line the shipped machine never used.
#[test]
fn wss_device_default_matches_the_profile_default() {
    let profile = izarravm_core::WssConfig::default();
    let device = izarravm_audio::Ad1848Config::default();
    assert_eq!(device.irq, profile.irq.line(), "IRQ default");
    assert_eq!(
        usize::from(device.dma),
        profile.dma.channel(),
        "DMA default"
    );
}

#[test]
fn wss_16bit_stereo_auto_init_refills_across_live_clock_changes() {
    let mut machine = test_machine();
    let pattern = [(1i16, -2i16), (3, -4), (5, -6), (7, -8)];
    for (index, &(left, right)) in pattern.iter().enumerate() {
        let address = 0x1_0000 + index as u32 * 4;
        for (offset, byte) in left
            .to_le_bytes()
            .into_iter()
            .chain(right.to_le_bytes())
            .enumerate()
        {
            machine.write_physical_u8(address + offset as u32, byte);
        }
    }

    with_bus(&mut machine, |bus| {
        // Four 16-bit stereo frames in an auto-init DMA channel. A 1 ms
        // advance at 48 kHz crosses this buffer twelve times.
        bus.write_io(0x0B, BusWidth::Byte, 0x58, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap();

        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x48, false)
            .unwrap();
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x5C, false).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
            .unwrap();
        wss_write_indirect(bus, 10, 0x02);
        wss_write_indirect(bus, 15, 3);
        wss_write_indirect(bus, 14, 0);
        wss_write_indirect(bus, 9, 0x09);
        wss_write_indirect(bus, 6, 0);
        wss_write_indirect(bus, 7, 0);
    });

    let mut elapsed_ticks = 0u64;
    for mode in [
        GswMode::Gsw586,
        GswMode::Gsw486,
        GswMode::Gsw386,
        GswMode::Gsw386Slow,
    ] {
        machine.set_mode(mode);
        let clocks = mode.clock_rate().clocks_for_fraction_floor(1, 1_000);
        let expected_ticks = mode.clock_rate().master_ticks_for_clocks_floor(clocks);
        let before = machine.master_ticks();
        machine.advance_devices_clocks(clocks);
        assert_eq!(machine.master_ticks() - before, expected_ticks, "{mode}");
        elapsed_ticks += expected_ticks;
    }

    let expected_frames =
        (u128::from(elapsed_ticks) * 48_000 / u128::from(izarravm_core::MASTER_CLOCK_HZ)) as usize;
    let mut frames = Vec::new();
    while let Some(frame) = machine.wss.drain_frame() {
        frames.push(frame);
    }
    assert_eq!(frames.len(), expected_frames);
    for (index, frame) in frames.into_iter().enumerate() {
        assert_eq!(frame, pattern[index % pattern.len()], "frame {index}");
    }
    assert!(
        machine.pic.irr_bit(11),
        "auto-init terminal count raised IRQ7"
    );
    assert!(
        !machine.wss.take_irq(),
        "the machine forwarded the IRQ edge"
    );
}

#[test]
fn wss_coexists_with_sb16_and_opl_without_cross_talk() {
    // With WSS enabled, the SB16 DSP + OPL must still function and there must
    // be no port/IRQ/DMA cross-talk: WSS uses base 0x530 / IRQ7 / DMA0, the
    // SB16 uses 0x220-0x22F / IRQ5 / DMA1, the OPL uses 0x388/9.
    let mut machine = test_machine();

    // SB16 8-bit mono playback on DMA ch1 (the SB default), exactly like the
    // standalone DSP golden, at byte base 0x02_0000.
    let bytes: Vec<u8> = (0..16).map(|i| (i * 16) as u8).collect();
    for (i, &b) in bytes.iter().enumerate() {
        machine.write_physical_u8(0x2_0000 + i as u32, b);
    }
    // A distinct WSS 8-bit mono buffer on DMA ch0 at byte base 0x01_0000:
    // a constant near-full-positive value so the WSS stream is unmistakable.
    for i in 0..16u32 {
        machine.write_physical_u8(0x1_0000 + i, 0xFF);
    }

    with_bus(&mut machine, |bus| {
        // --- SB16 DMA ch1 + DSP (IRQ5/DMA1, ports 0x220-0x22F) ---
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap(); // mode ch1: single, read
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x02, false).unwrap(); // ch1 page -> 0x02_0000
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap(); // unmask ch1
        for &b in &[0x41u8, 0x2B, 0x11, 0x14, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
        // OPL: key a full sustained tone on channel 0 (modulator + carrier +
        // key-on) so the OPL stream is genuinely audible, not just touched.
        program_tone(bus, 0x388, 0x389);

        // --- WSS DMA ch0 + codec (IRQ7/DMA0, ports 0x530-0x537) ---
        bus.write_io(0x0B, BusWidth::Byte, 0x48, false).unwrap(); // mode ch0: single, read
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap(); // ch0 page -> 0x01_0000
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap(); // unmask ch0
        // Codec: 8-bit unsigned PCM mono at 48000 Hz (I8 = CFS6 only -> 0x0C).
        bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08), false)
            .unwrap();
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x0C, false).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
            .unwrap();
        wss_write_indirect(bus, 10, 0x02); // IEN
        wss_write_indirect(bus, 15, 0x07); // count low
        wss_write_indirect(bus, 14, 0x00); // count high
        wss_write_indirect(bus, 9, 0x09); // PEN | ACAL
        wss_write_indirect(bus, 6, 0x00);
        wss_write_indirect(bus, 7, 0x00);
    });

    machine.advance_devices_clocks(200_000);

    // Both producers filled their own rings independently.
    let mut wss_frames = Vec::new();
    while let Some(f) = machine.wss.drain_frame() {
        wss_frames.push(f);
    }
    assert!(!wss_frames.is_empty(), "WSS still plays alongside the SB16");
    assert!(
        wss_frames.iter().all(|&(l, r)| l == r && l > 0),
        "WSS 8-bit unsigned 0xFF -> near-full-positive mono dup, undisturbed"
    );
    let dsp_out = machine.sb16.test_render_dsp_audio(16);
    assert_eq!(dsp_out.len(), 16, "SB16 DSP still plays its own buffer");

    // No IRQ cross-talk: WSS fired IRQ7, the SB16 fired its mixer-selected
    // IRQ5, and neither stepped on the other.
    assert!(machine.pic.irr_bit(11), "WSS raised IRQ11");
    assert!(machine.pic.irr_bit(7), "SB16 raised its own IRQ7 line");

    // No DMA cross-talk: the WSS drew from ch0, the SB16 from ch1; both single
    // channels reached terminal count on their own.
    assert_eq!(machine.dma_read_byte(0), None, "WSS ch0 reached TC");
    assert_eq!(machine.dma_read_byte(1), None, "SB16 ch1 reached TC");

    // The OPL really produces output (the keyed note is audible), not just a
    // non-empty render_audio that the speaker/WSS streams alone would satisfy.
    // Render the OPL in isolation and assert a non-zero sample magnitude.
    let opl_nonsilent = (0..512)
        .map(|_| machine.opl.render_sample())
        .any(|(l, r)| l != 0 || r != 0);
    assert!(
        opl_nonsilent,
        "OPL still synthesizes its keyed note alongside the WSS and SB16"
    );

    // The full mix is non-empty (OPL + SB16 + WSS all summed).
    let mixed = machine.render_audio(64);
    assert!(!mixed.is_empty());
}

#[test]
fn sb_dsp_auto_init_edges_forward_within_their_advance_and_rearm_after_ack() {
    // Multi-edge contract (see advance_devices): every block edge reaches
    // the PIC inside the advance whose sample tick produced it, and edges
    // that land while the previous request is still latched coalesce in the
    // 8259's IRR. The CPU does not execute during advance_devices, so the
    // guest cannot acknowledge between intra-step edges; the IRR absorbing
    // the extra requests is exactly the hardware's still-asserted-line
    // behavior, not a loss. After the guest-style ack sequence the NEXT
    // step's edges must raise the line again: nothing may stay parked in
    // the DSP's take_irq latch across steps.
    let mut machine = test_machine();
    // 16-byte auto-init DMA loop on ch1 feeding an 8-sample auto-init DSP
    // block: one 200k-clock advance at 22 MHz (~9 ms, ~99 frames at
    // 11025 Hz) spans many completed DSP blocks.
    for (i, b) in (0..16u8).map(|i| i * 16).enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        // PIC base 0x08 (IRQ5 -> vector 0x0D), all lines unmasked.
        bus.write_io(0x20, BusWidth::Byte, 0x11, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x08, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x00, false).unwrap();
        // DMA ch1: auto-init read of 16 bytes at 0x1_0000 (mode 0x59).
        bus.write_io(0x0B, BusWidth::Byte, 0x59, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        // DSP: rate 11025 Hz, block size 8, 8-bit auto-init output.
        for &b in &[0x41u8, 0x2B, 0x11, 0x48, 0x07, 0x00, 0x1C] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    machine.advance_devices_clocks(200_000);
    assert!(
        machine.sb16.test_is_playing(),
        "auto-init keeps the block looping"
    );
    assert!(
        machine.pic.irr_bit(7),
        "the block edges latched IRR5 within their own step"
    );
    assert!(
        !machine.sb16.test_take_irq(),
        "no edge stays parked in the DSP latch after the step"
    );
    // Guest ISR: PIC acknowledge, device ack (0x22E read), then EOI.
    with_bus(&mut machine, |bus| {
        assert_eq!(bus.acknowledge_interrupt(), Some(0x0F));
        bus.read_io(0x22E, BusWidth::Byte, 0, false).unwrap();
        bus.write_io(0x20, BusWidth::Byte, 0x20, false).unwrap(); // OCW2 EOI
    });
    assert!(!machine.pic.irr_bit(7), "IRR clear after the acknowledge");
    // Later edges arrive in a later advance and re-request the line: the
    // edge stream survives the ack instead of being lost with it.
    machine.advance_devices_clocks(200_000);
    assert!(
        machine.pic.irr_bit(7),
        "the next step's block edges re-request IRQ5"
    );
}

#[test]
fn wss_terminal_count_edge_forwards_within_its_advance() {
    // Same multi-edge contract as the DSP loop: the codec's terminal-count
    // edge is drained per output frame inside the producer loop, so it
    // reaches the PIC within the advance whose frame produced it and never
    // parks in the take_irq latch across steps.
    let mut machine = test_machine();
    for i in 0..64u32 {
        machine.write_physical_u8(0x1_0000 + i, 0x80);
    }
    with_bus(&mut machine, |bus| {
        wss_arm_8bit_mono(bus, 31); // TC after 32 frames (~0.67 ms at 48 kHz)
    });
    // One advance spanning the whole 32-frame window plus slack.
    machine.advance_devices_clocks(200_000);
    assert!(
        machine.pic.irr_bit(11),
        "the terminal-count edge latched IRR7 within the step"
    );
    assert!(
        !machine.wss.take_irq(),
        "no edge stays parked in the codec latch after the step"
    );
}

#[test]
fn pit_channel0_multi_edge_advance_latches_irq0() {
    // Channel 0 is the per-edge exemplar (a request per OUT rising edge,
    // issued inside the producer): several mode-2 periods inside ONE
    // advance coalesce in the IRR, and the request is present at step end.
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0x34, false).unwrap(); // ch0 mode 2
        bus.write_io(0x40, BusWidth::Byte, 100, false).unwrap();
        bus.write_io(0x40, BusWidth::Byte, 0, false).unwrap();
    });
    // ~325 PIT ticks at 22 MHz: three-plus full periods in one advance.
    machine.advance_devices_clocks(6_000);
    assert!(
        machine.pic.irr_bit(0),
        "channel-0 edges latched IRQ0 within the multi-period step"
    );
}

#[test]
fn event_batch_cap_reaches_a_near_due_pit_edge_in_every_mode() {
    for mode in [
        GswMode::Gsw586,
        GswMode::Gsw486,
        GswMode::Gsw386,
        GswMode::Gsw386Slow,
    ] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        // Audio is idle here, so the coarse fallback applies in every class;
        // the fine one is exercised by
        // `fine_batch_fallback_is_gated_on_an_active_audio_consumer`.
        let ceiling = mode.clock_hz() / 1000;

        // With channels 0 and 2 stopped, the mode-scaled fallback binds.
        assert_eq!(machine.event_batch_cap(u64::MAX), ceiling, "{mode:?}");
        assert_eq!(machine.event_batch_cap(123), 123, "{mode:?}");

        with_bus(&mut machine, |bus| {
            bus.write_io(0x43, BusWidth::Byte, 0x34, false).unwrap();
            bus.write_io(0x40, BusWidth::Byte, 0x08, false).unwrap();
            bus.write_io(0x40, BusWidth::Byte, 0x00, false).unwrap();
        });
        let pit_ticks = machine.pit.clocks_until_out_rise(0).unwrap();
        let due_ticks = timeline::RatePhase::default()
            .ticks_until(pit_ticks, u64::from(PIT_INPUT_HZ))
            .unwrap();
        machine.advance_devices_ticks(due_ticks - 1);

        let pit_ticks = machine.pit.clocks_until_out_rise(0).unwrap();
        let expected = machine
            .timeline
            .cpu_clocks_until(
                timeline::DeviceClock::Pit,
                pit_ticks,
                u64::from(PIT_INPUT_HZ),
            )
            .unwrap();
        assert_eq!(expected, 1, "{mode:?}: edge is one master tick away");
        assert_eq!(machine.event_batch_cap(u64::MAX), expected, "{mode:?}");
    }
}

/// Accurate-class (386) modes only: every other mode has no fine fallback.
const ACCURATE_MODES: [GswMode; 2] = [GswMode::Gsw386, GswMode::Gsw386Slow];

fn coarse_fallback(mode: GswMode) -> u64 {
    mode.clock_hz() / 1000
}

fn fine_fallback(mode: GswMode) -> u64 {
    mode.clock_hz() / u64::from(DAC_HZ)
}

/// Fire a Margo FILL through the MMIO aperture so STATUS.BUSY has modeled busy
/// time to drain.
fn margo_start_fill(machine: &mut Machine) {
    write_mmio_reg(machine, 0x100, 0); // DST_BASE
    write_mmio_reg(machine, 0x104, 640); // DST_PITCH
    write_mmio_reg(machine, 0x110, 1); // DEPTH
    write_mmio_reg(machine, 0x114, (2 << 16) | 3); // DST_XY
    write_mmio_reg(machine, 0x11c, (4 << 16) | 5); // DIM
    write_mmio_reg(machine, 0x120, 0xab); // FG_COLOR
    write_mmio_reg(machine, 0x128, 0xf0); // ROP: PATCOPY
    write_mmio_reg(machine, 0x150, 0x01); // COMMAND: FILL
    assert!(
        machine.vega.blitter_busy_ns() > 0,
        "the FILL must leave BUSY set"
    );
}

/// Start OPL timer 1 (one 80 us step), the AdLib detection probe's arming
/// sequence. Both writes go through the real port pair, so this is the same
/// state a guest reaches.
fn opl_start_timer1(machine: &mut Machine) {
    with_bus(machine, |bus| {
        bus.write_io(0x388, BusWidth::Byte, 0x02, false).unwrap(); // timer-1 preset
        bus.write_io(0x389, BusWidth::Byte, 0xFF, false).unwrap(); // one step
        bus.write_io(0x388, BusWidth::Byte, 0x04, false).unwrap(); // timer control
        bus.write_io(0x389, BusWidth::Byte, 0x01, false).unwrap(); // START timer 1
    });
}

/// Stop both OPL timers, then clear the overflow flags (the probe's
/// cleanup). Two writes, per the OPL3 reference: an IRQ-RESET write ignores
/// its other bits, so 0x80 alone clears the flags but stops nothing. Write
/// the mask-only byte first to stop the timers, then 0x80 to clear.
fn opl_stop_timers(machine: &mut Machine) {
    with_bus(machine, |bus| {
        bus.write_io(0x388, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x389, BusWidth::Byte, 0x60, false).unwrap(); // mask both, stop both
        bus.write_io(0x388, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x389, BusWidth::Byte, 0x80, false).unwrap(); // IRQ RESET
    });
}

fn write_port_0x61(machine: &mut Machine, value: u32) {
    with_bus(machine, |bus| {
        bus.write_io(0x61, BusWidth::Byte, value, false).unwrap();
    });
}

/// Arm 8-bit mono DSP DMA output at 11025 Hz for a 4096-frame block, so the
/// block IRQ deadline (~370 ms) is far past either fallback and cannot be what
/// binds the cap.
fn dsp_arm_long_8bit_block(machine: &mut Machine) {
    for index in 0..64u32 {
        machine.write_physical_u8(0x1_0000 + index, 0x80);
    }
    with_bus(machine, |bus| {
        bus.write_io(0x0b, BusWidth::Byte, 0x49, false).unwrap(); // DMA1 ch1: single, read
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0xff, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0f, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0a, BusWidth::Byte, 0x01, false).unwrap();
        for byte in [0x41u8, 0x2b, 0x11, 0xc0, 0x00, 0xff, 0x0f] {
            bus.write_io(0x22c, BusWidth::Byte, u32::from(byte), false)
                .unwrap();
        }
    });
}

/// Arm WSS playback with a block long enough that its terminal-count deadline
/// (4096 frames at 48 kHz, ~85 ms) cannot bind the cap either.
fn wss_arm_long_block(machine: &mut Machine) {
    for index in 0..64u32 {
        machine.write_physical_u8(0x1_0000 + index, 0x80);
    }
    with_bus(machine, |bus| {
        bus.write_io(0x0B, BusWidth::Byte, 0x48, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0xff, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x0f, false).unwrap();
        bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08), false)
            .unwrap();
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x0C, false).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
            .unwrap(); // clear MCE
        wss_write_indirect(bus, 10, 0x02); // I10 IEN
        wss_write_indirect(bus, 15, 0xff); // I15 lower count
        wss_write_indirect(bus, 14, 0x0f); // I14 upper count (loads current)
        wss_write_indirect(bus, 9, 0x01); // I9 PEN
        wss_write_indirect(bus, 6, 0x00);
        wss_write_indirect(bus, 7, 0x00);
    });
}

/// Mount a disc with one audio track and start it from the front panel, the
/// Red Book term in `fine_batch_grain_required`
/// (`self.ide.device().playback().playing`). No ATAPI packet command is
/// involved, so nothing here arms a storage deadline that could bind the cap.
fn cd_start_red_book_audio(machine: &mut Machine) {
    use crate::cdimage::{CdImage, DATA_SECTOR, RAW_SECTOR};
    let cue = "TRACK 01 MODE1/2048\nINDEX 01 00:00:00\n\
               TRACK 02 AUDIO\nINDEX 01 00:00:01\n";
    let mut bin = vec![0u8; DATA_SECTOR + 100 * RAW_SECTOR];
    for byte in bin[DATA_SECTOR..].iter_mut() {
        *byte = 0x20;
    }
    machine.mount_cd(CdImage::from_cue(cue, bin).unwrap());
    machine.cd_front_panel_play();
    assert!(
        machine.cd_audio_state().playing,
        "the front-panel play must leave the transport playing"
    );
}

#[test]
fn fine_batch_fallback_is_gated_on_an_active_audio_consumer() {
    // Both directions, per consumer, in the Accurate class: an idle machine gets
    // the 1 ms fallback; arming a consumer that can observe batch granularity
    // pulls the cap down to one DAC period. Every arming path below leaves its
    // own event deadline far outside 1 ms, so the DAC period is the ONLY thing
    // that can produce the fine value -- an ungated cap would fail the idle
    // assertions, and a gate stuck off would fail the armed ones.
    type ArmConsumer = fn(&mut Machine);
    let consumers: [(&str, ArmConsumer); 5] = [
        ("OPL timer 1 running", opl_start_timer1),
        ("speaker data enable", |machine| {
            write_port_0x61(machine, 0x02)
        }),
        ("DSP DMA playback", dsp_arm_long_8bit_block),
        ("WSS playback", wss_arm_long_block),
        ("Red Book CD audio playing", cd_start_red_book_audio),
    ];

    for mode in ACCURATE_MODES {
        let coarse = coarse_fallback(mode);
        let fine = fine_fallback(mode);
        assert!(
            fine < coarse,
            "{mode:?}: the two fallbacks must be distinguishable"
        );

        for (name, arm) in consumers {
            let mut machine = test_machine();
            machine.set_mode(mode);
            assert_eq!(
                machine.event_batch_cap(u64::MAX),
                coarse,
                "{mode:?}/{name}: idle audio must take the 1 ms fallback"
            );
            arm(&mut machine);
            assert_eq!(
                machine.event_batch_cap(u64::MAX),
                fine,
                "{mode:?}/{name}: an active consumer must take the DAC-period fallback"
            );
        }
    }
}

#[test]
fn fine_batch_fallback_is_released_when_the_consumer_goes_idle_again() {
    // The gate is live state, not a sticky first-touch admission: silencing the
    // consumer returns the cap to 1 ms within the same machine.
    for mode in ACCURATE_MODES {
        let coarse = coarse_fallback(mode);
        let fine = fine_fallback(mode);
        let mut machine = test_machine();
        machine.set_mode(mode);

        opl_start_timer1(&mut machine);
        assert_eq!(machine.event_batch_cap(u64::MAX), fine, "{mode:?}: armed");
        opl_stop_timers(&mut machine);
        assert_eq!(
            machine.event_batch_cap(u64::MAX),
            coarse,
            "{mode:?}: the probe finished, the fine cap is released"
        );

        write_port_0x61(&mut machine, 0x02);
        assert_eq!(
            machine.event_batch_cap(u64::MAX),
            fine,
            "{mode:?}: speaker driven"
        );
        write_port_0x61(&mut machine, 0x00);
        assert_eq!(
            machine.event_batch_cap(u64::MAX),
            coarse,
            "{mode:?}: speaker silenced"
        );
    }
}

#[test]
fn approximate_class_keeps_the_1ms_fallback_even_with_audio_active() {
    // The class test short-circuits ahead of the gate, so 486/586 never reach
    // the fine fallback however loud the machine is.
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        opl_start_timer1(&mut machine);
        write_port_0x61(&mut machine, 0x02);
        assert_eq!(
            machine.event_batch_cap(u64::MAX),
            coarse_fallback(mode),
            "{mode:?}"
        );
    }
}

/// Arm the DMA pusher over a zeroed ring with the doorbell rung, so
/// `Vega::pusher_has_queued_work` is true. The ring words are all zero: header
/// `method` 0, `count` 0, which consumes a word and writes nothing, so the pump
/// is exercised without any command side effect this test would have to model.
fn margo_arm_pusher_ring(machine: &mut Machine) {
    write_mmio_reg(machine, 0x84, 0x0001_0000); // PUSH_BASE (zeroed guest RAM)
    write_mmio_reg(machine, 0x88, 0x1000); // PUSH_SIZE
    write_mmio_reg(machine, 0x80, 1); // PUSH_CTRL = ENABLE
    write_mmio_reg(machine, 0x8c, 8); // PUSH_PUT = doorbell: two words queued
    assert!(
        machine.vega.pusher_has_queued_work(),
        "the ring must read as having queued work"
    );
}

#[test]
fn event_batch_cap_ends_the_batch_at_the_margo_blit_completion_edge_for_the_pusher() {
    // The blit deadline is armed ONLY while the DMA pusher has queued work, and
    // this is the first batch-level coverage section 9's pusher clause has had.
    //
    // `Vega::pump_pusher` runs at batch end and stalls on `busy_ns() == 0`, so
    // it consumes at most ONE command per batch. Section 9 promises PUSH_GET
    // "advances through the ring as it consumes commands"; without a batch
    // boundary at each modeled completion the ring would drain at batch cadence
    // instead -- a whole cap per ~740 ns operation. STATUS.BUSY does NOT need
    // the boundary (the analytic peek answers it at the read instant), which is
    // why the term is conditional rather than unconditional now.
    for mode in [
        GswMode::Gsw586,
        GswMode::Gsw486,
        GswMode::Gsw386,
        GswMode::Gsw386Slow,
    ] {
        let mut machine = test_machine();
        machine.set_mode(mode);

        // 1. The arming write does NOT end its own batch. It used to, and that
        //    break is what made the NEXT batch start with the engine busy,
        //    which is the only way the deadline below ever armed (the
        //    dependency is documented on `Machine::vega_edge_ticks`). Removing
        //    it was measured wall-neutral on current main.
        machine.io_touched = false;
        margo_start_fill(&mut machine);
        assert!(
            !machine.io_touched,
            "{mode:?}: the COMMAND write must NOT end the batch"
        );

        // 2. A 5x4 PATCOPY is 100 ns setup + 20 pixels * 5 ns = 200 ns, well
        //    inside every fallback, so only the deadline term can produce this
        //    value -- which makes the two arms below distinguishable.
        let busy_ns = machine.vega.blitter_busy_ns();
        assert_eq!(busy_ns, 200, "{mode:?}");
        let blit_edge = machine
            .timeline
            .cpu_clocks_until(timeline::DeviceClock::MargoNs, busy_ns, 1_000_000_000)
            .unwrap();
        assert!(
            blit_edge < mode.clock_hz() / u64::from(DAC_HZ),
            "{mode:?}: the blit edge must be shorter than either fallback"
        );

        // 3. With NO pusher work the batch is NOT cut at the completion edge.
        //    The guest can still see BUSY drop on time -- through the peek, not
        //    through the batch boundary.
        assert_ne!(
            machine.event_batch_cap(u64::MAX),
            blit_edge,
            "{mode:?}: an idle pusher must not arm the blit deadline"
        );
        assert!(
            machine.event_batch_cap(u64::MAX) > blit_edge,
            "{mode:?}: without the blit term the cap falls back to a longer edge"
        );

        // 4. Ring the doorbell and the deadline comes back, exactly.
        margo_arm_pusher_ring(&mut machine);
        assert_eq!(
            machine.event_batch_cap(u64::MAX),
            blit_edge,
            "{mode:?}: a pusher with queued work must arm the blit deadline"
        );

        // 5. Draining exactly that much device time clears BUSY, so the pump
        //    consumes its next command at the modeled cadence.
        machine.advance_devices_clocks(blit_edge);
        assert_eq!(
            machine.vega.blitter_busy_ns(),
            0,
            "{mode:?}: the batch ended at the completion edge"
        );
    }
}

#[test]
fn no_margo_write_ends_the_batch_while_a_long_blit_drains() {
    // No video-aperture write ends a batch, the arming one included. A blit can
    // outlast a batch -- this 640x480 PATCOPY models ~1.54 ms against a 1 ms
    // coarse cap -- and a guest that overlaps CPU rendering with one must keep
    // batching its framebuffer writes. This used to be an argument about the
    // arming break being EDGE- and not level-triggered; the break is gone, but
    // the edge survives it (`VideoWrite::ArmedBlit` now stamps the arm-time
    // drain credit), so the property it protects is pinned the same way: none of
    // these writes may end a batch or disturb the busy model.
    for mode in [
        GswMode::Gsw586,
        GswMode::Gsw486,
        GswMode::Gsw386,
        GswMode::Gsw386Slow,
    ] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
        write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
        write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
        write_mmio_reg(&mut machine, 0x114, 0); // DST_XY
        write_mmio_reg(&mut machine, 0x11c, (640 << 16) | 480); // DIM
        write_mmio_reg(&mut machine, 0x120, 0xab); // FG_COLOR
        write_mmio_reg(&mut machine, 0x128, 0xf0); // ROP: PATCOPY

        machine.io_touched = false;
        write_mmio_reg(&mut machine, 0x150, 0x01); // COMMAND: FILL
        assert!(
            !machine.io_touched,
            "{mode:?}: the arming write must not end its own batch"
        );
        let busy_ns = machine.vega.blitter_busy_ns();
        assert!(
            busy_ns > 1_000_000,
            "{mode:?}: this blit must outlast a coarse batch, modeled {busy_ns} ns"
        );

        // Everything below runs WHILE the engine is busy.
        machine.io_touched = false;
        machine.write_physical_u8(MARGO_LFB_BASE + 0x1234, 0x5a);
        assert!(
            !machine.io_touched,
            "{mode:?}: an LFB write during a blit must keep batching"
        );
        machine.write_physical_u8(0x000a_0000, 0x5a);
        assert!(
            !machine.io_touched,
            "{mode:?}: a legacy-aperture write during a blit must keep batching"
        );
        // Even an MMIO write: only one that MOVES busy time is an arming write.
        write_mmio_reg(&mut machine, 0x120, 0x17); // FG_COLOR, no command
        assert!(
            !machine.io_touched,
            "{mode:?}: a non-arming MMIO write during a blit must keep batching"
        );
        assert_eq!(
            machine.vega.blitter_busy_ns(),
            busy_ns,
            "{mode:?}: none of those writes may have touched the busy model"
        );
    }
}

#[test]
fn a_pit_counter_observer_holds_the_fine_batch_grain_on_the_accurate_class() {
    // A counter read reports the counting element as of BATCH START, so a guest
    // timing itself against the PIT is a batch-granularity consumer exactly like
    // the audio producers. Reading a counter port arms the window; letting the
    // window expire in guest time releases it.
    for mode in ACCURATE_MODES {
        let coarse = coarse_fallback(mode);
        let fine = fine_fallback(mode);
        let mut machine = test_machine();
        machine.set_mode(mode);
        assert_eq!(
            machine.event_batch_cap(u64::MAX),
            coarse,
            "{mode:?}: nothing has touched the PIT yet"
        );

        with_bus(&mut machine, |bus| {
            bus.read_io(0x40, BusWidth::Byte, 0, false).unwrap();
        });
        assert_eq!(
            machine.event_batch_cap(u64::MAX),
            fine,
            "{mode:?}: a counter read must pull the cap to the DAC period"
        );

        // The window is guest time, not a batch count: it survives an advance
        // shorter than itself and dies at one longer.
        machine.advance_devices_ticks(crate::timing::PIT_OBSERVER_FINE_WINDOW_TICKS / 2);
        assert_eq!(
            machine.event_batch_cap(u64::MAX),
            fine,
            "{mode:?}: still inside the observer window"
        );
        machine.advance_devices_ticks(crate::timing::PIT_OBSERVER_FINE_WINDOW_TICKS);
        assert_eq!(
            machine.event_batch_cap(u64::MAX),
            coarse,
            "{mode:?}: the observer window expired"
        );

        // A control-port write arms it too: that is the first touch of a
        // program-latch-compute-latch calibration.
        with_bus(&mut machine, |bus| {
            bus.write_io(0x43, BusWidth::Byte, 0x00, false).unwrap();
        });
        assert_eq!(
            machine.event_batch_cap(u64::MAX),
            fine,
            "{mode:?}: a latch command must arm the window"
        );
    }
}

#[test]
fn approximate_class_ignores_the_pit_observer_window() {
    // Same short-circuit as the audio terms: 486/586 never reach the fine
    // fallback, so a PIT-timing guest cannot change their batch length.
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        with_bus(&mut machine, |bus| {
            bus.read_io(0x40, BusWidth::Byte, 0, false).unwrap();
        });
        assert_eq!(
            machine.event_batch_cap(u64::MAX),
            coarse_fallback(mode),
            "{mode:?}"
        );
    }
}

#[test]
fn event_batch_cap_reaches_a_near_due_stereo_dsp_edge_in_every_mode() {
    for mode in [
        GswMode::Gsw586,
        GswMode::Gsw486,
        GswMode::Gsw386,
        GswMode::Gsw386Slow,
    ] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        with_bus(&mut machine, |bus| {
            // One 8-bit stereo frame reaches the block-completion edge.
            for &byte in &[0x41u8, 0x2B, 0x11, 0xC0, 0x20, 0x01, 0x00] {
                bus.write_io(0x22C, BusWidth::Byte, u32::from(byte), false)
                    .unwrap();
            }
        });
        assert_eq!(machine.sb16.test_frames_until_next_irq(), Some(1));
        let rate = u64::from(machine.sb16.test_output_frame_rate());
        let due_ticks = timeline::RatePhase::default().ticks_until(1, rate).unwrap();
        machine.advance_devices_ticks(due_ticks - 1);

        let expected = machine
            .timeline
            .cpu_clocks_until(timeline::DeviceClock::Dsp, 1, rate)
            .unwrap();
        assert_eq!(expected, 1, "{mode:?}: edge is one master tick away");
        assert_eq!(machine.event_batch_cap(u64::MAX), expected, "{mode:?}");
    }
}

#[test]
fn event_batch_cap_reaches_a_near_due_wss_edge_in_every_mode() {
    const WSS_FRAME_TICKS: u64 = izarravm_core::MASTER_CLOCK_HZ / 48_000;

    for mode in [
        GswMode::Gsw586,
        GswMode::Gsw486,
        GswMode::Gsw386,
        GswMode::Gsw386Slow,
    ] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        machine.write_physical_u8(0x1_0000, 0x80);
        with_bus(&mut machine, |bus| wss_arm_8bit_mono(bus, 1));

        // Retire the first frame and stop one master tick before the underflow.
        machine.advance_devices_ticks(2 * WSS_FRAME_TICKS - 1);
        assert_eq!(machine.wss.frames_until_next_irq(), Some(1));
        let expected = machine
            .timeline
            .cpu_clocks_until(timeline::DeviceClock::Wss, 1, 48_000)
            .unwrap();
        assert_eq!(expected, 1, "{mode:?}: edge is one master tick away");
        assert_eq!(machine.event_batch_cap(u64::MAX), expected, "{mode:?}");
    }
}

#[test]
fn approximate_class_delivers_pit_irq0_during_long_compute_stretches() {
    // In the Approximate class, a guest that computes for
    // many milliseconds without any port I/O still sees IRQ0 at the
    // programmed cadence. Each edge is requested by advance_devices in the
    // batch that spans it, the cap ends that batch at (about) the edge
    // instant, and the next batch entry services the interrupt.
    let code = [
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x00, 0x70, 0x00, 0x00, // mov word [0x7000], 0
        0xB0, 0x11, 0xE6, 0x20, // ICW1
        0xB0, 0x08, 0xE6, 0x21, // ICW2: vector base 0x08
        0xB0, 0x04, 0xE6, 0x21, // ICW3
        0xB0, 0x01, 0xE6, 0x21, // ICW4
        0xB0, 0x00, 0xE6, 0x21, // unmask all lines
        0xB0, 0x34, 0xE6, 0x43, // PIT ch0 mode 2, LSB/MSB
        0xB0, 0x00, 0xE6, 0x40, // reload low 0x00
        0xB0, 0x10, 0xE6, 0x40, // reload high 0x10 -> 4096 ticks (~3.43 ms)
        0xFB, // sti
        0xEB, 0xFE, // jmp $ (pure compute, no port I/O)
    ];
    let mut machine = Machine::new(
        MachineProfile::gsw_386(4, VideoCard::Vega),
        rom_with_code(&code),
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw586); // Approximate class, 166 MHz
    // IRQ0 handler at 0x0700: inc word [0x7000]; mov al,0x20; out 0x20,al; iret.
    let handler: [u8; 9] = [0xff, 0x06, 0x00, 0x70, 0xb0, 0x20, 0xe6, 0x20, 0xcf];
    for (i, &b) in handler.iter().enumerate() {
        machine.write_physical_u8(0x0700 + i as u32, b);
    }
    // IVT[0x08] (IRQ0 at PIC base 0x08) -> 0000:0700.
    machine.write_physical_u8(0x20, 0x00);
    machine.write_physical_u8(0x21, 0x07);
    machine.write_physical_u8(0x22, 0x00);
    machine.write_physical_u8(0x23, 0x00);
    // ~20 periods of 4096 PIT ticks at 166 MHz (~569.9k clocks each).
    machine.run_cycles(11_600_000).unwrap();
    let ticks = u16::from(machine.read_physical_u8(0x7000))
        | (u16::from(machine.read_physical_u8(0x7001)) << 8);
    assert!(
        (15..=22).contains(&ticks),
        "expected ~20 IRQ0 ticks across the compute stretch, saw {ticks}"
    );
}

#[test]
fn wss_irq7_wakes_a_halted_cpu_via_fast_forward() {
    // Mirror sb_dma_irq5_wakes_a_halted_cpu_via_fast_forward for the WSS wake
    // branch in next_device_wake: a guest arms WSS playback with IEN set and
    // IRQ7 unmasked, then sti;hlt. The run loop must fast-forward across the
    // codec's terminal-count window and deliver IRQ7 -- proving the wss_wake
    // estimator drives the machine, not just the wss.rs unit test.
    // WSS pinned to IRQ7 rather than taken from the default, which is now IRQ11.
    // This fixture's guest code programs the MASTER PIC and unmasks bit 7; IRQ11
    // lives on the slave, so following the default would mean rewriting the
    // cascade setup for no gain. The slave path already has its own coverage in
    // `wss_honors_a_configured_slave_irq11_end_to_end`.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.wss.irq = izarravm_core::WssIrq::I7;
    let mut machine = Machine::new(
        profile,
        // mov ax,0; mov ds,ax; sti; hlt; cli; hlt
        rom_with_code(&[0xb8, 0x00, 0x00, 0x8e, 0xd8, 0xfb, 0xf4, 0xfa, 0xf4]),
    )
    .unwrap();
    // 64-byte buffer at 0x01_0000 (DMA ch0 page 0x01) so the codec never
    // underruns before terminal count.
    for i in 0..64u32 {
        machine.write_physical_u8(0x1_0000 + i, 0x80);
    }
    // IRQ7 handler at 0x0700: inc word [0x0610]; mov al,0x20; out 0x20,al; iret.
    let handler: [u8; 9] = [0xff, 0x06, 0x10, 0x06, 0xb0, 0x20, 0xe6, 0x20, 0xcf];
    for (i, &b) in handler.iter().enumerate() {
        machine.write_physical_u8(0x0700 + i as u32, b);
    }
    // IVT[0x0F] (IRQ7 with PIC base 0x08) -> 0x0000:0x0700; clear the counter.
    machine.write_physical_u8(0x3C, 0x00);
    machine.write_physical_u8(0x3D, 0x07);
    machine.write_physical_u8(0x3E, 0x00);
    machine.write_physical_u8(0x3F, 0x00);
    machine.write_physical_u8(0x0610, 0x00);
    machine.write_physical_u8(0x0611, 0x00);
    with_bus(&mut machine, |bus| {
        // PIC base 0x08 so IRQ7 -> vector 0x0F; all IRQs unmasked.
        bus.write_io(0x20, BusWidth::Byte, 0x11, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x08, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x00, false).unwrap(); // unmask all
        // Base count 31 -> terminal count after 32 frames.
        wss_arm_8bit_mono(bus, 31);
    });
    let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let ticks = u16::from(machine.read_physical_u8(0x0610))
        | (u16::from(machine.read_physical_u8(0x0611)) << 8);
    assert!(ticks >= 1, "the WSS IRQ7 handler should have run");
    // The fast-forward crossed a real sample window (32 frames at 48 kHz ~=
    // 14.6k CPU clocks at 22 MHz), not a no-op halt.
    assert!(
        machine.elapsed_clocks() > 10_000,
        "the fast-forward should advance emulated time across the WSS window"
    );
}

#[test]
fn wss_honors_a_configured_slave_irq11_end_to_end() {
    // The default integration tests only exercise IRQ7 (master). Prove a machine
    // configured with a slave line (IRQ11) actually raises THAT line: wss_irq is
    // taken from profile.wss.irq.line() and fed to pic.request(wss_irq), so a
    // transposed or hardcoded line would route the terminal-count IRQ to the
    // wrong pin. Arm the codec and advance device time across its window; the
    // configured line must latch in the PIC's IRR and IRQ7 must stay clear.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.wss = WssConfig {
        irq: izarravm_core::WssIrq::I11,
        ..WssConfig::default()
    };
    let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();
    for i in 0..64u32 {
        machine.write_physical_u8(0x1_0000 + i, 0x80);
    }
    with_bus(&mut machine, |bus| {
        wss_arm_8bit_mono(bus, 31); // base count 31 -> TC after 32 frames
    });
    machine.advance_devices_clocks(200_000);
    assert!(
        machine.pic.irr_bit(11),
        "the codec raised its configured IRQ11 (slave line)"
    );
    assert!(
        !machine.pic.irr_bit(7),
        "the default IRQ7 was NOT raised when IRQ11 is configured"
    );
}

#[test]
fn wss_masked_irq7_does_not_wake_the_cpu() {
    // The IRQ-masked path: with IRQ7 masked, next_device_wake's wss branch must
    // be None (it gates on pic.deliverable), so sti;hlt is a genuine halt the
    // codec cannot wake. We mask EVERY line (IMR = 0xFF) so the WSS is the only
    // armed device and no other source can confound the wake -- the run loop
    // therefore halts at the first hlt and the handler never runs, even with
    // interrupts enabled. (The sticky Status INT bit is proven separately in
    // wss_masked_ien_clear_sets_sticky_status_without_a_pic_edge.)
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        // mov ax,0; mov ds,ax; sti; hlt; cli; hlt
        rom_with_code(&[0xb8, 0x00, 0x00, 0x8e, 0xd8, 0xfb, 0xf4, 0xfa, 0xf4]),
    )
    .unwrap();
    for i in 0..64u32 {
        machine.write_physical_u8(0x1_0000 + i, 0x80);
    }
    // IRQ7 handler that bumps a counter, so we can prove it never runs.
    let handler: [u8; 9] = [0xff, 0x06, 0x10, 0x06, 0xb0, 0x20, 0xe6, 0x20, 0xcf];
    for (i, &b) in handler.iter().enumerate() {
        machine.write_physical_u8(0x0700 + i as u32, b);
    }
    machine.write_physical_u8(0x3C, 0x00);
    machine.write_physical_u8(0x3D, 0x07);
    machine.write_physical_u8(0x0610, 0x00);
    machine.write_physical_u8(0x0611, 0x00);
    with_bus(&mut machine, |bus| {
        // PIC base 0x08, then mask ALL lines (IMR = 0xFF) so only the codec is
        // armed and nothing can wake the CPU.
        bus.write_io(0x20, BusWidth::Byte, 0x11, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x08, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0xFF, false).unwrap(); // mask every line
        wss_arm_8bit_mono(bus, 31);
    });
    // Run long enough that, were the WSS line a wake source, the codec would
    // fast-forward to terminal count and the CPU would advance well past the
    // window. A genuine halt makes no progress at the first hlt.
    let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let ticks = u16::from(machine.read_physical_u8(0x0610))
        | (u16::from(machine.read_physical_u8(0x0611)) << 8);
    assert_eq!(ticks, 0, "masked IRQ7 must not deliver the WSS interrupt");
    assert!(
        !machine.pic.irq_unmasked(7),
        "IRQ7 stayed masked for the duration"
    );
    // The codec did NOT wake the CPU: a masked WSS line is not a wake source,
    // so the run loop genuinely halted instead of fast-forwarding across the
    // ~32-frame codec window (which would have advanced emulated time by 10k+
    // clocks, as the unmasked twin test asserts).
    assert!(
        machine.elapsed_clocks() < 5_000,
        "a genuine halt does not fast-forward across the masked codec window"
    );
}

#[test]
fn wss_masked_ien_clear_sets_sticky_status_without_a_pic_edge() {
    // Underflow sets the codec's *internal* sticky Status INT bit regardless of
    // IEN (datasheet: the internal INT bit becomes one on counter underflow even
    // if IEN is zero), but the external INT *pin* -- and hence the PIC forward in
    // advance_devices -- is gated by IEN. Arm with IEN CLEAR and drive the codec
    // to terminal count directly (advance_devices, no CPU); the sticky bit must
    // be set while no edge ever reaches the PIC, proving the two are distinct.
    const R2_INT: u8 = 0x01;
    let mut machine = test_machine();
    for i in 0..64u32 {
        machine.write_physical_u8(0x1_0000 + i, 0x80);
    }
    with_bus(&mut machine, |bus| {
        // DMA ch0 for 32 bytes at 0x01_0000, 8-bit unsigned mono at 48 kHz, but
        // with IEN CLEAR (I10 = 0) so the underflow forwards no pin edge.
        bus.write_io(0x0B, BusWidth::Byte, 0x48, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x1F, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08), false)
            .unwrap();
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x0C, false).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
            .unwrap();
        wss_write_indirect(bus, 10, 0x00); // IEN CLEAR
        wss_write_indirect(bus, 15, 0x1F); // base count 31 -> TC after 32 frames
        wss_write_indirect(bus, 14, 0x00);
        wss_write_indirect(bus, 9, 0x09); // PEN | ACAL
        wss_write_indirect(bus, 6, 0x00);
        wss_write_indirect(bus, 7, 0x00);
    });
    // Advance device time across the full ~32-frame window at 48 kHz (~14.6k CPU
    // clocks at 22 MHz; use a generous budget so terminal count is reached).
    machine.advance_devices_clocks(200_000);
    assert_ne!(
        machine.wss.status() & R2_INT,
        0,
        "underflow sets the internal sticky Status INT bit even with IEN clear"
    );
    assert!(
        !machine.pic.irr_bit(11),
        "IEN clear forwards no pin edge, so the PIC line stays clear"
    );
}

#[test]
fn wss_disabled_leaves_its_ports_undecoded() {
    // With the codec disabled, its config/codec ports must NOT decode at all:
    // 0x530-0x537 is not in known_passive_ports(), so the bus must send both
    // reads and writes to open bus -- not to a stale latched value. Reading
    // 0xFF is not enough on its own to show that, since a decoded stub answers
    // 0xFF too, so each check also asks whether the port floated. Contrast with
    // the enabled machine, which
    // answers the I12 revision read with 0x0A, so the test proves the gate
    // toggles real decode rather than relying on an error either way.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.wss = WssConfig {
        enabled: false,
        ..WssConfig::default()
    };
    let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();
    with_bus(&mut machine, |bus| {
        // A write to the index port must not decode.
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x0C, false)
            .unwrap();
        assert!(
            bus.open_bus.floated(WSS_CODEC),
            "disabled WSS index write does not decode"
        );
        // A read of the data port (where an enabled codec would surface the
        // I12 revision) must not decode either.
        assert_eq!(bus.read_io(WSS_DATA, BusWidth::Byte, 0, false), Ok(0xff));
        assert!(
            bus.open_bus.floated(WSS_DATA),
            "disabled WSS data read does not decode"
        );
        // The window edges (base+7) are likewise undecoded.
        assert_eq!(bus.read_io(0x537, BusWidth::Byte, 0, false), Ok(0xff));
        assert!(
            bus.open_bus.floated(0x537),
            "disabled WSS upper window edge does not decode"
        );
    });

    // The same index-12 read on an ENABLED machine DOES decode to 0x0A, so the
    // disabled assertions above are a genuine contrast, not a vacuous pass.
    let mut enabled = test_machine();
    with_bus(&mut enabled, |bus| {
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x0C, false)
            .unwrap(); // select I12
        assert_eq!(
            bus.read_io(WSS_DATA, BusWidth::Byte, 0, false).unwrap(),
            0x0A,
            "enabled WSS answers the I12 revision read"
        );
    });
}

#[test]
fn wss_disabled_advance_and_render_run_cleanly_and_stay_silent() {
    // The disabled-codec branches in the producer loop (`if self.wss_enabled`)
    // and in render_audio (the `} else { Vec::new() }` WSS arm) are never reached
    // by the port-decode disabled test, which exits before any audio work. Run a
    // disabled machine through advance_devices AND render_audio to prove those
    // branches execute cleanly (no panic) and the WSS contributes silence. We
    // arm DMA/codec ports first to confirm a disabled codec ignores them entirely
    // -- but the ports do not decode, so the writes that would land on the codec
    // are skipped; only the DMA programming (separate decoder) is set up.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.wss = WssConfig {
        enabled: false,
        ..WssConfig::default()
    };
    let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();
    for i in 0..64u32 {
        machine.write_physical_u8(0x1_0000 + i, 0xFF);
    }
    with_bus(&mut machine, |bus| {
        // Program DMA ch0 (a separate decoder, still live) so the producer loop,
        // had it run the codec, would have data to read. The codec ports do not
        // decode while disabled, so we do not touch them here.
        bus.write_io(0x0B, BusWidth::Byte, 0x48, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x3F, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap();
    });
    // The disabled producer branch must be a no-op: this runs cleanly and queues
    // no codec frames.
    machine.advance_devices_clocks(200_000);
    assert!(
        machine.wss.drain_frame().is_none(),
        "a disabled codec renders no frames"
    );
    // The disabled render_audio arm (Vec::new()) must contribute nothing; with
    // OPL/DSP idle and the speaker silent the whole mix is silence.
    let mixed = machine.render_audio(64);
    assert!(
        mixed.iter().all(|&(l, r)| l == 0 && r == 0),
        "a disabled WSS adds nothing; idle OPL/DSP/speaker leave silence"
    );
}

#[test]
fn wss_stream_reaches_the_mixed_render_output_through_render_audio() {
    // Finding: the de-interleave smoke test pre-drains the ring before calling
    // render_audio, so the resampler + L/R summation path is never proven to
    // carry WSS audio. Here we arm an asymmetric stereo buffer, advance devices,
    // and call render_audio WITHOUT draining. With SB16 disabled, OPL idle, and
    // the speaker silent, the only possible signal is WSS, so the mixed output
    // must show the codec's L>0 / R<0 sign pattern. Disabling WSS for the same
    // buffer must then yield silence, proving the contribution came from the
    // WSS mix path and not from some other stream.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.sound_blaster.enabled = false;
    let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();
    load_asymmetric_stereo(&mut machine, 64);
    with_bus(&mut machine, |bus| wss_arm_16bit_stereo(bus, 64));
    machine.advance_devices_clocks(200_000);
    let mixed = machine.render_audio(64);
    assert!(
        mixed.iter().any(|&(l, r)| l > 0 && r < 0),
        "the WSS stream reaches the mixed L/R output with its asymmetric sign \
             pattern (left positive, right negative)"
    );

    // The identical buffer with WSS disabled produces silence: nothing else is
    // sounding, so the signal above was the WSS mix path, not a stray stream.
    let mut silent_profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    silent_profile.wss = WssConfig {
        enabled: false,
        ..WssConfig::default()
    };
    let mut silent = Machine::new(silent_profile, I386DX25_TEST_ROM).unwrap();
    load_asymmetric_stereo(&mut silent, 64);
    silent.advance_devices_clocks(200_000);
    let quiet = silent.render_audio(64);
    assert!(
        quiet.iter().all(|&(l, r)| l == 0 && r == 0),
        "with WSS disabled the same buffer mixes to silence"
    );
}

/// The FM leg of `Ct1745Mix` has to be exercised through `render_audio`, not just
/// through `SbMixer::fm_gain`.
///
/// A mixer-level test proves the register DECODES; it says nothing about whether
/// the decoded gain is ever applied to the OPL stream. Reverting
/// `Ct1745Mix::mix_opl_voice`'s FM multiply (passing `opl.0` straight through)
/// left the whole workspace green, because nothing drove real OPL output through
/// the mix with a non-default `0x34`/`0x35`. This does: one keyed OPL tone,
/// rendered twice through the real path, once at 0 dB and once at -24 dB.
#[test]
fn ct1745_fm_level_attenuates_the_opl_through_render_audio() {
    /// Peak absolute left-channel amplitude of a keyed OPL tone rendered through
    /// `render_audio` with `0x34`/`0x35` set to `level`.
    fn opl_peak_at_fm_level(level: u8) -> i32 {
        let mut machine = test_machine();
        with_bus(&mut machine, |bus| {
            // The FM/MIDI bus level is LEFT-aligned in D7-D3.
            for reg in [0x34u8, 0x35] {
                bus.write_io(0x224, BusWidth::Byte, u32::from(reg), false)
                    .unwrap();
                bus.write_io(0x225, BusWidth::Byte, u32::from(level << 3), false)
                    .unwrap();
            }
            program_tone(bus, 0x388, 0x389);
        });
        // The DSP is idle and the speaker/WSS/CD silent, so the mix is the OPL
        // tone with exactly the CT1745 gains on it. Master and output gain both
        // power on at 0 dB, so what is left is the FM leg.
        machine
            .render_audio(4_096)
            .iter()
            .map(|&(l, _)| i32::from(l).abs())
            .max()
            .unwrap()
    }

    let full = opl_peak_at_fm_level(31); // 0 dB
    let quiet = opl_peak_at_fm_level(19); // -24 dB
    assert!(full > 1_000, "the OPL tone must be audible at 0 dB: {full}");
    assert!(
        full < i32::from(i16::MAX),
        "the 0 dB reference must not clip, or the ratio is meaningless: {full}"
    );
    let ratio = quiet as f32 / full as f32;
    let expected = 10f32.powf(-24.0 / 20.0);
    assert!(
        (ratio - expected).abs() < 0.01,
        "level 19 is -24 dB on the FM bus: expected ratio {expected}, got {ratio} ({quiet}/{full})"
    );
    // And a hard-muted FM bus silences the OPL entirely -- proof the leg is the
    // only thing standing between the synthesiser and the DAC here.
    assert_eq!(opl_peak_at_fm_level(0), 0, "level 0 on 0x34/0x35 is a mute");
}

#[test]
fn wss_is_summed_raw_bypassing_the_ct1745_master_gain() {
    // Design contract: the WSS stream is summed into render_audio WITHOUT the
    // CT1745 master/voice/outgain scaling that OPL and the SB16 DSP take. Prove
    // it by HARD-MUTING the CT1745 master (level 0 = exactly 0.0 gain): an OPL
    // tone is silenced, but the WSS stream must still reach the output unchanged.
    let mut machine = test_machine();
    load_asymmetric_stereo(&mut machine, 64);
    with_bus(&mut machine, |bus| {
        // Hard-mute the CT1745 master (0x30/0x31 = 0) -- this scales OPL+DSP to
        // exactly zero but, per the contract, must NOT touch the WSS stream.
        bus.write_io(0x224, BusWidth::Byte, 0x30, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x224, BusWidth::Byte, 0x31, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x00, false).unwrap();
        // Key a loud OPL tone: with the master muted it must contribute zero.
        program_tone(bus, 0x388, 0x389);
        wss_arm_16bit_stereo(bus, 64);
    });
    machine.advance_devices_clocks(200_000);
    let mixed = machine.render_audio(64);
    assert!(
        mixed.iter().any(|&(l, r)| l > 0 && r < 0),
        "the WSS stream survives a hard-muted CT1745 master (summed raw)"
    );

    // Control: the SAME muted master with the OPL tone but WSS disabled yields
    // silence -- confirming the master mute really does zero the OPL/DSP path,
    // so the non-silence above is the raw WSS sum and not a leaking OPL tone.
    let mut control = test_machine();
    with_bus(&mut control, |bus| {
        bus.write_io(0x224, BusWidth::Byte, 0x30, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x224, BusWidth::Byte, 0x31, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x00, false).unwrap();
        program_tone(bus, 0x388, 0x389);
    });
    control.advance_devices_clocks(200_000);
    let muted = control.render_audio(64);
    assert!(
        muted.iter().all(|&(l, r)| l == 0 && r == 0),
        "a hard-muted master zeroes the OPL/DSP path (proving the mute is live)"
    );
}

#[test]
fn wss_autocal_window_drains_even_under_an_invalid_sample_rate() {
    // The autocal converter clock retires the ~128-sample ACI window regardless
    // of the programmed sample rate. If the producer only advanced the autocal
    // when rate_hz() > 0, a guest that cleared MCE while I8 selects one of the
    // two unsupported XTAL1 rates (rate_hz() == 0) would leave ACI asserted
    // forever. Arm an invalid rate (XTAL1 CFS4 -> 0 Hz), trigger autocal by an
    // MCE clear, and advance device time: ACI must retire on the fallback clock.
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        // Select an UNSUPPORTED rate: I8 = CFS4 (bits3:1 = 4 -> 0x08), CSS=0
        // (XTAL1). rate_hz() decodes this to 0. Set MCE to latch I8, then clear
        // MCE to assert ACI.
        bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08), false)
            .unwrap(); // MCE | index 8
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x08, false).unwrap(); // CFS4, XTAL1 -> 0 Hz
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
            .unwrap(); // clear MCE -> ACI
    });
    assert!(
        machine.wss.autocal_active(),
        "clearing MCE asserts the ACI autocal window"
    );
    assert_eq!(
        machine.wss.rate_hz(),
        0,
        "the selected rate is one of the unsupported (0 Hz) XTAL1 cells"
    );
    // Advance well past the 128-sample window at the 8000 Hz fallback cadence
    // (~16 ms; 200k clocks at 22 MHz is ~9 ms, so use a larger budget).
    machine.advance_devices_clocks(1_000_000);
    assert!(
        !machine.wss.autocal_active(),
        "the ACI window retires on the fallback converter clock despite rate 0"
    );
}

#[test]
fn wss_port_window_edges_and_config_region_decode_through_the_bus() {
    // Pin the wss_offset window math (`port.checked_sub(base).filter(|o| o < 8)`)
    // at its boundaries through the machine bus, plus the config-region readback
    // the decode comment promises does not overlap the SB16/mixer/OPL ranges:
    //   base+1 (0x531) -> the MSS presence signature 0x04,
    //   base+7 (0x537) -> decodes (Ok),
    //   base+8 (0x538) and base-1 (0x52F) -> nothing decodes them, so open bus.
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        assert_eq!(
            bus.read_io(0x531, BusWidth::Byte, 0, false).unwrap(),
            0x04,
            "config region reads the MSS presence signature"
        );
        assert!(
            bus.read_io(0x537, BusWidth::Byte, 0, false).is_ok(),
            "base+7 is the last decoded WSS port"
        );
        assert_eq!(bus.read_io(0x538, BusWidth::Byte, 0, false), Ok(0xff));
        assert!(
            bus.open_bus.floated(0x538),
            "base+8 is past the 8-port window"
        );
        assert_eq!(bus.read_io(0x52F, BusWidth::Byte, 0, false), Ok(0xff));
        assert!(bus.open_bus.floated(0x52F), "base-1 is below the window");
    });
}

/// The WSS codec must queue the frames a render window did not claim, exactly
/// as the SB16 voice does.
///
/// The codec's frame count comes from elapsed guest ticks (bursty) while the
/// mix window comes from the host-paced OPL resampler, so the two disagree
/// every call. `render_audio` reads the codec stream positionally, so anything
/// past the window used to be dropped on the floor and the next short window
/// covered with a repeated frame -- the same defect measured on the SB16 path,
/// where a Quake capture showed ~14k frames discarded and ~14k repeated per
/// second.
#[test]
fn wss_carries_frames_a_short_render_window_did_not_claim() {
    let mut machine = test_machine();
    // A rising ramp so queued frames are distinguishable from a held frame.
    for i in 0..256u32 {
        let sample = (i as u16).wrapping_mul(256);
        let [lo, hi] = sample.to_le_bytes();
        machine.write_physical_u8(0x1_0000 + i * 4, lo);
        machine.write_physical_u8(0x1_0000 + i * 4 + 1, hi);
        machine.write_physical_u8(0x1_0000 + i * 4 + 2, lo);
        machine.write_physical_u8(0x1_0000 + i * 4 + 3, hi);
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0B, BusWidth::Byte, 0x58, false).unwrap(); // ch0: auto-init, read
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0xFF, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x03, false).unwrap(); // 1024 bytes
        bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap();

        bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08), false)
            .unwrap();
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x5C, false).unwrap(); // 16-bit stereo 48 kHz
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
            .unwrap();
        wss_write_indirect(bus, 15, 0xFF);
        wss_write_indirect(bus, 14, 0x00);
        wss_write_indirect(bus, 9, 0x09); // PEN | ACAL
        wss_write_indirect(bus, 6, 0x00);
        wss_write_indirect(bus, 7, 0x00);
    });

    // Let the codec produce a healthy backlog of guest-clocked frames, then
    // claim only a tiny window of them.
    machine.advance_devices_clocks(200_000);
    assert!(machine.wss_pending.is_empty(), "nothing queued yet");
    let _ = machine.render_audio(16);

    let carried = machine.wss_pending.len();
    assert!(
        carried > 0,
        "frames past the window must be queued, not discarded"
    );

    // A second small window draws from the queue rather than re-deriving
    // everything, so the backlog falls instead of being thrown away and rebuilt.
    let _ = machine.render_audio(16);
    assert!(
        machine.wss_pending.len() < carried + 16,
        "the queue must be drained by later windows, not grow unbounded \
         (was {carried}, now {})",
        machine.wss_pending.len()
    );
}

#[test]
fn opl_diagnostics_classify_writes_reads_and_key_state() {
    // The counters exist to answer "did the guest strike any notes", so the
    // key-on/key-off split is the load-bearing part: 0xB0-0xB8 bit 5 is KEY ON,
    // and the same register with the bit clear is a release, not a note.
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        // Key ON channel 0.
        bus.write_io(0x0388, BusWidth::Byte, 0xb0, false).unwrap();
        bus.write_io(0x0389, BusWidth::Byte, 0x20, false).unwrap();
        // Key OFF the same channel: same register, bit 5 clear.
        bus.write_io(0x0388, BusWidth::Byte, 0xb0, false).unwrap();
        bus.write_io(0x0389, BusWidth::Byte, 0x00, false).unwrap();
        // Timer control, primary bank register 0x04.
        bus.write_io(0x0388, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x0389, BusWidth::Byte, 0x80, false).unwrap();
        // A status read.
        bus.read_io(0x0388, BusWidth::Byte, 0, false).unwrap();
    });

    let opl = machine.opl_diagnostics();
    assert_eq!(opl.key_on_writes, 1, "one keyed-on voice");
    assert_eq!(opl.key_off_writes, 1, "one released voice");
    assert_eq!(opl.timer_control_writes, 1);
    // Three DATA writes; the four address latches address no register and must
    // not be counted as register traffic.
    assert_eq!(opl.register_writes, 3);
    assert_eq!(opl.status_reads, 1);
}

#[test]
fn opl_diagnostics_count_the_sound_blaster_aliases_and_keep_the_guest_port() {
    // 0x220-0x223 mirror the OPL. A game driving the SB alias is doing OPL
    // traffic and must be counted, or a Sound Blaster title would look silent
    // to the counters while playing perfectly well.
    let mut machine = test_machine();
    machine.set_opl_trace_cap(8);
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0220, BusWidth::Byte, 0xb1, false).unwrap();
        bus.write_io(0x0221, BusWidth::Byte, 0x20, false).unwrap();
    });

    assert_eq!(machine.opl_diagnostics().key_on_writes, 1);
    let trace = machine.opl_trace();
    assert_eq!(trace.len(), 2);
    // The trace keeps the port the GUEST used, NOT the resolved 0x388/0x389,
    // so a reader can tell whether the game drove the AdLib ports or the SB
    // mirror. Resolving here would destroy the only evidence of which.
    assert_eq!(trace[0].port, 0x0220);
    assert_eq!(trace[1].port, 0x0221);
    assert_eq!(
        trace[0].register,
        OplTraceEntry::NO_REGISTER,
        "an address latch addresses no register"
    );
    assert_eq!(trace[1].register, 0xb1);
    assert_eq!(trace[1].value, 0x20);
}

#[test]
fn opl_trace_is_off_by_default_and_respects_its_cap() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0388, BusWidth::Byte, 0x04, false).unwrap();
    });
    assert!(
        machine.opl_trace().is_empty(),
        "tracing must stay off unless armed"
    );
    // Counters are always collected, tracing or not.
    assert_eq!(machine.opl_diagnostics().register_writes, 0);

    let mut capped = test_machine();
    capped.set_opl_trace_cap(3);
    with_bus(&mut capped, |bus| {
        for _ in 0..10 {
            bus.read_io(0x0388, BusWidth::Byte, 0, false).unwrap();
        }
    });
    assert_eq!(capped.opl_trace().len(), 3, "the cap bounds the trace");
    assert_eq!(
        capped.opl_diagnostics().status_reads,
        10,
        "counting must continue after the trace fills"
    );
}

/// Program the SB16 for 22050 Hz 16-bit signed stereo auto-init output on DMA5
/// -- what Duke Nukem 3D and its SETUP utility play through -- looping over an
/// 8-frame buffer of the given (left, right) sample pair. The CT1745 is left at
/// its power-on defaults: the point of these tests is what the DEFAULTS do.
fn play_16bit_stereo_pair(machine: &mut Machine, left: i16, right: i16) {
    let l = left.to_le_bytes();
    let r = right.to_le_bytes();
    let frame = [l[0], l[1], r[0], r[1]];
    for i in 0..8u32 {
        for (j, &b) in frame.iter().enumerate() {
            machine.write_physical_u8(0x2_0000 + i * 4 + j as u32, b);
        }
    }
    with_bus(machine, |bus| {
        // Slave ch5 (local ch1): word addr 0, page 0x02, count 15, auto-init read.
        bus.write_io(0xD6, BusWidth::Byte, 0x59, false).unwrap();
        bus.write_io(0xC4, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0xC4, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0xC6, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0xC6, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x8B, BusWidth::Byte, 0x02, false).unwrap();
        bus.write_io(0xD4, BusWidth::Byte, 0x01, false).unwrap();
        // DSP: 22050 Hz, 16-bit auto-init output, signed stereo, count 15.
        for &b in &[0x41u8, 0x56, 0x22, 0xB6, 0x30, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
}

/// Run the guest long enough to fill the DSP ring, then take the loudest
/// per-channel magnitude the host mix produces over a few render windows. The
/// first window is skipped so the voice resampler's startup ramp does not set
/// the reading.
fn host_mix_peaks(machine: &mut Machine) -> (i32, i32) {
    machine.advance_devices_clocks(2_000_000);
    machine.render_audio(512);
    let (mut peak_l, mut peak_r) = (0, 0);
    for _ in 0..4 {
        machine.advance_devices_clocks(500_000);
        for (l, r) in machine.render_audio(512) {
            peak_l = peak_l.max(i32::from(l).abs());
            peak_r = peak_r.max(i32::from(r).abs());
        }
    }
    (peak_l, peak_r)
}

/// A panned stereo source must still be panned when it reaches the host.
///
/// Duke Nukem 3D's SETUP left/right speaker test was reported as CENTERED but
/// quieter on both sides. Nothing in the voice path sums or averages the
/// channels -- the collapse is symmetric hard clipping. With enough gain ahead
/// of the final `clamp_i16`, BOTH channels rail at full scale no matter how far
/// apart they started, which is a centred image made of square waves: the
/// "peaked and muffled" half of the same bug report.
///
/// The second half of this test is the mutation proof: run the card's own
/// output stage at its maximum (+18 dB) and the very same 20 dB pan collapses
/// towards the centre. That is the shape of the bug the shipped 12.0x host gain
/// caused (the retired `amp_gain = 120`), reached through the only route left
/// now that the host knob is gone: the guest's `0x41`/`0x42` registers.
#[test]
fn the_default_output_stage_preserves_a_panned_stereo_source() {
    // A 20 dB pan: hard left with the right channel 10x down.
    const LEFT: i16 = 20_000;
    const RIGHT: i16 = 2_000;

    let mut machine = test_machine();
    play_16bit_stereo_pair(&mut machine, LEFT, RIGHT);
    let (peak_l, peak_r) = host_mix_peaks(&mut machine);
    assert!(
        peak_l > 1_000,
        "the panned source must reach the host mix at all (peak_l={peak_l})"
    );
    assert!(
        peak_r * 4 < peak_l,
        "a 20 dB pan must survive the default output stage as at least 4:1 \
         (peak_l={peak_l} peak_r={peak_r})"
    );

    // Mutation: put the card's output stage where the shipped 12.0x host gain
    // used to sit. That gain was calibrated when the CT1745 powered on at
    // master -14 dB AND voice -14 dB, so it compensated 28 dB of attenuation
    // that the volume-decode fix removed; the host knob is gone now, but the
    // guest can ask for the same abuse through the output-gain registers
    // (0x41/0x42, +18 dB), and the mix has to be judged the same way.
    let mut clipped = test_machine();
    set_mixer_register(&mut clipped, 0x41, 3 << 6);
    set_mixer_register(&mut clipped, 0x42, 3 << 6);
    play_16bit_stereo_pair(&mut clipped, LEFT, RIGHT);
    let (clip_l, clip_r) = host_mix_peaks(&mut clipped);
    assert_eq!(
        clip_l,
        i32::from(i16::MAX),
        "mutation proof: at +18 dB the loud channel rails at the clamp"
    );
    // The healthy image is at least twice as wide as the clipped one. Stated as
    // a RATIO against the same source rather than as an absolute, because how
    // far a clipped pan collapses depends on how much gain there is to clip
    // with, and the guest's own stage tops out at +18 dB where the host knob
    // went to 12.0x. The direction and the cause are the same.
    let healthy = f64::from(peak_l) / f64::from(peak_r.max(1));
    let squashed = f64::from(clip_l) / f64::from(clip_r.max(1));
    assert!(
        squashed * 2.0 < healthy,
        "mutation proof: the SAME 20 dB pan arrives as {peak_l}:{peak_r} \
         ({healthy:.1}:1) through the default stage and {clip_l}:{clip_r} \
         ({squashed:.1}:1) through a railed one -- clipping alone squashes the \
         image toward centre, which is the reported 'centered but quieter' \
         symptom. If this stops holding, the pan assertion above has stopped \
         proving anything."
    );
}

/// One full-scale source at power-on defaults must not reach the clamp.
///
/// A real SB16 does not clip its own full-scale DAC at its reset mixer
/// settings, and SNDMIXER.COM -- which owns amplification per the owner
/// directive -- needs somewhere to go UP to. Three dB is the floor.
#[test]
fn a_full_scale_voice_at_power_on_defaults_keeps_its_headroom() {
    let mut machine = test_machine();
    play_16bit_stereo_pair(&mut machine, i16::MAX, i16::MAX);
    let (peak_l, peak_r) = host_mix_peaks(&mut machine);
    let peak = peak_l.max(peak_r);
    assert!(
        peak > 1_000,
        "the full-scale source must reach the host mix at all (peak={peak})"
    );
    // 3 dB below full scale: 32767 * 10^(-3/20) = 23207.
    assert!(
        peak <= 23_207,
        "a single full-scale source at power-on defaults must leave at least \
         3 dB of headroom (peak={peak}, budget=23207)"
    );
}

/// A machine built with no sound card at all, for the legs whose routing
/// depends on whether there is a card in the path.
fn cardless_machine() -> Machine {
    Machine::new(
        MachineProfile {
            sound_blaster: izarravm_core::SoundBlasterConfig {
                enabled: false,
                ..Default::default()
            },
            ..MachineProfile::gsw_386(16, VideoCard::Vega)
        },
        I386DX25_TEST_ROM,
    )
    .expect("build a machine with no sound card")
}

/// Write one CT1745 mixer register through the guest's own port pair.
fn set_mixer_register(machine: &mut Machine, index: u8, value: u8) {
    with_bus(machine, |bus| {
        bus.write_io(0x224, BusWidth::Byte, u32::from(index), false)
            .unwrap();
        bus.write_io(0x225, BusWidth::Byte, u32::from(value), false)
            .unwrap();
    });
}

/// Program PIT channel 2 as a square-wave generator and open both speaker
/// gates, which is exactly what a DOS beep does: mode 3, a divisor around
/// 1 kHz, then `0x61` bits 0 (gate) and 1 (data enable).
fn beeper_on(machine: &mut Machine) {
    with_bus(machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0xB6, false).unwrap(); // ch2, lo/hi, mode 3
        bus.write_io(0x42, BusWidth::Byte, 0x97, false).unwrap(); // 1193 -> ~1 kHz
        bus.write_io(0x42, BusWidth::Byte, 0x04, false).unwrap();
    });
    write_port_0x61(machine, 0x03);
}

/// The loudest magnitude the host mix reaches with only the beeper running.
/// The speaker ring is filled by the device clock, so the machine is advanced
/// first and the opening window is skipped for the same reason
/// `host_mix_peaks` skips one.
fn speaker_peak(machine: &mut Machine) -> i32 {
    machine.advance_devices_clocks(2_000_000);
    machine.render_audio(512);
    let mut peak = 0;
    for _ in 0..4 {
        machine.advance_devices_clocks(500_000);
        for (left, right) in machine.render_audio(512) {
            peak = peak.max(i32::from(left).abs()).max(i32::from(right).abs());
        }
    }
    peak
}

/// CT1745 register `0x3B` attenuates the PC speaker, in the four steps the
/// hardware has and no more.
///
/// This register used to be a pure store: a guest could write any of its four
/// positions and the beeper never moved. The consequence was not only a missing
/// control -- it was a level error, because the beeper was also summed AFTER
/// the node where every card leg gives up 6 dB of headroom, so it arrived
/// louder than a full-scale card source instead of quieter.
///
/// The ratios asserted here are the register's own: -14, -7 and 0 dB relative
/// to each other, measured through the whole mix rather than at the decode.
#[test]
fn pc_speaker_mixer_level_attenuates_the_beeper_through_render_audio() {
    let peak_at = |position: u8| {
        let mut machine = test_machine();
        set_mixer_register(&mut machine, 0x3B, position << 6);
        beeper_on(&mut machine);
        speaker_peak(&mut machine)
    };

    let full = peak_at(3);
    let mid = peak_at(2);
    let low = peak_at(1);
    let muted = peak_at(0);

    assert!(full > 1_000, "the beeper must be audible at position 3");
    assert_eq!(muted, 0, "position 0 is a hard mute, not a quiet setting");
    // Each position against the one above it, to the register's own dB figures.
    // A 6% window absorbs the square wave's box-averaging into the DAC rate;
    // the steps are 7 dB apart, so nothing here could pass by accident.
    for (louder, quieter, db) in [(full, mid, 7.0f32), (mid, low, 7.0)] {
        let want = louder as f32 * 10f32.powf(-db / 20.0);
        assert!(
            (quieter as f32 - want).abs() < want * 0.06,
            "one PC-SPK position is {db} dB: {louder} -> {quieter}, wanted about {want}"
        );
    }

    // The routing, not just the ratio: the beeper now passes the CT1745 master
    // as well, which is what makes the MASTER fader mean "everything".
    let mut machine = test_machine();
    set_mixer_register(&mut machine, 0x3B, 3 << 6);
    set_mixer_register(&mut machine, 0x30, 0x00);
    set_mixer_register(&mut machine, 0x31, 0x00);
    beeper_on(&mut machine);
    assert_eq!(
        speaker_peak(&mut machine),
        0,
        "a muted master must silence the speaker leg too"
    );
}

/// Where the beeper joins, both ways round.
///
/// On a machine built with NO sound card there is no PC-SPK input to pass the
/// beeper through, so it must take neither the card's summing-node headroom nor
/// anything else the card does. On a machine WITH one it is a card leg and
/// takes the node's reserve like every other leg.
///
/// That reserve is what pins the PLACEMENT, which is the half of this routing
/// the level tests above cannot see: the PC-SPK register would decode
/// identically whether its output joined the mix before or after the node, and
/// `MIX_HEADROOM` is the node's remaining scalar. A beeper added AFTER the node
/// -- which is what it used to be -- reaches the output at the cardless level
/// on both machines, and the comparison below fails by exactly 6 dB.
#[test]
fn a_machine_with_no_card_keeps_its_beeper_outside_the_card_stage() {
    let mut cardless = cardless_machine();
    beeper_on(&mut cardless);
    let unity = speaker_peak(&mut cardless);
    assert!(unity > 1_000, "the beeper must be audible with no card");

    // Compared against the SAME beeper on a carded machine with the PC-SPK
    // level at 0 dB, which differs from this one by exactly the node's reserve.
    let mut carded = test_machine();
    set_mixer_register(&mut carded, 0x3B, 3 << 6);
    beeper_on(&mut carded);
    let through_card = speaker_peak(&mut carded) as f32;
    let want = unity as f32 * crate::MIX_HEADROOM;
    assert!(
        (through_card - want).abs() < want * 0.06,
        "the card path costs the node's reserve and the cardless path does not:          {unity} -> {through_card}, wanted about {want}"
    );
}

/// The guest side of the MIDI volume hook: the card's wavetable register pair
/// is what `Machine::midi_gain` reports, so the frontend has one place to read
/// it from and the value is a function of canonical device state rather than of
/// anything the host remembers separately.
///
/// The register pair is the ReSonique II's own extension. A real CT1745's
/// "MIDI" registers (`0x34`/`0x35`) are the FM bus, and this asserts they stay
/// that way: moving the wavetable must not move the OPL's level, or the tool's
/// FMSYNTH and MIDI faders would be two handles on one leg.
#[test]
fn the_wavetable_registers_drive_the_midi_gain_and_not_the_fm_bus() {
    let mut machine = test_machine();
    assert_eq!(
        machine.midi_gain(),
        (1.0, 1.0),
        "the wavetable leg powers on where it already was: unity"
    );

    set_mixer_register(&mut machine, 0x50, 21 << 3); // -20 dB
    set_mixer_register(&mut machine, 0x51, 0x01); // D0: the deliberate mute
    let (left, right) = machine.midi_gain();
    assert!(
        (left - 10f32.powf(-20.0 / 20.0)).abs() < 1e-4,
        "0x50 level 21 is -20 dB, got {left}"
    );
    assert_eq!(right, 0.0, "0x51 D0 is the mute bit");

    // The FM bus is untouched by all of that, and the read-back is the level
    // the guest wrote (D7-D3), not a bare number.
    assert_eq!(machine.sb_mixer_register(0x34), Some(31 << 3));
    assert_eq!(machine.sb_mixer_register(0x50), Some(21 << 3));
    assert_eq!(machine.sb_mixer_register(0x51), Some(0x01));

    // And the converse: the FM registers do not reach the MIDI leg.
    set_mixer_register(&mut machine, 0x34, 0x00);
    set_mixer_register(&mut machine, 0x35, 0x00);
    let (left, _) = machine.midi_gain();
    assert!(
        (left - 10f32.powf(-20.0 / 20.0)).abs() < 1e-4,
        "muting the OPL must not touch the wavetable leg, got {left}"
    );

    // A machine with no card carries no wavetable register, so the leg passes
    // at unity rather than being silenced by a control that is not there.
    let bare = cardless_machine();
    assert_eq!(bare.midi_gain(), (1.0, 1.0));
    assert_eq!(bare.sb_mixer_register(0x50), None);
}

/// A guest that clears a block of mixer registers writes `0x00` to the
/// wavetable pair as collateral. That must not silence the machine's MIDI:
/// the leg has no other control anywhere, so the silence would be both
/// unexplainable and unrecoverable from inside the guest, and
/// `gui_session::pump_audio` hands this one gain to BOTH engines, so it would
/// take the external MIDI receiver down with the wavetable.
///
/// The mutation this guards is the obvious implementation -- decoding the pair
/// with the same `VOL5_STEPS[level]` every other register uses, where level 0
/// is 0.0.
#[test]
fn a_stray_zero_to_the_wavetable_registers_attenuates_rather_than_muting() {
    let mut machine = test_machine();
    // The block clear: 0x30 upward, which is what the stray write looks like.
    for index in 0x30u8..=0x51 {
        set_mixer_register(&mut machine, index, 0x00);
    }
    let (left, right) = machine.midi_gain();
    let floor = 10f32.powf(-36.0 / 20.0);
    assert!(
        (left - floor).abs() < 1e-5 && (right - floor).abs() < 1e-5,
        "a cleared wavetable pair is -36 dB, not silence: {left}, {right}"
    );
    // Every other leg in that same sweep IS muted, which is what makes the
    // wavetable's exception an exception rather than a decode that never mutes.
    assert_eq!(machine.sb_mixer_register(0x32), Some(0x00));
    assert_eq!(machine.sb_mixer_register(0x3B), Some(0x00));

    // And the deliberate mute still reaches silence, so the floor costs no
    // capability: SNDMIXER.COM's step 0 writes D0.
    set_mixer_register(&mut machine, 0x50, 0x01);
    set_mixer_register(&mut machine, 0x51, 0x01);
    assert_eq!(machine.midi_gain(), (0.0, 0.0));
}
