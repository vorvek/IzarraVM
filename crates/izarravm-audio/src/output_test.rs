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
    let mut source = CallbackSource::with_debug(Arc::clone(&ring), Some(Arc::clone(&debug)));
    source.prefill_remaining = 0;
    while !ring.is_empty() {
        source.next();
    }
    source.next();

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
}
