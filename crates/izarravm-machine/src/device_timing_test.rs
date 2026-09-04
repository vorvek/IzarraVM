// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn unset_is_none() {
    let profile = parse_device_timing_profile(Err(std::env::VarError::NotPresent));
    assert!(profile.is_none());
    assert_eq!(profile, DeviceTimingProfile::none());
}

#[test]
fn empty_is_none() {
    let profile = parse_device_timing_profile(Ok(String::new()));
    assert!(profile.is_none());
}

#[test]
fn whitespace_only_is_none() {
    let profile = parse_device_timing_profile(Ok("   ".to_string()));
    assert!(profile.is_none());
}

#[test]
fn period_arms_every_family() {
    let profile = parse_device_timing_profile(Ok("period".to_string()));
    assert_eq!(profile, DeviceTimingProfile::all());
    assert!(!profile.is_none());
}

#[test]
fn period_is_case_insensitive() {
    let profile = parse_device_timing_profile(Ok("PERIOD".to_string()));
    assert_eq!(profile, DeviceTimingProfile::all());
}

#[test]
fn a_family_list_arms_only_those_families() {
    let profile = parse_device_timing_profile(Ok("ata,cd".to_string()));
    assert!(profile.ata);
    assert!(profile.cd);
    assert!(!profile.dma);
    assert!(!profile.pic);
    assert!(!profile.kbc);
    assert!(!profile.sb);
    assert!(!profile.fdc);
}

#[test]
fn every_family_name_is_recognised() {
    for name in ["pic", "dma", "ata", "cd", "fdc", "kbc", "sb"] {
        let profile = parse_device_timing_profile(Ok(name.to_string()));
        assert_ne!(
            profile,
            DeviceTimingProfile::none(),
            "family {name:?} did not arm"
        );
        assert_eq!(
            profile,
            DeviceTimingProfile::all().and_only(name),
            "family {name:?} armed more than itself"
        );
    }
}

#[test]
fn family_names_are_case_insensitive_and_trim_whitespace() {
    let profile = parse_device_timing_profile(Ok(" ATA , Kbc ".to_string()));
    assert!(profile.ata);
    assert!(profile.kbc);
    assert!(!profile.dma);
}

#[test]
fn stray_commas_are_ignored_not_errors() {
    let profile = parse_device_timing_profile(Ok(",ata,,cd,".to_string()));
    assert!(profile.ata);
    assert!(profile.cd);
    assert!(!profile.dma);
}

#[test]
fn period_anywhere_in_the_list_arms_everything() {
    let profile = parse_device_timing_profile(Ok("ata,period".to_string()));
    assert_eq!(profile, DeviceTimingProfile::all());
}

#[test]
#[should_panic(expected = "names an unknown family")]
fn an_unknown_family_panics() {
    let _ = parse_device_timing_profile(Ok("scsi".to_string()));
}

#[test]
#[should_panic(expected = "not valid UTF-8")]
fn non_utf8_panics() {
    let _ = parse_device_timing_profile(Err(std::env::VarError::NotUnicode(
        std::ffi::OsString::from("garbage"),
    )));
}

#[test]
fn default_reads_the_profile_default_construction_path() {
    // device_timing_profile_default() reads the real environment; this repo's
    // test harness does not set IZARRAVM_DEVICE_TIMING, so this exercises the
    // unset arm through the real entry point rather than the parse helper.
    if std::env::var("IZARRAVM_DEVICE_TIMING").is_err() {
        assert!(device_timing_profile_default().is_none());
    }
}

impl DeviceTimingProfile {
    /// Test-only helper: `DeviceTimingProfile::all()` restricted to exactly
    /// one family, for the "does this name arm only itself" assertion above.
    fn and_only(self, name: &str) -> Self {
        let mut out = DeviceTimingProfile::none();
        assert!(
            out.set_family(name),
            "unknown family {name:?} in test helper"
        );
        out
    }
}
