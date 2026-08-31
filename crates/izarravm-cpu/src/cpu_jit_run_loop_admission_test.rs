// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The run loop must let the Direct dispatcher SEE a block entry whose first instruction is
//! non-continuable.
//!
//! `run_straight_line` used to break on the `!screen.continuable` screen before it ever called
//! `dispatch_continuation`, and the `first` arm of the same loop interprets without consulting the
//! JIT at all. So a far transfer that is a block ENTRY was never keyed, never became `Seen`, never
//! reached a compile walk, and every static exit into it classified `absent` for the life of the
//! process. On the `15-move-hole-puzzle` corpus recipe that was 186.7 M of 186.7 M absent exits
//! over twelve addresses, ten `CALL FAR` and two `RETF`.
//!
//! The fixture is the two-address shape read out of that guest: a `Jcc`-terminated block whose
//! NOT-TAKEN fallthrough is a bare `RETF`. `RETF` is chosen because it already lowers
//! (`DirectKind::RetFar16` on the shipped `V86` arm), so the address can travel the whole way from
//! `absent` to a compiled block a predecessor binds to, which is what the fix is for. An opcode
//! with no `classify` arm would stop at `rejected` and could not show the second half.
//!
//! Everything here drives `run_straight_line` end to end rather than
//! `try_direct_continuation_for_test`, because the seam under test IS the break site and a direct
//! call to the dispatcher walks around it.

use super::*;

/// Where a run starts. One `NOP`, so the block entry below is reached as a CONTINUATION rather
/// than as the run's `first` interpreted cycle.
const RUN_ENTRY: u32 = 0x100;
/// The predecessor block: `OR AL,AL` then `JNZ`. `Jcc` is a terminal, so the walk installs these
/// two slots and stops, and its fallthrough static successor is the `RETF`.
const PRED: u32 = 0x101;
/// The address the whole fixture is about: the not-taken fallthrough, and always a
/// non-continuable entry.
const FALLTHROUGH: u32 = 0x105;

/// A bare `RETF`: non-continuable AND lowerable, so the break-site probe can carry it all the way
/// to a compiled block.
const RETF: [u8; 1] = [0xcb];
/// `CALL FAR 0020:0400`: non-continuable, and now (L8) LOWERABLE via `DirectKind::CallFar16`.
/// Before L8, `0x9a` had no `classify` arm anywhere in `jit/direct.rs`, so a walk starting here
/// stopped before its first instruction; see
/// `a_non_continuable_entry_the_walk_can_now_carry_is_admitted_and_compiled` below for the current
/// behaviour.
const CALL_FAR: [u8; 5] = [0x9a, 0x00, 0x04, 0x20, 0x00];
/// `JMP FAR 0020:0400`: non-continuable, and (unlike `0x9a`/`0xcb` above) has NO `classify` arm
/// anywhere in `jit/direct.rs` and is absent from `non_continuable_walk_candidate`'s `matches!`
/// set -- confirmed by grep, not assumed. This is the negative control the walk-admission gate
/// needs: an opcode the walk genuinely cannot carry, so a break-site probe at this address must
/// stay `rejected`/`absent` rather than being wrongly admitted. `0x9a` served this role before L8
/// gave it a `classify` arm (see `a_non_continuable_entry_the_walk_can_now_carry_is_admitted_and_compiled`'s
/// doc comment above); `0xea` is its replacement, chosen because JMP FAR ptr16:16 native lowering
/// has never landed in this codebase.
const JMP_FAR: [u8; 5] = [0xea, 0x00, 0x04, 0x20, 0x00];
/// `IMUL BX, [0x0600], 0x0010`, the word form. Non-continuable for the interpreter, IN
/// `non_continuable_walk_candidate`, and admitted by `jit_admits_non_continuable`, so a compile
/// walk carries it -- but it is NOT in `non_continuable_break_probe_candidate`, so the run loop
/// must not probe it at a break site. It also does not transfer control, which is why the fixture
/// below drives to the self-loop rather than to `TARGET_LINEAR`.
const IMUL_WORD: [u8; 6] = [0x69, 0x1e, 0x00, 0x06, 0x10, 0x00];

