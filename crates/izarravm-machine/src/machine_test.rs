// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_core::{SbDma8, SbDma16, SbIrq};
use izarravm_firmware::I386DX25_TEST_ROM;
use izarravm_video::{VGA_MODE13H_BASE, VGA_MONO_TEXT_BASE, VGA_TEXT_BASE};
// Re-exported from cache carve (Phase 3).
use super::cache_config::{CACHE_LINE_BYTES, CACHE_TIER_DISABLED_MASK, cache_geometry};

const BIOS_TEXT_WHITE: u8 = 0x3F;

#[test]
fn jit_auto_admission_policy_defaults_on_only_when_available() {
    assert!(jit_auto_admit_policy(None, true));
    assert!(jit_auto_admit_policy(Some("1"), true));
    assert!(jit_auto_admit_policy(Some("yes"), true));
    assert!(!jit_auto_admit_policy(Some("0"), true));
    assert!(!jit_auto_admit_policy(Some(""), true));
    assert!(!jit_auto_admit_policy(None, false));
    assert!(!jit_auto_admit_policy(Some("1"), false));
}

// The CacheModel tests below exercise tier IDENTITY, not the wait-state numbers:
// tier_cost is calibrated (non-zero) now, but these tests assert only that the
// model resolves L1/L2/RAM correctly per the per-mode geometry, not the specific
// costs.
// The per-batch interrupt check (Stage-1 lever 2) must not break the classic
// STI; HLT idle loop. The CPU services interrupts once at batch entry; the HLT
// ends a batch halted, and the NEXT batch's entry check must see IF set, the
// shadow already consumed by the HLT instruction, and IRQ0 pending - and take
// it. The wrong design (consuming the STI shadow at batch entry instead of per
// instruction) makes this loop spin forever and never tick.
// A run of straight-line instructions between interrupt checks must not delay
// the interrupt past the batch: `sti; nop x5; jmp $-7` keeps the CPU busy with
// no HLT and no port I/O, so a whole batch of NOPs runs through
// cycle_no_interrupt_check before the next batch entry, where IRQ0 is taken.
fn test_machine() -> Machine {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        I386DX25_TEST_ROM,
    )
    .unwrap();
    machine.set_bus_trace_detailed(true);
    machine
}

