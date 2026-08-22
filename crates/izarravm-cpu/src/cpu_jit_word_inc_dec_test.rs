// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The sixteen-bit INC/DEC lazy-flag DESCRIPTOR, the survivor `cpu_jit_word_memory_test.rs`'s
//! header recorded and left for its own change.
//!
//! `emit_pending_inc_dec`'s Word `width_tag` -> `0x200` passed the WHOLE crate. Nothing in the tree
//! ran a sixteen-bit INC or DEC through the direct JIT at all, so the descriptor it builds was
//! unmeasured. This file measures it.
//!
//! **The emitted code was CORRECT, not wrong.** The tag's bits 8-15 are the `BusWidth`
//! discriminant that `PendingFlags::width` decodes (0 Byte, 1 Word, 2 Dword), `from_legacy` packs
//! `BusWidth::Word` as 1, and the interpreter's `inc_dec` builds its `LazyFlags` with the
//! instruction's own `BusWidth`. So a sixteen-bit INC interprets to tag bits 8-15 = 1 and the
//! emitter's Word arm emits `0x100` = 1. Same for `a` (the operand masked to the width), `b` (1),
//! `result` (masked) and the CF override (INC/DEC preserve CF). This is a pure test addition; no
//! emitted byte changes.
//!
//! Two production-reachable forms reach the Word arm, and both are on classify's `OperandSize::Word`
//! allowlist:
//!
//! | form | encoding | kind |
//! |---|---|---|
//! | INC/DEC m16 | `66 FF /0`, `66 FF /1` | `RmwIncDec { width: Word }` |
//! | INC/DEC r16 | `66 40`..`66 4F` | `IncDecReg { width: Word }` |
//!
//! ## Which flags the width actually moves, and why the operand values are chosen
//!
//! `materialized_eflags` takes `mask` and `sign` from `width()`. Both emitters mask `a` and
//! `result` to sixteen bits BEFORE storing them, so under a Dword tag:
//!
//! * **ZF is unmoved.** `result & 0xffff_ffff == 0` and `result & 0xffff == 0` agree on an
//!   already-masked result.
//! * **AF is unmoved.** It is `(a ^ b ^ result) & 0x10`, which has no width in it.
//! * **PF is unmoved.** Low byte only.
//! * **CF is unmoved.** INC/DEC carry a CF override, and `cf_override` wins over the computed CF.
//! * **SF MOVES.** Correct is bit 15; a Dword tag reads bit 31 of a value that is masked to
//!   sixteen bits, so SF is stuck at 0.
//! * **OF MOVES.** Same `sign` in both the Add and the Sub expression, so a Dword tag pins OF at 0.
//!
//! So a row whose result has bit 15 clear and no signed overflow cannot see this mutation however
//! it is compared. The operand table below is built around the two axes that can:
//! `0x7fff` INC (SF and OF both flip), `0x8fff` INC (SF alone), `0x8000` DEC (OF alone), `0x0000`
//! DEC (SF alone), against `0xffff` INC and `0x0001` DEC as the ZF-only controls that must NOT move.
//!
//! ## Why the descriptor has to be CONSUMED, and by whom
//!
//! A flag reader INSIDE the block proves nothing. `emit_load_host_flags` reads RBP, the running
//! materialized shadow that `emit_capture_flags` filled from the HOST flags of the sixteen-bit
//! `alu_r16_r16` -- correct whatever the descriptor says. A natively lowered `SETcc` after a
//! natively lowered INC therefore agrees with the interpreter under the mutation as well as
//! without it.
//!
//! The descriptor is authoritative for whoever reads it AFTER the block: the interpreter. That is
//! the production shape (a block exits with a live descriptor and the next instruction is
//! interpreted), and it is what `the_descriptor_a_word_*_leaves_behind_*` exercises -- the readers
//! sit at their own address and BOTH roles step them interpreted, so the native role's descriptor
//! is the one driving its `SETO`/`SETS`.
//!
//! The two pairs of tests are deliberately split so the mutation fails on two different KINDS of
//! assertion: `..._matches_the_interpreter` compares the architectural flag word at the block exit,
//! `..._leaves_behind_...` compares nothing flag-shaped until an interpreted instruction has read
//! the descriptor, so its failure is a guest-visible BYTE.
//!
//! ## Mutation record
//!
//! `emit_pending_inc_dec`'s Word `width_tag` -> `0x200`, applied by hand, run against the WHOLE
//! crate (never a filter -- a filtered run is what let this survive in the first place), and
//! restored. **4 failed, 1324 passed**, and the 1324 is exactly the pre-addition baseline, so the
//! blast radius is these four tests and nothing else in the crate.
//!
//! All four fail first on the `0x7fff` INC row, which is the one that moves SF and OF together.
//! Verbatim, the first assertion each produced:
//!
//! ```text
//! the_word_memory_inc_dec_descriptor_matches_the_interpreter
//! assertion `left == right` failed: inc word [0x3010] operand=0x7fff cf=0 pending=false: EFLAGS
//!   left: 534
//!  right: 2710
//!
//! the_word_register_inc_dec_descriptor_matches_the_interpreter
//! assertion `left == right` failed: inc r16 dst=0 operand=0x7fff cf=0 pending=false: EFLAGS
//!   left: 534
//!  right: 2710
//!
//! the_descriptor_a_word_memory_inc_dec_leaves_behind_drives_the_next_flag_reader
//! assertion `left == right` failed: inc word [0x3010] operand=0x7fff cf=0 -> readers: after the
//! readers: registers
//!
//! the_descriptor_a_word_register_inc_dec_leaves_behind_drives_the_next_flag_reader
//! assertion `left == right` failed: inc r16 dst=0 operand=0x7fff cf=0 -> readers: after the
//! readers: registers
//! ```
//!
//! 534 is `0x216` and 2710 is `0xa96`: the interpreter has SF (bit 7) and OF (bit 11) and the
//! mutated native role has neither, with PF, AF and IF identical in both. That is the prediction
//! above, confirmed -- and note that CF and ZF do NOT appear, which is why a row chosen for ZF
//! would have passed under the mutation.
//!
//! The two `registers` failures are the same fact one instruction later: EBX and EDX come back
//! `0xdead_0000` on the native role against `0xdead_0001` on the interpreter, i.e. `seto bl` and
//! `sets dl` each wrote a 0 where the guest defines a 1.