/// Where the far return goes: selector 0x0020 (base 0x200) at offset 0x0400, linear 0x600.
const TARGET_SELECTOR: u16 = 0x0020;
const TARGET_OFFSET: u16 = 0x0400;
const TARGET_LINEAR: u32 = 0x0600;

/// SS is base 0 with SP here, clear of the code and of the target.
const STACK_SP: u32 = 0x0700;
/// The same stack pointer with a POISONED high half: SS.B is 0, so bits 31..16 of ESP are
/// architecturally untouched by the far return's pops.
const STACK_ESP: u32 = 0xdead_0000 | STACK_SP;

const MEMORY_LEN: usize = 0x1_0000;

/// How many times the fixture walks the predecessor. Six is the floor the mechanism needs -- one
/// visit to seed `Seen` at the predecessor, one to compile it, one for its first static-unbound
/// exit, one for the break-site probe to reach `BlockProbe::Compile` at the `RETF`, and one for
/// the bound link to be taken -- and the extra visits make the "later visits stop growing the
/// absent class" assertion non-vacuous.
const VISITS: usize = 10;

fn program(fallthrough: &[u8]) -> Vec<u8> {
    let mut memory = vec![0u8; MEMORY_LEN];
    memory[RUN_ENTRY as usize] = 0x90; // nop
    let taken = FALLTHROUGH as usize + fallthrough.len();
    memory[PRED as usize..PRED as usize + 4].copy_from_slice(&[
        0x08,
        0xc0, // or al,al
        0x75,
        fallthrough.len() as u8, // jnz over the fallthrough
    ]);
    memory[FALLTHROUGH as usize..FALLTHROUGH as usize + fallthrough.len()]
        .copy_from_slice(fallthrough);
    // Self-loops, not `HLT`: this fixture is V86 at CPL 3, where `HLT` is a #GP and the run
    // would never reach the far transfer at all. Neither self-loop is ever RETIRED -- the driver
    // stops the moment EIP arrives.
    memory[taken..taken + 2].copy_from_slice(&[0xeb, 0xfe]);
    memory[TARGET_LINEAR as usize..TARGET_LINEAR as usize + 2].copy_from_slice(&[0xeb, 0xfe]);
    // The far return frame, exactly as a `push cs; push ip` pair leaves it: offset then selector.
    // Ignored by the `CALL FAR` arm, which carries its target in the instruction.
    let sp = STACK_SP as usize;
    memory[sp..sp + 2].copy_from_slice(&TARGET_OFFSET.to_le_bytes());
    memory[sp + 2..sp + 4].copy_from_slice(&TARGET_SELECTOR.to_le_bytes());
    memory
}

/// V86, which is the mode the corpus guest runs in once TOKAEMM is resident and the mode the
/// shipped `RetfArm::V86` admits.
fn v86_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Es,
        SegmentIndex::Ss,
    ] {
        cpu.load_segment_real(segment, 0);
    }
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x202 | FLAG_VM | (3 << 12);
    cpu.cpl = 3;
    assert!(cpu.is_v86_mode(), "the fixture must actually be in V86");
    cpu
}

/// The machine every arm starts from. `jit` decides whether the Direct backend admits at all, so
/// the same helper builds the native arm and the interpreted control.
fn staged(jit: bool) -> (CpuGsw, TestBus) {
    staged_with(jit, &RETF)
}

fn staged_with(jit: bool, fallthrough: &[u8]) -> (CpuGsw, TestBus) {
    // The arm is stated, in both directions, rather than read off the ambient
    // IZARRAVM_DIRECT_RETF_V86: the suite runs on both legs and a fixture that leaned on the
    // default would be testing the refusal on one of them.
    jit::direct::set_direct_retf_v86_for_test(Some(jit::direct::RetfArm::V86));
    let mut cpu = v86_cpu();
    cpu.set_jit_auto_admit(jit);
    // One ask crosses the heat gate, so the visit count below measures the probe sequence and not
    // the warm-up.
    cpu.jit_direct.set_admission_heat_for_test(1);
    // Hard-false outside tests, ON by default inside them. Both blocks here are shorter than
    // `MIN_STANDALONE_INSTRUCTIONS`, so leaving it on would refuse every native entry.
    cpu.jit_direct.set_defer_short_for_test(false);
    cpu.jit_direct.enable_barrier_census_for_test();
    let mut bus = TestBus::with_memory(program(fallthrough));
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    arm(&mut cpu);
    (cpu, bus)
}

