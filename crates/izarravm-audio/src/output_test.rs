// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn queue_limits_match_the_requested_latency_band() {
    assert_eq!(TARGET_FRAMES, 1_323);
    assert_eq!(LOW_FRAMES, 662);
    assert_eq!(HIGH_FRAMES, 2_646);
    assert_eq!(CAPACITY_FRAMES, 4_410);
    assert_eq!(RAMP_FRAMES, 64);
}

#[test]
fn a_new_queue_has_thirty_milliseconds_of_silent_prefill() {
    let ring = new_ring();
    assert_eq!(ring.len(), TARGET_FRAMES);
    for _ in 0..TARGET_FRAMES {
        assert!(matches!(ring.pop(), Some(QueuedFrame::Padding)));
    }
    assert!(ring.is_empty());
}

#[test]
fn low_watermark_appends_new_audio_without_padding() {
    let ring = Arc::new(ArrayQueue::new(CAPACITY_FRAMES));
    let sink = AudioSink {
        ring: Arc::clone(&ring),
        debug: None,
    };

    sink.queue(&[(123, -123)]);

    assert_eq!(ring.len(), 1);
    assert!(matches!(ring.pop(), Some(QueuedFrame::Audio((123, -123)))));
}

#[test]
fn a_batch_within_the_high_watermark_is_not_truncated() {
    let ring = Arc::new(ArrayQueue::new(CAPACITY_FRAMES));
    let debug = Arc::new(AudioDebugCounters::new(ring.len()));
    let sink = AudioSink {
        ring: Arc::clone(&ring),
        debug: Some(Arc::clone(&debug)),
    };
    let frames = vec![(123, -123); HIGH_FRAMES];

    sink.queue(&frames);

    assert_eq!(ring.len(), HIGH_FRAMES);
    assert_eq!(debug.snapshot().overruns, 0);
    assert!(matches!(ring.pop(), Some(QueuedFrame::Audio((123, -123)))));
}

#[test]
fn high_watermark_drops_latency_and_marks_a_fade_boundary() {
    let ring = Arc::new(ArrayQueue::new(CAPACITY_FRAMES));
    for _ in 0..=HIGH_FRAMES {
        ring.push(QueuedFrame::Audio((10, 10))).unwrap();
    }
    let sink = AudioSink {
        ring: Arc::clone(&ring),
        debug: None,
    };

    sink.queue(&[(6_400, -6_400)]);

    assert_eq!(ring.len(), TARGET_FRAMES);
    let mut source = CallbackSource {
        ring: Arc::clone(&ring),
        debug: None,
        last: (6_400, -6_400),
        gain: RAMP_FRAMES,
        underruns: 0,
        prefill_remaining: TARGET_FRAMES,
        debug_frames_consumed: 0,
        debug_underruns_after_prefill: 0,
    };
    for step in (0..RAMP_FRAMES).rev() {
        assert_eq!(
            source.next(),
            (
                i16::try_from(step * 100).unwrap(),
                -i16::try_from(step * 100).unwrap()
            )
        );
    }
    for _ in usize::from(RAMP_FRAMES)..TARGET_FRAMES - 1 {
        assert_eq!(source.next(), (0, 0));
    }
    assert_eq!(source.next(), (100, -100));
    assert_eq!(source.underruns, 0);
}

#[test]
fn incoming_write_is_part_of_the_high_watermark_decision() {
    let ring = Arc::new(ArrayQueue::new(CAPACITY_FRAMES));
    for _ in 0..HIGH_FRAMES - 1 {
        ring.push(QueuedFrame::Audio((10, 10))).unwrap();
    }
    let sink = AudioSink {
        ring: Arc::clone(&ring),
        debug: None,
    };

    sink.queue(&[(20, 20), (30, 30)]);

    assert_eq!(ring.len(), TARGET_FRAMES);
    assert!(matches!(ring.pop(), Some(QueuedFrame::Padding)));
    for _ in 1..TARGET_FRAMES - 2 {
        ring.pop().unwrap();
    }
    assert!(matches!(ring.pop(), Some(QueuedFrame::Audio((20, 20)))));
    assert!(matches!(ring.pop(), Some(QueuedFrame::Audio((30, 30)))));
}

