// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_bus::{BusAccessKind, BusWidth, CpuBus, TracingMode};
use izarravm_core::{
    CanonicalSectionId, CanonicalSectionRequirement, CanonicalSectionVersion, CanonicalStateView,
    CanonicalStateWriter, GswMode, MASTER_CLOCK_HZ, VideoCard,
};
use izarravm_cpu::CpuCanonicalCaptureError;
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
use izarravm_cpu::SegmentIndex;

use super::*;
// The epoch-1 line size, which is what every machine in this file runs under.
use crate::cache_config::CACHE_LINE_BYTES;
use crate::{
    BIOS_ROM_SIZE, Bios32Call, JoystickState, MachineProfile, StopReason, WaitStateProfile,
    unittester,
};

const MACHINE_CONTROL_TIMING_PAYLOAD_LEN: usize = 163;
const EMPTY_MODELED_CACHE_PAYLOAD_LEN: usize = 16;
const PIC_PAYLOAD_LEN: usize = 34;
const PIT_PAYLOAD_LEN: usize = 60;
const DMA_PAYLOAD_LEN: usize = 152;
const DMA_EVENT_TOTALS_V1_PAYLOAD_LEN: usize = 64;
const RTC_PAYLOAD_LEN: usize = 82;
const UNIT_TESTER_PAYLOAD_LEN: usize = 33;
const SPEAKER_PAYLOAD_LEN: usize = 1;
const PCI_CONFIG_PAYLOAD_LEN: usize = 9;
const VEGA_ROUTING_PAYLOAD_LEN: usize = 14;
const ATA_IDLE_PAYLOAD_LEN: usize = 66;
const ATA_MID_SECTOR_PAYLOAD_LEN: usize = ATA_IDLE_PAYLOAD_LEN + crate::ata::SECTOR;
const BMIDE_IDLE_PAYLOAD_LEN: usize = 60;
const BMIDE_BASE: u16 = 0xf000;
const ATAPI_IDLE_PAYLOAD_LEN: usize = 130;
const GAMEPORT_PAYLOAD_LEN: usize = 192;
const PIT_COUNTER_PAYLOAD_LEN: usize = PIT_PAYLOAD_LEN / 3;
const PIT_CHANNEL_2_GATE_OFFSET: usize = 2 * PIT_COUNTER_PAYLOAD_LEN + 10;
const PIIX_IDE_DEVFN: u8 = 7 << 3 | 1;
const DISTIRA_DEVFN: u8 = 0x10 << 3;
const TEST_DMA_EVENT_TOTALS_ENVELOPE_ID: u32 = 0x7ffe_0001;
const TEST_MEMORY_MIB: u16 = 2;

fn test_machine() -> Machine {
    Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0; BIOS_ROM_SIZE],
    )
    .unwrap()
}

fn memory_test_machine() -> Machine {
    Machine::new(
        MachineProfile::gsw_386(TEST_MEMORY_MIB, VideoCard::Vega),
        vec![0; BIOS_ROM_SIZE],
    )
    .unwrap()
}

fn rom_with_code(code: &[u8]) -> Vec<u8> {
    let mut rom = vec![0; BIOS_ROM_SIZE];
    rom[..code.len()].copy_from_slice(code);
    rom[0xf000] = 0xcf;
    rom[0xfff0..0xfff5].copy_from_slice(&[0xea, 0x00, 0x00, 0x00, 0xf0]);
    rom
}

fn push_out_dx_eax(code: &mut Vec<u8>, port: u16, value: u32) {
    code.extend_from_slice(&[0xba, port as u8, (port >> 8) as u8]);
    code.extend_from_slice(&[
        0x66,
        0xb8,
        value as u8,
        (value >> 8) as u8,
        (value >> 16) as u8,
        (value >> 24) as u8,
    ]);
    code.extend_from_slice(&[0x66, 0xef]);
}

fn capture_error(machine: &Machine) -> MachineCanonicalCaptureError {
    machine.canonical_state_capture().err().unwrap()
}

fn machine_control_timing_payload(machine: &Machine) -> Vec<u8> {
    let capture = machine.canonical_state_capture().unwrap();
    machine_control_timing_payload_from_capture(&capture)
}

fn machine_control_timing_payload_from_capture(
    capture: &CanonicalMachineStateCapture<'_>,
) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_0001).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_machine_control_timing_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn modeled_cache_payload(machine: &Machine) -> Vec<u8> {
    let capture = machine.canonical_state_capture().unwrap();
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_0002).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_modeled_cache_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn ram_rom_payload(machine: &Machine) -> Vec<u8> {
    let capture = machine.canonical_state_capture().unwrap();
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_0003).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_ram_rom_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn pic_payload(machine: &Machine) -> Vec<u8> {
    let capture = machine.canonical_state_capture().unwrap();
    pic_payload_from_capture(&capture)
}

fn pic_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_0004).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_pic_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn pit_payload(machine: &Machine) -> Vec<u8> {
    let capture = machine.canonical_state_capture().unwrap();
    pit_payload_from_capture(&capture)
}

fn pit_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_0005).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_pit_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn dma_payload(machine: &Machine) -> Vec<u8> {
    let capture = machine.canonical_state_capture().unwrap();
    dma_payload_from_capture(&capture)
}

fn dma_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_0006).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_dma_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn dma_event_totals_v1_payload(machine: &Machine) -> Vec<u8> {
    let capture = machine.canonical_state_capture().unwrap();
    dma_event_totals_v1_payload_from_capture(&capture)
}

fn dma_event_totals_v1_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(TEST_DMA_EVENT_TOTALS_ENVELOPE_ID).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_dma_event_totals_v1_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn rtc_payload(machine: &Machine) -> Vec<u8> {
    let capture = machine.canonical_state_capture().unwrap();
    rtc_payload_from_capture(&capture)
}

fn rtc_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_0007).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_rtc_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn unit_tester_payload(machine: &Machine) -> Vec<u8> {
    let capture = machine.canonical_state_capture().unwrap();
    unit_tester_payload_from_capture(&capture)
}

fn unit_tester_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_0008).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_unit_tester_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn speaker_payload(machine: &Machine) -> Vec<u8> {
    let capture = machine.canonical_state_capture().unwrap();
    speaker_payload_from_capture(&capture)
}

fn speaker_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_0009).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_speaker_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn pci_config_payload(machine: &Machine) -> Vec<u8> {
    let capture = machine.canonical_state_capture().unwrap();
    pci_config_payload_from_capture(&capture)
}

fn pci_config_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_000a).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_pci_config_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn vega_routing_payload(machine: &Machine) -> Vec<u8> {
    let capture = machine.canonical_state_capture().unwrap();
    vega_routing_payload_from_capture(&capture)
}

fn vega_routing_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_000b).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_vega_routing_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

/// Model the run loop publishing deferred coherence work (direct-map and
/// device-memory flags) so a capture can proceed after a direct HLE or
/// port-path write inside a unit test.
fn publish_pending_coherence(machine: &mut Machine) {
    machine.direct_map_changed = false;
    machine.direct_data_map_changed = false;
    machine.device_wrote_memory = false;
    machine.pending_device_memory_write_range = None;
}

fn int10(machine: &mut Machine, eax: u32, ebx: u32, edx: u32) {
    machine.cpu.registers.set_eax(eax);
    machine.cpu.registers.set_ebx(ebx);
    machine.cpu.registers.set_edx(edx);
    machine.handle_int10();
    publish_pending_coherence(machine);
}

fn ata_payload(machine: &Machine) -> Vec<u8> {
    let capture = machine.canonical_state_capture().unwrap();
    ata_payload_from_capture(&capture)
}

fn ata_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_000c).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_ata_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn ata_machine(sectors: usize) -> Machine {
    let mut machine = test_machine();
    let mut bytes = vec![0u8; sectors * crate::ata::SECTOR];
    for (index, chunk) in bytes.chunks_mut(crate::ata::SECTOR).enumerate() {
        chunk[0] = (index as u8).wrapping_add(0x10);
    }
    machine.mount_hdd(bytes);
    publish_pending_coherence(&mut machine);
    machine
}

fn ata_idle_golden() -> Vec<u8> {
    // present, task file (status DRDY|DSC, sector_count/lba_low power on at
    // 1), CHS latch 63/16, nIEN off, Ultra DMA mode 2, no IRQ, phase Idle,
    // then the empty buffer and zeroed cursor/pending/DMA records.
    let mut expected = vec![1, 0, 1, 1, 0, 0, 0, 0x50, 0, 63, 16, 0, 2, 2, 0, 0];
    expected.extend_from_slice(&0u64.to_le_bytes());
    expected.extend_from_slice(&[0; 12]);
    expected.extend_from_slice(&[0; 12]);
    expected.extend_from_slice(&[0; 18]);
    expected
}

fn bmide_payload(machine: &Machine) -> Vec<u8> {
    let capture = machine.canonical_state_capture().unwrap();
    bmide_payload_from_capture(&capture)
}

fn bmide_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_000d).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_bmide_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

/// Arm a primary bus-master transfer: PRD table at 0x1000 from the given
/// descriptor dwords, then the BM command, then the ATA DMA command.
fn arm_bmide_transfer(
    machine: &mut Machine,
    descriptors: &[(u32, u32)],
    bm_command: u32,
    ata_command: u32,
) {
    for (index, (address, control)) in descriptors.iter().enumerate() {
        machine.write_physical_u32(0x1000 + index as u32 * 8, *address);
        machine.write_physical_u32(0x1004 + index as u32 * 8, *control);
    }
    write_pci_port(machine, BMIDE_BASE + 4, BusWidth::Dword, 0x1000);
    write_pci_port(machine, BMIDE_BASE, BusWidth::Byte, bm_command);
    let base = crate::ata::PRIMARY_CMD_BASE;
    write_pci_port(machine, base + 2, BusWidth::Byte, 1);
    write_pci_port(machine, base + 3, BusWidth::Byte, 2);
    write_pci_port(machine, base + 4, BusWidth::Byte, 0);
    write_pci_port(machine, base + 5, BusWidth::Byte, 0);
    write_pci_port(machine, base + 6, BusWidth::Byte, 0x40);
    write_pci_port(machine, base + 7, BusWidth::Byte, ata_command);
    publish_pending_coherence(machine);
}

fn atapi_payload(machine: &Machine) -> Vec<u8> {
    let capture = machine.canonical_state_capture().unwrap();
    atapi_payload_from_capture(&capture)
}

fn atapi_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_000e).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_atapi_channel_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn gameport_payload(machine: &Machine) -> Vec<u8> {
    let capture = machine.canonical_state_capture().unwrap();
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_000f).unwrap(),
            CanonicalSectionVersion::new(2).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| capture.write_gameport_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn atapi_idle_golden() -> Vec<u8> {
    // Fresh channel after soft reset: the ATAPI signature in the task file,
    // DRDY|DSC status, diagnostic-pass error, and full MODE SELECT volumes.
    let mut expected = vec![0u8; ATAPI_IDLE_PAYLOAD_LEN];
    expected[1] = 0x01;
    expected[2] = 0x01;
    expected[3] = 0x14;
    expected[4] = 0xeb;
    expected[6] = 0x50;
    expected[7] = 0x01;
    expected[118] = 0xff;
    expected[119] = 0xff;
    expected
}

fn advance_ide_deadline(machine: &mut Machine) {
    let ticks = machine.ide.ticks_until_completion().unwrap();
    machine.advance_devices_ticks(ticks);
}

fn atapi_send_cdb(machine: &mut Machine, cdb: [u8; 12]) {
    write_pci_port(
        machine,
        crate::ide::SECONDARY_CMD_BASE + 7,
        BusWidth::Byte,
        0xa0,
    );
    advance_ide_deadline(machine);
    read_pci_port(machine, crate::ide::SECONDARY_CMD_BASE + 7, BusWidth::Byte);
    for byte in cdb {
        write_pci_port(
            machine,
            crate::ide::SECONDARY_CMD_BASE,
            BusWidth::Byte,
            u32::from(byte),
        );
    }
}

fn write_speaker_port(machine: &mut Machine, value: u8) {
    let mut bus = machine.make_bus();
    bus.write_io(0x61, BusWidth::Byte, u32::from(value), false)
        .unwrap();
}

fn read_speaker_port(machine: &mut Machine) -> u8 {
    let mut bus = machine.make_bus();
    u8::try_from(bus.read_io(0x61, BusWidth::Byte, 0, false).unwrap()).unwrap()
}

/// Run `body` with the `IZARRAVM_ISA_IO_WAIT` arm forced on this thread, then restore the
/// ambient reading. Mirrors `with_isa_io_wait` in `machine_bus_timing_test.rs` -- the shipped
/// knob is a process-wide `OnceLock`, so both arms can only be exercised in one process
/// through the per-thread override.
fn with_isa_io_wait<R>(armed: bool, body: impl FnOnce() -> R) -> R {
    crate::bus::set_isa_io_wait_for_test(Some(armed));
    let result = body();
    crate::bus::set_isa_io_wait_for_test(None);
    result
}

fn write_pci_port(machine: &mut Machine, port: u16, width: BusWidth, value: u32) {
    let mut bus = machine.make_bus();
    bus.write_io(port, width, value, false).unwrap();
}

fn read_pci_port(machine: &mut Machine, port: u16, width: BusWidth) -> u32 {
    let mut bus = machine.make_bus();
    bus.read_io(port, width, 0, false).unwrap()
}

fn write_pci_bdf(
    machine: &mut Machine,
    bus: u8,
    devfn: u8,
    offset: u8,
    width: BusWidth,
    value: u32,
) {
    machine
        .pci
        .write_bdf(bus, devfn, offset, width, value, &mut machine.vega);
}

fn write_pit_port(machine: &mut Machine, port: u16, value: u8) {
    let mut bus = machine.make_bus();
    bus.write_io(port, BusWidth::Byte, u32::from(value), false)
        .unwrap();
}

fn read_pit_port(machine: &mut Machine, port: u16) -> u8 {
    let mut bus = machine.make_bus();
    u8::try_from(bus.read_io(port, BusWidth::Byte, 0, false).unwrap()).unwrap()
}

fn write_pic_port(machine: &mut Machine, port: u16, value: u8) {
    let mut bus = machine.make_bus();
    bus.write_io(port, BusWidth::Byte, u32::from(value), false)
        .unwrap();
}

fn read_pic_port(machine: &mut Machine, port: u16) -> u8 {
    let mut bus = machine.make_bus();
    u8::try_from(bus.read_io(port, BusWidth::Byte, 0, false).unwrap()).unwrap()
}

fn write_rtc_port(machine: &mut Machine, port: u16, value: u8) {
    let mut bus = machine.make_bus();
    bus.write_io(port, BusWidth::Byte, u32::from(value), false)
        .unwrap();
}

fn read_rtc_port(machine: &mut Machine, port: u16) -> u8 {
    let mut bus = machine.make_bus();
    u8::try_from(bus.read_io(port, BusWidth::Byte, 0, false).unwrap()).unwrap()
}

fn initialize_pic_pair(machine: &mut Machine, level_triggered: bool) {
    let icw1 = if level_triggered { 0x19 } else { 0x11 };
    for (command, data, vector, cascade) in [(0x20, 0x21, 0x20, 0x04), (0xa0, 0xa1, 0x70, 0x02)] {
        write_pic_port(machine, command, icw1);
        write_pic_port(machine, data, vector);
        write_pic_port(machine, data, cascade);
        write_pic_port(machine, data, 0x01);
    }
}

fn warm_modeled_cache_line(machine: &mut Machine, mode: GswMode, line: u32) {
    let _ = machine.cache_model.data_tier(mode, line * CACHE_LINE_BYTES);
}

fn raw_word_read_clocks(machine: &mut Machine, address: u32) -> (u16, u64) {
    let before = machine.trace.elapsed_clocks();
    let value = machine.read_physical_u16(address);
    (value, machine.trace.elapsed_clocks() - before)
}

fn approximate_cpu_bus_cost_contract(machine: &mut Machine) -> Vec<Option<u64>> {
    let bus = machine.make_bus();
    let mut values = Vec::new();
    for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
        for kind in [BusAccessKind::DataRead, BusAccessKind::DataWrite] {
            values.push(bus.jit_direct_memory_max_clocks(width, kind));
        }
    }
    values.push(bus.jit_cached_fetch_run_clocks(0x0002_0000, 16));
    values.push(bus.jit_projected_batch_scaled_bus_clocks(37));
    values.push(Some(bus.jit_fetch_cost_clocks()));
    values.push(Some(u64::from(u8::from(bus.native_fetches_are_uniform()))));
    values.push(Some(u64::from(u8::from(
        bus.native_aggregate_accounting_allowed(),
    ))));
    values.push(Some(bus.jit_data_byte_cost_clocks()));
    for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
        values.push(Some(bus.jit_data_cost_clocks(width)));
        values.push(Some(bus.jit_mode13_data_cost_clocks(width)));
    }
    values.push(Some(bus.jit_scale_bus_cost_upper(41)));
    values.push(Some(bus.rep_data_byte_cost_upper()));
    values.push(bus.rep_page_walk_cost_upper());
    values
}

fn direct_charge_delta(machine: &mut Machine) -> u64 {
    let before = machine.trace.elapsed_clocks();
    {
        let mut bus = machine.make_bus();
        bus.charge_direct_memory(0x0002_0000, BusWidth::Dword, BusAccessKind::DataRead)
            .unwrap();
    }
    machine.trace.elapsed_clocks() - before
}

