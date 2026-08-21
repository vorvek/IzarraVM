// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The S1 width lift: native Word emitters for ENTER imm16,0, LEAVE and LEA, plus the CLD/STD
//! Word policy lift.
//!
//! The rows are here because the Tomb Raider DOS/4GW loader phase measures them at the top of its
//! barrier census on 2026-08-21: ENTER 1,977,855 runtime hits, LEAVE 1,277,833, LEA 1,744,694 and
//! CLD 736,877. Watcom-compiled 16-bit C carries a frame pointer, so ENTER opens and LEAVE closes
//! every function and neither had a Word lowering.
//!
//! Two shapes per stack row, because operand size and stack width are ORTHOGONAL (386 PRM 16.2)
//! and the kinds carry both:
//!
//! * an UNPREFIXED form in a 16-bit code segment on a 16-bit stack, which is what the loader
//!   actually runs, and
//! * a 66-PREFIXED form in a 32-bit code segment on a 32-bit stack, which is the other admitted
//!   cell and the one no unprefixed fixture can reach.
//!
//! The Dword LEAVE encoding pin lives here too rather than beside the 32-bit batteries. It is not
//! a test of the Dword arm for its own sake: it exists because this slice adds Word siblings
//! INSIDE the same emitter arm, and its whole claim is that the 32-bit bytes did not move while
//! that happened. Keeping it next to the change it guards is what makes it get read.
//!
//! Every fixture here installs its block BY HAND and enters it through
//! `try_run_direct_block_for_test` rather than letting the run loop admit it. That is deliberate:
//! `sixteen_bit_admission_level` reads `IZARRAVM_JIT16` from the environment, so an auto-admitted
//! 16-bit fixture would go vacuous, not red, on a machine where the knob is off.
//!
//! **Every stack fixture puts its stack on a page the block's own code is not on.** Installing a
//! block arms a code watch over the pages it spans, and a store into a watched page side-exits at
//! the guard. The first version of the two 16-bit fixtures here seeded SP at 0x0700, on the same
//! page as the code at 0x0100, and both exited at the stack slot with one instruction retired.
//! That is the guard doing its job, not a defect in the lowering, but it makes a fixture measure
//! the guard instead of the emitter.

// `warm_sixteen_bit` is not 16-bit-specific despite its name: `fetch_decoded` keys on the
// CPU's live CS.D, so it warms a 32-bit segment's decode lines just as well, and the fixtures
// below use it at both widths rather than carrying a second copy.
use super::sixteen_bit::{
    arm_native_sixteen_bit, sixteen_bit_bus, sixteen_bit_code_cpu, warm_sixteen_bit,
};
use super::*;

const ENTRY: u32 = 0x100;

/// The exact bytes the Dword `Leave` arm emits after its stack read resolves.
///
/// Hand-derived from the emitter and the encoder, and each instruction pins a different property:
///
/// | bytes | instruction | property |
/// |---|---|---|
/// | `8B 57 00` | `mov edx, [rdi]` | the popped value is read at the resolved pointer, DWORD wide |
/// | `45 89 EC` | `mov r12d, r13d` | `ESP <- EBP`, FULL 32 bits, and in that direction |
/// | `41 81 C4 04 00 00 00` | `add r12d, 4` | the pointer advances by four, not two |
/// | `41 89 D5` | `mov r13d, edx` | `EBP <- popped`, and AFTER the move above |
///
/// R12 and R13 are `GUEST_HOMES[4]` and `GUEST_HOMES[5]`, the homes of ESP and EBP.
///
/// Deliberately a SUBSTRING pin rather than a whole-block one. A block's code carries link-cell
/// addresses baked with `mov r64, imm64` and its memory sites are shaped by `one_lookup_load`,
/// which is seeded from the environment, so whole-block bytes are neither stable across runs nor
/// across machines. This sequence is emitted unconditionally by the arm at both settings.
const LEAVE_DWORD_TAIL: [u8; 16] = [
    0x8B, 0x57, 0x00, 0x45, 0x89, 0xEC, 0x41, 0x81, 0xC4, 0x04, 0x00, 0x00, 0x00, 0x41, 0x89, 0xD5,
];

