// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The group-2 shift/rotate MEMORY lane: `0xC0`/`0xC1`/`0xD0`/`0xD1` with a ModRM memory operand,
//! sub-opcodes `/0` ROL, `/1` ROR, `/4` SHL, `/5` SHR, `/6` SAL and `/7` SAR.
//!
//! The row this claims is `123-talk-shareware`'s `0xD1 /1 mem word` at 29,698,831 static unbound
//! exits, 66.8% of that game's whole unbound class and the largest single lever on the 2026-09-01
//! board. The `#784` register lanes (`RotateReg`, `RotateRegByte`, `Shift`) could not reach it:
//! every one of them binds `DecodedOperand::Reg` and refuses the memory form, and no `DirectKind`
//! in the tree carried a read-modify-write memory lane for this family.
//!
//! ## What stays refused, and it is not an omission
//!
//! `/2` RCL and `/3` RCR take the incoming CF as a rotate INPUT (`core.rs`'s `shift_rotate` seeds
//! `cf` from `flag(FLAG_CF)` before its loop), which needs the guest flags loaded into the host
//! BEFORE the rotate rather than only captured after it. No emitter in this family does that, so
//! both stay hard boundaries at every width and both operand forms. `0xD2`/`0xD3` (the by-CL
//! counts) are a different classify arm and are untouched.
//!
//! A count that masks to zero is refused too, and the reason is accounting rather than semantics.
//! `execute.rs`'s group-2 arm reads the operand and writes it back even when `shift_rotate` returns
//! early, so the interpreter performs one read and one store at count zero. An emitted form that
//! elided both would register static read and store counts the emitted code never produces.
//!
//! ## The flag contract, per sub-opcode family
//!
//! * **Rotates (`/0`, `/1`)** define CF at every non-zero count and OF only at a masked count of
//!   exactly 1. SF, ZF, PF and AF are UNTOUCHED at every count, which is why the rotate arm
//!   captures `CF|OF` at count 1 and CF alone above it, exactly as `emit_rotate_reg` does.
//! * **Shifts (`/4`, `/5`, `/7`)** define CF, PF, ZF and SF at every non-zero count, plus OF at a
//!   masked count of 1. This is `emit_commit_shift_flags`'s `SHIFT_DEFINED`, shared with the
//!   register lanes so the two cannot drift.
//!
//! Every behavioural row below seeds EFLAGS at `0x8d7` -- SF, ZF, PF and AF all SET, bit 1 forced,
//! and NO live descriptor. The no-descriptor half is load-bearing: a seed of `(0x8d5, pending)`
//! never delivers a seeded CF, because the live descriptor swallows it, so a CF assertion made
//! under that seed passes whatever the emitter does with CF.
//!
//! ## Mutation ledger
//!
//! Each mutant was applied to `emit_rotate_shift_mem` and reverted, against this whole module.
//!
//! | # | mutation | outcome |
//! |---|---|---|
//! | M1 | drop `emit_capture_flags(e, defined)` | RED, 4 rows |
//! | M2 | drop the memory write-back `store_r{8,16,32}_disp8` | RED, 5 rows |
//! | M3 | deposit the split charge ONCE instead of twice | RED, 1 row (`a_misaligned_group_two_memory_form_runs_natively`) |
//! | M4 | rotate-above-1 tail publishes RBP wholesale instead of `emit_set_cf_only` | **SURVIVES, on both flag arms** |
//!
//! M4's survival is explained where the row that chases it lives, in
//! `a_memory_rotate_above_count_one_routes_cf_through_a_live_descriptor`: the eager arm makes the
//! two spellings the same instruction, and on the lazy arm `run.rs`'s entry clear means a native
//! slot never meets a live descriptor. It is a blind spot the register lanes share, not a hole
//! this kind opened.
//!
//! ## N1 (review finding, closed): the aperture's misaligned tail
//!
//! `a_misaligned_mode13_group_two_memory_form_side_exits` closes a gap the review found: before
//! it, `emit_rotate_shift_mem` admitted a misaligned Mode 13h access at every width, dropping
//! `emit_alignment_test` for the aperture arm and depositing the split charge unconditionally.
//! That falsified `memory.rs`'s `is_mode13_aperture` and `run.rs`'s split-cost comment, both of
//! which assert nothing in the RAM split-cost pool came from the aperture, and every other Mode
//! 13h-admitting emitter (`emit_alu_mem_dest`, `emit_rmw_inc_dec_dword`, `emit_push_mem`) refuses
//! or side exits on it. The fix re-runs `emit_alignment_test` for the Mode 13h arm alone, so a
//! misaligned aperture access side exits exactly as it does everywhere else in the tree while the
//! RAM in-page misalignment relaxation this module's other rows exercise is untouched. Proved red
//! against the unguarded emitter first.

use super::*;

/// `mov esi,esi` / `mov si,si`, the leading slot that keeps the tested opcode off the block entry.
/// An opcode at a block's ENTRY never executes natively, so an entry-position fixture certifies
/// nothing.
const FILL_A: [u8; 2] = [0x89, 0xf6];
/// `mov edi,edi` / `mov di,di`, the trailing slot, so the tested opcode is never the last either.
const FILL_B: [u8; 2] = [0x89, 0xff];

/// The operand address. Four-aligned, so the Dword rows clear `emit_wide_page_guard`'s alignment
/// test, and well inside the fixture's 0x5000 of memory at both address sizes.
const DATA: u32 = 0x3010;

/// The five flag consumers, as `(condition, byte destination)`. Between them they read every flag
/// this family can define.
const CONSUMERS: [(u8, u8); 5] = [
    (0x2, 3), // setc bl  -- CF
    (0x0, 6), // seto dh  -- OF
    (0x8, 5), // sets ch  -- SF
    (0x4, 2), // setz dl  -- ZF
    (0xa, 7), // setp bh  -- PF
];

