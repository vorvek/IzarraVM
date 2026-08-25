// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! TOKADESK.EXE smoke: VCPI switch, VBE 0x117, chrome, V86 thunk, Lotura exit.
//!
//! `/T` lists C:\\ via INT 21h 4Eh (fail 0xEB if TOKADESK.EXE is missing),
//! draws the Directory panes, probes INT 21h/10h/33h (0xE7..0xEA), waits
//! one V86 retrace, then CMD_EXIT 0. Hardware cursor stays off so the
//! chrome CRC is not composited.
//!
//! The EXE is the authoring artifact at `toka-dos/tools-src/tokadesk/tokadesk.exe`.
//! PR 1 overlays it at `C:\TOKADESK.EXE` (not yet in `DOS_FOLDER_BINARIES`).
//! AUTOEXEC is stock-shaped: PATH, LH TOKAMOUS, then the overlay path with `/T`.

use super::*;

fn tokadesk_exe() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("toka-dos")
        .join("tools-src")
        .join("tokadesk")
        .join("tokadesk.exe");
    fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "read {}: {error} (build with toka-dos/tools-src/tokadesk/build.ps1)",
            path.display()
        )
    })
}

#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn tokadesk_fills_mode_117_and_exits() {
    let scratch = TokaScratch::new("tokadesk");
    let autoexec = concat!(
        "@ECHO OFF\r\n",
        "PATH C:\\DOS\r\n",
        "LH TOKAMOUS\r\n",
        "C:\\TOKADESK.EXE /T\r\n"
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
                ("TOKADESK.EXE".to_string(), tokadesk_exe()),
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
                0,
                "TOKADESK self-test failed at step {code}\n{}",
                machine.screen_text().as_text()
            );
        }
        other => panic!(
            "TOKADESK did not exit through the test port (stop={other:?})\n{}",
            machine.screen_text().as_text()
        ),
    }
    let display = machine
        .margo_display()
        .expect("Margo should own the display after mode 0x117");
    assert_eq!(display.width, 1024, "width {display:?}");
    assert_eq!(display.height, 768, "height {display:?}");
    assert_eq!(display.bpp, 16, "bpp {display:?}");
    let menu = machine.screen_crc32(0, 0, 1024, 24);
    let well = machine.screen_crc32(128, 24, 896, 500);
    assert_ne!(menu, 0, "menu bar should hash a non-empty rectangle");
    assert_ne!(
        menu, well,
        "menu chrome and the Directory well should not be the same fill"
    );
}
