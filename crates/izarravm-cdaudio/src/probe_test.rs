// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// 0.2 s at 44.1 kHz is 8820 frames, which is exactly 15 Red Book sectors.
const TONE_SECTORS: u32 = 15;

/// The 3 s fixture is 132300 frames, which is exactly 225 sectors.
const TONE_3S_SECTORS: u32 = 225;

#[test]
fn wav_reports_its_shape_and_length() {
    let info = probe_info(&fixture("tone.wav")).unwrap().unwrap();
    assert_eq!(info.sample_rate, 44100);
    assert_eq!(info.channels, 2);
    assert_eq!(info.sectors, TONE_SECTORS);
}

#[test]
fn ogg_and_flac_report_the_same_length_as_the_wav_they_came_from() {
    for name in ["tone.ogg", "tone.flac"] {
        let info = probe_info(&fixture(name)).unwrap().unwrap();
        assert_eq!(info.sectors, TONE_SECTORS, "{name}");
    }
}

#[test]
fn a_22k_mono_source_probes_to_double_the_sectors() {
    // The sector count is derived from the *output* length, so a half-rate
    // source yields twice the sectors its own frame count would suggest.
    let info = probe_info(&fixture("tone-22k-mono.wav")).unwrap().unwrap();
    assert_eq!(info.sample_rate, 22050);
    assert_eq!(info.channels, 1);
    assert_eq!(info.sectors, TONE_SECTORS);
}

#[test]
fn an_mpeg_walk_has_the_encoders_delay_and_padding_taken_off_it() {
    // Walking counts coded frames, and LAME's own delay and tail padding are
    // coded frames: 10368 against the 8820 that are audio, which would put
    // this track three sectors long and shift every track after it on the
    // disc. A Xing/LAME header declares both, so they can be removed.
    let path = fixture("tone.mp3");
    let walked = walked_frames(&path).unwrap();
    assert!(
        walked > 8820,
        "fixture no longer carries encoder delay: walked {walked}"
    );
    let info = probe_info(&path).unwrap().unwrap();
    assert_eq!(info.sectors, TONE_SECTORS);
}

#[test]
fn a_tagless_vbr_mp3_is_measured_by_walking_its_packets() {
    // symphonia does not report "unknown" for a long tagless VBR file -- it
    // extrapolates a frame count from the average bitrate and returns it as a
    // `Some`, so a fallback conditioned on `None` would never fire. On this
    // fixture it is 19% short. The walk is exact, and the small overshoot that
    // remains is the encoder delay a tagless stream simply does not record.
    let path = fixture("tone-3s-noxing.mp3");
    let declared = declared_frames(&path).unwrap().expect(
        "fixture is meant to be long enough that symphonia extrapolates a count \
         instead of reporting None; a shorter one would test nothing",
    );
    let info = probe_info(&path).unwrap().unwrap();
    let from_declared = sectors_for(declared, info.sample_rate);
    assert!(
        info.sectors.abs_diff(TONE_3S_SECTORS) < from_declared.abs_diff(TONE_3S_SECTORS),
        "walk gave {} sectors and the container's own count would have given \
         {from_declared}, against a true {TONE_3S_SECTORS}",
        info.sectors
    );
}

#[test]
fn a_short_tagless_mp3_reports_no_count_at_all() {
    // The other half of the tagless case: below some length symphonia declines
    // to extrapolate. This is the branch the 3 s fixture cannot reach, and it
    // is here so that neither fixture can be dropped as redundant.
    assert_eq!(declared_frames(&fixture("tone-noxing.mp3")).unwrap(), None);
    assert!(probe_info(&fixture("tone-noxing.mp3")).unwrap().is_some());
}

#[test]
fn a_flac_without_a_declared_length_still_probes() {
    // total_samples = 0 is legal FLAC and comes out of any pipe-fed encode.
    // Failing the mount for it would be worse than today, where the disc at
    // least boots. The fallback walks the file once to count.
    assert_eq!(
        declared_frames(&fixture("tone-nolength.flac")).unwrap(),
        None
    );
    let info = probe_info(&fixture("tone-nolength.flac")).unwrap().unwrap();
    assert_eq!(info.sectors, TONE_SECTORS);
}

