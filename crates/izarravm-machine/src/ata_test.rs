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
/// `host_folder_disk` plus one extra file, for the multi-sector ranges the
/// coalescing tests need. `A.TXT` stays exactly where it was so every existing
/// caller keeps its LBAs.
fn host_folder_disk_with(tag: &str, extra: (&str, &[u8])) -> (AtaDisk, std::path::PathBuf) {
    host_folder_disk_inner(tag, Some(extra))
}

fn host_folder_disk(tag: &str) -> (AtaDisk, std::path::PathBuf) {
    host_folder_disk_inner(tag, None)
}

fn host_folder_disk_inner(
    tag: &str,
    extra: Option<(&str, &[u8])>,
) -> (AtaDisk, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("katea_ata_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("A.TXT"), b"hi").unwrap();
    if let Some((name, bytes)) = extra {
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    let img = izarravm_firmware::tokados_hdd_img();
    let mut mbr = [0u8; 512];
    mbr.copy_from_slice(&img[0..512]);
    let mut vbr = [0u8; 512];
    vbr.copy_from_slice(&img[2048 * 512..2048 * 512 + 512]);
    let sys = vec![("KERNEL.SYS".to_string(), vec![0xEBu8; 100])];
    let vol = crate::katea_tree::KateaTreeVolume::new(&mbr, &vbr, &dir, &sys).unwrap();
    (AtaDisk::from_host_folder(vol), dir)
}

fn root_file_lba(disk: &AtaDisk, name: &[u8; 11]) -> u32 {
    let part_start = crate::katea_volume::PART_START;
    let vbr = disk.read_lba(part_start).unwrap();
    let spc = u32::from(vbr[0x0D]);
    let reserved = u32::from(u16::from_le_bytes([vbr[0x0E], vbr[0x0F]]));
    let fats = u32::from(vbr[0x10]);
    let fatsz = u32::from_le_bytes([vbr[0x24], vbr[0x25], vbr[0x26], vbr[0x27]]);
    let root_cluster = u32::from_le_bytes([vbr[0x2C], vbr[0x2D], vbr[0x2E], vbr[0x2F]]);
    let data_start = part_start + reserved + fats * fatsz;
    let root = disk
        .read_lba(data_start + (root_cluster - 2) * spc)
        .unwrap();
    let slot = (0..16)
        .map(|index| index * 32)
        .find(|&offset| &root[offset..offset + 11] == name)
        .unwrap();
    let first_cluster = (u32::from(u16::from_le_bytes([root[slot + 20], root[slot + 21]])) << 16)
        | u32::from(u16::from_le_bytes([root[slot + 26], root[slot + 27]]));
    data_start + (first_cluster - 2) * spc
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

#[test]
fn pio_command_projects_to_the_host_before_flush() {
    let (mut disk, dir) = host_folder_disk("pio_live");
    let lba = root_file_lba(&disk, b"A       TXT");
    let mut sector = disk.read_lba(lba).unwrap();
    sector[..2].copy_from_slice(b"PI");

    program_lba(&mut disk, lba, 1);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x30);
    advance_to_deadline(&mut disk);
    for byte in sector {
        disk.write_port(PRIMARY_CMD_BASE, byte);
    }
    advance_to_deadline(&mut disk);

    assert_eq!(std::fs::read(dir.join("A.TXT")).unwrap(), b"PI");
    let counters = disk.katea_storage_counters().unwrap();
    assert_eq!(counters.pio_write_commands, 1);
    assert_eq!(counters.pio_write_sectors, 1);
    assert_eq!(counters.pio_write_wait_ticks, pio_transfer_ticks(1));
    drop(disk);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn pio_read_reports_its_guest_visible_wait() {
    let (mut disk, dir) = host_folder_disk("pio_read_timing");
    let lba = root_file_lba(&disk, b"A       TXT");

    program_lba(&mut disk, lba, 1);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x20);
    advance_to_deadline(&mut disk);
    for _ in 0..SECTOR {
        disk.read_port(PRIMARY_CMD_BASE).unwrap();
    }

    let counters = disk.katea_storage_counters().unwrap();
    assert_eq!(counters.pio_read_commands, 1);
    assert_eq!(counters.pio_read_sectors, 1);
    assert_eq!(counters.pio_read_wait_ticks, pio_transfer_ticks(1));
    drop(disk);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dma_command_projects_to_the_host_before_flush() {
    let (mut disk, dir) = host_folder_disk("dma_live");
    let lba = root_file_lba(&disk, b"A       TXT");
    disk.write_port(PRIMARY_CMD_BASE + 1, 0x03);
    disk.write_port(PRIMARY_CMD_BASE + 2, 0x42);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xEF);
    advance_to_deadline(&mut disk);

    program_lba(&mut disk, lba, 1);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xCA);
    let wait_ticks = dma_transfer_ticks(disk.pending_dma().unwrap());
    let mut sector = disk.read_lba(lba).unwrap();
    sector[..2].copy_from_slice(b"DM");
    assert!(disk.complete_dma_write(&sector));

    assert_eq!(std::fs::read(dir.join("A.TXT")).unwrap(), b"DM");
    let counters = disk.katea_storage_counters().unwrap();
    assert_eq!(counters.dma_write_commands, 1);
    assert_eq!(counters.dma_write_sectors, 1);
    assert_eq!(counters.dma_write_wait_ticks, wait_ticks);
    drop(disk);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dma_read_reports_its_guest_visible_wait() {
    let (mut disk, dir) = host_folder_disk("dma_read_timing");
    let lba = root_file_lba(&disk, b"A       TXT");
    disk.write_port(PRIMARY_CMD_BASE + 1, 0x03);
    disk.write_port(PRIMARY_CMD_BASE + 2, 0x42);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xEF);
    advance_to_deadline(&mut disk);

    program_lba(&mut disk, lba, 1);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xC8);
    let wait_ticks = dma_transfer_ticks(disk.pending_dma().unwrap());
    let payload = disk.read_dma_payload().unwrap();
    disk.complete_dma_read(payload.len());

    let counters = disk.katea_storage_counters().unwrap();
    assert_eq!(counters.dma_read_commands, 1);
    assert_eq!(counters.dma_read_sectors, 1);
    assert_eq!(counters.dma_read_wait_ticks, wait_ticks);
    drop(disk);
    std::fs::remove_dir_all(&dir).ok();
}

