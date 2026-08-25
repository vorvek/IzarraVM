// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The S1 width lift: native Word emitters for ENTER imm16,0, LEAVE and LEA, plus the CLD/STD
//! Word policy lift. The S4b row (native PUSH SS) joins at the end of the file: it is a different
//! slice, but it is the same question at the same seam (an operand size and a stack width chosen
//! independently), and `WidthCase` is the harness that asks it. A second copy of this machinery
//! in its own file would be the duplication house rule 6 bars.
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

/// The machine a fixture runs on: a CODE segment width and a STACK width, chosen independently.
///
/// They are independent in the architecture (386 PRM 16.2) and independent in the backend: CS.D
/// picks the block's `address_wrap` and the compile key's `d`, SS.B picks the pointer width the
/// stack sites emit. The two DIAGONAL machines are the common ones and the two OFF-DIAGONAL ones
/// are why this is an enum rather than a bool: a lowering that took the stack pointer's width from
/// the block's `address_wrap`, or the stack address's wrap from SS.B, agrees with the correct
/// answer on both diagonals and is only caught off them.
#[derive(Clone, Copy)]
enum Machine {
    /// Real mode, CS.D = 0, SS.B = 0. The loader's own machine and the ordinary DOS one.
    Code16Stack16,
    /// CS.D = 0 with a 32-bit stack, built by hand as
    /// `a_thirty_two_bit_stack_in_a_sixteen_bit_segment_keeps_its_full_pointer` does. The block's
    /// `address_wrap` is Word while every stack site must NOT wrap.
    Code16Stack32,
    /// Protected mode, flat, CS.D = 1, SS.B = 1.
    Code32Stack32,
    /// Flat 32-bit code with a 16-bit stack. The block's `address_wrap` is None while every stack
    /// site must wrap at 64K.
    Code32Stack16,
    // The three `...Ss` machines below give SS a selector that is DISTINCT FROM EVERY OTHER
    // SEGMENT's. A fixture that pushes the stack selector is vacuous without that, in two
    // different ways that both look like a pass:
    //
    //  * on the plain 16-bit machine every selector is 0, so a slot that pushed nothing at all
    //    would agree with a slot that pushed SS, because the stack is zero-filled already;
    //  * on the two flat machines `flat_stack_cpu` gives DS, SS, ES, FS and GS the SAME
    //    `flat(0x10, 0x93)`, so a classifier arm that mapped 0x16 to ES would agree with one
    //    that mapped it to SS. That mutation survived the first version of these fixtures and is
    //    why the two flat variants exist.
    //
    /// `Code16Stack16` with SS at a non-zero real-mode selector. Base 0x1000 also keeps the
    /// stack off the block's own code page, which the module note above explains.
    Code16Stack16Ss,
    /// `Code32Stack32` with SS at its own selector. Base and access byte are untouched, so
    /// nothing but the pushed value can tell the two machines apart.
    Code32Stack32Ss,
    /// `Code32Stack16` with SS at its own selector, SS.B and ESP left as that machine sets them.
    Code32Stack16Ss,
}

/// Give SS a selector no other segment carries, without disturbing its base, limit, access byte
/// or B bit: the block still bakes the same stack base, and the only observable change is the
/// value `PUSH SS` stores.
fn distinguish_ss(cpu: &mut CpuGsw) {
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.selector = 0x0018;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
}

impl Machine {
    /// The compile key's `d`, which is CS.D and nothing else.
    fn d(self) -> bool {
        matches!(
            self,
            Self::Code32Stack32
                | Self::Code32Stack16
                | Self::Code32Stack32Ss
                | Self::Code32Stack16Ss
        )
    }

    fn cpu(self, entry: u32) -> CpuGsw {
        match self {
            Self::Code16Stack16 => sixteen_bit_code_cpu(entry),
            Self::Code16Stack32 => {
                let mut cpu = sixteen_bit_code_cpu(entry);
                let mut ss = cpu.registers.segment(SegmentIndex::Ss);
                ss.default_size_32 = true;
                // WIDENED on purpose. Left at real mode's 0xFFFF every stack access above 64K
                // side-exits on the segment-limit compare and the fixture passes interpreted.
                ss.limit = u32::MAX;
                cpu.registers.set_segment(SegmentIndex::Ss, ss);
                cpu
            }
            Self::Code32Stack32 => flat_stack_cpu(entry),
            Self::Code32Stack16 => sixteen_bit_stack_cpu(entry),
            Self::Code16Stack16Ss => {
                let mut cpu = sixteen_bit_code_cpu(entry);
                cpu.load_segment_real(SegmentIndex::Ss, 0x0100);
                cpu.registers.set_esp(0x0700);
                cpu
            }
            Self::Code32Stack32Ss => {
                let mut cpu = flat_stack_cpu(entry);
                distinguish_ss(&mut cpu);
                cpu
            }
            Self::Code32Stack16Ss => {
                let mut cpu = sixteen_bit_stack_cpu(entry);
                distinguish_ss(&mut cpu);
                cpu
            }
        }
    }
}

