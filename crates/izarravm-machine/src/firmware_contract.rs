// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    BIOS_BASE_MEMORY_KIB, BIOS_EQUIPMENT_FPU, BIOS_EQUIPMENT_WORD, BIOS_ROM_SIZE, BusError,
    GswMode, INT10_STATIC_FUNCTIONALITY, INT10_VIDEO_PARAM_ENTRIES, INT10_VIDEO_PARAM_ENTRY_LEN,
    Memory, font,
};

pub(crate) mod address {
    use crate::{LOW_BIOS_BASE, UPPER_MEMORY_BASE};

    pub(crate) const VGA_BIOS_BASE: u32 = UPPER_MEMORY_BASE;
    pub(crate) const VGA_BIOS_SEGMENT: u16 = (VGA_BIOS_BASE >> 4) as u16;
    pub(crate) const INT10_FUNCTIONALITY_TABLE_OFFSET: u16 = 0x0100;
    pub(crate) const INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET: u16 = 0x0110;
    pub(crate) const INT10_VIDEO_SAVE_POINTER_TABLE_PTRS: usize = 7;
    pub(crate) const INT10_VIDEO_PARAM_TABLE_OFFSET: u16 = 0x0130;
    pub(crate) const INT10_VIDEO_PARAM_TABLE_ENTRIES: usize = 30;
    pub(crate) const BDA_VIDEO_SAVE_POINTER: usize = 0x4a8;
    pub(crate) const VGA_BIOS_INT1D_VIDEO_TABLE_OFF: u16 = 0x1000;
    pub(crate) const VGA_BIOS_INT1D_VIDEO_TABLE_ADDR: u32 =
        VGA_BIOS_BASE + VGA_BIOS_INT1D_VIDEO_TABLE_OFF as u32;
    pub(crate) const VGA_BIOS_FONT_TABLE_OFF: u16 = 0x2000;
    pub(crate) const VGA_BIOS_INT43_FONT_ADDR: u32 = VGA_BIOS_BASE + VGA_BIOS_FONT_TABLE_OFF as u32;
    pub(crate) const VGA_BIOS_INT44_FONT_OFF: u16 = 0x3000;
    pub(crate) const VGA_BIOS_INT44_FONT_ADDR: u32 = VGA_BIOS_BASE + VGA_BIOS_INT44_FONT_OFF as u32;
    pub(crate) const VGA_BIOS_INT1F_FONT_OFF: u16 = 0x3800;
    pub(crate) const VGA_BIOS_INT1F_FONT_ADDR: u32 = VGA_BIOS_BASE + VGA_BIOS_INT1F_FONT_OFF as u32;
    pub(crate) const CODEPAGE_FONT_WINDOW: u32 = 0xC4000;
    pub(crate) const VGA_BIOS_SPAN_SIZE: u32 = 0x8000;

    pub(crate) const BIOS_ROM_SEGMENT: u16 = (LOW_BIOS_BASE >> 4) as u16;
    pub(crate) const BIOS_FONT_8X8_ROM_OFFSET: u16 = 0xC000;
    pub(crate) const BIOS_FONT_8X14_ROM_OFFSET: u16 = 0xC800;
    pub(crate) const BIOS_FONT_8X16_ROM_OFFSET: u16 = 0xD600;
    pub(crate) const BIOS_FONT_8X8_HIGH_ROM_OFFSET: u16 = 0xE600;
    pub(crate) const BIOS_IRET_STUB_ADDRESS: usize = 0x0600;
    pub(crate) const RESULT_BLOCK_ADDRESS: usize = 0x9000;

    pub(crate) const CONVENTIONAL_MEMORY_KIB: u16 = 639;
    pub(crate) const CONVENTIONAL_MEMORY_TOP: u64 = CONVENTIONAL_MEMORY_KIB as u64 * 1024;
    pub(crate) const BIOS_BOOT_CHOICE_ADDR: u32 = 0x0537;
    pub(crate) const CMOS_PRIMARY_BOOT_DEVICE: usize = 0x11;
    pub(crate) const CMOS_GSW_MODE: usize = 0x12;

    pub(crate) const EBDA_SEGMENT: u16 = 0x9FC0;
    pub(crate) const EBDA_LINEAR: u32 = (EBDA_SEGMENT as u32) << 4;
    pub(crate) const EBDA_MOUSE_HANDLER_OFF: u32 = 0x0002;
    pub(crate) const EBDA_MOUSE_PACKET_OFF: u32 = 0x0006;
    pub(crate) const EBDA_MOUSE_INDEX_OFF: u32 = 0x000A;
    pub(crate) const EBDA_MOUSE_PKT_SIZE_OFF: u32 = 0x000B;
    pub(crate) const EBDA_CD_BOOTABLE_OFF: u32 = 0x000C;

