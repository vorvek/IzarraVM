// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The packed decode-line first touch (`IZARRAVM_DECODE_PACK`): a 16-byte side entry per decode
//! slot that answers the continuation loop's screens and the whole native admission path, so the
//! 56-byte line is faulted in only when the continuation is going to be interpreted.
//!
//! Two failure classes are worth fixtures, and they are not the same class:
//!
//! 1. The two arms disagree about what the guest did. Covered by running whole programs — a
//!    self-patching loop, a REP string move, a page-boundary break — through both arms and
//!    comparing architectural state, memory AND the break counters that say WHERE each run
//!    ended.
//! 2. A pack outlives the line it describes. That one is silent: the screens would pass on a
//!    dead slot, and the interpreted arm would only notice because its deferred `get_view`
//!    misses. Every fixture below that mutates the cache therefore asserts against `get_packed`
//!    DIRECTLY, and `assert_packs_consistent` walks the whole array, because an end-to-end
//!    program is the weakest possible witness for this class.

use super::*;

/// The array's reason to exist is that it is small. 65536 slots at this width is 1 MB against the
/// line table's 3.5 MB; a grown entry buys back the miss the slice was measured to remove
/// (11.75 percent of duke3d-586's wall, dev_docs/2026-08-08-dispatch-tier-next.md).
#[test]
fn packed_entry_stays_sixteen_bytes() {
    assert_eq!(
        core::mem::size_of::<DecodePack>(),
        16,
        "the packed first touch is sized to be resident; re-measure the slice before growing it"
    );
}

/// Select a first-touch arm for this thread and PROVE the selection took. The shipped knob is
/// opt-in and OFF by default (`decode_pack_enabled` carries the measurement that made it so), so
/// a fixture leaning on the ambient reading would test the unpacked path twice and call it a
/// differential. The override has to decide, in both directions, and this asserts that it did.
fn select_arm(pack: bool) {
    crate::set_decode_pack_for_test(Some(pack));
    assert_eq!(
        crate::decode_pack_enabled(),
        pack,
        "the fixture override must decide the arm, not the ambient IZARRAVM_DECODE_PACK"
    );
}

/// Put a fixture CPU in the state where the packed first touch is actually TAKEN, and prove it.
///
/// Without this the whole differential is vacuous, and it silently was for one revision: the arm
/// selection is gated on the run-invariant prefix of the dispatch gate chain (see the hoist in
/// `run_budgeted_inner`), and a default real-mode CPU has neither the backend admitted nor an
/// approximate-timing persona, so both arms took the unpacked path and every mutation of the
/// packed read survived. The assertions below are the guard against that returning.
fn arm_the_packed_path(cpu: &mut CpuGsw) {
    cpu.set_jit_auto_admit(true);
    assert!(
        cpu.direct_runtime.admission_active,
        "native continuations must be admitted or the packed arm is never selected"
    );
    assert!(
        cpu.mode().uses_approximate_timing(),
        "the persona must be in the approximate class or the packed arm is never selected"
    );
}

/// Run one program to completion the way the machine batch loop does, on the named first-touch
/// arm. Returns everything an arm comparison needs: the CPU, its memory, and the run-boundary
/// counters.
fn run_arm(
    pack: bool,
    code: &[u8],
    origin: u32,
    mem_len: usize,
) -> (CpuGsw, Vec<u8>, PerfCounters) {
    select_arm(pack);
    let mut memory = vec![0u8; mem_len];
    memory[origin as usize..origin as usize + code.len()].copy_from_slice(code);
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw486);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = origin;
    cpu.write_reg16(Reg16::Sp, 0x0f00);
    arm_the_packed_path(&mut cpu);
    let mut bus = TestBus::with_memory(memory);
    let mut halted = false;
    for _ in 0..10_000 {
        if cpu.run_budgeted(&mut bus, u64::MAX).unwrap().halted {
            halted = true;
            break;
        }
    }
    crate::set_decode_pack_for_test(None);
    assert!(halted, "fixture program must reach HLT");
    let counters = cpu.perf_counters().clone();
    (cpu, bus.memory.to_vec(), counters)
}