use super::*;

/// `mov esi,esi`, the leading slot that keeps the tested opcode off the block entry.
const MOV_ESI_ESI: [u8; 2] = [0x89, 0xf6];
/// `mov edi,edi`, the trailing slot, so the tested opcode is never the last one either.
const MOV_EDI_EDI: [u8; 2] = [0x89, 0xff];

/// The word operand's linear address: 2-ALIGNED (an odd one side-exits on the alignment guard and
/// the row would then certify the interpreter), on a page of its own, inside the fixture's RAM.
const OPERAND: u32 = 0x3010;

/// Where the interpreted flag readers live. Its own address rather than a tail slot of the block,
/// because `SETcc` IS lowered: appended after `mov edi,edi` it would join the block and read RBP
/// instead of the descriptor, which is exactly the reading that cannot see this bug.
const READER: u32 = 0x2000;

/// `seto bl` then `sets dl`: the two flags a wrong descriptor width moves, landed in two byte
/// registers that no row's INC/DEC target touches.
const READERS: [u8; 6] = [0x0f, 0x90, 0xc3, 0x0f, 0x98, 0xc2];

/// Register indices the reader bytes do not collide with: EAX, EBP, ESI, EDI. EBX and EDX are out
/// because `seto bl`/`sets dl` write into them; ESP is out because the fixture owns it; ECX is left
/// out with EBX and EDX so the four survivors are a clean set.
const READER_SAFE_DSTS: [u8; 4] = [0, 5, 6, 7];

/// A distinct byte at every address, so a wrong-WIDTH access changes guest RAM even when it writes
/// the right value at the right place.
fn memory_fill() -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    for (i, byte) in memory.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37).wrapping_add(11) & 0xff) as u8;
    }
    memory
}

/// The architectural state both roles start from. `gpr` poisons every register's HIGH half: a
/// sixteen-bit INC/DEC defines the low 16 bits and preserves what is above them.
#[derive(Clone, Copy)]
struct Seed {
    gpr: [u32; 8],
    eflags: u32,
    live_pending: bool,
    /// Written as two little-endian bytes at `OPERAND` before the run, over the fill.
    operand: u16,
}

impl Seed {
    fn new() -> Self {
        Self {
            gpr: std::array::from_fn(|i| 0xdead_0000 | (0xa0 + i as u32)),
            eflags: 0x202,
            live_pending: false,
            operand: 0x1234,
        }
    }

    fn gpr(mut self, index: usize, value: u32) -> Self {
        self.gpr[index] = value;
        self
    }

