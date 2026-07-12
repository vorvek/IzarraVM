// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn coherence_machine() -> Machine {
    Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &[0xf4]).unwrap()
}

const PATCH_PROGRAM: [u8; 23] = [
    0xb9, 0x64, 0x00, 0x00, 0x00, // mov ecx,100
    0xb8, 0x11, 0x11, 0x11, 0x11, // mov eax,11111111h
    0x83, 0xc0, 0x03, // add eax,3
    0x89, 0xc2, // mov edx,eax
    0x83, 0xe9, 0x01, // sub ecx,1
    0x75, 0xf6, // jnz to add eax,3
    0x89, 0xc3, // mov ebx,eax
    0xf4, // hlt
];
const PATCH_ENTRY: u32 = 0x100;
const PATCH_PHYSICAL: u32 = DOS_LOAD_SEGMENT as u32 * 16 + PATCH_ENTRY;
const PATCH_IMMEDIATE: u32 = PATCH_PHYSICAL + 6;

fn patch_machine() -> Machine {
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &PATCH_PROGRAM)
            .unwrap();
    machine.set_mode(GswMode::Gsw586);
    machine.set_jit_auto_admit(true);
    machine.cpu.set_native_backend_enabled(true);
    machine.trace.set_tracing_mode(TracingMode::Off);
    let mut cs = machine.cpu.registers.cs();
    cs.default_size_32 = true;
    cs.limit = u32::MAX;
    machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
    machine
}

fn execute_patch_program_at(machine: &mut Machine, entry: u32) {
    machine.cpu.halted = false;
    machine.cpu.registers.eip = entry;
    machine.cpu.registers.set_eax(0);
    machine.cpu.registers.set_ebx(0);
    machine.cpu.registers.set_edx(0);
    machine.run_until_halt_or_cycles(100_000).unwrap();
    assert!(machine.cpu.halted, "patch program did not halt");
}

fn execute_patch_program(machine: &mut Machine) {
    execute_patch_program_at(machine, PATCH_ENTRY);
}

fn warm_patch_program_at(machine: &mut Machine, entry: u32) {
    let installed = machine.cpu.perf_counters().jit_direct_blocks_installed;
    for _ in 0..4 {
        execute_patch_program_at(machine, entry);
    }
    assert!(
        machine.cpu.perf_counters().jit_direct_blocks_installed > installed,
        "patch fixture never installed a native block: {:#?}",
        machine.cpu.perf_counters()
    );
    assert!(
        machine.cpu.perf_counters().jit_direct_insns != 0,
        "patch fixture never executed native code"
    );
}

fn warm_patch_program(machine: &mut Machine) {
    warm_patch_program_at(machine, PATCH_ENTRY);
}

#[derive(Clone, Copy, Debug)]
enum PatchWriter {
    PublicByte,
    PublicWord,
    PublicDword,
    HleByte,
    HleWord,
    HleBlock,
    HleBlockSameValue,
}

fn apply_patch_writer(machine: &mut Machine, writer: PatchWriter) -> u32 {
    match writer {
        PatchWriter::PublicByte => {
            machine.write_physical_u8(PATCH_IMMEDIATE, 0x22);
            0x1111_124e
        }
        PatchWriter::PublicWord => {
            machine.write_physical_u16(PATCH_IMMEDIATE, 0x2222);
            0x1111_234e
        }
        PatchWriter::PublicDword => {
            machine.write_physical_u32(PATCH_IMMEDIATE, 0x2222_2222);
            0x2222_234e
        }
        PatchWriter::HleByte => {
            machine
                .write_guest_ram_u8(PATCH_IMMEDIATE as usize, 0x22)
                .unwrap();
            0x1111_124e
        }
        PatchWriter::HleWord => {
            machine
                .write_guest_ram_u16(PATCH_IMMEDIATE as usize, 0x2222)
                .unwrap();
            0x1111_234e
        }
        PatchWriter::HleBlock => {
            machine.write_guest_block(PATCH_IMMEDIATE, &0x2222_2222u32.to_le_bytes());
            0x2222_234e
        }
        PatchWriter::HleBlockSameValue => {
            machine.write_guest_block(PATCH_IMMEDIATE, &0x1111_1111u32.to_le_bytes());
            0x1111_123d
        }
    }
}

