// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn enabled_membrane_toggles_with_ch2_out() {
    let mut spk = Speaker::default();
    spk.write_control(0x03); // gate + data enable
    spk.accumulate(0.001, true, std::iter::empty::<(f64, bool)>()); // ~44 samples high
    spk.accumulate(0.001, false, std::iter::empty::<(f64, bool)>()); // ~44 samples low
    let s = spk.drain(88);
    assert!(s.iter().any(|&v| v > 0), "high half produced +AMP");
    assert!(s.iter().any(|&v| v < 0), "low half produced -AMP");
}

#[test]
fn disabled_speaker_is_silent() {
    let mut spk = Speaker::default(); // data_enable false
    spk.accumulate(0.01, true, std::iter::empty::<(f64, bool)>()); // OUT high but disabled
    assert!(spk.drain(100).iter().all(|&v| v == 0));
}

#[test]
fn drain_pads_with_zero_on_underrun() {
    let mut spk = Speaker::default();
    spk.write_control(0x03);
    spk.accumulate(0.0001, true, std::iter::empty::<(f64, bool)>()); // ~4 samples
    let s = spk.drain(50);
    assert_eq!(s.len(), 50);
    assert!(s[40..].iter().all(|&v| v == 0));
}

#[test]
fn sub_sample_pulse_width_changes_the_sample() {
    let mut short = Speaker::default();
    short.write_control(0x03);
    short.accumulate(
        SAMPLE_SECONDS,
        false,
        [
            (SAMPLE_SECONDS * 0.25, true),
            (SAMPLE_SECONDS * 0.50, false),
        ],
    );

    let mut long = Speaker::default();
    long.write_control(0x03);
    long.accumulate(SAMPLE_SECONDS, false, [(SAMPLE_SECONDS * 0.25, true)]);

    let short = short.drain(1)[0];
    let long = long.drain(1)[0];
    assert!(
        short < 0,
        "short high pulse should average low, got {short}"
    );
    assert!(long > 0, "long high pulse should average high, got {long}");
    assert!(long > short, "pulse width must affect the rendered sample");
}

#[test]
fn ever_enabled_latches_on_first_enable() {
    let mut spk = Speaker::default();
    assert!(!spk.ever_enabled());
    spk.write_control(0x01); // gate only, data enable off
    assert!(!spk.ever_enabled());
    spk.write_control(0x03); // data enable on
    assert!(spk.ever_enabled());
    spk.write_control(0x00); // off again, but the latch stays set
    assert!(spk.ever_enabled());
}
