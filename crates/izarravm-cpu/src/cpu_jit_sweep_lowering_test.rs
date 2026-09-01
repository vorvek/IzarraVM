// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Differential cover for the lowerings added by the runtime-weighted reject sweep
//! (`dev_docs/2026-07-30-dispatch-architecture-audit.md` §5d).
//!
//! Every case runs the same guest bytes natively and interpreted from identical state and
//! compares registers, lazy flags, EFLAGS, core clocks and bus clocks. The tested opcode is
//! placed MID-BLOCK, never at the entry: an opcode at a block's entry slot parks the block on
//! the interpreter, so an entry-position fixture silently tests nothing.

use super::*;

const ENTRY: u32 = 0x401;
/// Well inside the fixture's 0x5000 of memory, so a stack slot's store page resolves.
const STACK_TOP: u32 = 0x4000;

fn flat_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x08, 0x9b));
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
    cpu.set_eip(ENTRY);
    cpu
}

/// Run `body` mid-block against the interpreter. `seed_eflags` is applied to both roles, so a
/// lowering that reads a flag it should have ignored, or drops one it should have kept, diverges.
fn differential(body: &[u8], seed_eflags: u32, live_pending: bool, context: &str) {
    differential_with(body, seed_eflags, live_pending, 0, context);
}

/// As `differential`, but seeds ECX too, for the count-bearing forms.
fn differential_with(
    body: &[u8],
    seed_eflags: u32,
    live_pending: bool,
    seed_ecx: u32,
    context: &str,
) {
    differential_full(body, seed_eflags, live_pending, seed_ecx, 0, context);
}

/// As `differential`, but seeds EAX instead of ECX, for the accumulator-implicit forms
/// (CBW/CWDE/CWD/CDQ) that read the accumulator rather than a ModRM-selected register. Plain
/// eflags and no live pending flags: none of the four touch flags, so there is nothing for those
/// axes to catch here.
fn differential_seeded(body: &[u8], seed_eax: u32, context: &str) {
    differential_full(body, 0x202, false, 0, seed_eax, context);
}

