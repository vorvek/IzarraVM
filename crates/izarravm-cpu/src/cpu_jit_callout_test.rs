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

        let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, 0, 0);
        assert!(
            status >= 0,
            "lazy={lazy}: a served port read is not abnormal"
        );
        assert_eq!(
            status & 0xffff_ffff,
            i64::from(IN_AL_DX_CORE_CLOCKS),
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
    let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, prefix_raw, 0);
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

    let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, 0, 0);
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

    let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, 0, 0);
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
        jit::direct::CompileOutcome::Retry => panic!("compile asked for a retry"),
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
    assert_eq!(fixture.cpu.registers, interpreter.registers, "registers");
    assert_eq!(
        fixture.cpu.pending_flags, interpreter.pending_flags,
        "lazy flags"
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
    assert_eq!(fixture.cpu.registers, interpreter.registers, "registers");
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
        fixture.cpu.registers, interpreter.registers,
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
    let mut memory = vec![0u8; 0x9000];
    // PDE 0 -> the table: present, writable, user, accessed CLEAR.
    memory[PAGED_DIRECTORY as usize..PAGED_DIRECTORY as usize + 4]
        .copy_from_slice(&(PAGED_TABLE | 0x07).to_le_bytes());
    for page in 0..9u32 {
        let pte = (PAGED_TABLE + page * 4) as usize;
        memory[pte..pte + 4].copy_from_slice(&((page << 12) | 0x07).to_le_bytes());
    }
    let base = TSS_BASE as usize;
    memory[base + 0x66..base + 0x68].copy_from_slice(&TSS_IO_MAP_OFFSET.to_le_bytes());
    let bitmap = base + usize::from(TSS_IO_MAP_OFFSET) + usize::from(PORT / 8);
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
    cpu.tr.limit = 0x1000;
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

        let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, 0, 0);

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
fn v86_call_out_is_refused_before_the_tss_probe() {
    // The other half of the refused predicate, isolated: IOPL is 3 here, so the `CPL > IOPL` half
    // is FALSE and only `is_v86_mode()` can produce the refusal.
    let (mut cpu, mut bus) = paged_ring3_io_cpu(false);
    cpu.registers.eflags = 0x202 | FLAG_VM | (3 << 12);
    assert_eq!(cpu.current_privilege_level(), 3);
    assert_eq!(cpu.iopl(), 3);
    let before = cpu.registers.clone();

    let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, 0, 0);

    assert!(status < 0, "a V86 task must be refused");
    assert_eq!(before, cpu.registers);
    assert!(page_walk_writes(&bus).is_empty());
    assert_eq!(bus.last_read_io_core_clocks_so_far, None);
}

#[test]
fn the_emitted_slot_takes_the_abnormal_exit_when_the_privilege_state_is_refused() {
    // The guard wired through the EMITTED path, not just the helper.
    let (mut fixture, mut interpreter, mut interpreter_bus) = slot_block_with(
        |bus| {
            bus.lazy_io_reads = true;
            bus.io_read_value = Some(0x5a);
        },
        |cpu| {
            // CPL 3 with IOPL 0 and a zero-limit TSS: the interpreter would consult the bitmap,
            // so the helper must refuse.
            cpu.cpl = 3;
            cpu.tr.limit = 0;
        },
    );

    let retired = fixture.cpu.perf_counters().jit_direct_insns;
    assert!(
        fixture
            .cpu
            .try_run_direct_block_for_test(&mut fixture.bus, fixture.block)
            .unwrap()
    );
    interpreter.cycle(&mut interpreter_bus).unwrap();

    assert_eq!(
        fixture.cpu.perf_counters().jit_direct_insns - retired,
        1,
        "only the prefix may retire"
    );
    assert_eq!(fixture.cpu.registers, interpreter.registers);
    assert_eq!(fixture.cpu.registers.eax(), 0xdead_beef, "AL untouched");
    assert_eq!(fixture.cpu.elapsed_clocks, interpreter.elapsed_clocks);
    let stalls = fixture.cpu.direct_stall_snapshot();
    assert_eq!(stalls.side_exit_callout_abnormal, 1);
    assert_eq!(
        stalls.callout_executed, 1,
        "a refusal still counts as an executed call-out, so the ratio has a denominator"
    );
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

    let status = jit::direct::port_read_al_dx_for_test(&mut cpu, &mut bus, prefix_raw, prefix_fp);

    assert!(status >= 0);
    assert_eq!(bus.last_read_io_core_clocks_so_far, Some(100 + expected));
    // Still a preview: neither carry may be consumed, because the block still owes both charges.
    assert_eq!(
        cpu.timing_rem, 0,
        "preview scaling consumed the integer carry"
    );
    assert_eq!(cpu.fp_rem, 0, "preview scaling consumed the float carry");
}
