// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Cover for the interpreter CALL-OUT slot (`jit/direct/callout.rs`), Phase 5 Task 1.
//!
//! Two layers, deliberately separable so a reviewer can refute either claim on its own:
//!
//! * the HELPER, driven directly against a CPU and a device bus -- the status encoding, the
//!   exact charge, and the zero-partial-effects claim on the abnormal returns;
//! * the EMITTED SLOT, run mid-block against the interpreter -- registers, lazy flags, EFLAGS,
//!   core clocks and bus clocks, plus the two exit shapes.
//!
//! The tested opcode is MID-BLOCK in every emitted case. An opcode at a block's entry slot parks
//! the block on the interpreter, so an entry-position fixture certifies nothing.
//!
//! Mutation record for this slice (all verified by hand before the commit):
//! * dropping one `GUEST_HOMES` entry from the call-out's reload loop fails four tests on
//!   `registers`;
//! * deleting the runtime raw-clock add at the call site fails two tests on `core clocks`;
//! * dropping the weighted-FP lane from the device-timestamp preview fails
//!   `the_helper_folds_the_chains_float_clocks_into_the_device_timestamp`.
//!
//! The privileged port state is refused at TWO places, tested separately: `run_direct_block`
//! refuses to ENTER the block (`a_call_out_block_is_not_entered_at_all_in_the_privileged_port_state`,
//! the gate that decides cost), and the helper refuses before touching anything
//! (`a_permission_checked_port_is_refused_before_the_tss_probe_can_touch_memory`, the gate that
//! decides correctness).

use super::*;

const ENTRY: u32 = 0x401;
const STACK_TOP: u32 = 0x4000;
/// Any port above 7 works for the permission-denial case: with a zero TSS limit the bitmap byte
/// index for `port / 8` is already past the limit, which is the `#GP(0)` the interpreter raises.
const PORT: u16 = 0x03da;

fn flat_cpu() -> CpuGsw {
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
    cpu.set_eip(ENTRY);
    cpu
}

// ---------------------------------------------------------------------------------------------
// The helper, on its own.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_helper_charges_exactly_what_the_interpreter_charges_and_reports_the_step_break() {
    for lazy in [false, true] {
        let mut cpu = flat_cpu();
        cpu.registers.set_edx(u32::from(PORT));
        cpu.registers.set_eax(0xdead_beef);
        let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
        bus.lazy_io_reads = lazy;
        bus.io_read_value = Some(0x5a);

        let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, 0, 0, 0);
        assert!(
            status >= 0,
            "lazy={lazy}: a served port read is not abnormal"
        );
        assert_eq!(
            status & 0xffff_ffff,
            i64::from(IN_PORT_CORE_CLOCKS),
            "lazy={lazy}: the helper must return the interpreter's own raw charge"
        );
        assert_eq!(
            status >> jit::direct::STATUS_STEP_BREAK_BIT,
            i64::from(!lazy),
            "lazy={lazy}: the step-break bit must mirror the bus's own answer"
        );
        assert_eq!(
            cpu.registers.eax(),
            0xdead_be5a,
            "lazy={lazy}: only AL may change"
        );
        // The helper charges NOTHING itself: the caller folds the returned clocks into the
        // block's lane, and `run_direct_block` scales the whole lane once.
        assert_eq!(cpu.elapsed_clocks, 0, "lazy={lazy}: helper charged clocks");
        assert_eq!(cpu.perf_counters().instructions, 0);
    }
}

#[test]
fn the_helper_hands_the_device_the_running_clock_total_not_the_block_entry_total() {
    // The prefix is RAW clocks; what the device must see is the block-entry total plus the
    // SCALED prefix, which is exactly what an interpreted continuation would have passed. Without
    // the preview scaling this reads back the block-entry total and a mid-block poll samples a
    // beam in the past.
    let mut cpu = flat_cpu();
    cpu.registers.set_edx(u32::from(PORT));
    cpu.core_clocks_so_far = 100;
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
    bus.lazy_io_reads = true;

    let prefix_raw = 24u64;
    let expected_prefix = {
        // The same long division `scale_clocks_batch` performs, from an untouched carry.
        let mut probe = flat_cpu();
        probe.scale_clocks_batch(prefix_raw)
    };
    let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, prefix_raw, 0, 0);
    assert!(status >= 0);
    assert_eq!(
        bus.last_read_io_core_clocks_so_far,
        Some(100 + expected_prefix)
    );
    // And the preview must NOT have consumed the carry: the block still owes this charge.
    assert_eq!(cpu.timing_rem, 0, "preview scaling consumed the carry");
}

#[test]
fn a_denied_port_is_abnormal_with_zero_partial_effects() {
    // CPL 3 with IOPL 0 sends `check_io_permission` to the TSS bitmap, and a zero TSS limit
    // denies every port -- the `#GP(0)` producer, the first member of the abnormal set.
    let mut cpu = flat_cpu();
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x0b, 0xfb));
    cpu.registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x13, 0xf3));
    cpu.registers.set_edx(u32::from(PORT));
    cpu.registers.set_eax(0xdead_beef);
    cpu.registers.eflags = 0x202;
    // CPL is tracked as CPU state, not re-derived from CS on every read.
    cpu.cpl = 3;
    assert_eq!(cpu.current_privilege_level(), 3, "fixture must be at CPL 3");

    let before = cpu.registers.clone();
    let before_eflags = cpu.eflags();
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
    bus.lazy_io_reads = true;

    let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, 0, 0, 0);
    assert!(status < 0, "a denied port must be abnormal");
    assert_eq!(before, cpu.registers, "abnormal path wrote a register");
    assert_eq!(before_eflags, cpu.eflags(), "abnormal path wrote EFLAGS");
    assert_eq!(cpu.elapsed_clocks, 0, "abnormal path charged clocks");
    assert_eq!(
        bus.last_read_io_core_clocks_so_far, None,
        "the permission check must run BEFORE any device is addressed"
    );
}

#[test]
fn an_unsupported_port_is_abnormal_with_zero_partial_effects() {
    // The second abnormal producer: every device declined, so `read_io` errors and no device
    // observed the access -- which is what makes the interpreter's retry side-effect free.
    let mut cpu = flat_cpu();
    cpu.registers.set_edx(u32::from(PORT));
    cpu.registers.set_eax(0xdead_beef);
    let before = cpu.registers.clone();
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
    bus.io_read_fails = true;

    let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, 0, 0, 0);
    assert!(status < 0);
    assert_eq!(before, cpu.registers);
    assert_eq!(cpu.elapsed_clocks, 0);
    assert!(!bus.io_touched, "a failed read must not end the batch");
}

// ---------------------------------------------------------------------------------------------
// The emitted slot.
// ---------------------------------------------------------------------------------------------

struct Fixture {
    cpu: CpuGsw,
    bus: TestBus,
    block: jit::direct::CompiledBlock,
}

/// Build the three-slot block `mov esi,esi` / `in al,dx` / `mov edi,edi`, with the call-out
/// MID-BLOCK. Returns the compiled fixture and, separately, an interpreter twin on the same
/// bytes and the same bus knobs.
fn slot_block(configure: impl Fn(&mut TestBus)) -> (Fixture, CpuGsw, TestBus) {
    slot_block_with(configure, |_| {})
}

/// As `slot_block`, but `configure_cpu` also runs on BOTH CPUs before the block is compiled.
/// Privilege state has to be set before compilation, not just before the run: `memory_cpl3` is
/// sealed into the block and re-checked at entry, so a block compiled at CPL 0 is simply not run
/// at CPL 3 and the fixture would certify nothing.
fn slot_block_with(
    configure: impl Fn(&mut TestBus),
    configure_cpu: impl Fn(&mut CpuGsw),
) -> (Fixture, CpuGsw, TestBus) {
    let mut code = vec![0x89, 0xf6];
    let body_at = ENTRY + code.len() as u32;
    code.push(0xec);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);

    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu();
    let mut interpreter = flat_cpu();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interpreter_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        configure(bus);
    }
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interpreter, &mut interpreter_bus),
    ] {
        configure_cpu(cpu);
        cpu.registers.set_esp(STACK_TOP);
        for &linear in &[ENTRY, body_at, tail_at] {
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
            panic!("structurally rejected: IN AL,DX is still a barrier")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, 3,
        "the block must extend THROUGH the call-out, not stop at it"
    );
    assert_eq!(
        compilation.callout_slots, 1,
        "the call-out must be counted for the budget bound"
    );
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = native.jit_direct.block(id).expect("live block");

    for cpu in [&mut native, &mut interpreter] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(STACK_TOP);
        cpu.registers.set_edx(u32::from(PORT));
        cpu.registers.set_eax(0xdead_beef);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.set_eip(ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    (
        Fixture {
            cpu: native,
            bus: native_bus,
            block,
        },
        interpreter,
        interpreter_bus,
    )
}

#[test]
fn call_out_matches_the_interpreter_mid_block() {
    // Lazy reads so the bus asks for no step break and the block runs all three slots -- the
    // shape the whole slice exists for. `io_read_value` is non-zero so a reload that dropped AL
    // is visible; a bus that always returned 0 would pass against a broken reload.
    let (mut fixture, mut interpreter, mut interpreter_bus) = slot_block(|bus| {
        bus.lazy_io_reads = true;
        bus.io_read_value = Some(0x5a);
    });

    let retired = fixture.cpu.perf_counters().jit_direct_insns;
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap(),
        "block did not run natively"
    );
    for _ in 0..3 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(
        fixture.cpu.perf_counters().jit_direct_insns - retired,
        3,
        "all three slots, the call-out included, must retire natively"
    );
    assert_eq!(
        fixture.cpu.registers.eax() & 0xff,
        0x5a,
        "the port byte must land in AL"
    );
    assert_eq!(
        crate::tests::settled_registers(&fixture.cpu),
        crate::tests::settled_registers(&interpreter),
        "registers"
    );
    assert_eq!(fixture.cpu.eflags(), interpreter.eflags(), "EFLAGS");
    assert_eq!(
        fixture.cpu.elapsed_clocks, interpreter.elapsed_clocks,
        "core clocks"
    );
    assert_eq!(
        fixture.bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks(),
        "bus clocks"
    );
}

#[test]
fn a_step_breaking_port_ends_the_native_run_after_the_call_out() {
    // A non-lazy read touches time-dependent device state, so the run must end at the boundary
    // AFTER the IN -- the same boundary `run_straight_line`'s post-instruction check produces.
    // The third slot stays for the interpreter.
    let (mut fixture, mut interpreter, mut interpreter_bus) = slot_block(|bus| {
        bus.io_read_value = Some(0x5a);
    });

    let retired = fixture.cpu.perf_counters().jit_direct_insns;
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    for _ in 0..2 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(
        fixture.cpu.perf_counters().jit_direct_insns - retired,
        2,
        "the call-out and its prefix retire natively, the tail does not"
    );
    assert_eq!(
        fixture.cpu.registers.eip, interpreter.registers.eip,
        "EIP must sit AFTER the call-out"
    );
    assert_eq!(
        crate::tests::settled_registers(&fixture.cpu),
        crate::tests::settled_registers(&interpreter),
        "registers"
    );
    assert_eq!(
        fixture.cpu.elapsed_clocks, interpreter.elapsed_clocks,
        "core clocks"
    );
    assert_eq!(
        fixture.cpu.perf_counters().jit_direct_side_exits,
        1,
        "the step break leaves through the side-exit path"
    );
    let stalls = fixture.cpu.direct_stall_snapshot();
    assert_eq!(stalls.side_exit_callout_step_break, 1);
    assert_eq!(stalls.side_exit_callout_abnormal, 0);
}

#[test]
fn an_abnormal_call_out_ends_the_run_at_the_instruction_with_no_partial_effects() {
    let (mut fixture, mut interpreter, mut interpreter_bus) = slot_block(|bus| {
        bus.io_read_fails = true;
    });

    let retired = fixture.cpu.perf_counters().jit_direct_insns;
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    // The interpreter twin runs only the prefix: it is what the run loop has executed when a
    // block ends at an IN barrier today.
    interpreter.cycle(&mut interpreter_bus).unwrap();

    assert_eq!(
        fixture.cpu.perf_counters().jit_direct_insns - retired,
        1,
        "only the prefix may retire"
    );
    assert_eq!(
        crate::tests::settled_registers(&fixture.cpu),
        crate::tests::settled_registers(&interpreter),
        "the abnormal exit must leave exactly the pre-IN state"
    );
    assert_eq!(
        fixture.cpu.registers.eax(),
        0xdead_beef,
        "AL must be untouched"
    );
    assert_eq!(
        fixture.cpu.elapsed_clocks, interpreter.elapsed_clocks,
        "the refused instruction must not be charged"
    );
    let stalls = fixture.cpu.direct_stall_snapshot();
    assert_eq!(stalls.side_exit_callout_abnormal, 1);
    assert_eq!(stalls.side_exit_callout_step_break, 0);
}

#[test]
fn a_call_out_block_is_never_shaped_as_a_native_self_loop() {
    // `mov esi,esi` / `in al,dx` / `jz -3`: a self-loop tail. The self-loop SHAPE multiplies the
    // static accounting at exit, which cannot coexist with a per-iteration runtime deposit, so
    // the block must compile without it.
    let mut code = vec![0x89, 0xf6, 0xec];
    let jz_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&[0x74, 0xfd]);
    let _ = jz_at;

    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    cpu.registers.set_esp(STACK_TOP);
    for offset in [0u32, 2, 3] {
        let linear = ENTRY + offset;
        cpu.set_eip(linear);
        cpu.fetch_decoded(&mut bus, linear).unwrap();
    }
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("entry key");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut cpu, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        _ => panic!("a self-loop-shaped call-out block must still compile"),
    };
    assert_eq!(compilation.callout_slots, 1);
    assert!(
        !compilation.self_loop,
        "a block holding a call-out must not be shaped as a native self-loop"
    );
}

// ---------------------------------------------------------------------------------------------
// The privilege refusal: the TSS probe is EXCLUDED, not supported.
// ---------------------------------------------------------------------------------------------

const TSS_BASE: u32 = 0x2000;
const TSS_IO_MAP_OFFSET: u16 = 0x100;
const PAGED_DIRECTORY: u32 = 0x1000;
const PAGED_TABLE: u32 = 0x3000;

/// A CPU whose `check_io_permission` MUST consult the TSS bitmap, with paging on so that consult
/// takes a real page walk. Low memory is identity-mapped with the ACCESSED BITS CLEAR, so the
/// first walk through any entry writes one -- which is the hazard, and the thing the native path
/// must be shown not to do.
///
/// `deny` picks whether the bitmap refuses `PORT`. Either way the CONSULT is the subject.
fn paged_ring3_io_cpu(deny: bool) -> (CpuGsw, TestBus) {
    paged_ring3_io_cpu_with(deny, TSS_IO_MAP_OFFSET, 0x1000)
}

/// As `paged_ring3_io_cpu`, with the bitmap's distance from the TSS base and the TSS limit under
/// the caller's control.
///
/// The distance is what puts the io_base WORD and the bitmap BYTE on different pages, which is
/// the only way to separate phase P's two translate/peek pairs: with the default 0x100 offset
/// both live in page 2 and a single TLB entry serves them, so a fixture built on it cannot tell
/// P3 from P7 or P4 from P8.
fn paged_ring3_io_cpu_with(deny: bool, io_map_offset: u16, limit: u32) -> (CpuGsw, TestBus) {
    let mut memory = vec![0u8; 0x9000];
    // PDE 0 -> the table: present, writable, user, accessed CLEAR.
    memory[PAGED_DIRECTORY as usize..PAGED_DIRECTORY as usize + 4]
        .copy_from_slice(&(PAGED_TABLE | 0x07).to_le_bytes());
    for page in 0..9u32 {
        let pte = (PAGED_TABLE + page * 4) as usize;
        memory[pte..pte + 4].copy_from_slice(&((page << 12) | 0x07).to_le_bytes());
    }
    let base = TSS_BASE as usize;
    memory[base + 0x66..base + 0x68].copy_from_slice(&io_map_offset.to_le_bytes());
    let bitmap = base + usize::from(io_map_offset) + usize::from(PORT / 8);
    memory[bitmap] = if deny { 1 << (PORT % 8) } else { 0 };

    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = PAGED_DIRECTORY;
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x0b, 0xfb));
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(0x13, 0xf3));
    }
    // CPL 3 with IOPL 0 is the REACHABLE half of the refused predicate. Its V86 half is covered
    // by `v86_call_out_is_refused_before_the_tss_probe` through the helper alone, because a V86
    // BLOCK cannot exist: V86 code segments are always CS.D = 0 and no 16-bit block form is
    // admitted on any persona yet.
    cpu.cpl = 3;
    cpu.registers.eflags = 0x202;
    cpu.tr.base = TSS_BASE;
    cpu.tr.limit = limit;
    cpu.registers.set_edx(u32::from(PORT));
    cpu.registers.set_eax(0xdead_beef);

    let mut bus = TestBus::with_memory(memory);
    bus.lazy_io_reads = true;
    bus.io_read_value = Some(0x5a);
    (cpu, bus)
}

fn page_walk_writes(bus: &TestBus) -> Vec<u32> {
    bus.trace
        .cycles()
        .iter()
        .filter(|cycle| cycle.kind == BusAccessKind::PageWalkWrite)
        .map(|cycle| cycle.address)
        .collect()
}

#[test]
fn a_permission_checked_port_is_refused_before_the_tss_probe_can_touch_memory() {
    // Under `is_v86_mode() || CPL > IOPL` the interpreter's permission check walks the TSS, and
    // under paging that walk WRITES guest memory (accessed bits), records written_pages, can set
    // CR2, advances bus clocks and can reach `note_code_write` with the block's native code live
    // on the stack. The helper refuses that state before anything runs.
    for deny in [false, true] {
        let (mut cpu, mut bus) = paged_ring3_io_cpu(deny);
        let before = cpu.registers.clone();
        let before_cr2 = cpu.control.cr2;

        let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, 0, 0, 0);

        assert!(
            status < 0,
            "deny={deny}: the privileged state must be refused"
        );
        assert_eq!(before, cpu.registers, "deny={deny}: registers");
        assert_eq!(before_cr2, cpu.control.cr2, "deny={deny}: CR2");
        assert_eq!(cpu.written_count, 0, "deny={deny}: written_pages");
        assert!(!cpu.written_pages_overflow, "deny={deny}");
        assert_eq!(cpu.elapsed_clocks, 0, "deny={deny}: clocks");
        assert!(
            page_walk_writes(&bus).is_empty(),
            "deny={deny}: the refused path set a page-table accessed bit"
        );
        assert_eq!(
            bus.trace.elapsed_clocks(),
            0,
            "deny={deny}: the refused path advanced bus clocks"
        );
        assert_eq!(
            bus.last_read_io_core_clocks_so_far, None,
            "deny={deny}: the refused path reached the device"
        );
    }
}

#[test]
fn the_interpreter_still_does_the_tss_probe_the_call_out_refused() {
    // What stops the test above from being vacuous: from the SAME state the interpreter really
    // does walk the TSS, set accessed bits and charge for it. The hazard is live, and refusing it
    // costs the guest only the call-out -- the instruction still executes, completely, one
    // boundary later.
    for deny in [false, true] {
        let (mut cpu, mut bus) = paged_ring3_io_cpu(deny);
        let entry = 0x4000u32;
        bus.memory[entry as usize] = 0xec;
        bus.memory[entry as usize + 1] = 0xf4;
        cpu.set_eip(entry);

        // NOT unwrapped: with `deny` the #GP has no IDT to land in and nests. Irrelevant here --
        // the page walk that this test is about has already happened by then, which is the point.
        let outcome = cpu.cycle(&mut bus);

        assert!(
            !page_walk_writes(&bus).is_empty(),
            "deny={deny}: the fixture never reached a page walk, so it proves nothing"
        );
        if deny {
            // The fault has no IDT to land in, so this fixture stops before `finish_instruction`
            // can charge; the probe -- the subject -- already ran, which the walk above pins.
            assert_eq!(
                cpu.registers.eax(),
                0xdead_beef,
                "a denied port must not write AL"
            );
        } else {
            outcome.expect("a permitted port must retire");
            assert!(
                cpu.elapsed_clocks > 0,
                "the interpreted instruction must charge"
            );
            assert_eq!(cpu.registers.eip, entry + 1, "the IN must retire");
            assert_eq!(
                cpu.registers.eax(),
                0xdead_be5a,
                "the port byte lands in AL"
            );
        }
    }
}

