// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The rejected-row campaign's Slice 7: the THREE-OPERAND IMUL (0x69/0x6B, register source) and
//! the BYTE-LANE REGISTER ALU (`op r8, r8`, both operand orders, all eight operations).
//!
//! Every row runs the same guest bytes natively and through a block-free interpreter from
//! identical state and compares registers (EIP included), lazy flags, EFLAGS, the halt latch,
//! core clocks, bus clocks and the whole of guest RAM. The tested opcode is MID-BLOCK on every
//! row: an opcode at a block's entry slot parks the block on the interpreter, so an
//! entry-position fixture certifies nothing.
//!
//! Two things this file has to prove that a plain differential does not:
//!
//! * **The three-operand IMUL needs TWO changes to lower**, and either alone is inert. The
//!   compile walk refuses a non-continuable instruction before `classify` is consulted, so the
//!   classify arm is dead without `jit_admits_non_continuable`; and the admission alone just
//!   moves the census row from the `non_continuable` arm to `hard_boundary`. `build` asserting
//!   three retired slots is what fails if either half is reverted.
//! * **The byte lane is a different register file.** `dst`/`src` 4..=7 are AH/CH/DH/BH, the high
//!   byte of the first four registers, which x86-64 cannot name alongside a REX prefix. The
//!   high-byte rows below are the ones that fail if the emitter ever reaches its operands through
//!   `home(index)` — index 5 is the host register holding guest EBP, so `cmp al, ch` would
//!   compare against the wrong register at the wrong width and, for a writing op, corrupt EBP.
//!
//! The aliasing rows (`add al, ah`, `xor ch, ch`, `sub bl, bl`) are not decoration: two byte
//! lanes of one 32-bit home, and one lane named twice. They are what fails if the emitter ever
//! writes the destination before it has read the source.

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
    slots: usize,
}

/// Compile `mov esi,esi / body / mov edi,edi / hlt` at `ENTRY` on the native role, warm the same
/// decode lines on the interpreter role, and seed both identically.
///
/// The `StructuralReject` panic is the load-bearing assertion for the IMUL half of this slice:
/// before the compile walk admitted 0x69/0x6B, a block containing one stopped at that instruction
/// with a two-slot prefix, which is under the `slots.len() < 3 && !terminal` minimum and is
/// therefore returned as a structural reject rather than a short block.
fn build(body: &[u8], seed: Seed) -> Roles {
    build_n(&[body], seed)
}

/// `build` for a body of SEVERAL instructions, each given separately so the decode lines can be
/// warmed at every start. `bodies.len() + 2` slots are expected in the compiled block.
///
/// This exists for the CLOCK CHARGE and nothing else. A raw-clock error inside one slot is
/// invisible to a three-slot differential: the 586 dial divides the block's raw total by twelve
/// and floors, so `2 + 14 + 2 = 18` and `2 + 9 + 2 = 13` are the same scaled clock. It takes
/// accumulation to separate them, which is the lesson `cpu_jit_callout_matrix_test.rs` recorded
/// when the Phase 5 call-out shipped a two-clock double-charge that every single-slot fixture
/// agreed with. Four IMUL slots put 60 raw against 40, which is 5 scaled clocks against 3.
fn build_n(bodies: &[&[u8]], seed: Seed) -> Roles {
    let mut code = LEAD.to_vec();
    let mut starts = vec![ENTRY];
    for body in bodies {
        starts.push(ENTRY + code.len() as u32);
        code.extend_from_slice(body);
    }
    starts.push(ENTRY + code.len() as u32);
    code.extend_from_slice(&TAIL);
    code.push(0xf4);
    let slots = bodies.len() + 2;

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
        usize::from(compilation.span.instructions),
        slots,
        "the block must cover every slot, so the tested opcode really ran natively"
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
        slots,
    }
}

/// Run every slot natively, step the interpreter the same number of times, and compare everything.
fn differential(body: &[u8], seed: Seed, context: &str) {
    run_and_compare(build(body, seed), context);
}