/// A fixture: the same bytes run wholly interpreted and again with the leading block installed and
/// entered natively, then compared on the WHOLE CPU (registers, EIP, EFLAGS, clocks) and on guest
/// RAM.
struct WidthCase {
    machine: Machine,
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
    let mut interp = case.machine.cpu(ENTRY);
    (case.arm)(&mut interp, &mut interp_bus);
    drive(&mut interp, &mut interp_bus);

    let mut native_bus = sixteen_bit_bus(program);
    let mut native = case.machine.cpu(ENTRY);
    arm_native_sixteen_bit(&mut native, &mut native_bus, case.pages);
    let starts: Vec<u32> = case.starts.iter().map(|offset| ENTRY + offset).collect();
    warm_sixteen_bit(&mut native, &mut native_bus, &starts);

    let d = case.machine.d();
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
        crate::tests::settled_registers(&outcome.native),
        crate::tests::settled_registers(&outcome.interp),
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
        machine: Machine::Code16Stack16,
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
        machine: Machine::Code32Stack32,
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
        machine: Machine::Code16Stack16,
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
        machine: Machine::Code32Stack32,
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
// The two OFF-DIAGONAL machines: CS.D and SS.B disagreeing.
// ---------------------------------------------------------------------------
//
// Every fixture above runs a machine where the code-segment width and the stack width match, and
// on those two machines a lowering that read the stack pointer's width off the block's
// `address_wrap`, or the stack address's wrap off SS.B, gives the RIGHT answer. These two are
// where it does not. Each runs an ENTER and the LEAVE that undoes it, so the pair must return
// both pointers to where they started, and each pre-seeds the address a wrongly-wrapped stack site
// would have used so that a wrong answer is a wrong VALUE rather than an absence.

fn arm_sixteen_bit_code_thirty_two_bit_stack(cpu: &mut CpuGsw, bus: &mut TestBus) {
    cpu.halted = false;
    cpu.registers.gpr = [0; 8];
    // ABOVE 64K, which is the whole point: the block's `address_wrap` is Word because CS.D is 0,
    // and every stack site here must ignore it.
    cpu.registers.set_esp(0x0001_8100);
    cpu.registers.set_ebp(0x0001_8100);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    // Both candidate slots seeded, and with values a correct run never writes.
    bus.memory[0x1_80f0..0x1_8100].fill(0);
    bus.memory[0x80fe..0x8100].copy_from_slice(&0xa5a5u16.to_le_bytes());
    bus.trace = BusTrace::default();
}

/// UNPREFIXED ENTER and LEAVE in a 16-bit code segment on a 32-bit stack.
///
/// `Enter16 { stack32: true }` and `Leave16 { stack32: true }` reached without a prefix, which is
/// the cell the 32-bit-segment fixtures reach only with one. The block's `address_wrap` is Word,
/// so a stack site that took its wrap from the block instead of from SS.B pushes at 0x80FE and
/// then pops the seeded 0xA5A5 back into BP.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn enter16_and_leave16_on_a_thirty_two_bit_stack_in_a_sixteen_bit_segment() {
    let outcome = run_width_case(&WidthCase {
        machine: Machine::Code16Stack32,
        // inc ax; enter 0x10,0; leave; inc cx; hlt
        code: &[0x40, 0xC8, 0x10, 0x00, 0x00, 0xC9, 0x41, 0xF4],
        starts: &[0, 1, 5, 6],
        instructions: 4,
        // 2 (inc) + 10 (ENTER) + 4 (LEAVE) + 2 (inc).
        raw_clocks: 18,
        pages: &[0x0000, 0x8000, 0x1_8000],
        memory_len: 0x2_0000,
        arm: arm_sixteen_bit_code_thirty_two_bit_stack,
    });
    assert_same_state(&outcome);