    pub(crate) const SETUP_SCRATCH_ADDRESS: usize = 0x0610;
    pub(crate) const SETUP_SCRATCH_USED: usize = 12;
    pub(crate) const BIOS_RTC_ISR_ADDRESS: usize = 0x0610;
    pub(crate) const BIOS_HALT_STUB_ADDRESS: usize = 0x0620;
    pub(crate) const BIOS_SLAVE_IRQ_ISR_ADDRESS: usize = 0x0622;
    pub(crate) const BIOS_CRITICAL_ERROR_RETURN_STUB_ADDRESS: usize = 0x0630;
    pub(crate) const DOS_INT23_DEFAULT_STUB_ADDRESS: usize = 0x0632;
    pub(crate) const DOS_INT24_DEFAULT_STUB_ADDRESS: usize = 0x0637;

    pub(crate) const BDA_RTC_WAIT_COMPLETE: usize = 0x498;
    pub(crate) const BDA_RTC_WAIT_TIMEOUT: usize = 0x49C;
    pub(crate) const BDA_RTC_WAIT_FLAG: usize = 0x4A0;
    pub(crate) const BDA_RTC_WAIT_PENDING: u8 = 0x01;
    pub(crate) const BDA_DAY_COUNT: usize = 0x4f0;

    pub(crate) const BIOS32_HEADER_ROM_OFFSET: usize = 0xEA00;
    pub(crate) const BIOS32_DIRECTORY_ROM_OFFSET: usize = 0xEA10;
    pub(crate) const BIOS32_PCI_ROM_OFFSET: usize = 0xEA20;
    pub(crate) const BIOS32_DIRECTORY_LINEAR: u32 = 0xFEA10;
    pub(crate) const BIOS32_PCI_LINEAR: u32 = 0xFEA20;

    pub(crate) const BIOS_TIMER_ISR_ROM_OFFSET: usize = 0xF060;
    pub(crate) const BIOS_MASTER_IRQ_ISR_ROM_OFFSET: usize = 0xF080;
    pub(crate) const BIOS_TIMER_ISR_ROM_OFF: u16 = 0x0060;
    pub(crate) const BIOS_MASTER_IRQ_ISR_ROM_OFF: u16 = 0x0080;
    pub(crate) const BIOS_INT_STUB_TABLE_ROM_OFFSET: usize = 0xF200;
    pub(crate) const BIOS_INT_STUB_TABLE_LINEAR: u32 = 0xFF200;
    pub(crate) const BIOS_INT_STUB_TABLE_LEN: u32 = 512;
    pub(crate) const BIOS_ROM_IRET_SEG: u16 = 0xff00;
    pub(crate) const BIOS_LEGACY_IRET_LINEAR: u32 = 0xFF000;
    pub(crate) const BIOS_STUB_WINDOW_LEN: u32 = 0x400;
    pub(crate) const BIOS_LEGACY_IRET_ROM_OFFSET: usize = 0xF000;

    pub(crate) const BIOS_CONFIG_TABLE_ADDR: u32 = 0x9FC10;
    pub(crate) const BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR: u32 = 0x9FC20;
    pub(crate) const BIOS_DISKETTE_PARAMETER_TABLE_ADDR: u32 = 0x9FC30;
    pub(crate) const BIOS_POST_ERROR_LOG_COUNT_ADDR: u32 = 0x9FC3F;
    pub(crate) const BIOS_POST_ERROR_LOG_ADDR: u32 = 0x9FC40;
    pub(crate) const BIOS_POST_ERROR_LOG_MAX: u8 = 16;

    pub(crate) const fn bios_int_stub_off(vector: u8) -> u16 {
        0x0200 + (vector as u16) * 2
    }
}

use address::*;

const SYSINIT_HALT_STUB: usize = BIOS_IRET_STUB_ADDRESS + 1;
const DOS_CALL5_ENTRY_ADDRESS: usize = 0x00c0;
const DOS_CALL5_ENTRY_SEG: u16 = 0xff00;
const DOS_CALL5_ENTRY_OFF: u16 = 0x0020;
const DOS_CALL5_ROM_OFFSET: usize = 0xf020;
const DOS_CALL5_MAX_FUNCTION: u8 = 0x24;