fn direct_bulk_read(machine: &mut Machine) -> (usize, [u8; 16], u64) {
    let before = machine.trace.elapsed_clocks();
    let mut bytes = [0; 16];
    let read = {
        let mut bus = machine.make_bus();
        bus.read_memory_bytes_direct(
            0x0002_0000,
            &mut bytes,
            BusWidth::Dword,
            BusAccessKind::DataRead,
        )
        .unwrap()
    };
    (read, bytes, machine.trace.elapsed_clocks() - before)
}

fn native_fetch_charge_delta(machine: &mut Machine) -> (bool, u64) {
    let before = machine.trace.elapsed_clocks();
    let charged = {
        let mut bus = machine.make_bus();
        bus.charge_native_cached_fetches(0x0002_0000, 0x0002_0000, &[1, 2, 3], 4)
    };
    (charged, machine.trace.elapsed_clocks() - before)
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn take_len_prefixed_bytes<'a>(payload: &'a [u8], cursor: &mut usize) -> &'a [u8] {
    let length_end = *cursor + 8;
    let length = usize::try_from(u64::from_le_bytes(
        payload[*cursor..length_end].try_into().unwrap(),
    ))
    .unwrap();
    *cursor = length_end;
    let data_end = *cursor + length;
    let data = &payload[*cursor..data_end];
    *cursor = data_end;
    data
}

