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
    assert!(!img.tracks()[0].byte_swapped);
    assert_eq!(
        &img.read_data_sector(0).unwrap()[0..4],
        &[0x12, 0x34, 0x56, 0x78]
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