fn occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// `leave` inside a 32-bit block on a 32-bit stack still emits the bytes it emitted before the
/// Word siblings existed.
///
/// The negative control matters as much as the positive one: without a block that has no LEAVE in
/// it, an emitter that emitted this sequence for every stack kind would pass.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn leave_dword_encoding_is_unchanged() {
    fn compile_at(code: &[u8]) -> Vec<u8> {
        let mut memory = vec![0u8; 0x1_0000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        let mut cpu = flat_stack_cpu(ENTRY);
        cpu.registers.set_ebp(0x0000_2000);
        cpu.set_fast_map_enabled_for_test(true);
        for page in [0x0000u32, 0x2000] {
            map_direct_page(
                &mut cpu,
                &mut bus,
                page,
                page,
                jit::fast_map::PagePermissions::UNPAGED,
                true,
                true,
            );
        }
        let starts: Vec<u32> = (0..code.len() as u32).map(|i| ENTRY + i).collect();
        decode_fixture(&mut cpu, &mut bus, &starts);
        jit::direct::compile(&mut cpu, ENTRY, true)
            .expect("the fixture block must compile")
            .code
    }

    // `inc eax; leave; inc ecx; hlt`. The HLT is unclassifiable and ends the block.
    let with_leave = compile_at(&[0x40, 0xC9, 0x41, 0xF4]);
    // The same block with the LEAVE replaced by another one-byte register op.
    let without_leave = compile_at(&[0x40, 0x42, 0x41, 0xF4]);

    assert_eq!(
        occurrences(&with_leave, &LEAVE_DWORD_TAIL),
        1,
        "the Dword LEAVE tail moved; code={with_leave:02x?}"
    );
    assert_eq!(
        occurrences(&without_leave, &LEAVE_DWORD_TAIL),
        0,
        "the pinned sequence is not specific to LEAVE; code={without_leave:02x?}"
    );
}

/// How a fixture's guest state is set before each leg. Applied to the interpreted CPU and to the
/// native one at exactly the same point, so anything the two legs disagree about afterwards came
/// from the emitter.
type Arm = fn(&mut CpuGsw, &mut TestBus);

/// A fixture: the same bytes run wholly interpreted and again with the leading block installed and
/// entered natively, then compared on the WHOLE CPU (registers, EIP, EFLAGS, clocks) and on guest
/// RAM.
///
/// `sixteen_bit` picks the code segment and with it the compile key's `d`; the stack width comes
/// from whichever CPU constructor that selects, so the four (operand, SS.B) cells are addressed by
/// choosing the segment and the prefix rather than by a knob.
struct WidthCase {
    sixteen_bit: bool,
    code: &'static [u8],
    /// Instruction START offsets from `ENTRY`, which is what the decode cache must be warmed over.
    starts: &'static [u32],
    /// Instructions the compiled block must cover. Asserted, because a block that stopped early
    /// would leave the row under test to the interpreter and every state assertion would pass.
    instructions: u8,
    /// Sum of `raw_clocks` over the block's slots, which is what the interpreter charges for the
    /// same instructions. A wrong constant on a new kind is invisible to every state assertion
    /// (`completed_raw` sums the same accessor the emitter asserts against), so it is named here.
    raw_clocks: u32,
    pages: &'static [u32],
    memory_len: usize,
    arm: Arm,
}

struct WidthOutcome {
    interp: CpuGsw,
    interp_bus: TestBus,
    native: CpuGsw,
    native_bus: TestBus,
    block_len: u16,
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn run_width_case(case: &WidthCase) -> WidthOutcome {
    let mut program = vec![0u8; case.memory_len];
    program[ENTRY as usize..ENTRY as usize + case.code.len()].copy_from_slice(case.code);

    let mut interp_bus = sixteen_bit_bus(program.clone());
    let mut interp = if case.sixteen_bit {
        sixteen_bit_code_cpu(ENTRY)
    } else {
        flat_stack_cpu(ENTRY)
    };
    (case.arm)(&mut interp, &mut interp_bus);
    drive(&mut interp, &mut interp_bus);

    let mut native_bus = sixteen_bit_bus(program);
    let mut native = if case.sixteen_bit {
        sixteen_bit_code_cpu(ENTRY)
    } else {
        flat_stack_cpu(ENTRY)
    };
    arm_native_sixteen_bit(&mut native, &mut native_bus, case.pages);
    let starts: Vec<u32> = case.starts.iter().map(|offset| ENTRY + offset).collect();
    warm_sixteen_bit(&mut native, &mut native_bus, &starts);

    let d = !case.sixteen_bit;
    let compilation = jit::direct::compile(&mut native, ENTRY, d)
        .expect("the width-lift fixture must compile as one block");
    assert_eq!(
        compilation.span.instructions, case.instructions,
        "block shape moved; every assertion about this fixture is about a different block"
    );
    assert_eq!(
        compilation.raw_clocks, case.raw_clocks,
        "the block's raw clocks moved away from what the interpreter charges"
    );
    let block_len = compilation.span.guest_len;
    let key = jit::direct::key_for(&native, ENTRY, d).expect("a key for the fixture block");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("install the fixture block");
    let block = native.jit_direct.block(id).expect("the block must be live");

    (case.arm)(&mut native, &mut native_bus);
    let entered = native.perf_counters().jit_direct_entries;
    let retired = native.perf_counters().jit_direct_insns;
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap(),
        "the installed block must actually run"
    );
    assert_eq!(native.perf_counters().jit_direct_entries - entered, 1);
    assert_eq!(
        native.perf_counters().jit_direct_insns - retired,
        u64::from(case.instructions),
        "every slot must retire natively, or the row under test ran interpreted"
    );
    // The tail past the block (the HLT, and anything between) is interpreted on both legs, so the
    // two CPUs end at the same architectural point and the whole-struct comparison is exact.
    drive(&mut native, &mut native_bus);

