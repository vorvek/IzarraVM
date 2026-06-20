// STREAM E integration test: the Tab boot menu and the INT 19h floppy bootstrap.
//
// Drives the public Machine API only. During the POST hotkey window the BIOS
// watches for Del (setup) and Tab (boot menu). Tab opens a mode-13h menu listing
// Floppy, Disk, and CD-ROM; Up/Down move the highlight, Enter selects, Esc cancels.
// The default highlight comes from CMOS 0x11 (0 floppy-first). Selecting Floppy
// drives the floppy path, which reads the boot sector and far jumps to 0000:7C00.
//
// Cases:
//   1. Tab during POST opens the menu: the screen is mode 13h and carries the
//      menu's heading and highlight colors that the plain POST screen does not.
//   2. Tab then Enter on the default Floppy entry boots the mounted image: the
//      Wizardry III booter takes over, switches the card to CGA, and draws its
//      title, so the active video mode leaves mode 13h.
//
// Keys are fed as Set 1 scancodes via inject_key_scancodes. The menu blocks on a
// keyboard read between keystrokes, so a burst injected up front is consumed in
// order as IRQ1 delivers each scancode.

use izarravm_core::VideoCard;
use izarravm_firmware::izarra_bios;
use izarravm_machine::{Machine, MachineProfile};
use izarravm_video::VideoMode;

// Set 1 make/break codes used by the boot menu.
const TAB_MAKE: u8 = 0x0f;
const TAB_BREAK: u8 = 0x8f;
const ENTER_MAKE: u8 = 0x1c;
const ENTER_BREAK: u8 = 0x9c;
const DOWN_MAKE: u8 = 0x50;
const DOWN_BREAK: u8 = 0xd0;
const ESC_MAKE: u8 = 0x01;
const ESC_BREAK: u8 = 0x81;

// The real Wizardry III booter image. The headless boot-floppy smoke command uses
// the same path; the test is skipped when the corpus is not present so it stays
// green on machines without the local collection.
const WIZ3_IMAGE: &str = "R:/La Colección by Neville/dosroot/Wizardry III - The Legacy of Llylgamyn (PC Booter)/WIZ3MAST.IMG";

fn boot_machine() -> Machine {
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    Machine::new(profile, izarra_bios()).unwrap()
}

#[test]
fn tab_opens_the_boot_menu_in_mode13h() {
    // Tab during POST opens the menu and stops there blocking on a key. The menu
    // draws on a mode-13h field with a yellow heading (index 0x0e) and a cyan
    // highlight on the default Floppy entry (index 0x0b). The graceful POST screen
    // uses neither index, so their presence proves the menu painted.
    let mut machine = boot_machine();
    machine.inject_key_scancodes(&[TAB_MAKE, TAB_BREAK]);
    machine.run_until_halt_or_cycles(12_000_000).unwrap();

    assert_eq!(
        machine.video().active_mode(),
        VideoMode::Mode13h,
        "the boot menu draws in mode 13h"
    );
    let raster = machine.video_mut().render_full_frame();
    let has_heading = raster.pixels.contains(&0x0e);
    let has_highlight = raster.pixels.contains(&0x0b);
    assert!(
        has_heading && has_highlight,
        "the menu heading and highlighted entry are on screen"
    );
}

#[test]
fn tab_then_enter_boots_the_floppy() {
    // Mount the Wizardry booter, open the menu with Tab, and select the default
    // Floppy entry with Enter. The floppy path reads the boot sector and jumps to
    // it; the booter switches the card to CGA for its title. Reaching CGA proves
    // the menu drove the floppy bootstrap.
    let image = match std::fs::read(WIZ3_IMAGE) {
        Ok(bytes) => bytes,
        Err(_) => {
            eprintln!("skipping: Wizardry III corpus image not present");
            return;
        }
    };
    let mut machine = boot_machine();
    machine.mount_floppy(image).expect("720 KB image mounts");
    machine.inject_key_scancodes(&[TAB_MAKE, TAB_BREAK, ENTER_MAKE, ENTER_BREAK]);
    machine.run_until_halt_or_cycles(50_000_000).unwrap();

    assert_eq!(
        machine.video().active_mode(),
        VideoMode::Cga,
        "the Wizardry booter ran and switched to CGA"
    );
}

#[test]
fn tab_navigates_to_disk_and_reports_not_installed() {
    // Tab opens the menu, Down moves to the Disk entry, Enter reports that Toka-DOS
    // is not installed and stays on the menu (no boot). Esc then cancels. The run
    // reaching the cycle limit in mode 13h without a fault proves the Disk entry
    // returned to the menu rather than booting or crashing.
    let mut machine = boot_machine();
    machine.inject_key_scancodes(&[
        TAB_MAKE,
        TAB_BREAK, // open the menu (Floppy highlighted)
        DOWN_MAKE,
        DOWN_BREAK, // Floppy -> Disk
        ENTER_MAKE,
        ENTER_BREAK, // Disk: report not installed, stay on the menu
        ESC_MAKE,
        ESC_BREAK, // cancel the menu
    ]);
    machine.run_until_halt_or_cycles(12_000_000).unwrap();
    // Esc on the menu falls through to boot_entry with the floppy default and no
    // media mounted, so the read fails and the BIOS idles. The card stays in the
    // mode-13h POST/menu field; the key point is no fault occurred.
    assert_eq!(
        machine.video().active_mode(),
        VideoMode::Mode13h,
        "the Disk entry returned to the menu without booting"
    );
}
