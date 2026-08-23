// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Native RETF (`0xCA` / `0xCB`) in real mode and V86, behind `IZARRAVM_DIRECT_RETF_V86`.
//!
//! Design: `dev_docs/specs/2026-08-24-v86-retf-pic-design.md` (rev 3), reviewed to APPROVE in
//! `dev_docs/specs/2026-08-24-v86-retf-pic-review.md` round 3.
//!
//! The measured problem, on wolf3d-586 with TOKAEMM resident (so V86):
//!
//! | counter | value |
//! |---|---:|
//! | `straight_line_runs` | 280,565,241 |
//! | of which exist solely because of a RETF | 274,340,000 (97.8%) |
//! | interpreted instructions | 341,308,466 |
//! | of which are that RETF | 274,340,000 (80.4%) |
//! | `0xCB` share of block-stopping non-continuable breaks | 99.6989% |
//!
//! So one opcode is essentially the whole of that row's interpreted residue, and every one of its
//! executions also ends a `run_budgeted_inner` call.
//!
//! **Every fixture here states its arm through `set_direct_retf_v86_for_test`, in both
//! directions**, and nothing reads the ambient knob except the default pin itself. The knob ships
//! OFF, so a positive fixture that leaned on the ambient reading would be testing the refusal and
//! calling it a lowering -- and the suite is run on BOTH arms, so a fixture that hard-asserted
//! "off" would make the armed leg red by construction.
//!
//! The differential rows run the same guest bytes natively and through a BLOCK-FREE interpreter
//! from identical state and compare registers (segment registers and EIP included), the raw lazy
//! flags descriptor, materialized EFLAGS, the halt latch, core clocks, bus clocks and the WHOLE of
//! guest RAM.
//!
//! **The RETF is always the block's LAST slot and never its first.** It is a terminal, so it has
//! to be last; a one-slot block would be an entry-position fixture, which cannot tell a lowering
//! from a side exit that retired nothing, so every block here carries leading filler.
//!
//! **CS actually changes across every far return here.** The entry CS is selector 0 and the
//! returned-to CS is selector 0x0020, so a lowering that dropped the selector store, the base
//! shift or the whole CS record is observable in `registers`. With the usual "return to the same
//! segment" shape it would not be.

// MUTATION EVIDENCE. Each row names the fixture that caught it; a mutation nobody catches is a
// fixture bug, not a free pass. The full ledger, including the mutants that live in other files,
// is `dev_docs/MUTANTS-2026-08-24-v86-retf-pic.md`.

use super::*;

/// EIP of the block entry. The entry CS base is 0, so this is also its linear address.
const ENTRY: u32 = 0x100;
/// Where the far return goes: selector 0x0020 (base 0x200) at offset 0x0400, linear 0x600.
const TARGET_SELECTOR: u16 = 0x0020;
const TARGET_OFFSET: u16 = 0x0400;
const TARGET_LINEAR: u32 = 0x0600;
/// SS is base 0 with SP here, clear of the code and of the target.
const STACK_SP: u32 = 0x0700;
/// The same stack pointer with a POISONED high half. SS.B is 0 in every fixture here, so bits
/// 31..16 of ESP are architecturally untouched by a pop: `alu_r16_imm16` preserves them where an
/// `add r32` clears them, and with a ZERO high half the two are indistinguishable.
const STACK_ESP: u32 = 0xdead_0000 | STACK_SP;

/// A distinct byte at every address, so a read of the wrong WIDTH or through the wrong SEGMENT
/// differs from the interpreter even when the intended bytes happen to match.
fn memory_fill() -> Vec<u8> {
    let mut memory = vec![0u8; 0x1_0000];
    for (i, byte) in memory.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37).wrapping_add(11) & 0xff) as u8;
    }
    memory
}