fn int15_machine(mem_mib: u16) -> Machine {
    Machine::new(
        MachineProfile::gsw_386(mem_mib, VideoCard::Et4000Ax),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap()
}

/// Emulate a guest `INT n` end to end for interception-contract tests: the
/// opcode acknowledge (which posts only the raw-program low-RAM vectors
/// and stashes the legacy-chain attribution), then the IVT dispatch
/// landing wherever the vector points. A default vector lands on its
/// per-vector ROM stub, whose fetch seam posts the service; a guest hook
/// lands outside the table and gets NO HLE post (the hook owns the
/// vector); the legacy shared FF00:0000 posts the stashed vector.
fn ack_and_dispatch(m: &mut Machine, vector: u8) {
    let mut bus = m.make_bus();
    bus.interrupt_acknowledge(vector, 0).unwrap();
    let base = usize::from(vector) * 4;
    let off = bus.memory.read_u16(base).unwrap();
    let seg = bus.memory.read_u16(base + 2).unwrap();
    let target = (u32::from(seg) << 4) + u32::from(off);
    bus.note_stub_fetch(target);
}

fn color_crtc_reg(machine: &mut Machine, index: u8) -> u8 {
    let mut bus = machine.make_bus();
    bus.write_io(0x3D4, BusWidth::Byte, u32::from(index), false)
        .unwrap();
    bus.read_io(0x3D5, BusWidth::Byte, 0, false).unwrap() as u8
}

fn prime_dos_int_frame(m: &mut Machine) {
    m.cpu
        .registers
        .set_segment(SegmentIndex::Ss, SegmentRegister::real(0x9000));
    m.cpu.registers.set_esp(0x0100);
    m.memory.write_u16(0x9000 * 16 + 0x0104, 0x0001).unwrap();
}

fn dos_int_flags(m: &Machine) -> u16 {
    m.memory.read_u16(0x9000 * 16 + 0x0104).unwrap()
}

fn rom_with_code(code: &[u8]) -> Vec<u8> {
    let mut rom = vec![0; BIOS_ROM_SIZE];
    rom[..code.len()].copy_from_slice(code);
    // The ROM IRET at offset 0xF000 (FF00:0000) the real izarra BIOS emits.
    // The host-intercepted BIOS service vectors return through it, so the
    // bare test ROM supplies it too.
    rom[0xF000] = 0xCF;
    rom[0xfff0..0xfff5].copy_from_slice(&[0xea, 0x00, 0x00, 0x00, 0xf0]);
    rom
}

/// Run a .COM that reads one key via INT 16h AH=00h and stores AX at DS:0x200,
/// after injecting `scancodes`. Returns the value INT 16h handed the program.
/// This is the editor's keyboard path end to end: 8042 -> IRQ1 -> INT 09h ISR
/// -> BDA ring -> INT 16h read.
fn int16_read_after_with_layout(layout: u8, scancodes: &[u8]) -> u16 {
    // mov ah,0; int 16h; mov [0x200],ax; int 20h
    const PROG: [u8; 9] = [0xB4, 0x00, 0xCD, 0x16, 0xA3, 0x00, 0x02, 0xCD, 0x20];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), &PROG).unwrap();
    machine.write_physical_u8(0x0496, layout);
    machine.inject_key_scancodes(scancodes);
    machine.run_until_halt_or_cycles(3_000_000).unwrap();
    read_u16(&mut machine, (u32::from(DOS_LOAD_SEGMENT) << 4) + 0x200)
}

fn int16_read_after(scancodes: &[u8]) -> u16 {
    int16_read_after_with_layout(0, scancodes)
}

fn int16_peek_guest_exit(scancodes: &[u8], prog: &[u8]) -> StopReason {
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), prog).unwrap();
    machine.inject_key_scancodes(scancodes);
    machine.run_until_halt_or_cycles(1_000_000).unwrap()
}

/// Same path as `int16_read_after`, but the program reads with AH=10h (the
/// enhanced read). Before the DOS keyboard ROM aliased AH=10h to the AH=00h
/// reader, this fell through the int16 dispatch and returned stale AX.
fn int16_enhanced_read_after(scancodes: &[u8]) -> u16 {
    // mov ah,0x10; int 16h; mov [0x200],ax; int 20h
    const PROG: [u8; 9] = [0xB4, 0x10, 0xCD, 0x16, 0xA3, 0x00, 0x02, 0xCD, 0x20];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), &PROG).unwrap();
    machine.inject_key_scancodes(scancodes);
    machine.run_until_halt_or_cycles(3_000_000).unwrap();
    read_u16(&mut machine, (u32::from(DOS_LOAD_SEGMENT) << 4) + 0x200)
}

fn read_u16(machine: &mut Machine, addr: u32) -> u16 {
    u16::from(machine.read_physical_u8(addr)) | (u16::from(machine.read_physical_u8(addr + 1)) << 8)
}

fn read_u32(machine: &mut Machine, addr: u32) -> u32 {
    u32::from(read_u16(machine, addr)) | (u32::from(read_u16(machine, addr + 2)) << 16)
}

/// A small hard-disk image whose first byte per sector marks the LBA, plus an
/// otherwise-zero machine with the disk mounted as C:.
fn machine_with_hdd(sectors: usize) -> Machine {
    let mut bytes = vec![0u8; sectors * 512];
    for s in 0..sectors {
        bytes[s * 512] = (s as u8).wrapping_add(0x10);
    }
    let mut m = int15_machine(16);
    m.mount_hdd(bytes);
    m
}

// ---- AD1848 / Windows Sound System integration ------------------------