#[test]
fn every_machine_ram_writer_retires_native_code_before_reentry() {
    for writer in [
        PatchWriter::PublicByte,
        PatchWriter::PublicWord,
        PatchWriter::PublicDword,
        PatchWriter::HleByte,
        PatchWriter::HleWord,
        PatchWriter::HleBlock,
        PatchWriter::HleBlockSameValue,
    ] {
        let mut machine = patch_machine();
        warm_patch_program(&mut machine);
        machine.cpu.reset_perf_counters();

        let expected = apply_patch_writer(&mut machine, writer);

        assert_eq!(
            machine.cpu.perf_counters().device_write_code_hits,
            1,
            "{writer:?} did not retire overlapping code"
        );
        execute_patch_program(&mut machine);
        assert_eq!(
            machine.cpu.registers.eax(),
            expected,
            "{writer:?} replayed stale native or decoded bytes"
        );
        assert_eq!(machine.cpu.registers.ebx(), expected);
        assert_eq!(machine.cpu.registers.edx(), expected);
        assert_eq!(machine.cpu.registers.ecx(), 0);
    }
}

#[test]
fn precise_host_patch_keeps_an_adjacent_native_block_live() {
    const ADJACENT_ENTRY: u32 = 0x200;
    const ADJACENT_PHYSICAL: usize = (DOS_LOAD_SEGMENT as usize * 16) + ADJACENT_ENTRY as usize;
    let mut machine = patch_machine();
    machine.memory.as_mut_slice()[ADJACENT_PHYSICAL..ADJACENT_PHYSICAL + PATCH_PROGRAM.len()]
        .copy_from_slice(&PATCH_PROGRAM);
    warm_patch_program(&mut machine);
    warm_patch_program_at(&mut machine, ADJACENT_ENTRY);
    machine.cpu.reset_perf_counters();

    machine.write_physical_u32(PATCH_IMMEDIATE, 0x2222_2222);

    assert_eq!(machine.cpu.perf_counters().device_write_code_hits, 1);
    machine.cpu.reset_perf_counters();
    execute_patch_program_at(&mut machine, ADJACENT_ENTRY);
    assert!(
        machine.cpu.perf_counters().jit_direct_insns != 0,
        "an adjacent non-overlapping native block was retired"
    );
    assert_eq!(machine.cpu.registers.eax(), 0x1111_123d);
}

#[test]
fn lotura_font_bank_invalidates_executed_window_bytes() {
    let mut machine = coherence_machine();
    machine.set_mode(GswMode::Gsw586);
    machine.trace.set_tracing_mode(TracingMode::Off);
    machine.memory.as_mut_slice()
        [CODEPAGE_FONT_WINDOW as usize..CODEPAGE_FONT_WINDOW as usize + PATCH_PROGRAM.len()]
        .copy_from_slice(&PATCH_PROGRAM);
    let mut cs = SegmentRegister::real((CODEPAGE_FONT_WINDOW >> 4) as u16);
    cs.default_size_32 = true;
    machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
    execute_patch_program_at(&mut machine, 0);
    machine.cpu.reset_perf_counters();

    {
        let mut bus = machine.make_bus();
        bus.write_io(0x00e7, BusWidth::Byte, 0, false).unwrap();
    }
    assert_eq!(machine.cpu.perf_counters().device_write_code_hits, 0);
    machine.run_cycles(0).unwrap();

    let perf = machine.cpu.perf_counters();
    assert_eq!(perf.device_write_ranges, 1);
    assert_eq!(perf.device_write_bytes, 4096);
    assert_eq!(
        perf.device_write_code_hits, 1,
        "E7 did not invalidate decoded or prefetched window bytes"
    );
}