const DOS_CALL5_ENTRY_STUB: [u8; 49] = [
    0x80,
    0xf9,
    DOS_CALL5_MAX_FUNCTION,
    0x77,
    0x17,
    0x55,
    0x8b,
    0xec,
    0x50,
    0x8b,
    0x46,
    0x04,
    0x87,
    0x46,
    0x06,
    0x89,
    0x46,
    0x04,
    0x58,
    0x5d,
    0x83,
    0xc4,
    0x02,
    0x88,
    0xcc,
    0xcd,
    0x21,
    0xcb,
    0x55,
    0x8b,
    0xec,
    0x50,
    0x8b,
    0x46,
    0x04,
    0x87,
    0x46,
    0x06,
    0x89,
    0x46,
    0x04,
    0x58,
    0x5d,
    0x83,
    0xc4,
    0x02,
    0xb0,
    0x00,
    0xcb,
];

const BIOS_TIMER_ISR_STUB: [u8; 25] = [
    0x50, 0x1e, 0x31, 0xc0, 0x8e, 0xd8, 0x83, 0x06, 0x6c, 0x04, 0x01, 0x83, 0x16, 0x6e, 0x04, 0x00,
    0xcd, 0x1c, 0xb0, 0x20, 0xe6, 0x20, 0x1f, 0x58, 0xcf,
];

const BIOS_MASTER_IRQ_ISR_STUB: [u8; 7] = [0x50, 0xb0, 0x20, 0xe6, 0x20, 0x58, 0xcf];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Bios32Call {
    Directory,
    Pci,
}

const _: () = {
    const FONT_8X8_END: usize = BIOS_FONT_8X8_ROM_OFFSET as usize + 256 * 8;
    const FONT_8X14_END: usize = BIOS_FONT_8X14_ROM_OFFSET as usize + 256 * 14;
    const FONT_8X16_END: usize = BIOS_FONT_8X16_ROM_OFFSET as usize + 256 * 16;
    const FONT_8X8_HIGH_END: usize = BIOS_FONT_8X8_HIGH_ROM_OFFSET as usize + 128 * 8;
    assert!(FONT_8X8_END <= BIOS_FONT_8X14_ROM_OFFSET as usize);
    assert!(FONT_8X14_END <= BIOS_FONT_8X16_ROM_OFFSET as usize);
    assert!(FONT_8X16_END <= BIOS_FONT_8X8_HIGH_ROM_OFFSET as usize);
    assert!(FONT_8X8_HIGH_END <= BIOS32_HEADER_ROM_OFFSET);
    assert!(BIOS32_HEADER_ROM_OFFSET + 16 <= BIOS32_DIRECTORY_ROM_OFFSET);
    assert!(BIOS32_DIRECTORY_ROM_OFFSET + 2 <= BIOS32_PCI_ROM_OFFSET);
    assert!(BIOS32_PCI_ROM_OFFSET + 2 <= BIOS_LEGACY_IRET_ROM_OFFSET);
    assert!(BIOS_LEGACY_IRET_ROM_OFFSET + 2 <= DOS_CALL5_ROM_OFFSET);
    assert!(DOS_CALL5_ROM_OFFSET + DOS_CALL5_ENTRY_STUB.len() <= BIOS_TIMER_ISR_ROM_OFFSET);
    assert!(
        BIOS_TIMER_ISR_ROM_OFFSET + BIOS_TIMER_ISR_STUB.len() <= BIOS_MASTER_IRQ_ISR_ROM_OFFSET
    );
    assert!(
        BIOS_MASTER_IRQ_ISR_ROM_OFFSET + BIOS_MASTER_IRQ_ISR_STUB.len()
            <= BIOS_INT_STUB_TABLE_ROM_OFFSET
    );
    assert!(BIOS_INT_STUB_TABLE_ROM_OFFSET + BIOS_INT_STUB_TABLE_LEN as usize <= BIOS_ROM_SIZE);

    assert!(
        INT10_FUNCTIONALITY_TABLE_OFFSET as usize + INT10_STATIC_FUNCTIONALITY.len()
            <= INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET as usize
    );
    assert!(
        INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET as usize + INT10_VIDEO_SAVE_POINTER_TABLE_PTRS * 4
            <= INT10_VIDEO_PARAM_TABLE_OFFSET as usize
    );
    assert!(
        INT10_VIDEO_PARAM_TABLE_OFFSET as usize
            + INT10_VIDEO_PARAM_TABLE_ENTRIES * INT10_VIDEO_PARAM_ENTRY_LEN
            <= VGA_BIOS_INT1D_VIDEO_TABLE_OFF as usize
    );
    assert!(VGA_BIOS_INT1D_VIDEO_TABLE_OFF as usize + 8 * 16 <= VGA_BIOS_FONT_TABLE_OFF as usize);
    assert!(VGA_BIOS_FONT_TABLE_OFF as usize + 256 * 16 <= VGA_BIOS_INT44_FONT_OFF as usize);
    assert!(VGA_BIOS_INT44_FONT_OFF as usize + 256 * 8 <= VGA_BIOS_INT1F_FONT_OFF as usize);
    assert!(VGA_BIOS_INT1F_FONT_OFF as u32 + 128 * 8 <= CODEPAGE_FONT_WINDOW - VGA_BIOS_BASE);
    assert!(CODEPAGE_FONT_WINDOW + 4096 <= VGA_BIOS_BASE + VGA_BIOS_SPAN_SIZE);

    assert!(BIOS_IRET_STUB_ADDRESS + 2 <= BIOS_RTC_ISR_ADDRESS);
    assert!(BIOS_RTC_ISR_ADDRESS + 15 <= BIOS_HALT_STUB_ADDRESS);
    // Izarra setup retires the RTC vector before reusing its first 12 bytes.
    assert!(SETUP_SCRATCH_ADDRESS == BIOS_RTC_ISR_ADDRESS);
    assert!(SETUP_SCRATCH_ADDRESS + SETUP_SCRATCH_USED <= BIOS_RTC_ISR_ADDRESS + 15);
    assert!(SETUP_SCRATCH_ADDRESS + SETUP_SCRATCH_USED <= BIOS_HALT_STUB_ADDRESS);
    assert!(BIOS_HALT_STUB_ADDRESS + 2 <= BIOS_SLAVE_IRQ_ISR_ADDRESS);
    assert!(BIOS_SLAVE_IRQ_ISR_ADDRESS + 9 <= BIOS_CRITICAL_ERROR_RETURN_STUB_ADDRESS);
    assert!(BIOS_CRITICAL_ERROR_RETURN_STUB_ADDRESS < DOS_INT23_DEFAULT_STUB_ADDRESS);
    assert!(DOS_INT23_DEFAULT_STUB_ADDRESS + 5 <= DOS_INT24_DEFAULT_STUB_ADDRESS);

    assert!(EBDA_MOUSE_HANDLER_OFF + 4 <= EBDA_MOUSE_PACKET_OFF);
    assert!(EBDA_MOUSE_PACKET_OFF + 4 <= EBDA_MOUSE_INDEX_OFF);
    assert!(EBDA_MOUSE_INDEX_OFF < EBDA_MOUSE_PKT_SIZE_OFF);
    assert!(EBDA_MOUSE_PKT_SIZE_OFF < EBDA_CD_BOOTABLE_OFF);
    assert!(EBDA_CD_BOOTABLE_OFF < BIOS_CONFIG_TABLE_ADDR - EBDA_LINEAR);
    assert!(BIOS_CONFIG_TABLE_ADDR + 10 <= BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR);
    assert!(BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR + 16 <= BIOS_DISKETTE_PARAMETER_TABLE_ADDR);
    assert!(BIOS_DISKETTE_PARAMETER_TABLE_ADDR + 11 <= BIOS_POST_ERROR_LOG_COUNT_ADDR);
    assert!(BIOS_POST_ERROR_LOG_COUNT_ADDR < BIOS_POST_ERROR_LOG_ADDR);
    assert!(EBDA_LINEAR == CONVENTIONAL_MEMORY_TOP as u32);
    assert!(BIOS_POST_ERROR_LOG_ADDR + BIOS_POST_ERROR_LOG_MAX as u32 * 2 <= EBDA_LINEAR + 1024);
};

