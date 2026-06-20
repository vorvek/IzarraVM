// STREAM D integration test: the mode-13h setup page.
//
// Drives the public Machine API only. The setup page is a multi-row menu; the GSW
// compatibility mode is the third selectable row (Time, Keyboard, CPU mode, ...),
// so reaching it from the default Time highlight takes two Down presses before the
// Left/Right edit. Cases:
//   1. Without the setup hotkey the BIOS boots and parks at its idle loop, and the
//      live compatibility mode stays at the boot default.
//   2. With the hotkey, navigating to the CPU-mode row, editing it, and Save (F10),
//      the live Lotura write switches active_mode() to the chosen GswMode.
//   3. Discard (Esc) leaves the live mode untouched.
//
// Keys are fed as Set 1 scancodes via inject_key_scancodes. The menu blocks on a
// keyboard read between keystrokes, so a whole burst injected up front is consumed
// in order as IRQ1 delivers each scancode.

use izarravm_core::{GswMode, VideoCard};
use izarravm_firmware::izarra_bios;
use izarravm_machine::{Machine, MachineProfile, StopReason};

// Set 1 make/break codes used by the setup page.
const DEL_MAKE: u8 = 0x53;
const DEL_BREAK: u8 = 0xd3;
const DOWN_MAKE: u8 = 0x50;
const DOWN_BREAK: u8 = 0xd0;
const RIGHT_MAKE: u8 = 0x4d;
const RIGHT_BREAK: u8 = 0xcd;
const F10_MAKE: u8 = 0x44;
const F10_BREAK: u8 = 0xc4;
const ESC_MAKE: u8 = 0x01;
const ESC_BREAK: u8 = 0x81;
const ENTER_MAKE: u8 = 0x1c;
const ENTER_BREAK: u8 = 0x9c;
const A_MAKE: u8 = 0x1e;
const A_BREAK: u8 = 0x9e;

fn boot_machine() -> Machine {
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    Machine::new(profile, izarra_bios()).unwrap()
}

#[test]
fn setup_skipped_without_hotkey_boots_and_idles() {
    let mut machine = boot_machine();
    // No hotkey: inject an ordinary key so the keyboard path is exercised, then run.
    // The setup window peeks, sees no Del, and the BIOS boots straight to its idle
    // loop, which keeps running, so the run reaches the cycle budget.
    machine.inject_key_scancodes(&[A_MAKE, A_BREAK]);
    let reason = machine.run_until_halt_or_cycles(12_000_000).unwrap();
    assert!(
        matches!(reason, StopReason::CycleLimit { .. }),
        "BIOS boots and idles"
    );
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

    // Hotkey to enter setup, move down to the CPU-mode row, advance the mode from
    // 386 to 586 with two Right presses, then Save with F10. The save writes the
    // chosen code to the live Lotura register; the switch lands at once.
    machine.inject_key_scancodes(&[
        DEL_MAKE,
        DEL_BREAK, // enter setup
        DOWN_MAKE,
        DOWN_BREAK, // Time -> Keyboard
        DOWN_MAKE,
        DOWN_BREAK, // Keyboard -> CPU mode
        RIGHT_MAKE,
        RIGHT_BREAK, // 386 -> 486
        RIGHT_MAKE,
        RIGHT_BREAK, // 486 -> 586
        F10_MAKE,
        F10_BREAK, // Save
    ]);
    let reason = machine.run_until_halt_or_cycles(12_000_000).unwrap();
    assert!(
        matches!(reason, StopReason::CycleLimit { .. }),
        "setup saves then boots and idles"
    );
    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw586,
        "Save wrote the chosen mode to the live Lotura register"
    );
}

#[test]
fn setup_discard_keeps_boot_mode() {
    let mut machine = boot_machine();
    // Enter setup, move to the CPU-mode row, edit it, then Discard (Esc). The working
    // copy changes but active_mode must stay at the boot default.
    machine.inject_key_scancodes(&[
        DEL_MAKE,
        DEL_BREAK,
        DOWN_MAKE,
        DOWN_BREAK, // Time -> Keyboard
        DOWN_MAKE,
        DOWN_BREAK, // Keyboard -> CPU mode
        RIGHT_MAKE,
        RIGHT_BREAK, // edit to 486 in the working copy
        ESC_MAKE,
        ESC_BREAK, // Discard
    ]);
    let reason = machine.run_until_halt_or_cycles(12_000_000).unwrap();
    assert!(
        matches!(reason, StopReason::CycleLimit { .. }),
        "discard still boots and idles"
    );
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
        DOWN_MAKE,
        DOWN_BREAK, // Time -> Keyboard
        DOWN_MAKE,
        DOWN_BREAK, // Keyboard -> CPU mode
        RIGHT_MAKE,
        RIGHT_BREAK, // 386 -> 486
        F10_MAKE,
        F10_BREAK, // Save
    ]);
    machine.run_until_halt_or_cycles(12_000_000).unwrap();
    // Save switched the live mode to 486 (66 MHz), so a full mode-13h frame takes
    // more CPU clocks to scan than at the 386 boot clock. Advance generously so the
    // raster engine completes at least one frame before the snapshot.
    machine.advance_devices_clocks(112_000_000);
    let raster = machine
        .vga_raster()
        .expect("the setup page leaves mode 13h presented");
    assert_eq!(raster.width, 320);
    assert_eq!(machine.active_mode(), GswMode::Gsw486);
}

#[test]
fn setup_sub_pages_open_and_return() {
    // Open the Time, Peripherals, and Health sub-pages in turn and back out of each
    // with Esc, then Discard. The peripherals page re-runs the POST probes and the
    // health page formats jittered readings, so this exercises those code paths.
    // The run completing at the cycle limit (no fault) plus the boot mode staying at
    // the default proves the page returned cleanly each time.
    let mut machine = boot_machine();
    machine.inject_key_scancodes(&[
        DEL_MAKE,
        DEL_BREAK, // enter setup (highlight on Time, row 0)
        ENTER_MAKE,
        ENTER_BREAK, // open Time
        ESC_MAKE,
        ESC_BREAK, // back to the menu
        DOWN_MAKE,
        DOWN_BREAK, // Time -> Keyboard
        DOWN_MAKE,
        DOWN_BREAK, // Keyboard -> CPU mode
        DOWN_MAKE,
        DOWN_BREAK, // CPU mode -> Peripherals
        ENTER_MAKE,
        ENTER_BREAK, // open Peripherals (runs the probes)
        ESC_MAKE,
        ESC_BREAK, // back to the menu
        DOWN_MAKE,
        DOWN_BREAK, // Peripherals -> Health
        ENTER_MAKE,
        ENTER_BREAK, // open Health (jittered readings)
        ESC_MAKE,
        ESC_BREAK, // back to the menu
        ESC_MAKE,
        ESC_BREAK, // Esc on the menu discards and exits
    ]);
    let reason = machine.run_until_halt_or_cycles(12_000_000).unwrap();
    assert!(
        matches!(reason, StopReason::CycleLimit { .. }),
        "the sub-pages open and return without fault"
    );
    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw386,
        "backing out of the sub-pages and the menu leaves the boot mode"
    );
}
