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
    let cue = "TRACK 01 MODE9/9999\nINDEX 01 00:00:00\n";
    assert!(CdImage::from_cue(cue, vec![0u8; RAW_SECTOR]).is_err());
}

#[test]
fn cue_unwraps_mode2_2352_form1_payload() {
    // CD-XA Form 1: 12 sync + 4 header + 8 subheader, so user data at offset 24.
    let cue = "FILE \"d.bin\" BINARY\nTRACK 01 MODE2/2352\nINDEX 01 00:00:00\n";
    let mut bin = vec![0u8; RAW_SECTOR];
    bin[24] = 0x5A;
    let img = CdImage::from_cue(cue, bin).unwrap();
    assert_eq!(img.tracks()[0].mode, TrackMode::Mode2_2352);
    assert_eq!(img.read_data_sector(0).unwrap()[0], 0x5A);
}

#[test]
fn cue_unwraps_mode2_2336_payload() {
    // No sync/header: the 8-byte subheader leads, so user data at offset 8.
    let cue = "FILE \"d.bin\" BINARY\nTRACK 01 MODE2/2336\nINDEX 01 00:00:00\n";
    let mut bin = vec![0u8; MODE2_SECTOR];
    bin[8] = 0x36;
    let img = CdImage::from_cue(cue, bin).unwrap();
    assert_eq!(img.tracks()[0].mode, TrackMode::Mode2_2336);
    assert_eq!(img.read_data_sector(0).unwrap()[0], 0x36);
}

#[test]
fn cue_reads_mode2_2048_bare_payload() {
    let cue = "FILE \"d.bin\" BINARY\nTRACK 01 MODE2/2048\nINDEX 01 00:00:00\n";
    let mut bin = vec![0u8; DATA_SECTOR];
    bin[0] = 0x20;
    let img = CdImage::from_cue(cue, bin).unwrap();
    assert_eq!(img.read_data_sector(0).unwrap()[0], 0x20);
}

#[test]
fn xa_form2_sectors_are_not_readable_as_data() {
    // Form is per sector, not per track: submode bit 5 at frame offset 18 marks
    // Form 2, whose 2324-byte payload is streaming media, not a logical sector.
    let cue = "FILE \"d.bin\" BINARY\nTRACK 01 MODE2/2352\nINDEX 01 00:00:00\n";
    let mut bin = vec![0u8; 3 * RAW_SECTOR];
    bin[24] = 0x01; // sector 0: Form 1 (submode bit 5 clear)
    bin[RAW_SECTOR + 18] = 0x20; // sector 1: Form 2
    bin[RAW_SECTOR + 24] = 0x02;
    // Sector 2: Form 1 with EOF (0x80) set. Only bit 5 selects Form 2 -- other
    // submode bits (EOF 0x80, real-time 0x40, ...) are routine on real XA
    // discs and must not be mistaken for the Form 2 flag.
    bin[2 * RAW_SECTOR + 18] = 0x80;
    bin[2 * RAW_SECTOR + 24] = 0x03;
    let img = CdImage::from_cue(cue, bin).unwrap();
    assert_eq!(img.read_data_sector(0).unwrap()[0], 0x01);
    assert!(img.read_data_sector(1).is_none());
    assert_eq!(img.read_data_sector(2).unwrap()[0], 0x03);
}

#[test]
fn cue_discards_the_subchannel_tail_on_mode1_2448() {
    let cue = "FILE \"d.bin\" BINARY\nTRACK 01 MODE1/2448\nINDEX 01 00:00:00\n";
    let mut bin = vec![0u8; 2 * SUBCHANNEL_SECTOR];
    bin[16] = 0xA1; // sector 0 payload
    bin[SUBCHANNEL_SECTOR + 16] = 0xA2; // sector 1 payload, one full 2448 stride on
    let img = CdImage::from_cue(cue, bin).unwrap();
    assert_eq!(img.read_data_sector(0).unwrap()[0], 0xA1);
    assert_eq!(img.read_data_sector(1).unwrap()[0], 0xA2);
}

#[test]
fn cue_reads_cdg_audio_as_a_plain_red_book_frame() {
    let cue = "FILE \"d.bin\" BINARY\nTRACK 01 CDG\nINDEX 01 00:00:00\n";
    let mut bin = vec![0u8; 2 * SUBCHANNEL_SECTOR];
    bin[0] = 0xC1;
    bin[SUBCHANNEL_SECTOR] = 0xC2;
    let img = CdImage::from_cue(cue, bin).unwrap();
    assert!(img.tracks()[0].mode.is_audio());
    assert_eq!(img.read_audio_frame(0).unwrap()[0], 0xC1);
    assert_eq!(img.read_audio_frame(1).unwrap()[0], 0xC2);
}

#[test]
fn pregap_advances_the_lba_timeline_without_consuming_bytes() {
    // Track 1: 2 data sectors. Track 2: audio, preceded by a 2-frame PREGAP
    // that exists on the disc timeline but has no bytes in the BIN.
    let cue = "FILE \"disc.bin\" BINARY\n\
               TRACK 01 MODE1/2048\n\
               INDEX 01 00:00:00\n\
               TRACK 02 AUDIO\n\
               PREGAP 00:00:02\n\
               INDEX 01 00:00:02\n";
    let mut bin = vec![0u8; 2 * DATA_SECTOR + 3 * RAW_SECTOR];
    bin[2 * DATA_SECTOR] = 0xBB;
    let img = CdImage::from_cue(cue, bin).unwrap();
    let t1 = img.tracks()[0];
    let t2 = img.tracks()[1];
    assert_eq!((t1.start_lba, t1.sectors), (0, 2));
    // The 2-frame pregap pushes track 2 to LBA 4, but its bytes still start
    // right after track 1's in the BIN.
    assert_eq!(t2.start_lba, 4);
    assert_eq!(t2.sectors, 3);
    assert_eq!(img.read_audio_frame(4).unwrap()[0], 0xBB);
    assert_eq!(img.total_sectors(), 7);
}

