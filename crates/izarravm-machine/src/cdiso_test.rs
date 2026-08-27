// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::cdimage::CdImage;
use std::fs;

/// Build a folder disc: root/README.TXT, root/GAME/DATA.BIN,
/// root/GAME/LEVELS/E1M1.MAP. The TempDir must outlive the image — file
/// extents read lazily from the host folder.
fn folder_disc() -> (tempfile::TempDir, CdImage) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("readme.txt"), b"root file").unwrap();
    let game = dir.path().join("game");
    fs::create_dir(&game).unwrap();
    fs::write(game.join("data.bin"), vec![0xA5u8; 5000]).unwrap();
    let levels = game.join("levels");
    fs::create_dir(&levels).unwrap();
    fs::write(levels.join("e1m1.map"), b"level bytes").unwrap();
    let built = crate::iso9660::build(dir.path()).unwrap();
    let image = CdImage::from_folder(built).unwrap();
    (dir, image)
}

#[test]
fn index_builds_and_lists_the_tree() {
    let (_dir, image) = folder_disc();
    let index = IsoIndex::build(&image).unwrap();
    let root = index.lookup_dir("").unwrap();
    let names: Vec<[u8; 11]> = root.entries.iter().map(|e| e.name).collect();
    assert!(names.contains(b"README  TXT"), "{names:?}");
    assert!(names.contains(b"GAME       "), "{names:?}");
    let game = index.lookup_dir("\\GAME").unwrap();
    assert!(
        game.entries
            .iter()
            .any(|e| e.name == *b"LEVELS     " && e.is_dir())
    );
    // A subdirectory lists `.` and `..` first; the root lists neither.
    assert_eq!(game.entries[0].name, *b".          ");
    assert_eq!(game.entries[1].name, *b"..         ");
    assert!(game.entries[0].is_dir() && game.entries[0].subdir.is_none());
    assert!(!names.contains(b".          "), "root has no dot entries");
}

#[test]
fn lookup_resolves_nested_paths_with_either_separator_and_drive_prefix() {
    let (_dir, image) = folder_disc();
    let index = IsoIndex::build(&image).unwrap();
    let entry = index.lookup("D:\\GAME\\DATA.BIN").unwrap();
    assert_eq!(entry.size, 5000);
    assert!(!entry.is_dir());
    let nested = index.lookup("/game/levels/e1m1.map").unwrap();
    assert_eq!(nested.size, 11);
    let file_lba = nested.lba;
    let sector = image.read_data_sector(file_lba).unwrap();
    assert_eq!(&sector[..11], b"level bytes");
}

#[test]
fn lookup_misses_report_none() {
    let (_dir, image) = folder_disc();
    let index = IsoIndex::build(&image).unwrap();
    assert!(index.lookup("\\GAME\\MISSING.TXT").is_none());
    assert!(index.lookup_dir("\\NOSUCH").is_none());
    // A file used as a directory component must not resolve.
    assert!(index.lookup("\\README.TXT\\X").is_none());
    // The root itself has no entry.
    assert!(index.lookup("").is_none());
}

#[test]
fn fcb_names_truncate_and_uppercase() {
    assert_eq!(fcb_name(b"a.txt"), *b"A       TXT");
    assert_eq!(fcb_name(b"LONGNAMEHERE.EXTRA"), *b"LONGNAMEEXT");
    assert_eq!(fcb_name(b"NOEXT"), *b"NOEXT      ");
    assert_eq!(fcb_name(b"DIR."), *b"DIR        ");
}

#[test]
fn garbage_media_has_no_index() {
    // A data medium whose LBA 16 is not a PVD reports no ISO tree instead of
    // a partial index.
    let image = CdImage::from_iso(vec![0u8; 2048 * 32]).unwrap();
    assert!(IsoIndex::build(&image).is_none());
}

#[test]
fn dos_dates_pack_from_the_recording_stamp() {
    let mut record = vec![0u8; 34];
    record[18] = 96; // 1996
    record[19] = 8;
    record[20] = 28;
    record[21] = 13;
    record[22] = 45;
    record[23] = 31;
    assert_eq!(dos_date(&record), (16 << 9) | (8 << 5) | 28);
    assert_eq!(dos_time(&record), (13 << 11) | (45 << 5) | 15);
}
