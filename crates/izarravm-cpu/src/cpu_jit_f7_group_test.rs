// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The rejected-row campaign's Slice 2: the F7 register-dword group -- `/5` IMUL, `/6` DIV and
//! `/7` IDIV.
//!
//! Every row runs the same guest bytes natively and through a BLOCK-FREE interpreter from
//! identical state and compares registers (EIP included), lazy flags, EFLAGS, core clocks, bus
//! clocks and guest RAM. The tested opcode is MID-BLOCK on every row: an opcode at a block's
//! entry slot parks the block on the interpreter, so an entry-position fixture certifies nothing.
//!
//! Two row shapes, because DIV and IDIV are the first lowered instructions that can FAULT:
//!
//! * `lowered` -- the divide completes natively. All three slots retire in the block and the
//!   divide guard must NOT have fired.
//! * `guarded` -- the emitted guard refuses. The native run must end AT the divide with the
//!   instruction un-started, `side_exit_divide_guard` must count exactly one, and the state at
//!   that boundary must equal the interpreter's after the one slot before it. Both roles then
//!   step the divide interpreted and must agree on the outcome, fault included.
//!
//! **The guard is what keeps a guest #DE out of the host.** A host `div` with a zero divisor
//! raises an exception on the JIT stack -- inside the emulator process, not in the guest. The
//! mutation that proves it is deleting either guard from `emit_div_reg`: the `guarded` rows below
//! then do not fail an assertion, they ABORT the test process. That is the whole point of the
//! `divide_by_zero_*` and `quotient_overflow_*` rows, and it is why they exercise a real host
//! divide rather than a classifier decision.
//!
//! Mutation record for this file (all five verified by hand and restored):
//! * `jae` -> `ja` in the unsigned guard: `div_by_zero_...` does not fail, it EXITS
//!   `0xc0000094 STATUS_INTEGER_DIVIDE_BY_ZERO`. `edx == ecx == 0` stops being refused and the
//!   host divide raises. This is the strongest form the evidence can take.
//! * dropping guard 2 (`divisor == -1`) from the signed arm:
//!   `idiv_min_dividend_over_minus_one_...` exits `0xc0000095 STATUS_INTEGER_OVERFLOW` on the
//!   host's own `i64::MIN / -1`.
//! * dropping guard 3 (the `i32` range compare): `idiv_quotient_overflow_...` fails on the
//!   retirement count at `quotient i32::MAX + 1` -- all three slots retire where one should,
//!   i.e. the divide completed natively at a point the interpreter faults.
//! * `emit_clear_pending` added to `emit_div_reg`: every `p=true` row fails on lazy flags
//!   (`PendingFlags { tag: 0, .. }` against the seeded descriptor), because `CpuGsw::div` leaves
//!   the descriptor alone.
//! * `imul_r32` -> `mul_r32` in `emit_imul_reg_acc`: both IMUL rows fail on registers, first at
//!   `eax=0xffffffff src=0xffffffff` -- (-1) * (-1), where the unsigned product's high half is
//!   0xfffffffe and the signed one is 0.

use super::*;

/// `mov esi,esi`, the leading slot that keeps the tested opcode off the block entry.
const MOV_ESI_ESI: [u8; 2] = [0x89, 0xf6];
/// `mov edi,edi`, the trailing slot, so the tested opcode is never the last one either.
const MOV_EDI_EDI: [u8; 2] = [0x89, 0xff];

/// Group-3 register-form encoding: `F7 /reg rm`, ModRM mod = 0b11.
fn f7(reg: u8, rm: u8) -> [u8; 2] {
    [0xf7, 0b1100_0000 | (reg << 3) | rm]
}

fn imul(rm: u8) -> [u8; 2] {
    f7(5, rm)
}

fn div(rm: u8) -> [u8; 2] {
    f7(6, rm)
}

fn idiv(rm: u8) -> [u8; 2] {
    f7(7, rm)
}

/// The seeded architectural state a row starts both roles from. `edx:eax` is the implicit
/// dividend / multiplicand pair; `divisor_reg` is the ModRM r/m register and `divisor` the value
/// put in it (ignored when `divisor_reg` is 0 or 2, which alias the accumulator pair -- those
/// rows are exactly the aliasing cover).
#[derive(Clone, Copy)]
struct Seed {
    eax: u32,
    edx: u32,
    divisor_reg: u8,
    divisor: u32,
    eflags: u32,
    live_pending: bool,
}

