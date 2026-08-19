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
        // On `finished`, not on the last frame being readable: a worker
        // publishes its final sectors and only then stores the flag, so a
        // readable track can still have a worker about to write `true` over the
        // `false` this test is about to install -- which hands the registry a
        // fourth candidate and evicts whatever it likes.
        assert!(wait_for(|| track.shared.finished.load(Ordering::SeqCst)));
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
        // On `finished` for the same reason as above: a worker still on its way
        // out would store `true` after the `false` installed below.
        assert!(wait_for(|| track.shared.finished.load(Ordering::SeqCst)));
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
    // finished. Only a worker reaching its end can clear the backlog.
    a.frame(0);
    assert!(
        wait_for(|| a.shared.finished.load(Ordering::SeqCst)),
        "the worker for the admitted track never reached its end"
    );

    // `a`'s own worker is not the one that can do it: it leaves exactly one
    // evictable track behind, and the registry never takes the last one -- from
    // here that track is indistinguishable from the one under the play head.
    registry.evict_excess();
    assert_eq!(
        resident(&registry),
        3,
        "the only decoded track was taken while two others were still decoding"
    );

    // The next worker out is. `c` is not a candidate at this point and never
    // becomes one here; the live comparison is `b` against `a`, and `b` was
    // touched first, so `b` is the one that goes.
    b.shared.finished.store(true, Ordering::SeqCst);
    registry.evict_excess();

    assert!(
        resident(&registry) <= MAX_RESIDENT,
        "the backlog outlived the worker that could have cleared it"
    );
    assert_eq!(
        b.resident_bytes(),
        0,
        "the backlog was cleared from the wrong end"
    );
}

#[test]
fn a_real_workers_tail_call_is_what_clears_the_backlog() {
    // The test above hand-drives the flag and calls `evict_excess` itself, which
    // pins the ORDER a backlog is cleared in but proves nothing about who calls
    // it. Deleting the body of the worker's tail call leaves that test green.
    // This one leaves the call in the worker's hands: nothing else here can
    // reach `evict_excess` after the flag lands.
    //
    // Three residents: one finished and idle, one that never finishes, and one
    // that takes a real worker.
    let registry = Registry::new();
    let played = registry.track(fixture("tone.wav")).unwrap();
    let stuck = registry.track(fixture("tone.ogg")).unwrap();
    let arriving = registry.track(fixture("tone.flac")).unwrap();
    for track in [&played, &stuck, &arriving] {
        track.frame(0);
        assert!(wait_for(|| track.shared.finished.load(Ordering::SeqCst)));
    }
    {
        let mut resident = registry.inner.lock().unwrap();
        resident.clear();
        for track in [&played, &stuck, &arriving] {
            let mut filled = track.shared.filled.lock().unwrap();
            filled.pcm = vec![0u8; AUDIO_FRAME_BYTES];
            track.shared.finished.store(true, Ordering::SeqCst);
            resident.push(Arc::clone(&track.shared));
        }
        // `stuck` stands in for a worker that has not reached its end -- the
        // reason a backlog exists at all.
        stuck.shared.finished.store(false, Ordering::SeqCst);
        // `arriving` is asked for afresh: no readable sector and nothing
        // started, so its `frame` misses and a real worker is spawned.
        let mut filled = arriving.shared.filled.lock().unwrap();
        filled.ready = 0;
        arriving.shared.finished.store(false, Ordering::SeqCst);
        arriving.shared.started.store(false, Ordering::SeqCst);
    }
    let resident = |registry: &Registry| registry.inner.lock().unwrap().len();

    // The decline, checked with no worker alive so the count cannot be a race:
    // one evictable track and the registry leaves it alone.
    registry.evict_excess();
    assert_eq!(
        resident(&registry),
        3,
        "the last evictable track was taken at admission"
    );
    assert!(played.resident_bytes() > 0);

    // Now the real worker. Admission declines again for the same reason -- it
    // still sees only `played` -- so the count can only come down once
    // `arriving` stores its flag, and the tail call is what asks afterwards.
    arriving.frame(0);

    assert!(
        wait_for(|| resident(&registry) <= MAX_RESIDENT),
        "no worker asked again on its way out, so the backlog is permanent"
    );
    assert_eq!(
        played.resident_bytes(),
        0,
        "the backlog was cleared from the wrong end"
    );
    assert!(
        stuck.resident_bytes() > 0,
        "a track still being written into was evicted"
    );
    assert!(
        arriving.resident_bytes() > 0,
        "the worker evicted the track it had just filled"
    );
}

