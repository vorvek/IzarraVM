// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const BMIDE_BASE: u16 = 0xf000;
const BM_COMMAND: u16 = BMIDE_BASE;
const BM_STATUS: u16 = BMIDE_BASE + 2;
const BM_PRD: u16 = BMIDE_BASE + 4;
const ATA_STATUS: u16 = ata::PRIMARY_CMD_BASE + 7;
const STATUS_BSY: u8 = 0x80;
const STATUS_DRQ: u8 = 0x08;

fn out(machine: &mut Machine, port: u16, width: BusWidth, value: u32) {
    with_bus(machine, |bus| {
        bus.write_io(port, width, value, false).unwrap();
    });
}

fn input(machine: &mut Machine, port: u16, width: BusWidth) -> u32 {
    with_bus(machine, |bus| bus.read_io(port, width, 0, false).unwrap())
}

fn program_lba(machine: &mut Machine, lba: u32, sectors: u8, command: u8) {
    out(
        machine,
        ata::PRIMARY_CMD_BASE + 2,
        BusWidth::Byte,
        u32::from(sectors),
    );
    out(
        machine,
        ata::PRIMARY_CMD_BASE + 3,
        BusWidth::Byte,
        lba & 0xff,
    );
    out(
        machine,
        ata::PRIMARY_CMD_BASE + 4,
        BusWidth::Byte,
        (lba >> 8) & 0xff,
    );
    out(
        machine,
        ata::PRIMARY_CMD_BASE + 5,
        BusWidth::Byte,
        (lba >> 16) & 0xff,
    );
    out(
        machine,
        ata::PRIMARY_CMD_BASE + 6,
        BusWidth::Byte,
        0x40 | ((lba >> 24) & 0x0f),
    );
    out(machine, ATA_STATUS, BusWidth::Byte, u32::from(command));
}

fn arm_dma(machine: &mut Machine, memory: u32, lba: u32, read_from_disk: bool) -> u64 {
    const PRD: u32 = 0x1000;
    machine.write_physical_u32(PRD, memory);
    machine.write_physical_u32(PRD + 4, 0x8000_0200);
    out(machine, BM_PRD, BusWidth::Dword, PRD);
    out(
        machine,
        BM_COMMAND,
        BusWidth::Byte,
        if read_from_disk { 0x09 } else { 0x01 },
    );
    program_lba(machine, lba, 1, if read_from_disk { 0xc8 } else { 0xca });
    machine.bmide.ticks_until_completion().unwrap()
}

fn enable_irq14_wake(machine: &mut Machine) {
    machine.cpu.registers.eflags |= 0x0200;
    for (port, value) in [
        (0x20, 0x11),
        (0x21, 0x08),
        (0x21, 0x04),
        (0x21, 0x01),
        (0xa0, 0x11),
        (0xa1, 0x70),
        (0xa1, 0x02),
        (0xa1, 0x01),
        (0x21, 0xfb),
        (0xa1, 0xbf),
    ] {
        out(machine, port, BusWidth::Byte, value);
    }
}

