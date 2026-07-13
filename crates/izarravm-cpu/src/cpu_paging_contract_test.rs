// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

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
    #[cfg(not(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )))]
    let _ = physical;
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    assert!(cpu.jit_direct.range_hits_compiled_code(physical, 1));
}

fn assert_page_walk_code_invalidated(cpu: &CpuGsw, entry: u32, physical: u32) {
    assert!(!cpu.decode_cache.line_live(entry, true));
    #[cfg(not(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )))]
    let _ = physical;
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

fn page_walk_cycles(bus: &TestBus) -> Vec<(BusAccessKind, u32, BusWidth)> {
    bus.trace
        .cycles()
        .iter()
        .filter(|cycle| {
            matches!(
                cycle.kind,
                BusAccessKind::PageWalkRead | BusAccessKind::PageWalkWrite
            )
        })
        .map(|cycle| (cycle.kind, cycle.address, cycle.width))
        .collect()
}

const PAGE_WALK_CR2_SENTINEL: u32 = 0xfeed_c0de;

fn page_walk_failure_fixture(pde_flags: u32, pte_flags: u32) -> (CpuGsw, TestBus) {
    let mut memory = vec![0; 0x7000];
    memory[PAGE_WALK_DIRECTORY as usize..PAGE_WALK_DIRECTORY as usize + 4]
        .copy_from_slice(&(PAGE_WALK_TABLE | pde_flags).to_le_bytes());
    memory[PAGE_WALK_TABLE as usize..PAGE_WALK_TABLE as usize + 4]
        .copy_from_slice(&(PAGE_WALK_FRAME | pte_flags).to_le_bytes());
    memory[PAGE_WALK_FRAME as usize] = 0xa5;
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    enable_page_walk_overlap_paging(&mut cpu);
    cpu.control.cr2 = PAGE_WALK_CR2_SENTINEL;
    #[cfg(feature = "jit")]
    cpu.set_jit_auto_admit(true);
    (cpu, bus)
}

fn assert_failed_page_walk_unpublished(cpu: &CpuGsw, linear: u32) {
    assert!(cpu.tlb.lookup(linear >> 12).is_none());
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    {
        assert!(!cpu.jit_fast_map.has_read_mapping(linear, PAGE_WALK_FRAME));
        assert!(!cpu.jit_fast_map.has_write_mapping(linear, PAGE_WALK_FRAME));
    }
}

#[test]
fn failed_pde_read_has_no_page_walk_effects_or_fault_state() {
    let (mut cpu, mut bus) = page_walk_failure_fixture(0x27, 0x27);
    let pde_before =
        bus.memory[PAGE_WALK_DIRECTORY as usize..PAGE_WALK_DIRECTORY as usize + 4].to_vec();
    bus.fail_page_walk(BusAccessKind::PageWalkRead, 1);

    let result = cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead);

    assert!(matches!(
        result,
        Err(InternalFault::Cpu(CpuError::Bus(
            BusError::UnmappedMemory {
                address: PAGE_WALK_DIRECTORY
            }
        )))
    ));
    assert_eq!(cpu.control.cr2, PAGE_WALK_CR2_SENTINEL);
    assert_eq!(
        &bus.memory[PAGE_WALK_DIRECTORY as usize..PAGE_WALK_DIRECTORY as usize + 4],
        pde_before.as_slice()
    );
    assert_eq!(bus.memory[PAGE_WALK_FRAME as usize], 0xa5);
    assert_eq!(cpu.written_count, 0);
    assert_failed_page_walk_unpublished(&cpu, 0);
    assert_eq!(
        page_walk_cycles(&bus),
        vec![(
            BusAccessKind::PageWalkRead,
            PAGE_WALK_DIRECTORY,
            BusWidth::Dword
        )]
    );
}

