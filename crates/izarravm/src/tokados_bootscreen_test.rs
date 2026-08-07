// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Boot-screen fixtures for the styled Toka-DOS init screen
//! (`dev_docs/2026-08-07-tokados-init-screen-design.md`).
//!
//! Two different boot mechanisms prove two different things, and each test
//! below picks the one that actually proves its claim:
//!
//! - `autoexec_self_call_loop_dispatches_in_order` overrides AUTOEXEC.BAT
//!   (via `mount_hdd_folder_with`, so every other system binary still comes
//!   from the committed image but the boot script is a minimal synthetic
//!   one) to isolate and prove the self-calling batch trick itself: a single
//!   AUTOEXEC.BAT that dispatches into labeled sections of itself with
//!   `IF NOT "%1"=="" GOTO %1` at the top and
//!   `FOR %%C IN (...) DO CALL C:\AUTOEXEC.BAT %%C` driving the loop,
//!   terminated by `GOTO END`. This settles whether stock FreeCOM
//!   (Toka-DOS's shell) actually supports the pattern, independent of
//!   whatever the real stock AUTOEXEC.BAT happens to contain.
//! - `stock_boot_paints_the_styled_init_screen` boots the committed disk
//!   image directly (`boot_hdd`, no overrides) and proves the shipped
//!   screen itself: the rainbow TOKA art actually painted (attribute
//!   diversity, not just characters), every owner's `/T`-styled tree line,
//!   the two-row ReSonique2 summary, the closed double-line footer box, and
//!   the shell prompt landing on the last row with zero scroll -- exactly
//!   what a user sees on a stock boot.

use super::*;

/// Boot from a host-folder facade with AUTOEXEC.BAT overridden, for tests
/// that need to control exactly what the boot script runs rather than prove
/// what the shipped one does. `mount_hdd_folder_with` still serves every
/// OTHER system binary from the committed image -- only AUTOEXEC.BAT is
/// swapped -- so this is the right tool for isolating batch-file mechanics,
/// never for proving what the stock boot actually looks like (`boot_hdd`
/// does that; see `stock_boot_paints_the_styled_init_screen` below).
fn boot_with_autoexec(
    label: &str,
    autoexec: Vec<u8>,
    cycles: u64,
    complete: impl FnMut(&Machine) -> bool,
) -> (Machine, StopReason) {
    let hdd_scratch = TokaScratch::new(label);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine
        .mount_hdd_folder_with(
            hdd_scratch.path(),
            vec![("AUTOEXEC.BAT".to_string(), autoexec)],
        )
        .expect("mount Toka-DOS folder");
    // hdd_scratch must outlive the boot: Katea's host-folder facade reads
    // the directory lazily while the machine runs, not just at mount time,
    // so dropping it early would pull the AUTOEXEC.BAT (and everything
    // else) out from under a still-running boot.
    let (stop, _) = run_until_toka_condition(&mut machine, cycles, complete);
    (machine, stop)
}

