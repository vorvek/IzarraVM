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

#[test]
fn a_third_track_evicts_the_least_recently_touched_finished_one() {
    // A full CD of audio is about 750 MB decoded, and a front-panel play walks
    // the whole disc, so residency has to be bounded. Two is what boundary
    // prefetch needs: the track being played and the one being read ahead.
    let registry = Registry::new();
    let a = registry.track(fixture("tone.wav")).unwrap();
    let b = registry.track(fixture("tone.ogg")).unwrap();
    a.frame(0);
    b.frame(0);
    // Waiting on `resident_bytes` alone would prove nothing: the buffer is
    // allocated when the worker starts, so it is non-zero immediately. What
    // decides which track is evictable is the decode having finished.
    assert!(wait_for(|| a.frame(a.sectors() - 1).is_some()));
    assert!(wait_for(|| b.frame(b.sectors() - 1).is_some()));

    let c = registry.track(fixture("tone.flac")).unwrap();
    c.frame(0);

    assert!(
        wait_for(|| a.resident_bytes() == 0),
        "the oldest finished track was never evicted"
    );
    assert!(
        b.resident_bytes() > 0,
        "the newer track was evicted instead"
    );
}

#[test]
fn an_evicted_track_decodes_again_on_replay() {
    let registry = Registry::new();
    let a = registry.track(fixture("tone.wav")).unwrap();
    let b = registry.track(fixture("tone.ogg")).unwrap();
    let c = registry.track(fixture("tone.flac")).unwrap();
    a.frame(0);
    b.frame(0);
    assert!(wait_for(|| a.frame(a.sectors() - 1).is_some()));
    assert!(wait_for(|| b.frame(b.sectors() - 1).is_some()));
    c.frame(0);
    assert!(wait_for(|| a.resident_bytes() == 0));

    // Touching it again brings it back rather than serving silence for the
    // rest of the mount.
    a.frame(0);

    assert!(
        wait_for(|| a.frame(0).is_some()),
        "an evicted track never decoded again"
    );
}

#[test]
fn residency_is_bounded_however_many_tracks_a_disc_has() {
    // Betrayal at Krondor has 62 audio tracks and a panel play touches every
    // one of them in order. The bound is on what is resident at once, not on
    // how many tracks exist.
    let registry = Registry::new();
    let tracks: Vec<_> = ["tone.wav", "tone.ogg", "tone.flac", "tone.mp3"]
        .iter()
        .cycle()
        .take(12)
        .map(|name| registry.track(fixture(name)).unwrap())
        .collect();
    for track in &tracks {
        track.frame(0);
        assert!(wait_for(|| track.frame(track.sectors() - 1).is_some()));
    }

    // Every worker has finished, so every buffer past the bound is evictable
    // and the last one out has asked.
    assert!(wait_for(|| tracks
        .iter()
        .filter(|t| t.resident_bytes() > 0)
        .count()
        <= MAX_RESIDENT));
    let resident = tracks.iter().filter(|t| t.resident_bytes() > 0).count();
    assert!(
        resident <= MAX_RESIDENT,
        "{resident} of 12 tracks are still holding decoded audio"
    );
}

#[test]
fn a_track_without_a_registry_keeps_its_buffer() {
    // The unbounded case is the one the worker tests use, and it has to stay
    // available: a track built on its own answers for itself.
    let track = DecodedTrack::new(fixture("tone.wav")).unwrap();
    track.frame(0);
    assert!(wait_for(|| track.resident_bytes() > 0));
    for _ in 0..8 {
        let other = DecodedTrack::new(fixture("tone.ogg")).unwrap();
        other.frame(0);
    }
    assert!(track.resident_bytes() > 0);
}

#[test]
fn a_live_track_is_never_the_one_evicted() {
    // Taking the buffer from a worker that is still writing does not corrupt
    // anything -- it copies under the same lock and checks the length -- but it
    // leaves that track silent for good, because the publish that would have
    // filled it finds nothing to write into, and it clears `started` so a
    // second worker can begin on a file the first is still reading.
    //
    // The fixtures decode faster than that race can be arranged, so the state
    // is built directly: three residents, the oldest still decoding.
    let registry = Registry::new();
    let live = registry.track(fixture("tone.wav")).unwrap();
    let older = registry.track(fixture("tone.ogg")).unwrap();
    let newer = registry.track(fixture("tone.flac")).unwrap();
    for track in [&live, &older, &newer] {
        track.frame(0);
        assert!(wait_for(|| track.frame(track.sectors() - 1).is_some()));
    }
    {
        let mut resident = registry.inner.lock().unwrap();
        resident.clear();
        for track in [&live, &older, &newer] {
            let mut filled = track.shared.filled.lock().unwrap();
            filled.pcm = vec![0u8; AUDIO_FRAME_BYTES];
            track.shared.finished.store(true, Ordering::SeqCst);
            resident.push(Arc::clone(&track.shared));
        }
        live.shared.finished.store(false, Ordering::SeqCst);
    }

    registry.evict_excess();

    assert!(
        live.resident_bytes() > 0,
        "the track still being decoded was evicted"
    );
    assert_eq!(
        older.resident_bytes(),
        0,
        "the oldest finished one survived"
    );
}

#[test]
fn a_backlog_is_cleared_by_the_next_worker_to_finish() {
    // When a third track starts while the first two are still decoding, nothing
    // is evictable at that moment. Evicting only at admission would leave the
    // disc over its bound for the rest of the mount, so a worker asks again on
    // its way out.
    //
    // Built by hand for the same reason as above: the state is reached by
    // losing a race these fixtures are too small to lose.
    let registry = Registry::new();
    let a = registry.track(fixture("tone.wav")).unwrap();
    let b = registry.track(fixture("tone.ogg")).unwrap();
    let c = registry.track(fixture("tone.flac")).unwrap();
    for track in [&a, &b, &c] {
        track.frame(0);
        assert!(wait_for(|| track.frame(track.sectors() - 1).is_some()));
    }
    // Three resident with none evictable, and `a` ready to be decoded afresh --
    // exactly what an admission during two live decodes leaves behind.
    {
        let mut resident = registry.inner.lock().unwrap();
        resident.clear();
        for track in [&b, &c, &a] {
            let mut filled = track.shared.filled.lock().unwrap();
            filled.pcm = vec![0u8; AUDIO_FRAME_BYTES];
            track.shared.finished.store(false, Ordering::SeqCst);
            resident.push(Arc::clone(&track.shared));
        }
        a.shared.started.store(false, Ordering::SeqCst);
    }
    let resident = |registry: &Registry| registry.inner.lock().unwrap().len();
    assert_eq!(resident(&registry), 3);

    // Touching `a` admits it, which cannot evict anything -- nothing is
    // finished. Only the worker reaching its end can clear the backlog.
    a.frame(0);

    assert!(
        wait_for(|| resident(&registry) <= MAX_RESIDENT),
        "the backlog outlived the worker that could have cleared it"
    );
}
