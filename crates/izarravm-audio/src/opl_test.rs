// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// For
/// every `Timer` state in the sweep and every `micros_elapsed`, peeking must
/// report the exact same `expired` a real `.advance(micros_elapsed, preset)`
/// on a clone would produce, without mutating the original. Sweeps running/
/// stopped, already-expired, a spread of accumulated_us/step_us/count/preset,
/// and elapsed values straddling the overflow boundary (including 0).
#[test]
fn expired_after_matches_a_real_advance_on_a_clone() {
    let step_us_values = [80u64, 320u64];
    // count never actually reaches 0x100 in reachable Timer state:
    // `advance` resets it to the preset in the same step that crosses
    // 0xff, so the invariant is count <= 0xff always.
    let count_values = [0u16, 1, 0xfe, 0xff];
    let accumulated_us_values = [0u64, 1, 39, 79, 80, 319, 320];
    let preset_values = [0u8, 1, 0x7f, 0xfe, 0xff];
    let elapsed_values = [0u64, 1, 39, 79, 80, 81, 159, 160, 319, 320, 321, 5_000];

    for &step_us in &step_us_values {
        for &running in &[false, true] {
            for &already_expired in &[false, true] {
                for &count in &count_values {
                    for &accumulated_us in &accumulated_us_values {
                        for &preset in &preset_values {
                            let timer = Timer {
                                step_us,
                                count,
                                accumulated_us,
                                running,
                                expired: already_expired,
                            };
                            for &elapsed in &elapsed_values {
                                let mut clone = timer.clone();
                                clone.advance(elapsed, preset);
                                let expected = clone.expired;
                                let got = timer.expired_after(elapsed);
                                assert_eq!(
                                    got, expected,
                                    "step_us={step_us} running={running} \
                                     already_expired={already_expired} \
                                     count={count} accumulated_us={accumulated_us} \
                                     preset={preset} elapsed={elapsed}: \
                                     expired_after must match a real advance"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The
/// predicted status byte at T microseconds from now must equal what a real
/// `advance_micros(T)` followed by `status()` would produce, across both
/// timers running, various presets/masks, and T values straddling the
/// overflow boundary.
#[test]
fn status_after_matches_a_real_advance_micros_then_status() {
    let t_values = [0u64, 1, 39, 79, 80, 81, 319, 320, 321, 5_000];
    let masks = [0x00u8, 0x20, 0x40, 0x60];

    for &mask in &masks {
        let mut opl = OplChip::default();
        opl.write_register(0x02, 0xf0); // timer 1 preset
        opl.write_register(0x03, 0xe0); // timer 2 preset
        opl.write_register(0x04, 0x80); // reset IRQ flags
        opl.write_register(0x04, mask | 0x03); // start both timers, apply mask

        for &t in &t_values {
            let predicted = opl.status_after(t);

            let mut real = opl.clone();
            real.advance_micros(t);
            let expected = real.status();

            assert_eq!(
                predicted, expected,
                "mask={mask:#04x} t={t}: status_after must match a real \
                 advance_micros(t) then status()"
            );
        }
    }
}

#[test]
fn exp_of_logsin_reconstructs_the_sine_quarter_wave() {
    // The whole point of the two ROMs: running each quarter-wave index
    // through log-sin then exp must rebuild sin() to within quantization.
    let max = f64::from(exp_lookup(u32::from(logsin(255)))); // loudest point
    for i in 0..256 {
        let attenuation = u32::from(logsin(i));
        let got = f64::from(exp_lookup(attenuation));
        let expected = ((i as f64 + 0.5) * std::f64::consts::PI / 512.0).sin() * max;
        assert!(
            (got - expected).abs() <= 8.0,
            "index {i}: got {got}, expected {expected}"
        );
    }
}

#[test]
fn exp_lookup_saturates_to_zero_for_large_attenuation() {
    // A fully-silent envelope plus total level can push attenuation past a
    // valid 32-bit shift; it must saturate to silence, not overflow.
    assert_eq!(exp_lookup(0x1ff << 3), 0);
    assert_eq!(exp_lookup(0x2000), 0);
}

#[test]
fn rom_tables_match_their_known_anchor_values() {
    assert_eq!(logsin(0), 2137, "quietest log-sin entry");
    assert_eq!(logsin(255), 0, "loudest log-sin entry");
    assert_eq!(exp_lookup(0), 4084, "max amplitude at zero attenuation");
}

fn sine_operator(fnum: u16, block: u8, waveform: u8) -> Operator {
    let mut op = Operator::default();
    op.set_frequency(fnum, block);
    op.set_multiple(1); // register value 1 => x1
    op.set_waveform(waveform);
    op
}

fn peak_magnitude(mut op: Operator, samples: usize) -> i32 {
    let mut peak = 0;
    for _ in 0..samples {
        peak = peak.max(op.sample(0).abs());
        op.advance();
    }
    peak
}

#[test]
fn operator_runs_at_the_programmed_frequency() {
    // Count rising zero-crossings over one second and compare to the OPL
    // frequency formula: f = fnum * 2^block * rate / 2^20.
    let (fnum, block) = (0x200u16, 4u8);
    let rate = 49_716.0_f64;
    let expected = f64::from(fnum) * 2f64.powi(i32::from(block)) * rate / 2f64.powi(20);

    let mut op = sine_operator(fnum, block, 0);
    let mut crossings = 0u32;
    let mut prev = op.sample(0);
    op.advance();
    for _ in 1..rate as usize {
        let s = op.sample(0);
        if prev <= 0 && s > 0 {
            crossings += 1;
        }
        prev = s;
        op.advance();
    }

    let measured = f64::from(crossings);
    assert!(
        (measured - expected).abs() / expected < 0.01,
        "expected ~{expected:.1} Hz, measured {measured}"
    );
}

#[test]
fn operator_peaks_near_max_amplitude() {
    let peak = peak_magnitude(sine_operator(0x200, 4, 0), 512);
    assert!((peak - 4084).abs() <= 8, "peak {peak}");
}

#[test]
fn total_level_attenuates_six_db_per_eight_steps() {
    // Eight TL steps of 0.75 dB = 6 dB = a factor-of-two amplitude drop.
    let loud = peak_magnitude(sine_operator(0x200, 4, 0), 512);
    let mut quiet = sine_operator(0x200, 4, 0);
    quiet.set_total_level(8);
    let quiet = peak_magnitude(quiet, 512);
    let ratio = f64::from(loud) / f64::from(quiet);
    assert!((ratio - 2.0).abs() < 0.05, "ratio {ratio}");
}

#[test]
fn half_sine_silences_the_second_half() {
    // Period is 128 samples; samples 64..128 fall in the second half.
    let mut full = sine_operator(0x200, 4, 0);
    let mut half = sine_operator(0x200, 4, 1);
    for i in 0..128 {
        let (a, b) = (full.sample(0), half.sample(0));
        if i < 64 {
            assert_eq!(a, b, "first half should match the sine, i={i}");
        } else {
            assert_eq!(b, 0, "second half should be silent, i={i}");
        }
        full.advance();
        half.advance();
    }
}

#[test]
fn abs_sine_has_no_negative_samples() {
    let mut op = sine_operator(0x200, 4, 2);
    for _ in 0..128 {
        assert!(op.sample(0) >= 0);
        op.advance();
    }
}

#[test]
fn quarter_sine_silences_the_second_and_fourth_quarters() {
    // Quarter is 32 samples; the odd quarters (1 and 3) are silent.
    let mut op = sine_operator(0x200, 4, 3);
    for i in 0..128 {
        let s = op.sample(0);
        if (i / 32) % 2 == 1 {
            assert_eq!(s, 0, "odd quarter should be silent, i={i}");
        } else {
            assert!(s >= 0, "quarter sine is non-negative, i={i}");
        }
        op.advance();
    }
}

fn ksl_operator(fnum: u16, block: u8, setting: u8) -> Operator {
    let mut op = sine_operator(fnum, block, 0);
    op.set_key_scale_level(setting);
    op
}

#[test]
fn ksl_table_follows_the_six_db_per_octave_derivation() {
    // ceil(8*log2(16n)) in 0.75 dB units: 8 units = 6 dB = one octave.
    assert_eq!(KSL[0], 0, "no attenuation at the lowest F-number");
    for n in 1..16usize {
        let expected = (8.0 * (16.0 * n as f64).log2()).ceil() as u16;
        assert_eq!(KSL[n], expected, "ksl[{n}]");
    }
    // Reproduces the standard KSL ROM; top entry = 64 units = 48 dB.
    assert_eq!(
        *KSL,
        [
            0, 32, 40, 45, 48, 51, 53, 55, 56, 58, 59, 60, 61, 62, 63, 64
        ]
    );
}

#[test]
fn ksl_zero_leaves_output_unattenuated() {
    let plain = peak_magnitude(sine_operator(0x300, 6, 0), 256);
    let off = peak_magnitude(ksl_operator(0x300, 6, 0), 256);
    assert_eq!(off, plain, "KSL=0 must not change the output");
}

#[test]
fn ksl_attenuates_six_db_per_octave_at_max_setting() {
    // Setting 3 = 6 dB/oct; one octave up at the same F-number halves output.
    let lo = peak_magnitude(ksl_operator(0x200, 5, 3), 256);
    let hi = peak_magnitude(ksl_operator(0x200, 6, 3), 256);
    let ratio = f64::from(lo) / f64::from(hi);
    assert!(
        (ratio - 2.0).abs() < 0.05,
        "expected ~2x per octave, got {ratio}"
    );
}

#[test]
fn ksl_settings_scale_the_attenuation() {
    // block 6, fnum 0x200 (n=8): base = KSL[8] - 8*(7-6) = 56 - 8 = 48 units.
    // Settings 1/2/3 attenuate by a quarter/half/all of the 6 dB/oct value.
    let base = 48u16;
    assert_eq!(ksl_operator(0x200, 6, 3).ksl_attenuation(), base << 5);
    assert_eq!(
        ksl_operator(0x200, 6, 2).ksl_attenuation(),
        (base >> 1) << 5
    );
    assert_eq!(
        ksl_operator(0x200, 6, 1).ksl_attenuation(),
        (base >> 2) << 5
    );
    assert_eq!(ksl_operator(0x200, 6, 0).ksl_attenuation(), 0);
}

#[test]
fn ksl_clamps_to_zero_for_low_pitch() {
    // Bottom octave with a small F-number sits below the reference: no cost.
    assert_eq!(ksl_operator(0x000, 0, 3).ksl_attenuation(), 0);
}

// Channel 0 with only the modulator (op 0) audible: carrier never opens
// (attack 0), additive, modulator at instant attack, self-feedback `fb`.
fn feedback_channel_samples(fb: u8) -> Vec<i32> {
    let mut opl = OplChip::default();
    opl.write_register(0x20, 0x01); // modulator: multiple x1
    opl.write_register(0x23, 0x01); // carrier: multiple x1
    opl.write_register(0x40, 0x00); // modulator loud
    opl.write_register(0x43, 0x00);
    opl.write_register(0x60, 0xf0); // modulator: instant attack
    opl.write_register(0x63, 0x00); // carrier: attack 0 -> stays silent
    opl.write_register(0x80, 0x00);
    opl.write_register(0x83, 0x00);
    opl.write_register(0xe0, 0x00); // modulator waveform: sine
    opl.write_register(0xc0, 0x01 | (fb << 1)); // additive + feedback factor
    opl.write_register(0xa0, 0x00);
    opl.write_register(0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
    (0..256).map(|_| opl.render_sample().0).collect()
}

#[test]
fn feedback_zero_is_a_clean_sine() {
    // FB=0 leaves the modulator unmodulated: it must equal a bare sine.
    let mut reference = sine_operator(0x200, 4, 0);
    let expected: Vec<i32> = (0..256)
        .map(|_| {
            let s = reference.sample(0);
            reference.advance();
            s
        })
        .collect();
    assert_eq!(feedback_channel_samples(0), expected);
}

#[test]
fn feedback_alters_and_bounds_the_modulator() {
    let plain = feedback_channel_samples(0);
    let fed = feedback_channel_samples(7);
    assert_ne!(plain, fed, "feedback must reshape the waveform");
    let peak = fed.iter().map(|s| s.abs()).max().unwrap();
    assert!(peak <= 4200, "self-feedback stays bounded, got {peak}");
}

#[test]
fn stronger_feedback_deviates_further_from_a_sine() {
    // Distance from the FB=0 sine grows with the feedback factor.
    let base = feedback_channel_samples(0);
    let dist = |fb| {
        feedback_channel_samples(fb)
            .iter()
            .zip(&base)
            .map(|(a, b)| u64::from((a - b).unsigned_abs()))
            .sum::<u64>()
    };
    assert!(dist(2) < dist(5), "FB=5 should deviate more than FB=2");
}

#[test]
fn opl3_waveform4_is_a_double_rate_sine_in_the_first_half() {
    // Period 128, first half 0..64. WAVE4 packs a full sine into the first
    // half (both signs) and silences the second.
    let mut op = sine_operator(0x200, 4, 4);
    let (mut saw_pos, mut saw_neg) = (false, false);
    for i in 0..128 {
        let s = op.sample(0);
        if i < 64 {
            saw_pos |= s > 0;
            saw_neg |= s < 0;
        } else {
            assert_eq!(s, 0, "second half is silent, i={i}");
        }
        op.advance();
    }
    assert!(saw_pos && saw_neg, "first half is a full sine");
}

#[test]
fn opl3_waveform5_is_double_rate_abs_sine_in_the_first_half() {
    let mut op = sine_operator(0x200, 4, 5);
    let mut peak = 0;
    for i in 0..128 {
        let s = op.sample(0);
        if i < 64 {
            assert!(s >= 0, "abs sine is non-negative, i={i}");
            peak = peak.max(s);
        } else {
            assert_eq!(s, 0, "second half is silent, i={i}");
        }
        op.advance();
    }
    assert!(peak > 1000, "first half has audible humps");
}

#[test]
fn opl3_waveform6_is_a_square_wave() {
    // Constant full-scale magnitude (exp_lookup(0) = 4084), sign flips at half.
    let mut op = sine_operator(0x200, 4, 6);
    for i in 0..128 {
        let expected = if i < 64 { 4084 } else { -4084 };
        assert_eq!(op.sample(0), expected, "square wave, i={i}");
        op.advance();
    }
}

#[test]
fn opl3_waveform7_is_a_log_sawtooth() {
    // Each half starts at the peak and decays; sign flips at the half.
    let mut op = sine_operator(0x200, 4, 7);
    let samples: Vec<i32> = (0..128)
        .map(|_| {
            let s = op.sample(0);
            op.advance();
            s
        })
        .collect();
    assert!(samples[0] > 2000, "first half starts at the positive peak");
    assert!(
        samples[64] < -2000,
        "second half starts at the negative peak"
    );
    assert!(samples[32] < samples[0], "first half decays toward zero");
    assert!(samples[96].abs() < samples[64].abs(), "second half decays");
}

fn program_channel0(opl: &mut OplChip, fnum: u16, block: u8, additive: bool, modulator_tl: u8) {
    opl.write_register(0x20, 0x01); // modulator: multiple x1
    opl.write_register(0x23, 0x01); // carrier: multiple x1
    opl.write_register(0x40, modulator_tl); // modulator total level
    opl.write_register(0x43, 0x00); // carrier total level: loudest
    opl.write_register(0x60, 0xf0); // both operators: attack 15 (instant), decay 0
    opl.write_register(0x63, 0xf0);
    opl.write_register(0x80, 0x00); // sustain 0, release 0
    opl.write_register(0x83, 0x00);
    opl.write_register(0xe0, 0x00); // modulator waveform: sine
    opl.write_register(0xe3, 0x00); // carrier waveform: sine
    opl.write_register(0xc0, u8::from(additive)); // connection
    opl.write_register(0xa0, (fnum & 0xff) as u8); // f-number low
    opl.write_register(
        0xb0,
        0x20 | (block & 0x07) << 2 | ((fnum >> 8) & 0x03) as u8,
    );
}

// Carrier (operator 3) envelope after rendering `samples`, keyed at block 4.
fn carrier_eg_after(setup: impl Fn(&mut OplChip), samples: usize) -> u16 {
    let mut opl = OplChip::default();
    setup(&mut opl);
    for _ in 0..samples {
        opl.render_sample();
    }
    opl.envelope_level(3)
}

fn key_carrier(opl: &mut OplChip, ar: u8, dr: u8, sl: u8, rr: u8) {
    opl.write_register(0x23, 0x21); // EGT sustained, multiple 1
    opl.write_register(0x43, 0x00); // total level 0
    opl.write_register(0x63, (ar << 4) | dr);
    opl.write_register(0x83, (sl << 4) | rr);
    opl.write_register(0xa0, 0x00);
    opl.write_register(0xc0, 0x01); // additive, so the carrier reaches output
    opl.write_register(0xb0, 0x20 | (4 << 2)); // key-on, block 4
}

#[test]
fn attack_opens_the_envelope_to_full_volume() {
    let eg = carrier_eg_after(|opl| key_carrier(opl, 15, 0, 0, 0), 4);
    assert_eq!(eg, 0, "instant attack reaches full volume");
}

#[test]
fn zero_attack_rate_keeps_the_operator_silent() {
    let eg = carrier_eg_after(|opl| key_carrier(opl, 0, 0, 0, 0), 5000);
    assert_eq!(eg, 0x1ff, "attack rate 0 never opens");
}

#[test]
fn higher_attack_rate_opens_faster() {
    let slow = carrier_eg_after(|opl| key_carrier(opl, 6, 0, 0, 0), 1500);
    let fast = carrier_eg_after(|opl| key_carrier(opl, 8, 0, 0, 0), 1500);
    assert!(
        fast < slow,
        "AR=8 should be further along than AR=6: {fast} vs {slow}"
    );
}

#[test]
fn decay_falls_to_the_sustain_level_and_holds() {
    let eg = carrier_eg_after(|opl| key_carrier(opl, 15, 12, 8, 0), 2000);
    assert_eq!(eg, 0x80, "decay settles and holds at sustain level 8");
}

#[test]
fn key_off_releases_to_silence() {
    let mut opl = OplChip::default();
    key_carrier(&mut opl, 15, 0, 0, 8); // instant attack, release rate 8
    for _ in 0..8 {
        opl.render_sample();
    }
    assert_eq!(opl.envelope_level(3), 0, "keyed and open");

    opl.write_register(0xb0, 4 << 2); // key-off, keep block
    for _ in 0..20_000 {
        opl.render_sample();
    }
    assert_eq!(opl.envelope_level(3), 0x1ff, "released to silence");
}

#[test]
fn silent_chip_renders_zero() {
    let mut opl = OplChip::default();
    for _ in 0..64 {
        assert_eq!(opl.render_sample(), (0, 0));
    }
}

#[test]
fn keyed_channel_renders_a_tone_at_its_frequency() {
    // Additive with the modulator muted leaves a clean carrier sine, so
    // zero-crossings should match the channel's programmed frequency.
    let (fnum, block) = (0x200u16, 4u8);
    let mut opl = OplChip::default();
    program_channel0(&mut opl, fnum, block, true, 0x3f);

    let rate = 49_716.0_f64;
    let expected = f64::from(fnum) * 2f64.powi(i32::from(block)) * rate / 2f64.powi(20);
    let mut crossings = 0u32;
    let mut prev = opl.render_sample().0;
    for _ in 1..rate as usize {
        let s = opl.render_sample().0;
        if prev <= 0 && s > 0 {
            crossings += 1;
        }
        prev = s;
    }

    let measured = f64::from(crossings);
    assert!(
        (measured - expected).abs() / expected < 0.02,
        "expected ~{expected:.1} Hz, measured {measured}"
    );
}

#[test]
fn fm_and_additive_differ_with_an_active_modulator() {
    let collect = |additive| {
        let mut opl = OplChip::default();
        program_channel0(&mut opl, 0x200, 4, additive, 0x00);
        (0..128).map(|_| opl.render_sample().0).collect::<Vec<_>>()
    };
    assert_ne!(
        collect(false),
        collect(true),
        "FM should not equal additive"
    );
}

#[test]
fn channel_applies_key_scale_level_from_registers() {
    // Decode reg 0x40 bits6-7 into the carrier: at block 6 / fnum 0x200 a
    // KSL of 3 is 36 dB of attenuation, so the keyed tone is far quieter.
    let peak = |ksl_bits: u8| {
        let mut opl = OplChip::default();
        opl.write_register(0x23, 0x21); // carrier: sustained, multiple x1
        opl.write_register(0x43, ksl_bits << 6); // KSL in bits 6-7, total level 0
        opl.write_register(0x63, 0xf0); // attack 15 (instant)
        opl.write_register(0x83, 0x00);
        opl.write_register(0xa0, 0x00); // f-number low
        opl.write_register(0xc0, 0x01); // additive, so the carrier reaches output
        opl.write_register(0xb0, 0x20 | (6 << 2) | 0x02); // key-on, block 6, fnum 0x200
        (0..64).map(|_| opl.render_sample().0.abs()).max().unwrap()
    };
    let loud = peak(0);
    let scaled = peak(3);
    assert!(
        scaled * 10 < loud,
        "KSL=3 at block 6 must strongly attenuate: {scaled} vs {loud}"
    );
}

// Write `value` into secondary-bank register `index` via ports 0x38A/0x38B.
fn write_secondary(opl: &mut OplChip, index: u8, value: u8) {
    opl.write_port(0x38a, index);
    opl.write_port(0x38b, value);
}

#[test]
fn opl3_mode_unlocks_the_secondary_bank_channels() {
    // Channel 9 lives in the secondary bank; it is silent until OPL3 mode
    // (reg 0x105 / NEW) is enabled and the chip renders all 18 channels.
    let setup = |opl: &mut OplChip| {
        write_secondary(opl, 0x23, 0x21); // carrier (op 21): sustained, multiple x1
        write_secondary(opl, 0x43, 0x00); // carrier total level 0
        write_secondary(opl, 0x63, 0xf0); // attack 15 (instant)
        write_secondary(opl, 0x83, 0x00);
        write_secondary(opl, 0xa0, 0x00); // channel 9 f-number low
        write_secondary(opl, 0xc0, 0x31); // additive + left/right enable
        write_secondary(opl, 0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
    };
    let peak = |opl: &mut OplChip| (0..256).map(|_| opl.render_sample().0.abs()).max().unwrap();

    let mut off = OplChip::default();
    setup(&mut off);
    assert_eq!(
        peak(&mut off),
        0,
        "secondary channel is silent without OPL3 mode"
    );

    let mut on = OplChip::default();
    write_secondary(&mut on, 0x05, 0x01); // NEW: enable OPL3 mode
    setup(&mut on);
    assert!(
        peak(&mut on) > 1000,
        "secondary channel sounds in OPL3 mode"
    );
}

#[test]
fn opl3_panning_routes_the_channel_to_selected_outputs() {
    // reg 0xC0 bit4 = left, bit5 = right; neither set leaves the channel mute.
    let peaks = |c0: u8| {
        let mut opl = OplChip::default();
        write_secondary(&mut opl, 0x05, 0x01); // NEW
        write_secondary(&mut opl, 0x23, 0x21); // carrier sustained, multiple x1
        write_secondary(&mut opl, 0x43, 0x00);
        write_secondary(&mut opl, 0x63, 0xf0); // instant attack
        write_secondary(&mut opl, 0x83, 0x00);
        write_secondary(&mut opl, 0xa0, 0x00);
        write_secondary(&mut opl, 0xc0, c0);
        write_secondary(&mut opl, 0xb0, 0x20 | (4 << 2) | 0x02);
        let (mut lpk, mut rpk) = (0, 0);
        for _ in 0..256 {
            let (l, r) = opl.render_sample();
            lpk = lpk.max(l.abs());
            rpk = rpk.max(r.abs());
        }
        (lpk, rpk)
    };
    assert!(
        matches!(peaks(0x11), (l, 0) if l > 1000),
        "additive, left only"
    );
    assert!(
        matches!(peaks(0x21), (0, r) if r > 1000),
        "additive, right only"
    );
    assert!(
        matches!(peaks(0x31), (l, r) if l > 1000 && r > 1000),
        "both"
    );
    assert_eq!(peaks(0x01), (0, 0), "no pan bits: silent");
}

#[test]
fn four_op_mode_consumes_the_secondary_channel() {
    // Program channel 3 as a loud keyed tone (operators 6 and 9). Enabling
    // 4-op for pair 0/3 hands those operators to channel 0's 4-op voice,
    // which is unkeyed here, so the previously audible tone goes silent.
    let setup_channel3 = |opl: &mut OplChip| {
        write_secondary(opl, 0x05, 0x01); // OPL3 mode
        opl.write_register(0x2b, 0x21); // op 9 (slot 11): sustained, multiple x1
        opl.write_register(0x4b, 0x00); // op 9 total level 0
        opl.write_register(0x6b, 0xf0); // op 9 attack 15
        opl.write_register(0x8b, 0x00);
        opl.write_register(0xa3, 0x00); // channel 3 f-number low
        opl.write_register(0xc3, 0x31); // additive + left/right
        opl.write_register(0xb3, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
    };
    let peak = |opl: &mut OplChip| (0..256).map(|_| opl.render_sample().0.abs()).max().unwrap();

    let mut two_op = OplChip::default();
    setup_channel3(&mut two_op);
    assert!(
        peak(&mut two_op) > 1000,
        "channel 3 sounds as an independent 2-op"
    );

    let mut four_op = OplChip::default();
    setup_channel3(&mut four_op);
    write_secondary(&mut four_op, 0x04, 0x01); // enable 4-op for pair 0/3
    assert_eq!(peak(&mut four_op), 0, "4-op mode consumes channel 3");
}

#[test]
fn four_op_algorithms_produce_different_timbres() {
    // A keyed 4-op voice on channel 0 with all operators loud; the four
    // connection settings route the operators differently.
    let collect = |cnt1: u8, cnt2: u8| {
        let mut opl = OplChip::default();
        write_secondary(&mut opl, 0x05, 0x01); // OPL3 mode
        write_secondary(&mut opl, 0x04, 0x01); // 4-op for pair 0/3
        for slot in [0, 3, 8, 11] {
            // operators 0, 3, 6, 9: loud, instant attack, multiple x1
            opl.write_register(0x20 + slot, 0x01);
            opl.write_register(0x40 + slot, 0x00);
            opl.write_register(0x60 + slot, 0xf0);
            opl.write_register(0x80 + slot, 0x00);
        }
        opl.write_register(0xa0, 0x00); // channel 0 f-number low
        opl.write_register(0xc0, 0x30 | cnt1); // pan + connection bit 1
        opl.write_register(0xc3, cnt2); // secondary connection bit
        opl.write_register(0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
        (0..128).map(|_| opl.render_sample().0).collect::<Vec<_>>()
    };
    let fm_fm = collect(0, 0);
    assert!(fm_fm.iter().any(|&s| s != 0), "the 4-op voice is audible");
    assert_ne!(fm_fm, collect(0, 1), "FM-FM differs from FM-AM");
    assert_ne!(fm_fm, collect(1, 0), "FM-FM differs from AM-FM");
    assert_ne!(fm_fm, collect(1, 1), "FM-FM differs from AM-AM");
}

#[test]
fn tremolo_dips_the_amplitude_when_enabled() {
    // AM on a steady loud carrier swings the peak by ~4.8 dB (a ~1.74x
    // factor) over the 3.7 Hz cycle; without AM the amplitude is constant.
    let peaks = |am: u8, dam: u8| {
        let mut opl = OplChip::default();
        opl.write_register(0x23, 0x20 | am | 0x01); // carrier: sustained (+AM), mult x1
        opl.write_register(0x43, 0x00);
        opl.write_register(0x63, 0xf0); // instant attack
        opl.write_register(0x83, 0x00);
        opl.write_register(0xbd, dam); // tremolo depth (bit7)
        opl.write_register(0xa0, 0x00);
        opl.write_register(0xc0, 0x01); // additive
        opl.write_register(0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
        let (mut lo, mut hi) = (i32::MAX, 0);
        for _ in 0..(TREMOLO_PERIOD / 128) {
            let peak = (0..128).map(|_| opl.render_sample().0.abs()).max().unwrap();
            lo = lo.min(peak);
            hi = hi.max(peak);
        }
        (lo, hi)
    };

    let (lo, hi) = peaks(0x80, 0x80); // AM on, DAM=1 (4.8 dB)
    let ratio = f64::from(hi) / f64::from(lo);
    assert!(
        (ratio - 1.74).abs() < 0.2,
        "expected ~4.8 dB swing, got {ratio}"
    );

    let (lo, hi) = peaks(0x00, 0x80); // AM off
    assert_eq!(lo, hi, "no tremolo without AM");
}

#[test]
fn vibrato_wobbles_the_pitch_when_enabled() {
    // VIB bends the carrier frequency over the 6.1 Hz cycle, so the rendered
    // waveform diverges from a steady-pitch one; without VIB it is identical.
    let render = |vib: u8, dvb: u8| {
        let mut opl = OplChip::default();
        opl.write_register(0x20, vib | 0x01); // modulator: VIB (bit6) + multiple x1
        opl.write_register(0x40, 0x00); // modulator loud
        opl.write_register(0x60, 0xf0); // instant attack
        opl.write_register(0x80, 0x00);
        opl.write_register(0x63, 0x00); // carrier attack 0 -> silent
        opl.write_register(0xbd, dvb); // vibrato depth (bit6)
        opl.write_register(0xc0, 0x01); // additive
        opl.write_register(0xa0, 0x00);
        opl.write_register(0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
        (0..4096).map(|_| opl.render_sample().0).collect::<Vec<_>>()
    };
    assert_eq!(
        render(0x00, 0x40),
        render(0x00, 0x00),
        "no vibrato when VIB is off"
    );
    assert_ne!(
        render(0x40, 0x40),
        render(0x00, 0x40),
        "VIB bends the pitch"
    );
}

#[test]
fn vibrato_bends_the_fnumber_to_the_full_step_depth() {
    // One sample of phase advance equals the bent F-number's increment.
    // block 4, multiple x1: increment = fnum << 4. Deep vibrato adds
    // fnum>>7 at the peak (phase 2) and fnum>>8 at the half-steps; the
    // shallow setting is one bit weaker. (Regression: depth was halved.)
    let fnum = 0x200u32;
    let advance = |phase: u8, deep: bool| {
        let mut op = sine_operator(fnum as u16, 4, 0);
        op.set_vibrato(true);
        op.advance_with_lfo(phase, deep);
        op.phase
    };
    assert_eq!(advance(0, true), fnum << 4, "phase 0: no bend");
    assert_eq!(
        advance(2, true),
        (fnum + (fnum >> 7)) << 4,
        "deep peak = +fnum>>7"
    );
    assert_eq!(
        advance(6, true),
        (fnum - (fnum >> 7)) << 4,
        "deep trough = -fnum>>7"
    );
    assert_eq!(
        advance(1, true),
        (fnum + (fnum >> 8)) << 4,
        "deep half-step = +fnum>>8"
    );
    assert_eq!(
        advance(2, false),
        (fnum + (fnum >> 8)) << 4,
        "shallow peak = +fnum>>8"
    );
}

// Program a percussion operator (by register slot) loud and sustained.
fn loud_rhythm_op(opl: &mut OplChip, slot: usize) {
    opl.write_register((0x20 + slot) as u8, 0x21); // sustained, multiple x1
    opl.write_register((0x40 + slot) as u8, 0x00); // total level 0
    opl.write_register((0x60 + slot) as u8, 0xf0); // attack 15
    opl.write_register((0x80 + slot) as u8, 0x00);
}

#[test]
fn rhythm_mode_keys_instruments_via_the_0xbd_register() {
    let mut opl = OplChip::default();
    for slot in [16, 17, 18, 19, 20, 21] {
        loud_rhythm_op(&mut opl, slot); // operators 12..17
    }
    for ch in [6u8, 7, 8] {
        opl.write_register(0xa0 + ch, 0x00);
        opl.write_register(0xb0 + ch, (4 << 2) | 0x02); // block 4, fnum 0x200, no melodic key
        opl.write_register(0xc0 + ch, 0x00);
    }
    opl.write_register(0xbd, 0x3f); // rhythm + all five instruments on
    let peak = (0..1024)
        .map(|_| opl.render_sample().0.abs())
        .max()
        .unwrap();
    assert!(peak > 1000, "percussion sounds, peak {peak}");
}

#[test]
fn rhythm_bass_drum_sounds_as_a_two_op_voice() {
    let mut opl = OplChip::default();
    loud_rhythm_op(&mut opl, 19); // op 15 = channel 6 carrier
    opl.write_register(0xa6, 0x00);
    opl.write_register(0xc6, 0x00); // FM, modulator silent -> clean carrier
    opl.write_register(0xb6, (4 << 2) | 0x02); // block 4, fnum 0x200, no melodic key
    opl.write_register(0xbd, 0x20 | 0x10); // rhythm + bass drum on
    let peak = (0..256).map(|_| opl.render_sample().0.abs()).max().unwrap();
    assert!(peak > 1000, "bass drum sounds, peak {peak}");
}

#[test]
fn rhythm_mode_silences_the_melodic_channel() {
    // Channel 6 plays a melodic tone; enabling rhythm (bass drum unkeyed)
    // replaces those operators, so the channel falls silent.
    let melodic_peak = |bd: u8| {
        let mut opl = OplChip::default();
        loud_rhythm_op(&mut opl, 19); // op 15 = channel 6 carrier
        opl.write_register(0xa6, 0x00);
        opl.write_register(0xc6, 0x00); // FM
        opl.write_register(0xb6, 0x20 | (4 << 2) | 0x02); // melodic KEY-ON, block 4
        opl.write_register(0xbd, bd);
        (0..256).map(|_| opl.render_sample().0.abs()).max().unwrap()
    };
    assert!(
        melodic_peak(0x00) > 1000,
        "channel 6 tone sounds without rhythm"
    );
    assert_eq!(
        melodic_peak(0x20),
        0,
        "rhythm mode silences melodic channel 6"
    );
}

#[test]
fn waveforms_above_three_require_opl3_mode() {
    // E0=6 is a square wave (has negative samples) in OPL3 mode, but masks
    // to waveform 2 (abs sine, non-negative) when the chip is an OPL2.
    let has_negative = |new: bool| {
        let mut opl = OplChip::default();
        opl.write_register(0x01, 0x20); // WSEnable
        if new {
            write_secondary(&mut opl, 0x05, 0x01); // OPL3 mode
        }
        opl.write_register(0x20, 0x01); // modulator multiple x1
        opl.write_register(0x40, 0x00); // modulator loud
        opl.write_register(0x60, 0xf0); // modulator instant attack
        opl.write_register(0x80, 0x00);
        opl.write_register(0x63, 0x00); // carrier attack 0 -> silent
        opl.write_register(0xe0, 0x06); // modulator waveform 6 (square / abs sine)
        opl.write_register(0xc0, 0x31); // additive + left/right (pan ignored as OPL2)
        opl.write_register(0xa0, 0x00);
        opl.write_register(0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
        (0..256).any(|_| opl.render_sample().0 < 0)
    };
    assert!(!has_negative(false), "OPL2 masks waveform 6 to abs sine");
    assert!(has_negative(true), "OPL3 waveform 6 is a square wave");
}

#[test]
fn waveform_select_requires_wsenable() {
    // A half-sine (E0=1) silences the wave's negative half, but only when
    // WSEnable (reg 0x01 bit5) is set; otherwise the chip forces a full sine.
    let has_negative = |wse: u8| {
        let mut opl = OplChip::default();
        opl.write_register(0x01, wse); // 0x20 = WSEnable, 0x00 = off
        opl.write_register(0x20, 0x01); // modulator multiple x1
        opl.write_register(0x40, 0x00); // modulator loud
        opl.write_register(0x60, 0xf0); // modulator instant attack
        opl.write_register(0x80, 0x00);
        opl.write_register(0x63, 0x00); // carrier attack 0 -> stays silent
        opl.write_register(0xe0, 0x01); // modulator waveform: half-sine
        opl.write_register(0xc0, 0x01); // additive
        opl.write_register(0xa0, 0x00);
        opl.write_register(0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
        (0..256).any(|_| opl.render_sample().0 < 0)
    };
    assert!(
        has_negative(0x00),
        "WSEnable off forces a full sine (has negatives)"
    );
    assert!(
        !has_negative(0x20),
        "WSEnable on lets half-sine silence negatives"
    );
}

#[test]
fn adlib_detection_sequence_reports_present() {
    // The canonical AdLib probe: clear the timers, confirm the status
    // flags are quiet, fire timer 1, let it overflow, and confirm the
    // status port reports the IRQ + timer-1 flags (0xc0).
    let mut opl = OplChip::default();

    opl.write_register(0x04, 0x60); // mask both timers
    opl.write_register(0x04, 0x80); // reset the IRQ flags
    assert_eq!(opl.status() & 0xe0, 0x00, "flags clear after reset");

    opl.write_register(0x02, 0xff); // timer 1 preset: overflow in one step
    opl.write_register(0x04, 0x21); // start timer 1, mask timer 2
    opl.advance_micros(80); // one 80us timer-1 step -> overflow

    assert_eq!(
        opl.status() & 0xe0,
        0xc0,
        "timer 1 overflow raises IRQ (bit7) + timer-1 (bit6)"
    );
}

#[test]
fn masked_timer_overflow_sets_flag_but_not_irq() {
    // A masked timer still records its overflow flag in the status byte;
    // only the IRQ (bit7) is suppressed. Faithful to real OPL silicon.
    let mut opl = OplChip::default();

    opl.write_register(0x03, 0xff); // timer 2 preset
    opl.write_register(0x04, 0x22); // start timer 2 (bit1) with it masked (bit5)
    opl.advance_micros(320); // one 320us timer-2 step -> overflow

    let status = opl.status();
    assert_eq!(status & 0x20, 0x20, "timer-2 flag is set");
    assert_eq!(status & 0x80, 0x00, "masked timer raises no IRQ");
}

#[test]
fn timer_control_bit7_reset_leaves_a_running_timer_running() {
    // Register 0x04 bit7 is IRQ-RESET, and the YM3812/YMF262 register map
    // states that when it is set the remaining bits of the write are ignored.
    // A detection routine that clears the overflow flag between polls must
    // therefore find timer 1 still counting afterwards.
    let mut opl = OplChip::default();

    opl.write_register(0x02, 0xff); // timer 1 preset: overflow in one step
    opl.write_register(0x04, 0x21); // start timer 1, mask timer 2
    opl.advance_micros(80);
    assert_eq!(
        opl.status() & 0xe0,
        0xc0,
        "first overflow raises IRQ + flag"
    );

    opl.write_register(0x04, 0x80); // IRQ-RESET only
    assert_eq!(opl.status() & 0xe0, 0x00, "bit7 clears both overflow flags");
    assert!(
        opl.timers_running(),
        "an IRQ-RESET write must not stop a running timer"
    );

    opl.advance_micros(80);
    assert_eq!(
        opl.status() & 0xe0,
        0xc0,
        "timer 1 kept counting across the flag reset"
    );
}

#[test]
fn timer_control_bit7_reset_preserves_the_start_and_mask_bits() {
    // The other half of "all other bits are ignored": the mask bits the
    // status byte gates its IRQ on must survive an IRQ-RESET write. Storing
    // 0x80 verbatim would clear both masks and turn a deliberately masked
    // timer into one that asserts IRQ.
    let mut opl = OplChip::default();

    opl.write_register(0x03, 0xff); // timer 2 preset
    opl.write_register(0x04, 0x22); // start timer 2 with timer 2 masked (bit5)
    opl.advance_micros(320);
    assert_eq!(
        opl.status() & 0xa0,
        0x20,
        "flag set, IRQ suppressed by mask"
    );

    opl.write_register(0x04, 0x80); // IRQ-RESET only
    assert_eq!(
        opl.register(0x04),
        0x22,
        "an IRQ-RESET write leaves the start/mask bits at their prior value"
    );

    opl.advance_micros(320);
    let status = opl.status();
    assert_eq!(status & 0x20, 0x20, "timer 2 overflowed again");
    assert_eq!(
        status & 0x80,
        0x00,
        "timer 2 is still masked after the flag reset"
    );
}

#[test]
fn adlib_detection_poll_loop_measures_the_timer_1_period() {
    // A detection-style probe: preset timer 1, start it, then poll the status
    // port in short slices and clear the flag after each sighting, the way
    // period software measures the interval between overflows. Timer 1 counts
    // in 80 us steps and reloads from the preset, so with a preset of 0xff the
    // overflows must land on an exact 80 us cadence.
    let mut opl = OplChip::default();
    opl.write_register(0x02, 0xff);
    opl.write_register(0x04, 0x21);

    let mut overflow_micros = Vec::new();
    let mut elapsed = 0u64;
    while overflow_micros.len() < 3 && elapsed < 1_000 {
        opl.advance_micros(5);
        elapsed += 5;
        if opl.status() & 0x40 != 0 {
            overflow_micros.push(elapsed);
            opl.write_register(0x04, 0x80); // clear the flag, keep timing
        }
    }

    assert_eq!(
        overflow_micros,
        vec![80, 160, 240],
        "timer 1 must keep its 80 us period across mid-loop flag resets"
    );
}

#[test]
fn address_data_ports_store_registers() {
    let mut opl = OplChip::default();
    assert!(opl.write_port(0x388, 0x20)); // latch register address 0x20
    assert!(opl.write_port(0x389, 0x2f)); // write the data
    assert_eq!(opl.register(0x20), 0x2f);
    assert_eq!(opl.read_port(0x388), Some(opl.status()));
}

#[test]
fn selected_register_reports_the_latched_address_per_bank() {
    // The bus needs this to tell WHICH register a data-port write lands in,
    // because a data write does not carry the index itself. Per bank: the two
    // latches are independent, and confusing them would misattribute every
    // OPL3 secondary-bank write.
    let mut opl = OplChip::default();
    opl.write_port(0x0388, 0xb0);
    opl.write_port(0x038a, 0x04);
    assert_eq!(opl.selected_register(0), 0xb0);
    assert_eq!(opl.selected_register(1), 0x04);
    // A data write leaves the latch alone, so a run of writes to one register
    // keeps reporting that register.
    opl.write_port(0x0389, 0x20);
    assert_eq!(opl.selected_register(0), 0xb0);
}
