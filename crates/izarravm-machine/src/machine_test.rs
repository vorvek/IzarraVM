// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_core::{SbDma8, SbDma16, SbIrq};
use izarravm_firmware::I386DX25_TEST_ROM;
use izarravm_video::{VGA_MODE13H_BASE, VGA_MONO_TEXT_BASE, VGA_TEXT_BASE};
// Re-exported cache test helpers.
use super::cache_config::{CACHE_LINE_BYTES, CACHE_TIER_DISABLED_MASK, cache_geometry};

const BIOS_TEXT_WHITE: u8 = 0x3F;

/// The half of the scoped VGA-aperture invalidation that lives on this side of the seam, and the
/// one most likely to be reverted by accident because nothing else observes it locally: a
/// direct-write-token move must NOT advance the global direct-mapping epoch.
///
/// The epoch is the "every cached host pointer is void" signal. A token move voids exactly the
/// `0xA0000..0xAFFFF` aperture, and `CpuGsw::note_direct_data_map_changed` invalidates that range
/// by hand — but it can only keep the surviving RAM entries if the epoch they are stamped with
/// still matches. Advance the epoch here and the scoping is silently undone: every RAM entry stops
/// matching on the next interpreter probe and the direct-page caches empty on their next insert.
#[test]
fn a_direct_write_token_move_leaves_the_direct_mapping_epoch_alone() {
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &[0xCD, 0x20])
            .unwrap();
    assert!(machine.set_vga_mode(0x13));
    assert_eq!(machine.vega.direct_write_token(), 1);
    let epoch = machine.direct_mapping_epoch;
    machine.direct_data_map_changed = false;

    // Sequencer index 4 (memory mode), clearing the chain-4 bit: this is the one write doom makes
    // when it leaves chained Mode 13h, and it drops the token off 1.
    with_bus(&mut machine, |bus| {
        bus.write_io(0x3C4, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x3C5, BusWidth::Byte, 0x06, false).unwrap();
    });

    assert_eq!(machine.vega.direct_write_token(), 0);
    assert!(machine.direct_data_map_changed);
    assert_eq!(machine.direct_mapping_epoch, epoch);

    // The COARSE cause still advances it, so the two have not been conflated in the other
    // direction: a real RAM re-decode must still void every cached pointer.
    assert!(machine.set_vga_mode(0x0D));
    assert_ne!(machine.direct_mapping_epoch, epoch);
}

#[test]
fn jit_auto_admission_policy_defaults_on_only_when_available() {
    assert!(run::jit_auto_admit_policy(
        None,
        true,
        ExecutionBackend::Automatic
    ));
    assert!(run::jit_auto_admit_policy(
        Some("1"),
        true,
        ExecutionBackend::Automatic
    ));
    assert!(run::jit_auto_admit_policy(
        Some("yes"),
        true,
        ExecutionBackend::Automatic
    ));
    assert!(!run::jit_auto_admit_policy(
        Some("0"),
        true,
        ExecutionBackend::Automatic
    ));
    assert!(!run::jit_auto_admit_policy(
        Some(""),
        true,
        ExecutionBackend::Automatic
    ));
    assert!(!run::jit_auto_admit_policy(
        None,
        false,
        ExecutionBackend::Automatic
    ));
    assert!(!run::jit_auto_admit_policy(
        Some("1"),
        false,
        ExecutionBackend::Automatic
    ));
    assert!(!run::jit_auto_admit_policy(
        Some("1"),
        true,
        ExecutionBackend::Interpreter
    ));
}