#[test]
fn v86_call_out_on_a_cold_tlb_is_refused_before_the_tss_probe() {
    // The other half of the refused predicate, isolated: IOPL is 3 here, so the `CPL > IOPL` half
    // is FALSE and only `is_v86_mode()` can send the helper down the TSS-bitmap arm.
    //
    // On a COLD TLB that arm refuses at P3, which is the lane the whole zero-partial-effects
    // argument rests on: the interpreter would page-walk here, and a walk from inside a live
    // block is what this design will not do. The warm counterpart is
    // `a_v86_port_is_served_natively_once_the_tss_pages_are_tlb_resident`, and the two together
    // are what say this refusal is the TLB's answer rather than a blanket one.
    let (mut cpu, mut bus) = paged_ring3_io_cpu(false);
    cpu.registers.eflags = 0x202 | FLAG_VM | (3 << 12);
    assert_eq!(cpu.current_privilege_level(), 3);
    assert_eq!(cpu.iopl(), 3);
    let before = cpu.registers.clone();

    let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, 0, 0, 0);

    assert!(status < 0, "a V86 task on a cold TLB must be refused");
    assert_eq!(before, cpu.registers);
    assert!(page_walk_writes(&bus).is_empty());
    assert_eq!(bus.last_read_io_core_clocks_so_far, None);
}

// ---------------------------------------------------------------------------------------------
// The TSS-bitmap arm: phase P proves residency purely, phase C charges. One test per refusal
// lane, each built so the lane it names is the ONLY one that can produce the refusal.
// ---------------------------------------------------------------------------------------------

/// The io-map offset that puts the bitmap byte on a DIFFERENT page from the io_base word:
/// `0x2000 + 0x1000 + 0x3da/8 = 0x307b`, against the word at `0x2066`.
const TSS_SPLIT_PAGE_IO_MAP_OFFSET: u16 = 0x1000;
const IO_BASE_PAGE: u32 = 2;
const SPLIT_BITMAP_PAGE: u32 = 3;

/// Execute ONE interpreted `IN AL,DX` from this state and then rewind everything the assertions
/// look at.
///
/// This is not fixture convenience, it is the mechanism: the interpreted instruction is what
/// FILLS the two TLB entries phase P then requires, which is the design's self-healing claim --
/// a miss costs one interpreted IN and the next one is served natively. Building the entries by
/// hand would let the fixture pass against a phase P that read the wrong linear.
fn warm_the_tss_tlb(cpu: &mut CpuGsw, bus: &mut TestBus) {
    let entry = 0x4000u32;
    bus.memory[entry as usize] = 0xec;
    bus.memory[entry as usize + 1] = 0xf4;
    cpu.set_eip(entry);
    cpu.cycle(bus).expect("the warming IN must retire");
    assert!(
        !page_walk_writes(bus).is_empty(),
        "the warming pass never walked, so the TLB is not warm for the reason claimed"
    );

    cpu.registers.set_eax(0xdead_beef);
    cpu.registers.set_edx(u32::from(PORT));
    cpu.registers.eflags = 0x202;
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    cpu.written_count = 0;
    cpu.written_pages_overflow = false;
    bus.trace = BusTrace::default();
    bus.last_read_io_core_clocks_so_far = None;
    bus.io_reads.clear();
    bus.io_touched = false;
}

/// A CPL-3 paged fixture whose TSS pages are already TLB-resident.
fn warmed_tss_cpu(io_map_offset: u16, limit: u32) -> (CpuGsw, TestBus) {
    let (mut cpu, mut bus) = paged_ring3_io_cpu_with(false, io_map_offset, limit);
    warm_the_tss_tlb(&mut cpu, &mut bus);
    (cpu, bus)
}

/// Everything a refusal must leave untouched, in one place so no lane's test can quietly assert
/// less than another's.
fn assert_refused_with_zero_partial_effects(
    lane: &str,
    status: i64,
    cpu: &CpuGsw,
    bus: &TestBus,
    before: &Registers,
    before_cr2: u32,
) {
    assert!(status < 0, "{lane}: the lane must refuse");
    assert_eq!(before, &cpu.registers, "{lane}: a register was written");
    assert_eq!(before_cr2, cpu.control.cr2, "{lane}: CR2 moved");
    assert_eq!(cpu.written_count, 0, "{lane}: written_pages");
    assert!(
        !cpu.written_pages_overflow,
        "{lane}: written_pages_overflow"
    );
    assert_eq!(cpu.elapsed_clocks, 0, "{lane}: charged core clocks");
    assert!(
        page_walk_writes(bus).is_empty(),
        "{lane}: the refused path set a page-table accessed bit"
    );
    assert_eq!(
        bus.trace.cycles().len(),
        0,
        "{lane}: the refused path recorded a bus cycle -- phase P must charge and record nothing"
    );
    assert_eq!(
        bus.trace.elapsed_clocks(),
        0,
        "{lane}: the refused path advanced bus clocks"
    );
    assert_eq!(
        bus.last_read_io_core_clocks_so_far, None,
        "{lane}: the refused path reached the device"
    );
}

#[test]
fn a_v86_port_is_served_natively_once_the_tss_pages_are_tlb_resident() {
    // THE POINT OF THE SLICE. Same state as the cold test above, one interpreted IN later.
    let (mut cpu, mut bus) = warmed_tss_cpu(TSS_IO_MAP_OFFSET, 0x1000);
    cpu.registers.eflags = 0x202 | FLAG_VM | (3 << 12);
    assert!(cpu.is_v86_mode());
    assert_eq!(cpu.iopl(), 3, "only the V86 half may select the arm");
    let served_before = cpu.direct_stall_snapshot().callout_port_v86_served;

    let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, 0, 0, 0);

    assert!(status >= 0, "a permitted V86 port must be served");
    assert_eq!(
        status & 0xffff_ffff,
        i64::from(IN_PORT_CORE_CLOCKS),
        "the served V86 arm must charge the interpreter's own constant"
    );
    assert_eq!(
        cpu.registers.eax(),
        0xdead_be5a,
        "the port byte lands in AL"
    );
    assert!(
        page_walk_writes(&bus).is_empty(),
        "the served path page-walked -- the whole hazard the design refuses"
    );
    assert_eq!(cpu.written_count, 0, "the served path wrote guest memory");
    assert_eq!(
        cpu.direct_stall_snapshot().callout_port_v86_served - served_before,
        1,
        "the served count must attribute this to the bitmap arm, not to the CPL0 one"
    );
    let stalls = cpu.direct_stall_snapshot();
    assert_eq!(
        stalls.callout_executed, 1,
        "the denominator must count this call-out too"
    );
    assert_eq!(stalls.side_exit_callout_abnormal, 0);
}

#[test]
fn the_cpl0_arm_does_not_count_as_a_bitmap_serve() {
    // The counter's own non-vacuity: `callout_port_v86_served` must separate the two arms, or the
    // acceptance ratio is measuring `callout_executed` twice.
    let mut cpu = flat_cpu();
    cpu.registers.set_edx(u32::from(PORT));
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
    bus.lazy_io_reads = true;
    bus.io_read_value = Some(0x5a);

    let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, 0, 0, 0);

    assert!(status >= 0);
    let stalls = cpu.direct_stall_snapshot();
    assert_eq!(stalls.callout_executed, 1);
    assert_eq!(
        stalls.callout_port_v86_served, 0,
        "the CPL0 arm must not be counted as a bitmap serve"
    );
}

#[test]
fn the_bitmap_arm_charges_exactly_what_the_interpreter_charges() {
    // CHARGE IDENTITY, on the axis that carries it: the ordered list of DATA and IO bus cycles.
    //
    // A/B/A/B on ONE machine, not a single A-then-B pair. Wait states are priced against a data
    // cache tag array on the production bus, so the FIRST touch of a line is not the price of the
    // second: an A-then-B pair compares a cold A against a warm B and passes on an arm that
    // charges differently. Interleaving twice makes the second pair a repeat of the first, and
    // all four legs must agree.
    let (mut cpu, mut bus) = warmed_tss_cpu(TSS_IO_MAP_OFFSET, 0x1000);
    let entry = 0x4000u32;

    let interesting = |bus: &TestBus| -> Vec<(BusAccessKind, u32, BusWidth)> {
        bus.trace
            .cycles()
            .iter()
            .filter(|cycle| matches!(cycle.kind, BusAccessKind::DataRead | BusAccessKind::IoRead))
            .map(|cycle| (cycle.kind, cycle.address, cycle.width))
            .collect()
    };

    let mut legs: Vec<Vec<(BusAccessKind, u32, BusWidth)>> = Vec::new();
    // The DEVICE-VISIBLE TIMESTAMP, which the cycle list cannot see. 0x3DA is answered by the
    // lazy status-port path, whose `predicted_beam` is a pure function of the clock the CPU hands
    // it, so a helper that passed a different `now` would return a different retrace bit from the
    // same guest state -- a guest-visible divergence with an identical charge.
    let mut timestamps: Vec<Option<u64>> = Vec::new();
    for leg in 0..4 {
        bus.trace = BusTrace::default();
        bus.last_read_io_core_clocks_so_far = None;
        cpu.registers.set_eax(0xdead_beef);
        cpu.registers.set_edx(u32::from(PORT));
        cpu.elapsed_clocks = 0;
        cpu.core_clocks_so_far = 0;
        if leg % 2 == 0 {
            cpu.set_eip(entry);
            cpu.cycle(&mut bus).expect("the interpreted IN must retire");
        } else {
            let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, 0, 0, 0);
            assert!(status >= 0, "leg {leg}: the helper must serve");
        }
        assert_eq!(
            cpu.registers.eax(),
            0xdead_be5a,
            "leg {leg}: both roles must land the port byte"
        );
        legs.push(interesting(&bus));
        timestamps.push(bus.last_read_io_core_clocks_so_far);
    }

    assert_eq!(legs[0], legs[1], "interpreter/helper pair 1 disagreed");
    assert_eq!(legs[2], legs[3], "interpreter/helper pair 2 disagreed");
    assert_eq!(legs[1], legs[3], "the helper's own two legs disagreed");
    // Non-vacuity: the shape really is the io_base word, the bitmap byte and the port, in that
    // order. An empty or truncated list would satisfy every assertion above.
    assert_eq!(
        legs[0],
        vec![
            (BusAccessKind::DataRead, TSS_BASE + 0x66, BusWidth::Word),
            (
                BusAccessKind::DataRead,
                TSS_BASE + u32::from(TSS_IO_MAP_OFFSET) + u32::from(PORT) / 8,
                BusWidth::Byte
            ),
            (BusAccessKind::IoRead, u32::from(PORT), BusWidth::Byte),
        ],
        "the charge ORDER is the claim; this is what section 2 of the design pins"
    );
    assert!(
        timestamps[0].is_some(),
        "no leg reached the device, so the timestamp axis proves nothing"
    );
    assert_eq!(
        timestamps,
        vec![timestamps[0]; 4],
        "the device saw a different clock from the two roles"
    );
}

#[test]
fn every_phase_p_lane_refuses_with_zero_partial_effects() {
    // One case per refusal lane. Each starts from a state that would otherwise SERVE -- the warm
    // fixture above -- and breaks exactly one premise, so a lane that stopped firing would show up
    // as a SERVED port rather than as a silently different refusal (`fixtures-that-cannot-fail`).
    #[allow(clippy::type_complexity)]
    let lanes: Vec<(&str, u16, u32, Box<dyn Fn(&mut CpuGsw, &mut TestBus)>)> = vec![
        (
            // P1: the TSS cannot even hold the io_base word.
            "P1 limit below 0x67",
            TSS_IO_MAP_OFFSET,
            0x1000,
            Box::new(|cpu: &mut CpuGsw, _: &mut TestBus| cpu.tr.limit = 0x66),
        ),
        (
            // P2: the word straddles a page, so one translate cannot cover it.
            "P2 io_base word straddles a page",
            TSS_IO_MAP_OFFSET,
            0x1000,
            Box::new(|cpu: &mut CpuGsw, _: &mut TestBus| cpu.tr.base = 0x2f99),
        ),
        (
            // P3: the io_base page is not resident. This is the lane that stands in for the page
            // walk the interpreter would take.
            "P3 io_base page TLB miss",
            TSS_IO_MAP_OFFSET,
            0x1000,
            Box::new(|cpu: &mut CpuGsw, _: &mut TestBus| cpu.tlb.invalidate(IO_BASE_PAGE)),
        ),
        (
            // P4a: MISALIGNED io_base word. `should_split` rejects it, so the peek declines --
            // the kill condition the design's section 6.0 falsification was written against.
            "P4 misaligned io_base word",
            TSS_IO_MAP_OFFSET,
            0x1000,
            Box::new(|cpu: &mut CpuGsw, _: &mut TestBus| cpu.tr.base = TSS_BASE + 1),
        ),
        (
            // P4b: the page is readable but not plain direct RAM.
            "P4 io_base page is not direct RAM",
            TSS_IO_MAP_OFFSET,
            0x1000,
            Box::new(|_: &mut CpuGsw, bus: &mut TestBus| {
                bus.non_direct_read_pages.push(IO_BASE_PAGE)
            }),
        ),
        (
            // P5: the bitmap byte is past the limit -- the interpreter's first `#GP(0)`.
            "P5 bitmap byte past the TSS limit",
            TSS_IO_MAP_OFFSET,
            0x1000,
            Box::new(|cpu: &mut CpuGsw, _: &mut TestBus| cpu.tr.limit = 0x100),
        ),
        (
            // P7: the BITMAP page is not resident while the io_base page is. Needs the split-page
            // TSS, or one TLB entry covers both and the lane is unreachable.
            "P7 bitmap page TLB miss",
            TSS_SPLIT_PAGE_IO_MAP_OFFSET,
            0x2000,
            Box::new(|cpu: &mut CpuGsw, _: &mut TestBus| cpu.tlb.invalidate(SPLIT_BITMAP_PAGE)),
        ),
        (
            // P8: the bitmap page is readable but not plain direct RAM.
            "P8 bitmap page is not direct RAM",
            TSS_SPLIT_PAGE_IO_MAP_OFFSET,
            0x2000,
            Box::new(|_: &mut CpuGsw, bus: &mut TestBus| {
                bus.non_direct_read_pages.push(SPLIT_BITMAP_PAGE)
            }),
        ),
        (
            // P9: the guest TRAPS this port -- the interpreter's other `#GP(0)`. The byte is
            // flipped after warming so the warm pass itself stays clean.
            "P9 bitmap bit set",
            TSS_IO_MAP_OFFSET,
            0x1000,
            Box::new(|_: &mut CpuGsw, bus: &mut TestBus| {
                let at = TSS_BASE as usize + usize::from(TSS_IO_MAP_OFFSET) + usize::from(PORT / 8);
                bus.memory[at] = 1 << (PORT % 8);
            }),
        ),
    ];

    for (lane, io_map_offset, limit, break_it) in lanes {
        // The control: unbroken, this exact fixture SERVES. Without it a lane could pass because
        // the fixture never worked, which is the whole point of proving a guard fires.
        let (mut control, mut control_bus) = warmed_tss_cpu(io_map_offset, limit);
        assert!(
            jit::direct::port_read_al_dx_for_test(&mut control, &mut control_bus, 0, 0, 0) >= 0,
            "{lane}: the UNBROKEN fixture must serve, or this lane proves nothing"
        );

        let (mut cpu, mut bus) = warmed_tss_cpu(io_map_offset, limit);
        break_it(&mut cpu, &mut bus);
        let before = cpu.registers.clone();
        let before_cr2 = cpu.control.cr2;

        let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, 0, 0, 0);

        assert_refused_with_zero_partial_effects(lane, status, &cpu, &bus, &before, before_cr2);
        assert_eq!(
            cpu.direct_stall_snapshot().callout_port_v86_served,
            0,
            "{lane}: a refusal was counted as a serve"
        );
    }
}

#[test]
fn the_interpreter_raises_the_gp_the_denied_lanes_refused() {
    // What stops the two `#GP` lanes above from being vacuous: from the SAME state the
    // interpreter really does fault, so refusing costs the guest nothing but the call-out and the
    // architectural answer is unchanged.
    for (lane, limit, deny) in [("P5 limit", 0x100u32, false), ("P9 bit", 0x1000, true)] {
        let (mut cpu, mut bus) = paged_ring3_io_cpu_with(deny, TSS_IO_MAP_OFFSET, limit);
        let entry = 0x4000u32;
        bus.memory[entry as usize] = 0xec;
        bus.memory[entry as usize + 1] = 0xf4;
        cpu.set_eip(entry);

        // Not unwrapped: the #GP has no IDT to land in and nests. The subject is that the
        // instruction did NOT retire and AL did not move.
        let _ = cpu.cycle(&mut bus);
        assert_eq!(
            cpu.registers.eax(),
            0xdead_beef,
            "{lane}: the interpreter served a port the helper refused as denied"
        );
    }
}

#[test]
fn a_word_size_in_al_dx_joins_the_block_instead_of_ending_it() {
    // G1, the classifier half of the slice, and it must move in the same commit as the helper
    // arm: on its own it buys a spill, a call, a reload and a side exit where a free barrier used
    // to be (classify.rs records the measurement).
    //
    // `66 EC` mid-block, never at entry -- an opcode at a block's entry slot parks the block on
    // the interpreter and certifies nothing.
    let mut code = vec![0x89, 0xf6];
    code.extend_from_slice(&[0x66, 0xec]);
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);

    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    cpu.registers.set_esp(STACK_TOP);
    for offset in [0u32, 2, 4] {
        let linear = ENTRY + offset;
        cpu.set_eip(linear);
        cpu.fetch_decoded(&mut bus, linear).unwrap();
    }
    let compilation = match jit::direct::compile(&mut cpu, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        _ => panic!("a Word-size IN AL,DX is still a barrier -- the allowlist entry is missing"),
    };
    assert_eq!(
        compilation.span.instructions, 3,
        "the block must extend THROUGH the 66-prefixed call-out"
    );
    assert_eq!(compilation.callout_slots, 1);
}

#[test]
fn a_call_out_block_is_not_entered_at_all_in_the_privileged_port_state() {
    // The DISPATCH gate, and the one that decides cost. The helper's refusal is correct but it is
    // reached only after the whole-set spill, the scratch frame, the indirect call and the
    // whole-set reload -- and then the run still ends and the interpreter still executes the
    // instruction. Paying all of that to arrive where a barrier would have arrived for free is
    // strictly worse than the pre-slice behaviour, on every execution, for a paged V86 or
    // CPL>IOPL guest. `run_direct_block` therefore refuses to ENTER such a block, which returns
    // the block to the interpreter: pre-slice behaviour exactly.
    //
    // `NotRun`, not an abnormal exit: nothing native runs, no call-out is executed, no side exit
    // is recorded.
    //
    // GOVERNED as of round 2, which moves the boundary by exactly ONE entry: the first entry is
    // the governor's trial, and it is what LEARNS that this port is denied. From the second entry
    // on the gate is the pre-governor refusal, and that is the steady state this pins.
    let (mut fixture, _, _) = slot_block_with(
        |bus| {
            bus.lazy_io_reads = true;
            bus.io_read_value = Some(0x5a);
        },
        |cpu| {
            // CPL 3 with IOPL 0 and a zero-limit TSS: the state whose port reads would consult
            // the bitmap.
            cpu.cpl = 3;
            cpu.tr.limit = 0;
        },
    );
    // Spend the trial. `an_abnormal_trial_denies_the_block` is what asserts its outcome.
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    rearm_governor_block(&mut fixture);

    let retired = fixture.cpu.perf_counters().jit_direct_insns;
    let executed = fixture.cpu.direct_stall_snapshot().callout_executed;
    for entry in 1..4 {
        rearm_governor_block(&mut fixture);
        assert!(
            !fixture
                .cpu
                .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
                .unwrap(),
            "entry {entry}: the block must not be entered at all"
        );
        assert_eq!(
            fixture.cpu.perf_counters().jit_direct_insns - retired,
            0,
            "entry {entry}: nothing may retire natively"
        );
        assert_eq!(
            fixture.cpu.registers.eax(),
            0xdead_beef,
            "entry {entry}: AL untouched"
        );
        assert_eq!(fixture.cpu.elapsed_clocks, 0, "entry {entry}");
        let stalls = fixture.cpu.direct_stall_snapshot();
        assert_eq!(stalls.reject_callout_privileged, entry, "entry {entry}");
        assert_eq!(
            stalls.callout_executed, executed,
            "entry {entry}: the helper must never be reached, so nothing pays for the spill, \
             the call and the reload"
        );
    }
}

