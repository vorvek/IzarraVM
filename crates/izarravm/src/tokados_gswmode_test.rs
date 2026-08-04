// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! GSWMODE.COM writing the CPU speed to CMOS.
//!
//! The tool used to be runtime-only, on the grounds that the BIOS setup menu
//! owned the boot default. That left the speed as the one machine setting with
//! no way to change it permanently from DOS, which stopped being defensible
//! once CMOS became the machine's only record of its own configuration and the
//! config file stopped carrying a CPU speed at all.

use super::*;

fn boot_with_gswmode(label: &str, commands: &str) -> Machine {
    let scratch = TokaScratch::new(label);
    let dir = scratch.path().to_path_buf();
    std::mem::forget(scratch);
    fs::write(
        dir.join("AUTOEXEC.BAT"),
        format!("@ECHO OFF\r\nPROMPT $P$G\r\nPATH C:\\DOS\r\n{commands}"),
    )
    .expect("write AUTOEXEC");

    // Boot at 586 so every mode the tests switch to is a visible change, and so
    // the "saved" byte starts at a known value other than the ones written.
    let mut machine = Machine::new(
        MachineProfile {
            cpu: GswMode::Gsw586,
            ..MachineProfile::gsw_386(16, VideoCard::Vega)
        },
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd_folder(&dir).expect("mount host folder");
    let (stop, _) = run_until_toka_condition(&mut machine, 900_000_000, current_root_prompt);
    if let StopReason::CpuError(message) = &stop {
        panic!(
            "CPU fault running GSWMODE: {message}\n{}",
            machine.screen_text().as_text()
        );
    }
    let _ = fs::remove_dir_all(&dir);
    machine
}

/// The speed moves and stays moved, with a checksum the next POST will accept.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn gswmode_saves_the_speed_in_cmos() {
    let machine = boot_with_gswmode("gswmode_save", "GSWMODE 486\r\n");
    let screen = machine.screen_text().as_text();

    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw486,
        "the live speed did not change\n{screen}"
    );
    let cmos = machine.cmos_bytes();
    assert_eq!(
        cmos[0x12],
        GswMode::Gsw486.register_code(),
        "the speed was not written to CMOS 0x12\n{screen}"
    );
    // CMOS 0x12 sits inside the checksum window; a stale checksum would make
    // the next POST discard the keyboard layout and the sound card's
    // resources along with the speed this tool just saved.
    let sum = cmos[0x10..=0x2D]
        .iter()
        .fold(0u16, |acc, byte| acc.wrapping_add(u16::from(*byte)));
    assert_eq!(
        (cmos[0x2E], cmos[0x2F]),
        ((sum >> 8) as u8, sum as u8),
        "GSWMODE must refresh the NVRAM checksum it invalidated\n{screen}"
    );
    assert!(
        screen.contains("switched to 486, saved."),
        "the confirmation should say the choice was kept\n{screen}"
    );
}

/// `/T` is the escape for the thing the old behaviour was actually good at:
/// running one program slower without committing to it.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn gswmode_slash_t_moves_the_speed_without_saving_it() {
    let machine = boot_with_gswmode("gswmode_temp", "GSWMODE 386-slow /T\r\n");
    let screen = machine.screen_text().as_text();

    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw386Slow,
        "/T must still change the live speed\n{screen}"
    );
    assert_eq!(
        machine.cmos_bytes()[0x12],
        GswMode::Gsw586.register_code(),
        "/T must leave the saved speed alone\n{screen}"
    );
    assert!(
        screen.contains("for this session only."),
        "the confirmation should say the choice was not kept\n{screen}"
    );
}

/// With the two able to disagree, reporting only one of them would be the
/// least useful answer, so the no-argument form prints both.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn gswmode_reports_the_live_and_the_saved_speed_separately() {
    let machine = boot_with_gswmode(
        "gswmode_report",
        "GSWMODE 486\r\nGSWMODE 386 /T\r\nGSWMODE\r\n",
    );
    let screen = machine.screen_text().as_text();
    assert!(
        screen.contains("Current mode: 386"),
        "the live speed is the temporary one\n{screen}"
    );
    assert!(
        screen.contains("Saved mode:   486"),
        "the saved speed is the one that survives a reboot\n{screen}"
    );
}

/// A typo after the mode name must not be read as a switch and silently
/// accepted: the parser was strict about trailing text before `/T` existed and
/// stays strict about everything that is not a switch.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn gswmode_rejects_trailing_text_that_is_not_a_switch() {
    let machine = boot_with_gswmode("gswmode_typo", "GSWMODE 486 junk\r\n");
    let screen = machine.screen_text().as_text();
    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw586,
        "a rejected line must change nothing\n{screen}"
    );
    assert_eq!(machine.cmos_bytes()[0x12], GswMode::Gsw586.register_code());
    assert!(
        screen.contains("Usage: GSWMODE"),
        "the usage text should explain the rejection\n{screen}"
    );
}