/// The differential itself. `assert_eq!` on `CpuGsw` covers architectural state (the decode cache
/// is excluded from that equality, which is exactly right here: the two arms are allowed to
/// differ in which array they read, never in what the guest observed).
fn assert_arms_agree(name: &str, code: &[u8], origin: u32, mem_len: usize) -> PerfCounters {
    let (packed_cpu, packed_memory, packed_perf) = run_arm(true, code, origin, mem_len);
    let (plain_cpu, plain_memory, plain_perf) = run_arm(false, code, origin, mem_len);
    assert_eq!(packed_cpu, plain_cpu, "{name}: architectural state");
    assert_eq!(packed_memory, plain_memory, "{name}: guest memory");
    // The break counters are the part a screen bug would move without moving guest state: a
    // screen answered from the wrong array ends the run on a different boundary, which changes
    // WHICH counter fires and how many runs the program takes, while the retired instruction
    // sequence stays identical.
    for (label, packed, plain) in [
        (
            "brk_cont_not_continuable",
            packed_perf.brk_cont_not_continuable,
            plain_perf.brk_cont_not_continuable,
        ),
        (
            "brk_cont_page_cross",
            packed_perf.brk_cont_page_cross,
            plain_perf.brk_cont_page_cross,
        ),
        (
            "brk_cont_decode_miss",
            packed_perf.brk_cont_decode_miss,
            plain_perf.brk_cont_decode_miss,
        ),
        (
            "brk_decode_or_branch",
            packed_perf.brk_decode_or_branch,
            plain_perf.brk_decode_or_branch,
        ),
        (
            "straight_line_runs",
            packed_perf.straight_line_runs,
            plain_perf.straight_line_runs,
        ),
        (
            "decode_probes",
            packed_perf.decode_probes,
            plain_perf.decode_probes,
        ),
    ] {
        assert_eq!(packed, plain, "{name}: {label}");
    }
    packed_cpu.decode_cache.assert_packs_consistent();
    plain_cpu.decode_cache.assert_packs_consistent();
    packed_perf
}

/// A self-patching loop: address 0x10 is decoded and cached as `INC AX`, the store at 0x11
/// narrow-kills that very line, and the branch back re-decodes it as the patched `INC CX`. The
/// shape the slice has to survive — a live pack whose line dies under it — driven by a real
/// guest write rather than by calling the invalidator.
const SELF_PATCHING_LOOP: [u8; 0x1c] = [
    0xb9, 0x03, 0x00, // 0x00 MOV CX, 3
    0xb0, 0x41, // 0x03 MOV AL, 0x41 (the INC CX opcode)
    0xeb, 0x09, // 0x05 JMP 0x10
    0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, // 0x07..0x0f
    0x40, // 0x10 INC AX  <- patched to INC CX on the first pass
    0xa2, 0x10, 0x00, // 0x11 MOV [0x0010], AL
    0xfe, 0xc1, // 0x14 INC CL
    0x80, 0xf9, 0x05, // 0x16 CMP CL, 5
    0x72, 0xf5, // 0x19 JB 0x10
    0xf4, // 0x1b HLT
];

#[test]
fn arms_agree_on_a_self_patching_loop() {
    assert_arms_agree("self-patching loop", &SELF_PATCHING_LOOP, 0, 0x4000);
}

#[test]
fn arms_agree_on_a_non_continuable_terminator() {
    // `continuable` is the first screen and the only one whose input is a single packed bit, so a
    // fixture that never reaches a cached non-continuable line leaves that bit free to be wrong.
    // Choosing the terminator took two tries and both rejects are worth recording: every far
    // transfer LOADS CS, which flushes the decode cache and leaves the line cold at exactly the
    // probe that needs it warm; and `OUT imm8, AL` leaves `requires_step_break` asserted on the
    // test bus, so every later run ends after one instruction and the loop never chains far
    // enough to reach the screen. `XLAT` is neither: `DecodeGroup::Misc`, refused by
    // `block_continuable` on every persona, and it touches nothing but AL.
    let code = [
        0xb9, 0x02, 0x00, // 0x00 MOV CX, 2
        0x49, // 0x03 DEC CX   <- loop target
        0x90, // 0x04 NOP
        0xd7, // 0x05 XLAT
        0x85, 0xc9, // 0x06 TEST CX, CX
        0x75, 0xf9, // 0x08 JNZ 0x03
        0xf4, // 0x0a HLT
    ];
    let perf = assert_arms_agree("non-continuable terminator", &code, 0, 0x4000);
    assert!(
        perf.brk_cont_not_continuable > 0,
        "the fixture must actually reach the continuable screen with a warm line"
    );
}

