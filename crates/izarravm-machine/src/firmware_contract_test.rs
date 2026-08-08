// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::*;
use std::ops::Range;

fn nasm_literal(source: &str, name: &str) -> Result<u64, String> {
    let mut value = None;
    for (line_number, line) in source.lines().enumerate() {
        let code = line.split(';').next().unwrap_or_default().trim();
        if code.is_empty() {
            continue;
        }
        let mut tokens = code.split_whitespace();
        if tokens.next() != Some("%define") || tokens.next() != Some(name) {
            continue;
        }
        if value.is_some() {
            return Err(format!("duplicate definition {name}"));
        }
        let raw = tokens
            .next()
            .ok_or_else(|| format!("{name} has no value on line {}", line_number + 1))?;
        if tokens.next().is_some() {
            return Err(format!("{name} is not a single literal: {code}"));
        }
        let digits = raw.replace('_', "");
        let parsed = if let Some(hex) = digits
            .strip_prefix("0x")
            .or_else(|| digits.strip_prefix("0X"))
        {
            u64::from_str_radix(hex, 16)
        } else {
            digits.parse()
        }
        .map_err(|_| format!("{name} is not a numeric literal: {raw}"))?;
        value = Some(parsed);
    }
    value.ok_or_else(|| format!("missing definition {name}"))
}

#[test]
fn nasm_literal_parser_rejects_missing_duplicate_and_expression_values() {
    assert!(nasm_literal("%define FOUND 1", "MISSING").is_err());
    assert!(nasm_literal("%define X 1\n%define X 2", "X").is_err());
    assert!(nasm_literal("%define X (BASE + 1)", "X").is_err());
    assert_eq!(nasm_literal("%define X 0x1_2a ; comment", "X"), Ok(0x12a));
}

#[test]
fn named_machine_and_nasm_firmware_definitions_match() {
    let definitions = [
        ("ROM_SEG", u64::from((LOW_BIOS_BASE >> 4) as u16)),
        ("BDA_SEG", 0x0040),
        (
            "CONVENTIONAL_MEMORY_KIB",
            u64::from(CONVENTIONAL_MEMORY_KIB),
        ),
        ("RESULT_BLOCK", RESULT_BLOCK_ADDRESS as u64),
        ("BOOT_CMOS_PRIMARY", CMOS_PRIMARY_BOOT_DEVICE as u64),
        ("SU_NVRAM_GSW", CMOS_GSW_MODE as u64),
        ("BOOT_CHOICE", u64::from(BIOS_BOOT_CHOICE_ADDR)),
        ("BOOT_DEV_FLOPPY", BootDevice::Floppy as u64),
        ("BOOT_DEV_DISK", BootDevice::HardDisk as u64),
        ("BOOT_DEV_CDROM", BootDevice::CdRom as u64),
        (
            "BDA_RTC_WAIT_COMPLETE",
            (BDA_RTC_WAIT_COMPLETE - 0x400) as u64,
        ),
        (
            "BDA_RTC_WAIT_TIMEOUT",
            (BDA_RTC_WAIT_TIMEOUT - 0x400) as u64,
        ),
        ("BDA_RTC_WAIT_FLAG", (BDA_RTC_WAIT_FLAG - 0x400) as u64),
        ("BDA_RTC_WAIT_PENDING", u64::from(BDA_RTC_WAIT_PENDING)),
        ("SETUP_SCRATCH", SETUP_SCRATCH_ADDRESS as u64),
        ("SETUP_SCRATCH_USED", SETUP_SCRATCH_USED as u64),
        ("EBDA_SEG", u64::from(EBDA_SEGMENT)),
        ("EBDA_MOUSE_HANDLER", u64::from(EBDA_MOUSE_HANDLER_OFF)),
        ("EBDA_MOUSE_PKT", u64::from(EBDA_MOUSE_PACKET_OFF)),
        ("EBDA_MOUSE_IDX", u64::from(EBDA_MOUSE_INDEX_OFF)),
        ("EBDA_MOUSE_PKT_SIZE", u64::from(EBDA_MOUSE_PKT_SIZE_OFF)),
        ("EBDA_CD_BOOTABLE", u64::from(EBDA_CD_BOOTABLE_OFF)),
        ("LOTURA_ID_VALUE", u64::from(LOTURA_ID_VALUE)),
        ("MARGO_MMIO_BASE", u64::from(MARGO_MMIO_BASE)),
        ("LFB_BASE", u64::from(MARGO_LFB_BASE)),
    ];
    for (name, expected) in definitions {
        assert_eq!(
            nasm_literal(izarravm_firmware::IZARRA_BIOS_DEFS_SOURCE, name),
            Ok(expected),
            "NASM definition {name} drifted"
        );
    }
}

