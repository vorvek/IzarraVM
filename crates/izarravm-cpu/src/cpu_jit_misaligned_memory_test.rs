// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Guard 3: the MISALIGNED memory admission at the two lean one-lookup sites.
//!
//! Before this slice `emit_wide_page_guard` refused every access whose address was not a multiple
//! of its `alignment_bytes()`, and on the payload fixture that single `jnz` was 99.97% of all
//! native side exits. The guard is now two independent halves: the page-CROSSING bound, which
//! still refuses at all thirteen call sites, and the ALIGNMENT test, which at the lean load and
//! store sites targets the site's own slow stub instead of a side exit.
//!
//! What each row here is for:
//!
//! | row | what would break it |
//! |---|---|
//! | misaligned Word/Dword reads run natively | the relaxation not landing, or landing at the wrong half |
//! | page-edge IN page runs, page-edge CROSSING exits | the crossing bound being relaxed along with the alignment test |
//! | crossing AND misaligned exits | the two halves emitted in the wrong ORDER -- with alignment first, a crossing access reaches the recovery stub and is served across a page boundary its FastMap entry does not cover |
//! | Mode 13h misaligned exits | the aperture refusal being dropped from the stub, or placed after the access |
//! | non-relaxed sites still exit | a site being relaxed that this slice does not touch |
//! | the split charge | the deposit being omitted, or sized `bytes()` instead of `bytes() - 1` |
//!
//! **The bus-clock comparison against the interpreter is NOT the shape used here, and that is a
//! substantive point rather than a convenience.** The design's charge-equality claim -- a
//! misaligned N-byte access costs N byte cycles natively and interpreted alike -- is a property of
//! `MachineBus`, where `BusCycle::clocks_for` ignores width and the interpreter's `should_split`
//! turns a misaligned access into N byte reads. `TestBus` models neither: its direct-page wait
//! states are width-dependent (0/1/3 for Byte/Word/Dword) and its slow read path charges ONE cycle
//! at zero wait states without splitting. A `native == interpreted` bus-clock assertion here would
//! therefore assert something false about `TestBus` while proving nothing about the real bus.
//!
//! So the rows below assert the exact bus-clock DELTA at `TestBus`'s own dials instead, which is
//! strictly sharper for the thing this slice can get wrong: it pins that a misaligned N-byte
//! access charges one wide cycle plus exactly `N - 1` byte cycles. Off-by-one in either direction
//! fails. The charge EQUALITY against the real bus is asserted at the dial level in
//! `izarravm-machine`'s `machine_bus_timing_test.rs`, where a real `MachineBus` exists.
//!
//! Everything else -- registers, lazy flags, EFLAGS, the halt latch, core clocks and the WHOLE of
//! guest RAM -- is still compared against a block-free interpreted role, and the whole-array RAM
//! compare is what catches a wrong-value store.

use super::*;

/// `mov esi,esi`, the leading slot that keeps the tested opcode off the block entry. An opcode at
/// a block's entry slot parks the block on the interpreter, so an entry-position fixture would
/// certify nothing.
const MOV_ESI_ESI: [u8; 2] = [0x89, 0xf6];
/// `mov edi,edi`, the trailing slot, so the tested opcode is never the last one either.
const MOV_EDI_EDI: [u8; 2] = [0x89, 0xff];

/// The operand page: its own page, far from the code at `ENTRY` and the stack at `STACK_TOP`.
const OPERAND_PAGE: u32 = 0x5000;
/// The page above it, so a page-CROSSING access has real memory on the far side and the fixture
/// separates "refused because it crosses" from "faulted because nothing is there".
const NEXT_PAGE: u32 = 0x6000;
/// The canonical Mode 13h aperture base. `fast_map` classifies a page as `Mode13` purely by
/// physical range, so mapping this page is all it takes to build the aperture case.
const MODE13_PAGE: u32 = 0x000a_0000;

/// Big enough to contain the Mode 13h aperture, so `MODE13_PAGE` is real storage.
const MEMORY_LEN: usize = 0x000b_0000;

/// TestBus's direct-page wait states, restated here so the expected deltas below are derived
/// rather than copied. Keep in step with `TestBus::direct_page_wait_states`.
fn direct_cycle_clocks(bytes: u32) -> u64 {
    match bytes {
        1 => 2,
        2 => 3,
        4 => 5,
        other => unreachable!("no TestBus dial for a {other}-byte access"),
    }
}

/// What the block-free interpreted role pays for ONE misaligned access: `lookup_access` refuses a
/// misaligned width outright, so the access leaves the FastMap and lands on `TestBus::read_memory`
/// / `write_memory`, which record a single cycle at zero wait states and never split.
const INTERPRETED_MISALIGNED_CLOCKS: u64 = 2;

