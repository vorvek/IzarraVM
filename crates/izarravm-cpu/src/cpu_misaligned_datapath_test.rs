// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Misaligned page-local data accesses on the interpreter's direct paths.
//!
//! Before this slice, a misaligned sized access was refused at FIVE sites and fell all the way to
//! `MachineBus::read_memory`'s byte-splitting loop. It is now classified once (`split`) and
//! consumed once (the charge). These tests pin the three properties that removal must not have
//! changed:
//!
//! * **Value.** The data path never needed the refusal: the direct entries and the FastMap
//!   pointers already use `read_unaligned`/`write_unaligned`.
//! * **Reachability.** Page-crossing accesses must STILL split, and the Mode13h aperture must
//!   still be refused -- its split has different data semantics, not merely different timing.
//! * **Side-effect ordering.** A refused access must install NOTHING. That class of bug changes
//!   no guest byte and no clock, only which later accesses run fast, so only counters can see it.
//!
//! The charge's bit-identity is NOT tested here and must not be believed from a green run of this
//! file: `TestBus` does not override `charge_direct_ram_split`, so it takes the trait default (N
//! delegated byte charges) and the `record_memory_run` fold that only `MachineBus` performs is
//! invisible. That equality is pinned against `MachineBus` directly, by
//! `charge_direct_ram_split_is_bit_identical_to_the_byte_splitting_loop` in the machine crate.

use super::*;

/// Flat 4 GiB DS so an access resolves purely on its address, and a bus that serves direct pages
/// and covers the whole low megabyte including the Mode13h aperture.
fn flat_cpu_and_bus() -> (CpuGsw, TestBus) {
    let mut cpu = CpuGsw::default();
    cpu.set_fast_map_enabled_for_test(true);
    cpu.registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x10, 0x93));
    let mut bus = TestBus::with_memory(vec![0u8; 0x000c_0000]);
    bus.direct_pages_enabled = true;
    bus.direct_pages_writable = true;
    (cpu, bus)
}

/// A PAGED fixture: two adjacent linear pages mapped to two adjacent physical frames, both
/// writable and both primed into the FastMap. Paging must be ON for any crossing-scoped behaviour
/// to be reachable at all -- `linear_range_crosses_page` is consulted only when it is.
fn paged_cpu_and_bus() -> (CpuGsw, TestBus, u32) {
    const DIRECTORY: u32 = 0x1000;
    const TABLE: u32 = 0x2000;
    const LINEAR: u32 = 0x0000_3000;
    const FRAME0: u32 = 0x0000_6000;
    const FRAME1: u32 = 0x0000_7000;

    let mut memory = vec![0u8; 0x9000];
    memory[DIRECTORY as usize..DIRECTORY as usize + 4].copy_from_slice(&(TABLE | 7).to_le_bytes());
    let pte0 = TABLE as usize + (((LINEAR >> 12) as usize) & 0x3ff) * 4;
    let pte1 = TABLE as usize + ((((LINEAR + 0x1000) >> 12) as usize) & 0x3ff) * 4;
    memory[pte0..pte0 + 4].copy_from_slice(&(FRAME0 | 7).to_le_bytes());
    memory[pte1..pte1 + 4].copy_from_slice(&(FRAME1 | 7).to_le_bytes());

    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_pages_writable = true;
    let mut cpu = CpuGsw::default();
    cpu.set_fast_map_enabled_for_test(true);
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = DIRECTORY;
    cpu.registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x10, 0x93));

    // Prime both pages for read AND write, so nothing below can be blamed on a cold map.
    for page in [LINEAR, LINEAR + 0x1000] {
        write_sized(&mut cpu, &mut bus, page, BusWidth::Dword, 0);
        read_dword(&mut cpu, &mut bus, page);
    }
    (cpu, bus, LINEAR)
}

fn read_word(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32) -> u32 {
    cpu.read_memory_bus_width(
        bus,
        SegmentIndex::Ds,
        linear,
        BusWidth::Word,
        BusAccessKind::DataRead,
    )
    .unwrap()
}