fn differential_full(
    body: &[u8],
    seed_eflags: u32,
    live_pending: bool,
    seed_ecx: u32,
    seed_eax: u32,
    context: &str,
) {
    // A leading `mov esi,esi` keeps the tested opcode off the entry slot; the two trailing
    // register moves and the HLT give the block a tail and a terminator.
    let mut code = vec![0x89, 0xf6];
    let body_at = ENTRY + code.len() as u32;
    code.extend_from_slice(body);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);

    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu();
    let mut interpreter = flat_cpu();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interpreter_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, body_at, tail_at];
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interpreter, &mut interpreter_bus),
    ] {
        // ESP must be live BEFORE compiling, not only before running: a stack-touching slot
        // resolves its store page at compile time, and the default ESP of 0 makes that page
        // 0xFFFFFFFC, which cannot resolve and returns the whole block as Retry --
        // indistinguishable from the opcode still being a barrier.
        cpu.registers.set_esp(STACK_TOP);
        for &linear in &starts {
            cpu.set_eip(linear);
            cpu.fetch_decoded(bus, linear).unwrap();
        }
        // The page the stack slot writes into has to be in the fast map before compilation, for
        // the same reason ESP does: an unresolvable store page returns the block as Retry.
        let page = (STACK_TOP - 4) & !0xfff;
        let read = bus
            .direct_page(page, BusAccessKind::DataRead)
            .unwrap()
            .unwrap();
        assert!(cpu.jit_fast_map.populate_read(
            page,
            page,
            read,
            jit::fast_map::PagePermissions::UNPAGED,
            cpu.physical_page_watched(page)
        ));
        let write = bus
            .direct_page(page, BusAccessKind::DataWrite)
            .unwrap()
            .unwrap();
        assert!(cpu.jit_fast_map.populate_write(
            page,
            page,
            write,
            jit::fast_map::PagePermissions::UNPAGED,
            cpu.physical_page_watched(page)
        ));
    }

    let key = jit::direct::key_for(&native, ENTRY, true).expect("entry key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut native, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("{context}: structurally rejected; the opcode is still a barrier")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("{context}: compile asked for a retry"),
    };
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = native.jit_direct.block(id).expect("live block");
    assert_eq!(
        compilation.span.instructions, 3,
        "{context}: block must cover all three slots, so the tested opcode really ran natively"
    );

    for cpu in [&mut native, &mut interpreter] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(STACK_TOP);
        cpu.registers.set_ecx(seed_ecx);
        cpu.registers.set_eax(seed_eax);
        // Garbage in EDX's upper 16 bits for both roles identically: CWD/CBW must write only
        // the low 16 bits of their destination, so a lowering that widened the write to all 32
        // (e.g. a plain `mov_r32_r32` instead of `emit_write_gpr16`) clobbers this and diverges
        // from the interpreter, which never touches bits above the ones it defines.
        cpu.registers.set_edx(0xdead_0000);
        cpu.registers.eflags = seed_eflags;
        cpu.pending_flags = PendingFlags::default();
        if live_pending {
            // NOTE for carry-in callers: `flag(FLAG_CF)` routes CF through this pending
            // descriptor whenever one is live (`core.rs`'s ARITH-flag short circuit), so the
            // priming ADD's own CF (always 0: `0x7fff_ffff + 1` does not carry out of thirty-two
            // bits) is what a carry-in reader sees here, NOT `seed_eflags`'s CF bit. A row that
            // needs a genuine CF=1 delivered has to pair `live_pending: false` with a CF-set
            // `seed_eflags`, which reads straight from `registers.eflags` with no descriptor in
            // the way.
            let _ = cpu.alu(0, 0x7fff_ffff, 1, BusWidth::Dword);
        }
        cpu.set_eip(ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();

    let retired = native.perf_counters().jit_direct_insns;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap(),
        "{context}: block did not run natively"
    );
    for _ in 0..3 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(
        native.perf_counters().jit_direct_insns - retired,
        3,
        "{context}: all three slots must retire natively"
    );
    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interpreter),
        "{context}: registers"
    );
    assert_eq!(native.eflags(), interpreter.eflags(), "{context}: EFLAGS");
    assert_eq!(
        native.elapsed_clocks, interpreter.elapsed_clocks,
        "{context}: core clocks"
    );
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks(),
        "{context}: bus clocks"
    );
}

/// Phase 5 Task 2's call-out slot matrix. Nested here rather than registered beside the other JIT
/// test files so it inherits this module's differential fixture (`ENTRY`, `STACK_TOP`,
/// `flat_cpu`) instead of duplicating it.
#[path = "cpu_jit_callout_matrix_test.rs"]
mod callout_matrix;

/// The rejected-row campaign's F7 group (Slice 2). Nested here for the same reason the call-out
/// matrix is: it wants this module's differential fixture (`ENTRY`, `STACK_TOP`, `flat_cpu`)
/// rather than a third copy of it.
#[path = "cpu_jit_f7_group_test.rs"]
mod f7_group;

/// The rejected-row campaign's sixteen-bit MEMORY rows (Slice 3). Nested here for the same reason
/// the two modules above are: it wants this module's differential fixture (`ENTRY`, `STACK_TOP`,
/// `flat_cpu`) rather than a fourth copy of it.
#[path = "cpu_jit_word_memory_test.rs"]
mod word_memory;

/// Guard 3: the misaligned memory admission at the two lean one-lookup sites. Nested here for the
/// reason the modules above are: it wants this module's differential fixture (`ENTRY`,
/// `STACK_TOP`, `flat_cpu`) rather than another copy of it.
#[path = "cpu_jit_misaligned_memory_test.rs"]
mod misaligned_memory;

/// The rejected-row campaign's sixteen-bit REGISTER shift lane (Slice 3b), the repair for the
/// relocation Slice 3 created. Nested here for the reason the three modules above are: it wants
/// this module's differential fixture (`ENTRY`, `STACK_TOP`, `flat_cpu`) rather than a fifth copy.
#[path = "cpu_jit_word_shift_test.rs"]
mod word_shift;

