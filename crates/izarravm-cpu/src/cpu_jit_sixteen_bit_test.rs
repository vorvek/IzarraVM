// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Direct-backend coverage for 16-bit code segments (CS.D = 0).
//!
//! Before the S4a admission change there was NONE: sweeping the crate for a Direct entry point
//! called with `d == false` returned exactly two hits, the clif key and the test asserting a
//! 16-bit boundary is skipped. Eight merged slices built the 16-bit mechanisms and every one of
//! them was gated on nothing changing, so the fixtures here carry the whole correctness argument
//! for all of them.
//!
//! In particular #635 (16-bit addressing) merged with FIVE of eleven mutations surviving, all for
//! one cause: the routing of the block-level `address_wrap` to each address consumer is only
//! observable in a block whose wrap is `Word`, which requires `d == false`, which `key_for`
//! refused. Four tests below are that debt: the wrapping `[BP+disp]` operand, the LEA, the x87
//! memory pointer, and the 32-bit stack inside a 16-bit code segment.
//!
//! These execute through `try_run_direct_block_for_test`, which calls `run_direct_block`
//! directly. That is deliberate: the `run.rs` continuation early-out still refuses `!d` at this
//! stage, so the ordinary admission path cannot reach a 16-bit block until S4b. Compiling and
//! installing by hand is the only way to run one, and it is why S4a can carry the correctness
//! work while remaining inert in production.

use super::*;

const ENTRY: u32 = 0x100;

/// An owned snapshot of the counters these fixtures assert on.
///
/// `perf_counters()` hands back a reference, so reading it into a local would hold a borrow
/// across the `&mut self` run call. Copying the fields out is the whole reason this exists.
#[derive(Clone, Copy, Debug)]
struct Counts {
    entries: u64,
    entries_sixteen_bit: u64,
    insns: u64,
    insns_sixteen_bit: u64,
    side_exits: u64,
    alignment_exits: u64,
}

fn counts(cpu: &CpuGsw) -> Counts {
    let perf = cpu.perf_counters();
    Counts {
        entries: perf.jit_direct_entries,
        entries_sixteen_bit: perf.jit_direct_entries_sixteen_bit,
        insns: perf.jit_direct_insns,
        insns_sixteen_bit: perf.jit_direct_insns_sixteen_bit,
        side_exits: perf.jit_direct_side_exits,
        alignment_exits: perf.jit_direct_exit_cross_page_or_alignment,
    }
}

/// Real mode with a 16-bit CODE segment, which is what `fresh()` deliberately is not.
///
/// `fresh()` loads every segment in real mode and then forces `cs.default_size_32 = true`. This
/// drops that one line, so CS.D is 0 and SS.B is 0: the ordinary DOS configuration, and the
/// population S4 admits.
pub(super) fn sixteen_bit_code_cpu(entry: u32) -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
    ] {
        cpu.load_segment_real(segment, 0);
    }
    cpu.registers.set_esp(0x0700);
    cpu.set_eip(entry);
    cpu
}

pub(super) fn sixteen_bit_bus(memory: Vec<u8>) -> TestBus {
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus
}

/// Arm a CPU to run 16-bit blocks natively: fast map on, and every page the fixture touches
/// mapped for read and write. A memory-form slot silently never compiles without this, and the
/// test then passes interpreted.
pub(super) fn arm_native_sixteen_bit(cpu: &mut CpuGsw, bus: &mut TestBus, pages: &[u32]) {
    cpu.set_fast_map_enabled_for_test(true);
    for &page in pages {
        map_direct_page(
            cpu,
            bus,
            page,
            page,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            true,
        );
    }
}

/// Compile at `entry` with `d = false`, install, and hand back the runnable block.
///
/// The `probe` call is not decoration: `install` expects the key to have been seen, and the probe
/// is what inserts it. It also asserts the block is not somehow already present, which would make
/// every downstream assertion measure a stale compilation.
fn install_sixteen_bit_block(
    cpu: &mut CpuGsw,
    entry: u32,
    expected_instructions: u8,
) -> jit::direct::CompiledBlock {
    let compilation = match jit::direct::compile(cpu, entry, false) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("the 16-bit block became a structural rejection")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("the 16-bit block requested a retry"),
    };
    assert_eq!(
        compilation.span.instructions, expected_instructions,
        "block shape moved; every assertion below is about a different block"
    );
    let key = jit::direct::key_for(cpu, entry, false).expect("a 16-bit key after the flip");
    assert_eq!(key.mode_key & 1, 0, "mode-key bit 0 must report CS.D = 0");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("install a 16-bit block");
    cpu.jit_direct
        .block(id)
        .expect("installed block must be live")
}