/// Seed one visit: back at the `NOP`, AL clear so the `Jcc` falls through, and the far return
/// frame back under SP.
fn arm(cpu: &mut CpuGsw) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.gpr.fill(0);
    cpu.registers.set_esp(STACK_ESP);
    cpu.set_eip(RUN_ENTRY);
}

/// Walk the fixture once, from the `NOP` to the far transfer's landing site.
fn visit(cpu: &mut CpuGsw, bus: &mut TestBus) {
    visit_until(cpu, bus, TARGET_LINEAR);
}

/// The same walk with the stopping point named. A fallthrough that does not transfer control (the
/// `IMUL` arm) settles on the fixture's own self-loop instead of on the far target.
fn visit_until(cpu: &mut CpuGsw, bus: &mut TestBus, stop_at: u32) {
    arm(cpu);
    for _ in 0..32 {
        if cpu.linear_eip() == stop_at {
            return;
        }
        cpu.run_straight_line(bus, u64::MAX).unwrap();
    }
    panic!(
        "the guest never reached {stop_at:#x}, stuck at {:#x}",
        cpu.linear_eip()
    );
}

fn class(cpu: &CpuGsw, label: &str) -> u64 {
    cpu.direct_barrier_census_snapshot()
        .expect("the census is enabled for this fixture")
        .unbound_targets
        .iter()
        .find(|(name, _)| *name == label)
        .unwrap_or_else(|| panic!("missing unbound-target class {label}"))
        .1
}

/// The whole defect, in one fixture: the `RETF` entry acquires a state and the predecessor's
/// static link binds to it.
///
/// Without the break-site admission call every static exit at `RETF_AT` classifies `absent`
/// forever, no `RETF` block is ever compiled, and `jit_direct_linked_transfers` never moves. That
/// is the red state.
#[test]
fn a_non_continuable_block_entry_is_admitted_at_the_run_loop_break() {
    let (mut cpu, mut bus) = staged(true);
    for _ in 0..VISITS {
        visit(&mut cpu, &mut bus);
    }

    assert!(
        cpu.jit_direct.len() >= 2,
        "the predecessor AND the RETF entry must both compile, got {} blocks",
        cpu.jit_direct.len()
    );
    assert!(
        class(&cpu, "seen") + class(&cpu, "compiled") > 0,
        "the RETF entry must leave the absent class: {:?}",
        cpu.direct_barrier_census_snapshot()
            .unwrap()
            .unbound_targets
    );
    assert!(
        cpu.perf_counters().jit_direct_linked_transfers > 0,
        "the predecessor's fallthrough cell must bind to the RETF block and be taken natively"
    );

    // The absent class must STOP growing once the link is bound. A run that still ends at the
    // RETF every visit would keep charging it.
    let absent_before = class(&cpu, "absent");
    let breaks_before = cpu.perf_counters().brk_cont_not_continuable;
    let linked_before = cpu.perf_counters().jit_direct_linked_transfers;
    for _ in 0..VISITS {
        visit(&mut cpu, &mut bus);
    }
    assert_eq!(
        class(&cpu, "absent"),
        absent_before,
        "the RETF entry is compiled, so no later exit may classify absent"
    );
    assert!(
        cpu.perf_counters().jit_direct_linked_transfers > linked_before,
        "the bound link must be taken on every later visit"
    );
    assert!(
        cpu.perf_counters().brk_cont_not_continuable - breaks_before < (VISITS as u64) * 3,
        "the run must stop breaking at the RETF once it runs inside native code"
    );
}