/// `sectors` sectors whose every byte is the sector index or'd with 0x40, so a
/// sector served from the wrong offset is visible in its first byte alone.
fn patterned_ata(sectors: usize) -> Vec<u8> {
    (0..sectors * SECTOR)
        .map(|i| (i / SECTOR) as u8 | 0x40)
        .collect()
}

/// Arm a device-to-memory DMA request for `count` sectors at `lba`.
fn arm_dma_read(disk: &mut AtaDisk, lba: u32, count: u8) {
    disk.write_port(PRIMARY_CMD_BASE + 1, 0x03);
    disk.write_port(PRIMARY_CMD_BASE + 2, 0x42);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xEF);
    advance_to_deadline(disk);
    program_lba(disk, lba, count);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0xC8);
}

/// The first byte of each sector of a DMA payload.
fn payload_sector_heads(payload: &[u8]) -> Vec<u8> {
    payload.chunks(SECTOR).map(|s| s[0]).collect()
}

/// A BUS-MASTER DMA READ COALESCES ITS WHOLE CONTIGUOUS REQUEST.
///
/// `read_dma_payload` assembles every sector of the request inside one
/// host-side call, so unlike PIO it can declare the whole range. Four sectors
/// must cost one physical host read and still deliver each sector's own bytes.
///
/// NON-VACUOUS: deleting the `begin_read_command` call from `read_dma_payload`
/// makes the range undeclared, every sector falls back to a one-sector span,
/// and `host_read_operations` is 4 instead of 1.
#[test]
fn a_dma_read_coalesces_its_contiguous_request_into_one_host_read() {
    let (mut disk, dir) = host_folder_disk_with("dma_batch", ("BIG.DAT", &patterned_ata(4)));
    let lba = root_file_lba(&disk, b"BIG     DAT");
    arm_dma_read(&mut disk, lba, 4);

    let before = disk.katea_storage_counters().unwrap().host_read_operations;
    let payload = disk.read_dma_payload().unwrap();
    disk.complete_dma_read(payload.len());
    let after = disk.katea_storage_counters().unwrap().host_read_operations;

    assert_eq!(
        payload_sector_heads(&payload),
        vec![0x40, 0x41, 0x42, 0x43],
        "each sector still delivers its own bytes"
    );
    assert_eq!(
        after - before,
        1,
        "the whole four-sector request cost one physical host read"
    );
    let counters = disk.katea_storage_counters().unwrap();
    assert_eq!(counters.dma_read_commands, 1);
    assert_eq!(counters.dma_read_sectors, 4);
    drop(disk);
    std::fs::remove_dir_all(&dir).ok();
}