#[test]
fn callback_fades_out_and_back_in_over_sixty_four_source_frames() {
    let ring = Arc::new(ArrayQueue::new(CAPACITY_FRAMES));
    for _ in 0..RAMP_FRAMES {
        ring.push(QueuedFrame::Audio((6_400, -6_400))).unwrap();
    }
    let mut source = CallbackSource::with_debug(Arc::clone(&ring), None);

    for step in 1..=RAMP_FRAMES {
        assert_eq!(
            source.next(),
            (
                i16::try_from(step * 100).unwrap(),
                -i16::try_from(step * 100).unwrap()
            )
        );
    }
    for step in (0..RAMP_FRAMES).rev() {
        assert_eq!(
            source.next(),
            (
                i16::try_from(step * 100).unwrap(),
                -i16::try_from(step * 100).unwrap()
            )
        );
    }
    assert_eq!(source.underruns, u64::from(RAMP_FRAMES));

    ring.push(QueuedFrame::Audio((6_400, -6_400))).unwrap();
    assert_eq!(source.next(), (100, -100));
}

#[test]
fn oversized_writes_recover_to_target_with_the_newest_audio() {
    let ring = new_ring();
    let sink = AudioSink {
        ring: Arc::clone(&ring),
        debug: None,
    };
    let frames: Vec<_> = (0..CAPACITY_FRAMES * 2)
        .map(|index| (index as i16, -(index as i16)))
        .collect();

    sink.queue(&frames);

    assert_eq!(ring.len(), TARGET_FRAMES);
    for _ in 0..RAMP_FRAMES {
        assert!(matches!(ring.pop(), Some(QueuedFrame::Padding)));
    }
    let expected_start = frames.len() - (TARGET_FRAMES - usize::from(RAMP_FRAMES));
    assert!(matches!(
        ring.pop(),
        Some(QueuedFrame::Audio(frame)) if frame == frames[expected_start]
    ));
    let mut last = None;
    while let Some(frame) = ring.pop() {
        last = Some(frame);
    }
    assert!(matches!(
        last,
        Some(QueuedFrame::Audio(frame)) if frame == *frames.last().unwrap()
    ));
}

#[test]
fn debug_snapshot_records_producer_consumer_and_callback_pressure() {
    let ring = Arc::new(ArrayQueue::new(CAPACITY_FRAMES));
    let debug = Arc::new(AudioDebugCounters::new(ring.len()));
    let sink = AudioSink {
        ring: Arc::clone(&ring),
        debug: Some(Arc::clone(&debug)),
    };

    sink.queue(&[(1, -1), (2, -2)]);
    let queue_depth_before = ring.len();
    let mut source = CallbackSource::with_debug(Arc::clone(&ring), Some(Arc::clone(&debug)));
    source.prefill_remaining = 0;
    while !ring.is_empty() {
        source.next();
    }
    source.next();
    source.flush_debug_callback(Some(queue_depth_before));

    let oversized = vec![(3, -3); CAPACITY_FRAMES * 2];
    sink.queue(&oversized);
    debug.record_callback_lateness(2_500_000);

    let snapshot = debug.snapshot();
    assert_eq!(snapshot.frames_produced, 2 + oversized.len() as u64);
    assert_eq!(snapshot.frames_consumed, 3);
    assert_eq!(snapshot.queue_min_depth, 0);
    assert_eq!(snapshot.queue_max_depth, TARGET_FRAMES);
    assert_eq!(snapshot.low_water_writes, 2);
    assert_eq!(snapshot.underruns_after_prefill, 1);
    assert_eq!(snapshot.overruns, 1);
    assert_eq!(snapshot.late_callbacks, 1);
    assert_eq!(snapshot.callback_lateness_us, 2_500);
    assert_eq!(snapshot.max_callback_lateness_us, 2_500);
    assert_eq!(sink.debug_snapshot(), Some(snapshot));
}

