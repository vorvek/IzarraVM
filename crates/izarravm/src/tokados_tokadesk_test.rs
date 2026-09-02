// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! TOKADESK.EXE smoke: VCPI switch, VBE 0x117, V86 thunk, Lotura exit.
//!
//! `/T` lists C:\\ via INT 21h 4Eh and requires TOKADESK.EXE itself to appear
//! in that root listing, failing 0xEB otherwise (`dir_init`/`has_tokadesk` in
//! `dir.c`). That check is `/T`-only: interactive use never looks at its
//! result, so the workbench itself opens normally wherever it lives. But
//! `/T`'s own assumption -- that TOKADESK.EXE ships at the root -- is wrong
//! now that it ships at `C:\DOS\TOKADESK.EXE` like every other Toka-DOS tool,
//! so the self-test itself always reports 0xEB on the real image. Known
//! defect, recorded in `dev_docs/2026-09-02-tokadesk-ship.md`. This fixture
//! runs TOKADESK from its real shipped path and asserts the CURRENT outcome:
//! VBE mode 0x117 is set (that happens in the stub before the self-check
//! runs), then TOKADESK exits 0xEB without drawing any chrome.
//!
//! The EXE is the committed binary at `crates/izarravm-firmware/roms/dos/tokadesk.exe`
//! (`izarravm_firmware::tokadesk_exe()`), the same bytes that ship at
//! `C:\DOS\TOKADESK.EXE` in `tokados-hdd.img`. `TOKADESK.EXE` is in
//! `DOS_FOLDER_BINARIES` (`katea_tree.rs`), so the overlay below lands at that
//! shipped path, not at the root, matching AUTOEXEC.BAT on the real image.
//! AUTOEXEC is stock-shaped: PATH, LH TOKAMOUS, then the shipped path with `/T`.

use super::*;

#[test]
fn tokadesk_fills_mode_117_and_exits() {
    let scratch = TokaScratch::new("tokadesk");
    let autoexec = concat!(
        "@ECHO OFF\r\n",
        "PATH C:\\DOS\r\n",
        "LH TOKAMOUS\r\n",
        "C:\\DOS\\TOKADESK.EXE /T\r\n"
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
                ("AUTOEXEC.BAT".to_string(), autoexec.as_bytes().to_vec()),
                (
                    "TOKADESK.EXE".to_string(),
                    izarravm_firmware::tokadesk_exe().to_vec(),
                ),
            ],
        )
        .expect("mount Toka-DOS folder");
    let stop = machine
        .run_until_halt_or_cycles(1_800_000_000)
        .expect("run TOKADESK");
    match stop {
        StopReason::TestExit { code } => {
            assert_eq!(
                code,
                0xEB,
                "TOKADESK self-test exited {code}, expected the known \
                 root-listing defect (0xEB); see dev_docs/2026-09-02-tokadesk-ship.md\n{}",
                machine.screen_text().as_text()
            );
        }
        other => panic!(
            "TOKADESK did not exit through the test port (stop={other:?})\n{}",
            machine.screen_text().as_text()
        ),
    }
    // The VBE mode set happens in the 16-bit stub before the payload ever runs
    // dir_init's self-check, so the display switch is real even though the
    // self-check that follows it is not.
    let display = machine
        .margo_display()
        .expect("Margo should own the display after mode 0x117");
    assert_eq!(display.width, 1024, "width {display:?}");
    assert_eq!(display.height, 768, "height {display:?}");
    assert_eq!(display.bpp, 16, "bpp {display:?}");
}