impl Seed {
    fn new(eax: u32, edx: u32, divisor: u32) -> Self {
        Self {
            eax,
            edx,
            divisor_reg: 3,
            divisor,
            eflags: 0x202,
            live_pending: false,
        }
    }

    fn reg(mut self, divisor_reg: u8) -> Self {
        self.divisor_reg = divisor_reg;
        self
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
    /// Linear address of the tested opcode, i.e. where a guarded exit must leave EIP.
    body_at: u32,
}

/// Compile `mov esi,esi / body / mov edi,edi / hlt` at `ENTRY` on the native role, warm the same
/// decode lines on the interpreter role, and seed both identically.
fn build(body: &[u8], seed: Seed) -> Roles {
    let mut code = MOV_ESI_ESI.to_vec();
    let body_at = ENTRY + code.len() as u32;
    code.extend_from_slice(body);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&MOV_EDI_EDI);
    code.push(0xf4);

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
    }

    let key = jit::direct::key_for(&native, ENTRY, true).expect("entry key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut native, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("structurally rejected: the F7 form is still a barrier")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
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
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(STACK_TOP);
        cpu.registers.set_eax(seed.eax);
        cpu.registers.set_edx(seed.edx);
        if !matches!(seed.divisor_reg, 0 | 2) {
            cpu.registers.gpr[usize::from(seed.divisor_reg)] = seed.divisor;
        }
        cpu.registers.eflags = seed.eflags;
        cpu.pending_flags = PendingFlags::default();
        if seed.live_pending {
            // A descriptor produced BEFORE the divide. DIV and IDIV must leave it untouched;
            // IMUL must materialize and clear it, exactly as `set_flag(CF|OF, ..)` does.
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
        body_at,
    }
}

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

/// A row whose divide (or multiply) completes NATIVELY: all three slots retire in the block, the
/// guard does not fire, and the whole architectural state matches three interpreted steps.
fn lowered(body: &[u8], seed: Seed, context: &str) {
    let mut roles = build(body, seed);
    let retired = roles.native.perf_counters().jit_direct_insns;
    let guarded_before = roles.native.direct_stall_snapshot().side_exit_divide_guard;
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
    assert_eq!(
        roles.native.direct_stall_snapshot().side_exit_divide_guard,
        guarded_before,
        "{context}: the divide guard must not fire on a representable result"
    );
    for _ in 0..3 {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state(&roles, context);
}

/// A row whose emitted guard REFUSES. The native run must end at the divide with the instruction
/// un-started; the interpreter then executes it and both roles must reach the same outcome.
fn guarded(body: &[u8], seed: Seed, context: &str) {
    let mut roles = build(body, seed);
    let retired = roles.native.perf_counters().jit_direct_insns;
    let guarded_before = roles.native.direct_stall_snapshot().side_exit_divide_guard;
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
        "{context}: only the slot BEFORE the divide may retire natively"
    );
    assert_eq!(
        roles.native.direct_stall_snapshot().side_exit_divide_guard - guarded_before,
        1,
        "{context}: exactly one divide-guard side exit"
    );
    assert_eq!(
        roles.native.registers.eip, roles.body_at,
        "{context}: the run must end AT the divide, not after it"
    );
    // One interpreted step on the twin puts it at the same boundary. Everything -- the
    // accumulator pair included -- must match: a guard that fired after touching a home would
    // show up here and nowhere else.
    roles.interp.cycle(&mut roles.interp_bus).unwrap();
    compare_state(&roles, &format!("{context}: at the guard"));

    // Both roles now execute the divide INTERPRETED (auto-admit is off, so the native role does
    // not re-dispatch the block) and must agree on the outcome, fault included.
    let native = roles.native.cycle(&mut roles.native_bus);
    let interp = roles.interp.cycle(&mut roles.interp_bus);
    assert_eq!(
        format!("{native:?}"),
        format!("{interp:?}"),
        "{context}: the interpreted re-execution must produce the same outcome"
    );
    compare_state(&roles, &format!("{context}: after the re-execution"));
}