/// A HOST FAILURE PART-WAY THROUGH A DMA SPAN LOOKS THE SAME AS IT ALWAYS DID.
///
/// Batching changes how many host reads a span costs; it must not change what
/// the guest sees when one of them comes up short. The host file is cut to two
/// sectors while the directory still advertises four, so the back half of the
/// span fails inside a coalesced read rather than on its own.
///
/// The contract this pins: the transfer still COMPLETES (a short host file is
/// served as zeros, not an ABRT), the surviving sectors keep their real bytes,
/// the failed ones are zeros, and the reported sector count and wait are the
/// full request -- the same registers the per-sector path produced.
///
/// NON-VACUOUS, both directions, each applied and observed:
///
/// - Removing `begin_read_command`/`end_read_command` from `read_dma_payload`
///   leaves every assertion here passing and moves only `host_read_operations`.
///   That IS the claim: batching is invisible to the guest on the failure path.
/// - Discarding a short coalesced answer wholesale instead of keeping its
///   complete leading sectors (guarding `read_host_span`'s success arm with
///   `read_bytes >= read_len`) reads back `[0, 0, 0, 0]` instead of
///   `[64, 65, 0, 0]` -- the surviving sectors are lost.
#[test]
fn a_dma_span_that_fails_part_way_reports_what_the_per_sector_path_did() {
    let (mut disk, dir) = host_folder_disk_with("dma_short", ("BIG.DAT", &patterned_ata(4)));
    let lba = root_file_lba(&disk, b"BIG     DAT");

    // Two whole sectors survive; the directory still says four.
    std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join("BIG.DAT"))
        .unwrap()
        .set_len(2 * 512)
        .unwrap();

    arm_dma_read(&mut disk, lba, 4);
    let wait_ticks = dma_transfer_ticks(disk.pending_dma().unwrap());
    let payload = disk.read_dma_payload().expect(
        "a short host file is served as zeros, so the transfer completes rather \
         than aborting",
    );
    assert_eq!(
        payload.len(),
        4 * SECTOR,
        "the payload is still the full requested length"
    );
    assert_eq!(
        payload_sector_heads(&payload),
        vec![0x40, 0x41, 0x00, 0x00],
        "the sectors that survived the truncation keep their bytes; the rest \
         are the degraded zero fill"
    );
    disk.complete_dma_read(payload.len());

    let counters = disk.katea_storage_counters().unwrap();
    assert_eq!(counters.dma_read_commands, 1);
    assert_eq!(
        counters.dma_read_sectors, 4,
        "the guest asked for four and is charged for four; a host-side shortfall \
         is not a guest-visible short transfer"
    );
    assert_eq!(counters.dma_read_wait_ticks, wait_ticks);
    drop(disk);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn flush_cache_projects_pending_data_without_counting_a_write_command() {
    let (mut disk, dir) = host_folder_disk("flush_cache");
    let lba = root_file_lba(&disk, b"A       TXT");
    let mut sector = disk.read_lba(lba).unwrap();
    sector[..2].copy_from_slice(b"FC");
    assert!(disk.write_lba(lba, &sector));
    assert_eq!(std::fs::read(dir.join("A.TXT")).unwrap(), b"hi");

    disk.write_port(PRIMARY_CMD_BASE + 7, 0xE7);
    advance_to_deadline(&mut disk);
    assert_eq!(disk.status & status::ERR, 0);
    assert_eq!(std::fs::read(dir.join("A.TXT")).unwrap(), b"FC");
    let counters = disk.katea_storage_counters().unwrap();
    assert_eq!(counters.pio_write_commands, 0);
    assert!(counters.projection_operations > 0);
    drop(disk);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn pio_host_failure_aborts_the_guest_command_and_retains_the_overlay() {
    let (mut disk, dir) = host_folder_disk("pio_failure");
    let lba = root_file_lba(&disk, b"A       TXT");
    std::fs::remove_file(dir.join("A.TXT")).unwrap();
    std::fs::create_dir(dir.join("A.TXT")).unwrap();

    program_lba(&mut disk, lba, 1);
    disk.write_port(PRIMARY_CMD_BASE + 7, 0x30);
    advance_to_deadline(&mut disk);
    for byte in [0x66; SECTOR] {
        disk.write_port(PRIMARY_CMD_BASE, byte);
    }
    advance_to_deadline(&mut disk);

    assert_eq!(disk.status & status::ERR, status::ERR);
    assert_eq!(disk.error, error::ABRT);
    let counters = disk.katea_storage_counters().unwrap();
    assert_eq!(counters.pio_write_commands, 1);
    assert_eq!(counters.pio_write_sectors, 1);
    assert!(counters.overlay_pending_sectors > 0);
    assert!(counters.host_write_failures > 0);
    drop(disk);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn image_backed_drive_refuses_fat_chain_mapping() {
    let disk = marked_disk(8);
    assert_eq!(disk.map_fat_chain(2, 1), None);
}

// ---------------------------------------------------------------------------
// Slice 9C (`dev_docs/2026-09-05-device-timing-slice9-design.md`, the 9C row
// and §5 risk 2): the ATA seek / rotational / drive-buffer / sustained-media
// model, `charge_pio_transfer`. Every fixture states its `DeviceTimingProfile`
// arm explicitly, the same discipline the 9C-pre poll-skip fixtures follow.
// ---------------------------------------------------------------------------

/// UNARMED must be byte-identical to the pre-9C `pio_transfer_ticks_cached`
/// formula, and must touch neither the modelled head nor the sequential-run
/// state -- the knob-unset identity bar every slice-9 family holds.
#[test]
fn pio_transfer_charge_is_inert_while_the_ata_family_is_unarmed() {
    let mut disk = marked_disk(64);
    let unarmed = DeviceTimingProfile::default();
    for (lba, sectors, hits) in [(0u32, 1u32, 0u32), (5, 3, 1), (100, 1, 0)] {
        assert_eq!(
            disk.charge_pio_transfer(lba, sectors, hits, unarmed),
            pio_transfer_ticks_cached(sectors, hits),
            "unarmed must stay byte-identical to the pre-9C formula"
        );
    }
    assert_eq!(disk.head_lba, 0, "the modelled head never moved");
    assert_eq!(
        disk.last_transfer_end_lba, None,
        "the sequential-run state never armed"
    );
}

/// A request whose start LBA continues immediately where the previous
/// charged transfer left off costs only the command-latency floor plus a
/// cable-burst transfer -- no seek, no rotation. This is design §5 risk 2's
/// "a sequential single-sector stream... costs 50 us + cable transfer and no
/// seek", the property the ≤25%/≤15% Duke bar depends on (98.7% of its reads
/// are single-sector).
#[test]
fn sequential_pio_transfer_costs_the_buffer_price_not_a_seek() {
    let mut disk = marked_disk(1_000_000);
    let armed = DeviceTimingProfile {
        ata: true,
        ..DeviceTimingProfile::default()
    };
    // First transfer is necessarily cold (no prior state); its own cost is
    // not what this fixture is about.
    let first = disk.charge_pio_transfer(10, 1, 0, armed);
    assert!(first > 0);
    assert_eq!(disk.last_transfer_end_lba, Some(11));

    // Second transfer starts exactly at LBA 11, where the first left off.
    let second = disk.charge_pio_transfer(11, 1, 0, armed);
    let expected = COMMAND_LATENCY_TICKS_PERIOD + pio_sector_ticks();
    assert_eq!(
        second, expected,
        "a sequential continuation costs the command floor plus a cable-rate \
         transfer, exactly -- no seek, no rotation"
    );
    assert!(
        second < ROTATIONAL_LATENCY_TICKS,
        "and is nowhere near the 5.55 ms rotational latency a cold access pays"
    );
}

/// A sequential run longer than the drive's read-ahead buffer falls back to
/// the sustained media rate for the overflow -- the buffer only hides
/// latency, it does not raise the drive's throughput ceiling.
#[test]
fn a_sequential_run_beyond_the_buffer_falls_back_to_the_media_rate() {
    let sectors_in_buffer = (DRIVE_BUFFER_BYTES / SECTOR as u64) as u32;
    let total = sectors_in_buffer + 8;
    let mut disk = marked_disk((total + 16) as usize);
    let armed = DeviceTimingProfile {
        ata: true,
        ..DeviceTimingProfile::default()
    };
    let first = disk.charge_pio_transfer(0, 1, 0, armed);
    assert!(first > 0);

    // One big sequential transfer that overruns the buffer.
    let second = disk.charge_pio_transfer(1, total, 0, armed);
    let cable_only = COMMAND_LATENCY_TICKS_PERIOD
        + (u128::from(u64::from(total) * SECTOR as u64) * u128::from(MASTER_CLOCK_HZ))
            .div_ceil(u128::from(PIO_BYTES_PER_SECOND)) as u64;
    assert!(
        second > cable_only,
        "the overflow past the buffer must cost more than an all-cable-rate \
         transfer would: second={second} cable_only={cable_only}"
    );
}

/// A random (non-continuing) LBA pays both the seek and the rotational
/// latency, and costs strictly more than a sequential access of the same
/// size -- design §5 risk 2's other half.
#[test]
fn random_lba_pio_transfer_costs_seek_and_rotation() {
    let mut disk = marked_disk(1_000_000);
    let armed = DeviceTimingProfile {
        ata: true,
        ..DeviceTimingProfile::default()
    };
    let _ = disk.charge_pio_transfer(10, 1, 0, armed);
    // Far away from LBA 11 (where the first transfer left off): not a
    // sequential continuation.
    let random = disk.charge_pio_transfer(500_000, 1, 0, armed);

    let sequential_cost = COMMAND_LATENCY_TICKS_PERIOD + pio_sector_ticks();
    assert!(
        random > sequential_cost,
        "a random jump must cost strictly more than a sequential access: \
         random={random} sequential_cost={sequential_cost}"
    );
    assert!(
        random
            >= COMMAND_LATENCY_TICKS_PERIOD + SEEK_TRACK_TO_TRACK_TICKS + ROTATIONAL_LATENCY_TICKS,
        "and at least the command floor plus the seek floor plus one full \
         rotational latency: random={random}"
    );
}

/// A fully host-cache-served transfer (every sector a hit) touches the
/// modelled drive at all: no charge, no head move -- design §5 risk 1, the
/// host cache stays a pure host-time avoider.
#[test]
fn a_fully_cached_transfer_charges_nothing_and_does_not_move_the_modelled_head() {
    let mut disk = marked_disk(64);
    let armed = DeviceTimingProfile {
        ata: true,
        ..DeviceTimingProfile::default()
    };
    assert_eq!(disk.charge_pio_transfer(10, 4, 4, armed), 0);
    assert_eq!(disk.head_lba, 0);
    assert_eq!(disk.last_transfer_end_lba, None);
}