#[test]
fn dma_last_prerequisite_sets_the_physical_start_time() {
    use izarravm_core::MASTER_CLOCK_HZ;
    const CORE: u64 = 117;
    const DEST: u32 = 0x2000;
    let duration = MASTER_CLOCK_HZ / 10_000 + (512 * MASTER_CLOCK_HZ).div_ceil(33_300_000);
    for last in 0..3 {
        let mut machine = machine_with_hdd(8);
        machine.set_mode(GswMode::Gsw386Slow);
        machine.write_physical_u32(0x1000, DEST);
        machine.write_physical_u32(0x1004, 0x8000_0200);
        for offset in 0..514 {
            machine.write_physical_u8(DEST - 1 + offset, 0xa5);
        }
        {
            let mut bus = machine.make_construction_bus();
            for (port, width, value) in [
                (0xcf8, BusWidth::Dword, 0x8000_3904),
                (0xcfc, BusWidth::Word, if last == 2 { 1 } else { 5 }),
                (BM_PRD, BusWidth::Dword, 0x1000),
                (0x1f2, BusWidth::Byte, 1),
                (0x1f3, BusWidth::Byte, 2),
                (0x1f4, BusWidth::Byte, 0),
                (0x1f5, BusWidth::Byte, 0),
                (0x1f6, BusWidth::Byte, 0x40),
            ] {
                CpuBus::write_io(&mut bus, port, width, value, 0, false).unwrap();
            }
            if last != 0 {
                CpuBus::write_io(&mut bus, BM_COMMAND, BusWidth::Byte, 9, 0, false).unwrap();
            }
            if last != 1 {
                CpuBus::write_io(&mut bus, ATA_STATUS, BusWidth::Byte, 0xc8, 0, false).unwrap();
            }
        }
        assert_eq!(machine.bmide.ticks_until_completion(), None);
        assert_eq!(machine.port_bus_batch_clocks, 0);
        let (port, width, value, tariff) = match last {
            0 => (BM_COMMAND, BusWidth::Byte, 9, 56),
            1 => (ATA_STATUS, BusWidth::Byte, 0xc8, 56),
            _ => (0xcfc, BusWidth::Word, 5, 56),
        };
        let raw = machine.raw_bus_clocks();
        let prefix = with_bus(&mut machine, |bus| {
            bus.prior_runs_core_clocks = CORE - 17;
            CpuBus::write_io(bus, port, width, value, 17, false).unwrap();
            bus.in_batch_master_ticks()
        });
        assert_eq!(prefix, CORE * 747 + tariff * 33, "last={last}");
        assert_eq!(machine.raw_bus_clocks() - raw, 4);
        assert_eq!(
            std::mem::take(&mut machine.port_bus_batch_clocks),
            tariff - 4
        );
        assert_eq!(
            machine.bmide.ticks_until_completion(),
            Some(prefix + duration)
        );
        {
            let mut bus = machine.make_construction_bus();
            CpuBus::write_io(&mut bus, 0xcfc, BusWidth::Word, 5, 0, false).unwrap();
        }
        assert_eq!(
            machine.bmide.ticks_until_completion(),
            Some(prefix + duration)
        );
        machine.advance_devices_ticks(prefix + duration - 1);
        assert_eq!(machine.bmide.ticks_until_completion(), Some(1));
        for offset in 0..514 {
            assert_eq!(machine.read_physical_u8(DEST - 1 + offset), 0xa5);
        }
        machine.advance_devices_ticks(1);
        assert_eq!(machine.bmide.ticks_until_completion(), None);
        let expected = machine.ata.as_ref().unwrap().read_lba(2).unwrap();
        for (offset, byte) in expected.into_iter().enumerate() {
            assert_eq!(machine.read_physical_u8(DEST + offset as u32), byte);
        }
        assert_eq!(machine.read_physical_u8(DEST - 1), 0xa5);
        assert_eq!(machine.read_physical_u8(DEST + 512), 0xa5);
        let mut bus = machine.make_construction_bus();
        assert_eq!(
            bus.read_io(BM_STATUS, BusWidth::Byte, 0, false).unwrap() & 5,
            4
        );
    }
}

#[test]
fn mounted_pio_sector_joins_one_cable_bill_to_its_completion() {
    const CORE: u64 = 117;
    const CABLE: u64 = 256 * 20 * 33;
    for write in [false, true] {
        let mut machine = machine_with_hdd(8);
        machine.set_mode(GswMode::Gsw386Slow);
        {
            let mut bus = machine.make_construction_bus();
            for (port, value) in [
                (0x1f2, if write { 1 } else { 2 }),
                (0x1f3, 1),
                (0x1f4, 0),
                (0x1f5, 0),
                (0x1f6, 0x40),
                (0x1f7, if write { 0x30 } else { 0x20 }),
            ] {
                CpuBus::write_io(&mut bus, port, BusWidth::Byte, value, 0, false).unwrap();
            }
        }
        assert_eq!(
            machine.ata.as_ref().unwrap().ticks_until_completion(),
            Some(1)
        );
        machine.advance_devices_ticks(1);
        machine.ata.as_mut().unwrap().read_port(ATA_STATUS);
        {
            let mut bus = machine.make_construction_bus();
            CpuBus::write_io(&mut bus, BM_STATUS, BusWidth::Byte, 4, 0, false).unwrap();
        }
        let expected = machine.ata.as_ref().unwrap().read_lba(1).unwrap();
        let raw = machine.raw_bus_clocks();
        let mut observed = Vec::new();
        let prefix = with_bus(&mut machine, |bus| {
            bus.prior_runs_core_clocks = CORE - 17;
            for _ in 0..256 {
                if write {
                    CpuBus::write_io(bus, 0x1f0, BusWidth::Word, 0x5a5a, 17, false).unwrap();
                } else {
                    let word = bus.read_io(0x1f0, BusWidth::Word, 17, false).unwrap();
                    observed.extend_from_slice(&(word as u16).to_le_bytes());
                }
            }
            bus.in_batch_master_ticks()
        });
        if !write {
            assert_eq!(observed, expected);
        }
        assert_eq!(prefix, CORE * 747 + CABLE);
        assert_eq!(machine.raw_bus_clocks() - raw, 256 * 4);
        assert_eq!(std::mem::take(&mut machine.port_bus_batch_clocks), 256 * 16);
        assert_eq!(
            machine.ata.as_ref().unwrap().ticks_until_completion(),
            Some(prefix + 1)
        );
        machine.advance_devices_ticks(prefix);
        let disk = machine.ata.as_mut().unwrap();
        assert_eq!(disk.read_port(ata::PRIMARY_CTRL).unwrap() & STATUS_DRQ, 0);
        assert_eq!(disk.read_lba(1).unwrap(), expected);
        assert_eq!(disk.ticks_until_completion(), Some(1));
        machine.advance_devices_ticks(1);
        {
            let mut bus = machine.make_construction_bus();
            assert_ne!(
                bus.read_io(BM_STATUS, BusWidth::Byte, 0, false).unwrap() & 4,
                0
            );
        }
        let disk = machine.ata.as_mut().unwrap();
        assert_eq!(disk.ticks_until_completion(), None);
        if write {
            assert_eq!(disk.read_lba(1).unwrap(), [0x5a; 512]);
            assert_eq!(disk.read_port(0x1f2), Some(0));
        } else {
            assert_ne!(disk.read_port(ata::PRIMARY_CTRL).unwrap() & STATUS_DRQ, 0);
            assert_eq!(disk.read_port(0x1f2), Some(1));
            assert_eq!(disk.read_port(0x1f3), Some(2));
            assert_eq!(disk.read_port(0x1f0), Some(0x12));
        }
    }
}

