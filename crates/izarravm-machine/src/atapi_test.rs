// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::cdimage::{CdImage, DATA_SECTOR, RAW_SECTOR};

fn data_disc(sectors: u32) -> CdImage {
    let mut bytes = vec![0u8; sectors as usize * DATA_SECTOR];
    for s in 0..sectors as usize {
        bytes[s * DATA_SECTOR] = (s as u8).wrapping_add(0x40);
    }
    CdImage::from_iso(bytes).unwrap()
}

fn cdb(op: u8) -> [u8; 12] {
    let mut c = [0u8; 12];
    c[0] = op;
    c
}

fn data(result: CmdResult) -> Vec<u8> {
    match result {
        CmdResult::Data(d) => d,
        CmdResult::Error => panic!("expected data, got error"),
    }
}

#[test]
fn read10_returns_the_right_sector() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(8));
    let mut c = cdb(0x28);
    c[5] = 3; // LBA 3
    c[8] = 1; // 1 sector
    let buf = data(dev.execute(&c));
    assert_eq!(buf.len(), DATA_SECTOR);
    assert_eq!(buf[0], 0x43); // 0x40 + 3
}

#[test]
fn read10_past_end_is_an_error() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(4));
    let mut c = cdb(0x28);
    c[5] = 4; // LBA 4, past the 4-sector disc
    c[8] = 1;
    assert!(matches!(dev.execute(&c), CmdResult::Error));
    assert_eq!(dev.sense_key, sense_key::ILLEGAL_REQUEST);
}

#[test]
fn read_capacity_reports_last_lba() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(10));
    let buf = data(dev.execute(&cdb(0x25)));
    let last = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let block = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    assert_eq!(last, 9); // last LBA of a 10-sector disc
    assert_eq!(block, DATA_SECTOR as u32);
}

#[test]
fn read_toc_lists_tracks_and_leadout() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(6));
    let mut c = cdb(0x43);
    c[8] = 200; // allocation length, plenty
    let buf = data(dev.execute(&c));
    // header: data length (2), first track, last track.
    assert_eq!(buf[2], 1); // first track
    assert_eq!(buf[3], 1); // last track (one data track)
    // First descriptor starts at byte 4; the lead-out (0xAA) follows.
    assert_eq!(buf[4 + 2], 1); // track number of first descriptor
    assert_eq!(buf[4 + 8 + 2], 0xAA); // lead-out track number
}

#[test]
fn inquiry_reports_cdrom_type() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    let mut c = cdb(0x12);
    c[4] = 36;
    let buf = data(dev.execute(&c));
    assert_eq!(buf[0], 0x05); // CD-ROM peripheral type
    assert_eq!(buf[1] & 0x80, 0x80); // removable
}

#[test]
fn test_unit_ready_reports_no_medium_when_empty() {
    let mut dev = AtapiDevice::new();
    assert!(matches!(dev.execute(&cdb(0x00)), CmdResult::Error));
    assert_eq!(dev.sense_key, sense_key::NOT_READY);
}

#[test]
fn first_ready_after_insert_is_unit_attention_then_clears() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    // First TEST UNIT READY reports the medium-changed unit attention.
    assert!(matches!(dev.execute(&cdb(0x00)), CmdResult::Error));
    assert_eq!(dev.sense_key, sense_key::UNIT_ATTENTION);
    // It clears, so the next is ready.
    assert!(matches!(dev.execute(&cdb(0x00)), CmdResult::Data(_)));
}

#[test]
fn request_sense_returns_then_clears_latched_sense() {
    let mut dev = AtapiDevice::new();
    // No medium -> latch NOT READY.
    let _ = dev.execute(&cdb(0x00));
    let mut c = cdb(0x03);
    c[4] = 18;
    let buf = data(dev.execute(&c));
    assert_eq!(buf[2] & 0x0F, sense_key::NOT_READY);
    assert_eq!(buf[12], asc::NOT_READY_NO_MEDIUM.0);
    // A second REQUEST SENSE now reports NO SENSE.
    let buf2 = data(dev.execute(&c));
    assert_eq!(buf2[2] & 0x0F, sense_key::NO_SENSE);
}