/// The self-calling AUTOEXEC pattern the styled boot relies on: FOR over a
/// token list, CALL back into AUTOEXEC.BAT with the token as %1, GOTO %1
/// dispatch, GOTO END. Proven against ECHO markers so a FreeCOM batch
/// regression names itself before it can scramble the boot tree.
#[test]
#[ignore = "boots a full DOS image (slow in debug); run with --ignored"]
fn autoexec_self_call_loop_dispatches_in_order() {
    let autoexec = b"@ECHO OFF\r\n\
IF NOT \"%1\"==\"\" GOTO %1\r\n\
ECHO SETUP\r\n\
FOR %%C IN (ALPHA BETA) DO CALL C:\\AUTOEXEC.BAT %%C\r\n\
ECHO LOOPDONE\r\n\
GOTO END\r\n\
:ALPHA\r\n\
ECHO MARK-ALPHA\r\n\
GOTO END\r\n\
:BETA\r\n\
ECHO MARK-BETA\r\n\
GOTO END\r\n\
:END\r\n"
        .to_vec();

    let (machine, stop) = boot_with_autoexec(
        "bootscreen-forcall",
        autoexec,
        800_000_000,
        |machine| {
            current_root_prompt(machine) && machine.screen_text().as_text().contains("LOOPDONE")
        },
    );
    let text = machine.screen_text().as_text();
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault during the self-calling AUTOEXEC loop: {msg}\n{text}");
    }
    if !current_root_prompt(&machine) || !text.contains("LOOPDONE") {
        panic!(
            "self-calling AUTOEXEC FOR/CALL/GOTO loop did not reach C:\\> with \
             LOOPDONE on screen (stop={stop:?}).\n{text}"
        );
    }

    // SETUP must run exactly once: every sub-invocation carries %1 and jumps
    // straight to its label via "IF NOT %1==... GOTO %1", skipping past
    // ECHO SETUP entirely. If it printed once per FOR token instead of once
    // for the top-level invocation, the dispatch would not actually be
    // short-circuiting -- it would be falling through and re-running setup.
    assert_eq!(
        text.matches("SETUP").count(),
        1,
        "ECHO SETUP must run exactly once (the top-level invocation only), \
         not once per FOR token (stop={stop:?}).\n{text}"
    );

    // Assert ORDER, not just presence: ALPHA's label body must run (and print)
    // before BETA's, and both before the loop's own trailing ECHO, or the
    // self-call dispatch is not actually driving the labels in FOR-list order.
    let alpha_pos = text.find("MARK-ALPHA").unwrap_or_else(|| {
        panic!("MARK-ALPHA not on the current 25-row screen (stop={stop:?}).\n{text}")
    });
    let beta_pos = text.find("MARK-BETA").unwrap_or_else(|| {
        panic!("MARK-BETA not on the current 25-row screen (stop={stop:?}).\n{text}")
    });
    let loopdone_pos = text.find("LOOPDONE").unwrap_or_else(|| {
        panic!("LOOPDONE not on the current 25-row screen (stop={stop:?}).\n{text}")
    });

    assert!(
        alpha_pos < beta_pos && beta_pos < loopdone_pos,
        "self-calling AUTOEXEC dispatch ran out of order (MARK-ALPHA at \
         {alpha_pos}, MARK-BETA at {beta_pos}, LOOPDONE at {loopdone_pos}); \
         expected MARK-ALPHA before MARK-BETA before LOOPDONE \
         (stop={stop:?}).\n{text}"
    );
}

