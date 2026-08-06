// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Real mode with a 32-bit code segment (flat, 64 KB limit), at the 586 level so the FP
/// timing classes are non-identity and `fp_rem` actually carries.
fn fresh() -> CpuGsw {
    fresh_for(GswMode::Gsw586)
}

fn fresh_for(mode: GswMode) -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(mode);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    let mut cs = cpu.registers.cs();
    cs.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    cpu
}

fn drive_to_halt(cpu: &mut CpuGsw, bus: &mut TestBus) {
    for _ in 0..10_000 {
        if cpu.run_straight_line(bus, u64::MAX).unwrap().halted {
            return;
        }
    }
    panic!("guest never halted");
}

fn warm_poll_shape(code: &[u8], dx: u16) -> (CpuGsw, TestBus) {
    let mut memory = vec![0xf4; 0x1000];
    memory[..code.len()].copy_from_slice(code);
    let mut bus = TestBus::with_memory(memory);
    bus.lazy_io_reads = true;
    let mut cpu = fresh();
    cpu.set_native_backend_enabled(false);
    cpu.write_reg16(Reg16::Dx, dx);
    cpu.registers.eip = 0;
    for _ in 0..4 {
        let _ = cpu.run_budgeted(&mut bus, 100).unwrap();
    }
    cpu.set_eip(0);
    (cpu, bus)
}

#[test]
fn poll_loop_classifier_accepts_only_the_v1_shapes_and_restamps_smc() {
    let (mut cpu, mut bus) = warm_poll_shape(&[0xec, 0xa8, 0x08, 0x74, 0xfb], 0x03da);
    assert!(cpu.poll_skip_eligible());
    assert_eq!(cpu.read_reg16(Reg16::Dx), 0x03da);
    let bit3 = cpu.poll_loop().expect("JZ bit-3 poll");
    assert!(bit3.at_head());
    assert_eq!(bit3.status_mask(), 0x08);
    assert!(bit3.fresh_backedge_taken(0x00));
    assert!(!bit3.fresh_backedge_taken(0x08));
    cpu.set_eip(1);
    assert!(!cpu.poll_loop().expect("TEST slot membership").at_head());
    cpu.set_eip(3);
    assert!(!cpu.poll_loop().expect("Jcc slot membership").at_head());
    cpu.set_eip(0);

    bus.memory[2] = 0x01;
    assert!(cpu.note_code_write(2, 1));
    for _ in 0..4 {
        let _ = cpu.run_budgeted(&mut bus, 100).unwrap();
    }
    cpu.set_eip(0);
    let bit1 = cpu.poll_loop().expect("restamped bit-1 poll");
    assert_eq!(bit1.status_mask(), 0x01);

    let (mut cpu, _) = warm_poll_shape(&[0xec, 0xa8, 0x01, 0x75, 0xfb], 0x03da);
    assert!(cpu.poll_loop().is_some(), "JNZ bit-1 poll");
    for (code, dx) in [
        (&[0xec, 0xa8, 0x02, 0x74, 0xfb][..], 0x03da),
        (&[0xec, 0x90, 0xa8, 0x08, 0x74, 0xfa][..], 0x03da),
        (&[0x66, 0xec, 0xa8, 0x08, 0x74, 0xfa][..], 0x03da),
        (&[0xec, 0xa8, 0x08, 0x74, 0xfb][..], 0x03ba),
    ] {
        let (mut cpu, _) = warm_poll_shape(code, dx);
        assert!(cpu.poll_loop().is_none(), "rejected shape {code:02x?}");
    }

    let (mut cpu, _) = warm_poll_shape(&[0xec, 0xa8, 0x08, 0x74, 0xfb], 0x03da);
    assert!(
        cpu.poll_loop().is_some(),
        "broad warm limit recognizes the loop"
    );

    let mut cs = cpu.registers.cs();
    cs.limit = 1;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    cpu.set_eip(0);

    assert!(
        cpu.poll_loop().is_none(),
        "live limit admits IN but rejects the two-byte TEST fetch"
    );
}