#[test]
fn arms_agree_on_a_rep_string_move() {
    // REP MOVSB is the one continuation form that takes the budgeted interpreter arm, which the
    // packed arm reaches only after materialising the full line (the REP prefix is not a packed
    // field). A pack that answered for it would show up as a different instruction stream.
    let code = [
        0xbe, 0x00, 0x20, // MOV SI, 0x2000
        0xbf, 0x00, 0x28, // MOV DI, 0x2800
        0xb9, 0x40, 0x00, // MOV CX, 0x40
        0xf3, 0xa4, // REP MOVSB
        0xf4, // HLT
    ];
    assert_arms_agree("rep movsb", &code, 0, 0x4000);
}

/// The run loop's page-cross screen is GONE (audit 1.17), and this is the fixture that pins the
/// premise it was deleted on: `DecodeCache::put` refuses a page-straddling insert outright, under
/// the identical predicate the screen used, so no continuation can ever be handed such a line and
/// `brk_cont_page_cross` is identically zero.
///
/// It used to hand-publish a straddling line through `publish_line` to drive the screen. That
/// proved the screen ran, not that anything could reach it; `publish_line` has exactly one
/// production caller and it sits BELOW `put`'s straddle rejection. So the fixture now asks the
/// question that matters: let the decoder meet a page-straddling instruction the way a guest
/// would, and check the cache refuses to hold it, on both arms, with the run reaching HLT and the
/// right answer in AX either way.
fn page_cross_arm(pack: bool) -> (CpuGsw, PerfCounters) {
    select_arm(pack);
    let mut memory = vec![0u8; 0x4000];
    memory[0x0ffd] = 0x90; // NOP, so 0x0ffe is reached as a CONTINUATION
    memory[0x0ffe..0x1001].copy_from_slice(&[0xb8, 0x34, 0x12]); // MOV AX, 0x1234
    memory[0x1001] = 0xf4; // HLT
    memory[0x2000..0x2003].copy_from_slice(&[0xb8, 0x34, 0x12]); // the same bytes, page-interior
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw486);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    arm_the_packed_path(&mut cpu);
    let mut bus = TestBus::with_memory(memory);

    // The identical bytes at a page-INTERIOR address warm a line, which is the control: the
    // decoder produces a three-byte MOV for them and `put` keeps it.
    cpu.registers.eip = 0x2000;
    cpu.begin_instruction();
    cpu.fetch_decoded(&mut bus, 0x2000).expect("decode MOV");
    let insn = cpu.decode_cache.get(0x2000, false).expect("warm MOV line");
    assert_eq!(insn.len, 3);

    // The same instruction at 0x0ffe straddles the 4 KiB boundary. Decoding it works; CACHING it
    // does not, and that refusal is what makes the run loop's deleted page-cross screen dead.
    cpu.registers.eip = 0x0ffe;
    cpu.begin_instruction();
    cpu.fetch_decoded(&mut bus, 0x0ffe).expect("decode MOV");
    assert!(
        cpu.decode_cache.get(0x0ffe, false).is_none(),
        "put must refuse a page-straddling line"
    );

    cpu.registers.eip = 0x0ffd;
    cpu.begin_instruction();
    let mut halted = false;
    for _ in 0..64 {
        if cpu.run_budgeted(&mut bus, u64::MAX).unwrap().halted {
            halted = true;
            break;
        }
    }
    crate::set_decode_pack_for_test(None);
    assert!(halted, "the page-cross fixture must reach HLT");
    let counters = cpu.perf_counters().clone();
    (cpu, counters)
}

