// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Native CALL FAR ptr16:16 (`0x9A`) in real mode and V86, behind `IZARRAVM_DIRECT_RETF_V86`.
//!
//! L8 (`dev_docs/2026-08-31-corpus-lever-plan.md`, "L7 verdict" and "L8" sections). L7 Part 1
//! made the Direct dispatcher reachable at a non-continuable block entry; this is Part 2, the
//! `classify` arm and emitter that let those reachable `0x9A` sites actually compile.
//!
//! Mirrors `cpu_jit_retf_v86_test.rs`'s harness shape closely, adapted for a CALL rather than a
//! RETURN: the stack starts EMPTY (nothing needs to be pre-seeded, since nothing is popped), and
//! the differential compares the pushed frame's bytes as part of the whole-RAM comparison
//! `compare_state` already performs.

use super::*;

/// EIP of the block entry. The entry CS base is 0, so this is also its linear address.
const ENTRY: u32 = 0x100;
/// The far call's target: selector 0x0020 (base 0x200) at offset 0x0400, linear 0x600.
const TARGET_SELECTOR: u16 = 0x0020;
const TARGET_OFFSET: u16 = 0x0400;
const TARGET_LINEAR: u32 = 0x0600;
/// SS is base 0 with SP here, clear of the code and of the target.
const STACK_SP: u32 = 0x0700;
/// The same stack pointer with a POISONED high half. SS.B is 0 in every fixture here, so bits
/// 31..16 of ESP are architecturally untouched by a push: `alu_r16_imm16` preserves them where an
/// `add`/`sub r32` would clear them, and with a ZERO high half the two are indistinguishable.
const STACK_ESP: u32 = 0xdead_0000 | STACK_SP;

/// How much guest RAM every fixture here stages. See `memory_fill`.
const MEMORY_LEN: u32 = 0x2_0000;

/// A distinct byte at every address, so a read or write of the wrong WIDTH differs from the
/// interpreter even when the intended bytes happen to match.
fn memory_fill() -> Vec<u8> {
    let mut memory = vec![0u8; MEMORY_LEN as usize];
    for (i, byte) in memory.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37).wrapping_add(11) & 0xff) as u8;
    }
    memory
}

fn select_arm(arm: jit::direct::RetfArm) {
    jit::direct::set_direct_retf_v86_for_test(Some(arm));
    assert_eq!(
        jit::direct::direct_retf_v86(),
        arm,
        "the fixture override must decide the arm, not the ambient IZARRAVM_DIRECT_RETF_V86"
    );
}

/// Plain real mode, CS.D = 0 and SS.B = 0: the `on` arm's population.
fn real_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.set_esp(STACK_ESP);
    cpu.set_eip(ENTRY);
    cpu
}

/// The same machine in V86.
fn v86_cpu() -> CpuGsw {
    let mut cpu = real_cpu();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x202 | FLAG_VM | (3 << 12);
    cpu.cpl = 3;
    assert!(cpu.is_v86_mode(), "the V86 fixture must actually be in V86");
    cpu
}

/// PROTECTED 16-bit: PE set, not V86, CS.D = 0 and SS.B = 0. 16-bit rather than flat, so a
/// refusal here can only be the MODE term and not the operand-size or stack-width terms.
fn protected16_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE;
    let mut cs = SegmentRegister::flat(0x08, 0x9b);
    cs.default_size_32 = false;
    cs.base = 0;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        let mut data = SegmentRegister::flat(0x10, 0x93);
        data.default_size_32 = false;
        data.base = 0;
        cpu.registers.set_segment(segment, data);
    }
    cpu.registers.set_esp(STACK_ESP);
    cpu.set_eip(ENTRY);
    assert!(cpu.is_protected_mode() && !cpu.is_v86_mode());
    assert!(!cpu.stack_is_32bit());
    cpu
}

