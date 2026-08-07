// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The one-lookup load-path battery (`dev_docs/2026-08-07-one-lookup-load-design.md` §5),
//! split from `cpu_jit_direct_test.rs` for the source-line ceiling; it borrows that battery's
//! read-driving helpers. The derivation unit tests (L1-L3) live with the map in
//! `fast_map_test.rs`. What lives HERE is the emitted differential set: the fast path consults
//! nothing but the one table (L4, via the PAGE_USER injector — reads have no watch dimension,
//! so the flags byte's permission bit is the one thing the classic arm reads that the fast arm
//! must not), the cpl0 supervisor tag strips before the pointer forms (L7, the store slice's
//! round-one miscompile class), the trio's deferred mode13 lane survives the probe swap on both
//! the limit-exit and completing paths (L5), the F1 park-domination cell (a chained mode13 park
//! must not leak into a supervisor RET's completion), the x87 unavailable status through the
//! read-resolve stub, and the guard-fires size swap (L8).

use super::jit_direct::{
    READ_ENTRY, arm_read_fixture, drive, fresh, make_data_segments_flat, prime_direct_memory_block,
    successful_read_program,
};
use super::*;

fn read_fixture(one_lookup: bool) -> (CpuGsw, TestBus) {
    let mut cpu = fresh();
    cpu.jit_direct.one_lookup_load = one_lookup;
    let mut bus = TestBus::with_memory(successful_read_program());
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    prime_direct_memory_block(&mut cpu, &mut bus);
    (cpu, bus)
}

fn repopulate_read(
    cpu: &mut CpuGsw,
    bus: &mut TestBus,
    linear: u32,
    permissions: jit::fast_map::PagePermissions,
) {
    let page = bus
        .direct_page(linear, BusAccessKind::DataRead)
        .unwrap()
        .unwrap();
    let watched = cpu.physical_page_watched(linear);
    assert!(
        cpu.jit_fast_map
            .populate_read(linear, linear, page, permissions, watched)
    );
}

fn rearm(cpu: &mut CpuGsw, bus: &mut TestBus) {
    bus.trace = BusTrace::default();
    arm_read_fixture(cpu);
    cpu.registers.eip = READ_ENTRY;
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

fn enter_cpl3(cpu: &mut CpuGsw) {
    cpu.control.cr0 |= CR0_PE;
    cpu.cpl = 3;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 3,
            base: 0,
            limit: u32::MAX,
            access: 0xfb,
            default_size_32: true,
        },
    );
    for segment in [SegmentIndex::Ds, SegmentIndex::Ss, SegmentIndex::Es] {
        cpu.registers.set_segment(
            segment,
            SegmentRegister {
                selector: 3,
                base: 0,
                limit: u32::MAX,
                access: 0xf3,
                default_size_32: true,
            },
        );
    }
}

