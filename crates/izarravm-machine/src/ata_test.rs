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

fn advance_to_deadline(disk: &mut AtaDisk) {
    let ticks = disk.ticks_until_completion().expect("timed ATA command");
    assert!(ticks > 0);
    disk.advance_master_ticks(ticks);
}

#[test]
fn bios_and_pio_share_the_media_transfer_deadline() {
    assert_eq!(pio_transfer_ticks(0), 0);
    assert_eq!(
        pio_transfer_ticks(3),
        COMMAND_LATENCY_TICKS + 3 * pio_sector_ticks()
    );

    let mut disk = marked_disk(8);
    program_lba(&mut disk, 1, 1);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x20);
    assert_eq!(disk.ticks_until_completion(), Some(pio_transfer_ticks(1)));
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
    assert_eq!(disk.status, status::BSY);
    advance_to_deadline(&mut disk);
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
    assert_ne!(word(49) & (1 << 9), 0, "LBA is advertised");
    assert_ne!(word(49) & (1 << 8), 0, "DMA is advertised");
    assert_eq!(word(53), 0x0005, "CHS and UDMA words are valid");
    assert_eq!(word(63) & 0x0707, 0x0007, "MWDMA 0-2 supported");
    assert_eq!(word(88) & 0x0707, 0x0407, "UDMA2 supported and selected");
    let lba = u32::from(word(60)) | (u32::from(word(61)) << 16);
    assert_eq!(lba, disk_sectors as u32);
    // The drain dropped DRQ and returned to ready.
    assert_eq!(disk.status & status::DRQ, 0);
    assert_eq!(disk.status & status::DRDY, status::DRDY);
    // Completion raised the IRQ.
    assert!(disk.take_irq());
}

#[test]
fn set_features_selects_only_supported_transfer_modes() {
    let mut disk = marked_disk(8);
    disk.write_port(PRIMARY_CMD_BASE + 1, 0x03);
    disk.write_port(PRIMARY_CMD_BASE + 2, 0x21);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xef);
    advance_to_deadline(&mut disk);
    assert_eq!(disk.dma_mode, DmaMode::Multiword(1));
    assert_eq!(disk.status & status::ERR, 0);

    disk.write_port(PRIMARY_CMD_BASE + 1, 0x03);
    disk.write_port(PRIMARY_CMD_BASE + 2, 0x43);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xef);
    advance_to_deadline(&mut disk);
    assert_eq!(disk.status & status::ERR, status::ERR);
    assert_eq!(disk.error, error::ABRT);

    disk.write_port(PRIMARY_CMD_BASE + 1, 0x03);
    disk.write_port(PRIMARY_CMD_BASE + 2, 0x01);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xef);
    advance_to_deadline(&mut disk);
    assert_eq!(disk.dma_mode, DmaMode::None);
    assert_eq!(disk.status & status::ERR, 0);
}

#[test]
fn pio_write_then_read_round_trips_a_sector() {
    let mut disk = marked_disk(64);
    // WRITE one sector at LBA 5 with a recognizable pattern.
    program_lba(&mut disk, 5, 1);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x30); // WRITE SECTORS
    assert_eq!(disk.status, status::BSY);
    advance_to_deadline(&mut disk);
    assert_eq!(disk.status & status::DRQ, status::DRQ);
    let mut pattern = [0u8; SECTOR];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (i as u8) ^ 0xA5;
    }
    for b in pattern {
        disk.write_port(PRIMARY_CMD_BASE, b);
    }
    assert_eq!(disk.status, status::BSY);
    advance_to_deadline(&mut disk);
    // The write completed: DRQ dropped, IRQ raised.
    assert_eq!(disk.status & status::DRQ, 0);
    assert!(disk.take_irq());
    assert!(disk.dirty);

    // READ it back through the data port.
    program_lba(&mut disk, 5, 1);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x20); // READ SECTORS
    assert_eq!(disk.status, status::BSY);
    advance_to_deadline(&mut disk);
    assert_eq!(disk.status & status::DRQ, status::DRQ);
    let mut out = [0u8; SECTOR];
    for slot in out.iter_mut() {
        *slot = disk.read_port(PRIMARY_CMD_BASE).unwrap();
    }
    assert_eq!(out, pattern);
    assert_eq!(disk.status & status::DRQ, 0);
}