fn mark_range(owned: &mut [bool], range: Range<usize>) {
    owned[range].fill(true);
}

fn assert_rom_patch_contract(machine: &Machine, original: &[u8]) {
    let rom = &machine.rom;
    assert_eq!(rom.len(), BIOS_ROM_SIZE);
    assert_eq!(
        &rom[usize::from(BIOS_FONT_8X8_ROM_OFFSET)
            ..usize::from(BIOS_FONT_8X8_ROM_OFFSET) + font::VGAFONT_8X8.len()],
        &font::VGAFONT_8X8
    );
    assert_eq!(
        &rom[usize::from(BIOS_FONT_8X14_ROM_OFFSET)
            ..usize::from(BIOS_FONT_8X14_ROM_OFFSET) + font::VGAFONT_8X14.len()],
        &font::VGAFONT_8X14
    );
    assert_eq!(
        &rom[usize::from(BIOS_FONT_8X16_ROM_OFFSET)
            ..usize::from(BIOS_FONT_8X16_ROM_OFFSET) + font::VGAFONT_8X16.len()],
        &font::VGAFONT_8X16
    );
    let high = &font::VGAFONT_8X8[128 * 8..];
    assert_eq!(
        &rom[usize::from(BIOS_FONT_8X8_HIGH_ROM_OFFSET)
            ..usize::from(BIOS_FONT_8X8_HIGH_ROM_OFFSET) + high.len()],
        high
    );

    assert_eq!(
        &rom[BIOS_LEGACY_IRET_ROM_OFFSET..BIOS_LEGACY_IRET_ROM_OFFSET + 2],
        &[0x90, 0xcf]
    );
    assert_eq!(
        &rom[DOS_CALL5_ROM_OFFSET..DOS_CALL5_ROM_OFFSET + DOS_CALL5_ENTRY_STUB.len()],
        &DOS_CALL5_ENTRY_STUB
    );
    assert_eq!(
        &rom[BIOS_TIMER_ISR_ROM_OFFSET..BIOS_TIMER_ISR_ROM_OFFSET + BIOS_TIMER_ISR_STUB.len()],
        &BIOS_TIMER_ISR_STUB
    );
    assert_eq!(
        &rom[BIOS_MASTER_IRQ_ISR_ROM_OFFSET
            ..BIOS_MASTER_IRQ_ISR_ROM_OFFSET + BIOS_MASTER_IRQ_ISR_STUB.len()],
        &BIOS_MASTER_IRQ_ISR_STUB
    );
    for vector in 0..=255usize {
        let offset = BIOS_INT_STUB_TABLE_ROM_OFFSET + vector * 2;
        assert_eq!(&rom[offset..offset + 2], &[0x90, 0xcf]);
    }

    let header = &rom[BIOS32_HEADER_ROM_OFFSET..BIOS32_HEADER_ROM_OFFSET + 16];
    assert_eq!(&header[..4], b"_32_");
    assert_eq!(
        u32::from_le_bytes(header[4..8].try_into().unwrap()),
        BIOS32_DIRECTORY_LINEAR
    );
    assert_eq!(header[9], 1);
    assert_eq!(
        header.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
        0
    );
    assert_eq!(
        &rom[BIOS32_DIRECTORY_ROM_OFFSET..BIOS32_DIRECTORY_ROM_OFFSET + 2],
        &[0x90, 0xcb]
    );
    assert_eq!(
        &rom[BIOS32_PCI_ROM_OFFSET..BIOS32_PCI_ROM_OFFSET + 2],
        &[0x90, 0xcb]
    );

    let mut owned = vec![false; BIOS_ROM_SIZE];
    mark_range(
        &mut owned,
        usize::from(BIOS_FONT_8X8_ROM_OFFSET)
            ..usize::from(BIOS_FONT_8X8_ROM_OFFSET) + font::VGAFONT_8X8.len(),
    );
    mark_range(
        &mut owned,
        usize::from(BIOS_FONT_8X14_ROM_OFFSET)
            ..usize::from(BIOS_FONT_8X14_ROM_OFFSET) + font::VGAFONT_8X14.len(),
    );
    mark_range(
        &mut owned,
        usize::from(BIOS_FONT_8X16_ROM_OFFSET)
            ..usize::from(BIOS_FONT_8X16_ROM_OFFSET) + font::VGAFONT_8X16.len(),
    );
    mark_range(
        &mut owned,
        usize::from(BIOS_FONT_8X8_HIGH_ROM_OFFSET)
            ..usize::from(BIOS_FONT_8X8_HIGH_ROM_OFFSET) + high.len(),
    );
    mark_range(
        &mut owned,
        BIOS_LEGACY_IRET_ROM_OFFSET..BIOS_LEGACY_IRET_ROM_OFFSET + 2,
    );
    mark_range(
        &mut owned,
        DOS_CALL5_ROM_OFFSET..DOS_CALL5_ROM_OFFSET + DOS_CALL5_ENTRY_STUB.len(),
    );
    mark_range(
        &mut owned,
        BIOS_TIMER_ISR_ROM_OFFSET..BIOS_TIMER_ISR_ROM_OFFSET + BIOS_TIMER_ISR_STUB.len(),
    );
    mark_range(
        &mut owned,
        BIOS_MASTER_IRQ_ISR_ROM_OFFSET
            ..BIOS_MASTER_IRQ_ISR_ROM_OFFSET + BIOS_MASTER_IRQ_ISR_STUB.len(),
    );
    mark_range(
        &mut owned,
        BIOS_INT_STUB_TABLE_ROM_OFFSET
            ..BIOS_INT_STUB_TABLE_ROM_OFFSET + BIOS_INT_STUB_TABLE_LEN as usize,
    );
    mark_range(
        &mut owned,
        BIOS32_HEADER_ROM_OFFSET..BIOS32_HEADER_ROM_OFFSET + 16,
    );
    mark_range(
        &mut owned,
        BIOS32_DIRECTORY_ROM_OFFSET..BIOS32_DIRECTORY_ROM_OFFSET + 2,
    );
    mark_range(&mut owned, BIOS32_PCI_ROM_OFFSET..BIOS32_PCI_ROM_OFFSET + 2);
    for (offset, (&before, &after)) in original.iter().zip(rom).enumerate() {
        if !owned[offset] {
            assert_eq!(after, before, "unowned ROM byte changed at {offset:#06x}");
        }
    }
}

