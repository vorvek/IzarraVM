// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! `IZARRAVM_DIRECT_POLL_SKIP` (GP2 call-out-site poll skip), unit-level tests that do not need a
//! real bus. The engagement/mechanism fixtures that need `CpuBus::callout_poll_skip`'s real body
//! live in `izarravm-machine`'s `machine_direct_poll_skip_test.rs` (design BLOCKER D: `TestBus`
//! must not implement that method).

use super::*;

/// **M-28.** The `IZARRAVM_DIRECT_POLL_SKIP` spelling table: unset and `""` name the SAME arm
/// (the default, ON since the 2026-08-27 owner approval) -- the `IZARRAVM_CHAIN_ENTRY_CHECK` /
/// `IZARRAVM_JCC_SHADOW` shape, deliberately NOT `IZARRAVM_ATA_POLL_SKIP`'s (design §6).
#[test]
fn direct_poll_skip_spelling_table() {
    use std::env::VarError;
    let parse = jit::direct::parse_direct_poll_skip_arm_for_test;
    assert!(
        parse(Err(VarError::NotPresent)),
        "unset must name the ON arm: this knob ships default ON since 2026-08-27"
    );
    assert!(
        parse(Ok(String::new())),
        "the empty string must name the SAME arm as unset -- the default, deliberately NOT \
         ATA's inverted shape"
    );
    for off in ["0", "off", "OFF", " off ", "Off"] {
        assert!(!parse(Ok(off.to_string())), "{off:?} must name the OFF arm");
    }
    for on in ["1", "on", "ON", " On ", "poll", "POLL"] {
        assert!(parse(Ok(on.to_string())), "{on:?} must name the ON arm");
    }
}

/// A mistyped ladder leg must PANIC rather than silently run the default -- the one wrong
/// conclusion an arm ladder exists to avoid.
#[test]
#[should_panic(expected = "IZARRAVM_DIRECT_POLL_SKIP")]
fn a_mistyped_direct_poll_skip_arm_panics() {
    let _ = jit::direct::parse_direct_poll_skip_arm_for_test(Ok("yes".to_string()));
}

/// Non-UTF-8 is not a spelling of either arm -- reaches the panic, not the unset silence.
#[test]
#[should_panic(expected = "IZARRAVM_DIRECT_POLL_SKIP")]
fn non_utf8_direct_poll_skip_arm_panics() {
    let _ = jit::direct::parse_direct_poll_skip_arm_for_test(Err(std::env::VarError::NotUnicode(
        std::ffi::OsString::from("x"),
    )));
}

/// THE DEFAULT PIN: with the ambient env var read exactly as `direct_poll_skip_armed` reads it,
/// the process-wide OnceLock reading must agree with the spelling table, and with the variable
/// unset the arm must be ON (the 2026-08-27 flip). Reads the AMBIENT knob deliberately (on the
/// `jcc_shadow_ships_off_by_default` model) so the suite stays runnable on either arm.
#[test]
fn direct_poll_skip_ships_on_by_default() {
    let ambient = std::env::var("IZARRAVM_DIRECT_POLL_SKIP");
    let expected = jit::direct::parse_direct_poll_skip_arm_for_test(ambient.clone());
    assert_eq!(
        jit::direct::direct_poll_skip_armed(),
        expected,
        "the process-wide reading must agree with the spelling table applied to \
         IZARRAVM_DIRECT_POLL_SKIP={ambient:?}"
    );
    // GP2 poll-skip revision review N5: `ambient.is_err()` alone is imprecise here -- it reads as
    // covering every `VarError`, but `NotUnicode` would already have panicked one line up (inside
    // `parse_direct_poll_skip_arm_for_test`, pinned by `non_utf8_direct_poll_skip_arm_panics`
    // above), so this branch is only ever reached for `NotPresent` in practice. Spell that out
    // instead of the broader, misleading `is_err()`.
    if matches!(ambient, Err(std::env::VarError::NotPresent)) {
        assert!(
            expected,
            "IZARRAVM_DIRECT_POLL_SKIP must default ON: the L1 ladder priced the arm (gp2-586 \
             +40.1% min-wall) and the owner approved the flip on 2026-08-27"
        );
    }
}

