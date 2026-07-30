// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

// Integration test for the Tab boot menu and the INT 19h floppy bootstrap.
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
//   3. Walking down to the 386-slow row and accepting switches the CPU tier live.
//   4. HDD/CD device rows become markable only when their firmware probes pass.
//
// Keys are fed as Set 1 scancodes via inject_key_scancodes. Navigation sends
// one make/break pair at a time so each pair crosses the timed PS/2 wire at the
// menu state where it is intended.

use izarravm_core::{GswMode, VideoCard};
use izarravm_firmware::izarra_bios;
use izarravm_machine::{
    ActiveDisplay, CdImage, MARGO_LFB_BASE, Machine, MachineProfile, VideoMode,
};

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
const BOOT_CHOICE_ADDR: u32 = 0x0537;

fn bootable_cd() -> CdImage {
    const SECTOR: usize = 2048;
    let mut iso = vec![0u8; 32 * SECTOR];
    let record = 17 * SECTOR;
    iso[record] = 0;
    iso[record + 1..record + 6].copy_from_slice(b"CD001");
    iso[record + 6] = 1;
    iso[record + 7..record + 30].copy_from_slice(b"EL TORITO SPECIFICATION");
    iso[record + 71..record + 75].copy_from_slice(&18u32.to_le_bytes());

    let catalog = 18 * SECTOR;
    iso[catalog] = 1;
    iso[catalog + 30] = 0x55;
    iso[catalog + 31] = 0xAA;
    let validation_sum = (0..16).fold(0u16, |sum, word| {
        let at = catalog + word * 2;
        sum.wrapping_add(u16::from_le_bytes([iso[at], iso[at + 1]]))
    });
    iso[catalog + 28..catalog + 30].copy_from_slice(&validation_sum.wrapping_neg().to_le_bytes());
    iso[catalog + 32] = 0x88;
    iso[catalog + 33] = 0; // no emulation
    iso[catalog + 34..catalog + 36].copy_from_slice(&0x2000u16.to_le_bytes());
    iso[catalog + 38..catalog + 40].copy_from_slice(&1u16.to_le_bytes());
    iso[catalog + 40..catalog + 44].copy_from_slice(&20u32.to_le_bytes());

    let boot = 20 * SECTOR;
    iso[boot..boot + 12].copy_from_slice(&[
        0xBB, 0x00, 0x05, // mov bx,0500h
        0xB0, 0x43, // mov al,43h
        0x88, 0x07, // mov [bx],al
        0x88, 0x57, 0x01, // mov [bx+1],dl
        0xF4, 0x90, // hlt; nop
    ]);
    CdImage::from_iso(iso).unwrap()
}

fn signed_cga_boot_floppy() -> Vec<u8> {
    let mut image = vec![0u8; 737_280];
    image[..8].copy_from_slice(&[
        0xb8, 0x04, 0x00, // mov ax,0004h
        0xcd, 0x10, // int 10h
        0xfa, // cli
        0xf4, // hlt
        0x90, // nop
    ]);
    image[510] = 0x55;
    image[511] = 0xaa;
    image
}

fn boot_machine() -> Machine {
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    Machine::new(profile, izarra_bios()).unwrap()
}

fn press(machine: &mut Machine, make_code: u8, break_code: u8, clocks: u64) {
    machine.inject_key_scancodes(&[make_code, break_code]);
    machine.run_until_halt_or_cycles(clocks).unwrap();
}

