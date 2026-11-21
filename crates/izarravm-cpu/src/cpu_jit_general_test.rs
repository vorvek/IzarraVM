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
        cpu.decode_cache.packs[index].jit_direct_hotness
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