// ---------------------------------------------------------------------------
// 16-bit poll certification at the call-out (design D1 / D1b), unit level.
//
// The shapes here are certified through `build_poll_loop_from`'s explicit
// `sixteen_bit_ok` parameter, which is `false` at every interpreter call site
// (`build_poll_loop`) and `true` only at the Direct call-out under
// `IZARRAVM_DIRECT_POLL_SKIP_16`. Every row states which value it passes, so
// the parameter's gating is pinned rather than assumed.
// ---------------------------------------------------------------------------

use crate::jit::block::{PollScanOutcome, build_poll_loop_from};

const POLL16_ENTRY: u32 = 0x501;

/// Name a scan outcome, so a failing row prints which lane it landed in rather
/// than just `false`.
fn outcome_name(outcome: &PollScanOutcome) -> &'static str {
    match outcome {
        PollScanOutcome::Found(_) => "Found",
        PollScanOutcome::NegativeCacheable => "NegativeCacheable",
        PollScanOutcome::NegativeVolatile => "NegativeVolatile",
    }
}

/// Warm `code` at `POLL16_ENTRY` under a code segment with the given `d` and `limit`,
/// stepping the decode cache over each slot start exactly as the 32-bit poll fixtures'
/// `warm_exact_poll` does. The segment is installed BEFORE the warm walk, so every
/// cached line is keyed on this `d` (`DecodeCache`'s liveness test includes `line.d`).
fn warm_poll_code(code: &[u8], starts: &[u32], d: bool, limit: u32, ah: u8) -> (CpuGsw, TestBus) {
    let mut memory = vec![0xf4; 0x3000];
    let at = POLL16_ENTRY as usize;
    memory[at..at + code.len()].copy_from_slice(code);
    let mut cpu = CpuGsw::default();
    cpu.set_fast_map_enabled_for_test(true);
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE;
    let mut cs = SegmentRegister::flat(0x08, 0x9b);
    cs.default_size_32 = d;
    cs.limit = limit;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(0x10, 0x93));
    }
    cpu.registers.set_edx(0xaaaa_03da);
    cpu.write_gpr8(4, ah);
    let mut bus = TestBus::with_memory(memory);
    bus.lazy_io_reads = true;
    for offset in starts {
        cpu.set_eip(POLL16_ENTRY + offset);
        cpu.fetch_decoded(&mut bus, POLL16_ENTRY + offset)
            .expect("poll slot decode");
    }
    cpu.set_eip(POLL16_ENTRY);
    (cpu, bus)
}

/// The D1 shape: `IN AL,DX / TEST AL,imm8 / Jcc rel8`, self-loop.
const D1_CODE: &[u8] = &[0xec, 0xa8, 0x08, 0x75, 0xfb];
/// The D1b shape: `IN AL,DX / TEST AL,AH (84 E0) / Jcc rel8`, self-loop. The
/// `MOV AH,8` that stages the mask lives OUTSIDE the loop and is not a slot.
const D1B_CODE: &[u8] = &[0xec, 0x84, 0xe0, 0x75, 0xfb];
const POLL3_STARTS: &[u32] = &[0, 1, 3];

/// **T-D1.** The textbook 3-slot shape in a real-mode-shaped 16-bit code segment
/// certifies at the call-out's `sixteen_bit_ok`, with the SAME descriptor a 32-bit
/// segment produces: `Io` family, `raw_core_clocks == 17`, three fetches. None of the
/// three opcodes decodes differently under `CS.D = 0` (`0xEC` is fixed 1 byte and
/// always an 8-bit port read into AL; the accumulator `TEST` and `Jcc rel8` are
/// operand-size-invariant), which is the whole argument this row pins.
#[test]
fn a_sixteen_bit_three_slot_poll_shape_certifies_at_the_callout() {
    let (cpu, _bus) = warm_poll_code(D1_CODE, POLL3_STARTS, false, 0x0000_ffff, 0);
    let outcome = build_poll_loop_from(&cpu, POLL16_ENTRY, true);
    let PollScanOutcome::Found(poll) = outcome else {
        panic!(
            "the 16-bit 3-slot shape must certify with sixteen_bit_ok; got {}",
            outcome_name(&outcome)
        );
    };
    assert_eq!(poll.family(), PollFamily::Io);
    assert_eq!(poll.raw_core_clocks(), 17);
    assert_eq!(poll.fetch_count(), 3);
    assert_eq!(poll.status_mask(), 0x08);
    assert_eq!(poll.resolved_port(&cpu), 0x03da);
    assert!(poll.at_head());
}

