// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The live protected-mode data segment form (`IZARRAVM_LIVE_PM_DATA_SEGMENTS`).
//!
//! A 16-bit protected-mode block that holds a `MOV Sreg,r/m16` call-out reads ES, DS, FS, and GS
//! from the CPU state when the knob is on. The call-out can then resume after it loads a
//! different descriptor. Every row here runs the same bytes twice, once wholly interpreted and
//! once with the block installed, and compares state, memory, and clocks. Each row runs under both
//! arms of the knob. The knob is set before the CPU exists, because the cache reads it once.
//!
//! The fixtures reuse the tables, the fault handler, and the counters of the parent file. The code
//! segment is 16-bit and the stack segment is 16-bit.

use super::*;

/// Base 0x1900, limit 0xFF.
pub(crate) const SEL_A: u16 = 0x40;
/// Base 0x1A00, limit 0xFFFF.
pub(crate) const SEL_B: u16 = 0x48;
/// Base 0x1900, limit 0x7F.
pub(crate) const SEL_SHORT: u16 = 0x50;
/// Base 0x1900, limit 0x22. A dword at offset 0x20 crosses the limit.
pub(crate) const SEL_CROSS: u16 = 0x58;
/// Expand-down data, base 0x1900, limit 0xFF.
pub(crate) const SEL_EXPAND_DOWN: u16 = 0x60;
/// Readable code, base 0x1900, limit 0xFF.
pub(crate) const SEL_CODE_READ: u16 = 0x68;
/// G set, base 0, limit field 1, so the limit is 0x1FFF.
pub(crate) const SEL_GRAN: u16 = 0x70;
/// Base 0xFFFFF000, limit 0xFFFF. Offset 0x2A20 wraps to linear 0x1A20.
pub(crate) const SEL_HIGH: u16 = 0x78;
/// Data, not present.
pub(crate) const SEL_NOT_PRESENT_DATA: u16 = 0x80;
/// Data, base 0x1900, limit 0xFF, Accessed bit clear.
pub(crate) const SEL_NOT_ACCESSED: u16 = 0x88;
/// The LDT system descriptor.
pub(crate) const SEL_LDT: u16 = 0x90;
/// Data, DPL 3, base 0x1900, limit 0xFF. Load it with RPL 3.
pub(crate) const SEL_DPL3: u16 = 0x98;
/// Base 0x100, limit 0xFFFF. With offset 0x1920 it reaches linear 0x1A20, as SEL_B does at 0x20.
pub(crate) const SEL_LOW: u16 = 0xa0;
/// Index 0 of the LDT: data, base 0x1A00, limit 0xFFFF.
pub(crate) const SEL_IN_LDT: u16 = 0x04;
pub(crate) const LDT_BASE: u32 = 0x1300;

/// Offset of the first byte of the data pattern. The pattern is the same for every base.
pub(crate) const PATTERN_START: usize = 0x1900;

pub(crate) fn pattern_byte(linear: usize) -> u8 {
    ((linear * 7 + 3) & 0xff) as u8
}

/// One descriptor from its fields. `flags` is the high nibble of byte 6: G, D/B, L, AVL.
pub(crate) fn desc(base: u32, limit: u32, access: u8, flags: u8) -> [u8; 8] {
    let low = (limit & 0xffff) | (base << 16);
    let high = ((base >> 16) & 0xff)
        | (u32::from(access) << 8)
        | (((limit >> 16) & 0xf) << 16)
        | (u32::from(flags) << 20)
        | (base & 0xff00_0000);
    descriptor(low, high)
}

pub(crate) fn put(memory: &mut [u8], at: u32, bytes: [u8; 8]) {
    let at = at as usize;
    memory[at..at + 8].copy_from_slice(&bytes);
}

/// The guest memory image: the parent tables, the extra descriptors, the LDT, and the pattern.
pub(crate) fn image(selector_word: u16) -> Vec<u8> {
    let mut memory = vec![0u8; 0x2000];
    seed_protected_tables(&mut memory);
    put(
        &mut memory,
        IDT_BASE + 11 * 8,
        interrupt_gate(FAULT_HANDLER),
    );
    let gdt = |selector: u16| GDT_BASE + u32::from(selector);
    put(&mut memory, gdt(SEL_A), desc(0x1900, 0xff, 0x93, 0));
    put(&mut memory, gdt(SEL_B), desc(0x1a00, 0xffff, 0x93, 0));
    put(&mut memory, gdt(SEL_SHORT), desc(0x1900, 0x7f, 0x93, 0));
    put(&mut memory, gdt(SEL_CROSS), desc(0x1900, 0x22, 0x93, 0));
    put(
        &mut memory,
        gdt(SEL_EXPAND_DOWN),
        desc(0x1900, 0xff, 0x97, 0),
    );
    put(&mut memory, gdt(SEL_CODE_READ), desc(0x1900, 0xff, 0x9b, 0));
    put(&mut memory, gdt(SEL_LOW), desc(0x100, 0xffff, 0x93, 0));
    put(&mut memory, gdt(SEL_GRAN), desc(0, 0x1, 0x93, 0x8));
    put(
        &mut memory,
        gdt(SEL_HIGH),
        desc(0xffff_f000, 0xffff, 0x93, 0),
    );
    put(
        &mut memory,
        gdt(SEL_NOT_PRESENT_DATA),
        desc(0x1900, 0xff, 0x13, 0),
    );
    put(
        &mut memory,
        gdt(SEL_NOT_ACCESSED),
        desc(0x1900, 0xff, 0x92, 0),
    );
    put(&mut memory, gdt(SEL_LDT), desc(LDT_BASE, 0x1f, 0x82, 0));
    put(&mut memory, gdt(SEL_DPL3), desc(0x1900, 0xff, 0xf3, 0));
    put(&mut memory, LDT_BASE, desc(0x1a00, 0xffff, 0x93, 0));
    for (linear, byte) in memory.iter_mut().enumerate().skip(PATTERN_START) {
        *byte = pattern_byte(linear);
    }
    memory[POP_TARGET as usize..POP_TARGET as usize + 2]
        .copy_from_slice(&selector_word.to_le_bytes());
    memory
}

