// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Deciding what a file the CUE named actually contains.
//!
//! The sheet's own `FILE` type token is not trustworthy for this: rippers write
//! `MP3` for an Ogg file and `WAVE` for raw bytes often enough that honoring the
//! token would fail on sheets other emulators mount without complaint. Three of
//! the five real sheets this work was tested against declare `MP3` for files
//! whose first four bytes are `OggS`. So the bytes decide, and the token is kept
//! only for diagnostics and for `MOTOROLA`, which content cannot reveal.
//!
//! Sniffing must be conservative in one direction specifically. Mistaking a
//! container for raw bytes only reproduces today's behavior; mistaking raw CD-DA
//! for a container fabricates a track length and mounts wrong data, which is the
//! failure this whole change exists to remove.

/// Bytes read from the head of a file to identify it.
pub const SNIFF_BYTES: usize = 4096;

/// One raw Red Book frame; a raw audio file is always a whole number of these.
const RAW_FRAME: u64 = 2352;
/// A raw frame stored with its 96-byte subchannel tail (CD+G, MODE1/2448).
const RAW_FRAME_SUBCHANNEL: u64 = 2448;

/// A container this crate can decode a CD-DA track out of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Ogg,
    Flac,
    Wav,
    Mp3,
}

/// Identify `head` (the first [`SNIFF_BYTES`] of a file `len` bytes long), or
/// None when it is not one of the containers we decode -- which the caller
/// treats as "mount it as raw bytes", not as an error.
pub fn sniff(head: &[u8], len: u64) -> Option<Container> {
    if head.starts_with(b"OggS") {
        return Some(Container::Ogg);
    }
    if head.starts_with(b"fLaC") {
        return Some(Container::Flac);
    }
    if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WAVE") {
        return Some(Container::Wav);
    }
    if head.starts_with(b"ID3") {
        return Some(Container::Mp3);
    }
    is_bare_mpeg(head, len).then_some(Container::Mp3)
}

/// An MP3 with no ID3 tag opens directly on a frame header. `0xFF 0xEx` is also
/// an ordinary 16-bit PCM sample pair, so a lone sync proves nothing, and over a
/// whole track a second one will land a frame length after the first by
/// coincidence. Two independent guards keep raw CD-DA out: a raw audio file's
/// length is always a whole number of frames, and a real stream has a second
/// valid header exactly one frame after the first.
fn is_bare_mpeg(head: &[u8], len: u64) -> bool {
    if len.is_multiple_of(RAW_FRAME) || len.is_multiple_of(RAW_FRAME_SUBCHANNEL) {
        return false;
    }
    let Some(first) = mpeg_header(head) else {
        return false;
    };
    let Some(next) = head.get(first.frame_len..first.frame_len + 4) else {
        return false;
    };
    // The second header must describe the same stream: same version, layer
    // and sample rate.
    mpeg_header(next).is_some_and(|second| {
        second.version == first.version && second.layer == first.layer && second.rate == first.rate
    })
}

/// The fields of an MPEG Layer III frame header that identify a stream.
struct MpegHeader {
    /// Version bits: 0b11 MPEG-1, 0b10 MPEG-2, 0b00 MPEG-2.5.
    version: u8,
    layer: u8,
    /// Sample rate in Hz.
    rate: usize,
    /// Frame length in bytes, with padding.
    frame_len: usize,
}

/// Parse the Layer III frame header that leads `bytes`, or None if that is not
/// a valid header. Only the fields needed for the length are decoded: the full
/// layer and mode tables belong to the decoder.
fn mpeg_header(bytes: &[u8]) -> Option<MpegHeader> {
    let header: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
    // Sync is eleven set bits.
    if header[0] != 0xFF || header[1] & 0xE0 != 0xE0 {
        return None;
    }
    // MPEG-1, MPEG-2 and MPEG-2.5, Layer III only. Version bits 01 are
    // reserved. Every other layer mounts as raw bytes.
    let version = (header[1] >> 3) & 0x03;
    let layer = (header[1] >> 1) & 0x03;
    if version == 0b01 || layer != 0b01 {
        return None;
    }
    let mpeg1 = version == 0b11;
    /// MPEG-1 Layer III bitrates in kbps, indexed by the header's four-bit
    /// field. Index 0 is "free format" and 15 is reserved; both are refused.
    const BITRATES_V1: [usize; 16] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
    ];
    /// MPEG-2 and MPEG-2.5 Layer III bitrates in kbps.
    const BITRATES_V2: [usize; 16] = [
        0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
    ];
    /// Sample rates in Hz by version, indexed by the two-bit field. Index 3 is
    /// reserved.
    const RATES_V1: [usize; 4] = [44100, 48000, 32000, 0];
    const RATES_V2: [usize; 4] = [22050, 24000, 16000, 0];
    const RATES_V25: [usize; 4] = [11025, 12000, 8000, 0];
    let bitrate = if mpeg1 { BITRATES_V1 } else { BITRATES_V2 }[usize::from(header[2] >> 4)];
    let rates = match version {
        0b11 => RATES_V1,
        0b10 => RATES_V2,
        _ => RATES_V25,
    };
    let rate = rates[usize::from((header[2] >> 2) & 0x03)];
    if bitrate == 0 || rate == 0 {
        return None;
    }
    let padding = usize::from((header[2] >> 1) & 0x01);
    // A Layer III frame holds 1152 samples in MPEG-1 and 576 in MPEG-2 and
    // MPEG-2.5, so the byte count per frame halves.
    let slot = if mpeg1 { 144 } else { 72 };
    Some(MpegHeader {
        version,
        layer,
        rate,
        frame_len: slot * bitrate * 1000 / rate + padding,
    })
}

#[cfg(test)]
#[path = "sniff_test.rs"]
mod tests;