fn audio_disc() -> CdImage {
    // 1 data sector, then 100 audio frames with a nonzero marker.
    let cue = "TRACK 01 MODE1/2048\nINDEX 01 00:00:00\n\
                   TRACK 02 AUDIO\nINDEX 01 00:00:01\n";
    let mut bin = vec![0u8; DATA_SECTOR + 100 * RAW_SECTOR];
    for b in bin[DATA_SECTOR..].iter_mut() {
        *b = 0x20;
    }
    CdImage::from_cue(cue, bin).unwrap()
}

#[test]
fn play_audio_arms_playback_and_streams_frames() {
    let mut dev = AtapiDevice::new();
    dev.insert(audio_disc());
    // Play from LBA 1 (audio start) for 4 frames.
    let mut c = cdb(0x45);
    c[5] = 1; // LBA 1
    c[8] = 4; // 4 frames
    assert!(matches!(dev.execute(&c), CmdResult::Data(_)));
    assert!(dev.playback().playing);
    // The mixer pulls frames until the range is consumed.
    let mut frames = 0;
    while dev.next_audio_frame().is_some() {
        frames += 1;
        if frames > 10 {
            break;
        }
    }
    assert_eq!(frames, 4);
    assert!(!dev.playback().playing);
}

#[test]
fn pause_then_resume_toggles_playing() {
    let mut dev = AtapiDevice::new();
    dev.insert(audio_disc());
    let mut c = cdb(0x45);
    c[5] = 1;
    c[8] = 50;
    let _ = dev.execute(&c);
    // Pause (byte 8 bit0 = 0).
    let _ = dev.execute(&cdb(0x4B));
    assert!(!dev.playback().playing && dev.playback().paused);
    assert!(!dev.mixer_audio_active());
    assert!(dev.peek_mixer_audio_frame().is_none());
    // Resume (byte 8 bit0 = 1).
    let mut resume = cdb(0x4B);
    resume[8] = 0x01;
    let _ = dev.execute(&resume);
    assert!(dev.playback().playing);
    assert!(dev.mixer_audio_active());
    // Stop.
    let _ = dev.execute(&cdb(0x4E));
    assert!(!dev.playback().playing);
}

#[test]
fn read_subchannel_reports_audio_status() {
    let mut dev = AtapiDevice::new();
    dev.insert(audio_disc());
    let mut play = cdb(0x45);
    play[5] = 1;
    play[8] = 10;
    let _ = dev.execute(&play);
    let mut c = cdb(0x42);
    c[2] = 0x40; // SubQ
    c[3] = 0x01; // current position format
    c[8] = 48;
    let buf = data(dev.execute(&c));
    assert_eq!(buf[1], 0x11); // audio play in progress
}

#[test]
fn unknown_command_is_illegal_request() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    assert!(matches!(dev.execute(&cdb(0xFF)), CmdResult::Error));
    assert_eq!(dev.sense_key, sense_key::ILLEGAL_REQUEST);
    assert_eq!(dev.asc, asc::INVALID_COMMAND.0);
}

// Interrupt reason (C/D, I/O) and BSY behavior.

#[test]
fn arm_packet_sets_await_packet_reason() {
    let mut dev = AtapiDevice::new();
    dev.arm_packet();
    assert_eq!(dev.interrupt_reason(), interrupt_reason::AWAIT_PACKET);
    assert_eq!(interrupt_reason::AWAIT_PACKET, 0x01); // C/D=1, I/O=0
}

#[test]
fn data_in_command_leaves_data_in_reason() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(8));
    let _ = dev.execute(&cdb(0x00)); // clear unit attention
    let mut c = cdb(0x28); // READ(10), returns a sector
    c[5] = 0;
    c[8] = 1;
    let _ = dev.execute(&c);
    assert_eq!(dev.interrupt_reason(), interrupt_reason::DATA_IN);
    assert_eq!(interrupt_reason::DATA_IN, 0x02); // C/D=0, I/O=1
}

#[test]
fn no_data_command_lands_on_command_complete() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    let _ = dev.execute(&cdb(0x00)); // clear unit attention
    let _ = dev.execute(&cdb(0x00)); // TEST UNIT READY, no data
    assert_eq!(dev.interrupt_reason(), interrupt_reason::COMMAND_COMPLETE);
    assert_eq!(interrupt_reason::COMMAND_COMPLETE, 0x03); // C/D=1, I/O=1
}

#[test]
fn error_lands_on_command_complete_reason() {
    let mut dev = AtapiDevice::new();
    // No medium: TEST UNIT READY errors.
    assert!(matches!(dev.execute(&cdb(0x00)), CmdResult::Error));
    assert_eq!(dev.interrupt_reason(), interrupt_reason::COMMAND_COMPLETE);
}