/// L4, the anchor differential: with the one-lookup emission ON, a ring-3 load through an entry
/// whose LOAD BIAS is untagged completes natively even when the flags byte says supervisor —
/// a state only `force_fast_load_bias_for_test` can construct, because the derivation reads the
/// same byte. The classic arm's permission check reads the flags byte against the identical
/// state and refuses. Together the two arms prove the fast load path reads nothing but the one
/// table. Fixture ordering per the design's F6 note: the page's REAL permissions never change
/// (the interpreter's own walk stays user), so the differential targets the exit REASON, and no
/// repopulate may intervene between the injection and the run (it would re-derive and poison
/// the differential toward false failure).
#[test]
fn a_fast_load_bias_overrides_the_flags_byte_and_the_classic_arm_still_checks() {
    for (one_lookup, expected_permission_exits) in [(true, 0u64), (false, 1u64)] {
        let (mut cpu, mut bus) = read_fixture(one_lookup);
        enter_cpl3(&mut cpu);
        repopulate_read(
            &mut cpu,
            &mut bus,
            0x300,
            jit::fast_map::PagePermissions {
                writable: true,
                user: false,
            },
        );
        assert_eq!(
            cpu.jit_fast_map.load_bias_for_test(0x300) & jit::fast_map::NATIVE_LOAD_BIAS_TAG_MASK,
            jit::fast_map::NATIVE_LOAD_BIAS_SUPERVISOR,
            "the fixture must actually build a supervisor-tagged entry"
        );
        cpu.jit_fast_map.force_fast_load_bias_for_test(0x300);
        assert_eq!(
            cpu.jit_fast_map.load_bias_for_test(0x300) & jit::fast_map::NATIVE_LOAD_BIAS_TAG_MASK,
            0,
            "the injector must produce the underivable untagged-fast state"
        );

        arm_read_fixture(&mut cpu);
        cpu.registers.eip = READ_ENTRY;
        let key = jit::direct::key_for(&cpu, READ_ENTRY, true).unwrap();
        assert!(matches!(
            cpu.jit_direct.probe(key),
            jit::direct::BlockProbe::Interpret
        ));
        let compilation = jit::direct::compile(&mut cpu, READ_ENTRY, true).unwrap();
        let id = cpu.jit_direct.install(&compilation).unwrap();
        let block = cpu
            .jit_direct
            .block(id)
            .expect("installed block must be live");

        let permissions_before = cpu.perf_counters().jit_direct_exit_permission;
        assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
        assert_eq!(
            cpu.perf_counters().jit_direct_exit_permission - permissions_before,
            expected_permission_exits,
            "one_lookup={one_lookup}"
        );
        if one_lookup {
            assert_eq!(
                cpu.registers.eax(),
                0x1122_33d4,
                "the injected entry's load must have completed natively \
                 (moffs read then the byte-lane overwrite)"
            );
        } else {
            assert_eq!(
                cpu.registers.eip, READ_ENTRY,
                "the classic arm must refuse at the first load"
            );
        }
    }
}

/// L7: a ring-0 load through a supervisor-tagged entry (bit 1) must strip the tag and read the
/// RIGHT address, natively — the load twin of the store battery's round-one miscompile pin. A
/// leaked tag reads [0x302] (0x0000_1122 through the moffs dword) instead of [0x300]; the
/// zero-side-exit assert is what makes the cell non-vacuous.
#[test]
fn a_ring0_load_through_a_supervisor_entry_strips_the_tag_natively() {
    let (mut cpu, mut bus) = read_fixture(true);
    cpu.jit_fast_map.invalidate_page(0x300);
    repopulate_read(
        &mut cpu,
        &mut bus,
        0x300,
        jit::fast_map::PagePermissions {
            writable: true,
            user: false,
        },
    );
    assert_eq!(
        cpu.jit_fast_map.load_bias_for_test(0x300) & jit::fast_map::NATIVE_LOAD_BIAS_TAG_MASK,
        jit::fast_map::NATIVE_LOAD_BIAS_SUPERVISOR,
    );

    rearm(&mut cpu, &mut bus);
    let side_exits = cpu.perf_counters().jit_direct_side_exits;
    drive(&mut cpu, &mut bus);
    assert_eq!(
        cpu.perf_counters().jit_direct_side_exits - side_exits,
        0,
        "the supervisor load must stay native at ring 0"
    );
    assert_eq!(
        cpu.registers.eax(),
        0x1122_33d4,
        "and it must read [0x300], not [0x300+tag]"
    );
}

