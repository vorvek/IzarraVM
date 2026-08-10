// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn disabled_mix_bypasses_opl_and_discards_voice_and_cd() {
    let config = SoundBlasterConfig {
        enabled: false,
        ..SoundBlasterConfig::default()
    };
    let mut path = Sb16Path::new(&config);

    assert_eq!(
        path.mix_snapshot()
            .mix_opl_voice((16_777_217, -16_777_217), (123, 456)),
        (16_777_217, -16_777_217)
    );
    assert_eq!(path.mix_snapshot().mix_cd((32_767, -32_768)), (0, 0));
    assert_eq!(path.cd_levels(), (0, 0));
    path.set_linked_cd_level(31);
    assert_eq!(path.cd_levels(), (0, 0));
}

/// A window that produces nothing must still be served real audio out of the
/// carry-over queue, not a repeated frame.
///
/// `render_voice`'s input count comes from elapsed guest master ticks (bursty,
/// because the emulation thread runs in chunks) while its output count comes
/// from the OPL resampler (smooth, host-paced). The two disagree every window.
/// Discarding the surplus and repeating a frame for the shortfall is what tore
/// the stream: a real Quake capture showed ~14k frames dropped and ~14k
/// repeated per second against 44.1k rendered, audible as a crackle on every
/// DSP title regardless of rate or DMA width.
#[test]
fn render_voice_serves_a_dry_window_from_carry_over_not_a_held_frame() {
    let config = SoundBlasterConfig {
        enabled: true,
        ..SoundBlasterConfig::default()
    };
    let mut path = Sb16Path::new(&config);
    // 22050 Hz, 16-bit signed stereo, auto-init output.
    for byte in [0x41u8, 0x56, 0x22, 0xB6, 0x30, 0xFF, 0x00] {
        path.write_port(0x22C, byte);
    }
    let active = path.active.as_mut().expect("sound blaster enabled");
    assert!(active.dsp.needs_output_tick(), "DSP programmed for output");

    // A rising ramp, so consecutive frames differ and a held frame is obvious.
    let mut sample: i16 = 0;
    active.dsp.tick_n_samples(
        4096,
        || None,
        || {
            sample = sample.wrapping_add(64);
            Some(sample as u16)
        },
    );

    // Window 1 over-produces: a big native drain against a small demand. The
    // surplus must be kept.
    let first = path.render_voice(Sb16RenderWindow {
        elapsed_master_ticks: 0,
        fallback_opl_samples: 2048,
        output_frames: 64,
    });
    assert_eq!(first.len(), 64);
    let carried = path.active.as_ref().unwrap().pending.len();
    assert!(
        carried > 0,
        "surplus must be carried, not discarded (pending={carried})"
    );

    // Window 2 produces nothing at all. Every frame has to come from carry-over.
    let second = path.render_voice(Sb16RenderWindow {
        elapsed_master_ticks: 0,
        fallback_opl_samples: 0,
        output_frames: 64,
    });
    assert_eq!(second.len(), 64);
    let distinct = second
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len();
    assert!(
        distinct > 1,
        "a dry window served from carry-over must contain real audio, not one \
         repeated frame (distinct frames: {distinct})"
    );
}

/// Arm the SB16 for 22050 Hz 16-bit signed stereo auto-init output (DSP 0xB6,
/// mode 0x30) -- exactly what Duke Nukem 3D programs -- and push `frames`
/// stereo frames of `(left, right)` through the DMA word fetch.
fn feed_16bit_stereo(path: &mut Sb16Path, frames: usize, left: i16, right: i16) {
    for byte in [0x41u8, 0x56, 0x22, 0xB6, 0x30, 0xFF, 0x00] {
        path.write_port(0x22C, byte);
    }
    let active = path.active.as_mut().expect("sound blaster enabled");
    assert!(active.dsp.is_stereo(), "0xB6 mode 0x30 is stereo");
    let mut phase = 0usize;
    active.dsp.tick_n_samples(
        frames,
        || None,
        || {
            let word = if phase.is_multiple_of(2) { left } else { right };
            phase += 1;
            Some(word as u16)
        },
    );
}