fn run_and_compare(mut roles: Roles, context: &str) {
    let slots = roles.slots;
    let retired = roles.native.perf_counters().jit_direct_insns;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "{context}: block did not run natively"
    );
    assert_eq!(
        usize::try_from(roles.native.perf_counters().jit_direct_insns - retired).unwrap(),
        slots,
        "{context}: every slot must retire natively"
    );
    for _ in 0..slots {
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

/// The CLOCK CHARGE, separated by accumulation.
///
/// The three-operand IMUL charges `clocks(14)` where the two-operand form charges `clocks(9)` and
/// `DirectKind::raw_clocks`' default returns 2. None of those errors is visible in a three-slot
/// block: the 586 dial divides the block's raw total by twelve and floors, so 18, 13 and 6 raw
/// all round to the same scaled clock, and the emitter's own `completed_raw` assertion cannot
/// see it either because it sums the same accessor it checks. This is the shape that let the
/// Phase 5 call-out ship a two-clock double-charge, and it is why that battery counts slots
/// rather than checking one.
///
/// One to four IMUL slots. At four the correct charge is `2 + 4*14 + 2 = 60` raw against 40 for
/// `clocks(9)` and 16 for the default -- 5 scaled clocks against 3 and 1.
#[test]
fn three_operand_imul_charge_matches_the_interpreter_across_slot_counts() {
    for count in 1..=4usize {
        // A different destination per slot so no slot feeds the next, and `imm` of 3 so nothing
        // overflows and the flag path is the same on every row.
        let bodies: Vec<Vec<u8>> = (0..count)
            .map(|i| imul_imm32(u8::try_from(i).unwrap(), 3, 3))
            .collect();
        let refs: Vec<&[u8]> = bodies.iter().map(Vec::as_slice).collect();
        let seed = Seed::new([7; 8]);
        run_and_compare(build_n(&refs, seed), &format!("{count} IMUL slots"));
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

// ---------------------------------------------------------------------------
// The byte-lane register ALU.
// ---------------------------------------------------------------------------

/// ALU byte form 0 (`op r/m8, r8`): opcodes 0x00/0x08/0x10/0x18/0x20/0x28/0x30/0x38.
/// `dst` is the r/m (a byte-register index) and `src` is the ModRM reg field.
fn alu_byte_rm_dst(op: u8, dst: u8, src: u8) -> Vec<u8> {
    vec![op << 3, 0b1100_0000 | (src << 3) | dst]
}

/// ALU byte form 2 (`op r8, r/m8`): opcodes 0x02/0x0A/0x12/0x1A/0x22/0x2A/0x32/0x3A.
/// `dst` is the ModRM reg field and `src` is the r/m.
fn alu_byte_reg_dst(op: u8, dst: u8, src: u8) -> Vec<u8> {
    vec![(op << 3) | 2, 0b1100_0000 | (dst << 3) | src]
}

/// A seed whose eight registers carry DISTINCT bytes in every lane, so a lowering that read the
/// wrong register, the wrong half, or the wrong width lands on a different value rather than
/// coincidentally the right one. Index 4 (ESP) is included for completeness; `build` overwrites
/// it, and no byte row below names ESP's lanes.
fn byte_seed() -> Seed {
    Seed::new([
        0x1234_56f0, // eax: AL=f0 AH=56
        0x2345_6701, // ecx: CL=01 CH=67
        0x3456_787f, // edx: DL=7f DH=78
        0x4567_8980, // ebx: BL=80 BH=89
        0x5678_9aab, // esp (overwritten)
        0x6789_abcd, // ebp
        0x789a_bcde, // esi
        0x89ab_cdef, // edi
    ])
}

#[test]
fn byte_lane_register_alu_matches_the_interpreter_in_both_operand_orders() {
    // All eight operations, both operand orders, over a register pair set that covers the LOW
    // lanes (0..=3 -> AL/CL/DL/BL) and the HIGH lanes (4..=7 -> AH/CH/DH/BH) in every
    // combination of the two. CF is seeded both ways because ADC/SBB consume it.
    for op in 0u8..8 {
        for (dst, src) in [
            (0u8, 1u8), // AL, CL   -- low, low
            (0, 5),     // AL, CH   -- low destination, HIGH source
            (5, 0),     // CH, AL   -- HIGH destination, low source
            (4, 7),     // AH, BH   -- high, high
            (3, 6),     // BL, DH
            (7, 2),     // BH, DL
        ] {
            for eflags in [0x202u32, 0x203, 0x2d7] {
                for pending in [false, true] {
                    let mut seed = byte_seed().flags(eflags);
                    if pending {
                        seed = seed.pending();
                    }
                    let label =
                        format!("op={op} dst={dst} src={src} eflags={eflags:#x} pending={pending}");
                    differential(
                        &alu_byte_rm_dst(op, dst, src),
                        seed,
                        &format!("byte form 0 {label}"),
                    );
                    differential(
                        &alu_byte_reg_dst(op, dst, src),
                        seed,
                        &format!("byte form 2 {label}"),
                    );
                }
            }
        }
    }
}

#[test]
fn byte_lane_register_alu_handles_lanes_of_the_same_home() {
    // `add al, ah` names two byte lanes of ONE 32-bit home; `xor ch, ch` and `sub bl, bl` name
    // one lane twice. Both fail if the emitter writes the destination before reading the source,
    // and the same-lane rows are the ones whose result is a constant (0 for XOR/SUB) so a
    // divergence shows up in the flags as well as the register.
    for op in 0u8..8 {
        for (dst, src) in [(0u8, 4u8), (4, 0), (1, 1), (5, 5), (3, 3), (2, 6)] {
            for eflags in [0x202u32, 0x203] {
                let seed = byte_seed().flags(eflags).pending();
                let label = format!("op={op} dst={dst} src={src} eflags={eflags:#x}");
                differential(
                    &alu_byte_rm_dst(op, dst, src),
                    seed,
                    &format!("aliased form 0 {label}"),
                );
                differential(
                    &alu_byte_reg_dst(op, dst, src),
                    seed,
                    &format!("aliased form 2 {label}"),
                );
            }
        }
    }
}

#[test]
fn byte_lane_register_alu_leaves_the_rest_of_the_destination_alone() {
    // The write-back must define EIGHT bits. `SUB BL, BL` zeroes BL and must leave BH and the
    // upper sixteen bits of EBX exactly as seeded; a 32-bit write-back agrees on the lane and
    // diverges here. Checked directly rather than only through the differential so the failure
    // names the property.
    let seed = byte_seed();
    let mut roles = build(&alu_byte_rm_dst(5, 3, 3), seed);
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "block did not run natively"
    );
    assert_eq!(
        roles.native.registers.gpr[3], 0x4567_8900,
        "SUB BL,BL must clear only BL"
    );

    // And the high-byte mirror: `SUB BH, BH` clears bits 8..16 alone.
    let mut roles = build(&alu_byte_rm_dst(5, 7, 7), seed);
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "block did not run natively"
    );
    assert_eq!(
        roles.native.registers.gpr[3], 0x4567_0080,
        "SUB BH,BH must clear only BH"
    );
}

#[test]
fn byte_lane_alu_memory_source_form_is_still_a_barrier() {
    // `XOR AL, [EBX]` — 0x32 /r with mod = 0b00, quake's `0x32 /0` census row. The register arm
    // must not have widened into the memory form: `AluMemSource`'s byte path is unreachable and
    // incomplete (no byte lane in `emit_alu_preloaded`, and `byte_reads` does not count it), so
    // admitting it here would be a miscompile rather than a lowering.
    still_a_barrier(&[0x32, 0b0000_0011], "0x32 memory source");
    still_a_barrier(&[0x3a, 0b0000_0011], "0x3A memory source");
}

#[test]
fn sixteen_bit_byte_alu_register_form_is_still_a_barrier() {
    // A 66-prefixed byte ALU. None of 0x00/0x02/../0x38/0x3A is in `classify`'s
    // OperandSize::Word allowlist, so the prefixed encoding falls to None above the `match form`
    // rather than reaching the new arms.
    still_a_barrier(&[0x66, 0x38, 0b1100_0001], "66 0x38 register");
    still_a_barrier(&[0x66, 0x3a, 0b1100_0001], "66 0x3A register");
}
