// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_bus::{
    BusCycle, BusTrace, BusWidth, CompiledBusDelta, CompiledBusWindow, DirectPage, TracingMode,
};
use izarravm_bus::{DirectMemoryRead, DirectMemoryWrite};

/// The JIT's emitted native code addresses `gpr[i]` as `[regs_ptr + 4*i]`, relying on
/// `Registers` being `repr(C)` with `gpr` as the first field. A rustc or field reorder that broke
/// this offset would silently corrupt guest state through wrong native loads/stores, so this
/// test freezes the layout assumption the JIT bakes into its emitted bytes. The eip offset is
/// asserted too (the dispatch reads it); eflags follows eip at +4.
#[test]
fn registers_repr_c_offsets_are_stable() {
    // gpr is the first field of repr(C) Registers: offset 0, 4-byte element stride.
    assert_eq!(core::mem::offset_of!(Registers, gpr), 0);
    assert_eq!(core::mem::size_of::<u32>(), 4);
    // eip sits after gpr (32 bytes) + segments ([SegmentRegister; 6]). repr(C) guarantees this
    // declaration order is the memory order; eflags immediately follows eip at +4.
    let eip_off = core::mem::offset_of!(Registers, eip);
    assert_eq!(eip_off, 32 + core::mem::size_of::<[SegmentRegister; 6]>());
    assert_eq!(core::mem::offset_of!(Registers, eflags), eip_off + 4);
}

/// The paged JIT probe bakes this entry stride and these field offsets into native code.
#[test]
#[cfg(feature = "jit")]
fn tlb_entry_repr_c_offsets_are_stable() {
    assert_eq!(core::mem::size_of::<TlbEntry>(), 16);
    assert_eq!(core::mem::offset_of!(TlbEntry, tag), 0);
    assert_eq!(core::mem::offset_of!(TlbEntry, phys), 4);
    assert_eq!(core::mem::offset_of!(TlbEntry, generation), 8);
    assert_eq!(core::mem::offset_of!(TlbEntry, writable), 12);
    assert_eq!(core::mem::offset_of!(TlbEntry, user), 13);
    assert_eq!(core::mem::offset_of!(TlbEntry, dirty), 14);
}

/// The Direct backend's emitter bakes `offset_of!(CpuGsw, registers)` into its emitted bytes (the
/// prologue computes `regs_ptr = cpu_ptr + regs_offset`, and inline slots address gpr as
/// `[regs_ptr + 4*i]`). CpuGsw is NOT repr(C), so rustc is free to reorder its fields; this
/// test pins the current offset so a rustc version bump that moved `registers` is caught here
/// (the emitter reads the offset at emit time, so a changed value still produces correct code,
/// but the assertion documents the layout and guards against a silent perf shift from a
/// changed cache-line placement of gpr).
#[test]
#[cfg(feature = "jit")]
fn cpu_registers_field_offset_is_stable() {
    let off = core::mem::offset_of!(CpuGsw, registers);
    // The current layout places `registers` at a non-zero offset (rustc reorders CpuGsw's
    // fields for alignment). The emitter handles any value (it bakes `offset_of!` at emit
    // time, verified by the differential suite jit_general); this assertion freezes the known
    // position so a change is visible. The constant shifts whenever PerfCounters or another
    // preceding field grows or shrinks; the dynarec-refactor Task 2 region-JIT deletion removed
    // four PerfCounters fields (32 bytes) and the `jit_regions: jit::RegionTable` field from
    // `CpuGsw` itself, moving this pin from 504 to 464 (measured via a failing-test readout, not
    // derived: rustc's field reordering does not guarantee the naive byte-count shift). The
    // emitter re-reads the offset, so updating this number is a documentation change, not a code
    // fix. The decode-line first-touch slice adds the packed side array's `Box` to `DecodeCache`,
    // which sits ahead of `registers`, moving this pin from 464 to 480 -- measured, not derived.
    assert_eq!(
        off, 480,
        "CpuGsw.registers offset moved; update the emitter's baked offset"
    );
}

#[test]
fn perf_counter_tracks_code_invalidation_events() {
    let mut cpu = CpuGsw::default();
    let before = cpu.perf_counters().code_invalidations;

    cpu.note_a20_changed();

    assert_eq!(cpu.perf_counters().code_invalidations, before + 1);
}

#[test]
fn structural_code_invalidation_clears_stale_native_watch_marks() {
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0x100;
    let mut memory = vec![0; 0x1000];
    memory[0x100] = 0x90;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();
    let page = 0x100u32 >> 12;
    assert!(cpu.decode_cache.range_hits_code(0x100, 1));
    assert_ne!(
        cpu.decode_cache.code_pages[(page >> 6) as usize] & (1u64 << (page & 63)),
        0
    );
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    assert!(cpu.decode_cache.native_code_watch.is_watched(0x100));

    cpu.note_a20_changed();

    assert!(!cpu.decode_cache.range_hits_code(0x100, 1));
    assert_eq!(
        cpu.decode_cache.code_pages[(page >> 6) as usize] & (1u64 << (page & 63)),
        0
    );
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    assert!(!cpu.decode_cache.native_code_watch.is_watched(0x100));
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpreter_direct_pages_skip_fast_map_when_native_admission_is_disabled() {
    let mut bus = TestBus::with_memory(vec![0; 0x6000]);
    bus.direct_pages_enabled = true;
    bus.memory[0x2456] = 0x3c;
    bus.memory[0x3456] = 0x5a;
    let mut cpu = CpuGsw::default();

    assert_eq!(
        cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0x2456, BusAccessKind::DataRead,)
            .unwrap(),
        0x3c
    );
    assert!(!cpu.jit_fast_map.has_read_mapping(0x2456, 0x2456));
    cpu.set_jit_auto_admit(true);

    assert_eq!(
        cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0x3456, BusAccessKind::DataRead,)
            .unwrap(),
        0x5a
    );
    assert!(cpu.jit_fast_map.has_read_mapping(0x3456, 0x3456));
    assert!(!cpu.jit_fast_map.has_write_mapping(0x3456, 0x3456));
    cpu.set_jit_auto_admit(false);
    assert!(!cpu.jit_fast_map.has_read_mapping(0x3456, 0x3456));

    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        0x3456,
        0xa5,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    assert!(!cpu.jit_fast_map.has_write_mapping(0x3456, 0x3456));
    assert_eq!(bus.memory[0x3456], 0xa5);

    cpu.note_direct_map_changed();
    assert!(!cpu.jit_fast_map.has_read_mapping(0x3456, 0x3456));
    assert!(!cpu.jit_fast_map.has_write_mapping(0x3456, 0x3456));

    assert_eq!(
        cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0x3456, BusAccessKind::DataRead,)
            .unwrap(),
        0xa5
    );
    assert!(!cpu.jit_fast_map.has_read_mapping(0x3456, 0x3456));

    cpu.flush_tlb_and_code_caches();
    assert!(!cpu.jit_fast_map.has_read_mapping(0x3456, 0x3456));
    assert_eq!(
        cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0x3456, BusAccessKind::DataRead,)
            .unwrap(),
        0xa5
    );
    assert!(!cpu.jit_fast_map.has_read_mapping(0x3456, 0x3456));
}

#[cfg(feature = "jit")]
#[test]
fn direct_runtime_admission_tracks_backend_policy_and_clones_cold() {
    let mut cpu = CpuGsw::default();
    let assert_synchronized = |cpu: &CpuGsw| {
        assert_eq!(
            cpu.direct_runtime.admission_active,
            cpu.jit_direct.execution_enabled()
        );
    };

    assert_synchronized(&cpu);
    cpu.set_jit_auto_admit(true);
    assert_synchronized(&cpu);

    let clone = cpu.clone();
    assert!(!clone.direct_runtime.admission_active);
    assert_synchronized(&clone);

    cpu.set_native_backend_enabled(false);
    assert_synchronized(&cpu);
    cpu.set_jit_auto_admit(true);
    assert_synchronized(&cpu);
    cpu.set_native_backend_enabled(true);
    assert_synchronized(&cpu);
    cpu.set_jit_auto_admit(false);
    assert_synchronized(&cpu);

    cpu.reset();
    assert!(!cpu.direct_runtime.admission_active);
    assert_synchronized(&cpu);
}

#[cfg(feature = "jit")]
#[test]
fn direct_admission_heat_is_per_cpu_instance() {
    let mut changed = CpuGsw::default();
    let unchanged = CpuGsw::default();

    changed.jit_direct.set_admission_heat_for_test(8);

    assert_eq!(changed.jit_direct.admission_heat(), 8);
    assert_eq!(unchanged.jit_direct.admission_heat(), 1);
}

const PAGE_WALK_DIRECTORY: u32 = 0x1000;
const PAGE_WALK_TABLE: u32 = 0x3000;
const PAGE_WALK_FRAME: u32 = 0x5000;

fn page_walk_overlap_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    let mut cs = cpu.registers.cs();
    cs.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    cpu
}

fn decode_page_walk_overlap(cpu: &mut CpuGsw, bus: &mut TestBus, entry: u32) {
    for linear in std::iter::once(entry).chain(entry + 5..entry + 13) {
        cpu.set_eip(linear);
        cpu.fetch_decoded(bus, linear).expect("overlap decode");
    }
    assert!(cpu.decode_cache.line_live(entry, true));

    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    {
        let key = jit::direct::key_for(cpu, entry, true).expect("overlap block key");
        assert!(matches!(
            cpu.jit_direct.probe(key),
            jit::direct::BlockProbe::Interpret
        ));
        let compilation = jit::direct::compile(cpu, entry, true).unwrap();
        cpu.jit_direct
            .install(&compilation)
            .expect("overlap block install");
        assert!(
            cpu.jit_direct
                .range_hits_compiled_code(compilation.span.key.physical, 1)
        );
    }

    bus.trace.clear();
}

fn enable_page_walk_overlap_paging(cpu: &mut CpuGsw) {
    cpu.control.cr0 |= CR0_PE | CR0_PG | CR0_WP;
    cpu.control.cr3 = PAGE_WALK_DIRECTORY;
    cpu.registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x10, 0x93));
}

fn assert_page_walk_code_live(cpu: &CpuGsw, entry: u32, physical: u32) {
    assert!(cpu.decode_cache.line_live(entry, true));
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    assert!(cpu.jit_direct.range_hits_compiled_code(physical, 1));
}

fn assert_page_walk_code_invalidated(cpu: &CpuGsw, entry: u32, physical: u32) {
    assert!(!cpu.decode_cache.line_live(entry, true));
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    assert!(!cpu.jit_direct.range_hits_compiled_code(physical, 1));
}

fn page_walk_write_addresses(bus: &TestBus) -> Vec<u32> {
    bus.trace
        .cycles()
        .iter()
        .filter(|cycle| cycle.kind == BusAccessKind::PageWalkWrite)
        .map(|cycle| cycle.address)
        .collect()
}

#[test]
fn pde_accessed_write_invalidates_overlapping_decoded_and_compiled_code() {
    let entry = PAGE_WALK_DIRECTORY;
    let mut memory = vec![0; 0x7000];
    memory[entry as usize..entry as usize + 4]
        .copy_from_slice(&(PAGE_WALK_TABLE | 0x05).to_le_bytes());
    memory[entry as usize + 4] = 0x40;
    memory[entry as usize + 5..entry as usize + 13].fill(0x40);
    memory[PAGE_WALK_TABLE as usize..PAGE_WALK_TABLE as usize + 4]
        .copy_from_slice(&(PAGE_WALK_FRAME | 0x25).to_le_bytes());
    memory[PAGE_WALK_FRAME as usize] = 0xa5;
    let mut bus = TestBus::with_memory(memory);
    let mut cpu = page_walk_overlap_cpu();
    decode_page_walk_overlap(&mut cpu, &mut bus, entry);
    enable_page_walk_overlap_paging(&mut cpu);
    assert_page_walk_code_live(&cpu, entry, entry);
    let invalidations = cpu.perf_counters().code_invalidations;

    assert_eq!(
        cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead)
            .unwrap(),
        0xa5
    );

    assert_eq!(
        u32::from_le_bytes(
            bus.memory[entry as usize..entry as usize + 4]
                .try_into()
                .unwrap()
        ),
        PAGE_WALK_TABLE | 0x25
    );
    assert_page_walk_code_invalidated(&cpu, entry, entry);
    assert_eq!(cpu.perf_counters().code_invalidations, invalidations + 1);
    assert!(cpu.written_pages.contains(&Some(PAGE_WALK_DIRECTORY >> 12)));
    assert_eq!(page_walk_write_addresses(&bus), vec![PAGE_WALK_DIRECTORY]);
}

/// A TLB entry cached while a page was read-only must not fault a later write once the
/// guest has made the PTE writable, even with no INVLPG or CR3 reload in between. Real
/// silicon survives that missing flush by evicting the entry out of its 32-64 slots; with
/// 1024 direct-mapped slots the entry is still there, so the fault has to come from the
/// walk, never from the hit. TSUMERA (Borland 32RTM under VCPI) tripped exactly this at
/// exit: its ring-0 DPMI host flips a data page R/O -> R/W without reloading CR3, and the
/// ring-3 refcount decrement that follows took a spurious #PF(7).
#[test]
fn write_after_guest_unprotects_a_pte_without_flushing_rewalks_instead_of_faulting() {
    let linear = 0x1000;
    let pte = PAGE_WALK_TABLE + 4;
    let mut memory = vec![0; 0x7000];
    memory[PAGE_WALK_DIRECTORY as usize..PAGE_WALK_DIRECTORY as usize + 4]
        .copy_from_slice(&(PAGE_WALK_TABLE | 0x07).to_le_bytes());
    // Present, read-only. CR0.WP is set by the fixture, so a supervisor write faults too.
    memory[pte as usize..pte as usize + 4].copy_from_slice(&(PAGE_WALK_FRAME | 0x01).to_le_bytes());
    memory[PAGE_WALK_FRAME as usize] = 0xa5;
    let mut bus = TestBus::with_memory(memory);
    let mut cpu = page_walk_overlap_cpu();
    enable_page_walk_overlap_paging(&mut cpu);

    // The read caches a translation whose R/W bit is clear.
    assert_eq!(
        cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, linear, BusAccessKind::DataRead)
            .unwrap(),
        0xa5
    );
    assert_eq!(
        cpu.tlb.lookup(linear >> 12).map(|e| e.writable),
        Some(false)
    );

    // The guest sets R/W and skips the flush the architecture requires.
    bus.memory[pte as usize..pte as usize + 4]
        .copy_from_slice(&(PAGE_WALK_FRAME | 0x03).to_le_bytes());

    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        linear,
        0x5a,
        BusAccessKind::DataWrite,
    )
    .expect("the live page tables permit this write");
    assert_eq!(bus.memory[PAGE_WALK_FRAME as usize], 0x5a);
    assert_eq!(cpu.tlb.lookup(linear >> 12).map(|e| e.writable), Some(true));
}

fn pte_dirty_overlap_fixture() -> (CpuGsw, TestBus, u32, u32) {
    let linear = 0x1000;
    let pte = PAGE_WALK_TABLE + 4;
    let entry = pte - 1;
    let mut memory = vec![0; 0x7000];
    memory[PAGE_WALK_DIRECTORY as usize..PAGE_WALK_DIRECTORY as usize + 4]
        .copy_from_slice(&(PAGE_WALK_TABLE | 0x27).to_le_bytes());
    memory[entry as usize] = 0x05;
    memory[pte as usize..pte as usize + 4].copy_from_slice(&(PAGE_WALK_FRAME | 0x27).to_le_bytes());
    memory[entry as usize + 5..entry as usize + 13].fill(0x40);
    let mut bus = TestBus::with_memory(memory);
    let mut cpu = page_walk_overlap_cpu();
    decode_page_walk_overlap(&mut cpu, &mut bus, entry);
    enable_page_walk_overlap_paging(&mut cpu);
    (cpu, bus, entry, linear)
}

#[test]
fn pte_dirty_write_invalidates_overlapping_decoded_and_compiled_code() {
    let (mut cpu, mut bus, entry, linear) = pte_dirty_overlap_fixture();
    let pte = PAGE_WALK_TABLE + 4;
    assert_page_walk_code_live(&cpu, entry, pte);
    let invalidations = cpu.perf_counters().code_invalidations;

    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        linear,
        0x5a,
        BusAccessKind::DataWrite,
    )
    .unwrap();

    assert_eq!(
        u32::from_le_bytes(
            bus.memory[pte as usize..pte as usize + 4]
                .try_into()
                .unwrap()
        ),
        PAGE_WALK_FRAME | 0x67
    );
    assert_eq!(bus.memory[PAGE_WALK_FRAME as usize], 0x5a);
    assert_page_walk_code_invalidated(&cpu, entry, pte);
    assert_eq!(cpu.perf_counters().code_invalidations, invalidations + 1);
    assert!(cpu.written_pages.contains(&Some(PAGE_WALK_TABLE >> 12)));
    assert!(cpu.written_pages.contains(&Some(PAGE_WALK_FRAME >> 12)));
    assert_eq!(page_walk_write_addresses(&bus), vec![pte]);
}

