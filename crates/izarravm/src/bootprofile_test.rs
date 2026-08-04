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
        census: None,
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
        census: None,
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

/// Build a census snapshot from `(opcode, group, instructions)` rows plus an
/// address map, so a test can state the two boundaries directly.
fn snapshot(
    groups: &[(&'static str, u64, u64, u64)],
    opcodes: &[(u16, &'static str, u64, u64)],
    addrs: &[(u32, u64)],
) -> izarravm_cpu::CpuProfileSnapshot {
    izarravm_cpu::CpuProfileSnapshot {
        sample_stride: 512,
        groups: groups
            .iter()
            .map(|&(name, instructions, guest_core_clocks, sample_wall_ns)| {
                izarravm_cpu::CpuProfileBucket {
                    name,
                    instructions,
                    guest_core_clocks,
                    sample_wall_ns,
                    samples: 0,
                }
            })
            .collect(),
        opcodes: opcodes
            .iter()
            .map(|&(opcode, group, instructions, sample_wall_ns)| {
                izarravm_cpu::CpuOpcodeProfileBucket {
                    opcode,
                    group,
                    instructions,
                    guest_core_clocks: 0,
                    sample_wall_ns,
                    samples: 0,
                    register_instructions: 0,
                    memory_instructions: 0,
                    register_samples: 0,
                    memory_samples: 0,
                }
            })
            .collect(),
        hot_addrs: addrs.to_vec(),
        smc_flush_blocks: Vec::new(),
    }
}

#[test]
fn census_delta_subtracts_each_bucket() {
    let before = snapshot(
        &[("data_move", 100, 50, 10), ("stack", 20, 10, 2)],
        &[(0x89, "data_move", 60, 6)],
        &[(0x1000, 7)],
    );
    let after = snapshot(
        &[("data_move", 175, 90, 18), ("stack", 20, 10, 2)],
        &[(0x89, "data_move", 100, 11), (0xCD, "control_flow", 5, 3)],
        &[(0x1000, 30)],
    );
    let delta = census_delta(Some(&before), Some(&after)).expect("both boundaries carry a census");

    assert_eq!(
        delta.instructions(),
        75,
        "only the phase's own instructions"
    );
    let data_move = delta
        .groups
        .iter()
        .find(|g| g.0 == "data_move")
        .expect("group survives");
    assert_eq!((data_move.1, data_move.2, data_move.3), (75, 40, 8));
    // A group that did not move in this phase reports zero rather than its total.
    let stack = delta.groups.iter().find(|g| g.0 == "stack").unwrap();
    assert_eq!(stack.1, 0);
    // An opcode absent from the earlier snapshot counts from zero, not from its
    // whole-run total.
    let int_n = delta.opcodes.iter().find(|o| o.0 == 0xCD).unwrap();
    assert_eq!(int_n.2, 5);
    let mov = delta.opcodes.iter().find(|o| o.0 == 0x89).unwrap();
    assert_eq!(mov.2, 40);
}

/// The property that forced `hot_addrs` to carry every address rather than a
/// truncated head. An address hot in an EARLIER phase and quiet afterwards gets
/// pushed down the ranking by later phases; with a 64-entry head it would drop
/// out of the later snapshot entirely and its samples would reappear as if the
/// later phase had lost them. With the full map the later phase correctly
/// reports zero for it and it is filtered out.
#[test]
fn census_delta_is_exact_for_an_address_that_leaves_the_hot_head() {
    let mut before_addrs = vec![(0xF44A0u32, 5_000u64)];
    let mut after_addrs = vec![(0xF44A0u32, 5_000u64)];
    // 100 addresses that only run in the later phase, every one of them hotter
    // than the boot-phase address above.
    for i in 0..100u32 {
        before_addrs.push((0x2000 + i, 0));
        after_addrs.push((0x2000 + i, 9_000 + u64::from(i)));
    }
    before_addrs.retain(|&(_, n)| n > 0);
    let before = snapshot(&[("data_move", 5_000, 0, 0)], &[], &before_addrs);
    let after = snapshot(&[("data_move", 950_000, 0, 0)], &[], &after_addrs);

    let delta = census_delta(Some(&before), Some(&after)).unwrap();
    assert!(
        after.hot_addrs.len() > 64,
        "the fixture must exceed any truncated head to be meaningful"
    );
    assert!(
        !delta.addrs.iter().any(|&(lin, _)| lin == 0xF44A0),
        "an address that did not run in this phase must not appear in it"
    );
    assert_eq!(
        delta.addrs.iter().map(|&(_, n)| n).sum::<u64>(),
        (0..100u64).map(|i| 9_000 + i).sum::<u64>(),
        "every sample in the phase is attributed and none invented"
    );
    // Descending, so the printed head is the phase's hottest.
    assert!(delta.addrs.windows(2).all(|w| w[0].1 >= w[1].1));
}

#[test]
fn census_delta_is_none_without_a_census_on_both_boundaries() {
    let one = snapshot(&[("data_move", 1, 1, 1)], &[], &[]);
    assert!(census_delta(None, Some(&one)).is_none());
    assert!(census_delta(Some(&one), None).is_none());
    assert!(census_delta(None, None).is_none());
}

#[test]
fn address_region_names_the_jit_refused_windows() {
    assert_eq!(address_region(0x0000_4A53), "RAM");
    assert_eq!(address_region(0x000F_4B26), "BIOS ROM (JIT-refused)");
    assert_eq!(address_region(0x000C_8000), "option ROM (JIT-refused)");
    assert_eq!(address_region(0x000A_8000), "VGA aperture (JIT-refused)");
    assert_eq!(address_region(0x0010_0000), "RAM");
}
