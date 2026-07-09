// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn wizardry_720k_geometry() {
    let g = geometry_for(737_280).unwrap();
    assert_eq!((g.cylinders, g.heads, g.sectors), (80, 2, 9));
    assert_eq!(g.drive_type, 0x03);
}

#[test]
fn supported_sizes_map_to_geometry() {
    assert_eq!(geometry_for(368_640).unwrap().sectors, 9);
    assert_eq!(geometry_for(1_228_800).unwrap().sectors, 15);
    assert_eq!(geometry_for(1_474_560).unwrap().sectors, 18);
}

#[test]
fn early_525_formats_map_to_geometry() {
    // 160 KB and 180 KB are single-sided; 320 KB and 360 KB are double-sided.
    let g160 = geometry_for(163_840).unwrap();
    assert_eq!((g160.cylinders, g160.heads, g160.sectors), (40, 1, 8));
    let g180 = geometry_for(184_320).unwrap();
    assert_eq!((g180.cylinders, g180.heads, g180.sectors), (40, 1, 9));
    let g320 = geometry_for(327_680).unwrap();
    assert_eq!((g320.cylinders, g320.heads, g320.sectors), (40, 2, 8));
    // Each maps to a full disk: cyl * heads * sectors * 512 == file size.
    for size in [163_840, 184_320, 327_680] {
        let g = geometry_for(size).unwrap();
        let bytes = usize::from(g.cylinders) * usize::from(g.heads) * usize::from(g.sectors) * 512;
        assert_eq!(
            bytes, size,
            "geometry for {size} must cover the whole image"
        );
    }
}

#[test]
fn chs_offset_matches_lba() {
    let f = Floppy::from_image(vec![0u8; 737_280]).unwrap();
    // CHS(0,0,1) is LBA 0.
    assert_eq!(f.chs_offset(0, 0, 1), Some(0));
    // CHS(0,1,1) is LBA 9 on a 9-spt disk.
    assert_eq!(f.chs_offset(0, 1, 1), Some(9 * 512));
    // Sector 10 does not exist on a 9-spt disk.
    assert_eq!(f.chs_offset(0, 0, 10), None);
    // Sector 0 is not a valid 1-based sector.
    assert_eq!(f.chs_offset(0, 0, 0), None);
}

#[test]
fn access_duration_models_seek_latency_and_transfer() {
    let mut f = Floppy::from_image(vec![0u8; 1_474_560]).unwrap(); // 1.44M, HD
    // First read at track 0 (head starts there): no seek, no latency, just
    // the transfer of one sector at 62.5 KB/s.
    let one_sector = f.access_duration_secs(0, 512);
    assert!((one_sector - 512.0 / 62_500.0).abs() < 1e-9);
    // A read on the same track is transfer-only again (no fresh latency).
    assert!((f.access_duration_secs(0, 512) - 512.0 / 62_500.0).abs() < 1e-9);
    // Seeking to track 10 costs 10 steps of seek plus half a revolution of
    // rotational latency, on top of the transfer.
    let seek_read = f.access_duration_secs(10, 512);
    let expect = 0.003 * 10.0 + 0.2 / 2.0 + 512.0 / 62_500.0;
    assert!((seek_read - expect).abs() < 1e-9, "{seek_read} vs {expect}");
    // A full-stroke seek is clamped to 100 ms.
    f.access_duration_secs(0, 0);
    let full = f.access_duration_secs(79, 0);
    assert!((full - (0.100 + 0.2 / 2.0)).abs() < 1e-9);
}

#[test]
fn double_density_transfers_at_half_the_rate() {
    let mut hd = Floppy::from_image(vec![0u8; 1_474_560]).unwrap();
    let mut dd = Floppy::from_image(vec![0u8; 737_280]).unwrap();
    // Same bytes, same track: DD takes twice as long to transfer as HD.
    let hd_t = hd.access_duration_secs(0, 4096);
    let dd_t = dd.access_duration_secs(0, 4096);
    assert!((dd_t - 2.0 * hd_t).abs() < 1e-9);
}

#[test]
fn round_trip_sector() {
    let mut f = Floppy::from_image(vec![0u8; 737_280]).unwrap();
    let mut buf = [0u8; 512];
    buf[0] = 0xAB;
    assert!(f.write_sector(1, 1, 5, &buf));
    assert_eq!(f.read_sector(1, 1, 5).unwrap()[0], 0xAB);
    assert!(f.dirty);
}

#[test]
fn format_track_fills_the_addressed_track() {
    let mut f = Floppy::from_image(vec![0u8; 737_280]).unwrap(); // 720 KB, 9 spt
    assert!(f.format_track(2, 1, 0xF6));
    // Every sector of track (cyl 2, head 1) reads back the filler.
    for sector in 1..=9 {
        assert_eq!(f.read_sector(2, 1, sector).unwrap()[0], 0xF6);
        assert_eq!(f.read_sector(2, 1, sector).unwrap()[511], 0xF6);
    }
    // A neighbouring track is untouched.
    assert_eq!(f.read_sector(2, 0, 1).unwrap()[0], 0x00);
    assert!(f.dirty);
}

#[test]
fn format_track_rejects_out_of_range_track() {
    let mut f = Floppy::from_image(vec![0u8; 737_280]).unwrap();
    assert!(!f.format_track(80, 0, 0xF6)); // cyl 80 is off an 80-cyl disk
    assert!(!f.format_track(0, 2, 0xF6)); // head 2 is off a 2-head disk
}

#[test]
fn out_of_range_write_is_rejected() {
    let mut f = Floppy::from_image(vec![0u8; 737_280]).unwrap();
    let buf = [0u8; 512];
    assert!(!f.write_sector(0, 0, 10, &buf));
    assert!(!f.dirty);
}

#[test]
fn unknown_size_rejected() {
    assert!(Floppy::from_image(vec![0u8; 123]).is_err());
}