#[test]
fn failed_pte_dirty_write_does_not_notify_or_partially_commit() {
    let (mut cpu, mut bus, entry, linear) = pte_dirty_overlap_fixture();
    let pte = PAGE_WALK_TABLE + 4;
    let pte_before = bus.memory[pte as usize..pte as usize + 4].to_vec();
    let invalidations = cpu.perf_counters().code_invalidations;
    bus.fail_write_address = Some(pte);

    assert!(
        cpu.write_memory_u8(
            &mut bus,
            SegmentIndex::Ds,
            linear,
            0x5a,
            BusAccessKind::DataWrite,
        )
        .is_err()
    );

    assert_eq!(
        &bus.memory[pte as usize..pte as usize + 4],
        pte_before.as_slice()
    );
    assert_eq!(bus.memory[PAGE_WALK_FRAME as usize], 0);
    assert_page_walk_code_live(&cpu, entry, pte);
    assert_eq!(cpu.perf_counters().code_invalidations, invalidations);
    assert_eq!(cpu.written_count, 0);
    assert!(cpu.tlb.lookup(linear >> 12).is_none());
    assert_eq!(page_walk_write_addresses(&bus), vec![pte]);
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn physical_page_cache_hits_fill_every_paging_alias() {
    const ALIAS_A: u32 = 0x3000;
    const ALIAS_B: u32 = 0x4000;
    const FRAME: u32 = 0x5000;
    const OFFSET: u32 = 0x120;

    let mut memory = vec![0; 0x7000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes());
    memory[0x200c..0x2010].copy_from_slice(&0x0000_5007u32.to_le_bytes());
    memory[0x2010..0x2014].copy_from_slice(&0x0000_5007u32.to_le_bytes());
    memory[(FRAME + OFFSET) as usize..(FRAME + OFFSET + 4) as usize]
        .copy_from_slice(&0x4433_2211u32.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = 0x1000;
    cpu.set_jit_auto_admit(true);

    assert_eq!(
        cpu.read_memory_u8(
            &mut bus,
            SegmentIndex::Ds,
            ALIAS_A + OFFSET,
            BusAccessKind::DataRead,
        )
        .unwrap(),
        0x11
    );
    let page_fills = cpu.perf_counters().direct_page_hits;
    assert!(
        cpu.jit_fast_map
            .has_read_mapping(ALIAS_A + OFFSET, FRAME + OFFSET)
    );
    assert_eq!(
        cpu.read_memory_u8(
            &mut bus,
            SegmentIndex::Ds,
            ALIAS_B + OFFSET,
            BusAccessKind::DataRead,
        )
        .unwrap(),
        0x11
    );
    assert_eq!(cpu.perf_counters().direct_page_hits, page_fills);
    assert!(
        cpu.jit_fast_map
            .has_read_mapping(ALIAS_B + OFFSET, FRAME + OFFSET)
    );
    assert!(
        !cpu.jit_fast_map
            .has_read_mapping(ALIAS_B + OFFSET, 0x6000 + OFFSET)
    );

    cpu.jit_fast_map.invalidate_page(ALIAS_A);
    cpu.jit_fast_map.invalidate_page(ALIAS_B);
    assert_eq!(
        cpu.read_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            ALIAS_A + OFFSET,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )
        .unwrap(),
        0x4433_2211
    );
    assert_eq!(
        cpu.read_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            ALIAS_B + OFFSET,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )
        .unwrap(),
        0x4433_2211
    );
    assert!(
        cpu.jit_fast_map
            .has_read_mapping(ALIAS_A + OFFSET, FRAME + OFFSET)
    );
    assert!(
        cpu.jit_fast_map
            .has_read_mapping(ALIAS_B + OFFSET, FRAME + OFFSET)
    );

    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        ALIAS_A + OFFSET,
        0x55,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        ALIAS_B + OFFSET,
        0x66,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    assert!(
        cpu.jit_fast_map
            .has_write_mapping(ALIAS_A + OFFSET, FRAME + OFFSET)
    );
    assert!(
        cpu.jit_fast_map
            .has_write_mapping(ALIAS_B + OFFSET, FRAME + OFFSET)
    );

    cpu.jit_fast_map.invalidate_page(ALIAS_A);
    cpu.jit_fast_map.invalidate_page(ALIAS_B);
    cpu.write_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        ALIAS_A + OFFSET,
        OperandSize::Dword,
        0xaabb_ccdd,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    cpu.write_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        ALIAS_B + OFFSET,
        OperandSize::Dword,
        0x1020_3040,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    assert!(
        cpu.jit_fast_map
            .has_write_mapping(ALIAS_A + OFFSET, FRAME + OFFSET)
    );
    assert!(
        cpu.jit_fast_map
            .has_write_mapping(ALIAS_B + OFFSET, FRAME + OFFSET)
    );
    assert_eq!(
        &bus.memory[(FRAME + OFFSET) as usize..(FRAME + OFFSET + 4) as usize],
        &0x1020_3040u32.to_le_bytes()
    );

    cpu.jit_fast_map.invalidate_page(ALIAS_B);
    assert!(
        cpu.jit_fast_map
            .has_write_mapping(ALIAS_A + OFFSET, FRAME + OFFSET)
    );
    assert!(
        !cpu.jit_fast_map
            .has_write_mapping(ALIAS_B + OFFSET, FRAME + OFFSET)
    );
    cpu.note_direct_map_changed();
    assert!(
        !cpu.jit_fast_map
            .has_write_mapping(ALIAS_A + OFFSET, FRAME + OFFSET)
    );
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn active_fast_map_tracks_tlb_collision_and_rewalks_canonically() {
    const LINEAR_A: u32 = 0x0000_3000;
    const LINEAR_B: u32 = LINEAR_A + TLB_ENTRIES as u32 * 0x1000;
    const FRAME_A: u32 = 0x0000_5000;
    const FRAME_B: u32 = 0x0000_6000;

    // A COUPLING check, not an independent one: while LINEAR_B is defined as one TLB_ENTRIES
    // stride above LINEAR_A this cannot fail, because slot() masks with TLB_ENTRIES - 1. It earns
    // its place by failing the moment the stride and the slot function stop agreeing, which is the
    // exact regression this fixture suffered before: a stride hardcoded to 64 stopped colliding
    // and the test then failed far downstream on a consequence instead of here on the cause.
    // The teeth against a vacuous pass are lower down: PTE_A != PTE_B, the eviction of A, the
    // survival of B, and the exactly-two-PageWalkRead re-walk.
    assert_ne!(LINEAR_A, LINEAR_B);
    assert_eq!(Tlb::slot(LINEAR_A >> 12), Tlb::slot(LINEAR_B >> 12));

    let mut memory = vec![0; 0x8000];
    // Two directory entries, one page table each. At 1024 entries the collision stride is exactly
    // 4 MiB, which is one page-directory entry, so a colliding pair CANNOT share a page table and
    // the single hardcoded PDE this fixture used to install is not enough. Derived from the two
    // linear addresses so it keeps working at any TLB_ENTRIES.
    let mut next_table = 0x2000usize;
    let mut map_page = |memory: &mut Vec<u8>, linear: u32, frame: u32| {
        let pde = 0x1000 + ((linear >> 22) as usize * 4);
        let existing = u32::from_le_bytes(memory[pde..pde + 4].try_into().unwrap());
        let table = if existing & 1 != 0 {
            (existing & !0xfff) as usize
        } else {
            let table = next_table;
            next_table += 0x1000;
            memory[pde..pde + 4].copy_from_slice(&((table as u32) | 7).to_le_bytes());
            table
        };
        let pte = table + (((linear >> 12) & 0x3ff) as usize * 4);
        memory[pte..pte + 4].copy_from_slice(&(frame | 7).to_le_bytes());
    };
    map_page(&mut memory, LINEAR_A, FRAME_A);
    map_page(&mut memory, LINEAR_B, FRAME_B);
    memory[FRAME_A as usize] = 0xa5;
    memory[FRAME_B as usize] = 0x5a;

    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = CpuGsw::default();
    cpu.set_jit_auto_admit(true);
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = 0x1000;
    cpu.registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x10, 0x93));

    assert_eq!(
        cpu.read_memory_u8(
            &mut bus,
            SegmentIndex::Ds,
            LINEAR_A,
            BusAccessKind::DataRead,
        )
        .unwrap(),
        0xa5
    );
    assert_eq!(
        cpu.read_memory_u8(
            &mut bus,
            SegmentIndex::Ds,
            LINEAR_B,
            BusAccessKind::DataRead,
        )
        .unwrap(),
        0x5a
    );
    // A was evicted BY B, so B must be the entry that survived. Pinning both directions stops a
    // degenerate fixture (A == B, or a fault that re-walked A away) from satisfying the negative
    // assertion for the wrong reason.
    assert!(cpu.tlb.lookup(LINEAR_A >> 12).is_none());
    assert!(cpu.tlb.lookup(LINEAR_B >> 12).is_some());
    assert!(!cpu.jit_fast_map.has_read_mapping(LINEAR_A, FRAME_A));

    bus.trace.clear();
    assert_eq!(
        cpu.read_memory_u8(
            &mut bus,
            SegmentIndex::Ds,
            LINEAR_A,
            BusAccessKind::DataRead,
        )
        .unwrap(),
        0xa5
    );
    assert_eq!(
        bus.trace
            .cycles()
            .iter()
            .filter(|cycle| cycle.kind == BusAccessKind::PageWalkRead)
            .count(),
        2
    );
    assert!(
        bus.trace
            .cycles()
            .iter()
            .all(|cycle| cycle.kind != BusAccessKind::PageWalkWrite)
    );
    assert!(cpu.tlb.lookup(LINEAR_A >> 12).is_some());
    assert!(cpu.jit_fast_map.has_read_mapping(LINEAR_A, FRAME_A));
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn fast_map_same_tag_dirty_upgrade_keeps_read_residency() {
    const LINEAR: u32 = 0x3000;
    const FRAME: u32 = 0x5000;
    const PTE: usize = 0x2000 + ((LINEAR >> 12) as usize * 4);

    let mut memory = vec![0; 0x8000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes());
    memory[PTE..PTE + 4].copy_from_slice(&(FRAME | 7).to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = CpuGsw::default();
    cpu.set_jit_auto_admit(true);
    cpu.control.cr0 |= CR0_PE | CR0_PG | CR0_WP;
    cpu.control.cr3 = 0x1000;
    cpu.registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x10, 0x93));

    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, LINEAR, BusAccessKind::DataRead)
        .unwrap();
    assert!(!cpu.tlb.lookup(LINEAR >> 12).unwrap().dirty);
    assert!(cpu.jit_fast_map.has_read_mapping(LINEAR, FRAME));
    assert!(!cpu.jit_fast_map.has_write_mapping(LINEAR, FRAME));

    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        LINEAR,
        0xa5,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    assert!(cpu.tlb.lookup(LINEAR >> 12).unwrap().dirty);
    assert!(cpu.jit_fast_map.has_read_mapping(LINEAR, FRAME));
    assert!(cpu.jit_fast_map.has_write_mapping(LINEAR, FRAME));
    assert_eq!(
        u32::from_le_bytes(bus.memory[PTE..PTE + 4].try_into().unwrap()) & 0x60,
        0x60
    );
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn fast_map_same_tag_remap_replaces_residency_and_fault_keeps_read_mapping() {
    const LINEAR: u32 = 0x3000;
    const FRAME_A: u32 = 0x5000;
    const FRAME_B: u32 = 0x6000;
    const PTE: usize = 0x2000 + ((LINEAR >> 12) as usize * 4);

    let make_fixture = || {
        let mut memory = vec![0; 0x8000];
        memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes());
        memory[PTE..PTE + 4].copy_from_slice(&(FRAME_A | 7).to_le_bytes());
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        let mut cpu = CpuGsw::default();
        cpu.set_jit_auto_admit(true);
        cpu.control.cr0 |= CR0_PE | CR0_PG | CR0_WP;
        cpu.control.cr3 = 0x1000;
        cpu.registers
            .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x10, 0x93));
        (cpu, bus)
    };

    let (mut cpu, mut bus) = make_fixture();
    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, LINEAR, BusAccessKind::DataRead)
        .unwrap();
    bus.memory[PTE..PTE + 4].copy_from_slice(&(FRAME_B | 0x27).to_le_bytes());
    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        LINEAR,
        0x5a,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    assert!(!cpu.jit_fast_map.has_read_mapping(LINEAR, FRAME_A));
    assert!(cpu.jit_fast_map.has_write_mapping(LINEAR, FRAME_B));
    assert_eq!(bus.memory[FRAME_A as usize], 0);
    assert_eq!(bus.memory[FRAME_B as usize], 0x5a);

    let (mut denied_cpu, mut denied_bus) = make_fixture();
    denied_cpu
        .read_memory_u8(
            &mut denied_bus,
            SegmentIndex::Ds,
            LINEAR,
            BusAccessKind::DataRead,
        )
        .unwrap();
    denied_bus.memory[PTE..PTE + 4].copy_from_slice(&(FRAME_B | 0x25).to_le_bytes());
    assert!(matches!(
        denied_cpu.write_memory_u8(
            &mut denied_bus,
            SegmentIndex::Ds,
            LINEAR,
            0x3c,
            BusAccessKind::DataWrite,
        ),
        Err(InternalFault::Exception { vector: 14, .. })
    ));
    assert!(denied_cpu.jit_fast_map.has_read_mapping(LINEAR, FRAME_A));
    assert!(!denied_cpu.jit_fast_map.has_write_mapping(LINEAR, FRAME_A));
    assert_eq!(denied_bus.memory[FRAME_A as usize], 0);
    assert_eq!(denied_bus.memory[FRAME_B as usize], 0);
}

// --- Lever 1: interpreter FastMap serve path -------------------------------------------------
//
// The tests below exercise `CpuGsw::fast_map_data_slot` and its joined tails
// (`finish_fast_map_read`/`finish_fast_map_write`) ONLY through the public interpreter API
// (`read_memory_u8`/`write_memory_u8`/`read_memory_sized`/`write_memory_sized`) plus the
// `fast_map_probe_counters()` hit/miss counters and `jit_fast_map` mapping queries --
// those private helpers are not reachable from this module. None of these fixtures compile or run
// native code (no `jit_direct::compile`/`install`), so the JIT block-entry-position trap that has
// bitten this repo before does not apply here.

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn read_by_width(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32, width: BusWidth) -> u32 {
    match width {
        BusWidth::Byte => u32::from(
            cpu.read_memory_u8(bus, SegmentIndex::Ds, linear, BusAccessKind::DataRead)
                .unwrap(),
        ),
        BusWidth::Word => cpu
            .read_memory_sized(
                bus,
                SegmentIndex::Ds,
                linear,
                OperandSize::Word,
                BusAccessKind::DataRead,
            )
            .unwrap(),
        BusWidth::Dword => cpu
            .read_memory_sized(
                bus,
                SegmentIndex::Ds,
                linear,
                OperandSize::Dword,
                BusAccessKind::DataRead,
            )
            .unwrap(),
    }
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn write_by_width(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32, width: BusWidth, value: u32) {
    match width {
        BusWidth::Byte => cpu
            .write_memory_u8(
                bus,
                SegmentIndex::Ds,
                linear,
                value as u8,
                BusAccessKind::DataWrite,
            )
            .unwrap(),
        BusWidth::Word => cpu
            .write_memory_sized(
                bus,
                SegmentIndex::Ds,
                linear,
                OperandSize::Word,
                value,
                BusAccessKind::DataWrite,
            )
            .unwrap(),
        BusWidth::Dword => cpu
            .write_memory_sized(
                bus,
                SegmentIndex::Ds,
                linear,
                OperandSize::Dword,
                value,
                BusAccessKind::DataWrite,
            )
            .unwrap(),
    }
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn flip_bits(width: BusWidth, value: u32) -> u32 {
    match width {
        BusWidth::Byte => u32::from((value as u8) ^ 0xff),
        BusWidth::Word => u32::from((value as u16) ^ 0xffff),
        BusWidth::Dword => value ^ 0xffff_ffff,
    }
}

/// Fidelity anchor for the whole slice: a FastMap hit and a forced slow path must be
/// indistinguishable in guest-visible terms. `fast` primes the FastMap (`set_jit_auto_admit`) and
/// takes the hit on the measured access; `slow` never populates (auto-admit stays off), so it
/// always takes the canonical translate+DirectPageCache path, including on the measured access.
/// Both buses share the same wait-state model (`direct_page_clocks = true`), so
/// `trace.elapsed_clocks()` is directly comparable -- this is the interpreter-side counterpart of
/// the design doc's "compare `trace.elapsed_clocks`, never `trace.cycles`" rule.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn fast_map_serve_path_matches_slow_path_for_ram_reads_and_writes() {
    const LINEAR: u32 = 0x0000_3010;

    let fixture = |auto_admit: bool| {
        let mut memory = vec![0u8; 0x6000];
        memory[LINEAR as usize..LINEAR as usize + 4].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        let mut cpu = CpuGsw::default();
        cpu.set_jit_auto_admit(auto_admit);
        (cpu, bus)
    };

    for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
        // --- Read direction ---
        let (mut fast, mut fast_bus) = fixture(true);
        let (mut slow, mut slow_bus) = fixture(false);
        let prime_fast = read_by_width(&mut fast, &mut fast_bus, LINEAR, width);
        let prime_slow = read_by_width(&mut slow, &mut slow_bus, LINEAR, width);
        assert_eq!(prime_fast, prime_slow);
        assert!(fast.jit_fast_map.has_read_mapping(LINEAR, LINEAR));
        assert!(!slow.jit_fast_map.has_read_mapping(LINEAR, LINEAR));

        let hits_before = fast.fast_map_probe_counters().hits;
        fast_bus.trace.clear();
        slow_bus.trace.clear();
        let value_fast = read_by_width(&mut fast, &mut fast_bus, LINEAR, width);
        let value_slow = read_by_width(&mut slow, &mut slow_bus, LINEAR, width);

        assert_eq!(value_fast, value_slow, "{width:?} read value diverged");
        assert_eq!(
            fast.fast_map_probe_counters().hits,
            hits_before + 1,
            "{width:?} read did not take the fast path"
        );
        assert_eq!(slow.fast_map_probe_counters().hits, 0);
        assert_eq!(
            fast_bus.trace.elapsed_clocks(),
            slow_bus.trace.elapsed_clocks(),
            "{width:?} read charged different bus clocks"
        );
        assert_eq!(fast.registers.eflags, slow.registers.eflags);

        // --- Write direction ---
        let (mut fast, mut fast_bus) = fixture(true);
        let (mut slow, mut slow_bus) = fixture(false);
        // Prime with a same-value write (no guest-visible change) so it populates the fast CPU's
        // write bias without disturbing the byte pattern the measured write overwrites.
        let existing = read_by_width(&mut fast, &mut fast_bus, LINEAR, width);
        write_by_width(&mut fast, &mut fast_bus, LINEAR, width, existing);
        write_by_width(&mut slow, &mut slow_bus, LINEAR, width, existing);
        assert!(fast.jit_fast_map.has_write_mapping(LINEAR, LINEAR));
        assert!(!slow.jit_fast_map.has_write_mapping(LINEAR, LINEAR));

        let hits_before = fast.fast_map_probe_counters().hits;
        fast_bus.trace.clear();
        slow_bus.trace.clear();
        let new_value = flip_bits(width, existing);
        write_by_width(&mut fast, &mut fast_bus, LINEAR, width, new_value);
        write_by_width(&mut slow, &mut slow_bus, LINEAR, width, new_value);

        let end = LINEAR as usize + width.bytes() as usize;
        assert_eq!(
            fast_bus.memory[LINEAR as usize..end],
            slow_bus.memory[LINEAR as usize..end],
            "{width:?} write produced different memory"
        );
        assert_eq!(
            fast.fast_map_probe_counters().hits,
            hits_before + 1,
            "{width:?} write did not take the fast path"
        );
        assert_eq!(slow.fast_map_probe_counters().hits, 0);
        assert_eq!(
            fast_bus.trace.elapsed_clocks(),
            slow_bus.trace.elapsed_clocks(),
            "{width:?} write charged different bus clocks"
        );
        assert_eq!(fast.registers.eflags, slow.registers.eflags);
    }
}