#[test]
fn arms_agree_when_the_next_instruction_crosses_a_page() {
    let (packed_cpu, packed_perf) = page_cross_arm(true);
    let (plain_cpu, plain_perf) = page_cross_arm(false);
    assert_eq!(
        packed_perf.brk_cont_page_cross, 0,
        "the page-cross counter is dead: put refuses the line the screen looked for"
    );
    assert_eq!(
        plain_perf.brk_cont_page_cross, 0,
        "and it is dead on the unpacked arm too"
    );
    assert!(
        packed_perf.brk_cont_decode_miss > 0,
        "the straddling continuation must end the run as a decode miss"
    );
    assert_eq!(
        packed_perf.brk_cont_decode_miss, plain_perf.brk_cont_decode_miss,
        "and both arms must count the same number of them"
    );
    assert_eq!(packed_cpu, plain_cpu);
    assert_eq!(packed_cpu.read_reg16(Reg16::Ax), 0x1234);
}

/// Warm `lin` as a real-mode decode and hand back the CPU/bus so a fixture can mutate the cache
/// underneath it.
fn warmed(lines: &[u32]) -> (CpuGsw, TestBus) {
    let mut memory = vec![0u8; 0x5000];
    for &lin in lines {
        memory[lin as usize] = 0x90;
    }
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    let mut bus = TestBus::with_memory(memory);
    for &lin in lines {
        cpu.registers.eip = lin;
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, lin).expect("decode NOP");
    }
    (cpu, bus)
}

/// THE mutation this fixture exists for: drop the `packs[index].generation = 0` from
/// `kill_line_at` and the narrow kill retires the line while leaving a live pack behind. Every
/// end-to-end program still passes under that mutation, because the interpreted arm's deferred
/// `get_view` misses and the run simply ends; this assertion is what fails.
#[test]
fn narrow_smc_kill_retires_the_packed_entry() {
    let (mut cpu, _bus) = warmed(&[0x1000, 0x1020]);
    assert!(cpu.decode_cache.get_packed(0x1000, false).is_some());
    assert!(cpu.decode_cache.get_packed(0x1020, false).is_some());
    let generation = cpu.decode_cache_generation();

    cpu.note_device_memory_write_range(0x1000, 1);

    assert_eq!(
        cpu.decode_cache_generation(),
        generation,
        "the write must take the narrow path, not the wholesale flush"
    );
    assert!(
        cpu.decode_cache.get(0x1000, false).is_none(),
        "the line itself must be dead"
    );
    assert!(
        cpu.decode_cache.get_packed(0x1000, false).is_none(),
        "a killed line must not keep answering out of its packed entry"
    );
    assert!(cpu.decode_cache.get_packed(0x1020, false).is_some());
    cpu.decode_cache.assert_packs_consistent();
}

/// The other in-place retirement: a different key landing on the same slot. `put` republishes
/// both arrays from one value, so the displaced key must miss in the pack as well — and the new
/// key's hotness must start over, which is the field `put` used to reset on the line.
#[test]
fn eviction_republishes_the_packed_entry() {
    let (mut cpu, _bus) = warmed(&[0x1000]);
    let insn = cpu.decode_cache.get(0x1000, false).expect("warm line");
    let collision = 0x1000 + cpu.decode_cache.lines.len() as u32;
    let slot = (0x1000 & cpu.decode_cache.mask) as usize;
    #[cfg(feature = "jit")]
    {
        // Heat the slot so the republish has something to reset. The threshold is deliberately
        // above `DEFAULT_ADMISSION_HEAT` (1 under cfg(test)) so the call bumps rather than
        // reports hot; what the fixture cares about is the counter, not the verdict.
        let _ = cpu.decode_cache.direct_hot_at(slot as u32, 8);
        assert_ne!(cpu.decode_cache.packs[slot].jit_direct_hotness, 0);
    }

    assert!(
        cpu.decode_cache
            .put(collision, insn, false, 0x2000)
            .inserted
    );

    assert!(cpu.decode_cache.get_packed(0x1000, false).is_none());
    let packed = cpu
        .decode_cache
        .get_packed(collision, false)
        .expect("the new key owns the slot");
    assert_eq!(packed.phys_start, 0x2000);
    #[cfg(feature = "jit")]
    assert_eq!(cpu.decode_cache.packs[slot].jit_direct_hotness, 0);
    cpu.decode_cache.assert_packs_consistent();
}

