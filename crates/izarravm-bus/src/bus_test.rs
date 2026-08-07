// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn memory_reads_and_writes() {
    let mut memory = Memory::new(16).unwrap();
    memory.write_u8(3, 0x7f).unwrap();
    memory.write_u16(4, 0x1234).unwrap();
    memory.write_u32(8, 0x89abcdef).unwrap();

    assert_eq!(memory.read_u8(3).unwrap(), 0x7f);
    assert_eq!(memory.read_u16(4).unwrap(), 0x1234);
    assert_eq!(memory.read_u32(8).unwrap(), 0x89abcdef);
}

#[test]
fn memory_rejects_out_of_bounds_access() {
    let mut memory = Memory::new(16).unwrap();
    assert!(matches!(
        memory.write_u8(16, 0),
        Err(BusError::MemoryOutOfBounds { .. })
    ));
}

#[test]
fn bus_cycle_tracks_state_count_and_byte_enables() {
    let cycle = BusCycle::new(BusAccessKind::DataRead, 0x1002, BusWidth::Word, 2);

    assert_eq!(cycle.byte_enable, 0b1100);
    assert_eq!(
        cycle.states,
        vec![BusState::T1, BusState::T2, BusState::Tw, BusState::Tw]
    );
    assert_eq!(cycle.clocks, 4);
}

#[test]
fn bus_trace_caps_retained_cycles_but_keeps_total_clocks() {
    let mut trace = BusTrace::with_capacity(3);
    for index in 0..10u32 {
        trace.push(BusCycle::new(
            BusAccessKind::DataRead,
            index,
            BusWidth::Byte,
            0,
        ));
    }

    // Only the three most recent cycles survive, oldest first.
    assert_eq!(trace.cycles().len(), 3);
    assert_eq!(trace.cycles()[0].address, 7);
    assert_eq!(trace.cycles()[2].address, 9);
    assert_eq!(trace.last().unwrap().address, 9);
    // Every pushed cycle still counts toward the clock total (BusState::T1+T2
    // with no wait states is two clocks each, ten cycles is twenty clocks).
    assert_eq!(trace.elapsed_clocks(), 20);
}

#[test]
fn bus_trace_zero_capacity_keeps_no_history_but_totals_clocks() {
    let mut trace = BusTrace::with_capacity(0);
    trace.push(BusCycle::new(BusAccessKind::DataRead, 0, BusWidth::Byte, 0));

    assert_eq!(trace.cycles().len(), 0);
    assert_eq!(trace.last(), None);
    assert_eq!(trace.elapsed_clocks(), 2);
}

#[test]
fn bus_trace_off_mode_totals_clocks_without_detail() {
    let mut trace = BusTrace::with_capacity(DEFAULT_BUS_TRACE_CAPACITY);
    trace.set_tracing_mode(TracingMode::Off);
    for _ in 0..5 {
        trace.record(
            BusAccessKind::InstructionPrefetch,
            0x1000,
            BusWidth::Byte,
            0,
        );
    }

    assert_eq!(trace.cycles().len(), 0);
    assert_eq!(trace.elapsed_clocks(), 10);
    // Off records neither detail nor the access count.
    assert_eq!(trace.access_count(), 0);
}

#[test]
fn bus_trace_counts_mode_totals_clocks_and_accesses_without_detail() {
    let mut trace = BusTrace::with_capacity(DEFAULT_BUS_TRACE_CAPACITY);
    trace.set_tracing_mode(TracingMode::Counts);
    for _ in 0..3 {
        trace.record(BusAccessKind::DataRead, 0x2000, BusWidth::Word, 1);
    }

    assert_eq!(trace.cycles().len(), 0);
    assert_eq!(trace.elapsed_clocks(), 9); // (2 + 1) * 3
    assert_eq!(trace.access_count(), 3);
}

#[test]
fn bus_trace_full_mode_record_matches_push() {
    let mut a = BusTrace::with_capacity(4);
    let mut b = BusTrace::with_capacity(4);
    a.record(BusAccessKind::DataRead, 0x10, BusWidth::Dword, 2);
    b.push(BusCycle::new(
        BusAccessKind::DataRead,
        0x10,
        BusWidth::Dword,
        2,
    ));

    assert_eq!(a.cycles(), b.cycles());
    assert_eq!(a.elapsed_clocks(), b.elapsed_clocks());
}