/// A Mode13 hit must defer to the full `charge_direct_memory` (video wait states plus the
/// `note_direct_write`-equivalent bookkeeping `TestBus::note_mode13_write` stands in for), never
/// the flat RAM charge `charge_direct_ram_memory` takes. Same fast-vs-forced-slow differential as
/// the RAM fidelity test above, at an address inside the FastMap's and the bus's Mode13 aperture
/// (0xa0000..0xb0000 in both -- see `fast_map.rs::MODE13_BASE/END` and
/// `bus.rs::charge_direct_memory`'s range test, which this test's agreement depends on).
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn fast_map_mode13_write_charges_video_wait_states_like_the_slow_path() {
    const LINEAR: u32 = 0x000a_0100;

    let fixture = |auto_admit: bool| {
        // Big enough to cover the whole 0xa0000..0xb0000 aperture.
        let memory = vec![0u8; 0x000c_0000];
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        let mut cpu = CpuGsw::default();
        cpu.set_jit_auto_admit(auto_admit);
        // LINEAR (0xa0100) exceeds a default real-mode segment's 0xffff limit; go flat so the
        // access resolves purely on the address, matching the other paged/flat fixtures here.
        cpu.registers
            .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x10, 0x93));
        (cpu, bus)
    };

    for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
        let (mut fast, mut fast_bus) = fixture(true);
        let (mut slow, mut slow_bus) = fixture(false);
        write_by_width(&mut fast, &mut fast_bus, LINEAR, width, 0);
        write_by_width(&mut slow, &mut slow_bus, LINEAR, width, 0);
        assert!(fast.jit_fast_map.has_write_mapping(LINEAR, LINEAR));

        let hits_before = fast.fast_map_probe_counters().hits;
        fast_bus.trace.clear();
        slow_bus.trace.clear();
        write_by_width(&mut fast, &mut fast_bus, LINEAR, width, 0xa5);
        write_by_width(&mut slow, &mut slow_bus, LINEAR, width, 0xa5);

        assert_eq!(
            fast.fast_map_probe_counters().hits,
            hits_before + 1,
            "{width:?} mode13 write did not take the fast path"
        );
        assert_eq!(
            fast_bus.trace.elapsed_clocks(),
            slow_bus.trace.elapsed_clocks(),
            "{width:?} mode13 write charged different bus clocks"
        );
        assert_ne!(
            fast_bus.trace.elapsed_clocks(),
            0,
            "{width:?} mode13 write charged nothing -- the test would be vacuous"
        );
        assert_eq!(fast_bus.mode13_dirty_pages, slow_bus.mode13_dirty_pages);
        assert_ne!(fast_bus.mode13_dirty_pages, 0);
        let (fast_writes, slow_writes) = match width {
            BusWidth::Byte => (fast_bus.mode13_byte_writes, slow_bus.mode13_byte_writes),
            BusWidth::Word => (fast_bus.mode13_word_writes, slow_bus.mode13_word_writes),
            BusWidth::Dword => (fast_bus.mode13_dword_writes, slow_bus.mode13_dword_writes),
        };
        assert!(
            fast_writes > 0,
            "{width:?} note_direct_write-equivalent was not called on the fast path"
        );
        assert_eq!(fast_writes, slow_writes);
        let end = LINEAR as usize + width.bytes() as usize;
        assert_eq!(
            fast_bus.memory[LINEAR as usize..end],
            slow_bus.memory[LINEAR as usize..end]
        );
    }
}

/// `lookup_access` rejects an unaligned width even on a page that is otherwise live in the
/// FastMap (populated here by a prior ALIGNED dword read), so an unaligned probe misses by
/// construction of the hit predicate, not merely because the page was never touched.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn fast_map_probe_rejects_unaligned_widths() {
    const BASE: u32 = 0x0000_4000;
    let mut memory = vec![0u8; 0x6000];
    memory[BASE as usize..BASE as usize + 8]
        .copy_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = CpuGsw::default();
    cpu.set_jit_auto_admit(true);

    cpu.read_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        BASE,
        OperandSize::Dword,
        BusAccessKind::DataRead,
    )
    .unwrap();
    assert!(cpu.jit_fast_map.has_read_mapping(BASE, BASE));

    for (offset, width) in [
        (1u32, OperandSize::Word),
        (1, OperandSize::Dword),
        (2, OperandSize::Dword),
    ] {
        let hits_before = cpu.fast_map_probe_counters().hits;
        let misses_before = cpu.fast_map_probe_counters().misses;
        cpu.read_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            BASE + offset,
            width,
            BusAccessKind::DataRead,
        )
        .unwrap();
        assert_eq!(
            cpu.fast_map_probe_counters().hits,
            hits_before,
            "unaligned {width:?} at +{offset} took the fast path"
        );
        assert!(cpu.fast_map_probe_counters().misses > misses_before);
    }
}

/// A crossing dword decomposes into two page-local WORD fragments (`page_local_fragment_width`
/// never returns a width that would itself cross a page), so no single FastMap probe ever sees a
/// crossing width. This sets up two adjacent linear pages backed by DIFFERENT physical frames --
/// so a bug that served all 4 bytes from one page's bias would read garbage across the boundary
/// -- and checks both the combined value AND that both fragments independently took the fast path.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn fast_map_serves_cross_page_reads_correctly_via_page_local_fragments() {
    const DIRECTORY: u32 = 0x1000;
    const TABLE: u32 = 0x2000;
    const LINEAR_PAGE0: u32 = 0x0000_3000;
    const LINEAR_PAGE1: u32 = 0x0000_4000;
    const FRAME0: u32 = 0x0000_6000;
    const FRAME1: u32 = 0x0000_7000;
    const BOUNDARY_LINEAR: u32 = LINEAR_PAGE0 + 0x0ffe;

    let mut memory = vec![0u8; 0x9000];
    memory[DIRECTORY as usize..DIRECTORY as usize + 4].copy_from_slice(&(TABLE | 7).to_le_bytes());
    let pte0 = TABLE as usize + (((LINEAR_PAGE0 >> 12) as usize) & 0x3ff) * 4;
    let pte1 = TABLE as usize + (((LINEAR_PAGE1 >> 12) as usize) & 0x3ff) * 4;
    memory[pte0..pte0 + 4].copy_from_slice(&(FRAME0 | 7).to_le_bytes());
    memory[pte1..pte1 + 4].copy_from_slice(&(FRAME1 | 7).to_le_bytes());
    memory[(FRAME0 + 0x0ffe) as usize] = 0xaa;
    memory[(FRAME0 + 0x0fff) as usize] = 0xbb;
    memory[FRAME1 as usize] = 0xcc;
    memory[(FRAME1 + 1) as usize] = 0xdd;

    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = CpuGsw::default();
    cpu.set_jit_auto_admit(true);
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = DIRECTORY;
    cpu.registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x10, 0x93));

    // A page's read bias serves any width once live, so a plain byte read anywhere inside it is
    // enough to prime both pages.
    cpu.read_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        LINEAR_PAGE0,
        BusAccessKind::DataRead,
    )
    .unwrap();
    cpu.read_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        LINEAR_PAGE1,
        BusAccessKind::DataRead,
    )
    .unwrap();
    assert!(cpu.jit_fast_map.has_read_mapping(LINEAR_PAGE0, FRAME0));
    assert!(cpu.jit_fast_map.has_read_mapping(LINEAR_PAGE1, FRAME1));

    let hits_before = cpu.fast_map_probe_counters().hits;
    let value = cpu
        .read_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            BOUNDARY_LINEAR,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )
        .unwrap();

    assert_eq!(
        value, 0xddcc_bbaa,
        "cross-page fragments read the wrong bytes"
    );
    assert_eq!(
        cpu.fast_map_probe_counters().hits,
        hits_before + 2,
        "expected both page-local fragments to take the fast path"
    );
}

/// A CPL-3 access to a supervisor-only page must fault identically whether or not the FastMap is
/// armed. `fast` primes a CPL-0 read (which sets a live FastMap entry with PAGE_USER unset, since
/// the TLB entry's `user` bit is false), then flips to CPL 3 for the measured access, which must
/// still fault via `lookup_access`'s own PAGE_USER check. `slow` never populates (auto-admit off)
/// and takes the canonical `translate_linear_checked` path as the architectural reference.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn fast_map_hit_still_faults_a_cpl3_access_to_a_supervisor_page() {
    const DIRECTORY: u32 = 0x1000;
    const TABLE: u32 = 0x2000;
    const LINEAR: u32 = 0x0000_3000;
    const FRAME: u32 = 0x0000_5000;

    let fixture = || {
        let mut memory = vec![0u8; 0x7000];
        memory[DIRECTORY as usize..DIRECTORY as usize + 4]
            .copy_from_slice(&(TABLE | 0x07).to_le_bytes());
        let pte = TABLE as usize + (((LINEAR >> 12) as usize) & 0x3ff) * 4;
        // Present + writable, but NOT user (bit 2 clear): a supervisor-only page.
        memory[pte..pte + 4].copy_from_slice(&(FRAME | 0x03).to_le_bytes());
        memory[FRAME as usize] = 0xa5;
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        let mut cpu = CpuGsw::default();
        cpu.control.cr0 |= CR0_PE | CR0_PG;
        cpu.control.cr3 = DIRECTORY;
        cpu.registers
            .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x10, 0x93));
        (cpu, bus)
    };

    let (mut fast, mut fast_bus) = fixture();
    fast.set_jit_auto_admit(true);
    fast.cpl = 0;
    assert_eq!(
        fast.read_memory_u8(
            &mut fast_bus,
            SegmentIndex::Ds,
            LINEAR,
            BusAccessKind::DataRead
        )
        .unwrap(),
        0xa5
    );
    assert!(fast.jit_fast_map.has_read_mapping(LINEAR, FRAME));
    fast.cpl = 3;
    let fast_result = fast.read_memory_u8(
        &mut fast_bus,
        SegmentIndex::Ds,
        LINEAR,
        BusAccessKind::DataRead,
    );
    assert!(fast.fast_map_probe_counters().misses > 0);

    let (mut slow, mut slow_bus) = fixture();
    slow.cpl = 0;
    slow.read_memory_u8(
        &mut slow_bus,
        SegmentIndex::Ds,
        LINEAR,
        BusAccessKind::DataRead,
    )
    .unwrap();
    assert!(!slow.jit_fast_map.has_read_mapping(LINEAR, FRAME));
    slow.cpl = 3;
    let slow_result = slow.read_memory_u8(
        &mut slow_bus,
        SegmentIndex::Ds,
        LINEAR,
        BusAccessKind::DataRead,
    );

    match (fast_result, slow_result) {
        (
            Err(InternalFault::Exception {
                vector: fv,
                error_code: fe,
            }),
            Err(InternalFault::Exception {
                vector: sv,
                error_code: se,
            }),
        ) => {
            assert_eq!(fv, 14);
            assert_eq!(sv, 14);
            assert_eq!(fe, se, "fast-path and slow-path #PF error codes diverged");
        }
        other => panic!("expected both paths to fault identically with vector 14, got {other:?}"),
    }
}

/// A write to a page whose PTE dirty bit is not yet set must take the slow (walk) path even with
/// the FastMap armed, because a write bias is only ever created after `record_write_page`'s walk
/// commits the dirty bit -- see `fast_map_permissions` (memory.rs), which requires `entry.dirty`
/// for a write. This is the same fixture shape as `fast_map_same_tag_dirty_upgrade_keeps_read_
/// residency` above, extended with the new hit/miss counters this slice adds.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn first_write_to_a_clean_pte_page_takes_the_slow_path() {
    const LINEAR: u32 = 0x3000;
    const FRAME: u32 = 0x5000;
    const PTE: usize = 0x2000 + ((LINEAR >> 12) as usize * 4);

    let mut memory = vec![0u8; 0x8000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes());
    memory[PTE..PTE + 4].copy_from_slice(&(FRAME | 7).to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = CpuGsw::default();
    cpu.set_jit_auto_admit(true);
    cpu.control.cr0 |= CR0_PE | CR0_PG | CR0_WP;
    cpu.control.cr3 = 0x1000;
    cpu.registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x10, 0x93));

    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, LINEAR, BusAccessKind::DataRead)
        .unwrap();
    assert!(cpu.jit_fast_map.has_read_mapping(LINEAR, FRAME));
    assert!(!cpu.jit_fast_map.has_write_mapping(LINEAR, FRAME));
    assert!(!cpu.tlb.lookup(LINEAR >> 12).unwrap().dirty);

    let hits_before = cpu.fast_map_probe_counters().hits;
    let misses_before = cpu.fast_map_probe_counters().misses;
    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        LINEAR,
        0xa5,
        BusAccessKind::DataWrite,
    )
    .unwrap();

    assert_eq!(
        cpu.fast_map_probe_counters().hits,
        hits_before,
        "the clean-PTE write hit the fast path"
    );
    assert_eq!(cpu.fast_map_probe_counters().misses, misses_before + 1);
    assert!(
        cpu.tlb.lookup(LINEAR >> 12).unwrap().dirty,
        "the walk that sets the dirty bit was skipped"
    );
    assert!(cpu.jit_fast_map.has_write_mapping(LINEAR, FRAME));
    assert_eq!(bus.memory[FRAME as usize], 0xa5);
}

/// A write that hits the FastMap and changes a watched (decoded) code byte must still invalidate,
/// preserving the G2 same-value elision: a same-value priming write (which ALSO takes the slow
/// path here, since it is the first write and creates the write bias) must NOT invalidate, and
/// the measured, value-changing write through the fast path must.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn fast_map_write_hit_still_invalidates_watched_code() {
    const CODE: u32 = 0x0000_0200;

    let mut memory = vec![0u8; 0x1000];
    memory[CODE as usize] = 0x90; // NOP
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = CpuGsw::default();
    cpu.set_jit_auto_admit(true);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    // `line_live`'s second argument is the line's cached `default_size_32`, not "is decoded";
    // force 32-bit CS (as `page_walk_overlap_cpu` does elsewhere in this file) so the
    // `line_live(CODE, true)` checks below match what `fetch_decoded` actually cached.
    let mut cs = cpu.registers.cs();
    cs.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);

    cpu.set_eip(CODE);
    cpu.fetch_decoded(&mut bus, CODE)
        .expect("decode the watched NOP");
    assert!(cpu.decode_cache.line_live(CODE, true));
    let invalidations_before = cpu.perf_counters().code_invalidations;

    let existing = bus.memory[CODE as usize];
    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        CODE,
        existing,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    assert!(cpu.jit_fast_map.has_write_mapping(CODE, CODE));
    assert!(
        cpu.decode_cache.line_live(CODE, true),
        "a same-value priming write must not invalidate (G2 elision)"
    );
    assert_eq!(cpu.perf_counters().code_invalidations, invalidations_before);

    let hits_before = cpu.fast_map_probe_counters().hits;
    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        CODE,
        0xcc,
        BusAccessKind::DataWrite,
    )
    .unwrap();

    assert_eq!(
        cpu.fast_map_probe_counters().hits,
        hits_before + 1,
        "the measured write did not take the fast path"
    );
    assert!(
        !cpu.decode_cache.line_live(CODE, true),
        "a changed watched write through the fast path failed to invalidate"
    );
    assert_eq!(
        cpu.perf_counters().code_invalidations,
        invalidations_before + 1
    );
    assert_eq!(bus.memory[CODE as usize], 0xcc);
}

/// The N5 read/write/RMW shape census (`IZARRAVM_RMW_CENSUS`) must count what it claims to
/// count, because a verdict rests on the ratio it produces. Three shapes, one fixture:
///
/// - a plain read and a plain store on the SAME page but in DIFFERENT instructions are two
///   accesses and ZERO read-modify-write pairs (the instruction epoch, not just the page, has to
///   match -- otherwise every store into a page the previous instruction read would score),
/// - a read then a store at the same address WITHIN one instruction is one pair,
/// - a read then a store to a DIFFERENT page within one instruction is not (page equality is the
///   condition an interleaved entry would exploit; different pages are different table indices).
///
/// `perf.instructions` stands in for the interpreter's instruction epoch here exactly as it does
/// in production: it is constant for the duration of one instruction and bumped between them.
#[cfg(feature = "jit")]
#[test]
fn rmw_census_counts_same_instruction_same_page_pairs_only() {
    let mut bus = TestBus::with_memory(vec![0u8; 0x8000]);
    bus.direct_pages_enabled = true;
    let mut cpu = CpuGsw {
        rmw_census_enabled: true,
        ..Default::default()
    };

    let read = |cpu: &mut CpuGsw, bus: &mut TestBus, at: u32| {
        cpu.read_memory_sized(
            bus,
            SegmentIndex::Ds,
            at,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )
        .unwrap();
    };
    let write = |cpu: &mut CpuGsw, bus: &mut TestBus, at: u32| {
        cpu.write_memory_sized(
            bus,
            SegmentIndex::Ds,
            at,
            OperandSize::Dword,
            1,
            BusAccessKind::DataWrite,
        )
        .unwrap();
    };

    // Instruction 0: read 0x3000. Instruction 1: write 0x3000. Same page, different instructions.
    read(&mut cpu, &mut bus, 0x3000);
    cpu.perf.instructions += 1;
    write(&mut cpu, &mut bus, 0x3000);
    cpu.perf.instructions += 1;
    assert_eq!(cpu.fast_map_audit_counters().census_rmw_pairs, 0);

    // Instruction 2: read then write 0x3000. One pair.
    read(&mut cpu, &mut bus, 0x3000);
    write(&mut cpu, &mut bus, 0x3000);
    cpu.perf.instructions += 1;
    assert_eq!(cpu.fast_map_audit_counters().census_rmw_pairs, 1);

    // Instruction 3: read 0x3000, write 0x4000. Same instruction, different page: not a pair.
    read(&mut cpu, &mut bus, 0x3000);
    write(&mut cpu, &mut bus, 0x4000);
    cpu.perf.instructions += 1;

    let audit = cpu.fast_map_audit_counters();
    assert_eq!(audit.census_rmw_pairs, 1);
    assert_eq!(audit.census_reads, 3);
    assert_eq!(audit.census_writes, 3);
    assert!(audit.census_enabled);
}