#[test]
fn a_call_out_block_runs_again_once_the_privilege_state_clears() {
    // The dispatch gate is a TRANSIENT refusal, not a retirement: a V86 task returns to ring 0
    // and back, and the block is perfectly good -- it is the privilege level that is wrong. Pinned
    // because retiring here would recompile the block on every privilege transition.
    let (mut fixture, _, _) = slot_block_with(
        |bus| {
            bus.lazy_io_reads = true;
            bus.io_read_value = Some(0x5a);
        },
        |cpu| {
            cpu.cpl = 3;
            cpu.tr.limit = 0;
        },
    );
    // The trial, then the governed refusal it earns.
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    rearm_governor_block(&mut fixture);
    assert!(
        !fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );

    // IOPL 3 lets a CPL-3 task reach ports without the bitmap, which is the interpreter's own
    // early-return condition -- and it is checked BEFORE the governor, so a `Denied` class does
    // not follow the block into a privilege state the gate does not govern.
    rearm_governor_block(&mut fixture);
    fixture.cpu.registers.eflags |= 3 << 12;
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap(),
        "the same block must run once the privilege state permits it"
    );
    let stalls = fixture.cpu.direct_stall_snapshot();
    assert_eq!(stalls.reject_callout_privileged, 1);
    assert_eq!(
        stalls.callout_executed, 2,
        "the trial's refused call-out and the permitted one"
    );
    assert_eq!(fixture.cpu.registers.eax() & 0xff, 0x5a);
}

#[test]
fn every_call_out_is_counted_whichever_arm_it_takes() {
    // The denominator, pinned so a zero abnormal count on a fixture is evidence rather than an
    // absence of evidence.
    let (mut fixture, _, _) = slot_block(|bus| {
        bus.lazy_io_reads = true;
        bus.io_read_value = Some(0x5a);
    });
    fixture
        .cpu
        .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
        .unwrap();
    let stalls = fixture.cpu.direct_stall_snapshot();
    assert_eq!(stalls.callout_executed, 1);
    assert_eq!(stalls.side_exit_callout_abnormal, 0);
    assert_eq!(stalls.side_exit_callout_step_break, 0);
}

#[test]
fn the_helper_folds_the_chains_float_clocks_into_the_device_timestamp() {
    // A call-out block never holds an x87 slot, but a FLOAT-ENTERED CHAIN can hop into one, and
    // those earlier hops deposited their cost in the weighted-FP lane rather than the raw one.
    // Previewing only the raw lane hands the device a timestamp short by the whole float part of
    // the chain. The mutation record for this is the `fp.clocks` term deleted: this test then
    // reads back the raw-only timestamp.
    let mut cpu = flat_cpu();
    cpu.registers.set_edx(u32::from(PORT));
    cpu.core_clocks_so_far = 100;
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
    bus.lazy_io_reads = true;

    let prefix_raw = 24u64;
    let prefix_fp = 4096u64;
    let (expected, raw_only) = {
        let mut probe = flat_cpu();
        let fp = jit::native_x87::scale_weighted_fp_clocks(prefix_fp, probe.fp_rem);
        assert!(fp.clocks > 0, "the fixture must carry real float clocks");
        let mut raw_probe = flat_cpu();
        (
            probe.scale_clocks_batch(prefix_raw + fp.clocks),
            raw_probe.scale_clocks_batch(prefix_raw),
        )
    };
    assert_ne!(expected, raw_only, "the two readings must be separable");

    let status =
        jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, prefix_raw, prefix_fp, 0);

    assert!(status >= 0);
    assert_eq!(bus.last_read_io_core_clocks_so_far, Some(100 + expected));
    // Still a preview: neither carry may be consumed, because the block still owes both charges.
    assert_eq!(
        cpu.timing_rem, 0,
        "preview scaling consumed the integer carry"
    );
    assert_eq!(cpu.fp_rem, 0, "preview scaling consumed the float carry");
}

// ---------------------------------------------------------------------------------------------
// The MEMORY class helpers, driven directly.
//
// The emitted-slot cover for these lives in the differential matrix; this section is about the
// HELPER's own contract -- the charge, and the fail-closed pre-check clause by clause. Every
// refusal row asserts zero partial effects on all four channels the helper could touch: the
// register file, guest RAM, the bus trace and the clock counter.
// ---------------------------------------------------------------------------------------------

/// Eight distinct, high-bit-bearing register values. Distinct so a PUSHAD that pushed them in the
/// wrong ORDER, or a POPAD that loaded them into the wrong destinations, is visible; a fixture of
/// zeroes would pass against both.
const FRAME_SEED: [u32; 8] = [
    0x1111_0001,
    0x2222_0002,
    0x3333_0003,
    0x4444_0004,
    0x5555_0005,
    0x6666_0006,
    0x7777_0007,
    0x8888_0008,
];

/// A CPU whose stack frame is RESIDENT in the FastMap, which is what
/// `call_out_stack_frame_resident` requires before either memory helper will move anything.
///
/// Residency is established through real guest accesses rather than by populating the map
/// directly, because `lookup_access` compares the entry's mapping epoch against
/// `data_write_pages.mapping_epoch()` -- an entry installed behind the DirectPageCache's back is
/// not servable, and the fixture would then measure the refusal instead of the mechanism. This is
/// also exactly how the map is populated in production: the interpreter touches the stack once and
/// the next call-out finds it there.
fn resident_stack_cpu() -> (CpuGsw, TestBus) {
    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    cpu.set_fast_map_enabled_for_test(true);
    cpu.registers.gpr = FRAME_SEED;
    cpu.registers.set_esp(STACK_TOP);
    for page in [(STACK_TOP - 0x1000) & !0xfff, STACK_TOP & !0xfff] {
        let value = cpu
            .read_memory_bus_width(
                &mut bus,
                SegmentIndex::Ss,
                page,
                BusWidth::Dword,
                BusAccessKind::DataRead,
            )
            .expect("fixture warm read");
        cpu.write_memory_bus_width(
            &mut bus,
            SegmentIndex::Ss,
            page,
            BusWidth::Dword,
            value,
            BusAccessKind::DataWrite,
        )
        .expect("fixture warm write");
    }
    bus.trace = BusTrace::default();
    cpu.elapsed_clocks = 0;
    (cpu, bus)
}

/// Assert the helper touched NOTHING, on every channel it could have.
fn assert_no_partial_effects(
    cpu: &CpuGsw,
    bus: &TestBus,
    before: &(Registers, izarravm_bus::PageAlignedBytes, u8),
    context: &str,
) {
    assert_eq!(cpu.registers, before.0, "{context}: registers");
    assert_eq!(bus.memory, before.1, "{context}: guest RAM");
    assert_eq!(bus.trace.elapsed_clocks(), 0, "{context}: bus clocks");
    assert_eq!(cpu.elapsed_clocks, 0, "{context}: core clocks");
    // Snapshotted rather than asserted at zero: `resident_stack_cpu` warms the map with real guest
    // writes, which legitimately record pages. What must not move is the count ACROSS the helper.
    assert_eq!(cpu.written_count, before.2, "{context}: written pages");
}

#[test]
fn the_pushad_helper_moves_exactly_what_the_interpreters_own_pushad_moves() {
    // The oracle is `push_all_gpr` itself -- the SAME function the interpreter's 0x60 arm calls,
    // driven on a twin from the same state. That is the point of factoring it out of the opcode
    // arm: the claim "the call-out does what the interpreter does" is a claim about one body with
    // two callers, not about two implementations agreeing.
    let (mut cpu, mut bus) = resident_stack_cpu();
    let (mut twin, mut twin_bus) = resident_stack_cpu();

    let status = jit::direct::push_all_dword_for_test(&mut cpu, &mut bus);
    twin.push_all_gpr(&mut twin_bus, OperandSize::Dword)
        .expect("the twin's PUSHAD must succeed");

    assert!(status >= 0, "a resident frame must not be abnormal");
    assert_eq!(
        status & 0xffff_ffff,
        i64::from(PUSH_ALL_CORE_CLOCKS),
        "the helper must return the interpreter's own raw charge"
    );
    assert_eq!(cpu.registers, twin.registers, "registers");
    assert_eq!(bus.memory, twin_bus.memory, "guest RAM");
    assert_eq!(
        bus.trace.elapsed_clocks(),
        twin_bus.trace.elapsed_clocks(),
        "bus clocks"
    );
    // The helper charges nothing itself; the caller folds the returned clocks into the block's
    // runtime lane and `run_direct_block` scales the whole lane once.
    assert_eq!(cpu.elapsed_clocks, 0, "the helper charged clocks");
    assert_eq!(
        cpu.registers.esp(),
        STACK_TOP - 32,
        "ESP must have moved by the whole frame"
    );
}

#[test]
fn the_popad_helper_loads_exactly_what_the_interpreters_own_popad_loads() {
    // The register-file-mutating helper, against the same one-body oracle. Eight distinct values
    // in the frame, so a helper that loaded them in the wrong order matches on ESP and fails on
    // the register compare.
    let frame: [u32; 8] = [
        0xaaaa_0007,
        0xbbbb_0006,
        0xcccc_0005,
        0xdddd_0004,
        0xeeee_0003,
        0xffff_0002,
        0x9999_0001,
        0x8888_0000,
    ];
    let (mut cpu, mut bus) = resident_stack_cpu();
    let (mut twin, mut twin_bus) = resident_stack_cpu();
    let base = STACK_TOP - 32;
    for (target_bus, target_cpu) in [(&mut bus, &mut cpu), (&mut twin_bus, &mut twin)] {
        for (index, value) in frame.iter().enumerate() {
            let at = base as usize + index * 4;
            target_bus.memory[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        target_cpu.registers.set_esp(base);
    }

    let status = jit::direct::pop_all_dword_for_test(&mut cpu, &mut bus);
    twin.pop_all_gpr(&mut twin_bus, OperandSize::Dword)
        .expect("the twin's POPAD must succeed");

    assert!(status >= 0, "a resident frame must not be abnormal");
    assert_eq!(
        status & 0xffff_ffff,
        i64::from(POP_ALL_CORE_CLOCKS),
        "the helper must return the interpreter's own raw charge"
    );
    assert_eq!(cpu.registers, twin.registers, "registers");
    assert_eq!(cpu.registers.eax(), frame[7], "EAX comes from the top slot");
    assert_eq!(cpu.registers.edi(), frame[0], "EDI comes from the bottom");
    assert_eq!(
        cpu.registers.esp(),
        STACK_TOP,
        "ESP advances over the whole frame, discarded slot included"
    );
    assert_eq!(
        bus.trace.elapsed_clocks(),
        twin_bus.trace.elapsed_clocks(),
        "bus clocks"
    );
    assert_eq!(cpu.elapsed_clocks, 0, "the helper charged clocks");
}

#[test]
fn a_pushad_frame_that_is_not_resident_is_refused_with_zero_partial_effects() {
    // The residency clause, and the state EVERY PUSHAD is in the first time its stack page is
    // touched: no warm-up, so `lookup_access` misses and the whole frame is refused. Fail-closed
    // costs the guest one call-out; the interpreter then executes the instruction whole.
    let mut cpu = flat_cpu();
    cpu.registers.gpr = FRAME_SEED;
    cpu.registers.set_esp(STACK_TOP);
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
    bus.direct_pages_enabled = true;
    let before = (cpu.registers.clone(), bus.memory.clone(), cpu.written_count);

    let status = jit::direct::push_all_dword_for_test(&mut cpu, &mut bus);

    assert!(status < 0, "a non-resident frame must be refused");
    assert_no_partial_effects(&cpu, &bus, &before, "non-resident");
}

#[test]
fn a_pushad_frame_that_hits_watched_code_is_refused_with_zero_partial_effects() {
    // THE hazard. A push landing on watched code would reach `note_code_write_hit` with a compiled
    // block live on the stack, which is the situation `note_code_write_inner`'s proof rules out.
    // Here the watch is a DECODED LINE rather than a compiled block -- `code_write_watched` is the
    // disjunction of the two, so either establishes it, and a decode line needs no block cache.
    let (mut cpu, mut bus) = resident_stack_cpu();
    // Decode an instruction inside the frame the PUSHAD is about to write, so
    // `decode_cache.range_hits_code` reports the range as watched.
    let target = STACK_TOP - 16;
    bus.memory[target as usize] = 0x90; // NOP
    cpu.set_eip(target);
    cpu.fetch_decoded(&mut bus, target).unwrap();
    cpu.set_eip(ENTRY);
    bus.trace = BusTrace::default();
    cpu.elapsed_clocks = 0;
    let written_before = cpu.written_count;
    let before = (cpu.registers.clone(), bus.memory.clone(), cpu.written_count);

    let status = jit::direct::push_all_dword_for_test(&mut cpu, &mut bus);

    assert!(status < 0, "a watched frame must be refused");
    assert_eq!(cpu.registers, before.0, "watched: registers");
    assert_eq!(bus.memory, before.1, "watched: guest RAM");
    assert_eq!(bus.trace.elapsed_clocks(), 0, "watched: bus clocks");
    assert_eq!(cpu.elapsed_clocks, 0, "watched: core clocks");
    assert_eq!(
        cpu.written_count, written_before,
        "watched: written-page bookkeeping"
    );
}

#[cfg(feature = "direct-callout-attribution")]
fn callout_helper_counts(
    snapshot: &DirectCallOutAttributionSnapshot,
    helper: &str,
) -> DirectCallOutOutcomeCounts {
    snapshot
        .helpers
        .iter()
        .find(|row| row.helper == helper)
        .unwrap()
        .counts
}

#[cfg(feature = "direct-callout-attribution")]
#[test]
fn callout_attribution_splits_every_port_outcome() {
    for (lazy, fails, expected) in [
        (
            true,
            false,
            DirectCallOutOutcomeCounts {
                attempts: 1,
                continued: 1,
                step_break: 0,
                abnormal: 0,
            },
        ),
        (
            false,
            false,
            DirectCallOutOutcomeCounts {
                attempts: 1,
                continued: 0,
                step_break: 1,
                abnormal: 0,
            },
        ),
        (
            true,
            true,
            DirectCallOutOutcomeCounts {
                attempts: 1,
                continued: 0,
                step_break: 0,
                abnormal: 1,
            },
        ),
    ] {
        let (mut fixture, _, _) = slot_block(|bus| {
            bus.lazy_io_reads = lazy;
            bus.io_read_fails = fails;
            bus.io_read_value = Some(0x5a);
        });
        fixture.cpu.enable_direct_callout_attribution_for_test();
        assert!(
            fixture
                .cpu
                .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
                .unwrap()
        );

        let snapshot = fixture.cpu.direct_callout_attribution_snapshot().unwrap();
        // SEVEN helper rows since the `0xEE` slice gave `CallOutHelper` its `PortWriteAlDx` arm
        // (six since the gp2 `0xE6` slice's `PortWriteAlImm8`, five since the in-imm8 design's
        // `PortReadAlImm8`, four since S2's `InterpretOne`). The count moved once before when
        // this file's `--all-features` build was repaired: the attribution matched three arms
        // against a four-arm enum and had not compiled since, so the pin had been asserting a
        // shape no build could reach.
        assert_eq!(snapshot.helpers.len(), 7);
        assert_eq!(callout_helper_counts(&snapshot, "in_al_dx"), expected);
        assert_eq!(callout_helper_counts(&snapshot, "pushad").attempts, 0);
        assert_eq!(callout_helper_counts(&snapshot, "popad").attempts, 0);
        // The fixture drives the port helper alone, so the new row is present and silent -- which
        // is the claim worth making: one index for the whole family, and it is not absorbing the
        // port's calls.
        assert_eq!(
            callout_helper_counts(&snapshot, "interpret_one").attempts,
            0
        );
        // The gp2 in-imm8 callout design's row: present and silent, for the same reason
        // `interpret_one`'s is -- this fixture drives `0xEC` alone.
        assert_eq!(callout_helper_counts(&snapshot, "in_al_imm8").attempts, 0);
        assert_eq!(snapshot.ports.len(), 1);
        assert_eq!(snapshot.ports[0].port, PORT);
        assert_eq!(snapshot.ports[0].counts, expected);
        assert_eq!(snapshot.totals, expected);
    }
}

#[cfg(feature = "direct-callout-attribution")]
#[test]
fn callout_attribution_orders_ports_and_survives_unrelated_resets() {
    let (mut cpu, mut bus) = resident_stack_cpu();
    bus.lazy_io_reads = true;
    bus.io_read_value = Some(0x5a);
    cpu.enable_direct_callout_attribution_for_test();

    for port in [0x03dau16, 0x0201] {
        cpu.registers.set_edx(u32::from(port));
        assert!(jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, 0, 0, 0) >= 0);
    }
    assert!(jit::direct::push_all_dword_for_test(&mut cpu, &mut bus) >= 0);
    assert!(jit::direct::pop_all_dword_for_test(&mut cpu, &mut bus) >= 0);

    let expected = cpu.direct_callout_attribution_snapshot().unwrap();
    assert_eq!(expected.ports.len(), 2);
    assert_eq!(expected.ports[0].port, 0x0201);
    assert_eq!(expected.ports[1].port, 0x03da);
    assert_eq!(expected.totals.attempts, 4);
    assert_eq!(expected.totals.continued, 4);
    assert_eq!(callout_helper_counts(&expected, "in_al_dx").attempts, 2);
    assert_eq!(callout_helper_counts(&expected, "pushad").continued, 1);
    assert_eq!(callout_helper_counts(&expected, "popad").continued, 1);

    cpu.reset_perf_counters();
    assert_eq!(
        cpu.direct_callout_attribution_snapshot(),
        Some(expected.clone())
    );
    cpu.jit_direct.clear();
    assert_eq!(cpu.direct_callout_attribution_snapshot(), Some(expected));
    assert!(cpu.clone().direct_callout_attribution_snapshot().is_none());

    let env_armed = std::env::var("IZARRAVM_DIRECT_CALLOUT_ATTRIBUTION").as_deref() == Ok("1");
    cpu.reset();
    let fresh = cpu.direct_callout_attribution_snapshot();
    assert_eq!(fresh.is_some(), env_armed);
    if let Some(fresh) = fresh {
        assert_eq!(fresh.totals, DirectCallOutOutcomeCounts::default());
        assert!(fresh.ports.is_empty());
    }
}

#[cfg(feature = "direct-callout-attribution")]
#[test]
fn callout_attribution_closes_the_port_table_over_the_write_helper_too() {
    // THE REGRESSION. `CallOutAttribution::note` routes all THREE port helpers -- `0xEC`,
    // `0xE4` and (since the gp2 `0xE6` slice) `0xE6` -- into the one shared per-port table,
    // but `snapshot`'s closure assertion summed only the two READ helper rows on the right
    // hand side. Any run that fired an `OUT imm8, AL` call-out with the instrument armed
    // therefore panicked at teardown with "port call-out ports did not close", the port
    // total exceeding the read rows by exactly the OUT traffic. The instrument was doing its
    // job; the identity it checked was the wrong one.
    //
    // Plain and census builds were never affected: the whole table, the note and the
    // assertion are `direct-callout-attribution`-gated and inert without it.
    let (mut cpu, mut bus) = resident_stack_cpu();
    bus.lazy_io_reads = true;
    bus.io_read_value = Some(0x5a);
    cpu.enable_direct_callout_attribution_for_test();

    // One of each port helper, on three distinct ports so the table rows stay separable.
    cpu.registers.set_edx(0x03da);
    assert!(jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, 0, 0, 0) >= 0);
    assert!(jit::direct::port_read_al_imm8_for_test(&mut cpu, &mut bus, 0, 0, 0x0061) >= 0);
    assert!(jit::direct::port_write_al_imm8_for_test(&mut cpu, &mut bus, 0, 0, 0x0043) >= 0);
    // `OUT` is an unconditional step break, and the direct helper seam bypasses the emitted
    // side-exit stub that would note it, so mirror that one outer note the way
    // `callout_attribution_counts_each_memory_helper_refusal` mirrors the abnormal one.
    cpu.jit_direct.note_side_exit_callout_step_break();

    // The snapshot itself asserts the closure; reaching this line at all is the regression
    // test. The explicit identity below states what has to hold rather than leaving it to a
    // panic message.
    let snapshot = cpu.direct_callout_attribution_snapshot().unwrap();
    let port_total =
        snapshot
            .ports
            .iter()
            .fold(DirectCallOutOutcomeCounts::default(), |sum, row| {
                DirectCallOutOutcomeCounts {
                    attempts: sum.attempts + row.counts.attempts,
                    continued: sum.continued + row.counts.continued,
                    step_break: sum.step_break + row.counts.step_break,
                    abnormal: sum.abnormal + row.counts.abnormal,
                }
            });
    assert_eq!(port_total.attempts, 3, "one attempt per port helper call");
    assert_eq!(
        port_total.attempts,
        callout_helper_counts(&snapshot, "in_al_dx").attempts
            + callout_helper_counts(&snapshot, "in_al_imm8").attempts
            + callout_helper_counts(&snapshot, "out_al_imm8").attempts,
        "the per-port table must close over ALL THREE port helpers"
    );
    assert_eq!(
        callout_helper_counts(&snapshot, "out_al_imm8").attempts,
        1,
        "the write helper's own row must count its call"
    );
    assert_eq!(
        snapshot
            .ports
            .iter()
            .find(|row| row.port == 0x0043)
            .map(|row| row.counts.attempts),
        Some(1),
        "the write call-out's port must appear in the per-port table"
    );
}