fn read_dword(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32) -> u32 {
    cpu.read_memory_bus_width(
        bus,
        SegmentIndex::Ds,
        linear,
        BusWidth::Dword,
        BusAccessKind::DataRead,
    )
    .unwrap()
}

fn write_sized(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32, width: BusWidth, value: u32) {
    cpu.write_memory_bus_width(
        bus,
        SegmentIndex::Ds,
        linear,
        width,
        value,
        BusAccessKind::DataWrite,
    )
    .unwrap();
}

/// Every misalignment of every sized width, page-local, is now SERVED by the FastMap -- and the
/// bytes that come back are the bytes that went in.
///
/// The `+0xffd` / `+0xff9` rows are the page-edge cases that are still page-local; they are the
/// ones that would break if a relaxation went one byte too far.
#[test]
fn every_page_local_misalignment_is_served_and_round_trips() {
    const PAGE: u32 = 0x0002_1000;
    for (width, offset, value) in [
        (BusWidth::Word, 0x101u32, 0xbeefu32),
        (BusWidth::Dword, 0x0f1, 0xdead_beef),
        (BusWidth::Dword, 0x0f2, 0x1234_5678),
        (BusWidth::Dword, 0x0f3, 0x89ab_cdef),
        (BusWidth::Word, 0xffd, 0xcafe),
        (BusWidth::Dword, 0xff9, 0x0bad_f00d),
    ] {
        let (mut cpu, mut bus) = flat_cpu_and_bus();
        cpu.set_slot_census_enabled(true);
        let linear = PAGE + offset;
        assert!(
            width.misaligned_at(linear),
            "{width:?}@{offset:#x} is aligned"
        );

        // First access populates; the second must be served by the FastMap.
        write_sized(&mut cpu, &mut bus, linear, width, value);
        let hits = cpu.fast_map_probe_counters().hits;
        let admits = cpu.fast_map_audit_counters().slot_admit_misaligned;
        write_sized(&mut cpu, &mut bus, linear, width, value);
        assert_eq!(
            cpu.fast_map_probe_counters().hits,
            hits + 1,
            "{width:?}@{offset:#x}: the misaligned store was not served by the FastMap"
        );
        assert_eq!(
            cpu.fast_map_audit_counters().slot_admit_misaligned,
            admits + 1,
            "{width:?}@{offset:#x}: a served misaligned access was not counted"
        );

        let got = match width {
            BusWidth::Word => read_word(&mut cpu, &mut bus, linear),
            _ => read_dword(&mut cpu, &mut bus, linear),
        };
        assert_eq!(
            got, value,
            "{width:?}@{offset:#x}: value did not round trip"
        );
        // And the bytes really landed at the unaligned address, not at a rounded-down one.
        let end = linear as usize + width.bytes() as usize;
        assert_eq!(
            &bus.memory[linear as usize..end],
            &value.to_le_bytes()[..width.bytes() as usize],
            "{width:?}@{offset:#x}: stored bytes are at the wrong address"
        );
        // `slot_reject_misaligned` is the counter that must COLLAPSE. Nothing above may land in
        // it any more -- the alignment refusal no longer exists as a rung of the ladder.
        assert_eq!(cpu.fast_map_audit_counters().slot_reject_misaligned, 0);
    }
}