/// The guest-visible outcome is identical with and without the fix's admission. This is the
/// assertion that catches a batch-structure regression: the run may END somewhere else, but the
/// architectural state after the far return may not differ.
#[test]
fn break_site_admission_does_not_change_the_guest_visible_outcome() {
    let (mut native, mut native_bus) = staged(true);
    let (mut interp, mut interp_bus) = staged(false);
    for _ in 0..VISITS {
        visit(&mut native, &mut native_bus);
        visit(&mut interp, &mut interp_bus);
    }

    assert!(
        native.perf_counters().jit_direct_linked_transfers > 0,
        "the native arm must actually have linked, or this differential compares two interpreters"
    );
    assert_eq!(
        interp.perf_counters().jit_direct_linked_transfers,
        0,
        "the control arm must admit nothing"
    );
    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interp),
        "registers, segment registers and EIP must match the interpreter exactly"
    );
    assert_eq!(
        native.eflags(),
        interp.eflags(),
        "materialized EFLAGS must match: the far return's own flags are untouched, and the last
         flag writer before it is the OR"
    );
    assert_eq!(
        native.elapsed_clocks, interp.elapsed_clocks,
        "the same instructions retire either way, so the core-clock charge is the same"
    );
    assert_eq!(
        native.linear_eip(),
        interp.linear_eip(),
        "both arms must end on the same instruction boundary"
    );
    assert_eq!(
        native_bus.memory, interp_bus.memory,
        "guest RAM must be untouched by where the run ended"
    );
}

/// **UPDATED FOR L8.** This fixture used to be the CONTROL for the RETF fixture above: `0x9a` had
/// no `classify` arm anywhere in `jit/direct.rs`, so the break-site probe (admitted by
/// `walk_admits_non_continuable_entry`) would still walk into a structural reject, and the
/// original assertions here pinned that zero-gain outcome -- `jit_direct.len() == 1`, the class
/// staying `absent` rather than moving to `rejected`.
///
/// **L8 (`dev_docs/2026-08-31-corpus-lever-plan.md`) gives `0x9a` a `classify` arm** --
/// `DirectKind::CallFar16`, admitted by `call_far_admitted_here` and folded into
/// `walk_admits_non_continuable_entry`'s disjunction with NO edit to this predicate itself, which
/// is exactly the self-opening behaviour the OLD doc comment on this fixture predicted ("at which
/// point the same predicate admits it with no edit here"). The CALL FAR entry now compiles the
/// same way the RETF entry above does, so this fixture is rewritten to assert THAT instead of the
/// old refusal -- it is now the CALL FAR mirror of
/// `a_non_continuable_block_entry_is_admitted_at_the_run_loop_break`, not its control.
#[test]
fn a_non_continuable_entry_the_walk_can_now_carry_is_admitted_and_compiled() {
    let (mut cpu, mut bus) = staged_with(true, &CALL_FAR);
    for _ in 0..VISITS {
        visit(&mut cpu, &mut bus);
    }

    assert!(
        cpu.jit_direct.len() >= 2,
        "the predecessor AND the CALL FAR entry must both compile, got {} blocks",
        cpu.jit_direct.len()
    );
    assert!(
        class(&cpu, "seen") + class(&cpu, "compiled") > 0,
        "the CALL FAR entry must leave the absent class: {:?}",
        cpu.direct_barrier_census_snapshot()
            .unwrap()
            .unbound_targets
    );
    assert!(
        cpu.perf_counters().jit_direct_linked_transfers > 0,
        "the predecessor's fallthrough cell must bind to the CALL FAR block and be taken natively"
    );

    let absent_before = class(&cpu, "absent");
    let linked_before = cpu.perf_counters().jit_direct_linked_transfers;
    for _ in 0..VISITS {
        visit(&mut cpu, &mut bus);
    }
    assert_eq!(
        class(&cpu, "absent"),
        absent_before,
        "the CALL FAR entry is compiled, so no later exit may classify absent"
    );
    assert!(
        cpu.perf_counters().jit_direct_linked_transfers > linked_before,
        "the bound link must be taken on every later visit"
    );
}

