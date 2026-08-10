// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! SNDMIXER.COM end to end: the real guest binary, on a booted machine, moving
//! the real mixer registers.
//!
//! The fader law and the register decode both have host-side unit tests
//! (`mixer_test.rs`, `machine_audio_test.rs`). What only a boot can prove is
//! the part in between: that the tool finds the card, that the level it writes
//! is the level the register ends up holding, that `/CFG` at boot actually
//! reaches the hardware rather than only reading a file, that `/S` really is
//! silent, and that a key pressed on the full-screen fader moves a register.
//!
//! Unlike the SNDCTRL fixtures next door, these boot the tool from the SCRATCH
//! FOLDER rather than out of the committed image, by copying
//! `izarravm_firmware::sndmixer_com()` in beside `AUTOEXEC.BAT`. That is the
//! same bytes the image is built from (`katea_volume_test` pins the two equal),
//! and it means a change to `sndmixer.asm` is tested by the very next `cargo
//! test` instead of only after an image rebuild.

use super::*;

/// Levels, as this tool's fader law defines them: `-4 dB` per step, so step
/// `s >= 1` is level `11 + 2s` and the register byte is that level shifted into
/// D7-D3. Step 0 is the register's hard mute.
fn step_byte(step: u8) -> u8 {
    if step == 0 { 0 } else { (11 + 2 * step) << 3 }
}

/// Put SNDMIXER.COM and an AUTOEXEC in a scratch folder and boot it. Returns
/// the machine and the folder, which the caller inspects and then removes.
fn boot_with_sndmixer(label: &str, commands: &str) -> (Machine, PathBuf) {
    boot_with_sndmixer_files(label, commands, &[])
}

/// The same, with extra files staged in the folder first (a saved config, for
/// the restore path).
fn boot_with_sndmixer_files(
    label: &str,
    commands: &str,
    files: &[(&str, &str)],
) -> (Machine, PathBuf) {
    let scratch = TokaScratch::new(label);
    let dir = scratch.path().to_path_buf();
    // Leak the guard: the caller reads files back after the machine stops.
    std::mem::forget(scratch);
    fs::write(dir.join("SNDMIXER.COM"), izarravm_firmware::sndmixer_com())
        .expect("stage SNDMIXER.COM");
    for (name, body) in files {
        fs::write(dir.join(name), body.as_bytes()).expect("stage fixture file");
    }
    fs::write(
        dir.join("AUTOEXEC.BAT"),
        format!("@ECHO OFF\r\nPROMPT $P$G\r\nPATH C:\\DOS\r\n{commands}"),
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
            "CPU fault running SNDMIXER: {message}\n{}",
            machine.screen_text().as_text()
        );
    }
    machine.flush_hdd_folder();
    (machine, dir)
}

/// Boot into the full-screen mixer and stop on it. The condition is the mixer's
/// own title rather than a cycle count: a fixture that injected keys into a boot
/// screen would "pass" by leaving every register untouched.
fn open_the_mixer(label: &str, commands: &str) -> (Machine, PathBuf) {
    let scratch = TokaScratch::new(label);
    let dir = scratch.path().to_path_buf();
    std::mem::forget(scratch);
    fs::write(dir.join("SNDMIXER.COM"), izarravm_firmware::sndmixer_com())
        .expect("stage SNDMIXER.COM");
    fs::write(
        dir.join("AUTOEXEC.BAT"),
        format!("@ECHO OFF\r\nPROMPT $P$G\r\n{commands}"),
    )
    .expect("write AUTOEXEC");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd_folder(&dir).expect("mount host folder");
    let (stop, _) = run_until_toka_condition(&mut machine, 900_000_000, |machine| {
        machine
            .screen_text()
            .as_text()
            .contains("ReSonique 2 Volume Mixer")
    });
    if let StopReason::CpuError(message) = &stop {
        panic!(
            "CPU fault opening the mixer: {message}\n{}",
            machine.screen_text().as_text()
        );
    }
    (machine, dir)
}

/// Deliver scancodes one at a time. A batched injection is handed over faster
/// than the guest's `INT 16h` loop consumes it, so each code gets its own slice.
fn press(machine: &mut Machine, codes: &[u8]) {
    for code in codes {
        machine.inject_key_scancodes(&[*code]);
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("keystroke");
    }
}