#[test]
fn foundation_sections_pin_ids_versions_order_and_namespaces() {
    let sections = STATE_SNAPSHOT_V1_FOUNDATION_SECTIONS;
    assert_eq!(
        sections.map(|section| (section.id, section.version)),
        [
            (0x0000_0001, 1),
            (0x0001_0001, 1),
            (0x0001_0002, 1),
            (0x0001_0003, 1),
            (0x0002_0001, 1),
            (0x0002_0002, 1),
            (0x0002_0003, 1),
            (0x0002_0004, 1),
            (0x0002_0005, 1),
            (0x0002_0006, 1),
            (0x0002_0007, 1),
            (0x0002_0008, 1),
            (0x0002_0009, 1),
            (0x0002_000a, 1),
            (0x0002_000b, 1),
            (0x0002_000c, 1),
            (0x0002_000d, 1),
            (0x0002_000e, 1),
            (0x0002_000f, 2),
        ]
    );
    assert!(
        sections
            .iter()
            .all(|section| section.requirement == CanonicalSectionRequirement::Required)
    );
    assert!(sections.windows(2).all(|pair| pair[0].id < pair[1].id));
    assert_eq!(
        sections[0].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
        STATE_SNAPSHOT_V1_SCHEMA_NAMESPACE
    );
    assert!(sections[1..4].iter().all(|section| {
        section.id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK == STATE_SNAPSHOT_V1_CPU_NAMESPACE
    }));
    assert_eq!(
        sections[4].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
        STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert_eq!(
        sections[5].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
        STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert_eq!(
        sections[6].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
        STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert_eq!(
        sections[7].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
        STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert_eq!(
        sections[8].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
        STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert_eq!(
        sections[9].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
        STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert_eq!(
        sections[10].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
        STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert_eq!(
        sections[11].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
        STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert_eq!(
        sections[12].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
        STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert_eq!(
        sections[13].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
        STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert_eq!(
        sections[14].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
        STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert_eq!(
        sections[15].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
        STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert_eq!(
        sections[16].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
        STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert_eq!(
        sections[17].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
        STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
}

#[test]
fn gameport_payload_captures_attachment_controls_and_absolute_deadlines() {
    let mut machine = test_machine();
    assert_eq!(gameport_payload(&machine), vec![0; GAMEPORT_PAYLOAD_LEN]);

    machine.set_joystick_state(Some(JoystickState::joystick_a(17, 231, 0x03)));
    let uncharged = gameport_payload(&machine);
    assert_eq!(&uncharged[..8], &[1, 3, 17, 231, 0, 0, 3, 0]);
    assert_eq!(&uncharged[8..40], &[0; 32]);
    assert_eq!(uncharged[58], 1, "button 1 current normal drive");
    assert_eq!(uncharged[60], 1, "button 1 target normal drive");
    assert_eq!(uncharged[96], 1, "button 2 current normal drive");
    assert_eq!(uncharged[98], 1, "button 2 target normal drive");

    {
        let mut bus = machine.make_bus();
        bus.write_io(0x0207, BusWidth::Byte, 0, false).unwrap();
    }
    let charged = gameport_payload(&machine);
    assert_eq!(&charged[..8], &[1, 3, 17, 231, 0, 0, 3, 0]);
    assert_ne!(&charged[8..16], &[0; 8]);
    assert_ne!(&charged[16..24], &[0; 8]);
    assert_eq!(&charged[24..40], &[0; 16]);

    machine.set_joystick_state(None);
    assert_eq!(gameport_payload(&machine), vec![0; GAMEPORT_PAYLOAD_LEN]);
}

#[test]
fn ram_rom_projection_payload_layout_is_exact() {
    let projection = CanonicalRamRomProjection {
        ram: &[0x10, 0x20, 0x30],
        rom: &[0xa0, 0xb0],
    };
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_0003).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| projection.write_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();

    assert_eq!(
        view.sections()[0].payload(),
        &[
            3, 0, 0, 0, 0, 0, 0, 0, 0x10, 0x20, 0x30, 2, 0, 0, 0, 0, 0, 0, 0, 0xa0, 0xb0,
        ]
    );
}

#[test]
fn ram_rom_payload_covers_both_authoritative_stores_in_raw_order() {
    let mut machine = memory_test_machine();
    let ram_len = machine.memory.len();
    for (address, value) in [
        (0x0000_0000, 0x11),
        (0x0009_ffff, 0x22),
        (0x000a_0000, 0x33),
        (0x000c_0000, 0x44),
        (0x000c_8000, 0x55),
        (0x000f_0000, 0x66),
        (0x0010_0000, 0x77),
        (ram_len - 1, 0x88),
    ] {
        machine.memory.as_mut_slice()[address] = value;
    }
    machine.rom[0] = 0x91;
    machine.rom[BIOS_ROM_SIZE / 2] = 0x92;
    machine.rom[BIOS_ROM_SIZE - 1] = 0x93;

    let payload = ram_rom_payload(&machine);
    let mut cursor = 0;
    let ram = take_len_prefixed_bytes(&payload, &mut cursor);
    let rom = take_len_prefixed_bytes(&payload, &mut cursor);

    assert_eq!(ram, machine.memory.as_slice());
    assert_eq!(rom, machine.rom.as_slice());
    assert_eq!(cursor, payload.len());
    assert_eq!(payload.len(), 16 + ram_len + BIOS_ROM_SIZE);
}

#[test]
fn system_rom_aliases_share_one_store_and_ignore_guest_writes() {
    const ROM_OFFSET: usize = 0x8123;
    const ROM_VALUE: u8 = 0x5a;
    const HIDDEN_RAM_VALUE: u8 = 0xa5;

    let mut machine = memory_test_machine();
    machine.rom[ROM_OFFSET] = ROM_VALUE;
    machine.memory.as_mut_slice()[crate::LOW_BIOS_BASE as usize + ROM_OFFSET] = HIDDEN_RAM_VALUE;
    let expected = ram_rom_payload(&machine);

    {
        let mut bus = machine.make_bus();
        assert_eq!(
            CpuBus::read_memory(
                &mut bus,
                crate::LOW_BIOS_BASE + ROM_OFFSET as u32,
                BusWidth::Byte,
                BusAccessKind::DataRead,
            )
            .unwrap(),
            u32::from(ROM_VALUE)
        );
        assert_eq!(
            CpuBus::read_memory(
                &mut bus,
                crate::HIGH_ROM_BASE + ROM_OFFSET as u32,
                BusWidth::Byte,
                BusAccessKind::DataRead,
            )
            .unwrap(),
            u32::from(ROM_VALUE)
        );
        CpuBus::write_memory(
            &mut bus,
            crate::LOW_BIOS_BASE + ROM_OFFSET as u32,
            BusWidth::Byte,
            0x10,
            BusAccessKind::DataWrite,
        )
        .unwrap();
        CpuBus::write_memory(
            &mut bus,
            crate::HIGH_ROM_BASE + ROM_OFFSET as u32,
            BusWidth::Byte,
            0x20,
            BusAccessKind::DataWrite,
        )
        .unwrap();
    }

    assert_eq!(machine.rom[ROM_OFFSET], ROM_VALUE);
    assert_eq!(
        machine.memory.as_slice()[crate::LOW_BIOS_BASE as usize + ROM_OFFSET],
        HIDDEN_RAM_VALUE
    );
    assert_eq!(ram_rom_payload(&machine), expected);
}

#[test]
fn discarded_flash_prefix_is_not_canonical_state() {
    let mut first = vec![0; BIOS_ROM_SIZE * 2];
    let mut second = first.clone();
    first[0] = 0x11;
    second[0] = 0x22;
    let profile = MachineProfile::gsw_386(TEST_MEMORY_MIB, VideoCard::Vega);
    let first = Machine::new(profile.clone(), first).unwrap();
    let second = Machine::new(profile, second).unwrap();

    assert_eq!(first.rom, second.rom);
    assert_eq!(ram_rom_payload(&first), ram_rom_payload(&second));
}

#[test]
fn a20_routing_changes_raw_cells_without_projecting_an_alias() {
    const LOW: usize = 0x0000_1234;
    const HIGH: u32 = 0x0010_1234;

    let mut machine = memory_test_machine();
    let unchanged = ram_rom_payload(&machine);
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x0092, BusWidth::Byte, 0, false).unwrap();
    }
    assert_eq!(ram_rom_payload(&machine), unchanged);

    {
        let mut bus = machine.make_bus();
        CpuBus::write_memory(
            &mut bus,
            HIGH,
            BusWidth::Byte,
            0x41,
            BusAccessKind::DataWrite,
        )
        .unwrap();
    }
    assert_eq!(machine.memory.as_slice()[LOW], 0x41);
    assert_eq!(machine.memory.as_slice()[HIGH as usize], 0);

    {
        let mut bus = machine.make_bus();
        bus.write_io(0x0092, BusWidth::Byte, 0x02, false).unwrap();
        CpuBus::write_memory(
            &mut bus,
            HIGH,
            BusWidth::Byte,
            0x52,
            BusAccessKind::DataWrite,
        )
        .unwrap();
    }
    assert_eq!(machine.memory.as_slice()[LOW], 0x41);
    assert_eq!(machine.memory.as_slice()[HIGH as usize], 0x52);

    let payload = ram_rom_payload(&machine);
    let mut cursor = 0;
    let ram = take_len_prefixed_bytes(&payload, &mut cursor);
    assert_eq!(ram[LOW], 0x41);
    assert_eq!(ram[HIGH as usize], 0x52);
}

#[test]
fn live_pci_decode_rebuilds_and_publishes_the_excluded_ram_lookup() {
    const RAM_BAR: u32 = 0x0100_0000;
    const BAR_REGISTER: u32 = 0x10;
    const COMMAND_REGISTER: u32 = 0x04;

    let mut machine = Machine::new(
        MachineProfile::gsw_386(32, VideoCard::Vega),
        rom_with_code(&[0xf4]),
    )
    .unwrap();
    let expected = ram_rom_payload(&machine);
    assert!(
        machine
            .ram_lookup
            .is_consistent(machine.memory.len(), &machine.vega)
    );

    {
        let mut bus = machine.make_bus();
        assert!(
            CpuBus::direct_page(&mut bus, RAM_BAR, BusAccessKind::DataRead)
                .unwrap()
                .is_some()
        );
        let config_address =
            0x8000_0000 | (u32::from(crate::DISTIRA_PCI_SLOT) << 11) | COMMAND_REGISTER;
        bus.write_io(
            crate::PCI_CONFIG_ADDRESS_PORT,
            BusWidth::Dword,
            config_address,
            false,
        )
        .unwrap();
        bus.write_io(crate::PCI_CONFIG_DATA_PORT, BusWidth::Dword, 0, false)
            .unwrap();
        let config_address =
            0x8000_0000 | (u32::from(crate::DISTIRA_PCI_SLOT) << 11) | BAR_REGISTER;
        bus.write_io(
            crate::PCI_CONFIG_ADDRESS_PORT,
            BusWidth::Dword,
            config_address,
            false,
        )
        .unwrap();
        bus.write_io(crate::PCI_CONFIG_DATA_PORT, BusWidth::Dword, RAM_BAR, false)
            .unwrap();
        assert!(
            CpuBus::direct_page(&mut bus, RAM_BAR, BusAccessKind::DataRead)
                .unwrap()
                .is_some()
        );
        let config_address =
            0x8000_0000 | (u32::from(crate::DISTIRA_PCI_SLOT) << 11) | COMMAND_REGISTER;
        bus.write_io(
            crate::PCI_CONFIG_ADDRESS_PORT,
            BusWidth::Dword,
            config_address,
            false,
        )
        .unwrap();
        bus.write_io(
            crate::PCI_CONFIG_DATA_PORT,
            BusWidth::Dword,
            0x0000_0002,
            false,
        )
        .unwrap();
        assert!(
            CpuBus::direct_page(&mut bus, RAM_BAR, BusAccessKind::DataRead)
                .unwrap()
                .is_none()
        );
    }

    assert_eq!(
        capture_error(&machine),
        MachineCanonicalCaptureError::PendingDirectMapChange
    );
    assert!(
        machine
            .ram_lookup
            .is_consistent(machine.memory.len(), &machine.vega)
    );

    assert_eq!(
        machine.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::Halted
    );
    assert!(!machine.direct_map_changed);
    assert!(
        machine
            .ram_lookup
            .is_consistent(machine.memory.len(), &machine.vega)
    );
    assert_eq!(ram_rom_payload(&machine), expected);
}

#[test]
fn fresh_modeled_cache_payload_is_exact_in_every_mode() {
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        let mut machine = test_machine();
        machine.set_mode(mode);

        assert_eq!(
            modeled_cache_payload(&machine),
            vec![0; EMPTY_MODELED_CACHE_PAYLOAD_LEN],
            "{mode:?}"
        );
    }
}

#[test]
fn populated_modeled_cache_payload_sorts_full_tags_numerically() {
    let mut expected = Vec::new();
    push_u64(&mut expected, 0);
    push_u64(&mut expected, 3);
    push_u32(&mut expected, 0x0000_0002);
    push_u32(&mut expected, 0x0000_0401);
    push_u32(&mut expected, 0x0000_0800);
    assert_eq!(expected.len(), 28);

    for mode in [GswMode::Gsw386Slow, GswMode::Gsw386] {
        let mut forward = test_machine();
        forward.set_mode(mode);
        for line in [0x0800, 0x0002, 0x0401] {
            warm_modeled_cache_line(&mut forward, mode, line);
        }

        let mut reverse = test_machine();
        reverse.set_mode(mode);
        for line in [0x0401, 0x0002, 0x0800] {
            warm_modeled_cache_line(&mut reverse, mode, line);
        }

        assert_eq!(modeled_cache_payload(&forward), expected, "{mode:?}");
        assert_eq!(modeled_cache_payload(&reverse), expected, "{mode:?}");
    }
}

#[test]
fn accurate_cache_hit_and_collision_preserve_next_access_timing() {
    const TARGET_LINE: u32 = 0x0500;
    const COLLIDING_LINE: u32 = TARGET_LINE + 0x0400;
    const TARGET_ADDRESS: u32 = TARGET_LINE * CACHE_LINE_BYTES;

    for mode in [GswMode::Gsw386Slow, GswMode::Gsw386] {
        let mut hot = test_machine();
        hot.set_mode(mode);
        warm_modeled_cache_line(&mut hot, mode, TARGET_LINE);

        let mut displaced = test_machine();
        displaced.set_mode(mode);
        warm_modeled_cache_line(&mut displaced, mode, COLLIDING_LINE);

        assert_eq!(hot.cache_tier_lookups(), displaced.cache_tier_lookups());
        assert_ne!(
            modeled_cache_payload(&hot),
            modeled_cache_payload(&displaced),
            "{mode:?}"
        );

        let hot_read = raw_word_read_clocks(&mut hot, TARGET_ADDRESS);
        let displaced_read = raw_word_read_clocks(&mut displaced, TARGET_ADDRESS);
        assert_eq!(hot_read.0, displaced_read.0, "{mode:?}");
        assert_eq!(hot_read.1, 2, "{mode:?} L2 hit");
        assert_eq!(displaced_read.1, 5, "{mode:?} RAM miss");
        assert_eq!(
            modeled_cache_payload(&hot),
            modeled_cache_payload(&displaced),
            "{mode:?} caches must converge after the same access"
        );
    }
}

#[test]
fn inert_modeled_cache_residue_is_payload_and_continuation_neutral() {
    let mut clean = test_machine();
    let mut residue = test_machine();
    clean.set_mode(GswMode::Gsw386);
    residue.set_mode(GswMode::Gsw386);

    residue.cache_model.l1_tags[0] = 0;
    residue.cache_model.l2_tags[3] = 2;
    residue.cache_model.l2_tags[0] = MAX_MODELED_CACHE_LINE + 1;
    residue.cache_model.l2_tags[1024] = 0x0400;
    residue.cache_model.l2_tags[4] = crate::CACHE_EMPTY_TAG;

    assert_eq!(
        modeled_cache_payload(&clean),
        modeled_cache_payload(&residue)
    );

    for address in [2 * CACHE_LINE_BYTES, 0] {
        let clean_read = raw_word_read_clocks(&mut clean, address);
        let residue_read = raw_word_read_clocks(&mut residue, address);
        assert_eq!(clean_read, residue_read, "address {address:#010x}");
    }
    assert_eq!(
        modeled_cache_payload(&clean),
        modeled_cache_payload(&residue)
    );
}

#[test]
fn modeled_cache_capture_is_read_only_and_excludes_lookup_count() {
    const LINE: u32 = 0x0123;
    let mut once = test_machine();
    let mut repeated = test_machine();
    warm_modeled_cache_line(&mut once, GswMode::Gsw386, LINE);
    for _ in 0..4 {
        warm_modeled_cache_line(&mut repeated, GswMode::Gsw386, LINE);
    }
    assert_ne!(once.cache_tier_lookups(), repeated.cache_tier_lookups());
    assert_eq!(
        modeled_cache_payload(&once),
        modeled_cache_payload(&repeated)
    );

    let before_l1 = repeated.cache_model.l1_tags.to_vec();
    let before_l2 = repeated.cache_model.l2_tags.to_vec();
    let before_config = (
        repeated.cache_model.config.l1_mask,
        repeated.cache_model.config.l2_mask,
    );
    let before_cost = (
        repeated.cache_model.cost.l1,
        repeated.cache_model.cost.l2,
        repeated.cache_model.cost.ram,
    );
    let before_code_fetch_ws = repeated.cache_model.code_fetch_ws;
    let before_lookups = repeated.cache_tier_lookups();
    let first = modeled_cache_payload(&repeated);
    let second = modeled_cache_payload(&repeated);

    assert_eq!(first, second);
    assert_eq!(repeated.cache_model.l1_tags.as_ref(), before_l1);
    assert_eq!(repeated.cache_model.l2_tags.as_ref(), before_l2);
    assert_eq!(
        (
            repeated.cache_model.config.l1_mask,
            repeated.cache_model.config.l2_mask,
        ),
        before_config
    );
    assert_eq!(
        (
            repeated.cache_model.cost.l1,
            repeated.cache_model.cost.l2,
            repeated.cache_model.cost.ram,
        ),
        before_cost
    );
    assert_eq!(repeated.cache_model.code_fetch_ws, before_code_fetch_ws);
    assert_eq!(repeated.cache_tier_lookups(), before_lookups);

    let once_read = raw_word_read_clocks(&mut once, LINE * CACHE_LINE_BYTES);
    let repeated_read = raw_word_read_clocks(&mut repeated, LINE * CACHE_LINE_BYTES);
    assert_eq!(once_read, repeated_read);
}

#[test]
fn approximate_modes_ignore_all_tag_residue_on_normal_bus_accesses() {
    const ADDRESS: u32 = 0x0002_0000;
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut clean = test_machine();
        let mut residue = test_machine();
        clean.set_mode(mode);
        residue.set_mode(mode);

        residue.cache_model.l1_tags[0] = 0;
        residue.cache_model.l1_tags[1] = MAX_MODELED_CACHE_LINE + 1;
        residue.cache_model.l2_tags[3] = 2;
        residue.cache_model.l2_tags[CACHE_L2_MAX_LINES - 1] = 0x1fff;
        warm_modeled_cache_line(&mut residue, mode, ADDRESS / CACHE_LINE_BYTES);
        let lookups = residue.cache_tier_lookups();
        let l1_tags = residue.cache_model.l1_tags.to_vec();
        let l2_tags = residue.cache_model.l2_tags.to_vec();

        assert_eq!(
            modeled_cache_payload(&clean),
            vec![0; EMPTY_MODELED_CACHE_PAYLOAD_LEN],
            "{mode:?}"
        );
        assert_eq!(
            modeled_cache_payload(&residue),
            vec![0; EMPTY_MODELED_CACHE_PAYLOAD_LEN],
            "{mode:?}"
        );

        let clean_read = raw_word_read_clocks(&mut clean, ADDRESS);
        let residue_read = raw_word_read_clocks(&mut residue, ADDRESS);
        assert_eq!(clean_read, residue_read, "{mode:?} read");
        assert_eq!(residue.cache_tier_lookups(), lookups, "{mode:?} read");

        let clean_before = clean.trace.elapsed_clocks();
        let residue_before = residue.trace.elapsed_clocks();
        clean.write_physical_u16(ADDRESS, 0x5aa5);
        residue.write_physical_u16(ADDRESS, 0x5aa5);
        assert_eq!(
            clean.trace.elapsed_clocks() - clean_before,
            residue.trace.elapsed_clocks() - residue_before,
            "{mode:?} write"
        );
        assert_eq!(residue.cache_tier_lookups(), lookups, "{mode:?} write");
        assert_eq!(residue.cache_model.l1_tags.as_ref(), l1_tags, "{mode:?}");
        assert_eq!(residue.cache_model.l2_tags.as_ref(), l2_tags, "{mode:?}");
    }
}

#[test]
fn approximate_direct_and_native_bus_contract_ignores_tag_residue() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut clean = test_machine();
        let mut residue = test_machine();
        clean.set_mode(mode);
        residue.set_mode(mode);

        residue.cache_model.l1_tags.fill(0);
        residue.cache_model.l2_tags.fill(0);
        residue.cache_model.l1_tags[1] = MAX_MODELED_CACHE_LINE + 1;
        residue.cache_model.l2_tags[3] = 2;
        warm_modeled_cache_line(&mut residue, mode, 0x0800);
        let lookups = residue.cache_tier_lookups();
        let l1_tags = residue.cache_model.l1_tags.to_vec();
        let l2_tags = residue.cache_model.l2_tags.to_vec();

        assert_eq!(
            approximate_cpu_bus_cost_contract(&mut clean),
            approximate_cpu_bus_cost_contract(&mut residue),
            "{mode:?} cost contract"
        );
        assert_eq!(
            direct_charge_delta(&mut clean),
            direct_charge_delta(&mut residue),
            "{mode:?} direct charge"
        );
        assert_eq!(
            direct_bulk_read(&mut clean),
            direct_bulk_read(&mut residue),
            "{mode:?} direct bulk read"
        );
        assert_eq!(
            native_fetch_charge_delta(&mut clean),
            native_fetch_charge_delta(&mut residue),
            "{mode:?} native fetch charge"
        );

        assert_eq!(residue.cache_tier_lookups(), lookups, "{mode:?}");
        assert_eq!(residue.cache_model.l1_tags.as_ref(), l1_tags, "{mode:?}");
        assert_eq!(residue.cache_model.l2_tags.as_ref(), l2_tags, "{mode:?}");
        assert_eq!(
            modeled_cache_payload(&residue),
            vec![0; EMPTY_MODELED_CACHE_PAYLOAD_LEN],
            "{mode:?}"
        );
    }
}

#[test]
fn effective_tags_do_not_depend_on_current_a20_or_device_decode() {
    const DEVICE_LINE: u32 = 0x000a_0000 / CACHE_LINE_BYTES;
    const HIGH_LINE: u32 = 0x0010_0040 / CACHE_LINE_BYTES;
    let mut machine = test_machine();
    warm_modeled_cache_line(&mut machine, GswMode::Gsw386, DEVICE_LINE);
    warm_modeled_cache_line(&mut machine, GswMode::Gsw386, HIGH_LINE);
    let expected = modeled_cache_payload(&machine);

    machine.set_a20_gate(false);

    assert_eq!(modeled_cache_payload(&machine), expected);
    assert!(
        expected
            .windows(4)
            .any(|bytes| bytes == DEVICE_LINE.to_le_bytes())
    );
    assert!(
        expected
            .windows(4)
            .any(|bytes| bytes == HIGH_LINE.to_le_bytes())
    );
}

#[test]
fn every_mode_change_resets_raw_and_effective_cache_state() {
    let modes = [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ];
    for source in modes {
        for target in modes {
            let mut machine = test_machine();
            machine.set_mode(source);
            machine.cache_model.l1_tags.fill(0);
            machine.cache_model.l2_tags.fill(0);
            warm_modeled_cache_line(&mut machine, source, 0x0123);
            let lookups = machine.cache_tier_lookups();

            machine.set_mode(target);

            assert!(
                machine
                    .cache_model
                    .l1_tags
                    .iter()
                    .all(|tag| *tag == crate::CACHE_EMPTY_TAG),
                "{source:?} -> {target:?} L1"
            );
            assert!(
                machine
                    .cache_model
                    .l2_tags
                    .iter()
                    .all(|tag| *tag == crate::CACHE_EMPTY_TAG),
                "{source:?} -> {target:?} L2"
            );
            assert_eq!(machine.cache_tier_lookups(), lookups);
            assert_eq!(
                modeled_cache_payload(&machine),
                vec![0; EMPTY_MODELED_CACHE_PAYLOAD_LEN],
                "{source:?} -> {target:?}"
            );
            if !target.uses_approximate_timing() {
                assert_eq!(
                    raw_word_read_clocks(&mut machine, 0x0123 * CACHE_LINE_BYTES).1,
                    5,
                    "{source:?} -> {target:?} must resume cold"
                );
            }
        }
    }
}

#[test]
fn default_machine_control_timing_payload_is_exactly_pinned() {
    let payload = machine_control_timing_payload(&test_machine());
    let mut expected = vec![
        0x10, 0x00, // memory MiB
        0x00, 0x01, 0x01, 0x02, // RAM, ROM, video, and I/O wait states
        0x01, // fast POST
        0x00, 0x00, // no effective pending software INT
        0x00, 0x00, // no intercepted INT stash
    ];
    expected.resize(MACHINE_CONTROL_TIMING_PAYLOAD_LEN, 0);

    assert_eq!(payload, expected);
}

#[test]
fn populated_machine_control_timing_payload_pins_every_field_offset() {
    let mut profile = MachineProfile::gsw_386(24, VideoCard::Vega);
    profile.wait_states = WaitStateProfile {
        ram: 3,
        rom: 5,
        video: 7,
        io: 11,
    };
    let mut machine = Machine::new(profile, vec![0; BIOS_ROM_SIZE]).unwrap();
    machine.set_mode(GswMode::Gsw586);
    machine.cpu.control.cr0 |= 1;
    machine.set_fast_post(false);
    machine.pending_soft_int = Some(0x21);
    machine.last_int_vector = Some(0x13);
    machine.timeline.advance_io_stall_ticks(
        34,
        crate::timeline::DeviceRates {
            dsp_hz: 2,
            wss_hz: 3,
            cd_playing: true,
            vga_dot_hz: 4,
        },
    );
    machine.elapsed_clocks = 200;
    machine.io_stall_clocks = 40;
    machine.halted_ticks = 20;
    machine.trace.add_elapsed_clocks(300);
    machine.scaled_bus_clocks = 150;
    machine.bus_rem = 29;

    let payload = machine_control_timing_payload(&machine);
    let mut expected = vec![
        0x18, 0x00, // memory MiB
        3, 5, 7, 11, // wait states
        0,  // full POST pacing
        1, 0x21, // pending software INT
        1, 0x13, // intercepted INT stash
    ];
    for value in [
        34,
        34,
        1,
        34_000_000,
        40_568_188,
        68,
        102,
        2_550,
        34,
        1_132_000_000,
        2_040,
        1_071_000,
        136,
        200,
        40,
        20,
        300,
        150,
        29,
    ] {
        push_u64(&mut expected, value);
    }

    assert_eq!(payload.len(), MACHINE_CONTROL_TIMING_PAYLOAD_LEN);
    assert_eq!(payload, expected);
}

#[test]
fn clean_capture_is_read_only_and_accepts_ring_zero_state() {
    let mut machine = test_machine();
    machine.cpu.control.cr0 |= 1;
    assert!(machine.cpu.is_ring0_protected());
    let before_cpu = machine.cpu.clone();
    let before_timeline = machine.timeline;
    let before_clocks = (
        machine.elapsed_clocks,
        machine.scaled_bus_clocks,
        machine.trace.elapsed_clocks(),
    );

    let first_payload = machine_control_timing_payload(&machine);
    let second_payload = machine_control_timing_payload(&machine);

    assert_eq!(machine.cpu, before_cpu);
    assert_eq!(machine.timeline, before_timeline);
    assert_eq!(first_payload, second_payload);
    assert_eq!(
        (
            machine.elapsed_clocks,
            machine.scaled_bus_clocks,
            machine.trace.elapsed_clocks(),
        ),
        before_clocks
    );
}

#[test]
fn dma_semantic_state_and_event_totals_share_one_read_only_capture() {
    let mut machine = test_machine();
    machine.dma.write_port(0x0c, 0);
    machine.dma.write_port(0x00, 0x34);
    machine.dma.master.channels[0].transfer_cycles = 17;
    machine.dma.master.channels[2].transfer_cycles = 29;

    let capture = machine.canonical_state_capture().unwrap();
    let first_state = dma_payload_from_capture(&capture);
    let first_totals = dma_event_totals_v1_payload_from_capture(&capture);
    let second_totals = dma_event_totals_v1_payload_from_capture(&capture);
    let second_state = dma_payload_from_capture(&capture);

    assert_eq!(first_state.len(), DMA_PAYLOAD_LEN);
    assert_eq!(first_totals.len(), DMA_EVENT_TOTALS_V1_PAYLOAD_LEN);
    assert_eq!(first_state, second_state);
    assert_eq!(first_totals, second_totals);
    assert_eq!(first_state[0], 1, "half-write continuation is retained");
    assert_eq!(
        u64::from_le_bytes(first_totals[0..8].try_into().unwrap()),
        17
    );
    assert_eq!(
        u64::from_le_bytes(first_totals[16..24].try_into().unwrap()),
        29
    );
    drop(capture);

    machine.dma.write_port(0x00, 0x12);
    assert_eq!(machine.dma.master.channels[0].base_addr, 0x1234);
    assert_eq!(dma_event_totals_v1_payload(&machine), first_totals);
    assert_ne!(dma_payload(&machine), first_state);
}

#[test]
fn rtc_pic_and_timeline_payloads_share_one_read_only_capture() {
    // Subject is capture/restore semantics of the RTC/PIC/timeline payloads at one
    // capture boundary, not ISA batch-timing accounting -- the `write_rtc_port` calls
    // below are setup, going through `machine.make_bus()` directly rather than a CPU
    // batch. Pin the ISA-wait arm OFF for this test so those raw setup pokes don't
    // leave an uncommitted charge sitting on `port_bus_batch_clocks`, which would refuse
    // the capture below with `UncommittedBatchTiming` for a reason unrelated to what
    // this test pins. The armed arm's capture-after-a-charged-batch behavior is
    // covered by `armed_isa_io_wait_captures_cleanly_after_a_pit_batch_boundary`.
    with_isa_io_wait(false, || {
        let mut machine = test_machine();
        machine.set_mode(GswMode::Gsw586);
        machine.seed_rtc(2026, 7, 19, 1, 23, 58, 41);
        initialize_pic_pair(&mut machine, false);
        write_rtc_port(&mut machine, 0x70, 0x0b);
        write_rtc_port(&mut machine, 0x71, 0x40);
        let deadline = machine.rtc.ticks_until_periodic_irq().unwrap();
        machine.advance_devices_ticks(deadline / 3);

        let capture = machine.canonical_state_capture().unwrap();
        let first_rtc = rtc_payload_from_capture(&capture);
        let first_pic = pic_payload_from_capture(&capture);
        let first_timeline = machine_control_timing_payload_from_capture(&capture);
        let second_timeline = machine_control_timing_payload_from_capture(&capture);
        let second_pic = pic_payload_from_capture(&capture);
        let second_rtc = rtc_payload_from_capture(&capture);

        assert_eq!(first_rtc.len(), RTC_PAYLOAD_LEN);
        assert_eq!(first_rtc, second_rtc);
        assert_eq!(first_pic, second_pic);
        assert_eq!(first_timeline, second_timeline);
        assert_eq!(
            machine.rtc.ticks_until_periodic_irq(),
            Some(deadline - deadline / 3)
        );
    });
}

#[test]
fn guest_cmos_dirty_notification_is_captureable_and_normalized() {
    let mut machine = test_machine();
    machine.rtc.write_port(0x70, 0x10);
    let value = machine.cmos_byte(0x10);
    let clean = rtc_payload(&machine);

    machine.rtc.write_port(0x71, value);
    let capture = machine.canonical_state_capture().unwrap();
    assert_eq!(rtc_payload_from_capture(&capture), clean);
    drop(capture);

    assert!(machine.take_cmos_dirty());
    assert_eq!(rtc_payload(&machine), clean);
    assert!(!machine.take_cmos_dirty());
}

#[test]
fn serviced_unit_tester_crc_changes_only_the_result_registers() {
    let program_rectangle = [
        0xb0, 0x00, 0xe6, 0xe4, // select rectangle register zero
        0xb0, 0x00, 0xe6, 0xe5, // X low
        0xb0, 0x00, 0xe6, 0xe5, // X high
        0xb0, 0x00, 0xe6, 0xe5, // Y low
        0xb0, 0x00, 0xe6, 0xe5, // Y high
        0xb0, 0x02, 0xe6, 0xe5, // W low
        0xb0, 0x00, 0xe6, 0xe5, // W high
        0xb0, 0x02, 0xe6, 0xe5, // H low
        0xb0, 0x00, 0xe6, 0xe5, // H high
    ];
    let mut baseline_code = program_rectangle.to_vec();
    baseline_code.push(0xf4);
    let mut serviced_code = program_rectangle.to_vec();
    serviced_code.extend_from_slice(&[
        0xb0,
        unittester::CMD_CRC,
        0xe6,
        0xe6, // issue deferred CRC command
        0xf4,
    ]);
    let mut baseline = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        rom_with_code(&baseline_code),
    )
    .unwrap();
    let mut serviced = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        rom_with_code(&serviced_code),
    )
    .unwrap();

    assert_eq!(
        baseline.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(
        serviced.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(serviced.unittester.pending_command(), None);

    let before = unit_tester_payload(&baseline);
    let after = unit_tester_payload(&serviced);
    let crc = serviced.screen_crc32(0, 0, 2, 2).to_le_bytes();
    let mut expected = before.clone();
    expected[1 + unittester::REG_CRC..1 + unittester::REG_CRC + 4].copy_from_slice(&crc);

    assert_eq!(before[0], 8);
    assert_eq!(after[0], 8);
    assert_eq!(after, expected);
    assert_ne!(crc, [0; 4]);
}

#[test]
fn serviced_unit_tester_noop_commands_leave_the_payload_stable() {
    let expected = unit_tester_payload(&test_machine());
    for command in [unittester::CMD_SNAPSHOT, 0x7f] {
        let rom = rom_with_code(&[
            0xb0, command, 0xe6, 0xe6, // issue deferred command
            0xf4,
        ]);
        let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

        assert_eq!(
            machine.run_until_halt_or_cycles(1_000_000).unwrap(),
            StopReason::Halted
        );
        assert_eq!(machine.unittester.pending_command(), None);
        assert_eq!(unit_tester_payload(&machine), expected);
    }
}

#[test]
fn speaker_payload_pins_every_port_61_latch_value() {
    // Subject is the speaker payload's pinned encoding of every port 0x61 latch
    // value, not ISA batch-timing accounting -- `write_speaker_port`/`read_speaker_port`
    // are raw `machine.make_bus()` probes, not CPU batches, so each one leaves an
    // uncommitted charge that would refuse the capture inside `speaker_payload` for a
    // reason unrelated to what this test pins. Pin the arm OFF; the armed arm's
    // capture-after-a-charged-batch behavior is covered by
    // `armed_isa_io_wait_captures_cleanly_after_a_pit_batch_boundary`.
    with_isa_io_wait(false, || {
        let mut machine = test_machine();
        machine.set_mode(GswMode::Gsw586);

        for value in u8::MIN..=u8::MAX {
            write_speaker_port(&mut machine, value);
            let expected = value & 0x03;

            assert_eq!(speaker_payload(&machine), [expected]);
            assert_eq!(read_speaker_port(&mut machine) & 0x03, expected);
            assert_eq!(
                pit_payload(&machine)[PIT_CHANNEL_2_GATE_OFFSET],
                u8::from(expected & 1 != 0)
            );
        }
    });
}

#[test]
fn speaker_latch_and_pit_gate_remain_independent_owners() {
    // Subject is that the speaker latch and the PIT channel-2 gate are independently
    // owned, not ISA batch-timing accounting -- `write_speaker_port`/`read_speaker_port`
    // are raw bus probes outside any CPU batch, so pin the arm OFF here (see
    // `speaker_payload_pins_every_port_61_latch_value` for the full rationale; the
    // armed arm's capture-after-a-charged-batch behavior is covered by
    // `armed_isa_io_wait_captures_cleanly_after_a_pit_batch_boundary`).
    with_isa_io_wait(false, || {
        let mut machine = test_machine();
        machine.set_mode(GswMode::Gsw586);

        write_speaker_port(&mut machine, 0x00);
        machine.set_timer_gate(2, true);
        assert_eq!(speaker_payload(&machine), [0]);
        assert_eq!(pit_payload(&machine)[PIT_CHANNEL_2_GATE_OFFSET], 1);
        assert_eq!(read_speaker_port(&mut machine) & 0x03, 0);

        write_speaker_port(&mut machine, 0x01);
        machine.set_timer_gate(2, false);
        assert_eq!(speaker_payload(&machine), [1]);
        assert_eq!(pit_payload(&machine)[PIT_CHANNEL_2_GATE_OFFSET], 0);
        assert_eq!(read_speaker_port(&mut machine) & 0x03, 1);
    });
}

#[test]
fn pit_advancement_changes_pit_without_changing_the_speaker_latch() {
    let mut machine = test_machine();
    write_pit_port(&mut machine, 0x43, 0xb6);
    write_pit_port(&mut machine, 0x42, 4);
    write_pit_port(&mut machine, 0x42, 0);
    write_speaker_port(&mut machine, 0x03);
    let speaker_before = speaker_payload(&machine);
    let pit_before = pit_payload(&machine);

    let pit_ticks = MASTER_CLOCK_HZ / u64::from(crate::PIT_INPUT_HZ) * 16;
    machine.advance_devices_ticks(pit_ticks);

    assert_ne!(pit_payload(&machine), pit_before);
    assert_eq!(speaker_payload(&machine), speaker_before);
}

#[test]
fn speaker_capture_is_read_only_and_excludes_host_audio_history() {
    let mut machine = test_machine();
    write_speaker_port(&mut machine, 0x03);
    machine
        .speaker
        .accumulate(MASTER_CLOCK_HZ / 100, true, [(17, false), (31, true)]);
    machine.speaker_transitions.push(crate::pit::OutTransition {
        tick: 7,
        level: false,
    });
    let mut expected_speaker = machine.speaker.clone();
    let transitions_before = machine.speaker_transitions.clone();
    let pit_before = machine.pit.clone();
    let timeline_before = machine.timeline;

    let capture = machine.canonical_state_capture().unwrap();
    let first_speaker = speaker_payload_from_capture(&capture);
    let captured_pit = pit_payload_from_capture(&capture);
    let second_speaker = speaker_payload_from_capture(&capture);
    drop(capture);

    assert_eq!(first_speaker, [3]);
    assert_eq!(first_speaker, second_speaker);
    assert_eq!(captured_pit, pit_payload(&machine));
    assert_eq!(machine.speaker_transitions, transitions_before);
    assert_eq!(machine.pit, pit_before);
    assert_eq!(machine.timeline, timeline_before);
    assert_eq!(machine.speaker.drain(64), expected_speaker.drain(64));

    let mut baseline = test_machine();
    let mut history = test_machine();
    history.speaker.write_control(0x03);
    history
        .speaker
        .accumulate(MASTER_CLOCK_HZ / 50, true, [(23, false), (71, true)]);
    let _ = history.speaker.drain(11);
    history.speaker_transitions.push(crate::pit::OutTransition {
        tick: 5,
        level: true,
    });
    history.speaker.write_control(0x00);
    baseline.speaker.write_control(0x00);

    assert!(history.speaker.ever_enabled());
    assert!(!history.speaker_transitions.is_empty());
    assert_eq!(speaker_payload(&history), speaker_payload(&baseline));
}

#[test]
fn pci_config_payload_pins_default_and_asymmetric_bytes() {
    assert_eq!(
        pci_config_payload(&test_machine()),
        [0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0xf0, 0x00, 0x00]
    );

    let mut machine = test_machine();
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_ADDRESS_PORT,
        BusWidth::Dword,
        0xd5b3_a69d,
    );
    write_pci_bdf(&mut machine, 0, PIIX_IDE_DEVFN, 0x04, BusWidth::Byte, 0x04);
    write_pci_bdf(
        &mut machine,
        0,
        PIIX_IDE_DEVFN,
        0x20,
        BusWidth::Dword,
        0x1234_e10f,
    );

    assert_eq!(
        pci_config_payload(&machine),
        [0x9d, 0xa6, 0xb3, 0xd5, 0x04, 0x00, 0xe1, 0x34, 0x12]
    );
}

#[test]
fn pci_config_payload_normalizes_every_piix_command_write() {
    let mut machine = test_machine();
    for value in u8::MIN..=u8::MAX {
        write_pci_bdf(
            &mut machine,
            0,
            PIIX_IDE_DEVFN,
            0x04,
            BusWidth::Byte,
            u32::from(value),
        );
        let payload = pci_config_payload(&machine);
        assert_eq!(payload[4], value & 0x05);
        assert_eq!(machine.pci.ide_io_enabled(), value & 0x01 != 0);
        assert_eq!(machine.pci.ide_bus_master_enabled(), value & 0x04 != 0);
    }

    let expected = pci_config_payload(&machine);
    for offset in [0x05, 0x06, 0x07] {
        write_pci_bdf(
            &mut machine,
            0,
            PIIX_IDE_DEVFN,
            offset,
            BusWidth::Byte,
            0xff,
        );
        assert_eq!(pci_config_payload(&machine), expected);
    }
}

#[test]
fn vega_routing_payload_pins_default_and_asymmetric_bytes() {
    assert_eq!(
        vega_routing_payload(&test_machine()),
        [0, 0, 0, 0, 0x02, 0x00, 0x00, 0x00, 0x00, 0xe1, 0, 0, 0, 0]
    );

    // A banked VBE mode with a bank selected, a moved BAR high byte, and an
    // init-enable latch: every payload field lands asymmetric.
    let mut machine = test_machine();
    int10(&mut machine, 0x4f02, 0x0101, 0);
    int10(&mut machine, 0x4f05, 0x0000, 0x00ab);
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_ADDRESS_PORT,
        BusWidth::Dword,
        0x8000_8010,
    );
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_DATA_PORT,
        BusWidth::Dword,
        0x5634_1200,
    );
    write_pci_bdf(
        &mut machine,
        0,
        DISTIRA_DEVFN,
        0x40,
        BusWidth::Dword,
        0xa55a_1234,
    );
    publish_pending_coherence(&mut machine);
    assert_eq!(
        vega_routing_payload(&machine),
        [
            1, 0, 0xab, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x56, 0x34, 0x12, 0x5a, 0xa5
        ]
    );
}

#[test]
fn vega_routing_legacy_mode_set_retains_stale_margo_latches() {
    let mut machine = test_machine();
    int10(&mut machine, 0x4f02, 0x0101, 0);
    int10(&mut machine, 0x4f05, 0x0000, 0x00ab);
    assert_eq!(vega_routing_payload(&machine)[..4], [1, 0, 0xab, 0x00]);

    // select_legacy clears only margo_active; the bank latch deliberately
    // stays stale across the legacy mode set.
    int10(&mut machine, 0x0003, 0, 0);
    assert_eq!(vega_routing_payload(&machine)[..4], [0, 0, 0xab, 0x00]);

    // A linear mode set flips the aperture latch and resets the bank.
    int10(&mut machine, 0x4f02, 0x4101, 0);
    assert_eq!(vega_routing_payload(&machine)[..4], [1, 1, 0x00, 0x00]);

    // Leaving for legacy again keeps the stale linear latch.
    int10(&mut machine, 0x0003, 0, 0);
    assert_eq!(vega_routing_payload(&machine)[..4], [0, 1, 0x00, 0x00]);
}

#[test]
fn vega_routing_payload_normalizes_every_distira_command_write() {
    let mut machine = test_machine();
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_ADDRESS_PORT,
        BusWidth::Dword,
        0x8000_8004,
    );
    for value in u8::MIN..=u8::MAX {
        write_pci_port(
            &mut machine,
            crate::PCI_CONFIG_DATA_PORT,
            BusWidth::Byte,
            u32::from(value),
        );
        publish_pending_coherence(&mut machine);
        let payload = vega_routing_payload(&machine);
        assert_eq!(payload[4], value & 0x02);
        assert_eq!(payload[5], 0);
    }

    // The high command byte discards every write.
    let expected = vega_routing_payload(&machine);
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_DATA_PORT + 1,
        BusWidth::Byte,
        0xff,
    );
    publish_pending_coherence(&mut machine);
    assert_eq!(vega_routing_payload(&machine), expected);
}

#[test]
fn vega_routing_bar_partial_writes_land_only_the_high_byte() {
    let mut machine = test_machine();
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_ADDRESS_PORT,
        BusWidth::Dword,
        0x8000_8010,
    );
    for lane in 0..3u16 {
        write_pci_port(
            &mut machine,
            crate::PCI_CONFIG_DATA_PORT + lane,
            BusWidth::Byte,
            0xff,
        );
        publish_pending_coherence(&mut machine);
        assert_eq!(
            vega_routing_payload(&machine)[6..10],
            [0x00, 0x00, 0x00, 0xe1]
        );
    }
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_DATA_PORT + 3,
        BusWidth::Byte,
        0x25,
    );
    publish_pending_coherence(&mut machine);
    assert_eq!(
        vega_routing_payload(&machine)[6..10],
        [0x00, 0x00, 0x00, 0x25]
    );
}