    fn flags(mut self, eflags: u32) -> Self {
        self.eflags = eflags;
        self
    }

    fn pending(mut self) -> Self {
        self.live_pending = true;
        self
    }

    fn operand(mut self, operand: u16) -> Self {
        self.operand = operand;
        self
    }
}

struct Roles {
    native: CpuGsw,
    native_bus: TestBus,
    interp: CpuGsw,
    interp_bus: TestBus,
    block: jit::direct::CompiledBlock,
}

/// Map one page for read and write on the fast map. A memory-form slot silently never compiles
/// without this and the fixture then certifies a refusal it did not intend to test.
fn map_page(cpu: &mut CpuGsw, bus: &mut TestBus, page: u32) {
    for write in [false, true] {
        let kind = if write {
            BusAccessKind::DataWrite
        } else {
            BusAccessKind::DataRead
        };
        let host = bus.direct_page(page, kind).unwrap().unwrap();
        let ok = if write {
            cpu.jit_fast_map.populate_write(
                page,
                page,
                host,
                jit::fast_map::PagePermissions::UNPAGED,
                cpu.physical_page_watched(page),
            )
        } else {
            cpu.jit_fast_map.populate_read(
                page,
                page,
                host,
                jit::fast_map::PagePermissions::UNPAGED,
                cpu.physical_page_watched(page),
            )
        };
        assert!(ok, "page {page:#x} must map");
    }
}

/// Compile `mov esi,esi / body / mov edi,edi / hlt` at `ENTRY` on the native role, warm the same
/// decode lines on the interpreter role, plant the flag readers at `READER`, and seed both roles
/// identically.
fn build(body: &[u8], seed: Seed) -> Roles {
    let mut code = MOV_ESI_ESI.to_vec();
    let body_at = ENTRY + code.len() as u32;
    code.extend_from_slice(body);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&MOV_EDI_EDI);
    code.push(0xf4);

    let mut memory = memory_fill();
    // A NOP before the entry, so the block is reachable as a continuation as well as directly.
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[READER as usize..READER as usize + READERS.len()].copy_from_slice(&READERS);
    memory[READER as usize + READERS.len()] = 0xf4;
    memory[OPERAND as usize..OPERAND as usize + 2].copy_from_slice(&seed.operand.to_le_bytes());

    let mut native = flat_cpu();
    let mut interp = flat_cpu();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, body_at, tail_at];
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interp, &mut interp_bus),
    ] {
        cpu.registers.set_esp(STACK_TOP);
        for &linear in &starts {
            cpu.set_eip(linear);
            cpu.fetch_decoded(bus, linear).unwrap();
        }
        map_page(cpu, bus, OPERAND & !0xfff);
        map_page(cpu, bus, (STACK_TOP - 4) & !0xfff);
    }

    let key = jit::direct::key_for(&native, ENTRY, true).expect("entry key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut native, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("structurally rejected: the sixteen-bit INC/DEC form is still a barrier")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, 3,
        "the block must cover all three slots, so the tested opcode really ran natively"
    );
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = native.jit_direct.block(id).expect("live block");

    for cpu in [&mut native, &mut interp] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr = seed.gpr;
        cpu.registers.set_esp(STACK_TOP);
        cpu.registers.eflags = seed.eflags;
        cpu.pending_flags = PendingFlags::default();
        if seed.live_pending {
            // A DWORD descriptor produced BEFORE the tested instruction. INC/DEC must REPLACE it
            // with a word one; a lowering that left it alone would keep this dword tag and the
            // comparison would see it.
            let _ = cpu.alu(0, 0x7fff_ffff, 1, BusWidth::Dword);
        }
        cpu.set_eip(ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    native_bus.trace = BusTrace::default();
    interp_bus.trace = BusTrace::default();

    Roles {
        native,
        native_bus,
        interp,
        interp_bus,
        block,
    }
}

/// Everything a guest can observe, plus the raw descriptor.
fn compare_state(roles: &Roles, context: &str) {
    compare_observable(roles, context);
    assert_eq!(
        roles.native.pending_flags, roles.interp.pending_flags,
        "{context}: lazy flags"
    );
}

