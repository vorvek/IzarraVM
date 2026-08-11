// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The non-direct data-read page histogram (`IZARRAVM_SLOW_READ_HISTO=1`).
//!
//! The instrument exists to answer one question the profile could not:
//! `dev_docs/wolf3d-586-measurement-results.md` measures 957,768,897 `data_slow_reads` against
//! 17,257,014 direct ones in that fixture's demo phase, one for one with
//! `jit_direct_exit_cross_page_or_alignment`, and the two candidate causes -- the mode-Y VGA
//! aperture (which `ram_lookup_page_is_direct` refuses BY DESIGN) and UMB/EMS RAM (which the same
//! range test refuses BY ACCIDENT) -- want completely different slices. Only the page split
//! separates them.
//!
//! `TestBus::non_direct_read_pages` is what makes these tests possible: it reproduces exactly that
//! production shape -- a page `read_memory` serves but `direct_memory_bytes` refuses -- because
//! without it every in-range `DataRead` on TestBus comes back `direct: true` and there is no slow
//! read to bucket. A test that forgot it would have asserted an empty histogram against an empty
//! expectation and proved nothing, which is why each case below also pins `data_slow_reads`.
//!
//! What these tests pin, in the order a reviewer should attack them:
//!
//! 1. **It is off unless armed**, and an unarmed run reports `None` rather than an empty table --
//!    so a caller can never print zero pages and read it as "no slow reads happened".
//! 2. **It buckets the LINEAR page**, not the address, which is what makes the region split a
//!    statement about memory layout rather than about access patterns.
//! 3. **It counts only what `data_slow_reads` counts**: a `BusAccessKind::DataRead` that came back
//!    `direct: false`. An instruction fetch or a direct read must not appear.
//!
//! The fixtures-that-cannot-fail hazard is (3): a histogram that also bucketed prefetch would
//! still look plausible on any real workload, because code and data share pages. The test below
//! therefore puts the code on page 0 and the data read on page 3, so a mislabelled contributor is
//! a whole extra bucket rather than a rounding difference.

use super::*;

/// A CPU and a bus in which `pages` are readable but never direct.
fn cpu_and_bus(pages: &[u32]) -> (CpuGsw, TestBus) {
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    let mut bus = TestBus::with_memory(vec![0; 0x8000]);
    bus.non_direct_read_pages = pages.to_vec();
    (cpu, bus)
}

/// Drive `n` byte reads of `linear` through the interpreter's ordinary data path.
fn read_bytes(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32, n: usize) {
    for _ in 0..n {
        cpu.read_memory_u8(bus, SegmentIndex::Ds, linear, BusAccessKind::DataRead)
            .unwrap();
    }
}

#[test]
fn the_histogram_is_absent_until_it_is_armed() {
    let (mut cpu, mut bus) = cpu_and_bus(&[0x1]);

    read_bytes(&mut cpu, &mut bus, 0x1234, 4);
    // NOT `Some(vec![])`. The distinction is the whole point: an unarmed run has no data, and a
    // report that printed an empty table would be read as "no slow reads", which is false here --
    // all four reads above missed the direct path, as `data_slow_reads` confirms.
    assert_eq!(cpu.slow_read_histo(), None);
    assert_eq!(cpu.perf.data_slow_reads, 4);

    cpu.set_slow_read_histo_enabled(true);
    read_bytes(&mut cpu, &mut bus, 0x1234, 3);
    // Only the three reads AFTER arming; arming does not retroactively invent the earlier four.
    assert_eq!(cpu.slow_read_histo(), Some(vec![(0x1, 3)]));
    assert_eq!(cpu.perf.data_slow_reads, 7);

    cpu.set_slow_read_histo_enabled(false);
    read_bytes(&mut cpu, &mut bus, 0x1234, 1);
    assert_eq!(cpu.slow_read_histo(), None);
    assert_eq!(cpu.perf.data_slow_reads, 8);
}