/// Which code segment a row runs in. Both are required on every behavioural row: the census row
/// this slice claims is a CS.D = 0 one, and the 32-bit segment is the control that says the
/// admission did not move anything that already worked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Seg {
    /// Real mode, CS.D = 0. Address size is 16 bits, so the memory operand is `mod=00, rm=110`
    /// with a disp16, and an unprefixed `0xC1`/`0xD1` decodes at `OperandSize::Word`. This is the
    /// shape `123-talk-shareware` runs.
    Sixteen,
    /// Protected flat, CS.D = 1. Address size is 32 bits (`mod=00, rm=101`, disp32) and an
    /// unprefixed `0xC1`/`0xD1` decodes at `OperandSize::Dword`.
    ThirtyTwo,
}

impl Seg {
    fn d(self) -> bool {
        self == Seg::ThirtyTwo
    }

    fn cpu(self) -> CpuGsw {
        match self {
            Seg::Sixteen => sixteen_bit_cpu(),
            Seg::ThirtyTwo => flat_cpu(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Seg::Sixteen => "16-bit segment",
            Seg::ThirtyTwo => "32-bit segment",
        }
    }

    /// The ModRM `rm` field and displacement bytes for the absolute-displacement memory mode.
    fn mem_rm(self) -> u8 {
        match self {
            Seg::Sixteen => 0x06,
            Seg::ThirtyTwo => 0x05,
        }
    }

    fn disp_bytes(self, at: u32) -> Vec<u8> {
        match self {
            Seg::Sixteen => (at as u16).to_le_bytes().to_vec(),
            Seg::ThirtyTwo => at.to_le_bytes().to_vec(),
        }
    }

    /// `0F 9x /r`, 66-prefixed in a 16-bit segment: the unprefixed form is refused by classify's
    /// `OperandSize::Word` allowlist, and the prefix is architecturally inert on `SETcc`.
    fn consumer_bytes(self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (condition, dst) in CONSUMERS {
            if self == Seg::Sixteen {
                bytes.push(0x66);
            }
            bytes.extend_from_slice(&[0x0f, 0x90 | condition, 0xc0 | dst]);
        }
        bytes
    }
}

const SEGMENTS: [Seg; 2] = [Seg::Sixteen, Seg::ThirtyTwo];

/// Real mode with CS.D = 0 and SS.B = 0: the ordinary DOS configuration the corpus games run in.
fn sixteen_bit_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
    ] {
        cpu.load_segment_real(segment, 0);
    }
    cpu.set_eip(ENTRY);
    cpu
}

/// A distinct byte at every address, so a stray write of any width is visible in the whole-RAM
/// compare rather than hidden by a zero fill matching a zero store.
fn memory_fill() -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    for (i, byte) in memory.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37).wrapping_add(11) & 0xff) as u8;
    }
    memory
}

// ---------------------------------------------------------------------------------------------
// Encodings
// ---------------------------------------------------------------------------------------------

/// The three operand widths a group-2 memory form can take, and which opcode and prefix produce
/// each in a given segment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum W {
    Byte,
    Word,
    Dword,
}

impl W {
    fn label(self) -> &'static str {
        match self {
            W::Byte => "byte",
            W::Word => "word",
            W::Dword => "dword",
        }
    }

    /// The even opcodes (`0xC0`, `0xD0`) are the byte forms; the odd ones follow the operand size.
    fn imm8_opcode(self) -> u8 {
        match self {
            W::Byte => 0xc0,
            W::Word | W::Dword => 0xc1,
        }
    }

    fn by_one_opcode(self) -> u8 {
        match self {
            W::Byte => 0xd0,
            W::Word | W::Dword => 0xd1,
        }
    }

    /// Whether this width needs a `0x66` operand-size prefix in `seg`. The byte forms never do:
    /// their width is the opcode's low bit and the prefix would be inert.
    fn needs_prefix(self, seg: Seg) -> bool {
        match self {
            W::Byte => false,
            W::Word => seg == Seg::ThirtyTwo,
            W::Dword => seg == Seg::Sixteen,
        }
    }
}

/// `[C0|C1] /op disp ib` against the absolute-displacement memory operand at `at`.
fn imm8_mem_at(seg: Seg, width: W, op: u8, count: u8, at: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    if width.needs_prefix(seg) {
        bytes.push(0x66);
    }
    bytes.push(width.imm8_opcode());
    bytes.push((op << 3) | seg.mem_rm());
    bytes.extend_from_slice(&seg.disp_bytes(at));
    bytes.push(count);
    bytes
}

fn imm8_mem(seg: Seg, width: W, op: u8, count: u8) -> Vec<u8> {
    imm8_mem_at(seg, width, op, count, DATA)
}

/// `[D0|D1] /op disp`. NO immediate: the count is the literal 1 baked into the opcode.
fn by_one_mem_at(seg: Seg, width: W, op: u8, at: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    if width.needs_prefix(seg) {
        bytes.push(0x66);
    }
    bytes.push(width.by_one_opcode());
    bytes.push((op << 3) | seg.mem_rm());
    bytes.extend_from_slice(&seg.disp_bytes(at));
    bytes
}

fn by_one_mem(seg: Seg, width: W, op: u8) -> Vec<u8> {
    by_one_mem_at(seg, width, op, DATA)
}

/// `mov eax,ecx` / `mov ax,cx`: the control row, which must compile in BOTH segment kinds. Without
/// it a refusal assertion could pass because the harness refuses everything.
const CONTROL: [u8; 2] = [0x89, 0xc8];

/// The six admitted sub-opcodes, `(label, /digit)`. Listed rather than ranged, because a range
/// hides a member and `/6` is the alias whose normalisation is stated at classify.
const SUB_OPS: [(&str, u8); 6] = [
    ("/0 rol", 0),
    ("/1 ror", 1),
    ("/4 shl", 4),
    ("/5 shr", 5),
    ("/6 sal", 6),
    ("/7 sar", 7),
];

/// The two sub-opcodes this slice refuses at every opcode and every width.
const REFUSED_SUB_OPS: [(&str, u8); 2] = [("/2 rcl", 2), ("/3 rcr", 3)];