/// Warm the decode cache over `starts`, which the compile loop reads through `decode_cache.get`.
pub(super) fn warm_sixteen_bit(cpu: &mut CpuGsw, bus: &mut TestBus, starts: &[u32]) {
    let saved = cpu.registers.eip;
    for &linear in starts {
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(bus, linear).expect("fixture decode");
    }
    cpu.set_eip(saved);
}

// ---------------------------------------------------------------------------
// 1. The anti-vacuity gate: a 16-bit block compiles, runs, and is attributed.
// ---------------------------------------------------------------------------

/// Four register slots in a 16-bit code segment. Every opcode here is in the Word allowlist, and
/// every one of them decodes at `OperandSize::Word` because the size follows CS.D rather than the
/// opcode.
///
/// This is the test that fails if the admission flip did not happen, and it is the only thing
/// standing between S4a and a slice that ships doing nothing while every counter-identity gate
/// passes. The campaign has shipped that shape twice.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_sixteen_bit_block_compiles_runs_and_counts_as_sixteen_bit() {
    let mut memory = vec![0u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + 6].copy_from_slice(&[
        0x40, // inc ax
        0x41, // inc cx
        0x42, // inc dx
        0x8b, 0xc3, // mov ax,bx
        0xf4, // hlt, unclassifiable, so it ends the block
    ]);
    let mut bus = sixteen_bit_bus(memory);
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000]);
    warm_sixteen_bit(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );

    let block = install_sixteen_bit_block(&mut cpu, ENTRY, 4);

    cpu.registers.gpr = [0; 8];
    cpu.registers.set_ebx(0x1111_2222);
    cpu.set_eip(ENTRY);
    let before = counts(&cpu);
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block).unwrap(),
        "the installed 16-bit block must actually run"
    );
    let after = counts(&cpu);

    // Guest state, which is what any of this is for. `mov ax,bx` is a 16-bit move, so EAX keeps
    // the high half it had, which is 0 here, and takes BX's low half.
    assert_eq!(cpu.registers.eax(), 0x2222);
    assert_eq!(cpu.registers.ecx(), 1);
    assert_eq!(cpu.registers.edx(), 1);
    assert_eq!(cpu.registers.eip, ENTRY + 5);

    // The mechanism counters, which are the campaign's only attribution for whether the 16-bit
    // work is reached. `_insns` carries the yield model's own unit.
    assert_eq!(after.entries - before.entries, 1);
    assert_eq!(after.entries_sixteen_bit - before.entries_sixteen_bit, 1);
    assert_eq!(after.insns - before.insns, 4);
    assert_eq!(after.insns_sixteen_bit - before.insns_sixteen_bit, 4);
    assert_eq!(after.side_exits, before.side_exits);
}

/// The negative control for the counters above: a 32-bit block must leave both `_sixteen_bit`
/// counters alone. Without this the split could be a plain copy of `jit_direct_entries` and every
/// assertion in the test above would still pass.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_thirty_two_bit_block_does_not_count_as_sixteen_bit() {
    let mut memory = vec![0u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + 6]
        .copy_from_slice(&[0x40, 0x41, 0x42, 0x8b, 0xc3, 0xf4]);
    let mut bus = sixteen_bit_bus(memory);
    // `fresh()` is the same real-mode CPU with CS.D forced to 1.
    let mut cpu = fresh();
    cpu.set_eip(ENTRY);
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000]);
    warm_sixteen_bit(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
    );

    let compilation = match jit::direct::compile(&mut cpu, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        _ => panic!("the 32-bit control must compile"),
    };
    let key = jit::direct::key_for(&cpu, ENTRY, true).unwrap();
    assert_eq!(key.mode_key & 1, 1);
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = cpu.jit_direct.install(&compilation).unwrap();
    let block = cpu.jit_direct.block(id).unwrap();

    cpu.registers.gpr = [0; 8];
    cpu.set_eip(ENTRY);
    let before = counts(&cpu);
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    let after = counts(&cpu);

    assert!(after.entries > before.entries);
    assert_eq!(after.entries_sixteen_bit, before.entries_sixteen_bit);
    assert_eq!(after.insns_sixteen_bit, before.insns_sixteen_bit);
}