#[cfg(feature = "jit")]
#[test]
fn poll_skip_policy_is_default_on_and_interpreter_only() {
    // Default on for the interpreter; "0" or empty disables; never on elsewhere.
    assert!(run::poll_skip_policy(None, ExecutionBackend::Interpreter));
    assert!(run::poll_skip_policy(
        Some("1"),
        ExecutionBackend::Interpreter
    ));
    assert!(run::poll_skip_policy(
        Some("yes"),
        ExecutionBackend::Interpreter
    ));
    assert!(!run::poll_skip_policy(
        Some("0"),
        ExecutionBackend::Interpreter
    ));
    assert!(!run::poll_skip_policy(
        Some(""),
        ExecutionBackend::Interpreter
    ));
    assert!(!run::poll_skip_policy(None, ExecutionBackend::Automatic));
    assert!(!run::poll_skip_policy(
        Some("1"),
        ExecutionBackend::Automatic
    ));
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
        MachineProfile::gsw_386(16, VideoCard::Vega),
        I386DX25_TEST_ROM,
    )
    .unwrap();
    machine.set_bus_trace_detailed(true);
    machine
}

fn int15_machine(mem_mib: u16) -> Machine {
    Machine::new(
        MachineProfile::gsw_386(mem_mib, VideoCard::Vega),
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
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &PROG).unwrap();
    machine.write_physical_u8(0x04b0, layout);
    machine.inject_key_scancodes(scancodes);
    machine.run_until_halt_or_cycles(3_000_000).unwrap();
    read_u16(&mut machine, (u32::from(DOS_LOAD_SEGMENT) << 4) + 0x200)
}

fn idle_boot_floppy_image() -> Vec<u8> {
    let mut image = vec![0u8; 1_474_560];
    image[..3].copy_from_slice(&[0xfb, 0xeb, 0xfd]);
    image[510] = 0x55;
    image[511] = 0xaa;
    image
}

fn int16_read_after(scancodes: &[u8]) -> u16 {
    int16_read_after_with_layout(0, scancodes)
}

fn int16_peek_guest_exit(scancodes: &[u8], prog: &[u8]) -> StopReason {
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), prog).unwrap();
    machine.inject_key_scancodes(scancodes);
    if !scancodes.is_empty() {
        let deadline = machine.keyboard.ticks_until_event().unwrap();
        machine.advance_devices_ticks(deadline);
    }
    machine.run_until_halt_or_cycles(1_000_000).unwrap()
}

