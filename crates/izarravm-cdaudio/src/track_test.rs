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
        .as_chunks::<2>()
        .0
        .iter()
        .map(|s| i16::from_le_bytes(*s).unsigned_abs())
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
    // until the decode lands: eighteen thousand of them for a four-minute
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
    // its own handle on the shared state, so a weak handle taken here goes dead
    // exactly when the worker has stopped and let go of it.
    let track = DecodedTrack::new(fixture("tone.flac")).unwrap();
    track.frame(0);
    let shared = Arc::clone(&track.shared);
    let before = shared.generation.load(Ordering::SeqCst);
    let weak = Arc::downgrade(&shared);

    drop(track);

    assert!(
        shared.generation.load(Ordering::SeqCst) > before,
        "the worker was never told to stop"
    );
    drop(shared);
    assert!(
        wait_for(|| weak.upgrade().is_none()),
        "the worker never let go of the track"
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
fn a_third_track_evicts_the_least_recently_touched_one() {
    // A full CD of audio is about 750 MB decoded, and a front-panel play walks
    // the whole disc, so residency has to be bounded. Two is what boundary
    // prefetch needs: the track being played and the one being read ahead.
    let registry = Registry::new();
    let a = registry.track(fixture("tone.wav")).unwrap();
    let b = registry.track(fixture("tone.ogg")).unwrap();
    a.frame(0);
    b.frame(0);
    // Waiting on `resident_bytes` alone would prove nothing: the buffer is
    // allocated when the worker starts, so it is non-zero immediately.
    assert!(wait_for(|| a.frame(a.sectors() - 1).is_some()));
    assert!(wait_for(|| b.frame(b.sectors() - 1).is_some()));

    let c = registry.track(fixture("tone.flac")).unwrap();
    c.frame(0);

    assert!(
        wait_for(|| a.resident_bytes() == 0),
        "the oldest track was never evicted"
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
fn the_track_under_the_play_head_survives_an_idle_older_one() {
    // Three tracks, one being played and read frame by frame, an older idle one
    // whose worker is still alive, and a third arriving from boundary prefetch.
    // The idle one is the least recently touched, so it goes at once, live or
    // not. The played track must stay: taking it sends the mixer back to a
    // decode from sector 0 in the middle of a song.
    let registry = Registry::gated();
    let idle = registry.track(fixture("tone.ogg")).unwrap();
    let played = registry.track(fixture("tone.wav")).unwrap();
    let arriving = registry.track(fixture("tone.flac")).unwrap();
    idle.frame(0);
    played.frame(0);
    // The mixer reads the played track, which makes it the most recent.
    played.frame(0);

    arriving.frame(0);

    assert!(
        played.resident_bytes() > 0,
        "the track being played was evicted out from under the mixer"
    );
    assert_eq!(
        idle.resident_bytes(),
        0,
        "the idle older track kept its buffer"
    );
    registry.open_gate();
}

#[test]
fn a_track_being_replayed_is_not_the_one_evicted() {
    // Recency has to count reads, not decode starts. A track played from a
    // buffer it already holds serves nothing but hits, so if only a miss
    // refreshed its place it would sit at the bottom of the order however long
    // it had been playing, and the next admission would take its buffer away
    // mid-song, sending the mixer back to a decode from sector 0.
    let registry = Registry::new();
    let played = registry.track(fixture("tone.wav")).unwrap();
    let other = registry.track(fixture("tone.ogg")).unwrap();
    played.frame(0);
    other.frame(0);
    // Waited on `done`, not on the last frame being readable: a worker
    // publishes its last run of sectors and only then marks itself done, so
    // this is what establishes that both tracks are decoded and idle.
    for track in [&played, &other] {
        assert!(
            wait_for(|| is_done(track)),
            "a worker never reached its end"
        );
    }
    assert!(played.frame(played.sectors() - 1).is_some());
    assert!(other.frame(other.sectors() - 1).is_some());

    // `played` was admitted first, so it is the oldest by admission order. Now
    // read it the way the mixer would: every one of these is a hit.
    for index in 0..played.sectors() {
        assert!(played.frame(index).is_some());
    }

    // A third track arrives, as boundary prefetch would bring it.
    let arriving = registry.track(fixture("tone.flac")).unwrap();
    arriving.frame(0);

    assert!(
        wait_for(|| other.resident_bytes() == 0),
        "the track nobody was reading should have been the one evicted"
    );
    assert!(
        played.resident_bytes() > 0,
        "the track being played was evicted out from under the mixer"
    );
}

#[test]
fn formatting_a_track_does_not_print_its_audio() {
    // `CdImage` derives `Debug` and holds these behind an `Arc`, so one `{:?}`
    // of a mounted disc reaches the decoded buffer. A derived `Debug` on it
    // prints every sample byte, tens of megabytes into a single log line,
    // which is the hazard the `AudioTrackSource` contract tells implementors to
    // hand-write `Debug` around.
    let track = DecodedTrack::new(fixture("tone.wav")).unwrap();
    track.frame(0);
    assert!(wait_for(|| track.frame(track.sectors() - 1).is_some()));
    assert!(
        track.resident_bytes() > 30_000,
        "nothing was decoded to print"
    );

    let rendered = format!("{track:?}");

    assert!(
        rendered.len() < 500,
        "formatting a track produced {} characters; it is printing the buffer",
        rendered.len()
    );
}

// Tests for the worker lifetime model. The gated registries hold workers at
// their start, so a test keeps several alive at once without sleeping.

fn generation_of(track: &DecodedTrack) -> u64 {
    track.shared.filled.lock().unwrap().generation
}

fn is_done(track: &DecodedTrack) -> bool {
    track.shared.filled.lock().unwrap().done
}

const FIXTURES: [&str; 4] = ["tone.wav", "tone.ogg", "tone.flac", "tone.mp3"];

fn gated_tracks(registry: &Registry, count: usize) -> Vec<DecodedTrack> {
    FIXTURES
        .iter()
        .cycle()
        .take(count)
        .map(|name| registry.track(fixture(name)).unwrap())
        .collect()
}

#[test]
fn a_gated_worker_is_counted_live_until_the_gate_opens() {
    let registry = Registry::gated();
    let tracks = gated_tracks(&registry, 1);
    assert!(tracks[0].frame(0).is_none());
    // Raised before the spawn, so the count is exact right after the touch.
    assert_eq!(registry.live_workers(), 1);
    assert_eq!(tracks[0].shared.live.load(Ordering::SeqCst), 1);
    registry.open_gate();
    assert!(wait_for(|| registry.live_workers() == 0));
    assert!(wait_for(
        || tracks[0].shared.live.load(Ordering::SeqCst) == 0
    ));
}

#[test]
fn a_stuck_worker_outlives_its_track_until_the_gate_opens() {
    let registry = Registry::gated_stuck();
    let tracks = gated_tracks(&registry, 1);
    tracks[0].frame(0);
    drop(tracks);
    // The stuck gate ignores the stop signal, as a worker blocked in a slow
    // read would, so the count stays up after the track is gone.
    assert_eq!(registry.live_workers(), 1);
    registry.open_gate();
    assert!(wait_for(|| registry.live_workers() == 0));
}

fn resident_count(tracks: &[DecodedTrack]) -> usize {
    tracks.iter().filter(|t| t.resident_bytes() > 0).count()
}

#[test]
fn a_scan_over_many_tracks_never_holds_more_than_two_buffers() {
    let registry = Registry::gated();
    let tracks = gated_tracks(&registry, 6);
    for (n, track) in tracks.iter().enumerate() {
        assert!(track.frame(0).is_none());
        assert!(
            resident_count(&tracks) <= MAX_RESIDENT,
            "{} buffers after touching track {n}",
            resident_count(&tracks)
        );
        assert!(
            registry.live_workers() <= MAX_LIVE_WORKERS,
            "{} live workers after touching track {n}",
            registry.live_workers()
        );
    }
    registry.open_gate();
}

#[test]
fn workers_return_to_zero_after_a_scan() {
    let registry = Registry::gated();
    let tracks = gated_tracks(&registry, 6);
    for track in &tracks {
        track.frame(0);
        assert!(resident_count(&tracks) <= MAX_RESIDENT);
    }
    registry.open_gate();
    assert!(
        wait_for(|| registry.live_workers() == 0),
        "{} workers are still alive",
        registry.live_workers()
    );
    assert!(resident_count(&tracks) <= MAX_RESIDENT);
}

#[test]
fn switching_tracks_stops_the_old_worker() {
    let registry = Registry::gated();
    let tracks = gated_tracks(&registry, 3);
    for track in &tracks {
        track.frame(0);
    }
    let [a, _b, _c] = &tracks[..] else { panic!() };
    assert_eq!(a.resident_bytes(), 0, "the oldest track kept its buffer");
    assert!(
        wait_for(|| a.shared.live.load(Ordering::SeqCst) == 0),
        "the worker of the evicted track never stopped"
    );
    registry.open_gate();
}

#[test]
fn the_spawn_cap_stops_new_threads() {
    let registry = Registry::gated_stuck();
    let tracks = gated_tracks(&registry, 6);
    for track in &tracks[..4] {
        track.frame(0);
    }
    // Evicted workers stay alive here: they are stuck before a cancel check.
    assert_eq!(registry.live_workers(), MAX_LIVE_WORKERS);
    let fifth = &tracks[4];
    assert!(fifth.frame(0).is_none());
    assert_eq!(
        fifth.shared.workers.load(Ordering::Relaxed),
        0,
        "a worker started over the cap"
    );
    assert_eq!(registry.live_workers(), MAX_LIVE_WORKERS);

    registry.open_gate();
    assert!(wait_for(|| registry.live_workers() < MAX_LIVE_WORKERS));
    fifth.frame(0);
    assert_eq!(
        fifth.shared.workers.load(Ordering::Relaxed),
        1,
        "the track never retried after the cap cleared"
    );
}

#[test]
fn dropping_the_disc_stops_every_worker_and_frees_every_buffer() {
    let registry = Registry::gated();
    let tracks = gated_tracks(&registry, 2);
    for track in &tracks {
        track.frame(0);
    }
    assert_eq!(registry.live_workers(), 2);
    let weak: Vec<_> = tracks.iter().map(|t| Arc::downgrade(&t.shared)).collect();
    drop(tracks);
    // The worker holds the last `Arc<Shared>`, so the handle going dead is the
    // proof that the worker has let go of the buffer.
    assert!(
        wait_for(|| weak.iter().all(|w| w.upgrade().is_none())),
        "a worker still holds its track after the disc was dropped"
    );
    assert!(wait_for(|| registry.live_workers() == 0));
}

#[test]
fn the_playing_track_and_the_next_survive_a_scan() {
    // Part a: the mixer touches the playing track, then the prefetch target, on
    // every tick. The target walks through the rest of the disc and the playing
    // track keeps its decode.
    let registry = Registry::gated();
    let tracks = gated_tracks(&registry, 5);
    let playing = &tracks[0];
    playing.frame(0);
    let generation = generation_of(playing);
    for target in &tracks[1..] {
        playing.frame(0);
        target.frame(0);
        assert_eq!(
            generation_of(playing),
            generation,
            "the playing track was evicted"
        );
        assert!(playing.resident_bytes() > 0);
    }
    registry.open_gate();

    // Part b: the head crosses a short track N into the track behind it. One
    // tick touches P (the last frame it plays), then N (the head track), then
    // N+1 (the prefetch). P is the oldest of the three and is the one that
    // goes. N stays, and so does its decode.
    let registry = Registry::gated();
    let p = registry.track(fixture("tone.wav")).unwrap();
    let n = registry.track(fixture("tone.ogg")).unwrap();
    let after = registry.track(fixture("tone.flac")).unwrap();
    p.frame(0);
    n.frame(0);
    let n_generation = generation_of(&n);

    p.frame(0);
    n.frame(0);
    after.frame(0);

    assert_eq!(
        generation_of(&n),
        n_generation,
        "the next track was evicted"
    );
    assert!(n.resident_bytes() > 0);
    assert_eq!(p.resident_bytes(), 0, "the finished track kept its buffer");
    registry.open_gate();
}

#[test]
fn an_evicted_worker_never_writes_into_the_new_buffer() {
    let registry = Registry::gated();
    let tracks = gated_tracks(&registry, 3);
    let a = &tracks[0];
    a.frame(0);
    tracks[1].frame(0);
    tracks[2].frame(0);
    assert_eq!(a.resident_bytes(), 0, "the oldest track was not evicted");
    let evicted_generation = generation_of(a);
    // The same track again: a new worker with a new generation, while the old
    // one still waits at the gate.
    a.frame(0);
    assert!(generation_of(a) >= evicted_generation);
    registry.open_gate();

    assert!(wait_for(|| a.frame(a.sectors() - 1).is_some()));
    assert!(wait_for(|| registry.live_workers() == 0));
    assert_eq!(a.shared.workers.load(Ordering::Relaxed), 2);

    let path = fixture("tone.wav");
    let info = probe_info(&path).unwrap().unwrap();
    let mut reference = vec![0u8; info.sectors as usize * AUDIO_FRAME_BYTES];
    crate::decode_into(&path, info, &mut reference, &mut |_, _| {}).unwrap();
    for index in 0..a.sectors() {
        let from = index as usize * AUDIO_FRAME_BYTES;
        assert_eq!(
            a.frame(index).unwrap()[..],
            reference[from..from + AUDIO_FRAME_BYTES],
            "frame {index} differs from a single clean decode"
        );
    }
}

#[test]
fn a_full_cap_still_evicts_the_old_tracks() {
    // Four stuck workers: two on tracks that were evicted long ago, two on the
    // tracks that are resident now. A fifth track starts. It must still push
    // the least recently touched resident track out, even though it cannot
    // start a thread, or the old tracks would stay pinned until the stuck
    // reads return.
    let registry = Registry::gated_stuck();
    let tracks = gated_tracks(&registry, 5);
    let [s1, s2, p, n, q] = &tracks[..] else {
        panic!()
    };
    for track in [s1, s2, p, n] {
        track.frame(0);
    }
    assert_eq!(registry.live_workers(), MAX_LIVE_WORKERS);
    let p_generation = generation_of(p);

    assert!(q.frame(0).is_none());

    assert_eq!(q.resident_bytes(), 0, "a track over the cap got a buffer");
    assert_eq!(
        p.resident_bytes(),
        0,
        "the least recent track kept its buffer"
    );
    assert!(generation_of(p) > p_generation);
    assert!(n.resident_bytes() > 0);
    assert_eq!(registry.live_workers(), MAX_LIVE_WORKERS);
    assert_eq!(q.shared.workers.load(Ordering::Relaxed), 0);

    registry.open_gate();
    assert!(wait_for(|| registry.live_workers() < MAX_LIVE_WORKERS));
    assert!(
        wait_for(|| q.frame(0).is_some()),
        "the track never started once the cap cleared"
    );
}
