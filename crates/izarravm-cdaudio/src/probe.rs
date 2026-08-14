// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Deciding, at mount time and without committing to a decode, how long a
//! track is and what shape its samples are.
//!
//! The TOC is built from this number and cannot be revised afterwards: a
//! track's sector count fixes every following track's LBA. So the probe is
//! authoritative and the later decode conforms to it, rather than the other way
//! round.

use crate::sniff::{Container, SNIFF_BYTES, sniff};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use symphonia::core::codecs::audio::{AudioCodecId, AudioCodecParameters, well_known};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Samples per second the CD-DA path renders at.
pub const CD_SAMPLE_RATE: u32 = 44100;
/// Stereo sample pairs in one Red Book frame.
pub const SAMPLES_PER_FRAME: u64 = 588;

#[derive(Debug, thiserror::Error)]
pub enum CdAudioError {
    #[error("could not read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is {codec}, which the CD-ROM emulation cannot decode")]
    UnsupportedCodec { path: String, codec: String },
    #[error("could not decode {path}: {message}")]
    Decode { path: String, message: String },
}

/// A track's shape and its length on the disc timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackInfo {
    pub sample_rate: u32,
    pub channels: u16,
    /// Source sample frames of audio, with any encoder delay and trailing
    /// padding already excluded.
    ///
    /// Carried so that the decode works from the number the TOC was built from
    /// rather than deriving its own. The two agreeing is the invariant the
    /// whole design rests on, and the cheapest way to guarantee it is to have
    /// only one number.
    pub frames: u64,
    /// Red Book frames this track occupies once converted to 44.1 kHz stereo.
    pub sectors: u32,
}

/// The longest track this will measure, in Red Book frames: 100 minutes.
///
/// Longer than any disc a drive will accept -- a 99-minute CD-R is the extreme
/// of what was ever pressed -- so nothing legitimate is turned away. It exists
/// because the frame count arrives from a container and nothing upstream
/// constrains it: an Ogg whose final granule position is corrupt hands back a
/// number near `u64::MAX`, and the sector count derived from it would size both
/// the TOC and, on first touch, a `vec![0u8; sectors * 2352]` on the emulation
/// thread. An allocation that large fails, and this build aborts on panic.
pub const MAX_TRACK_SECTORS: u32 = 100 * 60 * 75;

/// Sector count for `src_frames` samples at `src_rate`, converted to CD-DA.
///
/// Ceiling at both steps: a partial output sample still has to be carried, and
/// a partial frame still occupies a whole sector on the disc. The decode pads
/// the tail with silence to match.
///
/// Saturating rather than wrapping, because `src_frames` is whatever the
/// container said: the multiply overflows `u64` above about 4.2e14 frames,
/// which panics in a debug build and silently wraps to a plausible-looking
/// small number in a release one. Saturating sends it to `u32::MAX` instead,
/// where [`MAX_TRACK_SECTORS`] refuses it by name.
pub fn sectors_for(src_frames: u64, src_rate: u32) -> u32 {
    let rate = u64::from(src_rate.max(1));
    let out_samples = src_frames
        .saturating_mul(u64::from(CD_SAMPLE_RATE))
        .div_ceil(rate);
    out_samples
        .div_ceil(SAMPLES_PER_FRAME)
        .try_into()
        .unwrap_or(u32::MAX)
}

/// Identify `path` and measure it. Ok(None) means it is not an audio container
/// at all, which the caller mounts as raw bytes exactly as before.
pub fn probe_info(path: &Path) -> Result<Option<TrackInfo>, CdAudioError> {
    let mut head = vec![0u8; SNIFF_BYTES];
    let mut file = File::open(path).map_err(|source| io_error(path, source))?;
    let read = read_up_to(&mut file, &mut head).map_err(|source| io_error(path, source))?;
    head.truncate(read);
    let len = file
        .metadata()
        .map_err(|source| io_error(path, source))?
        .len();
    let Some(container) = sniff(&head, len) else {
        return Ok(None);
    };
    // The handle is dropped at the end of this function. Holding it for the
    // mount's life would stop the user moving or deleting their own rips on
    // Windows while a disc is mounted; the decode worker reopens the path.
    measure(path, container).map(Some)
}

fn io_error(path: &Path, source: std::io::Error) -> CdAudioError {
    CdAudioError::Io {
        path: path.display().to_string(),
        source,
    }
}

