// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The rejected-row campaign's Slice 7: the THREE-OPERAND IMUL (0x69/0x6B, register source).
//!
//! Every row runs the same guest bytes natively and through a block-free interpreter from
//! identical state and compares registers (EIP included), lazy flags, EFLAGS, the halt latch,
//! core clocks, bus clocks and the whole of guest RAM. The tested opcode is MID-BLOCK on every
//! row: an opcode at a block's entry slot parks the block on the interpreter, so an
//! entry-position fixture certifies nothing.
//!
//! What this file has to prove that a plain differential does not: **the three-operand IMUL needs
//! TWO changes to lower**, and either alone is inert. The compile walk refuses a non-continuable
//! instruction before `classify` is consulted, so the classify arm is dead without
//! `jit_admits_non_continuable`; and the admission alone just moves the census row from the
//! `non_continuable` arm to `hard_boundary`. `build` asserting three retired slots is what fails
//! if either half is reverted.

use super::*;

/// `mov esi,esi`, the leading slot that keeps the tested opcode off the block entry.
const LEAD: [u8; 2] = [0x89, 0xf6];
/// `mov edi,edi`, the trailing slot, so the tested opcode is never the last one either.
const TAIL: [u8; 2] = [0x89, 0xff];

/// The seeded architectural state a row starts BOTH roles from. `gpr` is written verbatim, so a
/// row picks its own byte lanes; ESP is overwritten with `STACK_TOP` afterwards because a
/// stack-touching slot resolves its store page at compile time.
#[derive(Clone, Copy)]
struct Seed {
    gpr: [u32; 8],
    eflags: u32,
    live_pending: bool,
}

impl Seed {
    fn new(gpr: [u32; 8]) -> Self {
        Self {
            gpr,
            eflags: 0x202,
            live_pending: false,
        }
    }

    fn flags(mut self, eflags: u32) -> Self {
        self.eflags = eflags;
        self
    }

    fn pending(mut self) -> Self {
        self.live_pending = true;
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

/// Compile `mov esi,esi / body / mov edi,edi / hlt` at `ENTRY` on the native role, warm the same
/// decode lines on the interpreter role, and seed both identically.
///
/// The `StructuralReject` panic is the load-bearing assertion for the IMUL half of this slice:
/// before the compile walk admitted 0x69/0x6B, a block containing one stopped at that instruction
/// with a two-slot prefix, which is under the `slots.len() < 3 && !terminal` minimum and is
/// therefore returned as a structural reject rather than a short block.
fn build(body: &[u8], seed: Seed) -> Roles {
    let mut code = LEAD.to_vec();
    let body_at = ENTRY + code.len() as u32;
    code.extend_from_slice(body);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&TAIL);
    code.push(0xf4);

    let mut memory = vec![0u8; 0x5000];
    // A NOP before the entry, so the block is reachable as a continuation as well as directly.
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

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
    }

