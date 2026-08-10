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
    // CD powers on at 0 dB, like master/voice/FM (DOSBox-X CTMIXER_Reset cda=31,
    // 86Box resets 0x36/0x37 to 0xF8).
    let (dl, dr) = mixer.cd_gain();
    assert!((dl - 1.0).abs() < 1e-3 && (dr - 1.0).abs() < 1e-3);
    assert_eq!(read_reg(&mut mixer, 0x36), 0xF8);
    // The 5-bit CD registers set the gain directly. The level lives in D7-D3,
    // so level 31 is the byte 0xF8, not 31.
    write_reg(&mut mixer, 0x36, 0x00);
    write_reg(&mut mixer, 0x37, 0x00);
    assert_eq!(mixer.cd_gain(), (0.0, 0.0), "level 0 is a hard mute");
    write_reg(&mut mixer, 0x36, 31 << 3);
    write_reg(&mut mixer, 0x37, 31 << 3);
    let (l, r) = mixer.cd_gain();
    assert!(l > 0.9 && r > 0.9, "full CD volume is near unity: {l},{r}");
    // The CT1345 compat alias maps into the same 5-bit registers, and its max
    // nibble reaches level 31 -- 0 dB, not the -2 dB a `nibble << 1` would give.
    let mut compat = SbMixer::default();
    // Nibble 0 is level 1, NOT the mute step: the compat registers physically
    // cannot reach level 0, so the quietest they express is the -60 dB floor.
    write_reg(&mut compat, 0x28, 0x00);
    assert_eq!(compat.cd_levels(), (1, 1));
    let (ql, _) = compat.cd_gain();
    assert!(ql > 0.0 && ql < 0.002, "compat floor is -60 dB, got {ql}");
    write_reg(&mut compat, 0x28, 0xFF); // both nibbles max
    let (cl, cr) = compat.cd_gain();
    assert!((cl - 1.0).abs() < 1e-3 && (cr - 1.0).abs() < 1e-3);
    assert_eq!(read_reg(&mut compat, 0x36), 0xF8, "full compat is 0 dB");
    // A read of 0x28 round-trips the compat byte.
    assert_eq!(read_reg(&mut compat, 0x28), 0xFF);
    // The SB1/2 alias at 0x08 drives BOTH channels from one nibble.
    let mut sb1 = SbMixer::default();
    write_reg(&mut sb1, 0x08, 0x07);
    assert_eq!(sb1.cd_levels(), (15, 15), "nibble 7 -> level (7<<1)|1");
    assert_eq!(read_reg(&mut sb1, 0x08), 0x07);
}

#[test]
fn direct_cd_levels_preserve_latch_irq_and_keep_alias_coherent() {
    let mut mixer = SbMixer::default();
    mixer.write_port(MIXER_INDEX_PORT, 0x81);
    mixer.set_irq_status(true);

    mixer.set_cd_levels(40, 17);

    assert_eq!(mixer.cd_levels(), (31, 17));
    assert_eq!(mixer.read_port(MIXER_INDEX_PORT), Some(0x81));
    assert_eq!(mixer.read_port(MIXER_DATA_PORT), Some(0x22));
    mixer.write_port(MIXER_INDEX_PORT, 0x82);
    assert_eq!(mixer.read_port(MIXER_DATA_PORT), Some(0x02));
    mixer.write_port(MIXER_INDEX_PORT, 0x28);
    assert_eq!(mixer.read_port(MIXER_DATA_PORT), Some(0xF8));

    write_reg(&mut mixer, 0x36, 11 << 3);
    write_reg(&mut mixer, 0x37, 24 << 3);
    assert_eq!(read_reg(&mut mixer, 0x28), 0x5C);
    // The native register reads back left-aligned, the way it was written.
    assert_eq!(read_reg(&mut mixer, 0x36), 11 << 3);
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
    assert_eq!(
        read_reg(&mut mixer, 0x30),
        31 << 3,
        "master powers on at 0 dB, as DOSBox-X and 86Box also do"
    );
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
    // The max nibble reaches level 31 (0xF8), so full compat volume is 0 dB. A
    // `nibble << 1` mapping would land on level 30 / 0xF0, quietly -2 dB down and
    // disagreeing with the same level written natively.
    assert_eq!(read_reg(&mut mixer, 0x30), 0xF8);
    assert_eq!(read_reg(&mut mixer, 0x31), 0xF8);
    assert!((mixer.master_gain().0 - 1.0).abs() < 1e-3);
    // Read-back through the alias packs each side back to 4-bit (0x1F>>1 = 0xF).
    assert_eq!(read_reg(&mut mixer, 0x22), 0xFF);
    // The 0 dB default (level 31) packs to 15|15 => 0xFF.
    let mut fresh = SbMixer::default();
    assert_eq!(read_reg(&mut fresh, 0x22), 0xFF, "default master alias");
    // The SB1/2 alias at 0x02 drives both channels from one nibble.
    let mut sb1 = SbMixer::default();
    write_reg(&mut sb1, 0x02, 0x00);
    assert_eq!(read_reg(&mut sb1, 0x30), 0x08, "nibble 0 -> level 1");
    assert_eq!(read_reg(&mut sb1, 0x31), 0x08);
    assert_eq!(read_reg(&mut sb1, 0x02), 0x00);
}