#[test]
fn failed_pde_accessed_write_does_not_commit_or_notify() {
    let (mut cpu, mut bus) = page_walk_failure_fixture(0x07, 0x27);
    let pde_before =
        bus.memory[PAGE_WALK_DIRECTORY as usize..PAGE_WALK_DIRECTORY as usize + 4].to_vec();
    let invalidations = cpu.perf_counters().code_invalidations;
    bus.fail_page_walk(BusAccessKind::PageWalkWrite, 1);

    let result = cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead);

    assert!(matches!(
        result,
        Err(InternalFault::Cpu(CpuError::Bus(
            BusError::UnmappedMemory {
                address: PAGE_WALK_DIRECTORY
            }
        )))
    ));
    assert_eq!(cpu.control.cr2, PAGE_WALK_CR2_SENTINEL);
    assert_eq!(
        &bus.memory[PAGE_WALK_DIRECTORY as usize..PAGE_WALK_DIRECTORY as usize + 4],
        pde_before.as_slice()
    );
    assert_eq!(cpu.written_count, 0);
    assert_eq!(cpu.perf_counters().code_invalidations, invalidations);
    assert_failed_page_walk_unpublished(&cpu, 0);
    assert_eq!(
        page_walk_cycles(&bus),
        vec![
            (
                BusAccessKind::PageWalkRead,
                PAGE_WALK_DIRECTORY,
                BusWidth::Dword
            ),
            (
                BusAccessKind::PageWalkWrite,
                PAGE_WALK_DIRECTORY,
                BusWidth::Dword
            )
        ]
    );
}

#[test]
fn pde_accessed_write_survives_a_later_pte_read_failure() {
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
    cpu.control.cr2 = PAGE_WALK_CR2_SENTINEL;
    let invalidations = cpu.perf_counters().code_invalidations;
    bus.fail_page_walk(BusAccessKind::PageWalkRead, 2);

    let result = cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead);

    assert!(matches!(
        result,
        Err(InternalFault::Cpu(CpuError::Bus(
            BusError::UnmappedMemory {
                address: PAGE_WALK_TABLE
            }
        )))
    ));
    assert_eq!(
        u32::from_le_bytes(
            bus.memory[entry as usize..entry as usize + 4]
                .try_into()
                .unwrap()
        ),
        PAGE_WALK_TABLE | 0x25
    );
    assert_eq!(cpu.control.cr2, PAGE_WALK_CR2_SENTINEL);
    assert_eq!(cpu.written_count, 1);
    assert!(cpu.written_pages.contains(&Some(PAGE_WALK_DIRECTORY >> 12)));
    assert_page_walk_code_invalidated(&cpu, entry, entry);
    assert_eq!(cpu.perf_counters().code_invalidations, invalidations + 1);
    assert_failed_page_walk_unpublished(&cpu, 0);
    assert_eq!(
        page_walk_cycles(&bus),
        vec![
            (
                BusAccessKind::PageWalkRead,
                PAGE_WALK_DIRECTORY,
                BusWidth::Dword
            ),
            (
                BusAccessKind::PageWalkWrite,
                PAGE_WALK_DIRECTORY,
                BusWidth::Dword
            ),
            (
                BusAccessKind::PageWalkRead,
                PAGE_WALK_TABLE,
                BusWidth::Dword
            )
        ]
    );
}

