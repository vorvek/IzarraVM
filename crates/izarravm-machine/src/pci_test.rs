// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::{DISTIRA_PCI_BAR_SIZE, DISTIRA_PCI_LFB_OFFSET, DISTIRA_PCI_TEX_OFFSET};

struct PciRig {
    config: PciConfig,
    vega: Vega,
}

impl PciRig {
    fn new() -> Self {
        Self {
            config: PciConfig::new(),
            vega: Vega::default(),
        }
    }

    fn select(&mut self, slot: u8, function: u8, register: u8) {
        let address = 0x8000_0000
            | (u32::from(slot) << 11)
            | (u32::from(function) << 8)
            | u32::from(register & 0xfc);
        assert!(self.config.write_io(
            PCI_CONFIG_ADDRESS_PORT,
            BusWidth::Dword,
            address,
            &mut self.vega,
        ));
    }

    fn read_dword(&mut self, slot: u8, function: u8, register: u8) -> u32 {
        self.select(slot, function, register);
        self.config
            .read_io(PCI_CONFIG_DATA_PORT, BusWidth::Dword, &self.vega)
            .unwrap()
    }

    fn write_dword(&mut self, slot: u8, function: u8, register: u8, value: u32) {
        self.select(slot, function, register);
        assert!(
            self.config
                .write_io(PCI_CONFIG_DATA_PORT, BusWidth::Dword, value, &mut self.vega,)
        );
    }
}

#[test]
fn piix4_functions_enumerate_as_a_multifunction_ide_controller() {
    let mut rig = PciRig::new();
    assert_eq!(rig.read_dword(7, 0, 0), 0x7110_8086);
    let isa_class = rig.read_dword(7, 0, 8);
    assert_eq!(isa_class >> 8, 0x0006_0100);
    assert_eq!(rig.read_dword(7, 0, 0x0c) >> 16 & 0xff, 0x80);

    assert_eq!(rig.read_dword(7, 1, 0), 0x7111_8086);
    let ide_class = rig.read_dword(7, 1, 8);
    assert_eq!(ide_class >> 8, 0x0001_0180);
    assert_eq!(rig.read_dword(7, 1, 0x3c) & 0xffff, 0x00ff);
    assert_eq!(rig.read_dword(7, 2, 0), u32::MAX);
}

#[test]
fn ide_bar4_sizes_relocates_and_command_bits_gate_decode() {
    let mut rig = PciRig::new();
    assert_eq!(rig.read_dword(7, 1, 0x20), 0x0000_f001);
    assert_eq!(rig.config.ide_bus_master_io_base(), Some(0xf000));
    assert!(rig.config.ide_io_enabled());
    assert!(rig.config.ide_bus_master_enabled());

    rig.write_dword(7, 1, 0x20, u32::MAX);
    assert_eq!(rig.read_dword(7, 1, 0x20), 0xffff_fff1);
    assert_eq!(rig.config.ide_bus_master_io_base(), None);

    rig.write_dword(7, 1, 0x20, 0x0000_e007);
    assert_eq!(rig.read_dword(7, 1, 0x20), 0x0000_e001);
    assert_eq!(rig.config.ide_bus_master_io_base(), Some(0xe000));

    rig.write_dword(7, 1, 0x04, 0);
    assert!(!rig.config.ide_io_enabled());
    assert!(!rig.config.ide_bus_master_enabled());
    rig.write_dword(
        7,
        1,
        0x04,
        u32::from(PCI_COMMAND_IO | PCI_COMMAND_BUS_MASTER),
    );
    assert!(rig.config.ide_io_enabled());
    assert!(rig.config.ide_bus_master_enabled());
}

#[test]
fn distira_bar0_probe_reports_a_16_mib_aligned_aperture() {
    let mut rig = PciRig::new();
    let power_on_base = crate::DISTIRA_MMIO_BASE;

    assert_eq!(rig.read_dword(DISTIRA_PCI_SLOT, 0, 0x10), power_on_base);
    assert_eq!(rig.vega.memory_decode_key(), Some(power_on_base));

    assert_eq!(
        rig.vega.distira_mmio_offset(power_on_base + 0x0020_0000, 4),
        Some(0x0020_0000)
    );
    assert_eq!(
        rig.vega
            .distira_mmio_offset(power_on_base + DISTIRA_PCI_LFB_OFFSET - 1, 1),
        Some((DISTIRA_PCI_LFB_OFFSET - 1) as usize)
    );
    assert_eq!(
        rig.vega
            .distira_mmio_offset(power_on_base + DISTIRA_PCI_LFB_OFFSET - 1, 2),
        None
    );
    assert_eq!(
        rig.vega
            .distira_lfb_offset(power_on_base + DISTIRA_PCI_LFB_OFFSET, 4),
        Some(0)
    );
    assert_eq!(
        rig.vega
            .distira_texture_offset(power_on_base + DISTIRA_PCI_TEX_OFFSET, 4),
        Some(0)
    );
    assert_eq!(
        rig.vega
            .distira_texture_offset(power_on_base + DISTIRA_PCI_BAR_SIZE - 1, 1),
        Some((DISTIRA_PCI_BAR_SIZE - DISTIRA_PCI_TEX_OFFSET - 1) as usize)
    );
    assert_eq!(
        rig.vega
            .distira_texture_offset(power_on_base + DISTIRA_PCI_BAR_SIZE - 1, 2),
        None
    );
    assert_eq!(
        rig.vega
            .distira_texture_offset(power_on_base + DISTIRA_PCI_BAR_SIZE, 1),
        None
    );

    rig.write_dword(DISTIRA_PCI_SLOT, 0, 0x10, u32::MAX);
    assert_eq!(rig.read_dword(DISTIRA_PCI_SLOT, 0, 0x10), 0xff00_0000);
    assert_eq!(rig.vega.memory_decode_key(), Some(0xff00_0000));
    assert_eq!(
        rig.vega.distira_texture_offset(u32::MAX, 1),
        Some(0x7f_ffff)
    );

    rig.write_dword(DISTIRA_PCI_SLOT, 0, 0x10, 0xe2ab_cdef);
    assert_eq!(rig.read_dword(DISTIRA_PCI_SLOT, 0, 0x10), 0xe200_0000);
}