/// The page-local clause SURVIVES. A crossing access must still be split upstream, whatever its
/// alignment, because the FastMap resolves exactly one page.
#[test]
fn page_crossing_accesses_still_split() {
    const PAGE: u32 = 0x0002_1000;
    for (width, offset) in [
        (BusWidth::Word, 0xfffu32),
        (BusWidth::Dword, 0xffd),
        (BusWidth::Dword, 0xffe),
        (BusWidth::Dword, 0xfff),
    ] {
        let (mut cpu, mut bus) = flat_cpu_and_bus();
        cpu.set_slot_census_enabled(true);
        let linear = PAGE + offset;
        let value = 0x1122_3344u32 & ((1u64 << (width.bytes() * 8)) - 1) as u32;

        // Warm both pages so a refusal cannot be blamed on a cold map.
        write_sized(&mut cpu, &mut bus, PAGE, BusWidth::Dword, 0);
        write_sized(&mut cpu, &mut bus, PAGE + 0x1000, BusWidth::Dword, 0);

        let hits_before = cpu.fast_map_probe_counters().hits;
        write_sized(&mut cpu, &mut bus, linear, width, value);
        let got = match width {
            BusWidth::Word => read_word(&mut cpu, &mut bus, linear),
            _ => read_dword(&mut cpu, &mut bus, linear),
        };
        // THE load-bearing assertion. Neither the store nor the load may be served as ONE
        // whole-width probe: the FastMap resolves a single page, so a crossing access must be
        // refused and split upstream. Deleting `lookup_access`'s page-local clause makes both of
        // them hit, and this is what catches it.
        //
        // The value equalities below are NOT sufficient on their own and must not be read as if
        // they were: `TestBus` backs consecutive linear pages with consecutive HOST bytes, so a
        // wrongly-served crossing access still returns the right bytes here. Two adjacent linear
        // pages on DIFFERENT physical frames -- which needs paging -- are what make the value
        // wrong, and `fast_map_serves_cross_page_reads_correctly_via_page_local_fragments` in
        // `cpu_test.rs` is the test that sets that up.
        assert_eq!(
            cpu.fast_map_probe_counters().hits,
            hits_before,
            "{width:?}@{offset:#x}: a page-crossing access was served as one whole-width probe"
        );
        assert_eq!(got, value, "{width:?}@{offset:#x}: crossing value");
        let end = linear as usize + width.bytes() as usize;
        assert_eq!(
            &bus.memory[linear as usize..end],
            &value.to_le_bytes()[..width.bytes() as usize],
            "{width:?}@{offset:#x}: crossing store landed wrong"
        );
    }
}

/// A misaligned access to the Mode13h aperture is REFUSED, and the refusal installs NOTHING.
///
/// Two separate properties, on two separate aperture pages, because they fail independently:
///
/// * **The probe's `split && is_mode13` reject.** Only observable on a page the FastMap has
///   ALREADY populated -- on a cold page the probe misses anyway and the reject is unreachable,
///   so a test that only ever touches a cold aperture page passes with the reject deleted. Page
///   one is therefore warmed with an ALIGNED access first.
/// * **Classify-before-populate in the DirectPageCache.** A misaligned aperture access must
///   decline BEFORE `data_read_pages.insert` and `populate_fast_map`. This changes no guest byte
///   and no clock -- only which later accesses run fast -- so only counters and the map can see
///   it. Page two is touched misaligned FIRST, while still cold.
#[test]
fn a_misaligned_mode13_access_is_refused_and_installs_nothing() {
    let (mut cpu, mut bus) = flat_cpu_and_bus();
    cpu.set_slot_census_enabled(true);

    // --- page one: the probe's aperture reject, on a WARM page ---
    const WARM: u32 = 0x000a_0100; // aligned
    read_word(&mut cpu, &mut bus, WARM);
    read_word(&mut cpu, &mut bus, WARM); // second access publishes/uses the bias
    assert!(
        cpu.jit_fast_map.has_read_mapping(WARM, WARM),
        "the aperture page did not populate; the reject below would be unreachable"
    );
    let hits_before = cpu.fast_map_probe_counters().hits;
    // Aligned on the same warm page: SERVED. This is the control that proves the page is live.
    read_word(&mut cpu, &mut bus, WARM);
    assert_eq!(
        cpu.fast_map_probe_counters().hits,
        hits_before + 1,
        "an aligned access to the warm aperture page was not served"
    );
    // Misaligned on that same live page: must be REFUSED by `split && is_mode13`.
    let hits_before = cpu.fast_map_probe_counters().hits;
    let admits_before = cpu.fast_map_audit_counters().slot_admit_misaligned;
    read_word(&mut cpu, &mut bus, WARM + 1);
    assert_eq!(
        cpu.fast_map_probe_counters().hits,
        hits_before,
        "a MISALIGNED access to a live Mode13 page took the fast path; the aperture reject is gone"
    );
    assert_eq!(
        cpu.fast_map_audit_counters().slot_admit_misaligned,
        admits_before,
        "a refused Mode13 access was counted as a misaligned admission"
    );

    // --- page two: classify-before-populate, on a COLD page ---
    const COLD: u32 = 0x000a_3001; // misaligned, a page nothing has touched
    let page_hits_before = cpu.perf.direct_page_hits;
    let value = read_word(&mut cpu, &mut bus, COLD);
    assert_eq!(value, 0); // it still WORKS -- it went the old way
    assert_eq!(
        cpu.perf.direct_page_hits, page_hits_before,
        "a declined Mode13 access installed a DirectPageCache entry"
    );
    assert!(
        !cpu.jit_fast_map.has_read_mapping(COLD, COLD),
        "a declined Mode13 access installed a FastMap entry"
    );

    // And the same cold page ALIGNED does populate, so the refusal above is about alignment and
    // not about that page being unreachable -- without this the case could not fail.
    read_word(&mut cpu, &mut bus, 0x000a_3000);
    assert!(
        cpu.jit_fast_map.has_read_mapping(0x000a_3000, 0x000a_3000),
        "an ALIGNED read of the same page did not populate either; the case proves nothing"
    );
}