#[test]
fn ct1345_compat_voice_alias_round_trips_through_0x32_0x33() {
    let mut mixer = SbMixer::default();
    write_reg(&mut mixer, 0x04, 0x00);
    // The compat scale bottoms out at level 1 (-60 dB), not the mute step.
    assert_eq!(read_reg(&mut mixer, 0x32), 0x08);
    assert_eq!(read_reg(&mut mixer, 0x33), 0x08);
    let (vl, _) = mixer.voice_gain();
    assert!(vl > 0.0 && vl < 0.002);
    write_reg(&mut mixer, 0x04, 0xFF);
    assert_eq!(read_reg(&mut mixer, 0x32), 0xF8);
}

#[test]
fn volume_gain_tables_match_the_guide_scales() {
    let mixer = SbMixer::default();
    // Master/voice/FM power on at level 31 => 0 dB => unity.
    let (ml, mr) = mixer.master_gain();
    let (vl, vr) = mixer.voice_gain();
    let (fl, fr) = mixer.fm_gain();
    assert!((ml - 1.0).abs() < 1e-3 && (mr - 1.0).abs() < 1e-3);
    assert!((vl - 1.0).abs() < 1e-3 && (vr - 1.0).abs() < 1e-3);
    assert!((fl - 1.0).abs() < 1e-3 && (fr - 1.0).abs() < 1e-3);
    // Level 24, the Guide's documented -14 dB step, written left-aligned.
    let mut quiet = mixer.clone();
    write_reg(&mut quiet, 0x30, 24 << 3);
    let expected = 10f32.powf(-14.0 / 20.0);
    assert!((quiet.master_gain().0 - expected).abs() < 1e-3);
    // Level 0 is a hard mute (both channels).
    let mut muted = mixer.clone();
    write_reg(&mut muted, 0x30, 0x00);
    write_reg(&mut muted, 0x31, 0x00);
    assert_eq!(muted.master_gain(), (0.0, 0.0));
    // Level 31 is unity (0 dB).
    let mut full = mixer.clone();
    write_reg(&mut full, 0x30, 0x1F << 3);
    let (fullgain, _) = full.master_gain();
    assert!(
        (fullgain - 1.0).abs() < 1e-3,
        "level 31 => 0 dB => gain 1.0"
    );
    // Output gain default 0 => 0 dB => 1.0; level 3 => +18 dB. The 2-bit field
    // lives in D7-D6.
    assert_eq!(mixer.outgain_gain(), (1.0, 1.0));
    let mut boosted = mixer.clone();
    write_reg(&mut boosted, 0x41, 0x03 << 6);
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
    // A guest write round-trips through the stored-but-inert slot.
    write_reg(&mut mixer, 0x3C, 0x02);
    assert_eq!(read_reg(&mut mixer, 0x3C), 0x02);
}

