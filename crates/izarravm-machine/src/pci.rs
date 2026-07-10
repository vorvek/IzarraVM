// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! PCI configuration for the PIIX4-compatible southbridge and Distira.

use izarravm_bus::BusWidth;

use crate::video_params::{
    DISTIRA_PCI_BAR_SIZE, DISTIRA_PCI_CMDFIFO_OFFSET, DISTIRA_PCI_DEVICE_ID,
    DISTIRA_PCI_LFB_OFFSET, DISTIRA_PCI_REVISION, DISTIRA_PCI_SLOT, DISTIRA_PCI_TEX_OFFSET,
    DISTIRA_PCI_VENDOR_ID, PCI_CONFIG_ADDRESS_PORT, PCI_CONFIG_DATA_END, PCI_CONFIG_DATA_PORT,
};

const PIIX_SLOT: u8 = 7;
const INTEL_VENDOR_ID: u16 = 0x8086;
const PIIX4_ISA_DEVICE_ID: u16 = 0x7110;
const PIIX4_IDE_DEVICE_ID: u16 = 0x7111;
const PIIX4_REVISION: u8 = 0x01;
const PIIX4_BMIDE_POWER_ON_BASE: u32 = 0x0000_f000;
const PCI_COMMAND_IO: u16 = 0x0001;
const PCI_COMMAND_BUS_MASTER: u16 = 0x0004;

#[derive(Debug, Clone)]
pub(crate) struct PciConfig {
    address: u32,
    distira_command: u16,
    distira_mem_base: u32,
    distira_init_enable: u32,
    piix_ide_command: u16,
    piix_ide_bm_base: u32,
}

impl PciConfig {
    pub(crate) fn new() -> Self {
        Self {
            address: 0,
            // Izarra has no PCI BIOS yet, so Distira powers on with its fixed BAR
            // decoded. Guest drivers can still rewrite command/BAR0 through CF8/CFC.
            distira_command: 0x0002,
            distira_mem_base: crate::DISTIRA_MMIO_BASE & !(DISTIRA_PCI_BAR_SIZE - 1),
            distira_init_enable: 0,
            // There is no PCI BIOS yet, so the legacy IDE controller powers on
            // decoded and bus-master capable at its fixed 0xF000 BAR4.
            piix_ide_command: PCI_COMMAND_IO | PCI_COMMAND_BUS_MASTER,
            piix_ide_bm_base: PIIX4_BMIDE_POWER_ON_BASE,
        }
    }

    pub(crate) fn read_io(&self, port: u16, width: BusWidth) -> Option<u32> {
        if port_span_in(
            port,
            width,
            PCI_CONFIG_ADDRESS_PORT,
            PCI_CONFIG_ADDRESS_PORT + 3,
        ) {
            return Some(read_register_bytes(
                self.address,
                port - PCI_CONFIG_ADDRESS_PORT,
                width,
            ));
        }
        if port_span_in(port, width, PCI_CONFIG_DATA_PORT, PCI_CONFIG_DATA_END) {
            return Some(self.read_data(port - PCI_CONFIG_DATA_PORT, width));
        }
        None
    }

    pub(crate) fn write_io(&mut self, port: u16, width: BusWidth, value: u32) -> bool {
        if port_span_in(
            port,
            width,
            PCI_CONFIG_ADDRESS_PORT,
            PCI_CONFIG_ADDRESS_PORT + 3,
        ) {
            self.address =
                write_register_bytes(self.address, port - PCI_CONFIG_ADDRESS_PORT, width, value);
            return true;
        }
        if port_span_in(port, width, PCI_CONFIG_DATA_PORT, PCI_CONFIG_DATA_END) {
            self.write_data(port - PCI_CONFIG_DATA_PORT, width, value);
            return true;
        }
        false
    }

    pub(crate) fn ide_io_enabled(&self) -> bool {
        self.piix_ide_command & PCI_COMMAND_IO != 0
    }

    pub(crate) fn ide_bus_master_enabled(&self) -> bool {
        self.piix_ide_command & PCI_COMMAND_BUS_MASTER != 0
    }

    /// Current BAR4 I/O base when it fits the x86 16-bit I/O space. An all-ones
    /// size probe therefore decodes nowhere until the driver restores the BAR.
    pub(crate) fn ide_bus_master_io_base(&self) -> Option<u16> {
        (self.piix_ide_bm_base <= u32::from(u16::MAX) - 15).then_some(self.piix_ide_bm_base as u16)
    }

    pub(crate) fn distira_mmio_offset(&self, address: u32, width: usize) -> Option<usize> {
        let offset = self.distira_bar_offset(address, width)?;
        if offset < DISTIRA_PCI_LFB_OFFSET && offset + width as u32 <= DISTIRA_PCI_LFB_OFFSET {
            Some(offset as usize)
        } else {
            None
        }
    }