#[test]
fn callback_debug_counts_flush_once_at_the_callback_boundary() {
    let ring = Arc::new(ArrayQueue::new(CAPACITY_FRAMES));
    ring.push(QueuedFrame::Audio((1, -1))).unwrap();
    ring.push(QueuedFrame::Audio((2, -2))).unwrap();
    let debug = Arc::new(AudioDebugCounters::new(ring.len()));
    let mut source = CallbackSource::with_debug(Arc::clone(&ring), Some(Arc::clone(&debug)));

    source.next();
    source.next();
    assert_eq!(debug.snapshot().frames_consumed, 0);

    source.flush_debug_callback(Some(2));
    assert_eq!(debug.snapshot().frames_consumed, 2);
}

#[test]
fn sink_exposes_no_snapshot_when_diagnostics_are_disabled() {
    let sink = AudioSink {
        ring: Arc::new(ArrayQueue::new(CAPACITY_FRAMES)),
        debug: None,
    };

    assert_eq!(sink.debug_snapshot(), None);
}

/// A detached sink is a real queue that simply has no callback behind it.
///
/// It exists so the emulation thread's audio path can be driven and INSPECTED
/// in a test: what the pump queues is otherwise only observable by listening,
/// which is how a gain that was never applied went unnoticed.
#[test]
fn a_detached_sink_queues_audio_and_hands_it_back_without_the_prefill_padding() {
    let sink = AudioSink::detached();
    assert!(
        sink.take_queued_frames().is_empty(),
        "a fresh queue holds only the prefill padding, which is not audio"
    );

    let frames = [(1_i16, -1_i16), (2, -2), (3, -3)];
    sink.queue(&frames);
    assert_eq!(sink.take_queued_frames(), frames);
    assert!(
        sink.take_queued_frames().is_empty(),
        "taking the frames drains them"
    );
    assert!(sink.debug_snapshot().is_none());
}

/// A stream that fails DURING the open must leave recovery armed.
///
/// `open` starts the stream, so the callback can report a dead device inside
/// the very call that built it: an endpoint that vanished between the
/// enumeration and the `play()`. Clearing the failed flag AFTER the attempt
/// wipes that report: the player then holds a stream that will never call back,
/// believes it is healthy, and never tries again. That is a permanent-silence
/// hole inside the code whose whole job is to prevent permanent silence.
#[test]
fn a_stream_that_fails_while_opening_stays_armed_for_the_next_attempt() {
    let mut recovery = StreamRecovery::default();
    recovery.arm();
    let start = Instant::now();

    // The open succeeds, and the stream reports its own death from inside it.
    let opened = recovery.poll(start, |failed| {
        failed.store(true, Ordering::Release);
        Ok(7_u32)
    });
    assert_eq!(opened, Some(7), "the stream was installed");
    assert!(
        recovery.is_armed(),
        "the failure the new stream reported must survive the attempt that built it"
    );

    // And it is retried, once the backoff is up.
    let later = start + STREAM_RETRY_INTERVAL;
    assert_eq!(recovery.poll(later, |_| Ok(8_u32)), Some(8));
    assert!(!recovery.is_armed(), "a clean open leaves it healthy");
}

/// A device that opens and then errors must not be rebuilt every frame.
///
/// The GUI polls this once per frame. Without a backoff that covers the SUCCESS
/// path, a stream that reports failure immediately after each open sends the UI
/// thread through a full endpoint enumeration and a WASAPI stream build at the
/// host's refresh rate, forever.
#[test]
fn a_device_that_opens_then_errors_is_retried_on_the_backoff_not_every_frame() {
    let mut recovery = StreamRecovery::default();
    recovery.arm();
    let start = Instant::now();

    // Counted through an Arc so the closure that increments it and the
    // assertions that read it can coexist.
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempt = |recovery: &mut StreamRecovery, now: Instant| {
        let attempts = Arc::clone(&attempts);
        recovery.poll(now, move |failed| {
            attempts.fetch_add(1, Ordering::Relaxed);
            failed.store(true, Ordering::Release);
            Ok(1_u32)
        })
    };

    assert!(attempt(&mut recovery, start).is_some());
    // Sixty frames' worth of polling inside one backoff interval.
    for frame in 1..=60_u32 {
        let now = start + Duration::from_millis(u64::from(frame) * 8);
        if now >= start + STREAM_RETRY_INTERVAL {
            break;
        }
        assert!(
            attempt(&mut recovery, now).is_none(),
            "no second attempt inside the backoff"
        );
    }
    assert_eq!(
        attempts.load(Ordering::Relaxed),
        1,
        "one attempt, not one per frame"
    );

    assert!(
        attempt(&mut recovery, start + STREAM_RETRY_INTERVAL).is_some(),
        "and it does try again once the interval is up"
    );
    assert_eq!(attempts.load(Ordering::Relaxed), 2);
}

