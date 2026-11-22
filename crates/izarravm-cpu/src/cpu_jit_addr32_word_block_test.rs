// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Direct-backend coverage for the 0x67 address-size override inside 16-bit blocks
//! (`IZARRAVM_ADDR32_WORD_ROWS`, `addr32_word_admitted`).
//!
//! Every row runs the same program twice: natively through one compiled block, and through the
//! interpreter. It compares registers, EFLAGS, clocks, bus clocks and the whole of guest RAM.
//!
//! The data segments get distinct bases (DS 0x10000, ES 0x20000, SS 0x8000), so a wrong segment
//! or a dropped base add changes the address. The RAM pattern does not repeat inside 256 KiB, so
//! a 16-bit-masked address reads other bytes than the 32-bit one. The "high" seeds put bits above
//! bit 15 into EBX, ECX, ESP and EBP: only a 32-bit address calculation reads the right byte.

use super::retire_governor::protected16_at;
use super::*;

const ENTRY: u32 = 0x1000;
const MEMORY_SIZE: usize = 0x4_0000;
const DS_BASE: u32 = 0x1_0000;
const ES_BASE: u32 = 0x2_0000;
const SS_BASE: u32 = 0x8000;

const A32: u8 = 0x67;
const O32: u8 = 0x66;
const SEG_ES: u8 = 0x26;
const SEG_SS: u8 = 0x36;

/// `mov si,si` and `mov di,di`: no-ops that keep the tested opcode off the block entry and off
/// the block end.
const FILL_A: [u8; 2] = [0x89, 0xf6];
const FILL_B: [u8; 2] = [0x89, 0xff];

/// The initial registers. The high halves of EAX, EDX are poisoned so a store of the wrong width
/// shows. EBX, ESP and EBP are small, so that no address leaves a 64 KiB segment by default.
const DEFAULT_GPR: [u32; 8] = [
    0xdead_00a0, // eax
    0x0000_0004, // ecx
    0xdead_00a2, // edx
    0x0000_1000, // ebx
    0x0000_0700, // esp
    0x0000_0800, // ebp
    0x0000_2000, // esi
    0x0000_3000, // edi
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Real,
    V86,
    Pm16,
    /// A flat protected-mode 32-bit code segment. Used only for the byte-identity row.
    Flat32,
}

const SIXTEEN_BIT_MODES: [Mode; 3] = [Mode::Real, Mode::V86, Mode::Pm16];

impl Mode {
    fn d(self) -> bool {
        self == Mode::Flat32
    }
}

/// Set the knob for the length of a test and restore the ambient reading afterwards, also when
/// the test panics.
struct Knob;

impl Knob {
    fn set(on: bool) -> Self {
        jit::direct::set_addr32_word_rows_for_test(Some(on));
        Self
    }
}

impl Drop for Knob {
    fn drop(&mut self) {
        jit::direct::set_addr32_word_rows_for_test(None);
        jit::direct::set_v86_loop_rows_for_test(None);
        jit::direct::set_generic_callout_for_test(None);
    }
}

fn pattern_memory() -> Vec<u8> {
    (0..MEMORY_SIZE as u32)
        .map(|i| (i.wrapping_mul(0x9e37_79b1) >> 24) as u8)
        .collect()
}

fn set_data_segment(cpu: &mut CpuGsw, segment: SegmentIndex, selector: u16, base: u32) {
    let mut register = cpu.registers.segment(segment);
    register.selector = selector;
    register.base = base;
    register.limit = 0xffff;
    register.access = 0x93;
    register.default_size_32 = false;
    cpu.registers.set_segment(segment, register);
}

fn set_limit(cpu: &mut CpuGsw, segment: SegmentIndex, limit: u32) {
    let mut register = cpu.registers.segment(segment);
    register.limit = limit;
    cpu.registers.set_segment(segment, register);
}

fn cpu_for(mode: Mode) -> CpuGsw {
    let mut cpu = match mode {
        Mode::Pm16 => protected16_at(ENTRY),
        Mode::Flat32 => {
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
            cpu
        }
        Mode::Real | Mode::V86 => {
            let mut cpu = CpuGsw::default();
            cpu.set_mode(GswMode::Gsw586);
            cpu.load_segment_real(SegmentIndex::Cs, 0);
            cpu.load_segment_real(SegmentIndex::Ds, (DS_BASE >> 4) as u16);
            cpu.load_segment_real(SegmentIndex::Es, (ES_BASE >> 4) as u16);
            cpu.load_segment_real(SegmentIndex::Ss, (SS_BASE >> 4) as u16);
            if mode == Mode::V86 {
                cpu.control.cr0 |= CR0_PE;
                cpu.registers.eflags = 0x202 | FLAG_VM | (3 << 12);
                cpu.cpl = 3;
                assert!(cpu.is_v86_mode(), "the V86 fixture must be in V86");
            }
            cpu
        }
    };
    if mode == Mode::Pm16 {
        set_data_segment(&mut cpu, SegmentIndex::Ds, 0x10, DS_BASE);
        set_data_segment(&mut cpu, SegmentIndex::Es, 0x10, ES_BASE);
        set_data_segment(&mut cpu, SegmentIndex::Ss, 0x10, SS_BASE);
    }
    cpu.set_eip(ENTRY);
    cpu
}

/// DS, ES and SS reach 128 KiB, so that a 32-bit offset with bit 16 set is inside the segment.
fn wide_limits(cpu: &mut CpuGsw) {
    for segment in [SegmentIndex::Ds, SegmentIndex::Es, SegmentIndex::Ss] {
        set_limit(cpu, segment, 0x1_ffff);
    }
}

fn ds_limit_1ffff(cpu: &mut CpuGsw) {
    set_limit(cpu, SegmentIndex::Ds, 0x1_ffff);
}

