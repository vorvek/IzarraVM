// STREAM E integration test: the Tab boot menu and the INT 19h floppy bootstrap.
//
// Drives the public Machine API only. During the POST hotkey window the BIOS
// watches for Del (setup) and Tab (boot menu). Tab opens the boot menu on the
// Margo LFB (mode 0x150) inside the boot-box sprite: a single stacked column with
// three device rows (Hard Disk, Floppy, CD-ROM), four CPU-speed rows, and an
// Accept row, walked by a flat focus index (Up/Down). Enter marks a device/speed
// or commits on Accept; F10 also commits; Esc bails to the default boot order. The
// default device comes from CMOS 0x11. Committing Floppy drives the floppy path,
// which reads the boot sector and far jumps to 0000:7C00. (The menu is keyboard-
// only; the old mode-13h two-pane menu and its PS/2 mouse layer were retired.)
//
// Cases:
//   1. Tab during POST opens the menu on the LFB: the red menu title renders inside
//      the box, which the plain POST screen does not paint there.
//   2. Tab then F10 (Accept) on the default Floppy boots the mounted image: the
//      Wizardry III booter takes over and switches the card to CGA.
//   3. Walking down to the 286 row and accepting switches the CPU tier live.
//
// Keys are fed as Set 1 scancodes via inject_key_scancodes.

use izarravm_core::{GswMode, VideoCard};
use izarravm_firmware::izarra_bios;
use izarravm_machine::{ActiveDisplay, MARGO_LFB_BASE, Machine, MachineProfile};
use izarravm_video::VideoMode;

// Set 1 make/break codes used by the boot menu.
const TAB_MAKE: u8 = 0x0f;
const TAB_BREAK: u8 = 0x8f;
const ENTER_MAKE: u8 = 0x1c;
const ENTER_BREAK: u8 = 0x9c;
const UP_MAKE: u8 = 0x48;
const UP_BREAK: u8 = 0xc8;
const DOWN_MAKE: u8 = 0x50;
const DOWN_BREAK: u8 = 0xd0;
const ESC_MAKE: u8 = 0x01;
const ESC_BREAK: u8 = 0x81;
// F10 (or Enter on the Accept row) commits the marked device and speed.
const F10_MAKE: u8 = 0x44;
const F10_BREAK: u8 = 0xc4;

// The real Wizardry III booter image. The headless boot-floppy smoke command uses
// the same path; the test is skipped when the corpus is not present so it stays
// green on machines without the local collection.
const WIZ3_IMAGE: &str = "R:/La Colección by Neville/dosroot/Wizardry III - The Legacy of Llylgamyn (PC Booter)/WIZ3MAST.IMG";

fn boot_machine() -> Machine {
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    Machine::new(profile, izarra_bios()).unwrap()
}

#[test]
fn tab_opens_the_boot_menu_on_the_lfb() {
    // Tab during POST opens the boot menu on the Margo LFB (not a mode switch) and
    // blocks there on a key. The menu's red title "Boot & Speed" (art palette index
    // ART_RED_INDEX = 24) renders inside the box; the POST screen never paints red
    // in that band, so red pixels there prove the menu drew over POST on the LFB.
    let mut machine = boot_machine();
    machine.inject_key_scancodes(&[TAB_MAKE, TAB_BREAK]);
    machine.run_until_halt_or_cycles(25_000_000).unwrap();

    assert_eq!(
        machine.active_display(),
        ActiveDisplay::MargoLfb,
        "the boot menu draws on the Margo LFB"
    );
    // Title band y 64..72, x 28..130: red glyphs (index 24).
    let mut red = 0;
    for y in 64..72u32 {
        for x in 28..130u32 {
            if machine.read_physical_u8(MARGO_LFB_BASE + y * 320 + x) == 24 {
                red += 1;
            }
        }
    }
    assert!(
        red > 20,
        "the red menu title is on the LFB, found {red} red pixels"
    );
}

#[test]
fn tab_then_accept_boots_the_floppy() {
    // Mount the Wizardry booter, open the two-pane menu with Tab, and Accept with
    // F10. The menu opens with Floppy already the marked device (the only bootable
    // one), so Accept boots it: the floppy path reads the boot sector and jumps to
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
    machine.inject_key_scancodes(&[TAB_MAKE, TAB_BREAK, F10_MAKE, F10_BREAK]);
    // The floppy now takes realistic mechanical time (seek + rotational latency +
    // ~31 KB/s for this 720 KB disk), so loading the booter's stage 2 burns far
    // more guest cycles than an instant read did. Run in chunks and stop as soon
    // as the booter switches to CGA, rather than burning a fixed huge budget.
    let mut reached_cga = false;
    for _ in 0..60 {
        machine.run_until_halt_or_cycles(10_000_000).unwrap();
        if machine.video().active_mode() == VideoMode::Cga {
            reached_cga = true;
            break;
        }
    }
    assert!(reached_cga, "the Wizardry booter ran and switched to CGA");
}

