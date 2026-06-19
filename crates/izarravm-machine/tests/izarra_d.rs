// STREAM D integration test: the mode-13h setup page.
//
// Drives the public Machine API only. Two cases:
//   1. Without the setup hotkey the BIOS boots and parks at its idle halt, and the
//      live compatibility mode stays at the boot default.
//   2. With the hotkey, a CHOICE edit, and Save (F10), the live Lotura write on the
//      Save row switches active_mode() to the chosen GswMode.
//
// Keys are fed as Set 1 scancodes via inject_key_scancodes. The setup page polls
// the keyboard in software (it never halts inside the window), so a whole burst
// injected up front is consumed in order as IRQ1 delivers each scancode.

use izarravm_core::{GswMode, VideoCard};
use izarravm_firmware::izarra_bios;
use izarravm_machine::{Machine, MachineProfile, StopReason};

// Set 1 make/break codes used by the setup page.
const DEL_MAKE: u8 = 0x53;
const DEL_BREAK: u8 = 0xd3;
const RIGHT_MAKE: u8 = 0x4d;
const RIGHT_BREAK: u8 = 0xcd;
const F10_MAKE: u8 = 0x44;
const F10_BREAK: u8 = 0xc4;
const ESC_MAKE: u8 = 0x01;
const ESC_BREAK: u8 = 0x81;
const A_MAKE: u8 = 0x1e;
const A_BREAK: u8 = 0x9e;

fn boot_machine() -> Machine {
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    Machine::new(profile, izarra_bios()).unwrap()
}

#[test]
fn setup_skipped_without_hotkey_boots_and_halts() {
    let mut machine = boot_machine();
    // No hotkey: inject an ordinary key so the keyboard path is exercised, then run.
    // The setup window peeks, sees no Del, and the BIOS boots straight to its idle
    // halt. With IRQ0 masked there is no timer wake, so the run ends in Halted.
    machine.inject_key_scancodes(&[A_MAKE, A_BREAK]);
    let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted, "BIOS reaches the idle halt");
    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw386,
        "no setup entry leaves the boot mode unchanged"
    );
}

#[test]
fn setup_save_applies_chosen_gsw_mode() {
    let mut machine = boot_machine();
    assert_eq!(machine.active_mode(), GswMode::Gsw386, "boot mode is 386");

    // Hotkey to enter setup, then advance the CHOICE from 386 to 586 with two Right
    // presses, then Save with F10. The Save row writes the chosen code to the live
    // Lotura register; the switch lands on the next instruction.
    machine.inject_key_scancodes(&[
        DEL_MAKE,
        DEL_BREAK, // enter setup
        RIGHT_MAKE,
        RIGHT_BREAK, // CHOICE 386 -> 486
        RIGHT_MAKE,
        RIGHT_BREAK, // CHOICE 486 -> 586
        F10_MAKE,
        F10_BREAK, // Save
    ]);
    let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted, "setup saves then boots to halt");
    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw586,
        "Save wrote the chosen mode to the live Lotura register"
    );
}

#[test]
fn setup_discard_keeps_boot_mode() {
    let mut machine = boot_machine();
    // Enter setup, edit the CHOICE, then Discard (Esc). The working copy changes but
    // active_mode must stay at the boot default.
    machine.inject_key_scancodes(&[
        DEL_MAKE,
        DEL_BREAK,
        RIGHT_MAKE,
        RIGHT_BREAK, // edit to 486 in the working copy
        ESC_MAKE,
        ESC_BREAK, // Discard
    ]);
    let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted, "discard still boots to halt");
    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw386,
        "Discard leaves the active mode untouched"
    );
}

#[test]
fn setup_save_then_setup_draws_mode13h() {
    // After a Save the BIOS is halted with the setup page on screen in mode 13h.
    // Advance the beam and confirm a 320-wide raster is presented (the page used the
    // foundation mode-13h primitives).
    let mut machine = boot_machine();
    machine.inject_key_scancodes(&[
        DEL_MAKE,
        DEL_BREAK,
        RIGHT_MAKE,
        RIGHT_BREAK,
        F10_MAKE,
        F10_BREAK,
    ]);
    machine.run_until_halt_or_cycles(5_000_000).unwrap();
    // Save switched the live mode to 486 (66 MHz), so a full mode-13h frame takes
    // more CPU clocks to scan than at the 386 boot clock. Advance generously so the
    // raster engine completes at least one frame before the snapshot.
    machine.advance_devices_clocks(15_000_000);
    let raster = machine
        .vga_raster()
        .expect("the setup page leaves mode 13h presented");
    assert_eq!(raster.width, 320);
    assert_eq!(machine.active_mode(), GswMode::Gsw486);
}