/// The GATE'S NEGATIVE CONTROL, rebased onto `0xea` (`JMP FAR ptr16:16`).
///
/// `walk_admits_non_continuable_entry` (`non_continuable_walk_candidate(insn.opcode) && ...` for
/// each admitted family) must still say NO to an opcode that is genuinely inadmissible: one with
/// no `classify` arm and absent from `non_continuable_walk_candidate`'s set. Without this test the
/// gate mechanism has no proof it can ever refuse -- a future bug that made the predicate
/// always-true, or a future non-continuable opcode added to the walk-candidate set without ever
/// getting a `classify` arm, would go undetected by every other fixture in this file, because they
/// all exercise opcodes the gate is SUPPOSED to admit.
///
/// This is the same shape the CALL FAR fixture above used to be, before L8 gave `0x9a` a
/// `classify` arm and turned it into a positive control (see that test's doc comment). `0xea` is
/// its replacement: still a far transfer, still non-continuable, still with no native lowering
/// anywhere in this codebase.
#[test]
fn a_genuinely_inadmissible_non_continuable_entry_stays_absent() {
    let (mut cpu, mut bus) = staged_with(true, &JMP_FAR);
    for _ in 0..VISITS {
        visit(&mut cpu, &mut bus);
    }

    assert_eq!(
        cpu.jit_direct.len(),
        1,
        "only the predecessor may compile; the JMP FAR entry has no classify arm and must never \
         become a block, got {} blocks",
        cpu.jit_direct.len()
    );
    assert!(
        class(&cpu, "absent") > 0,
        "the JMP FAR entry must classify absent, not admitted into a compile attempt: {:?}",
        cpu.direct_barrier_census_snapshot()
            .unwrap()
            .unbound_targets
    );
    assert_eq!(
        class(&cpu, "rejected"),
        0,
        "an opcode the walk cannot carry at all must never reach a structural reject -- that \
         would mean the break-site probe admitted it into a compile attempt, which is exactly the \
         regression this control exists to catch: {:?}",
        cpu.direct_barrier_census_snapshot()
            .unwrap()
            .unbound_targets
    );
}

/// A TRANSIENT Dormant park at a non-continuable entry must still recover.
///
/// `BlockCache::probe` collapses `Dormant` and `Rejected` into one `BlockProbe::Rejected`, and
/// `Dormant` is the transient half: a heat demote or a clearable compile retry parks a key there
/// expecting a later probe to lift it back to `Seen`. Break-site probes are the ONLY probes a
/// non-continuable entry ever gets, so a break-site short circuit on that arm would strand such a
/// key parked for the life of the process -- the same never-revisited defect this whole file is
/// about, narrowed to the entries that lose a first-touch race.
///
/// `TranslationMismatch` is one of the two causes `RetryCause::clearable_by_retry` admits, and the
/// visit counter is pre-loaded to one short of the threshold so the FIRST break-site probe after
/// the park is the one that lifts. That keeps the fixture about the recovery reaching the break
/// site at all, not about how many eras the memo paces it over.
#[test]
fn a_transient_dormant_park_at_a_break_site_entry_still_recovers() {
    jit::direct::set_retry_lift_for_test(Some(true));
    let (mut cpu, mut bus) = staged(true);

    // One visit is what puts the RETF key in the cache at all: the break-site probe inserts
    // `Seen` on its first miss.
    visit(&mut cpu, &mut bus);
    let key = jit::direct::key_for(&cpu, FALLTHROUGH, false).expect("the RETF entry keys");
    cpu.jit_direct.direct.park_dormant_for_test(
        key,
        jit::direct::DormantReason::CompileRetry,
        Some(jit::direct::RetryCause::TranslationMismatch),
    );
    cpu.jit_direct
        .direct
        .set_dormant_visits_for_test(key, jit::direct::RETRY_LIFT_VISITS - 1);
    assert!(
        cpu.jit_direct.direct.is_dormant_for_test(key),
        "the fixture must actually park the RETF entry, or the recovery below proves nothing"
    );
    assert!(
        !cpu.jit_direct.direct.key_is_compiled_for_test(key),
        "the park must undo the first visit's admission"
    );

    for _ in 0..VISITS {
        visit(&mut cpu, &mut bus);
    }

    assert!(
        !cpu.jit_direct.direct.is_dormant_for_test(key),
        "the break-site probe must reach the dormant recovery and lift the key"
    );
    assert!(
        cpu.jit_direct.direct.key_is_compiled_for_test(key),
        "a lifted key must go on to compile through the same break-site probe"
    );
    assert!(
        cpu.direct_stall_snapshot().retry_lifts > 0,
        "the lift must be the retry arm, not an incidental cache wipe"
    );
    jit::direct::set_retry_lift_for_test(None);
}