/// Drain one steady-state window of the voice path, mixed through the CT1745
/// snapshot with a silent OPL leg -- i.e. the DSP-only signal as the final mix
/// in `render_audio` sees it. The first window is discarded so the resampler's
/// startup transient does not contaminate the reading.
fn steady_voice_window(path: &mut Sb16Path) -> Vec<(i32, i32)> {
    path.render_voice(Sb16RenderWindow {
        elapsed_master_ticks: 0,
        fallback_opl_samples: 1024,
        output_frames: 256,
    });
    let frames = path.render_voice(Sb16RenderWindow {
        elapsed_master_ticks: 0,
        fallback_opl_samples: 1024,
        output_frames: 256,
    });
    let mix = path.mix_snapshot();
    frames
        .into_iter()
        .map(|voice| mix.mix_opl_voice((0, 0), voice))
        .collect()
}

fn enabled_path() -> Sb16Path {
    Sb16Path::new(&SoundBlasterConfig {
        enabled: true,
        ..SoundBlasterConfig::default()
    })
}

/// A left-only 16-bit stereo stream must leave the right output silent.
///
/// Duke Nukem 3D's SETUP plays its left-channel and right-channel tests through
/// exactly this mode (DSP 0xB6, mode 0x30 on DMA5). Both tests were reported as
/// CENTERED but quieter, which is the signature of L and R being averaged into
/// both outputs.
#[test]
fn left_only_16bit_stereo_leaves_the_right_output_silent() {
    let mut path = enabled_path();
    feed_16bit_stereo(&mut path, 8192, 20_000, 0);
    let out = steady_voice_window(&mut path);

    let peak_l = out.iter().map(|f| f.0.abs()).max().unwrap_or(0);
    let peak_r = out.iter().map(|f| f.1.abs()).max().unwrap_or(0);
    assert!(
        peak_l > 1000,
        "the left channel must carry the signal (peak_l={peak_l})"
    );
    assert!(
        peak_r < peak_l / 100,
        "a left-only stream must not leak into the right output \
         (peak_l={peak_l} peak_r={peak_r})"
    );
}

/// The mirror image of the left-only case: asserting only one direction would
/// pass on a path that hard-wires the right output to the left input.
#[test]
fn right_only_16bit_stereo_leaves_the_left_output_silent() {
    let mut path = enabled_path();
    feed_16bit_stereo(&mut path, 8192, 0, 20_000);
    let out = steady_voice_window(&mut path);

    let peak_l = out.iter().map(|f| f.0.abs()).max().unwrap_or(0);
    let peak_r = out.iter().map(|f| f.1.abs()).max().unwrap_or(0);
    assert!(
        peak_r > 1000,
        "the right channel must carry the signal (peak_r={peak_r})"
    );
    assert!(
        peak_l < peak_r / 100,
        "a right-only stream must not leak into the left output \
         (peak_l={peak_l} peak_r={peak_r})"
    );
}

/// The CT1745 legs must stay at unity through this path.
///
/// The headroom that keeps the summed mix off the clamp is reserved once on the
/// machine's summing node (`MIX_HEADROOM`), NOT here -- putting it in a per-leg
/// gain would shift the FM/voice/CD balance the volume-decode fix established.
/// This pins that placement: the voice leg alone still arrives at full scale,
/// and `machine_audio_test` asserts the headroom downstream.
#[test]
fn the_voice_leg_alone_is_unity_at_power_on_defaults() {
    let mut path = enabled_path();
    feed_16bit_stereo(&mut path, 8192, i16::MAX, i16::MAX);
    let out = steady_voice_window(&mut path);

    let peak = out
        .iter()
        .map(|f| f.0.abs().max(f.1.abs()))
        .max()
        .unwrap_or(0);
    assert_eq!(
        peak,
        i32::from(i16::MAX),
        "voice 0x32/0x33, master 0x30/0x31 and outgain 0x41/0x42 all power on \
         at 0 dB, so the voice leg is unity here and headroom is downstream"
    );
}