#[test]
fn slow_reads_bucket_by_linear_page_and_sort_by_count() {
    let (mut cpu, mut bus) = cpu_and_bus(&[0x1, 0x2, 0x4]);
    cpu.set_slow_read_histo_enabled(true);

    // Two addresses inside page 2 differing in every bit below 12, so a bucket keyed on the
    // ADDRESS rather than the page would report four entries instead of three.
    read_bytes(&mut cpu, &mut bus, 0x2000, 5);
    read_bytes(&mut cpu, &mut bus, 0x2ffc, 4);
    read_bytes(&mut cpu, &mut bus, 0x1000, 2);
    read_bytes(&mut cpu, &mut bus, 0x4008, 7);

    // Count descending, then page ascending -- page 2's nine beat page 4's seven only after the
    // two page-2 addresses have been merged, which is the property under test.
    assert_eq!(
        cpu.slow_read_histo(),
        Some(vec![(0x2, 9), (0x4, 7), (0x1, 2)])
    );
    assert_eq!(cpu.perf.data_slow_reads, 18);
    // Every read above is a byte read, which is aligned by definition.
    assert_eq!(cpu.slow_read_alignment(), Some((0, 18)));
}

#[test]
fn the_alignment_split_separates_should_split_from_a_non_direct_region() {
    // The distinction N2 turns on. Both of these reads land in `data_slow_reads` and both would
    // raise `jit_direct_exit_cross_page_or_alignment`, but one is refused because its ADDRESS is
    // odd for its width and the other because its PAGE is not direct RAM -- and they want opposite
    // slices. Page 6 is direct here, so only `should_split`'s shape can refuse the word read.
    let (mut cpu, mut bus) = cpu_and_bus(&[0x1]);
    cpu.set_slow_read_histo_enabled(true);

    // Odd address, word width, on a page the bus is happy to serve directly.
    cpu.read_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        0x6001,
        OperandSize::Word,
        BusAccessKind::DataRead,
    )
    .unwrap();
    // Even address, word width, on the page the bus refuses.
    cpu.read_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        0x1002,
        OperandSize::Word,
        BusAccessKind::DataRead,
    )
    .unwrap();

    assert_eq!(cpu.slow_read_alignment(), Some((1, 2)));
    assert_eq!(cpu.slow_read_histo(), Some(vec![(0x1, 1), (0x6, 1)]));
}

#[test]
fn direct_reads_are_counted_by_neither_the_histogram_nor_data_slow_reads() {
    // Page 5 is NOT in the non-direct list, so TestBus serves it directly -- the production shape
    // for ordinary RAM. If those reads appeared here the histogram would describe ALL reads and
    // its region split would say nothing about N2.
    let (mut cpu, mut bus) = cpu_and_bus(&[0x1]);
    cpu.set_slow_read_histo_enabled(true);

    read_bytes(&mut cpu, &mut bus, 0x5000, 6);
    read_bytes(&mut cpu, &mut bus, 0x1000, 2);

    assert_eq!(cpu.perf.data_direct_reads, 6);
    assert_eq!(cpu.perf.data_slow_reads, 2);
    assert_eq!(cpu.slow_read_histo(), Some(vec![(0x1, 2)]));
}

#[test]
fn instruction_fetches_stay_out_of_the_histogram() {
    let mut memory = vec![0u8; 0x8000];
    // `mov al, [0x3000]` -- opcode 0xA0 with a 16-bit displacement in real mode.
    memory[..3].copy_from_slice(&[0xa0, 0x00, 0x30]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);
    // Both the code page and the data page are non-direct, so a histogram that bucketed prefetch
    // would report page 0 as well -- the refusal cannot be what hides it.
    bus.non_direct_read_pages = vec![0x0, 0x3];
    cpu.set_slow_read_histo_enabled(true);

    cpu.cycle(&mut bus).unwrap();

    // Page 3 only. The fetch bytes live on page 0 and arrive as `InstructionPrefetch`, which
    // `data_slow_reads` does not count either -- the two numbers agreeing is the check.
    assert_eq!(cpu.slow_read_histo(), Some(vec![(0x3, 1)]));
    assert_eq!(cpu.perf.data_slow_reads, 1);
}
