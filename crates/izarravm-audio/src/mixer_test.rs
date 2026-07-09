// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Program one mixer register via the `0x224`/`0x225` protocol.
fn write_reg(mixer: &mut SbMixer, index: u8, value: u8) {
    mixer.write_port(MIXER_INDEX_PORT, index);
    mixer.write_port(MIXER_DATA_PORT, value);
}

/// Read one mixer register via the `0x224`/`0x225` protocol.
fn read_reg(mixer: &mut SbMixer, index: u8) -> u8 {
    mixer.write_port(MIXER_INDEX_PORT, index);
    mixer.read_port(MIXER_DATA_PORT).unwrap()
}

#[test]
fn mixer_claims_only_its_two_ports() {
    let mut mixer = SbMixer::default();
    assert!(mixer.write_port(MIXER_INDEX_PORT, 0x80));
    assert!(mixer.write_port(MIXER_DATA_PORT, 0x04));
    assert!(
        !mixer.write_port(0x226, 0x01),
        "DSP reset is not a mixer port"
    );
    assert_eq!(mixer.read_port(MIXER_DATA_PORT), Some(0x04));
    assert!(mixer.read_port(0x226).is_none());
}

#[test]
fn index_latch_and_data_round_trip() {
    // out 0x224,0x80; out 0x225,0x04; in 0x225 -> 0x04 (0x80 is IRQ setup).
    let mut mixer = SbMixer::default();
    write_reg(&mut mixer, 0x80, 0x04);
    assert_eq!(read_reg(&mut mixer, 0x80), 0x04);
}

#[test]
fn cd_volume_attenuates_via_both_register_paths() {
    let mut mixer = SbMixer::default();
    // Default CD volume is muted.
    assert_eq!(mixer.cd_gain(), (0.0, 0.0));
    // The 5-bit CD registers set the gain directly.
    write_reg(&mut mixer, 0x36, 31);
    write_reg(&mut mixer, 0x37, 31);
    let (l, r) = mixer.cd_gain();
    assert!(l > 0.9 && r > 0.9, "full CD volume is near unity: {l},{r}");
    // The CT1345 compat alias maps into the same 5-bit registers. The 4-bit
    // max nibble maps to 5-bit level 30 (level<<1), ~0.79 gain, well above
    // the muted floor.
    let mut compat = SbMixer::default();
    write_reg(&mut compat, 0x28, 0xFF); // both nibbles max
    let (cl, cr) = compat.cd_gain();
    assert!(cl > 0.5 && cr > 0.5, "compat CD volume is loud: {cl},{cr}");
    // A read of 0x28 round-trips the compat byte.
    assert_eq!(read_reg(&mut compat, 0x28), 0xFF);
}

#[test]
fn irq_decode_maps_each_bit_and_picks_the_lowest_set() {
    let mut mixer = SbMixer::default();
    for (byte, irq) in [(0x01u8, 2u8), (0x02, 5), (0x04, 7), (0x08, 10)] {
        write_reg(&mut mixer, 0x80, byte);
        assert_eq!(mixer.selected_irq(), irq, "0x80={:#04x}", byte);
    }
    // Multiple bits set: lowest set bit wins (here D1 => IRQ5 over D2/D3).
    write_reg(&mut mixer, 0x80, 0x0E); // D1 | D2 | D3
    assert_eq!(mixer.selected_irq(), 5);
    // No valid bit set: keep the hardware default IRQ5.
    write_reg(&mut mixer, 0x80, 0x00);
    assert_eq!(mixer.selected_irq(), 5);
}