/// The BYTE shift/rotate register rows (`0xD0 /4..=7` and `0xC0 /5,/6,/7`), behind
/// `IZARRAVM_BYTE_SHIFT_ROWS`. Nested here for the reason the modules above are: it wants this
/// module's differential fixture (`ENTRY`, `STACK_TOP`, `flat_cpu`) rather than another copy of it.
#[path = "cpu_jit_byte_shift_test.rs"]
mod byte_shift;

/// `vorvek/direct-word-rot1`: the sixteen-bit REGISTER rotate lane, `0xC1`/`0xD1 /0,/1`. Nested
/// here for the reason the modules above are: it wants this module's differential fixture
/// (`ENTRY`, `STACK_TOP`, `flat_cpu`) rather than another copy of it.
#[path = "cpu_jit_word_rotate_test.rs"]
mod word_rotate;

/// The sixteen-bit INC/DEC lazy-flag descriptor, the survivor `word_memory`'s header recorded and
/// left for its own change. Nested here for the reason the four modules above are: it wants this
/// module's differential fixture (`ENTRY`, `STACK_TOP`, `flat_cpu`) rather than a sixth copy.
#[path = "cpu_jit_word_inc_dec_test.rs"]
mod word_inc_dec;

/// The rejected-row campaign's Slice 7: the three-operand IMUL and the byte-lane register ALU.
/// Nested here for the reason the five modules above are: it wants this module's differential
/// fixture (`ENTRY`, `STACK_TOP`, `flat_cpu`) rather than a seventh copy.
#[path = "cpu_jit_slice7_test.rs"]
mod slice7;

/// `IZARRAVM_DIRECT_EAGER_FLAGS`: flag producers publish the RBP shadow instead of writing a lazy
/// descriptor. Nested here for the reason the six modules above are: it wants this module's
/// differential fixture (`ENTRY`, `STACK_TOP`, `flat_cpu`) rather than an eighth copy.
#[path = "cpu_jit_eager_flags_test.rs"]
mod eager_flags;

/// The four unprefixed STRING FAMILIES as `InterpretOne` call-out rows, behind
/// `IZARRAVM_GENERIC_CALLOUT`. Nested here for the reason the seven modules above are: it wants
/// this module's `ENTRY`, `STACK_TOP` and `flat_cpu` rather than a ninth copy of them.
#[path = "cpu_jit_string_callout_test.rs"]
mod string_callout;

#[test]
fn direction_flag_matches_the_interpreter_from_both_polarities() {
    // 0x202 has DF clear, 0x602 has DF (bit 10) set, so each opcode is exercised both as a
    // no-op and as a real transition. Live pending flags on every case: DF is outside the lazy
    // descriptor's ARITH mask, so a lowering that cleared or rebuilt the descriptor diverges.
    for (opcode, name) in [(0xfcu8, "CLD"), (0xfd, "STD")] {
        for seed in [0x202u32, 0x602] {
            for pending in [false, true] {
                differential(
                    &[opcode],
                    seed,
                    pending,
                    &format!("{name} seed={seed:#x} pending={pending}"),
                );
            }
        }
    }
}

#[test]
fn shift_by_cl_matches_the_interpreter_for_every_count_class() {
    // The classes that matter are MASK classes, not magnitudes: 0 (no flag may move at all -- the
    // merge must be skipped entirely), 1 (OF defined), 2..=31 (OF preserved from the seed), and
    // 32/33 which mask back to 0 and 1. The two garbage seeds carry bits above CL and above the
    // five-bit mask that the host masks away and the lowering must not react to.
    //
    // Seeds with OF set (0xa02, 0x8d7) are load-bearing: on a count above 1 the lowering must
    // PRESERVE the seeded OF, so a version that always merged the OF bit fails here and passes
    // every OF-clear seed.
    for op in [4u8, 5, 6, 7] {
        for ecx in [0u32, 1, 5, 31, 32, 33, 0xffff_ff05, 0x0000_2101] {
            for seed in [0x202u32, 0xa02, 0x8d7] {
                // 0xD3 /op with mod=11 on EAX (rm=0), and on ECX (rm=1) to pin that the count is
                // read before a destination write can disturb it.
                for rm in [0u8, 1] {
                    let modrm = 0b1100_0000 | (op << 3) | rm;
                    let context = format!("d3 /{op} rm={rm} ecx={ecx:#x} seed={seed:#x}");
                    differential_with(&[0xd3, modrm], seed, true, ecx, &context);
                }
            }
        }
    }
}