#[test]
fn index00_pregap_folds_into_the_preceding_track() {
    // Track 2 declares INDEX 00 one frame before INDEX 01: those bytes ARE in
    // the file, and belong to track 1's span. Pinning the documented policy.
    let cue = "FILE \"disc.bin\" BINARY\n\
               TRACK 01 MODE1/2048\n\
               INDEX 01 00:00:00\n\
               TRACK 02 AUDIO\n\
               INDEX 00 00:00:02\n\
               INDEX 01 00:00:03\n";
    let bin = vec![0u8; 3 * DATA_SECTOR + 2 * RAW_SECTOR];
    let img = CdImage::from_cue(cue, bin).unwrap();
    let t1 = img.tracks()[0];
    let t2 = img.tracks()[1];
    // Track 1 runs to track 2's INDEX 01, absorbing the INDEX 00 pregap frame.
    assert_eq!((t1.start_lba, t1.sectors), (0, 3));
    assert_eq!(t2.start_lba, 3);
}

#[test]
fn leading_pregap_on_the_first_track_still_shifts_its_start_lba() {
    // A single-track sheet whose only track has a 2-frame PREGAP before its
    // own INDEX 01. The loop applies `disc_lba += p.pregap_frames`
    // unconditionally, with no `i == 0` special case, so this should push
    // even the very first track off LBA 0.
    let cue = "FILE \"disc.bin\" BINARY\n\
               TRACK 01 MODE1/2048\n\
               PREGAP 00:00:02\n\
               INDEX 01 00:00:00\n";
    let bin = vec![0u8; 3 * DATA_SECTOR];
    let img = CdImage::from_cue(cue, bin).unwrap();
    let t1 = img.tracks()[0];
    assert_eq!(t1.start_lba, 2);
    assert_eq!(t1.sectors, 3);
    assert_eq!(img.total_sectors(), 5);
}

#[test]
fn cue_binds_each_track_to_its_own_file() {
    // A rip with the data track in one file and two audio tracks in their own
    // files. Byte offsets restart at zero in each file; the LBA timeline does not.
    let cue = "FILE \"data.bin\" BINARY\n\
               TRACK 01 MODE2/2352\n\
               INDEX 01 00:00:00\n\
               FILE \"t2.bin\" BINARY\n\
               TRACK 02 AUDIO\n\
               INDEX 01 00:00:00\n\
               FILE \"t3.bin\" BINARY\n\
               TRACK 03 AUDIO\n\
               INDEX 01 00:00:00\n";
    let mut data = vec![0u8; 2 * RAW_SECTOR];
    data[24] = 0xD1;
    let mut t2 = vec![0u8; 3 * RAW_SECTOR];
    t2[0] = 0xE2;
    let mut t3 = vec![0u8; 4 * RAW_SECTOR];
    t3[0] = 0xE3;
    let files = vec![
        ("data.bin".to_string(), data),
        ("t2.bin".to_string(), t2),
        ("t3.bin".to_string(), t3),
    ];

    let img = CdImage::from_cue_files(cue, files).unwrap();
    assert_eq!(img.track_count(), 3);
    assert_eq!((img.tracks()[0].start_lba, img.tracks()[0].sectors), (0, 2));
    assert_eq!((img.tracks()[1].start_lba, img.tracks()[1].sectors), (2, 3));
    assert_eq!((img.tracks()[2].start_lba, img.tracks()[2].sectors), (5, 4));
    assert_eq!(img.total_sectors(), 9);
    assert_eq!(img.read_data_sector(0).unwrap()[0], 0xD1);
    assert_eq!(img.read_audio_frame(2).unwrap()[0], 0xE2);
    assert_eq!(img.read_audio_frame(5).unwrap()[0], 0xE3);
}

#[test]
fn cue_reports_a_file_it_was_not_given() {
    let cue = "FILE \"there.bin\" BINARY\n\
               TRACK 01 AUDIO\n\
               INDEX 01 00:00:00\n\
               FILE \"missing.bin\" BINARY\n\
               TRACK 02 AUDIO\n\
               INDEX 01 00:00:00\n";
    let files = vec![("there.bin".to_string(), vec![0u8; RAW_SECTOR])];
    let err = CdImage::from_cue_files(cue, files).unwrap_err();
    assert!(
        err.contains("missing.bin"),
        "error should name the file: {err}"
    );
}

#[test]
fn cue_refuses_a_supplied_file_the_sheet_never_named() {
    // Silently ignoring a file the caller supplied is how the BOM bug hid for
    // as long as it did: the loader read two files off disk, the parser listed
    // one because the BOM had fused onto the first FILE keyword, and the extra
    // file went nowhere without a word while a track bound to the wrong bytes.
    // The two sides disagreed about what the sheet said, and only the quieter
    // side got to decide. Nobody has to know what a BOM is to be told the
    // counts do not match -- whatever the next cause of a dropped FILE line
    // turns out to be, this refuses the mount instead of guessing.
    let cue = "FILE \"named.bin\" BINARY\n\
               TRACK 01 AUDIO\n\
               INDEX 01 00:00:00\n";
    let files = vec![
        ("named.bin".to_string(), vec![0u8; RAW_SECTOR]),
        ("surplus.bin".to_string(), vec![0u8; RAW_SECTOR]),
    ];

    let err = CdImage::from_cue_files(cue, files).unwrap_err();

    assert!(
        err.contains("surplus.bin"),
        "error should name the file the sheet never asked for: {err}"
    );

    // A sheet whose files are supplied exactly still mounts: the check has to
    // be a count comparison, not a refusal to mount anything at all.
    assert!(
        CdImage::from_cue_files(cue, vec![("named.bin".to_string(), vec![0u8; RAW_SECTOR])])
            .is_ok()
    );
}