/// Select the arm for this thread and PROVE the selection took.
fn select_retf(arm: jit::direct::RetfArm) {
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

/// The same machine in V86, which is what wolf3d runs in once TOKAEMM is resident and therefore
/// the `v86` arm's whole population.
fn v86_cpu() -> CpuGsw {
    let mut cpu = real_cpu();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x202 | FLAG_VM | (3 << 12);
    // The CACHED CPL, which `current_privilege_level` debug-asserts is 3 in V86.
    cpu.cpl = 3;
    assert!(cpu.is_v86_mode(), "the V86 fixture must actually be in V86");
    cpu
}

/// PROTECTED 16-bit: PE set, not V86, CS.D = 0 and SS.B = 0.
///
/// Deliberately 16-bit rather than the usual flat fixture. A flat protected CPU would refuse the
/// RETF for the operand size as well as for the mode, and the refusal fixture could not say which
/// term did it.
fn protected16_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE;
    // 0x9b: present, code, EXECUTE/READ, and `default_size_32` false so the operand size is Word.
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

/// Real mode with an UNREAL stack: SS's cached `default_size_32` is true, so `stack_is_32bit` is
/// true while the CPU is still in real mode. The one shape that isolates the SS.B term of
/// `retf_admitted_here`.
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

/// `mov si,si`, `mov di,di`, `mov bx,bx`: three filler slots, so the RETF is never the entry slot
/// and the OFF arm's block still clears the three-slot minimum.
const FILL: [[u8; 2]; 3] = [[0x89, 0xf6], [0x89, 0xff], [0x89, 0xdb]];

fn filler() -> Vec<u8> {
    FILL.concat()
}

/// `retf` (0xCB) or `retf imm16` (0xCA).
fn retf(release: Option<u16>) -> Vec<u8> {
    match release {
        None => vec![0xcb],
        Some(release) => [vec![0xca], release.to_le_bytes().to_vec()].concat(),
    }
}

/// Build `FILL * 3 / body`, warm every decode line, map every page, and compile at `ENTRY`.
///
/// Returns the compilation, or `None` when the walk refused to carry the body.
fn compile_on(builder: fn() -> CpuGsw, body: &[u8]) -> Option<jit::direct::Compilation> {
    let (mut cpu, _bus, _starts) = stage(builder, body);
    cpu.set_eip(ENTRY);
    match jit::direct::compile(&mut cpu, ENTRY, false) {
        jit::direct::CompileOutcome::Compiled(compilation) => Some(compilation),
        _ => None,
    }
}

/// The shared staging: code at `ENTRY`, the far return address on the stack, every decode line
/// warmed and every page fast-mapped.
fn stage(builder: fn() -> CpuGsw, body: &[u8]) -> (CpuGsw, TestBus, Vec<u32>) {
    stage_with(builder, body, TARGET_SELECTOR, TARGET_OFFSET, STACK_ESP)
}

fn stage_with(
    builder: fn() -> CpuGsw,
    body: &[u8],
    selector: u16,
    offset: u16,
    esp: u32,
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
    // The far return address: offset first, selector above it, exactly as a `push cs; push ip`
    // pair or a far CALL leaves them.
    // Written byte by byte with a 16-bit WRAP, because the SP = 0xFFFE row puts the selector at
    // SS:0x0000: the guest's two pops wrap and the fixture's seeding has to wrap with them.
    let sp = (esp & 0xffff) as u16;
    for (i, byte) in offset
        .to_le_bytes()
        .into_iter()
        .chain(selector.to_le_bytes())
        .enumerate()
    {
        memory[usize::from(sp.wrapping_add(i as u16))] = byte;
    }
    // Something recognisable at the target, so a run that lands there does not decode filler.
    memory[TARGET_LINEAR as usize] = 0xf4;

    let mut cpu = builder();
    cpu.registers.set_esp(esp);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    cpu.set_fast_map_enabled_for_test(true);
    // `defer_short_enabled` is hard-false outside tests but defaults ON inside them, and a far
    // block flips both of its consumers once its cell binds. Every fixture here runs with it off.
    cpu.jit_direct.set_defer_short_for_test(false);
    for &linear in &starts {
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("fixture decode");
    }
    for page in (0..0x1_0000u32).step_by(0x1000) {
        map_direct_page(&mut cpu, &mut bus, page);
    }
    cpu.set_eip(ENTRY);
    (cpu, bus, starts)
}

/// How many slots the walk covered at `ENTRY`.
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

/// Stage the same guest bytes on two identical machines, compile and install the block on one of
/// them, and hand both back seeded and clock-zeroed.
fn build(builder: fn() -> CpuGsw, body: &[u8], selector: u16, offset: u16, esp: u32) -> Roles {
    let (mut native, mut native_bus, _) = stage_with(builder, body, selector, offset, esp);
    let (mut interp, mut interp_bus, _) = stage_with(builder, body, selector, offset, esp);

    let compilation = match jit::direct::compile(&mut native, ENTRY, false) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("structurally rejected: the RETF is still a barrier on this arm")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions,
        FILL.len() as u8 + 1,
        "the block must cover the filler AND the RETF, or the far return never ran natively"
    );
    // `probe` first, as every install fixture in the tree does: it is what moves the key to
    // `Seen`, which `install` requires.
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
        // 0xdead in every high half, so a lowering that writes 32 bits where the operand size says
        // 16 is a distinguishable failure.
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
        roles.native.registers, roles.interp.registers,
        "{context}: registers (segment registers and EIP included)"
    );
    assert_eq!(
        roles.native.pending_flags, roles.interp.pending_flags,
        "{context}: raw lazy-flags descriptor"
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
    // The WHOLE array. A read of the wrong width cannot change RAM, but the far return's SP
    // arithmetic can, through a later fixture's pushes, and a window sized to the intended access
    // is exactly the wrong shape to see that.
    assert_eq!(
        roles.native_bus.memory, roles.interp_bus.memory,
        "{context}: guest RAM"
    );
}