/// The bus-clock delta a natively-served misaligned `bytes`-wide access must produce against the
/// interpreted role, at `TestBus`'s dials: one wide cycle for the access the static count already
/// carries, plus `bytes - 1` byte cycles from the split deposit, less what the interpreter paid.
fn expected_split_delta(bytes: u32) -> u64 {
    direct_cycle_clocks(bytes) + u64::from(bytes - 1) * direct_cycle_clocks(1)
        - INTERPRETED_MISALIGNED_CLOCKS
}

/// A distinct byte at every address, so a store of the wrong WIDTH or the wrong VALUE changes
/// guest RAM even when it writes plausible bytes in the right place.
fn memory_fill() -> Vec<u8> {
    let mut memory = vec![0u8; MEMORY_LEN];
    for (i, byte) in memory.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37).wrapping_add(11) & 0xff) as u8;
    }
    memory
}

struct Roles {
    native: CpuGsw,
    native_bus: TestBus,
    interp: CpuGsw,
    interp_bus: TestBus,
    block: jit::direct::CompiledBlock,
    /// Linear address of the tested opcode, i.e. where a guarded exit must leave EIP.
    body_at: u32,
}

/// Map one page for read and write on the fast map. A memory-form slot silently never compiles
/// without this, and the fixture would then certify a refusal it did not intend to test.
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
/// decode lines on the interpreter role, and seed both identically.
fn build(body: &[u8]) -> Roles {
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
        for page in [
            OPERAND_PAGE,
            NEXT_PAGE,
            MODE13_PAGE,
            (STACK_TOP - 4) & !0xfff,
        ] {
            map_page(cpu, bus, page);
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
            panic!("structurally rejected: the tested form never reached the memory guard")
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
        // 0xdead in every high half; the low half is the register index, so a row that reads or
        // writes the wrong register is a distinguishable failure rather than a coincidence.
        cpu.registers.gpr = std::array::from_fn(|i| 0xdead_0000 | (0xa0 + i as u32));
        cpu.registers.set_esp(STACK_TOP);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
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
        body_at,
    }
}

/// Everything except bus clocks. See the module header for why the bus-clock axis is asserted as
/// an exact delta instead of an equality here.
fn compare_state(roles: &Roles, context: &str) {
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
    // The whole array, not a window around the operand. A store that widened, or that wrote a
    // STALE value, touches bytes the interpreter did not, and a window sized to the intended
    // access is exactly the wrong shape to see either.
    assert_eq!(
        roles.native_bus.memory, roles.interp_bus.memory,
        "{context}: guest RAM"
    );
}

/// A row that completes NATIVELY through the misaligned recovery: all three slots retire in the
/// block, the architectural state matches three interpreted steps, and the bus-clock delta is
/// exactly the split charge for `bytes` -- one wide cycle plus `bytes - 1` byte cycles.
fn lowered_misaligned(body: &[u8], bytes: u32, context: &str) {
    let mut roles = build(body);
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
    compare_state(&roles, context);
    assert_eq!(
        roles.native_bus.trace.elapsed_clocks() - roles.interp_bus.trace.elapsed_clocks(),
        expected_split_delta(bytes),
        "{context}: the split charge must be one wide cycle plus {} byte cycles",
        bytes - 1
    );
}

/// A row that completes NATIVELY with no misaligned access at all: the aligned control. Bus clocks
/// must match the interpreter EXACTLY here -- an aligned access takes the same one direct cycle in
/// both roles -- which is what says the split deposit is conditional rather than unconditional.
fn lowered_aligned(body: &[u8], context: &str) {
    let mut roles = build(body);
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "{context}: block did not run natively"
    );
    for _ in 0..3 {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state(&roles, context);
    assert_eq!(
        roles.native_bus.trace.elapsed_clocks(),
        roles.interp_bus.trace.elapsed_clocks(),
        "{context}: an ALIGNED access must charge exactly what it charged before the slice"
    );
}