// ---------------------------------------------------------------------------------------------
// Arm selection
// ---------------------------------------------------------------------------------------------

/// Restores every arm this file forces, on the way out of a fixture -- normally OR by panic.
///
/// A plain `set_*_for_test(Some(..))` LEAKS: the overrides are thread-local and the harness reuses
/// threads, so the next fixture on that thread inherits an arm it never asked for.
struct ArmOverride;

impl Drop for ArmOverride {
    fn drop(&mut self) {
        jit::direct::set_byte_shift_rows_for_test(None);
        jit::direct::set_rotate_rows_arm_for_test(None);
        jit::direct::set_count_lanes_for_test(None);
    }
}

/// Both group-2 row knobs ON and the count-lane arm OFF.
///
/// The lane arm is forced rather than left ambient for `cpu_jit_count_lane_test.rs`'s recorded
/// reason: when `IZARRAVM_COUNT_LANES` flipped default ON, every unforced group-2 fixture in the
/// tree quietly stopped exercising the baked emitter. `count_lane_for` bars memory destinations
/// today, so forcing it OFF is a pin on that bar rather than a selection between two emitters --
/// and it is the pin that would catch a future relaxation reaching this kind.
#[must_use]
fn force_rows(on: bool) -> ArmOverride {
    jit::direct::set_byte_shift_rows_for_test(Some(on));
    jit::direct::set_rotate_rows_arm_for_test(Some(if on {
        jit::direct::RotateRowsArm::On
    } else {
        jit::direct::RotateRowsArm::Off
    }));
    jit::direct::set_count_lanes_for_test(Some(false));
    assert_eq!(
        jit::direct::byte_shift_rows_enabled(),
        on,
        "the fixture override must decide the arm, not the ambient IZARRAVM_BYTE_SHIFT_ROWS"
    );
    assert_eq!(
        jit::direct::rotate_rows_enabled(),
        on,
        "the fixture override must decide the arm, not the ambient IZARRAVM_ROTATE_ROWS"
    );
    ArmOverride
}

// ---------------------------------------------------------------------------------------------
// The compile-only harness, for the admission rows
// ---------------------------------------------------------------------------------------------

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

/// Compile `FILL_A / body / FILL_B / hlt` at `ENTRY` and report the span length, or `None` when the
/// walk refused it.
///
/// Every page is mapped for read and write unconditionally: with the operand page absent from the
/// fast map every memory kind is refused, so a negative assertion made without it would pass for
/// the harness's reason rather than the row's.
fn compile_span(seg: Seg, body: &[u8]) -> Option<u8> {
    let mut code = FILL_A.to_vec();
    code.extend_from_slice(body);
    code.extend_from_slice(&FILL_B);
    code.push(0xf4);

    let mut memory = memory_fill();
    // A NOP before the entry, so the block is reachable as a continuation as well as directly.
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = seg.cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    cpu.set_fast_map_enabled_for_test(true);
    cpu.registers.set_esp(STACK_TOP);
    for offset in 0..code.len() as u32 {
        let linear = ENTRY + offset;
        cpu.set_eip(linear);
        cpu.begin_instruction();
        let _ = cpu.fetch_decoded(&mut bus, linear);
    }
    for page in (0..0x5000u32).step_by(0x1000) {
        map_direct_page(&mut cpu, &mut bus, page);
    }
    cpu.set_eip(ENTRY);
    match jit::direct::compile(&mut cpu, ENTRY, seg.d()) {
        jit::direct::CompileOutcome::Compiled(compilation) => Some(compilation.span.instructions),
        _ => None,
    }
}

/// The span a three-slot block reports when the tested opcode JOINED it.
const ADMITTED: Option<u8> = Some(3);
/// A barrier in the body slot. The walk stops one slot in, which is shorter than the minimum
/// installable block, so the outcome is a `StructuralReject` and the harness reports `None`.
const REFUSED: Option<u8> = None;

// ---------------------------------------------------------------------------------------------
// The differential harness
// ---------------------------------------------------------------------------------------------

/// The architectural state both roles start from.
#[derive(Clone, Copy)]
struct Seed {
    /// The dword written at `DATA` before either role runs. The row under test reads its own
    /// width out of the low end of it; the bytes above that width are the stray-write witness.
    data: u32,
    eflags: u32,
    live_pending: bool,
}