#[test]
fn cue_reports_the_missing_file_by_name_when_a_supplied_one_is_also_surplus() {
    // Counts alone cannot tell these two apart: one file named, one file
    // supplied, and they are different files. The name lookup has to run
    // first, so the caller is told which file the sheet wanted rather than
    // being handed an arithmetic identity that happens to hold.
    let cue = "FILE \"wanted.bin\" BINARY\n\
               TRACK 01 AUDIO\n\
               INDEX 01 00:00:00\n";
    let files = vec![("other.bin".to_string(), vec![0u8; RAW_SECTOR])];

    let err = CdImage::from_cue_files(cue, files).unwrap_err();

    assert!(
        err.contains("wanted.bin"),
        "error should name the file the sheet wanted: {err}"
    );
}

#[test]
fn cue_rejects_a_file_name_repeated_across_two_file_sections() {
    // Two separate FILE sections naming the same file is not the "two tracks,
    // one file" layout (that's a single FILE section with multiple TRACK
    // blocks, covered by `cue_shares_one_file_across_two_tracks_then_a_third_in_another`
    // below). `build` cannot honor a repeated section: each section gets its
    // own file_index and its own cursor starting at 0, so the second
    // section's track would silently read back the first section's bytes
    // instead of its own INDEX 01 offset. This must be rejected, not mounted.
    let cue = "FILE \"shared.bin\" BINARY\n\
               TRACK 01 AUDIO\n\
               INDEX 01 00:00:00\n\
               FILE \"shared.bin\" BINARY\n\
               TRACK 02 AUDIO\n\
               INDEX 01 00:00:00\n";
    let files = vec![("shared.bin".to_string(), vec![0u8; 4 * RAW_SECTOR])];

    let err = CdImage::from_cue_files(cue, files).unwrap_err();

    assert!(
        err.contains("shared.bin"),
        "error should name the repeated file: {err}"
    );
}

#[test]
fn cue_shares_one_file_across_two_tracks_then_a_third_in_another() {
    // FILE A holds two tracks back-to-back; FILE B holds a third track alone.
    // Track 1's span is bounded by track 2's INDEX 01 *within FILE A* (the
    // `n.file_index == fi` comparison the per-file rework added actually
    // evaluates true here); track 2 then runs to FILE A's own end, and
    // track 3 runs to FILE B's end -- three different file-boundary cases
    // in one sheet.
    let cue = "FILE \"a.bin\" BINARY\n\
               TRACK 01 MODE1/2048\n\
               INDEX 01 00:00:00\n\
               TRACK 02 AUDIO\n\
               INDEX 01 00:00:02\n\
               FILE \"b.bin\" BINARY\n\
               TRACK 03 AUDIO\n\
               INDEX 01 00:00:00\n";
    // Track 1: 2 sectors of MODE1/2048 (4096 bytes). Track 2: 3 sectors of
    // AUDIO (7056 bytes), filling the rest of FILE A exactly.
    let mut a = vec![0u8; 2 * DATA_SECTOR + 3 * RAW_SECTOR];
    a[0] = 0xA1; // track 1 marker
    a[2 * DATA_SECTOR] = 0xA2; // track 2 marker, right after track 1's bytes
    let mut b = vec![0u8; 2 * RAW_SECTOR];
    b[0] = 0xB3; // track 3 marker
    let files = vec![("a.bin".to_string(), a), ("b.bin".to_string(), b)];

    let img = CdImage::from_cue_files(cue, files).unwrap();
    assert_eq!(img.track_count(), 3);
    assert_eq!((img.tracks()[0].start_lba, img.tracks()[0].sectors), (0, 2));
    assert_eq!((img.tracks()[1].start_lba, img.tracks()[1].sectors), (2, 3));
    assert_eq!((img.tracks()[2].start_lba, img.tracks()[2].sectors), (5, 2));
    assert_eq!(img.total_sectors(), 7);
    assert_eq!(img.read_data_sector(0).unwrap()[0], 0xA1);
    assert_eq!(img.read_audio_frame(2).unwrap()[0], 0xA2);
    assert_eq!(img.read_audio_frame(5).unwrap()[0], 0xB3);
}

#[test]
fn cue_with_a_utf8_bom_still_binds_each_track_to_its_own_file() {
    // Editors and rippers on Windows routinely save a sheet with a UTF-8 BOM,
    // and `str::trim` does not remove it: U+FEFF is a `Cf` format character,
    // not `White_Space`. So the first line's keyword arrives as
    // "\u{FEFF}FILE", misses every arm of the keyword match, and falls through
    // the catch-all -- the whole line is dropped.
    //
    // Dropping the *first* FILE line is not a cosmetic loss. Tracks bind to a
    // file by `files.len().saturating_sub(1)`, so with the first FILE gone
    // every track before the second one still binds to index 0 and index 0 is
    // now the wrong file. The sheet mounts, no error is raised (the missing
    // name is never looked up, so `from_cue_files` has nothing to complain
    // about), and track 1 serves track 2's bytes. That is the silent
    // wrong-data mount this whole class of fix exists to remove.
    //
    // Asserting the file *list* alone would not catch it: the damage lives in
    // the index binding, so the markers below have to prove which file each
    // track actually reads from.
    let sheet = "FILE \"t1.bin\" BINARY\n\
                 TRACK 01 AUDIO\n\
                 INDEX 01 00:00:00\n\
                 FILE \"t2.bin\" BINARY\n\
                 TRACK 02 AUDIO\n\
                 INDEX 01 00:00:00\n";
    let with_bom = format!("\u{feff}{sheet}");

    let (clean_files, clean_tracks) = parse_cue(sheet).unwrap();
    let (bom_files, bom_tracks) = parse_cue(&with_bom).unwrap();
    assert_eq!(bom_files, clean_files);
    assert_eq!(
        bom_tracks
            .iter()
            .map(|t| (t.number, t.file_index))
            .collect::<Vec<_>>(),
        clean_tracks
            .iter()
            .map(|t| (t.number, t.file_index))
            .collect::<Vec<_>>()
    );

    let mut t1 = vec![0u8; 2 * RAW_SECTOR];
    t1[0] = 0xE1;
    let mut t2 = vec![0u8; 3 * RAW_SECTOR];
    t2[0] = 0xE2;
    let files = vec![("t1.bin".to_string(), t1), ("t2.bin".to_string(), t2)];

    let img = CdImage::from_cue_files(&with_bom, files).unwrap();

    assert_eq!(img.track_count(), 2);
    assert_eq!((img.tracks()[0].start_lba, img.tracks()[0].sectors), (0, 2));
    assert_eq!((img.tracks()[1].start_lba, img.tracks()[1].sectors), (2, 3));
    assert_eq!(img.tracks()[0].image_offset, 0);
    assert_eq!(img.tracks()[1].image_offset, 2 * RAW_SECTOR);
    assert_eq!(img.read_audio_frame(0).unwrap()[0], 0xE1);
    assert_eq!(img.read_audio_frame(2).unwrap()[0], 0xE2);
}