/// The persona clause. A 16-bit segment is admitted on I586 ONLY.
///
/// Without it, `key_for` would accept a 16-bit entry on I486, the compile loop's Word persona
/// gate would then reject the FIRST slot, and the outcome would be a `StructuralReject` that
/// installs a rejected span and a physical-page watch for every hot 16-bit boundary in a 486
/// guest. That is a real cost for a yield of exactly zero, since every instruction in a 16-bit
/// segment is Word-sized and Word is 586-only.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_sixteen_bit_segment_is_admitted_on_i586_only() {
    let mut memory = vec![0u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + 4].copy_from_slice(&[0x40, 0x41, 0x42, 0xf4]);

    // `key_for` reads `decode_cache.line_phys_start(lin, d)`, so an unwarmed line refuses for a
    // reason that has nothing to do with the persona. Warming is what makes this test about what
    // it claims to be about.
    let mut bus = sixteen_bit_bus(memory.clone());
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    warm_sixteen_bit(&mut cpu, &mut bus, &[ENTRY]);
    assert_eq!(cpu.persona(), CpuPersona::I586);
    assert!(
        jit::direct::key_for(&cpu, ENTRY, false).is_some(),
        "I586 must admit a 16-bit code segment"
    );

    // Re-warm AFTER the mode change, and reload the segments it clears. `set_mode` calls
    // `invalidate_code_caches`, so without this the decode line is gone, `key_for` refuses on
    // `line_phys_start` returning None, and the assertion below passes on a build with no persona
    // clause at all. That is the very mutation this test exists to catch, and the first version
    // of it did not.
    cpu.set_mode(GswMode::Gsw486);
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
    ] {
        cpu.load_segment_real(segment, 0);
    }
    cpu.set_eip(ENTRY);
    warm_sixteen_bit(&mut cpu, &mut bus, &[ENTRY]);
    assert_eq!(cpu.persona(), CpuPersona::I486);
    // EXPLICIT: the default admits since the 486 measurement, so the refusing arm has to be asked
    // for. Left implicit, this assertion would test the default rather than the mechanism.
    cpu.set_word_operands_at_486(false);
    assert!(
        jit::direct::key_for(&cpu, ENTRY, false).is_none(),
        "I486 must refuse a 16-bit code segment outright, not compile and then reject the slot"
    );

    // The LIFTED arm, which is the whole reason the refusal above is now a policy rather than a
    // constant. `key_for_phys`'s persona clause and the compile walk's Word refusal are one
    // predicate, so flipping it here must move this key from None to Some without touching
    // anything else. Without this the lifted path would ship with no coverage at all.
    cpu.set_word_operands_at_486(true);
    assert!(
        jit::direct::key_for(&cpu, ENTRY, false).is_some(),
        "with Word operands admitted at I486, a 16-bit segment must key like it does at I586"
    );
    cpu.set_word_operands_at_486(false);
    assert!(
        jit::direct::key_for(&cpu, ENTRY, false).is_none(),
        "and the flag must be the only thing that moved it"
    );

    // The flag SURVIVES a clone, and this is not bookkeeping. `JitState::clone` deliberately drops
    // the barrier census, so copying that shape for a COMPILE POLICY would make a clone silently
    // compile differently from its origin. `CpuGsw::clone` is what the lockstep
    // interpreter-versus-native comparisons build their second role from, so a dropped flag there
    // compares a lifted CPU against an unlifted one and reports the disagreement as agreement.
    // Both twins are re-warmed for the same reason the mode change above is: a clone does not
    // carry the decode line, so an un-warmed twin refuses on `line_phys_start` and the assertion
    // would pass against a build that dropped the flag entirely. The pair is what isolates it.
    for (label, admitted, expect_key) in [("dropped", false, false), ("carried", true, true)] {
        cpu.set_word_operands_at_486(admitted);
        let mut twin = cpu.clone();
        warm_sixteen_bit(&mut twin, &mut bus, &[ENTRY]);
        assert_eq!(
            jit::direct::key_for(&twin, ENTRY, false).is_some(),
            expect_key,
            "{label}: a clone must inherit the Word-admission policy, not revert to the default"
        );
    }

    // The positive control: a 486 must still admit 32-bit code, so the refusal is proven to key
    // on `!d` and not to have swallowed the persona's ordinary population. A separate CPU,
    // because the decode line is keyed on `d` and this one is warmed at `d == true`.
    let mut wide_bus = sixteen_bit_bus(memory);
    let mut wide = fresh();
    wide.set_mode(GswMode::Gsw486);
    let mut cs = wide.registers.cs();
    cs.default_size_32 = true;
    wide.registers.set_segment(SegmentIndex::Cs, cs);
    wide.set_eip(ENTRY);
    warm_sixteen_bit(&mut wide, &mut wide_bus, &[ENTRY]);
    assert_eq!(wide.persona(), CpuPersona::I486);
    assert!(
        jit::direct::key_for(&wide, ENTRY, true).is_some(),
        "I486 must still admit 32-bit code"
    );
}

// ---------------------------------------------------------------------------
// 2. The #635 debt: the block-level address wrap, routed to each consumer.
// ---------------------------------------------------------------------------