#[test]
fn piix4_ide_function_exposes_bar4_and_honors_io_decode() {
    let mut machine = machine_with_hdd(8);
    out(&mut machine, 0xcf8, BusWidth::Dword, 0x8000_3900);
    assert_eq!(input(&mut machine, 0xcfc, BusWidth::Dword), 0x7111_8086);

    out(&mut machine, 0xcf8, BusWidth::Dword, 0x8000_3920);
    assert_eq!(input(&mut machine, 0xcfc, BusWidth::Dword), 0x0000_f001);

    out(&mut machine, 0xcf8, BusWidth::Dword, 0x8000_3904);
    out(&mut machine, 0xcfc, BusWidth::Word, 0x0004);
    assert_eq!(input(&mut machine, BM_STATUS, BusWidth::Byte), 0xff);
    out(&mut machine, 0xcfc, BusWidth::Word, 0x0005);
    assert_eq!(input(&mut machine, BM_STATUS, BusWidth::Byte), 0x00);
}

#[test]
fn pci_bios_bus_master_disable_fails_inflight_dma_like_the_port_path() {
    // Dropping the PIIX bus-master enable with a DMA transfer in flight must
    // fail the transfer identically whether the config write arrives through
    // CF8/CFC or through the PCI BIOS service.
    let mut port_m = machine_with_hdd(8);
    let mut bios_m = machine_with_hdd(8);
    arm_dma(&mut port_m, 0x2000, 2, true);
    arm_dma(&mut bios_m, 0x2000, 2, true);
    assert!(port_m.bmide.ticks_until_completion().is_some());
    assert!(bios_m.bmide.ticks_until_completion().is_some());

    out(&mut port_m, 0xcf8, BusWidth::Dword, 0x8000_3904);
    out(&mut port_m, 0xcfc, BusWidth::Word, 0x0001);

    bios_m.cpu.registers.set_ebx(0x0039);
    bios_m.cpu.registers.set_edi(4);
    bios_m.cpu.registers.set_ecx(0x0001);
    bios_m.cpu.registers.set_eax(0xB10C);
    bios_m.handle_pci_bios(true);
    assert_eq!(bios_m.cpu.registers.eflags & 1, 0);

    assert!(!port_m.pci.ide_bus_master_enabled());
    assert!(!bios_m.pci.ide_bus_master_enabled());
    assert!(port_m.bmide.ticks_until_completion().is_none());
    assert!(bios_m.bmide.ticks_until_completion().is_none());
    assert_eq!(
        bios_m
            .pci
            .read_bdf(0, 0x39, 4, BusWidth::Word, &bios_m.vega),
        port_m
            .pci
            .read_bdf(0, 0x39, 4, BusWidth::Word, &port_m.vega),
    );
    assert_eq!(
        input(&mut port_m, BM_STATUS, BusWidth::Byte),
        input(&mut bios_m, BM_STATUS, BusWidth::Byte),
    );
}