/// Every power-on byte an inert register returns must be encoded in the field
/// that register actually uses, the same way the live registers are decoded.
/// Returning a bare level (8 for a 4-bit tone control, 0 for the speaker) hands
/// the guest a byte the card cannot produce and contradicts the convention the
/// rest of the file is written to. 86Box's reset block is the reference.
#[test]
fn inert_defaults_are_hardware_encoded_not_bare_levels() {
    let mut mixer = SbMixer::default();
    // Tone controls: 4-bit field in D7-D4, centre 8 => 0x80 (86Box 0x44-0x47).
    for reg in [0x44u8, 0x45, 0x46, 0x47] {
        assert_eq!(read_reg(&mut mixer, reg), 0x80, "{reg:#04x} centre is 0x80");
    }
    // PC Speaker volume (86Box 0x3B = 0x80, "steps of 64").
    assert_eq!(read_reg(&mut mixer, 0x3B), 0x80);
    // Mic: 86Box writes `(regs[0x0a] << 5) | 0x18` into 0x3A, so the default is
    // 0x18 and the alias tracks it in both directions.
    assert_eq!(read_reg(&mut mixer, 0x3A), 0x18);
    assert_eq!(read_reg(&mut mixer, 0x0A), 0x00);
    write_reg(&mut mixer, 0x0A, 0x05);
    assert_eq!(read_reg(&mut mixer, 0x3A), (5 << 5) | 0x18);
    assert_eq!(read_reg(&mut mixer, 0x0A), 0x05);
}

/// The FM/MIDI bus has three register paths -- 0x34/0x35 (SB16), 0x26 (SB Pro,
/// packed nibbles) and 0x06 (SB1/2, one nibble for both channels) -- and they are
/// one control. 86Box `sb_ct1745_mixer_write` cases 0x06 and 0x26 copy into
/// 0x34/0x35. Leaving 0x26/0x06 in the inert store meant an SB Pro-era title
/// setting its music volume got no attenuation at all, and the register file
/// contradicted itself at power-on (0x26 read 0xCC while 0x34 read 0xF8).
#[test]
fn fm_compat_aliases_drive_the_same_level_as_0x34_0x35() {
    // Power-on: every FM path agrees on 0 dB.
    let mut mixer = SbMixer::default();
    assert_eq!(read_reg(&mut mixer, 0x34), 0xF8);
    assert_eq!(read_reg(&mut mixer, 0x26), 0xFF);
    assert_eq!(read_reg(&mut mixer, 0x06), 0x0F);

    // Alias -> native: the SB Pro packed byte attenuates the FM bus.
    write_reg(&mut mixer, 0x26, 0x94); // L nibble 9 -> level 19, R nibble 4 -> 9
    assert_eq!(read_reg(&mut mixer, 0x34), 19 << 3);
    assert_eq!(read_reg(&mut mixer, 0x35), 9 << 3);
    let (fl, fr) = mixer.fm_gain();
    assert!(
        (fl - 10f32.powf(-24.0 / 20.0)).abs() < 1e-3,
        "level 19: {fl}"
    );
    assert!(
        (fr - 10f32.powf(-44.0 / 20.0)).abs() < 1e-4,
        "level 9: {fr}"
    );

    // Native -> alias: a write to 0x34/0x35 moves the alias read-back too, so a
    // read-modify-write through either path sees one consistent control.
    write_reg(&mut mixer, 0x34, 31 << 3);
    write_reg(&mut mixer, 0x35, 21 << 3);
    assert_eq!(read_reg(&mut mixer, 0x26), 0xFA); // 31>>1 = 0xF, 21>>1 = 0xA
    assert_eq!(read_reg(&mut mixer, 0x06), 0x0F);

    // The SB1/2 alias sets both channels from one nibble, and reads back the
    // nibble the FM level currently sits on.
    let mut sb1 = SbMixer::default();
    write_reg(&mut sb1, 0x06, 0x06); // level (6<<1)|1 = 13
    assert_eq!(read_reg(&mut sb1, 0x34), 13 << 3);
    assert_eq!(read_reg(&mut sb1, 0x35), 13 << 3);
    assert_eq!(read_reg(&mut sb1, 0x06), 0x06);
    assert_eq!(read_reg(&mut sb1, 0x26), 0x66);
    let (gl, gr) = sb1.fm_gain();
    let expected = 10f32.powf(-36.0 / 20.0);
    assert!((gl - expected).abs() < 1e-4 && (gr - expected).abs() < 1e-4);

    // A mixer reset restores every FM path to 0 dB together.
    write_reg(&mut sb1, 0x00, 0x00);
    assert_eq!(read_reg(&mut sb1, 0x34), 0xF8);
    assert_eq!(read_reg(&mut sb1, 0x26), 0xFF);
    assert_eq!(read_reg(&mut sb1, 0x06), 0x0F);
}