fn open_boot_menu(machine: &mut Machine) {
    press(machine, TAB_MAKE, TAB_BREAK, 25_000_000);
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
fn tab_then_accept_boots_a_signed_floppy() {
    let mut machine = boot_machine();
    machine
        .mount_floppy(signed_cga_boot_floppy())
        .expect("720 KB image mounts");
    open_boot_menu(&mut machine);
    press(&mut machine, F10_MAKE, F10_BREAK, 3_000_000);
    // The floppy now takes realistic mechanical time (seek + rotational latency +
    // ~31 KB/s for this 720 KB disk), so loading the booter's stage 2 burns far
    // more guest cycles than an instant read did. Run in chunks and stop as soon
    // as the booter switches to CGA, rather than burning a fixed huge budget.
    let mut reached_cga = false;
    for _ in 0..60 {
        machine.run_until_halt_or_cycles(10_000_000).unwrap();
        if machine.active_video_mode() == VideoMode::Cga {
            reached_cga = true;
            break;
        }
    }
    assert!(
        reached_cga,
        "the signed boot sector ran and switched to CGA"
    );
}

#[test]
fn accept_super_slow_commits_the_386_slow_tier() {
    // Open the menu, cross to the speed pane, walk down to the 386-slow row,
    // mark it with Enter, then Accept with F10. The Accept maps the marked row to GSW
    // code 3 and writes it to the live Lotura register (port 0xE1) and to CMOS 0x12.
    // The 0xE1 write is a live switch (no cold reset), so the firmware keeps running
    // at the new speed; active_mode() reads back 386-slow and CMOS 0x12 holds 3,
    // mirroring the other speed tiers.
    let mut machine = boot_machine();
    assert_eq!(machine.active_mode(), GswMode::Gsw386, "boot mode is 386");

    // Tab opens the menu with focus on the marked device (flat index 1 = Floppy).
    // The flat list is dev0..dev2 (0..2), spd0..spd3 (3..6), Accept (7), so five
    // Downs walk from Floppy (1) to the 386-slow row (focus 6 = speed row 3).
    // Enter marks it, F10 accepts.
    open_boot_menu(&mut machine);
    for _ in 0..5 {
        press(&mut machine, DOWN_MAKE, DOWN_BREAK, 3_000_000);
    }
    press(&mut machine, ENTER_MAKE, ENTER_BREAK, 3_000_000);
    press(&mut machine, F10_MAKE, F10_BREAK, 20_000_000);

    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw386Slow,
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
    // 3 (386-slow) retunes the machine during POST bring-up, even though the host
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
        GswMode::Gsw386Slow,
        "POST applied the saved GSW code to the live Lotura register"
    );
}

#[test]
fn loaded_cmos_gsw_mode_is_active_before_post() {
    let mut saved = boot_machine();
    saved.set_cmos_byte(0x12, 3);
    let cmos = saved.cmos_bytes();

    let mut machine = boot_machine();
    assert_eq!(machine.active_mode(), GswMode::Gsw386);
    assert!(machine.load_cmos(&cmos));
    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw386Slow,
        "loading persisted CMOS should set the CPU mode before the BIOS runs"
    );
}

#[test]
fn fresh_cmos_seeds_the_profile_speed_code() {
    // The host seeds NVRAM 0x12 from the boot profile, so POST's apply is a
    // same-mode no-op until the user saves a different choice from the BIOS.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
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
    open_boot_menu(&mut machine);
    press(&mut machine, UP_MAKE, UP_BREAK, 3_000_000);
    press(&mut machine, ENTER_MAKE, ENTER_BREAK, 3_000_000);
    press(&mut machine, ESC_MAKE, ESC_BREAK, 25_000_000);
    // Esc bails to boot_entry with the floppy default and no media mounted, so the
    // read fails and the BIOS idles on the LFB. The key point is no fault occurred.
    assert_eq!(
        machine.active_display(),
        ActiveDisplay::MargoLfb,
        "the unavailable Hard Disk row returned to the menu without booting"
    );
}

#[test]
fn tab_selects_available_hard_disk_and_boots_it() {
    let mut machine = boot_machine();
    let mut img = vec![0u8; 512 * 4];
    let boot = [
        0xFA, // cli
        0xBB, 0x00, 0x05, // mov bx,0500h
        0xB0, 0x42, // mov al,42h
        0x88, 0x07, // mov [bx],al
        0x88, 0x57, 0x01, // mov [bx+1],dl
        0xF4, // hlt
    ];
    img[..boot.len()].copy_from_slice(&boot);
    img[510] = 0x55;
    img[511] = 0xAA;
    machine.mount_hdd(img);

    open_boot_menu(&mut machine);
    press(&mut machine, UP_MAKE, UP_BREAK, 3_000_000);
    press(&mut machine, F10_MAKE, F10_BREAK, 60_000_000);

    assert_eq!(machine.read_physical_u8(0x0500), 0x42, "the MBR ran");
    assert_eq!(
        machine.read_physical_u8(0x0501),
        0x80,
        "the MBR received DL=80h"
    );
    assert_eq!(
        machine.cmos_byte(0x11),
        1,
        "F10 persisted HDD as the primary device"
    );
}

#[test]
fn tab_selects_available_cd_rom_row() {
    let mut machine = boot_machine();
    machine.mount_cd(bootable_cd());
    open_boot_menu(&mut machine);
    press(&mut machine, DOWN_MAKE, DOWN_BREAK, 3_000_000);
    press(&mut machine, F10_MAKE, F10_BREAK, 35_000_000);

    assert_eq!(
        machine.read_physical_u8(BOOT_CHOICE_ADDR),
        2,
        "Accept committed CD-ROM as this session's boot device"
    );
    assert_eq!(machine.read_physical_u8(0x0500), 0x43, "the CD image ran");
    assert_eq!(
        machine.read_physical_u8(0x0501),
        0xE0,
        "no-emulation boot used DL=E0h"
    );
    assert_eq!(machine.cmos_byte(0x11), 2, "F10 persisted CD as primary");
}