#[test]
fn poll_skip_eligibility_rejects_native_privilege_v86_and_shadow() {
    let mut cpu = fresh();
    cpu.set_native_backend_enabled(false);
    assert!(cpu.poll_skip_eligible());

    cpu.set_native_backend_enabled(true);
    assert!(!cpu.poll_skip_eligible());
    cpu.set_native_backend_enabled(false);

    cpu.interrupt_shadow = true;
    assert!(!cpu.poll_skip_eligible());
    cpu.interrupt_shadow = false;

    cpu.enable_profiling(1);
    assert!(!cpu.poll_skip_eligible());
    cpu.disable_profiling();

    cpu.control.cr0 |= CR0_PE;
    cpu.cpl = 3;
    cpu.registers.eflags &= !FLAG_IOPL;
    assert!(!cpu.poll_skip_eligible());

    cpu.registers.eflags |= FLAG_VM | FLAG_IOPL;
    assert!(!cpu.poll_skip_eligible());
}

#[test]
fn poll_head_alignment_runs_one_real_instruction_per_zero_cap() {
    let (mut taken, mut taken_bus) = warm_poll_shape(&[0xec, 0xa8, 0x08, 0x74, 0xfb], 0x03da);
    taken.set_eip(1);
    assert!(!taken.poll_loop().unwrap().at_head());
    taken.run_budgeted(&mut taken_bus, 0).unwrap();
    assert_eq!(taken.registers.eip, 3, "TEST advances to Jcc");
    taken.run_budgeted(&mut taken_bus, 0).unwrap();
    assert_eq!(taken.registers.eip, 0, "taken Jcc reaches the IN head");

    let (mut not_taken, mut not_taken_bus) =
        warm_poll_shape(&[0xec, 0xa8, 0x01, 0x75, 0xfb], 0x03da);
    not_taken.set_eip(3);
    assert!(!not_taken.poll_loop().unwrap().at_head());
    not_taken.run_budgeted(&mut not_taken_bus, 0).unwrap();
    assert_eq!(not_taken.registers.eip, 5, "non-taken Jcc exits the loop");
}

/// Real mode with a 16-bit code segment (the default DOS-game target): CS.D is clear, so
/// the unprefixed mov/add/shr register forms are 16-bit ops. Kept for
/// `sixteen_bit_boundaries_skip_the_direct_admission_path` below (the region-JIT tests that
/// used to share this helper are gone).
fn fresh16() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.load_segment_real(SegmentIndex::Cs, 0); // default_size_32 = false
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu
}

// ---- Unit-simulator feed from the interpreter (Track C, Task 2) ----
//
// These tests drive small guest programs through the plain interpreter (no JIT admission, so
// `run_budgeted`'s native continuation path stays off and every retired instruction flows through
// an observe site) and check the trace the simulator reconstructs. The simulator never influences
// execution, so a sim-on run must match a sim-off run bit-for-bit.

/// The unit-sim probe loop base. A NOP starter so the loop head is reached as a continuation,
/// then a small ALU body decrementing ECX (all `0x83`-form, so continuable and fall-through), and
/// a `jnz` rel8 self-loop back-edge; a trailing HLT at the fall-through halts the machine.
const USIM_START: u32 = 0x200;
const USIM_LOOP: u32 = 0x201;

/// Build the probe program with `body_adds` leading `add r32,1` instructions before the
/// `sub ecx,1` / `jnz` back-edge. `body_adds = 3` gives the "few ALU ops + a near branch" shape;
/// `body_adds = 0` gives the tight two-instruction self-loop.
fn usim_program(body_adds: usize) -> Vec<u8> {
    let mut m = vec![0u8; 0x1000];
    m[USIM_START as usize] = 0x90; // nop starter
    let mut body: Vec<u8> = Vec::new();
    // add eax,1 / add ebx,1 / add edx,1 (0x83 /0, ModRM mode 3): continuable ALU, no transfer.
    for &rm in [0xc0u8, 0xc3, 0xc2].iter().take(body_adds) {
        body.extend_from_slice(&[0x83, rm, 0x01]);
    }
    body.extend_from_slice(&[0x83, 0xe9, 0x01]); // sub ecx,1 (0x83 /5): sets ZF at ecx==0
    let jnz_at = USIM_LOOP as usize + body.len();
    let rel = (USIM_LOOP as i32 - (jnz_at as i32 + 2)) as i8;
    body.push(0x75); // jnz rel8
    body.push(rel as u8);
    m[USIM_LOOP as usize..USIM_LOOP as usize + body.len()].copy_from_slice(&body);
    m[USIM_LOOP as usize + body.len()] = 0xf4; // hlt at the loop fall-through
    m
}

