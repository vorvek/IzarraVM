// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// A valid MPEG-1 Layer III frame header: 128 kbps, 44.1 kHz, no padding.
const MPEG_HEADER: [u8; 4] = [0xFF, 0xFB, 0x90, 0x00];

/// Byte offset of the frame that follows a [`MPEG_HEADER`] frame, which is that
/// frame's own length: `144 * 128000 / 44100`, truncated.
const MPEG_FRAME_LEN: usize = 417;

/// A buffer that opens with `head` and is padded to `len` bytes.
fn head_of(head: &[u8], len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    bytes[..head.len()].copy_from_slice(head);
    bytes
}

/// A sniff buffer carrying two consecutive valid MPEG frame headers -- what a
/// real MP3 with no ID3 tag looks like from the front.
fn two_mpeg_frames() -> Vec<u8> {
    let mut bytes = vec![0u8; SNIFF_BYTES];
    bytes[0..4].copy_from_slice(&MPEG_HEADER);
    bytes[MPEG_FRAME_LEN..MPEG_FRAME_LEN + 4].copy_from_slice(&MPEG_HEADER);
    bytes
}

#[test]
fn recognizes_the_four_supported_containers() {
    assert_eq!(
        sniff(&head_of(b"OggS\0\x02", 4096), 4096),
        Some(Container::Ogg)
    );
    assert_eq!(
        sniff(&head_of(b"fLaC\0\0\0\x22", 4096), 4096),
        Some(Container::Flac)
    );
    let mut wav = head_of(b"RIFF", 4096);
    wav[8..12].copy_from_slice(b"WAVE");
    assert_eq!(sniff(&wav, 4096), Some(Container::Wav));
    assert_eq!(
        sniff(&head_of(b"ID3\x04\0\0", 4096), 4096),
        Some(Container::Mp3)
    );
}

#[test]
fn riff_that_is_not_wave_is_not_a_container() {
    // RIFF is a family, not a format: an AVI opens the same way.
    let mut avi = head_of(b"RIFF", 4096);
    avi[8..12].copy_from_slice(b"AVI ");
    assert_eq!(sniff(&avi, 4096), None);
}

#[test]
fn a_bare_mpeg_stream_needs_two_consecutive_frames() {
    // A real MP3 with no ID3 tag starts at a frame header, and the next header
    // sits exactly one frame later.
    assert_eq!(sniff(&two_mpeg_frames(), 4096), Some(Container::Mp3));

    // One header alone is not enough. This is the control for the two
    // length-guard tests below: it fixes what a *lone* sync proves, so those
    // tests can hold the byte pattern constant and vary only the file length.
    let mut lone = vec![0u8; SNIFF_BYTES];
    lone[0..4].copy_from_slice(&MPEG_HEADER);
    assert_eq!(sniff(&lone, 4096), None);
}

#[test]
fn raw_cd_da_whose_bytes_look_exactly_like_mpeg_is_not_mp3() {
    // Every byte the sniffer reads to identify a bare MPEG stream can occur
    // inside raw Red Book audio: 0xFF 0xFB is an ordinary 16-bit sample pair,
    // and across a whole track another one will eventually land exactly a frame
    // length later. Raw audio misidentified as MP3 is a wrong-data mount, the
    // failure class this work exists to remove, so the file's length is an
    // independent guard -- raw audio is always a whole number of frames.
    //
    // The buffer here is byte-for-byte the one that sniffs as MP3 above. Only
    // the length differs, so nothing but the length guard can be rejecting it.
    assert_eq!(sniff(&two_mpeg_frames(), 2352 * 4), None);
}

#[test]
fn subchannel_sized_raw_audio_is_not_mp3() {
    // A CD+G or MODE1/2448 rip is a whole number of 2448-byte frames and not of
    // 2352-byte ones, so it needs its own guard: 2448 * 3 is not a multiple of
    // 2352, and this passes only because the second length is also checked.
    assert_eq!(sniff(&two_mpeg_frames(), 2448 * 3), None);
}

#[test]
fn an_ordinary_binary_file_is_not_a_container() {
    assert_eq!(sniff(&head_of(b"\x00\x01\x02\x03", 4096), 4096), None);
}