    assert_eq!(
        u16::from_le_bytes(
            outcome.native_bus.memory[0x1_80fe..0x1_8100]
                .try_into()
                .unwrap()
        ),
        0x8100,
        "the pushed word must land at the UNWRAPPED ESP - 2"
    );
    assert_eq!(
        u16::from_le_bytes(
            outcome.native_bus.memory[0x80fe..0x8100]
                .try_into()
                .unwrap()
        ),
        0xa5a5,
        "nothing may be written at the 16-bit-masked address"
    );
    assert_eq!(
        outcome.native.registers.esp(),
        0x0001_8100,
        "the ENTER/LEAVE pair must return the full stack pointer"
    );
    assert_eq!(
        outcome.native.registers.ebp(),
        0x0001_8100,
        "the ENTER/LEAVE pair must return the frame pointer"
    );
}

fn arm_flat_code_sixteen_bit_stack(cpu: &mut CpuGsw, bus: &mut TestBus) {
    cpu.halted = false;
    cpu.registers.gpr = [0; 8];
    // SP AT ZERO, so the ENTER's two-byte push BORROWS across bit 16. A 32-bit subtract gives
    // 0x1233_FFFE and a 16-bit one gives 0xFFFE with ESP[31:16] preserved, and the two differ in
    // both the address written and the pointer left behind.
    cpu.registers.set_esp(0x1234_0000);
    cpu.registers.set_ebp(0x1234_0000);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.memory[0xfff0..0x1_0000].fill(0);
    bus.trace = BusTrace::default();
}

/// 66-PREFIXED ENTER and LEAVE in a 32-bit code segment on a 16-bit stack.
///
/// `Enter16 { stack32: false }` and `Leave16 { stack32: false }` reached from a block whose
/// `address_wrap` is None, which is the mirror of the fixture above. The stack sites must wrap and
/// must move SP alone, and SP starts at zero so both halves of that borrow across bit 16: the
/// pushed word lands at 0xFFFE rather than at 0x1233_FFFE, and ESP[31:16] survives the trip.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn enter16_and_leave16_on_a_sixteen_bit_stack_in_a_flat_segment() {
    let outcome = run_width_case(&WidthCase {
        machine: Machine::Code32Stack16,
        // inc eax; 66 enter 0x10,0; 66 leave; inc ecx; hlt
        code: &[0x40, 0x66, 0xC8, 0x10, 0x00, 0x00, 0x66, 0xC9, 0x41, 0xF4],
        starts: &[0, 1, 6, 8],
        instructions: 4,
        raw_clocks: 18,
        pages: &[0x0000, 0xf000],
        memory_len: 0x1_0000,
        arm: arm_flat_code_sixteen_bit_stack,
    });
    assert_same_state(&outcome);

    assert_eq!(
        u16::from_le_bytes(
            outcome.native_bus.memory[0xfffe..0x1_0000]
                .try_into()
                .unwrap()
        ),
        0x0000,
        "the pushed word is the old BP and it lands at (SP - 2) & 0xFFFF"
    );
    assert_eq!(
        outcome.native.registers.esp(),
        0x1234_0000,
        "the pair must return SP with ESP[31:16] untouched"
    );
    assert_eq!(
        outcome.native.registers.ebp(),
        0x1234_0000,
        "the pair must return BP with EBP[31:16] untouched"
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
        machine: Machine::Code16Stack16,
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
        machine: Machine::Code32Stack32,
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
        machine: Machine::Code16Stack16,
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
        machine: Machine::Code16Stack16,
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

// ---------------------------------------------------------------------------
// S4b: PUSH SS (0x16), the last member of the selector-push arm.
//
// 747,415 block-stopping hits on the tombraid DOS/4GW loader census of 2026-08-22, the largest
// remaining barrier row after S3. It was excluded from the `0x06 | 0x0e | 0x1e` arm on a
// misreading: the interrupt-shadow argument belongs to POP SS and MOV SS, which LOAD the stack
// segment. PUSH SS reads the selector and arms nothing, and the interpreter's 0x16 arm is the
// 0x06 arm with a different `SegmentIndex`.
//
// Three cells, which is every cell `stack_width_kind` admits a push in:
//
//   SS.B = 0 + Word   `Push16`, two bytes, the pointer wraps at 64K
//   SS.B = 1 + Dword  `Push`, four bytes
//   SS.B = 1 + Word   no cell, refused (asserted in cpu_jit_s5_allowlist_test.rs)
//
// The Word cell is reached unprefixed from a 16-bit code segment and 66-prefixed from a 32-bit
// one, and both are here because they take different paths to the same kind.
// ---------------------------------------------------------------------------

fn arm_push_ss(cpu: &mut CpuGsw, bus: &mut TestBus, esp: u32) {
    cpu.halted = false;
    cpu.registers.gpr = [0; 8];
    cpu.registers.set_esp(esp);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
}

fn arm_push_ss16(cpu: &mut CpuGsw, bus: &mut TestBus) {
    arm_push_ss(cpu, bus, 0x0700);
}

fn arm_push_ss32(cpu: &mut CpuGsw, bus: &mut TestBus) {
    arm_push_ss(cpu, bus, 0x0000_1800);
}

/// The stack pointer's upper half is non-zero and its low half is zero, so the two-byte push
/// wraps SP to 0xFFFE. A lowering that took the pointer width from the operand size would move
/// ESP to 0x1233_FFFE and write four bytes at a different page.
fn arm_push_ss_wrap(cpu: &mut CpuGsw, bus: &mut TestBus) {
    arm_push_ss(cpu, bus, 0x1234_0000);
}

/// PUSH SS unprefixed in a 16-bit code segment on a 16-bit stack: the loader's own shape.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn push_ss16_ssb0_stores_the_stack_selector() {
    let outcome = run_width_case(&WidthCase {
        machine: Machine::Code16Stack16Ss,
        // inc ax; push ss; inc cx; hlt
        code: &[0x40, 0x16, 0x41, 0xF4],
        starts: &[0, 1, 2],
        instructions: 3,
        // 2 (inc) + 2 (the 0x16 arm's clocks(2)) + 2 (inc).
        raw_clocks: 6,
        pages: &[0x0000, 0x1000],
        memory_len: 0x2000,
        arm: arm_push_ss16,
    });
    assert_same_state(&outcome);

    assert_eq!(
        outcome.native.registers.esp(),
        0x0000_06fe,
        "SP drops by two, not four"
    );
    // SS base 0x1000 plus SP 0x06FE. The selector, not the base and not zero: this is the
    // assertion the machine variant exists for.
    assert_eq!(
        u16::from_le_bytes(
            outcome.native_bus.memory[0x16fe..0x1700]
                .try_into()
                .expect("two stack bytes")
        ),
        0x0100,
        "the pushed word is the SS SELECTOR"
    );
    assert_eq!(outcome.block_len, 3);
}