/// Every CT1745 level register the tool owns, at its power-on value, and the
/// PC-speaker register at the position the card powers on in. This is the
/// baseline every other fixture here is a departure from, so if the card's
/// power-on state ever moves, this is what says so first.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_lists_the_levels_the_card_powers_on_with() {
    let (machine, dir) = boot_with_sndmixer("sndmixer_list", "SNDMIXER /L\r\n");
    let screen = machine.screen_text().as_text();
    for row in [
        "MASTER      10   0 dB",
        "FMSYNTH     10   0 dB",
        "WAVE        10   0 dB",
        "CD-ROM      10   0 dB",
        "MIDI        10   0 dB",
        // Two bits, and the card powers on at position 2 of 4. The listing
        // reports the step that position IS, not a step it rounded from.
        "SPEAKER      7   -7 dB",
        // Two bits as well, and the card powers on at position 0 -- which on
        // this fader is 0 dB and not a mute, because it is the card's output
        // amplifier and 0 dB is an amplifier passing its input through.
        "AMP          0   0 dB",
    ] {
        assert!(
            screen
                .lines()
                .any(|line| line.trim_end() == format!("  {row}")),
            "the listing must carry the row {row:?}\n{screen}"
        );
    }
    // Nothing was asked for, so nothing was applied: /L is a read.
    assert!(
        !screen.contains("Applied to the mixer"),
        "/L must not write the hardware\n{screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The whole point of the tool: a level named on the command line reaches the
/// register. Read back through the mixer's own register file, not through the
/// tool's report -- a tool that printed the right number and wrote nothing
/// would pass a screen-scrape and fail this.
///
/// `/P 2` is the PC speaker's snap-up rule in the same run: two bits give four
/// stops and 2 is not one of them, so the request lands on the next stop up
/// (step 3, register position 1) rather than on the nearer stop below, which
/// is silence.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_command_line_levels_reach_the_mixer_registers() {
    let (machine, dir) =
        boot_with_sndmixer("sndmixer_cli", "SNDMIXER /M 5 /F 7 /W 8 /C 0 /I 3 /P 2\r\n");
    let screen = machine.screen_text().as_text();
    for (index, step, what) in [
        (0x30u8, 5u8, "master left"),
        (0x31, 5, "master right"),
        (0x34, 7, "FM left"),
        (0x35, 7, "FM right"),
        (0x32, 8, "voice left"),
        (0x33, 8, "voice right"),
        (0x36, 0, "CD left"),
        (0x37, 0, "CD right"),
        (0x50, 3, "wavetable left"),
        (0x51, 3, "wavetable right"),
    ] {
        assert_eq!(
            machine.sb_mixer_register(index),
            Some(step_byte(step)),
            "{what} ({index:#04x}) must hold step {step}\n{screen}"
        );
    }
    // The speaker's own register: position 1 in D7-D6, from a request of 2.
    assert_eq!(
        machine.sb_mixer_register(0x3B),
        Some(1 << 6),
        "/P 2 must snap UP to the next hardware stop, not down into silence\n{screen}"
    );
    // WAVE drives the codec too, at the count nearest the same dB figure
    // (step 8 is -8 dB; the codec moves in 1.5 dB, so 5 counts).
    assert_eq!(machine.wss_register(6), Some(5), "AD1848 I6\n{screen}");
    assert_eq!(machine.wss_register(7), Some(5), "AD1848 I7\n{screen}");
    let _ = fs::remove_dir_all(&dir);
}