/// A `[BP+disp]` operand whose sum crosses 0xFFFF.
///
/// EBP is 0x00AB_FFF0 and the displacement is 0x22, so the architectural effective address is
/// `(0xFFF0 + 0x22) & 0xFFFF == 0x12`. Without the 64K mask the emitted form computes
/// 0x00AC_0012.
///
/// **The high half of EBP is load-bearing, and the first version of this fixture got it wrong.**
/// With EBP = 0xFFFF_FFF0 the emitter's own 32-bit sum wraps to exactly 0x12, so the mask is a
/// no-op and a mutation deleting it SURVIVES. A high half that does not carry out of bit 32 is
/// what makes the mask observable at all, and it doubles as the proof that only the low 16 bits
/// of the base register are used. The LEA and x87 fixtures below carry the same value for the
/// same reason.
///
/// Three fixture properties are load-bearing and none is decoration:
///
/// - **0x12 is EVEN**, so the Word alignment guard does not side-exit and hide the result.
/// - **0x12 is 2 mod 4**, which is the only way a wrong READ WIDTH is observable: a dword read
///   at an address 2 mod 4 trips the guard, while at 0 mod 4 it would quietly read four bytes and
///   the narrowing destination write would hide it.
/// - **`side_exits` must not move.** A wrong address side-exits and the interpreter then produces
///   the RIGHT answer, so state equality alone passes on a broken emitter. This is the assertion
///   that actually catches the mutation.
///
/// The operand also pins the segment: `parse_16bit_address` selects SS for the BP forms, and a
/// wrong choice here is the wrong-memory-read class rather than lost bookkeeping.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_wrapping_bp_operand_reads_the_masked_address() {
    fn program() -> Vec<u8> {
        let mut memory = vec![0u8; 0x2000];
        memory[ENTRY as usize..ENTRY as usize + 7].copy_from_slice(&[
            0x40, // inc ax
            0x41, // inc cx
            0x8b, 0x46, 0x22, // mov ax,[bp+0x22]
            0x42, // inc dx
            0xf4, // hlt
        ]);
        memory[0x12..0x14].copy_from_slice(&0xbeefu16.to_le_bytes());
        // The value the masked address reaches. An unmasked former computes 0x00AC_0012,
        // which is neither mapped nor inside SS's 0xFFFF limit, so it takes a side exit and the
        // interpreter then produces the right answer: that is why `side_exits`, and not state
        // equality, is the assertion that catches this.
        memory
    }

    let mut interp = sixteen_bit_code_cpu(ENTRY);
    let mut native = sixteen_bit_code_cpu(ENTRY);
    let mut interp_bus = sixteen_bit_bus(program());
    let mut native_bus = sixteen_bit_bus(program());
    // Warm (decode) before mapping the fast map: warming sticky-marks page 0 watched, and its E1
    // sweep would clear `arm_native_sixteen_bit`'s fast-map entries for that same page (the
    // bp-relative operand lands on it too) if the mark ran after they were populated.
    warm_sixteen_bit(
        &mut native,
        &mut native_bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 5],
    );
    arm_native_sixteen_bit(&mut native, &mut native_bus, &[0x0000]);

    let block = install_sixteen_bit_block(&mut native, ENTRY, 4);

    let arm = |cpu: &mut CpuGsw| {
        cpu.registers.gpr = [0; 8];
        cpu.registers.set_ebp(0x00ab_fff0);
        cpu.registers.set_esp(0x0700);
        cpu.set_eip(ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    };
    arm(&mut interp);
    arm(&mut native);
    // The interpreter runs the same four instructions and then halts on the 0xF4.
    drive(&mut interp, &mut interp_bus);

    let before = counts(&native);
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    let after = counts(&native);

    assert_eq!(native.registers.eax(), 0xbeef, "read the masked address");
    assert_eq!(interp.registers.eax(), 0xbeef);
    assert_eq!(native.registers.eip, ENTRY + 6);
    assert_eq!(after.insns - before.insns, 4);
    assert_eq!(
        after.side_exits, before.side_exits,
        "a wrong effective address side-exits and lets the interpreter give the right answer"
    );
    assert_eq!(
        after.alignment_exits, before.alignment_exits,
        "specifically not the alignment exit, which is what a wrong width or an odd address takes"
    );
    // BP itself is not an address register the emitter may narrow: only the effective address
    // wraps, and the high half of EBP must survive untouched on both sides.
    assert_eq!(native.registers.ebp(), 0x00ab_fff0);
    assert_eq!(interp.registers.ebp(), 0x00ab_fff0);
}

