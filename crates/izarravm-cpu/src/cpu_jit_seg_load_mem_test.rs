// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! L4 (first half): the memory-source segment-load family, `MOV Sreg, m16` (0x8E memory form,
//! `DirectKind::LoadSegRealMem`) and `LES`/`LDS` (0xC4/0xC5, `DirectKind::LesLds`).
//!
//! Real mode and V86 only, WORD operand size only. Pyramid's `0x8E` non_continuable rows are
//! 1,401,930 (#1) and 134,070 (#3); `0xC4`/`0xC5` never reached `classify` at all before this
//! slice, because `jit_admits_non_continuable` withheld them and every occurrence stopped block
//! compilation outright.
//!
//! The differential rows run the same guest bytes natively and through a BLOCK-FREE interpreter
//! from identical seeded state and compare registers (segment registers included), materialized
//! EFLAGS, the halt latch, clocks and the whole of guest RAM -- the shape
//! `cpu_jit_v86_loop_rows_test.rs` established. `Seed::stale_segments` gives ES and DS an
//! already-real-mode-INCOMPATIBLE descriptor (an unreal-mode limit, a code access byte,
//! `default_size_32` true) before the run, so a lowering that dropped the access store, or that
//! stored a limit where `LoadSegReal` never does, is observable rather than accidentally agreeing
//! with a role that started from the value it should have written.

use super::*;

const ENTRY: u32 = 0x100;
/// The far pointer's OFFSET field, word-aligned so `addr + 2` lands cleanly on the selector.
const OPERAND: u16 = 0x0220;
const CS_BASE: u32 = 0x0000;
const DS_BASE: u32 = 0x0400;
const ES_BASE: u32 = 0x0300;

/// Sized past `DS_BASE + 0xFFFF` (0x103FE), not just 64K: the wrap-boundary rows place their far
/// pointer at a guest OFFSET near 0xFFFF, and DS_BASE is a real-mode segment base (paragraph
/// aligned, not itself wrapped), so the LINEAR byte a wrapped offset near the top of the segment
/// lands at is past the first 64K of this buffer.
fn memory_fill() -> Vec<u8> {
    let mut memory = vec![0u8; 0x1_1000];
    for (i, byte) in memory.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37).wrapping_add(11) & 0xff) as u8;
    }
    memory
}

/// Real mode, CS.D = 0, SS.B = 0: the ordinary DOS configuration the census measured.
fn sixteen_bit_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.load_segment_real(SegmentIndex::Cs, (CS_BASE >> 4) as u16);
    cpu.load_segment_real(SegmentIndex::Ds, (DS_BASE >> 4) as u16);
    cpu.load_segment_real(SegmentIndex::Es, (ES_BASE >> 4) as u16);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.set_esp(0x0700);
    cpu.set_eip(ENTRY);
    cpu
}

/// The same machine in V86: CR0.PE set, EFLAGS.VM with IOPL 3, CPL cached at 3. The mode both
/// `LoadSegRealMem` and `LesLds` are admitted in alongside real mode.
fn v86_cpu() -> CpuGsw {
    let mut cpu = sixteen_bit_cpu();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x202 | FLAG_VM | (3 << 12);
    cpu.cpl = 3;
    assert!(cpu.is_v86_mode(), "the V86 fixture must actually be in V86");
    cpu
}

/// Protected mode with a 16-BIT code segment (CS.D = 0, CR0.PE = 1, EFLAGS.VM = 0): the mode
/// `LesLds` must refuse at Word operand size, and the one `LoadSegRealMem` rewrites into the
/// pre-existing `InterpretOne` call-out instead of refusing.
fn protected_word_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE;
    // NOT `SegmentRegister::flat`: that constructor hard-codes `default_size_32 = true`, which
    // would make this CS.D = 1 -- a 32-bit code segment reached with `d = true`, defeating the
    // whole point of a 16-bit-code protected-mode fixture. `default_size_32: false` here is what
    // makes `d = false` the correct compile argument and what puts every unprefixed instruction
    // at `OperandSize::Word`, matching `sixteen_bit_cpu`'s real-mode configuration.
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x08,
            base: CS_BASE,
            limit: 0xffff,
            access: 0x9b,
            default_size_32: false,
        },
    );
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        cpu.registers.set_segment(
            segment,
            SegmentRegister {
                selector: 0x10,
                base: DS_BASE,
                limit: 0xffff,
                access: 0x93,
                default_size_32: false,
            },
        );
    }
    cpu.registers.set_esp(0x0700);
    cpu.set_eip(ENTRY);
    assert!(cpu.is_protected_mode() && !cpu.is_v86_mode());
    cpu
}

