// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn millis(value: u64) -> u64 {
    izarravm_core::MASTER_CLOCK_HZ / 1000 * value
}

#[test]
fn enabled_membrane_toggles_with_ch2_out() {
    let mut spk = Speaker::default();
    spk.write_control(0x03); // gate + data enable
    spk.accumulate(millis(1), true, std::iter::empty::<(u64, bool)>());
    spk.accumulate(millis(1), false, std::iter::empty::<(u64, bool)>());
    let s = spk.drain(88);
    assert!(s.iter().any(|&v| v > 0), "high half produced +AMP");
    assert!(s.iter().any(|&v| v < 0), "low half produced -AMP");
}

/// The membrane's swing is 86Box's, peak to peak.
///
/// Every other speaker level assertion in the tree is a RATIO -- the PC-SPK
/// positions against each other, the card path against the cardless one, an
/// ultrasonic tone against an audible one -- and every one of them is invariant
/// under this constant, by construction. That is what a level test should be,
/// and it is also why nothing caught the beeper resting 9.88 dB above 86Box's
/// for as long as it did. This is the one assertion that pins the absolute
/// number, so the reference the routing note in `timing.rs` derives against is
/// a fact the tree checks rather than a comment it carries.
///
/// 5120 peak to peak is 86Box's mode-3 beeper: `snd_speaker.c` swings
/// 0..0x1400. The constant is half of that because this membrane is bipolar,
/// so a toggling square wave carries no DC bias.
///
/// Both halves are asserted and they fail independently. The constant alone
/// would pass on a build where nothing read it; the rendered levels alone are
/// the voicing chain's output rather than the membrane's, and would let the
/// reference drift without saying which of the two moved.
#[test]
fn membrane_swing_matches_the_86box_beeper_peak_to_peak() {
    assert_eq!(
        SPEAKER_AMPLITUDE * 2,
        5120,
        "the membrane's peak-to-peak swing is 86Box's 0..0x1400"
    );

    // And it reaches the output. A segment held at one level settles to the
    // membrane's own amplitude through the fixed voicing chain -- the cone
    // biquads pass DC at unity, the case ambience adds its wet path -- so the
    // rendered level is a constant multiple of the constant above, and the two
    // directions are symmetric because the swing is.
    let held = |level: bool| {
        let mut spk = Speaker::default();
        spk.write_control(0x03); // gate + data enable
        spk.accumulate(millis(20), level, std::iter::empty::<(u64, bool)>());
        *spk.drain(880).last().expect("a settled sample")
    };
    let high = held(true);
    let low = held(false);
    assert_eq!(high, 3121, "a membrane held high settles here");
    assert_eq!(low, -3121, "and held low, at the mirror of it: no DC bias");
}

#[test]
fn disabled_speaker_is_silent() {
    let mut spk = Speaker::default(); // data_enable false
    spk.accumulate(millis(10), true, std::iter::empty::<(u64, bool)>());
    assert!(spk.drain(100).iter().all(|&v| v == 0));
}

#[test]
fn drain_pads_with_zero_on_underrun() {
    let mut spk = Speaker::default();
    spk.write_control(0x03);
    spk.accumulate(millis(1) / 10, true, std::iter::empty::<(u64, bool)>());
    let s = spk.drain(50);
    assert_eq!(s.len(), 50);
    assert!(s[40..].iter().all(|&v| v == 0));
}

#[test]
fn sub_sample_pulse_width_changes_the_sample() {
    let mut short = Speaker::default();
    short.write_control(0x03);
    let sample_ticks = izarravm_core::MASTER_CLOCK_HZ.div_ceil(u64::from(DAC_HZ));
    short.accumulate(
        sample_ticks,
        false,
        [(sample_ticks / 4, true), (sample_ticks / 2, false)],
    );

    let mut long = Speaker::default();
    long.write_control(0x03);
    long.accumulate(sample_ticks, false, [(sample_ticks / 4, true)]);

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