fn usim_arm(cpu: &mut CpuGsw, count: u32) {
    cpu.registers.eip = USIM_START;
    cpu.registers.set_esp(0x0700);
    cpu.registers.set_eax(0);
    cpu.registers.set_ebx(0);
    cpu.registers.set_edx(0);
    cpu.registers.set_ecx(count);
}

/// Enabling the simulator must not perturb architectural state (it is excluded from `CpuGsw`
/// equality), and a sim-on run must produce a non-empty trace.
#[test]
fn unit_sim_feed_is_state_neutral() {
    let mut sim_off = fresh();
    let mut sim_on = fresh();
    let mut bus_off = TestBus::with_memory(usim_program(3));
    let mut bus_on = TestBus::with_memory(usim_program(3));

    usim_arm(&mut sim_off, 6);
    usim_arm(&mut sim_on, 6);
    sim_on.set_unit_sim_enabled(true);

    drive_to_halt(&mut sim_off, &mut bus_off);
    drive_to_halt(&mut sim_on, &mut bus_on);

    // The sim slot is excluded from equality, so the two CPUs must compare equal despite one
    // carrying an enabled simulator. (If the field ever leaked into the derived PartialEq this
    // assertion would fail, which is exactly the signal the plan calls for.)
    assert_eq!(sim_off, sim_on, "enabling the unit sim changed CPU state");
    assert_eq!(bus_off.memory, bus_on.memory, "guest memory diverged");

    let reports = sim_on
        .take_unit_sim_report()
        .expect("the sim was enabled, so a report exists");
    // The wired ladder fans out to the measurement set {L0, L4, L6, P}; every rung sees the same
    // non-empty trace.
    assert_eq!(
        reports.len(),
        4,
        "the ladder must report the four measurement rungs"
    );
    for (cfg, report, histogram) in &reports {
        assert!(report.entries > 0, "rung {cfg} recorded no entries");
        assert!(
            report.retired_in_units > 0,
            "rung {cfg} accrued no retired instructions"
        );
        assert!(
            !histogram.is_empty(),
            "rung {cfg} should show at least one unit in the histogram"
        );
    }
    // Taking the report disables the sim.
    assert!(
        sim_on.take_unit_sim_report().is_none(),
        "the report was already taken; the sim is disabled"
    );
}

/// Coverage invariant (review finding B2): every retired interpreter instruction is observed
/// exactly once, so `retired_in_units` equals the run's `perf.instructions` delta. Each `observe`
/// accrues exactly once, so `retired_in_units` is the total observed count.
///
/// The ladder fans one stream to every wired rung, and each observed instruction accrues to exactly
/// one open entry in every rung (a deferred L2/L4/L5 check keeps the entry open across the closing
/// transfer, but that transfer already accrued, and the next instruction accrues to the switched
/// unit or a fresh entry either way; the L6 io call-out likewise accrues and keeps the entry open).
/// So `retired_in_units` is config-independent: EVERY rung must report the same total, equal to the
/// perf delta. A rung that differs is a ladder bug, not a test artifact.
#[test]
fn unit_sim_observes_every_retired_instruction() {
    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(usim_program(3));
    usim_arm(&mut cpu, 9);
    cpu.set_unit_sim_enabled(true);
    cpu.reset_perf_counters();

    drive_to_halt(&mut cpu, &mut bus);

    let retired = cpu.perf_counters().instructions;
    let reports = cpu.take_unit_sim_report().expect("sim enabled");
    assert!(retired > 0, "the program retired no instructions");
    assert_eq!(
        reports.len(),
        4,
        "the ladder must report the four measurement rungs"
    );
    for (cfg, report, _) in &reports {
        assert_eq!(
            report.retired_in_units, retired,
            "rung {cfg} must observe every retired instruction exactly once \
             (retired_in_units {} vs perf.instructions {retired})",
            report.retired_in_units,
        );
    }
}