/// The exact register traffic Duke Nukem 3D emits, and the balance it asks for.
///
/// Duke computes each mixer level as `volume * 31 / 255` and writes it
/// LEFT-ALIGNED (`level << 3`): with `FXVolume = 228` and `MusicVolume = 224`
/// from DUKE3D.CFG that is `0xD8` to the voice pair and `0xC8` to the FM pair.
/// Reading those bytes as `& 0x1F` decoded the voice as level 24 instead of 27
/// and, because `0x34`/`0x35` were an inert store, applied no FM attenuation at
/// all -- 6 dB off the effects and 12 dB of music that should not have been
/// there, which is why the game's music played and its digital effects did not.
#[test]
fn duke3d_mixer_writes_put_the_effects_above_the_music() {
    let mut mixer = SbMixer::default();
    write_reg(&mut mixer, 0x32, 0xD8); // FX volume 228 -> level 27
    write_reg(&mut mixer, 0x33, 0xD8);
    write_reg(&mut mixer, 0x34, 0xC8); // music volume -> level 25
    write_reg(&mut mixer, 0x35, 0xC8);

    let (voice, _) = mixer.voice_gain();
    let (fm, _) = mixer.fm_gain();
    assert!(
        (voice - 10f32.powf(-8.0 / 20.0)).abs() < 1e-3,
        "level 27 is -8 dB, not the -14 dB `& 0x1F` produced: {voice}"
    );
    assert!(
        (fm - 10f32.powf(-12.0 / 20.0)).abs() < 1e-3,
        "level 25 is -12 dB, not the 0 dB an inert 0x34 produced: {fm}"
    );
    assert!(
        voice > fm,
        "Duke asks for effects ABOVE music ({voice} vs {fm})"
    );
    // Duke never writes 0x30/0x31, so the balance it asks for is only preserved
    // if the master leaves it alone.
    assert_eq!(mixer.master_gain(), (1.0, 1.0), "untouched master is 0 dB");
    // Round-trip: a read-modify-write setup utility must see what it wrote.
    assert_eq!(read_reg(&mut mixer, 0x32), 0xD8);
    assert_eq!(read_reg(&mut mixer, 0x34), 0xC8);
}

/// The failure mode the old decode had beyond the few-dB error: any level that
/// is a multiple of four writes a byte whose low five bits are zero, so
/// `value & 0x1F` read it as level 0 -- a hard mute. Level 24 is the Guide's own
/// documented power-on value, i.e. a card told to restore its default went
/// silent.
#[test]
fn levels_that_are_multiples_of_four_are_not_muted() {
    for level in [4u8, 8, 12, 16, 20, 24, 28] {
        let mut mixer = SbMixer::default();
        write_reg(&mut mixer, 0x32, level << 3);
        let (gain, _) = mixer.voice_gain();
        assert!(gain > 0.0, "level {level} must not decode as a mute");
        assert_eq!(read_reg(&mut mixer, 0x32), level << 3);
    }
}

