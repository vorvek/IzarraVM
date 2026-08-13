// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::sync::Arc;

/// A source that reports three sectors and serves only the first two, standing
/// in for a track whose decode has not caught up with the play head yet.
#[derive(Debug)]
struct PartialSource;

impl AudioTrackSource for PartialSource {
    fn sectors(&self) -> u32 {
        3
    }

    fn frame(&self, index: u32) -> Option<[u8; AUDIO_FRAME_BYTES]> {
        (index < 2).then_some([index as u8; AUDIO_FRAME_BYTES])
    }
}

#[test]
fn source_is_object_safe_behind_arc() {
    // The disc model stores these as `Arc<dyn AudioTrackSource>`, so the trait
    // must stay object-safe: no generics, no `Self` in return position.
    let source: Arc<dyn AudioTrackSource> = Arc::new(PartialSource);
    assert_eq!(source.sectors(), 3);
    assert_eq!(source.frame(0).unwrap()[0], 0);
    assert_eq!(source.frame(1).unwrap()[0], 1);
}

#[test]
fn frame_past_the_decoded_region_is_absent() {
    // None means "not decoded yet", which the mixer renders as silence. It is
    // deliberately not distinguishable from "no such frame" -- see the spec's
    // "What the listener actually loses".
    let source = PartialSource;
    assert!(source.frame(2).is_none());
    assert!(source.frame(99).is_none());
}

#[test]
fn a_red_book_frame_is_2352_bytes() {
    // Pinned against cdimage::RAW_SECTOR by a compile-time assert in cdimage.rs.
    // Written as the arithmetic the constant's own doc claims -- 588 stereo
    // samples of 16-bit PCM -- so the framing is pinned alongside the number.
    // The mixer reaches 588 from the other direction, dividing its frame size by
    // those same 4 bytes per sample; nothing here can see that constant, but
    // holding the framing honest on this side keeps the number it divides right.
    assert_eq!(AUDIO_FRAME_BYTES, 588 * 2 * 2);
}

#[test]
fn a_source_survives_the_move_to_a_worker_thread() {
    // This is what pins `Send + Sync`. `Arc<dyn AudioTrackSource>` builds with or
    // without those supertraits, so the test above it would keep passing if they
    // were dropped -- and they are exactly what looks removable to someone
    // reading this file before the decoder's worker thread exists to need them.
    // Sending the Arc across a thread boundary requires both, so removing either
    // one fails here instead of much later.
    let source: Arc<dyn AudioTrackSource> = Arc::new(PartialSource);
    let worker = std::thread::spawn(move || source.frame(1).map(|frame| frame[0]));
    assert_eq!(worker.join().unwrap(), Some(1));
}