#[test]
fn record_instruction_fetch_run_matches_per_byte_record_loop() {
    // The bulk run must be bit-identical to a loop of per-byte `record` calls in
    // all three accounting fields (elapsed_clocks, access_count, retained cycle
    // detail) across every tracing mode. Each case uses a non-zero wait-state so
    // `clocks_for` parity is exercised away from 0, and the Full case picks a
    // capacity SMALLER than the run so the `pop_front` eviction loop runs.
    const ADDR: u32 = 0x4_0000;
    const COUNT: u32 = 5;
    const WAIT: u8 = 3;

    for (mode, capacity) in [
        (TracingMode::Off, DEFAULT_BUS_TRACE_CAPACITY),
        (TracingMode::Counts, DEFAULT_BUS_TRACE_CAPACITY),
        // capacity (3) < count (5): the run must evict the two oldest cycles.
        (TracingMode::Full, 3),
    ] {
        let mut bulk = BusTrace::with_capacity(capacity);
        bulk.set_tracing_mode(mode);
        let mut loop_ = BusTrace::with_capacity(capacity);
        loop_.set_tracing_mode(mode);

        bulk.record_instruction_fetch_run(ADDR, COUNT, WAIT);
        for i in 0..COUNT {
            loop_.record(
                BusAccessKind::InstructionPrefetch,
                ADDR.wrapping_add(i),
                BusWidth::Byte,
                WAIT,
            );
        }

        assert_eq!(
            bulk.elapsed_clocks(),
            loop_.elapsed_clocks(),
            "elapsed_clocks must match in {mode:?} mode"
        );
        assert_eq!(
            bulk.access_count(),
            loop_.access_count(),
            "access_count must match in {mode:?} mode"
        );
        assert_eq!(
            bulk.cycles(),
            loop_.cycles(),
            "retained cycle detail must match in {mode:?} mode"
        );
    }
}

#[test]
fn record_memory_run_matches_equal_width_record_loop() {
    const ADDR: u32 = 0x12_3400;
    const COUNT: u32 = 5;
    const WAIT: u8 = 2;

    for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
        for (mode, capacity) in [
            (TracingMode::Off, DEFAULT_BUS_TRACE_CAPACITY),
            (TracingMode::Counts, DEFAULT_BUS_TRACE_CAPACITY),
            (TracingMode::Full, 3),
        ] {
            let mut bulk = BusTrace::with_capacity(capacity);
            bulk.set_tracing_mode(mode);
            let mut loop_ = BusTrace::with_capacity(capacity);
            loop_.set_tracing_mode(mode);

            bulk.record_memory_run(BusAccessKind::DataWrite, ADDR, COUNT, width, WAIT);
            for i in 0..COUNT {
                loop_.record(
                    BusAccessKind::DataWrite,
                    ADDR.wrapping_add(i * width.bytes()),
                    width,
                    WAIT,
                );
            }

            assert_eq!(bulk.elapsed_clocks(), loop_.elapsed_clocks());
            assert_eq!(bulk.access_count(), loop_.access_count());
            assert_eq!(bulk.cycles(), loop_.cycles());
        }
    }
}

#[test]
fn add_elapsed_clocks_bumps_only_the_clock_total() {
    // The JIT cost-fold's bulk flush must advance elapsed_clocks by exactly the amount given,
    // and touch nothing else (no access-count bump, no per-cycle detail): it stands in for the
    // clocks a run of already-accounted accesses would have added, recorded in one op.
    let mut t = BusTrace::default();
    t.record(BusAccessKind::DataRead, 0x1000, BusWidth::Byte, 1); // 3 clocks, 1 access
    let clocks_before = t.elapsed_clocks();
    let count_before = t.access_count();
    let cycles_before = t.cycles().len();
    t.add_elapsed_clocks(40);
    assert_eq!(t.elapsed_clocks(), clocks_before + 40);
    assert_eq!(t.access_count(), count_before, "no access-count bump");
    assert_eq!(t.cycles().len(), cycles_before, "no per-cycle detail added");
    // Additive.
    t.add_elapsed_clocks(2);
    assert_eq!(t.elapsed_clocks(), clocks_before + 42);
}