pub(super) fn patch_rom(rom: &mut [u8]) {
    install_bios_font_mirror(rom);
    rom[DOS_CALL5_ROM_OFFSET..DOS_CALL5_ROM_OFFSET + DOS_CALL5_ENTRY_STUB.len()]
        .copy_from_slice(&DOS_CALL5_ENTRY_STUB);
    rom[BIOS_TIMER_ISR_ROM_OFFSET..BIOS_TIMER_ISR_ROM_OFFSET + BIOS_TIMER_ISR_STUB.len()]
        .copy_from_slice(&BIOS_TIMER_ISR_STUB);
    rom[BIOS_MASTER_IRQ_ISR_ROM_OFFSET
        ..BIOS_MASTER_IRQ_ISR_ROM_OFFSET + BIOS_MASTER_IRQ_ISR_STUB.len()]
        .copy_from_slice(&BIOS_MASTER_IRQ_ISR_STUB);
    write_bios_int_stub_table(rom);
}

fn install_bios_font_mirror(rom: &mut [u8]) {
    let off = usize::from(BIOS_FONT_8X8_ROM_OFFSET);
    rom[off..off + font::VGAFONT_8X8.len()].copy_from_slice(&font::VGAFONT_8X8);
    let off = usize::from(BIOS_FONT_8X14_ROM_OFFSET);
    rom[off..off + font::VGAFONT_8X14.len()].copy_from_slice(&font::VGAFONT_8X14);
    let off = usize::from(BIOS_FONT_8X16_ROM_OFFSET);
    rom[off..off + font::VGAFONT_8X16.len()].copy_from_slice(&font::VGAFONT_8X16);
    let high = &font::VGAFONT_8X8[128 * 8..];
    let off = usize::from(BIOS_FONT_8X8_HIGH_ROM_OFFSET);
    rom[off..off + high.len()].copy_from_slice(high);
}