/// The census must cost the default build nothing it can observe: with the gate off, not one of
/// its three tallies moves, while the ordinary data counters prove the fixture is not vacuous.
#[cfg(feature = "jit")]
#[test]
fn rmw_census_stays_silent_when_the_gate_is_off() {
    let mut bus = TestBus::with_memory(vec![0u8; 0x8000]);
    bus.direct_pages_enabled = true;
    let mut cpu = CpuGsw {
        rmw_census_enabled: false,
        ..Default::default()
    };

    for _ in 0..4 {
        cpu.read_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            0x3000,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )
        .unwrap();
        cpu.write_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            0x3000,
            OperandSize::Dword,
            7,
            BusAccessKind::DataWrite,
        )
        .unwrap();
    }

    let audit = cpu.fast_map_audit_counters();
    assert_eq!(audit.census_reads, 0);
    assert_eq!(audit.census_writes, 0);
    assert_eq!(audit.census_rmw_pairs, 0);
    assert!(!audit.census_enabled);
    assert!(
        cpu.perf_counters().data_direct_reads > 0,
        "fixture is vacuous"
    );
    assert!(
        cpu.perf_counters().data_direct_writes > 0,
        "fixture is vacuous"
    );
}

/// A20 toggles and TLB flushes wipe the whole FastMap but are NOT counted by
/// `direct_map_invalidations` -- that counter sits on only two of the five wipe sites. The N5
/// audit exists because nothing said so; this pins the gap rather than the fix.
#[cfg(feature = "jit")]
#[test]
fn fast_map_wipe_causes_split_out_the_sites_direct_map_invalidations_misses() {
    let mut cpu = CpuGsw::default();
    cpu.note_direct_map_changed();
    cpu.note_direct_data_map_changed();
    cpu.note_direct_data_map_changed();
    cpu.note_a20_changed();
    cpu.flush_tlb_and_code_caches();

    let audit = cpu.fast_map_audit_counters();
    assert_eq!(audit.wipes_direct_map, 1);
    assert_eq!(audit.wipes_direct_data_map, 2);
    assert_eq!(audit.wipes_a20, 1);
    assert_eq!(audit.wipes_tlb_flush, 1);
    // The pre-existing counter sees three of those five wipes and misses two.
    assert_eq!(cpu.perf_counters().direct_map_invalidations, 3);
}

/// `fast_map_population_enabled` gates population on `mode().uses_approximate_timing()`, and
/// `fast_map_serve_enabled` (the interpreter serve path's cached mirror of that predicate) gates
/// entry to `fast_map_data_slot` on the SAME condition; this asserts the consequence rather than
/// assuming it: 386-slow and 386 (Accurate) never record a single FastMap hit, and never even
/// probe (the miss counter also stays at zero, since `fast_map_serve_enabled` is false and
/// `fast_map_data_slot` is never entered), even with native admission force-armed and repeated
/// reads/writes at the same address. `FastMap::has_storage()` (a coarser, separate diagnostic --
/// "has population ever run at all") also stays false, since population never runs either.
/// Vacuousness is ruled out by checking that the accesses actually reached the direct-page path
/// (`data_direct_reads`/`data_direct_writes` move).
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn accurate_timing_personas_never_take_the_interpreter_fast_path() {
    for mode in [GswMode::Gsw386Slow, GswMode::Gsw386] {
        assert!(!mode.uses_approximate_timing());
        let mut memory = vec![0u8; 0x6000];
        memory[0x3000..0x3004].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        let mut cpu = CpuGsw::default();
        cpu.set_mode(mode);
        cpu.set_jit_auto_admit(true);

        for _ in 0..4 {
            cpu.read_memory_sized(
                &mut bus,
                SegmentIndex::Ds,
                0x3000,
                OperandSize::Dword,
                BusAccessKind::DataRead,
            )
            .unwrap();
            cpu.write_memory_sized(
                &mut bus,
                SegmentIndex::Ds,
                0x3000,
                OperandSize::Dword,
                0x5566_7788,
                BusAccessKind::DataWrite,
            )
            .unwrap();
        }

        assert_eq!(
            cpu.fast_map_probe_counters().hits,
            0,
            "{mode:?} took the interpreter fast path"
        );
        assert_eq!(
            cpu.fast_map_probe_counters().misses,
            0,
            "{mode:?} probed the FastMap at all -- fast_map_serve_enabled should be false here"
        );
        assert!(
            !cpu.jit_fast_map.has_storage(),
            "{mode:?} populated the FastMap despite Accurate timing"
        );
        // Rule out vacuousness a different way now that the miss counter no longer moves: the
        // accesses must still have reached the direct-page path.
        assert!(
            cpu.perf_counters().data_direct_reads > 0,
            "{mode:?}: reads never reached the direct-page path -- fixture is vacuous"
        );
        assert!(
            cpu.perf_counters().data_direct_writes > 0,
            "{mode:?}: writes never reached the direct-page path -- fixture is vacuous"
        );
        assert!(!cpu.jit_fast_map.has_read_mapping(0x3000, 0x3000));
        assert!(!cpu.jit_fast_map.has_write_mapping(0x3000, 0x3000));
    }
}

/// BLOCKING regression test (adversarial review of the lever-1 slice): a LIVE mode switch away
/// from an Approximate persona -- e.g. `OUT 0xE1` selecting 386-slow mid-run, izarravm-machine
/// `bus.rs`/`run.rs` -- must stop the interpreter's FastMap serve path immediately, both the hits
/// AND the probe itself (misses), proving `fast_map_serve_enabled` gates ENTRY to
/// `fast_map_data_slot`, not merely population. Before this fix, `set_mode` never touched the
/// FastMap at all: `FastMap::storage` stays allocated forever once created, so gating on
/// `has_storage()` stayed true after the switch -- every post-switch access on the Accurate
/// persona kept paying the full preamble (reinstating the steady-state regression this whole
/// slice exists to fix) while surviving live entries could even keep SERVING from the wrong
/// persona (a transient violation of "386-slow/386 Accurate never enter it, by construction").
/// The final assertion checks the gate is a live mirror, not a one-way latch: switching back to
/// an Approximate persona must re-arm the fast path without needing to repopulate from scratch.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn live_mode_switch_to_an_accurate_persona_stops_the_fast_map_probe() {
    const LINEAR: u32 = 0x0000_3000;
    let mut memory = vec![0u8; 0x6000];
    memory[LINEAR as usize..LINEAR as usize + 4].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = CpuGsw::default(); // Gsw586 by default: an Approximate persona.
    cpu.set_jit_auto_admit(true);

    // Populate, then confirm the fast path is actually live on the Approximate persona before
    // the switch (otherwise the test proves nothing about the transition).
    for _ in 0..2 {
        cpu.read_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            LINEAR,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )
        .unwrap();
    }
    assert!(
        cpu.fast_map_probe_counters().hits > 0,
        "fixture did not warm the fast path before the switch"
    );
    assert!(cpu.jit_fast_map.has_read_mapping(LINEAR, LINEAR));

    cpu.set_mode(GswMode::Gsw386Slow);

    let hits_before = cpu.fast_map_probe_counters().hits;
    let misses_before = cpu.fast_map_probe_counters().misses;
    for _ in 0..4 {
        cpu.read_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            LINEAR,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )
        .unwrap();
        cpu.write_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            LINEAR,
            OperandSize::Dword,
            0x5566_7788,
            BusAccessKind::DataWrite,
        )
        .unwrap();
    }

    assert_eq!(
        cpu.fast_map_probe_counters().hits,
        hits_before,
        "hits kept increasing after a live switch to an Accurate persona"
    );
    assert_eq!(
        cpu.fast_map_probe_counters().misses,
        misses_before,
        "the probe itself kept running after the switch -- fast_map_serve_enabled did not go \
         false, so the guaranteed-miss preamble cost is back"
    );
    // Vacuousness check now that neither counter moves: the accesses still reached the
    // direct-page path.
    assert!(cpu.perf_counters().data_direct_reads > 0);
    assert!(cpu.perf_counters().data_direct_writes > 0);

    // Switching back to an Approximate persona must re-arm the fast path. The FastMap's own
    // entries were never invalidated by the mode switch (only the serve gate closed), so this
    // hits immediately without needing to repopulate.
    cpu.set_mode(GswMode::Gsw586);
    let hits_before_return = cpu.fast_map_probe_counters().hits;
    cpu.read_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        LINEAR,
        OperandSize::Dword,
        BusAccessKind::DataRead,
    )
    .unwrap();
    assert!(
        cpu.fast_map_probe_counters().hits > hits_before_return,
        "fast path did not re-arm after switching back to an Approximate persona"
    );
}

// --- end lever 1 tests -------------------------------------------------------------------------

#[test]
#[cfg(feature = "jit")]
fn pending_flags_offset() {
    // Pending flags offset for direct native writes; shifts whenever PerfCounters or CpuGsw's
    // other fields grow or shrink. The lever-1 slice's interp_fast_map_hits/_misses counters live
    // in FastMapProbeCounters at the CpuGsw tail instead (see that type), to avoid moving this
    // pin. The dynarec-refactor Task 2 region-JIT deletion drops the `jit_regions:
    // jit::RegionTable` field from `CpuGsw` (RegionTable itself is gone) and four PerfCounters
    // fields (jit_region_entries, jit_region_insns, jit_native_block_ns,
    // jit_native_block_samples), moving this pin from 4528 to 4456 (measured via a failing-test
    // readout, not derived: rustc's field reordering is not guaranteed to move linearly with a
    // struct's size change). Task 3b then deletes four more dead region-only PerfCounters fields
    // (jit_native_insns, jit_helper_exits, jit_native_memory_helpers, jit_table_clears; 32 bytes),
    // moving this pin from 4456 to 4424 -- again measured against a failing test, matching the
    // sibling pin in `arch_payload_keeps_pending_flags_offset_pinned` (canonical_state_test.rs).
    // The mutable-imm-lane slice adds four PerfCounters fields (the lane registration, accept and
    // two rejection-reason counters; 32 bytes), moving this pin back from 4424 to 4456, measured
    // the same way. They belong in PerfCounters rather than at the CpuGsw tail because they are
    // the slice's diagnostic trio and have to appear in the probe JSON alongside the SMC counters. The Phase 5
    // call-out slice adds `native_callout: CallOutTable` to `CpuGsw` (a raw pointer and a usize;
    // 16 bytes), moving this pin from 4456 to 4472, measured the same way. Slice 1 of the
    // rejected-row campaign adds the PUSHAD and POPAD helpers, so `CallOutTable` gains two more
    // function-pointer `usize`s (16 bytes) and this pin moves from 4472 to 4488 -- measured, not
    // derived. Three pointers rather than one dispatching trampoline is deliberate: the emitted
    // slot stays one plain quadword load and one indirect call, with no per-call-out branch on
    // 20 M doom executions. The fatal-fault diagnostics slice adds `fault_site: FaultSite` to
    // `CpuGsw` (an `Option` around a `SegmentRegister`, a `u32` and a `bool`; 24 bytes), moving
    // this pin from 4488 to 4512 -- measured, not derived. Declaring it at the struct tail does
    // NOT keep the pin, because repr(Rust) reorders fields by alignment: source position buys
    // nothing here, so it is written at the tail for readability (it is cold, written at most
    // once per run) rather than for layout.
    // The invalidation-scan counters (smc_scan_calls, smc_scan_keys) add two u64 fields to
    // PerfCounters and move this pin from 4512 to 4528 -- measured, not derived. They belong in
    // PerfCounters rather than at the CpuGsw tail because they have to appear in the probe JSON
    // beside the other SMC counters, which is where the invalidation cost is read.
    // The R15 table-bases slice adds `native_table_slots: NativeTableSlots` ([usize; 6],
    // 48 bytes) so emitted code can load table bases R15-relative instead of baking imm64s --
    // it must be a by-value CpuGsw field: behind a Box the emitted load would need a second,
    // dependent indirection, which is half the point of the slice gone. Its first position
    // (mid-struct, beside native_callout) moved this pin 4528 -> 4576, and the one-lookup
    // slice's growth to [usize; 24] moved it again to 4720 -- where the quiet-window gate
    // measured a uniform ~2-5% doom regression with byte-identical counters: every hot
    // interpreter field after the array had shifted cache lines. The array now lives at the
    // struct TAIL (fault_site's precedent) and this pin is back at its pre-R15 value --
    // measured, not derived. Do not let this array migrate mid-struct again.
    // The decode-line first-touch slice adds one `Box<[DecodePack]>` to `DecodeCache` (16 bytes
    // of fat pointer, mid-struct because the cache is a by-value field), moving this pin 4528 ->
    // 4544 -- measured, not derived. It is a pointer, not a payload: the array it addresses is a
    // separate 1 MB allocation whose whole purpose is to be the resident one.
    // Dropping MMX removes the `[u64; 8]` MM register file from `X87`, 64 bytes of state that sat
    // ahead of the hot interpreter fields, moving this pin 4544 -> 4480. Unlike the growth events
    // above this is a SHRINK of dead architectural state, so it pulls the fields after it toward
    // the front of the struct rather than pushing them apart -- but it is still a cache-line
    // reshuffle of the hot region, so it is measured by the fixture sweep, not assumed inert.
    // The arena-size slice adds one PerfCounters field (jit_direct_arena_compaction_ns; 8 bytes),
    // moving this pin 4480 -> 4488 -- measured off a failing-test readout, not derived. It belongs
    // in PerfCounters rather than at the CpuGsw tail because the phase-mark series carries
    // `PerfCounters` by value and this counter's whole purpose is to appear per-interval beside
    // jit_direct_arena_compactions, which is where compaction wall is read.
    assert_eq!(core::mem::offset_of!(CpuGsw, pending_flags), 4488);
}