/// The wholesale paths. The generation bump is O(1) for both arrays because the pack stores the
/// stamp rather than reading through to the line; the wrap is the one case that must physically
/// clear, and a pack surviving it would be live again the next time the counter came round.
#[test]
fn wholesale_invalidation_retires_every_packed_entry() {
    let (mut cpu, _bus) = warmed(&[0x1000, 0x1020]);
    cpu.decode_cache.invalidate_and_clear_code_marks();
    assert!(cpu.decode_cache.get_packed(0x1000, false).is_none());
    assert!(cpu.decode_cache.get_packed(0x1020, false).is_none());
    cpu.decode_cache.assert_packs_consistent();

    let (mut cpu, _bus) = warmed(&[0x1000]);
    cpu.decode_cache.generation = u32::MAX;
    let slot = (0x1000 & cpu.decode_cache.mask) as usize;
    cpu.decode_cache.lines[slot].generation = u32::MAX;
    cpu.decode_cache.packs[slot].generation = u32::MAX;
    cpu.decode_cache.invalidate_and_clear_code_marks();
    assert_eq!(cpu.decode_cache.generation, 1);
    assert!(
        cpu.decode_cache
            .packs
            .iter()
            .all(|pack| pack.generation == 0 && pack.len == 0),
        "the wrap must physically clear the packed array, not only the lines"
    );
}

/// The hit condition itself, field by field. A pack that ignored the tag or the D bit would serve
/// one key's screens for another's — a colliding linear address and a mode change are the two
/// ways that happens in practice.
#[test]
fn packed_hit_condition_screens_tag_and_d_bit() {
    let (cpu, _bus) = warmed(&[0x1000]);
    let collision = 0x1000 + cpu.decode_cache.lines.len() as u32;
    assert!(cpu.decode_cache.get_packed(0x1000, false).is_some());
    assert!(
        cpu.decode_cache.get_packed(collision, false).is_none(),
        "a colliding tag must miss"
    );
    assert!(
        cpu.decode_cache.get_packed(0x1000, true).is_none(),
        "a line decoded under D=0 must not answer a D=1 lookup"
    );
    let packed = cpu.decode_cache.get_packed(0x1000, false).expect("warm");
    let view = cpu.decode_cache.get_view(0x1000, false).expect("warm");
    assert_eq!(packed.len, view.insn.len);
    assert_eq!(packed.continuable, view.insn.continuable);
    assert_eq!(packed.phys_start, view.phys_start);
}

/// `warmed`, for programs that are not a NOP. Each pair is a linear address and the bytes to
/// decode there; the addresses must not share a decode slot or a page.
fn warmed_bytes(programs: &[(u32, &[u8])]) -> (CpuGsw, TestBus) {
    let mut memory = vec![0u8; 0x5000];
    for (lin, bytes) in programs {
        let at = *lin as usize;
        memory[at..at + bytes.len()].copy_from_slice(bytes);
    }
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    let mut bus = TestBus::with_memory(memory);
    for (lin, _) in programs {
        cpu.registers.eip = *lin;
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, *lin).expect("decode");
    }
    (cpu, bus)
}