fn write_bios_int_stub_table(rom: &mut [u8]) {
    for vector in 0..=255usize {
        rom[BIOS_INT_STUB_TABLE_ROM_OFFSET + vector * 2] = 0x90;
        rom[BIOS_INT_STUB_TABLE_ROM_OFFSET + vector * 2 + 1] = 0xcf;
    }
    rom[BIOS_LEGACY_IRET_ROM_OFFSET] = 0x90;
    rom[BIOS_LEGACY_IRET_ROM_OFFSET + 1] = 0xcf;

    let mut header = [0u8; 16];
    header[..4].copy_from_slice(b"_32_");
    header[4..8].copy_from_slice(&BIOS32_DIRECTORY_LINEAR.to_le_bytes());
    header[9] = 1;
    header[10] = 0u8.wrapping_sub(header.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)));
    rom[BIOS32_HEADER_ROM_OFFSET..BIOS32_HEADER_ROM_OFFSET + 16].copy_from_slice(&header);
    rom[BIOS32_DIRECTORY_ROM_OFFSET] = 0x90;
    rom[BIOS32_DIRECTORY_ROM_OFFSET + 1] = 0xcb;
    rom[BIOS32_PCI_ROM_OFFSET] = 0x90;
    rom[BIOS32_PCI_ROM_OFFSET + 1] = 0xcb;
}

pub(super) fn install_boot_memory(memory: &mut Memory, mode: GswMode) -> Result<(), BusError> {
    for vector in 0x00usize..=0x07 {
        write_ivt(
            memory,
            vector as u8,
            bios_int_stub_off(vector as u8),
            BIOS_ROM_IRET_SEG,
        )?;
    }
    write_ivt(memory, 0x08, BIOS_TIMER_ISR_ROM_OFF, BIOS_ROM_IRET_SEG)?;
    for vector in [0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f] {
        write_ivt(
            memory,
            vector,
            BIOS_MASTER_IRQ_ISR_ROM_OFF,
            BIOS_ROM_IRET_SEG,
        )?;
    }

    for vector in [
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x25, 0x26, 0x27,
        0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38,
        0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e, 0x3f, 0x40, 0x42, 0x47, 0x4b, 0x4c, 0x4d, 0x4e, 0x4f,
        0x45, 0x48, 0x49, 0x4a, 0x58, 0x59, 0x5a, 0x5b, 0x5c, 0x5d, 0x5e, 0x5f, 0x60, 0x61, 0x62,
        0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x6b, 0x6c, 0x6d, 0x6e, 0x6f, 0x78, 0x79,
        0x7a, 0x7b, 0x7c, 0x7d, 0x7e, 0x7f, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0xe0, 0xe4,
        0xef, 0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd,
        0xfe, 0xff,
    ] {
        write_ivt(memory, vector, bios_int_stub_off(vector), BIOS_ROM_IRET_SEG)?;
    }

    write_ivt(memory, 0x70, BIOS_RTC_ISR_ADDRESS as u16, 0)?;
    for vector in 0x71..=0x77 {
        write_ivt(memory, vector, BIOS_SLAVE_IRQ_ISR_ADDRESS as u16, 0)?;
    }
    install_rtc_isr_stub(memory)?;
    install_slave_irq_isr_stub(memory)?;
    install_dos_low_memory_stubs(memory)?;

    seed_vga_option_rom_header(memory)?;
    seed_int1d_video_parameter_table(memory)?;
    seed_int1e_diskette_parameter_table(memory)?;
    seed_int1f_graphics_font_table(memory)?;
    seed_int43_font_table(memory)?;
    seed_int44_font_table(memory)?;
    seed_int46_absent_fixed_disk_table(memory)?;
    seed_video_bios_tables(memory)?;

    let equipment = if mode.persona().has_fpu() {
        BIOS_EQUIPMENT_WORD | BIOS_EQUIPMENT_FPU
    } else {
        BIOS_EQUIPMENT_WORD
    };
    memory.write_u16(0x410, equipment)?;
    memory.write_u16(0x413, BIOS_BASE_MEMORY_KIB - 1)?;
    memory.write_u8(EBDA_LINEAR as usize, 1)?;
    seed_bios_config_table(memory)?;

    for (address, value) in [
        (0x400, 0x03f8),
        (0x402, 0x02f8),
        (0x404, 0),
        (0x406, 0),
        (0x408, 0x0378),
        (0x40a, 0x0278),
        (0x40c, 0),
        (0x40e, 0),
    ] {
        memory.write_u16(address, value)?;
    }
    for offset in 0x47c..=0x47f {
        memory.write_u8(offset, 0x01)?;
    }
    for offset in 0x478..=0x47b {
        memory.write_u8(offset, 0x14)?;
    }

    memory.write_u8(0x449, 0x03)?;
    memory.write_u16(0x44a, 80)?;
    memory.write_u16(0x44c, 0x1000)?;
    memory.write_u16(0x44e, 0)?;
    memory.write_u8(0x462, 0)?;
    memory.write_u16(0x463, 0x03d4)?;
    memory.write_u8(0x465, 0x29)?;
    memory.write_u8(0x466, 0)?;
    memory.write_u8(0x484, 24)?;
    memory.write_u16(0x485, 16)?;
    memory.write_u8(0x487, 0x60)?;
    memory.write_u8(0x488, 0xf9)?;
    memory.write_u8(0x489, 0x51)?;
    memory.write_u8(0x48a, 0x08)?;
    seed_bda_video_save_pointer(memory)?;

    memory.write_u8(0x475, 0)?;
    memory.write_u8(0x471, 0)?;
    memory.write_u16(0x472, 0x1234)?;
    memory.write_u8(0x417, 0)?;
    memory.write_u8(0x418, 0)?;
    memory.write_u16(0x41a, 0x001e)?;
    memory.write_u16(0x41c, 0x001e)?;
    memory.write_u16(0x480, 0x001e)?;
    memory.write_u16(0x482, 0x003e)?;
    memory.write_u8(0x496, 0)?;
    memory.write_u8(0x497, 0)?;
    memory.write_u8(0x43e, 0)?;
    memory.write_u8(0x441, 0)?;
    memory.write_u8(0x474, 0)?;

    finalize_vga_option_rom_checksum(memory)
}