#[test]
fn distira_memory_command_and_zero_bar_disable_decode() {
    let mut rig = PciRig::new();

    rig.write_dword(DISTIRA_PCI_SLOT, 0, 0x04, 0);
    assert_eq!(rig.vega.memory_decode_key(), None);
    assert_eq!(
        rig.vega.distira_mmio_offset(crate::DISTIRA_MMIO_BASE, 4),
        None
    );

    rig.write_dword(DISTIRA_PCI_SLOT, 0, 0x10, 0xe300_0000);
    assert_eq!(rig.vega.memory_decode_key(), None);

    rig.write_dword(DISTIRA_PCI_SLOT, 0, 0x04, 0x0000_0002);
    assert_eq!(rig.vega.memory_decode_key(), Some(0xe300_0000));
    assert_eq!(rig.vega.distira_mmio_offset(0xe300_0000, 4), Some(0));

    rig.write_dword(DISTIRA_PCI_SLOT, 0, 0x10, 0);
    assert_eq!(rig.read_dword(DISTIRA_PCI_SLOT, 0, 0x10), 0);
    assert_eq!(rig.vega.memory_decode_key(), None);
}

#[test]
fn mechanism_one_port_spans_preserve_partial_address_cycles() {
    const INITIAL: u32 = 0x1234_5678;
    const VALUE: u32 = 0xa1b2_c3d4;

    for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
        let last_valid = 4 - width.bytes() as u16;
        for offset in 0..=last_valid {
            let mut rig = PciRig::new();
            assert!(rig.config.write_io(
                PCI_CONFIG_ADDRESS_PORT,
                BusWidth::Dword,
                INITIAL,
                &mut rig.vega,
            ));
            assert!(rig.config.write_io(
                PCI_CONFIG_ADDRESS_PORT + offset,
                width,
                VALUE,
                &mut rig.vega,
            ));
            assert_eq!(
                rig.config
                    .read_io(PCI_CONFIG_ADDRESS_PORT, BusWidth::Dword, &rig.vega),
                Some(write_register_bytes(INITIAL, offset, width, VALUE))
            );
        }

        if last_valid < 3 {
            let mut rig = PciRig::new();
            assert!(rig.config.write_io(
                PCI_CONFIG_ADDRESS_PORT,
                BusWidth::Dword,
                INITIAL,
                &mut rig.vega,
            ));
            assert!(!rig.config.write_io(
                PCI_CONFIG_ADDRESS_PORT + last_valid + 1,
                width,
                VALUE,
                &mut rig.vega,
            ));
            assert_eq!(
                rig.config
                    .read_io(PCI_CONFIG_ADDRESS_PORT, BusWidth::Dword, &rig.vega),
                Some(INITIAL)
            );
        }
    }
}

#[test]
fn disabled_mechanism_one_data_cycles_are_lane_exact_and_inert() {
    let mut rig = PciRig::new();
    assert!(rig.config.write_io(
        PCI_CONFIG_ADDRESS_PORT,
        BusWidth::Dword,
        0x0000_3923,
        &mut rig.vega,
    ));

    for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
        let last_valid = 4 - width.bytes() as u16;
        let expected = match width {
            BusWidth::Byte => 0xff,
            BusWidth::Word => 0xffff,
            BusWidth::Dword => u32::MAX,
        };
        for offset in 0..=last_valid {
            assert_eq!(
                rig.config
                    .read_io(PCI_CONFIG_DATA_PORT + offset, width, &rig.vega),
                Some(expected)
            );
            assert!(rig.config.write_io(
                PCI_CONFIG_DATA_PORT + offset,
                width,
                0xa1b2_c3d4,
                &mut rig.vega,
            ));
        }
        if last_valid < 3 {
            assert_eq!(
                rig.config
                    .read_io(PCI_CONFIG_DATA_PORT + last_valid + 1, width, &rig.vega),
                None
            );
            assert!(!rig.config.write_io(
                PCI_CONFIG_DATA_PORT + last_valid + 1,
                width,
                0xa1b2_c3d4,
                &mut rig.vega,
            ));
        }
    }

    assert_eq!(rig.config.address, 0x0000_3923);
    assert_eq!(
        rig.config.piix_ide_command,
        PCI_COMMAND_IO | PCI_COMMAND_BUS_MASTER
    );
    assert_eq!(rig.config.piix_ide_bm_base, PIIX4_BMIDE_POWER_ON_BASE);
}