#[test]
fn machine_patches_only_the_owned_rom_windows() {
    let original: Vec<u8> = (0..BIOS_ROM_SIZE)
        .map(|offset| (offset as u8).wrapping_mul(37).wrapping_add(11))
        .collect();
    let machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), &original).unwrap();
    assert_rom_patch_contract(&machine, &original);
    assert_eq!(crate::unittester::crc32(&machine.rom), 0xda7d_7b97);
}

#[test]
fn committed_izarra_bios_reserves_every_machine_patch_window() {
    let rom = izarravm_firmware::IZARRA_BIOS;
    for range in [
        usize::from(BIOS_FONT_8X8_ROM_OFFSET)
            ..usize::from(BIOS_FONT_8X8_ROM_OFFSET) + font::VGAFONT_8X8.len(),
        usize::from(BIOS_FONT_8X14_ROM_OFFSET)
            ..usize::from(BIOS_FONT_8X14_ROM_OFFSET) + font::VGAFONT_8X14.len(),
        usize::from(BIOS_FONT_8X16_ROM_OFFSET)
            ..usize::from(BIOS_FONT_8X16_ROM_OFFSET) + font::VGAFONT_8X16.len(),
        usize::from(BIOS_FONT_8X8_HIGH_ROM_OFFSET)..BIOS32_HEADER_ROM_OFFSET,
        BIOS32_HEADER_ROM_OFFSET..BIOS32_PCI_ROM_OFFSET + 0x10,
        DOS_CALL5_ROM_OFFSET..DOS_CALL5_ROM_OFFSET + DOS_CALL5_ENTRY_STUB.len(),
        BIOS_TIMER_ISR_ROM_OFFSET..BIOS_TIMER_ISR_ROM_OFFSET + BIOS_TIMER_ISR_STUB.len(),
        BIOS_MASTER_IRQ_ISR_ROM_OFFSET
            ..BIOS_MASTER_IRQ_ISR_ROM_OFFSET + BIOS_MASTER_IRQ_ISR_STUB.len(),
        BIOS_INT_STUB_TABLE_ROM_OFFSET
            ..BIOS_INT_STUB_TABLE_ROM_OFFSET + BIOS_INT_STUB_TABLE_LEN as usize,
    ] {
        assert!(
            rom[range.clone()].iter().all(|byte| *byte == 0),
            "Izarra BIOS patch window {range:?} is not reserved"
        );
    }
    assert_eq!(
        &rom[BIOS_LEGACY_IRET_ROM_OFFSET..BIOS_LEGACY_IRET_ROM_OFFSET + 2],
        &[0xcf, 0x00]
    );
}