#[test]
fn pde_accessed_write_survives_a_later_pte_accessed_write_failure() {
    let entry = PAGE_WALK_DIRECTORY;
    let mut memory = vec![0; 0x7000];
    memory[entry as usize..entry as usize + 4]
        .copy_from_slice(&(PAGE_WALK_TABLE | 0x05).to_le_bytes());
    memory[entry as usize + 4] = 0x40;
    memory[entry as usize + 5..entry as usize + 13].fill(0x40);
    memory[PAGE_WALK_TABLE as usize..PAGE_WALK_TABLE as usize + 4]
        .copy_from_slice(&(PAGE_WALK_FRAME | 0x05).to_le_bytes());
    memory[PAGE_WALK_FRAME as usize] = 0xa5;
    let mut bus = TestBus::with_memory(memory);
    let mut cpu = page_walk_overlap_cpu();
    decode_page_walk_overlap(&mut cpu, &mut bus, entry);
    enable_page_walk_overlap_paging(&mut cpu);
    cpu.control.cr2 = PAGE_WALK_CR2_SENTINEL;
    let invalidations = cpu.perf_counters().code_invalidations;
    bus.fail_page_walk(BusAccessKind::PageWalkWrite, 2);

    let result = cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead);

    assert!(matches!(
        result,
        Err(InternalFault::Cpu(CpuError::Bus(
            BusError::UnmappedMemory {
                address: PAGE_WALK_TABLE
            }
        )))
    ));
    assert_eq!(
        u32::from_le_bytes(
            bus.memory[entry as usize..entry as usize + 4]
                .try_into()
                .unwrap()
        ),
        PAGE_WALK_TABLE | 0x25
    );
    assert_eq!(
        u32::from_le_bytes(
            bus.memory[PAGE_WALK_TABLE as usize..PAGE_WALK_TABLE as usize + 4]
                .try_into()
                .unwrap()
        ),
        PAGE_WALK_FRAME | 0x05
    );
    assert_eq!(bus.memory[PAGE_WALK_FRAME as usize], 0xa5);
    assert_eq!(cpu.control.cr2, PAGE_WALK_CR2_SENTINEL);
    assert_eq!(cpu.written_count, 1);
    assert!(cpu.written_pages.contains(&Some(PAGE_WALK_DIRECTORY >> 12)));
    assert_page_walk_code_invalidated(&cpu, entry, entry);
    assert_eq!(cpu.perf_counters().code_invalidations, invalidations + 1);
    assert_failed_page_walk_unpublished(&cpu, 0);
    assert_eq!(
        page_walk_cycles(&bus),
        vec![
            (
                BusAccessKind::PageWalkRead,
                PAGE_WALK_DIRECTORY,
                BusWidth::Dword
            ),
            (
                BusAccessKind::PageWalkWrite,
                PAGE_WALK_DIRECTORY,
                BusWidth::Dword
            ),
            (
                BusAccessKind::PageWalkRead,
                PAGE_WALK_TABLE,
                BusWidth::Dword
            ),
            (
                BusAccessKind::PageWalkWrite,
                PAGE_WALK_TABLE,
                BusWidth::Dword
            )
        ]
    );
}

#[test]
fn pde_accessed_write_survives_a_later_not_present_fault() {
    let (mut cpu, mut bus) = page_walk_failure_fixture(0x07, 0);

    let result = cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead);

    assert!(matches!(
        result,
        Err(InternalFault::Exception {
            vector: 14,
            error_code: Some(0)
        })
    ));
    assert_eq!(
        u32::from_le_bytes(
            bus.memory[PAGE_WALK_DIRECTORY as usize..PAGE_WALK_DIRECTORY as usize + 4]
                .try_into()
                .unwrap()
        ),
        PAGE_WALK_TABLE | 0x27
    );
    assert_eq!(cpu.control.cr2, 0);
    assert_eq!(cpu.written_count, 1);
    assert!(cpu.written_pages.contains(&Some(PAGE_WALK_DIRECTORY >> 12)));
    assert_failed_page_walk_unpublished(&cpu, 0);
    assert_eq!(
        page_walk_cycles(&bus),
        vec![
            (
                BusAccessKind::PageWalkRead,
                PAGE_WALK_DIRECTORY,
                BusWidth::Dword
            ),
            (
                BusAccessKind::PageWalkWrite,
                PAGE_WALK_DIRECTORY,
                BusWidth::Dword
            ),
            (
                BusAccessKind::PageWalkRead,
                PAGE_WALK_TABLE,
                BusWidth::Dword
            )
        ]
    );
}

#[test]
fn pde_accessed_write_survives_a_later_user_protection_fault() {
    let (mut cpu, mut bus) = page_walk_failure_fixture(0x07, 0x23);
    cpu.cpl = 3;

    let result = cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead);

    assert!(matches!(
        result,
        Err(InternalFault::Exception {
            vector: 14,
            error_code: Some(5)
        })
    ));
    assert_eq!(
        u32::from_le_bytes(
            bus.memory[PAGE_WALK_DIRECTORY as usize..PAGE_WALK_DIRECTORY as usize + 4]
                .try_into()
                .unwrap()
        ),
        PAGE_WALK_TABLE | 0x27
    );
    assert_eq!(
        u32::from_le_bytes(
            bus.memory[PAGE_WALK_TABLE as usize..PAGE_WALK_TABLE as usize + 4]
                .try_into()
                .unwrap()
        ),
        PAGE_WALK_FRAME | 0x23
    );
    assert_eq!(cpu.control.cr2, 0);
    assert_eq!(cpu.written_count, 1);
    assert_failed_page_walk_unpublished(&cpu, 0);
    assert_eq!(
        page_walk_cycles(&bus),
        vec![
            (
                BusAccessKind::PageWalkRead,
                PAGE_WALK_DIRECTORY,
                BusWidth::Dword
            ),
            (
                BusAccessKind::PageWalkWrite,
                PAGE_WALK_DIRECTORY,
                BusWidth::Dword
            ),
            (
                BusAccessKind::PageWalkRead,
                PAGE_WALK_TABLE,
                BusWidth::Dword
            )
        ]
    );
}