#[cfg(feature = "direct-callout-attribution")]
#[test]
fn callout_attribution_counts_each_memory_helper_refusal() {
    let mut push_cpu = flat_cpu();
    push_cpu.enable_direct_callout_attribution_for_test();
    push_cpu.registers.set_esp(STACK_TOP);
    let mut push_bus = TestBus::with_memory(vec![0u8; 0x5000]);
    push_bus.direct_pages_enabled = true;
    assert!(jit::direct::push_all_dword_for_test(&mut push_cpu, &mut push_bus) < 0);
    // The direct helper seam bypasses the emitted side-exit stub, so mirror that one outer note.
    push_cpu.jit_direct.note_side_exit_callout_abnormal();
    let push = push_cpu.direct_callout_attribution_snapshot().unwrap();
    assert_eq!(callout_helper_counts(&push, "pushad").abnormal, 1);

    let mut pop_cpu = flat_cpu();
    pop_cpu.enable_direct_callout_attribution_for_test();
    pop_cpu.registers.set_esp(STACK_TOP - 32);
    let mut pop_bus = TestBus::with_memory(vec![0u8; 0x5000]);
    pop_bus.direct_pages_enabled = true;
    assert!(jit::direct::pop_all_dword_for_test(&mut pop_cpu, &mut pop_bus) < 0);
    pop_cpu.jit_direct.note_side_exit_callout_abnormal();
    let pop = pop_cpu.direct_callout_attribution_snapshot().unwrap();
    assert_eq!(callout_helper_counts(&pop, "popad").abnormal, 1);
}

#[test]
fn a_pushad_that_would_read_the_frame_is_not_refused_by_the_code_watch() {
    // What stops the row above from being vacuous in the wrong direction: the code-watch clause is
    // asked only for a WRITE. POPAD reads the same watched bytes and must be accepted, because a
    // read cannot reach `note_code_write` at all.
    let (mut cpu, mut bus) = resident_stack_cpu();
    let target = STACK_TOP - 16;
    bus.memory[target as usize] = 0x90;
    cpu.set_eip(target);
    cpu.fetch_decoded(&mut bus, target).unwrap();
    // The decode above sticky-marks the stack page as watched, whose E1 sweep invalidates the
    // fast-map entries `resident_stack_cpu` just populated (their PAGE_WATCHED bit was clear).
    // Re-touch the same pages the same way `resident_stack_cpu` did, so the fast map is
    // repopulated AFTER the mark and its entries carry bit = 1, matching production ordering.
    for page in [(STACK_TOP - 0x1000) & !0xfff, STACK_TOP & !0xfff] {
        let value = cpu
            .read_memory_bus_width(
                &mut bus,
                SegmentIndex::Ss,
                page,
                BusWidth::Dword,
                BusAccessKind::DataRead,
            )
            .expect("fixture re-warm read");
        cpu.write_memory_bus_width(
            &mut bus,
            SegmentIndex::Ss,
            page,
            BusWidth::Dword,
            value,
            BusAccessKind::DataWrite,
        )
        .expect("fixture re-warm write");
    }
    cpu.set_eip(ENTRY);
    cpu.registers.set_esp(STACK_TOP - 32);
    bus.trace = BusTrace::default();
    cpu.elapsed_clocks = 0;

    let status = jit::direct::pop_all_dword_for_test(&mut cpu, &mut bus);

    assert!(status >= 0, "a READ of watched bytes must be accepted");
    assert_eq!(cpu.registers.esp(), STACK_TOP);
}

#[test]
fn a_sixteen_bit_stack_pushad_is_refused_with_zero_partial_effects() {
    // SS.B = 0 addresses through SP alone and POPAD then merges the discarded slot's high half
    // into ESP. `push_all_gpr` handles both, but the pre-check's address arithmetic would have to
    // fork to match, so the population is refused rather than mirrored.
    let (mut cpu, mut bus) = resident_stack_cpu();
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.default_size_32 = false;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    assert!(
        !cpu.stack_is_32bit(),
        "the fixture must have a 16-bit stack"
    );
    let before = (cpu.registers.clone(), bus.memory.clone(), cpu.written_count);

    let status = jit::direct::push_all_dword_for_test(&mut cpu, &mut bus);

    assert!(status < 0, "a 16-bit stack must be refused");
    assert_no_partial_effects(&cpu, &bus, &before, "16-bit stack");
}

#[test]
fn an_unaligned_stack_pointer_pushad_is_refused_with_zero_partial_effects() {
    // Four-byte alignment is what makes every slot page-local, servable by `lookup_access`, and
    // safe from `check_alignment`'s CPL-3 `#AC`. One clause, three hazards.
    let (mut cpu, mut bus) = resident_stack_cpu();
    cpu.registers.set_esp(STACK_TOP - 2);
    let before = (cpu.registers.clone(), bus.memory.clone(), cpu.written_count);

    let status = jit::direct::push_all_dword_for_test(&mut cpu, &mut bus);

    assert!(status < 0, "an unaligned ESP must be refused");
    assert_no_partial_effects(&cpu, &bus, &before, "unaligned ESP");
}

#[test]
fn a_stack_limit_violation_pushad_is_refused_with_zero_partial_effects() {
    // The SS limit, checked per slot with the SAME call `push` will make. The interpreter would
    // discover this by FAULTING part-way with sub-pushes already committed; the pre-check finds it
    // before anything moves, which is the whole reason the pre-check exists.
    let (mut cpu, mut bus) = resident_stack_cpu();
    let mut ss = cpu.registers.segment(SegmentIndex::Ss);
    ss.limit = STACK_TOP - 16;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    let before = (cpu.registers.clone(), bus.memory.clone(), cpu.written_count);

    let status = jit::direct::push_all_dword_for_test(&mut cpu, &mut bus);

    assert!(status < 0, "a frame past the SS limit must be refused");
    assert_no_partial_effects(&cpu, &bus, &before, "SS limit");
}

#[test]
fn an_esp_wrap_pushad_is_refused_with_zero_partial_effects() {
    // ESP below thirty-two sends the frame's lower slots to the far end of the address space by
    // `wrapping_sub`, exactly as `push` would. The pre-check does not special-case the wrap: it
    // evaluates every wrapped offset, and here the wrapped pages are not resident, so the frame is
    // refused whole rather than half written.
    let (mut cpu, mut bus) = resident_stack_cpu();
    cpu.registers.set_esp(16);
    let before = (cpu.registers.clone(), bus.memory.clone(), cpu.written_count);

    let status = jit::direct::push_all_dword_for_test(&mut cpu, &mut bus);

    assert!(status < 0, "a wrapping frame must be refused");
    assert_no_partial_effects(&cpu, &bus, &before, "ESP wrap");
}

// ---------------------------------------------------------------------------------------------
// The call-out admission governor (run.rs G2). Each transition of the state machine is driven
// through `run_direct_block` on a real compiled block, and each one is asserted to CHANGE what
// the next entry does -- a governor whose classifications never reached the gate would pass a
// test that only read the counters.
// ---------------------------------------------------------------------------------------------

/// A TSS placed above the fixture's stack, inside the fixture's zeroed RAM. `io_base` reads 0 and
/// every bitmap byte reads 0, so the bitmap PERMITS `PORT` -- which is what lets a CPL-3 / IOPL-0
/// block reach a real serve instead of the `#GP` the zero-limit TSS produces.
const GOVERNOR_TSS_BASE: u32 = 0x4800;
const GOVERNOR_TSS_LIMIT: u32 = 0x200;

/// `slot_block`'s block, generalised over its instruction list and compiled in the state G2
/// governs. `insns` is the exact set primed into the decode cache; a `HLT` is appended as the
/// terminator and left unprimed. `head_instructions` is how many of them the compile walk is
/// expected to take -- fewer than all of them when a terminal instruction ends the head block and
/// the rest are a successor the caller compiles separately.
fn governor_block(
    insns: &[&[u8]],
    head_instructions: usize,
    configure: impl Fn(&mut TestBus),
    configure_cpu: impl Fn(&mut CpuGsw),
) -> Fixture {
    let mut code = Vec::new();
    let mut starts = Vec::new();
    for insn in insns {
        starts.push(ENTRY + code.len() as u32);
        code.extend_from_slice(insn);
    }
    code.push(0xf4);

    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    configure(&mut bus);

    // Privilege state is sealed into the block at compile time (`memory_cpl3`), so it has to be
    // set before the compile and not merely before the run.
    cpu.cpl = 3;
    cpu.tr.base = GOVERNOR_TSS_BASE;
    cpu.tr.limit = GOVERNOR_TSS_LIMIT;
    configure_cpu(&mut cpu);
    cpu.registers.set_esp(STACK_TOP);
    for &linear in &starts {
        cpu.set_eip(linear);
        cpu.fetch_decoded(&mut bus, linear).unwrap();
    }

    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("entry key");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut cpu, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => panic!("structurally rejected"),
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        usize::from(compilation.span.instructions),
        head_instructions,
        "the head block must cover exactly the instructions it is supposed to"
    );
    assert_eq!(compilation.callout_slots, 1);
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = cpu.jit_direct.block(id).expect("live block");

    let mut fixture = Fixture { cpu, bus, block };
    rearm_governor_block(&mut fixture);
    fixture
}

/// Put the CPU back at the block's head with the same architectural state. The governor is about
/// what the SECOND and later entries do, so every one of these tests re-enters.
fn rearm_governor_block(fixture: &mut Fixture) {
    let cpu = &mut fixture.cpu;
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_esp(STACK_TOP);
    cpu.registers.set_edx(u32::from(PORT));
    cpu.registers.set_eax(0xdead_beef);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

/// The three-slot block of `slot_block`, in the governed state.
fn governed_slot_block(configure: impl Fn(&mut TestBus)) -> Fixture {
    governor_block(
        &[&[0x89, 0xf6], &[0xec], &[0x89, 0xff]],
        3,
        configure,
        |_| {},
    )
}

fn admission(fixture: &Fixture) -> jit::direct::CallOutAdmission {
    fixture
        .cpu
        .callout_admission_for_test(&fixture.bus, fixture.block.id())
}

#[test]
fn a_lazy_trial_promotes_the_block_and_every_later_entry_runs_it() {
    // THE TRANSITION THE SLICE EXISTS FOR. A serve that leaves the bus untouched lets the block
    // complete, so refusing it at head buys nothing and costs a dispatcher round trip per poll.
    let mut fixture = governed_slot_block(|bus| {
        bus.lazy_io_reads = true;
        bus.io_read_value = Some(0x5a);
    });
    assert_eq!(
        admission(&fixture),
        jit::direct::CallOutAdmission::Untried(0)
    );

    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap(),
        "an Untried block must be ADMITTED for its trial, not refused"
    );
    let stalls = fixture.cpu.direct_stall_snapshot();
    assert_eq!(stalls.callout_governor_trials, 1, "one trial entry");
    assert_eq!(stalls.callout_executed, 1, "the trial reached the helper");
    assert_eq!(
        stalls.callout_port_v86_served, 1,
        "the bitmap arm served it -- otherwise this is measuring the CPL0 arm"
    );
    assert_eq!(stalls.side_exit_callout_step_break, 0);
    assert_eq!(stalls.side_exit_callout_abnormal, 0);
    assert_eq!(stalls.callout_governor_lazy, 1);
    assert_eq!(stalls.reject_callout_privileged, 0);
    assert_eq!(admission(&fixture), jit::direct::CallOutAdmission::Lazy);
    assert_eq!(fixture.cpu.registers.eax() & 0xff, 0x5a);

    // The classification has to CHANGE the gate, not merely be stored: three more entries, none
    // of them a trial and none of them refused.
    for entry in 1..4 {
        rearm_governor_block(&mut fixture);
        assert!(
            fixture
                .cpu
                .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
                .unwrap(),
            "entry {entry}: a Lazy block must run"
        );
        let stalls = fixture.cpu.direct_stall_snapshot();
        assert_eq!(
            stalls.callout_governor_trials, 1,
            "entry {entry}: a classified block must never be re-tried"
        );
        assert_eq!(stalls.reject_callout_privileged, 0, "entry {entry}");
        assert_eq!(stalls.callout_executed, entry + 1, "entry {entry}");
    }
}

#[test]
fn an_io_touching_trial_leaves_the_block_refused_at_head() {
    // The other side of the same fixture, one bus knob apart: a serve that touches device state
    // step-breaks, so admitting the block buys a spill/call/reload/side-exit where a free barrier
    // used to be. Pre-governor behaviour is the right answer here, and the trial is what pays for
    // learning that.
    let mut fixture = governed_slot_block(|bus| {
        bus.lazy_io_reads = false;
        bus.io_read_value = Some(0x5a);
    });

    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap(),
        "the trial itself must run"
    );
    let stalls = fixture.cpu.direct_stall_snapshot();
    assert_eq!(stalls.callout_governor_trials, 1);
    assert_eq!(
        stalls.side_exit_callout_step_break, 1,
        "the serve step-broke"
    );
    assert_eq!(stalls.callout_governor_io_touching, 1);
    assert_eq!(stalls.callout_governor_lazy, 0);
    assert_eq!(
        admission(&fixture),
        jit::direct::CallOutAdmission::IoTouching
    );

    let retired = fixture.cpu.perf_counters().jit_direct_insns;
    for entry in 1..4 {
        rearm_governor_block(&mut fixture);
        assert!(
            !fixture
                .cpu
                .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
                .unwrap(),
            "entry {entry}: an IoTouching block must be refused at head"
        );
        let stalls = fixture.cpu.direct_stall_snapshot();
        assert_eq!(stalls.reject_callout_privileged, entry, "entry {entry}");
        assert_eq!(
            stalls.callout_executed, 1,
            "entry {entry}: nothing may pay for the spill, the call and the reload"
        );
        assert_eq!(stalls.callout_governor_trials, 1, "entry {entry}");
    }
    assert_eq!(
        fixture.cpu.perf_counters().jit_direct_insns - retired,
        0,
        "nothing may retire natively once the block is refused"
    );
}

#[test]
fn an_abnormal_trial_denies_the_block() {
    // A permanently trapped port: the zero-limit TSS denies every one, so the helper refuses and
    // the run ends at the instruction. The trial bounds that cost to once per epoch -- exactly
    // the residual Q3 item 4 records as an accepted price.
    let mut fixture = governor_block(
        &[&[0x89, 0xf6], &[0xec], &[0x89, 0xff]],
        3,
        |bus| {
            bus.lazy_io_reads = true;
            bus.io_read_value = Some(0x5a);
        },
        |cpu| cpu.tr.limit = 0,
    );

    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    let stalls = fixture.cpu.direct_stall_snapshot();
    assert_eq!(stalls.callout_governor_trials, 1);
    assert_eq!(stalls.side_exit_callout_abnormal, 1);
    assert_eq!(stalls.callout_governor_lazy, 0);
    assert_eq!(stalls.callout_governor_io_touching, 0);
    assert_eq!(admission(&fixture), jit::direct::CallOutAdmission::Denied);
    assert_eq!(
        fixture.cpu.registers.eax(),
        0xdead_beef,
        "a denied port must never reach AL"
    );

    rearm_governor_block(&mut fixture);
    assert!(
        !fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap(),
        "a Denied block must be refused at head from the second entry on"
    );
    let stalls = fixture.cpu.direct_stall_snapshot();
    assert_eq!(stalls.reject_callout_privileged, 1);
    assert_eq!(stalls.callout_executed, 1);
}

#[test]
fn a_trial_that_never_reaches_its_call_out_gives_up_after_the_cap() {
    // The trial cap, and the block it exists for: a call-out the run never reaches. `DIV ECX`
    // with ECX zero side-exits on its emitted guard, so the slot behind it never serves and the
    // trial learns nothing. Without the cap this block would sit at quota 1 forever, which is a
    // real regression for its OTHER instructions.
    let mut fixture = governor_block(
        &[&[0x89, 0xf6], &[0xf7, 0xf1], &[0xec], &[0x89, 0xff]],
        4,
        |bus| {
            bus.lazy_io_reads = true;
            bus.io_read_value = Some(0x5a);
        },
        |_| {},
    );

    for entry in 1..=u64::from(jit::direct::MAX_UNTRIED_TRIALS) {
        rearm_governor_block(&mut fixture);
        // EDX:EAX / 0 -- the guard refuses and the run ends AT the divide.
        fixture.cpu.registers.set_ecx(0);
        assert!(
            fixture
                .cpu
                .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
                .unwrap(),
            "entry {entry}: an Untried block is admitted for its trial"
        );
        let stalls = fixture.cpu.direct_stall_snapshot();
        assert_eq!(
            stalls.side_exit_divide_guard, entry,
            "entry {entry}: the guard is what must end this run"
        );
        assert_eq!(
            stalls.callout_executed, 0,
            "entry {entry}: the call-out must never be reached"
        );
        assert_eq!(stalls.callout_governor_trials, entry, "entry {entry}");
        let expected = if entry < u64::from(jit::direct::MAX_UNTRIED_TRIALS) {
            jit::direct::CallOutAdmission::Untried(entry as u8)
        } else {
            jit::direct::CallOutAdmission::Unclassified
        };
        assert_eq!(admission(&fixture), expected, "entry {entry}");
    }

    // Unclassified is today's behaviour exactly: refused at head, no further trials.
    for entry in 1..3 {
        rearm_governor_block(&mut fixture);
        fixture.cpu.registers.set_ecx(0);
        assert!(
            !fixture
                .cpu
                .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
                .unwrap(),
            "entry {entry}: an Unclassified block is refused at head"
        );
        let stalls = fixture.cpu.direct_stall_snapshot();
        assert_eq!(stalls.reject_callout_privileged, entry);
        assert_eq!(
            stalls.callout_governor_trials,
            u64::from(jit::direct::MAX_UNTRIED_TRIALS),
            "entry {entry}: the cap must stop the trials"
        );
    }
}

#[test]
fn a_recycled_block_slot_reads_untried_rather_than_its_predecessors_class() {
    // The storage discipline, copied from `iteration_upper_cache` rather than invented: the
    // protection against a recycled slot is `active_index` generational identity plus the
    // install-time reset, NOT a clear in `retire_block`. One invariant, one mechanism.
    let mut fixture = governed_slot_block(|bus| {
        bus.lazy_io_reads = true;
        bus.io_read_value = Some(0x5a);
    });
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    assert_eq!(admission(&fixture), jit::direct::CallOutAdmission::Lazy);
    let old_id = fixture.block.id();

    let key = jit::direct::key_for(&fixture.cpu, ENTRY, true).expect("entry key");
    assert!(fixture.cpu.jit_direct.retire_key_for_recompile(key));
    let compilation = match jit::direct::compile(&mut fixture.cpu, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        _ => panic!("the same bytes must recompile"),
    };
    let new_id = fixture
        .cpu
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    assert_eq!(
        new_id.index_for_test(),
        old_id.index_for_test(),
        "the fixture needs the freed slot back or it proves nothing"
    );
    assert_ne!(new_id, old_id, "a recycled slot must take a new generation");

    let epoch = fixture.bus.jit_cost_dial_epoch();
    assert_eq!(
        fixture.cpu.jit_direct.callout_admission(new_id, epoch),
        jit::direct::CallOutAdmission::Untried(0),
        "the new occupant must not inherit its predecessor's class"
    );
    assert_eq!(
        fixture.cpu.jit_direct.callout_admission(old_id, epoch),
        jit::direct::CallOutAdmission::Untried(0),
        "the retired id must read as a miss, not as its successor's class"
    );
}

#[test]
fn a_classification_does_not_survive_the_cost_dial_epoch() {
    // The other half of the memo key, and it is a SAFETY key, not a refresh mechanism. The epoch
    // is `active_mode + 1` and its only writer clears every compiled block first, so it cannot
    // roll inside a run and it bounds nothing: what this pins is that a class learned under one
    // persona can never be read under another. A classification is otherwise terminal for the
    // block's lifetime.
    let mut fixture = governed_slot_block(|bus| {
        bus.lazy_io_reads = true;
        bus.io_read_value = Some(0x5a);
    });
    let id = fixture.block.id();
    let epoch = fixture.bus.jit_cost_dial_epoch();
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    assert_eq!(
        fixture.cpu.jit_direct.callout_admission(id, epoch),
        jit::direct::CallOutAdmission::Lazy
    );
    assert_eq!(
        fixture
            .cpu
            .jit_direct
            .callout_admission(id, epoch.wrapping_add(1)),
        jit::direct::CallOutAdmission::Untried(0),
        "a class learned under one dial epoch must not be read under another"
    );
    // And storing under the new epoch drops the old epoch's entry rather than leaving it live.
    fixture.cpu.jit_direct.set_callout_admission(
        id,
        epoch.wrapping_add(1),
        jit::direct::CallOutAdmission::IoTouching,
    );
    assert_eq!(
        fixture.cpu.jit_direct.callout_admission(id, epoch),
        jit::direct::CallOutAdmission::Untried(0)
    );
}

