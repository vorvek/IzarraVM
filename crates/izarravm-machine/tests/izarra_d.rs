// STREAM D integration test: the Margo-LFB setup page.
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
//   4. The setup page draws on the Margo LFB (mode 0x150), like the graphical POST
//      screen and the Tab boot menu: the red title bar renders inside the box.
//
// Keys are fed as Set 1 scancodes via inject_key_scancodes. Device bytes cross
// the PS/2 wire on timed deadlines, so navigation sends one make/break pair and
// runs the guest before sending the next pair.

use izarravm_core::{GswMode, VideoCard};
use izarravm_firmware::izarra_bios;
use izarravm_machine::{ActiveDisplay, MARGO_LFB_BASE, Machine, MachineProfile, StopReason};

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
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    Machine::new(profile, izarra_bios()).unwrap()
}

fn press(machine: &mut Machine, make_code: u8, break_code: u8, clocks: u64) -> StopReason {
    machine.inject_key_scancodes(&[make_code, break_code]);
    machine.run_until_halt_or_cycles(clocks).unwrap()
}

fn enter_setup(machine: &mut Machine) {
    let _ = press(machine, DEL_MAKE, DEL_BREAK, 20_000_000);
}

#[test]
fn setup_skipped_without_hotkey_boots_and_idles() {
    let mut machine = boot_machine();
    // No hotkey: inject an ordinary key so the keyboard path is exercised, then run.
    // The setup window peeks, sees no Del, and the BIOS boots straight to its idle
    // loop, which keeps running, so the run reaches the cycle budget.
    machine.inject_key_scancodes(&[A_MAKE, A_BREAK]);
    let reason = machine.run_until_halt_or_cycles(20_000_000).unwrap();
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
    enter_setup(&mut machine);
    let _ = press(&mut machine, DOWN_MAKE, DOWN_BREAK, 3_000_000);
    let _ = press(&mut machine, DOWN_MAKE, DOWN_BREAK, 3_000_000);
    let _ = press(&mut machine, RIGHT_MAKE, RIGHT_BREAK, 3_000_000);
    let _ = press(&mut machine, RIGHT_MAKE, RIGHT_BREAK, 3_000_000);
    let reason = press(&mut machine, F10_MAKE, F10_BREAK, 30_000_000);
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
    enter_setup(&mut machine);
    let _ = press(&mut machine, DOWN_MAKE, DOWN_BREAK, 3_000_000);
    let _ = press(&mut machine, DOWN_MAKE, DOWN_BREAK, 3_000_000);
    let _ = press(&mut machine, RIGHT_MAKE, RIGHT_BREAK, 3_000_000);
    let reason = press(&mut machine, ESC_MAKE, ESC_BREAK, 20_000_000);
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
fn setup_save_then_setup_draws_the_lfb() {
    // After a Save the BIOS commits the change and cold-resets (the setup page's
    // documented exit); POST then runs again and, finding no further hotkey,
    // boots straight through to the idle loop. Both POST and the setup page
    // present on the Margo LFB (mode 0x150), so the display mode stays LFB
    // across the reset, and the chosen GSW mode is the one that was saved
    // (proving the reset replayed POST at the new live speed rather than
    // leaving some stale mode-13h/text state behind, the way the old mode-13h
    // page could).
    let mut machine = boot_machine();
    enter_setup(&mut machine);
    let _ = press(&mut machine, DOWN_MAKE, DOWN_BREAK, 3_000_000);
    let _ = press(&mut machine, DOWN_MAKE, DOWN_BREAK, 3_000_000);
    let _ = press(&mut machine, RIGHT_MAKE, RIGHT_BREAK, 3_000_000);
    let _ = press(&mut machine, F10_MAKE, F10_BREAK, 30_000_000);

    assert_eq!(
        machine.active_display(),
        ActiveDisplay::MargoLfb,
        "the post-save reboot leaves the Margo LFB presented"
    );
    assert_eq!(machine.active_mode(), GswMode::Gsw486);
}

#[test]
fn setup_menu_draws_the_title_on_the_lfb() {
    // While the setup menu is open (before any Save/Discard resets the machine)
    // it draws its own screen on the Margo LFB: a red title bar reading "IZARRA
    // 3000 SETUP" at the top-left, inside the bordered menu box. The graphical
    // POST screen never paints red text in that exact band (its own title sits
    // elsewhere and the wordmark/mascot art is further right), so red pixels
    // there prove the setup page's own chrome drew over the LFB, not just that
    // POST happened to leave the LFB active.
    let mut machine = boot_machine();
    machine.inject_key_scancodes(&[DEL_MAKE, DEL_BREAK]); // enter setup, then block on a key
    machine.run_until_halt_or_cycles(20_000_000).unwrap();

    assert_eq!(
        machine.active_display(),
        ActiveDisplay::MargoLfb,
        "the setup menu draws on the Margo LFB"
    );
    // Title band y 4..12, x 8..152 ("IZARRA 3000 SETUP", red index ART_RED_INDEX
    // = 24): scan for red glyph pixels.
    let mut red = 0;
    for y in 4..12u32 {
        for x in 8..152u32 {
            if machine.read_physical_u8(MARGO_LFB_BASE + y * 320 + x) == 24 {
                red += 1;
            }
        }
    }
    assert!(
        red > 20,
        "the red setup title is on the LFB, found {red} red pixels"
    );
}

#[test]
fn setup_sub_pages_open_and_return() {
    // Open the Time, Peripherals, and Health sub-pages in turn and back out of each
    // with Esc, then Discard. The peripherals page re-runs the POST probes and the
    // health page formats jittered readings, so this exercises those code paths.
    // The run completing at the cycle limit (no fault) plus the boot mode staying at
    // the default proves the page returned cleanly each time.
    let mut machine = boot_machine();
    enter_setup(&mut machine);
    let _ = press(&mut machine, ENTER_MAKE, ENTER_BREAK, 4_000_000);
    let _ = press(&mut machine, ESC_MAKE, ESC_BREAK, 4_000_000);
    let _ = press(&mut machine, DOWN_MAKE, DOWN_BREAK, 3_000_000);
    let _ = press(&mut machine, DOWN_MAKE, DOWN_BREAK, 3_000_000);
    let _ = press(&mut machine, DOWN_MAKE, DOWN_BREAK, 3_000_000);
    let _ = press(&mut machine, ENTER_MAKE, ENTER_BREAK, 5_000_000);
    let _ = press(&mut machine, ESC_MAKE, ESC_BREAK, 4_000_000);
    let _ = press(&mut machine, DOWN_MAKE, DOWN_BREAK, 3_000_000);
    let _ = press(&mut machine, ENTER_MAKE, ENTER_BREAK, 4_000_000);
    let _ = press(&mut machine, ESC_MAKE, ESC_BREAK, 4_000_000);
    let reason = press(&mut machine, ESC_MAKE, ESC_BREAK, 20_000_000);
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