#[test]
fn multi_sector_write_reports_each_committed_sector() {
    let mut disk = marked_disk(8);
    program_lba(&mut disk, 2, 2);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x30);
    advance_to_deadline(&mut disk);
    assert_eq!(disk.status & status::DRQ, status::DRQ);
    assert!(!disk.take_irq(), "the initial write DRQ is polled");

    for byte in [0x5a; SECTOR] {
        disk.write_port(PRIMARY_CMD_BASE, byte);
    }
    assert_eq!(disk.status, status::BSY);
    advance_to_deadline(&mut disk);
    assert_eq!(disk.status & status::DRQ, status::DRQ);
    assert!(disk.take_irq(), "sector one committed");

    for byte in [0xa5; SECTOR] {
        disk.write_port(PRIMARY_CMD_BASE, byte);
    }
    advance_to_deadline(&mut disk);
    assert_eq!(disk.status & status::DRQ, 0);
    assert!(disk.take_irq(), "sector two committed");
    assert_eq!(disk.read_lba(2).unwrap(), [0x5a; SECTOR]);
    assert_eq!(disk.read_lba(3).unwrap(), [0xa5; SECTOR]);
}

#[test]
fn pio_deadlines_are_batch_invariant() {
    let mut whole = marked_disk(8);
    let mut split = marked_disk(8);
    program_lba(&mut whole, 3, 1);
    program_lba(&mut split, 3, 1);
    whole.write_port(PRIMARY_CMD_BASE + 7, 0x20);
    split.write_port(PRIMARY_CMD_BASE + 7, 0x20);
    let deadline = whole.ticks_until_completion().unwrap();

    whole.advance_master_ticks(deadline);
    split.advance_master_ticks(deadline / 3);
    split.advance_master_ticks(deadline / 5);
    split.advance_master_ticks(deadline - deadline / 3 - deadline / 5);

    assert_eq!(whole.status, split.status);
    assert_eq!(whole.phase, split.phase);
    assert_eq!(whole.buffer, split.buffer);
    assert_eq!(whole.irq_pending, split.irq_pending);
}

#[test]
fn read_past_end_aborts() {
    let mut disk = marked_disk(8);
    program_lba(&mut disk, 8, 1); // LBA 8 on an 8-sector disk
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x20);
    assert_eq!(disk.status, status::BSY);
    advance_to_deadline(&mut disk);
    assert_eq!(disk.status & status::ERR, status::ERR);
    assert_eq!(disk.error, error::ABRT);
    assert_eq!(disk.status & status::DRQ, 0);
}

#[test]
fn dma_command_rejects_an_off_media_range_or_pio_only_mode() {
    let mut disk = marked_disk(8);
    program_lba(&mut disk, 8, 1);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xc8);
    assert_eq!(disk.status & status::ERR, status::ERR);
    assert_eq!(disk.pending_dma(), None);

    disk.write_port(PRIMARY_CMD_BASE + 1, 0x03);
    disk.write_port(PRIMARY_CMD_BASE + 2, 0x01);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xef);
    advance_to_deadline(&mut disk);
    program_lba(&mut disk, 0, 1);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xc8);
    assert_eq!(disk.status & status::ERR, status::ERR);
    assert_eq!(disk.pending_dma(), None);
}

#[test]
fn read_and_write_dma_retry_opcodes_arm_the_same_requests() {
    for (command, direction) in [
        (0xc8, AtaDmaDirection::DeviceToMemory),
        (0xc9, AtaDmaDirection::DeviceToMemory),
        (0xca, AtaDmaDirection::MemoryToDevice),
        (0xcb, AtaDmaDirection::MemoryToDevice),
    ] {
        let mut disk = marked_disk(8);
        program_lba(&mut disk, 2, 1);
        disk.write_port(PRIMARY_CMD_BASE + 7, command);
        assert_eq!(disk.pending_dma().unwrap().direction, direction);
        assert_eq!(disk.status & status::BSY, status::BSY);
        assert_eq!(disk.status & status::DRQ, 0);
    }
}

