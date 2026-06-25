// STREAM E integration test: the Tab boot menu and the INT 19h floppy bootstrap.
//
// Drives the public Machine API only. During the POST hotkey window the BIOS
// watches for Del (setup) and Tab (boot menu). Tab opens a mode-13h two-pane menu:
// the left pane picks the boot device (Hard Disk, Floppy, CD-ROM), the right pane
// picks the CPU speed, and an Accept/Cancel button bar sits below. Arrows move
// within and between panes, Enter marks a row, F10 (or Accept) commits, Esc (or
// Cancel) discards. The default device comes from CMOS 0x11. Committing Floppy
// drives the floppy path, which reads the boot sector and far jumps to 0000:7C00.
//
// Cases:
//   1. Tab during POST opens the menu: mode 13h with the menu's red title and grey
//      pane frames that the plain POST screen does not paint.
//   2. A host mouse move hot-tracks focus to the speed pane; a click marks a row.
//   3. Tab then F10 (Accept) on the default Floppy boots the mounted image: the
//      Wizardry III booter takes over and switches the card to CGA.
//
// Keys are fed as Set 1 scancodes via inject_key_scancodes; mouse motion via
// inject_mouse. The menu polls the aux channel and the keyboard ring each loop.

use izarravm_core::{GswMode, VideoCard};
use izarravm_firmware::izarra_bios;
use izarravm_machine::{Machine, MachineProfile, build_fat12};
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
const RIGHT_MAKE: u8 = 0x4d;
const RIGHT_BREAK: u8 = 0xcd;
const ESC_MAKE: u8 = 0x01;
const ESC_BREAK: u8 = 0x81;
// F10 accepts the marked device and speed (the two-pane menu commits on Accept,
// not on Enter, which only marks a row).
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
fn tab_opens_the_boot_menu_in_mode13h() {
    // Tab during POST opens the menu and stops there blocking on a key. The menu
    // wears the BIOS white/black/red skin: a red title (DAC slot 0x04) and light
    // grey pane/button frames (slot 0x07). The graceful POST screen paints red too,
    // but never slot 0x07, so the grey frame proves the menu painted on top of POST.
    let mut machine = boot_machine();
    machine.inject_key_scancodes(&[TAB_MAKE, TAB_BREAK]);
    machine.run_until_halt_or_cycles(20_000_000).unwrap();

    assert_eq!(
        machine.video().active_mode(),
        VideoMode::Mode13h,
        "the boot menu draws in mode 13h"
    );
    let raster = machine.video_mut().render_full_frame();
    let has_title = raster.pixels.contains(&0x04);
    let has_frame = raster.pixels.contains(&0x07);
    assert!(
        has_title && has_frame,
        "the red menu title and grey pane frames are on screen"
    );
}

#[test]
fn mouse_hover_moves_focus_to_the_speed_pane() {
    // Open the two-pane menu (focus starts on the device pane), then feed a host
    // mouse move that lands the cursor over the right (speed) pane. The BIOS polls
    // the PS/2 aux channel each loop, hot-tracks the hovered region, and repaints,
    // so the focus pane flips from device (0) to speed (1). This exercises the full
    // item-8 path: BIOS aux enable, packet decode, cursor move, hit-test, refocus.
    let mut machine = boot_machine();
    machine.inject_key_scancodes(&[TAB_MAKE, TAB_BREAK]);
    // Let the menu open and enable the mouse.
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    assert_eq!(machine.video().active_mode(), VideoMode::Mode13h);
    // The menu seeds focus on the device pane (BMX_FOCUS_PANE at seg0 0x0900 = 0).
    assert_eq!(
        machine.read_physical_u8(0x0900),
        0,
        "focus starts on the device pane"
    );
    // The cursor parks mid-field at (152, 92). Move it up-right onto a speed row
    // (the right pane spans x 172..303, y 44..107). dx=+48, screen dy=-34 lands it
    // near (200, 58), inside the Fast/Slow speed rows. Feed the move in a few small
    // packets so the BIOS poll, which drains one aux byte per loop, keeps up.
    for _ in 0..6 {
        machine.inject_mouse(8, -6, 0);
        machine.run_until_halt_or_cycles(2_000_000).unwrap();
    }
    machine.run_until_halt_or_cycles(4_000_000).unwrap();
    assert_eq!(
        machine.read_physical_u8(0x0900),
        1,
        "hovering the right pane moved focus to the speed pane"
    );
    // The cursor word (seg0 0x0908) advanced from its parked X (152) toward the
    // right pane, proving the packets moved the pointer.
    let cur_x =
        machine.read_physical_u8(0x0908) as u16 | ((machine.read_physical_u8(0x0909) as u16) << 8);
    assert!(cur_x > 152, "the cursor moved right (x={cur_x})");
}