#[test]
fn pushfd_matches_the_interpreter_including_the_persona_mask() {
    // The live-pending cases are the point: the interpreter calls `materialize_flags()` before
    // reading EFLAGS, so a lowering that pushed a stale image, or that pushed the right image
    // but left the descriptor standing, diverges on the pushed dword or on `pending_flags`.
    //
    // Seeds carry bits the persona mask must KEEP (AC 0x40000, ID 0x200000 on 586) and bits it
    // must DROP (RF 0x10000). A lowering that pushed raw EFLAGS passes the plain seeds and fails
    // these.
    for seed in [0x202u32, 0x602, 0x1_0202, 0x24_0202, 0x25_0a02] {
        for pending in [false, true] {
            differential(
                &[0x9c],
                seed,
                pending,
                &format!("PUSHFD seed={seed:#x} pending={pending}"),
            );
        }
    }
}

#[test]
fn byte_alu_memory_destination_matches_the_interpreter_for_every_op_and_lane() {
    // ALU form 0 with a memory destination: all eight ops, both the writing path and CMP's
    // read-only one, driven through a low byte lane (CL) and a high one (CH). The lane matters
    // because `StoreSource::Reg` picks it from the ModRM reg field exactly as `read_gpr8` does,
    // and a lowering that took the low byte of the right register would pass every CL case.
    //
    // The disp32 lands inside the page the harness already populates in the fast map for the
    // stack slot, so the access resolves without a side exit; anywhere else returns the whole
    // block as Retry and the fixture would report the opcode as still being a barrier.
    //
    // `op = 2` (`0x10` ADC) and `op = 3` (`0x18` SBB) consume CF as an operand, and `flag(FLAG_CF)`
    // (`core.rs`) routes it through a live pending descriptor whenever one exists. `pending` used
    // to be hardcoded `true`, so `0x8d7`'s seeded CF was always replaced by the priming op's own CF
    // (always clear) and ADC/SBB never saw a real carry-in. Sweeping `pending` here delivers it:
    // `(0x8d7, false)` is the no-descriptor row that actually carries CF=1 into the ALU.
    const TARGET: u32 = 0x3f00;
    for op in 0u8..8 {
        for lane in [1u8, 5] {
            for ecx in [0x0000_0000u32, 0x0000_017f, 0x0000_ab01, 0xffff_ffff] {
                for seed in [0x202u32, 0x8d7] {
                    for pending in [false, true] {
                        let modrm = (lane << 3) | 0b101;
                        let mut body = vec![op << 3, modrm];
                        body.extend_from_slice(&TARGET.to_le_bytes());
                        let context = format!(
                            "alu form0 mem op={op} lane={lane} ecx={ecx:#x} seed={seed:#x} \
                             pending={pending}"
                        );
                        differential_with(&body, seed, pending, ecx, &context);
                    }
                }
            }
        }
    }
}

#[test]
fn setcc_register_form_matches_the_interpreter_for_every_condition() {
    // All sixteen conditions against flag seeds that separate them: OF, CF, ZF, SF and PF are
    // each set and clear across the set. The signed pairs (0xc/0xd/0xe/0xf) turn on SF != OF, and
    // BOTH polarities of that are present: 0x282 has SF set with OF clear, 0xa02 has OF set with
    // SF clear. 0x8d7 and 0xed7 carry SF and OF together, which is the SF == OF side.
    //
    // Both a low lane (DL, rm=2) and a high one (BH, rm=7) because `emit_write_gpr8` splits on
    // the register index and the high lanes take the shift-and-merge path. `live_pending` is
    // driven both ways: the interpreter materializes lazily before reading a flag, so a lowering
    // that read a stale EFLAGS image passes every eager seed and fails these.
    for condition in 0u8..16 {
        for rm in [2u8, 7] {
            for seed in [0x202u32, 0x206, 0x246, 0x282, 0x8d7, 0xa02, 0xed7] {
                for pending in [false, true] {
                    let modrm = 0b1100_0000 | rm;
                    let context =
                        format!("setcc {condition:#x} rm={rm} seed={seed:#x} pending={pending}");
                    differential(&[0x0f, 0x90 | condition, modrm], seed, pending, &context);
                }
            }
        }
    }
}