// The default WSS board: config region at 0x530, codec direct registers at
// 0x534-0x537 (base+4), IRQ7, byte-wide DMA channel 0.
const WSS_CODEC: u16 = 0x534; // R0 Index
const WSS_DATA: u16 = 0x535; // R1 Indexed Data

/// Write one AD1848 indirect register through the codec's R0 (index) + R1
/// (data) direct ports on the machine bus.
fn wss_write_indirect(bus: &mut MachineBus, index: u8, value: u8) {
    bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(index), false)
        .unwrap();
    bus.write_io(WSS_DATA, BusWidth::Byte, u32::from(value), false)
        .unwrap();
}

/// Program DMA channel 0 (the WSS default) for a single-cycle 8-bit read of
/// `count + 1` bytes at physical `0x01_0000`, then arm the AD1848 codec for
/// 8-bit unsigned mono at 48000 Hz with IEN set and `count` base count.
fn wss_arm_8bit_mono(bus: &mut MachineBus, count: u8) {
    // DMA ch0: mode single+read, addr 0x0000, count, page 0x01, unmask.
    bus.write_io(0x0B, BusWidth::Byte, 0x48, false).unwrap();
    bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
    bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
    bus.write_io(0x01, BusWidth::Byte, u32::from(count), false)
        .unwrap();
    bus.write_io(0x01, BusWidth::Byte, 0x00, false).unwrap();
    bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap();
    bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap();
    // Codec: 8-bit unsigned PCM mono at 48000 Hz (I8 = CFS6 -> 0x0C), MCE-gated.
    bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08), false)
        .unwrap();
    bus.write_io(WSS_DATA, BusWidth::Byte, 0x0C, false).unwrap();
    bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
        .unwrap(); // clear MCE
    wss_write_indirect(bus, 10, 0x02); // I10 IEN
    wss_write_indirect(bus, 15, count); // I15 lower count
    wss_write_indirect(bus, 14, 0x00); // I14 upper count (loads current)
    wss_write_indirect(bus, 9, 0x09); // I9 PEN | ACAL
    wss_write_indirect(bus, 6, 0x00);
    wss_write_indirect(bus, 7, 0x00);
}

/// Program DMA channel 0 and the codec for 16-bit signed stereo at 48 kHz with
/// IEN set, drawing `frames` frames (4 bytes each) at physical 0x01_0000.
fn wss_arm_16bit_stereo(bus: &mut MachineBus, frames: u8) {
    let byte_count = u16::from(frames) * 4 - 1; // count is bytes-1
    bus.write_io(0x0B, BusWidth::Byte, 0x48, false).unwrap(); // mode ch0: single, read
    bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
    bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
    bus.write_io(0x01, BusWidth::Byte, u32::from(byte_count & 0xFF), false)
        .unwrap();
    bus.write_io(0x01, BusWidth::Byte, u32::from(byte_count >> 8), false)
        .unwrap();
    bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap();
    bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap();
    // I8 = FMT(0x40) | S/M(0x10) | CFS6(0x0C) -> 0x5C, MCE-gated.
    bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08), false)
        .unwrap();
    bus.write_io(WSS_DATA, BusWidth::Byte, 0x5C, false).unwrap();
    bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
        .unwrap(); // clear MCE
    wss_write_indirect(bus, 10, 0x02); // IEN
    let count = u16::from(frames) - 1;
    wss_write_indirect(bus, 15, (count & 0xFF) as u8);
    wss_write_indirect(bus, 14, (count >> 8) as u8);
    wss_write_indirect(bus, 9, 0x09); // PEN | ACAL
    wss_write_indirect(bus, 6, 0x00); // left DAC 0 dB
    wss_write_indirect(bus, 7, 0x00); // right DAC 0 dB
}