fn install_dos_low_memory_stubs(memory: &mut Memory) -> Result<(), BusError> {
    memory.write_u8(BIOS_IRET_STUB_ADDRESS, 0xcf)?;
    memory.write_u8(DOS_CALL5_ENTRY_ADDRESS, 0xea)?;
    memory.write_u16(DOS_CALL5_ENTRY_ADDRESS + 1, DOS_CALL5_ENTRY_OFF)?;
    memory.write_u16(DOS_CALL5_ENTRY_ADDRESS + 3, DOS_CALL5_ENTRY_SEG)?;
    memory.write_u8(SYSINIT_HALT_STUB, 0xf4)?;
    memory.write_u8(BIOS_HALT_STUB_ADDRESS, 0xfa)?;
    memory.write_u8(BIOS_HALT_STUB_ADDRESS + 1, 0xf4)?;
    memory.write_u8(BIOS_CRITICAL_ERROR_RETURN_STUB_ADDRESS, 0xf4)?;
    for (address, value) in [
        (DOS_INT23_DEFAULT_STUB_ADDRESS, 0xb8),
        (DOS_INT23_DEFAULT_STUB_ADDRESS + 1, 0x00),
        (DOS_INT23_DEFAULT_STUB_ADDRESS + 2, 0x4c),
        (DOS_INT23_DEFAULT_STUB_ADDRESS + 3, 0xcd),
        (DOS_INT23_DEFAULT_STUB_ADDRESS + 4, 0x21),
        (DOS_INT24_DEFAULT_STUB_ADDRESS, 0xb0),
        (DOS_INT24_DEFAULT_STUB_ADDRESS + 1, 0x03),
        (DOS_INT24_DEFAULT_STUB_ADDRESS + 2, 0xcf),
    ] {
        memory.write_u8(address, value)?;
    }
    for (vector, target) in [
        (0x20, BIOS_IRET_STUB_ADDRESS),
        (0x21, BIOS_IRET_STUB_ADDRESS),
        (0x22, BIOS_HALT_STUB_ADDRESS),
        (0x23, DOS_INT23_DEFAULT_STUB_ADDRESS),
        (0x24, DOS_INT24_DEFAULT_STUB_ADDRESS),
    ] {
        write_ivt(memory, vector, target as u16, 0)?;
    }
    Ok(())
}