#[test]
fn dma_decode_picks_the_lowest_set_bit_per_group() {
    let mut mixer = SbMixer::default();
    // Hardware default: DMA1 | DMA5.
    write_reg(&mut mixer, 0x81, 0x22);
    assert_eq!(mixer.selected_dma_8(), 1);
    assert_eq!(mixer.selected_dma_16(), 5);
    // 8-bit only (no 16-bit bit): 16-bit falls back to the 8-bit channel.
    write_reg(&mut mixer, 0x81, 0x09); // D0 | D3 => DMA0 wins (lowest)
    assert_eq!(mixer.selected_dma_8(), 0);
    assert_eq!(mixer.selected_dma_16(), 0, "16-bit over 8-bit channel");
    // 16-bit only (no 8-bit bit): 8-bit keeps the default channel.
    write_reg(&mut mixer, 0x81, 0x80); // D7 => DMA7
    assert_eq!(mixer.selected_dma_16(), 7);
    assert_eq!(mixer.selected_dma_8(), 1, "8-bit defaults to DMA1");
    // Mixed, lowest of each group.
    write_reg(&mut mixer, 0x81, 0x48); // D3 | D6 => DMA3 / DMA6
    assert_eq!(mixer.selected_dma_8(), 3);
    assert_eq!(mixer.selected_dma_16(), 6);
}

#[test]
fn reset_register_restores_hardware_defaults() {
    let mut mixer = SbMixer::default();
    write_reg(&mut mixer, 0x80, 0x08); // IRQ10
    write_reg(&mut mixer, 0x81, 0x80); // DMA7
    write_reg(&mut mixer, 0x30, 0x00); // master mute
    // Write any value to the Reset register (index 0x00).
    write_reg(&mut mixer, 0x00, 0x01);
    assert_eq!(read_reg(&mut mixer, 0x80), 0x02, "IRQ5 default");
    assert_eq!(read_reg(&mut mixer, 0x81), 0x22, "DMA1|DMA5 default");
    assert_eq!(mixer.selected_irq(), 5);
    assert_eq!(read_reg(&mut mixer, 0x30), 24, "master -14 dB default");
}

#[test]
fn interrupt_status_is_read_only_and_lifecycle() {
    let mut mixer = SbMixer::default();
    // Writes to 0x82 are ignored.
    write_reg(&mut mixer, 0x82, 0xFF);
    assert_eq!(read_reg(&mut mixer, 0x82), 0x00, "writes ignored at rest");
    // Producer sets the 8-bit then 16-bit source bit.
    mixer.set_irq_status(false);
    assert_eq!(read_reg(&mut mixer, 0x82), 0x01, "8-bit DMA / SB-MIDI bit");
    mixer.set_irq_status(true);
    assert_eq!(
        read_reg(&mut mixer, 0x82),
        0x02,
        "16-bit DMA bit (Guide: test al,02h)"
    );
    // Guest ack clears it.
    mixer.clear_irq_status();
    assert_eq!(read_reg(&mut mixer, 0x82), 0x00);
}

#[test]
fn ct1345_compat_master_alias_round_trips_through_0x30_0x31() {
    let mut mixer = SbMixer::default();
    // out 0x224,0x22; out 0x225,0xFF; then 0x30/0x31 reflect 0x1E/0x1E.
    write_reg(&mut mixer, 0x22, 0xFF);
    assert_eq!(read_reg(&mut mixer, 0x30), 0x1E);
    assert_eq!(read_reg(&mut mixer, 0x31), 0x1E);
    // Read-back through the alias packs each side back to 4-bit (0x1E>>1 = 0xF).
    assert_eq!(read_reg(&mut mixer, 0x22), 0xFF);
    // The 5-bit default (24) packs to 12|12 => 0xCC.
    let mut fresh = SbMixer::default();
    assert_eq!(read_reg(&mut fresh, 0x22), 0xCC, "default master alias");
}

#[test]
fn ct1345_compat_voice_alias_round_trips_through_0x32_0x33() {
    let mut mixer = SbMixer::default();
    write_reg(&mut mixer, 0x04, 0x00);
    assert_eq!(read_reg(&mut mixer, 0x32), 0x00);
    assert_eq!(read_reg(&mut mixer, 0x33), 0x00);
    assert_eq!(mixer.voice_gain(), (0.0, 0.0), "level 0 is a hard mute");
    write_reg(&mut mixer, 0x04, 0xFF);
    assert_eq!(read_reg(&mut mixer, 0x32), 0x1E);
}