#[test]
fn bsy_const_is_the_high_bit() {
    assert_eq!(BSY, 0x80);
}

// START STOP UNIT (0x1B) and PREVENT/ALLOW (0x1E).

#[test]
fn prevent_allow_latches_the_flag() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    let mut c = cdb(0x1E);
    c[4] = 0x01; // prevent
    assert!(matches!(dev.execute(&c), CmdResult::Data(_)));
    assert!(dev.removal_prevented());
    c[4] = 0x00; // allow
    let _ = dev.execute(&c);
    assert!(!dev.removal_prevented());
}

#[test]
fn prevent_allow_not_ready_when_empty() {
    let mut dev = AtapiDevice::new();
    assert!(matches!(dev.execute(&cdb(0x1E)), CmdResult::Error));
    assert_eq!(dev.sense_key, sense_key::NOT_READY);
}

#[test]
fn start_stop_eject_clears_the_disc() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    let mut c = cdb(0x1B);
    c[4] = 0x02; // LoEj=1, Start=0: eject
    assert!(matches!(dev.execute(&c), CmdResult::Data(_)));
    assert!(!dev.is_loaded());
}

#[test]
fn start_stop_eject_blocked_when_removal_prevented() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    let mut prevent = cdb(0x1E);
    prevent[4] = 0x01;
    let _ = dev.execute(&prevent);
    let mut eject = cdb(0x1B);
    eject[4] = 0x02; // eject
    assert!(matches!(dev.execute(&eject), CmdResult::Error));
    assert_eq!(dev.sense_key, sense_key::NOT_READY);
    assert_eq!((dev.asc, dev.ascq), asc::MEDIUM_REMOVAL_PREVENTED);
    assert!(dev.is_loaded()); // still mounted
}

#[test]
fn start_stop_flips_started_flag() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    let mut start = cdb(0x1B);
    start[4] = 0x01; // Start=1
    let _ = dev.execute(&start);
    assert!(dev.started());
    let mut stop = cdb(0x1B);
    stop[4] = 0x00; // Start=0, no eject
    let _ = dev.execute(&stop);
    assert!(!dev.started());
    assert!(dev.is_loaded()); // a plain stop does not eject
}

// SEEK (0x2B).

#[test]
fn seek_in_range_succeeds_with_no_data() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(8));
    let mut c = cdb(0x2B);
    c[5] = 4; // LBA 4, in range
    let buf = data(dev.execute(&c));
    assert!(buf.is_empty());
}

#[test]
fn seek_past_end_is_lba_out_of_range() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(4));
    let mut c = cdb(0x2B);
    c[5] = 4; // LBA 4 on a 4-sector disc
    assert!(matches!(dev.execute(&c), CmdResult::Error));
    assert_eq!(dev.sense_key, sense_key::ILLEGAL_REQUEST);
    assert_eq!(dev.asc, asc::LBA_OUT_OF_RANGE.0);
}

#[test]
fn seek_not_ready_when_empty() {
    let mut dev = AtapiDevice::new();
    assert!(matches!(dev.execute(&cdb(0x2B)), CmdResult::Error));
    assert_eq!(dev.sense_key, sense_key::NOT_READY);
}

// REQUEST SENSE information, INQUIRY EVPD, and READ HEADER.

#[test]
fn request_sense_carries_failing_lba_with_valid_bit() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(4));
    let _ = dev.execute(&cdb(0x00)); // clear unit attention
    // SEEK past the end latches the failing LBA.
    let mut seek = cdb(0x2B);
    seek[5] = 7;
    assert!(matches!(dev.execute(&seek), CmdResult::Error));
    let mut c = cdb(0x03);
    c[4] = 18;
    let buf = data(dev.execute(&c));
    assert_eq!(buf[0] & 0x80, 0x80); // VALID bit
    let info = u32::from_be_bytes([buf[3], buf[4], buf[5], buf[6]]);
    assert_eq!(info, 7);
    // A clean error (no LBA) leaves the VALID bit clear next time.
    let _ = dev.execute(&cdb(0x00)); // ready, no latch
    let buf2 = data(dev.execute(&c));
    assert_eq!(buf2[0] & 0x80, 0x00);
}