fn decode_error(path: &Path, message: impl std::fmt::Display) -> CdAudioError {
    CdAudioError::Decode {
        path: path.display().to_string(),
        message: message.to_string(),
    }
}

/// `Read::read` is permitted to return fewer bytes than asked for without being
/// at end of file, so a single call cannot be trusted to have filled the sniff
/// buffer. `read_exact` is not usable either: a file shorter than the buffer is
/// ordinary here, not an error.
fn read_up_to(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

fn measure(path: &Path, container: Container) -> Result<TrackInfo, CdAudioError> {
    let mut open = open(path, container)?;
    // A declared rate of zero is refused rather than defaulted. It is not a
    // legal stream, its duration is undefined, and it reaches the resampler as
    // a step of 0.0 -- a loop that emits output forever without consuming
    // input, on a worker thread, until the allocation fails and takes the
    // process with it. symphonia's RIFF parser does not validate the field, so
    // a WAV with a zeroed fmt chunk gets this far.
    if open.params.sample_rate == Some(0) {
        return Err(decode_error(path, "declares a sample rate of zero"));
    }
    let sample_rate = open.params.sample_rate.unwrap_or(CD_SAMPLE_RATE);
    let channels = open
        .params
        .channels
        .as_ref()
        .map_or(2, |c| u16::try_from(c.count()).unwrap_or(u16::MAX));

    let src_frames = match (container, open.num_frames) {
        // For MPEG the container's own count is never trusted. When the encoder
        // left no Xing/LAME header symphonia extrapolates one from the average
        // bitrate, and it hands that back as a `Some`, so any fallback
        // conditioned on `None` would never run. Measured on a 3 s tagless VBR
        // tone: 107136 frames declared against a true 132300, 19% short. (A
        // short enough file does report `None`, which is why the 0.2 s fixture
        // alone cannot show this.) Walking parses frame headers without
        // decoding anything and is exact.
        (Container::Mp3, _) => {
            let coded = walk_packets(&mut open.reader, open.track_id, path)?;
            // The walk counts coded frames, and an MPEG stream begins with the
            // encoder's own delay and ends padded out to a frame boundary --
            // 1105 + 443 samples for LAME on a 0.2 s tone, which would put the
            // track three sectors long. Only a Xing/LAME header records those,
            // so a tagless stream keeps them: nothing in it says where the
            // audio truly starts.
            //
            // This subtraction is MPEG's alone. The Ogg reader has already
            // trimmed by granule position before a packet duration is
            // reported, so applying the same correction there would take the
            // trim off twice and cut the track short.
            coded
                .saturating_sub(u64::from(open.delay.unwrap_or(0)))
                .saturating_sub(u64::from(open.padding.unwrap_or(0)))
        }
        (_, Some(frames)) => frames,
        // A FLAC whose STREAMINFO says total_samples = 0, or a WAV whose data
        // chunk length was never patched. Both are legal and both come out of
        // pipe-fed encodes. Counting costs one pass and only happens for these,
        // so it is cheaper than refusing the disc.
        (_, None) => walk_packets(&mut open.reader, open.track_id, path)?,
    };

    let sectors = sectors_for(src_frames, sample_rate);
    if sectors > MAX_TRACK_SECTORS {
        return Err(decode_error(
            path,
            format!(
                "measures {sectors} Red Book frames, longer than the {MAX_TRACK_SECTORS} \
                 a disc can hold; the container's length is not believable"
            ),
        ));
    }

    Ok(TrackInfo {
        sample_rate,
        channels,
        frames: src_frames,
        sectors,
    })
}

/// Sum every packet's duration to the end of the stream. Header parsing only --
/// no packet is decoded.
fn walk_packets(
    reader: &mut Box<dyn FormatReader>,
    track_id: u32,
    path: &Path,
) -> Result<u64, CdAudioError> {
    let mut frames = 0u64;
    loop {
        match reader.next_packet() {
            Ok(Some(packet)) => {
                if packet.track_id == track_id {
                    frames = frames.saturating_add(packet.dur.get());
                }
            }
            // End of stream. 0.6 reports this as a packet-less Ok rather than
            // as an unexpected-EOF I/O error, so an error arm never sees it.
            Ok(None) => break,
            // The track list has changed mid-stream -- a chained Ogg. Every
            // page after this belongs to a different logical stream, so its
            // frames are not this track's and the walk is finished.
            Err(symphonia::core::errors::Error::ResetRequired) => break,
            Err(symphonia::core::errors::Error::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(err) => return Err(decode_error(path, err)),
        }
    }
    Ok(frames)
}

/// An opened container, positioned at the start of its audio track.
///
/// Shared by the probe and the decoder so the two cannot disagree about which
/// track of a container is the audio one, or about the shape of its samples.
pub(crate) struct OpenTrack {
    pub reader: Box<dyn FormatReader>,
    pub track_id: u32,
    pub params: AudioCodecParameters,
    /// What the container claims the track's length is. `None` when it does not
    /// say; for MPEG it is an extrapolation and is not used at all.
    pub num_frames: Option<u64>,
    /// Leading samples the encoder inserted, which are not part of the audio.
    /// Only a Xing/LAME header records this for MPEG; `None` otherwise.
    pub delay: Option<u32>,
    /// Trailing samples the encoder added to fill its last frame.
    pub padding: Option<u32>,
}

pub(crate) fn open(path: &Path, container: Container) -> Result<OpenTrack, CdAudioError> {
    let file = File::open(path).map_err(|source| io_error(path, source))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    // The hint comes from the sniff rather than from the file's name: a CUE
    // sheet's own FILE token has already been shown to lie about what a file
    // holds, and so does its extension on the same discs.
    let mut hint = Hint::new();
    hint.with_extension(match container {
        Container::Ogg => "ogg",
        Container::Flac => "flac",
        Container::Wav => "wav",
        Container::Mp3 => "mp3",
    });
    let reader = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|err| decode_error(path, err))?;
    let track = reader
        .default_track(TrackType::Audio)
        .or_else(|| reader.first_track(TrackType::Audio))
        .ok_or_else(|| decode_error(path, "no audio track in the container"))?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| decode_error(path, "audio track declares no codec parameters"))?;
    // Opus rides inside Ogg, so the container sniffs as OggS and only the codec
    // identifies it. The registry knows the id but we did not compile a decoder
    // for it, so name the codec rather than failing later with a generic error
    // from the middle of a decode.
    if symphonia::default::get_codecs()
        .get_audio_decoder(params.codec)
        .is_none()
    {
        return Err(CdAudioError::UnsupportedCodec {
            path: path.display().to_string(),
            codec: codec_name(params.codec),
        });
    }
    let params = params.clone();
    let track_id = track.id;
    let num_frames = track.num_frames;
    let delay = track.delay;
    let padding = track.padding;
    Ok(OpenTrack {
        reader,
        track_id,
        params,
        num_frames,
        delay,
        padding,
    })
}