/// Exactly ONE FastMap probe per page-local sized access, on the hit path AND the miss path.
///
/// The invariant `read_linear_fragment` has always documented, now the property that keeps the
/// probe's relocation honest. A hoist that leaves the sized entry falling into the PROBING
/// fragment function instead of the probe-less tail double-probes every miss: it pays the
/// epoch/CPL/CR0.WP preamble twice (~2.66 ns per access, the mechanism behind a recorded 4.6%
/// wall regression), doubles `fast_map_probe.misses`, and fires the census twice. Every one of
/// those is invisible to a state assertion and shows up only as a wall number nobody can explain.
#[test]
fn exactly_one_probe_and_one_census_note_per_page_local_sized_access() {
    for (width, offset) in [
        (BusWidth::Word, 0u32),
        (BusWidth::Word, 1),
        (BusWidth::Dword, 0),
        (BusWidth::Dword, 3),
    ] {
        let (mut cpu, mut bus) = flat_cpu_and_bus();
        cpu.set_rmw_census_enabled(true);
        let linear = 0x0002_1000 + offset;

        // MISS: a page nothing has populated.
        let before_probe = cpu.fast_map_probe_counters();
        let before_census = cpu.fast_map_audit_counters();
        let _ = match width {
            BusWidth::Word => read_word(&mut cpu, &mut bus, linear),
            _ => read_dword(&mut cpu, &mut bus, linear),
        };
        let after_probe = cpu.fast_map_probe_counters();
        let after_census = cpu.fast_map_audit_counters();
        assert_eq!(
            (
                after_probe.misses - before_probe.misses,
                after_probe.hits - before_probe.hits
            ),
            (1, 0),
            "{width:?}@{offset:#x}: a page-local sized MISS must probe exactly once"
        );
        assert_eq!(
            after_census.census_reads - before_census.census_reads,
            1,
            "{width:?}@{offset:#x}: a page-local sized miss must note the census exactly once"
        );

        // HIT: the same access again, now that the miss populated the map.
        let before_probe = cpu.fast_map_probe_counters();
        let before_census = cpu.fast_map_audit_counters();
        let _ = match width {
            BusWidth::Word => read_word(&mut cpu, &mut bus, linear),
            _ => read_dword(&mut cpu, &mut bus, linear),
        };
        let after_probe = cpu.fast_map_probe_counters();
        let after_census = cpu.fast_map_audit_counters();
        assert_eq!(
            (
                after_probe.hits - before_probe.hits,
                after_probe.misses - before_probe.misses
            ),
            (1, 0),
            "{width:?}@{offset:#x}: a page-local sized HIT must probe exactly once"
        );
        assert_eq!(
            after_census.census_reads - before_census.census_reads,
            1,
            "{width:?}@{offset:#x}: a page-local sized hit must note the census exactly once"
        );
    }
}

