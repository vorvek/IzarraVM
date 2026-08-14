// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Turning an encoded file into the bytes a Red Book frame is made of.
//!
//! The output shape is fixed: 44.1 kHz, 16-bit, stereo, interleaved
//! little-endian, which is what `read_audio_frame` hands the mixer. Everything
//! a container might carry instead -- a different rate, one channel, more than
//! two, a wider sample format -- is converted here rather than rejected,
//! because a rip that plays in every other emulator should play in this one.
//!
//! The destination buffer is sized by the probe and never grows. A decode that
//! runs long is truncated and one that runs short leaves silence, so the TOC
//! written at mount time stays true whatever the decoder does. That is the
//! backstop, not the mechanism: the decode works from the probe's own frame
//! count, so the two agree by construction rather than by both being right.

use crate::probe::{CD_SAMPLE_RATE, CdAudioError, SAMPLES_PER_FRAME, TrackInfo, open};
use crate::sniff::{Container, SNIFF_BYTES, sniff};
use izarravm_audio::Resampler;
use izarravm_core::AUDIO_FRAME_BYTES;
use std::path::Path;

/// Sectors decoded between progress reports early in a track. Small, because
/// nothing is playable until the first report and the play head does not wait.
const FINE_PUBLISH: u32 = 4;
/// Sectors between reports once past [`FINE_PUBLISH_UNTIL`]. Larger, to keep
/// the synchronization a report implies off the mixer's path once the head is
/// no longer about to run into the decoder.
const COARSE_PUBLISH: u32 = 64;
/// Sector after which publishing relaxes from fine to coarse.
const FINE_PUBLISH_UNTIL: u32 = 64;

/// Decode `path` into `pcm`, calling `progress` with the number of whole
/// sectors that are complete and the bytes that just became complete.
///
/// The callback is handed the new bytes rather than left to read them out of
/// `pcm`, which it cannot do: `pcm` is borrowed mutably for the length of the
/// decode. Handing them over also says exactly which bytes are new, so a caller
/// republishing into a second buffer copies each byte once instead of recopying
/// the whole prefix at every report -- quadratic in the length of a track, and
/// a four-minute one reports some 280 times.
///
/// Returns the output sample frames actually produced, before padding or
/// truncation -- the quantity the length-agreement test compares against what
/// the mount published.
pub fn decode_into(
    path: &Path,
    info: TrackInfo,
    pcm: &mut [u8],
    progress: &mut dyn FnMut(u32, &[u8]),
) -> Result<u64, CdAudioError> {
    decode_into_cancellable(path, info, pcm, progress, &mut || false)
}