/// `sixteen_bit_cpu` with DS's cached limit dropped to exactly `OPERAND`: the offset word at
/// `OPERAND..OPERAND+2` is one byte past it, so the FIRST (and, for `LoadSegRealMem`, only)
/// memory read this kind performs takes the segment-limit side exit immediately.
///
/// The truncation happens BEFORE compilation, not after: `LoadSegRealMem`/`LesLds` pin DS's
/// descriptor into the block's `SegmentLayout`, so mutating it after `jit::direct::compile` would
/// desync the live descriptor from the one the entry check baked and the block would refuse to
/// run at all (`data_matches` failing), which is a harness bug rather than the mid-block fault
/// this fixture wants.
fn sixteen_bit_cpu_with_ds_limit_at_operand() -> CpuGsw {
    let mut cpu = sixteen_bit_cpu();
    let mut ds = cpu.registers.segment(SegmentIndex::Ds);
    ds.limit = u32::from(OPERAND);
    cpu.registers.set_segment(SegmentIndex::Ds, ds);
    cpu
}

/// The same truncation, one word later: the offset word (`OPERAND..OPERAND+2`) is fully inside
/// the limit and its read succeeds; the selector word (`OPERAND+2..OPERAND+4`) is not, so
/// `LesLds`'s SECOND read is the one that takes the side exit.
fn sixteen_bit_cpu_with_ds_limit_after_offset() -> CpuGsw {
    let mut cpu = sixteen_bit_cpu();
    let mut ds = cpu.registers.segment(SegmentIndex::Ds);
    ds.limit = u32::from(OPERAND) + 1;
    cpu.registers.set_segment(SegmentIndex::Ds, ds);
    cpu
}

fn map_direct_page(cpu: &mut CpuGsw, bus: &mut TestBus, page: u32) {
    let permissions = jit::fast_map::PagePermissions::UNPAGED;
    let read = bus
        .direct_page(page, BusAccessKind::DataRead)
        .unwrap()
        .unwrap();
    assert!(cpu.jit_fast_map.populate_read(
        page,
        page,
        read,
        permissions,
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
        permissions,
        cpu.physical_page_watched(page)
    ));
}

/// `mov si,si` at Word: the leading slot that keeps the tested opcode off the block entry.
const FILL_A: [u8; 2] = [0x89, 0xf6];
/// `mov di,di`: the trailing slot, so the tested opcode is never last either.
const FILL_B: [u8; 2] = [0x89, 0xff];

fn compile_leading_block_on(builder: fn() -> CpuGsw, d: bool, body: &[u8]) -> Option<u8> {
    let mut code = FILL_A.to_vec();
    let body_at = ENTRY + code.len() as u32;
    code.extend_from_slice(body);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&FILL_B);
    code.push(0xf4);

    let mut memory = memory_fill();
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = builder();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    cpu.set_fast_map_enabled_for_test(true);
    for &linear in &[ENTRY, body_at, tail_at] {
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("fixture decode");
    }
    for page in (0..0x11_000u32).step_by(0x1000) {
        map_direct_page(&mut cpu, &mut bus, page);
    }
    cpu.set_eip(ENTRY);
    match jit::direct::compile(&mut cpu, ENTRY, d) {
        jit::direct::CompileOutcome::Compiled(compilation) => Some(compilation.span.instructions),
        _ => None,
    }
}

fn compile16(body: &[u8]) -> Option<u8> {
    compile_leading_block_on(sixteen_bit_cpu, false, body)
}

fn w(value: u16) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

/// `MOV Sreg, [OPERAND]` -- direct addressing, `mod = 00`, `rm = 110`.
fn mov_sreg_mem(reg_field: u8) -> Vec<u8> {
    [vec![0x8e, reg_field << 3 | 0x06], w(OPERAND)].concat()
}

/// `LES`/`LDS reg, [OPERAND]` -- direct addressing, `mod = 00`, `rm = 110`.
fn les_lds_mem(opcode: u8, dst: u8) -> Vec<u8> {
    les_lds_mem_at(opcode, dst, OPERAND)
}