#[test]
fn inquiry_evpd_supported_pages() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    let mut c = cdb(0x12);
    c[1] = 0x01; // EVPD
    c[2] = 0x00; // supported pages
    c[4] = 32;
    let buf = data(dev.execute(&c));
    assert_eq!(buf[1], 0x00); // page code
    let len = buf[3] as usize;
    let pages = &buf[4..4 + len];
    assert!(pages.contains(&0x00));
    assert!(pages.contains(&0x80));
    assert!(pages.contains(&0x83));
}

#[test]
fn inquiry_evpd_unit_serial() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    let mut c = cdb(0x12);
    c[1] = 0x01; // EVPD
    c[2] = 0x80; // unit serial
    c[4] = 64;
    let buf = data(dev.execute(&c));
    assert_eq!(buf[1], 0x80);
    let len = buf[3] as usize;
    let serial = &buf[4..4 + len];
    assert_eq!(serial, b"IZARRA-CD-0001");
}

#[test]
fn inquiry_evpd_unknown_page_is_illegal() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    let mut c = cdb(0x12);
    c[1] = 0x01; // EVPD
    c[2] = 0x55; // unsupported page
    c[4] = 32;
    assert!(matches!(dev.execute(&c), CmdResult::Error));
    assert_eq!(dev.sense_key, sense_key::ILLEGAL_REQUEST);
}

#[test]
fn inquiry_nonzero_page_without_evpd_is_illegal() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    let mut c = cdb(0x12);
    c[2] = 0x80; // page code without EVPD
    c[4] = 36;
    assert!(matches!(dev.execute(&c), CmdResult::Error));
    assert_eq!(dev.sense_key, sense_key::ILLEGAL_REQUEST);
}

#[test]
fn read_header_reports_data_mode_and_address() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(8));
    let _ = dev.execute(&cdb(0x00)); // clear unit attention
    let mut c = cdb(0x44);
    c[5] = 3; // LBA 3
    c[8] = 8; // allocation
    let buf = data(dev.execute(&c));
    assert_eq!(buf.len(), 8);
    assert_eq!(buf[0], 0x01); // MODE1 data
    let lba = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    assert_eq!(lba, 3);
}

#[test]
fn read_header_msf_address() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(8));
    let _ = dev.execute(&cdb(0x00));
    let mut c = cdb(0x44);
    c[1] = 0x02; // MSF
    c[5] = 0; // LBA 0 -> 00:02:00 with lead-in
    c[8] = 8;
    let buf = data(dev.execute(&c));
    // Byte 4 reserved (0), then M, S, F.
    assert_eq!((buf[5], buf[6], buf[7]), (0, 2, 0));
}

#[test]
fn read_header_past_end_is_out_of_range() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(4));
    let _ = dev.execute(&cdb(0x00));
    let mut c = cdb(0x44);
    c[5] = 9;
    c[8] = 8;
    assert!(matches!(dev.execute(&c), CmdResult::Error));
    assert_eq!(dev.sense_key, sense_key::ILLEGAL_REQUEST);
    assert_eq!(dev.asc, asc::LBA_OUT_OF_RANGE.0);
}

// READ CD (0xBE), MODE SENSE(10) pages, and MODE SELECT(10).

#[test]
fn read_cd_returns_user_data_like_read10() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(8));
    let _ = dev.execute(&cdb(0x00)); // clear unit attention
    let mut c = cdb(0xBE);
    c[1] = 0x04 << 5; // expected sector type 4 (Mode 2 Form 1) over a data track
    c[5] = 3; // LBA 3
    c[8] = 2; // 2 sectors of transfer length
    c[9] = 0x10; // main-channel: user data
    let buf = data(dev.execute(&c));
    assert_eq!(buf.len(), 2 * DATA_SECTOR);
    assert_eq!(buf[0], 0x43); // 0x40 + 3
    assert_eq!(buf[DATA_SECTOR], 0x44); // next sector marker
}

#[test]
fn read_cd_without_user_data_selection_returns_nothing() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(8));
    let _ = dev.execute(&cdb(0x00));
    let mut c = cdb(0xBE);
    c[5] = 0;
    c[8] = 1;
    c[9] = 0x00; // no main-channel data requested
    let buf = data(dev.execute(&c));
    assert!(buf.is_empty());
}