/// THE POLICY GATE, and the reason it exists: an opcode a compile WALK can carry is not
/// automatically an opcode the run loop probes at a break site.
///
/// `walk_admits_non_continuable_entry` is a capability test and it says yes to word `IMUL`, word
/// `LES`/`LDS` and `OUT` -- all three have `classify` arms and all three are in
/// `jit_admits_non_continuable`. Reaching the break-site probe through that test alone priced
/// duke3d-586 at -5.7%: 2.79 M probes across those three families bought 92 blocks, and the block
/// keys and rejected watch spans they minted moved `jit_direct_blocks_installed` 200,711 ->
/// 235,235 and `smc_scan_keys` 657.5 M -> 669.2 M. `non_continuable_break_probe_candidate` is the
/// narrower door, and this fixture is what holds it shut.
///
/// RED WITHOUT THE GATE: the `IMUL` entry compiles a second block, its census class leaves
/// `absent`, and `jit_direct.len()` reads 2.
#[test]
fn a_walk_admissible_non_probe_opcode_is_never_probed_at_a_break_site() {
    let stop_at = FALLTHROUGH + IMUL_WORD.len() as u32;
    let (mut cpu, mut bus) = staged_with(true, &IMUL_WORD);
    for _ in 0..VISITS {
        visit_until(&mut cpu, &mut bus, stop_at);
    }

    assert_eq!(
        cpu.jit_direct.len(),
        1,
        "only the predecessor may compile; the IMUL entry is outside the break-probe set and must \
         never be keyed by the run loop, got {} blocks",
        cpu.jit_direct.len()
    );
    assert!(
        class(&cpu, "absent") > 0,
        "the IMUL entry must stay absent, which is what costs nothing: {:?}",
        cpu.direct_barrier_census_snapshot()
            .unwrap()
            .unbound_targets
    );
    assert_eq!(
        class(&cpu, "rejected") + class(&cpu, "seen") + class(&cpu, "compiled"),
        0,
        "a break-site probe at the IMUL would show up as one of these classes; none may move: \
         {:?}",
        cpu.direct_barrier_census_snapshot()
            .unwrap()
            .unbound_targets
    );
}

/// The two predicates answer different questions, pinned on the four families the narrowing
/// removed and on the three it kept. Without this, a future edit that re-pointed the decode-pack
/// bit at `non_continuable_walk_candidate` would put the duke3d regression back with every other
/// fixture in this file still green.
#[test]
fn the_break_probe_set_is_a_strict_subset_of_the_walk_candidate_set() {
    for opcode in [0x69u16, 0x6b, 0xc4, 0xc5, 0xe6, 0xee] {
        assert!(
            jit::direct::non_continuable_walk_candidate(opcode),
            "{opcode:#04x} must stay admissible to a compile walk"
        );
        assert!(
            !jit::direct::non_continuable_break_probe_candidate(opcode),
            "{opcode:#04x} must not be probed at a break site"
        );
    }
    for opcode in [0x9au16, 0xca, 0xcb] {
        assert!(
            jit::direct::non_continuable_break_probe_candidate(opcode),
            "the far-transfer family carries L7's and L8's whole measured win; {opcode:#04x} must \
             stay in the probe set"
        );
        assert!(
            jit::direct::non_continuable_walk_candidate(opcode),
            "the probe set must be a subset of the walk-candidate set, {opcode:#04x} is not"
        );
    }
    // The subset property in full, over every opcode a `DecodedInsn` can carry.
    for opcode in 0u16..=u16::MAX {
        assert!(
            !jit::direct::non_continuable_break_probe_candidate(opcode)
                || jit::direct::non_continuable_walk_candidate(opcode),
            "{opcode:#06x} is probed at a break site but is not a walk candidate"
        );
    }
}