#[test]
fn vega_routing_capture_is_read_only_and_excludes_device_internals() {
    let mut machine = test_machine();
    let baseline = vega_routing_payload(&machine);

    // Legacy VGA register and Margo VRAM churn is other-owner state and must
    // not perturb the routing payload.
    write_pci_port(&mut machine, 0x3c2, BusWidth::Byte, 0x67);
    machine.vega.margo_mut().vram_mut()[0] = 0x5a;
    publish_pending_coherence(&mut machine);
    assert_eq!(vega_routing_payload(&machine), baseline);

    let capture = machine.canonical_state_capture().unwrap();
    let first = vega_routing_payload_from_capture(&capture);
    let second = vega_routing_payload_from_capture(&capture);
    assert_eq!(first, second);
    assert_eq!(first.len(), VEGA_ROUTING_PAYLOAD_LEN);
}

#[test]
fn ata_payload_pins_no_disk_and_fresh_mount_bytes() {
    assert_eq!(ata_payload(&test_machine()), [0]);

    let machine = ata_machine(8);
    let payload = ata_payload(&machine);
    assert_eq!(payload.len(), ATA_IDLE_PAYLOAD_LEN);
    assert_eq!(payload, ata_idle_golden());
}

#[test]
fn ata_payload_tracks_task_file_and_set_features_writes() {
    let mut machine = ata_machine(8);
    let base = crate::ata::PRIMARY_CMD_BASE;
    for (port, value) in [
        (base + 1, 0x55u32),
        (base + 2, 3),
        (base + 3, 0x11),
        (base + 4, 0x22),
        (base + 5, 0x33),
        (base + 6, 0x4f),
        (crate::ata::PRIMARY_CTRL, 0x02),
    ] {
        write_pci_port(&mut machine, port, BusWidth::Byte, value);
    }
    let payload = ata_payload(&machine);
    assert_eq!(&payload[1..7], &[0x55, 3, 0x11, 0x22, 0x33, 0x4f]);
    assert_eq!(payload[11], 1); // nIEN latched

    // SET FEATURES: transfer mode Multiword DMA 1, completed on the timeline.
    write_pci_port(&mut machine, base + 1, BusWidth::Byte, 0x03);
    write_pci_port(&mut machine, base + 2, BusWidth::Byte, 0x21);
    write_pci_port(&mut machine, base + 7, BusWidth::Byte, 0xef);
    let pending = ata_payload(&machine);
    assert_eq!(pending[36], 1); // pending command present
    assert_eq!(&pending[45..48], &[7, 0x03, 0x21]); // SetFeatures{feature, mode}
    let deadline = machine
        .ata
        .as_ref()
        .and_then(crate::ata::AtaDisk::ticks_until_completion)
        .unwrap();
    machine.advance_devices_ticks(deadline);
    let applied = ata_payload(&machine);
    assert_eq!(&applied[12..14], &[1, 1]); // Multiword(1)
    assert_eq!(applied[36], 0); // pending consumed
}

#[test]
fn ata_payload_captures_the_commit_write_window_buffer() {
    let mut machine = ata_machine(8);
    let base = crate::ata::PRIMARY_CMD_BASE;
    write_pci_port(&mut machine, base + 2, BusWidth::Byte, 1);
    write_pci_port(&mut machine, base + 3, BusWidth::Byte, 2);
    write_pci_port(&mut machine, base + 4, BusWidth::Byte, 0);
    write_pci_port(&mut machine, base + 5, BusWidth::Byte, 0);
    write_pci_port(&mut machine, base + 6, BusWidth::Byte, 0x40);
    write_pci_port(&mut machine, base + 7, BusWidth::Byte, 0x30);
    let deadline = machine
        .ata
        .as_ref()
        .and_then(crate::ata::AtaDisk::ticks_until_completion)
        .unwrap();
    machine.advance_devices_ticks(deadline);

    // Fill the sector. After the 512th byte the phase is back at Idle and the
    // guest's not-yet-committed data lives only in the buffer, so the payload
    // must carry it unconditionally.
    for word in 0..(crate::ata::SECTOR as u32 / 2) {
        write_pci_port(&mut machine, base, BusWidth::Word, 0xa000 | word);
    }
    let payload = ata_payload(&machine);
    assert_eq!(payload.len(), ATA_MID_SECTOR_PAYLOAD_LEN);
    assert_eq!(payload[15], 0); // phase Idle
    assert_eq!(payload[16..24], (crate::ata::SECTOR as u64).to_le_bytes());
    assert_eq!(payload[24], 0x00); // first data byte (low byte of word 0xa000)
    assert_eq!(payload[25], 0xa0);
    assert_eq!(payload[536..540], (crate::ata::SECTOR as u32).to_le_bytes());
    assert_eq!(payload[548], 1); // pending present
    assert_eq!(payload[557], 3); // CommitWrite

    let deadline = machine
        .ata
        .as_ref()
        .and_then(crate::ata::AtaDisk::ticks_until_completion)
        .unwrap();
    machine.advance_devices_ticks(deadline);
    let committed = ata_payload(&machine);
    assert_eq!(committed.len(), ATA_IDLE_PAYLOAD_LEN); // buffer drained
    assert_eq!(committed[36], 0); // pending consumed
}