/// The four segments a `MOV Sreg` call-out loads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Seg {
    Es,
    Ds,
    Fs,
    Gs,
}

impl Seg {
    const ALL: [Seg; 4] = [Seg::Es, Seg::Ds, Seg::Fs, Seg::Gs];

    fn index(self) -> SegmentIndex {
        match self {
            Seg::Es => SegmentIndex::Es,
            Seg::Ds => SegmentIndex::Ds,
            Seg::Fs => SegmentIndex::Fs,
            Seg::Gs => SegmentIndex::Gs,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Seg::Es => "ES",
            Seg::Ds => "DS",
            Seg::Fs => "FS",
            Seg::Gs => "GS",
        }
    }

    /// The prefix that makes a memory operand use this segment. DS needs none.
    fn prefix(self) -> Option<u8> {
        match self {
            Seg::Es => Some(0x26),
            Seg::Ds => None,
            Seg::Fs => Some(0x64),
            Seg::Gs => Some(0x65),
        }
    }

    /// The ModRM `reg` field of this segment, shifted into place with `mod` 11 and `rm` AX.
    fn reg_ax(self) -> u8 {
        0xc0 | (self.field() << 3)
    }

    fn field(self) -> u8 {
        match self {
            Seg::Es => 0,
            Seg::Ds => 3,
            Seg::Fs => 4,
            Seg::Gs => 5,
        }
    }

    fn push(self) -> Vec<u8> {
        match self {
            Seg::Es => vec![0x06],
            Seg::Ds => vec![0x1e],
            Seg::Fs => vec![0x0f, 0xa0],
            Seg::Gs => vec![0x0f, 0xa8],
        }
    }
}

/// A program: the bytes, and the offset of every instruction that the block is expected to hold.
/// The final HLT is not in `starts`.
struct Prog {
    bytes: Vec<u8>,
    starts: Vec<u32>,
}

impl Prog {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            starts: Vec::new(),
        }
    }

    fn op(mut self, bytes: &[u8]) -> Self {
        self.starts.push(self.bytes.len() as u32);
        self.bytes.extend_from_slice(bytes);
        self
    }

    /// `mov Sreg,dx`.
    fn load_dx(self, seg: Seg) -> Self {
        self.op(&[0x8e, 0xc2 | (seg.field() << 3)])
    }

    /// `mov Sreg,ax`.
    fn load_ax(self, seg: Seg) -> Self {
        self.op(&[0x8e, seg.reg_ax()])
    }

    /// `mov Sreg,[bx]`.
    fn load_mem(self, seg: Seg) -> Self {
        self.op(&[0x8e, 0x07 | (seg.field() << 3)])
    }

    /// One instruction with the segment prefix of `seg` in front.
    fn op_in(self, seg: Seg, bytes: &[u8]) -> Self {
        let mut full: Vec<u8> = seg.prefix().into_iter().collect();
        full.extend_from_slice(bytes);
        self.op(&full)
    }

    fn byte_load(self, seg: Seg) -> Self {
        self.op_in(seg, &[0x8a, 0x05]) // mov al,[di]
    }

    fn word_load(self, seg: Seg) -> Self {
        self.op_in(seg, &[0x8b, 0x0d]) // mov cx,[di]
    }

    fn dword_load(self, seg: Seg) -> Self {
        self.op_in(seg, &[0x66, 0x8b, 0x2d]) // mov ebp,[di]
    }

    fn byte_store(self, seg: Seg) -> Self {
        self.op_in(seg, &[0x88, 0x04]) // mov [si],al
    }

    fn word_store(self, seg: Seg) -> Self {
        self.op_in(seg, &[0x89, 0x0c]) // mov [si],cx
    }

    fn dword_store(self, seg: Seg) -> Self {
        self.op_in(seg, &[0x66, 0x89, 0x2c]) // mov [si],ebp
    }

    /// A byte, word, and dword load and store through `seg`.
    fn probe(self, seg: Seg) -> Self {
        self.byte_load(seg)
            .word_load(seg)
            .dword_load(seg)
            .byte_store(seg)
            .word_store(seg)
            .dword_store(seg)
    }

    /// `count` NOPs. A block of fewer than a handful of instructions is a structural reject.
    fn nops(mut self, count: usize) -> Self {
        for _ in 0..count {
            self = self.op(&[0x90]);
        }
        self
    }

    /// A shorter probe: a byte load, a dword load, and a word store.
    fn short_probe(self, seg: Seg) -> Self {
        self.byte_load(seg).dword_load(seg).word_store(seg)
    }

    fn insns(&self) -> u8 {
        self.starts.len() as u8
    }

    /// Append the HLT that ends both legs.
    fn finish(mut self) -> Self {
        self.bytes.push(0xf4);
        self
    }
}