/// A tight two-instruction self-loop: the `jnz` back-edge is a `DirectNear` transfer to the loop
/// head, so the open entry stays open across it and the fall-through body accrues to the same
/// entry. `entries` is therefore bounded by the batch count (one open entry per batch), never by
/// the far larger retired-instruction count.
#[test]
fn unit_sim_self_loop_keeps_entry_open() {
    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(usim_program(0));
    usim_arm(&mut cpu, 60);
    cpu.set_unit_sim_enabled(true);
    cpu.reset_perf_counters();

    drive_to_halt(&mut cpu, &mut bus);

    let batches = cpu.perf_counters().straight_line_runs;
    let reports = cpu.take_unit_sim_report().expect("sim enabled");
    // Assert on the L0 rung (v1 parity): its `DirectNear` back-edge keeps the entry open.
    let (cfg, report, _) = &reports[0];
    assert_eq!(*cfg, "L0", "the first ladder rung is L0");

    // The loop runs 60 iterations of `sub ecx,1; jnz`, so retired is large.
    assert!(
        report.retired_in_units >= 100,
        "expected a long retired stream, got {}",
        report.retired_in_units
    );
    // Exactly one entry opens per batch: the `jnz` back-edge is `DirectNear`, so it keeps the open
    // entry alive and the fall-through `sub` accrues to it rather than opening a second entry. A
    // broken back-edge (treating `jnz` as `Indirect`) would close after the branch and reopen on
    // the body, pushing entries past the batch count. So `entries <= batches` is the discriminating
    // proof, and entries stays strictly below the far larger retired stream.
    assert!(
        report.entries <= batches,
        "entries {} exceeded the batch count {} (back-edge did not keep the entry open)",
        report.entries,
        batches
    );
    assert!(
        report.entries < report.retired_in_units,
        "entries {} should stay below the retired stream {}",
        report.entries,
        report.retired_in_units
    );
}

/// Adversarial review F2: `finish_fast_map_write`'s fast BYTE-write path must feed the unit-sim
/// diagnostic (`note_code_write_hit`'s unconditional first action) on every real change, exactly
/// like the slow byte path (`write_linear_u8`'s `if changed { self.note_code_write(..) }`) --
/// even when the changed byte hits no watched code at all. The sim tracks unit ownership at PAGE
/// granularity (`page_owners`), coarser than `code_watch`'s precise per-instruction/per-block
/// spans, so "a byte the sim's unit owns but code_watch does not watch" is a real, constructible
/// case, not a hypothetical one: this fixture builds a unit over the `usim_program` loop, then
/// writes a DIFFERENT, never-decoded byte in the SAME page through the fast map. `IZARRAVM_UNIT_SIM`
/// exists specifically to model 486/586 -- the personas where the fast path is armed -- so silently
/// dropping this feed there defeats the diagnostic's purpose. Before this fix, gating on
/// `watched && changed` for every width silently skipped the sim feed here (report.sim_invalidations
/// would read 0); the fix keeps that gate for sized writes but not byte writes.
#[test]
fn fast_map_byte_write_feeds_unit_sim_even_when_unwatched() {
    // Same page as USIM_START/USIM_LOOP (0x200/0x201) but far from any decoded or compiled byte.
    const UNRELATED_BYTE: u32 = 0x0800;

    let mut cpu = fresh();
    let mut bus = TestBus::with_memory(usim_program(3));
    bus.direct_pages_enabled = true;
    cpu.set_jit_auto_admit(true);
    cpu.set_unit_sim_enabled(true);
    usim_arm(&mut cpu, 6);

    drive_to_halt(&mut cpu, &mut bus);
    assert!(
        !cpu.code_write_watched(UNRELATED_BYTE, 1),
        "fixture picked a byte that is already watched -- pick a different offset"
    );

    // Priming write: unpaged real mode is always permissive, but the FastMap write bias for this
    // page does not exist until a first write completes, so this one still takes the slow path.
    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        UNRELATED_BYTE,
        0,
        BusAccessKind::DataWrite,
    )
    .unwrap();

    // The measured write: must take the fast path, must change the byte, must hit no watched code.
    let hits_before = cpu.fast_map_probe_counters().hits;
    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        UNRELATED_BYTE,
        0xaa,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    assert_eq!(
        cpu.fast_map_probe_counters().hits,
        hits_before + 1,
        "the measured write did not take the fast path -- fixture is vacuous"
    );

    let reports = cpu.take_unit_sim_report().expect("sim enabled");
    let (cfg, report, _) = &reports[0];
    assert_eq!(*cfg, "L0", "the first ladder rung is L0");
    assert!(
        report.sim_invalidations > 0,
        "L0 saw no SMC kill -- the fast byte-write path did not feed the unit sim for a \
         changed-but-unwatched byte"
    );
}