    WidthOutcome {
        interp,
        interp_bus,
        native,
        native_bus,
        block_len,
    }
}

fn assert_same_state(outcome: &WidthOutcome) {
    assert_eq!(
        outcome.native.registers, outcome.interp.registers,
        "register or EIP state differs between the native and interpreted legs"
    );
    assert_eq!(outcome.native.eflags(), outcome.interp.eflags(), "EFLAGS");
    assert_eq!(
        outcome.native_bus.memory, outcome.interp_bus.memory,
        "guest RAM differs"
    );
    assert_eq!(
        outcome.native.elapsed_clocks, outcome.interp.elapsed_clocks,
        "guest clocks differ"
    );
}

// ---------------------------------------------------------------------------
// LEAVE, the four-way (operand size x SS.B) matrix.
// ---------------------------------------------------------------------------

fn arm_leave16_ssb0(cpu: &mut CpuGsw, bus: &mut TestBus) {
    cpu.halted = false;
    cpu.registers.gpr = [0; 8];
    // BOTH high halves are non-zero and DIFFERENT. On a 16-bit stack LEAVE moves BP into SP and
    // must leave ESP[31:16] alone; a full-width move would copy EBP's high half across and the
    // two would agree by accident if either were zero.
    cpu.registers.set_esp(0xdead_1700);
    cpu.registers.set_ebp(0xbeef_1710);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.memory[0x1710..0x1712].copy_from_slice(&0x1234u16.to_le_bytes());
    bus.trace = BusTrace::default();
}

/// LEAVE at Word operand size on a 16-bit stack: `SP <- BP`, then a two-byte pop into BP.
///
/// This is the loader's own shape, and the cell an unprefixed 16-bit segment reaches.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn leave16_ssb0_pops_bp_and_restores_sp() {
    let outcome = run_width_case(&WidthCase {
        sixteen_bit: true,
        // inc ax; leave; inc cx; hlt
        code: &[0x40, 0xC9, 0x41, 0xF4],
        starts: &[0, 1, 2],
        instructions: 3,
        // 2 (inc) + 4 (the 0xC9 arm's clocks(4)) + 2 (inc).
        raw_clocks: 8,
        pages: &[0x0000, 0x1000],
        memory_len: 0x2000,
        arm: arm_leave16_ssb0,
    });
    assert_same_state(&outcome);

    assert_eq!(
        outcome.native.registers.esp(),
        0xdead_1712,
        "SP takes BP and then advances two; ESP[31:16] must survive"
    );
    assert_eq!(
        outcome.native.registers.ebp(),
        0xbeef_1234,
        "the popped word merges into BP and EBP[31:16] must survive"
    );
    assert_eq!(outcome.block_len, 3);
}

fn arm_leave16_ssb1(cpu: &mut CpuGsw, bus: &mut TestBus) {
    cpu.halted = false;
    cpu.registers.gpr = [0; 8];
    cpu.registers.set_esp(0x0000_9000);
    // The high half is NON-ZERO and the pop address is above 64K. On a 32-bit stack LEAVE moves
    // the FULL EBP into ESP even at Word operand size (386 PRM 17-96), so a 16-bit move here
    // would leave ESP at 0x0000_8100 and read the pop from the wrong page.
    cpu.registers.set_ebp(0x0001_8100);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.memory[0x1_8100..0x1_8102].copy_from_slice(&0x1234u16.to_le_bytes());
    bus.trace = BusTrace::default();
}

