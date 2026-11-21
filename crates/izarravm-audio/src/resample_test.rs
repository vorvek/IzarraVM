// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::f64::consts::PI;

const IN_HZ: f64 = 49_716.0;
const OUT_HZ: f64 = 44_100.0;

// A one-second stereo sine at `freq` (input rate), amplitude `amp`.
fn sine(freq: f64, amp: f64) -> Vec<(i32, i32)> {
    (0..IN_HZ as usize)
        .map(|n| {
            let s = (amp * (2.0 * PI * freq * n as f64 / IN_HZ).sin()).round() as i32;
            (s, s)
        })
        .collect()
}

fn resample(freq: f64, amp: f64) -> Vec<(i32, i32)> {
    Resampler::new(49716, 44100).process(&sine(freq, amp))
}

fn steady_peak(out: &[(i32, i32)]) -> i32 {
    out[1000..out.len() - 1000]
        .iter()
        .map(|f| f.0.abs())
        .max()
        .unwrap()
}

#[test]
fn produces_the_target_output_rate() {
    let out = Resampler::new(49716, 44100).process(&vec![(0, 0); 49716]);
    let expected = OUT_HZ as i32;
    assert!(
        (out.len() as i32 - expected).abs() < 50,
        "got {} frames, expected ~{expected}",
        out.len()
    );
}

#[test]
fn preserves_a_passband_tone() {
    let amp = 10_000.0;
    let out = resample(1000.0, amp);
    // Amplitude is preserved (unity gain).
    let peak = steady_peak(&out);
    assert!(
        (f64::from(peak) - amp).abs() < amp * 0.05,
        "peak {peak} vs {amp}"
    );
    // Frequency is unchanged at the new rate.
    let mid = &out[1000..out.len() - 1000];
    let crossings = mid.windows(2).filter(|w| w[0].0 <= 0 && w[1].0 > 0).count();
    let measured = crossings as f64 * OUT_HZ / mid.len() as f64;
    assert!((measured - 1000.0).abs() < 5.0, "measured {measured} Hz");
}

#[test]
fn attenuates_content_above_the_output_nyquist() {
    // 24500 Hz is below the input Nyquist (24858) but well above the output
    // Nyquist (22050); without band-limiting it would alias to ~19600 Hz.
    let amp = 10_000.0;
    let passband = f64::from(steady_peak(&resample(1000.0, amp)));
    let stopband = f64::from(steady_peak(&resample(24500.0, amp)));
    assert!(
        stopband < passband * 0.1,
        "above-Nyquist tone should be filtered: stop {stopband} vs pass {passband}"
    );
}

#[test]
fn streaming_matches_single_shot() {
    let input = sine(1000.0, 10_000.0);
    let whole = Resampler::new(49716, 44100).process(&input);
    let mut split_rs = Resampler::new(49716, 44100);
    let mut split = split_rs.process(&input[..20_000]);
    split.extend(split_rs.process(&input[20_000..]));
    assert_eq!(whole, split, "chunked input must match one-shot");
}

#[test]
fn stream_resampler_returns_exactly_the_frames_process_returns() {
    for (input, output) in [
        (44_100, 48_000),
        (44_100, 96_000),
        (44_100, 192_000),
        (44_100, 22_050),
        (49_716, 44_100),
    ] {
        let input_frames: Vec<(i32, i32)> = (0..5_000)
            .map(|n| ((n * 37 % 20_001) - 10_000, (n * 91 % 20_001) - 10_000))
            .collect();
        let expected = Resampler::new(input, output).process(&input_frames);
        let mut stream = StreamResampler::new(input, output);
        let mut actual = Vec::new();
        let mut fed = input_frames.iter();
        while actual.len() < expected.len() {
            match stream.pull() {
                Some(frame) => actual.push(frame),
                None => stream.push(*fed.next().expect("input ran out")),
            }
        }
        assert_eq!(actual, expected, "{input} -> {output}");
    }
}

#[test]
fn process_into_appends_and_matches_process() {
    let input: Vec<(i32, i32)> = (0..3_000).map(|n| (n % 977, -(n % 613))).collect();
    let expected = Resampler::new(44_100, 48_000).process(&input);
    let mut resampler = Resampler::new(44_100, 48_000);
    let mut out = vec![(7, 7)];
    resampler.process_into(&input[..1_000], &mut out);
    resampler.process_into(&input[1_000..], &mut out);
    assert_eq!(out[0], (7, 7));
    assert_eq!(&out[1..], &expected[..]);
}