/// Real mode with an UNREAL stack: SS.B = 1 while the CPU is still in real mode. Isolates the
/// SS.B term of `call_far_admitted_here`.
fn unreal_stack_cpu() -> CpuGsw {
    let mut cpu = real_cpu();
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    assert!(!cpu.is_protected_mode());
    assert!(cpu.stack_is_32bit());
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

/// `mov si,si`, `mov di,di`, `mov bx,bx`: three filler slots, so the CALL FAR is never the entry
/// slot and the OFF arm's block still clears the three-slot minimum.
const FILL: [[u8; 2]; 3] = [[0x89, 0xf6], [0x89, 0xff], [0x89, 0xdb]];

fn filler() -> Vec<u8> {
    FILL.concat()
}

/// `call far selector:offset` (`0x9A imm16 imm16`), or the 0x66-prefixed Dword form
/// (`0x9A imm32 imm16` -- a 4-byte offset followed by a 2-byte selector, six operand bytes in
/// all, NOT four: a `0x66`-prefixed `CALL FAR` widens the offset half only, and the selector
/// stays a 16-bit far pointer's word regardless of operand size).
fn call_far(selector: u16, offset: u16, dword: bool) -> Vec<u8> {
    let mut bytes = if dword { vec![0x66] } else { vec![] };
    bytes.push(0x9a);
    if dword {
        bytes.extend_from_slice(&(offset as u32).to_le_bytes());
    } else {
        bytes.extend_from_slice(&offset.to_le_bytes());
    }
    bytes.extend_from_slice(&selector.to_le_bytes());
    bytes
}

fn compile_on(builder: fn() -> CpuGsw, body: &[u8]) -> Option<jit::direct::Compilation> {
    let (mut cpu, _bus, _starts) = stage(builder, body);
    cpu.set_eip(ENTRY);
    match jit::direct::compile(&mut cpu, ENTRY, false) {
        jit::direct::CompileOutcome::Compiled(compilation) => Some(compilation),
        _ => None,
    }
}

fn stage(builder: fn() -> CpuGsw, body: &[u8]) -> (CpuGsw, TestBus, Vec<u32>) {
    stage_with(builder, body, STACK_ESP, &[])
}

/// Stage code at `ENTRY`; `skip_pages` are physical pages deliberately left OUT of the fast map,
/// for the fault-ordering fixtures.
fn stage_with(
    builder: fn() -> CpuGsw,
    body: &[u8],
    esp: u32,
    skip_pages: &[u32],
) -> (CpuGsw, TestBus, Vec<u32>) {
    let mut code = filler();
    let mut starts = vec![ENTRY];
    for i in 1..=FILL.len() {
        starts.push(ENTRY + 2 * i as u32);
    }
    code.extend_from_slice(body);

    let mut memory = memory_fill();
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    // Something recognisable at the target, so a run that lands there does not decode filler.
    memory[TARGET_LINEAR as usize] = 0xf4;

    let mut cpu = builder();
    cpu.registers.set_esp(esp);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    cpu.set_fast_map_enabled_for_test(true);
    cpu.jit_direct.set_defer_short_for_test(false);
    for &linear in &starts {
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("fixture decode");
    }
    for page in (0..MEMORY_LEN).step_by(0x1000) {
        if skip_pages.contains(&page) {
            continue;
        }
        map_direct_page(&mut cpu, &mut bus, page);
    }
    cpu.set_eip(ENTRY);
    (cpu, bus, starts)
}

fn span_of(builder: fn() -> CpuGsw, body: &[u8]) -> Option<u8> {
    compile_on(builder, body).map(|compilation| compilation.span.instructions)
}

// -------------------------------------------------------------------------------------------
// The differential harness
// -------------------------------------------------------------------------------------------

struct Roles {
    native: CpuGsw,
    native_bus: TestBus,
    interp: CpuGsw,
    interp_bus: TestBus,
    block: jit::direct::CompiledBlock,
}

fn build(builder: fn() -> CpuGsw, body: &[u8], esp: u32) -> Roles {
    build_with_skips(builder, body, esp, &[])
}

fn build_with_skips(builder: fn() -> CpuGsw, body: &[u8], esp: u32, skip_pages: &[u32]) -> Roles {
    let (mut native, mut native_bus, _) = stage_with(builder, body, esp, skip_pages);
    let (mut interp, mut interp_bus, _) = stage_with(builder, body, esp, &[]);

    let compilation = match jit::direct::compile(&mut native, ENTRY, false) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("structurally rejected: the CALL FAR is still a barrier on this arm")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions,
        FILL.len() as u8 + 1,
        "the block must cover the filler AND the CALL FAR, or it never ran natively"
    );
    let key = jit::direct::key_for(&native, ENTRY, false).expect("entry key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = native.jit_direct.block(id).expect("live block");

    for (cpu, _bus) in [
        (&mut native, &mut native_bus),
        (&mut interp, &mut interp_bus),
    ] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr = std::array::from_fn(|i| 0xdead_0000 | (0xa0 + i as u32));
        cpu.registers.set_esp(esp);
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
    }
}