fn install_rtc_isr_stub(memory: &mut Memory) -> Result<(), BusError> {
    const STUB: [u8; 15] = [
        0x50, 0xb0, 0x0c, 0xe6, 0x70, 0xe4, 0x71, 0xb0, 0x20, 0xe6, 0xa0, 0xe6, 0x20, 0x58, 0xcf,
    ];
    for (offset, byte) in STUB.into_iter().enumerate() {
        memory.write_u8(BIOS_RTC_ISR_ADDRESS + offset, byte)?;
    }
    Ok(())
}

fn install_slave_irq_isr_stub(memory: &mut Memory) -> Result<(), BusError> {
    const STUB: [u8; 9] = [0x50, 0xb0, 0x20, 0xe6, 0xa0, 0xe6, 0x20, 0x58, 0xcf];
    for (offset, byte) in STUB.into_iter().enumerate() {
        memory.write_u8(BIOS_SLAVE_IRQ_ISR_ADDRESS + offset, byte)?;
    }
    Ok(())
}

fn seed_bda_video_save_pointer(memory: &mut Memory) -> Result<(), BusError> {
    memory.write_u16(
        BDA_VIDEO_SAVE_POINTER,
        INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET,
    )?;
    memory.write_u16(BDA_VIDEO_SAVE_POINTER + 2, VGA_BIOS_SEGMENT)
}

fn seed_video_bios_tables(memory: &mut Memory) -> Result<(), BusError> {
    let vga_base = VGA_BIOS_BASE as usize;
    for (index, byte) in INT10_STATIC_FUNCTIONALITY.iter().copied().enumerate() {
        memory.write_u8(
            vga_base + usize::from(INT10_FUNCTIONALITY_TABLE_OFFSET) + index,
            byte,
        )?;
    }
    let save_ptr = vga_base + usize::from(INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET);
    memory.write_u16(save_ptr, INT10_VIDEO_PARAM_TABLE_OFFSET)?;
    memory.write_u16(save_ptr + 2, VGA_BIOS_SEGMENT)?;
    for slot in 1..INT10_VIDEO_SAVE_POINTER_TABLE_PTRS {
        memory.write_u16(save_ptr + slot * 4, 0)?;
        memory.write_u16(save_ptr + slot * 4 + 2, 0)?;
    }
    let param_table = vga_base + usize::from(INT10_VIDEO_PARAM_TABLE_OFFSET);
    for offset in 0..INT10_VIDEO_PARAM_TABLE_ENTRIES * INT10_VIDEO_PARAM_ENTRY_LEN {
        memory.write_u8(param_table + offset, 0)?;
    }
    for &(entry, bytes) in INT10_VIDEO_PARAM_ENTRIES {
        let base = param_table + entry * INT10_VIDEO_PARAM_ENTRY_LEN;
        for (offset, byte) in bytes.iter().copied().enumerate() {
            memory.write_u8(base + offset, byte)?;
        }
    }
    Ok(())
}

fn seed_vga_option_rom_header(memory: &mut Memory) -> Result<(), BusError> {
    let base = VGA_BIOS_BASE as usize;
    for offset in 0..VGA_BIOS_SPAN_SIZE as usize {
        memory.write_u8(base + offset, 0)?;
    }
    memory.write_u8(base, 0x55)?;
    memory.write_u8(base + 1, 0xaa)?;
    memory.write_u8(base + 2, 0x40)?;
    memory.write_u8(base + 3, 0xcb)?;
    for (index, byte) in b"IzarraVM VGA BIOS".iter().copied().enumerate() {
        memory.write_u8(base + 4 + index, byte)?;
    }
    Ok(())
}

fn finalize_vga_option_rom_checksum(memory: &mut Memory) -> Result<(), BusError> {
    let base = VGA_BIOS_BASE as usize;
    let checksum_at = base + VGA_BIOS_SPAN_SIZE as usize - 1;
    memory.write_u8(checksum_at, 0)?;
    let sum = (0..VGA_BIOS_SPAN_SIZE as usize).try_fold(0u8, |sum, offset| {
        memory
            .read_u8(base + offset)
            .map(|byte| sum.wrapping_add(byte))
    })?;
    memory.write_u8(checksum_at, 0u8.wrapping_sub(sum))
}

fn seed_bios_config_table(memory: &mut Memory) -> Result<(), BusError> {
    let table = [0x08, 0x00, 0xfc, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0x00];
    for (offset, byte) in table.into_iter().enumerate() {
        memory.write_u8(BIOS_CONFIG_TABLE_ADDR as usize + offset, byte)?;
    }
    Ok(())
}

fn write_ivt(memory: &mut Memory, vector: u8, offset: u16, segment: u16) -> Result<(), BusError> {
    let address = usize::from(vector) * 4;
    memory.write_u16(address, offset)?;
    memory.write_u16(address + 2, segment)
}

