// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The CMOS block SNDCTRL.COM owns (0x1B-0x21) and the host side of applying it.
//!
//! The tool itself is a guest binary; what is tested here is the contract it
//! writes against: which bytes carry what, that the block round-trips through a
//! saved cmos.bin onto the live devices, and that a block the card could not
//! have written is refused rather than routing the card somewhere it cannot
//! answer.

use super::*;
use izarravm_core::{SbDma8, SbDma16, SbIrq, WssIrq};

fn machine() -> Machine {
    Machine::new(
        MachineProfile::gsw_386(16, izarravm_core::VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine")
}

/// The block is stamped at construction, not left at zero, so a machine that
/// has never run the tool still presents something valid to read back.
#[test]
fn construction_seeds_the_audio_block_from_the_profile() {
    let machine = machine();
    let cmos = machine.cmos_bytes();
    assert_eq!(cmos[0x1B], b'R', "magic byte");
    assert_eq!(cmos[0x1C], 7, "SB IRQ defaults to 7");
    assert_eq!(cmos[0x1D], 1, "SB 8-bit DMA");
    assert_eq!(cmos[0x1E], 5, "SB 16-bit DMA");
    assert_eq!(cmos[0x1F], 11, "WSS IRQ defaults to 11");
    assert_eq!(cmos[0x20], 0, "WSS DMA");
    assert_eq!(cmos[0x21], 0, "MPU port selector: 0 = 0x300");
}

/// The whole point of persisting it: a cmos.bin carrying the tool's choice has
/// to reach the devices on the next boot, because both are built from the
/// profile before any NVRAM is read.
#[test]
fn a_saved_block_repoints_both_devices_on_load() {
    let mut source = machine();
    let mut cmos = source.cmos_bytes();
    cmos[0x1C] = 5; // SB IRQ 5
    cmos[0x1D] = 3; // SB DMA 3
    cmos[0x1E] = 7; // SB DMA16 7
    cmos[0x1F] = 9; // WSS IRQ 9
    cmos[0x20] = 1; // WSS DMA 1
    cmos[0x21] = 1; // MPU 0x330
    refresh_checksum(&mut cmos);
    assert!(source.load_cmos(&cmos), "checksummed image is accepted");

    assert_eq!(source.sound_blaster_routing(), Some((5, 3, 7)));
    assert_eq!(source.wss_routing(), Some((9, 1)));
    assert_eq!(source.cmos_mpu_port(), 0x330);
}

/// The profile follows the block too, not just the devices. Everything that
/// *describes* the card -- the BLASTER line written into an emulator-owned
/// AUTOEXEC.BAT, the environment injected on the HLE path -- is derived from
/// the profile, and a profile left on the old value is how the card ends up
/// answering on one IRQ while BLASTER advertises another.
#[test]
fn a_saved_block_also_moves_the_profile_the_blaster_line_comes_from() {
    let mut machine = machine();
    let mut cmos = machine.cmos_bytes();
    cmos[0x1C] = 5;
    cmos[0x21] = 1;
    refresh_checksum(&mut cmos);
    machine.load_cmos(&cmos);

    let entries =
        sound_blaster_env_entries(&machine.profile.sound_blaster, machine.cmos_mpu_port());
    assert_eq!(
        entries[0],
        ("BLASTER".to_string(), "A220 I5 D1 H5 P330 T6".to_string()),
        "BLASTER follows the CMOS block, IRQ and MPU port alike"
    );
}

/// Nothing stops a guest from writing arbitrary bytes into NVRAM, so a block
/// naming a line the card cannot route to is not partially applied: the whole
/// block is one setting and a bad byte reseeds all of it.
#[test]
fn a_block_the_card_could_not_have_written_is_reseeded() {
    for (index, bad) in [
        (0x1C, 3),  // SB cannot route to IRQ 3
        (0x1D, 2),  // nor to DMA 2
        (0x1E, 1),  // 16-bit DMA is a slave channel
        (0x1F, 5),  // the codec's lines are 7/9/10/11
        (0x20, 15), // not a channel at all
    ] {
        let mut machine = machine();
        let mut cmos = machine.cmos_bytes();
        cmos[index] = bad;
        refresh_checksum(&mut cmos);
        machine.load_cmos(&cmos);
        assert_eq!(
            machine.sound_blaster_routing(),
            Some((7, 1, 5)),
            "byte {index:#04x} = {bad} must fall back to the profile"
        );
        assert_eq!(machine.wss_routing(), Some((11, 0)));
        assert_eq!(
            machine.cmos_bytes()[index],
            machine_default_for(index),
            "the rejected block is rewritten, not left to be re-read next boot"
        );
    }
}

/// The same two collisions izarravm.conf refuses. The tool's menus make these
/// unreachable and its command line rejects them, but a hand-edited cmos.bin
/// is not bound by either.
#[test]
fn a_block_with_the_two_devices_on_one_line_is_reseeded() {
    for (index, value) in [(0x1F, 7u8), (0x20, 1u8)] {
        let mut machine = machine();
        let mut cmos = machine.cmos_bytes();
        // Put the codec on whatever the Sound Blaster already holds.
        cmos[if index == 0x1F { 0x1C } else { 0x1D }] = value;
        cmos[index] = value;
        refresh_checksum(&mut cmos);
        machine.load_cmos(&cmos);
        assert_eq!(
            machine.wss_routing(),
            Some((11, 0)),
            "a shared resource at {index:#04x} must not be applied"
        );
    }
}

/// A cmos.bin written before this block existed carries zeros there. Zero is a
/// legal DMA channel, so only the magic byte distinguishes "never configured"
/// from "configured to zero" -- without it, an old image would silently route
/// the card to channel 0.
#[test]
fn an_image_predating_the_block_is_reseeded_rather_than_believed() {
    let mut machine = machine();
    let mut cmos = machine.cmos_bytes();
    cmos[0x1B..=0x21].fill(0);
    refresh_checksum(&mut cmos);
    machine.load_cmos(&cmos);
    assert_eq!(machine.sound_blaster_routing(), Some((7, 1, 5)));
    assert_eq!(machine.wss_routing(), Some((11, 0)));
    assert_eq!(machine.cmos_bytes()[0x1B], b'R', "the block is re-stamped");
}

/// The MPU port is the one setting with no hardware register behind it: both
/// ports stay decoded whatever it says, and it only decides which one BLASTER
/// advertises. It still has to reach the AUTOEXEC line.
#[test]
fn the_mpu_port_selector_reaches_the_stock_autoexec_line() {
    let base = b"@ECHO OFF\r\nSET BLASTER=A220 I7 D1 H5 P300 T6\r\nLH TOKAMOUS\r\n";
    let rewritten = crate::storage::stock_autoexec(base, &SoundBlasterConfig::default(), 0x330);
    assert_eq!(
        rewritten,
        b"@ECHO OFF\r\nSET BLASTER=A220 I7 D1 H5 P330 T6\r\nLH TOKAMOUS\r\n"
    );
}

/// SNDCTRL.COM edits the SET line in place. The result must still be
/// recognised as a file the emulator owns, or the tool's own edit would demote
/// a stock AUTOEXEC to user-owned and the host would stop keeping it in step.
#[test]
fn an_autoexec_the_tool_rewrote_is_still_emulator_stock() {
    let dir = std::env::temp_dir().join(format!("sndctrl_stock_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let payload = katea_volume::extract_system_payload(izarravm_firmware::tokados_hdd_img());
    let base = payload_file(&payload, "AUTOEXEC.BAT");

    // Every routing the tool can produce, including both MPU ports.
    let moved = SoundBlasterConfig {
        irq: SbIrq::I10,
        dma: SbDma8::D3,
        high_dma: SbDma16::D7,
        ..SoundBlasterConfig::default()
    };
    let tool_output = crate::storage::stock_autoexec(&base, &moved, 0x330);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("AUTOEXEC.BAT"), &tool_output).unwrap();

    // Mounting again with the profile the tool persisted must leave it alone
    // in content terms -- it is regenerated, but to the same bytes.
    crate::storage::ensure_user_config(&dir, b"FILES=40\r\n", &base, &moved, 0x330).unwrap();
    assert_eq!(
        std::fs::read(dir.join("AUTOEXEC.BAT")).unwrap(),
        tool_output,
        "the rewritten line must survive the next mount"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Both enums have to round-trip, because the block stores raw line and channel
/// numbers rather than the discriminants.
#[test]
fn resource_enums_round_trip_through_their_raw_numbers() {
    for irq in [SbIrq::I2, SbIrq::I5, SbIrq::I7, SbIrq::I10] {
        assert_eq!(SbIrq::from_line(irq.line()), Some(irq));
    }
    for irq in [WssIrq::I7, WssIrq::I9, WssIrq::I10, WssIrq::I11] {
        assert_eq!(WssIrq::from_line(irq.line()), Some(irq));
    }
    for dma in [SbDma8::D0, SbDma8::D1, SbDma8::D3] {
        assert_eq!(SbDma8::from_channel(dma.channel()), Some(dma));
    }
    for dma in [SbDma16::D5, SbDma16::D6, SbDma16::D7] {
        assert_eq!(SbDma16::from_channel(dma.channel()), Some(dma));
    }
    assert_eq!(
        SbIrq::from_line(11),
        None,
        "11 is a codec line, not an SB one"
    );
    assert_eq!(
        WssIrq::from_line(5),
        None,
        "5 is an SB line, not a codec one"
    );
}

fn machine_default_for(index: usize) -> u8 {
    match index {
        0x1C => 7,
        0x1D => 1,
        0x1E => 5,
        0x1F => 11,
        0x20 => 0,
        _ => unreachable!(),
    }
}

/// The stored checksum covers 0x10..=0x2D; an image with a stale one is
/// discarded wholesale, which would mask everything these tests assert.
fn refresh_checksum(cmos: &mut [u8; 64]) {
    let sum = cmos[0x10..=0x2D]
        .iter()
        .fold(0u16, |acc, b| acc.wrapping_add(u16::from(*b)));
    cmos[0x2E] = (sum >> 8) as u8;
    cmos[0x2F] = sum as u8;
}