impl Seed {
    fn new(data: u32) -> Self {
        Self {
            data,
            // Bit 1 SET on every seed in this file. The publishing paths write the RBP shadow
            // without re-asserting the reserved bit where the interpreter's `set_flag_live` ors
            // `0x2` on every write, so a seed with it clear produces a one-bit disagreement that
            // says nothing about this lowering.
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
    slots: u8,
}

/// Compile `FILL_A / body / FILL_B / hlt` at `ENTRY` on the native role, warm the same decode lines
/// on the interpreter role, and seed both identically.
///
/// `slots` is the EXACT instruction count the block must cover. An exact count rather than a lower
/// bound is what says the tested opcode joined the block instead of ending it.
fn build(seg: Seg, body: &[u8], slots: u8, seed: Seed) -> Roles {
    build_with_pages(seg, body, slots, seed, 0x5000, &[])
}

/// `build`'s general form: a caller-chosen memory length and an extra set of physical pages to map
/// beyond the default code/stack range `0..0x5000`. Exists for the Mode 13h aperture row, whose
/// operand sits at `0xA0000` and needs both a bigger backing buffer and that page mapped so
/// `populate_read`/`populate_write` classify it by physical range.
fn build_with_pages(
    seg: Seg,
    body: &[u8],
    slots: u8,
    seed: Seed,
    memory_len: usize,
    extra_pages: &[u32],
) -> Roles {
    let mut code = FILL_A.to_vec();
    let mut starts = vec![ENTRY, ENTRY + code.len() as u32];
    code.extend_from_slice(body);
    starts.push(ENTRY + code.len() as u32);
    code.extend_from_slice(&FILL_B);
    code.push(0xf4);

    let mut memory = vec![0u8; memory_len];
    for (i, byte) in memory.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37).wrapping_add(11) & 0xff) as u8;
    }
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA as usize..DATA as usize + 4].copy_from_slice(&seed.data.to_le_bytes());

    let mut native = seg.cpu();
    let mut interp = seg.cpu();
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
        // ESP must be live BEFORE compiling: an unresolvable store page returns the whole block as
        // Retry, which is indistinguishable from the opcode still being a barrier.
        cpu.registers.set_esp(STACK_TOP);
        cpu.set_fast_map_enabled_for_test(true);
        for offset in 0..code.len() as u32 {
            let linear = ENTRY + offset;
            cpu.set_eip(linear);
            cpu.begin_instruction();
            let _ = cpu.fetch_decoded(bus, linear);
        }
        for &linear in &starts {
            cpu.set_eip(linear);
            cpu.begin_instruction();
            cpu.fetch_decoded(bus, linear).unwrap();
        }
        for page in (0..0x5000u32).step_by(0x1000) {
            map_direct_page(cpu, bus, page);
        }
        for &page in extra_pages {
            map_direct_page(cpu, bus, page);
        }
    }

    let key = jit::direct::key_for(&native, ENTRY, seg.d()).expect("entry key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut native, ENTRY, seg.d()) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("structurally rejected: the group-2 memory form is still a barrier")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, slots,
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
        cpu.registers.set_esp(STACK_TOP);
        cpu.registers.eflags = seed.eflags;
        cpu.pending_flags = PendingFlags::default();
        if seed.live_pending {
            // A DWORD descriptor produced BEFORE the tested instruction. A shift, and a rotate by
            // exactly 1, must destroy it and publish live flags; a rotate above count 1 must route
            // CF through it rather than materializing.
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

/// `TestBus`'s width-DEPENDENT direct dial, in clocks for one `bytes`-wide cycle: 0, 1 and 3 wait
/// states for Byte, Word and Dword. Copied from `cpu_jit_misaligned_memory_test.rs`, which
/// derives the same quantity for the same reason.
fn direct_cycle_clocks(bytes: u32) -> u64 {
    match bytes {
        1 => 2,
        2 => 3,
        4 => 5,
        other => unreachable!("no TestBus dial for a {other}-byte access"),
    }
}

/// What a natively served MISALIGNED read-modify-write costs over the interpreted role, at
/// `TestBus`'s dials.
///
/// Per access the native role charges one WIDE cycle -- the one the block's static count carries
/// -- plus `bytes - 1` byte cycles from the split deposit, where the interpreted role charges
/// `bytes` byte cycles through `charge_direct_ram_split`. The `bytes - 1` cancel and what remains
/// is `wide_cycle(bytes) - byte_cycle`, which is a pure `TestBus` artifact: on a real `MachineBus`
/// `BusCycle::clocks_for` ignores width and the quantity is exactly zero.
///
/// **TWICE, and that factor is the assertion.** A read-modify-write splits on BOTH accesses where
/// a plain load or store splits on one, so `emit_rotate_shift_mem` deposits twice. An emitter that
/// deposited once, or three times, moves this number immediately.
fn expected_split_delta(width: W) -> u64 {
    let bytes = match width {
        W::Byte => return 0,
        W::Word => 2,
        W::Dword => 4,
    };
    2 * (direct_cycle_clocks(bytes) - direct_cycle_clocks(1))
}

/// `split_delta` is the bus-clock excess the native role is EXPECTED to carry, which is zero for
/// every aligned row and `expected_split_delta` for a misaligned one.
fn compare_state_with_split(roles: &Roles, split_delta: u64, context: &str) {
    assert_eq!(
        crate::tests::settled_registers(&roles.native),
        crate::tests::settled_registers(&roles.interp),
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
        roles.interp_bus.trace.elapsed_clocks() + split_delta,
        "{context}: bus clocks (expected split delta {split_delta})"
    );
    // The WHOLE array, not a window over `DATA`. A window would hide a store of the wrong width
    // one byte past the operand, which is the exact way a Byte lane running as a Word one fails.
    assert_eq!(
        roles.native_bus.memory, roles.interp_bus.memory,
        "{context}: guest RAM"
    );
}

/// A row that completes NATIVELY: every slot retires in the block and the whole architectural
/// state matches the same number of interpreted steps.
fn lowered(seg: Seg, body: &[u8], slots: u8, seed: Seed, context: &str) {
    lowered_with_split(seg, body, slots, seed, 0, context);
}

fn lowered_with_split(
    seg: Seg,
    body: &[u8],
    slots: u8,
    seed: Seed,
    split_delta: u64,
    context: &str,
) {
    let mut roles = build(seg, body, slots, seed);
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
        u64::from(roles.slots),
        "{context}: every slot must retire natively"
    );
    for _ in 0..roles.slots {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state_with_split(&roles, split_delta, context);
}

/// The form under test plus the five flag consumers, as one block body.
fn with_consumers(seg: Seg, form: &[u8]) -> Vec<u8> {
    let mut body = form.to_vec();
    body.extend_from_slice(&seg.consumer_bytes());
    body
}

/// Two fillers, the form, and the five consumers.
const CONSUMED_SLOTS: u8 = 2 + 1 + CONSUMERS.len() as u8;

/// The count sweep for the imm8 opcodes. 1 is the only count that defines OF; 7 is the last
/// in-width count at Byte; 8, 9, 16 and 31 walk the range where the SDM leaves CF undefined and
/// this tree's oracle -- the interpreter -- does not; 33 tests that the five-bit mask is applied
/// to the RAW immediate, so `shr byte [m], 33` is a shift by 1.
///
/// Zero and 32 are absent on purpose: both mask to zero and are REFUSED at classify, which
/// `a_count_that_masks_to_zero_stays_a_barrier` pins.
const COUNTS: [u8; 8] = [1, 2, 7, 8, 9, 16, 31, 33];

/// Operand seeds, as a dword whose low byte and low word are themselves boundary values. `0x80`,
/// `0x8000` and `0x8000_0000` are the MSB-only cases each width needs; `0x01` is LSB-only;
/// `0x5555_5555` and `0xaaaa_aaaa` alternate, so every count crosses the boundary both ways;
/// `0xf0f0_f0f0` is the negative one SAR's sign fill needs.
const OPERANDS: [u32; 8] = [
    0x0000_0000,
    0x0000_0001,
    0x0000_0080,
    0x0000_8000,
    0x8000_0000,
    0x5555_5555,
    0xaaaa_aaaa,
    0xf0f0_f0f0,
];

/// A domain-real prior EFLAGS state with SF, ZF, PF and AF all SET, bit 1 forced, and NO live
/// descriptor.
///
/// **The no-descriptor half is the whole point.** The recorded carry-seed trap is that a seed of
/// `(0x8d5, pending: true)` never delivers a seeded CF: the live descriptor swallows it, so a CF
/// assertion made under that seed passes even against an emitter that never touches CF at all.
/// `0x8d7` with `live_pending` false is the seed that bites.
const SEEDED_EFLAGS: u32 = 0x8d7;

/// The widths reachable in each segment. All three are reachable in both, and the pairing matters:
/// the byte forms take no prefix in either, and Word and Dword swap which one needs the `0x66`.
const WIDTHS: [W; 3] = [W::Byte, W::Word, W::Dword];

// =============================================================================================
// Admission
// =============================================================================================

/// The row this slice claims, on its own: `0xD1 /1` with a memory operand in a CS.D = 0 segment,
/// `123-talk-shareware`'s 29,698,831-exit census row.
#[test]
fn the_word_memory_ror_by_one_row_is_admitted() {
    let _arm = force_rows(true);
    assert_eq!(
        compile_span(Seg::Sixteen, &by_one_mem(Seg::Sixteen, W::Word, 1)),
        ADMITTED,
        "`ror word [0x3010], 1` in a CS.D = 0 segment must join the block: it is 66.8% of \
         123-talk-shareware's whole static unbound class"
    );
    assert_eq!(
        compile_span(Seg::Sixteen, &CONTROL),
        ADMITTED,
        "control: the harness compiles a register move"
    );
}

/// Every admitted cell of the (opcode x sub-opcode x width x segment) matrix.
#[test]
fn every_admitted_group_two_memory_cell_compiles() {
    let _arm = force_rows(true);
    for seg in SEGMENTS {
        for width in WIDTHS {
            for (label, op) in SUB_OPS {
                for (form, name) in [
                    (imm8_mem(seg, width, op, 3), "imm8"),
                    (by_one_mem(seg, width, op), "by-one"),
                ] {
                    assert_eq!(
                        compile_span(seg, &form),
                        ADMITTED,
                        "{} {label} {} {name} memory form must join the block",
                        seg.label(),
                        width.label()
                    );
                }
            }
        }
    }
}

/// RCL and RCR stay hard boundaries at every opcode, width and segment, with the knobs ON.
#[test]
fn rcl_and_rcr_memory_forms_stay_a_barrier() {
    let _arm = force_rows(true);
    for seg in SEGMENTS {
        for width in WIDTHS {
            for (label, op) in REFUSED_SUB_OPS {
                for (form, name) in [
                    (imm8_mem(seg, width, op, 3), "imm8"),
                    (by_one_mem(seg, width, op), "by-one"),
                ] {
                    assert_eq!(
                        compile_span(seg, &form),
                        REFUSED,
                        "{} {label} {} {name} takes the incoming CF as a rotate input and has no \
                         emitter: it must stay refused",
                        seg.label(),
                        width.label()
                    );
                }
            }
            assert_eq!(
                compile_span(seg, &CONTROL),
                ADMITTED,
                "control: the harness compiles a register move in {}",
                seg.label()
            );
        }
    }
}

/// A count whose five-bit mask is zero is refused, at every sub-opcode and width.
///
/// Not a semantic refusal: the interpreter still reads the operand and writes it back, so an
/// emitted form that elided both would register a static read and store the emitted code never
/// performs.
#[test]
fn a_count_that_masks_to_zero_stays_a_barrier() {
    let _arm = force_rows(true);
    for seg in SEGMENTS {
        for width in WIDTHS {
            for (label, op) in SUB_OPS {
                for count in [0u8, 32, 64, 0xe0] {
                    assert_eq!(
                        compile_span(seg, &imm8_mem(seg, width, op, count)),
                        REFUSED,
                        "{} {label} {} count={count} masks to zero and must stay refused",
                        seg.label(),
                        width.label()
                    );
                }
            }
        }
    }
}

/// The memory form rides EXACTLY the row axis its register sibling does, which is not a uniform
/// axis and this row is the pin on that.
///
/// With both row knobs off:
///
/// * `0xC0`/`0xD0` (the Byte opcodes) refuse EVERY sub-opcode. `/0` and `/1` are on
///   `IZARRAVM_ROTATE_ROWS` and `/4..=7` on `IZARRAVM_BYTE_SHIFT_ROWS`, and the arm reads both
///   above the operand bind, so the memory form goes with the register form.
/// * `0xC1`/`0xD1` refuse `/0` ALONE. `/1` ROR was lowered before the rotate-rows slice and stays
///   ungated by design -- the off arm has to restore the PRE-SLICE world, not a no-rotates world,
///   or an A/B prices two slices as one -- and `/4..=7` are older still.
///
/// A uniform expectation here would be the easier assertion and the wrong one: it would force a
/// knob read into the `0xc1 | 0xd1` arm that the register lane does not have, and the off arm
/// would then stop reproducing the shipped tree.
#[test]
fn the_row_knobs_off_restore_the_pre_slice_barrier() {
    let _arm = force_rows(false);
    for seg in SEGMENTS {
        for width in WIDTHS {
            for (label, op) in SUB_OPS {
                let expected = if width == W::Byte || op == 0 {
                    REFUSED
                } else {
                    ADMITTED
                };
                assert_eq!(
                    compile_span(seg, &by_one_mem(seg, width, op)),
                    expected,
                    "{} {label} {} with the row knobs off",
                    seg.label(),
                    width.label()
                );
            }
        }
        assert_eq!(
            compile_span(seg, &CONTROL),
            ADMITTED,
            "control: the harness compiles a register move in {}",
            seg.label()
        );
    }
}

// =============================================================================================
// The alignment verdict: misaligned is SERVED, page-crossing is REFUSED
// =============================================================================================

/// A MISALIGNED but page-local operand runs natively, with the split bus charge deposited
/// dynamically.
///
/// This is the property that makes the Word row admissible at all. `emit_wide_page_guard` sends
/// every misaligned Word or Dword access to a side exit, and 16-bit DOS code has no alignment
/// discipline: under that guard an odd operand would sit inside the block and exit at that slot on
/// EVERY execution, which is worse than the barrier it replaced. `emit_rotate_shift_mem` keeps the
/// crossing half of the guard and serves the alignment half instead.
///
/// The bus-clock column in `compare_state` is what proves the deposit, and it proves the DOUBLE
/// deposit specifically: a read-modify-write splits twice where a plain store splits once, so an
/// emitter that deposited a single extra charge would read low against the interpreter here and
/// nowhere else.
#[test]
fn a_misaligned_group_two_memory_form_runs_natively() {
    let _arm = force_rows(true);
    for seg in SEGMENTS {
        for (width, at) in [
            (W::Word, DATA + 1),
            (W::Dword, DATA + 1),
            (W::Dword, DATA + 2),
            (W::Dword, DATA + 3),
        ] {
            for (label, op) in SUB_OPS {
                for count in [1u8, 3] {
                    let context = format!(
                        "{} {label} {} misaligned at {at:#x} count={count}",
                        seg.label(),
                        width.label()
                    );
                    lowered_with_split(
                        seg,
                        &with_consumers(seg, &imm8_mem_at(seg, width, op, count, at)),
                        CONSUMED_SLOTS,
                        Seed::new(0xaaaa_aaaa).flags(SEEDED_EFLAGS),
                        expected_split_delta(width),
                        &context,
                    );
                }
            }
        }
    }
}

/// The one shape `certainly_exits_on_alignment` can decide statically stays ADMITTED.
///
/// That rule refuses a slot whose operand address is decidable at compile time and fails the
/// alignment guard the emitter will produce, because such a slot would exit on every execution.
/// It reads `DirectKind::unrelaxed_wide_guard_access`, and `RotateShiftMem` is deliberately absent
/// from that list: this emitter SERVES a misaligned access instead of exiting on it, so the rule
/// must not fire for it. A `2E`-prefixed, displacement-only operand at an odd address is the exact
/// shape the rule decides, so this row goes red the moment the kind is added to that accessor.
#[test]
fn a_statically_odd_cs_override_word_rotate_is_still_admitted() {
    let _arm = force_rows(true);
    // REAL MODE ONLY, and the restriction is the fixture's rather than the rule's. `flat_cpu`'s CS
    // is an executable segment (access byte 0x9B), so a WRITE through it is refused by
    // `segment_access_supported` long before any alignment question, and a 32-bit row here would
    // assert nothing about this rule. Real mode is also the shape the rule was written for: a
    // real-mode segment base is a multiple of 16, so the operand's parity is the displacement's.
    {
        let seg = Seg::Sixteen;
        for (label, op) in SUB_OPS {
            // `2E` CS override in front of the by-one form. A CS-override, displacement-only
            // operand is the ONE shape `certainly_exits_on_alignment` can decide statically, and
            // at an odd displacement it decides MISALIGNED. Every other unrelaxed memory kind is
            // refused there; this one must not be, because its emitter serves the access.
            let mut form = vec![0x2eu8];
            form.extend_from_slice(&by_one_mem_at(seg, W::Word, op, DATA + 1));
            assert_eq!(
                compile_span(seg, &form),
                ADMITTED,
                "{} {label} cs:[odd] word must still join the block: the alignment half of the \
                 guard is served, not refused",
                seg.label()
            );
        }
    }
}

/// A PAGE-CROSSING operand still side exits, and the block still forms around it.
///
/// The two-byte access at `0x2FFF` spans two FastMap entries where the slot resolved one bias, so
/// this is the half of the guard the emitter keeps. The block compiles, the slot exits, and the
/// interpreter runs the instruction. The row asserts the architectural result is still right,
/// which is what a side exit has to guarantee.
#[test]
fn a_page_crossing_group_two_memory_form_side_exits_and_still_agrees() {
    let _arm = force_rows(true);
    const CROSSING: u32 = 0x2fff;
    for seg in SEGMENTS {
        for (label, op) in SUB_OPS {
            let mut roles = build(
                seg,
                &by_one_mem_at(seg, W::Word, op, CROSSING),
                3,
                Seed::new(0x0f0f_0f0f).flags(SEEDED_EFLAGS),
            );
            let _ = roles
                .native
                .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
                .unwrap();
            // Whatever the block did, finish the three slots on both roles and compare. The
            // native role may have exited part-way, so it is stepped to the same place.
            while roles.native.registers.eip < ENTRY + 6 && !roles.native.halted {
                roles.native.cycle(&mut roles.native_bus).unwrap();
            }
            for _ in 0..3 {
                roles.interp.cycle(&mut roles.interp_bus).unwrap();
            }
            assert_eq!(
                roles.native_bus.memory[CROSSING as usize..CROSSING as usize + 2],
                roles.interp_bus.memory[CROSSING as usize..CROSSING as usize + 2],
                "{} {label} crossing: the operand must still be right",
                seg.label()
            );
            assert_eq!(
                roles.native.eflags(),
                roles.interp.eflags(),
                "{} {label} crossing: EFLAGS",
                seg.label()
            );
        }
    }
}

/// The Mode 13h aperture's misaligned word rotate must side exit, not run natively.
///
/// N1 from the review: `memory.rs`'s `is_mode13_aperture` and `run.rs`'s split-cost comment both
/// assert that a misaligned access to the aperture is refused elsewhere in the tree, so
/// `run.rs`'s RAM split-cost pool never carries an aperture byte. This emitter's whole point is to
/// SERVE a misaligned in-page RAM access -- see `a_misaligned_group_two_memory_form_runs_natively`
/// -- but that service must stop at the aperture boundary: `emit_alu_mem_dest`,
/// `emit_rmw_inc_dec_dword` and `emit_push_mem` all refuse Mode 13h outright or refuse it whenever
/// it is misaligned, and this kind must not be the first native site that bills an aperture access
/// through the RAM-only pool.
///
/// The operand sits at `MODE13_BASE + 1`, which is misaligned but stays inside the aperture's own
/// page, so this row isolates misalignment from page-crossing (`emit_page_cross_bound` already
/// refuses a crossing access at any kind, aperture included, and is not what this row is about).
///
/// 32-bit segments only. `by_one_mem_at`'s Sixteen arm encodes a disp16-only operand against a
/// FLAT (base-zero) real-mode segment, so `MODE13_BASE + 1` truncates to `0x0001` and never
/// reaches the aperture at all -- the emitter-level guard this row proves is segment-width
/// independent, so the 32-bit segment alone is enough to exercise it.
#[test]
fn a_misaligned_mode13_group_two_memory_form_side_exits() {
    let _arm = force_rows(true);
    const MODE13_BASE: u32 = 0x000a_0000;
    const MODE13_MEMORY_LEN: usize = 0x000b_0000;
    for seg in [Seg::ThirtyTwo] {
        for (label, op) in SUB_OPS {
            let mut roles = build_with_pages(
                seg,
                &by_one_mem_at(seg, W::Word, op, MODE13_BASE + 1),
                3,
                Seed::new(0x0f0f_0f0f).flags(SEEDED_EFLAGS),
                MODE13_MEMORY_LEN,
                &[MODE13_BASE],
            );
            let _ = roles
                .native
                .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
                .unwrap();
            assert!(
                roles.native.registers.eip < ENTRY + 6,
                "{} {label} misaligned mode13 word rotate ran natively to completion, so the \
                 aperture's misaligned access was served rather than refused",
                seg.label()
            );
            // Whatever the block did, finish the three slots on both roles and compare: a side
            // exit still has to leave the architectural state right.
            while roles.native.registers.eip < ENTRY + 6 && !roles.native.halted {
                roles.native.cycle(&mut roles.native_bus).unwrap();
            }
            for _ in 0..3 {
                roles.interp.cycle(&mut roles.interp_bus).unwrap();
            }
            assert_eq!(
                roles.native_bus.memory[MODE13_BASE as usize..MODE13_BASE as usize + 3],
                roles.interp_bus.memory[MODE13_BASE as usize..MODE13_BASE as usize + 3],
                "{} {label} mode13 misaligned: the operand must still be right",
                seg.label()
            );
            assert_eq!(
                roles.native.eflags(),
                roles.interp.eflags(),
                "{} {label} mode13 misaligned: EFLAGS",
                seg.label()
            );
        }
    }
}

// =============================================================================================
// The differential
// =============================================================================================

/// `0xD0`/`0xD1`, the by-one encodings: every sub-opcode, every width, every operand, in both
/// segment kinds, with the five flag consumers reading CF, OF, SF, ZF and PF back through EMITTED
/// code.
#[test]
fn group_two_memory_by_one_forms_match_the_interpreter() {
    let _arm = force_rows(true);
    for seg in SEGMENTS {
        for width in WIDTHS {
            for (label, op) in SUB_OPS {
                for operand in OPERANDS {
                    let context = format!(
                        "{} {label} {} by-one data={operand:#010x}",
                        seg.label(),
                        width.label()
                    );
                    lowered(
                        seg,
                        &with_consumers(seg, &by_one_mem(seg, width, op)),
                        CONSUMED_SLOTS,
                        Seed::new(operand).flags(SEEDED_EFLAGS),
                        &context,
                    );
                }
            }
        }
    }
}

/// `0xC0`/`0xC1`, the imm8 encodings, across the whole count sweep.
#[test]
fn group_two_memory_imm8_forms_match_the_interpreter_for_every_count() {
    let _arm = force_rows(true);
    for seg in SEGMENTS {
        for width in WIDTHS {
            for (label, op) in SUB_OPS {
                for count in COUNTS {
                    for operand in OPERANDS {
                        let context = format!(
                            "{} {label} {} count={count} data={operand:#010x}",
                            seg.label(),
                            width.label()
                        );
                        lowered(
                            seg,
                            &with_consumers(seg, &imm8_mem(seg, width, op, count)),
                            CONSUMED_SLOTS,
                            Seed::new(operand).flags(SEEDED_EFLAGS),
                            &context,
                        );
                    }
                }
            }
        }
    }
}

/// Restores the ambient `IZARRAVM_DIRECT_EAGER_FLAGS` reading on the way out of a fixture --
/// normally OR by panic. The override is thread-local and the harness reuses threads.
struct EagerOverride;

impl Drop for EagerOverride {
    fn drop(&mut self) {
        jit::direct::set_direct_eager_flags_for_test(None);
    }
}

/// The whole matrix on the LAZY-DESCRIPTOR arm (`IZARRAVM_DIRECT_EAGER_FLAGS=0`), which emits a
/// different tail: `emit_set_cf_only`'s two-branch descriptor sequence and `emit_clear_pending`'s
/// four stores, in place of one publish each.
///
/// **A recorded SURVIVOR sits here, and it is recorded rather than papered over.** Replacing the
/// rotate-above-1 tail's `emit_set_cf_only(e)` with a wholesale `store eflags, RBP` -- the publish
/// a rotate must NOT do, since it architecturally preserves SF, ZF, PF and AF -- leaves this file
/// green on BOTH arms. Two reasons stack:
///
/// * on the shipped eager arm `emit_set_cf_only` IS that store, so the two spellings are the same
///   instruction and nothing can separate them;
/// * on the lazy arm the descriptor branch is unreachable from inside a native block anyway.
///   `run.rs`'s entry clear (E1) materialises and clears any live descriptor before the block is
///   entered, and it is deliberately NOT under the knob, so a native slot never sees one. Only an
///   `InterpretOne` call-out can install one mid-block, and this kind emits none.
///
/// The call therefore stays because it is the shared idiom for a CF-only write and because the
/// descriptor branch is the correct answer if a future mid-block producer ever creates one -- not
/// because a fixture can currently tell. The register lanes carry the identical blind spot. What
/// this row DOES buy is that the lazy arm's emitted tail is executed and differentially compared
/// at every cell, rather than being shipped untested behind a default-on knob.
///
/// The arm is read at EMISSION time, so the override is set BEFORE `build` compiles anything.
#[test]
fn a_memory_rotate_above_count_one_routes_cf_through_a_live_descriptor() {
    let _arm = force_rows(true);
    jit::direct::set_direct_eager_flags_for_test(Some(false));
    let _eager = EagerOverride;
    for seg in SEGMENTS {
        for width in WIDTHS {
            for (label, op) in SUB_OPS {
                for count in [1u8, 2, 5, 31] {
                    let context = format!(
                        "{} {label} {} count={count} lazy descriptor",
                        seg.label(),
                        width.label()
                    );
                    lowered(
                        seg,
                        &with_consumers(seg, &imm8_mem(seg, width, op, count)),
                        CONSUMED_SLOTS,
                        Seed::new(0xaaaa_aaaa).flags(SEEDED_EFLAGS).pending(),
                        &context,
                    );
                }
            }
        }
    }
}

/// A live lazy-flag descriptor across every admitted cell.
///
/// This is where the rotate and shift flag contracts diverge most sharply: a rotate above count 1
/// writes CF THROUGH the descriptor (`emit_set_cf_only`) and must leave the other five bits under
/// its authority, while a shift and a rotate by exactly 1 must destroy it and publish live flags.
#[test]
fn group_two_memory_forms_agree_with_a_live_descriptor() {
    let _arm = force_rows(true);
    for seg in SEGMENTS {
        for width in WIDTHS {
            for (label, op) in SUB_OPS {
                for count in [1u8, 2, 5] {
                    let context = format!(
                        "{} {label} {} count={count} live descriptor",
                        seg.label(),
                        width.label()
                    );
                    lowered(
                        seg,
                        &with_consumers(seg, &imm8_mem(seg, width, op, count)),
                        CONSUMED_SLOTS,
                        Seed::new(0xaaaa_aaaa).flags(SEEDED_EFLAGS).pending(),
                        &context,
                    );
                }
            }
        }
    }
}

/// A rotate PRESERVES SF, ZF, PF and AF at every count, including 1. A shift redefines SF, ZF and
/// PF, so this row is a rotate-only claim and is asserted on the raw EFLAGS image rather than
/// through the consumers.
///
/// The seed sets all four bits and the operands are chosen so an emitter that published the whole
/// shadow -- what `emit_commit_shift_flags` does, and what a rotate must NOT do -- would clear at
/// least one of them.
#[test]
fn memory_rotates_preserve_sf_zf_pf_and_af() {
    let _arm = force_rows(true);
    for seg in SEGMENTS {
        for width in WIDTHS {
            for op in [0u8, 1] {
                for count in [1u8, 2, 7, 31] {
                    let seed = Seed::new(0x0000_0001).flags(SEEDED_EFLAGS);
                    let mut roles = build(seg, &imm8_mem(seg, width, op, count), 3, seed);
                    assert!(
                        roles
                            .native
                            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
                            .unwrap(),
                        "block did not run natively"
                    );
                    let flags = roles.native.eflags();
                    let preserved =
                        crate::FLAG_SF | crate::FLAG_ZF | crate::FLAG_PF | crate::FLAG_AF;
                    assert_eq!(
                        flags & preserved,
                        SEEDED_EFLAGS & preserved,
                        "{} /{op} {} count={count}: a rotate must leave SF, ZF, PF and AF exactly \
                         where it found them",
                        seg.label(),
                        width.label()
                    );
                }
            }
        }
    }
}

/// The stray-write witness, stated as its own row rather than left to the whole-RAM compare.
///
/// A Byte form must write ONE byte and a Word form TWO. The seed puts a distinct value in every
/// byte of the dword at `DATA`, so a lowering that stored the wrong width leaves the bytes above
/// its operand changed.
#[test]
fn a_narrow_memory_form_writes_only_its_own_width() {
    let _arm = force_rows(true);
    for seg in SEGMENTS {
        for (width, untouched) in [(W::Byte, 1usize), (W::Word, 2)] {
            for (label, op) in SUB_OPS {
                // 0x9234_5678 puts a 1 in the MSB of all three widths, so every sub-opcode here
                // genuinely changes the operand and the row cannot pass against an emitter that
                // wrote nothing at all.
                const OPERAND: u32 = 0x9234_5678;
                let seed = Seed::new(OPERAND).flags(SEEDED_EFLAGS);
                let mut roles = build(seg, &by_one_mem(seg, width, op), 3, seed);
                assert!(
                    roles
                        .native
                        .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
                        .unwrap(),
                    "block did not run natively"
                );
                for _ in 0..3 {
                    roles.interp.cycle(&mut roles.interp_bus).unwrap();
                }
                let window = DATA as usize..DATA as usize + 4;
                assert_eq!(
                    roles.native_bus.memory[DATA as usize + untouched..window.end],
                    OPERAND.to_le_bytes()[untouched..],
                    "{} {label} {}: the bytes above the operand must be untouched",
                    seg.label(),
                    width.label()
                );
                assert_ne!(
                    roles.native_bus.memory[window.clone()],
                    OPERAND.to_le_bytes()[..],
                    "{} {label} {}: the write-back itself must have happened",
                    seg.label(),
                    width.label()
                );
                assert_eq!(
                    roles.native_bus.memory[window.clone()],
                    roles.interp_bus.memory[window],
                    "{} {label} {}: the written operand must match the interpreter",
                    seg.label(),
                    width.label()
                );
            }
        }
    }
}