/// L8, the guard-fires size swap: the same read block emits STRICTLY LESS under the one-lookup
/// arm (the classify/permission/resolve/completion front collapses to the probe; the resolve
/// bodies live once per cache in the out-of-arena read pad). Without this, every test above
/// could pass with the flag wired to nothing.
#[test]
fn the_one_lookup_arm_shrinks_the_read_block_and_reads_identically() {
    let mut emitted = [0u64; 2];
    let mut finals = [0u32; 2];
    for (slot, one_lookup) in [(0usize, false), (1, true)] {
        let (mut cpu, mut bus) = read_fixture(one_lookup);
        emitted[slot] = cpu.jit_direct.total_live_code_len_for_test();
        assert_ne!(
            emitted[slot], 0,
            "one_lookup={one_lookup}: a block must have installed"
        );
        rearm(&mut cpu, &mut bus);
        drive(&mut cpu, &mut bus);
        finals[slot] = cpu.registers.eax();
    }
    assert_eq!(finals[0], finals[1], "both arms must read identical values");
    assert!(
        emitted[1] < emitted[0],
        "the one-lookup arm must emit strictly less: classic {} bytes, one-lookup {} bytes",
        emitted[0],
        emitted[1],
    );
}

/// The program behind the two trio cells below: `mov eax, imm32; ret` (no other memory read in
/// the block, so the static dword snapshot at a RET side exit is ZERO — an eagerly-incremented
/// mode13 lane then trips run.rs's `mode13 <= static` debug_assert instead of hiding inside
/// another slot's allowance) with the stack parked IN the mode13 aperture.
fn mode13_ret_program(target: u32) -> Vec<u8> {
    let mut memory = vec![0; 0x000b_0000];
    memory[0x100] = 0x90;
    memory[0x101..0x106].copy_from_slice(&[0xb8, 0x78, 0x56, 0x34, 0x12]); // mov eax,imm32
    memory[0x106] = 0xc3; // ret
    memory[0x200] = 0xf4; // hlt at the good target
    memory[0x000a_0100..0x000a_0104].copy_from_slice(&target.to_le_bytes());
    memory
}

fn arm_mode13_ret(cpu: &mut CpuGsw) {
    cpu.halted = false;
    cpu.registers.eip = 0x100;
    cpu.registers.set_eax(0);
    cpu.registers.set_esp(0x000a_0100);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    // A 32-bit stack: with SS.B = 0 the pop addresses SS:SP (the low 16 bits alone), which
    // lands in the CODE bytes at 0x100 and rides a garbage target into vector 0 — and the
    // stack-width admission matrix would refuse the 32-bit RET on a 16-bit stack anyway.
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
}

