// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_core::AudioTrackSource;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Poll `f` until it returns true or the deadline passes. Decoding runs on a
/// worker, so a test that asserts on its output has to wait for it; a bare
/// sleep would be either flaky or slow.
fn wait_for(mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

/// Peak absolute sample in a frame.
fn peak(frame: &[u8; AUDIO_FRAME_BYTES]) -> u16 {
    frame
        .chunks_exact(2)
        .map(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs())
        .max()
        .unwrap_or(0)
}

#[test]
fn a_track_reports_its_length_before_any_decoding_happens() {
    let track = DecodedTrack::new(fixture("tone.ogg")).unwrap();
    assert_eq!(track.sectors(), 15);
    // Nothing has been touched, so nothing is ready. This is deterministic
    // rather than a race with the worker: `frame` reads the buffer before it
    // starts anything.
    assert!(track.frame(0).is_none());
}

#[test]
fn touching_a_frame_starts_the_decode_and_it_completes() {
    let track = DecodedTrack::new(fixture("tone.ogg")).unwrap();
    // The first touch kicks the worker off and reports the frame absent,
    // because at that instant it is.
    assert!(track.frame(0).is_none());
    assert!(
        wait_for(|| track.frame(0).is_some()),
        "the decode never produced frame 0"
    );
    assert!(
        wait_for(|| track.frame(track.sectors() - 1).is_some()),
        "the decode never reached the last frame"
    );
}

#[test]
fn a_decoded_frame_is_not_silence() {
    let track = DecodedTrack::new(fixture("tone.wav")).unwrap();
    track.frame(0);
    assert!(wait_for(|| track.frame(2).is_some()));
    let frame = track.frame(2).unwrap();
    assert!(
        peak(&frame) > 1000,
        "decoded frame peaked at {}",
        peak(&frame)
    );
}

#[test]
fn every_container_reaches_the_mixer_as_the_same_audio() {
    // The point of the whole crate, asserted end to end: four containers of one
    // tone all arrive as Red Book frames carrying that tone. A per-container
    // conversion bug that produced silence, or noise, or half the samples,
    // shows up here even though no two encoders agree bit for bit.
    for name in ["tone.wav", "tone.ogg", "tone.flac", "tone.mp3"] {
        let track = DecodedTrack::new(fixture(name)).unwrap();
        assert_eq!(track.sectors(), 15, "{name}");
        track.frame(0);
        assert!(
            wait_for(|| track.frame(7).is_some()),
            "{name} never decoded its middle"
        );
        let frame = track.frame(7).unwrap();
        assert!(peak(&frame) > 1000, "{name} decoded to near-silence");
    }
}

#[test]
fn a_frame_past_the_track_is_absent_even_when_fully_decoded() {
    let track = DecodedTrack::new(fixture("tone.wav")).unwrap();
    track.frame(0);
    assert!(wait_for(|| track.frame(track.sectors() - 1).is_some()));
    assert!(track.frame(track.sectors()).is_none());
    assert!(track.frame(u32::MAX).is_none());
}

#[test]
fn the_decode_runs_once_however_many_frames_are_touched() {
    // `frame` starts the worker on each miss, and a track is nothing but misses
    // until the decode lands -- eighteen thousand of them for a four-minute
    // track. The audio would come out right anyway, since every worker decodes
    // the same file to the same bytes, so the count is what has to be asserted:
    // the damage is threads, not sound.
    let track = DecodedTrack::new(fixture("tone.flac")).unwrap();
    for index in 0..track.sectors() {
        track.frame(index);
    }
    assert!(wait_for(|| track.frame(track.sectors() - 1).is_some()));
    for index in 0..track.sectors() {
        assert!(track.frame(index).is_some(), "frame {index} went missing");
    }
    assert_eq!(
        track.shared.workers.load(Ordering::Relaxed),
        1,
        "one worker per track, however many frames are touched"
    );
}

#[test]
fn dropping_a_track_cancels_its_worker() {
    // Unmounting a disc must not leave a thread decoding it. The worker owns
    // its own handle on the shared state, so holding a third one here is how
    // the flag can be read after the track itself is gone.
    let track = DecodedTrack::new(fixture("tone.flac")).unwrap();
    track.frame(0);
    let shared = Arc::clone(&track.shared);
    assert!(!shared.cancel.load(Ordering::Relaxed));

    drop(track);

    assert!(
        shared.cancel.load(Ordering::Relaxed),
        "the worker was never told to stop"
    );
}

#[test]
fn an_unreadable_file_fails_at_construction_not_at_play_time() {
    let missing = fixture("this-file-does-not-exist.ogg");
    assert!(DecodedTrack::new(missing).is_err());
}

#[test]
fn a_file_that_is_not_a_container_is_refused_by_name() {
    let raw = std::env::temp_dir().join(format!(
        "izarravm-cdaudio-track-raw-{}.bin",
        std::process::id()
    ));
    std::fs::write(&raw, vec![0u8; 2352 * 3]).unwrap();
    let err = DecodedTrack::new(raw.clone()).unwrap_err();
    assert!(
        err.to_string().contains("not an audio container"),
        "message was: {err}"
    );
    std::fs::remove_file(&raw).ok();
}