/// `les_lds_mem` with an explicit disp16, for the wrap-boundary rows that need an offset other
/// than `OPERAND` (0xFFFE, 0xFFFF).
fn les_lds_mem_at(opcode: u8, dst: u8, offset: u16) -> Vec<u8> {
    [vec![opcode, dst << 3 | 0x06], w(offset)].concat()
}

// -------------------------------------------------------------------------------------------
// The differential harness
// -------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Seed {
    gpr: [u32; 8],
    /// The GUEST OFFSET the far pointer's first word sits at, matching whatever disp16 the
    /// instruction under test was encoded with (`OPERAND` by default). Not `OPERAND` itself:
    /// the wrap-boundary rows need 0xFFFE/0xFFFF here.
    read_offset: u16,
    /// The word at LINEAR `DS_BASE + read_offset`, placed there UNWRAPPED (the far pointer's
    /// offset half every row here reads first). A disp16 is already a plain 16-bit value, so
    /// this is where `AddressWrap::Word`'s mask leaves it -- and neither engine re-wraps an
    /// individual word access that straddles the top of the segment (86Box's `readmemw` is a
    /// flat fetch), so `read_offset = 0xFFFF` places this word's second byte one PAST the
    /// segment rather than wrapping it back to offset 0. See `build`.
    offset_word: u16,
    /// The word at LINEAR `DS_BASE + ((read_offset + 2) & 0xffff)` -- the far pointer's selector
    /// half, whose START wraps mod 0x10000 (`far_pointer_second_word_offset`'s whole point) but
    /// whose own two bytes are then placed linearly from there, for the same reason
    /// `offset_word` is. Unused by `LoadSegRealMem`, which performs only the one read.
    selector_word: u16,
    /// Give ES and DS an already-real-mode-incompatible descriptor (unreal-mode limit, code
    /// access byte, `default_size_32` true) before the run, so a dropped access store or a spare
    /// limit store is observable. See the module doc.
    stale_segments: bool,
}

impl Seed {
    fn new() -> Self {
        Self {
            read_offset: OPERAND,
            gpr: std::array::from_fn(|i| 0xdead_0000 | (0xa0 + i as u32)),
            offset_word: 0x1234,
            selector_word: 0x9000,
            stale_segments: false,
        }
    }

    fn stale_segments(mut self) -> Self {
        self.stale_segments = true;
        self
    }