#[test]
fn secondary_pio_interrupt_latches_in_the_bmide_status_bank() {
    let mut machine = machine_with_hdd(8);
    out(
        &mut machine,
        ide::SECONDARY_CMD_BASE + 7,
        BusWidth::Byte,
        0xa1,
    );
    let deadline = machine.ide.ticks_until_completion().unwrap();
    machine.advance_devices_ticks(deadline - 1);
    assert_eq!(
        input(&mut machine, BMIDE_BASE + 10, BusWidth::Byte) & 0x04,
        0
    );
    machine.advance_devices_ticks(1);
    assert_ne!(
        input(&mut machine, BMIDE_BASE + 10, BusWidth::Byte) & 0x04,
        0
    );
}

#[test]
fn dma_read_lands_on_its_exact_master_tick_across_a_live_mode_switch() {
    let mut machine = machine_with_hdd(8);
    machine.set_mode(GswMode::Gsw586);
    let deadline = arm_dma(&mut machine, 0x2000, 2, true);
    assert_eq!(
        input(&mut machine, ATA_STATUS, BusWidth::Byte) as u8,
        STATUS_BSY
    );
    assert_eq!(input(&mut machine, BM_STATUS, BusWidth::Byte) & 1, 1);
    assert_eq!(machine.read_physical_u8(0x2000), 0);
    let expected_cap = machine
        .timeline
        .cpu_clocks_for_master_ticks_ceil(deadline)
        .max(1);
    assert_eq!(machine.event_batch_cap(u64::MAX), expected_cap);

    let generation = machine.cpu.decode_cache_generation();
    machine.advance_devices_ticks(deadline - 1);
    assert_eq!(machine.read_physical_u8(0x2000), 0);
    assert_eq!(machine.cpu.decode_cache_generation(), generation);
    machine.set_mode(GswMode::Gsw386Slow);
    let generation = machine.cpu.decode_cache_generation();
    machine.advance_devices_ticks(1);

    assert_eq!(machine.read_physical_u8(0x2000), 0x12);
    assert_eq!(
        machine.cpu.decode_cache_generation(),
        generation,
        "DMA into unrelated data must preserve decoded and compiled code"
    );
    assert_eq!(input(&mut machine, BM_STATUS, BusWidth::Byte) & 0x05, 0x04);
    assert_eq!(
        input(&mut machine, ATA_STATUS, BusWidth::Byte) as u8 & STATUS_BSY,
        0
    );
    assert_eq!(
        input(&mut machine, ata::PRIMARY_CMD_BASE + 2, BusWidth::Byte),
        0
    );
    assert_eq!(
        input(&mut machine, ata::PRIMARY_CMD_BASE + 3, BusWidth::Byte),
        3
    );
}

#[test]
fn pio_read_raises_one_ide_interrupt_at_each_sector_boundary() {
    let mut machine = machine_with_hdd(8);
    program_lba(&mut machine, 0, 2, 0x20);
    let first = machine
        .ata
        .as_ref()
        .and_then(ata::AtaDisk::ticks_until_completion)
        .unwrap();
    machine.advance_devices_ticks(first - 1);
    assert_eq!(
        input(&mut machine, ATA_STATUS, BusWidth::Byte) as u8,
        STATUS_BSY
    );
    machine.advance_devices_ticks(1);
    assert_ne!(
        input(&mut machine, ATA_STATUS, BusWidth::Byte) as u8 & STATUS_DRQ,
        0
    );
    assert_ne!(input(&mut machine, BM_STATUS, BusWidth::Byte) & 0x04, 0);
    out(&mut machine, BM_STATUS, BusWidth::Byte, 0x04);

    let first_word = input(&mut machine, ata::PRIMARY_CMD_BASE, BusWidth::Dword);
    for _ in 1..128 {
        input(&mut machine, ata::PRIMARY_CMD_BASE, BusWidth::Dword);
    }
    assert_eq!(first_word as u8, 0x10);
    assert_eq!(
        input(&mut machine, ATA_STATUS, BusWidth::Byte) as u8,
        STATUS_BSY
    );
    assert_eq!(
        input(&mut machine, ata::PRIMARY_CMD_BASE + 2, BusWidth::Byte),
        1
    );
    assert_eq!(
        input(&mut machine, ata::PRIMARY_CMD_BASE + 3, BusWidth::Byte),
        1
    );

    let second = machine
        .ata
        .as_ref()
        .and_then(ata::AtaDisk::ticks_until_completion)
        .unwrap();
    machine.advance_devices_ticks(second);
    assert_ne!(input(&mut machine, BM_STATUS, BusWidth::Byte) & 0x04, 0);
    let second_word = input(&mut machine, ata::PRIMARY_CMD_BASE, BusWidth::Dword);
    for _ in 1..128 {
        input(&mut machine, ata::PRIMARY_CMD_BASE, BusWidth::Dword);
    }
    assert_eq!(second_word as u8, 0x11);
    assert_eq!(
        input(&mut machine, ATA_STATUS, BusWidth::Byte) as u8 & STATUS_DRQ,
        0
    );
    assert_eq!(
        input(&mut machine, ata::PRIMARY_CMD_BASE + 2, BusWidth::Byte),
        0
    );
    assert_eq!(
        input(&mut machine, ata::PRIMARY_CMD_BASE + 3, BusWidth::Byte),
        2
    );
}