#[test]
fn cue_keeps_the_file_type_token() {
    let cue = "FILE \"disc.bin\" BINARY\n\
               TRACK 01 MODE1/2048\n\
               INDEX 01 00:00:00\n\
               FILE \"track02.ogg\" WAVE\n\
               TRACK 02 AUDIO\n\
               INDEX 01 00:00:00\n";
    let (files, _tracks) = parse_cue(cue).unwrap();
    assert_eq!(files[0].0, "disc.bin");
    assert_eq!(files[0].1, CueFileType::Binary);
    assert_eq!(files[1].0, "track02.ogg");
    // WAVE is preserved rather than mapped to a format: the token is advisory
    // and sniffing decides what the file really is.
    assert_eq!(files[1].1, CueFileType::Other("WAVE".to_string()));
}

#[test]
fn cue_file_type_defaults_to_binary_when_absent() {
    // A sheet with no type token at all is legal and means BINARY.
    let cue = "FILE \"disc.bin\"\nTRACK 01 MODE1/2048\nINDEX 01 00:00:00\n";
    let (files, _tracks) = parse_cue(cue).unwrap();
    assert_eq!(files[0].1, CueFileType::Binary);
}

#[test]
fn cue_recognizes_motorola_as_its_own_type() {
    // MOTOROLA is the one token that carries information sniffing cannot: the
    // file is raw frames, but big-endian.
    let cue = "FILE \"d.bin\" MOTOROLA\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n";
    let (files, _tracks) = parse_cue(cue).unwrap();
    assert_eq!(files[0].1, CueFileType::Motorola);
}

#[test]
fn motorola_audio_track_swaps_each_sample_s_bytes() {
    // MOTOROLA means big-endian 16-bit samples. The swap is within each sample,
    // not between the left and right channels: bytes 0,1 are one sample and
    // come back reversed, while the sample at bytes 2,3 stays the sample at
    // bytes 2,3.
    let cue = "FILE \"d.bin\" MOTOROLA\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n";
    let mut bin = vec![0u8; RAW_SECTOR];
    bin[0] = 0x12;
    bin[1] = 0x34;
    bin[2] = 0x56;
    bin[3] = 0x78;
    let img = CdImage::from_cue_files(cue, vec![("d.bin".to_string(), bin)]).unwrap();
    let frame = img.read_audio_frame(0).unwrap();
    assert_eq!(&frame[0..4], &[0x34, 0x12, 0x78, 0x56]);
}

#[test]
fn binary_audio_track_is_not_swapped() {
    let cue = "FILE \"d.bin\" BINARY\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n";
    let mut bin = vec![0u8; RAW_SECTOR];
    bin[0] = 0x12;
    bin[1] = 0x34;
    let img = CdImage::from_cue_files(cue, vec![("d.bin".to_string(), bin)]).unwrap();
    assert_eq!(&img.read_audio_frame(0).unwrap()[0..2], &[0x12, 0x34]);
}

#[test]
fn from_cue_takes_its_byte_order_from_the_first_file_line() {
    // `from_cue` flattens every track onto the one blob its caller handed in,
    // whatever the sheet's FILE line names, and that flattening is why it used
    // to hardcode BINARY: a sheet with several FILE lines gave no way to say
    // which one described the bytes (such a sheet is now refused outright, see
    // `from_cue_refuses_a_sheet_that_names_several_files`). The single-BIN
    // sheet is the case this entry point exists for, and there the FILE line
    // describes exactly those bytes, so imposing BINARY on it contradicted the
    // sheet in the one respect no amount of sniffing can recover. No user
    // could reach that: the loader calls this only for a sheet naming no file,
    // and a single-BIN MOTOROLA sheet goes through `from_cue_files`, which has
    // read the token all along. It is the API surface that is pinned here.
    let mut bin = vec![0u8; RAW_SECTOR];
    bin[0] = 0x12;
    bin[1] = 0x34;
    bin[2] = 0x56;
    bin[3] = 0x78;

    let motorola = CdImage::from_cue(
        "FILE \"d.bin\" MOTOROLA\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n",
        bin.clone(),
    )
    .unwrap();
    assert!(motorola.tracks()[0].byte_swapped);
    assert_eq!(
        &motorola.read_audio_frame(0).unwrap()[0..4],
        &[0x34, 0x12, 0x78, 0x56]
    );

    // BINARY is still BINARY: reading the token must not turn into swapping
    // whenever a token is present at all.
    let binary = CdImage::from_cue(
        "FILE \"d.bin\" BINARY\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n",
        bin.clone(),
    )
    .unwrap();
    assert!(!binary.tracks()[0].byte_swapped);
    assert_eq!(
        &binary.read_audio_frame(0).unwrap()[0..4],
        &[0x12, 0x34, 0x56, 0x78]
    );

    // A sheet with no FILE line at all is legal here and reaches `build` with
    // an empty file list, so the fallback has to stay BINARY rather than index
    // into nothing. The loader relies on this path when a sheet names no files
    // and it substitutes the CUE's sibling .bin.
    let no_file = CdImage::from_cue("TRACK 01 AUDIO\nINDEX 01 00:00:00\n", bin).unwrap();
    assert!(!no_file.tracks()[0].byte_swapped);
    assert_eq!(
        &no_file.read_audio_frame(0).unwrap()[0..4],
        &[0x12, 0x34, 0x56, 0x78]
    );
}