/// The relative FM / voice / CD balance must be exactly what the volume-decode
/// fix established.
///
/// That balance is the fix's whole achievement: before it, registers `0x34`/`0x35`
/// were inert, so the FM bus took no attenuation at all while the voice took
/// `0x32`/`0x33` on top of the master, and Duke Nukem 3D's music ran far over its
/// effects. Reserving headroom must not claw any of that back, which is the
/// reason it is a single post-sum scalar rather than a per-leg trim.
#[test]
fn the_headroom_placement_leaves_the_fm_voice_cd_balance_untouched() {
    let mut path = enabled_path();
    let mix = path.mix_snapshot();

    // At power-on defaults every leg is 0 dB, so equal input must contribute
    // equally: FM and voice sit level with each other, not 14 dB apart.
    const SIGNAL: i32 = 10_000;
    let fm_only = mix.mix_opl_voice((SIGNAL, SIGNAL), (0, 0));
    let voice_only = mix.mix_opl_voice((0, 0), (SIGNAL, SIGNAL));
    assert_eq!(
        fm_only,
        (SIGNAL, SIGNAL),
        "the FM leg is unity at its power-on default"
    );
    assert_eq!(
        voice_only, fm_only,
        "FM and voice are level at power-on defaults"
    );
    path.set_linked_cd_level(31);
    assert_eq!(
        path.mix_snapshot().mix_cd((SIGNAL, SIGNAL)),
        (SIGNAL, SIGNAL),
        "the CD leg is unity at level 31, level with FM and voice"
    );

    // A guest attenuating one leg moves only that leg, by the decoded amount.
    // Level 21 is -20 dB (-62 + 2*21), i.e. a tenth.
    path.write_port(0x224, 0x34);
    path.write_port(0x225, 21 << 3);
    path.write_port(0x224, 0x35);
    path.write_port(0x225, 21 << 3);
    let mix = path.mix_snapshot();
    let (fm_l, _) = mix.mix_opl_voice((SIGNAL, SIGNAL), (0, 0));
    assert!(
        (fm_l - SIGNAL / 10).abs() <= 1,
        "0x34 at level 21 is -20 dB on the FM leg alone (got {fm_l})"
    );
    assert_eq!(
        mix.mix_opl_voice((0, 0), (SIGNAL, SIGNAL)),
        (SIGNAL, SIGNAL),
        "attenuating FM must not move the voice leg"
    );
}

/// What the reserve is worth, in legs -- the policy stated instead of implied.
///
/// `MIX_HEADROOM` is 0.5, exactly one bit, and every CT1745 leg powers on at
/// 0 dB. So the reserve is worth exactly ONE full-scale source, and the
/// behaviour at the summing node falls out of that:
///
/// - one leg at full scale lands at half scale, 6 dB of room left over
/// - TWO legs at full scale -- digital voice over FM music, which is Duke Nukem
///   3D's exact case and the report this branch came from -- land on the rail
///   EXACTLY: nothing to spare, and nothing clipped
/// - THREE legs at full scale clip, by design
///
/// The third row is a taste call, so it is pinned here rather than left to be
/// rediscovered. Covering it needs 9.5 dB rather than 6, and that extra 3.5 dB
/// would be paid by every title in the library to protect a case that needs
/// voice, FM and CD-audio all pegged at digital full scale in the same frame.
/// Music and effects together is the common case; music, effects and a Red Book
/// track all at maximum is not, and when it happens the guest has a mixer to
/// turn something down with. What must NOT happen is the two-leg case clipping,
/// which is the row above and the one with the bug report behind it.
#[test]
fn the_headroom_reserve_is_worth_exactly_one_full_scale_leg() {
    const FS: i32 = i16::MAX as i32;

    let mut path = enabled_path();
    path.set_linked_cd_level(31);
    let mix = path.mix_snapshot();

    // The summing node from `render_audio`: legs sum raw, then one scalar.
    let staged = |sum: i32| (sum as f32 * crate::MIX_HEADROOM) as i32;

    let voice_only = mix.mix_opl_voice((0, 0), (FS, FS)).0;
    assert_eq!(voice_only, FS, "one leg is unity at its power-on default");
    assert_eq!(staged(voice_only), 16_383, "half scale: 6 dB still spare");
    assert_eq!(crate::clamp_i16(staged(voice_only)), 16_383);

    let voice_over_fm = mix.mix_opl_voice((FS, FS), (FS, FS)).0;
    assert_eq!(voice_over_fm, 2 * FS, "the legs sum; neither is trimmed");
    assert_eq!(
        crate::clamp_i16(staged(voice_over_fm)),
        i16::MAX,
        "effects over music at full scale must land ON the rail, not past it"
    );
    assert_eq!(
        staged(voice_over_fm),
        FS,
        "and it must be an exact landing, not a clamp hiding an overshoot -- \
         this is what fails if MIX_HEADROOM is loosened"
    );

    let plus_cd = voice_over_fm + mix.mix_cd((FS, FS)).0;
    assert_eq!(plus_cd, 3 * FS, "the CD leg is unity at level 31 as well");
    assert_eq!(
        staged(plus_cd),
        49_150,
        "three full-scale legs overshoot by 3.5 dB -- accepted, see above"
    );
    assert_eq!(
        crate::clamp_i16(staged(plus_cd)),
        i16::MAX,
        "and the overshoot is what clipping is for"
    );
}