#[test]
fn direct_guest_scalars_report_same_value_writes_without_bus_time() {
    let mut machine = coherence_machine();
    let address = 0x70000usize;
    let byte = machine.memory.read_u8(address).unwrap();
    let word = machine.memory.read_u16(address + 2).unwrap();
    let clocks = machine.bus_trace().elapsed_clocks();
    machine.cpu.reset_perf_counters();

    machine.write_guest_ram_u8(address, byte).unwrap();
    machine.write_guest_ram_u16(address + 2, word).unwrap();

    let perf = machine.cpu.perf_counters();
    assert_eq!(perf.device_write_ranges, 2);
    assert_eq!(perf.device_write_bytes, 3);
    assert_eq!(perf.device_write_coarse_resets, 0);
    assert_eq!(machine.bus_trace().elapsed_clocks(), clocks);
}

#[test]
fn direct_guest_scalar_prevalidates_the_whole_write() {
    let mut machine = coherence_machine();
    let address = machine.memory.len() - 1;
    machine.memory.write_u8(address, 0x5a).unwrap();
    machine.cpu.reset_perf_counters();

    assert!(machine.write_guest_ram_u16(address, 0xbeef).is_err());

    assert_eq!(machine.memory.read_u8(address).unwrap(), 0x5a);
    let perf = machine.cpu.perf_counters();
    assert_eq!(perf.device_write_ranges, 0);
    assert_eq!(perf.device_write_bytes, 0);
    assert_eq!(perf.device_write_coarse_resets, 0);
}

#[test]
fn guest_block_reports_one_contiguous_range_and_empty_is_a_noop() {
    let mut machine = coherence_machine();
    let address = 0x71000u32;
    let bytes = [0x10, 0x20, 0x30, 0x40, 0x50];
    let clocks = machine.bus_trace().elapsed_clocks();
    machine.cpu.reset_perf_counters();

    machine.write_guest_block(address, &bytes);

    assert_eq!(
        &machine.memory.as_slice()[address as usize..address as usize + bytes.len()],
        &bytes
    );
    let perf = machine.cpu.perf_counters();
    assert_eq!(perf.device_write_ranges, 1);
    assert_eq!(perf.device_write_bytes, bytes.len() as u64);
    assert_eq!(perf.device_write_coarse_resets, 0);
    assert_eq!(machine.bus_trace().elapsed_clocks(), clocks);

    machine.cpu.reset_perf_counters();
    machine.write_guest_block(address, &[]);
    let perf = machine.cpu.perf_counters();
    assert_eq!(perf.device_write_ranges, 0);
    assert_eq!(perf.device_write_coarse_resets, 0);
}

#[test]
fn guest_block_keeps_exact_physical_routing_when_a20_is_closed() {
    let mut machine = coherence_machine();
    machine.keyboard.set_a20(false);
    machine.memory.write_u16(0, 0x2211).unwrap();
    machine.memory.write_u16(0x10_0000, 0x4433).unwrap();
    machine.cpu.reset_perf_counters();

    machine.write_guest_block(0x10_0000, &[0xaa, 0xbb]);

    assert_eq!(machine.memory.read_u16(0).unwrap(), 0x2211);
    assert_eq!(machine.memory.read_u16(0x10_0000).unwrap(), 0xbbaa);
    let perf = machine.cpu.perf_counters();
    assert_eq!(perf.device_write_ranges, 1);
    assert_eq!(perf.device_write_bytes, 2);
}