#[derive(Debug, PartialEq, Eq)]
struct BootMemorySnapshot {
    ivt_00_08: Vec<u8>,
    ivt_0a_15: Vec<u8>,
    ivt_1d_24: Vec<u8>,
    ivt_42_46: Vec<u8>,
    ivt_70_77: Vec<u8>,
    bda_ports: Vec<u8>,
    bda_after_equipment: Vec<u8>,
    bda_after_disk_count: Vec<u8>,
    low_stubs: Vec<u8>,
    ebda_before_hdd_table: Vec<u8>,
    ebda_after_hdd_table: Vec<u8>,
}

impl BootMemorySnapshot {
    fn crc32(&self) -> u32 {
        let mut bytes = Vec::new();
        for region in [
            self.ivt_00_08.as_slice(),
            self.ivt_0a_15.as_slice(),
            self.ivt_1d_24.as_slice(),
            self.ivt_42_46.as_slice(),
            self.ivt_70_77.as_slice(),
            self.bda_ports.as_slice(),
            self.bda_after_equipment.as_slice(),
            self.bda_after_disk_count.as_slice(),
            self.low_stubs.as_slice(),
            self.ebda_before_hdd_table.as_slice(),
            self.ebda_after_hdd_table.as_slice(),
        ] {
            bytes.extend_from_slice(region);
        }
        crate::unittester::crc32(&bytes)
    }
}

fn memory_range(machine: &Machine, range: Range<usize>) -> Vec<u8> {
    machine.memory.as_slice()[range].to_vec()
}

fn boot_memory_snapshot(machine: &Machine) -> BootMemorySnapshot {
    BootMemorySnapshot {
        ivt_00_08: memory_range(machine, 0x0000..0x0024),
        ivt_0a_15: memory_range(machine, 0x0028..0x0058),
        ivt_1d_24: memory_range(machine, 0x0074..0x0094),
        ivt_42_46: memory_range(machine, 0x0108..0x011c),
        ivt_70_77: memory_range(machine, 0x01c0..0x01e0),
        bda_ports: memory_range(machine, 0x0400..0x0410),
        bda_after_equipment: memory_range(machine, 0x0412..0x0475),
        bda_after_disk_count: memory_range(machine, 0x0476..0x04ac),
        low_stubs: memory_range(machine, 0x0600..0x063a),
        ebda_before_hdd_table: memory_range(machine, 0x9fc00..0x9fc20),
        ebda_after_hdd_table: memory_range(machine, 0x9fc30..0x9fc50),
    }
}

fn ivt_pointer(machine: &Machine, vector: u8) -> (u16, u16) {
    let address = usize::from(vector) * 4;
    (
        machine.memory.read_u16(address).unwrap(),
        machine.memory.read_u16(address + 2).unwrap(),
    )
}