/// Measure fully register-allocated native code against the interpreter. Runs a
/// 32-bit flat drawcolumn-shaped loop
/// (15 instructions, 7 memory ops) through the REAL interpreter (`run_straight_line`) and through
/// a hand-emitted native version that keeps every guest register in a host register and folds the
/// texture base into a host pointer so each guest base+index memory operand lowers to one host SIB
/// access (no per-access address-add, which would clobber the loop's live flags). The two runs
/// execute on identical fresh memory and their framebuffers are compared byte-for-byte, so a
/// codegen bug fails the test instead of faking a speed number. The speedup is an OPTIMISTIC
/// ceiling (best-case register allocation + raw-pointer memory vs a lean TestBus interpreter); the
/// realistic dynarec lands below it. Run:
///   cargo test -j8 -p izarravm-cpu --release --features jit g0_prime_cpu_ceiling -- --ignored --nocapture
#[cfg(feature = "jit")]
#[test]
#[ignore]
fn g0_prime_cpu_ceiling_probe() {
    use crate::jit::encoder::{Encoder, Reg};
    use crate::jit::exec_mem::ExecutableBuffer;
    use std::time::Instant;

    const CODE: u32 = 0x0000;
    const TEX: u32 = 0x1000; // 512-byte texture region
    const COUNT_ADDR: u32 = 0x3000;
    const FB: u32 = 0x0010_0000; // framebuffer
    const STRIDE: u32 = 0x50; // guest edi advance per iteration (bytes)
    const STEP1: u32 = 0x0134_5677;
    const STEP2: u32 = 0x0023_4561;
    const EBP0: u32 = 0x1234_5678;
    const ITERS: u32 = 200_000;
    const TRIALS: usize = 7;
    const FB_LEN: usize = ITERS as usize * STRIDE as usize;
    const MEM_LEN: usize = FB as usize + FB_LEN + 0x1000;

    // --- guest loop bytes (32-bit); a trailing HLT ends run_straight_line ---
    let mut code: Vec<u8> = Vec::new();
    code.extend_from_slice(&[0x8B, 0xCD]); // mov ecx,ebp
    code.extend_from_slice(&[0x81, 0xC5]); // add ebp,STEP1
    code.extend_from_slice(&STEP1.to_le_bytes());
    code.extend_from_slice(&[0x89, 0x07]); // mov [edi],eax
    code.extend_from_slice(&[0xC1, 0xE9, 0x18]); // shr ecx,24
    code.extend_from_slice(&[0x8B, 0xD5]); // mov edx,ebp
    code.extend_from_slice(&[0x81, 0xC5]); // add ebp,STEP2
    code.extend_from_slice(&STEP2.to_le_bytes());
    code.extend_from_slice(&[0x89, 0x5F, 0x04]); // mov [edi+4],ebx
    code.extend_from_slice(&[0xC1, 0xEA, 0x18]); // shr edx,24
    code.extend_from_slice(&[0x8B, 0x04, 0x0E]); // mov eax,[esi+ecx]
    code.extend_from_slice(&[0x81, 0xC7]); // add edi,STRIDE
    code.extend_from_slice(&STRIDE.to_le_bytes());
    code.extend_from_slice(&[0x8B, 0x1C, 0x16]); // mov ebx,[esi+edx]
    code.extend_from_slice(&[0xFF, 0x0D]); // dec dword [COUNT_ADDR]
    code.extend_from_slice(&COUNT_ADDR.to_le_bytes());
    code.extend_from_slice(&[0x8B, 0x04, 0x0E]); // mov eax,[esi+ecx]
    code.extend_from_slice(&[0x8B, 0x1C, 0x16]); // mov ebx,[esi+edx]
    let jnz_at = code.len();
    let rel = (0i32 - (jnz_at as i32 + 2)) as i8; // back to CODE (offset 0)
    code.extend_from_slice(&[0x75, rel as u8]); // jnz entry
    code.push(0xF4); // hlt
    assert!(
        code.len() < TEX as usize,
        "code overruns the texture region"
    );

    let build_mem = || {
        let mut m = vec![0u8; MEM_LEN];
        m[..code.len()].copy_from_slice(&code);
        for i in 0..512u32 {
            m[(TEX + i) as usize] = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        m[COUNT_ADDR as usize..COUNT_ADDR as usize + 4].copy_from_slice(&ITERS.to_le_bytes());
        m
    };

    let seg = |selector: u16, access: u8| SegmentRegister {
        selector,
        base: 0,
        limit: 0xffff_ffff,
        access,
        default_size_32: true,
    };
    let setup = |cpu: &mut CpuGsw| {
        cpu.control.cr0 |= CR0_PE;
        cpu.registers.set_segment(SegmentIndex::Cs, seg(0x08, 0x9b)); // 32-bit code
        cpu.registers.set_segment(SegmentIndex::Ds, seg(0x10, 0x93)); // data
        cpu.registers.set_segment(SegmentIndex::Ss, seg(0x10, 0x93));
        cpu.registers.set_segment(SegmentIndex::Es, seg(0x10, 0x93));
        cpu.registers.eip = CODE;
        cpu.registers.set_eax(0);
        cpu.registers.set_ebx(0);
        cpu.registers.set_ecx(0);
        cpu.registers.set_edx(0);
        cpu.registers.set_ebp(EBP0);
        cpu.registers.set_esi(TEX);
        cpu.registers.set_edi(FB);
    };

    // --- native emission: guest regs pinned in host regs, memory via host pointers ---
    // ebp=R8 ecx=R9 edx=R10 eax=R11 ebx=RBX ; esi_host=R12 (ram+TEX) edi_host=R13 (ram+FB) count=R14
    // arg0(RCX)=ram_base, arg1(RDX)=iters.
    let native = {
        let mut e = Encoder::new();
        e.push(Reg::RBX);
        e.push(Reg::R12);
        e.push(Reg::R13);
        e.push(Reg::R14);
        e.mov_r64_r64(Reg::R12, Reg::RCX);
        e.add_r64_imm32(Reg::R12, TEX); // esi_host = ram_base + TEX
        e.mov_r64_r64(Reg::R13, Reg::RCX);
        e.add_r64_imm32(Reg::R13, FB); // edi_host = ram_base + FB
        e.mov_r32_r32(Reg::R14, Reg::RDX); // count = iters
        e.mov_r32_imm32(Reg::R8, EBP0); // ebp
        e.mov_r32_imm32(Reg::R9, 0); // ecx
        e.mov_r32_imm32(Reg::R10, 0); // edx
        e.mov_r32_imm32(Reg::R11, 0); // eax
        e.mov_r32_imm32(Reg::RBX, 0); // ebx
        let top = e.label();
        e.place(top);
        e.mov_r32_r32(Reg::R9, Reg::R8); // mov ecx,ebp
        e.add_r32_imm32(Reg::R8, STEP1); // add ebp,STEP1
        e.store_r32_disp8(Reg::R13, 0, Reg::R11); // mov [edi],eax
        e.shr_r32_imm8(Reg::R9, 24); // shr ecx,24
        e.mov_r32_r32(Reg::R10, Reg::R8); // mov edx,ebp
        e.add_r32_imm32(Reg::R8, STEP2); // add ebp,STEP2
        e.store_r32_disp8(Reg::R13, 4, Reg::RBX); // mov [edi+4],ebx
        e.shr_r32_imm8(Reg::R10, 24); // shr edx,24
        e.load_r32_sib(Reg::R11, Reg::R12, Reg::R9); // mov eax,[esi+ecx]
        e.add_r64_imm32(Reg::R13, STRIDE); // add edi,STRIDE (host ptr)
        e.load_r32_sib(Reg::RBX, Reg::R12, Reg::R10); // mov ebx,[esi+edx]
        e.add_r32_imm32(Reg::R14, 0xFFFF_FFFF); // dec count (sets ZF)
        e.load_r32_sib(Reg::R11, Reg::R12, Reg::R9); // mov eax,[esi+ecx] (no flag change)
        e.load_r32_sib(Reg::RBX, Reg::R12, Reg::R10); // mov ebx,[esi+edx] (no flag change)
        e.jnz(top); // jnz entry
        e.pop(Reg::R14);
        e.pop(Reg::R13);
        e.pop(Reg::R12);
        e.pop(Reg::RBX);
        e.ret();
        ExecutableBuffer::new(&e.finish()).expect("W^X alloc must succeed on the dev host")
    };
    type NativeFn = unsafe extern "C" fn(*mut u8, u32);
    let native_fn: NativeFn = unsafe { std::mem::transmute(native.entry_ptr()) };

    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let insns = 15u64 * ITERS as u64;
    let mut interp_ns = Vec::new();
    let mut native_ns = Vec::new();
    for trial in 0..=TRIALS {
        // interpreter: run_straight_line chains a bounded number of instructions per call then
        // returns (a non-continuable insn / the final HLT ends the run), exactly as under the
        // machine. Drive it until the guest loop counter reaches 0.
        let mut cpu = CpuGsw::default();
        setup(&mut cpu);
        let mut bus = TestBus::with_memory(build_mem());
        // TestBus defaults to Full tracing (an unbounded per-access cycle Vec) — a test
        // instrumentation cost the real MachineBus never pays. Disable it so the interpreter
        // baseline is representative. (Residual caveat: TestBus still lacks MachineBus's cached
        // raw-pointer direct-page path, so it is marginally slower than the real bus — a small
        // bias in the native side's favor, noted in the results.)
        bus.trace.set_tracing_mode(izarravm_bus::TracingMode::Off);
        let count_of = |bus: &TestBus| {
            u32::from_le_bytes(
                bus.memory[COUNT_ADDR as usize..COUNT_ADDR as usize + 4]
                    .try_into()
                    .unwrap(),
            )
        };
        let t = Instant::now();
        let mut calls = 0u64;
        loop {
            let out = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
            calls += 1;
            if out.halted || count_of(&bus) == 0 {
                break;
            }
            assert!(
                calls < ITERS as u64 * 4 + 1000,
                "run_straight_line not converging (calls={calls}, left={})",
                count_of(&bus)
            );
        }
        let ins = t.elapsed().as_secs_f64();
        let left = count_of(&bus);
        assert_eq!(
            left, 0,
            "loop did not run all {ITERS} iterations (count={left})"
        );
        if trial == 0 {
            eprintln!(
                "(interp chaining: {calls} run_straight_line calls for {ITERS} iters = {:.1} insns/call)",
                insns as f64 / calls as f64
            );
        }

        // native (identical fresh memory)
        let mut memn = build_mem();
        let t = Instant::now();
        unsafe { native_fn(memn.as_mut_ptr(), ITERS) };
        let nns = t.elapsed().as_secs_f64();

        // correctness: framebuffers must be byte-identical
        let a = &bus.memory[FB as usize..FB as usize + FB_LEN];
        let b = &memn[FB as usize..FB as usize + FB_LEN];
        if a != b {
            let idx = a.iter().zip(b).position(|(x, y)| x != y).unwrap();
            panic!(
                "native framebuffer diverges from interpreter at FB+{idx}: interp={} native={}",
                a[idx], b[idx]
            );
        }

        if trial > 0 {
            // discard trial 0 (cold host caches)
            interp_ns.push(ins / insns as f64 * 1e9);
            native_ns.push(nns / insns as f64 * 1e9);
        }
    }
    let mi = median(interp_ns);
    let mn = median(native_ns);
    eprintln!("\n=== CPU ceiling probe ({ITERS} iters x 15 insns, median of {TRIALS}) ===");
    eprintln!("interpreter : {mi:.3} ns/guest-insn");
    eprintln!("native (best-case, reg-allocated + raw-ptr mem) : {mn:.3} ns/guest-insn");
    eprintln!(
        "SPEEDUP CEILING : {:.2}x   [4x = 'already very good' bar]",
        mi / mn
    );
    eprintln!("=== end memory-mixed probe ===\n");
}

/// Dispatch-only companion to the CPU ceiling probe. A 15-instruction register-only loop
/// (no memory operands at all, so ZERO bus involvement — the TestBus memory-path caveat cannot
/// apply) isolating the interpreter's pure per-instruction dispatch/decode/flag/clock-accounting
/// overhead vs native register ops. This anchors that the memory-mixed interpreter figure is real
/// and not a bus artifact. Correctness is checked by comparing final guest register values.
#[cfg(feature = "jit")]
#[test]
#[ignore]
fn g0_prime_dispatch_ceiling_probe() {
    use crate::jit::encoder::{Encoder, Reg};
    use crate::jit::exec_mem::ExecutableBuffer;
    use std::time::Instant;

    const STEP1: u32 = 0x0134_5677;
    const STEP2: u32 = 0x0023_4561;
    const EBP0: u32 = 0x1234_5678;
    const ITERS: u32 = 300_000;
    const TRIALS: usize = 7;

    // register-only guest loop (counter in edi); trailing HLT.
    let mut code: Vec<u8> = Vec::new();
    code.extend_from_slice(&[0x8B, 0xCD]); // mov ecx,ebp
    code.extend_from_slice(&[0x81, 0xC5]);
    code.extend_from_slice(&STEP1.to_le_bytes()); // add ebp,STEP1
    code.extend_from_slice(&[0xC1, 0xE9, 0x18]); // shr ecx,24
    code.extend_from_slice(&[0x8B, 0xD5]); // mov edx,ebp
    code.extend_from_slice(&[0x81, 0xC5]);
    code.extend_from_slice(&STEP2.to_le_bytes()); // add ebp,STEP2
    code.extend_from_slice(&[0xC1, 0xEA, 0x18]); // shr edx,24
    code.extend_from_slice(&[0x8B, 0xC1]); // mov eax,ecx
    code.extend_from_slice(&[0x81, 0xC0]);
    code.extend_from_slice(&STEP1.to_le_bytes()); // add eax,STEP1
    code.extend_from_slice(&[0xC1, 0xE8, 0x03]); // shr eax,3
    code.extend_from_slice(&[0x8B, 0xDA]); // mov ebx,edx
    code.extend_from_slice(&[0x81, 0xC3]);
    code.extend_from_slice(&STEP2.to_le_bytes()); // add ebx,STEP2
    code.extend_from_slice(&[0xC1, 0xEB, 0x03]); // shr ebx,3
    code.extend_from_slice(&[0x81, 0xC6]);
    code.extend_from_slice(&STEP1.to_le_bytes()); // add esi,STEP1
    code.extend_from_slice(&[0x81, 0xC7]);
    code.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // add edi,-1 (dec, sets ZF)
    let jnz_at = code.len();
    let rel = (0i32 - (jnz_at as i32 + 2)) as i8;
    code.extend_from_slice(&[0x75, rel as u8]); // jnz entry
    code.push(0xF4); // hlt

    let seg = |selector: u16, access: u8| SegmentRegister {
        selector,
        base: 0,
        limit: 0xffff_ffff,
        access,
        default_size_32: true,
    };

    // native register-only emission; final regs written to out[0..6] = eax,ebx,ecx,edx,ebp,esi.
    let native = {
        let mut e = Encoder::new();
        e.push(Reg::RBX);
        e.push(Reg::R12);
        e.push(Reg::R14);
        e.push(Reg::R15);
        e.mov_r32_r32(Reg::R14, Reg::RCX); // count = iters (arg0)
        e.mov_r64_r64(Reg::R15, Reg::RDX); // out ptr (arg1)
        e.mov_r32_imm32(Reg::R8, EBP0); // ebp
        e.mov_r32_imm32(Reg::R9, 0); // ecx
        e.mov_r32_imm32(Reg::R10, 0); // edx
        e.mov_r32_imm32(Reg::R11, 0); // eax
        e.mov_r32_imm32(Reg::RBX, 0); // ebx
        e.mov_r32_imm32(Reg::R12, 0); // esi
        let top = e.label();
        e.place(top);
        e.mov_r32_r32(Reg::R9, Reg::R8); // mov ecx,ebp
        e.add_r32_imm32(Reg::R8, STEP1);
        e.shr_r32_imm8(Reg::R9, 24);
        e.mov_r32_r32(Reg::R10, Reg::R8); // mov edx,ebp
        e.add_r32_imm32(Reg::R8, STEP2);
        e.shr_r32_imm8(Reg::R10, 24);
        e.mov_r32_r32(Reg::R11, Reg::R9); // mov eax,ecx
        e.add_r32_imm32(Reg::R11, STEP1);
        e.shr_r32_imm8(Reg::R11, 3);
        e.mov_r32_r32(Reg::RBX, Reg::R10); // mov ebx,edx
        e.add_r32_imm32(Reg::RBX, STEP2);
        e.shr_r32_imm8(Reg::RBX, 3);
        e.add_r32_imm32(Reg::R12, STEP1); // add esi,STEP1
        e.add_r32_imm32(Reg::R14, 0xFFFF_FFFF); // dec edi (counter), sets ZF
        e.jnz(top);
        e.store_r32_disp8(Reg::R15, 0, Reg::R11); // out[0]=eax
        e.store_r32_disp8(Reg::R15, 4, Reg::RBX); // out[1]=ebx
        e.store_r32_disp8(Reg::R15, 8, Reg::R9); // out[2]=ecx
        e.store_r32_disp8(Reg::R15, 12, Reg::R10); // out[3]=edx
        e.store_r32_disp8(Reg::R15, 16, Reg::R8); // out[4]=ebp
        e.store_r32_disp8(Reg::R15, 20, Reg::R12); // out[5]=esi
        e.pop(Reg::R15);
        e.pop(Reg::R14);
        e.pop(Reg::R12);
        e.pop(Reg::RBX);
        e.ret();
        ExecutableBuffer::new(&e.finish()).expect("W^X alloc must succeed")
    };
    type NativeFn = unsafe extern "C" fn(u32, *mut u32);
    let native_fn: NativeFn = unsafe { std::mem::transmute(native.entry_ptr()) };

    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let insns = 15u64 * ITERS as u64;
    let mut interp_ns = Vec::new();
    let mut native_ns = Vec::new();
    for trial in 0..=TRIALS {
        let mut cpu = CpuGsw::default();
        cpu.control.cr0 |= CR0_PE;
        cpu.registers.set_segment(SegmentIndex::Cs, seg(0x08, 0x9b));
        cpu.registers.set_segment(SegmentIndex::Ds, seg(0x10, 0x93));
        cpu.registers.set_segment(SegmentIndex::Ss, seg(0x10, 0x93));
        cpu.registers.eip = 0;
        cpu.registers.set_eax(0);
        cpu.registers.set_ebx(0);
        cpu.registers.set_ecx(0);
        cpu.registers.set_edx(0);
        cpu.registers.set_ebp(EBP0);
        cpu.registers.set_esi(0);
        cpu.registers.set_edi(ITERS);
        let mut mem = vec![0u8; 0x1000];
        mem[..code.len()].copy_from_slice(&code);
        let mut bus = TestBus::with_memory(mem);
        bus.trace.set_tracing_mode(izarravm_bus::TracingMode::Off);
        let t = Instant::now();
        let mut calls = 0u64;
        loop {
            let out = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
            calls += 1;
            if out.halted || cpu.registers.edi() == 0 {
                break;
            }
            assert!(calls < ITERS as u64 * 4 + 1000, "not converging");
        }
        let ins = t.elapsed().as_secs_f64();
        assert_eq!(cpu.registers.edi(), 0, "loop did not complete");

        let mut out = [0u32; 6];
        let t = Instant::now();
        unsafe { native_fn(ITERS, out.as_mut_ptr()) };
        let nns = t.elapsed().as_secs_f64();

        // correctness: final registers must match.
        let interp_regs = [
            cpu.registers.eax(),
            cpu.registers.ebx(),
            cpu.registers.ecx(),
            cpu.registers.edx(),
            cpu.registers.ebp(),
            cpu.registers.esi(),
        ];
        assert_eq!(
            out, interp_regs,
            "native register-only result diverges from interpreter"
        );

        if trial > 0 {
            interp_ns.push(ins / insns as f64 * 1e9);
            native_ns.push(nns / insns as f64 * 1e9);
        }
    }
    let mi = median(interp_ns);
    let mn = median(native_ns);
    eprintln!("\n=== Dispatch-only ceiling (register-only loop) ===");
    eprintln!("interpreter : {mi:.3} ns/guest-insn");
    eprintln!("native      : {mn:.3} ns/guest-insn");
    eprintln!("DISPATCH SPEEDUP CEILING : {:.2}x", mi / mn);
    eprintln!("=== end dispatch-only probe ===\n");
}

/// Measure the per-slot bookkeeping cost. The register cache alone is not the limiting factor; the per-slot
/// fetch/cap CALLs back into Rust are the floor (the region is wall-neutral with the interpreter
/// precisely because native emitted code cannot inline the bus/cpu work the Rust trampoline
/// inlines). This measures the per-slot BOOKKEEPING cost (fetch charge + clock accumulate +
/// cross-multiplied cap check) under the three candidate models, the variable that decides the
/// emitted path. Combined with the native compute and interpreter
/// (~96 ns/insn) numbers, it gives the drawcolumn per-insn estimate for each model. Throwaway.
///   cargo test -j8 -p izarravm-cpu --release --features jit s2_bookkeeping -- --ignored --nocapture
#[cfg(feature = "jit")]
#[test]
#[ignore]
fn s2_bookkeeping_model_spike() {
    use crate::jit::encoder::{Encoder, Reg};
    use crate::jit::exec_mem::ExecutableBuffer;
    use std::time::Instant;

    #[repr(C)]
    struct SpikeState {
        raw_clocks: u64, // off 0
        bus_accum: u64,  // off 8
        cap: u64,        // off 16
        num: u64,        // off 24
    }
    const FETCH: u32 = 2; // representative RAM I-cache fetch wait-state (machine/lib.rs:9799)
    const NUM: u32 = 1; // 586 timing numerator

    // One slot's realistic bookkeeping: fetch charge + clock accumulate + cross-mult cap check.
    unsafe extern "C" fn book_one(s: *mut SpikeState) -> u8 {
        let s = unsafe { &mut *s };
        s.bus_accum += FETCH as u64;
        s.raw_clocks += 2;
        u8::from(s.raw_clocks.wrapping_mul(s.num).wrapping_add(s.bus_accum) >= s.cap)
    }
    // The same, batched: n slots in one call (amortizes the CALL over the block iteration).
    unsafe extern "C" fn book_batch(s: *mut SpikeState, n: u32) -> u8 {
        let s = unsafe { &mut *s };
        for _ in 0..n {
            s.bus_accum += FETCH as u64;
            s.raw_clocks += 2;
            if s.raw_clocks.wrapping_mul(s.num).wrapping_add(s.bus_accum) >= s.cap {
                return 1;
            }
        }
        0
    }
    let one_addr = (book_one as unsafe extern "C" fn(*mut SpikeState) -> u8) as usize as u64;
    let batch_addr =
        (book_batch as unsafe extern "C" fn(*mut SpikeState, u32) -> u8) as usize as u64;

    // Model 1 (today's region): one Rust bookkeeping CALL per slot. win64 arg0=RCX(state).
    let call_per_slot = {
        let mut e = Encoder::new();
        e.push(Reg::RBX);
        e.push(Reg::R14);
        e.push(Reg::R15);
        e.sub_r64_imm32(Reg::RSP, 32); // shadow space; RSP 16-aligned before the CALL
        e.mov_r64_r64(Reg::R15, Reg::RCX); // state
        e.mov_r64_r64(Reg::R14, Reg::RDX); // iters
        e.mov_r64_imm64(Reg::RBX, one_addr);
        let top = e.label();
        e.place(top);
        e.mov_r64_r64(Reg::RCX, Reg::R15);
        e.call_r64(Reg::RBX);
        e.add_r64_imm32(Reg::R14, 0xFFFF_FFFF); // dec (sets ZF)
        e.jnz(top);
        e.add_r64_imm32(Reg::RSP, 32);
        e.pop(Reg::R15);
        e.pop(Reg::R14);
        e.pop(Reg::RBX);
        e.ret();
        ExecutableBuffer::new(&e.finish()).expect("W^X on a supported host")
    };

    // Model 2 (Option A): bookkeeping inline, accumulators cached in host registers (no CALL).
    let native_per_slot = {
        let mut e = Encoder::new();
        e.push(Reg::RSI);
        e.push(Reg::R12);
        e.push(Reg::R13);
        e.push(Reg::R14);
        e.push(Reg::R15);
        e.mov_r64_r64(Reg::R15, Reg::RCX); // state
        e.mov_r64_r64(Reg::R14, Reg::RDX); // iters
        e.load_r64_disp8(Reg::R12, Reg::R15, 0); // raw_clocks
        e.load_r64_disp8(Reg::R13, Reg::R15, 8); // bus_accum
        e.load_r64_disp8(Reg::RSI, Reg::R15, 16); // cap
        let top = e.label();
        let exit = e.label();
        e.place(top);
        e.add_r64_imm32(Reg::R12, 2); // raw += 2
        e.add_r64_imm32(Reg::R13, FETCH); // bus += fetch cost
        e.mov_r64_r64(Reg::RAX, Reg::R12);
        e.imul_r64_imm32(Reg::RAX, NUM); // raw * num
        e.add_r64_r64(Reg::RAX, Reg::R13); // + bus term
        e.cmp_r64_r64(Reg::RAX, Reg::RSI); // vs cap
        e.jae(exit);
        e.add_r64_imm32(Reg::R14, 0xFFFF_FFFF);
        e.jnz(top);
        e.place(exit);
        e.store_r64_disp8(Reg::R15, 0, Reg::R12);
        e.store_r64_disp8(Reg::R15, 8, Reg::R13);
        e.pop(Reg::R15);
        e.pop(Reg::R14);
        e.pop(Reg::R13);
        e.pop(Reg::R12);
        e.pop(Reg::RSI);
        e.ret();
        ExecutableBuffer::new(&e.finish()).expect("W^X on a supported host")
    };

    // Model 3 (Option B): one Rust CALL per block-iteration doing n slots' bookkeeping.
    let batched = {
        let mut e = Encoder::new();
        e.push(Reg::RBX);
        e.push(Reg::R14);
        e.push(Reg::R15);
        e.sub_r64_imm32(Reg::RSP, 32);
        e.mov_r64_r64(Reg::R15, Reg::RCX); // state
        e.mov_r64_r64(Reg::R14, Reg::RDX); // iters (= SLOTS / 15)
        e.mov_r64_imm64(Reg::RBX, batch_addr);
        let top = e.label();
        e.place(top);
        e.mov_r64_r64(Reg::RCX, Reg::R15);
        e.mov_r32_imm32(Reg::RDX, 15); // n slots / iteration
        e.call_r64(Reg::RBX);
        e.add_r64_imm32(Reg::R14, 0xFFFF_FFFF);
        e.jnz(top);
        e.add_r64_imm32(Reg::RSP, 32);
        e.pop(Reg::R15);
        e.pop(Reg::R14);
        e.pop(Reg::RBX);
        e.ret();
        ExecutableBuffer::new(&e.finish()).expect("W^X on a supported host")
    };

    type Fn2 = unsafe extern "C" fn(*mut SpikeState, u64);
    const SLOTS: u64 = 15_000_000;
    const TRIALS: usize = 7;
    let run = |buf: &ExecutableBuffer, iters: u64| -> f64 {
        let f: Fn2 = unsafe { std::mem::transmute(buf.entry_ptr()) };
        let mut st = SpikeState {
            raw_clocks: 0,
            bus_accum: 0,
            cap: u64::MAX, // never fires: every model processes all SLOTS
            num: NUM as u64,
        };
        let t = Instant::now();
        unsafe { f(&mut st, iters) };
        t.elapsed().as_secs_f64() / SLOTS as f64 * 1e9 // ns per slot
    };
    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let (mut c, mut n, mut b) = (Vec::new(), Vec::new(), Vec::new());
    for trial in 0..=TRIALS {
        let tc = run(&call_per_slot, SLOTS);
        let tn = run(&native_per_slot, SLOTS);
        let tb = run(&batched, SLOTS / 15);
        if trial > 0 {
            c.push(tc);
            n.push(tn);
            b.push(tb);
        }
    }
    let (mc, mn, mb) = (median(c), median(n), median(b));
    eprintln!("\n=== Bookkeeping models (ns per slot, median of {TRIALS}) ===");
    eprintln!("1. call-per-slot  (today's region model)     : {mc:.3} ns/slot");
    eprintln!("2. native-per-slot (Option A, cached accum)  : {mn:.3} ns/slot");
    eprintln!("3. batched CALL/iter (Option B, 15 slots)    : {mb:.3} ns/slot");
    eprintln!("--- drawcolumn per-insn estimate = ~0.38 ns native compute + bookkeeping/slot ---");
    eprintln!("current region (wall-neutral w/ interp)      : ~96 ns/insn");
    eprintln!(
        "Option A (native per-slot)  : {:.2} ns/insn  => {:.0}x over current",
        0.38 + mn,
        96.0 / (0.38 + mn)
    );
    eprintln!(
        "Option B (batched)          : {:.2} ns/insn  => {:.0}x over current  (+ omitted spill/reload)",
        0.38 + mb,
        96.0 / (0.38 + mb)
    );
    eprintln!("=== end bookkeeping models ===\n");
}

#[test]
fn scale_clocks_batches_exactly() {
    // The JIT accumulates raw core_clocks across a straight-line block and scales ONCE at
    // block exit. That is bit-identical to per-instruction scaling because scale_clocks is
    // exact long division with a remainder carry. Verified across every mode, several clock
    // sequences, and a non-zero starting remainder. A regression here silently breaks the
    // JIT's cyc/iter identity, so this guards the property.
    let seqs: [&[u32]; 3] = [
        &[3, 5, 1, 1, 61, 2, 7, 4, 9, 2],
        &[1; 32],
        &[255, 1, 100, 3, 17, 61, 61, 2],
    ];
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        for start_rem in [0u64, 1, 7, 100] {
            for seq in seqs {
                let mut indiv = CpuGsw::default();
                indiv.set_mode(mode);
                indiv.timing_rem = start_rem;
                let mut batch = CpuGsw::default();
                batch.set_mode(mode);
                batch.timing_rem = start_rem;

                let sum_individual: u64 = seq.iter().map(|&c| indiv.scale_clocks(c)).sum();
                let total: u32 = seq.iter().sum();
                let batched = batch.scale_clocks(total);

                assert_eq!(
                    sum_individual, batched,
                    "mode {mode:?} rem {start_rem} seq {seq:?}: per-insn sum != batched"
                );
                assert_eq!(
                    indiv.timing_rem, batch.timing_rem,
                    "mode {mode:?} rem {start_rem} seq {seq:?}: remainder carry diverged"
                );
            }
        }
    }
}