/// Run the whole block natively and the same four instructions through the interpreter, then
/// compare everything.
fn differential(
    builder: fn() -> CpuGsw,
    body: &[u8],
    selector: u16,
    offset: u16,
    esp: u32,
    context: &str,
) -> Roles {
    let mut roles = build(builder, body, selector, offset, esp);
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
        "{context}: every slot including the RETF must retire natively"
    );
    for _ in 0..=FILL.len() {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state(&roles, context);
    roles
}

// -------------------------------------------------------------------------------------------
// The knob
// -------------------------------------------------------------------------------------------

/// THE DEFAULT PIN. Reads the AMBIENT knob deliberately, so the suite is green on both arms: it
/// asserts the spelling table applied to the real environment, which with the variable unset
/// reduces to "the default is OFF".
#[test]
fn direct_retf_v86_ships_off_by_default() {
    jit::direct::set_direct_retf_v86_for_test(None);
    let ambient = std::env::var("IZARRAVM_DIRECT_RETF_V86");
    let expected = jit::direct::parse_direct_retf_v86_arm_for_test(ambient.clone());
    assert_eq!(
        jit::direct::direct_retf_v86(),
        expected,
        "the process-wide reading must agree with the spelling table applied to \
         IZARRAVM_DIRECT_RETF_V86={ambient:?}"
    );
    if ambient.is_err() {
        assert_eq!(
            expected,
            jit::direct::RetfArm::Off,
            "IZARRAVM_DIRECT_RETF_V86 must default OFF"
        );
    }
}