fn assert_shared_boot_memory(machine: &Machine, equipment: u16) {
    for vector in 0x00..=0x07 {
        assert_eq!(
            ivt_pointer(machine, vector),
            (bios_int_stub_off(vector), BIOS_ROM_IRET_SEG)
        );
    }
    assert_eq!(
        ivt_pointer(machine, 0x08),
        (BIOS_TIMER_ISR_ROM_OFF, BIOS_ROM_IRET_SEG)
    );
    for vector in 0x0a..=0x0f {
        assert_eq!(
            ivt_pointer(machine, vector),
            (BIOS_MASTER_IRQ_ISR_ROM_OFF, BIOS_ROM_IRET_SEG)
        );
    }
    assert_eq!(ivt_pointer(machine, 0x1d), (0, 0xc100));
    assert_eq!(ivt_pointer(machine, 0x1e), (0, 0x9fc3));
    assert_eq!(ivt_pointer(machine, 0x1f), (0, 0xc380));
    assert_eq!(
        ivt_pointer(machine, 0x20),
        (BIOS_IRET_STUB_ADDRESS as u16, 0)
    );
    assert_eq!(
        ivt_pointer(machine, 0x21),
        (BIOS_IRET_STUB_ADDRESS as u16, 0)
    );
    assert_eq!(
        ivt_pointer(machine, 0x22),
        (BIOS_HALT_STUB_ADDRESS as u16, 0)
    );
    assert_eq!(
        ivt_pointer(machine, 0x23),
        (DOS_INT23_DEFAULT_STUB_ADDRESS as u16, 0)
    );
    assert_eq!(
        ivt_pointer(machine, 0x24),
        (DOS_INT24_DEFAULT_STUB_ADDRESS as u16, 0)
    );
    assert_eq!(
        ivt_pointer(machine, 0x42),
        (bios_int_stub_off(0x42), BIOS_ROM_IRET_SEG)
    );
    assert_eq!(
        ivt_pointer(machine, 0x43),
        (VGA_BIOS_FONT_TABLE_OFF, VGA_BIOS_SEGMENT)
    );
    assert_eq!(
        ivt_pointer(machine, 0x44),
        (VGA_BIOS_INT44_FONT_OFF, VGA_BIOS_SEGMENT)
    );
    assert_eq!(ivt_pointer(machine, 0x46), (0, 0));
    assert_eq!(ivt_pointer(machine, 0x70), (BIOS_RTC_ISR_ADDRESS as u16, 0));
    for vector in 0x71..=0x77 {
        assert_eq!(
            ivt_pointer(machine, vector),
            (BIOS_SLAVE_IRQ_ISR_ADDRESS as u16, 0)
        );
    }

    assert_eq!(&machine.memory.as_slice()[0x0600..0x0602], &[0xcf, 0xf4]);
    assert_eq!(
        &machine.memory.as_slice()[BIOS_RTC_ISR_ADDRESS..BIOS_RTC_ISR_ADDRESS + 15],
        &[
            0x50, 0xb0, 0x0c, 0xe6, 0x70, 0xe4, 0x71, 0xb0, 0x20, 0xe6, 0xa0, 0xe6, 0x20, 0x58,
            0xcf
        ]
    );
    assert_eq!(
        &machine.memory.as_slice()[0x0620..0x062b],
        &[
            0xfa, 0xf4, 0x50, 0xb0, 0x20, 0xe6, 0xa0, 0xe6, 0x20, 0x58, 0xcf
        ]
    );
    assert_eq!(
        &machine.memory.as_slice()[0x0630..0x063a],
        &[0xf4, 0x00, 0xb8, 0x00, 0x4c, 0xcd, 0x21, 0xb0, 0x03, 0xcf]
    );

    assert_eq!(machine.memory.read_u16(0x400).unwrap(), 0x03f8);
    assert_eq!(machine.memory.read_u16(0x402).unwrap(), 0x02f8);
    assert_eq!(machine.memory.read_u16(0x408).unwrap(), 0x0378);
    assert_eq!(machine.memory.read_u16(0x40a).unwrap(), 0x0278);
    assert_eq!(machine.memory.read_u16(0x410).unwrap(), equipment);
    assert_eq!(machine.memory.read_u16(0x413).unwrap(), 639);
    assert_eq!(machine.memory.read_u16(0x41a).unwrap(), 0x001e);
    assert_eq!(machine.memory.read_u16(0x41c).unwrap(), 0x001e);
    assert_eq!(machine.memory.read_u8(0x449).unwrap(), 0x03);
    assert_eq!(machine.memory.read_u16(0x44a).unwrap(), 80);
    assert_eq!(machine.memory.read_u16(0x463).unwrap(), 0x03d4);
    assert_eq!(machine.memory.read_u16(0x472).unwrap(), 0x1234);
    assert_eq!(machine.memory.read_u16(0x480).unwrap(), 0x001e);
    assert_eq!(machine.memory.read_u16(0x482).unwrap(), 0x003e);
    assert_eq!(machine.memory.read_u16(0x485).unwrap(), 16);
    assert_eq!(
        machine.memory.read_u16(BDA_VIDEO_SAVE_POINTER).unwrap(),
        INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET
    );
    assert_eq!(
        machine.memory.read_u16(BDA_VIDEO_SAVE_POINTER + 2).unwrap(),
        VGA_BIOS_SEGMENT
    );

    assert_eq!(machine.memory.read_u8(EBDA_LINEAR as usize).unwrap(), 1);
    assert_eq!(
        &machine.memory.as_slice()
            [BIOS_CONFIG_TABLE_ADDR as usize..BIOS_CONFIG_TABLE_ADDR as usize + 10],
        &[0x08, 0x00, 0xfc, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0x00]
    );
    assert_eq!(
        &machine.memory.as_slice()[BIOS_DISKETTE_PARAMETER_TABLE_ADDR as usize
            ..BIOS_DISKETTE_PARAMETER_TABLE_ADDR as usize + 11],
        &[
            0xdf, 0x02, 0x25, 0x02, 0x12, 0x1b, 0xff, 0x6c, 0xf6, 0x0f, 0x08
        ]
    );
    assert_eq!(
        machine
            .memory
            .read_u8(BIOS_POST_ERROR_LOG_COUNT_ADDR as usize)
            .unwrap(),
        0
    );

    let vga_base = VGA_BIOS_BASE as usize;
    assert_eq!(
        &machine.memory.as_slice()[vga_base..vga_base + 4],
        &[0x55, 0xaa, 0x40, 0xcb]
    );
    assert_eq!(
        &machine.memory.as_slice()[vga_base + 4..vga_base + 21],
        b"IzarraVM VGA BIOS"
    );
    assert_eq!(
        &machine.memory.as_slice()[VGA_BIOS_INT43_FONT_ADDR as usize
            ..VGA_BIOS_INT43_FONT_ADDR as usize + font::VGAFONT_8X16.len()],
        &font::VGAFONT_8X16
    );
    let checksum_at = vga_base + VGA_BIOS_SPAN_SIZE as usize - 1;
    let body_sum = machine.memory.as_slice()[vga_base..checksum_at]
        .iter()
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    assert_eq!(
        machine.memory.read_u8(checksum_at).unwrap(),
        0u8.wrapping_sub(body_sum)
    );
}