#[test]
fn public_scalars_preserve_byte_exact_and_wide_a20_routing() {
    let mut machine = coherence_machine();
    machine.keyboard.set_a20(false);
    machine.memory.write_u8(0, 0x11).unwrap();
    machine.memory.write_u8(0x10_0000, 0x22).unwrap();
    machine.cpu.reset_perf_counters();

    machine.write_physical_u8(0x10_0000, 0x33);

    assert_eq!(machine.memory.read_u8(0).unwrap(), 0x11);
    assert_eq!(machine.memory.read_u8(0x10_0000).unwrap(), 0x33);

    machine.write_physical_u16(0x10_0001, 0xbbaa);

    assert_eq!(machine.memory.read_u16(1).unwrap(), 0xbbaa);
    let perf = machine.cpu.perf_counters();
    assert_eq!(perf.device_write_ranges, 2);
    assert_eq!(perf.device_write_bytes, 3);
    assert_eq!(perf.device_write_coarse_resets, 0);
}

#[test]
fn public_wide_a20_split_reports_the_actual_disjoint_ram_spans() {
    let mut machine = coherence_machine();
    machine.keyboard.set_a20(false);
    machine.cpu.reset_perf_counters();

    machine.write_physical_u32(0x002f_fffe, 0x4433_2211);

    assert_eq!(machine.memory.read_u16(0x002f_fffe).unwrap(), 0x2211);
    assert_eq!(machine.memory.read_u16(0x0020_0000).unwrap(), 0x4433);
    let perf = machine.cpu.perf_counters();
    assert_eq!(perf.device_write_ranges, 2);
    assert_eq!(perf.device_write_bytes, 4);

    machine.cpu.reset_perf_counters();
    machine.write_physical_u32(0x000f_fffe, 0x8877_6655);
    assert_eq!(machine.memory.read_u16(0).unwrap(), 0x8877);
    let perf = machine.cpu.perf_counters();
    assert_eq!(perf.device_write_ranges, 1);
    assert_eq!(perf.device_write_bytes, 2);
}

#[test]
fn public_byte_writes_publish_only_when_the_live_route_accepts_ram() {
    let mut machine = coherence_machine();
    machine.cpu.reset_perf_counters();

    // VGA memory is disabled in the raw-program reset state, so this address reaches the
    // historical fallback RAM route and must be reported as RAM even though it is in an adapter
    // aperture.
    machine.write_physical_u8(0x000a_0000, 0x12);
    assert_eq!(machine.memory.read_u8(0x000a_0000).unwrap(), 0x12);
    let perf = machine.cpu.perf_counters();
    assert_eq!(perf.device_write_ranges, 1);
    assert_eq!(perf.device_write_bytes, 1);

    machine.cpu.reset_perf_counters();
    machine.write_physical_u8(0x000d_0000, 0x34);
    machine.write_physical_u8(0x000f_0000, 0x56);

    let perf = machine.cpu.perf_counters();
    assert_eq!(perf.device_write_ranges, 0);
    assert_eq!(perf.device_write_bytes, 0);
    assert_eq!(perf.device_write_coarse_resets, 0);
}

#[test]
fn live_vga_wide_writes_do_not_publish_system_ram() {
    let mut machine = coherence_machine();
    assert!(machine.set_vga_mode(0x13));
    machine.cpu.reset_perf_counters();

    machine.write_physical_u32(izarravm_video::VGA_MODE13H_BASE, 0x4433_2211);

    let perf = machine.cpu.perf_counters();
    assert_eq!(perf.device_write_ranges, 0);
    assert_eq!(perf.device_write_bytes, 0);
    assert_eq!(perf.device_write_coarse_resets, 0);
    assert_eq!(
        machine.read_physical_u8(izarravm_video::VGA_MODE13H_BASE),
        0x11
    );
}