/// LEAVE at Word operand size on a 32-bit stack, the other admitted cell.
///
/// `ESP <- EBP` is a full 32-bit move here while the popped frame pointer is still two bytes
/// merged into BP. Getting either half of that from the operand size instead of from SS.B is the
/// classic version of this bug.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn leave16_ssb1_pops_bp_and_restores_esp() {
    let outcome = run_width_case(&WidthCase {
        sixteen_bit: false,
        // inc eax; 66 leave; inc ecx; hlt
        code: &[0x40, 0x66, 0xC9, 0x41, 0xF4],
        starts: &[0, 1, 3],
        instructions: 3,
        raw_clocks: 8,
        pages: &[0x0000, 0x1_8000],
        memory_len: 0x2_0000,
        arm: arm_leave16_ssb1,
    });
    assert_same_state(&outcome);

    assert_eq!(
        outcome.native.registers.esp(),
        0x0001_8102,
        "ESP takes the WHOLE EBP and then advances two"
    );
    assert_eq!(
        outcome.native.registers.ebp(),
        0x0001_1234,
        "the popped word merges into BP and EBP[31:16] must survive"
    );
    assert_eq!(outcome.block_len, 4);
}

// ---------------------------------------------------------------------------
// ENTER imm16, 0.
// ---------------------------------------------------------------------------

fn arm_enter16_ssb0(cpu: &mut CpuGsw, bus: &mut TestBus) {
    cpu.halted = false;
    cpu.registers.gpr = [0; 8];
    cpu.registers.set_esp(0xdead_1700);
    cpu.registers.set_ebp(0xbeef_1234);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.memory[0x16f0..0x1700].fill(0);
    bus.trace = BusTrace::default();
}

/// ENTER imm16, 0 at Word operand size on a 16-bit stack.
///
/// Three effects in one instruction and the order between them is the whole content: the pushed
/// word is the OLD BP, the new BP is the pointer AFTER that push, and the frame allocation comes
/// last and touches no memory.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn enter16_level0_pushes_bp_and_allocates_frame() {
    let outcome = run_width_case(&WidthCase {
        sixteen_bit: true,
        // inc ax; enter 0x10,0; inc cx; hlt
        code: &[0x40, 0xC8, 0x10, 0x00, 0x00, 0x41, 0xF4],
        starts: &[0, 1, 5],
        instructions: 3,
        // 2 (inc) + 10 (the 0xC8 arm's clocks(10)) + 2 (inc).
        raw_clocks: 14,
        pages: &[0x0000, 0x1000],
        memory_len: 0x2000,
        arm: arm_enter16_ssb0,
    });
    assert_same_state(&outcome);

    assert_eq!(
        u16::from_le_bytes(
            outcome.native_bus.memory[0x16fe..0x1700]
                .try_into()
                .unwrap()
        ),
        0x1234,
        "the pushed word is the OLD BP, at (SP - 2) & 0xFFFF"
    );
    assert_eq!(
        outcome.native.registers.ebp(),
        0xbeef_16fe,
        "BP becomes the stack pointer AFTER the push, merged into EBP"
    );
    assert_eq!(
        outcome.native.registers.esp(),
        0xdead_16ee,
        "the frame allocation subtracts from SP alone"
    );
}

fn arm_enter16_ssb1(cpu: &mut CpuGsw, bus: &mut TestBus) {
    cpu.halted = false;
    cpu.registers.gpr = [0; 8];
    cpu.registers.set_esp(0x0001_8100);
    cpu.registers.set_ebp(0xbeef_1234);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.memory[0x1_80f0..0x1_8100].fill(0);
    bus.trace = BusTrace::default();
}