/// As [`decode_into`], but `cancel` is consulted at each publish point.
/// Returning true from it stops the decode where it stands and leaves the rest
/// of `pcm` as it was, which for a fresh buffer is silence.
pub fn decode_into_cancellable(
    path: &Path,
    info: TrackInfo,
    pcm: &mut [u8],
    progress: &mut dyn FnMut(u32, &[u8]),
    cancel: &mut dyn FnMut() -> bool,
) -> Result<u64, CdAudioError> {
    let container = container_of(path)?;
    let mut source = open(path, container)?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&source.params, &Default::default())
        .map_err(|err| decode_error(path, err))?;

    // The zero check is not redundant with the probe's refusal: this function
    // takes a `TrackInfo` from its caller, and a zero rate here is a step of
    // 0.0 in the resampler, which emits output forever without consuming input.
    // A loop that cannot end is worse than a track at the wrong pitch.
    let mut resampler = (info.sample_rate != CD_SAMPLE_RATE && info.sample_rate != 0)
        .then(|| Resampler::new(info.sample_rate, CD_SAMPLE_RATE));

    // Counted in source sample frames, and from the probe. Nothing is skipped
    // at the head: an MPEG stream does carry the encoder's delay ahead of the
    // audio, but the reader has already applied it by the time a decoded buffer
    // reaches here -- the first packet of a LAME file arrives stamped
    // `ts = -1105` and yields 47 of its 1152 frames, and the last is trimmed
    // to its padding the same way. Skipping again here would take the delay off
    // twice and cut the track short, which is what it did until measured.
    //
    // So the probe's subtraction and the reader's trim are one operation done
    // twice over, from the same declared numbers, and they agree: 8820 either
    // way on the LAME fixture. `remaining` is what holds them to that.
    let mut remaining = info.frames;

    let mut produced = 0u64;
    let mut written = 0u64;
    let mut last_published = 0u32;
    let mut published_bytes = 0usize;
    let total_sectors = (pcm.len() / AUDIO_FRAME_BYTES) as u32;
    let mut interleaved: Vec<i16> = Vec::new();
    let mut pairs: Vec<(i32, i32)> = Vec::new();

    while remaining > 0 {
        let packet = match source.reader.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(symphonia::core::errors::Error::ResetRequired) => break,
            Err(symphonia::core::errors::Error::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(err) => return Err(decode_error(path, err)),
        };
        if packet.track_id != source.track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // A corrupt packet is skipped, not fatal: a scratched rip should
            // glitch where the damage is and keep playing, which is what a real
            // drive does with a bad frame.
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(err) => return Err(decode_error(path, err)),
        };

        // `copy_to_vec_interleaved` converts whatever sample format the codec
        // produced -- u8 through f64 -- into 16-bit for us, so only the channel
        // count is left to deal with here.
        let channels = decoded.spec().channels().count().max(1);
        interleaved.clear();
        decoded.copy_to_vec_interleaved(&mut interleaved);
        let frames_in_packet = interleaved.len() / channels;

        let take = remaining.min(frames_in_packet as u64) as usize;
        remaining -= take as u64;
        if take == 0 {
            continue;
        }

        pairs.clear();
        pairs.reserve(take);
        for frame in interleaved[..take * channels].chunks(channels) {
            // A CD has two channels. Mono is duplicated so it is heard on both
            // rather than only on the left; anything past the second is
            // dropped, which is the same reduction any player does for a
            // surround source on a stereo device.
            let left = i32::from(frame[0]);
            let right = frame.get(1).map_or(left, |s| i32::from(*s));
            pairs.push((left, right));
        }

        let out = match resampler.as_mut() {
            Some(resampler) => resampler.process(&pairs),
            None => std::mem::take(&mut pairs),
        };
        produced += out.len() as u64;

        for (left, right) in &out {
            let byte = written as usize * 4;
            // The mount already published `total_sectors`, so a decode that
            // runs long loses the surplus rather than writing into the next
            // track's space. It is still counted in `produced`, which is what
            // makes a disagreement visible instead of silently absorbed.
            let Some(slot) = pcm.get_mut(byte..byte + 4) else {
                break;
            };
            slot[..2].copy_from_slice(&clamp_i16(*left).to_le_bytes());
            slot[2..].copy_from_slice(&clamp_i16(*right).to_le_bytes());
            written += 1;
        }
        if resampler.is_none() {
            // `out` was moved out of `pairs`; give the allocation back so the
            // next packet does not have to ask for one.
            pairs = out;
        }

        let complete = (written / SAMPLES_PER_FRAME) as u32;
        let step = if complete < FINE_PUBLISH_UNTIL {
            FINE_PUBLISH
        } else {
            COARSE_PUBLISH
        };
        if complete >= last_published + step {
            last_published = complete;
            let upto = (complete as usize * AUDIO_FRAME_BYTES).min(pcm.len());
            progress(complete, &pcm[published_bytes..upto]);
            published_bytes = upto;
            if cancel() {
                return Ok(produced);
            }
        }
    }

    // Whatever is left of `pcm` was never written, and a fresh buffer is zeros,
    // which is silence. Only the sectors that actually hold audio are handed
    // over: a caller mirroring into its own zeroed buffer would be copying
    // zeros onto zeros for the rest, and on a decode that ended far short of
    // the promise -- a file replaced or truncated between mount and play --
    // that is hundreds of megabytes of memcpy, which `DecodedTrack` performs
    // while holding the lock the mixer pull takes.
    //
    // The full sector count still goes out, because it is what tells the caller
    // the track is whole and the tail is silence rather than pending.
    let audio_bytes = (written as usize * 4).min(pcm.len());
    let covered = audio_bytes
        .div_ceil(AUDIO_FRAME_BYTES)
        .saturating_mul(AUDIO_FRAME_BYTES)
        .min(pcm.len())
        .max(published_bytes);
    progress(total_sectors, &pcm[published_bytes..covered]);
    Ok(produced)
}

fn decode_error(path: &Path, message: impl std::fmt::Display) -> CdAudioError {
    CdAudioError::Decode {
        path: path.display().to_string(),
        message: message.to_string(),
    }
}

/// Identify the file again at decode time. The mount deliberately let go of its
/// handle so the user could still move or delete their own rips, which means
/// this is a second look at a file that may have changed since -- and a file
/// that is no longer a container must fail rather than have its new contents
/// played as audio.
fn container_of(path: &Path) -> Result<Container, CdAudioError> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|source| CdAudioError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let len = file
        .metadata()
        .map_err(|source| CdAudioError::Io {
            path: path.display().to_string(),
            source,
        })?
        .len();
    let mut head = vec![0u8; SNIFF_BYTES];
    let mut filled = 0;
    while filled < head.len() {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(source) => {
                return Err(CdAudioError::Io {
                    path: path.display().to_string(),
                    source,
                });
            }
        }
    }
    head.truncate(filled);
    sniff(&head, len)
        .ok_or_else(|| decode_error(path, "is no longer an audio container the CD path can read"))
}

fn clamp_i16(sample: i32) -> i16 {
    sample.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

#[cfg(test)]
#[path = "decode_test.rs"]
mod tests;