#[test]
fn from_cue_refuses_a_sheet_that_names_several_files() {
    // Flattening every track onto one blob is a defensible reading of a sheet
    // that names one file, and no reading at all of a sheet that names two.
    // The tracks after the first belong to *other* files, with their own byte
    // origins, and this entry point lays every track out sequentially against
    // the single blob it was handed: track 2 would be read at track 1's end
    // rather than at the head of its own file. That is not an approximation of
    // the disc, it is a different disc -- served without an error, which is the
    // failure mode this whole line of work exists to remove.
    let cue = "FILE \"t1.bin\" BINARY\n\
               TRACK 01 AUDIO\n\
               INDEX 01 00:00:00\n\
               FILE \"t2.bin\" BINARY\n\
               TRACK 02 AUDIO\n\
               INDEX 01 00:00:00\n";

    let err = CdImage::from_cue(cue, vec![0u8; 5 * RAW_SECTOR]).unwrap_err();

    assert!(
        err.contains("from_cue_files"),
        "the error should point at the entry point that can mount this: {err}"
    );

    // The one-file and no-file sheets this entry point does serve are
    // untouched, so the guard cannot be satisfied by refusing everything.
    assert!(
        CdImage::from_cue(
            "FILE \"t1.bin\" BINARY\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n",
            vec![0u8; RAW_SECTOR],
        )
        .is_ok()
    );
    assert!(
        CdImage::from_cue("TRACK 01 AUDIO\nINDEX 01 00:00:00\n", vec![0u8; RAW_SECTOR]).is_ok()
    );
}

#[test]
fn motorola_data_track_payload_is_left_alone() {
    // The endianness a MOTOROLA line declares is a property of Red Book audio
    // samples, which the drive hands to a mixer that has to know which byte is
    // which. A data track has no samples: its payload is a byte stream the
    // guest interprets, and a drive that reordered those bytes would corrupt
    // every file on the disc. So the token has to stop at the audio path even
    // when the same sheet, or the same FILE, declares it.
    let cue = "FILE \"d.bin\" MOTOROLA\nTRACK 01 MODE1/2048\nINDEX 01 00:00:00\n";
    let mut bin = vec![0u8; DATA_SECTOR];
    bin[0] = 0x12;
    bin[1] = 0x34;
    bin[2] = 0x56;
    bin[3] = 0x78;
    let img = CdImage::from_cue_files(cue, vec![("d.bin".to_string(), bin)]).unwrap();
    // This is the assertion that carries the test: it is what fails if the
    // `is_audio()` guard on the swap is dropped.
    assert!(!img.tracks()[0].byte_swapped);
    // The read below is a forward guard, not a second proof. `read_data_sector`
    // never consults `byte_swapped`, so no change to the swap alone can make it
    // fail -- it would take someone teaching the data path to swap as well. It
    // stays because that is exactly the change worth catching, but do not read
    // it as independent coverage of the line above.
    assert_eq!(
        &img.read_data_sector(0).unwrap()[0..4],
        &[0x12, 0x34, 0x56, 0x78]
    );
}

#[test]
fn motorola_swaps_the_audio_track_of_a_file_whose_data_track_it_leaves_alone() {
    // MOTOROLA is declared per FILE, and a mixed-mode rip puts the data track
    // and the audio tracks in the same file. So the guard on the swap has to
    // be the *track's* mode, not the file's type: resolving it per file would
    // read correctly on `motorola_data_track_payload_is_left_alone` (one data
    // track, nothing to swap) and on
    // `motorola_audio_track_swaps_each_sample_s_bytes` (one audio track, swap
    // everything) and still corrupt every file on the disc here. Only a sheet
    // that puts both kinds of track behind one MOTOROLA line can tell the two
    // rules apart.
    let cue = "FILE \"d.bin\" MOTOROLA\n\
               TRACK 01 MODE1/2048\n\
               INDEX 01 00:00:00\n\
               TRACK 02 AUDIO\n\
               INDEX 01 00:00:02\n";
    let mut bin = vec![0u8; 2 * DATA_SECTOR + RAW_SECTOR];
    bin[0..4].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
    bin[2 * DATA_SECTOR..2 * DATA_SECTOR + 4].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);

    let img = CdImage::from_cue_files(cue, vec![("d.bin".to_string(), bin)]).unwrap();

    assert_eq!(img.track_count(), 2);
    assert!(!img.tracks()[0].byte_swapped);
    assert!(img.tracks()[1].byte_swapped);
    // Identical bytes in the same file, read back two different ways.
    assert_eq!(
        &img.read_data_sector(0).unwrap()[0..4],
        &[0x12, 0x34, 0x56, 0x78]
    );
    assert_eq!(
        &img.read_audio_frame(2).unwrap()[0..4],
        &[0x34, 0x12, 0x78, 0x56]
    );
}

/// Build a one-FILE sheet around `file_line` and return the parsed FILE entry.
/// Every case below needs a TRACK/INDEX pair after the FILE line for the sheet
/// to be well-formed, and none of them care what that pair says.
///
/// An empty file list is reported as an error rather than indexed into: a line
/// `parse_cue` does not recognize as a FILE at all falls through its catch-all
/// arm and parses fine, just with nothing in the list, and that case should
/// reach the caller as a failed assertion naming the input rather than as a
/// panic from the harness.
fn parse_one_file_line(file_line: &str) -> Result<CueFile, String> {
    let cue = format!("{file_line}\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n");
    let (files, _tracks) = parse_cue(&cue)?;
    files
        .into_iter()
        .next()
        .ok_or_else(|| "no FILE parsed".to_string())
}

