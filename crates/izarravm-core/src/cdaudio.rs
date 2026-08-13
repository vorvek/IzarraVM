// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The contract between the disc model and whatever produces CD-DA frames for
//! a track whose bytes are not on the disc image.
//!
//! A CUE sheet may name an OGG, MP3, WAV, or FLAC file for an AUDIO track. Such
//! a file has no Red Book framing and its byte length says nothing useful about
//! its duration, so the disc model cannot derive the track's sector count from
//! it the way it does for a BINARY file. This trait is how the decoder tells the
//! disc model both things it needs: how long the track is, and what a given
//! frame of it contains.
//!
//! It lives here rather than in `izarravm-machine` so that the crate
//! implementing it does not have to depend on the whole emulator. Both sides
//! depend on this contract and neither depends on the other.

/// Bytes in one Red Book frame: 588 stereo samples of 16-bit PCM.
///
/// Pinned against `RAW_SECTOR` in `izarravm-machine`'s `cdimage` by a
/// compile-time assertion there, so the two definitions cannot drift apart.
pub const AUDIO_FRAME_BYTES: usize = 2352;

/// Frames for one audio track, produced from something other than the disc
/// image's own bytes.
///
/// `Send + Sync` because the disc lives on the emulation thread while decoding
/// happens on a worker; `Debug` because `CdImage` derives it. `CdImage` derives
/// `Clone` as well, which is why a source is held as `Arc<dyn AudioTrackSource>`
/// rather than `Box`: cloning a mounted disc has to share the one decode in
/// flight, not ask a half-decoded track to copy itself.
///
/// An implementor that holds decoded samples should write its `Debug` by hand
/// rather than derive one over the buffer. A whole disc of PCM is hundreds of
/// megabytes, and a derived `Debug` prints every byte of it the first time
/// anything formats the disc.
///
/// Note what the contract deliberately cannot express: nothing here separates a
/// frame that is still decoding from one belonging to a file that is corrupt and
/// will never yield anything. The disc model does not need the difference, since
/// both reach the listener as silence, so carrying it would be weight for one
/// side only. A caller that does need it -- a mount-time diagnostic, say -- has
/// to ask the concrete decoder rather than the trait object.
pub trait AudioTrackSource: std::fmt::Debug + Send + Sync {
    /// Red Book frames this track contributes to the disc timeline.
    ///
    /// Fixed for the life of the source and known before any decoding happens:
    /// the TOC is built from it at mount time, so it cannot be revised later
    /// without moving every following track's LBA.
    fn sectors(&self) -> u32;

    /// Frame `index` within this track, or None when it is not available yet.
    ///
    /// Index 0 is the track's first audio frame, its INDEX 01, and never the
    /// start of a pregap. An encoded file holds the audio alone, and the disc
    /// model has already counted the pregap into a track's start LBA before it
    /// subtracts that start from the address being read, so the two sides meet
    /// at INDEX 01 without either one adjusting for the gap.
    ///
    /// **This must not block.** It is called from the mixer pull on the
    /// emulation thread, so an implementor that waits here for the decoder to
    /// catch up stalls audio rendering and the machine behind it. Return None
    /// and let the frame go by instead.
    ///
    /// None is not an error. The mixer renders an absent frame as silence and
    /// advances the play head anyway, which is also what a real drive does: it
    /// never stalls the disc for the listener.
    fn frame(&self, index: u32) -> Option<[u8; AUDIO_FRAME_BYTES]>;
}

#[cfg(test)]
#[path = "cdaudio_test.rs"]
mod tests;
