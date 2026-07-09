// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use time::Month;

#[test]
fn weekday_maps_sunday_to_one() {
    // 2026-06-21 is a Sunday.
    let dt = OffsetDateTime::new_utc(
        time::Date::from_calendar_date(2026, Month::June, 21).unwrap(),
        time::Time::from_hms(12, 0, 0).unwrap(),
    );
    let seed = from_offset(dt);
    assert_eq!(seed.weekday, 1);
    assert_eq!((seed.year, seed.month, seed.day), (2026, 6, 21));
}

#[test]
fn weekday_maps_saturday_to_seven() {
    // 2026-06-20 is a Saturday.
    let dt = OffsetDateTime::new_utc(
        time::Date::from_calendar_date(2026, Month::June, 20).unwrap(),
        time::Time::from_hms(8, 30, 15).unwrap(),
    );
    let seed = from_offset(dt);
    assert_eq!(seed.weekday, 7);
    assert_eq!((seed.hour, seed.minute, seed.second), (8, 30, 15));
}

#[test]
fn load_round_trips_a_saved_image() {
    let dir = std::env::temp_dir().join(format!("izarra_cmos_{}", std::process::id()));
    let c_root = dir.join("c_drive");
    std::fs::create_dir_all(&c_root).unwrap();
    let path = cmos_path(&c_root);
    let mut image = [0u8; 64];
    image[0x10] = 3;
    image[0x2f] = 0xab;
    save_cmos_file(&path, &image);
    let loaded = load_cmos_file(&path).unwrap();
    assert_eq!(loaded, image);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn wrong_size_file_is_treated_as_absent() {
    let dir = std::env::temp_dir().join(format!("izarra_cmos_bad_{}", std::process::id()));
    let c_root = dir.join("c_drive");
    std::fs::create_dir_all(&c_root).unwrap();
    let path = cmos_path(&c_root);
    std::fs::write(&path, [0u8; 32]).unwrap();
    assert!(load_cmos_file(&path).is_none());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn cmos_path_sits_beside_c_root() {
    let c_root = PathBuf::from("/home/user/.izarravm/c_drive");
    assert_eq!(
        cmos_path(&c_root),
        PathBuf::from("/home/user/.izarravm/cmos.bin")
    );
}
