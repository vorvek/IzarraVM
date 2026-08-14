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
    let Some(first) = mpeg_frame_len(head) else {
        return false;
    };
    let Some(next) = head.get(first..first + 4) else {
        return false;
    };
    mpeg_frame_len(next).is_some()
}

/// Length in bytes of the MPEG audio frame whose header leads `bytes`, or None
/// if that is not a valid header. Only the fields needed for the length are
/// decoded: the full layer and mode tables belong to the decoder.
fn mpeg_frame_len(bytes: &[u8]) -> Option<usize> {
    let header: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
    // Sync is eleven set bits.
    if header[0] != 0xFF || header[1] & 0xE0 != 0xE0 {
        return None;
    }
    // MPEG 1 Layer III only. Other versions and layers exist but never appear
    // in a CUE-packaged rip, and every one we decline here simply mounts raw.
    let version = (header[1] >> 3) & 0x03;
    let layer = (header[1] >> 1) & 0x03;
    if version != 0b11 || layer != 0b01 {
        return None;
    }
    /// Layer III bitrates in kbps, indexed by the header's four-bit field. Index
    /// 0 is "free format" and 15 is reserved; both are refused as a length.
    const BITRATES: [usize; 16] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
    ];
    /// MPEG 1 sample rates in Hz. Index 3 is reserved.
    const RATES: [usize; 4] = [44100, 48000, 32000, 0];
    let bitrate = BITRATES[usize::from(header[2] >> 4)];
    let rate = RATES[usize::from((header[2] >> 2) & 0x03)];
    if bitrate == 0 || rate == 0 {
        return None;
    }
    let padding = usize::from((header[2] >> 1) & 0x01);
    Some(144 * bitrate * 1000 / rate + padding)
}

#[cfg(test)]
#[path = "sniff_test.rs"]
mod tests;
