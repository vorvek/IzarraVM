// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

pub(crate) const CONVENTIONAL_MEMORY_KIB: u16 = 639;
pub(crate) const CONVENTIONAL_MEMORY_TOP: u64 = CONVENTIONAL_MEMORY_KIB as u64 * 1024;

pub(crate) const BIOS_BOOT_CHOICE_ADDR: u32 = 0x0537;
pub(crate) const CMOS_PRIMARY_BOOT_DEVICE: usize = 0x11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BootDevice {
    Floppy = 0,
    HardDisk = 1,
    CdRom = 2,
}

impl BootDevice {
    pub(crate) fn from_code(code: u8) -> Self {
        match code {
            1 => Self::HardDisk,
            2 => Self::CdRom,
            _ => Self::Floppy,
        }
    }

    pub(crate) fn fallback_order(self) -> [Self; 3] {
        match self {
            Self::Floppy => [Self::Floppy, Self::HardDisk, Self::CdRom],
            Self::HardDisk => [Self::HardDisk, Self::Floppy, Self::CdRom],
            Self::CdRom => [Self::CdRom, Self::HardDisk, Self::Floppy],
        }
    }
}

pub(crate) const EBDA_SEGMENT: u16 = 0x9FC0;
pub(crate) const EBDA_LINEAR: u32 = (EBDA_SEGMENT as u32) << 4;

pub(crate) const BDA_RTC_WAIT_COMPLETE: usize = 0x498;
pub(crate) const BDA_RTC_WAIT_TIMEOUT: usize = 0x49C;
pub(crate) const BDA_RTC_WAIT_FLAG: usize = 0x4A0;
pub(crate) const BDA_RTC_WAIT_PENDING: u8 = 0x01;

pub(crate) const BIOS32_HEADER_ROM_OFFSET: usize = 0xEA00;
pub(crate) const BIOS32_DIRECTORY_ROM_OFFSET: usize = 0xEA10;
pub(crate) const BIOS32_PCI_ROM_OFFSET: usize = 0xEA20;
pub(crate) const BIOS32_DIRECTORY_LINEAR: u32 = 0xFEA10;
pub(crate) const BIOS32_PCI_LINEAR: u32 = 0xFEA20;

pub(crate) const BIOS_TIMER_ISR_ROM_OFFSET: usize = 0xF060;
pub(crate) const BIOS_MASTER_IRQ_ISR_ROM_OFFSET: usize = 0xF080;
pub(crate) const BIOS_INT_STUB_TABLE_ROM_OFFSET: usize = 0xF200;
pub(crate) const BIOS_INT_STUB_TABLE_LINEAR: u32 = 0xFF200;
pub(crate) const BIOS_INT_STUB_TABLE_LEN: u32 = 512;
