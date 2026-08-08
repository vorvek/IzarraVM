// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The one-lookup store-path battery (`dev_docs/2026-08-07-one-lookup-store-design.md` §5),
//! split from `cpu_jit_direct_test.rs` for the source-line ceiling; it borrows that battery's
//! store-driving helpers. The derivation unit tests (T1-T3) live with the map in
//! `fast_map_test.rs`; the coherence edges ride the watch-bit battery's sweeps
//! (`cpu_jit_watch_bit_test.rs`, whose `clear_entry` choke now poisons the store bias too).
//! What lives HERE is the emitted differential set: the fast path consults nothing but the one
//! table (T4), the watched path keeps its exit identity and its granule mask through the shared
//! stub (T5), the cpl0 supervisor tag strips before the pointer forms (the round-one
//! miscompile's pin), the x87 resolve stub's watched status, and the guard-fires size swap.

use super::jit_direct::{
    arm_store_fixture, drive, fresh, prime_direct_store_block, store_exit_program,
};
use super::*;

const TARGET: u32 = 0x4100;

fn store_fixture(one_lookup: bool) -> (CpuGsw, TestBus) {
    let mut cpu = fresh();
    cpu.jit_direct.one_lookup_store = one_lookup;
    let mut bus = TestBus::with_memory(store_exit_program(TARGET));
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    prime_direct_store_block(&mut cpu, &mut bus);
    (cpu, bus)
}

fn repopulate_target(
    cpu: &mut CpuGsw,
    bus: &mut TestBus,
    permissions: jit::fast_map::PagePermissions,
) {
    let page = bus
        .direct_page(TARGET, BusAccessKind::DataWrite)
        .unwrap()
        .unwrap();
    let watched = cpu.physical_page_watched(TARGET);
    assert!(
        cpu.jit_fast_map
            .populate_write(TARGET, TARGET, page, permissions, watched)
    );
}