/// A compiled block containing x87 operations carries `fp_rem` alongside the
/// integer `timing_rem`, and must batch both into one
/// block-exit flush with the SAME carry as per-op scaling, or the block's guest cycle count
/// diverges from the interpreter. Unlike integer clocks (one per-level numerator), FP ops have
/// PER-CLASS numerators, so the batched form weights each op by its class before summing; the
/// shared `FP_TIMING_DEN` is what keeps the single `fp_rem` carry exact across mixed classes.
/// This pins `Σ scale_fp_clocks == floor((Σ clocks·num_class + rem0) / DEN)` with the final
/// remainder `(Σ clocks·num_class + rem0) % DEN`. This guards against `scale_fp_clocks` dropping
/// the shared-denominator property the batch relies on.
#[test]
fn scale_fp_clocks_batches_exactly() {
    use FpOpClass::{F32Mem, F64Mem, IntConvert16, IntConvert32, Register, Wait};
    let seqs: [&[(u32, FpOpClass)]; 3] = [
        &[
            (4, IntConvert32),
            (1, Register),
            (3, F64Mem),
            (2, IntConvert16),
            (1, Register),
        ],
        &[(1, Register); 20],
        &[
            (7, F32Mem),
            (2, IntConvert32),
            (9, Register),
            (1, Wait),
            (5, IntConvert16),
            (3, F64Mem),
        ],
    ];
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        for start_rem in [0u64, 1, 5, 7] {
            for seq in seqs {
                let mut indiv = CpuGsw::default();
                indiv.set_mode(mode);
                indiv.fp_rem = start_rem;
                let sum_individual: u64 = seq
                    .iter()
                    .map(|&(c, cl)| u64::from(indiv.scale_fp_clocks(c, cl)))
                    .sum();

                // Closed-form batched value: sum the per-op class-weighted numerators, then one
                // exact division with the single carried remainder.
                let weighted: u64 = seq
                    .iter()
                    .map(|&(c, cl)| u64::from(c) * u64::from(fp_timing_class(mode.persona(), cl)))
                    .sum();
                let scaled = weighted + start_rem;
                let batched = scaled / u64::from(FP_TIMING_DEN);
                let final_rem = scaled % u64::from(FP_TIMING_DEN);

                assert_eq!(
                    sum_individual, batched,
                    "mode {mode:?} rem {start_rem}: per-op FP sum != batched"
                );
                assert_eq!(
                    indiv.fp_rem, final_rem,
                    "mode {mode:?} rem {start_rem}: fp_rem carry diverged"
                );
            }
        }
    }
}

#[test]
fn test_bus_compiled_window_applies_fixed_costs_and_vga_effects() {
    let mut bus = TestBus::with_memory(vec![0; 0x10_0000]);
    assert!(bus.begin_compiled_window().is_none());
    bus.direct_pages_enabled = true;
    bus.uniform_native_fetches = true;
    bus.direct_page_clocks = true;
    bus.trace.set_tracing_mode(TracingMode::Off);

    let window = bus.begin_compiled_window().unwrap();
    assert_eq!(window.mapping_epoch(), 1);
    assert_eq!(window.fetch_raw_clocks(), 2);
    assert_eq!(window.ram_raw_clocks(BusWidth::Dword), 5);
    assert_eq!(window.vga_raw_clocks(BusWidth::Dword), 9);
    let mut delta = CompiledBusDelta::default();
    delta.add_instruction_fetches(3);
    delta.add_ram_accesses(BusWidth::Dword, 2);
    delta.add_vga_reads(BusWidth::Word, 1);
    delta.add_vga_writes(izarravm_bus::NativeVgaWrites {
        dirty_pages: 0b0100,
        byte_writes: 1,
        word_writes: 2,
        dword_writes: 3,
    });
    let raw = window.delta_raw_clocks(&delta);
    bus.finish_compiled_window(window, delta);

    assert_eq!(bus.trace.elapsed_clocks(), raw);
    assert_eq!(bus.mode13_dirty_pages, 0b0100);
    assert_eq!(bus.mode13_byte_writes, 1);
    assert_eq!(bus.mode13_word_writes, 2);
    assert_eq!(bus.mode13_dword_writes, 3);
}

/// The defining property of the scoped VGA aperture invalidation: the aperture's cached pointers
/// go, and NOTHING else does. A VGA register write cannot move a RAM host pointer, and keeping
/// those is the entire win -- 88.1% of the 43.3M entries a doom timedemo used to discard here were
/// RAM. The epoch check is the load-bearing half: `has_read_mapping` alone would still pass if the
/// machine had advanced the global mapping epoch, and every surviving entry would then be dead on
/// the next interpreter probe anyway.
#[test]
fn a_vga_aperture_change_drops_the_aperture_and_keeps_ram_live_at_the_same_epoch() {
    let mut cpu = CpuGsw::default();
    let mut bus = TestBus::with_memory(vec![0; 0x10_0000]);
    bus.direct_pages_enabled = true;
    bus.direct_pages_writable = true;
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    cpu.jit_direct.set_fast_map_enabled_for_test(true);

    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0x2000, BusAccessKind::DataRead)
        .unwrap();
    cpu.load_segment_real(SegmentIndex::Ds, 0xa000);
    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead)
        .unwrap();
    let epoch = cpu.data_read_pages.mapping_epoch();
    assert_eq!(epoch, bus.direct_mapping_epoch);

    cpu.note_direct_data_map_changed();

    // The aperture re-pointed: its pointers are void.
    assert!(cpu.data_read_pages.get(0x000a_0000).is_none());
    // RAM did not move, and the cache keeps the epoch that certifies the survivors.
    assert!(cpu.data_read_pages.get(0x2000).is_some());
    assert_eq!(cpu.data_read_pages.mapping_epoch(), epoch);
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    {
        assert!(!cpu.jit_fast_map.has_read_mapping(0x000a_0000, 0x000a_0000));
        assert!(cpu.jit_fast_map.has_read_mapping(0x2000, 0x2000));
        assert!(
            cpu.jit_fast_map
                .has_read_mapping_at_epoch(0x2000, 0x2000, epoch)
        );
    }
}

/// The COARSE cause keeps its coarse scope. A bus/PCI memory-decode change really can move a RAM
/// host pointer, so scoping the wrong one of the two would be silent corruption.
#[test]
fn a_bus_decode_change_still_drops_ram_and_vga_direct_pages() {
    let mut cpu = CpuGsw::default();
    let mut bus = TestBus::with_memory(vec![0; 0x10_0000]);
    bus.direct_pages_enabled = true;
    bus.direct_pages_writable = true;
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    cpu.jit_direct.set_fast_map_enabled_for_test(true);

    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0x2000, BusAccessKind::DataRead)
        .unwrap();
    cpu.load_segment_real(SegmentIndex::Ds, 0xa000);
    cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead)
        .unwrap();
    assert!(cpu.data_read_pages.get(0x2000).is_some());
    assert!(cpu.data_read_pages.get(0x000a_0000).is_some());
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    {
        assert!(cpu.jit_fast_map.has_read_mapping(0x2000, 0x2000));
        assert!(cpu.jit_fast_map.has_read_mapping(0x000a_0000, 0x000a_0000));
    }

    cpu.note_direct_map_changed();

    assert!(cpu.data_read_pages.get(0x2000).is_none());
    assert!(cpu.data_read_pages.get(0x000a_0000).is_none());
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    {
        assert!(!cpu.jit_fast_map.has_read_mapping(0x2000, 0x2000));
        assert!(!cpu.jit_fast_map.has_read_mapping(0x000a_0000, 0x000a_0000));
    }
}

#[derive(Default)]
struct TestBus {
    // Aligned like the production `Memory` backing, and for the same reason: `direct_page`
    // below hands `memory.as_mut_ptr()` pages to the fast map, and an unaligned backing would
    // silently poison every store-bias entry — the whole one-lookup fixture suite would pass
    // while exercising only the slow path (the fixtures-that-cannot-fail trap, design D7).
    memory: izarravm_bus::PageAlignedBytes,
    trace: BusTrace,
    pending_irq: Option<u8>,
    // Mirrors the machine's `io_touched`: set by any port access, so `requires_step_break`
    // reports the same step-break edge the real bus does.
    io_touched: bool,
    // When true, `read_io` does NOT set `io_touched`, modeling the machine's Approximate-class lazy
    // status-port path (MachineBus::read_io's 3DA/3BA/3C2 arm), so poll-loop chaining across an IN
    // can be exercised through the CPU alone. Writes still set io_touched. Default false.
    lazy_io_reads: bool,
    // What `read_io` hands back. `None` keeps the historical constant 0; a value lets a port-read
    // differential fixture see a byte actually land in AL (a call-out that never wrote the
    // destination would pass against a bus that always returns 0).
    io_read_value: Option<u32>,
    // When true, `read_io` fails with `UnsupportedPort` -- the machine bus's only `read_io` error
    // producer, and the second member of the call-out helper's abnormal set.
    io_read_fails: bool,
    // A SEQUENCE of port-read values, one consumed per `read_io`, falling back to `io_read_value`
    // once exhausted. `io_read_value` is a CONSTANT, which cannot separate "the device was read
    // again" from "the first value was cached"; a native block that re-executes must observe the
    // fresh device value on every execution. See the varying-device row in
    // cpu_jit_callout_matrix_test.rs.
    io_read_sequence: Vec<u32>,
    io_read_cursor: usize,
    // Every `read_io` in order, as `(port, core_clocks_so_far)`: the device-visible read order and
    // timestamps, which the call-out matrix compares between the native and the block-free
    // interpreted role. `last_read_io_core_clocks_so_far` keeps only the most recent one and so
    // cannot see an order or a count difference.
    io_reads: Vec<(u16, u64)>,
    // Records the `core_clocks_so_far` the CPU threaded into the most recent `read_io` call, so
    // tests can assert on it (see core_clocks_so_far_reflects_prior_instructions_not_the_in_flight).
    last_read_io_core_clocks_so_far: Option<u64>,
    last_write_io_core_clocks_so_far: Option<u64>,
    // When true, `direct_page` hands out host-pointer pages into `memory` (mirroring the production
    // MachineBus), so data accesses take the CPU's cached host-pointer deref path instead of the
    // slow `read_memory_direct` fallback. Default false (the historical no-direct-page behavior).
    direct_pages_enabled: bool,
    direct_pages_writable: bool,
    direct_write_denied_page: Option<u32>,
    // G4: deny direct pages under InstructionPrefetch only, modeling a non-RAM code page.
    deny_instruction_prefetch_direct_page: bool,
    uniform_native_fetches: bool,
    // Opt-in width-sensitive timing for direct-page tests. Historical TestBus direct pages were
    // timing-free, so keep that default and let direct-memory differential tests request clocks.
    direct_page_clocks: bool,
    // Opt-in batch-clock reporting for tight event-budget tests. Historical CPU tests leave it
    // off because their TestBus predates machine-level combined core/bus caps.
    report_batch_clocks: bool,
    // The (num, den) the batch-clock reporting scales raw bus clocks by, mirroring MachineBus's
    // batch-start snapshot of `bus_timing`. Default (1, 1) keeps every historical test on the
    // identity scaling it was written against; the cap-screen tests set a real persona ratio so
    // the screen's bound is looser than the exact test and the fall-through arm is exercised.
    batch_bus_scale: (u64, u64),
    page_walk_bound_available: bool,
    rep_data_byte_cost_override: Option<u64>,
    direct_memory_max_clock_override: Option<u64>,
    project_additional_bus_clocks: bool,
    native_aggregate_accounting_disabled: bool,
    jit_cached_fetch_requests: std::cell::RefCell<Vec<(u32, u32)>>,
    fail_write_address: Option<u32>,
    mode13_dirty_pages: u16,
    mode13_byte_writes: u64,
    mode13_word_writes: u64,
    mode13_dword_writes: u64,
    direct_mapping_epoch: u64,
}

