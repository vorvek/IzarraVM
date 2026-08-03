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
