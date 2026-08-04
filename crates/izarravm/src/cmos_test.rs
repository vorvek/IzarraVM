// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_core::{GswMode, SbDma8, SbIrq};
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

/// A machine that has never been configured takes the flags: there is no saved
/// CMOS to disagree with them, and the image written on the way out is what
/// they asked for. Nothing to warn about, so nothing is reported.
#[test]
fn flags_set_the_machine_when_there_is_no_saved_cmos() {
    let dir = std::env::temp_dir().join(format!("izarra_cmos_fresh_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let c_root = dir.join("c_drive");
    std::fs::create_dir_all(&c_root).unwrap();

    let mut setup = RtcSetup::from_c_root(&c_root);
    setup.requested = RequestedHardware {
        cpu: Some(GswMode::Gsw486),
        sb_irq: Some(SbIrq::I5),
        ..RequestedHardware::default()
    };
    let mut machine = izarravm_machine::Machine::new(
        izarravm_machine::MachineProfile {
            cpu: GswMode::Gsw486,
            sound_blaster: izarravm_core::SoundBlasterConfig {
                irq: SbIrq::I5,
                ..Default::default()
            },
            ..izarravm_machine::MachineProfile::gsw_386(16, izarravm_core::VideoCard::Vega)
        },
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    setup.apply(&mut machine);

    assert_eq!(machine.active_mode(), GswMode::Gsw486);
    assert_eq!(machine.sound_blaster_routing().unwrap().0, 5);
    assert!(
        setup.requested.overridden_by(&machine).is_empty(),
        "nothing overrode the flags on a fresh machine"
    );
    assert!(cmos_path(&c_root).is_file(), "the image is persisted");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The case the warning exists for: a saved CMOS beats the flags, correctly but
/// invisibly. Every flag the load did not honour must be named, and only those
/// -- a flag that happens to agree with the saved value is not news.
#[test]
fn a_saved_cmos_overrides_the_flags_and_every_ignored_one_is_named() {
    let dir = std::env::temp_dir().join(format!("izarra_cmos_override_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let c_root = dir.join("c_drive");
    std::fs::create_dir_all(&c_root).unwrap();

    // A machine someone has already configured: 586, and the card moved to
    // IRQ 10 with 8-bit DMA 3.
    let mut saved = izarravm_machine::Machine::new(
        izarravm_machine::MachineProfile {
            cpu: GswMode::Gsw586,
            sound_blaster: izarravm_core::SoundBlasterConfig {
                irq: SbIrq::I10,
                dma: SbDma8::D3,
                ..Default::default()
            },
            ..izarravm_machine::MachineProfile::gsw_386(16, izarravm_core::VideoCard::Vega)
        },
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    saved.load_cmos(&saved.cmos_bytes());
    save_cmos_file(&cmos_path(&c_root), &saved.cmos_bytes());

    // Now start it again asking for something else, plus one flag (DMA 3) that
    // happens to match what was saved.
    let mut setup = RtcSetup::from_c_root(&c_root);
    setup.requested = RequestedHardware {
        cpu: Some(GswMode::Gsw386Slow),
        sb_irq: Some(SbIrq::I5),
        sb_dma: Some(SbDma8::D3),
        sb_high_dma: None,
    };
    let mut machine = izarravm_machine::Machine::new(
        izarravm_machine::MachineProfile {
            cpu: GswMode::Gsw386Slow,
            sound_blaster: izarravm_core::SoundBlasterConfig {
                irq: SbIrq::I5,
                dma: SbDma8::D3,
                ..Default::default()
            },
            ..izarravm_machine::MachineProfile::gsw_386(16, izarravm_core::VideoCard::Vega)
        },
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    setup.apply(&mut machine);

    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw586,
        "the saved speed wins over --cpu"
    );
    assert_eq!(machine.sound_blaster_routing().unwrap().0, 10);

    let ignored = setup.requested.overridden_by(&machine);
    assert_eq!(
        ignored,
        vec!["--cpu 386-slow".to_string(), "--sb-irq 5".to_string()],
        "only the flags the saved CMOS actually contradicted"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A flag nobody typed cannot be overridden, so a plain run stays quiet however
/// far the saved machine has drifted from the built-in defaults.
#[test]
fn untyped_flags_are_never_reported() {
    let machine = izarravm_machine::Machine::new(
        izarravm_machine::MachineProfile::gsw_386(16, izarravm_core::VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    assert!(RequestedHardware::default().is_empty());
    assert!(
        RequestedHardware::default()
            .overridden_by(&machine)
            .is_empty()
    );
}