    fn read_offset(mut self, offset: u16) -> Self {
        self.read_offset = offset;
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

fn build(builder: fn() -> CpuGsw, d: bool, body: &[u8], seed: Seed) -> Roles {
    let mut code = FILL_A.to_vec();
    let body_at = ENTRY + code.len() as u32;
    code.extend_from_slice(body);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&FILL_B);
    code.push(0xf4);

    let mut memory = memory_fill();
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    // The offset word sits at `read_offset` unwrapped -- a disp16 is already a plain 16-bit
    // value, so a linear two-byte placement there is exactly what a wrapping AddressWrap::Word
    // mask leaves it as (a no-op below 0x10000). It is placed LINEARLY from there, not wrapped
    // byte by byte: neither engine re-wraps an individual word access that straddles the top of
    // the segment (86Box's `readmemw` is a flat `easeg + a` fetch; matching that is what makes
    // `read_offset = 0xFFFF` exercise something real rather than a fixture invention).
    //
    // The selector word's START wraps mod 0x10000 -- `far_pointer_second_word_offset`'s whole
    // point -- and is placed linearly from THAT wrapped start. For `read_offset = 0xFFFE` this
    // is offset 0x0000; for `0xFFFF` it is 0x0001 (`0xFFFF + 2 = 0x10001`, masked).
    let offset_at = (DS_BASE + u32::from(seed.read_offset)) as usize;
    memory[offset_at..offset_at + 2].copy_from_slice(&seed.offset_word.to_le_bytes());
    let selector_offset = (u32::from(seed.read_offset) + 2) & 0xffff;
    let selector_at = (DS_BASE + selector_offset) as usize;
    memory[selector_at..selector_at + 2].copy_from_slice(&seed.selector_word.to_le_bytes());

    let mut native = builder();
    let mut interp = builder();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    for cpu in [&mut native, &mut interp] {
        // Applied BEFORE compilation, not after: `LoadSegRealMem`/`LesLds` PIN the memory
        // SOURCE segment (DS here) into the block's `SegmentLayout`, so a stale descriptor
        // installed after compile would fail the entry `data_matches` check and the block
        // would correctly refuse to run at all -- a harness bug, not a row under test. Staling
        // both roles identically before either warms its decode cache or compiles keeps the
        // compiled record and the live one in agreement.
        if seed.stale_segments {
            for segment in [SegmentIndex::Es, SegmentIndex::Ds] {
                let mut stale = cpu.registers.segment(segment);
                stale.limit = 0xffff_ffff;
                stale.access = 0x9b;
                stale.default_size_32 = true;
                cpu.registers.set_segment(segment, stale);
            }
        }
    }
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interp, &mut interp_bus),
    ] {
        cpu.set_fast_map_enabled_for_test(true);
        for &linear in &[ENTRY, body_at, tail_at] {
            cpu.set_eip(linear);
            cpu.begin_instruction();
            cpu.fetch_decoded(bus, linear).expect("fixture decode");
        }
        for page in (0..0x11_000u32).step_by(0x1000) {
            map_direct_page(cpu, bus, page);
        }
    }

    let compilation = match jit::direct::compile(&mut native, ENTRY, d) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("structurally rejected: the row under test is still a barrier")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, 3,
        "the block must cover all three slots, so the tested opcode really ran natively"
    );
    let key = jit::direct::key_for(&native, ENTRY, d).expect("entry key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = native.jit_direct.block(id).expect("live block");

    for cpu in [&mut native, &mut interp] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr = seed.gpr;
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

/// What a natively-served MISALIGNED word access costs OVER the interpreted role, at `TestBus`'s
/// dials -- a `TestBus` modelling artifact, not a divergence, per
/// `cpu_jit_misaligned_memory_test.rs`'s module header (which this is a narrow copy of: native
/// charges one WIDE cycle where the interpreted role charges two BYTE cycles for the same split
/// access, and `TestBus`'s direct-page wait states are width-dependent where a real `MachineBus`'s
/// are not, so the residual is a fixture artifact rather than a real cost). `LesLds` can trigger
/// it TWICE in one instruction -- once per word read -- where every other kind in that file
/// triggers it at most once, which is why this lives here rather than being imported.
const MISALIGNED_WORD_BUS_CLOCK_DELTA: u64 = 1; // TestBus: 3 (word) - 2 (byte)

fn compare_state_with_bus_slack(roles: &Roles, bus_clock_slack: u64, context: &str) {
    assert_eq!(
        crate::tests::settled_registers(&roles.native),
        crate::tests::settled_registers(&roles.interp),
        "{context}: registers (segment registers and EIP included)"
    );
    assert_eq!(
        roles.native.eflags(),
        roles.interp.eflags(),
        "{context}: materialized EFLAGS"
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
        roles.interp_bus.trace.elapsed_clocks() + bus_clock_slack,
        "{context}: bus clocks"
    );
    assert_eq!(
        roles.native_bus.memory, roles.interp_bus.memory,
        "{context}: guest RAM"
    );
}

fn lowered_on(builder: fn() -> CpuGsw, d: bool, body: &[u8], seed: Seed, context: &str) {
    lowered_on_with_bus_slack(builder, d, body, seed, 0, context);
}

/// `lowered_on` with a caller-supplied `MISALIGNED_WORD_BUS_CLOCK_DELTA`-scaled bus-clock slack,
/// for the rows that deliberately read at an odd address (the wrap-boundary rows). See
/// `MISALIGNED_WORD_BUS_CLOCK_DELTA`'s own doc for why the slack is legitimate rather than a
/// loosened assertion.
fn lowered_on_with_bus_slack(
    builder: fn() -> CpuGsw,
    d: bool,
    body: &[u8],
    seed: Seed,
    bus_clock_slack: u64,
    context: &str,
) {
    let mut roles = build(builder, d, body, seed);
    let before = roles.native.perf_counters().jit_direct_insns;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "{context}: block did not run natively"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - before,
        3,
        "{context}: all three slots must retire natively"
    );
    for _ in 0..3 {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state_with_bus_slack(&roles, bus_clock_slack, context);
}

fn lowered16(body: &[u8], seed: Seed, context: &str) {
    lowered_on(sixteen_bit_cpu, false, body, seed, context);
}

fn lowered_v86(body: &[u8], seed: Seed, context: &str) {
    lowered_on(v86_cpu, false, body, seed, context);
}

/// The fault-parity shape: `body` is expected to FAULT (a legitimate segment-limit violation,
/// not a wrap bug), and what this proves is that NEITHER engine silently succeeds where the
/// other does not.
///
/// Native: the side exit must fire AT `body`, so only `FILL_A` retires (`jit_direct_insns`
/// advances by exactly 1) -- the same "leaves the instruction un-started" proof the ordinary
/// fault-ordering tests use, here read the other way around: a native lowering that silently
/// wrapped the limit check right along with the address would complete all three slots instead.
///
/// Interpreter: EIP after stepping past `body` must NOT be `tail_at` (where completing `body`
/// normally would leave it) -- it faulted and got redirected somewhere else, whatever this
/// fixture's IVT (the same pseudo-random `memory_fill` bytes both engines share) sends it to.
/// This does not chase the fault to a specific vector or delivery site; it only has to disagree
/// with "the instruction ran to completion," which is the property a real-address-past-the-mask
/// bug would break silently.
fn fault_parity_on(builder: fn() -> CpuGsw, d: bool, body: &[u8], seed: Seed, context: &str) {
    let tail_at = ENTRY + FILL_A.len() as u32 + body.len() as u32;
    let mut roles = build(builder, d, body, seed);
    let before = roles.native.perf_counters().jit_direct_insns;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "{context}: block did not run natively at all"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - before,
        1,
        "{context}: only FILL_A may retire natively; a faulting row must side-exit instead of \
         silently completing"
    );
    roles.interp.cycle(&mut roles.interp_bus).unwrap(); // FILL_A
    // `body`: expected to fault. This fixture's IVT is the same pseudo-random `memory_fill`
    // bytes every other row here shares, not a real handler, so delivering the #GP can itself
    // escalate into a double or triple fault (`CpuError`) rather than a clean redirect -- either
    // outcome is equally good proof the row did NOT complete normally, which is the only claim
    // this helper makes.
    if roles.interp.cycle(&mut roles.interp_bus).is_ok() {
        assert_ne!(
            roles.interp.registers.eip, tail_at,
            "{context}: the interpreter completed the row normally instead of faulting"
        );
    }
}