    pub(crate) fn distira_lfb_offset(&self, address: u32, width: usize) -> Option<usize> {
        let offset = self.distira_bar_offset(address, width)?;
        if (DISTIRA_PCI_LFB_OFFSET..DISTIRA_PCI_TEX_OFFSET).contains(&offset)
            && offset + width as u32 <= DISTIRA_PCI_TEX_OFFSET
        {
            Some((offset - DISTIRA_PCI_LFB_OFFSET) as usize)
        } else {
            None
        }
    }

    pub(crate) fn distira_texture_offset(&self, address: u32, width: usize) -> Option<usize> {
        let offset = self.distira_bar_offset(address, width)?;
        if offset >= DISTIRA_PCI_TEX_OFFSET && offset + width as u32 <= DISTIRA_PCI_BAR_SIZE {
            Some((offset - DISTIRA_PCI_TEX_OFFSET) as usize)
        } else {
            None
        }
    }

    pub(crate) fn distira_cmdfifo_offset(&self, address: u32, width: usize) -> Option<usize> {
        let offset = self.distira_bar_offset(address, width)?;
        if (DISTIRA_PCI_CMDFIFO_OFFSET..DISTIRA_PCI_LFB_OFFSET).contains(&offset)
            && offset + width as u32 <= DISTIRA_PCI_LFB_OFFSET
        {
            Some((offset - DISTIRA_PCI_CMDFIFO_OFFSET) as usize)
        } else {
            None
        }
    }

    fn distira_bar_offset(&self, address: u32, width: usize) -> Option<u32> {
        if !self.distira_memory_enabled() {
            return None;
        }
        let offset = address.checked_sub(self.distira_mem_base)?;
        let end = offset.checked_add(width as u32)?;
        if end <= DISTIRA_PCI_BAR_SIZE {
            Some(offset)
        } else {
            None
        }
    }

    fn distira_memory_enabled(&self) -> bool {
        self.distira_command & 0x0002 != 0
    }

    pub(crate) fn distira_memory_decode_key(&self) -> (bool, u32) {
        (self.distira_memory_enabled(), self.distira_mem_base)
    }

    pub(crate) fn distira_bar_overlaps(&self, start: usize, end: usize) -> bool {
        if !self.distira_memory_enabled() {
            return false;
        }
        let bar_start = u64::from(self.distira_mem_base);
        let bar_end = bar_start + u64::from(DISTIRA_PCI_BAR_SIZE);
        (start as u64) < bar_end && bar_start < (end as u64)
    }

    fn read_data(&self, port_offset: u16, width: BusWidth) -> u32 {
        let base = (self.address & 0xfc) + u32::from(port_offset);
        (0..width.bytes())
            .map(|index| u32::from(self.read_config_byte(base + index)) << (index * 8))
            .fold(0, |a, b| a | b)
    }

    fn write_data(&mut self, port_offset: u16, width: BusWidth, value: u32) {
        let base = (self.address & 0xfc) + u32::from(port_offset);
        for index in 0..width.bytes() {
            self.write_config_byte(base + index, ((value >> (index * 8)) & 0xff) as u8);
        }
    }

    fn read_config_byte(&self, offset: u32) -> u8 {
        if self.piix_selected(0) {
            return read_piix4_isa_byte(offset);
        }
        if self.piix_selected(1) {
            return self.read_piix4_ide_byte(offset);
        }
        if self.distira_selected() {
            return self.read_distira_byte(offset);
        }
        0xff
    }

    fn read_distira_byte(&self, offset: u32) -> u8 {
        match offset {
            0x00 => (DISTIRA_PCI_VENDOR_ID & 0xff) as u8,
            0x01 => (DISTIRA_PCI_VENDOR_ID >> 8) as u8,
            0x02 => (DISTIRA_PCI_DEVICE_ID & 0xff) as u8,
            0x03 => (DISTIRA_PCI_DEVICE_ID >> 8) as u8,
            0x04 => (self.distira_command & 0xff) as u8,
            0x05 => (self.distira_command >> 8) as u8,
            0x08 => DISTIRA_PCI_REVISION,
            0x09 => 0x00,
            0x0a => 0x00,
            0x0b => 0x04,
            0x0e => 0x00,
            0x10..=0x13 => ((self.distira_mem_base >> ((offset - 0x10) * 8)) & 0xff) as u8,
            0x40..=0x43 => ((self.distira_init_enable >> ((offset - 0x40) * 8)) & 0xff) as u8,
            _ => 0x00,
        }
    }

