// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Proves the self-calling AUTOEXEC.BAT trick the styled boot screen design
//! (`dev_docs/2026-08-07-tokados-init-screen-design.md`) depends on: a single
//! AUTOEXEC.BAT that dispatches into labeled sections of itself with
//! `IF NOT "%1"=="" GOTO %1` at the top and
//! `FOR %%C IN (...) DO CALL C:\AUTOEXEC.BAT %%C` driving the loop, terminated
//! by `GOTO END`. This is proven against the CURRENT committed disk image with
//! no guest code changes -- it settles whether stock FreeCOM (Toka-DOS's
//! shell) actually supports the pattern before the styled boot is built on
//! top of it.

use super::*;

/// The self-calling AUTOEXEC pattern the styled boot relies on: FOR over a
/// token list, CALL back into AUTOEXEC.BAT with the token as %1, GOTO %1
/// dispatch, GOTO END. Proven against ECHO markers so a FreeCOM batch
/// regression names itself before it can scramble the boot tree.
#[test]
#[ignore = "boots a full DOS image (slow in debug); run with --ignored"]
fn autoexec_self_call_loop_dispatches_in_order() {
    let autoexec = b"@ECHO OFF\r\n\
IF NOT \"%1\"==\"\" GOTO %1\r\n\
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

    let hdd_scratch = TokaScratch::new("bootscreen-forcall");
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

    let (stop, _) = run_until_toka_condition(&mut machine, 800_000_000, |machine| {
        current_root_prompt(machine) && machine.screen_text().as_text().contains("LOOPDONE")
    });
    let text = machine.screen_text().as_text();
    if !current_root_prompt(&machine) || !text.contains("LOOPDONE") {
        panic!(
            "self-calling AUTOEXEC FOR/CALL/GOTO loop did not reach C:\\> with \
             LOOPDONE on screen (stop={stop:?}).\n{text}"
        );
    }

    // Assert ORDER, not just presence: ALPHA's label body must run (and print)
    // before BETA's, and both before the loop's own trailing ECHO, or the
    // self-call dispatch is not actually driving the labels in FOR-list order.
    let alpha_pos = text.find("MARK-ALPHA").unwrap_or_else(|| {
        panic!("MARK-ALPHA never appeared on screen (stop={stop:?}).\n{text}")
    });
    let beta_pos = text.find("MARK-BETA").unwrap_or_else(|| {
        panic!("MARK-BETA never appeared on screen (stop={stop:?}).\n{text}")
    });
    let loopdone_pos = text.find("LOOPDONE").unwrap_or_else(|| {
        panic!("LOOPDONE never appeared on screen (stop={stop:?}).\n{text}")
    });

    assert!(
        alpha_pos < beta_pos && beta_pos < loopdone_pos,
        "self-calling AUTOEXEC dispatch ran out of order (MARK-ALPHA at \
         {alpha_pos}, MARK-BETA at {beta_pos}, LOOPDONE at {loopdone_pos}); \
         expected MARK-ALPHA before MARK-BETA before LOOPDONE \
         (stop={stop:?}).\n{text}"
    );
}