fn fault_parity_on16(body: &[u8], seed: Seed, context: &str) {
    fault_parity_on(sixteen_bit_cpu, false, body, seed, context);
}

fn fault_parity_on_v86(body: &[u8], seed: Seed, context: &str) {
    fault_parity_on(v86_cpu, false, body, seed, context);
}

// -------------------------------------------------------------------------------------------
// `MOV Sreg, m16` -- `DirectKind::LoadSegRealMem`
// -------------------------------------------------------------------------------------------

/// ES and DS, real mode and V86, both matching the interpreter's `load_segment_real_mode`:
/// selector, base, and the unchanged (stale) limit and access byte, exactly as `LoadSegReal`'s
/// register form already proves for the register source.
#[test]
fn mov_sreg_memory_matches_the_interpreter_in_real_mode_and_v86() {
    for (name, reg_field) in [("es", 0u8), ("ds", 3u8)] {
        // Real mode gets the STALE seed, which is what makes the access/limit stores
        // observable. V86 does not: `load_segment_checked`'s V86 branch always canonicalizes
        // through `load_segment_real` (limit = 0xFFFF unconditionally), so nothing in a real V86
        // run ever leaves a stale limit behind for this instruction to inherit -- the doc on
        // `DirectKind::LoadSegReal`'s emit arm states the invariant this leans on. Staling ES/DS
        // there would assert a precondition V86 execution can never produce.
        lowered16(
            &mov_sreg_mem(reg_field),
            Seed::new().stale_segments(),
            &format!("mov {name}, [m] (real)"),
        );
        lowered_v86(
            &mov_sreg_mem(reg_field),
            Seed::new(),
            &format!("mov {name}, [m] (v86)"),
        );
    }
}