#[test]
fn slave_select_aborts_commands() {
    let mut disk = marked_disk(8);
    disk.write_port(PRIMARY_CMD_BASE + 6, 0x10); // select slave (drive bit 4)
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xEC); // IDENTIFY to the absent slave
    advance_to_deadline(&mut disk);
    assert_eq!(disk.status & status::ERR, status::ERR);
    assert_eq!(disk.error, error::ABRT);
}

#[test]
fn nien_suppresses_the_irq() {
    let mut disk = marked_disk(8);
    disk.write_port(PRIMARY_CTRL, 0x02); // nIEN
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x90); // EXECUTE DEVICE DIAGNOSTIC
    advance_to_deadline(&mut disk);
    assert!(!disk.take_irq());
}

#[test]
fn non_data_command_becomes_ready_on_its_master_tick() {
    let mut disk = marked_disk(8);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x90);
    assert_eq!(disk.status, status::BSY);
    let deadline = disk.ticks_until_completion().unwrap();
    disk.advance_master_ticks(deadline - 1);
    assert_eq!(disk.status, status::BSY);
    assert!(!disk.take_irq());
    disk.advance_master_ticks(1);
    assert_eq!(disk.status, status::DRDY | status::DSC);
    assert_eq!(disk.error, 0x01);
    assert!(disk.take_irq());
}

#[test]
fn sector_count_zero_means_256() {
    let mut disk = marked_disk(300);
    program_lba(&mut disk, 0, 0); // count 0 -> 256 sectors
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x20); // READ SECTORS
    for _ in 0..256 {
        advance_to_deadline(&mut disk);
        assert_eq!(disk.status & status::DRQ, status::DRQ);
        for _ in 0..SECTOR {
            disk.read_port(PRIMARY_CMD_BASE);
        }
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
    advance_to_deadline(&mut disk);
    let first = disk.read_port(PRIMARY_CMD_BASE).unwrap();
    assert_eq!(first, 63u8.wrapping_add(0x10));
}

#[test]
fn initialize_device_parameters_acks() {
    let mut disk = marked_disk(8);
    disk.write_port(PRIMARY_CMD_BASE + 2, 63); // sectors per track
    disk.write_port(PRIMARY_CMD_BASE + 6, 0x0F); // 15+1 = 16 heads
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x91); // INITIALIZE DEVICE PARAMETERS
    advance_to_deadline(&mut disk);
    assert_eq!(disk.status & status::ERR, 0);
    assert_eq!(disk.status & status::DRDY, status::DRDY);
    assert!(disk.take_irq());
}

#[test]
fn initialized_geometry_translates_task_file_chs() {
    let mut disk = marked_disk(64);
    disk.write_port(PRIMARY_CMD_BASE + 2, 4); // four sectors per track
    disk.write_port(PRIMARY_CMD_BASE + 6, 0x01); // head 1 means two heads
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x91);
    advance_to_deadline(&mut disk);
    assert!(disk.take_irq());

    // CHS(1,1,1) maps through the programmed 2-head, 4-sector geometry to LBA 12.
    disk.write_port(PRIMARY_CMD_BASE + 2, 1);
    disk.write_port(PRIMARY_CMD_BASE + 3, 1);
    disk.write_port(PRIMARY_CMD_BASE + 4, 1);
    disk.write_port(PRIMARY_CMD_BASE + 5, 0);
    disk.write_port(PRIMARY_CMD_BASE + 6, 0x01);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x20);
    advance_to_deadline(&mut disk);
    assert_eq!(disk.read_port(PRIMARY_CMD_BASE), Some(12 + 0x10));
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
