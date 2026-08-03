// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::path::PathBuf;

/// A temp dir that cleans itself up, so a failing assert cannot leave one behind.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "bootprofile_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn overrides_append_the_mark_to_the_users_own_autoexec() {
    let dir = TempDir::new("autoexec");
    std::fs::write(
        dir.0.join("AUTOEXEC.BAT"),
        b"@ECHO OFF\r\nPATH C:\\DOS\r\nLH TOKAMOUS\r\n",
    )
    .unwrap();

    let overrides = build_overrides(&dir.0, Some("C:\\BIG.DAT")).unwrap();
    let autoexec = overrides
        .iter()
        .find(|(name, _)| name == "AUTOEXEC.BAT")
        .map(|(_, bytes)| String::from_utf8(bytes.clone()).unwrap())
        .expect("an AUTOEXEC.BAT override");

    // The user's own lines survive verbatim: this boot has to be the boot they
    // are complaining about, not a stripped-down stand-in.
    assert!(autoexec.starts_with("@ECHO OFF\r\nPATH C:\\DOS\r\nLH TOKAMOUS\r\n"));
    assert!(autoexec.trim_end().ends_with("@C:\\MARK.COM 1"));
}

#[test]
fn overrides_never_touch_the_host_folder() {
    let dir = TempDir::new("readonly");
    let autoexec = dir.0.join("AUTOEXEC.BAT");
    std::fs::write(&autoexec, b"@ECHO OFF\r\n").unwrap();
    let before = std::fs::read(&autoexec).unwrap();

    build_overrides(&dir.0, Some("C:\\BIG.DAT")).unwrap();

    assert_eq!(std::fs::read(&autoexec).unwrap(), before);
}

#[test]
fn a_missing_autoexec_still_yields_a_mark() {
    let dir = TempDir::new("noautoexec");
    let overrides = build_overrides(&dir.0, None).unwrap();
    let autoexec = overrides
        .iter()
        .find(|(name, _)| name == "AUTOEXEC.BAT")
        .map(|(_, bytes)| String::from_utf8(bytes.clone()).unwrap())
        .expect("an AUTOEXEC.BAT override");
    assert_eq!(autoexec, "@C:\\MARK.COM 1\r\n");
}

#[test]
fn a_tail_without_a_newline_does_not_swallow_the_mark() {
    let dir = TempDir::new("nonewline");
    std::fs::write(dir.0.join("AUTOEXEC.BAT"), b"LH TOKAMOUS").unwrap();
    let overrides = build_overrides(&dir.0, None).unwrap();
    let autoexec = overrides
        .iter()
        .find(|(name, _)| name == "AUTOEXEC.BAT")
        .map(|(_, bytes)| String::from_utf8(bytes.clone()).unwrap())
        .unwrap();
    assert_eq!(autoexec, "LH TOKAMOUS\r\n@C:\\MARK.COM 1\r\n");
}

#[test]
fn loadtest_is_overlaid_only_when_there_is_a_target() {
    let dir = TempDir::new("target");
    let named = build_overrides(&dir.0, Some("C:\\BIG.DAT")).unwrap();
    assert!(named.iter().any(|(name, _)| name == "LOADTEST.COM"));
    let unnamed = build_overrides(&dir.0, None).unwrap();
    assert!(!unnamed.iter().any(|(name, _)| name == "LOADTEST.COM"));
}

#[test]
fn auto_pick_takes_the_largest_plain_83_root_file() {
    let dir = TempDir::new("pick");
    std::fs::write(dir.0.join("SMALL.COM"), vec![0u8; 10]).unwrap();
    std::fs::write(dir.0.join("BIG.DAT"), vec![0u8; 5000]).unwrap();
    std::fs::create_dir(dir.0.join("GAMES")).unwrap();

    assert_eq!(
        auto_pick_load_target(&dir.0).unwrap(),
        Some("C:\\BIG.DAT".to_string())
    );
}