/// LEA at a 16-bit address size, which reaches the address former WITHOUT a segment.
///
/// This is the survivor that changed #635's design: `DirectKind::Lea` calls
/// `emit_effective_address` directly and never reaches `emit_segmented_linear_address`, so it had
/// no wrap parameter to receive until the mask moved into the address former itself. The
/// interpreter writes an already-narrowed offset while an unmasked path would add the whole
/// 32-bit base register, so the divergence is arbitrary rather than merely high.
///
/// The explicit 0x66 is what keeps this fixture about the DWORD operand form. It used to be what
/// made the fixture possible at all -- `0x8d` was off the Word allowlist, so an unprefixed
/// `8D 46 22` in a 16-bit segment was refused before the emitter was reached, and the corollary
/// recorded here was that nothing on any corpus would ever reach this path. The S1 width lift
/// admitted the row, so the unprefixed form is now ordinary loader traffic and is covered by
/// `cpu_jit_width_lift_test.rs`. What this fixture still owns is the cell that one does not: a
/// Dword destination write over a 16-bit address.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_word_addressed_lea_writes_the_masked_offset() {
    let mut memory = vec![0u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + 8].copy_from_slice(&[
        0x40, // inc ax
        0x41, // inc cx
        0x66, 0x8d, 0x46, 0x22, // lea eax,[bp+0x22] at Dword operand, Word address
        0x42, // inc dx
        0xf4, // hlt
    ]);
    let mut bus = sixteen_bit_bus(memory);
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000]);
    warm_sixteen_bit(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 6],
    );

    let block = install_sixteen_bit_block(&mut cpu, ENTRY, 4);

    cpu.registers.gpr = [0; 8];
    cpu.registers.set_ebp(0x00ab_fff0);
    cpu.registers.set_esp(0x0700);
    cpu.set_eip(ENTRY);
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());

    // `(0xFFF0 + 0x22) & 0xFFFF`. An unmasked former adds the whole 32-bit base and gives
    // 0x00AC_0012. LEA writes the offset to a register rather than touching memory, so this is
    // the one wrap consumer of the four whose failure is a wrong VALUE and not a side exit.
    assert_eq!(cpu.registers.eax(), 0x12);
    assert_eq!(cpu.registers.eip, ENTRY + 7);
}

/// An x87 memory form at a 16-bit address size.
///
/// `emit_x87_memory_pointer` guards on OPERAND size, not address size, so a 66-prefixed x87
/// instruction in a 16-bit segment reaches it with a 16-bit address. It hard-coded the wrap until
/// #635, and that fix is the third of the five survivors.
///
/// Like the LEA it needs the explicit 0x66: `classify`'s FPU arm returns `None` unless the
/// operand size is Dword, and in a 16-bit segment an unprefixed x87 decodes as Word. Same
/// corollary: unreachable on unprefixed 16-bit code even after S4.
///
/// Both operands are 4-ALIGNED, which is not cosmetic: an x87 m32 access is a DWORD access and
/// the emitted wide-access guard side-exits an unaligned one. At an address 2 mod 4 the block
/// exits at the FLD, the two leading register slots retire, and every assertion below passes for
/// the wrong reason. That is how the first version of this fixture failed.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_word_addressed_x87_load_uses_the_masked_address() {
    fn program() -> Vec<u8> {
        let mut memory = vec![0u8; 0x2000];
        memory[ENTRY as usize..ENTRY as usize + 11].copy_from_slice(&[
            0x40, // inc ax
            0x41, // inc cx
            0x66, 0xd9, 0x46, 0x24, // fld dword [bp+0x24]
            0x66, 0xd9, 0x5e, 0x28, // fstp dword [bp+0x28]
            0xf4, // hlt
        ]);
        memory[0x14..0x18].copy_from_slice(&1.5f32.to_le_bytes());
        // A wrong (unmasked) pointer is 0x00AC_0014, which is unmapped, so the slot side-exits.
        memory
    }

    let mut bus = sixteen_bit_bus(program());
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    // `warm_sixteen_bit` decodes code on page 0, which sticky-marks that page watched; its E1
    // sweep would invalidate `arm_native_sixteen_bit`'s fast-map entries for the SAME page
    // (the fld/fstp targets sit on it too) if it ran after them. Warm first so the mark is in
    // place before the fast map is populated, matching production ordering.
    warm_sixteen_bit(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 6],
    );
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000]);

    let block = install_sixteen_bit_block(&mut cpu, ENTRY, 4);

    cpu.registers.gpr = [0; 8];
    cpu.registers.set_ebp(0x00ab_fff0);
    cpu.registers.set_esp(0x0700);
    cpu.set_eip(ENTRY);
    let before = counts(&cpu);
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    let after = counts(&cpu);

    // Loaded from the masked address and stored to the masked address.
    let stored = f32::from_le_bytes(bus.memory[0x18..0x1c].try_into().unwrap());
    assert_eq!(stored, 1.5f32);
    assert_eq!(cpu.registers.eip, ENTRY + 10);
    assert_eq!(after.insns - before.insns, 4);
    assert_eq!(
        after.side_exits, before.side_exits,
        "an unmasked x87 pointer side-exits rather than reading wrong, so state alone passes"
    );
}