    fn read_piix4_ide_byte(&self, offset: u32) -> u8 {
        match offset {
            0x00 => (INTEL_VENDOR_ID & 0xff) as u8,
            0x01 => (INTEL_VENDOR_ID >> 8) as u8,
            0x02 => (PIIX4_IDE_DEVICE_ID & 0xff) as u8,
            0x03 => (PIIX4_IDE_DEVICE_ID >> 8) as u8,
            0x04 => self.piix_ide_command as u8,
            0x05 => (self.piix_ide_command >> 8) as u8,
            0x08 => PIIX4_REVISION,
            0x09 => 0x80, // legacy primary/secondary channels, bus mastering
            0x0a => 0x01, // IDE subclass
            0x0b => 0x01, // mass-storage class
            0x0e => 0x00,
            0x20..=0x23 => {
                let bar = self.piix_ide_bm_base | 1;
                ((bar >> ((offset - 0x20) * 8)) & 0xff) as u8
            }
            // Compatibility channels use legacy IRQ14/15, not one PCI INTA pin.
            0x3c => 0xff,
            0x3d => 0,
            _ => 0,
        }
    }

    fn write_config_byte(&mut self, offset: u32, value: u8) {
        if self.piix_selected(1) {
            self.write_piix4_ide_byte(offset, value);
            return;
        }
        if !self.distira_selected() {
            return;
        }
        match offset {
            0x04 => {
                self.distira_command = (self.distira_command & !0x0002) | u16::from(value & 0x02);
            }
            0x10..=0x12 => {}
            0x13 => self.distira_mem_base = u32::from(value) << 24,
            0x40..=0x43 => {
                let shift = (offset - 0x40) * 8;
                self.distira_init_enable =
                    (self.distira_init_enable & !(0xff << shift)) | (u32::from(value) << shift);
            }
            _ => {}
        }
    }

    fn write_piix4_ide_byte(&mut self, offset: u32, value: u8) {
        match offset {
            0x04 => {
                self.piix_ide_command =
                    u16::from(value) & (PCI_COMMAND_IO | PCI_COMMAND_BUS_MASTER);
            }
            0x05 => {}
            0x20..=0x23 => {
                let shift = (offset - 0x20) * 8;
                self.piix_ide_bm_base =
                    (self.piix_ide_bm_base & !(0xff << shift)) | (u32::from(value) << shift);
                self.piix_ide_bm_base &= !0x0f;
            }
            _ => {}
        }
    }

    fn distira_selected(&self) -> bool {
        self.address & 0x8000_0000 != 0
            && ((self.address >> 16) & 0xff) == 0
            && ((self.address >> 11) & 0x1f) as u8 == DISTIRA_PCI_SLOT
            && ((self.address >> 8) & 0x07) == 0
    }

    fn piix_selected(&self, function: u8) -> bool {
        self.address & 0x8000_0000 != 0
            && ((self.address >> 16) & 0xff) == 0
            && ((self.address >> 11) & 0x1f) as u8 == PIIX_SLOT
            && ((self.address >> 8) & 0x07) as u8 == function
    }

    pub(crate) fn distira_init_enable(&self) -> u32 {
        self.distira_init_enable
    }
}

fn read_piix4_isa_byte(offset: u32) -> u8 {
    match offset {
        0x00 => (INTEL_VENDOR_ID & 0xff) as u8,
        0x01 => (INTEL_VENDOR_ID >> 8) as u8,
        0x02 => (PIIX4_ISA_DEVICE_ID & 0xff) as u8,
        0x03 => (PIIX4_ISA_DEVICE_ID >> 8) as u8,
        0x08 => PIIX4_REVISION,
        0x09 => 0x00,
        0x0a => 0x01, // ISA bridge subclass
        0x0b => 0x06, // bridge class
        0x0e => 0x80, // multifunction, so enumerators probe function 1
        _ => 0,
    }
}

fn port_span_in(port: u16, width: BusWidth, start: u16, end: u16) -> bool {
    let size = width.bytes() as u16;
    port >= start && port + size - 1 <= end
}

fn read_register_bytes(register: u32, byte_offset: u16, width: BusWidth) -> u32 {
    let shift = byte_offset * 8;
    (register >> shift) & (0xffffffff >> (32 - width.bytes() * 8))
}

fn write_register_bytes(register: u32, byte_offset: u16, width: BusWidth, value: u32) -> u32 {
    let mask = 0xffffffff >> (32 - width.bytes() * 8);
    let shift = byte_offset * 8;
    (register & !(mask << shift)) | ((value & mask) << shift)
}

#[cfg(test)]
#[path = "pci_test.rs"]
mod tests;
