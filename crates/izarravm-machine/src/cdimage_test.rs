// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Build a minimal single-track ISO whose first two sectors carry markers.
fn tiny_iso(sectors: u32) -> Vec<u8> {
    let mut bytes = vec![0u8; sectors as usize * DATA_SECTOR];
    bytes[0] = 0xCD;
    bytes[DATA_SECTOR] = 0x02; // first byte of LBA 1
    bytes
}

#[test]
fn iso_is_one_data_track() {
    let img = CdImage::from_iso(tiny_iso(4)).unwrap();
    assert_eq!(img.track_count(), 1);
    assert_eq!(img.total_sectors(), 4);
    let t = &img.tracks()[0];
    assert_eq!(t.mode, TrackMode::Mode1_2048);
    assert_eq!((t.start_lba, t.sectors), (0, 4));
}

#[test]
fn iso_reads_back_logical_sectors() {
    let img = CdImage::from_iso(tiny_iso(4)).unwrap();
    assert_eq!(img.read_data_sector(0).unwrap()[0], 0xCD);
    assert_eq!(img.read_data_sector(1).unwrap()[0], 0x02);
    // Past the end reads nothing.
    assert!(img.read_data_sector(4).is_none());
}

#[test]
fn iso_rejects_unaligned_length() {
    assert!(CdImage::from_iso(vec![0u8; 100]).is_err());
    assert!(CdImage::from_iso(Vec::new()).is_err());
}

#[test]
fn cue_parses_data_plus_audio_tracks() {
    // Track 1: MODE1/2048 data, 2 sectors starting at frame 0.
    // Track 2: AUDIO, starting at frame 2 (right after the data).
    let cue = "FILE \"disc.bin\" BINARY\n\
                   TRACK 01 MODE1/2048\n\
                   INDEX 01 00:00:00\n\
                   TRACK 02 AUDIO\n\
                   INDEX 01 00:00:02\n";
    // Data: 2 sectors * 2048. Audio: 3 frames * 2352.
    let mut bin = vec![0u8; 2 * DATA_SECTOR + 3 * RAW_SECTOR];
    bin[0] = 0xAA; // data LBA 0 marker
    let audio_off = 2 * DATA_SECTOR;
    bin[audio_off] = 0xBB; // audio frame 0 marker
    let img = CdImage::from_cue(cue, bin).unwrap();
    assert_eq!(img.track_count(), 2);
    let t1 = img.tracks()[0];
    let t2 = img.tracks()[1];
    assert_eq!(t1.mode, TrackMode::Mode1_2048);
    assert_eq!((t1.start_lba, t1.sectors), (0, 2));
    assert_eq!(t2.mode, TrackMode::Audio);
    assert_eq!((t2.start_lba, t2.sectors), (2, 3));
    // Data sector reads back through the data track.
    assert_eq!(img.read_data_sector(0).unwrap()[0], 0xAA);
    // Audio frame reads back through the audio path; data read of audio fails.
    assert_eq!(img.read_audio_frame(2).unwrap()[0], 0xBB);
    assert!(img.read_data_sector(2).is_none());
}

#[test]
fn cue_unwraps_mode1_2352_payload() {
    let cue = "FILE \"d.bin\" BINARY\nTRACK 01 MODE1/2352\nINDEX 01 00:00:00\n";
    let mut bin = vec![0u8; RAW_SECTOR];
    bin[16] = 0x7E; // user data starts at offset 16 in a raw frame
    let img = CdImage::from_cue(cue, bin).unwrap();
    assert_eq!(img.read_data_sector(0).unwrap()[0], 0x7E);
}

#[test]
fn msf_round_trips_through_lba() {
    // LBA 0 is 00:02:00 with the lead-in.
    assert_eq!(lba_to_msf(0), (0, 2, 0));
    assert_eq!(msf_to_lba(0, 2, 0), 0);
    // 75 frames after the lead-in is one second later.
    assert_eq!(lba_to_msf(75), (0, 3, 0));
    assert_eq!(msf_to_lba(0, 3, 0), 75);
}

#[test]
fn cue_rejects_unknown_mode() {
    let cue = "TRACK 01 MODE2/2336\nINDEX 01 00:00:00\n";
    assert!(CdImage::from_cue(cue, vec![0u8; RAW_SECTOR]).is_err());
}