/// A 32-bit stack INSIDE a 16-bit code segment, which is the asymmetry #635 stated as a rule and
/// got the count wrong for.
///
/// SS.B and CS.D are independent. `address_wrap` is the ADDRESS-SIZE property and governs
/// ModRM-derived addresses only; every `stack_addr` site follows SS.B and stays a literal. Wiring
/// the stack arms to the block property would mask this push's address to 16 bits and store four
/// bytes at 0x07FC instead of 0x1_07FC.
///
/// SS.B = 1 requires protected mode architecturally; this fixture builds it by hand, exactly as
/// `flat_code_sixteen_bit_stack_fixture` does for the mirror case. **`ss.limit` is widened on
/// purpose:** left at real mode's 0xFFFF, every push here would side-exit on the segment-limit
/// compare and the whole test would pass interpreted.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_thirty_two_bit_stack_in_a_sixteen_bit_segment_keeps_its_full_pointer() {
    const ESP: u32 = 0x0001_0800;

    fn program() -> Vec<u8> {
        let mut memory = vec![0u8; 0x1_2000];
        memory[ENTRY as usize..ENTRY as usize + 6].copy_from_slice(&[
            0x40, // inc ax
            0x41, // inc cx
            0x66, 0x50, // push eax at Dword operand size on a 32-bit stack
            0x42, // inc dx
            0xf4, // hlt
        ]);
        memory
    }

    let mut bus = sixteen_bit_bus(program());
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.default_size_32 = true;
    ss.limit = u32::MAX;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    cpu.registers.set_esp(ESP);
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000, 0x1_0000]);
    warm_sixteen_bit(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 4],
    );

    let block = install_sixteen_bit_block(&mut cpu, ENTRY, 4);

    cpu.registers.gpr = [0; 8];
    cpu.registers.set_eax(0xcafe_f00d);
    cpu.registers.set_esp(ESP);
    cpu.set_eip(ENTRY);
    // Both candidate slots pre-seeded so a wrong one is a wrong VALUE, not an absence.
    bus.memory[0x07fc..0x0800].copy_from_slice(&0u32.to_le_bytes());
    bus.memory[ESP as usize - 4..ESP as usize].copy_from_slice(&0u32.to_le_bytes());
    let before = counts(&cpu);
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    let after = counts(&cpu);

    assert_eq!(cpu.registers.esp(), ESP - 4);
    assert_eq!(
        u32::from_le_bytes(
            bus.memory[ESP as usize - 4..ESP as usize]
                .try_into()
                .unwrap()
        ),
        0xcafe_f00e,
        "the push must land at the full 32-bit stack address"
    );
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x07fc..0x0800].try_into().unwrap()),
        0,
        "nothing may be written at the 16-bit-masked address"
    );
    assert_eq!(after.insns - before.insns, 4);
    assert_eq!(after.side_exits, before.side_exits);
}

// ---------------------------------------------------------------------------
// 3. The fall-through at the segment top.
// ---------------------------------------------------------------------------