/// F13. All four rows of the spelling table INCLUDING the panic, and `v86` refusing plain real
/// mode.
///
/// Catches: a `_ => Off` fallthrough replacing the panic. A mistyped ladder leg
/// (`IZARRAVM_DIRECT_RETF_V86=yes`) that fell through would run exactly what an unset environment
/// runs and be read as "the arm I asked for changed nothing", which is the single wrong conclusion
/// an arm ladder exists to avoid. And the EMPTY string is a spelling of OFF while unset is also
/// OFF -- the two agree here, but they must agree by the table rather than by accident, because
/// nulling an environment variable in PowerShell leaves it PRESENT and EMPTY.
#[test]
fn direct_retf_v86_knob_spellings() {
    use jit::direct::RetfArm;
    use std::env::VarError;
    assert_eq!(
        jit::direct::parse_direct_retf_v86_arm_for_test(Err(VarError::NotPresent)),
        RetfArm::Off
    );
    for off in ["", "0", "off", "OFF", " off ", "Off"] {
        assert_eq!(
            jit::direct::parse_direct_retf_v86_arm_for_test(Ok(off.to_string())),
            RetfArm::Off,
            "{off:?} must name the off arm"
        );
    }
    for v86 in ["v86", "V86", " v86 "] {
        assert_eq!(
            jit::direct::parse_direct_retf_v86_arm_for_test(Ok(v86.to_string())),
            RetfArm::V86,
            "{v86:?} must name the v86 arm"
        );
    }
    for on in ["1", "on", "ON", " On "] {
        assert_eq!(
            jit::direct::parse_direct_retf_v86_arm_for_test(Ok(on.to_string())),
            RetfArm::On,
            "{on:?} must name the on arm"
        );
    }
    for typo in ["yes", "true", "2", "real", "vm86"] {
        let panicked = std::panic::catch_unwind(|| {
            jit::direct::parse_direct_retf_v86_arm_for_test(Ok(typo.to_string()))
        })
        .is_err();
        assert!(
            panicked,
            "IZARRAVM_DIRECT_RETF_V86={typo:?} names no arm and must panic rather than silently \
             running the default"
        );
    }

    // The `v86` arm's blast-radius property, which is the whole reason it exists: it must leave a
    // plain real-mode boot alone. Asserted through the compile walk, not through `admits` alone,
    // because that is where a leg's byte-identity claim actually lives.
    select_retf(RetfArm::V86);
    assert_eq!(
        span_of(real_cpu, &retf(None)),
        Some(FILL.len() as u8),
        "the v86 arm must leave plain real mode stopping at the RETF"
    );
    assert_eq!(
        span_of(v86_cpu, &retf(None)),
        Some(FILL.len() as u8 + 1),
        "the v86 arm must admit the RETF in V86, or the row above is vacuous"
    );
    select_retf(RetfArm::On);
    assert_eq!(
        span_of(real_cpu, &retf(None)),
        Some(FILL.len() as u8 + 1),
        "the on arm must admit plain real mode"
    );
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// F14. The OFF arm reproduces main's span and break reason exactly.
///
/// Catches: an admission that forgot its arm test, i.e. a RETF lowered while the knob says off --
/// which would make the OFF leg disagree with `main` and destroy the A/B base for the whole
/// ladder.
///
/// The control row is a plain `mov ax,cx` tail, which must extend the block on BOTH arms: it
/// proves the harness is not simply refusing everything, which is how this test would go vacuous.
#[test]
fn off_arm_stops_before_the_retf() {
    select_retf(jit::direct::RetfArm::Off);
    for (name, builder) in [
        ("real mode", real_cpu as fn() -> CpuGsw),
        ("V86", v86_cpu as fn() -> CpuGsw),
    ] {
        assert_eq!(
            span_of(builder, &retf(None)),
            Some(FILL.len() as u8),
            "{name}: the OFF arm must stop the block BEFORE the RETF"
        );
        assert_eq!(
            span_of(builder, &retf(Some(4))),
            Some(FILL.len() as u8),
            "{name}: and before RETF imm16 too"
        );
        assert_eq!(
            span_of(builder, &[0x89, 0xc8]),
            Some(FILL.len() as u8 + 1),
            "{name}: the control tail must extend the block, or this fixture cannot fail"
        );
    }
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// F11. The three shapes `retf_admitted_here` must refuse even on the WIDEST arm, each stopping
/// the block exactly where the OFF arm stops it.
///
/// Every term is isolated. Protected mode is tested on a 16-BIT protected CPU, so the operand size
/// and the stack width are both admissible and only `arm.admits` can be what refused it; the
/// 32-bit stack is tested in plain REAL mode with an unreal SS, so only the `stack_is_32bit` term
/// can be what refused it; the 0x66 prefix is tested on the V86 CPU that admits the unprefixed
/// form two lines above.
///
/// Catches: `arm.admits` written as `is_v86_mode() || !is_protected_mode()` on the `v86` arm
/// (M17); the operand-size or stack-width term dropped from `retf_admitted_here` and left to
/// `stack_width_kind`, which would refuse the KIND instead and end the block on a different reason.
#[test]
fn retf_is_refused_in_protected_mode_and_on_a_32_bit_stack() {
    select_retf(jit::direct::RetfArm::On);
    assert_eq!(
        span_of(v86_cpu, &retf(None)),
        Some(FILL.len() as u8 + 1),
        "the control: the widest arm admits an unprefixed 16-bit RETF in V86"
    );
    assert_eq!(
        span_of(protected16_cpu, &retf(None)),
        Some(FILL.len() as u8),
        "protected mode outside V86 must stay a barrier on every arm: the CS record is not a pure \
         function of the popped selector there"
    );
    assert_eq!(
        span_of(unreal_stack_cpu, &retf(None)),
        Some(FILL.len() as u8),
        "SS.B = 1 must stay a barrier: the emitted pointer arithmetic is 16-bit throughout"
    );
    assert_eq!(
        span_of(v86_cpu, &[0x66, 0xcb]),
        Some(FILL.len() as u8),
        "a 0x66-prefixed RETF is the 32-bit operand form and has no emitter"
    );
    jit::direct::set_direct_retf_v86_for_test(None);
}

// -------------------------------------------------------------------------------------------
// The lowering
// -------------------------------------------------------------------------------------------

/// F1. The whole CS record, written from the popped selector, in V86.
///
/// The stack is seeded exactly as `push cs; call near f` leaves it -- offset below selector -- and
/// CS really changes across the return (selector 0 in, selector 0x0020 out), so every field of the
/// record is observable in `registers`.
///
/// Catches, one mutation each: the CS SELECTOR store dropped (M1); the CS BASE store dropped or
/// the `shl 4` changed (M2); the access / `default_size_32` store dropped (M3, through full
/// `Registers` equality); the two pops swapped (M4); `release.wrapping_add(2)` for `+ 4` (M5); the
/// ledger increment moved above the CS record, where `mov RDX, 1 << 32` destroys the popped
/// selector still live in RDX (M23).
#[test]
fn a_far_return_writes_the_whole_cs_record_and_matches_the_interpreter_in_v86() {
    select_retf(jit::direct::RetfArm::V86);
    let roles = differential(
        v86_cpu,
        &retf(None),
        TARGET_SELECTOR,
        TARGET_OFFSET,
        STACK_ESP,
        "V86 far return",
    );
    // Spelled out beyond the differential, because "both machines are wrong the same way" is the
    // one failure a differential cannot see, and these four values are computable by hand.
    assert_eq!(roles.native.registers.cs().selector, TARGET_SELECTOR);
    assert_eq!(
        roles.native.registers.cs().base,
        u32::from(TARGET_SELECTOR) << 4
    );
    assert_eq!(roles.native.registers.cs().limit, 0xffff);
    assert_eq!(roles.native.registers.cs().access, 0x93);
    assert!(!roles.native.registers.cs().default_size_32);
    assert_eq!(roles.native.registers.eip, u32::from(TARGET_OFFSET));
    // SP advanced by exactly 4, and the poisoned high half survived.
    assert_eq!(roles.native.registers.esp(), STACK_ESP + 4);
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// F2. The same thing in PLAIN REAL MODE on the `on` arm, plus the CPL no-op the design takes on
/// trust everywhere else.
///
/// `return_far_body` writes `self.cpl = 0` in real mode. That is claimed to be a no-op because
/// every PE-clearing transfer sets `cpl = 0` and no real-mode path raises it -- true in this tree,
/// and exactly the kind of invariant this campaign has been burned by, so it is asserted rather
/// than assumed.
#[test]
fn a_far_return_in_plain_real_mode_leaves_cpl_at_zero() {
    select_retf(jit::direct::RetfArm::On);
    let roles = differential(
        real_cpu,
        &retf(None),
        TARGET_SELECTOR,
        TARGET_OFFSET,
        STACK_ESP,
        "real-mode far return",
    );
    assert_eq!(
        roles.native.current_privilege_level(),
        0,
        "a real-mode far return must leave CPL 0; the native path writes no CPL at all, so this \
         is the assertion that says the interpreter's `self.cpl = 0` really is a no-op"
    );
    assert_eq!(
        roles.interp.current_privilege_level(),
        0,
        "and the interpreted twin agrees, so the row above is not testing the native path alone"
    );
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// F4. `0xCA` RETF imm16: the release is added AFTER both pops, and one 16-bit add of
/// `4 + release` is congruent with the interpreter's `pop`, `pop`, `release_stack` sequence.
///
/// Catches: `release.wrapping_add(2)` (M5), and a release applied BEFORE the second pop, which
/// would read the selector from the wrong address.
#[test]
fn retf_imm16_releases_its_parameters_after_the_pops() {
    select_retf(jit::direct::RetfArm::V86);
    for release in [0u16, 2, 6, 0x0100, 0xfffe] {
        let roles = differential(
            v86_cpu,
            &retf(Some(release)),
            TARGET_SELECTOR,
            TARGET_OFFSET,
            STACK_ESP,
            &format!("retf {release:#x}"),
        );
        let expected_sp = (STACK_SP as u16).wrapping_add(release.wrapping_add(4));
        assert_eq!(
            roles.native.registers.esp() & 0xffff,
            u32::from(expected_sp),
            "retf {release:#x}: SP must advance by 4 + release, 16-bit and wrapping"
        );
        assert_eq!(
            roles.native.registers.esp() >> 16,
            STACK_ESP >> 16,
            "retf {release:#x}: ESP[31:16] is architecturally untouched by a 16-bit stack"
        );
    }
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// F5. SP = 0xFFFE: the offset is read at SS:0xFFFE and the SELECTOR AT SS:0x0000, across the
/// 16-bit wrap, and SP ends at 0x0002 with its high half intact.
///
/// This is the row that forces two word reads rather than one dword read. A single 32-bit read at
/// SS:0xFFFE would take four bytes from 0xFFFE..0x10001, which is neither what the guest sees nor
/// what the bus is charged for.
///
/// Catches: a dword read for the pair; a 32-bit `add` on SP (M6), which at this SP carries out of
/// bit 15 and is therefore visible here and nowhere else in this file.
#[test]
fn sp_wraps_across_the_two_far_pops() {
    select_retf(jit::direct::RetfArm::V86);
    let esp = 0xdead_0000 | 0xfffe;
    let roles = differential(
        v86_cpu,
        &retf(None),
        TARGET_SELECTOR,
        TARGET_OFFSET,
        esp,
        "far return at SP = 0xFFFE",
    );
    assert_eq!(
        roles.native.registers.esp(),
        0xdead_0000 | 0x0002,
        "SP wraps to 0x0002 and ESP[31:16] survives"
    );
    assert_eq!(roles.native.registers.cs().selector, TARGET_SELECTOR);
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// F9. The clock and bus-transaction charges are PINS against the interpreter, not
/// approximations: 17 raw clocks and TWO word reads.
///
/// A guest instruction charged differently natively moves `elapsed_clocks` and `raw_bus_clocks`,
/// and a cycle-budgeted fixture then stops at a different point in its demo. `compare_state`
/// already compares all three, so this fixture's job is to say WHY, and to fail by name.
///
/// Catches: `raw_clocks() => 10` (Ret's value) or `=> 2` (the default), and `word_reads() => 1`
/// (M15). It also catches the missing `run.rs` mask at the Dword bus-clock reader (M20), because
/// an unmasked lane adds `2^32 * jit_data_cost_clocks(Dword)` to the bus total per far return.
#[test]
fn native_retf_charges_exactly_what_the_interpreter_charges() {
    select_retf(jit::direct::RetfArm::V86);
    let roles = differential(
        v86_cpu,
        &retf(None),
        TARGET_SELECTOR,
        TARGET_OFFSET,
        STACK_ESP,
        "far return timing",
    );
    // Named rather than left to `compare_state`: the three filler slots are 2 clocks each, so the
    // RETF's own charge is the remainder and a wrong arm shows up as an off-by-a-constant here.
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

/// F15. The far-return LEDGER reaches Rust, and it inflates neither the write counters nor the
/// bus clocks on the way.
///
/// The count rides the free HIGH half of `STACK_RAM_DWORD_WRITES`, a lane `run.rs` reads at TWO
/// sites. Leaving the low half unmasked at the `writes` sum is the QUIET failure --
/// `jit_native_store_hits`, `data_direct_writes` and `direct_data_pointer_writes` inflate by 2^32
/// per far return, plausibly, and read as the slice working. Leaving it unmasked at the Dword
/// bus-clock charge is the LOUD, guest-visible one, and it would be misread as the timing change
/// this slice admits to.
///
/// Catches: M20 at either reader, and a ledger that never reaches Rust at all (H1).
#[test]
fn the_far_return_ledger_reaches_rust_and_inflates_neither_writes_nor_clocks() {
    select_retf(jit::direct::RetfArm::V86);
    let mut roles = build(
        v86_cpu,
        &retf(None),
        TARGET_SELECTOR,
        TARGET_OFFSET,
        STACK_ESP,
    );
    let perf = roles.native.perf_counters();
    let before = (
        perf.jit_native_store_hits,
        perf.data_direct_writes,
        perf.direct_data_pointer_writes,
    );
    let ledger_before = roles.native.jit_direct.far_ret_native_for_test();
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap()
    );
    let perf = roles.native.perf_counters();
    let after = (
        perf.jit_native_store_hits,
        perf.data_direct_writes,
        perf.direct_data_pointer_writes,
    );
    assert_eq!(
        roles.native.jit_direct.far_ret_native_for_test() - ledger_before,
        1,
        "one far return must reach Rust through the lane"
    );
    for (name, before, after) in [
        ("jit_native_store_hits", before.0, after.0),
        ("data_direct_writes", before.1, after.1),
        ("direct_data_pointer_writes", before.2, after.2),
    ] {
        assert_eq!(
            after - before,
            0,
            "{name} must not move: a far return writes no guest memory, and an unmasked lane \
             would inflate this by 2^32"
        );
    }
    // The loud half. The interpreted twin charges the same two word reads and nothing else, so an
    // unmasked lane at the Dword bus-clock reader shows up here as a 2^32-scaled excess.
    for _ in 0..=FILL.len() {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    assert_eq!(
        roles.native_bus.trace.elapsed_clocks(),
        roles.interp_bus.trace.elapsed_clocks(),
        "raw bus clocks: the Dword bus-clock reader must mask the lane's low half"
    );
    assert_eq!(
        roles.native.elapsed_clocks, roles.interp.elapsed_clocks,
        "elapsed clocks"
    );
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// F19. A far block is a SEGMENT-WRITE block, and the derived predicate says so.
///
/// `is_segment_write_block` is DERIVED as `successors == [None, None] && !dynamic_successor`, and
/// the derivation's exactness proof rests on those two arms being mutually exclusive. A far block
/// breaks that: it reaches `[None, None]` through the segment-write arm while `dynamic_successor`
/// is true. Left unfixed the predicate would silently under-count the segment-write population by
/// 274 M on wolf3d.
///
/// Read through the `DirectStallTally` lane `note_segment_write_block_entry` feeds, which is the
/// one production consumer, at one far-block entry.
///
/// Catches: M13, the unchanged derivation.
#[test]
fn a_far_block_counts_as_a_segment_write_block() {
    select_retf(jit::direct::RetfArm::V86);
    let mut roles = build(
        v86_cpu,
        &retf(None),
        TARGET_SELECTOR,
        TARGET_OFFSET,
        STACK_ESP,
    );
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
        1,
        "the one production consumer of the derived predicate must count this entry"
    );
    jit::direct::set_direct_retf_v86_for_test(None);
}

/// F16. A far block registers NO static-fallthrough refusal-census row.
///
/// `emitted_static_targets` is built from `terminal_links` and is NOT masked by
/// `segment_write_block`, so a `RetFar16` riding the catch-all would register a phantom static
/// fallthrough on SLOT 0 -- which is the FAR cell. The campaign ranks and closes on censuses.
///
/// **The census is ARMED, and that is not optional.** `register_direct_link_refusal_cells`
/// computes the id as `self.direct_link_refusal_census.as_mut().map_or(0, ..)`, so with the census
/// object `None` the id is 0 for EVERY block whatever `emitted_static_targets` holds, and this
/// fixture would pass under M19. Building with the feature is not arming it:
/// `direct_link_refusal_census_active` is separate runtime state.
///
/// Catches: M19, the `RetFar16` arm deleted from `terminal_links`.
#[cfg(feature = "direct-link-refusal-census")]
#[test]
fn a_far_block_registers_no_static_fallthrough_census_row() {
    select_retf(jit::direct::RetfArm::V86);
    let (mut cpu, _bus, _) = stage(v86_cpu, &retf(None));
    cpu.enable_direct_link_refusal_census(true);
    assert!(
        cpu.jit_direct.direct_link_refusal_census_active(),
        "the census must be ARMED, not merely compiled in, or this fixture cannot fail"
    );
    let compilation = match jit::direct::compile(&mut cpu, ENTRY, false) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        _ => panic!("the far block must compile on the v86 arm"),
    };
    assert_eq!(
        compilation.emitted_static_targets,
        [None, None],
        "a far block emits no static edge on either slot; slot 0 is the FAR cell and a phantom \
         fallthrough there is a census row that names an edge the block cannot take"
    );
    assert_eq!(
        compilation.successors,
        [None, None],
        "and no successor either"
    );
    jit::direct::set_direct_retf_v86_for_test(None);
}