impl TestBus {
    fn with_memory(memory: Vec<u8>) -> Self {
        Self {
            memory: memory.into(),
            trace: BusTrace::default(),
            pending_irq: None,
            io_touched: false,
            lazy_io_reads: false,
            io_read_value: None,
            io_read_fails: false,
            io_read_sequence: Vec::new(),
            io_read_cursor: 0,
            io_reads: Vec::new(),
            last_read_io_core_clocks_so_far: None,
            last_write_io_core_clocks_so_far: None,
            direct_pages_enabled: false,
            direct_pages_writable: true,
            direct_write_denied_page: None,
            deny_instruction_prefetch_direct_page: false,
            uniform_native_fetches: false,
            direct_page_clocks: false,
            report_batch_clocks: false,
            batch_bus_scale: (1, 1),
            page_walk_bound_available: true,
            rep_data_byte_cost_override: None,
            direct_memory_max_clock_override: None,
            project_additional_bus_clocks: false,
            native_aggregate_accounting_disabled: false,
            jit_cached_fetch_requests: std::cell::RefCell::new(Vec::new()),
            fail_write_address: None,
            mode13_dirty_pages: 0,
            mode13_byte_writes: 0,
            mode13_word_writes: 0,
            mode13_dword_writes: 0,
            direct_mapping_epoch: 1,
        }
    }

    fn direct_page_wait_states(width: BusWidth) -> u8 {
        match width {
            BusWidth::Byte => 0,
            BusWidth::Word => 1,
            BusWidth::Dword => 3,
        }
    }

    fn mode13_wait_states(width: BusWidth) -> u8 {
        match width {
            BusWidth::Byte => 4,
            BusWidth::Word => 5,
            BusWidth::Dword => 7,
        }
    }

    fn note_mode13_write(&mut self, address: u32, width: BusWidth) {
        if !(0x000a_0000..0x000b_0000).contains(&address) {
            return;
        }
        self.mode13_dirty_pages |= 1 << ((address - 0x000a_0000) >> 12);
        match width {
            BusWidth::Byte => self.mode13_byte_writes += 1,
            BusWidth::Word => self.mode13_word_writes += 1,
            BusWidth::Dword => self.mode13_dword_writes += 1,
        }
    }
}

impl CpuBus for TestBus {
    fn read_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<u32, BusError> {
        self.trace.push(BusCycle::new(kind, address, width, 0));
        let start = address as usize;
        let end = start
            .checked_add(width.bytes() as usize)
            .ok_or(BusError::UnmappedMemory { address })?;
        if end > self.memory.len() {
            return Err(BusError::UnmappedMemory { address });
        }
        Ok(match width {
            BusWidth::Byte => u32::from(self.memory[start]),
            BusWidth::Word => u32::from(u16::from_le_bytes([
                self.memory[start],
                self.memory[start + 1],
            ])),
            BusWidth::Dword => u32::from_le_bytes([
                self.memory[start],
                self.memory[start + 1],
                self.memory[start + 2],
                self.memory[start + 3],
            ]),
        })
    }

    fn write_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        self.trace.push(BusCycle::new(kind, address, width, 0));
        if self.fail_write_address == Some(address) {
            return Err(BusError::UnmappedMemory { address });
        }
        let start = address as usize;
        let end = start
            .checked_add(width.bytes() as usize)
            .ok_or(BusError::UnmappedMemory { address })?;
        if end > self.memory.len() {
            return Err(BusError::UnmappedMemory { address });
        }
        match width {
            BusWidth::Byte => self.memory[start] = value as u8,
            BusWidth::Word => {
                self.memory[start..start + 2].copy_from_slice(&(value as u16).to_le_bytes())
            }
            BusWidth::Dword => self.memory[start..start + 4].copy_from_slice(&value.to_le_bytes()),
        }
        Ok(())
    }

    fn read_memory_direct(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<DirectMemoryRead, BusError> {
        if self.direct_memory_bytes(address, width.bytes() as usize, width, kind)
            == width.bytes() as usize
        {
            return self
                .read_memory(address, width, kind)
                .map(|value| DirectMemoryRead {
                    value,
                    direct: true,
                });
        }
        self.read_memory(address, width, kind)
            .map(|value| DirectMemoryRead {
                value,
                direct: false,
            })
    }

    fn write_memory_direct(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> Result<DirectMemoryWrite, BusError> {
        if self.direct_memory_bytes(address, width.bytes() as usize, width, kind)
            == width.bytes() as usize
        {
            self.write_memory(address, width, value, kind)?;
            return Ok(DirectMemoryWrite { direct: true });
        }
        self.write_memory(address, width, value, kind)?;
        Ok(DirectMemoryWrite { direct: false })
    }

    fn read_memory_bytes_direct(
        &mut self,
        address: u32,
        out: &mut [u8],
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<usize, BusError> {
        if kind != BusAccessKind::DataRead {
            return Ok(0);
        }
        if self.direct_memory_bytes(address, out.len(), width, kind) != out.len() {
            return Ok(0);
        }
        let access = width.bytes() as usize;
        for offset in (0..out.len()).step_by(access) {
            self.trace
                .push(BusCycle::new(kind, address + offset as u32, width, 0));
        }
        let start = address as usize;
        out.copy_from_slice(&self.memory[start..start + out.len()]);
        Ok(out.len())
    }

    fn write_memory_bytes_direct(
        &mut self,
        address: u32,
        data: &[u8],
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<usize, BusError> {
        if kind != BusAccessKind::DataWrite {
            return Ok(0);
        }
        if self.direct_memory_bytes(address, data.len(), width, kind) != data.len() {
            return Ok(0);
        }
        let access = width.bytes() as usize;
        for offset in (0..data.len()).step_by(access) {
            self.trace
                .push(BusCycle::new(kind, address + offset as u32, width, 0));
        }
        let start = address as usize;
        self.memory[start..start + data.len()].copy_from_slice(data);
        Ok(data.len())
    }

    fn direct_memory_bytes(
        &self,
        address: u32,
        bytes: usize,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> usize {
        if !matches!(kind, BusAccessKind::DataRead | BusAccessKind::DataWrite)
            || bytes == 0
            || !bytes.is_multiple_of(width.bytes() as usize)
            || (address as usize & 0x0fff) + bytes > 0x1000
        {
            return 0;
        }
        if matches!(width, BusWidth::Word) && address & 1 != 0
            || matches!(width, BusWidth::Dword) && address & 3 != 0
        {
            return 0;
        }
        let start = address as usize;
        if start
            .checked_add(bytes)
            .is_some_and(|end| end <= self.memory.len())
        {
            bytes
        } else {
            0
        }
    }

    fn prefetch_memory(&mut self, address: u32, out: &mut [u8]) -> Result<usize, BusError> {
        let start = address as usize;
        if start >= self.memory.len() {
            return Err(BusError::UnmappedMemory { address });
        }
        let len = out.len().min(self.memory.len() - start);
        out[..len].copy_from_slice(&self.memory[start..start + len]);
        Ok(len)
    }

    fn charge_instruction_fetch(&mut self, address: u32) -> Result<(), BusError> {
        self.trace.push(BusCycle::new(
            BusAccessKind::InstructionPrefetch,
            address,
            BusWidth::Byte,
            0,
        ));
        Ok(())
    }

    // The trait default charges a fetch run byte-by-byte (one cross-crate call + push per
    // byte), whose call overhead dominates JIT-region wall-clock microbenchmarks. This override
    // is bit-identical to that default loop in EVERY accounting field (clocks, access count,
    // Full-mode detail) but does it in one op, so no existing test changes. It does NOT
    // reproduce the production MachineBus, which collapses a cacheable-RAM run to ONE access at
    // the code-fetch wait state (this keeps `count` byte accesses at wait-state 0); the
    // microbenchmark runs tracing Off, so it measures wall clock, not the fetch-clock total.
    // Do not treat this TestBus's instruction-fetch clock accounting as production-representative.
    fn charge_physical_instruction_fetch_run(
        &mut self,
        physical_start: u32,
        count: u32,
    ) -> Result<(), BusError> {
        self.trace.record_instruction_fetch_run(
            physical_start,
            if self.uniform_native_fetches && count != 0 {
                1
            } else {
                count
            },
            0,
        );
        Ok(())
    }

    fn jit_fetch_cost_clocks(&self) -> u64 {
        u64::from(self.uniform_native_fetches) * 2
    }

    /// The two dial flags are plain public fields that fixtures flip between runs, and at least
    /// one existing fixture flips `uniform_native_fetches` after priming a block
    /// (`generated_three_block_chain_aggregates_across_event_caps`). Fingerprint both, so the
    /// Direct backend's memo of the derived worst-case hop cost cannot go stale. Offset by one to
    /// stay clear of the trait default's 0.
    fn jit_cost_dial_epoch(&self) -> u64 {
        1 + u64::from(self.uniform_native_fetches) + 2 * u64::from(self.direct_page_clocks)
    }

    fn native_fetches_are_uniform(&self) -> bool {
        self.uniform_native_fetches
    }

    fn native_aggregate_accounting_allowed(&self) -> bool {
        !self.native_aggregate_accounting_disabled
    }

    fn jit_data_cost_clocks(&self, width: BusWidth) -> u64 {
        if self.direct_page_clocks {
            u64::from(izarravm_bus::BusCycle::clocks_for(
                width,
                Self::direct_page_wait_states(width),
            ))
        } else {
            0
        }
    }

    fn in_batch_scaled_bus_clocks(&self) -> u64 {
        if self.report_batch_clocks {
            let (num, den) = self.batch_bus_scale;
            self.trace.elapsed_clocks() * num / den
        } else {
            0
        }
    }

    fn in_batch_raw_bus_clocks(&self) -> u64 {
        if self.report_batch_clocks {
            self.trace.elapsed_clocks()
        } else {
            0
        }
    }

    fn in_batch_scaled_bus_clocks_screen_scale(&self) -> u64 {
        if self.report_batch_clocks {
            let (num, den) = self.batch_bus_scale;
            num.div_ceil(den).max(1)
        } else {
            0
        }
    }

    fn jit_mode13_data_cost_clocks(&self, width: BusWidth) -> u64 {
        if self.direct_page_clocks {
            u64::from(izarravm_bus::BusCycle::clocks_for(
                width,
                Self::mode13_wait_states(width),
            ))
        } else {
            0
        }
    }

    fn rep_page_walk_cost_upper(&self) -> Option<u64> {
        self.page_walk_bound_available
            .then_some(if self.report_batch_clocks { 8 } else { 0 })
    }

    fn rep_data_byte_cost_upper(&self) -> u64 {
        self.rep_data_byte_cost_override.unwrap_or_else(|| {
            self.jit_data_cost_clocks(BusWidth::Byte)
                .max(self.jit_mode13_data_cost_clocks(BusWidth::Byte))
        })
    }

    fn charge_native_mode13_writes(&mut self, writes: izarravm_bus::NativeMode13Writes) {
        self.mode13_dirty_pages |= writes.dirty_pages;
        self.mode13_byte_writes += writes.byte_writes;
        self.mode13_word_writes += writes.word_writes;
        self.mode13_dword_writes += writes.dword_writes;
        let clocks = self
            .jit_mode13_data_cost_clocks(BusWidth::Byte)
            .saturating_mul(writes.byte_writes)
            .saturating_add(
                self.jit_mode13_data_cost_clocks(BusWidth::Word)
                    .saturating_mul(writes.word_writes),
            )
            .saturating_add(
                self.jit_mode13_data_cost_clocks(BusWidth::Dword)
                    .saturating_mul(writes.dword_writes),
            );
        self.trace.add_elapsed_clocks(clocks);
    }

    fn charge_bus_clocks_bulk(&mut self, clocks: u64) {
        self.trace.add_elapsed_clocks(clocks);
    }

    fn charge_direct_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        if kind == BusAccessKind::DataWrite {
            self.note_mode13_write(address, width);
        }
        if self.direct_page_clocks {
            let wait_states = if (0x000a_0000..0x000b_0000).contains(&address) {
                Self::mode13_wait_states(width)
            } else {
                Self::direct_page_wait_states(width)
            };
            self.trace.record(kind, address, width, wait_states);
        }
        Ok(())
    }

    // Mirrors ONLY the non-Mode13 branch of `charge_direct_memory` above: same wait-state
    // function, same `direct_page_clocks` gate, no `note_mode13_write`. The interpreter's FastMap
    // serve path calls this instead of `charge_direct_memory` once it already knows (from the
    // FastMap's own `PageKind`) that the hit is plain RAM, so a fast-path RAM access and a
    // forced-slow-path RAM access must charge byte-identical clocks through this and the sibling
    // method respectively -- that equivalence is exactly what the lever-1 fidelity tests check.
    fn charge_direct_ram_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        if self.direct_page_clocks {
            self.trace
                .record(kind, address, width, Self::direct_page_wait_states(width));
        }
        Ok(())
    }

    fn jit_direct_memory_max_clocks(&self, _width: BusWidth, _kind: BusAccessKind) -> Option<u64> {
        Some(self.direct_memory_max_clock_override.unwrap_or(0))
    }

    fn jit_cached_fetch_run_clocks(&self, start: u32, count: u32) -> Option<u64> {
        self.jit_cached_fetch_requests
            .borrow_mut()
            .push((start, count));
        Some(u64::from(count) * 2)
    }

    fn jit_projected_batch_scaled_bus_clocks(&self, additional_raw: u64) -> Option<u64> {
        Some(if self.project_additional_bus_clocks {
            self.in_batch_scaled_bus_clocks()
                .saturating_add(additional_raw)
        } else {
            0
        })
    }

    // Hand out a host-pointer page into `memory`, mirroring MachineBus::direct_page, so the
    // CPU's data_read_pages/data_write_pages caches populate and subsequent accesses are host
    // derefs. Gated: off by default (the default trait None keeps every existing test on the
    // slow read_memory_direct path with its trace cycles), on only for the JIT microbenchmark.
    fn direct_page(
        &mut self,
        address: u32,
        kind: BusAccessKind,
    ) -> Result<Option<DirectPage>, BusError> {
        if !self.direct_pages_enabled {
            return Ok(None);
        }
        if self.deny_instruction_prefetch_direct_page && kind == BusAccessKind::InstructionPrefetch
        {
            return Ok(None);
        }
        let physical_page = address & !0x0fff;
        if kind == BusAccessKind::DataWrite
            && (!self.direct_pages_writable || self.direct_write_denied_page == Some(physical_page))
        {
            return Ok(None);
        }
        let start = physical_page as usize;
        if start + 0x1000 > self.memory.len() {
            return Ok(None);
        }
        Ok(Some(DirectPage {
            physical_page,
            ptr: unsafe { self.memory.as_mut_ptr().add(start) },
            len: 0x1000,
            writable: matches!(kind, BusAccessKind::DataWrite) && self.direct_pages_writable,
            mapping_epoch: self.direct_mapping_epoch,
        }))
    }

    fn begin_compiled_window(&mut self) -> Option<CompiledBusWindow> {
        if !self.direct_pages_enabled
            || !self.uniform_native_fetches
            || self.native_aggregate_accounting_disabled
        {
            return None;
        }
        CompiledBusWindow::certify(
            self.direct_mapping_epoch,
            self.trace.tracing_mode(),
            self.jit_fetch_cost_clocks(),
            [
                self.jit_data_cost_clocks(BusWidth::Byte),
                self.jit_data_cost_clocks(BusWidth::Word),
                self.jit_data_cost_clocks(BusWidth::Dword),
            ],
            [
                self.jit_mode13_data_cost_clocks(BusWidth::Byte),
                self.jit_mode13_data_cost_clocks(BusWidth::Word),
                self.jit_mode13_data_cost_clocks(BusWidth::Dword),
            ],
            self.trace.elapsed_clocks(),
            0,
            1,
            1,
        )
    }

    fn finish_compiled_window(&mut self, window: CompiledBusWindow, delta: CompiledBusDelta) {
        debug_assert_eq!(window.mapping_epoch(), self.direct_mapping_epoch);
        debug_assert_eq!(window.batch_raw_clocks(), self.trace.elapsed_clocks());
        let writes = delta.vga_writes();
        self.mode13_dirty_pages |= writes.dirty_pages;
        self.mode13_byte_writes = self.mode13_byte_writes.saturating_add(writes.byte_writes);
        self.mode13_word_writes = self.mode13_word_writes.saturating_add(writes.word_writes);
        self.mode13_dword_writes = self.mode13_dword_writes.saturating_add(writes.dword_writes);
        self.trace
            .add_elapsed_clocks(window.delta_raw_clocks(&delta));
    }

    fn read_io(
        &mut self,
        port: u16,
        width: BusWidth,
        core_clocks_so_far: u64,
        _cpu_is_ring0_pm: bool,
    ) -> Result<u32, BusError> {
        if self.io_read_fails {
            return Err(BusError::UnsupportedPort { port });
        }
        if !self.lazy_io_reads {
            self.io_touched = true;
        }
        self.last_read_io_core_clocks_so_far = Some(core_clocks_so_far);
        self.io_reads.push((port, core_clocks_so_far));
        self.trace.push(BusCycle::new(
            BusAccessKind::IoRead,
            u32::from(port),
            width,
            0,
        ));
        let sequenced = self.io_read_sequence.get(self.io_read_cursor).copied();
        if sequenced.is_some() {
            self.io_read_cursor += 1;
        }
        Ok(sequenced.or(self.io_read_value).unwrap_or(0))
    }

    fn write_io(
        &mut self,
        port: u16,
        width: BusWidth,
        _value: u32,
        core_clocks_so_far: u64,
        _cpu_is_ring0_pm: bool,
    ) -> Result<(), BusError> {
        self.io_touched = true;
        self.last_write_io_core_clocks_so_far = Some(core_clocks_so_far);
        self.trace.push(BusCycle::new(
            BusAccessKind::IoWrite,
            u32::from(port),
            width,
            0,
        ));
        Ok(())
    }

    fn interrupt_acknowledge(&mut self, vector: u8, _ax: u16) -> Result<(), BusError> {
        self.trace.push(BusCycle::new(
            BusAccessKind::InterruptAcknowledge,
            u32::from(vector),
            BusWidth::Byte,
            0,
        ));
        Ok(())
    }

    fn interrupt_pending(&self) -> bool {
        self.pending_irq.is_some()
    }

    fn acknowledge_interrupt(&mut self) -> Option<u8> {
        self.pending_irq.take()
    }

    fn requires_step_break(&self) -> bool {
        self.io_touched
    }
}

/// Give a real-mode `TestBus` a full 1 MiB image (room for a wrap-safe stack and every
/// vector's IVT slot) and point vector 0 (#DE) at a distinguishing trap address, then run
/// one `cycle` and assert the CPU landed there -- i.e. the fault was DELIVERED through
/// `real_mode_interrupt`, not raised as a host-fatal error. Batch A converted #DE from
/// `CpuError::DivideError` (host-fatal) to `InternalFault::Exception { vector: 0, .. }`
/// (guest-deliverable), so a real-mode DIV-by-zero now runs to completion (`cycle` returns
/// `Ok`) with CS:IP retargeted at the IVT's vector-0 entry instead of erroring `cycle` itself.
const DE_TRAP_CS: u16 = 0x0200;
const DE_TRAP_IP: u16 = 0x0010;
// Code origin for the de_trap_bus helpers: away from offset 0, which is the vector-0 IVT
// slot these buses populate (code and the IVT slot must not overlap).
const DE_CODE_ORIGIN: u32 = 0x20;

fn expect_de_delivered<B: CpuBus>(cpu: &mut CpuGsw, bus: &mut B) {
    let outcome = cpu
        .cycle(bus)
        .expect("a delivered #DE must not error `cycle`");
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.cs().selector, DE_TRAP_CS);
    assert_eq!(cpu.registers.eip, u32::from(DE_TRAP_IP));
}

fn de_trap_bus(code: &[u8]) -> TestBus {
    let mut memory = vec![0u8; 0x1_0000];
    let origin = DE_CODE_ORIGIN as usize;
    memory[origin..origin + code.len()].copy_from_slice(code);
    // IVT[0] (bytes 0..4): IP then CS, little-endian.
    memory[0..2].copy_from_slice(&DE_TRAP_IP.to_le_bytes());
    memory[2..4].copy_from_slice(&DE_TRAP_CS.to_le_bytes());
    TestBus::with_memory(memory)
}

fn cpl3_code(code: &[u8]) -> (CpuGsw, TestBus) {
    // Protected mode with a flat CPL-3 code segment (selector RPL 3), the same shape
    // the #AC/CPUID privilege tests use, but loaded with arbitrary code.
    let mut memory = vec![0u8; 256];
    memory[..code.len()].copy_from_slice(code);
    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: true,
        },
    );
    cpu.registers.eip = 0;
    // This helper builds CPL-3 state directly (no transfer instruction runs), so the
    // cached `cpl` must be seeded to match the CS RPL it just installed by hand.
    cpu.cpl = 3;
    (cpu, TestBus::with_memory(memory))
}