/// `call_out_stack_frame_resident` owns the 4-alignment clause now.
///
/// The hazard is a BASED SS: `segment_linear_range` adds `descriptor.base`, so a 4-aligned ESP
/// with an SS base of 2 puts every slot at a linear address that is 2 mod 4. `lookup_access` used
/// to refuse that; it no longer does, and a call-out that proceeded would split a PUSHAD frame
/// into fragments it never proved resident, with no way to deliver a fault part-way.
///
/// **The frame must be page-LOCAL**, or this test proves nothing: `lookup_access` still refuses a
/// crossing slot, so a based frame straddling a page boundary is refused whether or not the
/// alignment clause exists. ESP is chosen mid-page for exactly that reason.
///
/// **This test must FAIL against a build with the relocated clause omitted.**
#[test]
fn a_based_ss_frame_is_refused_by_the_relocated_alignment_clause() {
    // Mid-page, so all eight slots stay inside one page under both SS bases below.
    const ESP: u32 = 0x0002_1800;
    let (mut cpu, mut bus) = flat_cpu_and_bus();
    cpu.registers
        .set_segment(SegmentIndex::Ss, SegmentRegister::flat(0x10, 0x93));
    cpu.registers.set_esp(ESP);

    // Warm every slot BOTH bases will touch, so a refusal can never be blamed on a cold page.
    for linear in (ESP - 64)..(ESP + 4) {
        if linear.is_multiple_of(4) {
            write_sized(&mut cpu, &mut bus, linear, BusWidth::Dword, 0);
        }
    }
    assert!(
        cpu.call_out_stack_frame_resident(8, true),
        "the aligned control frame was refused; the negative case below cannot fail"
    );

    // Base SS at 2. ESP stays 4-aligned -- `esp.is_multiple_of(4)` is happy -- but every slot's
    // LINEAR address is 2 mod 4, and every slot is still page-local.
    let mut based = SegmentRegister::flat(0x10, 0x93);
    based.base = 2;
    cpu.registers.set_segment(SegmentIndex::Ss, based);
    // Prove the slots really are page-local, so a refusal cannot come from the crossing clause.
    for slot in 0..8u32 {
        let linear = 2 + ESP.wrapping_sub(4 * (slot + 1));
        assert!(
            linear & 0xfff <= 0xffc,
            "slot {slot} at {linear:#x} crosses a page; the test would not isolate alignment"
        );
        assert!(!linear.is_multiple_of(4));
    }
    assert!(
        !cpu.call_out_stack_frame_resident(8, true),
        "a based-SS frame at a 2-mod-4 PAGE-LOCAL linear address was ADMITTED; the relocated          is_multiple_of(4) clause is missing"
    );
}