/// The program, the register seeds, and the state tweaks of one row.
struct Case {
    prog: Prog,
    /// The selector in DX, and in the word at `[bx]`.
    dx: u16,
    /// The selector in AX, for a second load.
    ax: u16,
    di: u32,
    si: u32,
    cpl3: bool,
    perturb: fn(&mut CpuGsw, &mut TestBus),
    /// Run exactly the block's instructions on the interpreter leg, and no HLT. Needed at CPL 3,
    /// where the HLT would fault.
    steps_only: bool,
    /// The number of `InterpretOne` slots the block must hold.
    callouts: u8,
}

impl Case {
    fn new(mut prog: Prog, dx: u16, callouts: u8) -> Self {
        let short = MIN_ROW_INSNS.saturating_sub(prog.starts.len());
        prog = prog.nops(short);
        Self {
            prog: prog.finish(),
            dx,
            ax: SEL_DATA,
            di: 0x20,
            si: 0x80,
            cpl3: false,
            perturb: no_perturb,
            steps_only: false,
            callouts,
        }
    }

    fn offsets(mut self, di: u32, si: u32) -> Self {
        self.di = di;
        self.si = si;
        self
    }

    fn perturbed(mut self, perturb: fn(&mut CpuGsw, &mut TestBus)) -> Self {
        self.perturb = perturb;
        self
    }
}

/// The fewest instructions a fixture block holds. The walk refuses a shorter one.
const MIN_ROW_INSNS: usize = 5;

/// A 16-bit protected-mode CPU: a 16-bit code segment, a 16-bit stack segment, flat 4 GiB data
/// segments, the GDT of the parent file with room for the extra entries, and an LDT.
pub(crate) fn pm16_cpu(cpl3: bool) -> CpuGsw {
    let mut cpu = protected_cpu();
    let (selector, access) = if cpl3 { (0x0b, 0xfb) } else { (SEL_CODE, 0x9b) };
    let mut cs = SegmentRegister::flat(selector, access);
    cs.limit = 0xffff;
    cs.default_size_32 = false;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    let mut ss = SegmentRegister::flat(SEL_DATA, 0x93);
    ss.limit = 0xffff;
    ss.default_size_32 = false;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    cpu.gdtr.limit = 0xa7;
    cpu.ldtr = SegmentRegister {
        selector: SEL_LDT,
        base: LDT_BASE,
        limit: 0x1f,
        access: 0x82,
        default_size_32: false,
    };
    cpu.cpl = if cpl3 { 3 } else { 0 };
    cpu
}

/// The linear of every instruction, and of the final HLT, so the walk finds a decode for each.
fn warm_list(prog: &Prog) -> Vec<u32> {
    let mut linears: Vec<u32> = prog.starts.iter().map(|offset| ENTRY + offset).collect();
    linears.push(ENTRY + prog.bytes.len() as u32 - 1);
    linears
}

fn arm(cpu: &mut CpuGsw, bus: &mut TestBus, case: &Case) {
    arm_protected(cpu, bus, case.dx);
    cpu.registers.set_eax(u32::from(case.ax));
    cpu.registers.set_ebx(POP_TARGET);
    cpu.registers.set_edi(case.di);
    cpu.registers.set_esi(case.si);
}

/// The knob, forced for the CPUs built while the guard lives.
pub(crate) struct Knob;

impl Knob {
    pub(crate) fn set(on: bool) -> Self {
        jit::direct::set_live_pm_data_segments_for_test(Some(on));
        Knob
    }
}

impl Drop for Knob {
    fn drop(&mut self) {
        jit::direct::set_live_pm_data_segments_for_test(None);
    }
}

/// What one run of a case produced.
struct Ran {
    legs: Legs,
    /// Instructions in the compiled block.
    compiled_insns: u8,
    /// The segments the entry check still compares.
    used: u8,
    /// The block publishes no successor.
    no_successors: bool,
}

/// Run the interpreter to the end of the program. A case that stops before the HLT steps one
/// instruction at a time until the block's instruction count has retired since `start`.
fn run_interpreted(cpu: &mut CpuGsw, bus: &mut TestBus, case: &Case, start: u64) {
    if !case.steps_only {
        drive(cpu, bus);
        return;
    }
    let wanted = u64::from(case.prog.insns());
    while cpu.perf_counters().instructions - start < wanted {
        let before = cpu.perf_counters().instructions;
        cpu.run_straight_line(bus, 1)
            .expect("a stepped instruction must not stop the machine");
        assert_eq!(
            cpu.perf_counters().instructions,
            before + 1,
            "one clock of budget must retire one instruction"
        );
    }
}