#[test]
fn ata_payload_captures_an_armed_dma_request() {
    let mut machine = ata_machine(8);
    let base = crate::ata::PRIMARY_CMD_BASE;
    write_pci_port(&mut machine, base + 2, BusWidth::Byte, 2);
    write_pci_port(&mut machine, base + 3, BusWidth::Byte, 3);
    write_pci_port(&mut machine, base + 4, BusWidth::Byte, 0);
    write_pci_port(&mut machine, base + 5, BusWidth::Byte, 0);
    write_pci_port(&mut machine, base + 6, BusWidth::Byte, 0x40);
    write_pci_port(&mut machine, base + 7, BusWidth::Byte, 0xc8);
    let payload = ata_payload(&machine);
    assert_eq!(payload[48], 1); // request present
    assert_eq!(payload[49], 0); // DeviceToMemory
    assert_eq!(payload[50..54], 3u32.to_le_bytes());
    assert_eq!(payload[54..58], 2u32.to_le_bytes());
    assert_eq!(payload[58..66], 33_300_000u64.to_le_bytes());
}

#[test]
fn ata_capture_is_read_only_and_excludes_content_and_telemetry() {
    let mut machine = ata_machine(8);
    let baseline = ata_payload(&machine);

    // Content writes, the host flush flag, and BMIDE state belong to other
    // owners and must not perturb this payload.
    machine
        .ata
        .as_mut()
        .unwrap()
        .write_lba(1, &[0x5a; crate::ata::SECTOR]);
    machine.ata.as_mut().unwrap().dirty = false;
    machine.bmide.note_ide_irq(false);
    assert_eq!(ata_payload(&machine), baseline);

    let capture = machine.canonical_state_capture().unwrap();
    let first = ata_payload_from_capture(&capture);
    let second = ata_payload_from_capture(&capture);
    assert_eq!(first, second);
    assert_eq!(first, baseline);
}

#[test]
fn bmide_payload_pins_default_golden_and_register_writes() {
    assert_eq!(
        bmide_payload(&test_machine()),
        [0u8; BMIDE_IDLE_PAYLOAD_LEN]
    );
    assert_eq!(
        bmide_payload(&ata_machine(8)),
        [0u8; BMIDE_IDLE_PAYLOAD_LEN]
    );

    // Register writes only, both channels; the PRD pointer stores its
    // 4-byte-aligned value and the secondary bank is a real register file.
    let mut machine = ata_machine(8);
    write_pci_port(&mut machine, BMIDE_BASE, BusWidth::Byte, 0x08);
    write_pci_port(&mut machine, BMIDE_BASE + 2, BusWidth::Byte, 0x60);
    write_pci_port(&mut machine, BMIDE_BASE + 4, BusWidth::Dword, 0x2003);
    write_pci_port(&mut machine, BMIDE_BASE + 12, BusWidth::Dword, 0x3000);
    let payload = bmide_payload(&machine);
    let mut expected = [0u8; BMIDE_IDLE_PAYLOAD_LEN];
    expected[0] = 0x08;
    expected[1] = 0x60;
    expected[2..6].copy_from_slice(&0x2000u32.to_le_bytes());
    expected[32..36].copy_from_slice(&0x3000u32.to_le_bytes());
    assert_eq!(payload, expected);

    let capture = machine.canonical_state_capture().unwrap();
    let first = bmide_payload_from_capture(&capture);
    let second = bmide_payload_from_capture(&capture);
    assert_eq!(first, second);
}

#[test]
fn bmide_payload_captures_a_multi_span_write_transfer() {
    let mut machine = ata_machine(8);
    // Two 256-byte PRD entries; addresses carry the A1 bit set so the
    // payload must hold the parsed, masked span addresses.
    arm_bmide_transfer(
        &mut machine,
        &[(0x2002, 0x100), (0x2102, 0x8000_0100)],
        0x01,
        0xca,
    );
    let ticks = machine.bmide.ticks_until_completion().unwrap();
    let payload = bmide_payload(&machine);
    assert_eq!(payload.len(), BMIDE_IDLE_PAYLOAD_LEN + 16);
    assert_eq!(payload[0], 0x01); // START
    assert_eq!(payload[1] & 0x01, 0x01); // ACTIVE
    assert_eq!(payload[2..6], 0x1000u32.to_le_bytes());
    assert_eq!(payload[7], 1); // transfer present
    assert_eq!(payload[8], 1); // MemoryToDevice
    assert_eq!(payload[9..17], ticks.to_le_bytes());
    assert_eq!(payload[17..21], 512u32.to_le_bytes());
    assert_eq!(payload[21], 1); // retires at EOT
    assert_eq!(payload[22..30], 2u64.to_le_bytes());
    assert_eq!(payload[30..34], 0x2000u32.to_le_bytes());
    assert_eq!(payload[34..38], 0x100u32.to_le_bytes());
    assert_eq!(payload[38..42], 0x2100u32.to_le_bytes());
    assert_eq!(payload[42..46], 0x100u32.to_le_bytes());
    assert_eq!(&payload[46..], &[0u8; 30]); // secondary untouched

    machine.advance_devices_ticks(ticks);
    publish_pending_coherence(&mut machine);
    let done = bmide_payload(&machine);
    assert_eq!(done.len(), BMIDE_IDLE_PAYLOAD_LEN);
    assert_eq!(done[1] & 0x01, 0); // ACTIVE retired at EOT
    assert_ne!(done[1] & 0x04, 0); // INTERRUPT latched
}

#[test]
fn bmide_payload_captures_the_waits_for_stop_window() {
    let mut machine = ata_machine(8);
    // One descriptor larger than the transfer: completion must hold ACTIVE
    // and wait for the guest's STOP write instead of retiring at EOT.
    arm_bmide_transfer(&mut machine, &[(0x2000, 0x8000_0400)], 0x09, 0xc8);
    let ticks = machine.bmide.ticks_until_completion().unwrap();
    machine.advance_devices_ticks(ticks);
    publish_pending_coherence(&mut machine);
    let payload = bmide_payload(&machine);
    assert_eq!(payload.len(), BMIDE_IDLE_PAYLOAD_LEN);
    assert_eq!(payload[1] & 0x01, 0x01); // ACTIVE held
    assert_eq!(payload[6], 1); // completion waits for stop
    assert_eq!(payload[7], 0); // transfer already consumed

    write_pci_port(&mut machine, BMIDE_BASE, BusWidth::Byte, 0);
    let stopped = bmide_payload(&machine);
    assert_eq!(stopped[1] & 0x01, 0);
    assert_eq!(stopped[6], 0);
}

#[test]
fn atapi_payload_pins_default_golden_and_task_file_writes() {
    let machine = test_machine();
    let payload = atapi_payload(&machine);
    assert_eq!(payload.len(), ATAPI_IDLE_PAYLOAD_LEN);
    assert_eq!(payload, atapi_idle_golden());

    let mut machine = test_machine();
    let base = crate::ide::SECONDARY_CMD_BASE;
    write_pci_port(&mut machine, base + 1, BusWidth::Byte, 0x55);
    write_pci_port(&mut machine, base + 4, BusWidth::Byte, 0x34);
    write_pci_port(&mut machine, base + 5, BusWidth::Byte, 0x12);
    write_pci_port(
        &mut machine,
        crate::ide::SECONDARY_CTRL,
        BusWidth::Byte,
        0x02,
    );
    let payload = atapi_payload(&machine);
    assert_eq!(payload[0], 0x55); // features
    assert_eq!(payload[3], 0x34); // byte-count low
    assert_eq!(payload[4], 0x12); // byte-count high
    assert_eq!(payload[8], 1); // nIEN latched
}

#[test]
fn atapi_payload_captures_a_mid_cdb_packet() {
    let mut machine = test_machine();
    let base = crate::ide::SECONDARY_CMD_BASE;
    write_pci_port(&mut machine, base + 7, BusWidth::Byte, 0xa0);
    advance_ide_deadline(&mut machine);
    read_pci_port(&mut machine, base + 7, BusWidth::Byte);
    for byte in [0x12u8, 0x34, 0x56, 0x78, 0x9a, 0xbc] {
        write_pci_port(&mut machine, base, BusWidth::Byte, u32::from(byte));
    }
    let payload = atapi_payload(&machine);
    assert_eq!(payload[9], 1); // AwaitPacket
    assert_eq!(&payload[10..16], &[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
    assert_eq!(&payload[16..22], &[0; 6]); // unfilled CDB tail
    assert_eq!(payload[22], 6); // packet_filled
}

#[test]
fn atapi_payload_captures_the_present_read_sector_window() {
    let mut machine = test_machine();
    let mut bytes = vec![0u8; 8 * crate::cdimage::DATA_SECTOR];
    for (sector, chunk) in bytes.chunks_mut(crate::cdimage::DATA_SECTOR).enumerate() {
        chunk[0] = 0x60u8.wrapping_add(sector as u8);
    }
    machine.mount_cd(crate::cdimage::CdImage::from_iso(bytes).unwrap());
    publish_pending_coherence(&mut machine);
    // Clear the mount's UNIT ATTENTION with a TEST UNIT READY round trip.
    atapi_send_cdb(&mut machine, [0u8; 12]);
    advance_ide_deadline(&mut machine);
    read_pci_port(
        &mut machine,
        crate::ide::SECONDARY_CMD_BASE + 7,
        BusWidth::Byte,
    );

    let base = crate::ide::SECONDARY_CMD_BASE;
    write_pci_port(&mut machine, base + 4, BusWidth::Byte, 0x00);
    write_pci_port(&mut machine, base + 5, BusWidth::Byte, 0x08);
    let mut cdb = [0u8; 12];
    cdb[0] = 0x28; // READ(10)
    cdb[5] = 1; // LBA 1
    cdb[8] = 1; // one sector
    atapi_send_cdb(&mut machine, cdb);
    advance_ide_deadline(&mut machine);

    // The least intuitive legal state: the sector data is fully staged while
    // the phase is back at Idle and the DRQ presentation is still pending.
    let payload = atapi_payload(&machine);
    let sector = crate::cdimage::DATA_SECTOR;
    assert_eq!(payload.len(), ATAPI_IDLE_PAYLOAD_LEN + sector);
    assert_eq!(payload[9], 0); // phase Idle
    assert_eq!(payload[23..31], (sector as u64).to_le_bytes());
    assert_eq!(payload[31], 0x61); // first byte of LBA 1
    assert_eq!(payload[47 + sector..55 + sector], 0u64.to_le_bytes()); // ready_end
    assert_eq!(payload[76 + sector], 1); // pending present
    assert_eq!(payload[85 + sector], 3); // PresentReadSector

    advance_ide_deadline(&mut machine);
    read_pci_port(&mut machine, base, BusWidth::Word);
    let payload = atapi_payload(&machine);
    assert_eq!(payload[9], 2); // DataIn
    assert_eq!(payload[31 + sector..39 + sector], 2u64.to_le_bytes()); // data_in_pos
}

#[test]
fn capture_rejects_the_armed_test_stall_packet_seam() {
    let mut machine = test_machine();
    machine.set_test_cd_packet_stall(true);
    assert_eq!(
        capture_error(&machine),
        MachineCanonicalCaptureError::TestStallPacketEnabled
    );
}

#[test]
fn capture_rejects_a_dangling_bmide_transfer() {
    let mut machine = ata_machine(8);
    arm_bmide_transfer(&mut machine, &[(0x2000, 0x8000_0200)], 0x09, 0xc8);
    machine.ata.as_mut().unwrap().abort_dma();
    assert_eq!(
        capture_error(&machine),
        MachineCanonicalCaptureError::DanglingBmideTransfer
    );
}

#[test]
fn capture_rejects_a_drifted_distira_init_enable_mirror() {
    let mut machine = test_machine();
    machine.vega.distira_mut().set_init_enable(0xdead_beef);
    assert_eq!(
        capture_error(&machine),
        MachineCanonicalCaptureError::InconsistentDistiraInitEnableMirror {
            latch: 0,
            mirror: 0xdead_beef,
        }
    );
}

#[test]
fn pci_config_address_partial_cycles_preserve_raw_selection() {
    let mut machine = test_machine();
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_ADDRESS_PORT,
        BusWidth::Dword,
        0x1122_3344,
    );
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_ADDRESS_PORT + 1,
        BusWidth::Byte,
        0xaa,
    );
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_ADDRESS_PORT + 2,
        BusWidth::Word,
        0xbbcc,
    );
    assert_eq!(
        &pci_config_payload(&machine)[..4],
        &0xbbcc_aa44_u32.to_le_bytes()
    );
    assert_eq!(
        read_pci_port(
            &mut machine,
            crate::PCI_CONFIG_ADDRESS_PORT,
            BusWidth::Dword
        ),
        0xbbcc_aa44
    );

    let disabled_bar = 0x0000_3923;
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_ADDRESS_PORT,
        BusWidth::Dword,
        disabled_bar,
    );
    let before = pci_config_payload(&machine);
    assert_eq!(
        read_pci_port(&mut machine, crate::PCI_CONFIG_DATA_PORT, BusWidth::Dword),
        u32::MAX
    );
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_DATA_PORT,
        BusWidth::Dword,
        0x1234_e10f,
    );
    assert_eq!(pci_config_payload(&machine), before);

    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_ADDRESS_PORT + 3,
        BusWidth::Byte,
        0x80,
    );
    assert_eq!(
        read_pci_port(
            &mut machine,
            crate::PCI_CONFIG_ADDRESS_PORT,
            BusWidth::Dword
        ),
        0x8000_3923
    );
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_DATA_PORT,
        BusWidth::Dword,
        0x1234_e10f,
    );
    assert_eq!(
        &pci_config_payload(&machine)[5..],
        &0x1234_e100_u32.to_le_bytes()
    );
}

#[test]
fn pci_config_bar_partial_writes_preserve_probe_and_decode_state() {
    let mut machine = test_machine();
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_ADDRESS_PORT,
        BusWidth::Dword,
        0x8000_3920,
    );

    for (lane, value) in [(0, 0xaf), (1, 0xe1), (2, 0x34), (3, 0x12)] {
        write_pci_port(
            &mut machine,
            crate::PCI_CONFIG_DATA_PORT + lane,
            BusWidth::Byte,
            value,
        );
    }
    assert_eq!(
        &pci_config_payload(&machine)[5..],
        &0x1234_e1a0_u32.to_le_bytes()
    );
    assert_eq!(machine.pci.ide_bus_master_io_base(), None);

    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_DATA_PORT,
        BusWidth::Dword,
        u32::MAX,
    );
    assert_eq!(
        &pci_config_payload(&machine)[5..],
        &0xffff_fff0_u32.to_le_bytes()
    );
    assert_eq!(
        read_pci_port(&mut machine, crate::PCI_CONFIG_DATA_PORT, BusWidth::Dword),
        0xffff_fff1
    );
    assert_eq!(machine.pci.ide_bus_master_io_base(), None);

    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_DATA_PORT,
        BusWidth::Dword,
        0x0000_e007,
    );
    assert_eq!(
        &pci_config_payload(&machine)[5..],
        &0x0000_e000_u32.to_le_bytes()
    );
    assert_eq!(machine.pci.ide_bus_master_io_base(), Some(0xe000));

    write_pci_port(&mut machine, 0xe000, BusWidth::Byte, 0x08);
    assert_eq!(read_pci_port(&mut machine, 0xe000, BusWidth::Byte), 0x08);
    {
        // One window up from the BAR nothing decodes, so the read floats. The
        // open-bus set is what proves that: the value alone cannot, since a
        // decoded register may answer 0xFF too.
        let mut bus = machine.make_bus();
        assert_eq!(bus.read_io(0xf000, BusWidth::Byte, 0, false), Ok(0xff));
        assert!(bus.open_bus.floated(0xf000));
    }

    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_ADDRESS_PORT,
        BusWidth::Dword,
        0x8000_3904,
    );
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_DATA_PORT,
        BusWidth::Byte,
        0x04,
    );
    assert_eq!(read_pci_port(&mut machine, 0xe000, BusWidth::Byte), 0xff);
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_DATA_PORT,
        BusWidth::Byte,
        0x05,
    );
    assert_eq!(read_pci_port(&mut machine, 0xe000, BusWidth::Byte), 0x08);
}

#[test]
fn enabled_pci_config_word_lanes_merge_and_read_exact_bar_bytes() {
    const VALUE: u32 = 0xa1b2;

    for lane in 0_u16..=2 {
        let mut machine = test_machine();
        write_pci_port(
            &mut machine,
            crate::PCI_CONFIG_ADDRESS_PORT,
            BusWidth::Dword,
            0x8000_3920,
        );
        write_pci_port(
            &mut machine,
            crate::PCI_CONFIG_DATA_PORT + lane,
            BusWidth::Word,
            VALUE,
        );

        let shift = u32::from(lane) * 8;
        let mask = 0xffff_u32 << shift;
        let expected_base = ((0x0000_f000 & !mask) | (VALUE << shift)) & !0x0f;
        assert_eq!(
            &pci_config_payload(&machine)[5..],
            &expected_base.to_le_bytes()
        );
        assert_eq!(
            read_pci_port(
                &mut machine,
                crate::PCI_CONFIG_DATA_PORT + lane,
                BusWidth::Word
            ),
            ((expected_base | 1) >> shift) & 0xffff
        );
    }
}