/// A page-CROSSING sized access must note the N5 census exactly ONCE PER FRAGMENT, never once
/// more at the entry.
///
/// 4g moved the FastMap probe above the crossing test, which is deliberate and costs a crossing
/// access one extra probe (1 + N). The census must NOT follow it up, and the reason is sharper
/// than an inflated total: `census_note_write` scores a read-modify-write pair when the same
/// instruction already read the same linear PAGE. An entry-level note fires at the first
/// fragment's page while `last_read_page` still matches, so an instruction that reads a page and
/// then crossing-writes it would score TWO pairs where it scores one -- a manufactured pair, in
/// the one counter the whole N5 interleaving question is decided on.
///
/// Paging is ON here on purpose: `linear_range_crosses_page` is only consulted when it is, so a
/// real-mode fixture cannot exercise the scoping at all.
#[test]
fn a_crossing_access_notes_the_census_once_per_fragment_and_never_at_the_entry() {
    let (mut cpu, mut bus, base) = paged_cpu_and_bus();
    cpu.set_rmw_census_enabled(true);

    // A page-local dword: one access, one census note. The control.
    let before = cpu.fast_map_audit_counters();
    read_dword(&mut cpu, &mut bus, base);
    let after = cpu.fast_map_audit_counters();
    assert_eq!(
        after.census_reads - before.census_reads,
        1,
        "a page-local dword must note the census exactly once"
    );

    // A dword straddling the page boundary at +0xffe splits into two WORD fragments
    // (`page_local_fragment_width` never returns a width that would itself cross), so the census
    // must advance by exactly TWO -- not three.
    let before = cpu.fast_map_audit_counters();
    read_dword(&mut cpu, &mut bus, base + 0xffe);
    let after = cpu.fast_map_audit_counters();
    assert_eq!(
        after.census_reads - before.census_reads,
        2,
        "a crossing dword must note the census once per page-local fragment, not 1 + N"
    );

    // THE MANUFACTURED PAIR. One instruction epoch: read page P, then crossing-write starting in
    // page P. The read scores `last_read_page = P`. If the entry noted the write's census at
    // linear `base + 0xffe` -- still page P -- it would score an RMW pair, and the first fragment
    // would then score a second one.
    let before = cpu.fast_map_audit_counters();
    read_dword(&mut cpu, &mut bus, base);
    write_sized(
        &mut cpu,
        &mut bus,
        base + 0xffe,
        BusWidth::Dword,
        0x1234_5678,
    );
    let after = cpu.fast_map_audit_counters();
    assert_eq!(
        after.census_rmw_pairs - before.census_rmw_pairs,
        1,
        "the crossing write manufactured an extra read-modify-write pair"
    );
}

/// R14: `finish_fast_map_write`'s `IZARRAVM_WATCH_WRITE` hook FIRES for a misaligned store.
///
/// Before this slice a misaligned store could not reach `finish_fast_map_write`, so the two slow
/// paths' hooks (`write_linear_u8`, `write_linear_fragment`) saw every watched store and the
/// absence of a hook here was survivable. After the admission it is not: misaligned stores are
/// served here, and a hook that was never executed would let the instrument answer "no writes to
/// this range" while the writes simply moved -- which is this instrument's documented failure
/// mode, not a hypothetical one.
///
/// **This test only compiles under `--features watch-write`**, so it does NOT run on a default CI
/// leg. Run it explicitly: `cargo test -p izarravm-cpu --features watch-write watch_write`.
/// Its value is that the argument list is type-checked and the hook is executed at least once.
#[cfg(feature = "watch-write")]
#[test]
fn the_watch_write_hook_fires_for_a_misaligned_store_served_by_the_fast_map() {
    let (mut cpu, mut bus) = flat_cpu_and_bus();
    const PAGE: u32 = 0x0002_1000;
    let linear = PAGE + 1; // misaligned, page-local

    // Warm the page so the store below is genuinely served by the FastMap rather than the slow
    // path (whose hook already existed and would make this test pass for the wrong reason).
    write_sized(&mut cpu, &mut bus, linear, BusWidth::Word, 0);

    crate::set_write_watch(linear, 2);
    let before_reports = crate::write_watch_report_count();
    let before_hits = cpu.fast_map_probe_counters().hits;

    write_sized(&mut cpu, &mut bus, linear, BusWidth::Word, 0xbeef);

    assert_eq!(
        cpu.fast_map_probe_counters().hits,
        before_hits + 1,
        "the watched store did not take the FastMap path; this test would be proving the SLOW \
         path's hook, which already existed"
    );
    assert_eq!(
        crate::write_watch_report_count(),
        before_reports + 1,
        "finish_fast_map_write did not report a watched MISALIGNED store"
    );
    crate::set_write_watch(0, 0);
}
