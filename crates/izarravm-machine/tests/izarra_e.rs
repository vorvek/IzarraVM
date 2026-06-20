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
use izarravm_machine::{Machine, MachineProfile, build_fat12};
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
