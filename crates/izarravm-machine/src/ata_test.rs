// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// A disk whose first byte of each sector is a marker derived from the LBA.
fn marked_disk(sectors: usize) -> AtaDisk {
    let mut bytes = vec![0u8; sectors * SECTOR];
    for s in 0..sectors {
        bytes[s * SECTOR] = (s as u8).wrapping_add(0x10);
    }
    AtaDisk::new(bytes)
}

/// Set up the task file for an LBA28 access at `lba` of `count` sectors.
fn program_lba(disk: &mut AtaDisk, lba: u32, count: u8) {
    disk.write_port(PRIMARY_CMD_BASE + 2, count); // sector count
    disk.write_port(PRIMARY_CMD_BASE + 3, lba as u8); // LBA 0-7
    disk.write_port(PRIMARY_CMD_BASE + 4, (lba >> 8) as u8); // LBA 8-15
    disk.write_port(PRIMARY_CMD_BASE + 5, (lba >> 16) as u8); // LBA 16-23
    disk.write_port(PRIMARY_CMD_BASE + 6, 0x40 | ((lba >> 24) as u8 & 0x0F)); // LBA mode + 24-27
}

#[test]
fn geometry_is_16_heads_63_spt() {
    // 16 * 63 = 1008 sectors per cylinder; 4032 sectors is 4 cylinders.
    let disk = marked_disk(4032);
    assert_eq!(disk.heads(), 16);
    assert_eq!(disk.sectors_per_track(), 63);
    assert_eq!(disk.cylinders(), 4);
    assert_eq!(disk.total_sectors(), 4032);
}

#[test]
fn chs_round_trips_to_lba() {
    let disk = marked_disk(4032);
    // CHS(0,0,1) is LBA 0; sector is 1-based.
    assert_eq!(disk.chs_to_lba(0, 0, 1), Some(0));
    // CHS(0,1,1) is the start of the second track: head 1 * 63 spt.
    assert_eq!(disk.chs_to_lba(0, 1, 1), Some(63));
    // CHS(1,0,1) is the start of the second cylinder: 16 heads * 63 spt.
    assert_eq!(disk.chs_to_lba(1, 0, 1), Some(16 * 63));
    // Sector 0 and an oversize sector are invalid.
    assert_eq!(disk.chs_to_lba(0, 0, 0), None);
    assert_eq!(disk.chs_to_lba(0, 0, 64), None);
}

#[test]
fn identify_round_trips_geometry() {
    let disk_sectors = 4032usize;
    let mut disk = marked_disk(disk_sectors);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xEC); // IDENTIFY DEVICE
    assert_eq!(disk.status & status::DRQ, status::DRQ);
    let mut block = Vec::with_capacity(512);
    for _ in 0..512 {
        block.push(disk.read_port(PRIMARY_CMD_BASE).unwrap());
    }
    let word = |i: usize| u16::from_le_bytes([block[i * 2], block[i * 2 + 1]]);
    assert_eq!(word(0), 0x0040); // fixed ATA device
    assert_eq!(word(1), 4); // cylinders
    assert_eq!(word(3), 16); // heads
    assert_eq!(word(6), 63); // sectors per track
    let lba = u32::from(word(60)) | (u32::from(word(61)) << 16);
    assert_eq!(lba, disk_sectors as u32);
    // The drain dropped DRQ and returned to ready.
    assert_eq!(disk.status & status::DRQ, 0);
    assert_eq!(disk.status & status::DRDY, status::DRDY);
    // Completion raised the IRQ.
    assert!(disk.take_irq());
}

#[test]
fn pio_write_then_read_round_trips_a_sector() {
    let mut disk = marked_disk(64);
    // WRITE one sector at LBA 5 with a recognizable pattern.
    program_lba(&mut disk, 5, 1);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x30); // WRITE SECTORS
    assert_eq!(disk.status & status::DRQ, status::DRQ);
    let mut pattern = [0u8; SECTOR];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (i as u8) ^ 0xA5;
    }
    for b in pattern {
        disk.write_port(PRIMARY_CMD_BASE, b);
    }
    // The write completed: DRQ dropped, IRQ raised.
    assert_eq!(disk.status & status::DRQ, 0);
    assert!(disk.take_irq());
    assert!(disk.dirty);

    // READ it back through the data port.
    program_lba(&mut disk, 5, 1);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x20); // READ SECTORS
    assert_eq!(disk.status & status::DRQ, status::DRQ);
    let mut out = [0u8; SECTOR];
    for slot in out.iter_mut() {
        *slot = disk.read_port(PRIMARY_CMD_BASE).unwrap();
    }
    assert_eq!(out, pattern);
    assert_eq!(disk.status & status::DRQ, 0);
}