/// Run `case` under one arm of the knob. `full` says the block must hold every instruction of the
/// program: it is false for a row whose OFF arm ends the block early on purpose.
fn run_case(case: &Case, on: bool, full: bool) -> Ran {
    let _knob = Knob::set(on);
    let memory = image(case.dx);
    let mut program = memory.clone();
    program[ENTRY as usize..ENTRY as usize + case.prog.bytes.len()]
        .copy_from_slice(&case.prog.bytes);
    let mut interp_bus = sixteen_bit_bus(program.clone());
    let mut interp = pm16_cpu(case.cpl3);
    (case.perturb)(&mut interp, &mut interp_bus);
    arm(&mut interp, &mut interp_bus, case);
    let interp_start = interp.perf_counters().instructions;
    run_interpreted(&mut interp, &mut interp_bus, case, interp_start);

    let mut native_bus = sixteen_bit_bus(program);
    let mut native = pm16_cpu(case.cpl3);
    arm_native_sixteen_bit(&mut native, &mut native_bus, &[0x0000, DATA_PAGE]);
    (case.perturb)(&mut native, &mut native_bus);
    let linears = warm_list(&case.prog);
    warm_sixteen_bit(&mut native, &mut native_bus, &linears);
    let compilation = match jit::direct::compile(&mut native, ENTRY, false) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::Retry(cause) => {
            panic!(
                "the fixture walk gave up: {cause:?}, knob {on}, bytes {:02x?}",
                case.prog.bytes
            )
        }
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!(
                "the fixture block was a structural reject, knob {on}, bytes {:02x?}",
                case.prog.bytes
            )
        }
    };
    if full {
        assert_eq!(
            compilation.span.instructions,
            case.prog.insns(),
            "the block stopped early, knob {on}, bytes {:02x?}",
            case.prog.bytes
        );
        assert_eq!(compilation.callout_interpret_one_slots, case.callouts);
    }
    let compiled_insns = compilation.span.instructions;
    let used = compilation.pinned_segments_for_test();
    let key = jit::direct::key_for(&native, ENTRY, false).expect("a key for the fixture block");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("install the fixture block");
    let block = native.jit_direct.block(id).expect("the block must be live");
    let no_successors = block.is_segment_write_block();
    arm(&mut native, &mut native_bus, case);
    let before = native.perf_counters().jit_direct_insns;
    #[cfg(feature = "timing-class-histogram")]
    let native_histogram_before_entry = native.timing_class_histogram_snapshot();
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .expect("the fixture block must not stop the machine"),
        "the installed block must actually run"
    );
    let native_insns = native.perf_counters().jit_direct_insns - before;
    let exit_reason = native.jit_direct.last_side_exit_reason_for_test();
    #[cfg(feature = "timing-class-histogram")]
    let native_histogram_after_entry = native.timing_class_histogram_snapshot();
    run_interpreted(&mut native, &mut native_bus, case, before);
    let mut legs = Legs {
        interp,
        interp_bus,
        native,
        native_bus,
        exit_reason,
        native_insns,
        #[cfg(feature = "timing-class-histogram")]
        native_histogram_before_entry,
        #[cfg(feature = "timing-class-histogram")]
        native_histogram_after_entry,
    };
    assert_legs_agree(&mut legs);
    Ran {
        legs,
        compiled_insns,
        used,
        no_successors,
    }
}

fn mov_sreg_counts(ran: &Ran) -> crate::InterpretOneRowCounts {
    row_counts(&ran.legs.native, "0x8e_mov_sreg")
}

/// The block ran to its end with every call-out resumed.
fn assert_resumed(ran: &Ran, label: &str, loads: u64) {
    let counts = mov_sreg_counts(ran);
    assert_eq!(ran.legs.exit_reason, None, "{label}: the block must finish");
    assert_eq!(
        ran.legs.native_insns,
        u64::from(ran.compiled_insns),
        "{label}: every slot must run natively"
    );
    assert_eq!(counts.executed, loads, "{label}: call-outs executed");
    assert_eq!(counts.resync, 0, "{label}: resyncs");
    assert_eq!(counts.demoted, 0, "{label}: demotions");
}

fn assert_resynced_on_segment(ran: &Ran, label: &str) {
    let counts = mov_sreg_counts(ran);
    assert_eq!(
        ran.legs.exit_reason,
        Some(jit::direct::SideExitReason::CallOutResync as u32),
        "{label}: the call-out must resync"
    );
    assert_eq!(counts.resync, 1, "{label}: resyncs");
    assert_eq!(
        counts.resync_reasons[reason_index("segment")],
        1,
        "{label}: the segment clause"
    );
}

fn live_pm_exits(ran: &Ran) -> u64 {
    ran.legs
        .native
        .direct_stall_snapshot()
        .side_exit_live_pm_access
}

fn segment_limit_exits(ran: &Ran) -> u64 {
    ran.legs
        .native
        .direct_stall_snapshot()
        .side_exit_segment_limit
}

