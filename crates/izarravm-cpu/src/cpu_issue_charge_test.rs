// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Unit tests for the 2026-09-05 issue-charge prototype: the IZARRAVM_ISSUE_RAW,
//! IZARRAVM_X87_INTCONVERT_MUL and (machine-side) IZARRAVM_VIDEO_MODE13_WS knobs.
//!
//! Every knob defaults OFF (`issue_raw_policy(None) == 0`,
//! `x87_intconvert32_override_policy(None) == None`) and the CPU reads them ONCE at
//! construction into plain fields, never per instruction -- these tests exercise the pure
//! policy functions and the field-driven math directly rather than mutating process
//! environment state, so they are safe under a parallel test run.

use super::*;

// ---------------------------------------------------------------------------
// Knob-unset defaults: bit-identical to today's model.
// ---------------------------------------------------------------------------

#[test]
fn issue_raw_policy_unset_or_empty_is_zero() {
    assert_eq!(issue_raw_policy(None), 0);
    assert_eq!(issue_raw_policy(Some("")), 0);
}

#[test]
fn issue_raw_policy_malformed_falls_back_to_zero_rather_than_panicking() {
    assert_eq!(issue_raw_policy(Some("not-a-number")), 0);
    assert_eq!(issue_raw_policy(Some("-3")), 0);
}

#[test]
fn issue_raw_policy_parses_a_decimal_value() {
    assert_eq!(issue_raw_policy(Some("7")), 7);
    assert_eq!(issue_raw_policy(Some("0")), 0);
}

#[test]
fn x87_intconvert32_override_policy_unset_or_empty_is_none() {
    assert_eq!(x87_intconvert32_override_policy(None), None);
    assert_eq!(x87_intconvert32_override_policy(Some("")), None);
}

#[test]
fn x87_intconvert32_override_policy_malformed_is_none() {
    assert_eq!(x87_intconvert32_override_policy(Some("x")), None);
}

#[test]
fn x87_intconvert32_override_policy_parses_the_multiplier() {
    assert_eq!(x87_intconvert32_override_policy(Some("2")), Some(2));
}

#[test]
fn cpu_gsw_default_has_every_issue_charge_knob_off() {
    // `Default` does not go through `issue_raw_default`/`x87_intconvert32_override_default`
    // (those read the environment), but production construction does; this pins the SHAPE the
    // fields must have when unset -- 0 and None -- which is what every hot-path add-site relies
    // on to be a true no-op.
    let cpu = CpuGsw::default();
    assert_eq!(cpu.issue_raw, 0, "issue_raw must default to 0 (off)");
    assert_eq!(
        cpu.x87_intconvert32_num, None,
        "x87_intconvert32_num must default to None (off)"
    );
}

// ---------------------------------------------------------------------------
// scale_clocks: issue_raw unset is a true no-op; issue_raw=N charges exactly N raw
// clocks per instruction, exact over a run through the shared timing_rem carry.
// ---------------------------------------------------------------------------

#[test]
fn issue_raw_zero_leaves_scale_clocks_unchanged() {
    let mut with_zero = CpuGsw::default();
    with_zero.set_mode(GswMode::Gsw586);
    with_zero.issue_raw = 0;
    let mut baseline = CpuGsw::default();
    baseline.set_mode(GswMode::Gsw586);

    // A representative core-clocks sequence (2-raw ALU ops, a 3-raw Jcc, a fetch-sized value),
    // charged instruction by instruction the way `finish_instruction` does.
    for &core_clocks in &[2u32, 2, 3, 7, 61, 0] {
        let a = with_zero.scale_clocks(core_clocks.saturating_add(with_zero.issue_raw));
        let b = baseline.scale_clocks(core_clocks);
        assert_eq!(
            a, b,
            "issue_raw=0 must charge exactly what scale_clocks alone charges"
        );
    }
    assert_eq!(with_zero.timing_rem, baseline.timing_rem);
}