/// A human-readable name for a codec we did not compile support for. The
/// registry cannot describe a codec it cannot instantiate, so the ones likely
/// to turn up in a rip are named here and anything else falls back to its id.
fn codec_name(codec: AudioCodecId) -> String {
    match codec {
        well_known::CODEC_ID_OPUS => "Opus".to_string(),
        well_known::CODEC_ID_AAC => "AAC".to_string(),
        well_known::CODEC_ID_ALAC => "ALAC".to_string(),
        well_known::CODEC_ID_SPEEX => "Speex".to_string(),
        well_known::CODEC_ID_MUSEPACK => "Musepack".to_string(),
        well_known::CODEC_ID_WMA => "WMA".to_string(),
        other => format!("codec {other:?}"),
    }
}

/// What the container itself claims the track's length is, which for MPEG is an
/// extrapolation. Exists so a test can show that the probe's answer did not
/// come from here.
#[cfg(test)]
fn declared_frames(path: &Path) -> Result<Option<u64>, CdAudioError> {
    let head = std::fs::read(path).map_err(|source| io_error(path, source))?;
    let len = head.len() as u64;
    let container = sniff(&head, len).expect("fixture is a container");
    Ok(open(path, container)?.num_frames)
}

/// The exact frame count, from walking every packet.
#[cfg(test)]
fn walked_frames(path: &Path) -> Result<u64, CdAudioError> {
    let head = std::fs::read(path).map_err(|source| io_error(path, source))?;
    let len = head.len() as u64;
    let container = sniff(&head, len).expect("fixture is a container");
    let mut open = open(path, container)?;
    walk_packets(&mut open.reader, open.track_id, path)
}

#[cfg(test)]
#[path = "probe_test.rs"]
mod tests;