fn compare_state(roles: &Roles, context: &str) {
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
        roles.native.timing_rem, roles.interp.timing_rem,
        "{context}: timing remainder"
    );
    assert_eq!(
        roles.native_bus.trace.elapsed_clocks(),
        roles.interp_bus.trace.elapsed_clocks(),
        "{context}: bus clocks"
    );
    assert_eq!(
        roles.native_bus.memory, roles.interp_bus.memory,
        "{context}: guest RAM, including the two pushed frame words"
    );
}

fn differential(builder: fn() -> CpuGsw, body: &[u8], esp: u32, context: &str) -> Roles {
    let mut roles = build(builder, body, esp);
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
        FILL.len() as u64 + 1,
        "{context}: every slot including the CALL FAR must retire natively"
    );
    for _ in 0..=FILL.len() {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state(&roles, context);
    roles
}

// -------------------------------------------------------------------------------------------
// Admission
// -------------------------------------------------------------------------------------------

/// The three shapes `call_far_admitted_here` must refuse even on the widest arm, each stopping
/// the block exactly where the OFF arm stops it, plus the two admitted rows.
#[test]
fn call_far_admission_matches_retf_admission_term_for_term() {
    select_arm(jit::direct::RetfArm::On);
    let far = call_far(TARGET_SELECTOR, TARGET_OFFSET, false);
    assert_eq!(
        span_of(real_cpu, &far),
        Some(FILL.len() as u8 + 1),
        "the on arm admits plain real mode"
    );
    assert_eq!(
        span_of(v86_cpu, &far),
        Some(FILL.len() as u8 + 1),
        "the on arm admits V86"
    );
    assert_eq!(
        span_of(protected16_cpu, &far),
        Some(FILL.len() as u8),
        "protected mode outside V86 must stay a barrier on every arm"
    );
    assert_eq!(
        span_of(unreal_stack_cpu, &far),
        Some(FILL.len() as u8),
        "SS.B = 1 must stay a barrier: the emitted pointer arithmetic is 16-bit throughout"
    );
    let dword = call_far(TARGET_SELECTOR, TARGET_OFFSET, true);
    assert_eq!(
        span_of(v86_cpu, &dword),
        Some(FILL.len() as u8),
        "a 0x66-prefixed CALL FAR is the 32-bit operand form and has no emitter"
    );
    select_arm(jit::direct::RetfArm::V86);
    assert_eq!(
        span_of(real_cpu, &far),
        Some(FILL.len() as u8),
        "the v86 arm must leave plain real mode stopping at the CALL FAR"
    );
    assert_eq!(
        span_of(v86_cpu, &far),
        Some(FILL.len() as u8 + 1),
        "the v86 arm must admit the CALL FAR in V86"
    );
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// The escape arm reproduces main's span exactly: no `0x9A` classify arm fires without the knob.
#[test]
fn the_escape_arm_stops_before_the_call_far() {
    let escape = jit::direct::parse_direct_retf_v86_arm_for_test(Ok("0".to_string()));
    assert_eq!(escape, jit::direct::RetfArm::Off);
    select_arm(escape);
    let far = call_far(TARGET_SELECTOR, TARGET_OFFSET, false);
    for (name, builder) in [
        ("real mode", real_cpu as fn() -> CpuGsw),
        ("V86", v86_cpu as fn() -> CpuGsw),
    ] {
        assert_eq!(
            span_of(builder, &far),
            Some(FILL.len() as u8),
            "{name}: the OFF arm must stop the block BEFORE the CALL FAR"
        );
        assert_eq!(
            span_of(builder, &[0x89, 0xc8]),
            Some(FILL.len() as u8 + 1),
            "{name}: the control tail must extend the block, or this fixture cannot fail"
        );
    }
    jit::direct::set_direct_retf_v86_for_test(None);
}

// -------------------------------------------------------------------------------------------
// The lowering
// -------------------------------------------------------------------------------------------

/// The whole CS record, written from the IMMEDIATE selector, and the pushed frame, in V86.
///
/// The stack starts at STACK_ESP; nothing is pre-seeded. The far call pushes CS (old selector 0)
/// then the return IP, and CS really changes across the call (selector 0 in, 0x0020 out), so
/// every field of the CS record is observable, and the two pushed words are observable through
/// the whole-RAM comparison `compare_state` performs.
#[test]
fn a_far_call_writes_the_whole_cs_record_and_the_pushed_frame_in_v86() {
    select_arm(jit::direct::RetfArm::V86);
    let far = call_far(TARGET_SELECTOR, TARGET_OFFSET, false);
    let roles = differential(v86_cpu, &far, STACK_ESP, "V86 far call");
    assert_eq!(roles.native.registers.cs().selector, TARGET_SELECTOR);
    assert_eq!(
        roles.native.registers.cs().base,
        u32::from(TARGET_SELECTOR) << 4
    );
    assert_eq!(roles.native.registers.cs().limit, 0xffff);
    assert_eq!(roles.native.registers.cs().access, 0x93);
    assert!(!roles.native.registers.cs().default_size_32);
    assert_eq!(roles.native.registers.eip, u32::from(TARGET_OFFSET));
    // SP moved back by exactly 4, and the poisoned high half survived.
    assert_eq!(roles.native.registers.esp(), STACK_ESP - 4);
    // The pushed frame bytes, spelled out beyond the whole-RAM comparison: CS at the higher
    // address (SP-2, above the return IP), the return IP at the lower one (SP-4).
    let sp = (STACK_SP & 0xffff) as u16;
    let cs_addr = sp.wrapping_sub(2) as usize;
    let ip_addr = sp.wrapping_sub(4) as usize;
    let pushed_cs = u16::from_le_bytes([
        roles.native_bus.memory[cs_addr],
        roles.native_bus.memory[cs_addr + 1],
    ]);
    let pushed_ip = u16::from_le_bytes([
        roles.native_bus.memory[ip_addr],
        roles.native_bus.memory[ip_addr + 1],
    ]);
    assert_eq!(pushed_cs, 0, "the OLD CS selector (0) must be pushed");
    assert_eq!(
        pushed_ip,
        (ENTRY + 2 * FILL.len() as u32 + far.len() as u32) as u16,
        "the return IP, the address right after the 5-byte CALL FAR"
    );
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// Plain real mode on the `on` arm, plus the CPL no-op the interpreter's `far_call` takes.
#[test]
fn a_far_call_in_plain_real_mode_leaves_cpl_at_zero() {
    select_arm(jit::direct::RetfArm::On);
    let far = call_far(TARGET_SELECTOR, TARGET_OFFSET, false);
    let roles = differential(real_cpu, &far, STACK_ESP, "real-mode far call");
    assert_eq!(roles.native.current_privilege_level(), 0);
    assert_eq!(roles.interp.current_privilege_level(), 0);
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// SP = 0x0002: the CLEAN wrap, the push-side mirror of `RetFar16`'s own SP = 0xFFFE fixture.
/// CS is pushed at SS:0x0000 (no wrap: bytes 0x0000-0x0001), and the return IP wraps CLEANLY to
/// SS:0xFFFE (bytes 0xFFFE-0xFFFF) -- a 16-bit BASE wrap with neither 2-byte access itself
/// straddling the 0xFFFF/0x0000 boundary, exactly the shape `RetFar16`'s SP = 0xFFFE fixture
/// exercises for its two pops. SP ends at 0xFFFE with its high half intact.
#[test]
fn sp_wraps_cleanly_across_the_two_far_pushes() {
    select_arm(jit::direct::RetfArm::V86);
    let esp = 0xdead_0000 | 0x0002;
    let far = call_far(TARGET_SELECTOR, TARGET_OFFSET, false);
    let roles = differential(v86_cpu, &far, esp, "far call at SP = 2");
    assert_eq!(
        roles.native.registers.esp(),
        0xdead_0000 | 0xfffe,
        "SP wraps to 0xFFFE and ESP[31:16] survives"
    );
    assert_eq!(roles.native.registers.cs().selector, TARGET_SELECTOR);
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// SP = 0x0000, named explicitly in the task brief. `(SP-2)&0xFFFF = 0xFFFE` and
/// `(SP-4)&0xFFFF = 0xFFFC`: both pushes land entirely inside the top page (0xF000-0xFFFF),
/// so this is a BASE wrap with neither 2-byte access straddling a page, and it retires natively
/// and byte-correct exactly like the SP = 2 clean-wrap row above.
#[test]
fn sp_zero_retires_natively_and_byte_correct() {
    select_arm(jit::direct::RetfArm::V86);
    let esp = 0xdead_0000u32;
    let far = call_far(TARGET_SELECTOR, TARGET_OFFSET, false);
    let roles = differential(v86_cpu, &far, esp, "far call at SP = 0");
    assert_eq!(
        roles.native.registers.esp(),
        0xdead_0000 | 0xfffc,
        "SP wraps to 0xFFFC and ESP[31:16] survives"
    );
    assert_eq!(roles.native.registers.cs().selector, TARGET_SELECTOR);
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// SP = 0x0001, named explicitly in the task brief and the genuinely HARD row: the FIRST push
/// (CS, at `(SP-2)&0xFFFF = 0xFFFF`) is a 2-byte access whose two bytes straddle the
/// 0xFFFF/0x0000 boundary itself -- byte 0 physically in the top fast-map page, byte 1 in the
/// bottom one. **Measured, not assumed**: this DOES side-exit rather than retire (the emitted
/// guard correctly refuses a genuinely cross-page-after-wrap access instead of writing the second
/// byte to the wrong physical location), which is the SAFE outcome -- the interpreter then serves
/// the instruction from where the block stopped, exactly the fallback every other wide native
/// access in this file relies on for an ordinary cross-page word. This is a REFUSAL boundary
/// worth pinning explicitly rather than a defect: `RetFar16`'s own wrap fixture (SP = 0xFFFE) was
/// chosen so neither of ITS two reads ever straddles this way.
#[test]
fn sp_one_straddles_the_wrap_and_side_exits_rather_than_misdirecting_the_store() {
    select_arm(jit::direct::RetfArm::On);
    let esp = 0xdead_0000 | 0x0001u32;
    let far = call_far(TARGET_SELECTOR, TARGET_OFFSET, false);
    let mut roles = build(real_cpu, &far, esp);
    let before = roles.native.perf_counters().jit_direct_insns;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "the block must still run (the filler slots), even though it side-exits before the CALL \
         FAR"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - before,
        FILL.len() as u64,
        "the CALL FAR must NOT retire natively across a straddling wrap"
    );
    assert_eq!(roles.native.registers.esp(), esp, "SP must not have moved");
    assert_eq!(
        roles.native.registers.cs().selector,
        0,
        "CS must still hold its OLD record"
    );
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// The clock charge is a PIN against the interpreter: 17 raw clocks, exactly like RETF.
#[test]
fn native_call_far_charges_exactly_what_the_interpreter_charges() {
    select_arm(jit::direct::RetfArm::V86);
    let far = call_far(TARGET_SELECTOR, TARGET_OFFSET, false);
    let roles = differential(v86_cpu, &far, STACK_ESP, "far call timing");
    assert_eq!(
        roles.native.elapsed_clocks, roles.interp.elapsed_clocks,
        "core clocks"
    );
    assert!(
        roles.native_bus.trace.elapsed_clocks() > 0,
        "the fixture must actually charge bus clocks, or the equality above is vacuous"
    );
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// The far-CALL ledger reaches Rust, once per native far call, and inflates neither the write
/// counters nor the bus clocks on the way -- the identical shape RETF's own F15 fixture checks
/// for `far_ret_native`.
#[test]
fn the_far_call_ledger_reaches_rust_and_inflates_neither_writes_nor_clocks() {
    select_arm(jit::direct::RetfArm::V86);
    let far = call_far(TARGET_SELECTOR, TARGET_OFFSET, false);
    let mut roles = build(v86_cpu, &far, STACK_ESP);
    let ledger_before = roles.native.jit_direct.far_call_native_for_test();
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap()
    );
    assert_eq!(
        roles.native.jit_direct.far_call_native_for_test() - ledger_before,
        1,
        "one far call must reach Rust through the STACK_FAR_CALL_NATIVE lane"
    );
    for _ in 0..=FILL.len() {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    assert_eq!(
        roles.native_bus.trace.elapsed_clocks(),
        roles.interp_bus.trace.elapsed_clocks(),
        "raw bus clocks must match: the ledger deposit must not price as a data access"
    );
    assert_eq!(
        roles.native.elapsed_clocks, roles.interp.elapsed_clocks,
        "elapsed clocks"
    );
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// A far-call block is a SEGMENT-WRITE block: it writes CS, so `successors == [None, None]` and
/// `is_segment_write_block` must say so, exactly like `RetFar16`'s own F19.
#[test]
fn a_far_call_block_counts_as_a_segment_write_block() {
    select_arm(jit::direct::RetfArm::V86);
    let far = call_far(TARGET_SELECTOR, TARGET_OFFSET, false);
    let mut roles = build(v86_cpu, &far, STACK_ESP);
    assert!(
        roles.block.is_segment_write_block(),
        "a block whose terminal writes CS is a segment-write block"
    );
    let before = roles
        .native
        .direct_stall_snapshot()
        .segment_write_block_head_entries;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap()
    );
    assert_eq!(
        roles
            .native
            .direct_stall_snapshot()
            .segment_write_block_head_entries
            - before,
        1
    );
    jit::direct::set_direct_retf_v86_for_test(None);
}

// -------------------------------------------------------------------------------------------
// Fault ordering
// -------------------------------------------------------------------------------------------

/// A fault on the FIRST push (CS, at the higher address SP-2) leaves the whole instruction
/// UN-STARTED: SP unmoved, CS still the OLD record, EIP still at the CALL FAR.
#[test]
fn a_fault_on_the_first_far_push_leaves_the_instruction_unstarted() {
    select_arm(jit::direct::RetfArm::On);
    fn short_ss_cpu() -> CpuGsw {
        let mut cpu = real_cpu();
        let mut ss = cpu.registers.segment(SegmentIndex::Ss);
        // CS pushes at SP-2 = 0x6FE..0x6FF; a limit of 0x6FD makes even that push overrun.
        ss.limit = 0x06fd;
        cpu.registers.set_segment(SegmentIndex::Ss, ss);
        cpu
    }
    let far = call_far(TARGET_SELECTOR, TARGET_OFFSET, false);
    let mut roles = build(short_ss_cpu, &far, STACK_ESP);
    let before = roles.native.perf_counters().jit_direct_insns;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap()
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - before,
        FILL.len() as u64,
        "the CALL FAR must NOT retire: its first stack write leaves the SS limit"
    );
    assert_eq!(
        roles.native.registers.esp(),
        STACK_ESP,
        "SP must not have moved"
    );
    assert_eq!(
        roles.native.registers.cs().selector,
        0,
        "CS must still hold its OLD record"
    );
    assert_eq!(
        roles.native.registers.eip,
        ENTRY + 2 * FILL.len() as u32,
        "and EIP must still point at the CALL FAR, so the interpreter re-runs it"
    );
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// A fault on the SECOND push (the return IP, at the lower address SP-4) ALSO leaves the whole
/// instruction un-started, even though the first push's word (CS) may already be physically in
/// memory: SP is reverted, so nothing architectural ever reads it back. Forced with an unmapped
/// page rather than a segment limit, because the two pushes' addresses DECREASE (unlike RETF's
/// two pops, which increase), so an upper-bound SS limit can never separate them -- if the
/// higher address (CS) is in bounds, the lower one (the IP) necessarily is too.
#[test]
fn a_fault_on_the_second_far_push_leaves_the_instruction_unstarted() {
    select_arm(jit::direct::RetfArm::On);
    // ESP chosen so CS's push (SP-2) lands in the LAST two bytes of page 0x1000 (mapped) and the
    // return IP's push (SP-4) lands in page 0x0000 (deliberately left OUT of the fast map).
    let esp = 0x1002u32;
    let cs_addr = (esp - 2) as usize;
    let ip_addr = (esp - 4) as usize;
    assert!(
        (0x1000..0x1000 + 0x1000).contains(&cs_addr),
        "sanity: CS push in page 0x1000"
    );
    assert!(ip_addr < 0x1000, "sanity: IP push in page 0x0000");
    let far = call_far(TARGET_SELECTOR, TARGET_OFFSET, false);
    let mut roles = build_with_skips(real_cpu, &far, esp, &[0x0000]);
    let before = roles.native.perf_counters().jit_direct_insns;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap()
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - before,
        FILL.len() as u64,
        "the CALL FAR must NOT retire: its second stack write's page is unmapped"
    );
    assert_eq!(
        roles.native.registers.esp(),
        esp,
        "SP must not have moved, even though CS's word may already be physically in memory"
    );
    assert_eq!(
        roles.native.registers.cs().selector,
        0,
        "CS register must still hold its OLD record"
    );
    assert_eq!(
        roles.native.registers.eip,
        ENTRY + 2 * FILL.len() as u32,
        "and EIP must still point at the CALL FAR"
    );
    jit::direct::set_direct_retf_v86_for_test(None);
}