fn rearm(cpu: &mut CpuGsw, bus: &mut TestBus) {
    bus.trace = BusTrace::default();
    arm_store_fixture(cpu);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

/// T4, the anchor differential: with the one-lookup emission ON, an entry whose STORE BIAS is
/// fast is stored through natively even when the flags byte says watched AND both published
/// watch tables cover the page — a state only the `force_fast_store_bias_for_test` injector
/// can construct, because the derivation reads the same byte. The classic arm consults the
/// flags bit against the identical state and takes the code-watch exit. Together the two arms
/// prove the fast path reads nothing but the one table — the fixtures-that-cannot-fail swap
/// this battery is anchored on (a tables-only poke would be vacuous: BOTH arms ignore the
/// tables while the flags bit is clear).
#[test]
fn a_fast_store_bias_overrides_flags_and_tables_and_the_classic_arm_still_probes() {
    for (one_lookup, expected_watch_exits) in [(true, 0u64), (false, 1u64)] {
        let (mut cpu, mut bus) = store_fixture(one_lookup);
        // The watched state: flags bit SET (populate under a raw-marked table) and the sticky
        // table published for the page — then the injector recomputes the store bias as if the
        // page were unwatched, the exact incoherence INV-P forbids.
        let _ = cpu.decode_cache.native_code_watch.mark_range(TARGET, 1);
        repopulate_target(&mut cpu, &mut bus, jit::fast_map::PagePermissions::UNPAGED);
        assert!(cpu.jit_fast_map.page_watched_bit_for_test(TARGET));
        assert_eq!(
            cpu.jit_fast_map.store_bias_for_test(TARGET),
            jit::fast_map::NATIVE_STORE_BIAS_POISON,
            "watched implies poisoned, by derivation"
        );
        cpu.jit_fast_map.force_fast_store_bias_for_test(TARGET);
        assert_ne!(
            cpu.jit_fast_map.store_bias_for_test(TARGET),
            jit::fast_map::NATIVE_STORE_BIAS_POISON
        );

        rearm(&mut cpu, &mut bus);
        let watch_exits = cpu.perf_counters().jit_direct_exit_code_watch;
        drive(&mut cpu, &mut bus);
        assert_eq!(
            cpu.perf_counters().jit_direct_exit_code_watch - watch_exits,
            expected_watch_exits,
            "one_lookup={one_lookup}"
        );
        assert_eq!(
            &bus.memory[TARGET as usize..TARGET as usize + 4],
            &0x1234_5678u32.to_le_bytes(),
            "one_lookup={one_lookup}: the store must land either way"
        );
    }
}

/// T5, both halves, both arms: a store INTO a watched granule takes the code-watch exit with
/// the same reason and count through the shared slow stub as through the classic inline guard;
/// a store to a watched PAGE whose granule is clear completes natively on both arms — the #711
/// byte-granular mask (NASCAR's win) running inside the stub.
#[test]
fn watched_stores_keep_their_exit_identity_and_their_granule_mask_through_the_stub() {
    for one_lookup in [true, false] {
        // Half one: the marked granule IS the store target — the exit must fire. The mark's
        // strict-edge sweep kills the entry (INV-P), so the fixture repopulates: without the
        // refill the first native store takes the UNAVAILABLE exit off the dead entry and the
        // watch path is never on trial — the mark-before-populate rule every watched-store
        // fixture follows.
        let (mut cpu, mut bus) = store_fixture(one_lookup);
        cpu.mark_decode_code_for_test(TARGET, 1);
        repopulate_target(&mut cpu, &mut bus, jit::fast_map::PagePermissions::UNPAGED);
        assert_eq!(
            cpu.jit_fast_map.store_bias_for_test(TARGET),
            jit::fast_map::NATIVE_STORE_BIAS_POISON,
        );
        rearm(&mut cpu, &mut bus);
        let watch_exits = cpu.perf_counters().jit_direct_exit_code_watch;
        let side_exits = cpu.perf_counters().jit_direct_side_exits;
        drive(&mut cpu, &mut bus);
        assert_eq!(
            cpu.perf_counters().jit_direct_exit_code_watch - watch_exits,
            1,
            "one_lookup={one_lookup}: the watched-granule store must exit"
        );
        assert_eq!(
            cpu.perf_counters().jit_direct_side_exits - side_exits,
            1,
            "one_lookup={one_lookup}: and it is the only side exit"
        );
        assert_eq!(
            &bus.memory[TARGET as usize..TARGET as usize + 4],
            &0x1234_5678u32.to_le_bytes(),
            "one_lookup={one_lookup}: the exit's re-run lands the store"
        );

        // Half two: a mark elsewhere in the SAME page — the granule mask must let the store
        // complete natively, with no exit at all (not even the unavailable one: the zero
        // side-exit assert is what proves the store completed IN the stub rather than landing
        // via an interpreter re-run).
        let (mut cpu, mut bus) = store_fixture(one_lookup);
        cpu.mark_decode_code_for_test(TARGET + 0x200, 1);
        repopulate_target(&mut cpu, &mut bus, jit::fast_map::PagePermissions::UNPAGED);
        rearm(&mut cpu, &mut bus);
        let watch_exits = cpu.perf_counters().jit_direct_exit_code_watch;
        let side_exits = cpu.perf_counters().jit_direct_side_exits;
        drive(&mut cpu, &mut bus);
        assert_eq!(
            cpu.perf_counters().jit_direct_exit_code_watch - watch_exits,
            0,
            "one_lookup={one_lookup}: a clear granule on a watched page stays native"
        );
        assert_eq!(
            cpu.perf_counters().jit_direct_side_exits - side_exits,
            0,
            "one_lookup={one_lookup}: with no side exit of any kind"
        );
        assert_eq!(
            &bus.memory[TARGET as usize..TARGET as usize + 4],
            &0x1234_5678u32.to_le_bytes(),
            "one_lookup={one_lookup}"
        );
    }
}

/// The cpl0 supervisor tag: a ring-0 store through a supervisor-tagged entry (bit 1) must
/// strip the tag and store at the RIGHT address, natively. Round one of this battery shipped a
/// fast arm that tested only bit 0 at cpl0, so a supervisor entry's pointer formed as
/// `bias|2 + linear` — a store two bytes off, which the WP+supervisor differential generator
/// caught. This is that miscompile's standing pin: the assert on TARGET+2 is the tag bits
/// leaking into the pointer, and the zero-side-exit assert is what makes the cell
/// non-vacuous (a store that side-exited to the interpreter would land correctly too).
#[test]
fn a_ring0_store_through_a_supervisor_entry_strips_the_tag_natively() {
    let (mut cpu, mut bus) = store_fixture(true);
    cpu.jit_fast_map.invalidate_page(TARGET);
    repopulate_target(
        &mut cpu,
        &mut bus,
        jit::fast_map::PagePermissions {
            writable: true,
            user: false,
        },
    );
    assert_eq!(
        cpu.jit_fast_map.store_bias_for_test(TARGET) & jit::fast_map::NATIVE_STORE_BIAS_TAG_MASK,
        jit::fast_map::NATIVE_STORE_BIAS_SUPERVISOR,
        "the fixture must actually build a supervisor-tagged entry"
    );

    rearm(&mut cpu, &mut bus);
    let side_exits = cpu.perf_counters().jit_direct_side_exits;
    drive(&mut cpu, &mut bus);
    assert_eq!(
        cpu.perf_counters().jit_direct_side_exits - side_exits,
        0,
        "the supervisor store must stay native at ring 0"
    );
    assert_eq!(
        &bus.memory[TARGET as usize..TARGET as usize + 4],
        &0x1234_5678u32.to_le_bytes(),
        "and it must land at TARGET, not TARGET+tag"
    );
    assert_eq!(
        &bus.memory[TARGET as usize + 4..TARGET as usize + 8],
        &[0u8; 4],
        "nothing may leak past the store's own bytes"
    );
}

/// The guard-fires size swap: the same store block emits STRICTLY LESS under the one-lookup
/// arm (the classify/permission/resolve/watch front collapses to the probe; the slow bodies
/// live once per cache in the out-of-arena pad). This is what proves the gate actually flips
/// emission — without it, every test above could pass with the flag wired to nothing.
#[test]
fn the_one_lookup_arm_shrinks_the_store_block_and_lands_identically() {
    let mut emitted = [0u64; 2];
    for (slot, one_lookup) in [(0usize, false), (1, true)] {
        let (mut cpu, mut bus) = store_fixture(one_lookup);
        emitted[slot] = cpu.jit_direct.total_live_code_len_for_test();
        assert_ne!(
            emitted[slot], 0,
            "one_lookup={one_lookup}: a block must have installed"
        );
        rearm(&mut cpu, &mut bus);
        drive(&mut cpu, &mut bus);
        assert_eq!(
            &bus.memory[TARGET as usize..TARGET as usize + 4],
            &0x1234_5678u32.to_le_bytes(),
            "one_lookup={one_lookup}"
        );
    }
    assert!(
        emitted[1] < emitted[0],
        "the one-lookup arm must emit strictly less: classic {} bytes, one-lookup {} bytes",
        emitted[0],
        emitted[1],
    );
}

/// The x87 resolve stub's watched status (design T6's missing cell, review F10b): an FSTP m64
/// into a watched granule must take the code-watch exit through the resolve stub with the
/// value landing via the exit's re-run — and the same program against a clear page completes
/// natively, which is what proves the exit above came from the stub's status and not from a
/// refused lowering.
#[test]
fn an_x87_store_to_a_watched_granule_exits_through_the_resolve_stub() {
    use super::jit_x87_direct::{arm as arm_x87, direct_memory, run_to_halt, x87_cpu};

    // fld qword [0x200]; fstp qword [TARGET]; mov eax,[0x200]; hlt — the x87 harness's
    // ENTRY/DATA shape with the store aimed at a far page so its watch state is independent of
    // the code's, and a third slot because the compile walk refuses spans under three slots
    // that do not end in a terminal.
    fn x87_program() -> Vec<u8> {
        let mut memory = vec![0; 0x7000];
        memory[0xff] = 0x90;
        let mut code = vec![0xdd, 0x05, 0x00, 0x02, 0x00, 0x00]; // fld qword [0x200]
        code.extend_from_slice(&[0xdd, 0x1d]); // fstp qword [TARGET]
        code.extend_from_slice(&TARGET.to_le_bytes());
        code.extend_from_slice(&[0xa1, 0x00, 0x02, 0x00, 0x00]); // mov eax,[0x200]
        code.push(0xf4);
        memory[0x100..0x100 + code.len()].copy_from_slice(&code);
        memory[0x200..0x208].copy_from_slice(&2.5f64.to_le_bytes());
        memory
    }

    for (mark_target, expected_watch_exits) in [(true, 1u64), (false, 0u64)] {
        let mut cpu = x87_cpu(GswMode::Gsw586);
        cpu.jit_direct.one_lookup_store = true;
        let mut bus = direct_memory(x87_program());
        arm_x87(&mut cpu, 0x037f);
        run_to_halt(&mut cpu, &mut bus);
        cpu.set_jit_auto_admit(true);
        for _ in 0..3 {
            arm_x87(&mut cpu, 0x037f);
            run_to_halt(&mut cpu, &mut bus);
        }
        assert!(
            cpu.jit_direct.len() > 0,
            "the x87 block must compile: {:?}",
            cpu.perf_counters()
        );

        if mark_target {
            // The strict-edge sweep kills the entry; the refill re-derives the poison with the
            // watched bit — the same mark-before-populate rule the GPR halves follow.
            cpu.mark_decode_code_for_test(TARGET, 1);
            let page = bus
                .direct_page(TARGET, BusAccessKind::DataWrite)
                .unwrap()
                .unwrap();
            let watched = cpu.physical_page_watched(TARGET);
            assert!(cpu.jit_fast_map.populate_write(
                TARGET,
                TARGET,
                page,
                jit::fast_map::PagePermissions::UNPAGED,
                watched,
            ));
            assert_eq!(
                cpu.jit_fast_map.store_bias_for_test(TARGET),
                jit::fast_map::NATIVE_STORE_BIAS_POISON,
            );
        }
        bus.memory[TARGET as usize..TARGET as usize + 8].fill(0);
        bus.trace = BusTrace::default();
        arm_x87(&mut cpu, 0x037f);
        let watch_exits = cpu.perf_counters().jit_direct_exit_code_watch;
        run_to_halt(&mut cpu, &mut bus);
        assert_eq!(
            cpu.perf_counters().jit_direct_exit_code_watch - watch_exits,
            expected_watch_exits,
            "mark_target={mark_target}"
        );
        assert_eq!(
            &bus.memory[TARGET as usize..TARGET as usize + 8],
            &2.5f64.to_le_bytes(),
            "mark_target={mark_target}: the m64 value lands either way"
        );
    }
}