/// Load `frames` asymmetric 16-bit LE stereo frames at 0x01_0000: L = +0x4000,
/// R = -0x4000, so the de-interleaved, mixed output carries L > 0 and R < 0.
fn load_asymmetric_stereo(machine: &mut Machine, frames: u32) {
    // L = 0x4000 (+16384) -> bytes 0x00,0x40; R = 0xC000 (-16384) -> 0x00,0xC0.
    let frame: [u8; 4] = [0x00, 0x40, 0x00, 0xC0];
    for i in 0..frames {
        for (j, &b) in frame.iter().enumerate() {
            machine.write_physical_u8(0x1_0000 + i * 4 + j as u32, b);
        }
    }
}

// Run one closure against a freshly-borrowed bus over the whole machine.
fn with_bus<R>(machine: &mut Machine, f: impl FnOnce(&mut MachineBus) -> R) -> R {
    // Captured before the struct literal below since video/trace are also
    // mutably borrowed by other fields in that same literal.
    let beam_at_batch_start = machine.video.beam_dots();
    let trace_elapsed_at_batch_start = machine.trace.elapsed_clocks();
    let (bus_num_at_batch_start, bus_den_at_batch_start) = bus_timing(machine.cpu.level());
    let mut bus = MachineBus {
        memory: &mut machine.memory,
        ram_lookup: &mut machine.ram_lookup,
        video: &mut machine.video,
        margo: &mut machine.margo,
        distira: &mut machine.distira,
        pci: &mut machine.pci,
        rom: &machine.rom,
        serial: &mut machine.serial,
        serial2: &mut machine.serial2,
        lpt: &mut machine.lpt,
        lpt2: &mut machine.lpt2,
        device_ports: &mut machine.device_ports,
        pic: &mut machine.pic,
        pit: &mut machine.pit,
        keyboard: &mut machine.keyboard,
        speaker: &mut machine.speaker,
        rtc: &mut machine.rtc,
        dma: &mut machine.dma,
        fdc: &mut machine.fdc,
        floppy: &mut machine.floppy,
        opl: &mut machine.opl,
        dsp: &mut machine.dsp,
        mixer: &mut machine.mixer,
        wavetable_mpu: &mut machine.wavetable_mpu,
        midi_input_mpu: &mut machine.midi_input_mpu,
        wss: &mut machine.wss,
        wss_base: machine.wss_base,
        wss_enabled: machine.wss_enabled,
        ide: &mut machine.ide,
        ata: &mut machine.ata,
        trace: &mut machine.trace,
        pending_soft_int: &mut machine.pending_soft_int,
        last_int_vector: &mut machine.last_int_vector,
        active_mode: machine.active_mode,
        pending_mode: &mut machine.pending_mode,
        fast_post: machine.fast_post,
        booter_inert: machine.booter_inert,
        program_runtime: machine.program_runtime,
        pending_toka_service: &mut machine.pending_toka_service,
        toka_service_status: machine.toka_service_status,
        unittester: &mut machine.unittester,
        wait_states: machine.profile.wait_states,
        cache: &mut machine.cache_model,
        flat_data_cost: machine.active_mode.uses_approximate_timing(),
        lazy_port_reads: machine.active_mode.uses_approximate_timing(),
        io_touched: &mut machine.io_touched,
        isa_io_clocks: &mut machine.isa_io_batch_clocks,
        device_wrote_memory: &mut machine.device_wrote_memory,
        direct_map_changed: &mut machine.direct_map_changed,
        core_clocks_so_far: 0,
        prior_runs_core_clocks: 0,
        timeline_at_batch_start: machine.timeline,
        master_ticks_at_batch_start: machine.timeline.now_ticks(),
        beam_at_batch_start,
        trace_elapsed_at_batch_start,
        bus_rem_at_batch_start: machine.bus_rem,
        bus_num_at_batch_start,
        bus_den_at_batch_start,
    };
    f(&mut bus)
}