// ---------------------------------------------------------------------------
// 1. Both arms, with and without a segment load.
// ---------------------------------------------------------------------------

/// The compilation of `case` under one arm, for the rows that compare the emitted bytes.
fn compile_under(case: &Case, on: bool) -> jit::direct::Compilation {
    let _knob = Knob::set(on);
    let mut program = image(case.dx);
    program[ENTRY as usize..ENTRY as usize + case.prog.bytes.len()]
        .copy_from_slice(&case.prog.bytes);
    let mut bus = sixteen_bit_bus(program);
    let mut cpu = pm16_cpu(false);
    arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000, DATA_PAGE]);
    warm_sixteen_bit(&mut cpu, &mut bus, &warm_list(&case.prog));
    jit::direct::compile(&mut cpu, ENTRY, false).expect("the fixture must compile")
}

/// A block without a segment load compiles to the same bytes under both arms.
///
/// The bytes are compared with the link cell addresses cleared, because two compilations of one
/// program hold different cell addresses. The pinned segment set and the differential results
/// are checked as well.
#[test]
fn a_block_without_a_segment_load_is_the_same_under_both_arms() {
    let program = || {
        Case::new(
            Prog::new()
                .byte_load(Seg::Ds)
                .word_load(Seg::Es)
                .dword_load(Seg::Fs)
                .byte_store(Seg::Gs),
            SEL_DATA,
            0,
        )
        .offsets(0x1920, 0x1a80)
    };
    let off_code = compile_under(&program(), false);
    let on_code = compile_under(&program(), true);
    assert_eq!(off_code.callout_interpret_one_slots, 0);
    assert_eq!(off_code.code.len(), on_code.code.len());
    assert_eq!(
        super::super::sixteen_bit::code_without_cell_addresses(&off_code),
        super::super::sixteen_bit::code_without_cell_addresses(&on_code),
        "the knob must not move a byte of a block without a segment load"
    );
    assert_eq!(
        off_code.pinned_segments_for_test(),
        on_code.pinned_segments_for_test()
    );
    let off = run_case(&program(), false, true);
    let on = run_case(&program(), true, true);
    assert_eq!(off.compiled_insns, on.compiled_insns);
    assert_eq!(off.used, on.used, "the entry check must not change");
    assert_eq!(
        off.legs.native.registers.ebp(),
        on.legs.native.registers.ebp()
    );
}

/// The same program with a DS load: the OFF arm resyncs when the descriptor changes, the ON arm
/// resumes. This is the twin of `interpret_one_protected_mode_segment_change_resyncs` for a
/// 16-bit block.
#[test]
fn a_ds_load_resyncs_when_off_and_resumes_when_on() {
    let case = || Case::new(Prog::new().load_dx(Seg::Ds).probe(Seg::Ds), SEL_B, 1);
    let off = run_case(&case(), false, true);
    assert_resynced_on_segment(&off, "OFF");
    assert_eq!(mov_sreg_counts(&off).executed, 1);
    assert_ne!(off.used & segment_bit_of(Seg::Ds), 0, "OFF pins DS");
    let on = run_case(&case(), true, true);
    assert_resumed(&on, "ON", 1);
    assert_eq!(on.used & segment_bit_of(Seg::Ds), 0, "ON reads DS live");
    assert!(
        on.no_successors,
        "a block with a load publishes no successor"
    );
    assert!(off.no_successors);
    assert_eq!(
        on.legs.native.registers.ebp(),
        u32::from_le_bytes([0x1a20usize, 0x1a21, 0x1a22, 0x1a23].map(pattern_byte)),
        "the dword load must read through the new base"
    );
    assert_eq!(live_pm_exits(&on), 0);
}

fn segment_bit_of(seg: Seg) -> u8 {
    1 << seg.index().index()
}

// ---------------------------------------------------------------------------
// 2. Every segment, both forms, every width.
// ---------------------------------------------------------------------------

#[test]
fn every_segment_loads_by_register_and_reads_the_new_base() {
    for seg in Seg::ALL {
        let case = || Case::new(Prog::new().load_dx(seg).probe(seg), SEL_B, 1);
        let off = run_case(&case(), false, true);
        assert_resynced_on_segment(&off, seg.name());
        let on = run_case(&case(), true, true);
        assert_resumed(&on, seg.name(), 1);
        assert_eq!(
            on.legs.native.registers.ebp(),
            u32::from_le_bytes([0x1a20usize, 0x1a21, 0x1a22, 0x1a23].map(pattern_byte)),
            "{}: the load must use the new base",
            seg.name()
        );
        assert_eq!(
            on.legs.native_bus.memory[0x1a80],
            pattern_byte(0x1a20),
            "{}: the store must use the new base",
            seg.name()
        );
    }
}