/// Set the TSS I/O bitmap bit for `PORT`, turning the fixture's permitting TSS into a denying
/// one without recompiling anything. `io_base` is zero in the zeroed TSS, so the bitmap byte is
/// at `base + PORT / 8`.
/// `(jit_direct_insns, jit_direct_linked_transfers)`, copied out so the borrow ends here.
fn counters(fixture: &Fixture) -> (u64, u64) {
    let perf = fixture.cpu.perf_counters();
    (perf.jit_direct_insns, perf.jit_direct_linked_transfers)
}

fn deny_the_port_in_the_bitmap(fixture: &mut Fixture) {
    let byte = (GOVERNOR_TSS_BASE + u32::from(PORT) / 8) as usize;
    fixture.bus.memory[byte] |= 1 << (PORT % 8);
}

#[test]
fn a_lazy_block_that_later_meets_a_denied_port_is_demoted_once() {
    // The ONE post-trial transition. Without it, a block classified `Lazy` whose port is later
    // revoked -- a V86 monitor rewriting the bitmap under a running task -- pays
    // spill/call/refuse/side-exit on every execution of that IN, unbounded in count. With it the
    // residual is one abnormal per block.
    let mut fixture = governed_slot_block(|bus| {
        bus.lazy_io_reads = true;
        bus.io_read_value = Some(0x5a);
    });
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    assert_eq!(admission(&fixture), jit::direct::CallOutAdmission::Lazy);

    // The monitor revokes the port. The next entry is NOT a trial -- the block is already
    // classified -- so this is the demotion arm and nothing else.
    deny_the_port_in_the_bitmap(&mut fixture);
    let trials = fixture.cpu.direct_stall_snapshot().callout_governor_trials;
    rearm_governor_block(&mut fixture);
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap(),
        "the demoting entry is a normal Lazy entry: it runs and the helper refuses"
    );
    let stalls = fixture.cpu.direct_stall_snapshot();
    assert_eq!(
        stalls.callout_governor_trials, trials,
        "the demoting entry must not be counted as a trial"
    );
    assert_eq!(stalls.side_exit_callout_abnormal, 1);
    assert_eq!(
        admission(&fixture),
        jit::direct::CallOutAdmission::Denied,
        "an abnormal serve outside the trial must demote the block"
    );

    // And the demotion has to reach the gate: every later entry is refused at head, so the
    // abnormal is paid exactly once.
    let executed = fixture.cpu.direct_stall_snapshot().callout_executed;
    for entry in 1..4 {
        rearm_governor_block(&mut fixture);
        assert!(
            !fixture
                .cpu
                .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
                .unwrap(),
            "entry {entry}: a demoted block must be refused at head"
        );
        let stalls = fixture.cpu.direct_stall_snapshot();
        assert_eq!(stalls.reject_callout_privileged, entry, "entry {entry}");
        assert_eq!(
            stalls.callout_executed, executed,
            "entry {entry}: the abnormal must be paid once, not once per entry"
        );
    }
}

#[test]
fn a_trial_entry_runs_one_block_where_a_classified_entry_chains() {
    // THE CLAMP, and it needs a block that would otherwise chain or it proves nothing. Block A
    // carries the call-out and ends at a NOT-TAKEN `jz`, so it falls through into block B and
    // publishes a static successor; the trial must still retire A alone.
    //
    // The link is warmed with IOPL 3 first. That is not scaffolding: at IOPL 3 a CPL-3 task
    // reaches ports without the bitmap, G2's privilege predicate is false, and the governor never
    // sees the entry -- so the warm pass both binds the edge and establishes that this block DOES
    // chain when nothing clamps it. Without that leg a broken clamp and an unbound link look the
    // same.
    let mut fixture = governor_block(
        &[
            &[0x89, 0xf6], // mov esi,esi
            &[0xec],       // in al,dx
            &[0x85, 0xc0], // test eax,eax
            &[0x74, 0x06], // jz +6, never taken: EAX keeps its high bytes
            &[0x89, 0xff], // block B: mov edi,edi
            &[0x89, 0xf6], // block B: mov esi,esi
            &[0x89, 0xff], // block B: mov edi,edi
        ],
        4,
        |bus| {
            bus.lazy_io_reads = true;
            bus.io_read_value = Some(0x5a);
        },
        |_| {},
    );
    // `governor_block` compiled and installed block A only -- the walk stops at the `jz`. Install
    // B so the fall-through edge has somewhere to resolve to.
    // 2 + 1 + 2 + 2 bytes of block A. The `jz +6` measures from the end of the branch, so its
    // TAKEN target is the HLT past block B and its fall-through is block B's first byte. Block B
    // is three instructions because a two-slot non-terminal block does not compile.
    let b = ENTRY + 7;
    // Re-prime B's line: compiling A consumed the decode slots the builder primed, and a stale
    // line is what `CompileOutcome::Retry` means.
    for linear in [b, b + 2, b + 4] {
        fixture.cpu.set_eip(linear);
        fixture.cpu.fetch_decoded(&mut fixture.bus, linear).unwrap();
    }
    let key_b = jit::direct::key_for(&fixture.cpu, b, true).expect("block B key");
    assert!(matches!(
        fixture.cpu.jit_direct.probe(key_b),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut fixture.cpu, b, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("block B structurally rejected")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("block B asked for a retry"),
    };
    assert_eq!(compilation.span.instructions, 3);
    assert_eq!(compilation.callout_slots, 0);
    fixture
        .cpu
        .jit_direct
        .install(&compilation)
        .expect("block B installs");

    // Warm the edge, ungoverned. Two passes: the first exits unbound and binds, the second takes
    // the hop.
    let mut warm_insns = 0;
    for pass in 0..2 {
        rearm_governor_block(&mut fixture);
        fixture.cpu.registers.eflags |= 3 << 12;
        let before = fixture.cpu.perf_counters().jit_direct_insns;
        assert!(
            fixture
                .cpu
                .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
                .unwrap(),
            "warm pass {pass}: the block must run"
        );
        warm_insns = fixture.cpu.perf_counters().jit_direct_insns - before;
    }
    assert_eq!(
        warm_insns, 7,
        "the warmed edge must carry the run through both blocks, or the clamp has nothing to clamp"
    );
    assert_eq!(
        fixture.cpu.direct_stall_snapshot().callout_governor_trials,
        0,
        "an IOPL-3 entry is not governed at all"
    );

    // The trial: same bound edge, IOPL back to 0, quota clamped to one block.
    rearm_governor_block(&mut fixture);
    let before = counters(&fixture);
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    let after = counters(&fixture);
    assert_eq!(
        fixture.cpu.direct_stall_snapshot().callout_governor_trials,
        1
    );
    assert_eq!(
        after.1 - before.1,
        0,
        "a trial entry must take no chain hop"
    );
    assert_eq!(
        after.0 - before.0,
        4,
        "a trial entry must retire the head block alone"
    );
    assert_eq!(admission(&fixture), jit::direct::CallOutAdmission::Lazy);

    // And the classification restores the full quota on the very next entry.
    rearm_governor_block(&mut fixture);
    let before = counters(&fixture);
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    let after = counters(&fixture);
    assert_eq!(after.1 - before.1, 1, "a Lazy entry must chain");
    assert_eq!(
        after.0 - before.0,
        7,
        "a Lazy entry must retire both blocks"
    );
    assert_eq!(
        fixture.cpu.direct_stall_snapshot().callout_governor_trials,
        1,
        "and it must not be a second trial"
    );
}

// =================================================================================================
// `0xE4` IN AL,imm8 -- `PortReadAlImm8`, the lean sibling of `PortReadAlDx` above.
//
// gp2 in-imm8 callout design rev 3 (`dev_docs/specs/2026-08-27-gp2-in-imm8-callout-design.md`),
// §8.2: the HELPER's contract lives beside `PortReadAlDx`'s equivalent fixtures here; the KNOB and
// the `classify` arm are covered separately in `cpu_jit_in_imm8_callout_test.rs`.
//
// Every positive fixture states the knob through `select_in_imm8_callout`, in both directions, for
// `cpu_jit_test_word_row_test.rs`'s reason: both arms, not the ambient default.
//
// `select_in_imm8_callout` returns a DROP GUARD rather than leaving the reset to the caller's own
// last statement (ROUND-3 review m3-6): a panicking assertion partway through a fixture used to
// skip the trailing `set_direct_in_imm8_callout_for_test(None)`, and under `--test-threads=1`
// every test shares the main thread, so a leaked `Some(true)` could silently arm a LATER fixture
// that meant to read the shipped default. `Drop::drop` runs during unwind too, so the guard resets
// the arm on every exit path, panicking or not, and removes the ordering dependency entirely.
// =================================================================================================

/// A port that fits the imm8 encoding. Distinct from `PORT` (0x03da, `0xEC`'s fixture port) only
/// so a fixture that accidentally shared state with the DX tests would be visible; the HELPER
/// itself places no range constraint on the port it is handed (that constraint is `classify`'s,
/// enforced by `insn.imm`'s `u8` fetch, and is covered in `cpu_jit_in_imm8_callout_test.rs`).
const IMM8_PORT: u16 = 0x40;

/// Resets `IZARRAVM_DIRECT_IN_IMM8_CALLOUT`'s thread-local override to the ambient reading when
/// dropped -- including on unwind, which is what closes the m3-6 gap a plain trailing reset left
/// open.
#[must_use]
struct InImm8CalloutGuard;

impl Drop for InImm8CalloutGuard {
    fn drop(&mut self) {
        jit::direct::set_direct_in_imm8_callout_for_test(None);
    }
}

fn select_in_imm8_callout(enabled: bool) -> InImm8CalloutGuard {
    jit::direct::set_direct_in_imm8_callout_for_test(Some(enabled));
    assert_eq!(jit::direct::direct_in_imm8_callout_armed(), enabled);
    InImm8CalloutGuard
}

/// Build the three-slot block `mov esi,esi` / `in al,PORT` / `mov edi,edi`, with the call-out
/// MID-BLOCK -- `slot_block_with`'s shape, for `0xE4` instead of `0xEC`. Requires the knob ON.
///
/// The guard is scoped to THIS function, not held by the caller: `classify` is consulted only
/// during the `compile` call below, so the arm only has to be live for that long, and the caller's
/// later `try_run_direct_block_for_test` runs an already-compiled block that never re-reads it.
fn slot_block_imm8(port: u8, configure: impl Fn(&mut TestBus)) -> (Fixture, CpuGsw, TestBus) {
    let _guard = select_in_imm8_callout(true);
    let mut code = vec![0x89, 0xf6];
    let body_at = ENTRY + code.len() as u32;
    code.push(0xe4);
    code.push(port);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);

    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu();
    let mut interpreter = flat_cpu();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interpreter_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        configure(bus);
    }
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interpreter, &mut interpreter_bus),
    ] {
        cpu.registers.set_esp(STACK_TOP);
        for &linear in &[ENTRY, body_at, tail_at] {
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
            panic!("structurally rejected: IN AL,imm8 was not admitted -- is the knob armed?")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, 3,
        "the block must extend THROUGH the call-out, not stop at it"
    );
    assert_eq!(compilation.callout_slots, 1);
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = native.jit_direct.block(id).expect("live block");

    for cpu in [&mut native, &mut interpreter] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(STACK_TOP);
        cpu.registers.set_eax(0xdead_beef);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.set_eip(ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    (
        Fixture {
            cpu: native,
            bus: native_bus,
            block,
        },
        interpreter,
        interpreter_bus,
    )
}

#[test]
fn imm8_the_helper_charges_exactly_what_the_interpreter_charges_and_reports_the_step_break() {
    for lazy in [false, true] {
        let mut cpu = flat_cpu();
        cpu.registers.set_eax(0xdead_beef);
        let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
        bus.lazy_io_reads = lazy;
        bus.io_read_value = Some(0x5a);

        let status = jit::direct::port_read_al_imm8_for_test(&mut cpu, &mut bus, 0, 0, IMM8_PORT);
        assert!(
            status >= 0,
            "lazy={lazy}: a served port read is not abnormal"
        );
        assert_eq!(
            status & 0xffff_ffff,
            i64::from(IN_PORT_CORE_CLOCKS),
            "lazy={lazy}: the helper must charge the SAME constant PortReadAlDx does"
        );
        assert_eq!(
            status >> jit::direct::STATUS_STEP_BREAK_BIT,
            i64::from(!lazy),
            "lazy={lazy}: the step-break bit must mirror the bus's own answer, exactly as PortReadAlDx's does"
        );
        assert_eq!(
            cpu.registers.eax(),
            0xdead_be5a,
            "lazy={lazy}: only AL may change"
        );
        assert_eq!(cpu.elapsed_clocks, 0, "lazy={lazy}: helper charged clocks");
        assert_eq!(cpu.perf_counters().instructions, 0);
    }
}

#[test]
fn imm8_a_denied_port_is_abnormal_with_zero_partial_effects() {
    let mut cpu = flat_cpu();
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x0b, 0xfb));
    cpu.registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x13, 0xf3));
    cpu.registers.set_eax(0xdead_beef);
    cpu.registers.eflags = 0x202;
    cpu.cpl = 3;
    assert_eq!(cpu.current_privilege_level(), 3, "fixture must be at CPL 3");

    let before = cpu.registers.clone();
    let before_eflags = cpu.eflags();
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
    bus.lazy_io_reads = true;

    let status = jit::direct::port_read_al_imm8_for_test(&mut cpu, &mut bus, 0, 0, IMM8_PORT);
    assert!(status < 0, "a denied port must be abnormal");
    assert_eq!(before, cpu.registers, "abnormal path wrote a register");
    assert_eq!(before_eflags, cpu.eflags(), "abnormal path wrote EFLAGS");
    assert_eq!(cpu.elapsed_clocks, 0, "abnormal path charged clocks");
    assert_eq!(
        bus.last_read_io_core_clocks_so_far, None,
        "the permission check must run BEFORE any device is addressed"
    );
}

#[test]
fn imm8_an_unsupported_port_is_abnormal_with_zero_partial_effects() {
    let mut cpu = flat_cpu();
    cpu.registers.set_eax(0xdead_beef);
    let before = cpu.registers.clone();
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
    bus.io_read_fails = true;

    let status = jit::direct::port_read_al_imm8_for_test(&mut cpu, &mut bus, 0, 0, IMM8_PORT);
    assert!(status < 0);
    assert_eq!(before, cpu.registers);
    assert_eq!(cpu.elapsed_clocks, 0);
    assert!(!bus.io_touched, "a failed read must not end the batch");
}

#[test]
fn imm8_v86_call_out_on_a_cold_tlb_is_refused_before_the_tss_probe() {
    // The TLB-miss refusal lane, isolated exactly as `v86_call_out_on_a_cold_tlb_is_refused_
    // before_the_tss_probe` isolates it for `0xEC`: IOPL 3 makes the `CPL > IOPL` half false, so
    // only `is_v86_mode()` can send the helper down the TSS-bitmap arm, and a cold TLB refuses it
    // there -- the interpreter would page-walk, which this design will not do from inside a block.
    let (mut cpu, mut bus) = paged_ring3_io_cpu(false);
    cpu.registers.eflags = 0x202 | FLAG_VM | (3 << 12);
    assert_eq!(cpu.current_privilege_level(), 3);
    assert_eq!(cpu.iopl(), 3);
    let before = cpu.registers.clone();

    let status = jit::direct::port_read_al_imm8_for_test(&mut cpu, &mut bus, 0, 0, IMM8_PORT);

    assert!(status < 0, "a V86 task on a cold TLB must be refused");
    assert_eq!(before, cpu.registers);
    assert!(page_walk_writes(&bus).is_empty());
    assert_eq!(bus.last_read_io_core_clocks_so_far, None);
}

#[test]
fn imm8_a_permission_checked_port_is_refused_before_the_tss_probe_can_touch_memory() {
    // The CPL>IOPL half, isolated exactly as `a_permission_checked_port_is_refused_before_the_
    // tss_probe_can_touch_memory` isolates it for `0xEC`.
    for deny in [false, true] {
        let (mut cpu, mut bus) = paged_ring3_io_cpu(deny);
        let before = cpu.registers.clone();
        let before_cr2 = cpu.control.cr2;

        let status = jit::direct::port_read_al_imm8_for_test(&mut cpu, &mut bus, 0, 0, IMM8_PORT);

        assert!(
            status < 0,
            "deny={deny}: the privileged state must be refused"
        );
        assert_eq!(before, cpu.registers, "deny={deny}: registers");
        assert_eq!(before_cr2, cpu.control.cr2, "deny={deny}: CR2");
        assert_eq!(cpu.written_count, 0, "deny={deny}: written_pages");
        assert!(!cpu.written_pages_overflow, "deny={deny}");
        assert_eq!(cpu.elapsed_clocks, 0, "deny={deny}: clocks");
        assert!(
            page_walk_writes(&bus).is_empty(),
            "deny={deny}: the refused path set a page-table accessed bit"
        );
        assert_eq!(
            bus.last_read_io_core_clocks_so_far, None,
            "deny={deny}: the refused path reached the device"
        );
    }
}

#[test]
fn imm8_a_v86_port_is_served_natively_once_the_tss_pages_are_tlb_resident() {
    // The positive counterpart of the cold-TLB refusal, AND the engagement-counter gate (rev 3
    // §8.1, "Engagement counter"): `callout_port_imm8_served` is the dedicated always-on numerator
    // for THIS helper, separate from `callout_port_v86_served` (which is `0xEC`'s) and from the
    // shared `callout_executed`.
    let (mut cpu, mut bus) = warmed_tss_cpu(TSS_IO_MAP_OFFSET, 0x1000);
    cpu.registers.eflags = 0x202 | FLAG_VM | (3 << 12);
    assert!(cpu.is_v86_mode());
    assert_eq!(cpu.iopl(), 3);
    let before = cpu.direct_stall_snapshot();

    let status = jit::direct::port_read_al_imm8_for_test(&mut cpu, &mut bus, 0, 0, IMM8_PORT);

    assert!(status >= 0, "a permitted V86 port must be served");
    assert_eq!(
        cpu.registers.eax(),
        0xdead_be5a,
        "the port byte lands in AL"
    );
    assert!(
        page_walk_writes(&bus).is_empty(),
        "the served path page-walked -- the whole hazard the design refuses"
    );
    let after = cpu.direct_stall_snapshot();
    assert_eq!(
        after.callout_port_imm8_served - before.callout_port_imm8_served,
        1,
        "the imm8 engagement counter must fire on a served execution"
    );
    assert_eq!(
        after.callout_port_v86_served - before.callout_port_v86_served,
        0,
        "the imm8 arm must NOT bump the DX arm's bitmap-serve counter"
    );
    assert_eq!(
        after.callout_executed - before.callout_executed,
        1,
        "the shared denominator must count this call-out too"
    );
    assert_eq!(
        after.side_exit_callout_abnormal,
        before.side_exit_callout_abnormal
    );
}

#[test]
fn imm8_engagement_counter_is_separate_from_the_shared_executed_count() {
    // The counter's own non-vacuity, mirroring `the_cpl0_arm_does_not_count_as_a_bitmap_serve`:
    // `callout_port_imm8_served` must not merely track `callout_executed` under another name.
    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
    bus.lazy_io_reads = true;
    bus.io_read_value = Some(0x5a);

    let status = jit::direct::port_read_al_imm8_for_test(&mut cpu, &mut bus, 0, 0, IMM8_PORT);

    assert!(status >= 0);
    let stalls = cpu.direct_stall_snapshot();
    assert_eq!(stalls.callout_executed, 1);
    assert_eq!(
        stalls.callout_port_imm8_served, 1,
        "the ring-0 arm still bumps the engagement counter -- it fires on EVERY served \
         execution, not only the V86/CPL>IOPL bitmap arm (unlike callout_port_v86_served)"
    );
    // ROUND-3 review m3-5: the non-conflation the helper's own comment states as an invariant
    // ("bumping it here too would conflate the two opcodes' engagement on a guest that runs both")
    // was never pinned. `0xEC`'s bitmap-arm counter must stay at zero for an `0xE4` execution.
    assert_eq!(
        stalls.callout_port_v86_served, 0,
        "the imm8 arm must never bump the DX arm's bitmap-serve counter"
    );

    // An ABNORMAL execution must not bump it.
    let mut cpu2 = flat_cpu();
    let mut bus2 = TestBus::with_memory(vec![0u8; 0x5000]);
    bus2.io_read_fails = true;
    let status2 = jit::direct::port_read_al_imm8_for_test(&mut cpu2, &mut bus2, 0, 0, IMM8_PORT);
    assert!(status2 < 0);
    assert_eq!(cpu2.direct_stall_snapshot().callout_port_imm8_served, 0);
}

