// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! TOKAMOUS.COM, the INT 33h driver, exercised from a guest program.
//!
//! The driver under test is the committed `roms/dos/tokamous.com`, placed in
//! the mounted folder's root so `LH TOKAMOUS` resolves it ahead of the copy on
//! the Toka-DOS image (the shell searches the current directory before PATH).
//! That keeps the test pinned to the source in `toka-dos/tools/tokamous.asm`
//! rather than to whatever image was last rebuilt.

use super::*;

/// Boot Toka-DOS with the committed driver and run `program` (a guest binary
/// that exits through the Lotura unit-test port). Returns the exit code.
fn run_mouse_fixture(label: &str, program_name: &str, program: &[u8]) -> u8 {
    let scratch = TokaScratch::new(label);
    let autoexec = format!(
        "@ECHO OFF\r\nPATH C:\\DOS\r\nLH TOKAMOUS\r\n{}\r\n",
        program_name.trim_end_matches(".COM")
    );
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine
        .mount_hdd_folder_with(
            scratch.path(),
            vec![
                ("AUTOEXEC.BAT".to_string(), autoexec.into_bytes()),
                (
                    "TOKAMOUS.COM".to_string(),
                    izarravm_firmware::tokamous_com().to_vec(),
                ),
                (program_name.to_string(), program.to_vec()),
            ],
        )
        .expect("mount Toka-DOS folder");
    let stop = machine
        .run_until_halt_or_cycles(900_000_000)
        .expect("run mouse fixture");
    match stop {
        StopReason::TestExit { code } => code,
        other => panic!(
            "{program_name} did not exit through the test port (stop={other:?})\n{}",
            machine.screen_text().as_text()
        ),
    }
}

/// The driver draws the fn 09h cursor in mode 13h, restores the background on
/// hide and on move, keeps a vertical range a program sets past the mode's
/// height, and hides the cursor across an INT 10h mode set. MOUSEGFX.COM
/// reports the first failing step; see its source for the step numbers.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn tokamous_draws_the_graphics_cursor_in_mode_13h() {
    let code = run_mouse_fixture(
        "mousegfx",
        "MOUSEGFX.COM",
        izarravm_firmware::mousegfx_com(),
    );
    assert_eq!(code, 0, "MOUSEGFX.COM failed at step {code}");
}