#[test]
fn every_segment_loads_from_memory_and_reads_the_new_base() {
    for seg in Seg::ALL {
        let case = || Case::new(Prog::new().load_mem(seg).probe(seg), SEL_B, 1);
        let on = run_case(&case(), true, true);
        assert_resumed(&on, seg.name(), 1);
        assert_eq!(
            on.legs.native.registers.ebp(),
            u32::from_le_bytes([0x1a20usize, 0x1a21, 0x1a22, 0x1a23].map(pattern_byte)),
            "{}: the load must use the new base",
            seg.name()
        );
        let off = run_case(&case(), false, true);
        assert_resynced_on_segment(&off, seg.name());
    }
}

#[test]
fn the_same_selector_reloaded_resumes_on_both_arms() {
    for on in [false, true] {
        let case = Case::new(Prog::new().load_dx(Seg::Ds).probe(Seg::Ds), SEL_DATA, 1)
            .offsets(0x1920, 0x1a80);
        let ran = run_case(&case, on, true);
        assert_resumed(&ran, if on { "ON" } else { "OFF" }, 1);
    }
}

// ---------------------------------------------------------------------------
// 3. Limits, null, not present, expand-down, readable code.
// ---------------------------------------------------------------------------

#[test]
fn an_access_past_a_shorter_limit_faults_like_the_interpreter() {
    for on in [false, true] {
        // Loads at 0x20 fit in 0x7F. The byte store at 0x80 does not.
        let case = Case::new(Prog::new().load_dx(Seg::Ds).probe(Seg::Ds), SEL_SHORT, 1);
        let ran = run_case(&case, on, true);
        if on {
            assert_eq!(segment_limit_exits(&ran), 1);
            assert_eq!(mov_sreg_counts(&ran).resync, 0);
        }
    }
}

#[test]
fn an_access_that_crosses_the_limit_faults_like_the_interpreter() {
    for on in [false, true] {
        // The byte and word loads at 0x20 fit in 0x22. The dword load does not.
        let case = Case::new(Prog::new().load_dx(Seg::Ds).probe(Seg::Ds), SEL_CROSS, 1);
        let ran = run_case(&case, on, true);
        if on {
            assert_eq!(segment_limit_exits(&ran), 1);
            assert_eq!(mov_sreg_counts(&ran).resync, 0);
        }
    }
}

#[test]
fn a_null_selector_then_an_access_faults_through_the_side_exit() {
    for seg in Seg::ALL {
        for on in [false, true] {
            let case = Case::new(Prog::new().load_dx(seg).probe(seg), 0, 1);
            let ran = run_case(&case, on, true);
            if on {
                assert_eq!(
                    live_pm_exits(&ran),
                    1,
                    "{}: the access check exit",
                    seg.name()
                );
                assert_eq!(
                    ran.legs.exit_reason,
                    Some(jit::direct::SideExitReason::LivePmAccess as u32)
                );
                assert_eq!(mov_sreg_counts(&ran).resync, 0);
            }
        }
    }
}

#[test]
fn a_not_present_descriptor_raises_np_from_the_call_out() {
    for on in [false, true] {
        let case = Case::new(
            Prog::new().load_dx(Seg::Es).probe(Seg::Es),
            SEL_NOT_PRESENT_DATA,
            1,
        );
        let ran = run_case(&case, on, true);
        let counts = mov_sreg_counts(&ran);
        assert_eq!(counts.resync_fault, 1);
        assert_eq!(counts.resync_reasons[reason_index("fault")], 1);
        assert_eq!(counts.demoted, 0);
        assert_eq!(
            ran.legs.exit_reason,
            Some(jit::direct::SideExitReason::CallOutResyncFault as u32)
        );
    }
}

#[test]
fn an_expand_down_segment_leaves_through_the_access_check() {
    for on in [false, true] {
        // Offsets below the limit are invalid for expand-down. The interpreter faults.
        let low = Case::new(
            Prog::new().load_dx(Seg::Ds).probe(Seg::Ds),
            SEL_EXPAND_DOWN,
            1,
        );
        let ran = run_case(&low, on, true);
        if on {
            assert_eq!(live_pm_exits(&ran), 1);
        }
        // Offsets above the limit are valid. The block still leaves and the interpreter serves.
        let high = Case::new(
            Prog::new().load_dx(Seg::Ds).probe(Seg::Ds),
            SEL_EXPAND_DOWN,
            1,
        )
        .offsets(0x120, 0x180);
        let ran = run_case(&high, on, true);
        if on {
            assert_eq!(live_pm_exits(&ran), 1);
        }
    }
}