#[test]
fn pci_config_capture_is_read_only_and_excludes_connected_device_state() {
    let mut machine = test_machine();
    write_pci_port(
        &mut machine,
        crate::PCI_CONFIG_ADDRESS_PORT,
        BusWidth::Dword,
        0x8123_4567,
    );
    let expected = pci_config_payload(&machine);

    write_pci_bdf(
        &mut machine,
        0,
        crate::DISTIRA_PCI_SLOT << 3,
        0x40,
        BusWidth::Dword,
        0xa55a_3cc3,
    );
    write_pci_port(&mut machine, 0xf000, BusWidth::Byte, 0x08);
    assert_eq!(read_pci_port(&mut machine, 0xf000, BusWidth::Byte), 0x08);
    assert_eq!(pci_config_payload(&machine), expected);
    assert_eq!(
        machine.pci.read_bdf(
            0,
            crate::DISTIRA_PCI_SLOT << 3,
            0x40,
            BusWidth::Dword,
            &machine.vega
        ),
        0xa55a_3cc3
    );

    let capture = machine.canonical_state_capture().unwrap();
    let first_pci = pci_config_payload_from_capture(&capture);
    let speaker = speaker_payload_from_capture(&capture);
    let second_pci = pci_config_payload_from_capture(&capture);
    drop(capture);

    assert_eq!(first_pci, expected);
    assert_eq!(first_pci, second_pci);
    assert_eq!(speaker.len(), SPEAKER_PAYLOAD_LEN);
    assert_eq!(read_pci_port(&mut machine, 0xf000, BusWidth::Byte), 0x08);
    assert_eq!(
        read_pci_port(
            &mut machine,
            crate::PCI_CONFIG_ADDRESS_PORT,
            BusWidth::Dword
        ),
        0x8123_4567
    );
}

#[test]
fn rtc_periodic_update_and_alarm_state_is_batch_invariant() {
    // "Batch" here means splitting device-tick advancement into one call versus two
    // (whole vs. split), not the ISA I/O batch this knob accrues against -- the subject
    // is RTC/PIC state equality across that split, and the `write_rtc_port` calls are
    // setup via a raw bus probe. Pin the arm OFF so setup doesn't leave an uncommitted
    // charge; the armed arm's capture-after-a-charged-batch behavior is covered by
    // `armed_isa_io_wait_captures_cleanly_after_a_pit_batch_boundary`.
    with_isa_io_wait(false, || {
        let mut whole = test_machine();
        let mut split = test_machine();
        for machine in [&mut whole, &mut split] {
            machine.set_mode(GswMode::Gsw586);
            machine.seed_rtc(2026, 7, 19, 1, 10, 30, 44);
            initialize_pic_pair(machine, false);
            for (index, value) in [
                (0x0a, 0x2f),
                (0x01, 45),
                (0x03, 30),
                (0x05, 10),
                (0x0b, 0x70),
            ] {
                write_rtc_port(machine, 0x70, index);
                write_rtc_port(machine, 0x71, value);
            }
        }

        whole.advance_devices_ticks(MASTER_CLOCK_HZ);
        let first_split = MASTER_CLOCK_HZ * 2 / 3;
        split.advance_devices_ticks(first_split);
        assert_eq!(rtc_payload(&split)[0x0c] & 0xf0, 0xc0);
        assert!(split.pic.irr_bit(8));
        let pic_after_periodic = pic_payload(&split);
        split.advance_devices_ticks(MASTER_CLOCK_HZ - first_split);
        assert_eq!(pic_payload(&split), pic_after_periodic);

        assert_eq!(rtc_payload(&whole), rtc_payload(&split));
        assert_eq!(pic_payload(&whole), pic_payload(&split));
        assert_eq!(
            machine_control_timing_payload(&whole),
            machine_control_timing_payload(&split)
        );
        assert_eq!(whole.rtc.clock(), split.rtc.clock());
        assert_eq!(rtc_payload(&whole)[0x0c] & 0xf0, 0xf0);
        assert!(whole.pic.irr_bit(8));
        assert!(split.pic.irr_bit(8));
    });
}

#[test]
fn rtc_and_pic_commit_the_irq8_deadline_at_one_capture_boundary() {
    // Subject is the RTC/PIC IRQ8 deadline commit at a capture boundary, not ISA
    // batch-timing accounting -- the `write_rtc_port`/`read_rtc_port` calls are raw
    // bus probes, not CPU batches, so pin the arm OFF (see
    // `speaker_payload_pins_every_port_61_latch_value` for the full rationale; the
    // armed arm's capture-after-a-charged-batch behavior is covered by
    // `armed_isa_io_wait_captures_cleanly_after_a_pit_batch_boundary`).
    with_isa_io_wait(false, || {
        let mut machine = test_machine();
        machine.set_mode(GswMode::Gsw586);
        initialize_pic_pair(&mut machine, false);
        write_rtc_port(&mut machine, 0x70, 0x0b);
        write_rtc_port(&mut machine, 0x71, 0x40);
        let deadline = machine.rtc.ticks_until_periodic_irq().unwrap();

        machine.advance_devices_ticks(deadline - 1);
        let before = machine.canonical_state_capture().unwrap();
        assert_eq!(rtc_payload_from_capture(&before)[0x0c] & 0xc0, 0);
        assert_eq!(pic_payload_from_capture(&before)[17] & 0x01, 0);
        drop(before);

        machine.advance_devices_ticks(1);
        let at_edge = machine.canonical_state_capture().unwrap();
        assert_eq!(rtc_payload_from_capture(&at_edge)[0x0c] & 0xc0, 0xc0);
        assert_eq!(pic_payload_from_capture(&at_edge)[17] & 0x01, 0x01);
        drop(at_edge);

        write_rtc_port(&mut machine, 0x70, 0x0c);
        assert_eq!(read_rtc_port(&mut machine, 0x71) & 0xc0, 0xc0);
        assert_eq!(rtc_payload(&machine)[0x0c], 0);
        assert!(machine.pic.irr_bit(8));
        assert_eq!(pic_payload(&machine)[17] & 0x01, 0x01);
    });
}

#[test]
fn batch_scratch_and_pending_semantic_state_remain_captureable() {
    let mut machine = test_machine();
    machine.io_touched = true;
    machine.pending_soft_int = Some(0x21);
    machine.pic.request(5);

    let capture = machine.canonical_state_capture().unwrap();
    drop(capture);
    let payload = pic_payload(&machine);

    assert!(machine.io_touched);
    assert_eq!(machine.pending_soft_int, Some(0x21));
    assert!(machine.pic.irr_bit(5));
    assert_eq!(payload[0] & (1 << 5), 1 << 5);
    assert_eq!(pit_payload(&machine).len(), PIT_PAYLOAD_LEN);
    assert_eq!(dma_payload(&machine).len(), DMA_PAYLOAD_LEN);
    assert_eq!(rtc_payload(&machine).len(), RTC_PAYLOAD_LEN);
    assert_eq!(unit_tester_payload(&machine).len(), UNIT_TESTER_PAYLOAD_LEN);
    assert_eq!(speaker_payload(&machine).len(), SPEAKER_PAYLOAD_LEN);
    assert_eq!(pci_config_payload(&machine).len(), PCI_CONFIG_PAYLOAD_LEN);
    assert_eq!(
        dma_event_totals_v1_payload(&machine).len(),
        DMA_EVENT_TOTALS_V1_PAYLOAD_LEN
    );
}

#[test]
fn pic_capture_preserves_an_armed_destructive_poll_read() {
    let mut machine = test_machine();
    initialize_pic_pair(&mut machine, false);
    machine.pic.request(3);
    write_pic_port(&mut machine, 0x20, 0x0c);

    let first = pic_payload(&machine);
    let second = pic_payload(&machine);

    assert_eq!(first.len(), PIC_PAYLOAD_LEN);
    assert_eq!(first, second);
    assert_eq!(first[12], 1);
    assert_eq!(read_pic_port(&mut machine, 0x20), 0x83);
    let consumed = pic_payload(&machine);
    assert_eq!(consumed[12], 0);
    assert_eq!(consumed[2] & (1 << 3), 1 << 3);
}

#[test]
fn pic_payload_preserves_held_master_and_slave_level_continuation() {
    let mut machine = test_machine();
    initialize_pic_pair(&mut machine, true);

    machine.pic.set_irq_level(3, true);
    let master_pending = pic_payload(&machine);
    assert_eq!(master_pending[0] & (1 << 3), 1 << 3);
    assert_eq!(master_pending[1] & (1 << 3), 1 << 3);
    assert_eq!(machine.pic.acknowledge(), Some(0x23));
    let master_in_service = pic_payload(&machine);
    assert_eq!(master_in_service[0] & (1 << 3), 0);
    assert_eq!(master_in_service[2] & (1 << 3), 1 << 3);
    write_pic_port(&mut machine, 0x20, 0x20);
    let master_reasserted = pic_payload(&machine);
    assert_eq!(master_reasserted[0] & (1 << 3), 1 << 3);
    assert_eq!(master_reasserted[2] & (1 << 3), 0);
    machine.pic.set_irq_level(3, false);

    machine.pic.set_irq_level(10, true);
    let slave_pending = pic_payload(&machine);
    assert_eq!(slave_pending[17] & (1 << 2), 1 << 2);
    assert_eq!(slave_pending[18] & (1 << 2), 1 << 2);
    assert_eq!(machine.pic.acknowledge(), Some(0x72));
    let slave_in_service = pic_payload(&machine);
    assert_eq!(slave_in_service[17] & (1 << 2), 0);
    assert_eq!(slave_in_service[19] & (1 << 2), 1 << 2);
    write_pic_port(&mut machine, 0xa0, 0x20);
    write_pic_port(&mut machine, 0x20, 0x20);
    let slave_reasserted = pic_payload(&machine);
    assert_eq!(slave_reasserted[0] & (1 << 2), 1 << 2);
    assert_eq!(slave_reasserted[1] & (1 << 2), 1 << 2);
    assert_eq!(slave_reasserted[17] & (1 << 2), 1 << 2);
    assert_eq!(slave_reasserted[19] & (1 << 2), 0);
    assert_eq!(machine.pic.acknowledge(), Some(0x72));
    machine.pic.set_irq_level(10, false);
    write_pic_port(&mut machine, 0xa0, 0x20);
    write_pic_port(&mut machine, 0x20, 0x20);
    let settled = pic_payload(&machine);
    assert_eq!(settled[0] & (1 << 2), 0);
    assert_eq!(settled[1] & (1 << 2), 0);
    assert_eq!(settled[2] & (1 << 2), 0);
    assert_eq!(settled[17] & (1 << 2), 0);
    assert_eq!(settled[18] & (1 << 2), 0);
    assert_eq!(settled[19] & (1 << 2), 0);
}

#[test]
fn pit_capture_preserves_status_and_count_latch_read_order() {
    fn configured() -> Machine {
        let mut machine = test_machine();
        write_pit_port(&mut machine, 0x43, 0x36);
        write_pit_port(&mut machine, 0x40, 0x34);
        write_pit_port(&mut machine, 0x40, 0x12);
        machine.advance_devices_clocks(1_000);
        write_pit_port(&mut machine, 0x43, 0xc2);
        machine.advance_devices_clocks(1_000);
        machine
    }

    let mut captured = configured();
    let mut twin = configured();
    assert_eq!(captured.pit, twin.pit);
    assert_eq!(captured.timeline, twin.timeline);

    let frozen = pit_payload(&captured);
    assert_eq!(frozen.len(), PIT_PAYLOAD_LEN);
    assert_eq!(frozen, pit_payload(&captured));
    assert_eq!(frozen[13], 1);
    assert_eq!(frozen[16], 1);

    assert_eq!(read_pit_port(&mut captured, 0x40), frozen[17]);
    assert_eq!(read_pit_port(&mut twin, 0x40), frozen[17]);
    let after_status = pit_payload(&captured);
    assert_eq!(after_status[13], 1);
    assert_eq!(&after_status[16..18], &[0, 0]);
    assert_eq!(captured.pit, twin.pit);

    assert_eq!(read_pit_port(&mut captured, 0x40), frozen[14]);
    assert_eq!(read_pit_port(&mut twin, 0x40), frozen[14]);
    let half_read = pit_payload(&captured);
    assert_eq!(half_read, pit_payload(&captured));
    assert_eq!(half_read[13], 1);
    assert_eq!(half_read[14], 0);
    assert_eq!(half_read[15], frozen[15]);
    assert_eq!(half_read[19], 1);

    assert_eq!(read_pit_port(&mut captured, 0x40), frozen[15]);
    assert_eq!(read_pit_port(&mut twin, 0x40), frozen[15]);
    let consumed = pit_payload(&captured);
    assert_eq!(&consumed[13..16], &[0, 0, 0]);
    assert_eq!(consumed[19], 0);
    assert_eq!(captured.pit, twin.pit);
    assert_eq!(captured.timeline, twin.timeline);
    assert_eq!(captured.pic, twin.pic);
}

#[test]
fn pit_capture_preserves_the_exact_586_irq0_deadline() {
    // Subject is the exact IRQ0 deadline the PIT payload pins across capture, not ISA
    // batch-timing accounting -- `configured()`'s `write_pit_port`/`write_pic_port`
    // calls are raw bus probes, not CPU batches, so pin the arm OFF (see
    // `speaker_payload_pins_every_port_61_latch_value` for the full rationale; the
    // armed arm's capture-after-a-charged-batch behavior is covered by
    // `armed_isa_io_wait_captures_cleanly_after_a_pit_batch_boundary`).
    with_isa_io_wait(false, || {
        fn configured() -> Machine {
            let mut machine = test_machine();
            machine.set_mode(GswMode::Gsw586);
            initialize_pic_pair(&mut machine, false);
            write_pit_port(&mut machine, 0x43, 0x34);
            // The control word raised channel 0's OUT from its power-on low, and
            // that write-side move is a real IRQ0 edge (the 8254 forces OUT with
            // no CLK). Consume it here: this test pins the NEXT, counted edge.
            assert!(machine.pic.irr_bit(0));
            assert_eq!(machine.pic.acknowledge(), Some(0x20));
            write_pic_port(&mut machine, 0x20, 0x20);
            write_pit_port(&mut machine, 0x40, 4);
            write_pit_port(&mut machine, 0x40, 0);
            machine
        }

        let mut captured = configured();
        let mut twin = configured();
        let pit_clocks = captured.clocks_until_timer0_irq().unwrap();
        let deadline = captured
            .timeline
            .cpu_clocks_until(
                crate::timeline::DeviceClock::Pit,
                pit_clocks,
                u64::from(crate::PIT_INPUT_HZ),
            )
            .unwrap();
        assert!(deadline > 1);

        let before = pit_payload(&captured);
        assert_eq!(before, pit_payload(&captured));
        assert_eq!(captured.pit, twin.pit);
        assert_eq!(captured.timeline, twin.timeline);
        assert_eq!(captured.pic, twin.pic);

        captured.advance_devices_clocks(deadline - 1);
        twin.advance_devices_clocks(deadline - 1);
        assert!(!captured.pic.irr_bit(0));
        assert!(!twin.pic.irr_bit(0));
        assert_eq!(pit_payload(&captured), pit_payload(&twin));
        assert_eq!(
            machine_control_timing_payload(&captured),
            machine_control_timing_payload(&twin)
        );
        assert_eq!(pic_payload(&captured), pic_payload(&twin));

        captured.advance_devices_clocks(1);
        twin.advance_devices_clocks(1);
        assert!(captured.pic.irr_bit(0));
        assert!(twin.pic.irr_bit(0));
        assert_eq!(captured.pit, twin.pit);
        assert_eq!(captured.timeline, twin.timeline);
        assert_eq!(captured.pic, twin.pic);
        assert_eq!(pit_payload(&captured), pit_payload(&twin));
        assert_eq!(
            machine_control_timing_payload(&captured),
            machine_control_timing_payload(&twin)
        );
        assert_eq!(pic_payload(&captured), pic_payload(&twin));
        assert_eq!(captured.pic.acknowledge(), Some(0x20));
        assert_eq!(twin.pic.acknowledge(), Some(0x20));
    });
}

#[test]
fn pit_timeline_and_pic_payloads_match_split_586_advancement() {
    // "Split" here means splitting device-clock advancement into several calls versus
    // one (whole vs. split), not the ISA I/O batch this knob accrues against -- the
    // subject is PIT/timeline/PIC payload equality across that split, and
    // `configured()`'s port writes are raw bus probes. Pin the arm OFF (see
    // `speaker_payload_pins_every_port_61_latch_value` for the full rationale; the
    // armed arm's capture-after-a-charged-batch behavior is covered by
    // `armed_isa_io_wait_captures_cleanly_after_a_pit_batch_boundary`).
    with_isa_io_wait(false, || {
        fn configured() -> Machine {
            let mut machine = test_machine();
            machine.set_mode(GswMode::Gsw586);
            initialize_pic_pair(&mut machine, false);
            write_pit_port(&mut machine, 0x43, 0x34);
            // Consume the write-side edge (the control word raised OUT from its
            // power-on low), so the irr assertion below proves the COUNTED edge
            // of the advance arrived, not this configure-time one.
            assert_eq!(machine.pic.acknowledge(), Some(0x20));
            write_pic_port(&mut machine, 0x20, 0x20);
            write_pit_port(&mut machine, 0x40, 7);
            write_pit_port(&mut machine, 0x40, 0);
            machine
        }

        let mut whole = configured();
        let mut split = configured();
        let pit_clocks = whole.clocks_until_timer0_irq().unwrap();
        let first_deadline = whole
            .timeline
            .cpu_clocks_until(
                crate::timeline::DeviceClock::Pit,
                pit_clocks,
                u64::from(crate::PIT_INPUT_HZ),
            )
            .unwrap();
        let total = first_deadline * 5 + 17;
        whole.advance_devices_clocks(total);
        let parts = [1, 17, first_deadline, first_deadline * 2];
        for clocks in parts {
            split.advance_devices_clocks(clocks);
        }
        split.advance_devices_clocks(total - parts.into_iter().sum::<u64>());

        assert!(whole.pic.irr_bit(0));
        assert_eq!(whole.pit, split.pit);
        assert_eq!(whole.timeline, split.timeline);
        assert_eq!(whole.pic, split.pic);
        assert_eq!(pit_payload(&whole), pit_payload(&split));
        assert_eq!(
            machine_control_timing_payload(&whole),
            machine_control_timing_payload(&split)
        );
        assert_eq!(pic_payload(&whole), pic_payload(&split));
    });
}