#[test]
fn opus_in_ogg_is_rejected_by_name() {
    let err = probe_info(&fixture("tone-opus.ogg")).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("Opus"), "message was: {message}");
}

#[test]
fn a_raw_binary_file_is_not_an_audio_container() {
    // Ok(None), not an error: the caller mounts it as raw frames.
    let raw = std::env::temp_dir().join(format!(
        "izarravm-cdaudio-raw-probe-{}.bin",
        std::process::id()
    ));
    std::fs::write(&raw, vec![0u8; 2352 * 3]).unwrap();
    assert!(probe_info(&raw).unwrap().is_none());
    std::fs::remove_file(&raw).ok();
}

#[test]
fn an_absurd_frame_count_is_refused_rather_than_sized() {
    // The frame count comes from the container and nothing upstream constrains
    // it. A corrupt final granule position in an Ogg hands back a number near
    // u64::MAX; the sector count derived from it sizes the TOC, and on first
    // touch a `vec![0u8; sectors * 2352]` runs on the emulation thread. That
    // allocation fails, and this build aborts on panic.
    assert_eq!(sectors_for(u64::MAX, 44100), u32::MAX);
    // Saturating, not wrapping. This count is the one just past the u64 wrap:
    // multiplied by 44100 it comes back as 25184, which divides down to a
    // single sector. A wrapping multiply would mount a 418-trillion-frame
    // track as one sector long and raise nothing.
    assert_eq!(sectors_for(418_293_516_410_648, 44100), u32::MAX);
    // A 74-minute track is still measured.
    assert!(sectors_for(74 * 60 * 44100, 44100) < MAX_TRACK_SECTORS);
}

#[test]
fn a_zero_sample_rate_is_refused() {
    // Not a legal stream, and symphonia's RIFF parser does not validate the
    // field, so a WAV with a zeroed fmt chunk reaches the probe. Its duration
    // is undefined and it drives the resampler at a step of 0.0 -- a loop that
    // emits output forever without consuming input.
    let path = std::env::temp_dir().join(format!(
        "izarravm-cdaudio-zero-rate-{}.wav",
        std::process::id()
    ));
    let mut wav = std::fs::read(fixture("tone.wav")).unwrap();
    // Located rather than assumed: ffmpeg writes a LIST chunk ahead of the
    // data, so the sample rate is not at the canonical offset 24. Within the
    // fmt chunk it is 12 bytes in, past the id, the size, the format tag and
    // the channel count.
    let fmt = wav
        .windows(4)
        .position(|w| w == b"fmt ")
        .expect("fixture has no fmt chunk");
    wav[fmt + 12..fmt + 16].copy_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, &wav).unwrap();

    let err = probe_info(&path).unwrap_err();

    assert!(
        err.to_string().contains("sample rate of zero"),
        "message was: {err}"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_container_claiming_an_impossible_length_fails_the_mount() {
    // The refusal has to be reached through `probe_info`, not just proved of
    // `sectors_for`: a sector count that survives to the TOC is a buffer of
    // `sectors * 2352` bytes allocated on the emulation thread at first touch.
    let path =
        std::env::temp_dir().join(format!("izarravm-cdaudio-huge-{}.flac", std::process::id()));
    let mut flac = std::fs::read(fixture("tone.flac")).unwrap();
    // STREAMINFO's total_samples is 36 bits ending at byte 26. Filling it is
    // what a corrupt or crafted stream does.
    let huge = 0x0F_FFFF_FFFFu64;
    let packed = u64::from_be_bytes([0, 0, 0, flac[21], flac[22], flac[23], flac[24], flac[25]]);
    let cleared = packed & !0xF_FFFF_FFFFu64;
    let bytes = (cleared | huge).to_be_bytes();
    flac[21..26].copy_from_slice(&bytes[3..8]);
    std::fs::write(&path, &flac).unwrap();

    let err = probe_info(&path).unwrap_err();

    assert!(
        err.to_string().contains("not believable"),
        "message was: {err}"
    );
    std::fs::remove_file(&path).ok();
}
