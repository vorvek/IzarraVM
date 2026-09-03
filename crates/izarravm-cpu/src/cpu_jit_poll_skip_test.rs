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

use crate::PollMaskSource;
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
    // `poll_skip_eligible` (the interpreter's own gate) requires the Direct backend OFF,
    // so the rows that go through `poll_loop()` need this explicitly.
    cpu.set_native_backend_enabled(false);
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
///
/// The second half drives the INTERPRETER's own entry point rather than the raw scanner,
/// and it is the row that catches a hardcoded `true` at `build_poll_loop`'s call: a
/// 32-bit D1b shape must NOT certify there. That is also D1b-6's scope decision as an
/// assertion -- the `0x84` test slot is not inherently 16-bit, and admitting it for
/// 32-bit code would move gp2-586, which is this slice's control. A control that moves
/// is not a control.
#[test]
fn the_sixteen_bit_parameter_gates_the_three_slot_shape() {
    let (cpu, _bus) = warm_poll_code(D1_CODE, POLL3_STARTS, false, 0x0000_ffff, 0);
    let outcome = build_poll_loop_from(&cpu, POLL16_ENTRY, false);
    assert!(
        !matches!(outcome, PollScanOutcome::Found(_)),
        "sixteen_bit_ok == false must refuse the 16-bit shape; got {}",
        outcome_name(&outcome)
    );

    let (mut cpu, _bus) = warm_poll_code(D1B_CODE, POLL3_STARTS, true, 0xffff_ffff, 0x08);
    cpu.set_eip(POLL16_ENTRY);
    assert!(
        cpu.poll_loop().is_none(),
        "the D1b register-mask slot must stay refused in 32-bit code -- widening it          there would move gp2-586, the control"
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

/// **T-D1b-4** (round-2 MAJOR-8's form). `with_resolved_mask` is the IDENTITY on an
/// `Immediate` shape, and `fresh_iteration_spins` answers exactly as it did before the
/// D1b refactor for all four `(mask, branch_when_zero)` cells. This is what pins the D1
/// arm as unchanged by D1b: the mask resolution is a COPY on `PollLoop`, not a signature
/// change on `fresh_iteration_spins`, whose parameter is a STATUS BYTE and not a mask
/// (the interpreter passes a real device status there).
#[test]
fn resolving_the_mask_is_the_identity_on_an_immediate_shape() {
    for mask in [0x01u8, 0x08] {
        for jz in [false, true] {
            let code = [0xec, 0xa8, mask, if jz { 0x74 } else { 0x75 }, 0xfb];
            let (cpu, _bus) = warm_poll_code(&code, POLL3_STARTS, true, 0xffff_ffff, 0x40);
            let outcome = build_poll_loop_from(&cpu, POLL16_ENTRY, false);
            let PollScanOutcome::Found(poll) = outcome else {
                panic!(
                    "the 32-bit 3-slot shape must certify; got {}",
                    outcome_name(&outcome)
                );
            };
            assert_eq!(poll.mask_source(), PollMaskSource::Immediate(mask));
            let resolved = poll.with_resolved_mask(&cpu);
            assert_eq!(
                resolved, poll,
                "with_resolved_mask must be the identity on an Immediate shape, even with                  an unrelated AH live"
            );
            assert_eq!(poll.status_mask(), mask);
            assert_eq!(poll.fresh_iteration_spins(0), jz);
            assert_eq!(poll.fresh_iteration_spins(mask), !jz);
            assert_eq!(poll.fresh_backedge_taken(mask), !jz);
            assert!(poll.mask_is_resolved());
        }
    }
}

/// **T-D1b-5.** `TEST AL,AH` (`84 E0`) and `TEST AL,imm8` (`A8 imm`) with the SAME mask
/// value leave identical flags and charge identical clocks. Both reach the same
/// `alu(4, .., BusWidth::Byte)` call and both return `clocks(2)`, which is why D1b needs
/// no new clock constant and why `raw_core_clocks: 17` is shared unchanged. A
/// characterization pin: it is green today, and it exists so a future divergence in
/// either arm cannot land silently.
#[test]
fn the_two_test_encodings_charge_and_flag_identically() {
    for al in [0x00u8, 0x01, 0x08, 0x77, 0xff] {
        for mask in [0x01u8, 0x08] {
            let mut observed = Vec::new();
            for code in [[0xa8u8, mask], [0x84, 0xe0]] {
                let (mut cpu, mut bus) = warm_poll_code(&code, &[0], true, 0xffff_ffff, mask);
                cpu.registers
                    .set_eax((u32::from(mask) << 8) | u32::from(al));
                cpu.registers.eflags = 0x8d7;
                cpu.pending_flags = PendingFlags::default();
                cpu.elapsed_clocks = 0;
                cpu.set_eip(POLL16_ENTRY);
                cpu.run_budgeted(&mut bus, 0).expect("one TEST retires");
                cpu.materialize_flags();
                observed.push((cpu.registers.eflags, cpu.elapsed_clocks));
            }
            assert_eq!(
                observed[0], observed[1],
                "TEST AL,imm8 and TEST AL,AH must agree on flags and clocks                  (al={al:#04x} mask={mask:#04x})"
            );
        }
    }
}

/// **T-D1b-1.** The register-mask variant of the 3-slot shape -- `IN AL,DX` /
/// `TEST AL,AH` (`84 E0`) / `Jcc rel8` -- certifies in a 16-bit code segment with the
/// SAME descriptor as the immediate form: `raw_core_clocks == 17`, three fetches. Its
/// mask is recorded SYMBOLICALLY (`PollMaskSource::Ah`) and is not read at
/// certification. This is 81.68% of tyrian's declines, including the single hottest
/// site (0x1604fb, 965 M declines, 63% of all declines).
#[test]
fn the_register_mask_poll_shape_certifies_with_a_symbolic_mask() {
    let (cpu, _bus) = warm_poll_code(D1B_CODE, POLL3_STARTS, false, 0x0000_ffff, 0x08);
    let outcome = build_poll_loop_from(&cpu, POLL16_ENTRY, true);
    let PollScanOutcome::Found(poll) = outcome else {
        panic!(
            "the 16-bit TEST AL,AH shape must certify with sixteen_bit_ok; got {}",
            outcome_name(&outcome)
        );
    };
    assert_eq!(poll.family(), PollFamily::Io);
    assert_eq!(poll.raw_core_clocks(), 17);
    assert_eq!(poll.fetch_count(), 3);
    assert_eq!(poll.mask_source(), PollMaskSource::Ah);
    assert!(
        !poll.mask_is_resolved(),
        "an Ah shape is unresolved until the call-out reads AH"
    );
    let resolved = poll.with_resolved_mask(&cpu);
    assert_eq!(resolved.status_mask(), 0x08);
    assert!(resolved.mask_is_resolved());
}

/// **T-D1b-2.** The structural/register split, which is what keeps the 965 M-decline
/// site from rescanning: every `0x84` form that is NOT exactly `84 E0` refuses
/// CACHEABLY, because each rejection is a pure code-byte fact.
///
/// `84 C4` (`TEST AH,AL`) is included deliberately. AND is commutative, so it is the
/// SAME test with identical flags -- and it is still refused. The accepted set is
/// exactly one encoding, on purpose and fail-closed; a future site using the mirrored
/// form is a coverage gap to be diagnosed, not a bug to be hunted.
#[test]
fn every_other_register_mask_encoding_refuses_cacheably() {
    for (name, modrm) in [
        ("TEST AL,BH (reg != AH)", 0xf8u8),
        ("TEST BL,AH (rm != AL)", 0xe3),
        ("TEST [BX],AH (memory operand)", 0x27),
        (
            "TEST [BX+SI],AH (memory, same reg/rm fields as 84 E0)",
            0x20,
        ),
        ("TEST AH,AL (the mirrored encoding)", 0xc4),
    ] {
        let code = [0xec, 0x84, modrm, 0x75, 0xfb];
        let (cpu, _bus) = warm_poll_code(&code, POLL3_STARTS, false, 0x0000_ffff, 0x08);
        let outcome = build_poll_loop_from(&cpu, POLL16_ENTRY, true);
        assert!(
            matches!(outcome, PollScanOutcome::NegativeCacheable),
            "{name} must refuse CACHEABLY -- a structural negative is what stops a hot \
             site from rescanning per read; got {}",
            outcome_name(&outcome)
        );
    }
}

/// **T-D1b-6** (round-2 MAJOR-9's inverted form). `0x84` is deliberately NOT in
/// `poll_head_possible`'s opcode set, and the D1b arm carries a documented exemption
/// from the "every shape slot opcode is in the set" rule instead.
///
/// Adding it would have widened the INTERPRETER's cheap early-out on a common
/// instruction -- `TEST r/m8,r8` -- turning a free prefilter reject into a negative-cache
/// probe and, once per page generation, a full ten-probe scan, on a path no ladder arm
/// exercises. The exemption is sound because `poll_head_possible` is reachable only with
/// `sixteen_bit_ok == false`, under which the D1b arm cannot produce a shape at all.
/// Both halves are asserted here.
#[test]
fn the_register_mask_test_slot_is_exempt_from_the_head_prefilter_set() {
    // Half one: the prefilter rejects a 0x84 boundary, so 0x84 is not in the set.
    let (mut cpu, _bus) = warm_poll_code(D1B_CODE, POLL3_STARTS, true, 0xffff_ffff, 0x08);
    let before = cpu.perf_counters().poll_head_prefilter_rejects;
    cpu.set_eip(POLL16_ENTRY + 1); // the TEST slot
    assert!(cpu.poll_loop().is_none());
    assert_eq!(
        cpu.perf_counters().poll_head_prefilter_rejects,
        before + 1,
        "a 0x84 boundary must be answered by the prefilter, not by a scan"
    );

    // Half two: with the interpreter's own sixteen_bit_ok, the D1b arm produces nothing,
    // so the exemption cannot cost a certification that the prefilter would have allowed.
    let outcome = build_poll_loop_from(&cpu, POLL16_ENTRY, false);
    assert!(
        !matches!(outcome, PollScanOutcome::Found(_)),
        "the D1b arm must be unreachable without sixteen_bit_ok; got {}",
        outcome_name(&outcome)
    );
}

/// **T-D7.** The `IZARRAVM_DIRECT_POLL_SKIP_16` spelling table. A boolean arm knob on the
/// `IZARRAVM_JCC_SHADOW` / `IZARRAVM_CHAIN_ENTRY_CHECK` construction: unset and `""` name
/// the SAME arm, and since the 2026-08-29 flip that arm is ON.
///
/// **The `""` row moves WITH the default arm, deliberately, and that is why it is written
/// as "the same arm as unset" rather than as a literal.** The invariant it defends is the
/// AGREEMENT between the two spellings -- what stops a nulled PowerShell variable (present
/// and empty) from naming a different arm than an unset one. The flip moves both together;
/// it does not weaken the row. The cost is that the nulling trap now points at the OFF
/// leg: an OFF leg must export `0`.
#[test]
fn direct_poll_skip_16_spelling_table() {
    use std::env::VarError;
    let parse = jit::direct::parse_direct_poll_skip_16_arm_for_test;
    assert!(
        parse(Err(VarError::NotPresent)),
        "unset must name the ON arm: this knob ships default ON since 2026-08-29"
    );
    assert!(
        parse(Ok(String::new())),
        "the empty string must name the SAME arm as unset -- ON since the flip -- so a \
         nulled ladder leg cannot disagree with an unset one"
    );
    for off in ["0", "off", "OFF", " off ", "Off"] {
        assert!(!parse(Ok(off.to_string())), "{off:?} must name the OFF arm");
    }
    for on in ["1", "on", "ON", " On "] {
        assert!(parse(Ok(on.to_string())), "{on:?} must name the ON arm");
    }
}

/// A mistyped ladder leg must PANIC rather than silently run the default.
#[test]
#[should_panic(expected = "IZARRAVM_DIRECT_POLL_SKIP_16")]
fn a_mistyped_direct_poll_skip_16_arm_panics() {
    let _ = jit::direct::parse_direct_poll_skip_16_arm_for_test(Ok("yes".to_string()));
}

/// Non-UTF-8 is not a spelling of either arm -- it reaches the panic, not the unset
/// silence.
#[test]
#[should_panic(expected = "IZARRAVM_DIRECT_POLL_SKIP_16")]
fn non_utf8_direct_poll_skip_16_arm_panics() {
    let _ = jit::direct::parse_direct_poll_skip_16_arm_for_test(Err(
        std::env::VarError::NotUnicode(std::ffi::OsString::from("x")),
    ));
}

/// THE DEFAULT PIN: with the ambient variable read exactly as `direct_poll_skip_16_armed`
/// reads it, the process-wide reading must agree with the spelling table, and with the
/// variable unset the arm must be ON. Reads the AMBIENT knob deliberately, on the
/// `direct_poll_skip_ships_on_by_default` model, so the suite stays runnable on either arm.
#[test]
fn direct_poll_skip_16_ships_on_by_default() {
    let ambient = std::env::var("IZARRAVM_DIRECT_POLL_SKIP_16");
    let expected = jit::direct::parse_direct_poll_skip_16_arm_for_test(ambient.clone());
    assert_eq!(
        jit::direct::direct_poll_skip_16_armed(),
        expected,
        "the process-wide reading must agree with the spelling table applied to \
         IZARRAVM_DIRECT_POLL_SKIP_16={ambient:?}"
    );
    if matches!(ambient, Err(std::env::VarError::NotPresent)) {
        assert!(
            expected,
            "IZARRAVM_DIRECT_POLL_SKIP_16 must default ON: the ladder priced the arm \
             (tyrian-586 2.78x, 63.4 s -> 22.8 s min-wall, guest-visible state \
             bit-identical) and the owner approved the flip on 2026-08-29"
        );
    }
}

/// The D1b mask-decline memo's key discipline (review round-2 MAJOR-7's recommended
/// mitigation). It is a ONE-ENTRY, pure-refusal memo, and all three key components must
/// be load-bearing: a changed AH, an SMC restamp (page insert generation), or a different
/// slot must each re-enter the scan rather than inherit a stale refusal.
///
/// The memo exists because a `Found` is never written to the negative cache -- positives
/// are rebuilt on every call so an SMC restamp replaces the descriptor -- so a site whose
/// shape certifies structurally but whose live AH is not `0x01`/`0x08` would otherwise pay
/// a full ten-probe backward scan on EVERY read. There is no scan counter to assert
/// against, so the mechanism is pinned here structurally and its RATE is pinned at runtime
/// by the ladder STOP row on `poll_declined_mask_source` (1% of `poll_attempts`).
#[test]
fn the_mask_decline_memo_keys_on_the_slot_the_mask_and_the_page_generation() {
    let (mut cpu, _bus) = warm_poll_code(D1B_CODE, POLL3_STARTS, false, 0x0000_ffff, 0x40);
    assert!(
        !cpu.jit_direct
            .poll_mask_decline_memo_hit(POLL16_ENTRY, 0x40, 7),
        "an empty memo must refuse nothing"
    );
    cpu.jit_direct
        .record_poll_mask_decline(POLL16_ENTRY, 0x40, 7);
    assert!(
        cpu.jit_direct
            .poll_mask_decline_memo_hit(POLL16_ENTRY, 0x40, 7),
        "the recorded triple must be refused without a scan"
    );
    assert!(
        !cpu.jit_direct
            .poll_mask_decline_memo_hit(POLL16_ENTRY, 0x08, 7),
        "a changed AH must re-enter the scan -- the memo may not outlive the mask it          refused"
    );
    assert!(
        !cpu.jit_direct
            .poll_mask_decline_memo_hit(POLL16_ENTRY, 0x40, 8),
        "an SMC restamp bumps the page insert generation and must re-enter the scan"
    );
    assert!(
        !cpu.jit_direct
            .poll_mask_decline_memo_hit(POLL16_ENTRY + 1, 0x40, 7),
        "a different slot must re-enter the scan"
    );
}

// ---------------------------------------------------------------------------
// THE MUTATION RECORD for the 16-bit poll certification slice (D1 + D1b).
//
// Run by hand at implementation time against the design's §4.3 table. Recorded
// here rather than in a report file because this is where the rows it grades
// live, and because a mutation table nobody can re-run is a claim rather than
// evidence. Each row names the edit and the fixture that went RED under it.
//
//   1. Delete the D-O1 `cs.limit <= 0xFFFF` term in `build_poll_loop_at`
//      => a_sixteen_bit_shape_over_a_big_limit_segment_is_volatile RED ("got
//         Found"). Verified to FIRE, which is the point MAJOR-2 made: under a
//         real-mode `limit == 0xFFFF` fixture this row would still pass with the
//         term deleted, because poll_slots_within_live_cs already catches the
//         wrap. The big-limit segment is what makes the row able to fail.
//   2. Relax D1b's `modrm.reg == 4` to any reg
//      => every_other_register_mask_encoding_refuses_cacheably RED.
//   3. Accept a memory operand in the D1b test slot
//      => every_other_register_mask_encoding_refuses_cacheably RED.
//      This row needed the fixture fixed before it could fire. With `mod == 3`
//      also required, `84 27` was refused by the ModRM triple and the operand
//      term could never be load-bearing -- a gate that cannot fail. The check is
//      now `reg == 4` plus `DecodedOperand::Reg(0)`, and the `84 20` row
//      (`TEST [BX+SI],AH`, whose reg/rm fields match `84 E0`'s exactly) is what
//      the operand term alone refuses.
//   4. Add `0x84` to poll_head_possible's opcode set
//      => the_register_mask_test_slot_is_exempt_from_the_head_prefilter_set RED.
//      The INVERTED form MAJOR-9 asked for: the set membership is the defect and
//      the exemption is the invariant.
//   5. Hardcode `sixteen_bit_ok = true` at the interpreter's build_poll_loop
//      => the_sixteen_bit_parameter_gates_the_three_slot_shape RED.
//      This row also needed the fixture strengthened. Driving the raw scanner
//      with `false` cannot see a mutated call site, and a 16-bit head is refused
//      by poll_head_possible's `!d` first, so the row that fires is the 32-bit
//      D1b shape refusing through `cpu.poll_loop()` -- which is D1b-6's
//      gp2-stays-a-control decision as an assertion.
//   6. Change raw_core_clocks 17 -> 16 on the 3-slot arm
//      => both 16-bit certification rows RED.
//   7. Move D1b's mask check from the call-out into certification (make
//      with_resolved_mask synthesise a mask instead of reading AH)
//      => a_wrong_mask_value_declines_through_its_own_lane_without_caching RED.
//   8. Drop the mask-value check at the call-out entirely
//      => a_wrong_mask_value_declines_through_its_own_lane_without_caching RED.
//   9. Move the knob test out of the 16-bit screen, so the screen always opens
//      => the_sixteen_bit_off_arm_screens_before_the_scan RED, and
//         a_sixteen_bit_callout_declines_before_the_scan RED with it.
//      Worth naming: the design recorded knob ORDERING as a mutation NO unit
//      test could catch, guarded only by the ladder's A-arm identity check.
//      That gap is now closed by a fixture.
//
// TWO DISCLOSED GAPS, stated rather than papered over.
//
//   * Feeding `fresh_iteration_spins` and `req.status_mask` independently-derived
//     masks. Not expressible any more: with_resolved_mask returns a settled COPY
//     and both fields read `poll.status_mask()`, so there is exactly ONE
//     derivation by construction -- which is the obligation, obtained
//     structurally. A mutation that substitutes an arbitrary CONSTANT at one of
//     the two uses is a different defect, and it survives: it inverts
//     spins_when_bit_set, the seam declines NotSpinning while the guest is
//     spinning, and then commits during the opposite half of the line. Catching
//     that needs a skipped-versus-unskipped clock-identity fixture, which this
//     repository could not build (see this file's header note on converging
//     back-edges). The related mutation that IS caught is making
//     with_resolved_mask never read AH at all (row 7).
//   * Reading AH at certification time and reusing it across the span. No test
//     catches it TODAY because AH is loop-invariant over the certified slot set
//     (IN writes AL only, TEST writes flags only, Jcc writes nothing). It
//     becomes catchable only if a future shape admits a body that can write the
//     mask register. Carried forward from the design as a known gap.
// ---------------------------------------------------------------------------

/// **P2 / F8.** The per-iteration RAW core charge an elided iteration projects must be read from
/// the LIVE I/O privilege column, never baked. Every certified `Io` shape carries exactly one
/// `IN` slot at epoch 1's flat `IN_PORT_CORE_CLOCKS` (12) inside its `raw_core_clocks`; under
/// epoch 2 that one term becomes Intel's column for the mode the CPU is actually in, so an
/// elided iteration advances the guest clock by exactly what the executed `IN` would have.
///
/// The four expected values are written as literals -- Intel's `IN` 7 / 4 / 21 / 19 times the
/// I586 `level_timing` denominator 12 -- rather than read back from the table under test.
///
/// Epoch 1 must return the shape's own 17 in every column, byte-identically: that is the
/// knob-unset merge bar.
#[test]
fn poll_skip_raw_core_clocks_reads_the_live_privilege_column() {
    let (mut cpu, mut bus) = warm_poll_code(D1_CODE, POLL3_STARTS, true, 0xffff_ffff, 0);
    let PollScanOutcome::Found(poll) = build_poll_loop_from(&cpu, POLL16_ENTRY, false) else {
        panic!("the 3-slot shape must certify");
    };
    assert_eq!(poll.raw_core_clocks(), 17, "12 + TEST 2 + taken Jcc 3");

    // (set-up closure, expected epoch-2 raw): real, protected CPL<=IOPL, protected CPL>IOPL, V86.
    type Column = (fn(&mut CpuGsw), u64);
    let columns: [Column; 4] = [
        (
            |cpu| {
                cpu.control.cr0 &= !CR0_PE;
                cpu.registers.eflags &= !FLAG_VM;
                cpu.cpl = 0;
            },
            17 - 12 + 84,
        ),
        (
            |cpu| {
                cpu.control.cr0 |= CR0_PE;
                cpu.registers.eflags &= !FLAG_VM;
                cpu.registers.eflags |= 3 << 12;
                cpu.cpl = 0;
            },
            17 - 12 + 48,
        ),
        (
            |cpu| {
                cpu.control.cr0 |= CR0_PE;
                cpu.registers.eflags &= !FLAG_VM;
                cpu.registers.eflags &= !(3 << 12);
                cpu.cpl = 3;
            },
            17 - 12 + 252,
        ),
        (
            |cpu| {
                cpu.control.cr0 |= CR0_PE;
                cpu.registers.eflags |= FLAG_VM | (3 << 12);
                cpu.cpl = 3;
            },
            17 - 12 + 228,
        ),
    ];
    for (index, (setup, expected)) in columns.into_iter().enumerate() {
        setup(&mut cpu);
        bus.timing_epoch_two = false;
        assert_eq!(
            cpu.poll_skip_raw_core_clocks(poll, &bus),
            17,
            "column {index}: epoch 1 must return the shape's own baked figure, byte-identically"
        );
        bus.timing_epoch_two = true;
        assert_eq!(
            cpu.poll_skip_raw_core_clocks(poll, &bus),
            expected,
            "column {index}: epoch 2 must swap the baked IN for Intel's own column"
        );
    }
}