/// **T-D2.** The one reachable 16-bit hazard: `CS.D = 0` with a limit ABOVE `0xFFFF`
/// (a 16-bit protected-mode code segment with `G = 1`, or any descriptor whose D bit
/// and limit disagree). IP wraps at `0xFFFF` there but `fetch_within_limit` does not
/// catch it, and the call-out's own scan anchor (`cs.base + eip + slot_delta`) is
/// unmasked. The admission term `cs.limit <= 0xFFFF` refuses it, and it must refuse
/// through `NegativeVolatile`: `cs.limit` is a SEGMENT fact and the negative cache is
/// keyed on `(lin, d)` only, so caching this would poison the entry for every other
/// segment state over the same bytes (`PollScanOutcome`'s written forward rule).
///
/// The big-limit segment is mandatory rather than incidental: under a real-mode
/// `limit == 0xFFFF` fixture this row would still pass with the term DELETED, because
/// `poll_slots_within_live_cs` already catches every wrapping slot. It would be a
/// fixture that cannot fail.
#[test]
fn a_sixteen_bit_shape_over_a_big_limit_segment_is_volatile() {
    let (cpu, _bus) = warm_poll_code(D1_CODE, POLL3_STARTS, false, 0xffff_ffff, 0);
    let outcome = build_poll_loop_from(&cpu, POLL16_ENTRY, true);
    assert!(
        matches!(outcome, PollScanOutcome::NegativeVolatile),
        "CS.D=0 with limit 0xFFFFFFFF must refuse VOLATILE (never cacheable); got {}",
        outcome_name(&outcome)
    );
}

/// **T-D2b.** The parameter actually gates: the same certified bytes, under the
/// INTERPRETER's `sixteen_bit_ok == false`, are a negative. This is what keeps
/// `CpuGsw::poll_loop` byte-for-byte unchanged by the slice.
#[test]
fn the_sixteen_bit_parameter_gates_the_three_slot_shape() {
    let (cpu, _bus) = warm_poll_code(D1_CODE, POLL3_STARTS, false, 0x0000_ffff, 0);
    let outcome = build_poll_loop_from(&cpu, POLL16_ENTRY, false);
    assert!(
        !matches!(outcome, PollScanOutcome::Found(_)),
        "sixteen_bit_ok == false must refuse the 16-bit shape; got {}",
        outcome_name(&outcome)
    );
}

/// **T-D3.** D1 opened exactly ONE shape family. The 5-slot setup shape and the M1
/// memory shape stay refused in a 16-bit segment even with `sixteen_bit_ok`, and they
/// refuse CACHEABLY -- their `Dword` operand/address-size terms are code-byte facts.
#[test]
fn sixteen_bit_admission_opens_exactly_one_shape_family() {
    // `mov edx,ecx / sub eax,eax / in al,dx / test al,8 / jnz -9` -- the 5-slot form.
    let setup = [0x89u8, 0xca, 0x29, 0xc0, 0xec, 0xa8, 0x08, 0x75, 0xf7];
    let (cpu, _bus) = warm_poll_code(&setup, &[0, 2, 4, 5, 7], false, 0x0000_ffff, 0);
    let outcome = build_poll_loop_from(&cpu, POLL16_ENTRY, true);
    assert!(
        matches!(outcome, PollScanOutcome::NegativeCacheable),
        "the 5-slot setup shape stays 32-bit-only; got {}",
        outcome_name(&outcome)
    );

    // `cmp ecx,[disp32] / jnz -8` -- the M1 memory form's 32-bit bytes.
    let memory = [0x3bu8, 0x0d, 0x00, 0x20, 0x00, 0x00, 0x75, 0xf8];
    let (cpu, _bus) = warm_poll_code(&memory, &[0, 6], false, 0x0000_ffff, 0);
    let outcome = build_poll_loop_from(&cpu, POLL16_ENTRY, true);
    assert!(
        matches!(outcome, PollScanOutcome::NegativeCacheable),
        "the M1 memory shape stays 32-bit-only; got {}",
        outcome_name(&outcome)
    );
}