#[test]
fn read_past_end_aborts() {
    let mut disk = marked_disk(8);
    program_lba(&mut disk, 8, 1); // LBA 8 on an 8-sector disk
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x20);
    assert_eq!(disk.status & status::ERR, status::ERR);
    assert_eq!(disk.error, error::ABRT);
    assert_eq!(disk.status & status::DRQ, 0);
}

#[test]
fn slave_select_aborts_commands() {
    let mut disk = marked_disk(8);
    disk.write_port(PRIMARY_CMD_BASE + 6, 0x10); // select slave (drive bit 4)
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xEC); // IDENTIFY to the absent slave
    assert_eq!(disk.status & status::ERR, status::ERR);
    assert_eq!(disk.error, error::ABRT);
}

#[test]
fn nien_suppresses_the_irq() {
    let mut disk = marked_disk(8);
    disk.write_port(PRIMARY_CTRL, 0x02); // nIEN
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x90); // EXECUTE DEVICE DIAGNOSTIC
    assert!(!disk.take_irq());
}

#[test]
fn sector_count_zero_means_256() {
    let mut disk = marked_disk(300);
    program_lba(&mut disk, 0, 0); // count 0 -> 256 sectors
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x20); // READ SECTORS
    // 256 sectors are buffered; draining them all returns to ready.
    assert_eq!(disk.status & status::DRQ, status::DRQ);
    for _ in 0..(256 * SECTOR) {
        disk.read_port(PRIMARY_CMD_BASE);
    }
    assert_eq!(disk.status & status::DRQ, 0);
}

#[test]
fn chs_addressing_reads_the_right_sector() {
    let mut disk = marked_disk(4032);
    // CHS(0,1,1) is LBA 63; the marker there is 63 + 0x10.
    disk.write_port(PRIMARY_CMD_BASE + 2, 1); // count 1
    disk.write_port(PRIMARY_CMD_BASE + 3, 1); // sector number (1-based)
    disk.write_port(PRIMARY_CMD_BASE + 4, 0); // cylinder low
    disk.write_port(PRIMARY_CMD_BASE + 5, 0); // cylinder high
    disk.write_port(PRIMARY_CMD_BASE + 6, 0x01); // CHS mode (bit 6 clear), head 1
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x20); // READ SECTORS
    let first = disk.read_port(PRIMARY_CMD_BASE).unwrap();
    assert_eq!(first, 63u8.wrapping_add(0x10));
}

#[test]
fn initialize_device_parameters_acks() {
    let mut disk = marked_disk(8);
    disk.write_port(PRIMARY_CMD_BASE + 2, 63); // sectors per track
    disk.write_port(PRIMARY_CMD_BASE + 6, 0x0F); // 15+1 = 16 heads
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x91); // INITIALIZE DEVICE PARAMETERS
    assert_eq!(disk.status & status::ERR, 0);
    assert_eq!(disk.status & status::DRDY, status::DRDY);
    assert!(disk.take_irq());
}

/// A tiny host-folder-backed disk: a real KateaTreeVolume over a temp folder.
fn host_folder_disk(tag: &str) -> (AtaDisk, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("katea_ata_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("A.TXT"), b"hi").unwrap();
    let img = izarravm_firmware::tokados_hdd_img();
    let mut mbr = [0u8; 512];
    mbr.copy_from_slice(&img[0..512]);
    let mut vbr = [0u8; 512];
    vbr.copy_from_slice(&img[2048 * 512..2048 * 512 + 512]);
    let sys = vec![("KERNEL.SYS".to_string(), vec![0xEBu8; 100])];
    let vol = crate::katea_tree::KateaTreeVolume::new(&mbr, &vbr, &dir, &sys).unwrap();
    (AtaDisk::from_host_folder(vol), dir)
}

#[test]
fn host_folder_write_lands_in_the_overlay_and_reads_back() {
    let (mut disk, dir) = host_folder_disk("rw");
    // Pick a free data sector well past the system area.
    let lba = 2048 + 32 + 741 * 2 + 5000;
    let mut data = [0u8; SECTOR];
    data[0] = 0x77;
    data[SECTOR - 1] = 0x88;
    assert!(disk.write_lba(lba, &data), "folder write now accepted");
    assert!(disk.dirty, "a folder write marks the disk dirty");
    let back = disk.read_lba(lba).expect("in range");
    assert_eq!(back[0], 0x77);
    assert_eq!(back[SECTOR - 1], 0x88);
    std::fs::remove_dir_all(&dir).ok();
}