/// PUSH SS unprefixed in a 32-bit code segment on a 32-bit stack: four bytes, the selector
/// zero-extended, exactly as PUSH ES already does in that cell (386 PRM).
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn push_ss32_ssb1_stores_the_zero_extended_selector() {
    let outcome = run_width_case(&WidthCase {
        machine: Machine::Code32Stack32Ss,
        // inc eax; push ss; inc ecx; hlt
        code: &[0x40, 0x16, 0x41, 0xF4],
        starts: &[0, 1, 2],
        instructions: 3,
        raw_clocks: 6,
        pages: &[0x0000, 0x1000],
        memory_len: 0x2000,
        arm: arm_push_ss32,
    });
    assert_same_state(&outcome);

    assert_eq!(
        outcome.native.registers.esp(),
        0x0000_17fc,
        "ESP drops by four"
    );
    assert_eq!(
        u32::from_le_bytes(
            outcome.native_bus.memory[0x17fc..0x1800]
                .try_into()
                .expect("four stack bytes")
        ),
        0x0000_0018,
        "the pushed dword is the SS SELECTOR, zero-extended, and not ES's"
    );
    assert_eq!(outcome.block_len, 3);
}

/// The 66-prefixed form, which is the only way a 32-bit code segment reaches the Word cell.
///
/// On a 16-bit stack, so it also pins the two things a width-confused lowering gets wrong: the
/// pointer wraps at 64K and ESP's upper half survives.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn push_ss16_prefixed_wraps_the_pointer_and_keeps_the_upper_half() {
    let outcome = run_width_case(&WidthCase {
        machine: Machine::Code32Stack16Ss,
        // inc eax; 66 push ss; inc ecx; hlt
        code: &[0x40, 0x66, 0x16, 0x41, 0xF4],
        starts: &[0, 1, 3],
        instructions: 3,
        raw_clocks: 6,
        pages: &[0x0000, 0xf000],
        memory_len: 0x1_0000,
        arm: arm_push_ss_wrap,
    });
    assert_same_state(&outcome);

    assert_eq!(
        outcome.native.registers.esp(),
        0x1234_fffe,
        "SP wraps to 0xFFFE and ESP[31:16] survives"
    );
    assert_eq!(
        u16::from_le_bytes(
            outcome.native_bus.memory[0xfffe..0x1_0000]
                .try_into()
                .expect("two stack bytes")
        ),
        0x0018,
        "the pushed word is the SS SELECTOR, and not ES's"
    );
    assert_eq!(outcome.block_len, 4);
}
