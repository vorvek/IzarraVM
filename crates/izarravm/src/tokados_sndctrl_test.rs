// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! SNDCTRL.COM end to end: the real guest binary, booted from `C:\DOS` on the
//! committed image, moving the real devices.
//!
//! The host-side contract has its own fast tests (`machine_audio_cmos_test.rs`);
//! what only a boot can prove is that the tool finds the hardware, that its
//! mixer and codec writes land, that the CMOS block it writes is one the host
//! will accept back, and that its rewrite of `AUTOEXEC.BAT` reaches the host
//! folder. Every one of those crosses a boundary a unit test stubs out.

use super::*;

/// Drive the tool from the command line rather than through its full-screen
/// interface: same apply path, but expressible in a batch file, and it exits on
/// its own so the run has a definite end.
fn boot_with_sndctrl(label: &str, switches: &str) -> (Machine, PathBuf) {
    let scratch = TokaScratch::new(label);
    let dir = scratch.path().to_path_buf();
    // Leak the scratch guard: the caller inspects the folder after the machine
    // stops, and the assertions read files the guard would have deleted.
    std::mem::forget(scratch);
    fs::write(
        dir.join("AUTOEXEC.BAT"),
        format!(
            "@ECHO OFF\r\nPROMPT $P$G\r\nPATH C:\\DOS\r\n\
             SET BLASTER=A220 I7 D1 H5 P300 T6\r\n\
             SET AFTER=sentinel\r\n\
             SNDCTRL {switches}\r\n"
        ),
    )
    .expect("write AUTOEXEC");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd_folder(&dir).expect("mount host folder");
    let (stop, _) = run_until_toka_condition(&mut machine, 900_000_000, current_root_prompt);
    if let StopReason::CpuError(message) = &stop {
        panic!(
            "CPU fault running SNDCTRL: {message}\n{}",
            machine.screen_text().as_text()
        );
    }
    // Katea materializes a file on its first completed shape and holds later
    // shapes in the guest write store until an explicit flush. AUTOEXEC.BAT was
    // already on the host before the boot, so the tool's rewrite is exactly a
    // later shape: without this the host file still reads as it did at mount.
    machine.flush_hdd_folder();
    (machine, dir)
}