#[test]
fn cue_file_line_parsing_handles_quoting_and_whitespace() {
    // The name and the type token share one line, so reading the token means
    // resuming the scan at whatever follows the name. When the name is quoted,
    // that is the text after the *closing* quote -- a position the earlier
    // `rest.split('"').next()` could never report, because splitting on the
    // quote and taking the first piece yields the name and discards the rest of
    // the line by construction. It parsed names correctly and would keep doing
    // so, which is exactly why nothing here can be inferred from the name
    // assertions alone: each case below pins that the token was found too.

    // A quoted name is quoted because it may contain spaces, which is what
    // separates the two readings: the token is the first word past the closing
    // quote, not the second word of the line.
    let spaced = parse_one_file_line("FILE \"my disc 01.bin\" MOTOROLA").unwrap();
    assert_eq!(
        spaced,
        ("my disc 01.bin".to_string(), CueFileType::Motorola)
    );

    // Unquoted, the name ends at the first space and the token is what remains.
    let bare = parse_one_file_line("FILE d.bin BINARY").unwrap();
    assert_eq!(bare, ("d.bin".to_string(), CueFileType::Binary));

    // An unquoted name with nothing after it must not be mistaken for a name
    // with an empty token: no token at all means BINARY.
    let no_token = parse_one_file_line("FILE d.bin").unwrap();
    assert_eq!(no_token, ("d.bin".to_string(), CueFileType::Binary));

    // Sheets are written by rippers, not by a formatter: case and run-length of
    // the whitespace around the token are both free.
    let untidy = parse_one_file_line("FILE \"d.bin\"   wave  ").unwrap();
    assert_eq!(
        untidy,
        ("d.bin".to_string(), CueFileType::Other("WAVE".to_string()))
    );
}

#[test]
fn cue_file_line_without_a_name_is_rejected() {
    // Reaching past the closing quote gave the quoted branch a second way to
    // come back empty-handed, so pin the message and not merely `is_err`: a
    // FILE line that yields no name has to fail *as* a missing name, whichever
    // branch consumed it. An unterminated quote lands here too -- there is no
    // closing quote to end the name at, so the line names nothing.
    for line in ["FILE \"\" BINARY", "FILE", "FILE \"unterminated"] {
        let err = parse_one_file_line(line).unwrap_err();
        assert!(
            err.contains("missing FILE name"),
            "'{line}' should fail as a missing name: {err}"
        );
    }
}

/// A stand-in decoder: reports a fixed length and serves frames stamped with
/// their index, with everything from `ready` on withheld the way an unfinished
/// decode withholds its tail. It also records having been asked for anything,
/// which is how a real source learns it should start decoding.
#[derive(Debug)]
struct FakeSource {
    sectors: u32,
    ready: u32,
    touched: std::sync::atomic::AtomicBool,
}