#[test]
fn issue_raw_seven_charges_exactly_seven_raw_clocks_per_instruction_with_carry() {
    // Per-instruction charging, the way every retire site now calls scale_clocks:
    // scale_clocks(core_clocks + issue_raw).
    let mut per_instruction = CpuGsw::default();
    per_instruction.set_mode(GswMode::Gsw586);
    per_instruction.issue_raw = 7;
    let core_clocks_sequence = [2u32, 2, 2, 3, 2, 11, 2];
    let mut summed = 0u64;
    for &c in &core_clocks_sequence {
        summed += per_instruction.scale_clocks(c.saturating_add(per_instruction.issue_raw));
    }

    // The batched form a native block's single charge takes: one scale_clocks_batch call over
    // the whole raw total (sum of core clocks plus issue_raw times instruction count), from the
    // same starting remainder (0). Exactness here is exactly what `run_direct_block`'s per-block
    // charge depends on to agree with the interpreter's per-instruction charge.
    let mut batched = CpuGsw::default();
    batched.set_mode(GswMode::Gsw586);
    let raw_core_total: u64 = core_clocks_sequence.iter().map(|&c| u64::from(c)).sum();
    let issue_total = 7u64 * core_clocks_sequence.len() as u64;
    let charged = batched.scale_clocks_batch(raw_core_total + issue_total);

    assert_eq!(
        summed, charged,
        "per-instruction issue charge must sum to exactly the batched charge"
    );
}

// ---------------------------------------------------------------------------
// x87 IntConvert32 override: applies ONLY to (I586, IntConvert32); None reproduces
// fp_timing_class byte-for-byte.
// ---------------------------------------------------------------------------

#[test]
fn x87_override_none_reproduces_fp_timing_class_exactly() {
    for persona in [CpuPersona::I386, CpuPersona::I486, CpuPersona::I586] {
        for class in [
            FpOpClass::IntConvert32,
            FpOpClass::IntConvert16,
            FpOpClass::F32Mem,
            FpOpClass::F64Mem,
            FpOpClass::Register,
            FpOpClass::Wait,
        ] {
            assert_eq!(
                effective_fp_timing_class(persona, class, None),
                fp_timing_class(persona, class),
                "{persona:?}/{class:?}: None must be a true no-op"
            );
        }
    }
}

#[test]
fn x87_override_applies_only_to_i586_intconvert32() {
    assert_eq!(
        effective_fp_timing_class(CpuPersona::I586, FpOpClass::IntConvert32, Some(2)),
        16,
        "override value IS the multiplier: 2 * FP_TIMING_DEN(8) = 16"
    );
    // Every other class, same persona, same knob set: untouched.
    for class in [
        FpOpClass::IntConvert16,
        FpOpClass::F32Mem,
        FpOpClass::F64Mem,
        FpOpClass::Register,
        FpOpClass::Wait,
    ] {
        assert_eq!(
            effective_fp_timing_class(CpuPersona::I586, class, Some(2)),
            fp_timing_class(CpuPersona::I586, class),
            "{class:?} must not move when only IntConvert32 is overridden"
        );
    }
    // Other personas, same knob set: untouched (486/I386 route the x87 dial through
    // level_timing alone and never consult this override).
    for persona in [CpuPersona::I386, CpuPersona::I486] {
        assert_eq!(
            effective_fp_timing_class(persona, FpOpClass::IntConvert32, Some(2)),
            fp_timing_class(persona, FpOpClass::IntConvert32),
            "{persona:?} must not move when only I586 IntConvert32 is overridden"
        );
    }
}

#[test]
fn scale_fp_clocks_respects_the_hoisted_override_field() {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.x87_intconvert32_num = Some(2);
    cpu.fp_rem = 0;
    // One FILD/FISTP-shaped op at raw_clocks=14 (the doc's cited value): 14 * 16 / 8 = 28.
    let scaled = cpu.scale_fp_clocks(14, FpOpClass::IntConvert32);
    assert_eq!(scaled, 28);
}