fn flat_ds(cpu: &mut CpuGsw) {
    set_limit(cpu, SegmentIndex::Ds, u32::MAX);
}

/// DS at linear 0 (real mode), so that a store through DS reaches the code page.
fn ds_over_code(cpu: &mut CpuGsw) {
    set_data_segment(cpu, SegmentIndex::Ds, 0, 0);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Expect {
    /// The whole block retires natively with no side exit.
    Native,
    /// The instruction at this index side-exits with no guest-visible effect, and the
    /// interpreter then runs it.
    ExitAt(usize),
    /// The same side exit, without a fault: the interpreter then runs the instruction normally.
    SideExitAt(usize),
    /// No claim about the retirement. The state must still match the interpreter.
    Any,
}

struct Case {
    label: String,
    mode: Mode,
    insns: Vec<Vec<u8>>,
    gpr: Vec<(usize, u32)>,
    tweak: Option<fn(&mut CpuGsw)>,
    poke: Vec<(usize, u8)>,
    expect: Expect,
    /// Skip the bus-clock comparison. Used for the misaligned in-page word store, where the native
    /// block and the interpreter differ by one bus clock with or without the 0x67 prefix.
    loose_bus_clocks: bool,
}

impl Case {
    fn new(label: impl Into<String>, mode: Mode, insns: Vec<Vec<u8>>) -> Self {
        Self {
            label: label.into(),
            mode,
            insns,
            gpr: Vec::new(),
            tweak: None,
            poke: Vec::new(),
            expect: Expect::Native,
            loose_bus_clocks: false,
        }
    }

    /// `FILL_A / body / FILL_B`: the tested instruction is in the middle of the block.
    fn middle(label: impl Into<String>, mode: Mode, body: &[u8]) -> Self {
        Self::new(
            label,
            mode,
            vec![FILL_A.to_vec(), body.to_vec(), FILL_B.to_vec()],
        )
    }

    fn gpr(mut self, index: usize, value: u32) -> Self {
        self.gpr.push((index, value));
        self
    }

    fn tweak(mut self, tweak: fn(&mut CpuGsw)) -> Self {
        self.tweak = Some(tweak);
        self
    }

    fn poke(mut self, at: usize, value: u8) -> Self {
        self.poke.push((at, value));
        self
    }

    fn exit_at(mut self, index: usize) -> Self {
        self.expect = Expect::ExitAt(index);
        self
    }

    fn side_exit_at(mut self, index: usize) -> Self {
        self.expect = Expect::SideExitAt(index);
        self
    }

    fn loose_bus_clocks(mut self) -> Self {
        self.loose_bus_clocks = true;
        self
    }

    fn any(mut self) -> Self {
        self.expect = Expect::Any;
        self
    }
}

struct Rig {
    native: CpuGsw,
    native_bus: TestBus,
    interp: CpuGsw,
    interp_bus: TestBus,
    starts: Vec<u32>,
    pristine: Vec<u8>,
}

fn seed(cpu: &mut CpuGsw, case: &Case) {
    let mut gpr = DEFAULT_GPR;
    for &(index, value) in &case.gpr {
        gpr[index] = value;
    }
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr = gpr;
    let saved = cpu.registers.eflags & (FLAG_VM | (3 << 12));
    cpu.registers.eflags = 0x202 | saved;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

fn prepare(case: &Case) -> Rig {
    let mut memory = pattern_memory();
    let mut code = Vec::new();
    let mut starts = Vec::new();
    for insn in &case.insns {
        starts.push(ENTRY + code.len() as u32);
        code.extend_from_slice(insn);
    }
    code.push(0xf4);
    memory[ENTRY as usize - 1] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    for &(at, value) in &case.poke {
        memory[at] = value;
    }
    let pristine = memory.clone();

    let mut native = cpu_for(case.mode);
    let mut interp = cpu_for(case.mode);
    let mut native_bus = sixteen_bit_bus_sized(memory.clone());
    let mut interp_bus = sixteen_bit_bus_sized(memory);
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interp, &mut interp_bus),
    ] {
        if let Some(tweak) = case.tweak {
            tweak(cpu);
        }
        seed(cpu, case);
        cpu.set_fast_map_enabled_for_test(true);
        for &linear in &starts {
            cpu.set_eip(linear);
            cpu.begin_instruction();
            cpu.fetch_decoded(bus, linear).expect("fixture decode");
        }
        for page in (0..MEMORY_SIZE as u32).step_by(0x1000) {
            super::sixteen_bit::arm_native_sixteen_bit(cpu, bus, &[page]);
        }
        cpu.set_eip(ENTRY);
    }
    Rig {
        native,
        native_bus,
        interp,
        interp_bus,
        starts,
        pristine,
    }
}

fn sixteen_bit_bus_sized(memory: Vec<u8>) -> TestBus {
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    bus
}

/// One interpreted instruction, as a comparable value: the outcome, or the error text.
fn step(cpu: &mut CpuGsw, bus: &mut TestBus) -> String {
    match cpu.cycle(bus) {
        Ok(_) => "ok".to_string(),
        Err(error) => format!("{error:?}"),
    }
}

fn compare_state(rig: &Rig, context: &str, loose_bus_clocks: bool) {
    assert_eq!(
        crate::tests::settled_registers(&rig.native),
        crate::tests::settled_registers(&rig.interp),
        "{context}: registers"
    );
    assert_eq!(
        rig.native.eflags(),
        rig.interp.eflags(),
        "{context}: materialized EFLAGS"
    );
    assert_eq!(
        rig.native.halted, rig.interp.halted,
        "{context}: halt latch"
    );
    assert_eq!(
        rig.native.elapsed_clocks, rig.interp.elapsed_clocks,
        "{context}: core clocks"
    );
    assert_eq!(
        rig.native.timing_rem, rig.interp.timing_rem,
        "{context}: timing remainder"
    );
    if !loose_bus_clocks {
        assert_eq!(
            rig.native_bus.trace.elapsed_clocks(),
            rig.interp_bus.trace.elapsed_clocks(),
            "{context}: bus clocks"
        );
    }
    assert_eq!(
        rig.native_bus.memory, rig.interp_bus.memory,
        "{context}: guest RAM"
    );
}