/// The mutant table's "bus.read_io argument fidelity" gate: swap `port`, or swap the `now`/`ring0`
/// argument order, and this fixture must fail.
///
/// `TestBus::read_io` records `(port, core_clocks_so_far)` in `io_reads` and the `ring0` argument
/// in `last_read_io_ring0` (added for this slice); together the three assertions pin all four of
/// `bus.read_io`'s arguments (`width` is always `BusWidth::Byte` for this opcode, unlike `PortRead
/// AlDx`'s shared `BusWidth::Byte`, so there is nothing to swap it WITH here).
#[test]
fn imm8_bus_read_io_argument_fidelity() {
    let mut cpu = flat_cpu();
    // Ring 0: `is_ring0_protected()` must read true here, so the fixture can tell a swapped
    // `now`/`ring0` pair apart from a correct one (both are plausible-looking integers/bools, but
    // `now` is never 0 or 1 by construction once `core_clocks_so_far` is nonzero).
    cpu.core_clocks_so_far = 12_345;
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
    bus.lazy_io_reads = true;
    bus.io_read_value = Some(0x5a);

    let prefix_raw = 24u64;
    let expected_now = {
        let mut probe = flat_cpu();
        probe.core_clocks_so_far = 12_345;
        probe
            .core_clocks_so_far
            .saturating_add(probe.scale_clocks_batch(prefix_raw))
    };
    let status =
        jit::direct::port_read_al_imm8_for_test(&mut cpu, &mut bus, prefix_raw, 0, IMM8_PORT);
    assert!(status >= 0);
    assert_eq!(
        bus.io_reads,
        vec![(IMM8_PORT, expected_now)],
        "port or now argument wrong"
    );
    assert_eq!(
        bus.last_read_io_ring0,
        Some(true),
        "ring0 argument wrong -- a swap with `now` would read back a nonzero value cast to bool \
         (true) only by accident on this fixture, which is why `is_ring0_protected()` is pinned \
         to a KNOWN value here rather than left at its default"
    );
}

/// The mutant/pendency table's "pendency / mid-block visibility" gate, rev 3 §8.1: "a
/// `brk_interrupt`/pendency fixture shows a divergence from the interpreted path, proving the real
/// PIT's read-only property is load-bearing" -- REBUILT per ROUND-3 review M3-1.
///
/// The first version of this fixture ran only the HELPER, in isolation, and asserted that nothing
/// about its own return value moved when the device wrongly armed `pending_irq`. That is a gate
/// that CANNOT GO RED if the safety proof it stands behind were violated: it never exercised the
/// interpreted path, so it had nothing to compare against and nothing that could diverge --
/// `[[gates-that-cannot-fail-are-systemic]]`.
///
/// This version runs the SAME guest instruction, from the SAME seeded state, against the SAME
/// wrongly-arming device, through BOTH paths -- the native helper and `cycle()` -- and asserts they
/// AGREE. `run.rs:892`'s `can_take_interrupt` never consults the bus, and `requires_step_break()`
/// (`run.rs:933`, `TestBus`'s own impl reads `self.io_touched` alone) is false on a lazy read for
/// the interpreted path too, so agreement is the EXPECTED result -- a STRONGER fact than the
/// original design row asked for: even against a device that violates the pure-peek assumption,
/// the two paths reach the same guest state and the same block-continuation decision, because
/// there is nowhere in EITHER path that a wrongly-armed interrupt could make them diverge. The real
/// PIT's read-only property is still what the correctness argument rests on for the ADMITTED
/// classes (an admission that consulted `interrupt_pending()` would need this fixture to fail it,
/// which is exactly what the mutation below does).
#[test]
fn imm8_the_call_out_mechanism_agrees_with_the_interpreter_against_a_wrongly_arming_device() {
    for arms_pending in [false, true] {
        // The native leg: the helper alone.
        let mut native = flat_cpu();
        native.registers.set_eax(0xdead_beef);
        let mut native_bus = TestBus::with_memory(vec![0u8; 0x5000]);
        native_bus.lazy_io_reads = true;
        native_bus.io_read_value = Some(0x5a);
        native_bus.io_read_arms_pending_irq = arms_pending;

        let status =
            jit::direct::port_read_al_imm8_for_test(&mut native, &mut native_bus, 0, 0, IMM8_PORT);
        assert!(status >= 0, "arms_pending={arms_pending}");
        let native_step_break = status >> jit::direct::STATUS_STEP_BREAK_BIT != 0;

        // The interpreted leg: the SAME instruction, from the SAME seed, against the SAME
        // wrongly-arming device shape, through the real per-instruction path -- the oracle this
        // fixture was missing.
        let mut interp = flat_cpu();
        interp.registers.set_eax(0xdead_beef);
        let mut interp_bus = TestBus::with_memory(vec![0u8; 0x5000]);
        interp_bus.lazy_io_reads = true;
        interp_bus.io_read_value = Some(0x5a);
        interp_bus.io_read_arms_pending_irq = arms_pending;
        interp_bus.memory[ENTRY as usize] = 0xe4;
        interp_bus.memory[ENTRY as usize + 1] = IMM8_PORT as u8;
        interp.set_eip(ENTRY);
        interp
            .cycle(&mut interp_bus)
            .expect("the interpreted IN must retire");

        // AGREEMENT, not blindness: the whole claim this fixture stands behind. Charge parity
        // between the two paths is `imm8_the_helper_charges_exactly_what_the_interpreter_charges_
        // and_reports_the_step_break`'s claim, not this one -- `native.elapsed_clocks` reads 0 by
        // construction here (the helper charges nothing itself; the caller folds `status`'s raw
        // clocks into the block's lane), so it is not a fixture that could see this row's hazard
        // and is deliberately not asserted again here.
        assert_eq!(
            native.registers.eax(),
            interp.registers.eax(),
            "arms_pending={arms_pending}: AL must land the same way on both paths"
        );
        assert_eq!(
            native_step_break,
            interp_bus.requires_step_break(),
            "arms_pending={arms_pending}: the block-continuation decision must agree -- neither \
             path may notice the wrongly-armed interrupt, which is the load-bearing claim behind \
             run.rs:892 (can_take_interrupt never consults the bus) and run.rs:933 \
             (requires_step_break, unaffected by pending_irq on a lazy read)"
        );
        assert_eq!(
            native_bus.interrupt_pending(),
            interp_bus.interrupt_pending(),
            "arms_pending={arms_pending}: sanity -- both stubs actually armed as claimed, or this \
             fixture is vacuous in both directions"
        );
    }
}

#[test]
fn imm8_call_out_matches_the_interpreter_mid_block() {
    // The emitted-slot counterpart of `call_out_matches_the_interpreter_mid_block`, and the
    // `--interpreter` invariance evidence at unit scale: the same guest bytes run natively (through
    // the compiled block) and through the block-free interpreter from identical state, and every
    // axis of architectural state is compared.
    let (mut fixture, mut interpreter, mut interpreter_bus) =
        slot_block_imm8(IMM8_PORT as u8, |bus| {
            bus.lazy_io_reads = true;
            bus.io_read_value = Some(0x5a);
        });

    let retired = fixture.cpu.perf_counters().jit_direct_insns;
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap(),
        "block did not run natively"
    );
    for _ in 0..3 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(
        fixture.cpu.perf_counters().jit_direct_insns - retired,
        3,
        "all three slots, the call-out included, must retire natively"
    );
    assert_eq!(
        fixture.cpu.registers.eax() & 0xff,
        0x5a,
        "the port byte must land in AL"
    );
    assert_eq!(
        crate::tests::settled_registers(&fixture.cpu),
        crate::tests::settled_registers(&interpreter),
        "registers"
    );
    assert_eq!(fixture.cpu.eflags(), interpreter.eflags(), "EFLAGS");
    assert_eq!(
        fixture.cpu.elapsed_clocks, interpreter.elapsed_clocks,
        "core clocks"
    );
    assert_eq!(
        fixture.bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks(),
        "bus clocks"
    );
    // No trailing reset here: `slot_block_imm8`'s own guard already reset the arm the moment
    // `compile` returned, well before this point.
}

#[test]
fn imm8_a_step_breaking_port_ends_the_native_run_after_the_call_out() {
    let (mut fixture, mut interpreter, mut interpreter_bus) =
        slot_block_imm8(IMM8_PORT as u8, |bus| {
            bus.io_read_value = Some(0x5a);
        });

    let retired = fixture.cpu.perf_counters().jit_direct_insns;
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    for _ in 0..2 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(
        fixture.cpu.perf_counters().jit_direct_insns - retired,
        2,
        "the call-out and its prefix retire natively, the tail does not"
    );
    assert_eq!(fixture.cpu.registers.eip, interpreter.registers.eip);
    assert_eq!(
        crate::tests::settled_registers(&fixture.cpu),
        crate::tests::settled_registers(&interpreter)
    );
    let stalls = fixture.cpu.direct_stall_snapshot();
    assert_eq!(stalls.side_exit_callout_step_break, 1);
    assert_eq!(stalls.side_exit_callout_abnormal, 0);
}

#[test]
fn imm8_an_abnormal_call_out_ends_the_run_at_the_instruction_with_no_partial_effects() {
    let (mut fixture, mut interpreter, mut interpreter_bus) =
        slot_block_imm8(IMM8_PORT as u8, |bus| {
            bus.io_read_fails = true;
        });

    let retired = fixture.cpu.perf_counters().jit_direct_insns;
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    interpreter.cycle(&mut interpreter_bus).unwrap();

    assert_eq!(fixture.cpu.perf_counters().jit_direct_insns - retired, 1);
    assert_eq!(
        crate::tests::settled_registers(&fixture.cpu),
        crate::tests::settled_registers(&interpreter)
    );
    assert_eq!(
        fixture.cpu.registers.eax(),
        0xdead_beef,
        "AL must be untouched"
    );
    let stalls = fixture.cpu.direct_stall_snapshot();
    assert_eq!(stalls.side_exit_callout_abnormal, 1);
    assert_eq!(stalls.side_exit_callout_step_break, 0);
}

/// `CallOutHelper` exhaustiveness (mutant table gate 11): `helper_offset`, `probes_io_permission`,
/// `moves_a_stack_frame`, `interprets_one`, `carries_a_cell` and `republishes_flags` are every one
/// an EXHAUSTIVE `match` over `CallOutHelper` with no catch-all arm. The exhaustiveness itself is a
/// property the Rust compiler enforces at BUILD time -- removing an arm from any of the six is a
/// compile error, which no runtime assertion can strengthen -- but ROUND-3 review m3-4 is right
/// that an EMPTY test body is this campaign's own `gates-that-cannot-fail-are-systemic` shape: it
/// passes on any build that compiles, which says nothing about `PortReadAlImm8` in particular. This
/// body gives it something that CAN fail: it exercises `helper_offset` (via the test-only
/// `helper_offset_for_test`, since the real function is module-private) over every variant and
/// checks the ONE fact the whole call-out ABI depends on -- each helper CLASS loads its function
/// pointer from a DISTINCT, in-range `CallOutTable` field, so `PortReadAlImm8` cannot silently
/// alias `PortReadAlDx`'s slot (or any other's) and jump to the wrong helper.
///
/// MUTATION that turns this red: change `PortReadAlImm8`'s arm in `helper_offset` from
/// `offset_of!(CallOutTable, port_read_al_imm8)` to `offset_of!(CallOutTable, port_read_al_dx)` --
/// the two offsets collapse to the same value and the distinctness assertion fails.
#[test]
fn call_out_helper_match_exhaustiveness_is_a_compile_time_property() {
    let cpu_gsw_size = i32::try_from(core::mem::size_of::<CpuGsw>()).unwrap();
    let variants = [
        ("PortReadAlDx", jit::direct::CallOutHelper::PortReadAlDx),
        (
            "PortReadAlImm8",
            jit::direct::CallOutHelper::PortReadAlImm8 { port: 0x40 },
        ),
        ("PushAllDword", jit::direct::CallOutHelper::PushAllDword),
        ("PopAllDword", jit::direct::CallOutHelper::PopAllDword),
        (
            "PortWriteAlImm8",
            jit::direct::CallOutHelper::PortWriteAlImm8 { port: 0x43 },
        ),
        ("PortWriteAlDx", jit::direct::CallOutHelper::PortWriteAlDx),
        (
            "InterpretOne",
            jit::direct::CallOutHelper::InterpretOne {
                row: jit::direct::InterpretOneRow::PopRm,
            },
        ),
    ];
    let offsets: Vec<(&str, i32)> = variants
        .iter()
        .map(|(name, helper)| (*name, jit::direct::helper_offset_for_test(*helper)))
        .collect();
    for (name, offset) in &offsets {
        assert!(
            *offset >= 0 && *offset < cpu_gsw_size,
            "{name}: helper_offset {offset} is out of CpuGsw's bounds"
        );
    }
    for i in 0..offsets.len() {
        for j in (i + 1)..offsets.len() {
            assert_ne!(
                offsets[i].1, offsets[j].1,
                "{} and {} load their function pointer from the SAME CallOutTable field",
                offsets[i].0, offsets[j].0
            );
        }
    }
    // A second `InterpretOneRow` must still land on the SAME field as the first: the row travels
    // with the kind for the census, not to pick a different function pointer.
    assert_eq!(
        jit::direct::helper_offset_for_test(jit::direct::CallOutHelper::InterpretOne {
            row: jit::direct::InterpretOneRow::Sti,
        }),
        offsets[6].1,
        "every InterpretOneRow must share InterpretOne's one CallOutTable field"
    );
}

// =================================================================================================
// `0xE6` OUT imm8,AL -- `PortWriteAlImm8`, the WRITE twin of `PortReadAlImm8` above, behind
// `IZARRAVM_OUT_IMM8_ROWS` (`dev_docs/gp2-out-e6-research-2026-08-30.md` §5 Option B).
//
// Placement follows the `0xE4` slice: the HELPER's contract lives here beside its read siblings;
// the KNOB and the `classify` arm are covered in `cpu_jit_out_imm8_callout_test.rs`.
//
// THE ONE FACT THESE FIXTURES EXIST FOR. A port WRITE can make an interrupt newly deliverable from
// inside `write_io` -- `MachineBus`'s PIT arm calls `pic.request(0)` when a channel-0 control word
// raises OUT -- which is the premise the whole call-out family's pendency proof denied ("no
// InterpretOne row writes a port; the call-outs that touch a port at all are both READS"). The
// resolution is a step break reported UNCONDITIONALLY, not `bus.requires_step_break()`: on a
// ring-0 protected-mode guest (gp2, DOS/4GW) `MachineBus::write_io` takes its `skip_io_touched`
// exemption and never sets `io_touched` at all, so the bus's own answer is false for exactly the
// guest class this row was measured on. `out_imm8_forces_the_step_break_even_when_the_bus_does_not`
// is that claim, and `TestBus::lazy_io_writes` is the shape that lets it go red.
// =================================================================================================

/// The PIT control port -- gp2's own `0xE6` site, and the port whose write reaches
/// `pic.request(0)`. Distinct from `PORT` and `IMM8_PORT` so a fixture sharing state with the read
/// suites would be visible.
const OUT_IMM8_PORT: u16 = 0x43;

/// Resets `IZARRAVM_OUT_IMM8_ROWS`'s thread-local override on every exit path, unwind included.
#[must_use]
struct OutImm8RowsGuard;

impl Drop for OutImm8RowsGuard {
    fn drop(&mut self) {
        jit::direct::set_out_imm8_rows_for_test(None);
    }
}

fn select_out_imm8_rows(enabled: bool) -> OutImm8RowsGuard {
    jit::direct::set_out_imm8_rows_for_test(Some(enabled));
    assert_eq!(jit::direct::out_imm8_rows_armed(), enabled);
    OutImm8RowsGuard
}

/// Build the three-slot block `mov esi,esi` / `out PORT,al` / `mov edi,edi`, call-out MID-BLOCK,
/// exactly as `slot_block_imm8` does for `0xE4`. Requires the knob ON, which the guard supplies
/// for the length of the `compile` call and no longer.
fn slot_block_out_imm8(port: u8, configure: impl Fn(&mut TestBus)) -> (Fixture, CpuGsw, TestBus) {
    let _guard = select_out_imm8_rows(true);
    let mut code = vec![0x89, 0xf6];
    let body_at = ENTRY + code.len() as u32;
    code.push(0xe6);
    code.push(port);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);

    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu();
    let mut interpreter = flat_cpu();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interpreter_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        configure(bus);
    }
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interpreter, &mut interpreter_bus),
    ] {
        cpu.registers.set_esp(STACK_TOP);
        for &linear in &[ENTRY, body_at, tail_at] {
            cpu.set_eip(linear);
            cpu.fetch_decoded(bus, linear).unwrap();
        }
    }

    // The probe is what moves the entry to `Seen`, which `install` requires; it also states that
    // the fixture starts from an uncompiled site rather than inheriting one.
    let key = jit::direct::key_for(&native, ENTRY, true).expect("entry key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut native, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("structurally rejected: OUT imm8,AL was not admitted -- is the knob armed?")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, 2,
        "the walk must STOP at the OUT: its step break is unconditional, so any slot after \
         it would be emitted dead. `DirectKind::is_terminal` is what stops it"
    );
    assert_eq!(compilation.callout_slots, 1);
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = native.jit_direct.block(id).expect("live block");

    for cpu in [&mut native, &mut interpreter] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(STACK_TOP);
        cpu.registers.set_eax(0xdead_be5a);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.set_eip(ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    (
        Fixture {
            cpu: native,
            bus: native_bus,
            block,
        },
        interpreter,
        interpreter_bus,
    )
}

#[test]
fn out_imm8_the_helper_charges_exactly_what_the_interpreter_charges() {
    let mut cpu = flat_cpu();
    cpu.registers.set_eax(0xdead_be5a);
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);

    let status = jit::direct::port_write_al_imm8_for_test(&mut cpu, &mut bus, 0, 0, OUT_IMM8_PORT);
    assert!(status >= 0, "a served port write is not abnormal");
    assert_eq!(
        status & 0xffff_ffff,
        i64::from(OUT_PORT_CORE_CLOCKS),
        "the helper must charge the SAME constant the interpreter's 0xE6 arm charges -- 10, not \
         IN_PORT_CORE_CLOCKS' 12"
    );
    assert_eq!(
        bus.io_writes,
        vec![(OUT_IMM8_PORT, 0x5a, 0)],
        "AL, and only AL, must reach the device -- port, value and `now` all pinned"
    );
    assert_eq!(
        cpu.registers.eax(),
        0xdead_be5a,
        "OUT writes no register and no flag"
    );
    assert_eq!(cpu.elapsed_clocks, 0, "helper charged clocks itself");
    assert_eq!(cpu.perf_counters().instructions, 0);
}

/// **THE UNCONDITIONAL BREAK.** With `lazy_io_writes` the bus models `MachineBus`'s
/// `skip_io_touched` exemption -- the ring-0 protected-mode arm gp2 runs in, where a port write
/// sets NOTHING and `requires_step_break()` stays false. The helper must report the step break
/// anyway.
///
/// MUTATION that turns this red: write the status word as
/// `i64::from(bus.requires_step_break()) << STATUS_STEP_BREAK_BIT`, i.e. the shape both IN helpers
/// use. The `lazy = true` leg then reads 0 and the block would run on past a write that may have
/// just raised IRQ0 inside `pic.request(0)`.
#[test]
fn out_imm8_forces_the_step_break_even_when_the_bus_does_not() {
    for lazy in [false, true] {
        let mut cpu = flat_cpu();
        cpu.registers.set_eax(0xdead_be5a);
        let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
        bus.lazy_io_writes = lazy;

        let status =
            jit::direct::port_write_al_imm8_for_test(&mut cpu, &mut bus, 0, 0, OUT_IMM8_PORT);
        assert!(status >= 0, "lazy={lazy}");
        assert_eq!(
            status >> jit::direct::STATUS_STEP_BREAK_BIT,
            1,
            "lazy={lazy}: the step break is UNCONDITIONAL, not the bus's answer"
        );
        assert_eq!(
            bus.requires_step_break(),
            !lazy,
            "lazy={lazy}: the fixture is vacuous unless the bus really does disagree on one leg"
        );
    }
}