/// Same path as `int16_read_after`, but the program reads with AH=10h (the
/// enhanced read). Before the DOS keyboard ROM aliased AH=10h to the AH=00h
/// reader, this fell through the int16 dispatch and returned stale AX.
fn int16_enhanced_read_after(scancodes: &[u8]) -> u16 {
    // mov ah,0x10; int 16h; mov [0x200],ax; int 20h
    const PROG: [u8; 9] = [0xB4, 0x10, 0xCD, 0x16, 0xA3, 0x00, 0x02, 0xCD, 0x20];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &PROG).unwrap();
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
    // Captured before the struct literal below since VEGA and trace are also
    // mutably borrowed by other fields in that same literal.
    // `scanout_beam_dots`, not `vega.beam_dots()`: this helper is a hand copy of
    // `Machine::make_bus`, and a copy that captured the legacy raster's beam
    // would silently hand every Margo-mode test a beam from the wrong clock.
    let beam_at_batch_start = machine.scanout_beam_dots();
    let margo_scanout_at_batch_start = machine.vega.margo_scanout().is_some();
    let trace_elapsed_at_batch_start = machine.trace.elapsed_clocks();
    let (bus_num_at_batch_start, bus_den_at_batch_start) = bus_timing(machine.cpu.level());
    let icache_fetch_clocks = u64::from(izarravm_bus::BusCycle::clocks_for(
        BusWidth::Byte,
        machine.cache_model.code_fetch_wait_states(),
    ));
    let a20_open = machine.keyboard.a20_enabled();
    let device_free_extended_floor = machine.vega.device_free_extended_floor();
    let mut bus = MachineBus {
        memory: &mut machine.memory,
        ram_lookup: &mut machine.ram_lookup,
        vega: &mut machine.vega,
        pci: &mut machine.pci,
        rom: &machine.rom,
        serial: &mut machine.serial,
        serial2: &mut machine.serial2,
        lpt: &mut machine.lpt,
        lpt2: &mut machine.lpt2,
        device_ports: &mut machine.device_ports,
        open_bus: &mut machine.open_bus,
        pic: &mut machine.pic,
        pit: &mut machine.pit,
        keyboard: &mut machine.keyboard,
        gameport: &mut machine.gameport,
        speaker: &mut machine.speaker,
        rtc: &mut machine.rtc,
        dma: &mut machine.dma,
        fdc: &mut machine.fdc,
        opl: &mut machine.opl,
        sb16: &mut machine.sb16,
        wavetable_mpu: &mut machine.wavetable_mpu,
        midi_mpu: &mut machine.midi_mpu,
        wss: &mut machine.wss,
        wss_base: machine.wss_base,
        wss_enabled: machine.wss_enabled,
        ide: &mut machine.ide,
        ata: &mut machine.ata,
        bmide: &mut machine.bmide,
        trace: &mut machine.trace,
        pending_soft_int: &mut machine.pending_soft_int,
        pending_bios32: &mut machine.pending_bios32,
        last_int_vector: &mut machine.last_int_vector,
        active_mode: machine.active_mode,
        pending_mode: &mut machine.pending_mode,
        fast_post: machine.fast_post,
        booter_inert: machine.booter_inert,
        program_runtime: machine.program_runtime,
        pending_toka_service: &mut machine.pending_toka_service,
        toka_service_status: machine.toka_service_status,
        pending_cd_doorbell: &mut machine.pending_cd_doorbell,
        cd_doorbell_status: &mut machine.cd_doorbell_status,
        cd_redirector_armed: machine.cd_redirector_dos_ds.is_some(),
        unittester: &mut machine.unittester,
        wait_states: machine.profile.wait_states,
        cache: &mut machine.cache_model,
        icache_fetch_clocks,
        flat_data_cost: machine.active_mode.uses_approximate_timing(),
        a20_open,
        device_free_extended_floor,
        extended_ram_screen: crate::bus::extended_ram_screen_enabled(),
        lazy_port_reads: machine.active_mode.uses_approximate_timing(),
        isa_io_wait: crate::bus::isa_io_wait_armed(),
        timing_epoch: machine.timing_epoch,
        poll_skip_certificate: &machine.poll_skip_certificate,
        retrace_poll: &mut machine.retrace_poll,
        lazy_ports_386: crate::bus::lazy_ports_386_for(machine.active_mode),
        io_touched: &mut machine.io_touched,
        exempt_io_touched: &mut machine.exempt_io_touched,
        ata_poll_skip_enabled: machine.ata_poll_skip_enabled,
        ata_poll_skip_armed: &mut machine.ata_poll_skip_armed,
        ata_poll_skip_slice_too_short: machine.ata_poll_skip_slice_too_short,
        ata_poll_skip: &mut machine.ata_poll_skip,
        isa_io_clocks: &mut machine.port_bus_batch_clocks,
        port_accesses_by_class: &mut machine.port_accesses_by_class,
        pit_observer_fine_until: &mut machine.pit_observer_fine_until,
        opl_probe: &mut machine.opl_probe,
        shadow_l1: &mut machine.shadow_l1,
        device_wrote_memory: &mut machine.device_wrote_memory,
        pending_device_memory_write_range: &mut machine.pending_device_memory_write_range,
        direct_map_changed: &mut machine.direct_map_changed,
        direct_data_map_changed: &mut machine.direct_data_map_changed,
        aperture_content_changed: &mut machine.aperture_content_changed,
        direct_mapping_epoch: &mut machine.direct_mapping_epoch,
        vga_wipe_census: &mut machine.vga_wipe_census,
        core_clocks_so_far: 0,
        prior_runs_core_clocks: 0,
        timeline_at_batch_start: machine.timeline,
        master_ticks_at_batch_start: machine.timeline.now_ticks(),
        beam_at_batch_start,
        margo_scanout_at_batch_start,
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

// The standalone V86 spike boots, enters V86 via
// the real-mode -> PM+paging -> IRETD-into-V86 transition, and the V86 stub signals
// exit code 0xA5 through the unit-tester port. Proves the transition in isolation.
// Throughput probe for run-loop batching. Not a correctness
// test; run with: cargo test --release -- --ignored --nocapture batch_throughput
/// Build a CD image with one data sector and a stretch of loud audio frames,
/// for the CD-audio mixing test.
fn audio_cd(frames: u32) -> CdImage {
    let cue = "TRACK 01 MODE1/2048\nINDEX 01 00:00:00\n\
                   TRACK 02 AUDIO\nINDEX 01 00:00:01\n";
    let mut bin = vec![0u8; cdimage::DATA_SECTOR + frames as usize * cdimage::RAW_SECTOR];
    // Fill the audio region with signed stereo constants so channel scaling is
    // visible in the mix.
    for frame in bin[cdimage::DATA_SECTOR..].as_chunks_mut::<4>().0 {
        frame[..2].copy_from_slice(&8000i16.to_le_bytes());
        frame[2..].copy_from_slice(&(-8000i16).to_le_bytes());
    }
    CdImage::from_cue(cue, bin).unwrap()
}

fn iso_dir_record(lba: u32, len: u32, flags: u8, name: &[u8]) -> Vec<u8> {
    let pad = usize::from(name.len().is_multiple_of(2));
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

// Regression: the HLE BIOS INT 10h graphics services mutate the legacy video Adapter
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

// --- Izarra3000 BIOS foundation ---------------------------------------

// Boot the BIOS with the given CMOS 0x11 code-page index to its idle loop, then
// return `rows` font bytes for `glyph` from the VGA character generator (table 0).
// Mirrors the boot-to-idle pattern from izarra_kbd_layouts.rs.
fn boot_and_read_font_rows(cmos_codepage: u8, glyph: u8, rows: usize) -> Vec<u8> {
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.set_cmos_byte(0x13, cmos_codepage);
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    (0..rows)
        .map(|r| machine.video().active_font_glyph_row(glyph, r))
        .collect()
}

#[cfg(test)]
#[path = "machine_ata_dma_test.rs"]
mod ata_dma;
#[cfg(test)]
#[path = "machine_atapi_poll_skip_test.rs"]
mod atapi_poll_skip;
#[cfg(test)]
#[path = "machine_atapi_timing_test.rs"]
mod atapi_timing;
#[cfg(test)]
#[path = "machine_audio_test.rs"]
mod audio;
#[cfg(test)]
#[path = "machine_audio_cmos_test.rs"]
mod audio_cmos;
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
#[path = "machine_deadline_cache_test.rs"]
mod deadline_cache;
#[cfg(test)]
#[path = "machine_device_integration_test.rs"]
mod device_integration;
#[cfg(test)]
#[path = "machine_fdc_dma_test.rs"]
mod fdc_dma;
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
#[path = "machine_native_bus_timing_test.rs"]
mod native_bus_timing;
#[cfg(test)]
#[path = "machine_port_bus_class_test.rs"]
mod port_bus_class;
#[cfg(test)]
#[path = "machine_storage_test.rs"]
mod storage;
#[cfg(test)]
#[path = "machine_timed_io_test.rs"]
mod timed_io;
#[cfg(test)]
#[path = "machine_video_services_test.rs"]
mod video_services;

#[test]
fn margo_caps_match_the_end_to_end_coverage_matrix() {
    let coverage: [(u32, fn()); 12] = [
        (
            1 << 0,
            margo::fill_through_the_mmio_aperture_writes_vram_and_times_busy,
        ),
        (
            1 << 1,
            margo::copy_through_the_mmio_aperture_moves_vram_and_times_busy,
        ),
        (
            1 << 2,
            margo::color_expand_data_through_the_mmio_aperture_draws_a_glyph_and_times_busy,
        ),
        (
            1 << 3,
            margo::line_through_the_mmio_aperture_draws_and_times_busy,
        ),
        (1 << 4, margo::clipped_xor_fill_through_the_mmio_aperture),
        (1 << 5, margo::clipped_xor_fill_through_the_mmio_aperture),
        (
            1 << 6,
            video_services::overlay_color_key_gates_on_the_primary_pixel,
        ),
        (
            1 << 7,
            margo::pattern_fill_through_the_mmio_aperture_tiles_and_times_busy,
        ),
        (
            1 << 8,
            margo::hardware_cursor_composites_through_the_apertures,
        ),
        (
            1 << 9,
            video_services::overlay_yuy2_composites_through_the_apertures,
        ),
        (
            1 << 10,
            video_services::pusher_runs_a_fill_packet_from_the_ring,
        ),
        (
            1 << 11,
            video_services::overlay_orders_dither_on_a_16bpp_display,
        ),
    ];
    let covered = coverage
        .iter()
        .fold(0, |mask, (capability, _test)| mask | capability);

    let mut machine = test_machine();
    assert_eq!(read_mmio_reg(&mut machine, 0x0004), covered);
}

/// A1: `note_code_fetch_linear` is guarded by ONE range test against
/// `FIRMWARE_FETCH_WINDOW`. This proves the guard both ADMITS every address the
/// body reacts to and REJECTS everything else, so the hoist cannot have changed
/// behaviour: the BIOS32 arm arms exactly at its two entry points, the two
/// addresses immediately outside the window arm nothing, and an ordinary
/// conventional-RAM code address (what every interpreted fetch actually passes)
/// arms nothing either.
///
/// The window itself is checked against the four contract addresses at compile
/// time (`firmware_contract::address`); this is the runtime half.
#[test]
fn firmware_fetch_window_admits_exactly_the_bios32_entry_points() {
    let armed = |linear: u32| {
        let mut machine = test_machine();
        machine.pending_bios32 = None;
        with_bus(&mut machine, |bus| bus.note_code_fetch_linear(linear));
        machine.pending_bios32
    };

    assert_eq!(armed(BIOS32_DIRECTORY_LINEAR), Some(Bios32Call::Directory));
    assert_eq!(armed(BIOS32_PCI_LINEAR), Some(Bios32Call::Pci));

    // Inside the window but not an entry point.
    assert_eq!(armed(BIOS32_PCI_LINEAR + 1), None);
    assert_eq!(armed(BIOS_LEGACY_IRET_LINEAR), None);
    // The two addresses that bracket the window, and an ordinary code fetch.
    assert_eq!(armed(FIRMWARE_FETCH_WINDOW_START - 1), None);
    assert_eq!(
        armed(FIRMWARE_FETCH_WINDOW_START + FIRMWARE_FETCH_WINDOW_LEN),
        None
    );
    assert_eq!(armed(0x0002_1234), None);

    // An already-armed call is never overwritten, inside the window or out.
    for linear in [
        BIOS32_DIRECTORY_LINEAR,
        BIOS32_PCI_LINEAR,
        FIRMWARE_FETCH_WINDOW_START - 1,
        0x0002_1234,
    ] {
        let mut machine = test_machine();
        machine.pending_bios32 = Some(Bios32Call::Directory);
        with_bus(&mut machine, |bus| bus.note_code_fetch_linear(linear));
        assert_eq!(machine.pending_bios32, Some(Bios32Call::Directory));
    }
}

/// A change to what physical 0xA0000 ALIASES must not replay a stale decoded instruction.
///
/// The trigger is the planar read map select (GC index 4): in mode 0x0D, a CPU read at
/// A000:0 returns the selected plane's byte, so flipping the register changes what the same
/// physical address CONTAINS without any memory write. The SMC write watch therefore never
/// fires, and the only thing standing between the guest and a stale decode is the decode
/// generation. The program plants different code in plane 1 (marker 0xBB) and plane 0
/// (marker 0xAA) at A000:0, executes plane 0, flips read map select with ONE port write,
/// and executes the same linear address again. The second execution must run plane 1's
/// bytes.
///
/// Plane 1 is planted FIRST, deliberately: after the first far call caches the decode
/// line, the only operation before the second call is the port write, so a pass here
/// cannot be explained by a write-path invalidation.
#[test]
fn aperture_remap_reaches_the_decode_cache() {
    #[rustfmt::skip]
    const PROG: &[u8] = &[
        0xB8, 0x0D, 0x00,             // mov ax,0x000D   planar 320x200x16, aperture A0000
        0xCD, 0x10,                   // int 10h
        0xB8, 0x00, 0xA0,             // mov ax,0xA000
        0x8E, 0xC0,                   // mov es,ax
        0xBA, 0xC4, 0x03,             // mov dx,0x3C4    sequencer
        0xB8, 0x02, 0x02,             // mov ax,0x0202   map mask := plane 1
        0xEF,                         // out dx,ax
        0xBE, 0x5D, 0x01,             // mov si,0x015D   BB template
        0xBF, 0x00, 0x00,             // mov di,0
        0xB9, 0x06, 0x00,             // mov cx,6
        0xF3, 0xA4,                   // rep movsb       plane 1 := marker 0xBB code
        0xB8, 0x02, 0x01,             // mov ax,0x0102   map mask := plane 0
        0xEF,                         // out dx,ax
        0xBE, 0x57, 0x01,             // mov si,0x0157   AA template
        0xBF, 0x00, 0x00,             // mov di,0
        0xB9, 0x06, 0x00,             // mov cx,6
        0xF3, 0xA4,                   // rep movsb       plane 0 := marker 0xAA code
        0xBA, 0xCE, 0x03,             // mov dx,0x3CE    graphics controller
        0xB8, 0x04, 0x00,             // mov ax,0x0004   read map select := plane 0
        0xEF,                         // out dx,ax
        0x9A, 0x00, 0x00, 0x00, 0xA0, // call far 0xA000:0x0000   caches the decode line
        0xA0, 0x10, 0x02,             // mov al,[0x0210]
        0xA2, 0x11, 0x02,             // mov [0x0211],al          save the first marker
        0xB8, 0x04, 0x01,             // mov ax,0x0104   read map select := plane 1 (TRIGGER)
        0xEF,                         // out dx,ax
        0x9A, 0x00, 0x00, 0x00, 0xA0, // call far 0xA000:0x0000   must re-decode
        0xA0, 0x10, 0x02,             // mov al,[0x0210]
        0xA2, 0x12, 0x02,             // mov [0x0212],al          save the second marker
        0xB8, 0x04, 0x00,             // mov ax,0x0004   read map select := plane 0 AGAIN
        0xEF,                         // out dx,ax
        0x9A, 0x00, 0x00, 0x00, 0xA0, // call far 0xA000:0x0000   must re-decode AGAIN
        0xCD, 0x20,                   // int 20h
        // 0x0157: plane-0 payload
        0xC6, 0x06, 0x10, 0x02, 0xAA, // mov byte [0x0210],0xAA
        0xCB,                         // retf
        // 0x015D: plane-1 payload
        0xC6, 0x06, 0x10, 0x02, 0xBB, // mov byte [0x0210],0xBB
        0xCB,                         // retf
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    machine.run_until_halt_or_cycles(5_000_000).unwrap();
    let base = u32::from(DOS_LOAD_SEGMENT) << 4;
    assert_eq!(
        machine.read_physical_u8(base + 0x211),
        0xAA,
        "sanity: the first call must execute plane 0's bytes"
    );
    assert_eq!(
        machine.read_physical_u8(base + 0x212),
        0xBB,
        "the read-map flip changed what A000:0 contains; executing the OLD bytes means a \
         stale decoded instruction was replayed"
    );
    // The third leg is the RE-ARM pin. The first flush clears the aperture flag on its way
    // through the generation bump; if the re-decode of plane 1's bytes did not set it again,
    // flipping BACK to plane 0 would replay the cached plane-1 line and read 0xBB here.
    assert_eq!(
        machine.read_physical_u8(base + 0x210),
        0xAA,
        "remapping BACK must re-decode too: the flag must re-arm on every aperture insert, \
         not only on the first"
    );
}

/// The aperture flush is SELF-LIMITING: one flush per aperture line inserted, not one per
/// VGA register write forever. Two identical runs that differ only in how many times they
/// poke a VGA register after the one aperture execution must end at the SAME decode
/// generation, because the first poke's flush clears the flag and the later pokes find it
/// clear. A never-clearing mutation gives the longer machine extra generation bumps.
#[test]
fn aperture_flush_is_self_limiting() {
    fn run(extra_outs: usize) -> u32 {
        let mut prog = vec![
            0xB8, 0x00, 0xB8, // mov ax,0xB800    text mode 3 aperture, live at boot
            0x8E, 0xC0, // mov es,ax
            0xBE, 0x00, 0x00, // mov si,PAYLOAD (patched below)
            0xBF, 0x00, 0x00, // mov di,0
            0xB9, 0x06, 0x00, // mov cx,6
            0xF3, 0xA4, // rep movsb        plant the payload at B800:0
            0x9A, 0x00, 0x00, 0x00, 0xB8, // call far 0xB800:0    decode from the aperture
            0xBA, 0xCE, 0x03, // mov dx,0x3CE
        ];
        for _ in 0..extra_outs {
            prog.extend_from_slice(&[0xB8, 0x04, 0x00]); // mov ax,0x0004
            prog.push(0xEF); // out dx,ax   an accepted VGA write, batch ends
        }
        prog.extend_from_slice(&[0xCD, 0x20]); // int 20h
        let payload_offset = 0x100 + prog.len() as u16;
        prog.extend_from_slice(&[0xC6, 0x06, 0x10, 0x02, 0xAA, 0xCB]);
        prog[6] = payload_offset as u8;
        prog[7] = (payload_offset >> 8) as u8;
        let mut machine =
            Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &prog).unwrap();
        machine.run_until_halt_or_cycles(5_000_000).unwrap();
        assert_eq!(
            machine.read_physical_u8((u32::from(DOS_LOAD_SEGMENT) << 4) + 0x210),
            0xAA,
            "sanity: the aperture code executed"
        );
        machine.cpu.decode_cache_generation()
    }
    assert_eq!(
        run(1),
        run(3),
        "extra VGA writes after the flush must not keep flushing: the flag failed to clear"
    );
}

/// The CGA and text arms of int10_set_mode_number never move the direct-write identity, so
/// before this fix they reached no decode invalidation at all. With aperture code live, a
/// mode set through the CGA arm must bump the decode generation exactly once relative to an
/// identical run whose far call is replaced by NOPs; the NOP run doubles as the cost pin,
/// proving a guest that never executes from VRAM pays no flush for the same INT 10h.
#[test]
fn a_text_arm_mode_set_reaches_aperture_code() {
    fn run(execute: bool) -> u32 {
        let mut prog = vec![
            0xB8, 0x00, 0xB8, // mov ax,0xB800
            0x8E, 0xC0, // mov es,ax
            0xBE, 0x00, 0x00, // mov si,PAYLOAD (patched below)
            0xBF, 0x00, 0x00, // mov di,0
            0xB9, 0x06, 0x00, // mov cx,6
            0xF3, 0xA4, // rep movsb
        ];
        if execute {
            prog.extend_from_slice(&[0x9A, 0x00, 0x00, 0x00, 0xB8]); // call far 0xB800:0
        } else {
            prog.extend_from_slice(&[0x90, 0x90, 0x90, 0x90, 0x90]); // same length, no decode
        }
        prog.extend_from_slice(&[
            0xB8, 0x84, 0x00, // mov ax,0x0084   CGA mode 4, no clear: the CGA arm
            0xCD, 0x10, // int 10h
            0xCD, 0x20, // int 20h
        ]);
        let payload_offset = 0x100 + prog.len() as u16;
        prog.extend_from_slice(&[0xC6, 0x06, 0x10, 0x02, 0xAA, 0xCB]);
        prog[6] = payload_offset as u8;
        prog[7] = (payload_offset >> 8) as u8;
        let mut machine =
            Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &prog).unwrap();
        machine.run_until_halt_or_cycles(5_000_000).unwrap();
        machine.cpu.decode_cache_generation()
    }
    assert_eq!(
        run(true),
        run(false) + 1,
        "with aperture code live the CGA-arm mode set must flush exactly once; without it, \
         the same INT 10h must flush nothing"
    );
}