/// Protected mode, 16-bit code segment: the memory form must still compile -- through the SAME
/// `InterpretOne` call-out it always used, since `stack_width_kind` rewrites `LoadSegRealMem`
/// back into it there. This is the regression check for "a protected-mode compile sees no change
/// at all."
///
/// Not a full differential, for the reason `mov_ss_memory_keeps_its_own_call_out_row_in_every_mode`
/// gives: an `InterpretOne` block's resume predicate can legitimately resync after one execution
/// while the call-out admission governor is still learning it, which is that mechanism's own
/// behavior and not something this row's admission changed. `load_protected_segment`'s own
/// correctness -- a real descriptor fetch, privilege checks, #GP/#NP -- is that helper's coverage,
/// not this one's.
#[test]
fn mov_sreg_memory_still_matches_the_interpreter_in_protected_mode() {
    for (name, reg_field) in [("es", 0u8), ("ds", 3u8)] {
        assert_eq!(
            compile_leading_block_on(protected_word_cpu, false, &mov_sreg_mem(reg_field)),
            Some(3),
            "mov {name}, [m] (protected, word code) must still compile through the existing call-out"
        );
    }
}

/// `MOV SS, [m]` stays on its own `InterpretOne` row (`MovSsReg`) in every mode: `classify`
/// returns that call-out for `/2` before it ever looks at the operand form, so the memory
/// admission this slice adds cannot reach it -- proved here by the block still COMPILING (the
/// `/2` arm never falls through to a `None` the way an unmatched form would).
///
/// Not a full differential: `MovSsReg`'s row arms the one-instruction interrupt shadow
/// (`load_segment_arming_ss_shadow`), and the resume predicate correctly ends the run right
/// after it -- a real, expected two-of-three partial retirement rather than a bug. That
/// behavior predates this slice and belongs to `MovSsReg`'s own test coverage, not this one.
#[test]
fn mov_ss_memory_keeps_its_own_call_out_row_in_every_mode() {
    assert_eq!(
        compile16(&mov_sreg_mem(2)),
        Some(3),
        "mov ss, [m] (real) must still compile through the existing call-out"
    );
    assert_eq!(
        compile_leading_block_on(protected_word_cpu, false, &mov_sreg_mem(2)),
        Some(3),
        "mov ss, [m] (protected, word code) must still compile through the existing call-out"
    );
}

// -------------------------------------------------------------------------------------------
// LES / LDS -- `DirectKind::LesLds`
// -------------------------------------------------------------------------------------------

/// LES (dst <- offset, ES <- selector) and LDS (dst <- offset, DS <- selector), real mode and
/// V86, across every GPR destination. `stale_segments` catches a dropped access store or a spare
/// limit store; the poisoned GPR high half (from `Seed::new`) catches a write wider than 16 bits.
#[test]
fn les_lds_match_the_interpreter_in_real_mode_and_v86() {
    for (name, opcode) in [("les", 0xc4u8), ("lds", 0xc5u8)] {
        for dst in 0u8..8 {
            // See `mov_sreg_memory_matches_the_interpreter_in_real_mode_and_v86`: the stale seed
            // is a real-mode-only precondition, never a V86-reachable one.
            lowered16(
                &les_lds_mem(opcode, dst),
                Seed::new().stale_segments(),
                &format!("{name} r{dst}, [m] (real)"),
            );
            lowered_v86(
                &les_lds_mem(opcode, dst),
                Seed::new(),
                &format!("{name} r{dst}, [m] (v86)"),
            );
        }
    }
}

