// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn select(config: &mut PciConfig, slot: u8, function: u8, register: u8) {
    let address = 0x8000_0000
        | (u32::from(slot) << 11)
        | (u32::from(function) << 8)
        | u32::from(register & 0xfc);
    assert!(config.write_io(PCI_CONFIG_ADDRESS_PORT, BusWidth::Dword, address));
}

fn read_dword(config: &mut PciConfig, slot: u8, function: u8, register: u8) -> u32 {
    select(config, slot, function, register);
    config
        .read_io(PCI_CONFIG_DATA_PORT, BusWidth::Dword)
        .unwrap()
}

fn write_dword(config: &mut PciConfig, slot: u8, function: u8, register: u8, value: u32) {
    select(config, slot, function, register);
    assert!(config.write_io(PCI_CONFIG_DATA_PORT, BusWidth::Dword, value));
}

#[test]
fn piix4_functions_enumerate_as_a_multifunction_ide_controller() {
    let mut config = PciConfig::new();
    assert_eq!(read_dword(&mut config, 7, 0, 0), 0x7110_8086);
    let isa_class = read_dword(&mut config, 7, 0, 8);
    assert_eq!(isa_class >> 8, 0x0006_0100);
    assert_eq!(read_dword(&mut config, 7, 0, 0x0c) >> 16 & 0xff, 0x80);

    assert_eq!(read_dword(&mut config, 7, 1, 0), 0x7111_8086);
    let ide_class = read_dword(&mut config, 7, 1, 8);
    assert_eq!(ide_class >> 8, 0x0001_0180);
    assert_eq!(read_dword(&mut config, 7, 1, 0x3c) & 0xffff, 0x00ff);
    assert_eq!(read_dword(&mut config, 7, 2, 0), u32::MAX);
}

#[test]
fn ide_bar4_sizes_relocates_and_command_bits_gate_decode() {
    let mut config = PciConfig::new();
    assert_eq!(read_dword(&mut config, 7, 1, 0x20), 0x0000_f001);
    assert_eq!(config.ide_bus_master_io_base(), Some(0xf000));
    assert!(config.ide_io_enabled());
    assert!(config.ide_bus_master_enabled());

    write_dword(&mut config, 7, 1, 0x20, u32::MAX);
    assert_eq!(read_dword(&mut config, 7, 1, 0x20), 0xffff_fff1);
    assert_eq!(config.ide_bus_master_io_base(), None);

    write_dword(&mut config, 7, 1, 0x20, 0x0000_e007);
    assert_eq!(read_dword(&mut config, 7, 1, 0x20), 0x0000_e001);
    assert_eq!(config.ide_bus_master_io_base(), Some(0xe000));

    write_dword(&mut config, 7, 1, 0x04, 0);
    assert!(!config.ide_io_enabled());
    assert!(!config.ide_bus_master_enabled());
    write_dword(
        &mut config,
        7,
        1,
        0x04,
        u32::from(PCI_COMMAND_IO | PCI_COMMAND_BUS_MASTER),
    );
    assert!(config.ide_io_enabled());
    assert!(config.ide_bus_master_enabled());
}
