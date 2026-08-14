// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

#![forbid(unsafe_code)]

//! Decoding the OGG, MP3, WAV, and FLAC files a CUE sheet may name for its
//! AUDIO tracks into the raw Red Book frames the disc model serves.
//!
//! See `dev_docs/2026-08-14-cd-audio-decoding-design.md`.

mod decode;
mod probe;
mod sniff;
mod track;

pub use decode::{decode_into, decode_into_cancellable};
pub use probe::{
    CD_SAMPLE_RATE, CdAudioError, SAMPLES_PER_FRAME, TrackInfo, probe_info, sectors_for,
};
pub use sniff::{Container, SNIFF_BYTES, sniff};
pub use track::{DecodedTrack, Registry};

use std::path::Path;
use std::sync::Arc;

/// Measure `path` and return a source for it, or None when it is not an audio
/// container at all and should be mounted as raw bytes exactly as before.
///
/// The file is measured, never read into memory: Betrayal at Krondor is 155 MB
/// of Ogg across 62 tracks, and holding the decode of all of them would be
/// about 1.5 GB. Each track decodes on its own worker the first time the guest
/// asks for a frame of it, and `registry` bounds how many of one disc's
/// tracks hold decoded audio at the same time.
pub fn probe(
    registry: &Registry,
    path: &Path,
) -> Result<Option<Arc<dyn izarravm_core::AudioTrackSource>>, CdAudioError> {
    let Some(info) = probe_info(path)? else {
        return Ok(None);
    };
    Ok(Some(Arc::new(
        registry.track_with_info(path.to_path_buf(), info),
    )))
}