/// The selector address at the top of a 16-bit segment WRAPS mod 0x10000, matching 86Box's
/// `opLDS_w_a16` (`src/cpu/x86_ops_mov_seg.h`, `dev_docs/reference/86box`) and this codebase's
/// own `far_call_via_memory_wraps_selector_offset_at_64k` (`cpu_strings_segments_test.rs`), which
/// already pinned the identical shape for `CALL FAR [m]` against a SingleStepTests 80386
/// conformance vector.
///
/// Two EAs, both real mode (V86 where it is meaningful), both the default limit-0xFFFF fixture
/// and the stale/stretched-limit fixture `Seed::stale_segments` builds:
///
/// * `0xFFFE`: the offset word sits entirely inside the segment (bytes 0xFFFE, 0xFFFF); the
///   selector word wraps to 0x0000, 0x0001. Neither engine has a legitimate reason to fault here
///   in EITHER fixture -- this is the case a `u32` `wrapping_add(2)` with no re-mask would get
///   wrong SILENTLY (reading the selector from a linear byte one past the segment instead of the
///   wrapped one), which is exactly why it needs a full differential rather than a fault-shape
///   assertion. Both fixtures run it through `lowered16`/`lowered_v86` and must complete.
/// * `0xFFFF`: the OFFSET word itself straddles the segment top (bytes 0xFFFF, then one past the
///   limit). Under the DEFAULT limit-0xFFFF fixture this is a LEGITIMATE segment-limit
///   violation, not a wrap question at all -- 86Box's flat `readmemw(easeg, a)` fetch does not
///   re-wrap an individual straddling word access, and neither does this codebase's own
///   `MemorySideExits` limit compare, so a correct engine on either side must FAULT here rather
///   than complete. `fault_parity_on` is what proves neither engine silently disagrees and
///   completes where the other faults. Under the STALE/STRETCHED-limit fixture there is no limit
///   to violate, so it runs the ordinary full differential instead, and is the row that would
///   catch a wrap bug in the FIRST word's own address (as opposed to the selector's, which
///   `0xFFFE` already covers) if `MemorySideExits`' limit-guard-omitted path miscomputed it.
///
/// The stale/stretched-limit fixture is the reviewer's "silent wrong address" case named
/// separately from the default fixture's "fault parity" one: with DS's limit stretched to
/// `u32::MAX`, `MemorySideExits::new` omits the segment-limit guard entirely (a structurally
/// different emitted path from the default fixture's), so a wrap bug hiding behind THAT guard's
/// absence would not show up in the limit-0xFFFF fixture's `0xFFFE` row.
#[test]
fn les_lds_second_word_wraps_at_the_sixty_four_k_boundary() {
    for (name, opcode) in [("les", 0xc4u8), ("lds", 0xc5u8)] {
        let body_at = |offset: u16| les_lds_mem_at(opcode, 3, offset);

        // 0xFFFE: no legitimate fault in either fixture; a full differential in both.
        for stale in [false, true] {
            let seed = Seed::new().read_offset(0xfffe);
            let seed = if stale { seed.stale_segments() } else { seed };
            lowered16(
                &body_at(0xfffe),
                seed,
                &format!("{name} [0xfffe] (real, stale_segments={stale})"),
            );
        }
        lowered_v86(
            &body_at(0xfffe),
            Seed::new().read_offset(0xfffe),
            &format!("{name} [0xfffe] (v86)"),
        );

        // 0xFFFF, limit 0xFFFF: a legitimate segment-limit violation on the FIRST read. Fault
        // parity, both real mode and V86 (V86 canonicalizes every segment's limit to 0xFFFF too,
        // per `LoadSegReal`'s own doc, so the same violation applies there).
        fault_parity_on16(
            &body_at(0xffff),
            Seed::new().read_offset(0xffff),
            &format!("{name} [0xffff] (real, limit 0xffff)"),
        );
        fault_parity_on_v86(
            &body_at(0xffff),
            Seed::new().read_offset(0xffff),
            &format!("{name} [0xffff] (v86, limit 0xffff)"),
        );

        // 0xFFFF, stretched limit: nothing to violate, so this is a full differential again --
        // the row that would catch a wrap bug in the FIRST word's own straddling address.
        //
        // BOTH word reads land at an odd address here (the offset word at 0xFFFF itself, and the
        // wrapped selector word at 0x0001), so this is `MISALIGNED_WORD_BUS_CLOCK_DELTA` TWICE
        // over -- the one row in this file that needs the slack at all.
        lowered_on_with_bus_slack(
            sixteen_bit_cpu,
            false,
            &body_at(0xffff),
            Seed::new().read_offset(0xffff).stale_segments(),
            2 * MISALIGNED_WORD_BUS_CLOCK_DELTA,
            &format!("{name} [0xffff] (real, stale_segments=true)"),
        );
    }
}

/// `LES`/`LDS` at Word operand size, protected mode (16-bit code, CR0.PE = 1, not V86): must
/// stay a barrier. There is no `InterpretOne` row to fall back to, so the block stops one
/// instruction short of the far-pointer load, the same place it always has.
#[test]
fn les_lds_stay_barriers_in_protected_mode() {
    for opcode in [0xc4u8, 0xc5] {
        assert_eq!(
            compile_leading_block_on(protected_word_cpu, false, &les_lds_mem(opcode, 3)),
            None,
            "opcode {opcode:#04x}: protected mode has no lowering and no call-out for LES/LDS"
        );
    }
}