#[test]
fn live_cpu_switch_does_not_move_a_pio_deadline() {
    let mut machine = machine_with_hdd(8);
    machine.set_mode(GswMode::Gsw586);
    out(&mut machine, ATA_STATUS, BusWidth::Byte, 0xec);
    let deadline = machine
        .ata
        .as_ref()
        .and_then(ata::AtaDisk::ticks_until_completion)
        .unwrap();
    let first = deadline / 2;
    machine.advance_devices_ticks(first);
    machine.set_mode(GswMode::Gsw386Slow);
    assert_eq!(
        machine
            .ata
            .as_ref()
            .and_then(ata::AtaDisk::ticks_until_completion),
        Some(deadline - first)
    );
    machine.advance_devices_ticks(deadline - first - 1);
    assert_eq!(
        input(&mut machine, ATA_STATUS, BusWidth::Byte) as u8,
        STATUS_BSY
    );
    machine.advance_devices_ticks(1);
    assert_ne!(
        input(&mut machine, ATA_STATUS, BusWidth::Byte) as u8 & STATUS_DRQ,
        0
    );
}

#[test]
fn dma_write_is_visible_through_the_int13_disk_path() {
    let mut machine = machine_with_hdd(8);
    for offset in 0..ata::SECTOR as u32 {
        machine.write_physical_u8(0x3000 + offset, (offset as u8) ^ 0xa5);
    }
    let deadline = arm_dma(&mut machine, 0x3000, 0, false);
    machine.advance_devices_ticks(deadline);

    machine.cpu.registers.set_eax(0x0201);
    machine.cpu.registers.set_ecx(0x0001);
    machine.cpu.registers.set_edx(0x0080);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
    machine.cpu.registers.set_ebx(0);
    machine.handle_int13();

    assert_eq!((machine.cpu.registers.eax() >> 8) as u8, 0);
    for offset in 0..ata::SECTOR as u32 {
        assert_eq!(
            machine.read_physical_u8(0x4_0000 + offset),
            (offset as u8) ^ 0xa5
        );
    }
}

#[test]
fn halted_cpu_wake_uses_the_primary_ide_deadline_in_every_mode() {
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        let mut machine = machine_with_hdd(8);
        machine.set_mode(mode);
        enable_irq14_wake(&mut machine);
        out(&mut machine, ATA_STATUS, BusWidth::Byte, 0xec);
        let ticks = machine
            .ata
            .as_ref()
            .and_then(ata::AtaDisk::ticks_until_completion)
            .unwrap();
        let expected = machine
            .timeline
            .cpu_clocks_for_master_ticks_ceil(ticks)
            .max(1);
        assert_eq!(
            machine.next_timer_wake(machine.master_ticks() + ticks),
            Some(expected),
            "{mode:?}"
        );
    }
}

#[test]
fn initial_write_drq_is_a_batch_deadline_but_not_a_halt_wake() {
    let mut machine = machine_with_hdd(8);
    machine.set_mode(GswMode::Gsw586);
    enable_irq14_wake(&mut machine);
    program_lba(&mut machine, 0, 1, 0x30);
    let ticks = machine
        .ata
        .as_ref()
        .and_then(ata::AtaDisk::ticks_until_completion)
        .unwrap();
    assert_eq!(
        machine.event_batch_cap(u64::MAX),
        machine.timeline.cpu_clocks_for_master_ticks_ceil(ticks)
    );
    assert_eq!(
        machine.next_timer_wake(machine.master_ticks() + ticks),
        None
    );
}