#[test]
fn all_constructors_share_the_firmware_boot_memory_snapshot() {
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let new_machine = Machine::new(profile.clone(), vec![0; BIOS_ROM_SIZE]).unwrap();
    let boot_machine = Machine::new_boot_image(profile.clone(), vec![0; BOOT_IMAGE_SIZE]).unwrap();
    let raw_machine = Machine::new_raw_program(profile, &[0xf4]).unwrap();

    assert_shared_boot_memory(&new_machine, BIOS_EQUIPMENT_WORD);
    assert_shared_boot_memory(&boot_machine, BIOS_EQUIPMENT_WORD & !0x0001);
    assert_shared_boot_memory(&raw_machine, BIOS_EQUIPMENT_WORD);
    let new_snapshot = boot_memory_snapshot(&new_machine);
    let boot_snapshot = boot_memory_snapshot(&boot_machine);
    let raw_snapshot = boot_memory_snapshot(&raw_machine);
    const SNAPSHOT_CRC32: u32 = 0x2819_74f0;
    assert_eq!(new_snapshot.crc32(), SNAPSHOT_CRC32);
    assert_eq!(boot_snapshot.crc32(), SNAPSHOT_CRC32);
    assert_eq!(raw_snapshot.crc32(), SNAPSHOT_CRC32);
    assert_eq!(new_snapshot, boot_snapshot);
    assert_eq!(new_snapshot, raw_snapshot);

    assert_eq!(new_machine.memory.read_u8(0x475).unwrap(), 0);
    assert_eq!(boot_machine.memory.read_u8(0x475).unwrap(), 1);
    assert_eq!(raw_machine.memory.read_u8(0x475).unwrap(), 0);
    assert_eq!(ivt_pointer(&new_machine, 0x09), (0, 0));
    assert_eq!(ivt_pointer(&boot_machine, 0x09), (0, 0));
    assert_eq!(ivt_pointer(&new_machine, 0x16), (0, 0));
    assert_eq!(ivt_pointer(&boot_machine, 0x16), (0, 0));
    assert_eq!(ivt_pointer(&new_machine, 0x41), (0, 0));
    assert_eq!(ivt_pointer(&raw_machine, 0x41), (0, 0));
    assert_eq!(
        ivt_pointer(&boot_machine, 0x41),
        (
            (BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR & 0x0f) as u16,
            (BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR >> 4) as u16
        )
    );
    assert_eq!(
        &new_machine.memory.as_slice()[BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR as usize
            ..BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR as usize + 16],
        &[0; 16]
    );
    assert_eq!(
        &boot_machine.memory.as_slice()[BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR as usize
            ..BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR as usize + 16],
        &[2, 0, 16, 0, 0, 0, 0, 0, 8, 0, 0, 0, 2, 0, 63, 0]
    );
    assert_eq!(
        &raw_machine.memory.as_slice()[BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR as usize
            ..BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR as usize + 16],
        &[0; 16]
    );
    let keyboard = izarravm_firmware::KBD_RESIDENT_BIOS;
    assert_eq!(
        ivt_pointer(&raw_machine, 0x09),
        (
            u16::from_le_bytes([keyboard[0], keyboard[1]]),
            izarravm_firmware::KBD_RESIDENT_BIOS_SEG
        )
    );
    assert_eq!(
        ivt_pointer(&raw_machine, 0x16),
        (
            u16::from_le_bytes([keyboard[2], keyboard[3]]),
            izarravm_firmware::KBD_RESIDENT_BIOS_SEG
        )
    );
}