#[test]
fn non_ring_zero_pending_soft_int_is_resume_equivalent_to_none() {
    let rom = rom_with_code(&[0x90, 0xf4]);
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut none = Machine::new(profile.clone(), &rom).unwrap();
    let mut residue = Machine::new(profile, &rom).unwrap();
    residue.pending_soft_int = Some(0x21);

    assert_eq!(
        machine_control_timing_payload(&none),
        machine_control_timing_payload(&residue)
    );
    let none_reason = none.run_until_halt_or_cycles(10_000).unwrap();
    let residue_reason = residue.run_until_halt_or_cycles(10_000).unwrap();

    assert_eq!(none_reason, residue_reason);
    assert!(none.cpu.perf_counters().instructions > 0);
    assert_eq!(none.cpu, residue.cpu);
    assert_eq!(none.timeline, residue.timeline);
    assert_eq!(none.memory.as_slice(), residue.memory.as_slice());
    assert_eq!(none.pic, residue.pic);
    assert_eq!(none.pit, residue.pit);
    assert_eq!(none.pending_soft_int, None);
    assert_eq!(residue.pending_soft_int, None);
    assert_eq!(none.elapsed_clocks, residue.elapsed_clocks);
    assert_eq!(none.io_stall_clocks, residue.io_stall_clocks);
    assert_eq!(none.trace.elapsed_clocks(), residue.trace.elapsed_clocks());
    assert_eq!(none.scaled_bus_clocks, residue.scaled_bus_clocks);
    assert_eq!(
        machine_control_timing_payload(&none),
        machine_control_timing_payload(&residue)
    );
}

#[test]
fn ring_zero_pending_soft_int_remains_exact_state() {
    let mut none = test_machine();
    let mut pending = test_machine();
    none.cpu.control.cr0 |= 1;
    pending.cpu.control.cr0 |= 1;
    pending.pending_soft_int = Some(0x21);

    let none_payload = machine_control_timing_payload(&none);
    let pending_payload = machine_control_timing_payload(&pending);

    assert_eq!(&none_payload[7..9], &[0, 0]);
    assert_eq!(&pending_payload[7..9], &[1, 0x21]);
    assert_ne!(none_payload, pending_payload);
}

#[test]
fn control_payload_excludes_other_owner_and_host_mechanism_state() {
    let mut machine = test_machine();
    let expected = machine_control_timing_payload(&machine);

    machine.profile.cpu = GswMode::Gsw586;
    machine.profile.address_pipelining = true;
    machine.profile.cache_enabled = true;
    machine.io_touched = true;
    machine.direct_mapping_epoch = 0x1234_5678;
    machine.host_profile.enable();
    machine.trace.set_tracing_mode(TracingMode::Counts);
    let _ = machine.cache_model.data_tier(GswMode::Gsw386, 0x4000);
    machine.katea_root = Some(std::path::PathBuf::from("ignored-host-path"));
    #[cfg(feature = "jit")]
    {
        machine.poll_skip_enabled = !machine.poll_skip_enabled;
        machine.poll_skip_diagnostics.enable_for_test();
    }

    assert_eq!(machine_control_timing_payload(&machine), expected);

    let mut switched = test_machine();
    let before_switch = machine_control_timing_payload(&switched);
    switched.set_mode(GswMode::Gsw586);
    assert_eq!(machine_control_timing_payload(&switched), before_switch);
}

#[test]
fn deferred_services_are_rejected_independently() {
    for command in [
        unittester::CMD_CRC,
        unittester::CMD_SNAPSHOT,
        unittester::CMD_EXIT,
        0x7f,
    ] {
        let mut unit = test_machine();
        write_speaker_port(&mut unit, 0x03);
        let speaker_before = speaker_payload(&unit);
        let pit_before = pit_payload(&unit);
        let pci_before = pci_config_payload(&unit);
        assert!(
            unit.unittester
                .write_port(unittester::PORT_COMMAND, command)
        );
        for _ in 0..2 {
            assert_eq!(
                capture_error(&unit),
                MachineCanonicalCaptureError::PendingUnitTesterCommand { command }
            );
            assert_eq!(unit.speaker.control_bits(), 3);
        }
        assert_eq!(unit.unittester.take_pending(), Some(command));
        assert_eq!(unit_tester_payload(&unit).len(), UNIT_TESTER_PAYLOAD_LEN);
        assert_eq!(speaker_payload(&unit), speaker_before);
        assert_eq!(pit_payload(&unit), pit_before);
        assert_eq!(pci_config_payload(&unit), pci_before);
    }

    let mut mode = test_machine();
    mode.pending_mode = Some(GswMode::Gsw586);
    assert_eq!(
        capture_error(&mode),
        MachineCanonicalCaptureError::PendingModeChange
    );

    let mut toka = test_machine();
    toka.pending_toka_service = Some(1);
    assert_eq!(
        capture_error(&toka),
        MachineCanonicalCaptureError::PendingTokaService { command: 1 }
    );

    let mut bios32 = test_machine();
    bios32.pending_bios32 = Some(Bios32Call::Directory);
    assert_eq!(
        capture_error(&bios32),
        MachineCanonicalCaptureError::PendingBios32Service
    );
}

#[test]
fn uncommitted_coherence_is_rejected_independently() {
    let mut range = test_machine();
    range.pending_device_memory_write_range = Some((0x1000, 4));
    assert_eq!(
        capture_error(&range),
        MachineCanonicalCaptureError::PendingDeviceMemoryWriteRange
    );

    let mut coarse = test_machine();
    coarse.device_wrote_memory = true;
    assert_eq!(
        capture_error(&coarse),
        MachineCanonicalCaptureError::PendingDeviceMemoryWrite
    );

    let mut direct = test_machine();
    direct.direct_map_changed = true;
    assert_eq!(
        capture_error(&direct),
        MachineCanonicalCaptureError::PendingDirectMapChange
    );

    let mut data = test_machine();
    data.direct_data_map_changed = true;
    assert_eq!(
        capture_error(&data),
        MachineCanonicalCaptureError::PendingDirectDataMapChange
    );
}

#[test]
fn uncommitted_batch_timing_is_rejected() {
    let mut machine = test_machine();
    machine.port_bus_batch_clocks = 17;
    assert_eq!(
        capture_error(&machine),
        MachineCanonicalCaptureError::UncommittedBatchTiming { clocks: 17 }
    );
}