    let key = jit::direct::key_for(&native, ENTRY, true).expect("entry key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut native, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("structurally rejected: the form is still a barrier")
        }
        jit::direct::CompileOutcome::Retry => panic!("compile asked for a retry"),
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
            // A descriptor produced BEFORE the tested instruction. IMUL must materialize and
            // clear it (`set_flag(CF|OF, ..)` cannot take the single-bit shortcut); the byte ALU
            // forms must REPLACE it with a byte-width descriptor of their own.
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

/// Run all three slots natively, step the interpreter three times, and compare everything.
fn differential(body: &[u8], seed: Seed, context: &str) {
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
    assert_eq!(
        roles.native.registers, roles.interp.registers,
        "{context}: registers"
    );
    assert_eq!(
        roles.native.pending_flags, roles.interp.pending_flags,
        "{context}: lazy flags"
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

/// Assert `body` is still a compile barrier mid-block: the walk stops at it with a two-slot
/// prefix, which is under the three-slot minimum, so the entry comes back as a structural reject.
fn still_a_barrier(body: &[u8], context: &str) {
    let mut code = LEAD.to_vec();
    let body_at = ENTRY + code.len() as u32;
    code.extend_from_slice(body);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&TAIL);
    code.push(0xf4);

    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    cpu.registers.set_esp(STACK_TOP);
    for &linear in &[ENTRY, body_at, tail_at] {
        cpu.set_eip(linear);
        cpu.fetch_decoded(&mut bus, linear).unwrap();
    }
    assert!(
        matches!(
            jit::direct::compile(&mut cpu, ENTRY, true),
            jit::direct::CompileOutcome::StructuralReject(_)
        ),
        "{context}: expected the form to remain a barrier"
    );
}

// ---------------------------------------------------------------------------
// Three-operand IMUL (0x69 / 0x6B), register source.
// ---------------------------------------------------------------------------

/// `IMUL reg, rm, imm32` — 69 /r id, ModRM mod = 0b11.
fn imul_imm32(reg: u8, rm: u8, imm: u32) -> Vec<u8> {
    let mut bytes = vec![0x69, 0b1100_0000 | (reg << 3) | rm];
    bytes.extend_from_slice(&imm.to_le_bytes());
    bytes
}

/// `IMUL reg, rm, imm8` — 6B /r ib, ModRM mod = 0b11. The immediate is SIGN-EXTENDED by decode.
fn imul_imm8(reg: u8, rm: u8, imm: u8) -> Vec<u8> {
    vec![0x6b, 0b1100_0000 | (reg << 3) | rm, imm]
}

/// The register seed the IMUL rows share: distinct, non-zero, and index 4 (ESP) left alone
/// because `build` overwrites it with `STACK_TOP`.
fn imul_seed(dst_value: u32, src_value: u32, dst: u8, src: u8) -> Seed {
    let mut gpr = [0x1111_1111u32; 8];
    gpr[usize::from(dst)] = dst_value;
    gpr[usize::from(src)] = src_value;
    Seed::new(gpr)
}

#[test]
fn three_operand_imul_imm32_matches_the_interpreter() {
    // Values chosen so CF/OF land on both sides of the "does the truncated result sign-extend
    // back to the full product" rule: 3 * 7 fits, 0x0001_0000 * 0x0001_0000 does not, and the
    // negative cases exercise the SIGNED truncation the unsigned MUL rule would get wrong.
    for (dst_value, src_value, imm, name) in [
        (0u32, 3u32, 7u32, "small positive"),
        (0, 0x0001_0000, 0x0001_0000, "overflow, both positive"),
        (0, 0xffff_ffff, 0xffff_ffff, "(-1) * (-1)"),
        (0, 0xffff_ffff, 2, "(-1) * 2"),
        (0, 0x8000_0000, 0xffff_ffff, "i32::MIN * -1, overflow"),
        (0, 0, 0x1234_5678, "zero multiplicand"),
        (0, 0x7fff_ffff, 1, "identity at i32::MAX"),
    ] {
        for (dst, src) in [(0u8, 3u8), (3, 0), (5, 1), (1, 5), (2, 2)] {
            for pending in [false, true] {
                let mut seed = imul_seed(dst_value, src_value, dst, src);
                if pending {
                    seed = seed.pending();
                }
                differential(
                    &imul_imm32(dst, src, imm),
                    seed,
                    &format!("0x69 {name} dst={dst} src={src} pending={pending}"),
                );
            }
        }
    }
}

#[test]
fn three_operand_imul_imm8_sign_extends_its_immediate() {
    // 0x6B's immediate is sign-extended by `decode`, so 0xFF is -1 and 0x80 is -128. A lowering
    // that baked the raw byte would agree on every non-negative case and diverge on these.
    for (src_value, imm, name) in [
        (5u32, 0x03u8, "5 * 3"),
        (5, 0xff, "5 * -1"),
        (0xffff_fffb, 0xff, "-5 * -1"),
        (0x0100_0000, 0x80, "overflow via -128"),
        (0, 0x7f, "zero multiplicand"),
        (1, 0x80, "1 * -128"),
    ] {
        for (dst, src) in [(0u8, 3u8), (7, 7), (5, 6)] {
            for pending in [false, true] {
                let mut seed = imul_seed(0xdead_beef, src_value, dst, src);
                if pending {
                    seed = seed.pending();
                }
                differential(
                    &imul_imm8(dst, src, imm),
                    seed,
                    &format!("0x6B {name} dst={dst} src={src} pending={pending}"),
                );
            }
        }
    }
}

#[test]
fn three_operand_imul_preserves_the_flags_it_does_not_define() {
    // IMUL defines CF and OF and leaves SF/ZF/AF/PF exactly as they were. Seeding EFLAGS with
    // those four SET and then multiplying a value that clears CF/OF is what catches a lowering
    // that published the host's whole flag word instead of two bits of it.
    for seed_eflags in [0x202u32, 0x2d7, 0x8d7] {
        for (src_value, imm) in [(3u32, 7u32), (0x0001_0000, 0x0001_0000)] {
            differential(
                &imul_imm32(0, 3, imm),
                imul_seed(0, src_value, 0, 3).flags(seed_eflags),
                &format!("0x69 eflags={seed_eflags:#x} src={src_value:#x} imm={imm:#x}"),
            );
        }
    }
}

#[test]
fn three_operand_imul_memory_form_is_still_a_barrier() {
    // `IMUL eax, [ebx], 7` — 69 /r with mod = 0b00. The register arm must not have widened into
    // the memory form: `ImulMemImm` does not exist and the `else` has to return None.
    let mut body = vec![0x69, 0b0000_0011];
    body.extend_from_slice(&7u32.to_le_bytes());
    still_a_barrier(&body, "0x69 memory form");
    still_a_barrier(&[0x6b, 0b0000_0011, 0x07], "0x6B memory form");
}

#[test]
fn sixteen_bit_three_operand_imul_is_still_a_barrier() {
    // `66 69 /r iw`. 0x69/0x6B are absent from `classify`'s OperandSize::Word allowlist, so the
    // prefixed encoding falls to None there rather than reaching the register arm and being
    // lowered as a 32-bit multiply that clobbers the destination's high half.
    still_a_barrier(&[0x66, 0x69, 0b1100_0011, 0x07, 0x00], "66 0x69 register");
    still_a_barrier(&[0x66, 0x6b, 0b1100_0011, 0x07], "66 0x6B register");
}

#[test]
fn the_non_continuable_admission_is_narrow() {
    // The predicate admits 0x69/0x6B and nothing else. These three are the other shapes the
    // Slice 5 census named at that arm, and each must stay refused for its own reason: IRET
    // loads CS, OUT sets `io_touched`, HLT stops the machine.
    still_a_barrier(&[0xcf], "0xCF IRETD");
    still_a_barrier(&[0xee], "0xEE OUT DX,AL");
    still_a_barrier(&[0xf4], "0xF4 HLT");
}