/// The whole chain in one run: probe, mixer write, codec write, CMOS write with
/// a refreshed checksum, live environment patch, AUTOEXEC rewrite.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndctrl_moves_both_devices_and_records_the_choice() {
    let (machine, dir) = boot_with_sndctrl("sndctrl_apply", "/SBIRQ:5 /WSSIRQ:9 /MPU:330");
    let screen = machine.screen_text().as_text();

    // The devices themselves, read back from the mixer and the codec.
    assert_eq!(
        machine.sound_blaster_routing(),
        Some((5, 1, 5)),
        "the mixer must answer on IRQ 5 after the tool wrote register 0x80\n{screen}"
    );
    assert_eq!(
        machine.wss_routing(),
        Some((9, 0)),
        "the codec must answer on IRQ 9 after the tool wrote its config register\n{screen}"
    );

    // The CMOS block, and -- just as load-bearing -- its checksum: leaving that
    // stale would make the next POST discard the keyboard layout and CPU speed
    // along with the audio bytes.
    let cmos = machine.cmos_bytes();
    assert_eq!(cmos[0x1B], b'R', "magic byte\n{screen}");
    assert_eq!(cmos[0x1C], 5, "SB IRQ in CMOS");
    assert_eq!(cmos[0x1F], 9, "WSS IRQ in CMOS");
    assert_eq!(cmos[0x21], 1, "MPU port selector = 0x330");
    let sum = cmos[0x10..=0x2D]
        .iter()
        .fold(0u16, |acc, byte| acc.wrapping_add(u16::from(*byte)));
    assert_eq!(
        (cmos[0x2E], cmos[0x2F]),
        ((sum >> 8) as u8, sum as u8),
        "the tool must refresh the NVRAM checksum it invalidated\n{screen}"
    );

    // The file on the host, rewritten in place with everything else intact.
    let autoexec = fs::read_to_string(dir.join("AUTOEXEC.BAT")).expect("read AUTOEXEC");
    assert!(
        autoexec.contains("SET BLASTER=A220 I5 D1 H5 P330 T6"),
        "AUTOEXEC BLASTER line not rewritten:\n{autoexec}\n--- screen ---\n{screen}"
    );
    assert!(
        autoexec.contains("SET AFTER=sentinel"),
        "the rest of the file must survive the rewrite:\n{autoexec}"
    );

    // The tool reports what it did, and the summary is what the user sees.
    let lower = screen.to_ascii_lowercase();
    assert!(
        lower.contains("blaster updated in the current environment"),
        "the master environment patch was not reported\n{screen}"
    );
    assert!(
        lower.contains("autoexec.bat updated"),
        "the AUTOEXEC rewrite was not reported\n{screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The collision the menus make unreachable is still reachable from a command
/// line, and it has to be refused before anything is written -- a card with
/// both devices on one line is worse than one that ignored you.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndctrl_refuses_to_put_both_devices_on_one_line() {
    let (machine, dir) = boot_with_sndctrl("sndctrl_clash", "/WSSIRQ:7");
    let screen = machine.screen_text().as_text();
    assert!(
        screen.to_ascii_lowercase().contains("refused"),
        "the collision was not refused\n{screen}"
    );
    assert_eq!(
        machine.wss_routing(),
        Some((11, 0)),
        "nothing may be applied when the line is refused\n{screen}"
    );
    assert_eq!(
        machine.cmos_bytes()[0x1F],
        11,
        "and nothing may be persisted either\n{screen}"
    );
    let autoexec = fs::read_to_string(dir.join("AUTOEXEC.BAT")).expect("read AUTOEXEC");
    assert!(
        autoexec.contains("SET BLASTER=A220 I7 D1 H5 P300 T6"),
        "a refused line must leave AUTOEXEC alone:\n{autoexec}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// `/S` reports without changing anything, and what it reports is what the
/// hardware actually holds -- the tool reads the mixer and config register back
/// rather than trusting the CMOS copy.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndctrl_status_reads_the_hardware_and_changes_nothing() {
    let (machine, dir) = boot_with_sndctrl("sndctrl_status", "/S");
    let screen = machine.screen_text().as_text();
    assert!(
        screen.contains("BLASTER=A220 I7 D1 H5 P300 T6"),
        "the reported assignment does not match the power-on defaults\n{screen}"
    );
    assert!(
        screen.contains("IRQ 11"),
        "the codec's line should be read back from its config register\n{screen}"
    );
    assert!(
        !screen
            .to_ascii_lowercase()
            .contains("applied to the hardware"),
        "/S must not apply anything\n{screen}"
    );
    assert_eq!(machine.sound_blaster_routing(), Some((7, 1, 5)));
    let _ = fs::remove_dir_all(&dir);
}

/// `/B /T`: exactly two rows, matched as whole lines (not substrings) since
/// the kernel's own boot banner uses the same 0xC3/0xC4 bytes decoratively
/// ("ÃÄ> Kernel compatibility: ...") -- a substring check for the tree prefix
/// would pass even if SNDCTRL never emitted it. Row 1 is the tree-styled
/// heading prefix (bytes 0xC3 0xC4 '>' ' ', which `screen_text` renders as
/// U+00C3 U+00C4 '>' ' '); row 2 is the gutter byte 0xB3 (U+00B3) plus the
/// 5-space indent, then the BLASTER-style device summary exactly as
/// specified. Also pins the read-only property this design promises: the
/// hardware routing the tool itself reports is unchanged after the call, the
/// same way `/S` changes nothing.
///
/// Like every fixture in this file, `boot_with_sndctrl` serves SNDCTRL.COM out
/// of the *committed* `tokados-hdd.img` (`mount_hdd_folder` -> Katea's system
/// payload), not the freshly assembled `sndctrl.com` sitting next to the
/// source -- this test needs the committed image's SNDCTRL to already
/// support `/B`, permanently, since that is the only binary it boots.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndctrl_boot_summary_tree_prints_two_rows_and_changes_nothing() {
    let (machine, dir) = boot_with_sndctrl("sndctrl_boot_tree", "/B /T");
    let screen = machine.screen_text().as_text();
    let heading = "\u{00C3}\u{00C4}> ReSonique II Configuration [Run SNDCTRL to change]";
    let summary = "\u{00B3}     SB16 220 I7 D1 H5   WSS 530 I11 D0   MIDI 300 I9";
    assert!(
        screen.lines().any(|line| line == heading),
        "row 1 must be exactly the tree prefix (0xC3 0xC4 '>' ' ') followed by \
         the heading, and nothing else\n{screen}"
    );
    assert!(
        screen.lines().any(|line| line == summary),
        "row 2 must be exactly the gutter (0xB3), the 5-space indent, then the \
         BLASTER-style summary, and nothing else\n{screen}"
    );
    assert_eq!(
        machine.sound_blaster_routing(),
        Some((7, 1, 5)),
        "/B must not move the Sound Blaster, same guarantee as /S\n{screen}"
    );
    assert_eq!(
        machine.wss_routing(),
        Some((11, 0)),
        "/B must not move the codec either\n{screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Plain `/B` (no `/T`): the same two rows, but with no tree glyphs at all --
/// the prefix and gutter are opt-in, not default. Matched as whole lines, for
/// the same reason as the /T case: the kernel's own boot banner already
/// contains 0xC3/0xC4 bytes elsewhere on screen, so only an exact-line match
/// (no prefix at all on the heading line, no gutter before the indent) proves
/// SNDCTRL itself withheld them.
///
/// Same committed-image note as the /T case above: this needs the committed
/// image's SNDCTRL to already support `/B`, permanently.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndctrl_boot_summary_plain_has_no_tree_glyphs() {
    let (machine, dir) = boot_with_sndctrl("sndctrl_boot_plain", "/B");
    let screen = machine.screen_text().as_text();
    let heading = "ReSonique II Configuration [Run SNDCTRL to change]";
    let summary = "     SB16 220 I7 D1 H5   WSS 530 I11 D0   MIDI 300 I9";
    assert!(
        screen.lines().any(|line| line == heading),
        "row 1 must be exactly the heading with no tree prefix\n{screen}"
    );
    assert!(
        screen.lines().any(|line| line == summary),
        "row 2 must be exactly the 5-space indent then the summary, with no \
         leading gutter byte\n{screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}