#[test]
fn public_wide_write_keeps_the_canonical_bus_trace() {
    const ADDRESS: u32 = 0x72000;
    const VALUE: u32 = 0x4433_2211;
    let mut public = coherence_machine();
    let mut canonical = coherence_machine();
    public.trace = BusTrace::default();
    canonical.trace = BusTrace::default();
    public.trace.set_tracing_mode(TracingMode::Full);
    canonical.trace.set_tracing_mode(TracingMode::Full);

    public.write_physical_u32(ADDRESS, VALUE);
    {
        let mut bus = canonical.make_bus();
        bus.write_memory(ADDRESS, BusWidth::Dword, VALUE, BusAccessKind::DataWrite)
            .unwrap();
    }

    assert_eq!(public.trace, canonical.trace);
    assert_eq!(
        public.memory.read_u32(ADDRESS as usize).unwrap(),
        canonical.memory.read_u32(ADDRESS as usize).unwrap()
    );
}

#[test]
fn lotura_font_bank_writes_immediately_and_drains_one_exact_range() {
    let mut machine = coherence_machine();
    machine.set_mode(GswMode::Gsw586);
    machine.cpu.reset_perf_counters();
    machine.io_touched = false;

    {
        let mut bus = machine.make_bus();
        bus.write_io(0x00e7, BusWidth::Byte, 0, true).unwrap();
    }

    assert!(
        machine.io_touched,
        "ring-0 E7 writes must end the CPU batch"
    );
    assert_eq!(
        machine.pending_device_memory_write_range,
        Some((CODEPAGE_FONT_WINDOW, 4096))
    );
    assert_eq!(
        &machine.memory.as_slice()
            [CODEPAGE_FONT_WINDOW as usize..CODEPAGE_FONT_WINDOW as usize + 4096],
        &izarravm_firmware::CODEPAGE_FONTS[..4096]
    );
    assert_eq!(machine.cpu.perf_counters().device_write_ranges, 0);

    machine.io_touched = false;
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x00e7, BusWidth::Byte, 2, true).unwrap();
    }
    assert_eq!(
        machine.pending_device_memory_write_range,
        Some((CODEPAGE_FONT_WINDOW, 4096)),
        "a shorter repeated bank must not shrink the pending union"
    );
    assert_eq!(
        &machine.memory.as_slice()
            [CODEPAGE_FONT_WINDOW as usize..CODEPAGE_FONT_WINDOW as usize + 2048],
        &izarravm_firmware::CODEPAGE_FONTS[7680..9728]
    );

    machine.io_touched = false;
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x00e7, BusWidth::Byte, 15, true).unwrap();
    }
    assert!(machine.io_touched);
    assert_eq!(
        machine.pending_device_memory_write_range,
        Some((CODEPAGE_FONT_WINDOW, 4096)),
        "an invalid selector must preserve an earlier valid write"
    );

    machine.run_cycles(0).unwrap();
    assert_eq!(machine.pending_device_memory_write_range, None);
    let perf = machine.cpu.perf_counters();
    assert_eq!(perf.device_write_ranges, 1);
    assert_eq!(perf.device_write_bytes, 4096);
    assert_eq!(perf.device_write_coarse_resets, 0);

    machine.cpu.reset_perf_counters();
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x00e7, BusWidth::Byte, 2, true).unwrap();
    }
    machine.run_cycles(0).unwrap();
    let perf = machine.cpu.perf_counters();
    assert_eq!(
        perf.device_write_ranges, 1,
        "same-value banks still publish"
    );
    assert_eq!(perf.device_write_bytes, 2048);
}

#[test]
fn lotura_font_bank_reports_only_the_successful_ram_prefix() {
    for accepted in [0usize, 1, 2048, 4096] {
        let mut machine = coherence_machine();
        machine.memory = Memory::new(CODEPAGE_FONT_WINDOW as usize + accepted).unwrap();
        machine.cpu.reset_perf_counters();

        {
            let mut bus = machine.make_bus();
            bus.write_io(0x00e7, BusWidth::Byte, 0, false).unwrap();
        }

        let expected = u32::try_from(accepted).unwrap();
        assert_eq!(
            machine.pending_device_memory_write_range,
            (expected != 0).then_some((CODEPAGE_FONT_WINDOW, expected))
        );
        machine.run_cycles(0).unwrap();
        let perf = machine.cpu.perf_counters();
        assert_eq!(perf.device_write_ranges, u64::from(expected != 0));
        assert_eq!(perf.device_write_bytes, u64::from(expected));
    }
}