/// What a run measured on the native side.
#[derive(Clone, Copy, Debug)]
struct Run {
    retired: usize,
    side_exits: u64,
    code_watch_exits: u64,
}

fn compile_block(rig: &mut Rig, case: &Case) -> (jit::direct::CompiledBlock, u8) {
    let d = case.mode.d();
    let compilation = match jit::direct::compile(&mut rig.native, ENTRY, d) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!(
                "{}: structurally rejected, the row is still a barrier",
                case.label
            )
        }
        jit::direct::CompileOutcome::Retry(cause) => {
            panic!("{}: compile asked for a retry: {cause:?}", case.label)
        }
    };
    let instructions = compilation.span.instructions;
    let key = jit::direct::key_for(&rig.native, ENTRY, d).expect("entry key");
    assert!(matches!(
        rig.native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = rig
        .native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = rig.native.jit_direct.block(id).expect("live block");
    (block, instructions)
}

/// Compile and run the case natively, run the rest of it and the whole of it through the
/// interpreter, and compare.
fn execute(case: &Case) -> Run {
    execute_rig(case).0
}

fn execute_rig(case: &Case) -> (Run, Rig) {
    let mut rig = prepare(case);
    let (block, instructions) = compile_block(&mut rig, case);
    let n = case.insns.len();
    assert_eq!(
        usize::from(instructions),
        n,
        "{}: the block must cover every instruction",
        case.label
    );
    let before_insns = rig.native.perf_counters().jit_direct_insns;
    let before_exits = rig.native.perf_counters().jit_direct_side_exits;
    let before_watch = rig.native.perf_counters().jit_direct_exit_code_watch;
    assert!(
        rig.native
            .try_run_direct_block_for_test(&mut rig.native_bus, block)
            .unwrap(),
        "{}: the block did not run",
        case.label
    );
    let perf = rig.native.perf_counters();
    let run = Run {
        retired: (perf.jit_direct_insns - before_insns) as usize,
        side_exits: perf.jit_direct_side_exits - before_exits,
        code_watch_exits: perf.jit_direct_exit_code_watch - before_watch,
    };
    let total = match case.expect {
        Expect::Native => {
            assert_eq!(
                run.retired, n,
                "{}: every instruction must retire natively",
                case.label
            );
            assert_eq!(
                run.side_exits, 0,
                "{}: a wrong address side-exits and the interpreter then gives the right answer",
                case.label
            );
            n
        }
        Expect::ExitAt(index) | Expect::SideExitAt(index) => {
            assert_eq!(
                run.retired, index,
                "{}: the block must retire the prefix only",
                case.label
            );
            assert_eq!(run.side_exits, 1, "{}: one side exit", case.label);
            index + 1
        }
        Expect::Any => n,
    };
    let mut last_native = String::new();
    for _ in run.retired..total {
        last_native = step(&mut rig.native, &mut rig.native_bus);
    }
    let mut last_interp = String::new();
    for _ in 0..total {
        last_interp = step(&mut rig.interp, &mut rig.interp_bus);
    }
    if run.retired < total {
        assert_eq!(
            last_native, last_interp,
            "{}: the interpreted tail must give the same result",
            case.label
        );
    }
    if let Expect::ExitAt(index) = case.expect {
        let next = rig.starts[index] + case.insns[index].len() as u32;
        assert!(
            last_interp != "ok" || rig.interp.registers.eip != next,
            "{}: the instruction at {index} must fault, but the interpreter ran it",
            case.label
        );
    }
    compare_state(&rig, &case.label, case.loose_bus_clocks);
    (run, rig)
}

fn bus_clock_delta(rig: &Rig) -> i64 {
    rig.native_bus.trace.elapsed_clocks() as i64 - rig.interp_bus.trace.elapsed_clocks() as i64
}

fn memory_at(rig_memory: &[u8], at: usize, len: usize) -> Vec<u8> {
    rig_memory[at..at + len].to_vec()
}

// ---------------------------------------------------------------------------------------------
// a. The forms
// ---------------------------------------------------------------------------------------------

const FORMS: [(&str, &[u8]); 6] = [
    ("[ebx]", &[0x03]),
    ("[ebx+d8]", &[0x43, 0x10]),
    ("[ebx+d32]", &[0x83, 0x20, 0x00, 0x00, 0x00]),
    ("[ebx+ecx*4+d8]", &[0x44, 0x8b, 0x10]),
    ("[esp+8]", &[0x44, 0x24, 0x08]),
    ("[ebp-4]", &[0x45, 0xfc]),
];

fn high_seed(case: Case) -> Case {
    case.gpr(3, 0x0001_0100)
        .gpr(4, 0x0001_0700)
        .gpr(5, 0x0001_0800)
        .tweak(wide_limits)
}

/// Catches: a 16-bit mask on a 32-bit address (the load or store lands 64 KiB away), a
/// limit-compare elision that reads the block wrap, a base fold that is still refused, and a
/// width or segment mix-up in the 0x67 form. The high seeds are the discriminating ones: with
/// every register below 64 KiB the masked and unmasked addresses agree.
#[test]
fn word_and_dword_moves_through_every_address_form_match_the_interpreter() {
    let _knob = Knob::set(true);
    for mode in SIXTEEN_BIT_MODES {
        for opcode in [0x89u8, 0x8b] {
            for (name, tail) in FORMS {
                for segment in [None, Some(SEG_ES), Some(SEG_SS)] {
                    for dword in [false, true] {
                        for high in [false, true] {
                            let mut body = Vec::new();
                            body.extend(segment);
                            if dword {
                                body.push(O32);
                            }
                            body.push(A32);
                            body.push(opcode);
                            body.extend_from_slice(tail);
                            let label = format!(
                                "{mode:?} {opcode:#x} {name} seg={segment:?} dword={dword} high={high}"
                            );
                            let case = Case::middle(label, mode, &body);
                            execute(&if high { high_seed(case) } else { case });
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// b. LEA
// ---------------------------------------------------------------------------------------------

/// Catches: LEA masking a 32-bit effective address to 16 bits, and a word destination write that
/// clears the high half of the register.
#[test]
fn lea_with_a_dword_address_matches_the_interpreter_at_both_operand_sizes() {
    let _knob = Knob::set(true);
    for mode in SIXTEEN_BIT_MODES {
        for (name, body) in [
            ("word", vec![A32, 0x8d, 0x04, 0x8b]),
            ("dword", vec![O32, A32, 0x8d, 0x04, 0x8b]),
        ] {
            for (high, ebx, ecx) in [(false, 0x1000, 4), (true, 0x0001_0100, 0x0001_0001)] {
                let case = Case::middle(format!("{mode:?} lea {name} high={high}"), mode, &body)
                    .gpr(3, ebx)
                    .gpr(1, ecx);
                execute(&case);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// c, d. Faults
// ---------------------------------------------------------------------------------------------

/// THE MANDATORY ROW. EBX = 0x10000 against a 0xFFFF limit: a byte access at offset 0x10000 is
/// past the limit. The limit compare is elided under a 16-bit wrap, because a masked offset can
/// not exceed 0xFFFF. With the 32-bit address that elision is wrong. A build that reads the block
/// wrap here retires the instruction natively at offset 0, and this row sees it.
#[test]
fn a_dword_address_past_the_limit_faults_for_the_byte_test() {
    let _knob = Knob::set(true);
    for mode in SIXTEEN_BIT_MODES {
        let ds = Case::middle(
            format!("{mode:?} test byte [ebx] past DS"),
            mode,
            &[A32, 0xf6, 0x03, 0x5a],
        )
        .gpr(3, 0x1_0000)
        .exit_at(1);
        execute(&ds);
        let ss = Case::middle(
            format!("{mode:?} test byte [ebp] past SS"),
            mode,
            &[A32, 0xf6, 0x45, 0x00, 0x5a],
        )
        .gpr(5, 0x1_0000)
        .exit_at(1);
        execute(&ss);
    }
}

/// Catches: the same elision on the word-store path, and a store that lands before the check.
#[test]
fn a_word_store_past_the_limit_faults_and_writes_nothing() {
    let _knob = Knob::set(true);
    for mode in SIXTEEN_BIT_MODES {
        for (name, body, register, target) in [
            (
                "[ebx] DS",
                vec![A32, 0x89, 0x03],
                3,
                DS_BASE as usize + 0x1_0000,
            ),
            (
                "[esp+0] SS",
                vec![A32, 0x89, 0x44, 0x24, 0x00],
                4,
                SS_BASE as usize + 0x1_0000,
            ),
        ] {
            let case = Case::middle(format!("{mode:?} store {name}"), mode, &body)
                .gpr(register, 0x1_0000)
                .exit_at(1);
            let (_, rig) = execute_rig(&case);
            assert_eq!(
                memory_at(&rig.native_bus.memory, target, 4),
                memory_at(&rig.pristine, target, 4),
                "{name}: the faulting store must leave its target alone"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// e, f. Limits
// ---------------------------------------------------------------------------------------------

/// A flat limit with a nonzero base in 16-bit code is the base-fold path: nothing sits between
/// the address and the base add.
///
/// Catches: a fold that is applied under the 16-bit wrap, a missing base add, and a mask on the
/// flat offset.
#[test]
fn a_flat_data_segment_in_sixteen_bit_code_reaches_base_plus_the_full_offset() {
    let _knob = Knob::set(true);
    for mode in [Mode::Real, Mode::Pm16] {
        for (name, body, ebx, expect_native) in [
            ("word store", vec![A32, 0x89, 0x03], 0x1_2346, true),
            ("word load", vec![A32, 0x8b, 0x03], 0x1_2346, true),
            ("dword store", vec![O32, A32, 0x89, 0x03], 0x1_2344, true),
            ("dword load", vec![O32, A32, 0x8b, 0x03], 0x1_2344, true),
            ("odd word store", vec![A32, 0x89, 0x03], 0x1_2345, false),
            ("odd word load", vec![A32, 0x8b, 0x03], 0x1_2345, false),
        ] {
            let case = Case::middle(format!("{mode:?} flat {name}"), mode, &body)
                .gpr(3, ebx)
                .tweak(flat_ds);
            execute(&if expect_native {
                case
            } else {
                case.any().loose_bus_clocks()
            });
        }
    }
}

/// A 16-bit protected-mode DS with limit 0x1FFFF. With 0x67 a byte access at 0x1FFFF passes and
/// a dword access at 0x1FFFE faults. Without 0x67 the same register value wraps at 64 KiB.
#[test]
fn a_limit_above_sixty_four_k_decides_the_dword_address_and_not_the_sixteen_bit_one() {
    let _knob = Knob::set(true);
    for mode in [Mode::Pm16, Mode::Real] {
        let byte = Case::middle(
            format!("{mode:?} 67 test byte at limit"),
            mode,
            &[A32, 0xf6, 0x03, 0x5a],
        )
        .gpr(3, 0x1_ffff)
        .tweak(ds_limit_1ffff);
        execute(&byte);
        let dword = Case::middle(
            format!("{mode:?} 67 dword across the limit"),
            mode,
            &[O32, A32, 0x8b, 0x03],
        )
        .gpr(3, 0x1_fffe)
        .tweak(ds_limit_1ffff)
        .exit_at(1);
        execute(&dword);
        let wrapped = Case::middle(
            format!("{mode:?} no 67 dword wraps at 64 KiB"),
            mode,
            &[O32, 0x8b, 0x07],
        )
        .gpr(3, 0x1_fff0)
        .tweak(ds_limit_1ffff);
        execute(&wrapped);
        let wrapped_byte = Case::middle(
            format!("{mode:?} no 67 byte test wraps at 64 KiB"),
            mode,
            &[0xf6, 0x07, 0x5a],
        )
        .gpr(3, 0x1_ffff)
        .tweak(ds_limit_1ffff);
        execute(&wrapped_byte);
    }
}

// ---------------------------------------------------------------------------------------------
// g. Page crossing and misalignment
// ---------------------------------------------------------------------------------------------

/// Catches: a crossing dword access that the block serves, and an in-page misaligned word store
/// that the block refuses. The exit counts are what the word-memory rows assert too.
#[test]
fn crossing_and_misaligned_accesses_behave_as_without_the_prefix() {
    let _knob = Knob::set(true);
    for mode in SIXTEEN_BIT_MODES {
        let crossing = Case::middle(
            format!("{mode:?} dword load across a page"),
            mode,
            &[O32, A32, 0x8b, 0x03],
        )
        .gpr(3, 0x0ffe)
        .any();
        let run = execute(&crossing);
        assert_eq!(
            run.side_exits, 1,
            "{mode:?}: the crossing dword must exit, and only it"
        );
        let misaligned = Case::middle(
            format!("{mode:?} misaligned word store in a page"),
            mode,
            &[A32, 0x89, 0x03],
        )
        .gpr(3, 0x1001)
        .any()
        .loose_bus_clocks();
        let (run, rig) = execute_rig(&misaligned);
        assert_eq!(
            run.side_exits, 0,
            "{mode:?}: the in-page misaligned store is served"
        );
        // The native block and the interpreter differ in bus clocks on this shape. The 0x67
        // prefix must not change that difference.
        let control = Case::middle(
            format!("{mode:?} the same store without the prefix"),
            mode,
            &[0x89, 0x07],
        )
        .gpr(3, 0x1001)
        .any()
        .loose_bus_clocks();
        let (_, control_rig) = execute_rig(&control);
        assert_eq!(
            bus_clock_delta(&rig),
            bus_clock_delta(&control_rig),
            "{mode:?}: the prefix changes the bus-clock difference"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// h, i, j. The call-out rows
// ---------------------------------------------------------------------------------------------

#[test]
fn pop_to_a_dword_address_matches_the_interpreter() {
    let _knob = Knob::set(true);
    for mode in SIXTEEN_BIT_MODES {
        for (name, body, esp) in [
            ("pop word [ebx]", vec![A32, 0x8f, 0x03], 0x0700u32),
            ("pop dword [ebx]", vec![O32, A32, 0x8f, 0x03], 0x0700),
            (
                "pop word [esp+2] at SP=0xFFFE",
                vec![A32, 0x8f, 0x44, 0x24, 0x02],
                0xfffe,
            ),
        ] {
            let case = Case::middle(format!("{mode:?} {name}"), mode, &body)
                .gpr(4, esp)
                .any();
            execute(&case);
        }
    }
}

#[test]
fn group3_byte_forms_through_a_dword_address_match_the_interpreter() {
    let _knob = Knob::set(true);
    for mode in SIXTEEN_BIT_MODES {
        for (name, body) in [
            ("test /0", vec![A32, 0xf6, 0x03, 0x5a]),
            ("neg /3", vec![A32, 0xf6, 0x1b]),
            ("mul /4", vec![A32, 0xf6, 0x23]),
            ("idiv /7", vec![A32, 0xf6, 0x3b]),
        ] {
            // A small dividend, so that the quotient of the IDIV row fits a byte for any
            // divisor byte of the RAM pattern: no row here faults by accident.
            let case = Case::middle(format!("{mode:?} {name}"), mode, &body).gpr(0, 5);
            execute(&case.any());
        }
    }
    // IDIV by a zero byte raises #DE from inside the helper. Real mode only: the protected and
    // V86 fixtures have no IDT, so the fault would end in a triple fault on both sides.
    let divide_fault = Case::middle("Real idiv by zero", Mode::Real, &[A32, 0xf6, 0x3b])
        .gpr(0, 5)
        .poke(DS_BASE as usize + 0x1000, 0)
        .any();
    execute(&divide_fault);
}

#[test]
fn string_moves_with_a_dword_address_match_the_interpreter() {
    let _knob = Knob::set(true);
    jit::direct::set_generic_callout_for_test(Some(true));
    for mode in SIXTEEN_BIT_MODES {
        for (name, body) in [
            ("movsw", vec![A32, 0xa5]),
            ("movsd", vec![O32, A32, 0xa5]),
            ("movsb", vec![A32, 0xa4]),
        ] {
            for high in [false, true] {
                let case = Case::middle(format!("{mode:?} {name} high={high}"), mode, &body).any();
                execute(&if high {
                    case.gpr(6, 0x0001_2000)
                        .gpr(7, 0x0001_3000)
                        .tweak(wide_limits)
                } else {
                    case
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// k. Position in the block
// ---------------------------------------------------------------------------------------------

#[test]
fn the_prefixed_instruction_retires_natively_at_any_position_in_the_block() {
    let _knob = Knob::set(true);
    let body = vec![A32, 0x8b, 0x43, 0x10];
    for mode in SIXTEEN_BIT_MODES {
        for (name, insns) in [
            (
                "first",
                vec![body.clone(), FILL_A.to_vec(), FILL_B.to_vec()],
            ),
            (
                "middle",
                vec![FILL_A.to_vec(), body.clone(), FILL_B.to_vec()],
            ),
            ("last", vec![FILL_A.to_vec(), FILL_B.to_vec(), body.clone()]),
        ] {
            let case = Case::new(format!("{mode:?} {name}"), mode, insns)
                .gpr(3, 0x0001_0100)
                .tweak(wide_limits);
            execute(&case);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// l, m, n. Admission and refusal
// ---------------------------------------------------------------------------------------------

/// The compile result for a block, with the census on: its length in slots, and the barrier rows.
fn compile_census(
    mode: Mode,
    insns: Vec<Vec<u8>>,
) -> (Option<u8>, Vec<crate::DirectBarrierCensusRow>) {
    let case = Case::new("census", mode, insns);
    let mut rig = prepare(&case);
    rig.native.enable_direct_barrier_census(true);
    let length = match jit::direct::compile(&mut rig.native, ENTRY, mode.d()) {
        jit::direct::CompileOutcome::Compiled(compilation) => Some(compilation.span.instructions),
        _ => None,
    };
    let rows = rig
        .native
        .direct_barrier_census_snapshot()
        .expect("enabled census snapshot")
        .rows;
    (length, rows)
}

/// Three leading fills, so that a refusal at the body leaves a block of three slots: a shorter
/// block is not formed.
fn middle_insns(body: &[u8]) -> Vec<Vec<u8>> {
    vec![
        FILL_A.to_vec(),
        FILL_B.to_vec(),
        FILL_A.to_vec(),
        body.to_vec(),
        FILL_B.to_vec(),
    ]
}

#[test]
fn the_knob_off_stops_the_block_before_the_prefixed_instruction() {
    let _knob = Knob::set(false);
    for body in [
        vec![A32, 0x89, 0x03],
        vec![A32, 0x8b, 0x03],
        vec![A32, 0x8d, 0x04, 0x8b],
        vec![A32, 0x8f, 0x03],
        vec![A32, 0xf6, 0x03, 0x5a],
    ] {
        let (length, rows) = compile_census(Mode::Real, middle_insns(&body));
        assert_eq!(
            length,
            Some(3),
            "{body:02x?}: only the leading fill compiles"
        );
        let row = rows
            .iter()
            .find(|row| u8::try_from(row.opcode).ok() == Some(body[1]))
            .unwrap_or_else(|| panic!("{body:02x?}: the stop must be attributed"));
        assert_eq!(row.stop_reason, "prefix_unsupported");
        assert_eq!(row.prefix_mask & 2, 2, "the row names the address override");
    }
    // The counter reads zero on the OFF arm.
    let case = Case::middle("off counter", Mode::Real, &[A32, 0x89, 0x03]);
    let mut rig = prepare(&case);
    let _ = jit::direct::compile(&mut rig.native, ENTRY, false);
    assert_eq!(
        rig.native
            .perf_counters()
            .jit_direct_addr32_word_block_slots,
        0
    );
}

#[test]
fn the_knob_on_admits_the_allowlist_and_counts_the_slots() {
    let _knob = Knob::set(true);
    jit::direct::set_generic_callout_for_test(Some(true));
    // The native TEST ends its block one slot early with or without the prefix: the unprefixed
    // `F6 07 5A` compiles to four slots in the same position.
    for (body, slots) in [
        (vec![A32, 0x89, 0x03], 5),
        (vec![A32, 0x8b, 0x03], 5),
        (vec![O32, A32, 0x8d, 0x04, 0x8b], 5),
        (vec![A32, 0x8f, 0x03], 5),
        (vec![A32, 0xf6, 0x03, 0x5a], 4),
        (vec![A32, 0xf6, 0x1b], 5),
        (vec![A32, 0xa5], 5),
        (vec![A32, 0xa4], 5),
    ] {
        let (length, rows) = compile_census(Mode::Real, middle_insns(&body));
        assert_eq!(length, Some(slots), "{body:02x?}: the whole block compiles");
        assert!(
            rows.iter()
                .all(|row| row.stop_reason != "prefix_unsupported"),
            "{body:02x?}: no prefix refusal row"
        );
    }
    let case = Case::middle("on counter", Mode::Real, &[A32, 0x89, 0x03]);
    let mut rig = prepare(&case);
    let _ = jit::direct::compile(&mut rig.native, ENTRY, false);
    assert_eq!(
        rig.native
            .perf_counters()
            .jit_direct_addr32_word_block_slots,
        1
    );
}

/// Catches: an allowlist that is too wide, and an admission that reaches the CS rule, LOCK and
/// REP. Each form is refused at the same slot with the knob on.
#[test]
fn the_refusals_survive_with_the_knob_on() {
    let _knob = Knob::set(true);
    jit::direct::set_generic_callout_for_test(Some(true));
    jit::direct::set_v86_loop_rows_for_test(Some(false));
    // LOCK stops the walk before the prefix check, so it has no census row.
    for (name, body, attributed) in [
        ("LOCK", vec![0xf0, A32, 0x01, 0x03], false),
        ("REP movs", vec![0xf3, A32, 0xa5], true),
        ("add [edi],ax", vec![A32, 0x01, 0x07], true),
        ("loop", vec![A32, 0xe2, 0xfe], true),
        ("moffs", vec![A32, 0xa1, 0x10, 0x00, 0x00, 0x00], true),
        ("les", vec![A32, 0xc4, 0x03], true),
        ("push [edi]", vec![A32, 0xff, 0x37], true),
        ("xlat", vec![A32, 0xd7], true),
        ("CS override", vec![0x2e, A32, 0x8b, 0x07], true),
    ] {
        let (length, rows) = compile_census(Mode::Real, middle_insns(&body));
        assert_eq!(
            length,
            Some(3),
            "{name}: the walk stops at the refused slot"
        );
        assert!(
            !attributed
                || rows
                    .iter()
                    .any(|row| row.stop_reason == "prefix_unsupported"),
            "{name}: the stop is a prefix refusal"
        );
    }
    jit::direct::set_v86_loop_rows_for_test(None);
}

/// `F6 /1` is not on the native list: the classifier returns no kind for it, so it stays a hard
/// boundary and not an admission.
#[test]
fn group3_slash_one_stays_a_hard_boundary() {
    let _knob = Knob::set(true);
    let (length, rows) = compile_census(Mode::Real, middle_insns(&[A32, 0xf6, 0x0b, 0x5a]));
    assert_eq!(length, Some(3));
    let row = rows
        .iter()
        .find(|row| row.opcode == 0xf6)
        .expect("the stop is attributed");
    assert_eq!(row.stop_reason, "hard_boundary");
}

// ---------------------------------------------------------------------------------------------
// o. Byte identity
// ---------------------------------------------------------------------------------------------

/// Unprefixed blocks must compile to the same native bytes with the knob on and off, in 16-bit
/// and in 32-bit code.
#[test]
fn unprefixed_blocks_compile_to_the_same_bytes_with_the_knob_on_and_off() {
    let program16 = vec![
        FILL_A.to_vec(),
        vec![0x8b, 0x07],
        vec![0x89, 0x47, 0x04],
        vec![0x8d, 0x41, 0x02],
        vec![0xf6, 0x07, 0x5a],
        FILL_B.to_vec(),
    ];
    let program32 = vec![
        vec![0x8b, 0x03],
        vec![0x89, 0x43, 0x04],
        vec![0x8d, 0x41, 0x02],
        vec![0x40],
    ];
    for (mode, insns) in [
        (Mode::Real, program16.clone()),
        (Mode::Pm16, program16),
        (Mode::Flat32, program32),
    ] {
        // Each compile allocates new cells and writes their host addresses into the code as 64-bit
        // immediates. Those exact values are cleared before the compare; every other byte must
        // be the same.
        let case = Case::new(format!("{mode:?} identity"), mode, insns.clone());
        let mut rig = prepare(&case);
        let mut compile = |on: bool| {
            let _knob = Knob::set(on);
            match jit::direct::compile(&mut rig.native, ENTRY, mode.d()) {
                jit::direct::CompileOutcome::Compiled(compilation) => {
                    let (code, cleared) =
                        super::sixteen_bit::code_without_cell_addresses(&compilation);
                    (compilation.span.instructions, code, cleared)
                }
                _ => panic!("{mode:?}: the unprefixed block must compile"),
            }
        };
        let _warm_up = compile(false);
        let off = compile(false);
        let on = compile(true);
        assert_eq!(off.0, on.0, "{mode:?}: the slot count moved");
        assert!(off.2 > 0, "{mode:?}: the code holds its cell addresses");
        assert_eq!(off.2, on.2, "{mode:?}: the number of cell addresses moved");
        assert_eq!(off.1.len(), on.1.len(), "{mode:?}: the code length moved");
        if let Some(k) = (0..off.1.len()).find(|&k| off.1[k] != on.1[k]) {
            panic!("{mode:?}: the emitted byte {k} moved with the knob");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// q. Self-modifying store
// ---------------------------------------------------------------------------------------------

/// A prefixed store into the block's own code page takes the code-watch exit, and the
/// interpreter then writes the bytes.
#[test]
fn a_prefixed_store_into_the_code_page_takes_the_code_watch_exit() {
    let _knob = Knob::set(true);
    let case = Case::middle("smc store", Mode::Real, &[A32, 0x89, 0x03])
        .gpr(3, ENTRY + 6)
        .tweak(ds_over_code)
        .side_exit_at(1);
    // The Real fixture keeps DS at selector 0x1000, so `ds_over_code` moves its base to 0.
    let run = execute(&case);
    assert_eq!(
        run.code_watch_exits, 1,
        "the store must take the code-watch exit"
    );
}

// ---------------------------------------------------------------------------------------------
// The live data segment arms and the 64K wrap
// ---------------------------------------------------------------------------------------------

/// DS at 0x10800, so that the top of a 64K segment is inside one RAM page and the page guard
/// cannot hide a wrong limit check.
fn ds_mid_page(cpu: &mut CpuGsw) {
    let selector = (DS_BASE + 0x800) >> 4;
    let mut register = cpu.registers.segment(SegmentIndex::Ds);
    register.selector = selector as u16;
    register.base = DS_BASE + 0x800;
    cpu.registers.set_segment(SegmentIndex::Ds, register);
}

/// Forces one arm of the live data segment switch for the length of a test.
struct ArmKnob;

impl ArmKnob {
    fn set(arm: u8) -> Self {
        jit::direct::set_live_data_segments_for_test(Some(arm));
        Self
    }
}

impl Drop for ArmKnob {
    fn drop(&mut self) {
        jit::direct::set_live_data_segments_for_test(None);
    }
}

/// A 0x67 word access at EBX = 0xFFFFFFFF has no 64K mask, so the live limit check must keep
/// the 32-bit sequence and send the access to the interpreter, which faults.
///
/// Catches: a short limit check that trusts the block wrap and not the wrap of the address.
#[test]
fn a_prefixed_word_at_the_top_of_the_address_space_does_not_run_natively_in_any_arm() {
    let _knob = Knob::set(true);
    for arm in [0u8, 1, 2] {
        let _arm = ArmKnob::set(arm);
        for mode in [Mode::Real, Mode::V86] {
            for (name, body) in [
                ("word load", vec![A32, 0x8b, 0x03]),
                ("word store", vec![A32, 0x89, 0x03]),
                ("byte test", vec![A32, 0xf6, 0x03, 0x5a]),
            ] {
                let case = Case::middle(format!("arm {arm} {mode:?} {name}"), mode, &body)
                    .gpr(3, 0xffff_ffff)
                    .tweak(ds_mid_page)
                    .exit_at(1);
                execute(&case);
            }
        }
    }
}

/// One access through `[bx]` at each width, at the offsets around the top of a 64K segment with
/// limit 0xFFFF: 0xFFFF, limit - (bytes - 1) and limit - (bytes - 2). The access is native when
/// it ends inside the limit and exits to the interpreter otherwise.
///
/// Catches: a short limit check that adds the extent after the compare, or compares the start
/// and not the last byte.
#[test]
fn accesses_at_the_top_of_a_sixty_four_k_segment_match_the_interpreter_in_every_arm() {
    let _knob = Knob::set(true);
    // (name, instruction bytes, access width, the access is native-eligible)
    let widths: [(&str, &[u8], u32, bool); 4] = [
        ("byte", &[0xf6, 0x07, 0x5a], 1, true),
        ("word", &[0x8b, 0x07], 2, true),
        ("dword", &[O32, 0x8b, 0x07], 4, true),
        ("qword", &[O32, 0xdd, 0x07], 8, false),
    ];
    for arm in [0u8, 1, 2] {
        let _arm = ArmKnob::set(arm);
        for mode in SIXTEEN_BIT_MODES {
            for (name, body, bytes, plain) in widths {
                let mut offsets = vec![0xffffu32];
                offsets.push(0xffff - (bytes - 1));
                if bytes >= 2 {
                    offsets.push(0xffff - (bytes - 2));
                }
                for offset in offsets {
                    let label = format!("arm {arm} {mode:?} {name} at {offset:#x}");
                    let case = Case::middle(label, mode, body)
                        .gpr(3, offset)
                        .tweak(ds_mid_page);
                    let fits = offset + bytes - 1 <= 0xffff;
                    let case = if !plain {
                        case.any()
                    } else if fits {
                        case
                    } else {
                        case.exit_at(1)
                    };
                    execute(&case);
                }
            }
        }
    }
}

fn ds_mid_page_limit_0(cpu: &mut CpuGsw) {
    ds_mid_page(cpu);
    set_limit(cpu, SegmentIndex::Ds, 0);
}

fn ds_mid_page_limit_1(cpu: &mut CpuGsw) {
    ds_mid_page(cpu);
    set_limit(cpu, SegmentIndex::Ds, 1);
}

fn ds_mid_page_limit_1ffff(cpu: &mut CpuGsw) {
    ds_mid_page(cpu);
    set_limit(cpu, SegmentIndex::Ds, 0x1_ffff);
}

/// One access through a 0x67 `[ebx]` at each width, around a real-mode DS limit of 0, 1 and
/// 0x1FFFF: at the limit, limit - (bytes - 1) and limit - (bytes - 2). Without a 64K mask the live
/// check is the 32-bit sequence. The access is native when it ends inside the limit and exits to
/// the interpreter otherwise.
///
/// Catches: a 32-bit live check that misses a limit below the extent, or compares the start and
/// not the last byte.
#[test]
fn dword_address_accesses_at_a_real_mode_limit_match_the_interpreter_in_every_arm() {
    let _knob = Knob::set(true);
    let widths: [(&str, &[u8], u32); 3] = [
        ("byte", &[A32, 0xf6, 0x03, 0x5a], 1),
        ("word", &[A32, 0x8b, 0x03], 2),
        ("dword", &[O32, A32, 0x8b, 0x03], 4),
    ];
    type Tweak = fn(&mut CpuGsw);
    let limits: [(u32, Tweak); 3] = [
        (0, ds_mid_page_limit_0),
        (1, ds_mid_page_limit_1),
        (0x1_ffff, ds_mid_page_limit_1ffff),
    ];
    for arm in [0u8, 1, 2] {
        let _arm = ArmKnob::set(arm);
        for (limit, tweak) in limits {
            for (name, body, bytes) in widths {
                let mut offsets = vec![limit];
                for back in [bytes - 1, bytes.saturating_sub(2)] {
                    if let Some(offset) = limit.checked_sub(back) {
                        offsets.push(offset);
                    }
                }
                offsets.dedup();
                for offset in offsets {
                    let label = format!("arm {arm} limit {limit:#x} {name} at {offset:#x}");
                    let case = Case::middle(label, Mode::Real, body)
                        .gpr(3, offset)
                        .tweak(tweak);
                    let fits = offset + bytes - 1 <= limit;
                    execute(&if fits { case } else { case.exit_at(1) });
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The knob
// ---------------------------------------------------------------------------------------------

#[test]
fn the_knob_spelling_table_defaults_off() {
    let parse = jit::direct::parse_addr32_word_rows_arm_for_test;
    assert!(!parse(Err(std::env::VarError::NotPresent)));
    for off in ["", "0", "off", " OFF "] {
        assert!(!parse(Ok(off.to_string())), "{off:?}");
    }
    for on in ["1", "on", " On "] {
        assert!(parse(Ok(on.to_string())), "{on:?}");
    }
}

#[test]
#[should_panic(expected = "names no arm")]
fn the_knob_refuses_an_unknown_spelling() {
    jit::direct::parse_addr32_word_rows_arm_for_test(Ok("yes".to_string()));
}