/// A 16-bit code segment can never produce a Direct block, because `key_for` refuses on `!d`.
/// This asserts the admission path is not merely fruitless there but SKIPPED: the decode line's
/// `jit_direct_hotness` must stay at 0.
///
/// The hotness counter is the only observable difference this change makes, so it is the only
/// assertion that can detect it. Everything else about a 16-bit boundary is identical before and
/// after, because `try_direct_continuation` already returned Interpret on every `!d` path.
///
/// Three things this fixture must get right, each of which would otherwise make it pass on base
/// main and prove nothing:
///   - the decode line must be WARMED first, or `direct_hot` refuses on the tag/generation test
///     and never increments on either side;
///   - `set_auto_admit(true)` is required, because auto-admit defaults to false and the function
///     returns before `direct_hot` without it;
///   - the base-main expectation is 1, not 8: `DEFAULT_ADMISSION_HEAT` is 1 under `cfg(test)`,
///     so the counter saturates at 1 immediately.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn sixteen_bit_boundaries_skip_the_direct_admission_path() {
    fn hotness(cpu: &CpuGsw, lin: u32) -> u8 {
        let index = (lin & cpu.decode_cache.mask) as usize;
        cpu.decode_cache.lines[index].jit_direct_hotness
    }

    // A trivial 16-bit loop that halts, so the decode lines around 0x101 are warmed by a real
    // run rather than by hand.
    let program = || {
        let mut m = vec![0xf4u8; 0x1000];
        m[0x100] = 0x90; // nop, so 0x101 is reached as a continuation
        m[0x101] = 0x90;
        m[0x102] = 0x90;
        m[0x103] = 0xf4; // hlt
        m
    };

    // THE CASE: 16-bit CS. fresh16 leaves default_size_32 false.
    //
    // The level is now set EXPLICITLY. It used to be left at its default, which read 0 under
    // `cargo test` and made the early-out fire for free. Since the 486 measurement the default is
    // 1, so leaning on it would have quietly deleted this test's subject rather than failing.
    let mut cpu = fresh16();
    cpu.set_sixteen_bit_admission_level(0);
    let mut bus = TestBus::with_memory(program());
    cpu.registers.eip = 0x100;
    cpu.registers.set_esp(0x0700);
    drive_to_halt(&mut cpu, &mut bus);
    cpu.jit_direct.set_auto_admit(true);
    assert_eq!(
        hotness(&cpu, 0x101),
        0,
        "warm run must not have heated 0x101"
    );
    for _ in 0..4 {
        cpu.try_direct_continuation_for_test(&mut bus, 0x101, false)
            .unwrap();
    }
    assert_eq!(
        hotness(&cpu, 0x101),
        0,
        "a 16-bit boundary must not reach the decode cache's admission bookkeeping at all"
    );

    // POSITIVE CONTROL: the same shape in a 32-bit CS must still heat, so the early-out is proven
    // to be keyed on `!d` and not to have swallowed the 32-bit path. Warmed at d = true, so
    // `direct_hot`'s own `line.d != d` test cannot be what keeps the counter at 0.
    let mut wide = fresh();
    let mut wide_bus = TestBus::with_memory(program());
    wide.registers.eip = 0x100;
    wide.registers.set_esp(0x0700);
    drive_to_halt(&mut wide, &mut wide_bus);
    wide.jit_direct.set_auto_admit(true);
    wide.try_direct_continuation_for_test(&mut wide_bus, 0x101, true)
        .unwrap();
    assert!(
        hotness(&wide, 0x101) > 0,
        "a 32-bit boundary must still heat; the early-out must be keyed on !d only"
    );
}