#[test]
fn the_track_under_the_play_head_survives_an_older_one_that_has_not_flagged_itself() {
    // The state a two-core runner reaches on its own: one track fully decoded
    // and being read frame by frame, an older idle one whose worker has
    // published its last sector but not yet stored `finished`, and a third
    // arriving from boundary prefetch. Only the played track is a candidate,
    // and taking it sends the mixer back to a decode from sector 0 in the middle
    // of a song. Built by hand because reaching it live is a race lost a few
    // times in a hundred.
    let registry = Registry::new();
    let played = registry.track(fixture("tone.wav")).unwrap();
    let idle = registry.track(fixture("tone.ogg")).unwrap();
    let arriving = registry.track(fixture("tone.flac")).unwrap();
    for track in [&played, &idle, &arriving] {
        track.frame(0);
        assert!(wait_for(|| track.shared.finished.load(Ordering::SeqCst)));
    }
    {
        let mut resident = registry.inner.lock().unwrap();
        resident.clear();
        for track in [&idle, &played, &arriving] {
            let mut filled = track.shared.filled.lock().unwrap();
            filled.pcm = vec![0u8; AUDIO_FRAME_BYTES];
            track.shared.finished.store(true, Ordering::SeqCst);
            resident.push(Arc::clone(&track.shared));
        }
        // `idle` was touched first and is the one whose flag is late; `arriving`
        // has only just missed, so it is the newest and still decoding.
        idle.shared.finished.store(false, Ordering::SeqCst);
        arriving.shared.finished.store(false, Ordering::SeqCst);
    }
    // The mixer reads `played`, which is what makes it the most recent.
    played.shared.last_touch.store(
        TOUCH_CLOCK.fetch_add(1, Ordering::Relaxed),
        Ordering::Relaxed,
    );

    registry.evict_excess();

    assert!(
        played.resident_bytes() > 0,
        "the track being played was evicted out from under the mixer"
    );
    assert!(idle.resident_bytes() > 0, "an unfinished track was evicted");

    // And the backlog is not permanent: the late flag lands and the older track
    // goes, which is what a worker's tail call does.
    idle.shared.finished.store(true, Ordering::SeqCst);
    registry.evict_excess();

    assert_eq!(
        idle.resident_bytes(),
        0,
        "the older track survived its own worker's tail call"
    );
    assert!(
        played.resident_bytes() > 0,
        "the played track went on the second pass instead"
    );
}

#[test]
fn a_track_being_replayed_is_not_the_one_evicted() {
    // Recency has to count reads, not decode starts. A track played from a
    // buffer it already holds serves nothing but hits, so if only a miss
    // refreshed its place it would sit at the bottom of the order however long
    // it had been playing -- and the next admission would take its buffer away
    // mid-song, sending the mixer back to a decode from sector 0.
    let registry = Registry::new();
    let played = registry.track(fixture("tone.wav")).unwrap();
    let other = registry.track(fixture("tone.ogg")).unwrap();
    played.frame(0);
    other.frame(0);
    // Waited on `finished`, not on the last frame being readable. Those are not
    // the same instant: a worker publishes its last run of sectors and only then
    // stores the flag, so a track can read to its end while the registry still
    // sees it as one a worker is writing into. Waiting on the readable frame
    // leaves this test's stated precondition -- both tracks decoded and idle --
    // unestablished, and which side of that window `other` was on decided
    // whether the test found the eviction bug or a passing run.
    for track in [&played, &other] {
        assert!(
            wait_for(|| track.shared.finished.load(Ordering::SeqCst)),
            "a worker never reached its end"
        );
    }
    assert!(played.frame(played.sectors() - 1).is_some());
    assert!(other.frame(other.sectors() - 1).is_some());

    // `played` was admitted first, so it is the oldest by admission order. Now
    // read it the way the mixer would -- every one of these is a hit.
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
    // prints every sample byte -- tens of megabytes into a single log line --
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

// MUTATION EVIDENCE for residency eviction (2026-08-19, applied by hand, run, restored). Each row
// names the fixture that caught it; a mutation nobody catches is a fixture bug, not a free pass.
//
// | mutation | caught by |
// |---|---|
// | `evict_excess`: the last-candidate guard weakened from `evictable < 2` to `evictable < 1`, which is the pre-fix behaviour exactly | `the_track_under_the_play_head_survives_an_older_one_that_has_not_flagged_itself` AND `a_backlog_is_cleared_by_the_next_worker_to_finish` |
// | `frame`: the `last_touch` store deleted, so recency counts decode starts and not reads | `a_track_being_replayed_is_not_the_one_evicted` |
// | `decode_worker`: the tail call to `evict_excess` replaced with `let _ = registry;`, so a backlog is never asked about again | `a_real_workers_tail_call_is_what_clears_the_backlog` |
//
// The second row is why the first fixture is not the whole net: with the precondition of
// `a_track_being_replayed_is_not_the_one_evicted` finally established (it now waits on `finished`,
// not on the last frame being readable) that test no longer races into the eviction bug, so the
// deterministic fixture above carries it and the replay test keeps the recency rule honest.
//
// The third row was a hole this change opened and then closed, which is worth the ledger space.
// `a_backlog_is_cleared_by_the_next_worker_to_finish` caught that mutation before this change,
// when it drove the backlog with a real worker. Restructuring it for the guard meant hand-driving
// the flag and calling `evict_excess` from the test, and a fixture that makes the call itself
// cannot notice that the worker stopped making it: the mutation ran green, 47 of 47, until
// `a_real_workers_tail_call_is_what_clears_the_backlog` was written to leave that one call in the
// worker's hands. Both were run against the mutation in that order, and the second one fails on
// the full ten-second wait rather than on an assertion, because the backlog simply never clears.