impl FakeSource {
    fn was_touched(&self) -> bool {
        self.touched.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl izarravm_core::AudioTrackSource for FakeSource {
    fn sectors(&self) -> u32 {
        self.sectors
    }

    fn frame(&self, index: u32) -> Option<[u8; RAW_SECTOR]> {
        self.touched
            .store(true, std::sync::atomic::Ordering::SeqCst);
        (index < self.ready).then(|| {
            let mut frame = [0u8; RAW_SECTOR];
            frame[0] = 0xF0 | (index as u8 & 0x0F);
            frame
        })
    }
}

/// Build a fake source and return both the `CueSource` and a handle on the
/// fake, so a test can ask whether it was touched.
fn fake_pair(sectors: u32, ready: u32) -> (CueSource, std::sync::Arc<FakeSource>) {
    let source = std::sync::Arc::new(FakeSource {
        sectors,
        ready,
        touched: std::sync::atomic::AtomicBool::new(false),
    });
    (CueSource::Audio(source.clone()), source)
}

fn fake(sectors: u32, ready: u32) -> CueSource {
    fake_pair(sectors, ready).0
}

#[test]
fn audio_source_track_takes_its_length_from_the_source() {
    // The whole point: the ogg's byte length says nothing about its duration,
    // so the sector count comes from the source and every following track's LBA
    // follows from that.
    let cue = "FILE \"disc.bin\" BINARY\n\
               TRACK 01 MODE1/2048\n\
               INDEX 01 00:00:00\n\
               FILE \"track02.ogg\" WAVE\n\
               TRACK 02 AUDIO\n\
               INDEX 01 00:00:00\n";
    let img = CdImage::from_cue_sources(
        cue,
        vec![
            (
                "disc.bin".to_string(),
                CueSource::Raw(vec![0u8; 2 * DATA_SECTOR]),
            ),
            ("track02.ogg".to_string(), fake(9000, 9000)),
        ],
    )
    .unwrap();
    assert_eq!(img.track_count(), 2);
    assert_eq!((img.tracks()[0].start_lba, img.tracks()[0].sectors), (0, 2));
    assert_eq!(
        (img.tracks()[1].start_lba, img.tracks()[1].sectors),
        (2, 9000)
    );
    assert_eq!(img.total_sectors(), 9002);
}

#[test]
fn audio_source_frames_come_from_the_source() {
    let cue = "FILE \"t.ogg\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n";
    let img = CdImage::from_cue_sources(cue, vec![("t.ogg".to_string(), fake(4, 4))]).unwrap();
    assert_eq!(img.read_audio_frame(0).unwrap()[0], 0xF0);
    assert_eq!(img.read_audio_frame(3).unwrap()[0], 0xF3);
    // Past the track, nothing.
    assert!(img.read_audio_frame(4).is_none());
    // A data read of an audio track fails, source-backed or not.
    assert!(img.read_data_sector(0).is_none());
}

#[test]
fn frames_the_source_has_not_decoded_yet_read_as_absent() {
    // The mixer renders these as silence and steps past them.
    let cue = "FILE \"t.ogg\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n";
    let img = CdImage::from_cue_sources(cue, vec![("t.ogg".to_string(), fake(8, 2))]).unwrap();
    assert!(img.read_audio_frame(0).is_some());
    assert!(img.read_audio_frame(2).is_none());
    // The TOC still covers the whole track even though most of it is undecoded.
    assert_eq!(img.tracks()[0].sectors, 8);
}

#[test]
fn an_audio_sourced_track_is_addressed_from_its_own_start_not_the_discs() {
    // The index handed to the source is relative to the track, so a source
    // preceded by other tracks -- and by a PREGAP, which advances the disc
    // timeline with no bytes behind it -- still starts at its own frame 0.
    let cue = "FILE \"a.bin\" BINARY\n\
               TRACK 01 MODE1/2048\n\
               INDEX 01 00:00:00\n\
               FILE \"b.ogg\" WAVE\n\
               TRACK 02 AUDIO\n\
               PREGAP 00:00:02\n\
               INDEX 01 00:00:00\n";
    let img = CdImage::from_cue_sources(
        cue,
        vec![
            ("a.bin".to_string(), CueSource::Raw(vec![0u8; DATA_SECTOR])),
            ("b.ogg".to_string(), fake(4, 4)),
        ],
    )
    .unwrap();
    // One data sector, then two pregap frames: the audio starts at LBA 3.
    assert_eq!(img.tracks()[1].start_lba, 3);
    assert_eq!(img.read_audio_frame(3).unwrap()[0], 0xF0);
    assert_eq!(img.read_audio_frame(4).unwrap()[0], 0xF1);
}

#[test]
fn a_raw_file_after_an_audio_source_still_addresses_its_own_bytes() {
    // An audio-sourced file contributes no bytes to the backing at all, and a
    // raw file after one has to be unaffected by that: same base offset, same
    // bytes, and a track table that still lines up with the disc timeline the
    // source lengthened. Reserving space for the ogg would push c.bin's base
    // forward, and deriving the ogg's length from its (zero) bytes would
    // collapse track 3 onto track 2's LBA.
    let cue = "FILE \"a.bin\" BINARY\n\
               TRACK 01 MODE1/2048\n\
               INDEX 01 00:00:00\n\
               FILE \"b.ogg\" WAVE\n\
               TRACK 02 AUDIO\n\
               INDEX 01 00:00:00\n\
               FILE \"c.bin\" BINARY\n\
               TRACK 03 AUDIO\n\
               INDEX 01 00:00:00\n";
    let mut a = vec![0u8; DATA_SECTOR];
    a[0] = 0xA1;
    let mut c = vec![0u8; RAW_SECTOR];
    c[0] = 0xC1;
    let img = CdImage::from_cue_sources(
        cue,
        vec![
            ("a.bin".to_string(), CueSource::Raw(a)),
            ("b.ogg".to_string(), fake(100, 100)),
            ("c.bin".to_string(), CueSource::Raw(c)),
        ],
    )
    .unwrap();
    assert_eq!(img.read_data_sector(0).unwrap()[0], 0xA1);
    assert_eq!(img.read_audio_frame(1).unwrap()[0], 0xF0);
    // Track 3 starts at LBA 101 and its bytes are its own.
    assert_eq!(img.tracks()[2].start_lba, 101);
    assert_eq!(img.read_audio_frame(101).unwrap()[0], 0xC1);
    // c.bin's bytes begin directly after a.bin's, with nothing reserved in
    // between for the 100 sectors the ogg contributes to the timeline.
    assert_eq!(img.tracks()[2].image_offset, DATA_SECTOR);
}

#[test]
fn from_cue_files_still_works_through_the_new_entry_point() {
    // The wrapper must not change behavior for a sheet with no compressed audio.
    let cue = "FILE \"d.bin\" BINARY\nTRACK 01 MODE1/2048\nINDEX 01 00:00:00\n";
    let mut bin = vec![0u8; DATA_SECTOR];
    bin[0] = 0xDD;
    let img = CdImage::from_cue_files(cue, vec![("d.bin".to_string(), bin)]).unwrap();
    assert_eq!(img.read_data_sector(0).unwrap()[0], 0xDD);
}

#[test]
fn a_decoded_file_cannot_back_a_data_track() {
    // MODE1 data in an ogg is not a thing. Refuse it by name rather than
    // mounting a data track that reads back silence.
    let cue = "FILE \"t.ogg\" WAVE\nTRACK 01 MODE1/2048\nINDEX 01 00:00:00\n";
    let err =
        CdImage::from_cue_sources(cue, vec![("t.ogg".to_string(), fake(10, 10))]).unwrap_err();
    assert!(err.contains("t.ogg"), "message was: {err}");
    assert!(err.contains("data track"), "message was: {err}");
}

#[test]
fn a_decoded_file_cannot_be_split_across_two_tracks() {
    // One compressed file is one song: the sector count comes from the whole
    // file's duration, so there is no way to say where a second track would
    // start inside it.
    let cue = "FILE \"t.ogg\" WAVE\n\
               TRACK 01 AUDIO\n\
               INDEX 01 00:00:00\n\
               TRACK 02 AUDIO\n\
               INDEX 01 00:00:10\n";
    let err =
        CdImage::from_cue_sources(cue, vec![("t.ogg".to_string(), fake(20, 20))]).unwrap_err();
    assert!(err.contains("t.ogg"), "message was: {err}");
    assert!(err.contains("more than one"), "message was: {err}");
}

#[test]
fn many_tracks_may_still_share_one_raw_file() {
    // The rejection above is about encoded files alone. Tomb Raider Gold's
    // sheet puts 60 tracks on one BIN and Quake's puts 11 on one; refusing
    // those would break discs that work today, so the guard must be reachable
    // only through CueSource::Audio.
    let cue = "FILE \"d.bin\" BINARY\n\
               TRACK 01 AUDIO\n\
               INDEX 01 00:00:00\n\
               TRACK 02 AUDIO\n\
               INDEX 01 00:00:02\n\
               TRACK 03 AUDIO\n\
               INDEX 01 00:00:04\n";
    let img = CdImage::from_cue_sources(
        cue,
        vec![(
            "d.bin".to_string(),
            CueSource::Raw(vec![0u8; 6 * RAW_SECTOR]),
        )],
    )
    .unwrap();
    assert_eq!(img.track_count(), 3);
    assert_eq!(img.total_sectors(), 6);
}

#[test]
fn a_repeated_file_section_is_still_rejected_through_the_new_entry_point() {
    // This guard used to live in from_cue_files. It has to be on the path the
    // loader actually calls, or a sheet that repeats a FILE section mounts
    // wrong data.
    let cue = "FILE \"a.bin\" BINARY\n\
               TRACK 01 MODE1/2048\n\
               INDEX 01 00:00:00\n\
               FILE \"a.bin\" BINARY\n\
               TRACK 02 AUDIO\n\
               INDEX 01 00:00:00\n";
    let err = CdImage::from_cue_sources(
        cue,
        vec![("a.bin".to_string(), CueSource::Raw(vec![0u8; DATA_SECTOR]))],
    )
    .unwrap_err();
    assert!(
        err.contains("more than one FILE section"),
        "message was: {err}"
    );
}

/// Two audio tracks of `sectors` frames each, back to back, with a handle on
/// the second one's source.
fn two_track_disc(sectors: u32) -> (CdImage, std::sync::Arc<FakeSource>) {
    let cue = "FILE \"a.ogg\" WAVE\n\
               TRACK 01 AUDIO\n\
               INDEX 01 00:00:00\n\
               FILE \"b.ogg\" WAVE\n\
               TRACK 02 AUDIO\n\
               INDEX 01 00:00:00\n";
    let (first, _first_handle) = fake_pair(sectors, sectors);
    let (second, second_handle) = fake_pair(sectors, sectors);
    let img = CdImage::from_cue_sources(
        cue,
        vec![("a.ogg".to_string(), first), ("b.ogg".to_string(), second)],
    )
    .unwrap();
    (img, second_handle)
}

#[test]
fn approaching_a_track_boundary_warms_the_next_track() {
    // The play head advances in real time whether or not the decoder has caught
    // up, so a track whose decode starts when the head arrives loses its
    // opening. Starting it a couple of seconds early costs nothing and removes
    // the clip for sequential play, which is what a game and the front panel
    // both do.
    let (img, second) = two_track_disc(1000);

    // Far from the boundary, nothing ahead is touched.
    img.warm_upcoming(500);
    assert!(!second.was_touched());

    // Within the prefetch window of track 2's start at LBA 1000.
    img.warm_upcoming(1000 - PREFETCH_FRAMES);
    assert!(second.was_touched());
}

#[test]
fn warming_never_reaches_past_the_next_track() {
    // The window is measured from the end of the track the head is in, so a
    // track shorter than the window must not warm the one after the next: a
    // disc of two-second tracks would otherwise warm the whole disc at once,
    // which is exactly the residency the bound exists to prevent.
    let cue = "FILE \"a.ogg\" WAVE\n\
               TRACK 01 AUDIO\n\
               INDEX 01 00:00:00\n\
               FILE \"b.ogg\" WAVE\n\
               TRACK 02 AUDIO\n\
               INDEX 01 00:00:00\n\
               FILE \"c.ogg\" WAVE\n\
               TRACK 03 AUDIO\n\
               INDEX 01 00:00:00\n";
    let (a, _) = fake_pair(10, 10);
    let (b, b_handle) = fake_pair(10, 10);
    let (c, c_handle) = fake_pair(10, 10);
    let img = CdImage::from_cue_sources(
        cue,
        vec![
            ("a.ogg".to_string(), a),
            ("b.ogg".to_string(), b),
            ("c.ogg".to_string(), c),
        ],
    )
    .unwrap();

    img.warm_upcoming(0);

    assert!(b_handle.was_touched(), "the next track was not warmed");
    assert!(!c_handle.was_touched(), "a track two ahead was warmed");
}

#[test]
fn warming_at_the_end_of_the_disc_does_not_restart_the_last_track() {
    let (img, second) = two_track_disc(1000);
    for lba in [1999, 5000] {
        img.warm_upcoming(lba);
        assert!(!second.was_touched(), "last track warmed at LBA {lba}");
    }
}

#[test]
fn warming_leaves_a_raw_track_alone() {
    // A raw track has nothing to start and no source to touch. Reaching into
    // `audio_sources` for one would be a lookup that always misses, but the
    // point is that the byte-backed path is untouched by prefetch.
    let cue = "FILE \"d.bin\" BINARY\n\
               TRACK 01 AUDIO\n\
               INDEX 01 00:00:00\n\
               TRACK 02 AUDIO\n\
               INDEX 01 00:00:02\n";
    let img = CdImage::from_cue_sources(
        cue,
        vec![(
            "d.bin".to_string(),
            CueSource::Raw(vec![0u8; 4 * RAW_SECTOR]),
        )],
    )
    .unwrap();
    img.warm_upcoming(0);
    img.warm_upcoming(1);
}

#[test]
fn a_pregap_between_tracks_does_not_stop_the_next_one_being_warmed() {
    // A PREGAP belongs to no track: `build` counts its frames into the
    // following track's start_lba without giving them to either neighbour. So
    // the next track does not begin where the previous one ended, and while
    // the head crosses the gap it is inside no track at all.
    //
    // Matching the next track on exact adjacency made warming inert for every
    // sheet with a pregap, which is the ordinary layout -- Fatal Racing's real
    // sheet opens its audio with PREGAP 00:02:00.
    let cue = "FILE \"a.ogg\" WAVE\n\
               TRACK 01 AUDIO\n\
               INDEX 01 00:00:00\n\
               FILE \"b.ogg\" WAVE\n\
               TRACK 02 AUDIO\n\
               PREGAP 00:02:00\n\
               INDEX 01 00:00:00\n";
    let (first, _first) = fake_pair(1000, 1000);
    let (second, second_handle) = fake_pair(1000, 1000);
    let img = CdImage::from_cue_sources(
        cue,
        vec![("a.ogg".to_string(), first), ("b.ogg".to_string(), second)],
    )
    .unwrap();
    // Track 1 ends at 1000; the 150-frame pregap puts track 2 at 1150.
    assert_eq!(img.tracks()[0].end_lba(), 1000);
    assert_eq!(img.tracks()[1].start_lba, 1150);

    // Still outside the window, measured to the next track's start.
    img.warm_upcoming(900);
    assert!(!second_handle.was_touched());

    // Inside the window, and inside the pregap, where no track covers the head.
    img.warm_upcoming(1010);
    assert!(
        second_handle.was_touched(),
        "the track after a pregap was never warmed"
    );
}