/// A failed attempt re-arms, so a device that is absent keeps being looked for.
///
/// This is also the startup case: a host with no default output device arms the
/// recovery at construction, and every failed attempt has to leave it armed or
/// plugging a headset in after launch would never be noticed.
#[test]
fn a_failed_open_re_arms_and_backs_off() {
    let mut recovery = StreamRecovery::default();
    assert!(
        recovery.poll(Instant::now(), |_| Ok(1_u32)).is_none(),
        "an unarmed recovery does not open anything"
    );

    recovery.arm();
    let start = Instant::now();
    let failing = |_: &Arc<AtomicBool>| Err("no default audio output device".into());
    assert_eq!(recovery.poll(start, failing), None::<u32>);
    assert!(
        recovery.is_armed(),
        "a failed attempt must leave the search running"
    );
    assert_eq!(
        recovery.poll(start + Duration::from_millis(1), failing),
        None::<u32>,
        "and must not hammer the host between attempts"
    );
    assert_eq!(
        recovery.poll(start + STREAM_RETRY_INTERVAL, |_| Ok(2_u32)),
        Some(2),
        "the device appearing later is picked up"
    );
}

// The converter tests below drive the real conversion path with a mixer-rate
// source and read back host-rate frames.

fn tone_source(freq: f64, amp: f64) -> impl FnMut() -> StereoFrame {
    let mut n = 0u64;
    move || {
        let s = (amp * (2.0 * std::f64::consts::PI * freq * n as f64 / 44_100.0).sin()).round();
        n += 1;
        (s as i16, s as i16)
    }
}

fn convert(out_hz: u32, frames: usize, mut source: impl FnMut() -> StereoFrame) -> Vec<f32> {
    let mut converter = HostConverter::new(out_hz);
    (0..frames)
        .map(|_| converter.next_frame(&mut source).0)
        .collect()
}

// The old callback arithmetic, kept here as the baseline the new path is
// measured against.
fn linear_convert(out_hz: i64, frames: usize, mut source: impl FnMut() -> StereoFrame) -> Vec<f32> {
    let (mut cur, mut next, mut phase) = ((0i16, 0i16), (0i16, 0i16), 0i64);
    let mut out = Vec::new();
    for _ in 0..frames {
        let fraction = phase as f32 / out_hz as f32;
        out.push((f32::from(cur.0) + (f32::from(next.0) - f32::from(cur.0)) * fraction) / 32768.0);
        phase += i64::from(SOURCE_HZ);
        while phase >= out_hz {
            phase -= out_hz;
            cur = next;
            next = source();
        }
    }
    out
}

/// Level of `freq` in `samples` relative to full scale, in dB, with a Hann window.
fn level_db(samples: &[f32], freq: f64, rate: f64) -> f64 {
    let (mut re, mut im, mut norm) = (0.0f64, 0.0f64, 0.0f64);
    let n = samples.len() as f64;
    for (i, &x) in samples.iter().enumerate() {
        let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n).cos();
        let angle = 2.0 * std::f64::consts::PI * freq * i as f64 / rate;
        re += f64::from(x) * w * angle.cos();
        im -= f64::from(x) * w * angle.sin();
        norm += w;
    }
    20.0 * ((re * re + im * im).sqrt() * 2.0 / norm).max(1e-12).log10()
}