/// A 16-bit block whose last instruction ends exactly at EIP 0xFFFF.
///
/// The campaign carried an owed item saying the fall-through "must wrap at 0xFFFF" at CS.D = 0.
/// It does not, in this emulator: `decode.rs`'s sequential advance is an unconditional
/// `wrapping_add` with no operand-size mask and no CS.D branch, and the cold path's per-byte
/// advances are unmasked too. Both sides therefore leave EIP at 0x1_0000 and both then take the
/// same #GP on the next fetch, which this fixture drives all the way through: the vector lands at
/// 0:0 where a HLT is planted, so both CPUs halt in the same place.
///
/// What actually protects the slice is not that agreement but that the fall-through LinkTarget
/// names `cs.base + 0x1_0000`, outside the segment, and that cell can never bind: no block with
/// `entry_eip > cs.limit` can compile, since its first slot fails the per-slot fetch-limit check,
/// and `try_link_inner` requires `link_compatible`, which demands an identical CS. The successor
/// assertion below is what pins that.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_sixteen_bit_block_ending_at_the_segment_top_wraps_identically() {
    const TOP_ENTRY: u32 = 0xfffc;

    fn program() -> Vec<u8> {
        let mut memory = vec![0u8; 0x1_0000];
        // Four one-byte register ops occupying 0xFFFC through 0xFFFF exactly.
        memory[0xfffc..0x1_0000].copy_from_slice(&[0x40, 0x41, 0x42, 0x43]);
        // The 16-bit run-off wraps IP to 0 (stage-1 E7: real IP arithmetic is
        // mod 65536; the old build #GP'd here instead). A HLT at 0000:0000
        // stops both CPUs at the wrap landing.
        memory[0] = 0xf4;
        memory
    }

    let mut interp = sixteen_bit_code_cpu(TOP_ENTRY);
    let mut native = sixteen_bit_code_cpu(TOP_ENTRY);
    let mut interp_bus = sixteen_bit_bus(program());
    let mut native_bus = sixteen_bit_bus(program());
    arm_native_sixteen_bit(&mut native, &mut native_bus, &[0x0000]);
    warm_sixteen_bit(
        &mut native,
        &mut native_bus,
        &[TOP_ENTRY, TOP_ENTRY + 1, TOP_ENTRY + 2, TOP_ENTRY + 3],
    );

    let compilation = match jit::direct::compile(&mut native, TOP_ENTRY, false) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        _ => panic!("a block ending exactly at the segment top must compile"),
    };
    assert_eq!(compilation.span.instructions, 4);
    // The fall-through successor is the unmasked next address, which is outside the segment.
    let fallthrough = compilation.successors[0].expect("a non-terminal block has a fall-through");
    assert_eq!(fallthrough.linear, 0x1_0000);

    let key = jit::direct::key_for(&native, TOP_ENTRY, false).unwrap();
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = native.jit_direct.install(&compilation).unwrap();
    let block = native.jit_direct.block(id).unwrap();

    let arm = |cpu: &mut CpuGsw| {
        cpu.registers.gpr = [0; 8];
        cpu.registers.set_esp(0x0700);
        cpu.set_eip(TOP_ENTRY);
        cpu.halted = false;
    };
    arm(&mut native);
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    assert_eq!(
        native.registers.eip, 0,
        "the native exit seam wraps the run-off to IP 0, exactly as the          interpreter's retire seam does"
    );

    // Now drive both from the same start and compare the whole architectural
    // result: both wrap to 0000:0000 and halt there.
    arm(&mut interp);
    arm(&mut native);
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    assert_eq!(
        native.registers, interp.registers,
        "a block ending at the segment top must fault identically on both paths"
    );
    assert_eq!(native.halted, interp.halted);
}

/// THE ANTI-VACUITY GATE FOR THE TIER 0 ALLOWLIST, and the only fixture that exercises it in a
/// real 16-bit code segment.
///
/// On `main` the continuation early-out still refuses `!d`, so no 16-bit block can be compiled in
/// production and the pinned corpus reaches the Tier 0 opcodes only through a 0x66 prefix in
/// 32-bit code. Byte identity there is an inertness claim, not evidence. This is the test that
/// says the mechanism works.
///
/// Every slot is a Tier 0 opcode admitted by that slice, in a segment where each decodes at
/// `OperandSize::Word` because the size follows CS.D rather than the opcode. Without the
/// allowlist edit slot 1 fails to classify, the block holds one non-terminal slot, the three-slot
/// floor returns `Retry`, and `install_sixteen_bit_block` panics. It fails loudly if the
/// mechanism is absent.
///
/// The block ends on a terminal (`0xeb` JMP), so the three-slot floor does not apply and the
/// count is exact at 7.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn tier_zero_byte_forms_and_near_jmp_form_a_sixteen_bit_block() {
    let mut memory = vec![0u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + 15].copy_from_slice(&[
        0x40, // inc ax
        0x3c, 0x05, // cmp al,5
        0x84, 0xc0, // test al,al
        0x88, 0xc4, // mov ah,al
        0x8a, 0xd8, // mov bl,al
        0x80, 0xc1, 0x03, // add cl,3
        0xeb, 0x02, // jmp +2, terminal
        0x90, // landing pad inside the mapped page
    ]);
    let mut bus = sixteen_bit_bus(memory);
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000]);
    warm_sixteen_bit(
        &mut cpu,
        &mut bus,
        &[
            ENTRY,
            ENTRY + 1,
            ENTRY + 3,
            ENTRY + 5,
            ENTRY + 7,
            ENTRY + 9,
            ENTRY + 12,
        ],
    );

    let block = install_sixteen_bit_block(&mut cpu, ENTRY, 7);

    cpu.registers.gpr = [0; 8];
    cpu.registers.set_eax(0xdead_0004);
    cpu.registers.set_ecx(0xbeef_0010);
    cpu.set_eip(ENTRY);
    let before = counts(&cpu);
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block).unwrap(),
        "the Tier 0 block must run"
    );
    let after = counts(&cpu);

    // Byte ops narrow: every one of these writes 8 bits and preserves the rest of the register.
    // A width leak would clobber the high halves seeded above, which is why they are non-zero.
    assert_eq!(
        cpu.registers.eax(),
        0xdead_0505,
        "al and ah, high half kept"
    );
    assert_eq!(cpu.registers.ebx(), 0x0000_0005, "bl from mov bl,al");
    assert_eq!(cpu.registers.ecx(), 0xbeef_0013, "cl += 3, high half kept");
    // The JMP is the terminal, so EIP is its TARGET rather than the fall-through. The target is
    // end-of-instruction plus displacement: the jmp occupies 0x10c..0x10e and displaces by 2.
    assert_eq!(cpu.registers.eip, ENTRY + 16);

    assert_eq!(
        after.insns - before.insns,
        7,
        "exact native instruction count"
    );
    assert_eq!(
        after.insns_sixteen_bit - before.insns_sixteen_bit,
        7,
        "and all seven are attributed to the 16-bit population"
    );
    assert_eq!(
        after.side_exits, before.side_exits,
        "a side exit would let the interpreter produce the right answer anyway"
    );
}