/// A row whose emitted memory guard REFUSES. The native run must end at the tested opcode with the
/// instruction un-started -- no byte of guest RAM written, no register touched -- and the
/// interpreter must then execute it and reach the same state.
fn guarded(body: &[u8], exits: fn(&CpuGsw) -> u64, context: &str) {
    let mut roles = build(body);
    let retired = roles.native.perf_counters().jit_direct_insns;
    let before = exits(&roles.native);
    let ram_before = roles.native_bus.memory.to_vec();
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "{context}: block did not run natively"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - retired,
        1,
        "{context}: only the slot BEFORE the tested opcode may retire natively"
    );
    assert_eq!(
        exits(&roles.native) - before,
        1,
        "{context}: exactly one side exit of the expected reason"
    );
    assert_eq!(
        roles.native.registers.eip, roles.body_at,
        "{context}: the run must end AT the tested opcode, not after it"
    );
    // The refusal must be TRANSACTIONAL: not one byte of the access may have landed. This is the
    // assertion that separates "refused before the access" from "refused after it", and it is the
    // whole content of the Mode 13h store row -- an aperture byte written before the refusal would
    // be written a second time by the interpreter's re-execution.
    assert_eq!(
        &roles.native_bus.memory[..],
        &ram_before[..],
        "{context}: the refusal must leave guest RAM untouched"
    );
    roles.interp.cycle(&mut roles.interp_bus).unwrap();
    compare_state(&roles, &format!("{context}: at the guard"));

    for _ in 0..2 {
        roles.native.cycle(&mut roles.native_bus).unwrap();
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state(&roles, &format!("{context}: after the re-execution"));
}

fn alignment_exits(cpu: &CpuGsw) -> u64 {
    cpu.perf_counters().jit_direct_exit_cross_page_or_alignment
}

fn kind_exits(cpu: &CpuGsw) -> u64 {
    cpu.perf_counters().jit_direct_exit_unavailable_or_kind
}

/// `[disp32]` addressing: ModRM mod 00, rm 101. No base register, so no row's operand address can
/// depend on the poisoned register seeds.
fn disp32(opcode_head: &[u8], reg: u8, at: u32) -> Vec<u8> {
    let mut body = opcode_head.to_vec();
    body.push((reg << 3) | 0b101);
    body.extend_from_slice(&at.to_le_bytes());
    body
}

/// `movzx ebx, word [at]` — a Word READ through the lean one-lookup load site.
fn word_load(at: u32) -> Vec<u8> {
    disp32(&[0x0f, 0xb7], 3, at)
}

/// `mov ebx, dword [at]` — a Dword READ through the same site.
fn dword_load(at: u32) -> Vec<u8> {
    disp32(&[0x8b], 3, at)
}

// ---------------------------------------------------------------------------------------------
// The admission matrix: reads
// ---------------------------------------------------------------------------------------------

/// A misaligned Word read runs natively and charges two byte cycles' worth.
///
/// Every odd offset in a 16-byte window, so a row that happened to work at one alignment and not
/// another fails. The 386 admits misaligned accesses architecturally; before this slice the native
/// backend refused them all, which was a missed lowering rather than a divergence.
#[test]
fn a_misaligned_word_read_runs_natively_and_charges_the_split() {
    for offset in (1..0x20).step_by(2) {
        let at = OPERAND_PAGE + offset;
        lowered_misaligned(&word_load(at), 2, &format!("movzx ebx, word [{at:#x}]"));
    }
}

/// The sixteen-bit DESTINATION form of the same read, moved here from `word_memory` where it
/// asserted the old refusal.
///
/// It is a different lowering from the row above, not a duplicate: at Word operand size MOVZX
/// defines the destination's low 16 bits and PRESERVES its high 16, so the register seeds' `0xdead`
/// high halves are load-bearing. `word_memory` cannot host it any more -- that module's `lowered`
/// asserts bus clocks EQUAL to the interpreter, and a natively-served misaligned access charges
/// more than `TestBus`'s non-splitting slow path.
#[test]
fn a_misaligned_word_read_into_a_sixteen_bit_destination_runs_natively() {
    for offset in [1u32, 3, 0x11] {
        let at = OPERAND_PAGE + 0x400 + offset;
        let mut body = vec![0x66u8];
        body.extend_from_slice(&word_load(at));
        lowered_misaligned(&body, 2, &format!("movzx bx, word [{at:#x}]"));
    }
}

/// A misaligned Dword read runs natively at all three misalignments, and the ALIGNED one is the
/// control that says the deposit is conditional.
///
/// All three sub-alignments matter separately: `+1` and `+3` are odd, `+2` is even but not a
/// multiple of four, and an implementation that tested `al & 1` rather than
/// `al & (alignment_bytes() - 1)` would serve `+2` while charging it as aligned.
#[test]
fn a_misaligned_dword_read_runs_natively_at_every_sub_alignment() {
    let base = OPERAND_PAGE + 0x100;
    lowered_aligned(&dword_load(base), "mov ebx, dword [aligned]");
    for offset in 1..4 {
        let at = base + offset;
        lowered_misaligned(&dword_load(at), 4, &format!("mov ebx, dword [{at:#x}]"));
    }
}