#[test]
fn compiled_window_rejects_observable_tracing_and_projects_exact_batch_scaling() {
    assert!(
        CompiledBusWindow::certify(
            7,
            TracingMode::Counts,
            3,
            [4, 5, 7],
            [10, 11, 14],
            17,
            2,
            7,
            30,
        )
        .is_none()
    );
    assert!(
        CompiledBusWindow::certify(7, TracingMode::Off, 3, [4, 5, 7], [10, 11, 14], 17, 2, 7, 0)
            .is_none()
    );

    let window = CompiledBusWindow::certify(
        7,
        TracingMode::Off,
        3,
        [4, 5, 7],
        [10, 11, 14],
        17,
        2,
        7,
        30,
    )
    .unwrap();
    let mut delta = CompiledBusDelta::default();
    delta.add_instruction_fetches(4);
    delta.add_ram_accesses(BusWidth::Byte, 2);
    delta.add_ram_accesses(BusWidth::Word, 1);
    delta.add_vga_reads(BusWidth::Word, 3);
    delta.add_vga_writes(NativeVgaWrites {
        dirty_pages: 0b0101,
        byte_writes: 2,
        word_writes: 0,
        dword_writes: 1,
    });

    assert_eq!(window.mapping_epoch(), 7);
    assert_eq!(window.tracing_mode(), TracingMode::Off);
    assert_eq!(window.delta_raw_clocks(&delta), 92);
    assert_eq!(window.projected_scaled_bus_clocks(92), Some(25));
}

#[test]
fn compiled_delta_merges_vga_dirty_pages_and_width_counts() {
    let mut delta = CompiledBusDelta::default();
    delta.add_vga_writes(NativeVgaWrites {
        dirty_pages: 0b0001,
        byte_writes: 2,
        word_writes: 3,
        dword_writes: 4,
    });
    delta.add_vga_writes(NativeVgaWrites {
        dirty_pages: 0b1000,
        byte_writes: 5,
        word_writes: 6,
        dword_writes: 7,
    });

    assert_eq!(
        delta.vga_writes(),
        NativeVgaWrites {
            dirty_pages: 0b1001,
            byte_writes: 7,
            word_writes: 9,
            dword_writes: 11,
        }
    );
}

#[test]
fn io_bus_tracks_claimed_ports() {
    let mut bus = IoBus::default();
    bus.claim(PortRange::new(0x220, 0x22f));

    assert!(bus.is_claimed(0x220));
    assert!(bus.is_claimed(0x22f));
    assert!(!bus.is_claimed(0x230));
}

/// The one-lookup store table's tag-bit precondition (design D7): every buffer that backs a
/// `DirectPage` hands out 4096-aligned page pointers. Correctness never depends on this — a
/// misaligned backing degrades to the CPU's slow store path — but a silently unaligned
/// allocation would devacuate the whole fast-path fixture suite, so the contract is pinned
/// here at the allocation.
#[test]
fn memory_backing_is_page_aligned() {
    let mut memory = Memory::new(64 * 1024).unwrap();
    assert_eq!(memory.as_mut_ptr() as usize % 4096, 0);
    // Clones re-derive the alignment window in their own allocation rather than copying the
    // original's offset.
    let mut clone = memory.clone();
    assert_eq!(clone.as_mut_ptr() as usize % 4096, 0);
    assert_eq!(memory, clone);
}

#[test]
fn page_aligned_bytes_from_vec_preserves_content_and_aligns() {
    let bytes: Vec<u8> = (0..8192u32).map(|i| i as u8).collect();
    let mut buf = PageAlignedBytes::from(bytes.clone());
    assert_eq!(buf.as_mut_ptr() as usize % 4096, 0);
    assert_eq!(&buf[..], &bytes[..]);
    assert_eq!(PageAlignedBytes::default().len(), 0);
}
