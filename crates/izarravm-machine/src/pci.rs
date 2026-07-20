// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! PCI configuration routing and the PIIX4-compatible southbridge.

use izarravm_bus::BusWidth;
use izarravm_core::{CanonicalFieldWriter, CanonicalStateError};

use crate::vega::Vega;
use crate::video_params::{
    DISTIRA_PCI_SLOT, PCI_CONFIG_ADDRESS_PORT, PCI_CONFIG_DATA_END, PCI_CONFIG_DATA_PORT,
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
    piix_ide_command: u16,
    piix_ide_bm_base: u32,
}

/// Borrowed mechanism-1 selector and PIIX IDE state for canonical comparison.
///
/// Distira configuration belongs to the Vega owner, not this projection.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CanonicalPciConfig<'a> {
    pci: &'a PciConfig,
}

impl PciConfig {
    pub(crate) fn new() -> Self {
        Self {
            address: 0,
            // The legacy IDE controller powers on decoded and bus-master capable
            // at its fixed 0xF000 BAR4.
            piix_ide_command: PCI_COMMAND_IO | PCI_COMMAND_BUS_MASTER,
            piix_ide_bm_base: PIIX4_BMIDE_POWER_ON_BASE,
        }
    }

    pub(crate) fn canonical_projection(&self) -> CanonicalPciConfig<'_> {
        CanonicalPciConfig { pci: self }
    }

    pub(crate) fn read_io(&self, port: u16, width: BusWidth, vega: &Vega) -> Option<u32> {
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
            return Some(self.read_data(port - PCI_CONFIG_DATA_PORT, width, vega));
        }
        None
    }

    pub(crate) fn write_io(
        &mut self,
        port: u16,
        width: BusWidth,
        value: u32,
        vega: &mut Vega,
    ) -> bool {
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
            self.write_data(port - PCI_CONFIG_DATA_PORT, width, value, vega);
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

    /// Configuration-space access shared by mechanism #1 port I/O and PCI BIOS.
    /// `devfn` uses the standard device-in-bits-7:3/function-in-bits-2:0 form.
    pub(crate) fn read_bdf(
        &self,
        bus: u8,
        devfn: u8,
        offset: u8,
        width: BusWidth,
        vega: &Vega,
    ) -> u32 {
        (0..width.bytes())
            .map(|index| {
                u32::from(self.read_bdf_byte(bus, devfn, offset.wrapping_add(index as u8), vega))
                    << (index * 8)
            })
            .fold(0, |a, b| a | b)
    }

    pub(crate) fn write_bdf(
        &mut self,
        bus: u8,
        devfn: u8,
        offset: u8,
        width: BusWidth,
        value: u32,
        vega: &mut Vega,
    ) {
        for index in 0..width.bytes() {
            self.write_bdf_byte(
                bus,
                devfn,
                offset.wrapping_add(index as u8),
                ((value >> (index * 8)) & 0xff) as u8,
                vega,
            );
        }
    }

    fn read_data(&self, port_offset: u16, width: BusWidth, vega: &Vega) -> u32 {
        let bus = (self.address >> 16) as u8;
        let devfn = (self.address >> 8) as u8;
        let offset = ((self.address & 0xfc) as u8).wrapping_add(port_offset as u8);
        if self.address & 0x8000_0000 == 0 {
            return match width {
                BusWidth::Byte => 0xff,
                BusWidth::Word => 0xffff,
                BusWidth::Dword => u32::MAX,
            };
        }
        self.read_bdf(bus, devfn, offset, width, vega)
    }

    fn write_data(&mut self, port_offset: u16, width: BusWidth, value: u32, vega: &mut Vega) {
        if self.address & 0x8000_0000 != 0 {
            let bus = (self.address >> 16) as u8;
            let devfn = (self.address >> 8) as u8;
            let offset = ((self.address & 0xfc) as u8).wrapping_add(port_offset as u8);
            self.write_bdf(bus, devfn, offset, width, value, vega);
        }
    }

    fn read_bdf_byte(&self, bus: u8, devfn: u8, offset: u8, vega: &Vega) -> u8 {
        if bus == 0 && devfn == PIIX_SLOT << 3 {
            return read_piix4_isa_byte(u32::from(offset));
        }
        if bus == 0 && devfn == (PIIX_SLOT << 3 | 1) {
            return self.read_piix4_ide_byte(u32::from(offset));
        }
        if bus == 0 && devfn == DISTIRA_PCI_SLOT << 3 {
            return vega.pci_read_config_byte(u32::from(offset));
        }
        0xff
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

    fn write_bdf_byte(&mut self, bus: u8, devfn: u8, offset: u8, value: u8, vega: &mut Vega) {
        if bus == 0 && devfn == (PIIX_SLOT << 3 | 1) {
            self.write_piix4_ide_byte(u32::from(offset), value);
            return;
        }
        if bus == 0 && devfn == DISTIRA_PCI_SLOT << 3 {
            vega.pci_write_config_byte(u32::from(offset), value);
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
}

impl CanonicalPciConfig<'_> {
    /// Writes version 1 of the fixed nine-byte PciConfig payload.
    pub(crate) fn write_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        out.write_u32(self.pci.address)?;
        out.write_u8(
            (self.pci.piix_ide_command & (PCI_COMMAND_IO | PCI_COMMAND_BUS_MASTER)) as u8,
        )?;
        out.write_u32(self.pci.piix_ide_bm_base)
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