// ---------------------------------------------------------------------------------------------
// 0xF7 /5 -- IMUL r/m32, one-operand signed multiply. No fault path at all.
// ---------------------------------------------------------------------------------------------

#[test]
fn imul_register_form_matches_the_interpreter() {
    // The classes that matter are the OVERFLOW classes, because CF/OF are the only flags the
    // instruction defines: a product that fits 32 bits signed (both flags clear), one that does
    // not (both set), and the sign combinations either side of zero. 0x8000_0000 squared is the
    // extreme, and 0xffff_ffff * 0xffff_ffff is (-1)*(-1) = 1, which an UNSIGNED lowering would
    // report as overflowing.
    for (eax, src) in [
        (0u32, 0u32),
        (1, 1),
        (2, 3),
        (0xffff_ffff, 0xffff_ffff),
        (0xffff_ffff, 1),
        (0x7fff_ffff, 2),
        (0x8000_0000, 0x8000_0000),
        (0x8000_0000, 1),
        (0x0001_0000, 0x0001_0000),
        (0xdead_beef, 0x1234_5678),
    ] {
        for eflags in [0x202u32, 0x8d7] {
            for pending in [false, true] {
                let mut seed = Seed::new(eax, 0xfeed_face, src).flags(eflags);
                if pending {
                    seed = seed.pending();
                }
                lowered(
                    &imul(3),
                    seed,
                    &format!("imul ebx eax={eax:#x} src={src:#x} eflags={eflags:#x} p={pending}"),
                );
            }
        }
    }
}

