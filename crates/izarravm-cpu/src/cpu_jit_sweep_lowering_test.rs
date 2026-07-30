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
            jit::fast_map::PagePermissions::UNPAGED
        ));
        let write = bus
            .direct_page(page, BusAccessKind::DataWrite)
            .unwrap()
            .unwrap();
        assert!(cpu.jit_fast_map.populate_write(
            page,
            page,
            write,
            jit::fast_map::PagePermissions::UNPAGED
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
        jit::direct::CompileOutcome::Retry => panic!("{context}: compile asked for a retry"),
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
        native.registers, interpreter.registers,
        "{context}: registers"
    );
    assert_eq!(
        native.pending_flags, interpreter.pending_flags,
        "{context}: lazy flags"
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
    const TARGET: u32 = 0x3f00;
    for op in 0u8..8 {
        for lane in [1u8, 5] {
            for ecx in [0x0000_0000u32, 0x0000_017f, 0x0000_ab01, 0xffff_ffff] {
                for seed in [0x202u32, 0x8d7] {
                    let modrm = (lane << 3) | 0b101;
                    let mut body = vec![op << 3, modrm];
                    body.extend_from_slice(&TARGET.to_le_bytes());
                    let context =
                        format!("alu form0 mem op={op} lane={lane} ecx={ecx:#x} seed={seed:#x}");
                    differential_with(&body, seed, true, ecx, &context);
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