/// The DWORD form (a 66 prefix) is out of scope and stays exactly the barrier it always was:
/// `jit_admits_non_continuable` withholds it, so it never reaches `classify`.
#[test]
fn les_lds_dword_form_stays_a_barrier() {
    for opcode in [0xc4u8, 0xc5] {
        assert_eq!(
            compile16(&[vec![0x66], les_lds_mem(opcode, 3)].concat()),
            None,
            "opcode {opcode:#04x}: the dword form must stay unclassified"
        );
    }
    // Sanity: the WORD form at the same address really does compile, so the dword assertion
    // above is testing the width and not an unrelated setup mistake.
    assert_eq!(compile16(&les_lds_mem(0xc4, 3)), Some(3));
}

/// `mod = 3` (a register r/m) is #UD for LES/LDS -- `execute_system_seg_decoded`'s own comment
/// states it. `classify`'s `Mem` match must refuse it rather than treat `m.reg` as an address.
#[test]
fn les_lds_with_a_register_operand_stays_a_barrier() {
    for opcode in [0xc4u8, 0xc5] {
        // mod=11, reg=0 (dst), rm=3 (BX): a legal ModRM byte, an illegal LES/LDS encoding.
        assert_eq!(
            compile16(&[opcode, 0xc3]),
            None,
            "opcode {opcode:#04x}: mod=3 is #UD and must stay a barrier"
        );
    }
}

// -------------------------------------------------------------------------------------------
// Fault ordering: a memory exit must leave every guest write un-committed.
// -------------------------------------------------------------------------------------------

/// `MOV Sreg, m16`: DS's cached limit is dropped BELOW `OPERAND`, so the one read this kind
/// performs takes the segment-limit side exit before any guest write. ES must still hold its
/// PRE-instruction descriptor, and the block must have retired only the one FILL_A slot ahead of
/// it.
#[test]
fn mov_sreg_memory_leaves_the_segment_untouched_when_the_read_faults() {
    let seed = Seed::new();
    let mut roles = build(
        sixteen_bit_cpu_with_ds_limit_at_operand,
        false,
        &mov_sreg_mem(0),
        seed,
    );
    let original_es = roles.native.registers.segment(SegmentIndex::Es);

    let before = roles.native.perf_counters().jit_direct_insns;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap()
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - before,
        1,
        "only FILL_A may retire; the faulting MOV ES must not"
    );
    assert_eq!(
        roles.native.registers.segment(SegmentIndex::Es),
        original_es,
        "a faulted read must leave ES exactly as it was before the instruction"
    );
    assert_eq!(
        roles.native.registers.eip,
        ENTRY + FILL_A.len() as u32,
        "EIP must sit at the faulting instruction, un-advanced past it"
    );
}

/// `LES`/`LDS`: DS's cached limit is dropped between the offset word and the selector word, so
/// the FIRST read succeeds and the SECOND takes the segment-limit side exit. Neither the
/// destination GPR nor the segment record may show any trace of the first read having
/// succeeded -- the interpreter's own order (`execute_system_seg_decoded`) commits nothing until
/// BOTH reads are in hand.
#[test]
fn les_lds_leaves_both_writes_untouched_when_the_second_read_faults() {
    for (name, opcode) in [("les", 0xc4u8), ("lds", 0xc5u8)] {
        let dst = 6u8; // SI, distinct from every fixed home the emitter stages through
        let seed = Seed::new();
        let mut roles = build(
            sixteen_bit_cpu_with_ds_limit_after_offset,
            false,
            &les_lds_mem(opcode, dst),
            seed,
        );
        let segment = if opcode == 0xc4 {
            SegmentIndex::Es
        } else {
            SegmentIndex::Ds
        };
        let original_segment = roles.native.registers.segment(segment);
        let original_dst = roles.native.registers.gpr[dst as usize];

        let before = roles.native.perf_counters().jit_direct_insns;
        assert!(
            roles
                .native
                .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
                .unwrap(),
            "{name}: block did not run natively"
        );
        assert_eq!(
            roles.native.perf_counters().jit_direct_insns - before,
            1,
            "{name}: only FILL_A may retire; the faulting instruction must not"
        );
        assert_eq!(
            roles.native.registers.segment(segment),
            original_segment,
            "{name}: the segment record must be untouched by the first read's success"
        );
        assert_eq!(
            roles.native.registers.gpr[dst as usize], original_dst,
            "{name}: the destination GPR must be untouched by the first read's success"
        );
        assert_eq!(
            roles.native.registers.eip,
            ENTRY + FILL_A.len() as u32,
            "{name}: EIP must sit at the faulting instruction, un-advanced past it"
        );
    }
}