#[test]
fn guest_font_bank_out_ends_before_the_following_halt() {
    let mut machine = Machine::new_raw_program(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        &[0xb0, 0x02, 0xe6, 0xe7, 0xf4],
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw586);
    machine.cpu.reset_perf_counters();
    machine.test_batch_core_totals.clear();

    machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert!(
        machine.test_batch_core_totals.len() >= 2,
        "OUT E7 and the following HLT must execute in different CPU batches"
    );
    assert_eq!(machine.pending_device_memory_write_range, None);
    let perf = machine.cpu.perf_counters();
    assert_eq!(perf.device_write_ranges, 1);
    assert_eq!(perf.device_write_bytes, 2048);
    assert_eq!(perf.device_write_coarse_resets, 0);
}

#[test]
fn runtime_hle_modules_do_not_write_memory_behind_cpu_coherence() {
    fn compact(source: &str) -> String {
        source.chars().filter(|ch| !ch.is_whitespace()).collect()
    }

    const RAW_MUTATIONS: [&str; 6] = [
        "self.memory.write_u8(",
        "self.memory.write_u16(",
        "self.memory.write_u32(",
        "self.memory.as_mut_slice(",
        "self.memory.as_mut_ptr(",
        "&mutself.memory",
    ];

    for (name, source) in [
        ("bios.rs", include_str!("bios.rs")),
        ("storage.rs", include_str!("storage.rs")),
        ("video.rs", include_str!("video.rs")),
    ] {
        let source = compact(source);
        for needle in RAW_MUTATIONS {
            assert!(
                !source.contains(needle),
                "{name} contains a direct runtime guest-memory mutation: {needle}"
            );
        }
    }

    let dos = include_str!("dos.rs");
    assert_eq!(
        dos.matches("install_keyboard_bios").count(),
        2,
        "constructor-only keyboard installation gained another call site"
    );
    let install_start = dos
        .find("fn install_keyboard_bios")
        .expect("DOS constructor keyboard install exists");
    let install_end = dos[install_start..]
        .find("\n    fn ")
        .map_or(dos.len(), |offset| install_start + offset);
    let runtime = compact(&format!("{}{}", &dos[..install_start], &dos[install_end..]));
    for needle in RAW_MUTATIONS {
        assert!(
            !runtime.contains(needle),
            "dos.rs contains a direct runtime guest-memory mutation: {needle}"
        );
    }
    let install = compact(&dos[install_start..install_end]);
    for needle in [
        "self.memory.as_mut_slice(",
        "self.memory.as_mut_ptr(",
        "&mutself.memory",
    ] {
        assert!(
            !install.contains(needle),
            "DOS constructor contains an unexpected raw memory escape: {needle}"
        );
    }
    assert_eq!(install.matches("self.memory.write_u8(").count(), 1);
    assert_eq!(install.matches("self.memory.write_u16(").count(), 6);
    assert_eq!(install.matches("self.memory.write_u32(").count(), 0);

    let library = compact(include_str!("lib.rs"));
    for (needle, expected) in [
        ("self.memory.write_u8(", 1),
        ("self.memory.write_u16(", 1),
        ("self.memory.write_u32(", 0),
        ("self.memory.as_mut_slice(", 0),
        ("self.memory.as_mut_ptr(", 0),
        ("&mutself.memory", 2),
    ] {
        assert_eq!(
            library.matches(needle).count(),
            expected,
            "lib.rs changed its exact coherent-helper or DMA-read allowlist for {needle}"
        );
    }
}