#[test]
fn per_vector_linear_stub_identity_posts_the_matching_service() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0; BIOS_ROM_SIZE],
    )
    .unwrap();
    machine.pending_soft_int = None;
    machine.last_int_vector = None;
    {
        let mut bus = machine.make_bus();
        bus.note_stub_fetch(BIOS_INT_STUB_TABLE_LINEAR + 0x10 * 2);
    }
    assert_eq!(machine.pending_soft_int, Some(0x10));
}

/// TOKAEMM addresses itself with 16-bit offsets, so everything it can name has
/// to fit under 64 KB. The paging tables are the exception: they are reserved
/// past the end of the file and reached only by linear address through
/// `pd_lin`, so the file length IS the resident core and the whole of the
/// budget that new driver code competes for.
///
/// This exists because the driver spent a campaign at 16 bytes under the
/// ceiling, which blocked every change that touched it. The reservation had
/// been emitted into the file, charging 32,752 bytes of the budget on every
/// configuration to serve a fallback only a 1 MiB machine takes.
///
/// Absolute figures, measured 2026-08-06, not a proportion of anything: a
/// margin expressed as a fraction of the image grows when the image grows,
/// which is precisely backwards for a ceiling.
#[test]
fn the_tokaemm_image_keeps_room_under_the_16_bit_driver_ceiling() {
    const CEILING: usize = 0x10000;
    // Measured 2026-08-08: ~24.0 KB, after all three RAM-scaled tables (the
    // arena bitmap, the VCPI ownership bitmap, the EMS chain table) moved into
    // the system window at SYS_LIN_BASE. They were ~41.2 KB together in the
    // core; the 24 MB era was 29,552. The bar is deliberately well under the
    // ceiling rather than just below it, so that a change which eats the
    // headroom is caught while there is still room to think about it.
    //
    // It moves DOWN with a shrink, not only up with a growth: a bar left at
    // 50,000 after this change would have stopped measuring anything.
    //
    // This figure no longer depends on installed RAM. Before the move the core
    // grew ~288 bytes per megabyte of arena and stopped assembling entirely
    // past roughly 148 MB, on the 0xFFF0 offset ceiling below.
    const MAX: usize = 28_000;

    let image = izarravm_firmware::tokaemm_sys();
    assert!(
        image.len() < CEILING,
        "TOKAEMM is {} bytes, at or over the {CEILING}-byte 16-bit offset ceiling",
        image.len()
    );
    assert!(
        image.len() <= MAX,
        "TOKAEMM has grown to {} bytes, past the {MAX}-byte bar. That is not a \
         failure by itself, but the ceiling is {CEILING} and the last time this \
         got tight it blocked all driver work. Re-measure and move the bar \
         deliberately.",
        image.len()
    );
}