// ---------------------------------------------------------------------------
// 9. The count lane (L2 arm 2) must not reach a 16-bit code segment.
// ---------------------------------------------------------------------------

/// THE WORD GROUP-2 SHIFT IN A 16-BIT CODE SEGMENT, and it is a REGRESSION FIXTURE for a crash.
///
/// `count_lane_for`'s first form barred the Word shifts with the argument "a Word `0xC1` needs a
/// `0x66`, so the prefix and length bars already refuse it". **That argument is false in a 16-bit
/// code segment**, where the operand size follows CS.D rather than a prefix: an unprefixed
/// `c1 e0 03` decodes as `shl ax, 3` at `OperandSize::Word` with default prefixes, `disp_len 0`,
/// `imm_len 1` and `len 3` — every bar satisfied. `0xC1` is on classify's Word allowlist, so
/// `classify` produced `Shift { width: Word }`, the lane attached, and `emit_shift_lane` reached
/// its `unreachable!` and PANICKED THE COMPILER. The fix bars the kind's own width; this fixture
/// is what keeps it barred.
///
/// The assertion is threefold and each third would have been enough to catch the crash: the
/// compile does not panic, it takes NO lane, and the emitted block still computes the Word shift
/// the interpreter computes. The last one is what says the fix is a lane refusal rather than a
/// lowering refusal — the Word form must keep compiling with a baked count, exactly as it did
/// before this slice existed.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn a_word_group_two_shift_in_a_sixteen_bit_segment_takes_no_count_lane() {
    jit::direct::set_count_lanes_for_test(Some(true));
    let mut memory = vec![0u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + 8].copy_from_slice(&[
        0x40, // inc ax
        0xc1, 0xe0, 0x03, // shl ax, 3   -- Word by CS.D, no prefix, three bytes
        0xc1, 0xe8, 0x01, // shr ax, 1   -- the count-1 shape, likewise Word
        0xf4, // hlt
    ]);
    let mut bus = sixteen_bit_bus(memory.clone());
    let mut cpu = sixteen_bit_code_cpu(ENTRY);
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000]);
    warm_sixteen_bit(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 4]);

    // The compile itself is the crash site. `install_sixteen_bit_block` panics on a reject, so
    // reaching past it says the walk completed.
    let compilation = match jit::direct::compile(&mut cpu, ENTRY, false) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("the Word group-2 block must still compile: structural reject")
        }
        jit::direct::CompileOutcome::Retry(_) => {
            panic!("the Word group-2 block must still compile: retry")
        }
    };
    assert_eq!(
        compilation.count_lane_count(),
        0,
        "a Word group-2 shift must take no count lane; its emitter has no CL-form Word lane"
    );
    assert_eq!(compilation.imm_lane_count(), 0);

    let block = install_sixteen_bit_block(&mut cpu, ENTRY, 3);
    cpu.registers.gpr = [0; 8];
    cpu.registers.set_eax(0xdead_1234);
    cpu.set_eip(ENTRY);
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block).unwrap(),
        "the baked-count Word block must still run natively"
    );

    // The oracle: the same bytes interpreted, with no block at all.
    let mut interpreter_bus = sixteen_bit_bus(memory);
    let mut interpreter = sixteen_bit_code_cpu(ENTRY);
    interpreter.registers.gpr = [0; 8];
    interpreter.registers.set_eax(0xdead_1234);
    interpreter.set_eip(ENTRY);
    for _ in 0..3 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }
    assert_eq!(
        cpu.registers, interpreter.registers,
        "the baked Word lowering must still match the interpreter"
    );
    assert_eq!(cpu.eflags(), interpreter.eflags());
    jit::direct::set_count_lanes_for_test(None);
}