#[test]
fn cwde_matches_the_interpreter_across_both_sign_boundaries_and_widths() {
    // CBW/CWDE (0x98): CBW (Word, `0x66` prefix here) widens AL into AX; CWDE (Dword) widens AX
    // into EAX. Neither touches flags, so `differential_seeded` leaves eflags plain and lazy
    // flags inert. The seed set spans both sign boundaries this opcode can hit: 0x7fff/0x8000
    // (AL = 0x7f/0x80) is the byte boundary CBW reads, 0x7fff_ffff/0x8000_0000 (AX =
    // 0xffff/0x0000) is the word boundary CWDE reads, and 0/1/0x8000_0001/0xffff_ffff round out
    // the set with boundary-adjacent and all-ones controls. `differential_full`'s fixed
    // `0xdead_0000` seed in EDX (untouched by this opcode) and the seed's own upper 16 bits
    // (nonzero on the Dword-boundary seeds) double as CBW's write-width check: a lowering that
    // widened the write to all 32 bits of EAX instead of just AX would clobber that upper half
    // and diverge from the interpreter, which defines only the bits CBW actually writes.
    for word in [false, true] {
        for seed in [
            0u32,
            1,
            0x7fff_ffff,
            0x8000_0000,
            0x8000_0001,
            0xffff_ffff,
            0x7fff,
            0x8000,
        ] {
            let body = if word { vec![0x66, 0x98] } else { vec![0x98] };
            let context = format!("cwde word={word} eax={seed:#x}");
            differential_seeded(&body, seed, &context);
        }
    }
}

#[test]
fn cdq_matches_the_interpreter_across_both_sign_boundaries_and_widths() {
    // CWD/CDQ (0x99): CWD (Word) fills DX from AX's sign; CDQ (Dword) fills EDX from EAX's
    // sign. Same seed set and same no-flags rationale as the CBW/CWDE test above. The fixed
    // `0xdead_0000` seed in EDX is the write-width check for CWD: a lowering that filled all 32
    // bits of EDX instead of just DX would clobber that upper half.
    for word in [false, true] {
        for seed in [
            0u32,
            1,
            0x7fff_ffff,
            0x8000_0000,
            0x8000_0001,
            0xffff_ffff,
            0x7fff,
            0x8000,
        ] {
            let body = if word { vec![0x66, 0x99] } else { vec![0x99] };
            let context = format!("cdq word={word} eax={seed:#x}");
            differential_seeded(&body, seed, &context);
        }
    }
}