/// The two screen arms must answer `noncont_break_probe` identically for every line the cache can
/// hold, and this is the only fixture that says so.
///
/// The two producers do NOT compute it the same way. `publish_line` stores the opcode predicate
/// alone; `DecodeLineView::screen` ANDs it with `!continuable`, because the run loop reads the
/// field in one place inside the `!continuable` break arm and the guard buys back the predicate on
/// the nineteen-in-twenty continuable path (audit item 1.19). Those two definitions differ exactly
/// on a line that is continuable AND carries a probe-set opcode.
///
/// No such line exists, and THAT is the property worth pinning, because it is a property of the
/// opcode set rather than of either producer: every opcode
/// `non_continuable_break_probe_candidate` names is a far transfer, and a far transfer always
/// decodes non-continuable. Widen that set to anything continuable and this fixture goes red
/// before the disagreement can reach a second consumer of the field.
///
/// The three quadrants that are reachable are all covered: non-continuable and in the set (the far
/// family), non-continuable and out of it (`INT 21h`), continuable and out of it (`NOP`).
#[test]
fn two_arms_agree_on_the_break_probe_bit() {
    const RETF: u32 = 0x1000;
    const RETF_IMM: u32 = 0x1020;
    const CALL_FAR: u32 = 0x1040;
    const INT21: u32 = 0x1060;
    const NOP: u32 = 0x1080;

    let (cpu, _bus) = warmed_bytes(&[
        (RETF, &[0xcb]),
        (RETF_IMM, &[0xca, 0x00, 0x00]),
        (CALL_FAR, &[0x9a, 0x00, 0x00, 0x00, 0x00]),
        (INT21, &[0xcd, 0x21]),
        (NOP, &[0x90]),
    ]);

    for (lin, name) in [
        (RETF, "RETF"),
        (RETF_IMM, "RETF imm16"),
        (CALL_FAR, "CALL FAR"),
        (INT21, "INT 21h"),
        (NOP, "NOP"),
    ] {
        let packed = cpu.decode_cache.get_packed(lin, false).expect("warm");
        let view = cpu.decode_cache.get_view(lin, false).expect("warm");
        let unpacked = view.screen();
        assert_eq!(
            packed.continuable, unpacked.continuable,
            "{name}: the arms disagree about continuable"
        );
        assert_eq!(
            packed.noncont_break_probe, unpacked.noncont_break_probe,
            "{name}: the arms disagree about the break-probe bit, so the guard in \
             DecodeLineView::screen has become observable"
        );
    }

    // The premise the agreement rests on: every opcode in the probe set decodes non-continuable,
    // so `!continuable` is never the term that decides the unpacked arm's answer.
    for (lin, opcode, name) in [
        (RETF, 0xcbu16, "RETF"),
        (RETF_IMM, 0xca, "RETF imm16"),
        (CALL_FAR, 0x9a, "CALL FAR"),
    ] {
        assert!(
            jit::direct::non_continuable_break_probe_candidate(opcode),
            "{name} must be in the probe set for this fixture to test anything"
        );
        let view = cpu.decode_cache.get_view(lin, false).expect("warm");
        assert!(
            !view.insn.continuable,
            "{name} is in the probe set and must decode non-continuable; a continuable member \
             would split the two screen arms"
        );
        assert!(
            view.screen().noncont_break_probe,
            "{name} must reach the run loop's break gate"
        );
    }

    // And the negative, so the assertions above cannot pass by the bit being universally set.
    for (lin, name) in [(INT21, "INT 21h"), (NOP, "NOP")] {
        let view = cpu.decode_cache.get_view(lin, false).expect("warm");
        assert!(
            !view.screen().noncont_break_probe,
            "{name} is not in the probe set and must not reach the break gate"
        );
    }
    assert!(
        cpu.decode_cache
            .get_view(NOP, false)
            .expect("warm")
            .insn
            .continuable,
        "NOP must decode continuable, which is the quadrant the guard actually screens"
    );
}

/// The counter that says the deferred line fetch never misses is only worth reading if it CAN
/// report a miss. Nothing in a passing run exercises the increment or its snapshot copy, so a
/// mistyped field in `stall_snapshot` would make the counter report zero forever — which is
/// exactly the reading the whole staleness argument leans on. This drives the plumbing directly.
#[test]
fn the_late_view_miss_counter_reaches_the_snapshot() {
    let mut cpu = CpuGsw::default();
    assert_eq!(cpu.direct_stall_snapshot().decode_pack_late_view_miss, 0);
    cpu.jit_direct.direct.note_decode_pack_late_view_miss();
    assert_eq!(cpu.direct_stall_snapshot().decode_pack_late_view_miss, 1);
}