/// The PC-speaker leg (`0x3B`) is two bits wide on the CT1745, not five: the
/// control the guest sees has four positions and only D7-D6 select one. The
/// three audible ones are 86Box's `sb_att_7dbstep_2bits` figures (-14, -7 and
/// 0 dB); position 0 is the house hard mute rather than that table's -46 dB
/// floor, matching what level 0 does on every 5-bit register here.
///
/// Before this the register was inert: every byte read back and NOTHING moved,
/// which is why the beeper sat at full scale next to attenuated card audio.
#[test]
fn pc_speaker_register_decodes_two_bits_and_attenuates() {
    let mut mixer = SbMixer::default();
    // Power-on is position 2 (0x80), which is -7 dB and NOT unity: a card that
    // has never been programmed already pads its PC-SPK input.
    assert_eq!(mixer.speaker_level(), 2);
    let power_on = mixer.speaker_gain();
    assert!(
        (power_on - 10f32.powf(-7.0 / 20.0)).abs() < 1e-4,
        "power-on PC-SPK is -7 dB, got {power_on}"
    );

    let mut seen = Vec::new();
    for position in 0u8..4 {
        write_reg(&mut mixer, 0x3B, position << 6);
        assert_eq!(mixer.speaker_level(), position);
        seen.push(mixer.speaker_gain());
    }
    assert_eq!(seen[0], 0.0, "position 0 is a hard mute");
    for (position, db) in [(1usize, -14.0f32), (2, -7.0), (3, 0.0)] {
        let want = 10f32.powf(db / 20.0);
        assert!(
            (seen[position] - want).abs() < 1e-4,
            "position {position} is {db} dB: want {want}, got {}",
            seen[position]
        );
    }
    // Strictly monotonic: four positions, four distinct gains. A decode that
    // dropped a bit (`>> 7`) or masked the wrong field would collapse two of
    // them onto each other and this is what would catch it.
    for pair in seen.windows(2) {
        assert!(pair[1] > pair[0], "positions must be ordered: {seen:?}");
    }

    // D5-D0 are don't-care on the card: they neither change the decode nor get
    // masked out of the read-back (86Box stores the raw byte). A guest doing a
    // read-modify-write therefore sees exactly its own byte.
    write_reg(&mut mixer, 0x3B, 0x7F);
    assert_eq!(read_reg(&mut mixer, 0x3B), 0x7F);
    assert_eq!(mixer.speaker_level(), 1, "0x7F selects position 1");
    assert!((mixer.speaker_gain() - 10f32.powf(-14.0 / 20.0)).abs() < 1e-4);
}

/// The ReSonique 2 wavetable leg (`0x50`/`0x51`) is this card's own extension:
/// a real CT1745 has no register for a wavetable, because a real CT1745 has no
/// wavetable. It carries the card's ordinary 5-bit D7-D3 level so a guest
/// programs it with the sequence it already uses for master/voice/FM/CD, and it
/// powers on at 0 dB so adding the control does not move the leg.
#[test]
fn wavetable_extension_registers_decode_as_five_bit_levels() {
    let mut mixer = SbMixer::default();
    assert_eq!(read_reg(&mut mixer, 0x50), 0xF8);
    assert_eq!(read_reg(&mut mixer, 0x51), 0xF8);
    let (l, r) = mixer.wavetable_gain();
    assert!((l - 1.0).abs() < 1e-3 && (r - 1.0).abs() < 1e-3);

    // The two channels are independent, and the level lives in D7-D3: writing
    // the bare level 21 instead of `21 << 3` would land on level 2.
    write_reg(&mut mixer, 0x50, 21 << 3);
    assert_eq!(read_reg(&mut mixer, 0x50), 21 << 3);
    assert_eq!(read_reg(&mut mixer, 0x51), 0xF8, "0x50 does not touch 0x51");
    let (l, r) = mixer.wavetable_gain();
    assert!(
        (l - 10f32.powf(-20.0 / 20.0)).abs() < 1e-4,
        "level 21 is -20 dB, got {l}"
    );
    assert!((r - 1.0).abs() < 1e-3);

    write_reg(&mut mixer, 0x51, 0x00);
    assert_eq!(mixer.wavetable_gain().1, 0.0, "level 0 is a hard mute");

    // A mixer reset restores it with everything else.
    write_reg(&mut mixer, 0x00, 0x00);
    assert_eq!(read_reg(&mut mixer, 0x50), 0xF8);
    assert_eq!(read_reg(&mut mixer, 0x51), 0xF8);
}