/// `0x83 /op r16, imm8` at Word operand size -- the native half of the rejected-row campaign's
/// Slice 1, and the first WRITING word ALU form the emitter carries.
///
/// The census ranks `0x83 /5` SUB word at 9,776,289 doom dispatcher exits, forty-seven from
/// PUSHAD; the whole non-carry sub-op set is swept because they share one emitter arm and one
/// classifier arm, so covering only `/5` would leave five siblings emitted and untested.
///
/// Three properties this has to catch and a Dword lowering would not:
///
/// * the destination's HIGH SIXTEEN BITS must survive. Every seed below carries a recognisable
///   high half, and a lowering that wrote the result back with a 32-bit `mov` clobbers it.
/// * the flags are SIXTEEN-BIT flags. The seeds straddle 0x8000 and 0xffff so CF, OF and SF differ
///   between the 16-bit and 32-bit answers rather than agreeing by luck.
/// * the immediate is a SIGN-EXTENDED imm8, so a negative one must be masked to sixteen bits
///   before the operation and not after.
#[test]
fn word_alu_immediate_forms_match_the_interpreter_for_every_admitted_sub_op() {
    // (sub-op, name). ADC (/2) and SBB (/3) joined on the L1 width lift: `emit_carry_alu_preloaded`
    // grew a Word lane, so `classify` no longer refuses them here. THREE `(eflags, live_pending)`
    // pairs run below, not two: `differential_full`'s `cpu.alu` priming call leaves CF clear, and
    // `flag(FLAG_CF)` (`core.rs`) routes CF through that live descriptor rather than through
    // `seed_eflags` whenever one exists, so `(0x8d5, true)` alone would never deliver a real CF=1
    // to a carry reader. `(0x8d7, false)` is what does -- no descriptor in the way, CF comes
    // straight out of `registers.eflags`. `(0x202, false)` and `(0x8d5, true)` are still both run
    // for the pending-descriptor-replacement coverage every op needs, carry or not.
    let ops: [(u8, &str); 8] = [
        (0, "add"),
        (1, "or"),
        (2, "adc"),
        (3, "sbb"),
        (4, "and"),
        (5, "sub"),
        (6, "xor"),
        (7, "cmp"),
    ];
    // High halves that must be preserved, low halves at the sixteen-bit corners.
    let seeds: [u32; 6] = [
        0xdead_0000,
        0xdead_0001,
        0xdead_7fff,
        0xdead_8000,
        0xdead_ffff,
        0xffff_000f,
    ];
    // Sign-extended imm8s: zero, one, the positive and negative extremes.
    let imms: [u8; 5] = [0x00, 0x01, 0x7f, 0x80, 0xff];

    for (op, name) in ops {
        for seed in seeds {
            for imm in imms {
                for (eflags, live_pending) in [(0x202u32, false), (0x8d5, true), (0x8d7, false)] {
                    let body = [0x66u8, 0x83, 0xc0 | (op << 3) | 1, imm];
                    let context = format!(
                        "0x83 /{op} {name} cx,{imm:#04x} seed={seed:#010x} \
                         eflags={eflags:#x} pending={live_pending}"
                    );
                    differential_with(&body, eflags, live_pending, seed, &context);
                }
            }
        }
    }
}

/// `MOV r16, imm16` at Word operand size, the 16-bit campaign's fourth slice.
///
/// Every destination is swept because `home()` is a table lookup: `GUEST_HOMES` is
/// `[R8, R9, R10, R11, R12, R13, R14, RBX]`, so seven of the eight are extended registers and
/// exactly one, EDI's, is not. A lowering that emitted its prefixes in the wrong order writes a
/// DIFFERENT guest register rather than faulting, and the pair that would swap under it is
/// `0xbb` (EBX, R11) and `0xbf` (EDI, RBX). `0xbc` covers R12, the SIB-escape register.
///
/// The seeded high halves are the whole test. `decode` zero-extends the immediate, so with a
/// zero upper half the correct Word lowering and a plain 32-bit move produce identical state and
/// the slice's entire mutation is invisible. The differential compares registers, lazy flags,
/// EFLAGS, core clocks, bus clocks and whole guest RAM, and MOV touches no flags at all, so the
/// register comparison is what carries this.
#[test]
fn word_mov_immediate_matches_the_interpreter_for_every_destination() {
    for dst in 0..8u8 {
        for imm in [0x0000u16, 0x0001, 0x7fff, 0x8000, 0xffff] {
            // ECX and EAX are the two the harness seeds, and both carry a recognisable high half
            // so a widened write shows up whichever of them the destination happens to be.
            let body = [0x66u8, 0xb8 + dst, (imm & 0xff) as u8, (imm >> 8) as u8];
            let context = format!("0xb8+{dst} mov r16,{imm:#06x}");
            differential_full(&body, 0x202, false, 0xdead_beef, 0xfeed_face, &context);
        }
    }
}