#[test]
fn a_write_through_a_readable_code_segment_faults() {
    for on in [false, true] {
        let case = Case::new(
            Prog::new().load_dx(Seg::Ds).probe(Seg::Ds),
            SEL_CODE_READ,
            1,
        );
        let ran = run_case(&case, on, true);
        if on {
            assert_eq!(
                live_pm_exits(&ran),
                1,
                "the byte store must leave the block"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 4. Granularity, wrap, LDT.
// ---------------------------------------------------------------------------

#[test]
fn a_page_granular_limit_scales_and_bounds_the_access() {
    for on in [false, true] {
        let inside = Case::new(Prog::new().load_dx(Seg::Ds).probe(Seg::Ds), SEL_GRAN, 1)
            .offsets(0x1920, 0x1a80);
        let ran = run_case(&inside, on, true);
        if on {
            assert_resumed(&ran, "G=1 inside", 1);
        }
        // Limit 0x1FFF: offset 0x2000 is outside, and 0x1FFE crosses for a dword.
        for di in [0x2000u32, 0x1ffe] {
            let past = Case::new(Prog::new().load_dx(Seg::Ds).probe(Seg::Ds), SEL_GRAN, 1)
                .offsets(di, 0x1a80);
            let ran = run_case(&past, on, true);
            if on {
                assert_eq!(segment_limit_exits(&ran), 1, "di {di:#x}");
            }
        }
    }
}

#[test]
fn a_base_near_four_gib_wraps_like_the_interpreter() {
    for on in [false, true] {
        let case = Case::new(Prog::new().load_dx(Seg::Gs).probe(Seg::Gs), SEL_HIGH, 1)
            .offsets(0x2a20, 0x2a80);
        let ran = run_case(&case, on, true);
        if on {
            assert_resumed(&ran, "wrap", 1);
            assert_eq!(
                ran.legs.native.registers.ebp(),
                u32::from_le_bytes([0x1a20usize, 0x1a21, 0x1a22, 0x1a23].map(pattern_byte))
            );
        }
    }
}

#[test]
fn an_ldt_selector_loads_and_reads_the_new_base() {
    for on in [false, true] {
        let case = Case::new(Prog::new().load_dx(Seg::Fs).probe(Seg::Fs), SEL_IN_LDT, 1);
        let ran = run_case(&case, on, true);
        if on {
            assert_resumed(&ran, "LDT", 1);
        }
    }
}

// ---------------------------------------------------------------------------
// 5. The Accessed bit.
// ---------------------------------------------------------------------------

#[test]
fn a_load_that_sets_the_accessed_bit_resumes_on_a_plain_data_page() {
    let case = || {
        Case::new(
            Prog::new().load_dx(Seg::Ds).byte_load(Seg::Ds),
            SEL_NOT_ACCESSED,
            1,
        )
    };
    let on = run_case(&case(), true, true);
    assert_resumed(&on, "unaccessed", 1);
    assert_eq!(
        on.legs.native_bus.memory[(GDT_BASE + u32::from(SEL_NOT_ACCESSED) + 5) as usize] & 1,
        1,
        "the load must have written the Accessed bit"
    );
    run_case(&case(), false, true);
}

#[test]
fn a_load_that_writes_a_watched_descriptor_page_resyncs_on_the_deferred_write() {
    fn watch_the_descriptor(cpu: &mut CpuGsw, _: &mut TestBus) {
        cpu.mark_block_code_for_test(GDT_BASE + u32::from(SEL_NOT_ACCESSED), 8);
    }
    for on in [false, true] {
        let case = Case::new(
            Prog::new().load_dx(Seg::Ds).byte_load(Seg::Ds),
            SEL_NOT_ACCESSED,
            1,
        )
        .perturbed(watch_the_descriptor);
        let ran = run_case(&case, on, true);
        let counts = mov_sreg_counts(&ran);
        assert_eq!(counts.resync, 1);
        assert_eq!(
            counts.resync_reasons[reason_index("deferred_code_writes")]
                + counts.resync_reasons[reason_index("segment")],
            1,
            "the resync names one reason"
        );
        if on {
            assert_eq!(
                counts.resync_reasons[reason_index("deferred_code_writes")],
                1,
                "the live form leaves only the deferred write to refuse"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 6. Where the load sits, and what reads the selector afterwards.
// ---------------------------------------------------------------------------

#[test]
fn the_load_may_be_first_middle_or_last() {
    for on in [false, true] {
        let middle = Case::new(
            Prog::new()
                .byte_load(Seg::Ds)
                .load_dx(Seg::Ds)
                .short_probe(Seg::Ds),
            SEL_LOW,
            1,
        )
        .offsets(0x1920, 0x1a80);
        let ran = run_case(&middle, on, true);
        if on {
            assert_resumed(&ran, "middle", 1);
        }
        let last = Case::new(
            Prog::new().nops(4).byte_load(Seg::Ds).load_dx(Seg::Ds),
            SEL_LOW,
            1,
        )
        .offsets(0x1920, 0x1a80);
        let ran = run_case(&last, on, true);
        if on {
            assert_resumed(&ran, "last", 1);
        }
    }
}

#[test]
fn two_loads_in_one_block_both_resume() {
    for on in [false, true] {
        let case = Case::new(
            Prog::new()
                .load_dx(Seg::Ds)
                .load_ax(Seg::Es)
                .byte_load(Seg::Ds)
                .word_store(Seg::Ds)
                .dword_load(Seg::Es)
                .byte_store(Seg::Es),
            SEL_B,
            2,
        );
        let case = Case { ax: SEL_A, ..case };
        let ran = run_case(&case, on, true);
        if on {
            assert_resumed(&ran, "two loads", 2);
        }
    }
}

#[test]
fn the_selector_reads_after_a_load_return_the_new_selector() {
    for seg in Seg::ALL {
        for on in [false, true] {
            // PUSH FS and PUSH GS are not native slots, so only ES and DS push.
            let pushes = matches!(seg, Seg::Es | Seg::Ds);
            let mut prog = Prog::new().load_dx(seg).op(&[0x8c, seg.reg_ax()]);
            if pushes {
                prog = prog.op(&seg.push());
            }
            let case = Case::new(prog, SEL_B, 1);
            let ran = run_case(&case, on, true);
            assert_eq!(
                ran.legs.native.registers.eax() & 0xffff,
                u32::from(SEL_B),
                "{}: MOV r16,Sreg",
                seg.name()
            );
            let sp = ran.legs.native.registers.esp() as usize;
            if pushes {
                assert_eq!(
                    u16::from_le_bytes([
                        ran.legs.native_bus.memory[sp],
                        ran.legs.native_bus.memory[sp + 1]
                    ]),
                    SEL_B,
                    "{}: PUSH Sreg",
                    seg.name()
                );
            }
            if on {
                assert_resumed(&ran, seg.name(), 1);
                assert_eq!(ran.used & segment_bit_of(seg), 0);
            }
        }
    }
}

#[test]
fn a_ss_load_in_the_same_block_stays_strict() {
    for on in [false, true] {
        // mov ds,dx; mov ss,ax; mov cx,[di]. AX names the flat 32-bit stack, which differs from
        // the 16-bit stack of the fixture, so the SS call-out resyncs.
        let case = Case::new(
            Prog::new()
                .load_dx(Seg::Ds)
                .op(&[0x8e, 0xd0])
                .byte_load(Seg::Ds),
            SEL_DATA,
            2,
        )
        .offsets(0x1920, 0x80);
        let ran = run_case(&case, on, true);
        let ss = row_counts(&ran.legs.native, "0x8e2_mov_ss");
        if on {
            assert_eq!(mov_sreg_counts(&ran).resync, 0);
            assert_eq!(ss.executed, 1);
            assert_eq!(ss.resync, 1, "MOV SS keeps the strict predicate");
        }
    }
}

// ---------------------------------------------------------------------------
// 7. The retire governor, and CPL 3.
// ---------------------------------------------------------------------------

#[test]
fn a_key_promoted_to_live_ss_keeps_ss_live() {
    let _knob = Knob::set(true);
    let case = Case::new(
        Prog::new()
            .load_dx(Seg::Ds)
            .op(&Seg::Ds.push())
            .byte_load(Seg::Ds),
        SEL_B,
        1,
    );
    let mut program = image(case.dx);
    program[ENTRY as usize..ENTRY as usize + case.prog.bytes.len()]
        .copy_from_slice(&case.prog.bytes);
    let compile = |promote: bool| {
        let mut bus = sixteen_bit_bus(program.clone());
        let mut cpu = pm16_cpu(false);
        arm_native_sixteen_bit(&mut cpu, &mut bus, &[0x0000, DATA_PAGE]);
        warm_sixteen_bit(&mut cpu, &mut bus, &warm_list(&case.prog));
        let key = jit::direct::key_for(&cpu, ENTRY, false).expect("a key");
        if promote {
            cpu.jit_direct.direct.promote_live_for_test(key, true);
        }
        let compilation = jit::direct::compile(&mut cpu, ENTRY, false).expect("compiles");
        compilation.pinned_segments_for_test()
    };
    let ss = 1u8 << SegmentIndex::Ss.index();
    assert_ne!(compile(false) & ss, 0, "SS is pinned without the governor");
    assert_eq!(
        compile(true) & ss,
        0,
        "the governor's live SS must stay live"
    );
    assert_eq!(compile(true) & segment_bit_of(Seg::Ds), 0);
}

#[test]
fn a_cpl3_block_takes_the_form_too() {
    for on in [false, true] {
        // The DPL 3 descriptor loads with RPL 3 at CPL 3. The interpreter leg stops after the
        // block, because the HLT is privileged.
        let mut case = Case::new(
            Prog::new()
                .load_dx(Seg::Ds)
                .byte_load(Seg::Ds)
                .word_load(Seg::Ds),
            SEL_DPL3 | 3,
            1,
        );
        case.cpl3 = true;
        case.steps_only = true;
        let ran = run_case(&case, on, true);
        if on {
            assert_resumed(&ran, "CPL 3", 1);
        } else {
            assert_resynced_on_segment(&ran, "CPL 3 OFF");
        }
    }
}

// ---------------------------------------------------------------------------
// 8. The knob.
// ---------------------------------------------------------------------------

#[test]
fn the_knob_spellings() {
    let parse =
        |value: &str| jit::direct::parse_live_pm_data_segments_for_test(Ok(value.to_string()));
    assert!(!jit::direct::parse_live_pm_data_segments_for_test(Err(
        std::env::VarError::NotPresent
    )));
    assert!(!parse(""));
    assert!(!parse("0"));
    assert!(parse("1"));
    assert!(parse(" 1 "));
    for bad in ["2", "on", "true", "off"] {
        let refused = std::panic::catch_unwind(|| {
            jit::direct::parse_live_pm_data_segments_for_test(Ok(bad.to_string()))
        });
        assert!(refused.is_err(), "{bad:?} must be refused");
    }
}