/// The page edge, both sides of it, and this is the row the guard ORDER exists for.
///
/// An access that ends on the page's last byte is served; one that would run past it is refused,
/// because the FastMap entry the pointer was formed against covers exactly one page. The crossing
/// bound is emitted BEFORE the alignment test precisely so a crossing access can never reach the
/// recovery path the alignment test now targets — reverse the two halves and the last three rows
/// here are served across a page boundary instead of exiting.
#[test]
fn a_page_edge_access_runs_inside_the_page_and_exits_when_it_crosses() {
    // Word: 0xFFD and 0xFFE end inside the page (0xFFE is aligned, so it is the control); 0xFFF
    // crosses.
    lowered_misaligned(
        &word_load(OPERAND_PAGE + 0xffd),
        2,
        "movzx ebx, word [page+0xffd]",
    );
    lowered_aligned(&word_load(OPERAND_PAGE + 0xffe), "word [page+0xffe]");
    guarded(
        &word_load(OPERAND_PAGE + 0xfff),
        alignment_exits,
        "movzx ebx, word [page+0xfff] crosses",
    );

    // Dword: 0xFF9/0xFFA/0xFFB end inside; 0xFFD/0xFFE/0xFFF cross. 0xFFC is the aligned control.
    for offset in [0xff9u32, 0xffa, 0xffb] {
        let at = OPERAND_PAGE + offset;
        lowered_misaligned(&dword_load(at), 4, &format!("dword [{at:#x}] fits"));
    }
    lowered_aligned(&dword_load(OPERAND_PAGE + 0xffc), "dword [page+0xffc]");
    for offset in [0xffdu32, 0xffe, 0xfff] {
        let at = OPERAND_PAGE + offset;
        guarded(
            &dword_load(at),
            alignment_exits,
            &format!("dword [{at:#x}] crosses"),
        );
    }
}

/// A Mode 13h aperture read stays refused even when it is misaligned, and stays refused for the
/// aperture's sake rather than the alignment's.
///
/// The refusal lives at the counting read stub's mode13 tail, BEFORE the permission check, which
/// is where the pre-slice guard sat relative to every check: refusing later would re-attribute a
/// cpl3 aperture case from alignment to Permission.
///
/// The exit REASON is `UnavailableOrKind`, not `CrossPageOrAlignment`, and that is deliberate: a
/// dedicated status would add a compare and a branch to every read site's cold dispatch, roughly
/// ten per block, for an attribution difference over a population bounded near 0.01%.
#[test]
fn a_misaligned_mode13_read_still_exits() {
    for offset in [1u32, 3] {
        let at = MODE13_PAGE + 0x100 + offset;
        guarded(
            &word_load(at),
            kind_exits,
            &format!("movzx ebx, word [mode13+{offset}]"),
        );
    }
    let at = MODE13_PAGE + 0x101;
    guarded(&dword_load(at), kind_exits, "mov ebx, dword [mode13+1]");
}

// ---------------------------------------------------------------------------------------------
// The sites this slice does NOT relax
// ---------------------------------------------------------------------------------------------

/// The eleven non-relaxed sites keep refusing every misaligned access, and two of them are checked
/// here rather than argued: the read-modify-write ALU destination and the memory INC.
///
/// They are not an oversight. An RMW slot needs a read deposit AND a write deposit inside one
/// slot, which is its own change; until then the whole guard refuses there, unchanged.
#[test]
fn the_non_relaxed_sites_still_refuse_a_misaligned_access() {
    let at = OPERAND_PAGE + 0x201;

    // `add dword [at], 0x01` -- `emit_alu_mem_dest`, the read-modify-write site.
    let mut alu = disp32(&[0x83], 0, at);
    alu.push(0x01);
    guarded(&alu, alignment_exits, "add dword [odd], imm8");

    // `inc dword [at]` -- `emit_rmw_inc_dec_dword`.
    let inc = disp32(&[0xff], 0, at);
    guarded(&inc, alignment_exits, "inc dword [odd]");
}

/// x87 stays refused at every misalignment. `Qword` and `Tbyte` deliberately ask for 4-byte
/// alignment rather than their own size, because the interpreter issues an m64 as two independently
/// 4-aligned dword transactions; admitting a 2-aligned m80 would diverge on bus timing rather than
/// on bytes. Nothing in this slice touches that, and this row says so.
#[test]
fn a_misaligned_x87_access_still_exits() {
    // `fld qword [at]` -- DD /0.
    for offset in [1u32, 2, 3] {
        let at = OPERAND_PAGE + 0x300 + offset;
        guarded(
            &disp32(&[0xdd], 0, at),
            alignment_exits,
            &format!("fld qword [+{offset}]"),
        );
    }
}