/// The ALU REGISTER forms 1 and 3 at Word operand size, the 16-bit campaign's second slice.
///
/// `0x83` above proves the emitter's word lane through an immediate. What is new here is a second
/// register operand, which is one instruction of difference in `emit_alu`, and the operand ROLES,
/// which the two forms assign oppositely: form 1's destination is the r/m and form 3's is the
/// ModRM reg. Getting that backwards is silent for ADD/OR/AND/XOR and wrong for SUB and CMP, so
/// both forms are swept rather than one standing in for the other.
///
/// The three properties `0x83`'s sweep names apply unchanged, and the seeds carry recognisable
/// high halves and sixteen-bit corner low halves for the same reasons. CMP is included as the
/// control: it writes nothing, so a failure on the other five is the write-back and not the
/// operation. ADC (/2) and SBB (/3) joined on the L1 width lift, the same `emit_carry_alu_preloaded`
/// Word lane that admitted `0x83`'s carry sub-ops; see `word_alu_immediate_forms_match_the_
/// interpreter_for_every_admitted_sub_op`'s doc for why the `(eflags, live_pending)` loop below runs
/// THREE pairs rather than two -- `(0x8d7, false)` is the one that actually delivers a live CF=1 to
/// a carry reader, since a live pending descriptor routes CF through itself instead.
#[test]
fn word_alu_register_forms_match_the_interpreter_for_every_admitted_op() {
    // (sub-op, form-1 opcode, form-3 opcode, name).
    let ops: [(u8, u8, u8, &str); 8] = [
        (0, 0x01, 0x03, "add"),
        (1, 0x09, 0x0b, "or"),
        (2, 0x11, 0x13, "adc"),
        (3, 0x19, 0x1b, "sbb"),
        (4, 0x21, 0x23, "and"),
        (5, 0x29, 0x2b, "sub"),
        (6, 0x31, 0x33, "xor"),
        (7, 0x39, 0x3b, "cmp"),
    ];
    // (ECX, EAX). Both halves matter: the high halves must survive on the destination and must
    // NOT leak into the operation from the source, which is what the masks in the word lane are
    // for. The low halves straddle 0x8000 and 0xffff so CF, OF and SF differ between the 16-bit
    // and 32-bit answers rather than agreeing by luck.
    let seeds: [(u32, u32); 5] = [
        (0xdead_0000, 0xbeef_0001),
        (0xdead_7fff, 0xbeef_0001),
        (0xdead_8000, 0xbeef_ffff),
        (0xdead_ffff, 0xbeef_8000),
        (0xdead_0001, 0xbeef_ffff),
    ];

    for (op, form1, form3, name) in ops {
        for (ecx, eax) in seeds {
            for (eflags, live_pending) in [(0x202u32, false), (0x8d5, true), (0x8d7, false)] {
                // Form 1 is `op r/m16, r16`: ModRM r/m = CX is the destination, reg = AX.
                let body = [0x66u8, form1, 0xc1];
                let context = format!(
                    "{form1:#04x} /{op} {name} cx,ax ecx={ecx:#010x} eax={eax:#010x} \
                     eflags={eflags:#x} pending={live_pending}"
                );
                differential_full(&body, eflags, live_pending, ecx, eax, &context);

                // Form 3 is `op r16, r/m16`: ModRM reg = CX is the destination, r/m = AX. The
                // same registers in the same roles as form 1, so a role mix-up shows as the two
                // forms disagreeing rather than as both being wrong the same way.
                let body = [0x66u8, form3, 0xc8];
                let context = format!(
                    "{form3:#04x} /{op} {name} cx,ax ecx={ecx:#010x} eax={eax:#010x} \
                     eflags={eflags:#x} pending={live_pending}"
                );
                differential_full(&body, eflags, live_pending, ecx, eax, &context);

                // Destination aliased to source, the case only the register forms have. `xor
                // cx,cx` and `sub cx,cx` are the idioms that actually appear in 16-bit code, and
                // a lane that staged the operand through a scratch register would still pass the
                // unaliased rows above.
                let body = [0x66u8, form1, 0xc9];
                let context = format!(
                    "{form1:#04x} /{op} {name} cx,cx aliased ecx={ecx:#010x} \
                     eflags={eflags:#x} pending={live_pending}"
                );
                differential_full(&body, eflags, live_pending, ecx, eax, &context);
            }
        }
    }
}