/// The styled stock boot end-to-end: rainbow art actually painted (attribute
/// diversity, not just characters), every owner's tree line with its prefix,
/// the two-row ReSonique2 block, the closed footer box, the prompt on the
/// last row with ZERO scroll. Boots the committed image with no overrides --
/// exactly what a user sees.
#[test]
#[ignore = "boots a full DOS image (slow in debug); run with --ignored"]
fn stock_boot_paints_the_styled_init_screen() {
    let (mut machine, stop, boot_cycles) = boot_hdd(700_000_000);
    let frame = machine.screen_text();
    let text = frame.as_text();
    if let StopReason::CpuError(msg) = &stop {
        panic!(
            "CPU fault during the stock styled boot after {boot_cycles} \
             requested cycles: {msg}\nstop={stop:?}\n{text}"
        );
    }
    if !current_root_prompt(&machine) {
        panic!(
            "no C:\\> prompt after {boot_cycles} requested cycles \
             (stop={stop:?}).\n{text}"
        );
    }

    // Rainbow: the top art rows must show real attribute diversity on their
    // non-space cells, not just the right characters sitting at the default
    // 0x07 attribute -- that would prove the ASCII art shape but not that
    // the color ramp write in signon() actually executed.
    let mut rainbow_attrs = std::collections::HashSet::new();
    for row in 0..10.min(frame.rows) {
        for col in 0..frame.columns {
            let cell = frame.cells[row * frame.columns + col];
            if cell.character != b' ' && cell.character != 0 && cell.attribute != 0x07 {
                rainbow_attrs.insert(cell.attribute);
            }
        }
    }
    assert!(
        rainbow_attrs.len() >= 4,
        "rows 0-9 must show at least 4 distinct non-default (non-0x07) \
         attributes on non-space cells (found {}); the rainbow ramp did not \
         paint\n{text}",
        rainbow_attrs.len()
    );

    let lines: Vec<String> = (0..frame.rows).map(|row| frame.line_string(row)).collect();

    // Box title: the build number and compile date vary, so this is a
    // contains() check, not a whole-line needle.
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Toka-DOS 3.0 - Kernel build 20")),
        "welcome-box title line not on the current 25-row screen\n{text}"
    );

    // Whole-line needles: the kernel box edge rows also contain 0xC3/0xC4
    // decoratively, so a substring match on the tree prefix alone would pass
    // even if a tree line never actually printed. Exact-line comparison is
    // the only check that proves these specific lines exist as printed.
    let exact_needles: [&str; 6] = [
        "\u{C3}\u{C4}> Kernel compatibility: 7.10 - WATCOMC - FAT32 support",
        "\u{C3}\u{C4}> TOKAEMM XMS/UMB/EMS memory manager; system running in V86.",
        "\u{C3}\u{C4}> IZCDEX installed. Assigned [1] drive(s): [TOKACD01 D:]",
        "\u{C3}\u{C4}> Toka-DOS mouse driver installed.",
        "\u{C3}\u{C4}> ReSonique2 Configuration [Run SNDCTRL to change]",
        "\u{B3}     SB16 220 I7 D1 H5   WSS 530 I11 D0   MIDI 300 I9",
    ];
    for needle in exact_needles {
        assert!(
            lines.iter().any(|line| line.as_str() == needle),
            "expected exact line not on the current 25-row screen: {needle:?}\n{text}"
        );
    }

    // The C: drive line's size varies with image geometry; pin the prefix
    // and shape (ends in " MB") instead of the whole line.
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("\u{C3}\u{C4}> C: HD1, start=") && line.ends_with(" MB")),
        "the C: drive tree line (with its varying size) is not on the \
         current 25-row screen\n{text}"
    );

    // The footer box: opened by AUTOEXEC's epilogue after every tree line,
    // so reordering the FOR list can never orphan the closer.
    let footer_bar = "\u{CD}".repeat(76);
    let footer_top = format!("\u{C6}{footer_bar}\u{B8}");
    let footer_bottom = format!("\u{D4}{footer_bar}\u{BE}");
    let footer_middle = format!(
        "\u{B3}{:<76}\u{B3}",
        "   Starting in text mode. Run TOKADESK to enable the visual workbench."
    );
    for needle in [&footer_top, &footer_middle, &footer_bottom] {
        assert!(
            lines.iter().any(|line| line == needle),
            "footer box line not on the current 25-row screen: {needle:?}\n{text}"
        );
    }

    // Zero scroll: the logo is still at row 0 (never pushed off the top),
    // and the shell prompt lands exactly on the last row.
    let row0_has_logo = frame.cells[0..frame.columns]
        .iter()
        .any(|cell| cell.character == 0xB1 || cell.character == 0xDB);
    assert!(
        row0_has_logo,
        "row 0 no longer holds logo glyphs (0xB1/0xDB); the screen scrolled\n{text}"
    );
    let last_row = frame.rows - 1;
    let last_line = frame.line_string(last_row);
    assert!(
        last_line.starts_with("C:\\>"),
        "the shell prompt is not on the last row (row {last_row}): \
         {last_line:?}\n{text}"
    );

    // FreeCOM's startup banner is silenced (Task 5): "XMS_Swap" (part of the
    // VER banner's compiled-feature suffix) must not appear on the settled
    // screen from the boot alone.
    assert!(
        !text.contains("XMS_Swap"),
        "XMS_Swap text appeared on the settled boot screen; FreeCOM's silent \
         startup patch should have skipped the implicit VER banner\n{text}"
    );

    // Positive probe: typing VER manually DOES print XMS_Swap, proving the
    // negative match above is a real absence rather than a needle that could
    // never match regardless of what the screen holds (fixtures-that-cannot-fail).
    for ch in "ver\r".chars() {
        for code in ascii_to_set1(ch) {
            machine.inject_key_scancodes(&[code]);
        }
        machine
            .run_until_halt_or_cycles(5_000_000)
            .expect("type ver");
    }
    run_until_toka_condition(&mut machine, 20_000_000, |machine| {
        current_root_prompt(machine) && machine.screen_text().as_text().contains("XMS_Swap")
    });
    let ver_text = machine.screen_text().as_text();
    assert!(
        ver_text.contains("XMS_Swap"),
        "typing VER manually did not print XMS_Swap; the negative match above \
         is unproven\n{ver_text}"
    );
}