fn write_ivt_linear(memory: &mut Memory, vector: u8, linear: u32) -> Result<(), BusError> {
    write_ivt(memory, vector, (linear & 0x0f) as u16, (linear >> 4) as u16)
}

fn seed_int1d_video_parameter_table(memory: &mut Memory) -> Result<(), BusError> {
    const TEXT_40X25: [u8; 16] = [
        0x38, 0x28, 0x2d, 0x0a, 0x1f, 0x06, 0x19, 0x1c, 0x02, 0x07, 0x06, 0x07, 0x00, 0x00, 0x00,
        0x00,
    ];
    const TEXT_80X25: [u8; 16] = [
        0x71, 0x50, 0x5a, 0x0a, 0x1f, 0x06, 0x19, 0x1c, 0x02, 0x07, 0x06, 0x07, 0x00, 0x00, 0x00,
        0x00,
    ];
    const CGA_320X200: [u8; 16] = [
        0x38, 0x28, 0x2d, 0x0a, 0x7f, 0x06, 0x64, 0x70, 0x02, 0x01, 0x06, 0x07, 0x00, 0x00, 0x00,
        0x00,
    ];
    const CGA_640X200: [u8; 16] = [
        0x71, 0x50, 0x5a, 0x0a, 0x7f, 0x06, 0x64, 0x70, 0x02, 0x01, 0x06, 0x07, 0x00, 0x00, 0x00,
        0x00,
    ];
    const MDA_TEXT_80X25: [u8; 16] = [
        0x61, 0x50, 0x52, 0x0f, 0x19, 0x06, 0x19, 0x19, 0x02, 0x0d, 0x0b, 0x0c, 0x00, 0x00, 0x00,
        0x00,
    ];
    const TABLE: [[u8; 16]; 8] = [
        TEXT_40X25,
        TEXT_40X25,
        TEXT_80X25,
        TEXT_80X25,
        CGA_320X200,
        CGA_320X200,
        CGA_640X200,
        MDA_TEXT_80X25,
    ];
    let base = VGA_BIOS_INT1D_VIDEO_TABLE_ADDR as usize;
    for (mode, registers) in TABLE.iter().enumerate() {
        for (offset, byte) in registers.iter().copied().enumerate() {
            memory.write_u8(base + mode * registers.len() + offset, byte)?;
        }
    }
    write_ivt_linear(memory, 0x1d, VGA_BIOS_INT1D_VIDEO_TABLE_ADDR)
}

fn seed_int1e_diskette_parameter_table(memory: &mut Memory) -> Result<(), BusError> {
    const DPT_1440K: [u8; 11] = [
        0xdf, 0x02, 0x25, 0x02, 0x12, 0x1b, 0xff, 0x6c, 0xf6, 0x0f, 0x08,
    ];
    for (offset, byte) in DPT_1440K.into_iter().enumerate() {
        memory.write_u8(BIOS_DISKETTE_PARAMETER_TABLE_ADDR as usize + offset, byte)?;
    }
    write_ivt_linear(memory, 0x1e, BIOS_DISKETTE_PARAMETER_TABLE_ADDR)
}

fn seed_int1f_graphics_font_table(memory: &mut Memory) -> Result<(), BusError> {
    let upper_half = &font::VGAFONT_8X8[0x80 * 8..];
    for (offset, byte) in upper_half.iter().copied().enumerate() {
        memory.write_u8(VGA_BIOS_INT1F_FONT_ADDR as usize + offset, byte)?;
    }
    write_ivt_linear(memory, 0x1f, VGA_BIOS_INT1F_FONT_ADDR)
}

fn seed_int43_font_table(memory: &mut Memory) -> Result<(), BusError> {
    for (offset, byte) in font::VGAFONT_8X16.iter().copied().enumerate() {
        memory.write_u8(VGA_BIOS_INT43_FONT_ADDR as usize + offset, byte)?;
    }
    write_ivt(memory, 0x43, VGA_BIOS_FONT_TABLE_OFF, VGA_BIOS_SEGMENT)
}

fn seed_int44_font_table(memory: &mut Memory) -> Result<(), BusError> {
    for (offset, byte) in font::VGAFONT_8X8.iter().copied().enumerate() {
        memory.write_u8(VGA_BIOS_INT44_FONT_ADDR as usize + offset, byte)?;
    }
    write_ivt(memory, 0x44, VGA_BIOS_INT44_FONT_OFF, VGA_BIOS_SEGMENT)
}

fn seed_int46_absent_fixed_disk_table(memory: &mut Memory) -> Result<(), BusError> {
    write_ivt(memory, 0x46, 0, 0)
}

#[cfg(test)]
#[path = "firmware_contract_test.rs"]
mod tests;