#[test]
fn imul_register_form_handles_its_own_destination_registers() {
    // `IMUL EAX` squares the accumulator and `IMUL EDX` multiplies by the register the product's
    // high half is about to land in. Both read the PRE-instruction value, so an emitter that
    // wrote a home before reading the multiplicand diverges here and nowhere else.
    for eax in [0u32, 3, 0xffff_ffff, 0x8000_0000, 0x1234_5678] {
        for edx in [0u32, 7, 0xffff_ffff, 0x8000_0000] {
            for rm in [0u8, 2] {
                let seed = Seed::new(eax, edx, 0).reg(rm);
                lowered(
                    &imul(rm),
                    seed,
                    &format!("imul rm={rm} eax={eax:#x} edx={edx:#x}"),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 0xF7 /6 -- DIV r/m32, unsigned. The guard is EXACTLY the interpreter's fault set.
// ---------------------------------------------------------------------------------------------

#[test]
fn div_register_form_matches_the_interpreter() {
    // Representable rows only: EDX < divisor is the whole admissibility rule, so every pair here
    // has EDX strictly below the divisor. The 0xffff_ffff / 1 row is the largest representable
    // quotient and the row immediately below the guard's boundary.
    for (edx, eax, divisor) in [
        (0u32, 0u32, 1u32),
        (0, 1, 1),
        (0, 0xffff_ffff, 1),
        (0, 0xffff_ffff, 0xffff_ffff),
        (0, 7, 3),
        (1, 0, 2),
        (1, 0xffff_ffff, 2),
        (0xffff_fffe, 0xffff_ffff, 0xffff_ffff),
        (0x1234, 0x5678_9abc, 0x0001_0000),
    ] {
        for eflags in [0x202u32, 0x8d7] {
            for pending in [false, true] {
                let mut seed = Seed::new(eax, edx, divisor).flags(eflags);
                if pending {
                    seed = seed.pending();
                }
                lowered(
                    &div(3),
                    seed,
                    &format!("div ebx edx={edx:#x} eax={eax:#x} d={divisor:#x} p={pending}"),
                );
            }
        }
    }
}

#[test]
fn div_register_form_handles_its_own_destination_registers() {
    // `DIV EAX` and `DIV EDX` take the divisor out of the pair the result overwrites. Both are
    // representable only in narrow cases, and both of the rows here are: EDX < EAX for the first,
    // and EDX < EDX is false, so the `DIV EDX` rows are GUARDED rather than lowered -- a divisor
    // that equals EDX always overflows, which is the guard's own boundary.
    for eax in [1u32, 2, 0xffff_ffff] {
        lowered(
            &div(0),
            Seed::new(eax, 0, 0).reg(0),
            &format!("div eax eax={eax:#x} edx=0"),
        );
    }
    for edx in [1u32, 0xffff_ffff] {
        guarded(
            &div(2),
            Seed::new(0, edx, 0).reg(2),
            &format!("div edx edx={edx:#x}"),
        );
    }
}

#[test]
fn div_by_zero_exits_to_the_interpreter_instead_of_faulting_the_host() {
    // THE row this whole guard exists for. A host `div ecx` with ECX = 0 raises #DE on the JIT
    // stack, which is an emulator-process exception and not a guest fault. Reaching this test at
    // all -- never mind its assertions -- is the evidence that the guard runs first.
    for edx in [0u32, 1, 0xffff_ffff] {
        for eax in [0u32, 0xffff_ffff] {
            guarded(
                &div(3),
                Seed::new(eax, edx, 0),
                &format!("div ebx by zero edx={edx:#x} eax={eax:#x}"),
            );
        }
    }
}

#[test]
fn div_quotient_overflow_exits_at_the_guards_exact_boundary() {
    // `EDX >= divisor` is the interpreter's overflow rule restated on the operands, so the pairs
    // that straddle it are the ones worth pinning: EDX == divisor - 1 must be LOWERED and
    // EDX == divisor must be GUARDED, with everything else held fixed.
    for divisor in [1u32, 2, 0x1234_5678, 0xffff_ffff] {
        if divisor > 1 {
            lowered(
                &div(3),
                Seed::new(0xffff_ffff, divisor - 1, divisor),
                &format!("div ebx just under the boundary d={divisor:#x}"),
            );
        }
        // `divisor` itself is the first EDX that overflows; the other two are further above it,
        // and `saturating_add` keeps the third from wrapping BELOW the boundary at 0xffff_ffff.
        for edx in [
            divisor,
            divisor.saturating_add(1),
            divisor.max(0xffff_ffff / 2),
            0xffff_ffff,
        ] {
            guarded(
                &div(3),
                Seed::new(0, edx, divisor),
                &format!("div ebx overflow edx={edx:#x} d={divisor:#x}"),
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 0xF7 /7 -- IDIV r/m32, signed. Divided at 64 bits, then range-compared.
// ---------------------------------------------------------------------------------------------

#[test]
fn idiv_register_form_matches_the_interpreter() {
    // Sign combinations first, because IDIV truncates TOWARD ZERO and the remainder takes the
    // DIVIDEND's sign -- a lowering that used a flooring division would agree on every row where
    // the two operands share a sign and differ on every row where they do not.
    for (edx, eax, divisor) in [
        (0u32, 0u32, 1u32),
        (0, 7, 2),
        (0, 7, 0xffff_fffe),                     // 7 / -2
        (0xffff_ffff, 0xffff_fff9, 2),           // -7 / 2
        (0xffff_ffff, 0xffff_fff9, 0xffff_fffe), // -7 / -2
        (0, 1, 1),
        (0xffff_ffff, 0xffff_ffff, 1), // -1 / 1
        (0, 0x7fff_ffff, 1),           // the largest representable quotient
        (0xffff_ffff, 0x8000_0000, 1), // i32::MIN / 1, the smallest -- and legal
        (0, 0x0001_0000, 0x0001_0000),
        (0xffff_ffff, 0x8765_4321, 0x1234_5678),
    ] {
        for eflags in [0x202u32, 0x8d7] {
            for pending in [false, true] {
                let mut seed = Seed::new(eax, edx, divisor).flags(eflags);
                if pending {
                    seed = seed.pending();
                }
                lowered(
                    &idiv(3),
                    seed,
                    &format!("idiv ebx edx={edx:#x} eax={eax:#x} d={divisor:#x} p={pending}"),
                );
            }
        }
    }
}

#[test]
fn idiv_by_zero_exits_to_the_interpreter_instead_of_faulting_the_host() {
    for (edx, eax) in [(0u32, 0u32), (0, 1), (0xffff_ffff, 0xffff_ffff)] {
        guarded(
            &idiv(3),
            Seed::new(eax, edx, 0),
            &format!("idiv ebx by zero edx={edx:#x} eax={eax:#x}"),
        );
    }
}

#[test]
fn idiv_quotient_overflow_exits_at_the_guards_exact_boundary() {
    // Guard 3 is a COMPARISON on the 64-bit answer, so the boundary rows are the two quotients
    // either side of i32's ends: 0x7fff_ffff and i32::MIN are legal, 0x8000_0000 and
    // -0x8000_0001 are not. Held at divisor 1 so the quotient IS the dividend.
    lowered(
        &idiv(3),
        Seed::new(0x7fff_ffff, 0, 1),
        "idiv ebx quotient i32::MAX",
    );
    guarded(
        &idiv(3),
        Seed::new(0x8000_0000, 0, 1),
        "idiv ebx quotient i32::MAX + 1",
    );
    lowered(
        &idiv(3),
        Seed::new(0x8000_0000, 0xffff_ffff, 1),
        "idiv ebx quotient i32::MIN",
    );
    guarded(
        &idiv(3),
        Seed::new(0x7fff_ffff, 0xffff_ffff, 1),
        "idiv ebx quotient i32::MIN - 1",
    );
    // And a wide one, so the row set is not entirely divisor-1: 2^62 / 2 is still far outside.
    guarded(
        &idiv(3),
        Seed::new(0, 0x4000_0000, 2),
        "idiv ebx quotient 2^61",
    );
}

#[test]
fn idiv_min_dividend_over_minus_one_exits_instead_of_overflowing_the_host_divide() {
    // The one case a 64-bit `idiv` faults on that is NOT a zero divisor: RDX:RAX = i64::MIN over
    // -1. Guard 2 removes it, and this row is the reason guard 2 is not optional.
    guarded(
        &idiv(3),
        Seed::new(0, 0x8000_0000, 0xffff_ffff),
        "idiv ebx i64::MIN / -1",
    );
}

#[test]
fn idiv_by_minus_one_is_refused_conservatively_and_still_agrees() {
    // Guard 2 is the ONLY place this lowering refuses more than the interpreter faults on: a
    // legal `IDIV` by -1 takes a side exit and the interpreter completes it. The row is here so
    // the conservatism is a TESTED property rather than a comment -- if it is ever made exact,
    // these become `lowered` and the change is visible in the diff.
    for (edx, eax) in [
        (0u32, 0u32),
        (0xffff_ffff, 0xffff_fffe), // -2 / -1 = 2, perfectly representable
        (0, 5),
    ] {
        guarded(
            &idiv(3),
            Seed::new(eax, edx, 0xffff_ffff),
            &format!("idiv ebx by -1 edx={edx:#x} eax={eax:#x}"),
        );
    }
}

#[test]
fn idiv_register_form_handles_its_own_destination_registers() {
    // `IDIV EAX` with EDX = 0 divides a positive dividend by itself: quotient 1, remainder 0, and
    // representable. The row proves the divisor is read BEFORE EAX is overwritten -- an emitter
    // that wrote the quotient home first would divide by the quotient.
    for eax in [1u32, 2, 7, 0x7fff_ffff] {
        lowered(
            &idiv(0),
            Seed::new(eax, 0, 0).reg(0),
            &format!("idiv eax eax={eax:#x}"),
        );
    }
    // `IDIV EDX` is ALWAYS guarded, and that is arithmetic rather than a limitation: the quotient
    // is `(EDX*2^32 + EAX) / EDX = 2^32 + EAX/EDX`, which lands inside i32 only if `|EDX| == 1`,
    // and EDX == 1 still gives 2^32 while EDX == -1 is guard 2. The rows are kept because the
    // aliasing is what they cover: the guard must read EDX as the DIVISOR and not as the high
    // half it is about to overwrite.
    for edx in [1u32, 2, 0x7fff_ffff, 0xffff_ffff] {
        guarded(
            &idiv(2),
            Seed::new(0, edx, 0).reg(2),
            &format!("idiv edx edx={edx:#x}"),
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Admission is pinned next to the group's other admission cover, in
// `cpu_jit_test_imm_test.rs`: `group3_non_test_subops_remain_interpreter_only` holds the byte,
// memory and 66-prefixed forms out, and `group3_dword_neg_register_form_is_lowered` holds these
// three in. Both halves are needed -- a negative list alone cannot tell "this sub-opcode stayed
// out" from "nothing compiles any more".
// ---------------------------------------------------------------------------------------------