/// The boot line the image's AUTOEXEC carries: restore from a file, silently.
///
/// Both halves are load-bearing and both are checked here, because they fail
/// independently -- a tool that restored correctly but printed a line would
/// break the boot screen's row budget, and a tool that printed nothing but
/// restored nothing would look identical from the screen alone.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_cfg_restore_writes_the_registers_at_boot_and_says_nothing() {
    let saved = "; saved levels\r\nMASTER=6\r\nFMSYNTH=2\r\nCD=0\r\nSPEAKER=10\r\n";
    let (machine, dir) = boot_with_sndmixer_files(
        "sndmixer_restore",
        "SNDMIXER /CFG C:\\VOL.CFG /S\r\nECHO MIXER-DONE\r\n",
        &[("VOL.CFG", saved)],
    );
    let screen = machine.screen_text().as_text();

    assert_eq!(
        machine.sb_mixer_register(0x30),
        Some(step_byte(6)),
        "{screen}"
    );
    assert_eq!(
        machine.sb_mixer_register(0x34),
        Some(step_byte(2)),
        "{screen}"
    );
    assert_eq!(
        machine.sb_mixer_register(0x36),
        Some(step_byte(0)),
        "{screen}"
    );
    assert_eq!(machine.sb_mixer_register(0x3B), Some(3 << 6), "{screen}");
    // A channel the file does not name keeps what the card was holding, rather
    // than being reset to the tool's own idea of a default.
    assert_eq!(
        machine.sb_mixer_register(0x32),
        Some(step_byte(10)),
        "WAVE was not in the file and must not have moved\n{screen}"
    );

    // Silence: the run reached the ECHO after it, so the tool ran; and it put
    // nothing of its own on the screen.
    assert!(
        screen.contains("MIXER-DONE"),
        "the batch must have got past SNDMIXER\n{screen}"
    );
    for chatter in [
        "ReSonique 2 volume levels",
        "Volume levels restored",
        "Applied to the mixer",
    ] {
        assert!(
            !screen.contains(chatter),
            "/S must print nothing, found {chatter:?}\n{screen}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

/// The mutation proof for the silence assertion above: the SAME restore
/// without `/S` does print, so "found no output" is a property of the flag and
/// not of a fixture that never ran the tool.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_cfg_restore_without_the_silent_flag_reports_itself() {
    let saved = "MASTER=6\r\n";
    let (machine, dir) = boot_with_sndmixer_files(
        "sndmixer_restore_loud",
        "SNDMIXER /CFG C:\\VOL.CFG\r\n",
        &[("VOL.CFG", saved)],
    );
    let screen = machine.screen_text().as_text();
    assert!(
        screen.contains("Volume levels restored from C:\\VOL.CFG"),
        "without /S the restore names the file it read\n{screen}"
    );
    assert_eq!(
        machine.sb_mixer_register(0x30),
        Some(step_byte(6)),
        "{screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// A missing config file is the state every machine is in before the first
/// save, and the AUTOEXEC line runs anyway: it must leave the card alone and
/// exit 0 rather than fail the boot or reset the levels.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_cfg_restore_of_a_missing_file_leaves_the_card_alone() {
    let (machine, dir) = boot_with_sndmixer(
        "sndmixer_no_cfg",
        "SNDMIXER /M 4\r\nSNDMIXER /CFG C:\\NOSUCH.CFG /S\r\nECHO MIXER-DONE\r\n",
    );
    let screen = machine.screen_text().as_text();
    assert!(screen.contains("MIXER-DONE"), "{screen}");
    assert_eq!(
        machine.sb_mixer_register(0x30),
        Some(step_byte(4)),
        "a missing file must not disturb the level already set\n{screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Save then restore, through the file, across two invocations: what F10 and
/// the AUTOEXEC line do between them. The second run starts from the card's
/// power-on levels and has to end on the saved ones.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_saves_a_config_the_next_run_can_restore() {
    let (machine, dir) = boot_with_sndmixer(
        "sndmixer_roundtrip",
        "SNDMIXER /M 3 /F 9 /CFG C:\\VOL.CFG\r\n\
         SNDMIXER /M 10 /F 10\r\n\
         SNDMIXER /CFG C:\\VOL.CFG /S\r\n",
    );
    let screen = machine.screen_text().as_text();
    let written = fs::read_to_string(dir.join("VOL.CFG")).expect("read the saved config");
    assert!(
        written.contains("MASTER=3") && written.contains("FMSYNTH=9"),
        "the saved file must carry the levels that were set:\n{written}"
    );
    assert!(
        written.starts_with(';'),
        "the file leads with the comment that explains it:\n{written}"
    );
    // The middle command put both back to 10; the restore has to move them
    // again, so this cannot pass on a run where the restore did nothing.
    assert_eq!(
        machine.sb_mixer_register(0x30),
        Some(step_byte(3)),
        "{screen}"
    );
    assert_eq!(
        machine.sb_mixer_register(0x34),
        Some(step_byte(9)),
        "{screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The full-screen fader, driven by keys: select FMSYNTH, take it down two
/// steps, save with F10. Proves the interactive path writes the same register
/// the command line does, and that F10 persists to the `/CFG` file.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_full_screen_fader_keys_move_the_register_and_f10_saves() {
    let scratch = TokaScratch::new("sndmixer_keys");
    let dir = scratch.path().to_path_buf();
    std::mem::forget(scratch);
    fs::write(dir.join("SNDMIXER.COM"), izarravm_firmware::sndmixer_com())
        .expect("stage SNDMIXER.COM");
    // F10 with no `/CFG` saves to `C:\VOLCONF.CFG`, the path the image's
    // AUTOEXEC restores from. Deliberately NOT staging a host-side `DOS`
    // directory here: the default has to land somewhere that exists on a bare
    // host-folder mount, which is the GUI's default drive and need carry
    // nothing but the game the user dropped in it. The root always exists;
    // `C:\DOS` does not, and a create into a directory that is not there is
    // the failure this path is written to avoid.
    fs::write(
        dir.join("AUTOEXEC.BAT"),
        "@ECHO OFF\r\nPROMPT $P$G\r\nSNDMIXER\r\n",
    )
    .expect("write AUTOEXEC");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd_folder(&dir).expect("mount host folder");

    // No switches at all is the full-screen interface. Wait for the interface
    // itself rather than for a cycle count: a fixture that injected keys into
    // a boot screen would "pass" by leaving every register untouched.
    let (stop, _) = run_until_toka_condition(&mut machine, 900_000_000, |machine| {
        machine
            .screen_text()
            .as_text()
            .contains("ReSonique 2 Volume Mixer")
    });
    if let StopReason::CpuError(message) = &stop {
        panic!(
            "CPU fault opening the mixer: {message}\n{}",
            machine.screen_text().as_text()
        );
    }
    let opened = machine.screen_text().as_text();
    assert!(
        opened.contains("ReSonique 2 Volume Mixer"),
        "the full-screen mixer never opened\n{opened}"
    );
    assert!(
        opened.contains("MASTER") && opened.contains("SPEAKER") && opened.contains("AMP"),
        "all seven faders must be on screen\n{opened}"
    );

    // Right moves the selection off MASTER and onto FMSYNTH; Down twice takes
    // it from 10 to 8. One scancode per run slice -- a batched injection is
    // delivered faster than the guest's INT 16h loop consumes it.
    // 0x4D Right, 0x50 Down, 0x44 F10, each make then break.
    for code in [0x4Du8, 0xCD, 0x50, 0xD0, 0x50, 0xD0] {
        machine.inject_key_scancodes(&[code]);
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("fader keystroke");
    }
    // Live application: the register has already moved, before F10.
    assert_eq!(
        machine.sb_mixer_register(0x34),
        Some(step_byte(8)),
        "two Downs on FMSYNTH must reach register 0x34 as they are pressed\n{}",
        machine.screen_text().as_text()
    );
    assert_eq!(
        machine.sb_mixer_register(0x30),
        Some(step_byte(10)),
        "MASTER was moved off, not moved down\n{}",
        machine.screen_text().as_text()
    );

    for code in [0x44u8, 0xC4] {
        machine.inject_key_scancodes(&[code]);
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("F10 keystroke");
    }
    run_until_toka_condition(&mut machine, 200_000_000, current_root_prompt);
    machine.flush_hdd_folder();

    let screen = machine.screen_text().as_text();
    assert!(
        screen.contains("Saved in C:\\VOLCONF.CFG"),
        "with no /CFG, F10 saves to the path the image's AUTOEXEC restores from\n{screen}"
    );
    let written = fs::read_to_string(dir.join("VOLCONF.CFG")).expect("read the saved config");
    assert!(
        written.contains("FMSYNTH=8"),
        "the saved file must carry the level the fader was left on:\n{written}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Esc is a real undo, not a no-op.
///
/// Because levels are applied as the fader moves -- a mixer you cannot hear
/// while you set it is not a mixer -- "cancel" has to put the hardware back
/// rather than merely decline to write it. The run below moves a fader, leaves
/// with Esc, and the register has to read what it read before the mixer opened.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_escape_restores_the_levels_the_mixer_opened_on() {
    let scratch = TokaScratch::new("sndmixer_escape");
    let dir = scratch.path().to_path_buf();
    std::mem::forget(scratch);
    fs::write(dir.join("SNDMIXER.COM"), izarravm_firmware::sndmixer_com())
        .expect("stage SNDMIXER.COM");
    fs::write(
        dir.join("AUTOEXEC.BAT"),
        "@ECHO OFF\r\nPROMPT $P$G\r\nSNDMIXER /M 4\r\nSNDMIXER\r\n",
    )
    .expect("write AUTOEXEC");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd_folder(&dir).expect("mount host folder");
    let (stop, _) = run_until_toka_condition(&mut machine, 900_000_000, |machine| {
        machine
            .screen_text()
            .as_text()
            .contains("ReSonique 2 Volume Mixer")
    });
    if let StopReason::CpuError(message) = &stop {
        panic!(
            "CPU fault opening the mixer: {message}\n{}",
            machine.screen_text().as_text()
        );
    }
    assert_eq!(
        machine.sb_mixer_register(0x30),
        Some(step_byte(4)),
        "the mixer opens on the level the previous command set"
    );

    // Two Downs on MASTER (the fader the screen opens on), then Esc.
    for code in [0x50u8, 0xD0, 0x50, 0xD0] {
        machine.inject_key_scancodes(&[code]);
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("fader keystroke");
    }
    assert_eq!(
        machine.sb_mixer_register(0x30),
        Some(step_byte(2)),
        "the moves must have reached the hardware, or Esc has nothing to undo"
    );

    for code in [0x01u8, 0x81] {
        machine.inject_key_scancodes(&[code]);
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("escape keystroke");
    }
    run_until_toka_condition(&mut machine, 200_000_000, current_root_prompt);
    let screen = machine.screen_text().as_text();
    assert!(
        screen.contains("Cancelled"),
        "Esc reports that it put the levels back\n{screen}"
    );
    assert_eq!(
        machine.sb_mixer_register(0x30),
        Some(step_byte(4)),
        "Esc must restore the level the mixer opened on\n{screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The button row: Tab reaches it, Enter presses Accept, and Accept leaves with
/// the levels standing and says so.
///
/// The fixture moves a fader FIRST and then accepts, so "the levels are still
/// applied" is a claim about a level this run set rather than about one that was
/// never touched: a tool whose Accept quietly ran the Esc path would pass a
/// screen-scrape for the message and fail the register.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_accept_button_leaves_the_levels_applied() {
    let (mut machine, dir) = open_the_mixer("sndmixer_accept", "SNDMIXER /M 4\r\nSNDMIXER\r\n");
    // Two Downs on MASTER (the fader the screen opens on): 4 to 2.
    press(&mut machine, &[0x50, 0xD0, 0x50, 0xD0]);
    assert_eq!(
        machine.sb_mixer_register(0x30),
        Some(step_byte(2)),
        "the moves must have reached the hardware, or Accept has nothing to keep"
    );

    // Seven Tabs walk the selection off AMP (the seventh and last fader) and
    // onto the Accept button; the buttons are the last two stops on the ring.
    let opened = machine.screen_text().as_text();
    assert!(
        opened.contains("Accept") && opened.contains("Cancel"),
        "both buttons are on screen from the moment the mixer opens\n{opened}"
    );
    for _ in 0..7 {
        press(&mut machine, &[0x0F, 0x8F]);
    }
    let focused = machine.screen_text().as_text();
    assert!(
        focused.contains("ACCEPT    leave with these levels in effect"),
        "the selected button describes itself where a fader's description goes\n{focused}"
    );

    press(&mut machine, &[0x1C, 0x9C]); // Enter
    run_until_toka_condition(&mut machine, 200_000_000, current_root_prompt);
    let screen = machine.screen_text().as_text();
    assert!(
        screen.contains("Settings applied."),
        "Accept closes with its own message\n{screen}"
    );
    assert_eq!(
        machine.sb_mixer_register(0x30),
        Some(step_byte(2)),
        "Accept keeps the levels the run set\n{screen}"
    );
    assert!(
        !screen.contains("Cancelled"),
        "Accept is not the cancel path\n{screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The Cancel button is the Esc path with a face on it: the same restore, the
/// same message, reached with Tab and a keypress instead.
///
/// Pressed with SPACE, not Enter. Both keys press a button -- that is the
/// sibling tool's model for an input, and the `/?` text and the manuals say so
/// -- and a fixture that only ever pressed Enter would let the Space arm rot.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_cancel_button_restores_the_levels_the_mixer_opened_on() {
    let (mut machine, dir) = open_the_mixer("sndmixer_cancel", "SNDMIXER /M 4\r\nSNDMIXER\r\n");
    press(&mut machine, &[0x50, 0xD0, 0x50, 0xD0]);
    assert_eq!(
        machine.sb_mixer_register(0x30),
        Some(step_byte(2)),
        "the moves must have reached the hardware, or Cancel has nothing to undo"
    );

    // Eight Tabs: seven faders, then Accept, then Cancel.
    for _ in 0..8 {
        press(&mut machine, &[0x0F, 0x8F]);
    }
    let focused = machine.screen_text().as_text();
    assert!(
        focused.contains("CANCEL    leave and put the previous levels back"),
        "Cancel says what it will do before it is pressed\n{focused}"
    );

    press(&mut machine, &[0x39, 0xB9]); // Space
    run_until_toka_condition(&mut machine, 200_000_000, current_root_prompt);
    let screen = machine.screen_text().as_text();
    assert!(
        screen.contains("Cancelled. Previous levels restored."),
        "the button reports the same thing Esc does\n{screen}"
    );
    assert_eq!(
        machine.sb_mixer_register(0x30),
        Some(step_byte(4)),
        "Cancel restores the level the mixer opened on\n{screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// A key that moves a level does nothing while a button holds the selection.
///
/// This is the failure the button ring makes possible: the two extra stops have
/// no channel record behind them, so a level key that skipped the check would
/// write through whatever SI was last left pointing at -- which is a string in
/// the data area, not a channel. The whole screen and the whole mixer register
/// file are compared, not a chosen few of each, because the address that stray
/// write lands on depends on which routine painted last: with the check removed
/// it reads the bytes just past the channel table as a record, and the register
/// index it finds there is `0x41`.
///
/// That is why the oracle is "NOTHING moved" and not "`0x41`/`0x42` are still
/// zero", which is what it used to be. Those two registers are the card's
/// output gain, and since the AMP fader they are a pair this tool owns and
/// writes on purpose -- so their staying at zero is no longer evidence of
/// anything, and a fixture that asserted it would pass on a build where the
/// stray write had simply moved one record further along. The companion
/// fixture below is the other half: the same keys, with a FADER selected, must
/// move those very registers.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_level_keys_do_nothing_while_a_button_is_selected() {
    let (mut machine, dir) = open_the_mixer("sndmixer_btn_keys", "SNDMIXER /M 4\r\nSNDMIXER\r\n");
    // Seven Tabs: past all seven faders and onto Accept.
    for _ in 0..7 {
        press(&mut machine, &[0x0F, 0x8F]);
    }
    let before_screen = machine.screen_text().as_text();
    assert!(
        before_screen.contains("ACCEPT    leave with these levels in effect"),
        "the fixture must start with a BUTTON selected, or it proves nothing\n{before_screen}"
    );
    let before: Vec<Option<u8>> = (0u8..=0xFF)
        .map(|index| machine.sb_mixer_register(index))
        .collect();

    // Up, Down, Home, End, and the digit 9: every key that sets a level.
    for code in [0x48u8, 0x50, 0x47, 0x4F, 0x0A] {
        press(&mut machine, &[code, code | 0x80]);
    }
    let after_screen = machine.screen_text().as_text();
    let after: Vec<Option<u8>> = (0u8..=0xFF)
        .map(|index| machine.sb_mixer_register(index))
        .collect();
    assert_eq!(
        before, after,
        "a level key pressed on a button must move no mixer register\n{after_screen}"
    );
    assert_eq!(
        before_screen, after_screen,
        "and must leave the screen exactly as it found it"
    );
    assert!(
        after_screen.contains("ACCEPT    leave with these levels in effect"),
        "the selection is still on the button it started on\n{after_screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The other half of the guard above: the same keys, on the last FADER, DO
/// move the registers the stray write used to land on.
///
/// Without this, "no register moved" could be satisfied by a tool that had
/// stopped writing anything at all, and the guard would be measuring a dead
/// path. The keys and the register pair are deliberately the same ones; only
/// the selection differs, which is the single variable the guard is about.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_level_keys_on_the_amp_fader_move_the_output_gain_pair() {
    let (mut machine, dir) = open_the_mixer("sndmixer_amp_keys", "SNDMIXER /M 4\r\nSNDMIXER\r\n");
    // Six Tabs: MASTER through SPEAKER, landing on AMP, one short of Accept.
    for _ in 0..6 {
        press(&mut machine, &[0x0F, 0x8F]);
    }
    let focused = machine.screen_text().as_text();
    assert!(
        focused.contains("AMP       card output gain"),
        "six Tabs must land on the AMP fader, not on a button\n{focused}"
    );
    assert_eq!(
        machine.sb_mixer_register(0x41),
        Some(0),
        "the card powers on at 0 dB of output gain\n{focused}"
    );

    // Home is the same key the guard fixture pressed on the button.
    press(&mut machine, &[0x47, 0xC7]);
    let screen = machine.screen_text().as_text();
    assert_eq!(
        machine.sb_mixer_register(0x41),
        Some(3 << 6),
        "Home on AMP must take the left half of the pair to +18 dB\n{screen}"
    );
    assert_eq!(
        machine.sb_mixer_register(0x42),
        Some(3 << 6),
        "and the right half with it: one fader writes both, or it is a balance\n{screen}"
    );

    // Down walks one hardware POSITION, not one step: +18 dB to +12 dB.
    press(&mut machine, &[0x50, 0xD0]);
    let screen = machine.screen_text().as_text();
    for index in [0x41u8, 0x42] {
        assert_eq!(
            machine.sb_mixer_register(index),
            Some(2 << 6),
            "Down moves AMP one position, and moves both halves ({index:#04x})\n{screen}"
        );
    }
    assert!(
        screen.contains("+12 dB"),
        "the info line reads the gain out of the ladder that runs UPWARDS\n{screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The PC speaker set to step 3 in the full-screen mixer has to SAVE as step 3.
///
/// The owner's report: the fader was moved to 3, F10 written, and the file came
/// back saying 7. Both halves are asserted, and they fail independently -- a
/// tool that wrote the right register and the wrong number, or the wrong
/// register and the right number, are different bugs.
///
/// The run opens the fader on 10 (the command before it puts the register at
/// position 3) rather than on the card's power-on 7, because 7 is the number the
/// bug produces: a fixture that opened on 7 could not tell "saved what it was
/// set to" from "saved what it opened on".
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_speaker_set_to_three_saves_as_three() {
    let scratch = TokaScratch::new("sndmixer_spk3");
    let dir = scratch.path().to_path_buf();
    std::mem::forget(scratch);
    fs::write(dir.join("SNDMIXER.COM"), izarravm_firmware::sndmixer_com())
        .expect("stage SNDMIXER.COM");
    fs::write(
        dir.join("AUTOEXEC.BAT"),
        "@ECHO OFF\r\nPROMPT $P$G\r\nSNDMIXER /P 10\r\nSNDMIXER\r\n",
    )
    .expect("write AUTOEXEC");

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd_folder(&dir).expect("mount host folder");
    let (stop, _) = run_until_toka_condition(&mut machine, 900_000_000, |machine| {
        machine
            .screen_text()
            .as_text()
            .contains("ReSonique 2 Volume Mixer")
    });
    if let StopReason::CpuError(message) = &stop {
        panic!(
            "CPU fault opening the mixer: {message}\n{}",
            machine.screen_text().as_text()
        );
    }
    assert_eq!(
        machine.sb_mixer_register(0x3B),
        Some(3 << 6),
        "the mixer opens on the position the previous command set"
    );

    // Five Rights walk the selection from MASTER to SPEAKER, then the digit 3
    // asks for step 3 directly. 0x4D Right, 0x04 the '3' key; make then break.
    let mut codes = vec![];
    for _ in 0..5 {
        codes.push(0x4Du8);
        codes.push(0xCD);
    }
    codes.push(0x04);
    codes.push(0x84);
    for code in codes {
        machine.inject_key_scancodes(&[code]);
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("fader keystroke");
    }
    let screen = machine.screen_text().as_text();
    assert_eq!(
        machine.sb_mixer_register(0x3B),
        Some(1 << 6),
        "step 3 is the speaker's hardware position 1 (-14 dB)\n{screen}"
    );

    for code in [0x44u8, 0xC4] {
        machine.inject_key_scancodes(&[code]);
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("F10 keystroke");
    }
    run_until_toka_condition(&mut machine, 200_000_000, current_root_prompt);
    machine.flush_hdd_folder();

    let written = fs::read_to_string(dir.join("VOLCONF.CFG")).expect("read the saved config");
    assert!(
        written.contains("SPEAKER=3"),
        "the fader was left on step 3 and the file has to say so:\n{written}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The speaker's 2-bit READ-BACK, which is the only place the position-to-step
/// table is consulted: position 1 is step 3.
///
/// This is the leg the two fixtures above cannot reach. Both of them name the
/// step they want, so the number that reaches the file comes from the request
/// and the decode table is never read; transposing that table leaves them green.
/// Here the second command names a DIFFERENT channel, so the SPEAKER line it
/// writes can only have come from reading `0x3B` back -- which is exactly the
/// shape that would turn a speaker set to 3 into a file that says 7.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_speaker_read_back_off_the_register_is_the_step_that_was_set() {
    let (machine, dir) = boot_with_sndmixer(
        "sndmixer_spk_readback",
        "SNDMIXER /P 3 /S\r\n\
         SNDMIXER /M 8 /CFG C:\\VOL.CFG /S\r\n\
         SNDMIXER /L\r\n",
    );
    let screen = machine.screen_text().as_text();
    assert_eq!(
        machine.sb_mixer_register(0x3B),
        Some(1 << 6),
        "the register still holds position 1, so the read-back has 1 to decode\n{screen}"
    );
    let written = fs::read_to_string(dir.join("VOL.CFG")).expect("read the saved config");
    assert!(
        written.contains("SPEAKER=3"),
        "the SPEAKER line was composed from the register, and position 1 is \
         step 3:\n{written}"
    );
    // The same decode on the way to the screen, with the dB figure the position
    // really costs beside it.
    assert!(
        screen
            .lines()
            .any(|line| line.trim_end() == "  SPEAKER      3   -14 dB"),
        "and the listing reads it back the same way\n{screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The same shape on the command line: `/P 3` then a save, and the round trip
/// back through the file. A step the file carries has to survive being restored
/// to the register and read back off it -- position 1 reads as step 3, so a
/// second save of an untouched machine writes the same number the first did.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_speaker_step_three_survives_the_cli_save_and_restore_round_trip() {
    let (machine, dir) = boot_with_sndmixer(
        "sndmixer_spk3_cli",
        "SNDMIXER /P 3 /CFG C:\\VOL.CFG /S\r\n\
         SNDMIXER /P 10 /S\r\n\
         SNDMIXER /CFG C:\\VOL.CFG /S\r\n\
         SNDMIXER /M 8 /CFG C:\\VOL2.CFG /S\r\n\
         ECHO MIXER-DONE\r\n",
    );
    let screen = machine.screen_text().as_text();
    assert!(screen.contains("MIXER-DONE"), "{screen}");
    let first = fs::read_to_string(dir.join("VOL.CFG")).expect("read the saved config");
    assert!(
        first.contains("SPEAKER=3"),
        "/P 3 has to save as step 3:\n{first}"
    );
    // The middle command put the register at position 3, so the restore has to
    // move it back down: this cannot pass on a run where the restore did nothing.
    assert_eq!(
        machine.sb_mixer_register(0x3B),
        Some(1 << 6),
        "step 3 restores to hardware position 1\n{screen}"
    );
    // And a save composed after that restore says the same thing again. The
    // command that writes it names MASTER, not the speaker, so its SPEAKER line
    // comes from reading `0x3B` back rather than from a request.
    let second = fs::read_to_string(dir.join("VOL2.CFG")).expect("read the second config");
    assert!(
        second.contains("SPEAKER=3"),
        "the round trip must not drift the step:\n{second}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// `/?` still fits on the screen it is printed to.
///
/// DOS does not page this text and the tool does not either, so a usage screen
/// one line too long scrolls its own first line away -- which is exactly what
/// adding the seventh switch and AMP's explanation did before the prose below
/// them was cut to pay for it. Both ends are asserted: the title, which is what
/// scrolls off first, and the last line, which is what proves the text was
/// printed whole rather than truncated somewhere in the middle.
///
/// The screen holds 24 lines of it, measured by adding filler lines here until
/// this fails: 24 passes and 25 does not. The text is 22, so there are two
/// spare. That margin is why this fixture is worth having rather than obvious
/// -- the overflowing version was 25 and looked fine in the source.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_usage_text_fits_an_eighty_by_twentyfive_screen() {
    let (machine, dir) = boot_with_sndmixer("sndmixer_usage", "SNDMIXER /?\r\n");
    let screen = machine.screen_text().as_text();
    assert!(
        screen.contains("SNDMIXER - ReSonique 2 Volume Mixer"),
        "the title must not have scrolled off the top\n{screen}"
    );
    assert!(
        screen.contains("selected one. Esc and Cancel restore them. F10 saves."),
        "and the last line must be on it too\n{screen}"
    );
    assert!(
        screen.contains("SNDMIXER /A n"),
        "with the switch this slice added among them\n{screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// `/A` on the command line, over all four of the gain register's positions.
///
/// Both halves of the pair are checked at every stop, because a write that
/// reached only `0x41` would be a balance change that no fader offers, and the
/// listing's dB figure is checked with them, because the position and the
/// number printed beside it come from two different tables and can disagree.
///
/// `/A 1` is the snap-up rule on this fader: 1 is not one of the four stops, so
/// it lands on the next one up (step 3, position 1, +6 dB) rather than on the
/// nearer stop below, which is no gain at all.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_amp_switch_drives_both_output_gain_registers() {
    for (request, position, step, db) in [
        (0u8, 0u8, 0u8, "0 dB"),
        (1, 1, 3, "+6 dB"),
        (3, 1, 3, "+6 dB"),
        (7, 2, 7, "+12 dB"),
        (10, 3, 10, "+18 dB"),
    ] {
        let (machine, dir) = boot_with_sndmixer(
            &format!("sndmixer_amp_cli{request}"),
            &format!("SNDMIXER /A {request}\r\n"),
        );
        let screen = machine.screen_text().as_text();
        for index in [0x41u8, 0x42] {
            assert_eq!(
                machine.sb_mixer_register(index),
                Some(position << 6),
                "/A {request} must put position {position} in D7-D6 of {index:#04x}\n{screen}"
            );
        }
        // The listing pads the name to 12 columns, right-aligns the step in
        // two, then leaves three before the dB figure.
        let row = format!("AMP{:9}{step:>2}   {db}", "");
        assert!(
            screen
                .lines()
                .any(|line| line.trim_end() == format!("  {row}")),
            "/A {request} must list as {row:?}\n{screen}"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

/// AMP through the config file and back, the shape `F10` and the AUTOEXEC line
/// make between them.
///
/// The middle command drives the gain to its top position, so the restore has
/// to move it back down and cannot pass on a run where the restore did nothing.
/// The second save names MASTER rather than AMP, so its `AMP` line is written
/// from reading `0x41` back off the card rather than from anything requested on
/// that command line -- which is what makes it a round trip and not an echo.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_amp_survives_the_cli_save_and_restore_round_trip() {
    let (machine, dir) = boot_with_sndmixer(
        "sndmixer_amp_roundtrip",
        "SNDMIXER /A 7 /CFG C:\\VOL.CFG /S\r\n\
         SNDMIXER /A 10 /S\r\n\
         SNDMIXER /CFG C:\\VOL.CFG /S\r\n\
         SNDMIXER /M 8 /CFG C:\\VOL2.CFG /S\r\n\
         ECHO MIXER-DONE\r\n",
    );
    let screen = machine.screen_text().as_text();
    assert!(screen.contains("MIXER-DONE"), "{screen}");
    let first = fs::read_to_string(dir.join("VOL.CFG")).expect("read the saved config");
    assert!(
        first.contains("AMP=7"),
        "/A 7 has to save as step 7:\n{first}"
    );
    for index in [0x41u8, 0x42] {
        assert_eq!(
            machine.sb_mixer_register(index),
            Some(2 << 6),
            "step 7 restores to hardware position 2 on {index:#04x}\n{screen}"
        );
    }
    let second = fs::read_to_string(dir.join("VOL2.CFG")).expect("read the second config");
    assert!(
        second.contains("AMP=7"),
        "the round trip must not drift the step:\n{second}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// A level the fader scale does not have is refused, and refused BEFORE
/// anything is written: a batch where one switch is wrong must not leave the
/// card half-configured.
///
/// 65536 and 65546 are the interesting ones. The tail parser accumulates into
/// a 16-bit register, so before it saturated they wrapped to 0 and to 10 -- a
/// typo that MUTED the master and exited 0, and a typo that quietly set it to
/// full. Both are in range after the wrap and neither could be caught by the
/// bounds check that follows.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_refuses_an_out_of_range_level_without_writing_anything() {
    let (machine, dir) = boot_with_sndmixer(
        "sndmixer_range",
        "SNDMIXER /F 6 /M 11\r\n\
         SNDMIXER /M 65536\r\n\
         SNDMIXER /M 65546\r\n\
         SNDMIXER /M 999999999\r\n\
         ECHO MIXER-DONE\r\n",
    );
    let screen = machine.screen_text().as_text();
    assert_eq!(
        screen.matches("Level must be 0 to 10: MASTER").count(),
        4,
        "all four out-of-range MASTER values are refused by name\n{screen}"
    );
    assert!(
        screen.contains("MIXER-DONE"),
        "the batch continued\n{screen}"
    );
    assert_eq!(
        machine.sb_mixer_register(0x30),
        Some(step_byte(10)),
        "MASTER must be untouched: 65536 wrapped to 0 and muted it\n{screen}"
    );
    assert_eq!(
        machine.sb_mixer_register(0x34),
        Some(step_byte(10)),
        "the switch that parsed FIRST must not have been applied either: the \
         line is refused whole\n{screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// `/S` silences the run wherever it sits on the line.
///
/// Read in order, it only silenced what came after it, so the AUTOEXEC-shaped
/// `SNDMIXER /CFG ... /S` was fine but `SNDMIXER /M 99 /S` printed its refusal
/// onto a boot screen that has no row to spare. The mutation proof is the
/// second half: the same line without `/S` does print, so this is a property of
/// the flag and not of a run that had nothing to say.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_silent_flag_works_from_any_position_on_the_line() {
    let (machine, dir) = boot_with_sndmixer(
        "sndmixer_silent_pos",
        "SNDMIXER /M 99 /S\r\n\
         SNDMIXER /L /S\r\n\
         ECHO MIXER-DONE\r\n",
    );
    let screen = machine.screen_text().as_text();
    assert!(screen.contains("MIXER-DONE"), "the batch ran\n{screen}");
    for chatter in ["Level must be", "ReSonique 2 volume levels"] {
        assert!(
            !screen.contains(chatter),
            "/S after the switch it silences must still silence it, found \
             {chatter:?}\n{screen}"
        );
    }

    let (loud, loud_dir) = boot_with_sndmixer("sndmixer_silent_pos_loud", "SNDMIXER /M 99\r\n");
    let loud_screen = loud.screen_text().as_text();
    assert!(
        loud_screen.contains("Level must be 0 to 10: MASTER"),
        "without /S the same line does print\n{loud_screen}"
    );
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&loud_dir);
}

/// The MIDI fader's step 0 writes the wavetable pair's mute BIT, and the card
/// reads a bare level of 0 there as the quietest audible step instead.
///
/// Both halves matter to the tool: it has to be able to mute the leg, and it
/// has to display what the register is really doing when something else wrote
/// a zero into it. A fader that wrote level 0 for its mute would be
/// indistinguishable from that stray write and would come back reading 1.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_midi_mute_uses_the_wavetable_mute_bit() {
    let (machine, dir) = boot_with_sndmixer("sndmixer_midi_mute", "SNDMIXER /I 0\r\n");
    let screen = machine.screen_text().as_text();
    assert_eq!(
        machine.sb_mixer_register(0x50),
        Some(0x01),
        "step 0 on MIDI writes D0, not a level of zero\n{screen}"
    );
    assert_eq!(machine.sb_mixer_register(0x51), Some(0x01), "{screen}");
    assert!(
        screen
            .lines()
            .any(|line| line.trim_end() == "  MIDI         0   mute"),
        "and it reads back as a mute\n{screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// A config file a person edited by hand: spaces and tabs around the `=`, a
/// comment, and a channel the file does not name. The file is documented as
/// TYPE-able and TOKAEDIT-able, so it has to survive being typed.
#[test]
#[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
fn sndmixer_reads_a_config_that_was_edited_by_hand() {
    let saved = "; my levels\r\nMASTER = 6\r\n\tFMSYNTH\t=\t2\r\n  CD=0\r\nJUNK = 4\r\n";
    let (machine, dir) = boot_with_sndmixer_files(
        "sndmixer_handedit",
        "SNDMIXER /CFG C:\\VOL.CFG /S\r\n",
        &[("VOL.CFG", saved)],
    );
    let screen = machine.screen_text().as_text();
    assert_eq!(
        machine.sb_mixer_register(0x30),
        Some(step_byte(6)),
        "{screen}"
    );
    assert_eq!(
        machine.sb_mixer_register(0x34),
        Some(step_byte(2)),
        "{screen}"
    );
    assert_eq!(
        machine.sb_mixer_register(0x36),
        Some(step_byte(0)),
        "{screen}"
    );
    assert_eq!(
        machine.sb_mixer_register(0x32),
        Some(step_byte(10)),
        "a channel the file does not name keeps the card's level\n{screen}"
    );
    let _ = fs::remove_dir_all(&dir);
}