/// L5, the completing path: a RET whose stack dword lives in the mode13 aperture completes
/// natively with the mode13 read lane moving exactly once — asserted through the bus-clock
/// identity against the interpreter, because the lane IS the video-wait-state charge in run.rs
/// and a phantom or missing increment shows up as a clock skew, not a value error.
#[test]
fn a_ret_through_the_aperture_charges_the_mode13_lane_exactly_once() {
    let memory = mode13_ret_program(0x200);
    let mut interp = fresh();
    let mut native = fresh();
    make_data_segments_flat(&mut interp);
    make_data_segments_flat(&mut native);
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    arm_mode13_ret(&mut interp);
    drive(&mut interp, &mut interp_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..4 {
        arm_mode13_ret(&mut native);
        drive(&mut native, &mut native_bus);
    }
    assert!(native.jit_direct.len() > 0, "the RET block must compile");

    for cpu in [&mut interp, &mut native] {
        arm_mode13_ret(cpu);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    interp_bus.trace = BusTrace::default();
    native_bus.trace = BusTrace::default();

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(
        native_outcomes.last(),
        interp_outcomes.last(),
        "both arms must halt at the same place"
    );
    assert_eq!(native.registers.eip, interp.registers.eip);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks(),
        "the aperture RET's video charge must match the interpreter exactly"
    );
}

/// L5, the limit-exit path — the deferred-increment ordering the trio was built around: a RET
/// whose stack read lands in the aperture but whose target FAILS the CS limit must NOT move
/// the mode13 lane. The block's static dword snapshot at that side exit is zero (see
/// `mode13_ret_program`), so an eager increment in the parking probe panics run.rs's
/// debug_assert right here; the segment-limit fault itself must come out identical to the
/// interpreter's.
#[test]
fn a_ret_limit_exit_from_the_aperture_moves_no_mode13_lane() {
    // Prime with the good target, then swap the stacked dword for one past the 0xffff CS limit:
    // the block is already compiled, and stack cells are data.
    let memory = mode13_ret_program(0x200);
    let mut native = fresh();
    make_data_segments_flat(&mut native);
    let mut native_bus = TestBus::with_memory(memory.clone());
    native_bus.direct_pages_enabled = true;
    native_bus.direct_page_clocks = true;
    native.set_jit_auto_admit(true);
    for _ in 0..4 {
        arm_mode13_ret(&mut native);
        drive(&mut native, &mut native_bus);
    }
    assert!(native.jit_direct.len() > 0, "the RET block must compile");
    native_bus.memory[0x000a_0100..0x000a_0104].copy_from_slice(&0x0002_0000u32.to_le_bytes());

    let mut interp = fresh();
    make_data_segments_flat(&mut interp);
    let mut interp_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    interp_bus.memory[0x000a_0100..0x000a_0104].copy_from_slice(&0x0002_0000u32.to_le_bytes());

    arm_mode13_ret(&mut native);
    arm_mode13_ret(&mut interp);
    let limit_exits = native.direct_stall_snapshot().side_exit_segment_limit;
    let native_result = native.run_straight_line(&mut native_bus, u64::MAX);
    let interp_result = interp.run_straight_line(&mut interp_bus, u64::MAX);
    assert_eq!(
        native.direct_stall_snapshot().side_exit_segment_limit - limit_exits,
        1,
        "the native arm must reach the limit side exit (else this cell tests nothing)"
    );
    assert_eq!(
        native_result.is_err(),
        interp_result.is_err(),
        "the limit fault must resolve identically: native {native_result:?}, interp {interp_result:?}"
    );
}

/// The F1 park-domination cell: block A ends in a JmpMem through the APERTURE (its parking
/// probe parks MODE13 and its completion charges A's own lane), and chains into block B, which
/// ends in a RET through a SUPERVISOR RAM stack page at ring 0 — the strip-and-rejoin arm. The
/// frame survives the chained transfer, so if B's RAM park does not dominate the strip arm, B's
/// completion reads A's stale MODE13 and charges a phantom video read — caught here as a bus
/// clock skew against the interpreter.
#[test]
fn a_chained_aperture_park_does_not_leak_into_a_supervisor_ret() {
    fn chain_program() -> Vec<u8> {
        let mut memory = vec![0; 0x000b_0000];
        memory[0x100] = 0x90;
        memory[0x101..0x106].copy_from_slice(&[0xb8, 0x11, 0x00, 0x00, 0x00]); // mov eax,imm
        memory[0x106..0x10c].copy_from_slice(&[0xff, 0x25, 0x10, 0x00, 0x0a, 0x00]); // jmp [0xa0010]
        memory[0x000a_0010..0x000a_0014].copy_from_slice(&0x200u32.to_le_bytes());
        memory[0x200..0x205].copy_from_slice(&[0xbb, 0x22, 0x00, 0x00, 0x00]); // mov ebx,imm
        memory[0x205] = 0xc3; // ret
        memory[0x3000..0x3004].copy_from_slice(&0x300u32.to_le_bytes());
        memory[0x300] = 0xf4; // hlt
        memory
    }
    fn arm_chain(cpu: &mut CpuGsw) {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.set_eax(0);
        cpu.registers.set_ebx(0);
        cpu.registers.set_esp(0x3000);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        // 32-bit stack for the 32-bit RET — see `arm_mode13_ret`.
        let mut ss = cpu.registers.segment(SegmentIndex::Ss);
        ss.default_size_32 = true;
        cpu.registers.set_segment(SegmentIndex::Ss, ss);
    }

    let memory = chain_program();
    let mut interp = fresh();
    let mut native = fresh();
    make_data_segments_flat(&mut interp);
    make_data_segments_flat(&mut native);
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    arm_chain(&mut interp);
    drive(&mut interp, &mut interp_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..4 {
        arm_chain(&mut native);
        drive(&mut native, &mut native_bus);
    }
    assert!(
        native.jit_direct.len() >= 2,
        "both chain blocks must compile"
    );
    // The supervisor stack entry: ring 0 may read it, so B's RET takes the cpl0
    // strip-and-rejoin arm — the arm review F1 named as the park's blind spot.
    let page = native_bus
        .direct_page(0x3000, BusAccessKind::DataRead)
        .unwrap()
        .unwrap();
    let watched = native.physical_page_watched(0x3000);
    assert!(native.jit_fast_map.populate_read(
        0x3000,
        0x3000,
        page,
        jit::fast_map::PagePermissions {
            writable: true,
            user: false,
        },
        watched,
    ));
    assert_eq!(
        native.jit_fast_map.load_bias_for_test(0x3000) & jit::fast_map::NATIVE_LOAD_BIAS_TAG_MASK,
        jit::fast_map::NATIVE_LOAD_BIAS_SUPERVISOR,
    );

    for cpu in [&mut interp, &mut native] {
        arm_chain(cpu);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    interp_bus.trace = BusTrace::default();
    native_bus.trace = BusTrace::default();

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes.last(), interp_outcomes.last());
    assert_eq!(native.registers.eax(), 0x11);
    assert_eq!(native.registers.ebx(), 0x22);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks(),
        "a phantom mode13 charge on the supervisor RET means the RAM park did not \
         dominate the strip arm (design F1)"
    );
}

/// The x87 read-resolve stub's unavailable status: an FLD m64 through a DEAD entry (INVLPG'd
/// between runs) must side-exit unavailable through the stub, land the value via the
/// interpreter re-run, and heal — and the same program with the entry left alive completes
/// natively, which is what proves the exit above came from the stub's status.
#[test]
fn an_x87_load_from_a_dead_entry_resolves_through_the_read_stub() {
    use super::jit_x87_direct::{arm as arm_x87, direct_memory, run_to_halt, x87_cpu};

    fn x87_program() -> Vec<u8> {
        let mut memory = vec![0; 0x7000];
        memory[0xff] = 0x90;
        let mut code = vec![0xdd, 0x05, 0x00, 0x02, 0x00, 0x00]; // fld qword [0x200]
        code.extend_from_slice(&[0xdd, 0x1d, 0x00, 0x30, 0x00, 0x00]); // fstp qword [0x3000]
        code.extend_from_slice(&[0xa1, 0x00, 0x02, 0x00, 0x00]); // mov eax,[0x200]
        code.push(0xf4);
        memory[0x100..0x100 + code.len()].copy_from_slice(&code);
        memory[0x200..0x208].copy_from_slice(&2.5f64.to_le_bytes());
        memory
    }

    for (kill_entry, expected_unavailable) in [(true, 1u64), (false, 0u64)] {
        let mut cpu = x87_cpu(GswMode::Gsw586);
        cpu.jit_direct.one_lookup_load = true;
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

        if kill_entry {
            cpu.jit_fast_map.invalidate_page(0x200);
            assert_eq!(
                cpu.jit_fast_map.load_bias_for_test(0x200),
                jit::fast_map::NATIVE_LOAD_BIAS_POISON,
            );
        }
        bus.memory[0x3000..0x3008].fill(0);
        arm_x87(&mut cpu, 0x037f);
        let unavailable = cpu.perf_counters().jit_direct_exit_unavailable_or_kind;
        run_to_halt(&mut cpu, &mut bus);
        assert_eq!(
            cpu.perf_counters().jit_direct_exit_unavailable_or_kind - unavailable,
            expected_unavailable,
            "kill_entry={kill_entry}"
        );
        assert_eq!(
            &bus.memory[0x3000..0x3008],
            &2.5f64.to_le_bytes(),
            "kill_entry={kill_entry}: the m64 value lands either way"
        );
    }
}