// A 16 kHz tone sampled at 44.1 kHz leaves an image at 28.1 kHz. A 48 kHz host
// folds it down to 19.9 kHz, inside the audible band.
fn image_dbc(samples: &[f32]) -> f64 {
    let steady = &samples[2_000..2_000 + 4_800];
    level_db(steady, 19_900.0, 48_000.0) - level_db(steady, 16_000.0, 48_000.0)
}

#[test]
fn sinc_conversion_pushes_the_image_of_a_high_tone_below_minus_sixty_dbc() {
    let sinc = convert(48_000, 8_000, tone_source(16_000.0, 20_000.0));
    let linear = linear_convert(48_000, 8_000, tone_source(16_000.0, 20_000.0));
    let (sinc_db, linear_db) = (image_dbc(&sinc), image_dbc(&linear));
    eprintln!("image at 19.9 kHz: sinc {sinc_db:.1} dBc, linear {linear_db:.1} dBc");
    assert!(linear_db > -30.0, "the baseline has a strong image");
    assert!(sinc_db < -60.0, "sinc image {sinc_db:.1} dBc");
    // The tone itself must come through at about its input level.
    let tone = level_db(&sinc[2_000..6_800], 16_000.0, 48_000.0);
    let expected = 20.0 * (20_000.0f64 / 32_768.0).log10();
    assert!((tone - expected).abs() < 0.5, "{tone}");
}

#[test]
fn a_host_at_the_mixer_rate_gets_a_straight_copy() {
    let mut n = 0i16;
    let mut ramp = || {
        n = n.wrapping_add(257);
        (n, n.wrapping_neg())
    };
    let mut converter = HostConverter::new(SOURCE_HZ);
    assert!(matches!(converter, HostConverter::Copy));
    let mut reference = 0i16;
    for _ in 0..1_000 {
        reference = reference.wrapping_add(257);
        let (left, right) = converter.next_frame(&mut ramp);
        assert_eq!(left, f32::from(reference) / 32768.0);
        assert_eq!(right, f32::from(reference.wrapping_neg()) / 32768.0);
    }
}

#[test]
fn a_new_converter_for_a_new_rate_keeps_level_and_rate() {
    // A device change reopens the stream, which builds a new converter.
    for out_hz in [48_000u32, 96_000] {
        let frames = out_hz as usize / 2;
        let mut consumed = 0usize;
        let mut inner = tone_source(1_000.0, 20_000.0);
        let out = convert(out_hz, frames, || {
            consumed += 1;
            inner()
        });
        // Mixer frames drawn follow the rate ratio to within the filter lead-in.
        let expected = frames as f64 * 44_100.0 / f64::from(out_hz);
        assert!(
            (consumed as f64 - expected).abs() < 40.0,
            "{out_hz}: {consumed}"
        );
        let peak = out[out.len() / 2..]
            .iter()
            .fold(0.0f32, |m, &x| m.max(x.abs()));
        assert!(
            (peak - 20_000.0 / 32_768.0).abs() < 0.01,
            "{out_hz}: peak {peak}"
        );
    }
}

#[test]
#[ignore = "cost report, no assertion"]
fn conversion_cost_report() {
    for out_hz in [48_000u32, 192_000] {
        let reps = 20u32;
        let mut sink = 0.0f32;
        let mut converter = HostConverter::new(out_hz);
        let mut source = tone_source(440.0, 10_000.0);
        let start = Instant::now();
        for _ in 0..reps {
            for _ in 0..out_hz {
                sink += converter.next_frame(&mut source).0;
            }
        }
        let sinc = start.elapsed() / reps;
        let start = Instant::now();
        for _ in 0..reps {
            let mut source = tone_source(440.0, 10_000.0);
            sink += linear_convert(i64::from(out_hz), out_hz as usize, &mut source)[0];
        }
        let linear = start.elapsed() / reps;
        eprintln!("{out_hz} Hz: sinc {sinc:?} per audio second, linear {linear:?} (sink {sink})");
    }
}