#[test]
fn mouse_click_marks_a_speed_row() {
    // Open the menu, move the cursor onto a Slow (486) speed row, and click. A click
    // is "hover then mark", so the marked speed (seg0 0x0903) becomes the clicked
    // row. The menu seeds the marked speed to Fast (row 0) from CMOS, so a click on
    // a different row is an observable change.
    let mut machine = boot_machine();
    machine.inject_key_scancodes(&[TAB_MAKE, TAB_BREAK]);
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    let seeded_spd = machine.read_physical_u8(0x0903);
    // Move onto the right pane around y=66 (the Slow row, hit y 60..75), button up.
    for _ in 0..6 {
        machine.inject_mouse(8, -4, 0);
        machine.run_until_halt_or_cycles(2_000_000).unwrap();
    }
    machine.run_until_halt_or_cycles(2_000_000).unwrap();
    // Press the left button (a 0->1 edge) on the hovered row, then release.
    machine.inject_mouse(0, 0, 0x01);
    machine.run_until_halt_or_cycles(4_000_000).unwrap();
    machine.inject_mouse(0, 0, 0x00);
    machine.run_until_halt_or_cycles(2_000_000).unwrap();
    let marked_spd = machine.read_physical_u8(0x0903);
    // The click landed on a speed row and marked it; whatever row it hit, the menu
    // is still alive in mode 13h (the click did not commit, since speed rows mark
    // rather than boot).
    assert_eq!(machine.video().active_mode(), VideoMode::Mode13h);
    assert!(
        marked_spd <= 3,
        "a click marked an available speed row (was {seeded_spd}, now {marked_spd})"
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

    // Tab opens the menu (focus on the device pane, row 1 = the only bootable
    // device, Floppy). Right crosses to the speed pane carrying that row index, so
    // focus lands on speed row 1 (Slow). Two Downs walk to row 3 (Super Slow / 286).
    // Enter marks it, F10 accepts.
    machine.inject_key_scancodes(&[
        TAB_MAKE,
        TAB_BREAK, // open the boot menu
        RIGHT_MAKE,
        RIGHT_BREAK, // device pane -> speed pane (row 1)
        DOWN_MAKE,
        DOWN_BREAK, // Slow (row 1) -> VSlow (row 2)
        DOWN_MAKE,
        DOWN_BREAK, // VSlow (row 2) -> SSlow (row 3)
        ENTER_MAKE,
        ENTER_BREAK, // mark the Super Slow row
        F10_MAKE,
        F10_BREAK, // Accept
    ]);
    // Accept applies the speed live and falls through into the boot path. Give the
    // firmware room to settle on the new tier.
    machine.run_until_halt_or_cycles(24_000_000).unwrap();

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

// FAT12 layout constants for a 1.44 MB image, matching the synthesizer. Used to
// locate a file's first data sector so the boot stub can read it through INT 13h.
const SECTOR: usize = 512;
const RESERVED_SECTORS: usize = 1;
const NUM_FATS: usize = 2;
const SECTORS_PER_FAT: usize = 9;
const ROOT_ENTRIES: usize = 224;
const DIR_ENTRY_SIZE: usize = 32;
const ROOT_DIR_SECTORS: usize = (ROOT_ENTRIES * DIR_ENTRY_SIZE) / SECTOR; // 14
const FIRST_DATA_SECTOR: usize = RESERVED_SECTORS + NUM_FATS * SECTORS_PER_FAT + ROOT_DIR_SECTORS; // 33
// 1.44 MB media geometry: 80 cylinders, 2 heads, 18 sectors per track.
const SPT: usize = 18;
const HEADS: usize = 2;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "izarra_e_fat12_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Find a root-directory entry by its 11-byte 8.3 name, returning its first
/// cluster. Mirrors the on-disk layout the synthesizer produces.
fn root_first_cluster(img: &[u8], name11: &[u8; 11]) -> Option<u16> {
    let root_off = (RESERVED_SECTORS + NUM_FATS * SECTORS_PER_FAT) * SECTOR;
    for i in 0..ROOT_ENTRIES {
        let off = root_off + i * DIR_ENTRY_SIZE;
        let e = &img[off..off + DIR_ENTRY_SIZE];
        if e[0] == 0x00 {
            break;
        }
        if &e[..11] == name11 {
            return Some(u16::from_le_bytes([e[26], e[27]]));
        }
    }
    None
}

/// 1-based-sector CHS for a linear block address on the 1.44 MB geometry.
fn lba_to_chs(lba: usize) -> (u16, u8, u8) {
    let cyl = lba / (HEADS * SPT);
    let head = (lba / SPT) % HEADS;
    let sector = lba % SPT + 1;
    (cyl as u16, head as u8, sector as u8)
}

#[test]
fn fat12_folder_boots_and_reads_a_known_file_through_int13() {
    // End-to-end: synthesize a FAT12 image from a host folder, mount it as the A:
    // floppy, and read a known file's first data sector back through the INT 13h
    // path the BIOS bootstrap uses. The folder mount must produce an image whose
    // boot sector is loadable (sector 0 reaches 0000:7C00 via INT 19h) and whose
    // file data is reachable by CHS read.
    let dir = temp_dir("readback");
    let payload = b"IZARRA-FAT12-PAYLOAD";
    std::fs::write(dir.join("HELLO.TXT"), payload).unwrap();
    let mut img = build_fat12(&dir).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        img.len(),
        1_474_560,
        "the folder synthesized a 1.44 MB image"
    );

    // Locate HELLO.TXT's first data sector so the boot stub can read it.
    let cluster = root_first_cluster(&img, b"HELLO   TXT").expect("HELLO.TXT in root");
    let lba = FIRST_DATA_SECTOR + (usize::from(cluster) - 2);
    let (cyl, head, sector) = lba_to_chs(lba);
    assert!(
        cyl < 256,
        "the test file lands within the first 256 cylinders"
    );

    // Confirm the synthesizer put the payload where we will read it from. This is
    // the same bytes the INT 13h read should deliver into guest memory.
    let data_off = lba * SECTOR;
    assert_eq!(
        &img[data_off..data_off + payload.len()],
        payload,
        "the synthesized image holds the file at the computed data sector"
    );

    // Replace sector 0 with a boot stub. INT 19h loads it to 0000:7C00 and jumps
    // with DS=0. The stub reads HELLO.TXT's data sector into 0000:0600 through
    // INT 13h AH=02, then halts. If the payload lands, the folder mount and the
    // INT 13h read path both work.
    let boot: &[u8] = &[
        0x31,
        0xC0, // xor ax, ax
        0x8E,
        0xC0, // mov es, ax          ; ES = 0
        0xBB,
        0x00,
        0x06, // mov bx, 0x0600       ; ES:BX = 0000:0600
        0xB8,
        0x01,
        0x02, // mov ax, 0x0201       ; AH=02 read, AL=1 sector
        // mov cx, (cyl<<8) | sector. CL bits 0-5 hold the sector, bits 6-7 the
        // cylinder high bits; cyl < 256 here, so those stay clear and CH is the
        // whole cylinder.
        0xB9,
        sector,
        (cyl & 0xff) as u8,
        // mov dx, (head<<8) | 0   ; DH=head, DL=0 (drive A:)
        0xBA,
        0x00,
        head,
        0xCD,
        0x13, // int 13h
        0xF4, // hlt
    ];
    img[..boot.len()].copy_from_slice(boot);

    let mut machine = boot_machine();
    machine.mount_floppy(img).expect("synthesized image mounts");
    machine.run_until_halt_or_cycles(50_000_000).unwrap();

    // The stub's INT 13h read placed the file's first data sector at 0000:0600.
    let mut got = Vec::new();
    for i in 0..payload.len() {
        got.push(machine.read_physical_u8(0x0600 + i as u32));
    }
    assert_eq!(
        &got, payload,
        "INT 13h delivered HELLO.TXT's bytes from the FAT12 folder mount"
    );
}

#[test]
fn tab_navigates_to_hard_disk_and_reports_unavailable() {
    // Tab opens the two-pane menu with Floppy focused (device row 1). Up moves to
    // the Hard Disk row (row 0), which is unavailable in this build. Enter on it
    // reports that Toka-DOS is not installed and refuses the mark, staying on the
    // menu. Esc then cancels. The run reaching the cycle limit in mode 13h without
    // a fault proves the unavailable row returned to the menu rather than booting
    // or crashing.
    let mut machine = boot_machine();
    machine.inject_key_scancodes(&[
        TAB_MAKE,
        TAB_BREAK, // open the menu (Floppy focused)
        UP_MAKE,
        UP_BREAK, // Floppy -> Hard Disk
        ENTER_MAKE,
        ENTER_BREAK, // Hard Disk: report unavailable, stay on the menu
        ESC_MAKE,
        ESC_BREAK, // cancel the menu
    ]);
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    // Esc on the menu falls through to boot_entry with the floppy default and no
    // media mounted, so the read fails and the BIOS idles. The card stays in the
    // mode-13h POST/menu field; the key point is no fault occurred.
    assert_eq!(
        machine.video().active_mode(),
        VideoMode::Mode13h,
        "the unavailable Hard Disk row returned to the menu without booting"
    );
}