/// ENTER imm16, 0 at Word operand size on a 32-bit stack.
///
/// The pushed value is still two bytes, but the pointer arithmetic is 32-bit throughout, and the
/// saved frame pointer BP takes the low half of the full ESP. The 386 PRM states that split
/// (17-62): the frame pointer is read at StackAddrSize, the push at the operand size.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn enter16_ssb1_allocates_on_the_full_pointer() {
    let outcome = run_width_case(&WidthCase {
        sixteen_bit: false,
        // inc eax; 66 enter 0x10,0; inc ecx; hlt
        code: &[0x40, 0x66, 0xC8, 0x10, 0x00, 0x00, 0x41, 0xF4],
        starts: &[0, 1, 6],
        instructions: 3,
        raw_clocks: 14,
        pages: &[0x0000, 0x1_8000],
        memory_len: 0x2_0000,
        arm: arm_enter16_ssb1,
    });
    assert_same_state(&outcome);

    assert_eq!(
        u16::from_le_bytes(
            outcome.native_bus.memory[0x1_80fe..0x1_8100]
                .try_into()
                .unwrap()
        ),
        0x1234,
        "the pushed word is the OLD BP, at ESP - 2"
    );
    assert_eq!(
        outcome.native.registers.ebp(),
        0xbeef_80fe,
        "BP takes the low half of the full ESP and EBP[31:16] survives"
    );
    assert_eq!(
        outcome.native.registers.esp(),
        0x0001_80ee,
        "the frame allocation subtracts from the full ESP"
    );
}

/// ENTER with a nesting level above zero stays a hard boundary.
///
/// The display copy is a loop of reads and pushes with its own fault points, and no emitter
/// exists for it. The positive control in the same fixture is what makes this a boundary test
/// rather than a test that ENTER never compiles.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn enter16_level1_stays_hard_boundary() {
    fn span_instructions(level: u8) -> u8 {
        let mut memory = vec![0u8; 0x2000];
        let code = [0x40, 0x41, 0x42, 0x43, 0xC8, 0x10, 0x00, level, 0x40, 0xF4];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        let mut bus = sixteen_bit_bus(memory);
        let mut cpu = sixteen_bit_code_cpu(ENTRY);
        arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000]);
        let starts: Vec<u32> = [0u32, 1, 2, 3, 4, 8].iter().map(|o| ENTRY + o).collect();
        warm_sixteen_bit(&mut cpu, &mut bus, &starts);
        jit::direct::compile(&mut cpu, ENTRY, false)
            .expect("the fixture must compile up to the boundary")
            .span
            .instructions
    }

    assert_eq!(
        span_instructions(0),
        6,
        "level 0 must be lowered, or this test proves nothing about level 1"
    );
    assert_eq!(
        span_instructions(1),
        4,
        "level 1 must stop the block at the ENTER"
    );
}

/// The Dword LEAVE on a 16-bit stack is the fourth matrix cell and stays refused.
///
/// It would move four bytes with a 16-bit pointer, so admitting it is a miscompile rather than a
/// missed lowering, and the stack-width matrix is what says no.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn leave_dword_on_a_sixteen_bit_stack_stays_a_hard_boundary() {
    let mut memory = vec![0u8; 0x1_0000];
    // inc eax; inc ecx; inc edx; inc ebx; leave; inc eax; hlt, all at Dword operand size.
    let code = [0x40, 0x41, 0x42, 0x43, 0xC9, 0x40, 0xF4];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = sixteen_bit_stack_cpu(ENTRY);
    cpu.registers.set_ebp(0x1234_0100);
    cpu.set_fast_map_enabled_for_test(true);
    for page in [0x0000u32, 0xf000] {
        map_direct_page(
            &mut cpu,
            &mut bus,
            page,
            page,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            true,
        );
    }
    let starts: Vec<u32> = [0u32, 1, 2, 3, 4, 5].iter().map(|o| ENTRY + o).collect();
    decode_fixture(&mut cpu, &mut bus, &starts);

    let span = jit::direct::compile(&mut cpu, ENTRY, true)
        .expect("the leading register ops must compile")
        .span;
    assert_eq!(
        span.instructions, 4,
        "a Dword LEAVE on a 16-bit stack must stop the block"
    );
}

// ---------------------------------------------------------------------------
// LEA r16, m.
// ---------------------------------------------------------------------------

fn arm_lea16(cpu: &mut CpuGsw, bus: &mut TestBus) {
    cpu.halted = false;
    cpu.registers.gpr = [0; 8];
    cpu.registers.set_esp(0xdead_1700);
    // The effective address is 0xFFF0 + 0x22, which WRAPS at 64K in a 16-bit address size. The
    // destination carries a high half so a full-width write is a wrong VALUE rather than an
    // absence.
    cpu.registers.set_ebp(0x00ab_fff0);
    cpu.registers.set_eax(0xcafe_0000);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
}