/// Run one instruction through the production decode/execute split and return the raw
/// `InternalFault` (without exception delivery), so a test can assert `is_ok()`/`unwrap_err()`
/// directly on the result. This is the single per-instruction entry the test suite uses now that
/// the transitional fused reference is gone: it is exactly what `cycle` runs, minus the
/// interrupt-service prologue and the exception-delivery epilogue.
fn exec_one_split<B: CpuBus>(cpu: &mut CpuGsw, bus: &mut B) -> ExecResult<CycleOutcome> {
    cpu.begin_instruction();
    let insn = cpu.decode(bus)?;
    cpu.execute_decoded(&insn, bus)
}

fn real_mode_cpu(code: &[u8], mem_len: usize) -> (CpuGsw, Vec<u8>) {
    let mut memory = vec![0u8; mem_len];
    memory[..code.len()].copy_from_slice(code);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    (cpu, memory)
}

/// Shared seed for the seam differential / golden batteries below: a fixed real-mode register
/// set plus a known word at [0x20], so each instruction has stable inputs.
fn seam_seed(cpu: &mut CpuGsw) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0102);
    cpu.write_reg16(Reg16::Bx, 0x0010);
    cpu.write_reg16(Reg16::Cx, 0x0304);
    cpu.write_reg16(Reg16::Dx, 0x0506);
    cpu.write_reg16(Reg16::Si, 0x0008);
    cpu.write_reg16(Reg16::Di, 0x0018);
    cpu.write_reg16(Reg16::Bp, 0x0010);
}

fn seam_fetch_count(bus: &TestBus) -> usize {
    bus.trace
        .cycles()
        .iter()
        .filter(|c| c.kind == BusAccessKind::InstructionPrefetch)
        .count()
}

/// Protected-mode CPU with a GDT (base 0x100, limit 0x1f) holding one descriptor
/// at selector 0x08. CS selector 0 => CPL 0.
fn protected_cpu(code: &[u8], descriptor_low: u32, descriptor_high: u32) -> (CpuGsw, Vec<u8>) {
    let mut memory = vec![0u8; 0x200];
    memory[..code.len()].copy_from_slice(code);
    memory[0x108..0x10c].copy_from_slice(&descriptor_low.to_le_bytes());
    memory[0x10c..0x110].copy_from_slice(&descriptor_high.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.gdtr = DescriptorTable {
        base: 0x100,
        limit: 0x1f,
    };
    cpu.control.cr0 |= CR0_PE;
    (cpu, memory)
}

// ---- V86 monitor test harness -------------------------------------------------
// Memory map (physical == linear, identity paged):
//   0x00000 IVT area; 0x01000 page directory; 0x02000 page table 0 (identity,
//   present+rw+user); 0x03000 GDT; 0x04000 IDT; 0x05000 TSS (+ I/O bitmap);
//   ESP0 = 0x07000 (ring-0 stack, flat SS base 0); 0x08000 monitor code;
//   V86 guest: SS=0x0900, CS=0x0A00 (code at phys 0xA000).
// GDT selectors: 0x08 ring0 code (32-bit), 0x10 ring0 data/stack, 0x18 TSS.
const GDT: u32 = 0x3000;
const IDT: u32 = 0x4000;
const TSS: u32 = 0x5000;
const R0_CS: u16 = 0x08;
const R0_SS: u16 = 0x10;
const TSS_SEL: u16 = 0x18;
const MON_CODE: u32 = 0x8000;
const ESP0: u32 = 0x7000;

fn put32(m: &mut [u8], off: u32, v: u32) {
    m[off as usize..off as usize + 4].copy_from_slice(&v.to_le_bytes());
}
fn put16(m: &mut [u8], off: u32, v: u16) {
    m[off as usize..off as usize + 2].copy_from_slice(&v.to_le_bytes());
}
fn descriptor(base: u32, limit: u32, access: u8, gran: u8) -> [u8; 8] {
    let mut d = [0u8; 8];
    d[0..2].copy_from_slice(&(limit as u16).to_le_bytes());
    d[2..4].copy_from_slice(&(base as u16).to_le_bytes());
    d[4] = (base >> 16) as u8;
    d[5] = access;
    d[6] = ((limit >> 16) as u8 & 0x0f) | (gran & 0xf0);
    d[7] = (base >> 24) as u8;
    d
}
fn int_gate(m: &mut [u8], vector: u8, offset: u32) {
    let base = IDT + u32::from(vector) * 8;
    put16(m, base, offset as u16);
    put16(m, base + 2, R0_CS);
    m[base as usize + 4] = 0;
    m[base as usize + 5] = 0x8e; // present, DPL0, 32-bit interrupt gate
    put16(m, base + 6, (offset >> 16) as u16);
}
fn cpu_mem(bus: &TestBus, addr: u32) -> [u8; 4] {
    let a = addr as usize;
    [
        bus.memory[a],
        bus.memory[a + 1],
        bus.memory[a + 2],
        bus.memory[a + 3],
    ]
}

/// Build the world; CPU sits in protected mode + paging with TR/GDTR/IDTR loaded.
fn v86_world(monitor: &[u8], guest: &[u8], io_bitmap: &[u8]) -> (CpuGsw, TestBus) {
    let mut m = vec![0u8; 0x20000];
    // Identity paging: PDE[0] -> PT at 0x2000; first 0x20 pages identity present+rw+user.
    put32(&mut m, 0x1000, 0x2000 | 0x7);
    for i in 0..0x20u32 {
        put32(&mut m, 0x2000 + i * 4, (i << 12) | 0x7);
    }
    // GDT: null (offset 0), ring0 code 0x9b (sel 0x08), ring0 data 0x93 (sel 0x10),
    // TSS 0x89 (sel 0x18).
    let d = descriptor(0, 0xfffff, 0x9b, 0xc0);
    m[(GDT + 0x08) as usize..(GDT + 0x08) as usize + 8].copy_from_slice(&d);
    let d = descriptor(0, 0xfffff, 0x93, 0xc0);
    m[(GDT + 0x10) as usize..(GDT + 0x10) as usize + 8].copy_from_slice(&d);
    let tss_limit = 0x68 + io_bitmap.len() as u32;
    let d = descriptor(TSS, tss_limit, 0x89, 0x00);
    m[(GDT + 0x18) as usize..(GDT + 0x18) as usize + 8].copy_from_slice(&d);
    // TSS: ESP0, SS0, I/O-map base (word at TSS+0x66), bitmap.
    put32(&mut m, TSS + 4, ESP0);
    put16(&mut m, TSS + 8, R0_SS);
    put16(&mut m, TSS + 0x66, 0x68);
    m[(TSS + 0x68) as usize..(TSS + 0x68) as usize + io_bitmap.len()].copy_from_slice(io_bitmap);
    // IDT: #GP (13) and INT 0x21 -> monitor.
    int_gate(&mut m, 13, MON_CODE);
    int_gate(&mut m, 0x21, MON_CODE);
    m[MON_CODE as usize..MON_CODE as usize + monitor.len()].copy_from_slice(monitor);
    m[0xA000..0xA000 + guest.len()].copy_from_slice(guest);

    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = 0x1000;
    cpu.gdtr.base = GDT;
    cpu.gdtr.limit = 0xff;
    cpu.idtr.base = IDT;
    cpu.idtr.limit = 0xfff;
    cpu.tr = SegmentRegister {
        selector: TSS_SEL,
        base: TSS,
        limit: tss_limit,
        access: 0x89,
        default_size_32: false,
    };
    let bus = TestBus::with_memory(m);
    (cpu, bus)
}

/// Put `cpu` into a V86 task at CS:IP=0x0A00:ip, SS:SP=0x0900:sp, IOPL 0.
/// DS/ES/FS/GS are seeded with sensible defaults; a caller may overwrite them
/// afterward to probe the V86 segment frame (none of them are load-bearing here).
fn enter_v86_direct(cpu: &mut CpuGsw, ip: u32, sp: u32) {
    cpu.registers.eflags = (cpu.registers.eflags & !0x3000) | FLAG_VM | 0x2;
    cpu.registers.eip = ip;
    cpu.registers.set_esp(sp);
    cpu.load_segment_real(SegmentIndex::Cs, 0x0A00);
    cpu.load_segment_real(SegmentIndex::Ss, 0x0900);
    cpu.load_segment_real(SegmentIndex::Ds, 0x0A00);
    cpu.load_segment_real(SegmentIndex::Es, 0x0A00);
    cpu.load_segment_real(SegmentIndex::Fs, 0);
    cpu.load_segment_real(SegmentIndex::Gs, 0);
    // This helper sets EFLAGS.VM directly (no IRET/task-switch transition runs), so
    // the cached `cpl` must be seeded to the fixed V86 level by hand, same as a real
    // transition would leave it.
    cpu.cpl = 3;
}

#[path = "cpu_alu_data_test.rs"]
mod alu_data;
#[path = "cpu_bit_control_test.rs"]
mod bit_control;
#[path = "cpu_bit_system_test.rs"]
mod bit_system;
#[path = "cpu_control_string_test.rs"]
mod control_string;
#[path = "cpu_execution_test.rs"]
mod execution;
#[path = "cpu_fpu_flags_test.rs"]
mod fpu_flags;
#[path = "cpu_legacy_system_test.rs"]
mod legacy_system;
#[path = "cpu_persona_system_test.rs"]
mod persona_system;
#[path = "cpu_stack_branch_test.rs"]
mod stack_branch;
#[path = "cpu_straight_line_test.rs"]
mod straight_line;
#[path = "cpu_strings_segments_test.rs"]
mod strings_segments;
#[path = "cpu_v86_test.rs"]
mod v86;

/// Differential tests for the generic JIT block builder.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_general_test.rs"]
mod jit_general;

/// One byte that the Direct classifier refuses, for fixtures that need a block to STOP at a
/// known offset. CLC: no ModRM, no immediate, continuable, and absent from every arm of
/// `classify`, so the stop reason is `unclassifiable` and the rejected span is exactly one byte.
///
/// It used to be 0x90, until NOP was lowered. Whatever this byte is, it will eventually be
/// lowered too, and CLC is a likelier candidate than most now that `emit_set_cf_only` exists.
/// What makes that safe is `direct_barrier_opcode_is_still_unclassifiable`: a fixture whose
/// barrier quietly stops being a barrier keeps PASSING while certifying nothing, which is worse
/// than a failure. That test is the only thing standing between this constant and a suite full
/// of vacuous assertions, so do not delete it when you change the byte.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
const DIRECT_BARRIER: u8 = 0xf8;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_direct_test.rs"]
mod jit_direct;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_r15_tables_test.rs"]
mod jit_r15_tables;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_fetch_trace_test.rs"]
mod jit_fetch_trace;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_frame_zero_test.rs"]
mod jit_frame_zero;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_store_bias_test.rs"]
mod jit_store_bias;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_load_bias_test.rs"]
mod jit_load_bias;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_compile_outcome_test.rs"]
mod jit_compile_outcome;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_differential_generator_test.rs"]
mod jit_differential_generator;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_x87_direct_test.rs"]
mod jit_x87_direct;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_double_shift_test.rs"]
mod jit_double_shift;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_sweep_lowering_test.rs"]
mod jit_sweep_lowering;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_test_imm_test.rs"]
mod jit_test_imm;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_imm_lane_test.rs"]
mod jit_imm_lane;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_disp_lane_test.rs"]
mod jit_disp_lane;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_callout_test.rs"]
mod jit_callout;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_decode_pack_test.rs"]
mod decode_pack;

/// C1e: `DecodedInsn`'s recorded `{disp_len, imm_len}` pair (design section 1.2, review
/// finding M3) did NOT fit the struct's padding: the size grew 36 -> 40 and is pinned
/// here so the DecodeCache L2 sizing note (updated to ~52 bytes/line, ~208 KB at 4096
/// lines) stays truthful against any further growth.
#[test]
fn decoded_insn_size_is_pinned_after_the_operand_length_pair() {
    assert_eq!(
        core::mem::size_of::<DecodedInsn>(),
        40,
        "DecodedInsn's size is load-bearing for the DecodeCache L2 sizing note; \
         re-verify that note before accepting growth"
    );
}

// --- PodKeyHasher: the BlockKey hasher swap ---------------------------------
//
// The failure mode this guards is specific and would be invisible to every other assertion:
// `U32Hasher::write_u32` OVERWRITES its state, so reusing it for a three-field key would keep
// only the last field written and collapse every `BlockKey` sharing a `mode_key` into one
// bucket. The map would still be correct and would still pass every functional test; it would
// just degrade to a linear scan. These tests fail if that ever happens.

// Hash a REAL `BlockKey` through its derived `Hash`, not a hand-rolled sequence of three
// `write_u32` calls. That distinction is load-bearing: `PodKeyHasher` overrides only
// `write_u32`/`write_u64`/`write_usize`, so any narrower field (say `mode_key` widened to
// `u16`, or a new `u16` added) falls through to the byte-rotate `write` fallback, a different
// and much slower algorithm. A hand-rolled sequence would keep passing while the map itself
// quietly stopped using the fast path.
fn pod_key_hash(linear: u32, physical: u32, mode_key: u32) -> u64 {
    use std::hash::BuildHasher;
    crate::PodKeyBuildHasher::default().hash_one(crate::jit::direct::BlockKey::new(
        linear, physical, mode_key,
    ))
}

#[test]
fn pod_key_hasher_folds_every_field_and_does_not_overwrite() {
    let base = pod_key_hash(1, 2, 3);
    // Each field alone must move the hash. Overwriting semantics would make the first two
    // assertions fail while the third still passed.
    assert_ne!(base, pod_key_hash(9, 2, 3), "linear was not folded in");
    assert_ne!(base, pod_key_hash(1, 9, 3), "physical was not folded in");
    assert_ne!(base, pod_key_hash(1, 2, 9), "mode_key was not folded in");
    // Field order must matter, or permuted keys alias.
    assert_ne!(pod_key_hash(1, 2, 3), pod_key_hash(3, 2, 1));
}

#[test]
fn pod_key_hasher_spreads_the_low_bits_hashbrown_indexes_with() {
    // A Fibonacci multiply concentrates entropy high; hashbrown picks its bucket from the low
    // bits. Without the xor-shift finalizer a realistic key population lands in very few
    // buckets. Model 4096 buckets over page-strided linear/physical pairs, which is what a
    // guest actually produces.
    const BUCKETS: usize = 4096;
    let mut counts = vec![0u32; BUCKETS];
    let mut keys = 0u32;
    for page in 0..512u32 {
        for offset in [0u32, 0x40, 0x80, 0xc0, 0x100, 0x140, 0x180, 0x1c0] {
            let linear = (page << 12) | offset;
            let hash = pod_key_hash(linear, linear.wrapping_add(0x0010_0000), 0);
            counts[(hash as usize) & (BUCKETS - 1)] += 1;
            keys += 1;
        }
    }
    let occupied = counts.iter().filter(|&&c| c != 0).count();
    let worst = *counts.iter().max().unwrap();
    // 4096 keys into 4096 buckets: a sound hash fills well over half and keeps the worst
    // bucket shallow. The overwrite bug would give occupied == 1.
    assert!(
        occupied > BUCKETS / 2,
        "only {occupied} of {BUCKETS} buckets used for {keys} keys"
    );
    assert!(worst <= 8, "worst bucket depth {worst} for {keys} keys");
}