#[test]
fn volume_gain_tables_match_the_guide_scales() {
    let mixer = SbMixer::default();
    // Master/voice default level 24 => -14 dB => 10**(-14/20).
    let expected = 10f32.powf(-14.0 / 20.0);
    let (ml, mr) = mixer.master_gain();
    let (vl, vr) = mixer.voice_gain();
    assert!((ml - expected).abs() < 1e-3 && (mr - expected).abs() < 1e-3);
    assert!((vl - expected).abs() < 1e-3 && (vr - expected).abs() < 1e-3);
    // Level 0 is a hard mute (both channels).
    let mut muted = mixer.clone();
    write_reg(&mut muted, 0x30, 0x00);
    write_reg(&mut muted, 0x31, 0x00);
    assert_eq!(muted.master_gain(), (0.0, 0.0));
    // Level 31 is unity (0 dB).
    let mut full = mixer.clone();
    write_reg(&mut full, 0x30, 0x1F);
    let (fl, _) = full.master_gain();
    assert!((fl - 1.0).abs() < 1e-3, "level 31 => 0 dB => gain 1.0");
    // Output gain default 0 => 0 dB => 1.0; level 3 => +18 dB.
    assert_eq!(mixer.outgain_gain(), (1.0, 1.0));
    let mut boosted = mixer.clone();
    write_reg(&mut boosted, 0x41, 0x03);
    let (ol, _) = boosted.outgain_gain();
    assert!((ol - 10f32.powf(18.0 / 20.0)).abs() < 1e-3);
}

#[test]
fn with_power_on_keeps_the_configured_routing() {
    let mixer = SbMixer::with_power_on(7, 3, 6);
    assert_eq!(mixer.selected_irq(), 7);
    assert_eq!(mixer.selected_dma_8(), 3);
    assert_eq!(mixer.selected_dma_16(), 6);
    // A guest reset restores the hardware defaults, not the host config.
    let mut mixer = mixer;
    write_reg(&mut mixer, 0x00, 0x00);
    assert_eq!(mixer.selected_irq(), 5);
    assert_eq!(mixer.selected_dma_8(), 1);
    assert_eq!(mixer.selected_dma_16(), 5);
}

#[test]
fn sbpro_stereo_bit_in_register_0x0e_round_trips_and_decodes() {
    let mut mixer = SbMixer::default();
    // Reset/default leaves 0x0E = 0, so mono.
    assert!(!mixer.sbpro_stereo(), "default 0x0E is mono");
    // Writing bit1 selects stereo and the register still reads back.
    write_reg(&mut mixer, 0x0E, 0x02);
    assert!(mixer.sbpro_stereo(), "0x0E bit1 selects SB Pro stereo");
    assert_eq!(read_reg(&mut mixer, 0x0E), 0x02, "0x0E round-trips");
    // The output-filter bit (bit5) alone is cosmetic, not stereo.
    write_reg(&mut mixer, 0x0E, 0x20);
    assert!(!mixer.sbpro_stereo(), "bit5 alone is mono");
}

#[test]
fn inert_registers_round_trip_at_their_defaults() {
    let mixer = SbMixer::default();
    let mut mixer = mixer;
    // Output switches and tone defaults are returned verbatim.
    assert_eq!(read_reg(&mut mixer, 0x3C), 0x1F);
    assert_eq!(read_reg(&mut mixer, 0x44), 8);
    // A guest write round-trips through the stored-but-inert slot.
    write_reg(&mut mixer, 0x3C, 0x02);
    assert_eq!(read_reg(&mut mixer, 0x3C), 0x02);
}