/// LEA r16, m: sixteen bits of the effective address, and not one bit more.
///
/// `write_gpr_sized(reg, Word, offset)` merges, so the destination's high half is part of the
/// answer. The address itself also wraps here, which pins the two together: the emitted form has
/// to narrow the WRITE without narrowing the address former's own arithmetic.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn lea16_writes_low_half_only() {
    let outcome = run_width_case(&WidthCase {
        sixteen_bit: true,
        // inc cx; lea ax,[bp+0x22]; inc dx; hlt
        code: &[0x41, 0x8D, 0x46, 0x22, 0x42, 0xF4],
        starts: &[0, 1, 4],
        instructions: 3,
        // 2 (inc) + 2 (the 0x8D arm's clocks(2)) + 2 (inc).
        raw_clocks: 6,
        pages: &[0x0000],
        memory_len: 0x2000,
        arm: arm_lea16,
    });
    assert_same_state(&outcome);

    assert_eq!(
        outcome.native.registers.eax(),
        0xcafe_0012,
        "LEA writes the masked offset into AX and leaves EAX[31:16] alone"
    );
}

fn arm_lea16_dword_address(cpu: &mut CpuGsw, bus: &mut TestBus) {
    cpu.halted = false;
    cpu.registers.gpr = [0; 8];
    cpu.registers.set_esp(0x0000_9000);
    // Above 64K on purpose: at a Dword ADDRESS size nothing wraps, so the offset's high half is
    // real and the Word operand size is the only thing that drops it.
    cpu.registers.set_ebp(0x0001_0000);
    cpu.registers.set_eax(0xcafe_ffff);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
}

/// LEA at a Word operand size and a Dword address size, which is the shape a 66 prefix makes in
/// 32-bit code and the one an unprefixed 16-bit fixture can never reach.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn lea16_at_a_dword_address_size_keeps_the_high_half() {
    let outcome = run_width_case(&WidthCase {
        sixteen_bit: false,
        // inc ecx; 66 lea ax,[ebp+0x22]; inc edx; hlt
        code: &[0x41, 0x66, 0x8D, 0x45, 0x22, 0x42, 0xF4],
        starts: &[0, 1, 5],
        instructions: 3,
        raw_clocks: 6,
        pages: &[0x0000, 0x1_0000],
        memory_len: 0x2_0000,
        arm: arm_lea16_dword_address,
    });
    assert_same_state(&outcome);

    assert_eq!(
        outcome.native.registers.eax(),
        0xcafe_0022,
        "the offset's high half is dropped by the OPERAND size, not by an address mask"
    );
}

// ---------------------------------------------------------------------------
// CLD / STD at Word: a policy lift, not a new emitter.
// ---------------------------------------------------------------------------

fn arm_cld(cpu: &mut CpuGsw, bus: &mut TestBus) {
    cpu.halted = false;
    cpu.registers.gpr = [0; 8];
    cpu.registers.set_esp(0xdead_1700);
    // DF SET on entry, so a CLD that emitted nothing at all would be visible.
    cpu.registers.eflags = 0x202 | crate::FLAG_DF;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
}

/// CLD then STD then CLD in a 16-bit code segment.
///
/// `emit_direction_flag` is width-invariant and was already correct; what was missing was the
/// allowlist entry that lets a 16-bit segment reach it. Three slots rather than one so that a
/// polarity swap is caught as well as a missing write.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn cld_word_row_is_native() {
    let outcome = run_width_case(&WidthCase {
        sixteen_bit: true,
        // inc ax; cld; std; cld; inc cx; hlt
        code: &[0x40, 0xFC, 0xFD, 0xFC, 0x41, 0xF4],
        starts: &[0, 1, 2, 3, 4],
        instructions: 5,
        // 2 + 2 + 2 + 2 + 2: the DF arms ride the `_ => 2` default, as the interpreter does.
        raw_clocks: 10,
        pages: &[0x0000],
        memory_len: 0x2000,
        arm: arm_cld,
    });
    assert_same_state(&outcome);

    assert_eq!(
        outcome.native.eflags() & crate::FLAG_DF,
        0,
        "the last CLD must leave DF clear"
    );
}

/// STD last, the other polarity.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn std_word_row_is_native() {
    let outcome = run_width_case(&WidthCase {
        sixteen_bit: true,
        // inc ax; cld; std; inc cx; hlt
        code: &[0x40, 0xFC, 0xFD, 0x41, 0xF4],
        starts: &[0, 1, 2, 3],
        instructions: 4,
        raw_clocks: 8,
        pages: &[0x0000],
        memory_len: 0x2000,
        arm: arm_cld,
    });
    assert_same_state(&outcome);

    assert_eq!(
        outcome.native.eflags() & crate::FLAG_DF,
        crate::FLAG_DF,
        "STD must leave DF set"
    );
}