#[test]
fn construction_seed_writes_never_accrue_isa_bus_time() {
    // Adversarial-review finding on the `IZARRAVM_ISA_IO_WAIT` slice. `new_raw_program`
    // seeds PIT counter 0 with three `write_io` calls (dos.rs) and programs the 8259 pair,
    // modelling POST work that happened before the guest existed. With the charge armed
    // those writes accrued three ISA periods into `port_bus_batch_clocks` -- an accrual with no
    // batch to belong to, which whichever batch ran first would have paid, and which made
    // the machine uncapturable in the meantime. They go through `make_construction_bus`,
    // which disarms the charge; a freshly constructed machine must therefore hold zero and
    // capture cleanly with the knob ARMED.
    crate::bus::set_isa_io_wait_for_test(Some(true));
    let mut profile = MachineProfile::gsw_386(4, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let machine = Machine::new_raw_program(profile, &[0xf4]).unwrap();
    crate::bus::set_isa_io_wait_for_test(None);

    assert_eq!(
        machine.port_bus_batch_clocks, 0,
        "construction must not charge the guest for POST-time device programming"
    );
    assert!(
        machine.canonical_state_capture().is_ok(),
        "a freshly constructed machine must be capturable with the ISA charge armed"
    );
}

#[test]
fn armed_isa_io_wait_captures_cleanly_after_a_pit_batch_boundary() {
    // The shipped default (armed, since the 2026-08-30 flip) must not leave a machine
    // permanently uncaptureable: a guest OUT to a charged port inside a normal CPU
    // batch is flushed by the run loop's own batch-end step
    // (`std::mem::take(&mut self.port_bus_batch_clocks)` in `run_until_tick`) before the
    // run call returns, so canonical capture succeeds right after. No per-thread
    // override is used here -- this is the armed arm's OWN contract, complementing the
    // arm-pinned-OFF sibling tests elsewhere in this module that isolate PIT/RTC/
    // speaker capture semantics from a raw, batch-external port probe.
    let rom = rom_with_code(&[
        0xb0, 0x34, 0xe6, 0x43, // OUT 0x43, AL -- a charged PIT control-word write
        0xf4, // HLT
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    machine.set_mode(GswMode::Gsw586);

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.port_bus_batch_clocks, 0,
        "the batch-end step must flush the charge before the run call returns"
    );
    assert!(
        machine.canonical_state_capture().is_ok(),
        "a charged PIT access consumed at its own batch boundary must be captureable \
         under the shipped (armed) default"
    );
}

#[test]
fn uncommitted_console_publication_is_rejected() {
    let mut pending = test_machine();
    pending.program_output.extend_from_slice(b"pending");
    assert_eq!(
        capture_error(&pending),
        MachineCanonicalCaptureError::PendingConsolePublication { pending: 7 }
    );

    let mut invalid = test_machine();
    invalid.dos_screen_shown = 1;
    assert_eq!(
        capture_error(&invalid),
        MachineCanonicalCaptureError::InvalidConsoleTracker { shown: 1, total: 0 }
    );
}

#[test]
fn inconsistent_machine_timing_is_rejected_independently() {
    let mut mode = test_machine();
    mode.cpu.set_mode(GswMode::Gsw586);
    assert_eq!(
        capture_error(&mode),
        MachineCanonicalCaptureError::InconsistentCpuMode {
            machine: GswMode::Gsw386,
            cpu: GswMode::Gsw586,
        }
    );

    let mut halted = test_machine();
    halted.halted_ticks = 1;
    assert_eq!(
        capture_error(&halted),
        MachineCanonicalCaptureError::InvalidHaltedTicks {
            halted_ticks: 1,
            now_ticks: 0,
        }
    );

    let mut stall = test_machine();
    stall.io_stall_clocks = 1;
    assert_eq!(
        capture_error(&stall),
        MachineCanonicalCaptureError::InvalidIoStallClocks {
            io_stall_clocks: 1,
            elapsed_clocks: 0,
        }
    );

    let mut bus = test_machine();
    bus.bus_rem = 31;
    assert_eq!(
        capture_error(&bus),
        MachineCanonicalCaptureError::InvalidBusRemainder {
            remainder: 31,
            denominator: 31,
        }
    );
}

#[test]
fn inconsistent_modeled_cache_state_is_rejected_independently() {
    let mut l1_storage = test_machine();
    l1_storage.cache_model.l1_tags =
        vec![crate::CACHE_EMPTY_TAG; CACHE_L1_MAX_LINES - 1].into_boxed_slice();
    assert_eq!(
        capture_error(&l1_storage),
        MachineCanonicalCaptureError::InvalidModeledCacheStorageLength {
            tier: "L1",
            expected: CACHE_L1_MAX_LINES,
            actual: CACHE_L1_MAX_LINES - 1,
        }
    );

    let mut l2_storage = test_machine();
    l2_storage.cache_model.l2_tags =
        vec![crate::CACHE_EMPTY_TAG; CACHE_L2_MAX_LINES - 1].into_boxed_slice();
    assert_eq!(
        capture_error(&l2_storage),
        MachineCanonicalCaptureError::InvalidModeledCacheStorageLength {
            tier: "L2",
            expected: CACHE_L2_MAX_LINES,
            actual: CACHE_L2_MAX_LINES - 1,
        }
    );

    let expected_config = cache_level_config(GswMode::Gsw386, 1);
    let mut l1_mask = test_machine();
    l1_mask.cache_model.config.l1_mask = 0;
    assert_eq!(
        capture_error(&l1_mask),
        MachineCanonicalCaptureError::InconsistentModeledCacheConfiguration {
            expected_l1: expected_config.l1_mask,
            actual_l1: 0,
            expected_l2: expected_config.l2_mask,
            actual_l2: expected_config.l2_mask,
        }
    );

    let mut l2_mask = test_machine();
    l2_mask.cache_model.config.l2_mask = 0x01ff;
    assert_eq!(
        capture_error(&l2_mask),
        MachineCanonicalCaptureError::InconsistentModeledCacheConfiguration {
            expected_l1: expected_config.l1_mask,
            actual_l1: expected_config.l1_mask,
            expected_l2: expected_config.l2_mask,
            actual_l2: 0x01ff,
        }
    );

    let expected_cost = tier_cost(GswMode::Gsw386, 1);
    for (index, actual) in [[1, 0, 3], [0, 1, 3], [0, 0, 4]].into_iter().enumerate() {
        let mut machine = test_machine();
        machine.cache_model.cost.l1 = actual[0];
        machine.cache_model.cost.l2 = actual[1];
        machine.cache_model.cost.ram = actual[2];
        assert_eq!(
            capture_error(&machine),
            MachineCanonicalCaptureError::InconsistentModeledCacheCosts {
                expected: [expected_cost.l1, expected_cost.l2, expected_cost.ram],
                actual,
            },
            "cost component {index}"
        );
    }

    let mut code_fetch = test_machine();
    code_fetch.cache_model.code_fetch_ws = 1;
    assert_eq!(
        capture_error(&code_fetch),
        MachineCanonicalCaptureError::InconsistentModeledCodeFetchWaitStates {
            expected: code_fetch_ws(GswMode::Gsw386),
            actual: 1,
        }
    );
}

#[test]
fn cpu_mode_mismatch_precedes_modeled_cache_validation() {
    let mut machine = test_machine();
    machine.cpu.set_mode(GswMode::Gsw586);
    machine.cache_model.config.l2_mask = 0;

    assert_eq!(
        capture_error(&machine),
        MachineCanonicalCaptureError::InconsistentCpuMode {
            machine: GswMode::Gsw386,
            cpu: GswMode::Gsw586,
        }
    );
}

#[test]
fn cpu_capture_errors_keep_their_identity() {
    assert_eq!(
        MachineCanonicalCaptureError::from(CpuCanonicalCaptureError::ActiveRepContinuation),
        MachineCanonicalCaptureError::Cpu(CpuCanonicalCaptureError::ActiveRepContinuation)
    );
}

#[test]
fn inconsistent_ram_rom_and_lookup_state_is_rejected_independently() {
    let expected_ram_len = usize::from(TEST_MEMORY_MIB) * 1024 * 1024;

    let mut ram = memory_test_machine();
    ram.profile.memory_mib += 1;
    assert_eq!(
        capture_error(&ram),
        MachineCanonicalCaptureError::InconsistentRamLength {
            expected: expected_ram_len + 1024 * 1024,
            actual: expected_ram_len,
        }
    );

    let mut rom = memory_test_machine();
    rom.rom.pop();
    assert_eq!(
        capture_error(&rom),
        MachineCanonicalCaptureError::InconsistentSystemRomLength {
            expected: BIOS_ROM_SIZE,
            actual: BIOS_ROM_SIZE - 1,
        }
    );

    let mut lookup = memory_test_machine();
    lookup.ram_lookup = crate::RamPageLookup::new(
        lookup.memory.len() - crate::video_params::RAM_LOOKUP_PAGE_SIZE,
        &lookup.vega,
    );
    assert_eq!(
        capture_error(&lookup),
        MachineCanonicalCaptureError::InconsistentRamPageLookup
    );
}

#[test]
fn ram_rom_capture_and_serialization_are_read_only() {
    let mut machine = memory_test_machine();
    machine.memory.as_mut_slice()[0x1234] = 0xa5;
    machine.rom[0x4321] = 0x5a;
    machine.direct_mapping_epoch = 0x1234_5678;
    machine.trace.set_tracing_mode(TracingMode::Counts);
    let _ = machine
        .cache_model
        .data_tier(machine.active_mode, 0x0010_0000);

    let before_ram = machine.memory.as_slice().to_vec();
    let before_rom = machine.rom.clone();
    let before_cache = (
        machine.cache_model.l1_tags.clone(),
        machine.cache_model.l2_tags.clone(),
        machine.cache_model.lookups,
    );
    let before_mechanism = (
        machine.direct_mapping_epoch,
        machine.direct_map_changed,
        machine.direct_data_map_changed,
        machine.pending_device_memory_write_range,
        machine.device_wrote_memory,
    );
    let before_timing = (
        machine.elapsed_clocks,
        machine.scaled_bus_clocks,
        machine.trace.elapsed_clocks(),
    );

    let first = ram_rom_payload(&machine);
    let second = ram_rom_payload(&machine);

    assert_eq!(first, second);
    assert_eq!(machine.memory.as_slice(), before_ram);
    assert_eq!(machine.rom, before_rom);
    assert_eq!(
        (
            machine.cache_model.l1_tags.clone(),
            machine.cache_model.l2_tags.clone(),
            machine.cache_model.lookups,
        ),
        before_cache
    );
    assert_eq!(
        (
            machine.direct_mapping_epoch,
            machine.direct_map_changed,
            machine.direct_data_map_changed,
            machine.pending_device_memory_write_range,
            machine.device_wrote_memory,
        ),
        before_mechanism
    );
    assert_eq!(
        (
            machine.elapsed_clocks,
            machine.scaled_bus_clocks,
            machine.trace.elapsed_clocks(),
        ),
        before_timing
    );
    assert!(
        machine
            .ram_lookup
            .is_consistent(machine.memory.len(), &machine.vega)
    );
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn scalar_and_direct_native_stores_share_the_ram_owner() {
    const OFFSET: u32 = 0x6000;
    const VALUE: u32 = 0x1122_3344;
    const PROGRAM: &[u8] = &[
        0xb9, 0x64, 0x00, 0x00, 0x00, // mov ecx,100
        0xb8, 0x44, 0x33, 0x22, 0x11, // mov eax,11223344h
        0xa3, 0x00, 0x60, 0x00, 0x00, // store: mov [00006000h],eax
        0x83, 0xe9, 0x01, // sub ecx,1
        0x75, 0xf6, // jnz store
        0xf4, // hlt
    ];

    if !izarravm_cpu::native_backend_available() {
        return;
    }

    fn store_machine(program: &[u8], native: bool) -> Machine {
        let mut profile = MachineProfile::gsw_386(TEST_MEMORY_MIB, VideoCard::Vega);
        profile.cpu = GswMode::Gsw586;
        let mut machine = Machine::new_raw_program(profile, program).unwrap();
        let mut cs = machine.cpu.registers.cs();
        cs.default_size_32 = true;
        cs.limit = u32::MAX;
        machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
        machine.cpu.set_native_backend_enabled(native);
        machine.set_jit_auto_admit(native);
        machine.trace.set_tracing_mode(TracingMode::Off);
        machine.poll_skip_enabled = false;
        machine
    }

    let mut scalar = store_machine(PROGRAM, false);
    let mut bulk_direct = store_machine(PROGRAM, false);
    let mut direct = store_machine(PROGRAM, true);
    let target = direct.cpu.registers.segment(SegmentIndex::Ds).base + OFFSET;
    assert_eq!(
        scalar.cpu.registers.segment(SegmentIndex::Ds).base + OFFSET,
        target
    );
    assert_eq!(
        bulk_direct.cpu.registers.segment(SegmentIndex::Ds).base + OFFSET,
        target
    );

    {
        let mut bus = scalar.make_bus();
        CpuBus::write_memory(
            &mut bus,
            target,
            BusWidth::Dword,
            VALUE,
            BusAccessKind::DataWrite,
        )
        .unwrap();
    }
    {
        let mut bus = bulk_direct.make_bus();
        assert_eq!(
            CpuBus::write_memory_bytes_direct(
                &mut bus,
                target,
                &VALUE.to_le_bytes(),
                BusWidth::Dword,
                BusAccessKind::DataWrite,
            )
            .unwrap(),
            4
        );
    }

    let installed = direct.cpu.perf_counters().jit_direct_blocks_installed;
    let native_stores = direct.cpu.perf_counters().jit_native_store_hits;
    for _ in 0..4 {
        direct.cpu.halted = false;
        direct.cpu.registers.eip = 0x100;
        assert_eq!(
            direct.run_until_halt_or_cycles(100_000).unwrap(),
            StopReason::Halted
        );
    }

    assert!(direct.cpu.perf_counters().jit_direct_blocks_installed > installed);
    assert!(direct.cpu.perf_counters().jit_native_store_hits > native_stores);
    assert_eq!(
        &direct.memory.as_slice()[target as usize..target as usize + 4],
        &VALUE.to_le_bytes()
    );
    assert_eq!(direct.memory.as_slice(), scalar.memory.as_slice());
    assert_eq!(bulk_direct.memory.as_slice(), scalar.memory.as_slice());
    assert_eq!(direct.rom, scalar.rom);
    assert_eq!(bulk_direct.rom, scalar.rom);
    assert_eq!(ram_rom_payload(&direct), ram_rom_payload(&scalar));
    assert_eq!(ram_rom_payload(&bulk_direct), ram_rom_payload(&scalar));
}

#[test]
fn unit_tester_exit_zero_is_a_captureable_batch_boundary() {
    let rom = rom_with_code(&[
        0xb0, 0x0c, 0xe6, 0xe4, // select REG_EXIT
        0xb0, 0x00, 0xe6, 0xe5, // write exit code zero
        0xb0, 0x03, 0xe6, 0xe6, // issue CMD_EXIT
        0xf4, // must not execute
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert_eq!(reason, StopReason::TestExit { code: 0 });
    assert_eq!(machine.cpu.registers.eip, 12);
    assert!(!machine.cpu.halted);
    assert!(machine.io_touched);
    assert!(!machine.cpu.is_ring0_protected());
    assert_eq!(machine.dos_screen_shown, machine.program_output.len());
    assert_eq!(
        machine_control_timing_payload(&machine).len(),
        MACHINE_CONTROL_TIMING_PAYLOAD_LEN
    );
    assert_eq!(
        modeled_cache_payload(&machine),
        vec![0; EMPTY_MODELED_CACHE_PAYLOAD_LEN]
    );
    assert_eq!(pic_payload(&machine).len(), PIC_PAYLOAD_LEN);
    let pit_before = machine.pit.clone();
    let timeline_before = machine.timeline;
    let pic_before = machine.pic.clone();
    let trace_before = machine.trace.elapsed_clocks();
    let first_pit = pit_payload(&machine);
    let second_pit = pit_payload(&machine);
    assert_eq!(first_pit.len(), PIT_PAYLOAD_LEN);
    assert_eq!(first_pit, second_pit);
    assert_eq!(machine.pit, pit_before);
    assert_eq!(machine.timeline, timeline_before);
    assert_eq!(machine.pic, pic_before);
    assert_eq!(machine.trace.elapsed_clocks(), trace_before);
    let first_dma = dma_payload(&machine);
    let first_dma_totals = dma_event_totals_v1_payload(&machine);
    let second_dma_totals = dma_event_totals_v1_payload(&machine);
    let second_dma = dma_payload(&machine);
    assert_eq!(first_dma.len(), DMA_PAYLOAD_LEN);
    assert_eq!(first_dma, second_dma);
    assert_eq!(first_dma_totals.len(), DMA_EVENT_TOTALS_V1_PAYLOAD_LEN);
    assert_eq!(first_dma_totals, second_dma_totals);
    assert_eq!(first_dma_totals, vec![0; DMA_EVENT_TOTALS_V1_PAYLOAD_LEN]);
    let first_rtc = rtc_payload(&machine);
    let second_rtc = rtc_payload(&machine);
    assert_eq!(first_rtc.len(), RTC_PAYLOAD_LEN);
    assert_eq!(first_rtc, second_rtc);
    let first_unit_tester = unit_tester_payload(&machine);
    let second_unit_tester = unit_tester_payload(&machine);
    assert_eq!(first_unit_tester.len(), UNIT_TESTER_PAYLOAD_LEN);
    assert_eq!(first_unit_tester, second_unit_tester);
    assert_eq!(first_unit_tester[0], unittester::REG_EXIT as u8 + 1);
    assert_eq!(first_unit_tester[1 + unittester::REG_EXIT], 0);
    let first_pci = pci_config_payload(&machine);
    let second_pci = pci_config_payload(&machine);
    assert_eq!(first_pci.len(), PCI_CONFIG_PAYLOAD_LEN);
    assert_eq!(first_pci, second_pci);
    let payload = ram_rom_payload(&machine);
    let mut cursor = 0;
    assert_eq!(
        take_len_prefixed_bytes(&payload, &mut cursor),
        machine.memory.as_slice()
    );
    assert_eq!(
        take_len_prefixed_bytes(&payload, &mut cursor),
        machine.rom.as_slice()
    );
    assert_eq!(cursor, payload.len());
}

#[test]
fn speaker_latch_survives_a_real_586_test_exit_boundary() {
    // Subject is that the speaker latch survives capture at a real TestExit boundary,
    // not ISA batch-timing accounting. The guest OUT 0x61 inside the ROM is charged and
    // properly flushed by the run loop's own batch-end step (TestExit is itself a
    // captureable batch boundary -- see `unit_tester_exit_zero_is_a_captureable_batch_
    // boundary`), so that part needs no override. But the trailing `read_speaker_port`
    // probe below is a raw `machine.make_bus()` access with no batch to flush its
    // charge into, which would refuse the final capture with `UncommittedBatchTiming`
    // for a reason unrelated to what this test pins. Pin the arm OFF for the whole test
    // rather than split it, so the guest-side charge and the probe-side charge are both
    // absent uniformly; the armed arm's capture-after-a-charged-batch behavior is
    // covered by `armed_isa_io_wait_captures_cleanly_after_a_pit_batch_boundary`.
    with_isa_io_wait(false, || {
        let rom = rom_with_code(&[
            0xb0, 0xff, 0xe6, 0x61, // latch both speaker bits; upper bits normalize
            0xb0, 0x0c, 0xe6, 0xe4, // select REG_EXIT
            0xb0, 0x00, 0xe6, 0xe5, // write exit code zero
            0xb0, 0x03, 0xe6, 0xe6, // issue CMD_EXIT
            0xf4, // must not execute
        ]);
        let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
        machine.set_mode(GswMode::Gsw586);

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

        assert_eq!(reason, StopReason::TestExit { code: 0 });
        assert_eq!(machine.cpu.registers.eip, 16);
        assert!(!machine.cpu.halted);
        assert_eq!(machine.unittester.pending_command(), None);
        assert_eq!(speaker_payload(&machine), [3]);
        assert_eq!(pit_payload(&machine)[PIT_CHANNEL_2_GATE_OFFSET], 1);
        assert_eq!(read_speaker_port(&mut machine) & 0x03, 3);
        assert_eq!(speaker_payload(&machine), [3]);
    });
}

#[test]
fn pci_config_state_survives_a_real_586_test_exit_boundary() {
    let mut code = Vec::new();
    push_out_dx_eax(&mut code, crate::PCI_CONFIG_ADDRESS_PORT, 0x8000_3904);
    push_out_dx_eax(&mut code, crate::PCI_CONFIG_DATA_PORT, 0x0000_0004);
    push_out_dx_eax(&mut code, crate::PCI_CONFIG_ADDRESS_PORT, 0x8000_3920);
    push_out_dx_eax(&mut code, crate::PCI_CONFIG_DATA_PORT, 0x1234_e10f);
    push_out_dx_eax(&mut code, crate::PCI_CONFIG_ADDRESS_PORT, 0xd5b3_a69d);
    code.extend_from_slice(&[
        0xb0, 0x0c, 0xe6, 0xe4, // select REG_EXIT
        0xb0, 0x00, 0xe6, 0xe5, // write exit code zero
        0xb0, 0x03, 0xe6, 0xe6, // issue CMD_EXIT
        0xf4, // must not execute
    ]);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        rom_with_code(&code),
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw586);

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert_eq!(reason, StopReason::TestExit { code: 0 });
    assert_eq!(machine.cpu.registers.eip, 67);
    assert!(!machine.cpu.halted);
    assert_eq!(machine.unittester.pending_command(), None);
    let expected = [0x9d, 0xa6, 0xb3, 0xd5, 0x04, 0x00, 0xe1, 0x34, 0x12];
    assert_eq!(pci_config_payload(&machine), expected);
    assert_eq!(pci_config_payload(&machine), expected);
}

#[test]
fn ata_state_survives_a_real_586_test_exit_boundary() {
    fn push_out_dx_al(code: &mut Vec<u8>, port: u16, value: u8) {
        code.extend_from_slice(&[0xba, port as u8, (port >> 8) as u8]);
        code.extend_from_slice(&[0xb0, value]);
        code.push(0xee);
    }
    let base = crate::ata::PRIMARY_CMD_BASE;
    let mut code = Vec::new();
    push_out_dx_al(&mut code, base + 1, 0x55);
    push_out_dx_al(&mut code, base + 2, 3);
    push_out_dx_al(&mut code, base + 3, 0x11);
    push_out_dx_al(&mut code, base + 4, 0x22);
    push_out_dx_al(&mut code, base + 5, 0x33);
    push_out_dx_al(&mut code, base + 6, 0x4f);
    push_out_dx_al(&mut code, crate::ata::PRIMARY_CTRL, 0x02);
    code.extend_from_slice(&[
        0xb0, 0x0c, 0xe6, 0xe4, // select REG_EXIT
        0xb0, 0x00, 0xe6, 0xe5, // write exit code zero
        0xb0, 0x03, 0xe6, 0xe6, // issue CMD_EXIT
        0xf4, // must not execute
    ]);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        rom_with_code(&code),
    )
    .unwrap();
    machine.mount_hdd(vec![0u8; 8 * crate::ata::SECTOR]);
    machine.set_mode(GswMode::Gsw586);

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert_eq!(reason, StopReason::TestExit { code: 0 });
    assert_eq!(machine.cpu.registers.eip, 54);
    assert!(!machine.cpu.halted);
    assert_eq!(machine.unittester.pending_command(), None);
    let mut expected = ata_idle_golden();
    expected[1..7].copy_from_slice(&[0x55, 3, 0x11, 0x22, 0x33, 0x4f]);
    expected[11] = 1; // nIEN
    assert_eq!(ata_payload(&machine), expected);
    assert_eq!(ata_payload(&machine), expected);
}

#[test]
fn bmide_state_survives_a_real_586_test_exit_boundary() {
    let mut code = Vec::new();
    // BM command (read direction, no START), scratch status bits, PRD.
    code.extend_from_slice(&[0xba, 0x00, 0xf0, 0xb0, 0x08, 0xee]);
    code.extend_from_slice(&[0xba, 0x02, 0xf0, 0xb0, 0x20, 0xee]);
    push_out_dx_eax(&mut code, BMIDE_BASE + 4, 0x0000_4000);
    code.extend_from_slice(&[
        0xb0, 0x0c, 0xe6, 0xe4, // select REG_EXIT
        0xb0, 0x00, 0xe6, 0xe5, // write exit code zero
        0xb0, 0x03, 0xe6, 0xe6, // issue CMD_EXIT
        0xf4, // must not execute
    ]);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        rom_with_code(&code),
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw586);

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert_eq!(reason, StopReason::TestExit { code: 0 });
    assert_eq!(machine.cpu.registers.eip, 35);
    assert!(!machine.cpu.halted);
    assert_eq!(machine.unittester.pending_command(), None);
    let mut expected = [0u8; BMIDE_IDLE_PAYLOAD_LEN];
    expected[0] = 0x08;
    expected[1] = 0x20;
    expected[2..6].copy_from_slice(&0x4000u32.to_le_bytes());
    assert_eq!(bmide_payload(&machine), expected);
    assert_eq!(bmide_payload(&machine), expected);
}

#[test]
fn atapi_state_survives_a_real_586_test_exit_boundary() {
    let base = crate::ide::SECONDARY_CMD_BASE;
    let mut code = Vec::new();
    for (port, value) in [
        (base + 1, 0x55u8),
        (base + 4, 0x34),
        (base + 5, 0x12),
        (crate::ide::SECONDARY_CTRL, 0x02),
    ] {
        code.extend_from_slice(&[0xba, port as u8, (port >> 8) as u8, 0xb0, value, 0xee]);
    }
    code.extend_from_slice(&[
        0xb0, 0x0c, 0xe6, 0xe4, // select REG_EXIT
        0xb0, 0x00, 0xe6, 0xe5, // write exit code zero
        0xb0, 0x03, 0xe6, 0xe6, // issue CMD_EXIT
        0xf4, // must not execute
    ]);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        rom_with_code(&code),
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw586);

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert_eq!(reason, StopReason::TestExit { code: 0 });
    assert_eq!(machine.cpu.registers.eip, 36);
    assert!(!machine.cpu.halted);
    assert_eq!(machine.unittester.pending_command(), None);
    let mut expected = atapi_idle_golden();
    expected[0] = 0x55;
    expected[3] = 0x34;
    expected[4] = 0x12;
    expected[8] = 1;
    assert_eq!(atapi_payload(&machine), expected);
    assert_eq!(atapi_payload(&machine), expected);
}

#[test]
fn vega_routing_state_survives_a_real_586_test_exit_boundary() {
    // OUT-driven writes cover the Distira half of the payload; the Margo
    // latches are covered through the INT 10h HLE in the direct tests above.
    let mut code = Vec::new();
    push_out_dx_eax(&mut code, crate::PCI_CONFIG_ADDRESS_PORT, 0x8000_8010);
    push_out_dx_eax(&mut code, crate::PCI_CONFIG_DATA_PORT, 0x5634_1200);
    push_out_dx_eax(&mut code, crate::PCI_CONFIG_ADDRESS_PORT, 0x8000_8040);
    push_out_dx_eax(&mut code, crate::PCI_CONFIG_DATA_PORT, 0xa55a_1234);
    push_out_dx_eax(&mut code, crate::PCI_CONFIG_ADDRESS_PORT, 0x8000_8004);
    push_out_dx_eax(&mut code, crate::PCI_CONFIG_DATA_PORT, 0x0000_0000);
    code.extend_from_slice(&[
        0xb0, 0x0c, 0xe6, 0xe4, // select REG_EXIT
        0xb0, 0x00, 0xe6, 0xe5, // write exit code zero
        0xb0, 0x03, 0xe6, 0xe6, // issue CMD_EXIT
        0xf4, // must not execute
    ]);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        rom_with_code(&code),
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw586);

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert_eq!(reason, StopReason::TestExit { code: 0 });
    assert_eq!(machine.cpu.registers.eip, 78);
    assert!(!machine.cpu.halted);
    assert_eq!(machine.unittester.pending_command(), None);
    let expected = [
        0, 0, 0, 0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x56, 0x34, 0x12, 0x5a, 0xa5,
    ];
    assert_eq!(vega_routing_payload(&machine), expected);
    assert_eq!(vega_routing_payload(&machine), expected);
}