#[test]
fn auto_pick_skips_our_own_overlays() {
    // These are served from RAM once overlaid, so reading one would measure the
    // in-memory path and report zero host reads.
    let dir = TempDir::new("skipown");
    std::fs::write(dir.0.join("MARK.COM"), vec![0u8; 9000]).unwrap();
    std::fs::write(dir.0.join("LOADTEST.COM"), vec![0u8; 9000]).unwrap();
    std::fs::write(dir.0.join("AUTOEXEC.BAT"), vec![0u8; 9000]).unwrap();
    std::fs::write(dir.0.join("REAL.DAT"), vec![0u8; 100]).unwrap();

    assert_eq!(
        auto_pick_load_target(&dir.0).unwrap(),
        Some("C:\\REAL.DAT".to_string())
    );
}

#[test]
fn auto_pick_skips_names_katea_would_mangle() {
    // A mangled name would read a different file than the one reported.
    let dir = TempDir::new("mangle");
    std::fs::write(dir.0.join("A LONG NAME.DATA"), vec![0u8; 9000]).unwrap();
    std::fs::write(dir.0.join("OK.BIN"), vec![0u8; 100]).unwrap();

    assert_eq!(
        auto_pick_load_target(&dir.0).unwrap(),
        Some("C:\\OK.BIN".to_string())
    );
}

#[test]
fn auto_pick_reports_nothing_for_an_empty_folder() {
    let dir = TempDir::new("empty");
    assert_eq!(auto_pick_load_target(&dir.0).unwrap(), None);
}

#[test]
fn plain_83_accepts_real_names_and_rejects_mangled_ones() {
    assert!(is_plain_83("UNISOUND.COM"));
    assert!(is_plain_83("COMMAND.COM"));
    assert!(is_plain_83("README"));
    assert!(!is_plain_83("TOOLONGNAME.COM"));
    assert!(!is_plain_83("NAME.TOOLONG"));
    assert!(!is_plain_83("HAS SPACE.COM"));
    assert!(!is_plain_83("TWO.DOTS.COM"));
    assert!(!is_plain_83(""));
    assert!(!is_plain_83(".COM"));
}

#[test]
fn a_phase_whose_closing_mark_never_fired_reports_not_reached() {
    // The soft-failure contract: a missing disk phase must not fabricate a row,
    // and must not disturb the phases that did complete.
    let rows = build_rows(&[]);
    assert_eq!(rows.len(), 5);
    assert!(rows.iter().all(|row| !row.reached));
    assert!(rows.iter().all(|row| row.wall_seconds == 0.0));
}

#[test]
fn real_time_factor_and_coverage_are_zero_safe() {
    let row = PhaseRow {
        name: "idle",
        reached: true,
        wall_seconds: 0.0,
        guest_seconds: 5.0,
        instructions: 0,
        direct_native_insns: 0,
        katea: KateaStorageCounters::default(),
        machine_phases: Vec::new(),
    };
    assert_eq!(row.real_time_factor(), 0.0);
    assert_eq!(row.native_coverage(), 0.0);
}

#[test]
fn real_time_factor_is_guest_seconds_per_wall_second() {
    let row = PhaseRow {
        name: "idle",
        reached: true,
        wall_seconds: 4.0,
        guest_seconds: 1.0,
        instructions: 200,
        direct_native_insns: 50,
        katea: KateaStorageCounters::default(),
        machine_phases: Vec::new(),
    };
    assert_eq!(row.real_time_factor(), 0.25);
    assert_eq!(row.native_coverage(), 0.25);
}

#[test]
fn only_a_cycle_limit_lets_the_next_slice_run() {
    assert!(terminal_stop(&StopReason::CycleLimit { requested: 1 }).is_none());
    assert!(terminal_stop(&StopReason::Halted).is_some());
    assert!(terminal_stop(&StopReason::TestExit { code: 0 }).is_some());
    assert!(terminal_stop(&StopReason::DosExit { code: 0 }).is_some());
    assert!(terminal_stop(&StopReason::CpuError("boom".into())).is_some());
}