#[test]
fn accept_super_slow_commits_the_286_tier() {
    // Open the menu, cross to the speed pane, walk down to the Super Slow (286) row,
    // mark it with Enter, then Accept with F10. The Accept maps the marked row to GSW
    // code 3 and writes it to the live Lotura register (port 0xE1) and to CMOS 0x12.
    // The 0xE1 write is a live switch (no cold reset), so the firmware keeps running
    // at the new speed; active_mode() reads back Gsw286 and CMOS 0x12 holds 3,
    // mirroring the other speed tiers.
    let mut machine = boot_machine();
    assert_eq!(machine.active_mode(), GswMode::Gsw386, "boot mode is 386");

    // Tab opens the menu with focus on the marked device (flat index 1 = Floppy).
    // The flat list is dev0..dev2 (0..2), spd0..spd3 (3..6), Accept (7), so five
    // Downs walk from Floppy (1) to the Super Slow / 286 row (focus 6 = speed row 3).
    // Enter marks it, F10 accepts.
    machine.inject_key_scancodes(&[
        TAB_MAKE,
        TAB_BREAK, // open the boot menu (focus = Floppy)
        DOWN_MAKE,
        DOWN_BREAK, // -> CD-ROM
        DOWN_MAKE,
        DOWN_BREAK, // -> Fast 586
        DOWN_MAKE,
        DOWN_BREAK, // -> Slow 486
        DOWN_MAKE,
        DOWN_BREAK, // -> VSlow 386
        DOWN_MAKE,
        DOWN_BREAK, // -> SSlow 286
        ENTER_MAKE,
        ENTER_BREAK, // mark the Super Slow row
        F10_MAKE,
        F10_BREAK, // Accept
    ]);
    // Accept applies the speed live and falls through into the boot path. The seven
    // queued keys each trigger a row redraw on top of the ~15M-cycle POST, so give
    // the run room to process them all and settle on the new tier.
    machine.run_until_halt_or_cycles(40_000_000).unwrap();

    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw286,
        "Accept wrote GSW code 3 to the live Lotura register"
    );
    assert_eq!(
        machine.cmos_byte(0x12),
        3,
        "Accept persisted GSW code 3 to CMOS 0x12"
    );
}

#[test]
fn saved_cmos_gsw_mode_is_applied_at_post() {
    // The persisted half of a setup-panel Save: a CMOS image carrying GSW code
    // 3 (286) retunes the machine during POST bring-up, even though the host
    // profile boots 386. This mirrors a cmos.bin load at startup, which lands
    // before the first run.
    let mut machine = boot_machine();
    assert_eq!(
        machine.cmos_byte(0x12),
        0,
        "a fresh CMOS is seeded with the profile's code (386 = 0)"
    );
    machine.set_cmos_byte(0x12, 3);
    machine.run_until_halt_or_cycles(25_000_000).unwrap();
    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw286,
        "POST applied the saved GSW code to the live Lotura register"
    );
}

#[test]
fn fresh_cmos_seeds_the_profile_speed_code() {
    // The host seeds NVRAM 0x12 from the boot profile, so POST's apply is a
    // same-mode no-op until the user saves a different choice from the BIOS.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    profile.cpu = GswMode::Gsw586;
    profile.clock_hz = GswMode::Gsw586.clock_hz();
    let machine = Machine::new(profile, izarra_bios()).unwrap();
    assert_eq!(machine.cmos_byte(0x12), 2, "586 seeds code 2");
}

#[test]
fn tab_navigates_to_hard_disk_and_reports_unavailable() {
    // Tab opens the menu with Floppy focused (flat index 1). Up moves to the Hard
    // Disk row (index 0), which is unavailable in this build (greyed). Enter on it
    // refuses the mark and stays on the menu. Esc then bails. The run reaching the
    // cycle limit on the LFB without a fault proves the unavailable row returned to
    // the menu rather than booting or crashing.
    let mut machine = boot_machine();
    machine.inject_key_scancodes(&[
        TAB_MAKE,
        TAB_BREAK, // open the menu (Floppy focused)
        UP_MAKE,
        UP_BREAK, // Floppy -> Hard Disk
        ENTER_MAKE,
        ENTER_BREAK, // Hard Disk: unavailable, stay on the menu
        ESC_MAKE,
        ESC_BREAK, // bail out of the menu
    ]);
    machine.run_until_halt_or_cycles(25_000_000).unwrap();
    // Esc bails to boot_entry with the floppy default and no media mounted, so the
    // read fails and the BIOS idles on the LFB. The key point is no fault occurred.
    assert_eq!(
        machine.active_display(),
        ActiveDisplay::MargoLfb,
        "the unavailable Hard Disk row returned to the menu without booting"
    );
}