#[test]
fn out_imm8_a_denied_port_is_abnormal_with_zero_partial_effects() {
    let mut cpu = flat_cpu();
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x0b, 0xfb));
    cpu.registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x13, 0xf3));
    cpu.registers.set_eax(0xdead_be5a);
    cpu.registers.eflags = 0x202;
    cpu.cpl = 3;
    assert_eq!(cpu.current_privilege_level(), 3, "fixture must be at CPL 3");

    let before = cpu.registers.clone();
    let before_eflags = cpu.eflags();
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);

    let status = jit::direct::port_write_al_imm8_for_test(&mut cpu, &mut bus, 0, 0, OUT_IMM8_PORT);
    assert!(status < 0, "a denied port must be abnormal");
    assert_eq!(before, cpu.registers, "abnormal path wrote a register");
    assert_eq!(before_eflags, cpu.eflags(), "abnormal path wrote EFLAGS");
    assert_eq!(cpu.elapsed_clocks, 0, "abnormal path charged clocks");
    assert!(
        bus.io_writes.is_empty(),
        "the permission check must run BEFORE the device is written -- a refusal after the write \
         would be a guest-visible side effect the interpreter never produced"
    );
}

#[test]
fn out_imm8_an_unsupported_port_is_abnormal_with_zero_partial_effects() {
    let mut cpu = flat_cpu();
    cpu.registers.set_eax(0xdead_be5a);
    let before = cpu.registers.clone();
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
    bus.io_write_fails = true;

    let status = jit::direct::port_write_al_imm8_for_test(&mut cpu, &mut bus, 0, 0, OUT_IMM8_PORT);
    assert!(status < 0);
    assert_eq!(before, cpu.registers);
    assert_eq!(cpu.elapsed_clocks, 0);
    assert_eq!(
        cpu.direct_stall_snapshot().callout_port_out_imm8_served,
        0,
        "an abnormal execution must not bump the engagement counter"
    );
}

#[test]
fn out_imm8_v86_call_out_on_a_cold_tlb_is_refused_before_the_tss_probe() {
    // The TLB-miss refusal lane, isolated exactly as the two IN helpers' equivalents isolate it:
    // IOPL 3 makes the `CPL > IOPL` half false, so only `is_v86_mode()` can send the helper down
    // the TSS-bitmap arm, and a cold TLB refuses it there rather than page-walking from inside a
    // block. The gate is the READ helper's, verbatim -- a write that took a wider gate than 0xE4's
    // would admit a state the interpreter faults in.
    let (mut cpu, mut bus) = paged_ring3_io_cpu(false);
    cpu.registers.eflags = 0x202 | FLAG_VM | (3 << 12);
    assert_eq!(cpu.current_privilege_level(), 3);
    assert_eq!(cpu.iopl(), 3);
    let before = cpu.registers.clone();

    let status = jit::direct::port_write_al_imm8_for_test(&mut cpu, &mut bus, 0, 0, PORT);

    assert!(status < 0, "a V86 task on a cold TLB must be refused");
    assert_eq!(before, cpu.registers);
    assert!(page_walk_writes(&bus).is_empty());
    assert!(bus.io_writes.is_empty());
}

#[test]
fn out_imm8_a_permission_checked_port_is_refused_before_the_tss_probe_can_touch_memory() {
    for deny in [false, true] {
        let (mut cpu, mut bus) = paged_ring3_io_cpu(deny);
        let before = cpu.registers.clone();
        let before_cr2 = cpu.control.cr2;

        let status = jit::direct::port_write_al_imm8_for_test(&mut cpu, &mut bus, 0, 0, PORT);

        assert!(
            status < 0,
            "deny={deny}: the privileged state must be refused"
        );
        assert_eq!(before, cpu.registers, "deny={deny}: registers");
        assert_eq!(before_cr2, cpu.control.cr2, "deny={deny}: CR2");
        assert_eq!(cpu.written_count, 0, "deny={deny}: written_pages");
        assert!(!cpu.written_pages_overflow, "deny={deny}");
        assert_eq!(cpu.elapsed_clocks, 0, "deny={deny}: clocks");
        assert!(
            page_walk_writes(&bus).is_empty(),
            "deny={deny}: the refused path set a page-table accessed bit"
        );
        assert!(
            bus.io_writes.is_empty(),
            "deny={deny}: the refused path reached the device"
        );
    }
}

#[test]
fn out_imm8_a_v86_port_is_served_natively_once_the_tss_pages_are_tlb_resident() {
    // The positive counterpart of the cold-TLB refusal, AND the engagement counter's gate:
    // `callout_port_out_imm8_served` is the dedicated always-on numerator for THIS helper, so a
    // census can say whether the arm served anything without reading it out of the shared
    // `callout_executed`, which sums every helper class.
    let (mut cpu, mut bus) = warmed_tss_cpu(TSS_IO_MAP_OFFSET, 0x1000);
    cpu.registers.eflags = 0x202 | FLAG_VM | (3 << 12);
    cpu.registers.set_eax(0xdead_be5a);
    assert!(cpu.is_v86_mode());
    assert_eq!(cpu.iopl(), 3);
    let before = cpu.direct_stall_snapshot();

    let status = jit::direct::port_write_al_imm8_for_test(&mut cpu, &mut bus, 0, 0, PORT);

    assert!(status >= 0, "a permitted V86 port must be served");
    assert_eq!(
        bus.io_writes.first().map(|write| (write.0, write.1)),
        Some((PORT, 0x5a)),
        "AL must reach the device"
    );
    assert!(
        page_walk_writes(&bus).is_empty(),
        "the served path page-walked -- the whole hazard the design refuses"
    );
    let after = cpu.direct_stall_snapshot();
    assert_eq!(
        after.callout_port_out_imm8_served - before.callout_port_out_imm8_served,
        1,
        "the OUT engagement counter must fire on a served execution"
    );
    assert_eq!(
        after.callout_port_imm8_served - before.callout_port_imm8_served,
        0,
        "the OUT arm must NOT bump the 0xE4 read arm's counter -- a guest that runs both would be \
         unreadable"
    );
    assert_eq!(
        after.callout_port_v86_served - before.callout_port_v86_served,
        0,
        "nor the 0xEC bitmap-serve counter"
    );
    assert_eq!(
        after.callout_executed - before.callout_executed,
        1,
        "the shared denominator must count this call-out too"
    );
}

#[test]
fn out_imm8_call_out_matches_the_interpreter_mid_block() {
    // The emitted-slot differential: the same guest bytes through the compiled block and through
    // the block-free interpreter, from identical state. The block runs TWO slots -- the prefix and
    // the call-out -- because the forced step break ends the native run at the boundary AFTER the
    // OUT, so the interpreter twin is stepped twice to the same place.
    let (mut fixture, mut interpreter, mut interpreter_bus) =
        slot_block_out_imm8(OUT_IMM8_PORT as u8, |bus| {
            bus.lazy_io_writes = true;
        });

    let retired = fixture.cpu.perf_counters().jit_direct_insns;
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap(),
        "block did not run natively"
    );
    for _ in 0..2 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(
        fixture.cpu.perf_counters().jit_direct_insns - retired,
        2,
        "the prefix and the call-out retire natively; the tail does not, because the OUT's step \
         break is unconditional and this bus does not ask for one"
    );
    assert_eq!(
        fixture.cpu.registers.eip, interpreter.registers.eip,
        "EIP must advance past the OUT by exactly its own length"
    );
    assert_eq!(
        crate::tests::settled_registers(&fixture.cpu),
        crate::tests::settled_registers(&interpreter),
        "registers"
    );
    assert_eq!(fixture.cpu.eflags(), interpreter.eflags(), "EFLAGS");
    assert_eq!(
        fixture.cpu.elapsed_clocks, interpreter.elapsed_clocks,
        "core clocks"
    );
    assert_eq!(
        fixture.bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks(),
        "bus clocks"
    );
    assert_eq!(
        fixture.bus.io_writes, interpreter_bus.io_writes,
        "the device must see the same write, with the same value at the same guest time"
    );
    let stalls = fixture.cpu.direct_stall_snapshot();
    assert_eq!(stalls.side_exit_callout_step_break, 1);
    assert_eq!(stalls.side_exit_callout_abnormal, 0);
}

/// **THE BLOCK-SCOPED CLAIM.** A device that raises an IRQ from inside `write_io` is exactly
/// `MachineBus`'s PIT arm (`pic.request(0)` on a channel-0 control word that raises OUT, the
/// Tyrian 2000 fix). This fixture pairs it with `lazy_io_writes`, so nothing about the bus asks
/// for a break, and asserts what the step-break bit actually buys: the native run leaves the
/// BLOCK at the boundary after the OUT, so no later slot in it executes against the pendency
/// answer the write just changed.
///
/// It does NOT assert that the interrupt is delivered any sooner, because it is not:
/// `run_budgeted_inner`'s run break reads `bus.requires_step_break()`, which the `skip_io_touched`
/// regime leaves false, and `can_take_interrupt` never consults the bus. Delivery parity with the
/// interpreted path is the NEXT fixture's claim, measured on a real batch.
#[test]
fn out_imm8_an_irq_raised_inside_the_write_cannot_reach_a_later_slot_in_the_block() {
    let (mut fixture, mut interpreter, mut interpreter_bus) =
        slot_block_out_imm8(OUT_IMM8_PORT as u8, |bus| {
            bus.lazy_io_writes = true;
            bus.io_write_arms_pending_irq = true;
        });

    let retired = fixture.cpu.perf_counters().jit_direct_insns;
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    for _ in 0..2 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert!(
        fixture.bus.interrupt_pending(),
        "the fixture is vacuous unless the write really did raise the line"
    );
    assert!(
        !fixture.bus.requires_step_break(),
        "the fixture is vacuous unless the BUS declines to end the run -- that is the ring-0 \
         protected-mode exemption this row was measured under"
    );
    assert_eq!(
        fixture.cpu.perf_counters().jit_direct_insns - retired,
        2,
        "the OUT must be the LAST slot the block executes: the prefix and the call-out retire \
         natively and nothing after them does"
    );
    assert_eq!(fixture.cpu.registers.eip, interpreter.registers.eip);
    assert_eq!(
        fixture.bus.interrupt_pending(),
        interpreter_bus.interrupt_pending(),
        "both paths must reach the boundary with the same pendency answer"
    );
    assert_eq!(
        crate::tests::settled_registers(&fixture.cpu),
        crate::tests::settled_registers(&interpreter)
    );
}

/// **THE DELIVERY-PARITY PROBE, on a real batch, driven identically on both roles.**
///
/// The first draft of this slice claimed the forced step break made the IRQ deliverable earlier.
/// It does not, and a fixture that only ever ran the native side could not have told: the honest
/// question is whether the two roles deliver the vector at the SAME guest instruction, and the
/// only way to ask it is to run the same guest through both and look.
///
/// Both roles run `mov esi,esi` / `out 43h,al` / `inc edi` / `hlt` under `run_budgeted` -- the
/// production driver, which is what owns the per-instruction interrupt check -- against a device
/// that raises IRQ0 from inside the write and does NOT set `io_touched`. Real mode with CS.D = 1
/// (so the instruction decodes at Dword and is admitted) puts a usable IVT at zero: vector 0's
/// entry is four zero bytes, address zero holds a `HLT`, so a delivered interrupt is visible as
/// EIP 0 and a halt.
///
/// `EDI` IS THE INSTRUMENT. It is the one piece of guest state that separates "the vector was
/// taken at the boundary after the OUT" from "one more instruction ran first". Nothing here
/// presumes which answer is right: the assertion compares the two ROLES, and would go red for a
/// native path that delivered EARLY exactly as loudly as for one that delivered late.
///
/// WHAT IT MEASURED, recorded because it is the fact that corrected this slice's first draft:
/// **neither role delivers before the tail.** EDI is 1 and `brk_interrupt` is 0 on both, because
/// `run_budgeted_inner`'s run break reads `bus.requires_step_break()` -- false in the
/// `skip_io_touched` regime this bus models -- and `can_take_interrupt` never consults the bus. So
/// the guest runs `inc edi` with IRQ0 already raised, on the native path and on the interpreted
/// path alike. That is the parity the design actually has, and the two explicit assertions at the
/// end pin it so a future change on either side has to move both or fail.
///
/// Note what this fixture does NOT single out: with `DirectKind::is_terminal` stopping the walk
/// at the OUT, the block-scoped property has two independent guards, and removing either one
/// alone leaves this green. That is the design having belt and braces, not the fixture being
/// weak -- the claim under test is parity, and parity is what is asserted.
#[test]
fn out_imm8_delivers_the_irq_at_the_same_guest_instruction_as_the_block_free_role() {
    /// Everything about a role's run that is guest-visible or device-visible.
    #[derive(Debug, PartialEq, Eq)]
    struct RoleOutcome {
        eip: u32,
        edi: u32,
        eflags: u32,
        halted: bool,
        io_writes: Vec<(u16, u32, u64)>,
        interrupt_pending: bool,
        brk_interrupt: u64,
    }

    fn run_role(admit_natively: bool) -> (RoleOutcome, u64) {
        let _guard = select_out_imm8_rows(true);
        // `inc edi` (0x47) rather than another `mov`: the tail has to be OBSERVABLE, or the
        // instrument cannot see an instruction that ran when it should not have.
        let code = [0x89, 0xf6, 0xe6, OUT_IMM8_PORT as u8, 0x47, 0xf4];
        let mut memory = vec![0u8; 0x5000];
        // The IVT's vector-0 entry is the four zero bytes already there (offset 0, segment 0), and
        // address zero holds the HLT a delivered interrupt lands on.
        memory[0] = 0xf4;
        // A leading NOP, and BOTH roles start on it. A native block is entered through the
        // CONTINUATION path -- the second and later instructions of a run -- so a block sitting at
        // the run's first instruction would never be entered and the native role would silently
        // interpret. The NOP is what makes the block at `ENTRY` a continuation.
        memory[(ENTRY - 1) as usize] = 0x90;
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        let pristine = memory.clone();

        let mut cpu = flat_cpu();

        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;

        let arm = |cpu: &mut CpuGsw, bus: &mut TestBus| {
            bus.memory.copy_from_slice(&pristine);
            bus.trace.clear();
            bus.pending_irq = None;
            bus.io_touched = false;
            bus.io_writes.clear();
            bus.lazy_io_writes = true;
            bus.io_write_arms_pending_irq = true;
            cpu.halted = false;
            cpu.interrupt_shadow = false;
            cpu.registers.gpr.fill(0);
            cpu.registers.set_esp(STACK_TOP);
            cpu.registers.set_eax(0x0000_005a);
            cpu.registers.eflags = 0x202;
            cpu.pending_flags = PendingFlags::default();
            cpu.set_eip(ENTRY - 1);
            cpu.elapsed_clocks = 0;
            cpu.core_clocks_so_far = 0;
            cpu.timing_rem = 0;
        };
        let run_to_halt = |cpu: &mut CpuGsw, bus: &mut TestBus| {
            for _ in 0..64 {
                if cpu
                    .run_budgeted(bus, u64::MAX)
                    .expect("the guest must not fault")
                    .halted
                {
                    return;
                }
            }
            panic!("the guest did not halt");
        };

        // PRIMING. The block is compiled and installed EXPLICITLY rather than left to auto-admit
        // heat: this guest runs its entry once per run and would need a heat schedule to earn a
        // compile, which would make the fixture a test of the admission governor instead of a
        // test of delivery parity. The block-free role installs nothing and so can only
        // interpret; both roles keep auto-admit off, so nothing ELSE compiles under either.
        cpu.set_jit_auto_admit(false);
        if admit_natively {
            for &linear in &[ENTRY - 1, ENTRY, ENTRY + 2, ENTRY + 4] {
                cpu.set_eip(linear);
                cpu.fetch_decoded(&mut bus, linear).unwrap();
            }
            let key = jit::direct::key_for(&cpu, ENTRY, true).expect("entry key");
            assert!(matches!(
                cpu.jit_direct.probe(key),
                jit::direct::BlockProbe::Interpret
            ));
            let compilation = match jit::direct::compile(&mut cpu, ENTRY, true) {
                jit::direct::CompileOutcome::Compiled(compilation) => compilation,
                _ => panic!("the block must compile with the knob armed"),
            };
            cpu.jit_direct
                .install(&compilation)
                .expect("block installs");
            // Entering an installed block is gated the same way admitting one is, so the native
            // role turns the dial back on AFTER the install: what it compiles from here is
            // nothing (this guest's entry runs once per run and earns no heat), what it ENTERS is
            // the block installed above.
            cpu.set_jit_auto_admit(true);
        }

        let before = cpu.perf_counters().jit_direct_insns;
        arm(&mut cpu, &mut bus);
        run_to_halt(&mut cpu, &mut bus);
        let perf = cpu.perf_counters();
        (
            RoleOutcome {
                eip: cpu.registers.eip,
                edi: cpu.registers.edi(),
                eflags: cpu.eflags(),
                halted: cpu.halted,
                io_writes: bus.io_writes.clone(),
                interrupt_pending: bus.interrupt_pending(),
                brk_interrupt: perf.brk_interrupt,
            },
            perf.jit_direct_insns - before,
        )
    }

    let (block_free, block_free_native) = run_role(false);
    let (native, native_insns) = run_role(true);

    // ANTI-VACUITY, both directions: one role must really have run the OUT natively and the other
    // must really not have, or this is two interpreters agreeing with each other.
    assert_eq!(
        block_free_native, 0,
        "the block-free role must retire nothing natively"
    );
    assert!(
        native_insns > 0,
        "the native role never entered a block -- the comparison would be vacuous: {native:#?}"
    );
    assert_eq!(
        native.io_writes.len(),
        1,
        "the fixture is vacuous unless the OUT actually reached the device"
    );
    assert_eq!(
        native, block_free,
        "the two roles must reach the same guest state by the same device-visible route: EDI is \
         the instrument that separates `the vector was taken at the boundary after the OUT` from \
         `one more instruction ran first`"
    );
    // The measured answer, stated rather than left implicit in the equality above: BOTH roles run
    // the tail with the IRQ already raised. If a future change makes either path deliver at the
    // boundary instead, these two go red and the doc above has to be rewritten with them -- which
    // is the point, because the first draft of this slice claimed the delivery point moved and
    // nothing in the fixtures could contradict it.
    assert_eq!(
        native.edi, 1,
        "the step-break bit does not preempt the next instruction, on either path"
    );
    assert_eq!(
        native.brk_interrupt, 0,
        "no run on either path ended on an interrupt transition"
    );
}

#[test]
fn out_imm8_an_abnormal_call_out_ends_the_run_at_the_instruction_with_no_partial_effects() {
    let (mut fixture, mut interpreter, mut interpreter_bus) =
        slot_block_out_imm8(OUT_IMM8_PORT as u8, |bus| {
            bus.io_write_fails = true;
        });

    let retired = fixture.cpu.perf_counters().jit_direct_insns;
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    interpreter.cycle(&mut interpreter_bus).unwrap();

    assert_eq!(fixture.cpu.perf_counters().jit_direct_insns - retired, 1);
    assert_eq!(
        crate::tests::settled_registers(&fixture.cpu),
        crate::tests::settled_registers(&interpreter)
    );
    let stalls = fixture.cpu.direct_stall_snapshot();
    assert_eq!(stalls.side_exit_callout_abnormal, 1);
    assert_eq!(stalls.side_exit_callout_step_break, 0);
}

// =================================================================================================
// 0xEE OUT DX,AL -- PortWriteAlDx, the DX-port twin of PortWriteAlImm8 above, admitted
// unconditionally (no knob), mirroring PortReadAlDx's admission of 0xEC.
//
// The tests below mirror the 0xE6/PortWriteAlImm8 suite one section up, opcode for opcode: the
// helper contract (charge, unconditional break, zero-partial-effects abnormal returns), the
// TSS-bitmap V86 arm (cold-TLB refusal, warm engagement, permission denial before any effect),
// and the emitted-slot differential against the interpreter. The one structural difference is the
// port itself: 0xE6 bakes it as a compile-time immediate, 0xEE reads it from DX at runtime,
// exactly as 0xEC does against 0xE4.
// =================================================================================================

#[test]
fn out_dx_the_helper_charges_exactly_what_the_interpreter_charges() {
    let mut cpu = flat_cpu();
    cpu.registers.set_edx(u32::from(PORT));
    cpu.registers.set_eax(0xdead_be5a);
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);

    let status = jit::direct::port_write_al_dx_for_test(&mut cpu, &mut bus, 0, 0);
    assert!(status >= 0, "a served port write is not abnormal");
    assert_eq!(
        status & 0xffff_ffff,
        i64::from(OUT_PORT_CORE_CLOCKS),
        "the helper must charge the SAME constant the interpreter's 0xEE arm charges -- 10, not \
         IN_PORT_CORE_CLOCKS' 12"
    );
    assert_eq!(
        bus.io_writes,
        vec![(PORT, 0x5a, 0)],
        "AL, and only AL, must reach the device -- port, value and now all pinned, and the port \
         must come from DX"
    );
    assert_eq!(
        cpu.registers.eax(),
        0xdead_be5a,
        "OUT writes no register and no flag"
    );
    assert_eq!(cpu.elapsed_clocks, 0, "helper charged clocks itself");
    assert_eq!(cpu.perf_counters().instructions, 0);
}