/// Everything a guest can observe. Excludes the raw `pending_flags` on purpose: the consumption
/// test wants the divergence to arrive as a written byte, not as a descriptor field, so that it
/// says the width is guest-visible rather than merely stored.
fn compare_observable(roles: &Roles, context: &str) {
    assert_eq!(
        roles.native.registers, roles.interp.registers,
        "{context}: registers"
    );
    assert_eq!(
        roles.native.eflags(),
        roles.interp.eflags(),
        "{context}: EFLAGS"
    );
    assert_eq!(
        roles.native.halted, roles.interp.halted,
        "{context}: halt latch"
    );
    assert_eq!(
        roles.native.elapsed_clocks, roles.interp.elapsed_clocks,
        "{context}: core clocks"
    );
    assert_eq!(
        roles.native_bus.trace.elapsed_clocks(),
        roles.interp_bus.trace.elapsed_clocks(),
        "{context}: bus clocks"
    );
    assert_eq!(
        roles.native_bus.memory, roles.interp_bus.memory,
        "{context}: guest RAM"
    );
}

/// Run the block natively, step the interpreter role three times, compare EVERYTHING.
fn lowered(body: &[u8], seed: Seed, context: &str) {
    let roles = run_block(body, seed, context);
    compare_state(&roles, context);
}

fn run_block(body: &[u8], seed: Seed, context: &str) -> Roles {
    let mut roles = build(body, seed);
    let retired = roles.native.perf_counters().jit_direct_insns;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "{context}: block did not run natively"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - retired,
        3,
        "{context}: all three slots must retire natively"
    );
    for _ in 0..3 {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    roles
}

/// Run the block natively, then make BOTH roles step the flag readers INTERPRETED and compare what
/// the guest can see. The native role's reader evaluates the descriptor the emitted code built, so
/// a wrong width lands in BL/DL.
fn consumed(body: &[u8], seed: Seed, context: &str) {
    let mut roles = run_block(body, seed, context);
    // Registers and RAM only, on purpose: NOTHING flag-shaped is compared before the readers run.
    // `eflags()` materializes the descriptor and `pending_flags` is the descriptor, so comparing
    // either here would intercept a wrong width at the exit and this test would never reach the
    // instruction whose job is to prove the width is guest-visible. The full exit comparison for
    // these same rows lives in `..._descriptor_matches_the_interpreter`.
    assert_eq!(
        roles.native.registers, roles.interp.registers,
        "{context}: at the exit: registers"
    );
    assert_eq!(
        roles.native_bus.memory, roles.interp_bus.memory,
        "{context}: at the exit: guest RAM"
    );

    let retired = roles.native.perf_counters().jit_direct_insns;
    for cpu in [&mut roles.native, &mut roles.interp] {
        cpu.set_eip(READER);
    }
    for _ in 0..2 {
        roles.native.cycle(&mut roles.native_bus).unwrap();
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    // The readers must be INTERPRETED on both roles. A natively lowered SETcc reads RBP, the
    // running flag shadow, not the descriptor, and would make this test vacuous without saying so.
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - retired,
        0,
        "{context}: the flag readers must not run natively"
    );
    compare_observable(&roles, &format!("{context}: after the readers"));
}

/// `[disp32]` addressing: ModRM mod 00, rm 101. No base register, so no row's operand address can
/// depend on the poisoned register seeds.
fn word_rmw(is_dec: bool) -> Vec<u8> {
    let mut body = vec![0x66, 0xff];
    body.push(((if is_dec { 1u8 } else { 0 }) << 3) | 0b101);
    body.extend_from_slice(&OPERAND.to_le_bytes());
    body
}

fn word_reg(is_dec: bool, dst: u8) -> Vec<u8> {
    vec![0x66, (if is_dec { 0x48 } else { 0x40 }) + dst]
}

/// The operand values, paired with what each is there to move. The first four are the ONLY shapes
/// a wrong descriptor width can reach (SF from bit 15, OF from the signed edge); the rest are the
/// controls that must stay put.
fn operands(is_dec: bool) -> &'static [u16] {
    if is_dec {
        // 0x8000 -> 0x7fff is OF alone; 0x0000 -> 0xffff is SF alone (and AF, and CF preserved);
        // 0x0001 -> 0x0000 is the ZF control; 0x8001, 0x1234 and 0x0100 fill in the AF and
        // borrow-free shapes.
        &[0x8000, 0x0000, 0x0001, 0x8001, 0x1234, 0x0100]
    } else {
        // 0x7fff -> 0x8000 moves SF and OF together; 0x8fff -> 0x9000 is SF alone with a nibble
        // carry; 0xffff -> 0x0000 is the ZF control; 0xfffe, 0x1234 and 0x00ff fill in the rest.
        &[0x7fff, 0x8fff, 0xffff, 0xfffe, 0x1234, 0x00ff]
    }
}