#[test]
fn failed_pte_accessed_write_does_not_publish_or_touch_the_target() {
    let (mut cpu, mut bus) = page_walk_failure_fixture(0x27, 0x07);
    let pte_before = bus.memory[PAGE_WALK_TABLE as usize..PAGE_WALK_TABLE as usize + 4].to_vec();
    bus.fail_page_walk(BusAccessKind::PageWalkWrite, 1);

    let result = cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead);

    assert!(matches!(
        result,
        Err(InternalFault::Cpu(CpuError::Bus(
            BusError::UnmappedMemory {
                address: PAGE_WALK_TABLE
            }
        )))
    ));
    assert_eq!(
        &bus.memory[PAGE_WALK_TABLE as usize..PAGE_WALK_TABLE as usize + 4],
        pte_before.as_slice()
    );
    assert_eq!(bus.memory[PAGE_WALK_FRAME as usize], 0xa5);
    assert_eq!(cpu.control.cr2, PAGE_WALK_CR2_SENTINEL);
    assert_eq!(cpu.written_count, 0);
    assert_failed_page_walk_unpublished(&cpu, 0);
    assert_eq!(
        page_walk_cycles(&bus),
        vec![
            (
                BusAccessKind::PageWalkRead,
                PAGE_WALK_DIRECTORY,
                BusWidth::Dword
            ),
            (
                BusAccessKind::PageWalkRead,
                PAGE_WALK_TABLE,
                BusWidth::Dword
            ),
            (
                BusAccessKind::PageWalkWrite,
                PAGE_WALK_TABLE,
                BusWidth::Dword
            )
        ]
    );
}

#[test]
fn successful_walk_commits_effects_before_publishing_translation() {
    let (mut cpu, mut bus) = page_walk_failure_fixture(0x07, 0x07);

    assert_eq!(
        cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0, BusAccessKind::DataRead)
            .unwrap(),
        0xa5
    );

    assert_eq!(
        u32::from_le_bytes(
            bus.memory[PAGE_WALK_DIRECTORY as usize..PAGE_WALK_DIRECTORY as usize + 4]
                .try_into()
                .unwrap()
        ),
        PAGE_WALK_TABLE | 0x27
    );
    assert_eq!(
        u32::from_le_bytes(
            bus.memory[PAGE_WALK_TABLE as usize..PAGE_WALK_TABLE as usize + 4]
                .try_into()
                .unwrap()
        ),
        PAGE_WALK_FRAME | 0x27
    );
    assert_eq!(cpu.control.cr2, PAGE_WALK_CR2_SENTINEL);
    assert_eq!(cpu.written_count, 2);
    assert!(cpu.tlb.lookup(0).is_some());
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    assert!(cpu.jit_fast_map.has_read_mapping(0, PAGE_WALK_FRAME));
    assert_eq!(
        page_walk_cycles(&bus),
        vec![
            (
                BusAccessKind::PageWalkRead,
                PAGE_WALK_DIRECTORY,
                BusWidth::Dword
            ),
            (
                BusAccessKind::PageWalkWrite,
                PAGE_WALK_DIRECTORY,
                BusWidth::Dword
            ),
            (
                BusAccessKind::PageWalkRead,
                PAGE_WALK_TABLE,
                BusWidth::Dword
            ),
            (
                BusAccessKind::PageWalkWrite,
                PAGE_WALK_TABLE,
                BusWidth::Dword
            )
        ]
    );
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
    bus.fail_page_walk(BusAccessKind::PageWalkWrite, 1);

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