#[test]
fn out_dx_reads_the_port_from_dx_not_from_a_baked_immediate() {
    // The one structural difference from PortWriteAlImm8: two different DX values must reach two
    // different devices, from the SAME compiled call-out.
    for port in [PORT, 0x0040] {
        let mut cpu = flat_cpu();
        cpu.registers.set_edx(u32::from(port));
        cpu.registers.set_eax(0xdead_be5a);
        let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);

        let status = jit::direct::port_write_al_dx_for_test(&mut cpu, &mut bus, 0, 0);
        assert!(status >= 0, "port={port:#06x}");
        assert_eq!(
            bus.io_writes,
            vec![(port, 0x5a, 0)],
            "port={port:#06x}: the live DX value must be the port the device sees"
        );
    }
}

/// THE UNCONDITIONAL BREAK. With lazy_io_writes the bus models MachineBus's skip_io_touched
/// exemption -- the guest class 1000-miglia's PIT storm runs in, where a port write sets NOTHING
/// and requires_step_break() stays false. The helper must report the step break anyway, for the
/// identical reason PortWriteAlImm8 does.
#[test]
fn out_dx_forces_the_step_break_even_when_the_bus_does_not() {
    for lazy in [false, true] {
        let mut cpu = flat_cpu();
        cpu.registers.set_edx(u32::from(PORT));
        cpu.registers.set_eax(0xdead_be5a);
        let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
        bus.lazy_io_writes = lazy;

        let status = jit::direct::port_write_al_dx_for_test(&mut cpu, &mut bus, 0, 0);
        assert!(status >= 0, "lazy={lazy}");
        assert_eq!(
            status >> jit::direct::STATUS_STEP_BREAK_BIT,
            1,
            "lazy={lazy}: the step break is UNCONDITIONAL, not the bus's answer"
        );
        assert_eq!(
            bus.requires_step_break(),
            !lazy,
            "lazy={lazy}: the fixture is vacuous unless the bus really does disagree on one leg"
        );
    }
}

#[test]
fn out_dx_a_denied_port_is_abnormal_with_zero_partial_effects() {
    let mut cpu = flat_cpu();
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x0b, 0xfb));
    cpu.registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x13, 0xf3));
    cpu.registers.set_edx(u32::from(PORT));
    cpu.registers.set_eax(0xdead_be5a);
    cpu.registers.eflags = 0x202;
    cpu.cpl = 3;
    assert_eq!(cpu.current_privilege_level(), 3, "fixture must be at CPL 3");

    let before = cpu.registers.clone();
    let before_eflags = cpu.eflags();
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);

    let status = jit::direct::port_write_al_dx_for_test(&mut cpu, &mut bus, 0, 0);
    assert!(status < 0, "a denied port must be abnormal");
    assert_eq!(before, cpu.registers, "abnormal path wrote a register");
    assert_eq!(before_eflags, cpu.eflags(), "abnormal path wrote EFLAGS");
    assert_eq!(cpu.elapsed_clocks, 0, "abnormal path charged clocks");
    assert!(
        bus.io_writes.is_empty(),
        "the permission check must run BEFORE the device is written -- a refusal after the write \
         would be a guest-visible side effect the interpreter never produced"
    );
}

#[test]
fn out_dx_an_unsupported_port_is_abnormal_with_zero_partial_effects() {
    let mut cpu = flat_cpu();
    cpu.registers.set_edx(u32::from(PORT));
    cpu.registers.set_eax(0xdead_be5a);
    let before = cpu.registers.clone();
    let mut bus = TestBus::with_memory(vec![0u8; 0x5000]);
    bus.io_write_fails = true;

    let status = jit::direct::port_write_al_dx_for_test(&mut cpu, &mut bus, 0, 0);
    assert!(status < 0);
    assert_eq!(before, cpu.registers);
    assert_eq!(cpu.elapsed_clocks, 0);
    assert_eq!(
        cpu.direct_stall_snapshot().callout_port_out_dx_served,
        0,
        "an abnormal execution must not bump the engagement counter"
    );
}

#[test]
fn out_dx_v86_call_out_on_a_cold_tlb_is_refused_before_the_tss_probe() {
    // The TLB-miss refusal lane, isolated exactly as port_read_al_dx's and PortWriteAlImm8's
    // equivalents are: IOPL 3 makes the CPL > IOPL half false, so only is_v86_mode() can send the
    // helper down the TSS-bitmap arm, and a cold TLB refuses it there rather than page-walking
    // from inside a block.
    let (mut cpu, mut bus) = paged_ring3_io_cpu(false);
    cpu.registers.eflags = 0x202 | FLAG_VM | (3 << 12);
    assert_eq!(cpu.current_privilege_level(), 3);
    assert_eq!(cpu.iopl(), 3);
    let before = cpu.registers.clone();

    let status = jit::direct::port_write_al_dx_for_test(&mut cpu, &mut bus, 0, 0);

    assert!(status < 0, "a V86 task on a cold TLB must be refused");
    assert_eq!(before, cpu.registers);
    assert!(page_walk_writes(&bus).is_empty());
    assert!(bus.io_writes.is_empty());
}

#[test]
fn out_dx_a_permission_checked_port_is_refused_before_the_tss_probe_can_touch_memory() {
    for deny in [false, true] {
        let (mut cpu, mut bus) = paged_ring3_io_cpu(deny);
        let before = cpu.registers.clone();
        let before_cr2 = cpu.control.cr2;

        let status = jit::direct::port_write_al_dx_for_test(&mut cpu, &mut bus, 0, 0);

        assert!(
            status < 0,
            "deny={deny}: the privileged state must be refused"
        );
        assert_eq!(before, cpu.registers, "deny={deny}: registers");
        assert_eq!(before_cr2, cpu.control.cr2, "deny={deny}: CR2");
        assert_eq!(cpu.written_count, 0, "deny={deny}: written_pages");
        assert!(!cpu.written_pages_overflow, "deny={deny}");
        assert_eq!(cpu.elapsed_clocks, 0, "deny={deny}: clocks");
        assert!(
            page_walk_writes(&bus).is_empty(),
            "deny={deny}: the refused path set a page-table accessed bit"
        );
        assert!(
            bus.io_writes.is_empty(),
            "deny={deny}: the refused path reached the device"
        );
    }
}

/// V86 IOPL fault parity: the DENIED lane above (out_dx_a_permission_checked_port_is_refused,
/// deny=true) refuses the call-out, and this fixture proves the interpreter it hands the
/// instruction back to actually raises the SAME fault from the SAME state rather than silently
/// completing. Without this the "refusal costs only the call-out" claim would be untested on its
/// own terms -- a call-out that refused every V86 OUT unconditionally would pass every other test
/// in this section too.
#[test]
fn out_dx_the_interpreter_raises_the_gp_the_call_out_refused() {
    for deny in [false, true] {
        // NO eflags override: `paged_ring3_io_cpu`'s own default (0x202, IOPL 0, not V86) is the
        // CPL > IOPL lane, deliberately distinct from the V86 lane the cold-TLB test exercises.
        let (mut cpu, mut bus) = paged_ring3_io_cpu(deny);
        assert_eq!(cpu.current_privilege_level(), 3);
        assert_eq!(
            cpu.iopl(),
            0,
            "the fixture's TSS bitmap arm is the one under test"
        );
        let entry = 0x4000u32;
        bus.memory[entry as usize] = 0xee;
        bus.memory[entry as usize + 1] = 0xf4;
        cpu.set_eip(entry);

        // NOT unwrapped: with deny the #GP has no IDT to land in and nests. The subject -- the
        // TSS probe and the permission decision -- has already run by then, which the page-walk
        // assertion below pins.
        let outcome = cpu.cycle(&mut bus);

        assert!(
            !page_walk_writes(&bus).is_empty(),
            "deny={deny}: the fixture never reached a page walk, so it proves nothing"
        );
        if deny {
            assert!(
                bus.io_writes.is_empty(),
                "deny={deny}: a denied port must not reach the device"
            );
        } else {
            outcome.expect("a permitted port must retire");
            // NOT `elapsed_clocks > 0`: at I586 timing (`scale_clocks`'s 1/12 ratio) a lone
            // `OUT_PORT_CORE_CLOCKS` (10) charge floors to a zero quotient and lands entirely in
            // the fractional remainder -- `timing_rem` is the scale-invariant witness that the
            // charge was deposited at all, and it is what the call-out helper's own charge (this
            // section's `out_dx_the_helper_charges_exactly_what_the_interpreter_charges`) and the
            // differential (`out_dx_call_out_matches_the_interpreter_mid_block`, which compares
            // `elapsed_clocks` after the SAME scaling on both roles) both key off instead.
            assert_eq!(
                cpu.timing_rem, 10,
                "the interpreted OUT must charge the same OUT_PORT_CORE_CLOCKS the call-out does"
            );
            assert_eq!(cpu.registers.eip, entry + 1, "the OUT must retire");
            assert_eq!(
                bus.io_writes.first().map(|write| (write.0, write.1)),
                Some((PORT, 0xef)),
                "AL must reach the device at the DX port"
            );
        }
    }
}

#[test]
fn out_dx_a_v86_port_is_served_natively_once_the_tss_pages_are_tlb_resident() {
    // The positive counterpart of the cold-TLB refusal, AND the engagement counter's gate:
    // callout_port_out_dx_served is the dedicated always-on numerator for THIS helper, so a
    // census can say whether the arm served anything without reading it out of the shared
    // callout_executed, which sums every helper class.
    let (mut cpu, mut bus) = warmed_tss_cpu(TSS_IO_MAP_OFFSET, 0x1000);
    cpu.registers.eflags = 0x202 | FLAG_VM | (3 << 12);
    cpu.registers.set_eax(0xdead_be5a);
    assert!(cpu.is_v86_mode());
    assert_eq!(cpu.iopl(), 3);
    let before = cpu.direct_stall_snapshot();

    let status = jit::direct::port_write_al_dx_for_test(&mut cpu, &mut bus, 0, 0);

    assert!(status >= 0, "a permitted V86 port must be served");
    assert_eq!(
        bus.io_writes.first().map(|write| (write.0, write.1)),
        Some((PORT, 0x5a)),
        "AL must reach the device"
    );
    assert!(
        page_walk_writes(&bus).is_empty(),
        "the served path page-walked -- the whole hazard the design refuses"
    );
    let after = cpu.direct_stall_snapshot();
    assert_eq!(
        after.callout_port_out_dx_served - before.callout_port_out_dx_served,
        1,
        "the OUT DX engagement counter must fire on a served execution"
    );
    assert_eq!(
        after.callout_port_out_imm8_served - before.callout_port_out_imm8_served,
        0,
        "the OUT DX arm must NOT bump the 0xE6 imm8 arm's counter -- a guest that runs both would \
         be unreadable"
    );
    assert_eq!(
        after.callout_port_imm8_served - before.callout_port_imm8_served,
        0,
        "nor the 0xE4 read arm's counter"
    );
    assert_eq!(
        after.callout_port_v86_served - before.callout_port_v86_served,
        0,
        "nor the 0xEC bitmap-serve counter"
    );
    assert_eq!(
        after.callout_executed - before.callout_executed,
        1,
        "the shared denominator must count this call-out too"
    );
}

/// Build the three-slot block mov esi,esi / out dx,al / mov edi,edi, call-out MID-BLOCK, exactly
/// as slot_block_out_imm8 does for 0xE6 -- but DX is a live register rather than a baked
/// immediate, so the reset loop has to preload it on both CPUs every run.
fn slot_block_out_dx(port: u16, configure: impl Fn(&mut TestBus)) -> (Fixture, CpuGsw, TestBus) {
    let mut code = vec![0x89, 0xf6];
    let body_at = ENTRY + code.len() as u32;
    code.push(0xee);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);

    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu();
    let mut interpreter = flat_cpu();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interpreter_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        configure(bus);
    }
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interpreter, &mut interpreter_bus),
    ] {
        cpu.registers.set_esp(STACK_TOP);
        for &linear in &[ENTRY, body_at, tail_at] {
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
            panic!("structurally rejected: OUT DX,AL was not admitted")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, 2,
        "the walk must STOP at the OUT: its step break is unconditional, so any slot after it \
         would be emitted dead. DirectKind::is_terminal is what stops it"
    );
    assert_eq!(compilation.callout_slots, 1);
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = native.jit_direct.block(id).expect("live block");

    for cpu in [&mut native, &mut interpreter] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(STACK_TOP);
        cpu.registers.set_edx(u32::from(port));
        cpu.registers.set_eax(0xdead_be5a);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.set_eip(ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    native_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();
    (
        Fixture {
            cpu: native,
            bus: native_bus,
            block,
        },
        interpreter,
        interpreter_bus,
    )
}

/// Build a two-slot block whose FIRST slot writes DX (or DL alone) and whose SECOND is
/// `out dx,al`, then run it natively and return the port the device actually saw.
///
/// NIT-1: `port_write_al_dx` reads `cpu.read_gpr16(2)` at call time, after `emit_call_out`'s
/// unconditional `emit_store_homes` has flushed every guest register (including DX) from its host
/// home back to `cpu.registers`. This is the fixture that exercises that flush for DX specifically
/// -- every other `out_dx_*` fixture seeds DX before the block ever runs, which cannot tell "the
/// call-out reads the live register" apart from "the call-out reads whatever `set_edx` last wrote
/// before compilation", because both would be the same value. Here the block's OWN first slot is
/// what sets DX, so a call-out that baked the port at compile time (or read a stale host copy)
/// would serve the WRONG port.
fn port_seen_after_dx_write_slot(dx_write: &[u8], initial_edx: u32) -> u16 {
    let mut code = dx_write.to_vec();
    let out_at = ENTRY + code.len() as u32;
    code.push(0xee);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);

    let mut memory = vec![0u8; 0x5000];
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    bus.lazy_io_writes = true;
    cpu.registers.set_esp(STACK_TOP);
    for &linear in &[ENTRY, out_at, tail_at] {
        cpu.set_eip(linear);
        cpu.fetch_decoded(&mut bus, linear).unwrap();
    }

    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("entry key");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut cpu, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("structurally rejected: the DX-writing prefix must compile alongside the OUT")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, 2,
        "the DX write and the OUT must be the whole block"
    );
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = cpu.jit_direct.block(id).expect("live block");

    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_edx(initial_edx);
    cpu.registers.set_esp(STACK_TOP);
    cpu.registers.set_eax(0xdead_be5a);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
    bus.io_writes.clear();

    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block).unwrap(),
        "block did not run natively"
    );
    let (port, value, _) = *bus
        .io_writes
        .first()
        .expect("the OUT must have reached the device");
    assert_eq!(value, 0x5a, "AL must still be the value the OUT sends");
    port
}

#[test]
fn out_dx_uses_a_full_dx_write_from_the_same_block_not_a_stale_or_baked_value() {
    // `mov edx, imm32` (0xBA + 4-byte LE immediate): NEW_PORT (0x0060) must reach the device.
    // `initial_edx` seeds a DIFFERENT port (0x03DA) so a call-out reading a stale pre-block value
    // would be caught red-handed rather than accidentally agreeing.
    const NEW_PORT: u32 = 0x0060;
    let mut dx_write = vec![0xba];
    dx_write.extend_from_slice(&NEW_PORT.to_le_bytes());
    let port = port_seen_after_dx_write_slot(&dx_write, u32::from(PORT));
    assert_eq!(
        port, NEW_PORT as u16,
        "the call-out must serve the port the block's own DX write set, not the pre-block value"
    );
}

#[test]
fn out_dx_uses_a_dl_only_write_from_the_same_block_combined_with_the_live_dh() {
    // `mov dl, imm8` (0xB2 ib) touches only the low byte. `initial_edx` seeds DH = 0x12 (and a
    // DL the write must overwrite); the served port must be 0x1240 -- DH untouched by the write,
    // DL the write's new value -- which is what a byte-partial home reload has to get right.
    let dx_write = vec![0xb2, 0x40];
    let port = port_seen_after_dx_write_slot(&dx_write, 0x0000_1299);
    assert_eq!(
        port, 0x1240,
        "DL must come from the block's own write and DH must survive it unchanged"
    );
}

#[test]
fn out_dx_call_out_matches_the_interpreter_mid_block() {
    // The emitted-slot differential: same guest bytes, native block against the block-free
    // interpreter, from identical state. The block runs TWO slots -- the prefix and the call-out
    // -- because the forced step break ends the native run at the boundary AFTER the OUT, so the
    // interpreter twin is stepped twice to the same place. This is the fixture the task's "call-out
    // fires natively, assert retirement counters; device observes the write; wait-state cost
    // identical to interpreter" requirement names directly.
    let (mut fixture, mut interpreter, mut interpreter_bus) = slot_block_out_dx(PORT, |bus| {
        bus.lazy_io_writes = true;
    });

    let retired = fixture.cpu.perf_counters().jit_direct_insns;
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap(),
        "block did not run natively"
    );
    for _ in 0..2 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(
        fixture.cpu.perf_counters().jit_direct_insns - retired,
        2,
        "the prefix and the call-out retire natively; the tail does not, because the OUT's step \
         break is unconditional and this bus does not ask for one"
    );
    assert_eq!(
        fixture.cpu.registers.eip, interpreter.registers.eip,
        "EIP must advance past the OUT by exactly its own length"
    );
    assert_eq!(
        crate::tests::settled_registers(&fixture.cpu),
        crate::tests::settled_registers(&interpreter),
        "registers"
    );
    assert_eq!(fixture.cpu.eflags(), interpreter.eflags(), "EFLAGS");
    assert_eq!(
        fixture.cpu.elapsed_clocks, interpreter.elapsed_clocks,
        "core clocks -- the wait-state cost the call-out charges must match the interpreter's \
         IZARRAVM_ISA_IO_WAIT-priced OUT exactly"
    );
    assert_eq!(
        fixture.bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks(),
        "bus clocks"
    );
    assert_eq!(
        fixture.bus.io_writes, interpreter_bus.io_writes,
        "the device must see the same write, at the same port, with the same value, at the same \
         guest time -- the device observes the write claim"
    );
    let stalls = fixture.cpu.direct_stall_snapshot();
    assert_eq!(stalls.side_exit_callout_step_break, 1);
    assert_eq!(stalls.side_exit_callout_abnormal, 0);
}

#[test]
fn out_dx_an_irq_raised_inside_the_write_cannot_reach_a_later_slot_in_the_block() {
    // The block-scoped claim, mirroring out_imm8's equivalent: a device that raises an IRQ from
    // inside write_io is exactly MachineBus's PIT arm, and this row IS the PIT write storm
    // (1000-miglia's 0xEE census row is its post-#776 timer.pit_writes count exactly). Paired
    // with lazy_io_writes so nothing about the bus itself asks for a break, this proves the
    // step-break bit is what ends the block rather than the bus's own answer.
    let (mut fixture, mut interpreter, mut interpreter_bus) = slot_block_out_dx(PORT, |bus| {
        bus.lazy_io_writes = true;
        bus.io_write_arms_pending_irq = true;
    });

    let retired = fixture.cpu.perf_counters().jit_direct_insns;
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    for _ in 0..2 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert!(
        fixture.bus.interrupt_pending(),
        "the fixture is vacuous unless the write really did raise the line"
    );
    assert!(
        !fixture.bus.requires_step_break(),
        "the fixture is vacuous unless the BUS declines to end the run -- that is the ring-0 \
         protected-mode / skip_io_touched exemption this row was measured under"
    );
    assert_eq!(
        fixture.cpu.perf_counters().jit_direct_insns - retired,
        2,
        "the OUT must be the LAST slot the block executes: the prefix and the call-out retire \
         natively and nothing after them does"
    );
    assert_eq!(fixture.cpu.registers.eip, interpreter.registers.eip);
    assert_eq!(
        fixture.bus.interrupt_pending(),
        interpreter_bus.interrupt_pending(),
        "both paths must reach the boundary with the same pendency answer"
    );
    assert_eq!(
        crate::tests::settled_registers(&fixture.cpu),
        crate::tests::settled_registers(&interpreter)
    );
}

#[test]
fn out_dx_an_abnormal_call_out_ends_the_run_at_the_instruction_with_no_partial_effects() {
    let (mut fixture, mut interpreter, mut interpreter_bus) = slot_block_out_dx(PORT, |bus| {
        bus.io_write_fails = true;
    });

    let retired = fixture.cpu.perf_counters().jit_direct_insns;
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    interpreter.cycle(&mut interpreter_bus).unwrap();

    assert_eq!(fixture.cpu.perf_counters().jit_direct_insns - retired, 1);
    assert_eq!(
        crate::tests::settled_registers(&fixture.cpu),
        crate::tests::settled_registers(&interpreter)
    );
    let stalls = fixture.cpu.direct_stall_snapshot();
    assert_eq!(stalls.side_exit_callout_abnormal, 1);
    assert_eq!(stalls.side_exit_callout_step_break, 0);
}