// ---------------------------------------------------------------------------------------------
// `66 FF /0` and `66 FF /1` -- INC/DEC m16
// ---------------------------------------------------------------------------------------------

/// Under the `0x200` mutation this fails first at
/// `inc word [0x3010] operand=0x7fff cf=0 pending=false: EFLAGS`, 534 against 2710.
#[test]
fn the_word_memory_inc_dec_descriptor_matches_the_interpreter() {
    for is_dec in [false, true] {
        let body = word_rmw(is_dec);
        let name = if is_dec { "dec" } else { "inc" };
        for &operand in operands(is_dec) {
            // Both CF polarities: INC/DEC preserve CF through the descriptor's override, which is
            // a different tag field from the width and would otherwise ride on one value.
            for (eflags, cf) in [(0x202u32, 0), (0x203, 1)] {
                for pending in [false, true] {
                    let mut seed = Seed::new().operand(operand).flags(eflags);
                    if pending {
                        seed = seed.pending();
                    }
                    lowered(
                        &body,
                        seed,
                        &format!(
                            "{name} word [{OPERAND:#x}] operand={operand:#06x} cf={cf} pending={pending}"
                        ),
                    );
                }
            }
        }
    }
}

/// Under the `0x200` mutation this fails first at
/// `inc word [0x3010] operand=0x7fff cf=0 -> readers: after the readers: registers`, with the
/// native role's EBX and EDX holding `0xdead_0000` (SETO and SETS each wrote a 0) against the
/// interpreter's `0xdead_0001`.
#[test]
fn the_descriptor_a_word_memory_inc_dec_leaves_behind_drives_the_next_flag_reader() {
    for is_dec in [false, true] {
        let body = word_rmw(is_dec);
        let name = if is_dec { "dec" } else { "inc" };
        for &operand in operands(is_dec) {
            for (eflags, cf) in [(0x202u32, 0), (0x203, 1)] {
                consumed(
                    &body,
                    Seed::new().operand(operand).flags(eflags),
                    &format!(
                        "{name} word [{OPERAND:#x}] operand={operand:#06x} cf={cf} -> readers"
                    ),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// `66 40`..`66 4F` -- INC/DEC r16
// ---------------------------------------------------------------------------------------------

/// Every destination except ESP, which the fixture owns. The high half stays poisoned on both
/// roles, so a lowering that widened the arithmetic to 32 bits diverges on the register as well as
/// on the descriptor.
///
/// Under the `0x200` mutation this fails first at
/// `inc r16 dst=0 operand=0x7fff cf=0 pending=false: EFLAGS`, 534 against 2710.
#[test]
fn the_word_register_inc_dec_descriptor_matches_the_interpreter() {
    for is_dec in [false, true] {
        let name = if is_dec { "dec" } else { "inc" };
        for dst in [0u8, 1, 2, 3, 5, 6, 7] {
            let body = word_reg(is_dec, dst);
            for &operand in operands(is_dec) {
                for (eflags, cf) in [(0x202u32, 0), (0x203, 1)] {
                    for pending in [false, true] {
                        let mut seed = Seed::new()
                            .gpr(usize::from(dst), 0xdead_0000 | u32::from(operand))
                            .flags(eflags);
                        if pending {
                            seed = seed.pending();
                        }
                        lowered(
                            &body,
                            seed,
                            &format!(
                                "{name} r16 dst={dst} operand={operand:#06x} cf={cf} pending={pending}"
                            ),
                        );
                    }
                }
            }
        }
    }
}

/// Under the `0x200` mutation this fails first at
/// `inc r16 dst=0 operand=0x7fff cf=0 -> readers: after the readers: registers`, EBX and EDX again.
#[test]
fn the_descriptor_a_word_register_inc_dec_leaves_behind_drives_the_next_flag_reader() {
    for is_dec in [false, true] {
        let name = if is_dec { "dec" } else { "inc" };
        for dst in READER_SAFE_DSTS {
            let body = word_reg(is_dec, dst);
            for &operand in operands(is_dec) {
                for (eflags, cf) in [(0x202u32, 0), (0x203, 1)] {
                    consumed(
                        &body,
                        Seed::new()
                            .gpr(usize::from(dst), 0xdead_0000 | u32::from(operand))
                            .flags(eflags),
                        &format!("{name} r16 dst={dst} operand={operand:#06x} cf={cf} -> readers"),
                    );
                }
            }
        }
    }
}