#[test]
fn read_cd_past_end_is_out_of_range() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(4));
    let _ = dev.execute(&cdb(0x00));
    let mut c = cdb(0xBE);
    c[5] = 4; // LBA 4 on a 4-sector disc
    c[8] = 1;
    c[9] = 0x10;
    assert!(matches!(dev.execute(&c), CmdResult::Error));
    assert_eq!(dev.sense_key, sense_key::ILLEGAL_REQUEST);
    assert_eq!(dev.asc, asc::LBA_OUT_OF_RANGE.0);
}

#[test]
fn read_cd_wrong_sector_type_is_illegal() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(8));
    let _ = dev.execute(&cdb(0x00));
    let mut c = cdb(0xBE);
    c[1] = 0x01 << 5; // expected type 1 (CD-DA) over a data track
    c[5] = 0;
    c[8] = 1;
    c[9] = 0x10;
    assert!(matches!(dev.execute(&c), CmdResult::Error));
    assert_eq!(dev.sense_key, sense_key::ILLEGAL_REQUEST);
    assert_eq!(dev.asc, asc::INVALID_FIELD_IN_CDB.0);
}

#[test]
fn mode_sense10_page_2a_is_well_formed() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    let mut c = cdb(0x5A);
    c[2] = 0x2A; // capabilities page
    c[8] = 200; // allocation, plenty
    let buf = data(dev.execute(&c));
    // 8-byte MODE SENSE(10) header: mode data length (2), medium type at [2].
    let data_len = u16::from_be_bytes([buf[0], buf[1]]) as usize;
    assert_eq!(data_len, buf.len() - 2, "mode data length spans the rest");
    assert_eq!(buf[2], 0x05, "medium type CD-ROM");
    // The page follows the 8-byte header: page code then page length.
    let page = &buf[8..];
    assert_eq!(page[0] & 0x3F, 0x2A, "page code 0x2A");
    assert_eq!(page[1], 20, "page length");
    // The reported max read speed is the 12x figure.
    let speed = u16::from_be_bytes([page[8], page[9]]);
    assert_eq!(speed, (CD_BYTES_PER_SEC / 1024.0) as u16);
}

#[test]
fn mode_sense10_page_0e_reports_audio_volume() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    let mut c = cdb(0x5A);
    c[2] = 0x0E; // audio control page
    c[8] = 200;
    let buf = data(dev.execute(&c));
    let page = &buf[8..];
    assert_eq!(page[0] & 0x3F, 0x0E, "page code 0x0E");
    assert_eq!(page[1], 14, "page length");
    // Default power-up volume is full on both output ports.
    assert_eq!(page[9], 0xFF, "port 0 volume full");
    assert_eq!(page[11], 0xFF, "port 1 volume full");
}

#[test]
fn mode_sense10_unknown_page_is_illegal() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    let mut c = cdb(0x5A);
    c[2] = 0x12; // a page the model does not carry
    c[8] = 64;
    assert!(matches!(dev.execute(&c), CmdResult::Error));
    assert_eq!(dev.sense_key, sense_key::ILLEGAL_REQUEST);
}

#[test]
fn mode_select_stores_audio_volume_read_back_by_mode_sense() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    // The CDB only acks; the parameter list applies through mode_select_data.
    assert!(matches!(dev.execute(&cdb(0x55)), CmdResult::Data(_)));
    // Build an 8-byte header (no block descriptors) then a 16-byte page 0x0E
    // with both output-port volumes set to 0x40.
    let mut params = vec![0u8; 8];
    let mut page = audio_page_0e([0x40, 0x40]);
    params.append(&mut page);
    assert!(matches!(dev.mode_select_data(&params), CmdResult::Data(_)));
    // MODE SENSE(10) now reports the stored volumes.
    let mut c = cdb(0x5A);
    c[2] = 0x0E;
    c[8] = 64;
    let buf = data(dev.execute(&c));
    let sense_page = &buf[8..];
    assert_eq!(sense_page[9], 0x40, "port 0 volume stored");
    assert_eq!(sense_page[11], 0x40, "port 1 volume stored");
}

#[test]
fn mode_select_malformed_list_is_illegal() {
    let mut dev = AtapiDevice::new();
    dev.insert(data_disc(2));
    // A too-short parameter list (under the 8-byte header) is an illegal field.
    assert!(matches!(dev.mode_select_data(&[0u8; 4]), CmdResult::Error));
    assert_eq!(dev.sense_key, sense_key::ILLEGAL_REQUEST);
}