// Profiling probe for the RAM page lookup. Not a correctness test; run with:
// cargo test --release -p izarravm-machine ram_lookup_profile -- --ignored --nocapture
// Program channel 0 as a keyed sine tone through the given OPL address/data
// port pair (so the same routine can drive the native and aliased ports).
fn program_tone(bus: &mut MachineBus, addr: u16, data: u16) {
    let mut write = |reg: u8, value: u8| {
        bus.write_io(addr, BusWidth::Byte, u32::from(reg), false)
            .unwrap();
        bus.write_io(data, BusWidth::Byte, u32::from(value), false)
            .unwrap();
    };
    write(0x20, 0x01); // modulator: multiple x1
    write(0x40, 0x3f); // modulator muted
    write(0x60, 0xf0); // modulator instant attack
    write(0x80, 0x00);
    write(0x23, 0x21); // carrier: sustained, multiple x1
    write(0x43, 0x00); // carrier loud
    write(0x63, 0xf0); // carrier instant attack
    write(0x83, 0x00);
    write(0xc0, 0x01); // additive
    write(0xa0, 0x00); // f-number low
    write(0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
}

fn boot_image_with(code: &[u8]) -> Vec<u8> {
    let mut image = vec![0; BOOT_IMAGE_SIZE];
    image[..code.len()].copy_from_slice(code);
    image[510] = 0x55;
    image[511] = 0xaa;
    image
}

// SP-4b M0 Task 2 (increment 1): the standalone V86 spike boots, enters V86 via
// the real-mode -> PM+paging -> IRETD-into-V86 transition, and the V86 stub signals
// exit code 0xA5 through the unit-tester port. Proves the transition in isolation.
// Throughput probe for the run-loop batching (item 2.3). Not a correctness
// test; run with: cargo test --release -- --ignored --nocapture batch_throughput
/// Build a CD image with one data sector and a stretch of loud audio frames,
/// for the CD-audio mixing test.
fn audio_cd(frames: u32) -> CdImage {
    let cue = "TRACK 01 MODE1/2048\nINDEX 01 00:00:00\n\
                   TRACK 02 AUDIO\nINDEX 01 00:00:01\n";
    let mut bin = vec![0u8; cdimage::DATA_SECTOR + frames as usize * cdimage::RAW_SECTOR];
    // Fill the audio region with a loud constant so the mix is clearly nonzero.
    for chunk in bin[cdimage::DATA_SECTOR..].chunks_exact_mut(2) {
        chunk.copy_from_slice(&8000i16.to_le_bytes());
    }
    CdImage::from_cue(cue, bin).unwrap()
}

fn iso_dir_record(lba: u32, len: u32, flags: u8, name: &[u8]) -> Vec<u8> {
    let pad = usize::from(name.len() % 2 == 0);
    let mut record = vec![0u8; 33 + name.len() + pad];
    record[0] = record.len() as u8;
    record[2..6].copy_from_slice(&lba.to_le_bytes());
    record[6..10].copy_from_slice(&lba.to_be_bytes());
    record[10..14].copy_from_slice(&len.to_le_bytes());
    record[14..18].copy_from_slice(&len.to_be_bytes());
    record[18..25].copy_from_slice(&[126, 1, 1, 0, 0, 0, 0]);
    record[25] = flags;
    record[28..30].copy_from_slice(&1u16.to_le_bytes());
    record[30..32].copy_from_slice(&1u16.to_be_bytes());
    record[32] = name.len() as u8;
    record[33..33 + name.len()].copy_from_slice(name);
    record
}

// Write/read a 32-bit Margo register through the MMIO aperture.
fn write_mmio_reg(machine: &mut Machine, offset: u32, value: u32) {
    for (i, b) in value.to_le_bytes().into_iter().enumerate() {
        machine.write_physical_u8(MARGO_MMIO_BASE + offset + i as u32, b);
    }
}

fn read_mmio_reg(machine: &mut Machine, offset: u32) -> u32 {
    let mut value = 0u32;
    for i in 0..4 {
        value |= u32::from(machine.read_physical_u8(MARGO_MMIO_BASE + offset + i)) << (8 * i);
    }
    value
}

// Regression: the HLE BIOS INT 10h graphics services mutate `self.video` directly
// (bypassing the CPU bus), so the content generation must live inside the Vga
// mutators, not on the bus, or a BIOS-drawing program would be frozen by the cache.
// Each sub-case stays in an ALREADY-established graphics mode (same dims before and
// after the BIOS call) so the dims fold cannot mask a missing bump.
// The EXEC integration fixtures are nasm-assembled .COM programs (nasm 3.01,
// -f bin, org 0x100). Their source is in the comment above each const so the
// bytes are auditable without re-running the assembler.
const PMIRQ5_COM: &[u8] = include_bytes!("../tests/fixtures/pmirq5.com");
const VCPIPIC_COM: &[u8] = include_bytes!("../tests/fixtures/vcpipic.com");

// --- BLASTER environment seeding ---

/// Walk the env block at `seg` back into (KEY, VALUE) pairs, the way a DOS
/// game scans the segment named by PSP:0x2C.
fn parse_env_block(machine: &Machine, seg: u16) -> Vec<(String, String)> {
    let mem = machine.memory();
    let base = usize::from(seg) * 16;
    let mut entries = Vec::new();
    let mut offset = 0usize;
    loop {
        let mut bytes = Vec::new();
        loop {
            let byte = mem.read_u8(base + offset).unwrap();
            offset += 1;
            if byte == 0 {
                break;
            }
            bytes.push(byte);
        }
        if bytes.is_empty() {
            break; // the terminating empty string
        }
        let entry = String::from_utf8(bytes).unwrap();
        let (key, value) = entry.split_once('=').expect("KEY=VALUE");
        entries.push((key.to_string(), value.to_string()));
    }
    entries
}

/// The env-segment pointer the loader wrote into PSP:0x2C, or 0 if unset.
fn psp_env_segment(machine: &Machine) -> u16 {
    machine
        .memory()
        .read_u16(usize::from(DOS_LOAD_SEGMENT) * 16 + 0x2c)
        .unwrap()
}

// --- Izarra 3000 BIOS foundation ---------------------------------------

// Boot the BIOS with the given CMOS 0x11 code-page index to its idle loop, then
// return `rows` font bytes for `glyph` from the VGA character generator (table 0).
// Mirrors the boot-to-idle pattern from izarra_kbd_layouts.rs.
fn boot_and_read_font_rows(cmos_codepage: u8, glyph: u8, rows: usize) -> Vec<u8> {
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.set_cmos_byte(0x13, cmos_codepage);
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    (0..rows)
        .map(|r| machine.video().active_font_glyph_row(glyph, r))
        .collect()
}

#[cfg(test)]
#[path = "machine_audio_test.rs"]
mod audio;
#[cfg(test)]
#[path = "machine_bios_clock_test.rs"]
mod bios_clock;
#[cfg(test)]
#[path = "machine_bios_services_test.rs"]
mod bios_services;
#[cfg(test)]
#[path = "machine_bus_timing_test.rs"]
mod bus_timing;
#[cfg(test)]
#[path = "machine_core_test.rs"]
mod core;
#[cfg(test)]
#[path = "machine_device_integration_test.rs"]
mod device_integration;
#[cfg(test)]
#[path = "machine_firmware_video_test.rs"]
mod firmware_video;
#[cfg(test)]
#[path = "machine_guest_boot_test.rs"]
mod guest_boot;
#[cfg(test)]
#[path = "machine_keyboard_test.rs"]
mod keyboard;
#[cfg(test)]
#[path = "machine_legacy_video_test.rs"]
mod legacy_video;
#[cfg(test)]
#[path = "machine_margo_test.rs"]
mod margo;
#[cfg(test)]
#[path = "machine_midi_test.rs"]
mod midi;
#[cfg(test)]
#[path = "machine_mouse_test.rs"]
mod mouse;
#[cfg(test)]
#[path = "machine_storage_test.rs"]
mod storage;
#[cfg(test)]
#[path = "machine_video_services_test.rs"]
mod video_services;
