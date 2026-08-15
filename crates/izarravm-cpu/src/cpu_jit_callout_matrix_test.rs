// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Phase 5 Task 2: the differential matrix over the interpreter CALL-OUT slot.
//!
//! The mechanism's own cover lives in `cpu_jit_callout_test.rs` (the helper contract, the two
//! privilege gates, the three exit shapes). This file is the SHAPE matrix around it: what happens
//! when the call-out is one of several slots, the last slot, one of four, repeated across
//! executions against a device whose answer changes, composed with a mutable imm32 lane, entered
//! through a chain rather than the dispatcher, and pressed against the run loop's cap.
//!
//! Every row compares a native execution against a BLOCK-FREE interpreter running the same guest
//! bytes from the same state, on registers (EIP included), lazy flags, EFLAGS, core clocks, bus
//! clocks, guest RAM and -- wherever the fixture is driven through the run loop -- the
//! device-visible read order and timestamps.
//!
//! Two drivers, and the difference matters:
//!
//! * `run_block` enters one installed block directly and steps the interpreter twin the matching
//!   number of times. `CpuGsw::cycle` resets `core_clocks_so_far` to zero before every
//!   instruction, so this driver can compare the device's read ORDER but not its TIMESTAMPS.
//! * `run_loop` drives both roles through `run_straight_line`, which is what maintains
//!   `core_clocks_so_far` across a run. Timestamps are comparable there, and so are the cap and
//!   step-break boundaries, because both roles go through the same loop.
//!
//! The call-out is MID-BLOCK in every row except the one whose whole subject is the last slot. An
//! opcode at a block's entry slot parks the block on the interpreter, so an entry-position fixture
//! certifies nothing.
//!
//! Mutation record for this matrix (verified by hand, both restored):
//! * making the scripted device STICKY (`TestBus::read_io` returning `io_read_sequence[0]` forever
//!   instead of advancing the cursor) fails four rows, and fails
//!   `a_varying_device_is_read_fresh_on_every_native_re_entry` on exactly its subject -- "round 1:
//!   the block must see the CURRENT device value";
//! * dropping the NATIVE role's lane patch from the composition row (patching only the
//!   interpreter's memory) fails `a_lane_patch_and_a_call_out_compose_in_one_block` at the first
//!   patched round: no lane write is absorbed, so the fixture cannot silently compare two blocks
//!   that were never patched.
//!
//! KNOWN FAILING, deliberately, pending a production decision: the two rows that accumulate enough
//! call-outs for a per-slot clock error to cross the persona's scaling denominator
//! (`a_varying_device_is_read_fresh_on_every_native_re_entry` and
//! `a_block_takes_exactly_the_call_out_slot_cap_and_splits_at_the_next`) fail on CORE CLOCKS. A
//! call-out slot is charged its runtime clocks by the helper AND `DirectKind::raw_clocks`'s `_ => 2`
//! default statically, so every native `IN AL,DX` costs the guest two raw core clocks more than
//! the interpreter charges. `DirectKind::CallOut` needs an explicit `=> 0` arm, the way
//! `DirectKind::X87` has one. Do not weaken these two assertions to make the suite green.

use super::*;

/// Any port works; 0x3da is the VGA input-status register doom polls, which is the idiom the
/// slice was built for.
const PORT: u16 = 0x03da;
/// `IN AL, DX`, the phase's only call-out opcode.
const IN_AL_DX: u8 = 0xec;
const MOV_ESI_ESI: [u8; 2] = [0x89, 0xf6];
const MOV_EDI_EDI: [u8; 2] = [0x89, 0xff];
const HLT: u8 = 0xf4;

/// The two roles of one differential, plus the shape facts the block came out with.
struct Roles {
    native: CpuGsw,
    native_bus: TestBus,
    interp: CpuGsw,
    interp_bus: TestBus,
    block: jit::direct::CompiledBlock,
    instructions: u8,
    callout_slots: u8,
}

/// Assemble `body` between a leading `mov esi,esi` and a `mov edi,edi` / `hlt` tail, returning the
/// bytes and the linear start of every instruction in it (the decode cache has to be warmed at
/// each one before the compile walk can see them).
fn program(body: &[&[u8]]) -> (Vec<u8>, Vec<u32>) {
    program_with_tail(body, true)
}

/// As `program`, but `tail` decides whether the `mov edi,edi` is there. Without it the last
/// element of `body` is the block's LAST slot, which is the one row that wants exactly that.
fn program_with_tail(body: &[&[u8]], tail: bool) -> (Vec<u8>, Vec<u32>) {
    let mut code = Vec::new();
    let mut starts = Vec::new();
    let push = |code: &mut Vec<u8>, starts: &mut Vec<u32>, bytes: &[u8]| {
        starts.push(ENTRY + code.len() as u32);
        code.extend_from_slice(bytes);
    };
    push(&mut code, &mut starts, &MOV_ESI_ESI);
    for piece in body {
        push(&mut code, &mut starts, piece);
    }
    if tail {
        push(&mut code, &mut starts, &MOV_EDI_EDI);
    }
    // The HLT is a hard boundary, so the block is exactly the instructions before it.
    push(&mut code, &mut starts, &[HLT]);
    (code, starts)
}

/// Compile and install the block at `ENTRY` on the native role, warm the same decode lines on the
/// interpreter role, and hand both back armed.
///
/// `configure_cpu` runs on BOTH roles BEFORE the compile: privilege state is sealed into the block
/// and re-checked at entry, so a block compiled at CPL 0 is simply not run at CPL 3 and the
/// fixture would certify nothing.
fn build(
    code: &[u8],
    starts: &[u32],
    configure_bus: impl Fn(&mut TestBus),
    configure_cpu: impl Fn(&mut CpuGsw),
) -> Roles {
    let mut memory = vec![0u8; 0x5000];
    // A NOP before the entry, so a run-loop-driven role reaches ENTRY as a CONTINUATION. The run
    // loop always interprets its first instruction, so a role started AT the entry would never
    // dispatch the block.
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);

    let mut native = flat_cpu();
    let mut interp = flat_cpu();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        configure_bus(bus);
    }
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interp, &mut interp_bus),
    ] {
        configure_cpu(cpu);
        cpu.registers.set_esp(STACK_TOP);
        cpu.set_eip(ENTRY - 1);
        cpu.fetch_decoded(bus, ENTRY - 1).unwrap();
        for &linear in starts {
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
    let instructions = compilation.span.instructions;
    let callout_slots = compilation.callout_slots;
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = native.jit_direct.block(id).expect("live block");

    let mut roles = Roles {
        native,
        native_bus,
        interp,
        interp_bus,
        block,
        instructions,
        callout_slots,
    };
    arm(&mut roles, ENTRY);
    roles
}

/// Put both roles into the same architectural state at `eip` and wipe every observation channel.
fn arm(roles: &mut Roles, eip: u32) {
    for cpu in [&mut roles.native, &mut roles.interp] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(STACK_TOP);
        cpu.registers.set_edx(u32::from(PORT));
        cpu.registers.set_eax(0xdead_beef);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.set_eip(eip);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut roles.native_bus, &mut roles.interp_bus] {
        bus.trace = BusTrace::default();
        bus.io_reads.clear();
        bus.io_read_cursor = 0;
    }
}

/// Enter the installed block once and step the interpreter twin `interpreted` times. Returns
/// whether the block was entered at all (`false` is the dispatch gate's `NotRun`).
fn run_block(roles: &mut Roles, interpreted: usize) -> bool {
    let entered = roles
        .native
        .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
        .unwrap();
    for _ in 0..interpreted {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    entered
}

/// Turn the native role's dispatcher on, so a run-loop-driven fixture actually reaches the
/// installed block. The interpreter role is left alone: auto-admit is off by default, which is
/// exactly the block-free reference this matrix compares against.
fn enable_dispatch(roles: &mut Roles) {
    roles.native.set_jit_auto_admit(true);
    roles.native.jit_direct.set_admission_heat_for_test(1);
}

/// One capped run of the run loop on both roles.
fn run_loop_once(roles: &mut Roles, cap: u64) -> (CycleOutcome, CycleOutcome) {
    let native = roles
        .native
        .run_straight_line(&mut roles.native_bus, cap)
        .unwrap();
    let interp = roles
        .interp
        .run_straight_line(&mut roles.interp_bus, cap)
        .unwrap();
    (native, interp)
}

/// Drive the native role alone through the run loop until it halts. Used to warm a fall-through
/// link before the measured pass.
fn run_native_to_halt(roles: &mut Roles) {
    for _ in 0..64 {
        if roles
            .native
            .run_straight_line(&mut roles.native_bus, u64::MAX)
            .unwrap()
            .halted
        {
            return;
        }
    }
    panic!("the native role hung");
}

/// Drive both roles through the run loop until each halts.
fn run_loop_to_halt(roles: &mut Roles) {
    for cpu_bus in 0..2 {
        for _ in 0..64 {
            let (cpu, bus) = if cpu_bus == 0 {
                (&mut roles.native, &mut roles.native_bus)
            } else {
                (&mut roles.interp, &mut roles.interp_bus)
            };
            if cpu.run_straight_line(bus, u64::MAX).unwrap().halted {
                break;
            }
        }
    }
    assert!(roles.native.halted && roles.interp.halted, "a role hung");
}

/// The architectural axes, on every row.
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

/// The ORDER of the device's reads, without their timestamps. Everything a block-driven fixture
/// can say about the device, since `cycle` zeroes `core_clocks_so_far` on the twin.
fn compare_device_order(roles: &Roles, context: &str) {
    let ports = |bus: &TestBus| bus.io_reads.iter().map(|&(p, _)| p).collect::<Vec<_>>();
    assert_eq!(
        ports(&roles.native_bus),
        ports(&roles.interp_bus),
        "{context}: device read order"
    );
}

/// Read order AND timestamps. Only meaningful when both roles went through the run loop.
fn compare_device(roles: &Roles, context: &str) {
    assert_eq!(
        roles.native_bus.io_reads, roles.interp_bus.io_reads,
        "{context}: device read order and timestamps"
    );
}

/// Values a scripted device hands back, one per read. Distinct and non-zero, so a stale AL is
/// visible: a fixture whose device always answered 0 would pass against a reload that dropped AL
/// entirely.
const DEVICE_VALUES: [u32; 6] = [0x5a, 0xa5, 0x01, 0xfe, 0x7c, 0x33];

// ---------------------------------------------------------------------------------------------
// Row 1: a device whose answer CHANGES between executions of the same native block.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_varying_device_is_read_fresh_on_every_native_re_entry() {
    // The one thing a constant-valued device cannot separate: "the block called the device again"
    // from "the block kept the first byte". Both roles read the same scripted sequence, so a
    // native block that skipped a read, cached a value, or charged the port's clocks once instead
    // of once per execution diverges on AL, on the read log, or on core clocks.
    //
    // Lazy reads, so the bus asks for no step break and each execution runs all three slots --
    // the shape the whole slice exists for.
    let (code, starts) = program(&[&[IN_AL_DX]]);
    let mut roles = build(
        &code,
        &starts,
        |bus| {
            bus.lazy_io_reads = true;
            bus.io_read_sequence = DEVICE_VALUES.to_vec();
        },
        |_| {},
    );
    assert_eq!(
        roles.instructions, 3,
        "the block must cover all three slots"
    );
    assert_eq!(roles.callout_slots, 1);

    let mut previous_clocks = 0u64;
    for (round, &value) in DEVICE_VALUES.iter().enumerate() {
        let context = format!("round {round}");
        // NOT re-armed between rounds: the clocks accumulate, so a charge that happened once
        // instead of once per execution shows up as a growing divergence rather than a constant
        // offset. Only EIP and the halt latch go back.
        for cpu in [&mut roles.native, &mut roles.interp] {
            cpu.set_eip(ENTRY);
            cpu.halted = false;
        }
        let stalls_before = roles.native.direct_stall_snapshot().callout_executed;
        assert!(run_block(&mut roles, 3), "{context}: block did not run");

        assert_eq!(
            roles.native.direct_stall_snapshot().callout_executed - stalls_before,
            1,
            "{context}: the call-out must be executed once per block execution"
        );
        assert_eq!(
            roles.native.registers.eax() & 0xff,
            value,
            "{context}: the block must see the CURRENT device value"
        );
        assert!(
            roles.native.elapsed_clocks > previous_clocks,
            "{context}: every execution must charge fresh clocks"
        );
        previous_clocks = roles.native.elapsed_clocks;
        compare_state(&roles, &context);
        compare_device_order(&roles, &context);
    }
    assert_eq!(
        roles.native_bus.io_reads.len(),
        DEVICE_VALUES.len(),
        "one device read per execution, no more and no fewer"
    );
}

// ---------------------------------------------------------------------------------------------
// Row 4: the call-out as the LAST slot.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_call_out_in_the_last_slot_matches_the_interpreter() {
    // Slot accounting and block-exit accounting meet here: the call-out's RUNTIME clock deposit is
    // the last thing added to the lane before the exit path scales it, and there is no following
    // slot to reload homes for. A reload or a deposit emitted only on the "there is more to do"
    // path would pass every mid-block row and fail this one.
    for lazy in [false, true] {
        let context = format!("last slot lazy={lazy}");
        let (code, starts) = program_with_tail(&[&MOV_EDI_EDI, &[IN_AL_DX]], false);
        let mut roles = build(
            &code,
            &starts,
            |bus| {
                bus.lazy_io_reads = lazy;
                bus.io_read_value = Some(DEVICE_VALUES[0]);
            },
            |_| {},
        );
        assert_eq!(roles.instructions, 3, "{context}: block shape");
        assert_eq!(roles.callout_slots, 1);

        assert!(run_block(&mut roles, 3), "{context}: block did not run");
        assert_eq!(
            roles.native.registers.eax() & 0xff,
            DEVICE_VALUES[0],
            "{context}: the port byte must land in AL"
        );
        compare_state(&roles, &context);
        compare_device_order(&roles, &context);
        // A step break leaves through the side exit; a lazy read runs on to the block's own end.
        let stalls = roles.native.direct_stall_snapshot();
        assert_eq!(stalls.callout_executed, 1, "{context}");
        assert_eq!(stalls.side_exit_callout_abnormal, 0, "{context}");
        assert_eq!(
            stalls.side_exit_callout_step_break,
            u64::from(!lazy),
            "{context}: the step break must mirror the bus"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Row 5: several call-outs in one block, and the slot cap.
// ---------------------------------------------------------------------------------------------

#[test]
fn two_call_outs_in_one_block_match_the_interpreter() {
    // Two slots means the SECOND one's helper sees a raw-clock prefix that already contains the
    // first one's runtime deposit. A call site that passed the block's static prefix alone would
    // hand the device a timestamp short by the first port read's charge, which the run-loop row
    // below is what catches; here the subject is that both reads happen, in order, and the
    // charge is doubled.
    let (code, starts) = program(&[&[IN_AL_DX], &[IN_AL_DX]]);
    let mut roles = build(
        &code,
        &starts,
        |bus| {
            bus.lazy_io_reads = true;
            bus.io_read_sequence = DEVICE_VALUES.to_vec();
        },
        |_| {},
    );
    assert_eq!(roles.instructions, 4);
    assert_eq!(roles.callout_slots, 2);

    assert!(run_block(&mut roles, 4), "block did not run");
    assert_eq!(
        roles.native.registers.eax() & 0xff,
        DEVICE_VALUES[1],
        "the SECOND read's byte must be the one left in AL"
    );
    assert_eq!(roles.native.direct_stall_snapshot().callout_executed, 2);
    compare_state(&roles, "two call-outs");
    compare_device_order(&roles, "two call-outs");
}

#[test]
fn a_block_takes_exactly_the_call_out_slot_cap_and_splits_at_the_next() {
    // `MAX_BLOCK_CALLOUT_SLOTS` is a BUDGET bound (each slot widens `compute_iteration_upper`), so
    // it is enforced by stopping the compile walk rather than by refusing the block. Four fit; the
    // fifth ends the block before itself, and the four that did fit still run.
    let four = [
        &[IN_AL_DX][..],
        &[IN_AL_DX][..],
        &[IN_AL_DX][..],
        &[IN_AL_DX][..],
    ];
    let (code, starts) = program(&four);
    let mut roles = build(
        &code,
        &starts,
        |bus| {
            bus.lazy_io_reads = true;
            bus.io_read_sequence = DEVICE_VALUES.to_vec();
        },
        |_| {},
    );
    assert_eq!(roles.instructions, 6, "mov + four INs + mov");
    assert_eq!(
        u32::from(roles.callout_slots),
        u32::from(jit::direct::MAX_BLOCK_CALLOUT_SLOTS),
        "the block must hold exactly the cap"
    );
    assert!(run_block(&mut roles, 6), "the four-slot block did not run");
    assert_eq!(roles.native.direct_stall_snapshot().callout_executed, 4);
    assert_eq!(roles.native.registers.eax() & 0xff, DEVICE_VALUES[3]);
    compare_state(&roles, "four call-outs");
    compare_device_order(&roles, "four call-outs");

    // The fifth: the walk stops BEFORE it, so the block is the `mov esi,esi` plus four INs and
    // ends one instruction short of where the four-slot block ended.
    let five = [
        &[IN_AL_DX][..],
        &[IN_AL_DX][..],
        &[IN_AL_DX][..],
        &[IN_AL_DX][..],
        &[IN_AL_DX][..],
    ];
    let (code, starts) = program(&five);
    let mut split = build(
        &code,
        &starts,
        |bus| {
            bus.lazy_io_reads = true;
            bus.io_read_sequence = DEVICE_VALUES.to_vec();
        },
        |_| {},
    );
    assert_eq!(
        split.instructions, 5,
        "the fifth call-out must SPLIT the block, not join it"
    );
    assert_eq!(
        u32::from(split.callout_slots),
        u32::from(jit::direct::MAX_BLOCK_CALLOUT_SLOTS)
    );
    assert!(run_block(&mut split, 5), "the split block did not run");
    assert_eq!(split.native.direct_stall_snapshot().callout_executed, 4);
    assert_eq!(
        split.native.registers.eip,
        ENTRY + 6,
        "the block must end AT the fifth call-out, leaving it to the interpreter"
    );
    compare_state(&split, "split at the fifth call-out");
    compare_device_order(&split, "split at the fifth call-out");
}

// ---------------------------------------------------------------------------------------------
// Row 7: a step-breaking device with slots left after it.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_step_break_ends_the_run_and_the_interpreter_continues_identically() {
    // Quake's shape: the port touches time-dependent device state, so the helper reports the step
    // break and the run ends at the boundary AFTER the IN with two slots un-executed. The parent
    // module pins that boundary; what this row adds is the CONTINUATION -- both roles are driven
    // to HLT through the run loop from there, so the un-executed slots, the device timestamps and
    // every clock have to agree end to end.
    let (code, starts) = program(&[&[IN_AL_DX], &MOV_EDI_EDI]);
    let mut roles = build(
        &code,
        &starts,
        |bus| {
            bus.io_read_sequence = DEVICE_VALUES.to_vec();
        },
        |_| {},
    );
    assert_eq!(roles.instructions, 4);
    assert_eq!(roles.callout_slots, 1);
    enable_dispatch(&mut roles);
    arm(&mut roles, ENTRY - 1);

    run_loop_to_halt(&mut roles);

    let stalls = roles.native.direct_stall_snapshot();
    assert_eq!(
        stalls.side_exit_callout_step_break, 1,
        "the step break must have fired once"
    );
    assert_eq!(stalls.side_exit_callout_abnormal, 0);
    assert!(
        roles.native.perf_counters().jit_direct_insns >= 2,
        "the prefix and the call-out must have retired natively"
    );
    assert_eq!(roles.native.registers.eax() & 0xff, DEVICE_VALUES[0]);
    compare_state(&roles, "step break continuation");
    compare_device(&roles, "step break continuation");
}

// ---------------------------------------------------------------------------------------------
// Row 3: the run loop's cap, swept across the call-out slot.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_cap_boundary_lands_identically_with_and_without_the_block() {
    // The cap test steps along a non-decreasing per-instruction coordinate, so sweeping the cap
    // from below the block's first slot to past its last walks the break through every position
    // including the call-out's. What has to hold is the exact-clocks contract's consequence: a
    // block is admitted only when the remaining budget covers `compute_iteration_upper`, which
    // dominates the block's real charge INCLUDING the call-out's runtime deposit -- so a run that
    // enters the block cannot overshoot a cap the interpreter would have respected, and a run
    // that cannot afford the block falls back to the very interpretation it is compared against.
    //
    // A call-out term missing from `compute_iteration_upper` shows up here as a native run that
    // retired more instructions, or ran further past the cap, than the interpreter.
    let (code, starts) = program(&[&[IN_AL_DX], &MOV_EDI_EDI]);
    for cap in 0..40u64 {
        let context = format!("cap={cap}");
        let mut roles = build(
            &code,
            &starts,
            |bus| {
                bus.lazy_io_reads = true;
                bus.io_read_sequence = DEVICE_VALUES.to_vec();
            },
            |_| {},
        );
        enable_dispatch(&mut roles);
        arm(&mut roles, ENTRY - 1);
        roles.native.reset_perf_counters();
        roles.interp.reset_perf_counters();

        let (native_outcome, interp_outcome) = run_loop_once(&mut roles, cap);

        assert_eq!(
            native_outcome.core_clocks, interp_outcome.core_clocks,
            "{context}: the run's charge"
        );
        assert_eq!(
            native_outcome.halted, interp_outcome.halted,
            "{context}: halt"
        );
        let native_perf = roles.native.perf_counters();
        let interp_perf = roles.interp.perf_counters();
        assert_eq!(
            native_perf.instructions, interp_perf.instructions,
            "{context}: instructions retired"
        );
        assert_eq!(
            native_perf.brk_cap, interp_perf.brk_cap,
            "{context}: brk_cap"
        );
        compare_state(&roles, &context);
        compare_device(&roles, &context);
    }
}

// ---------------------------------------------------------------------------------------------
// Row 8: a call-out block entered through a CHAIN rather than the dispatcher.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_chained_entry_folds_its_prefix_into_the_devices_timestamp() {
    // The parent module's float-lane test drives the helper directly with a synthetic prefix. This
    // is the emitted, INTEGER case: a real linked transfer into a call-out block, where the prefix
    // the device must see was deposited by the block BEFORE this one.
    //
    // Block A is `add eax,1` / `test eax,eax` / `jz +N` with the branch NOT taken, so it links by
    // fall-through into block B, which carries the call-out. The device's timestamp therefore has
    // to contain A's charge as well as B's prefix, and the block-free interpreter -- which reaches
    // the same instruction with the same running total -- is what says so.
    const A: u32 = ENTRY;
    let mut code = Vec::new();
    let mut starts = Vec::new();
    let push = |code: &mut Vec<u8>, starts: &mut Vec<u32>, bytes: &[u8]| {
        starts.push(ENTRY + code.len() as u32);
        code.extend_from_slice(bytes);
    };
    // Block A: three slots and a not-taken conditional, the shape that falls through into B.
    push(&mut code, &mut starts, &MOV_ESI_ESI);
    push(&mut code, &mut starts, &[0x83, 0xc0, 0x01]); // add eax,1
    push(&mut code, &mut starts, &[0x85, 0xc0]); // test eax,eax
    push(&mut code, &mut starts, &[0x74, 0x06]); // jz +6 (never taken: EAX is 1)
    let b = ENTRY + code.len() as u32;
    // Block B: the call-out, mid-block as always.
    push(&mut code, &mut starts, &MOV_EDI_EDI);
    push(&mut code, &mut starts, &[IN_AL_DX]);
    push(&mut code, &mut starts, &MOV_ESI_ESI);
    push(&mut code, &mut starts, &[HLT]);

    let mut roles = build(
        &code,
        &starts,
        |bus| {
            bus.lazy_io_reads = true;
            bus.io_read_sequence = DEVICE_VALUES.to_vec();
        },
        |_| {},
    );
    assert_eq!(roles.instructions, 4, "block A must end at its branch");
    assert_eq!(
        roles.callout_slots, 0,
        "the call-out belongs to the SECOND block"
    );
    // Install block B as well, so the fall-through link can resolve to it.
    let key = jit::direct::key_for(&roles.native, b, true).expect("block B key");
    assert!(matches!(
        roles.native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut roles.native, b, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        _ => panic!("block B must compile"),
    };
    assert_eq!(compilation.span.instructions, 3);
    assert_eq!(compilation.callout_slots, 1);
    roles
        .native
        .jit_direct
        .install(&compilation)
        .expect("block B installs");
    // `A` is only used to make the fall-through relationship legible at the call site.
    assert_eq!(A, ENTRY);

    enable_dispatch(&mut roles);
    // A fall-through link is CREATED by the first unresolved exit and TAKEN from then on, so the
    // native role needs one warm traversal before the chain exists at all. Without it this row
    // would silently measure two dispatcher entries and call them a chain.
    arm(&mut roles, ENTRY - 1);
    run_native_to_halt(&mut roles);
    arm(&mut roles, ENTRY - 1);
    let transfers = roles.native.perf_counters().jit_direct_linked_transfers;
    let entries = roles.native.perf_counters().jit_direct_entries;
    let executed = roles.native.direct_stall_snapshot().callout_executed;

    run_loop_to_halt(&mut roles);

    assert_eq!(
        roles.native.perf_counters().jit_direct_linked_transfers - transfers,
        1,
        "block B must be reached through the CHAIN, not through a second dispatcher entry"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_entries - entries,
        1,
        "one dispatcher entry for the whole chain"
    );
    assert_eq!(
        roles.native.direct_stall_snapshot().callout_executed - executed,
        1
    );
    assert_eq!(roles.native.registers.eax() & 0xff, DEVICE_VALUES[0]);
    let (_, timestamp) = roles.native_bus.io_reads[0];
    assert!(
        timestamp > 0,
        "a chained entry's device timestamp must contain the prefix, not the block-entry zero"
    );
    compare_state(&roles, "chained entry");
    compare_device(&roles, "chained entry");
}

// ---------------------------------------------------------------------------------------------
// Row 2: the IO-permission denial, through the EMITTED path, all the way to the #GP.
// ---------------------------------------------------------------------------------------------

/// A GDT and an IDT inside the fixture's memory, so a #GP raised at CPL 3 has somewhere to land
/// WITHOUT a privilege change: the gate's target selector is the same DPL-3 code segment the
/// guest is already running, so delivery pushes onto the current stack and no TSS stack switch is
/// involved. That keeps the fixture about the denied port and nothing else.
const GDT_BASE: u32 = 0x1000;
const IDT_BASE: u32 = 0x1800;
const GP_HANDLER: u32 = 0x2000;
const GP_VECTOR: u32 = 13;

fn seed_protected_tables(memory: &mut [u8]) {
    let mut descriptor = |index: usize, high: u32| {
        let at = GDT_BASE as usize + index * 8;
        memory[at..at + 4].copy_from_slice(&0x0000_ffffu32.to_le_bytes());
        memory[at + 4..at + 8].copy_from_slice(&high.to_le_bytes());
    };
    // Selector 0x08/0x0b: base 0, limit 4 GB, granular, 32-bit, DPL 3 code (access 0xfb).
    descriptor(1, 0x00cf_fb00);
    // Selector 0x10/0x13: the same as data (access 0xf3).
    descriptor(2, 0x00cf_f300);
    // A 32-bit trap gate for #GP, present, DPL 3, targeting the DPL-3 code selector.
    let gate = IDT_BASE as usize + GP_VECTOR as usize * 8;
    let low = (0x000b_u32 << 16) | (GP_HANDLER & 0xffff);
    let high = (GP_HANDLER & 0xffff_0000) | (0xef_u32 << 8);
    memory[gate..gate + 4].copy_from_slice(&low.to_le_bytes());
    memory[gate + 4..gate + 8].copy_from_slice(&high.to_le_bytes());
    // The handler is a NOP, not a HLT: it runs at CPL 3, where HLT is itself a #GP and the
    // fixture would fault forever. The run ends at the delivery anyway -- a control transfer is
    // not continuable -- so one run is the whole observation.
    memory[GP_HANDLER as usize] = 0x90;
}

#[test]
fn a_denied_port_delivers_the_same_general_protection_fault_with_the_block_installed() {
    // The dispatch gate's own cover (parent module) stops at `NotRun`. This row carries it to the
    // end: with the block installed and refused, the INTERPRETER executes the IN, its permission
    // check consults a zero-limit TSS, and the #GP is delivered. The block-free role does exactly
    // the same thing from the same state, so every byte of the fault frame -- the pushed EFLAGS,
    // CS, EIP and error code, the handler EIP, ESP -- plus the clocks and the bus trace have to
    // agree. A gate that let the block run instead would show up as a device read the block-free
    // role never made.
    let (code, starts) = program(&[&[IN_AL_DX]]);
    let mut roles = build(
        &code,
        &starts,
        |bus| {
            bus.lazy_io_reads = true;
            bus.io_read_sequence = DEVICE_VALUES.to_vec();
        },
        |cpu| {
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
            // CPL is tracked as CPU state, not re-derived from CS on every read. With IOPL 0 this
            // is the half of the refused predicate a protected-mode guest can reach, and a
            // zero-limit TSS denies every port because the bitmap byte is already past the limit.
            cpu.cpl = 3;
            cpu.tr.base = 0;
            cpu.tr.limit = 0;
            cpu.gdtr = DescriptorTable {
                base: GDT_BASE,
                limit: 0x1f,
            };
            cpu.idtr = DescriptorTable {
                base: IDT_BASE,
                limit: 0xff,
            };
        },
    );
    assert_eq!(roles.instructions, 3);
    assert_eq!(roles.callout_slots, 1);
    for bus in [&mut roles.native_bus, &mut roles.interp_bus] {
        seed_protected_tables(&mut bus.memory);
    }
    enable_dispatch(&mut roles);
    arm(&mut roles, ENTRY - 1);

    let (native_outcome, interp_outcome) = run_loop_once(&mut roles, u64::MAX);
    assert_eq!(native_outcome.core_clocks, interp_outcome.core_clocks);
    assert_eq!(native_outcome.halted, interp_outcome.halted);

    // GOVERNED as of round 2: this first pass is the governor's TRIAL, so the helper is entered
    // once and refuses in phase P with nothing charged and nothing traced -- which is why the
    // guest-visible identity above still holds byte for byte.
    let stalls = roles.native.direct_stall_snapshot();
    assert_eq!(stalls.callout_governor_trials, 1);
    assert_eq!(stalls.callout_executed, 1, "the trial reached the helper");
    assert_eq!(stalls.side_exit_callout_abnormal, 1);
    assert_eq!(
        stalls.reject_callout_privileged, 0,
        "the trial entry itself is not a refusal"
    );
    // Non-vacuity: the fault really was delivered, and no device was ever addressed.
    assert_eq!(
        roles.native.registers.eip, GP_HANDLER,
        "the #GP must have been delivered to its handler"
    );
    assert!(
        roles.native_bus.io_reads.is_empty(),
        "a denied port must never reach the device"
    );
    assert_eq!(
        roles.native.registers.eax(),
        0xdead_beef,
        "AL must be untouched"
    );

    // The steady state the dispatch gate exists for: the trial classified the block `Denied`, so
    // every later pass is refused at head and the fault is delivered identically with nothing
    // paying for the spill, the call and the reload.
    for pass in 1..3 {
        arm(&mut roles, ENTRY - 1);
        let (native_outcome, interp_outcome) = run_loop_once(&mut roles, u64::MAX);
        assert_eq!(
            native_outcome.core_clocks, interp_outcome.core_clocks,
            "pass {pass}: core clocks"
        );
        let stalls = roles.native.direct_stall_snapshot();
        assert_eq!(
            stalls.reject_callout_privileged, pass,
            "pass {pass}: the dispatch gate must have refused the block"
        );
        assert_eq!(
            stalls.callout_executed, 1,
            "pass {pass}: nothing may pay for the spill, the call and the reload"
        );
        assert_eq!(stalls.callout_governor_trials, 1, "pass {pass}");
        assert_eq!(
            roles.native.registers.eip, GP_HANDLER,
            "pass {pass}: the #GP must still be delivered"
        );
    }
    assert_eq!(
        roles.native.registers.esp(),
        STACK_TOP - 16,
        "EFLAGS, CS, EIP and an error code, on the current stack"
    );
    compare_state(&roles, "denied port");
    compare_device(&roles, "denied port");
}

// ---------------------------------------------------------------------------------------------
// Row 6: a mutable imm32 lane and a call-out in the SAME block.
// ---------------------------------------------------------------------------------------------

/// Entry for the lane composition fixture. Chosen so the ADD's immediate field lands 4-byte
/// aligned, which is the alignment doom's patch store has and the one that takes the FastMap write
/// path rather than the fragment fallback.
const LANE_ENTRY: u32 = 0x500;
/// The lane: the `add ebp, imm32`'s immediate, two bytes into a six-byte instruction that itself
/// starts two bytes into the block.
const LANE: u32 = LANE_ENTRY + 4;

/// `mov esi,esi` / `add ebp,imm32` / `in al,dx` / `mov edi,edi` / `hlt`: a lane and a call-out in
/// one block, both mid-block.
fn lane_and_call_out_image(imm: u32) -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    let mut code = Vec::new();
    code.extend_from_slice(&MOV_ESI_ESI);
    code.extend_from_slice(&[0x81, 0xc5]);
    code.extend_from_slice(&imm.to_le_bytes());
    code.push(IN_AL_DX);
    code.extend_from_slice(&MOV_EDI_EDI);
    code.push(HLT);
    memory[LANE_ENTRY as usize..LANE_ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

fn lane_and_call_out_starts() -> [u32; 5] {
    [
        LANE_ENTRY,
        LANE_ENTRY + 2,
        LANE_ENTRY + 8,
        LANE_ENTRY + 9,
        LANE_ENTRY + 11,
    ]
}

fn lane_cpu() -> CpuGsw {
    let mut cpu = flat_cpu();
    cpu.set_fast_map_enabled_for_test(true);
    cpu.set_eip(LANE_ENTRY);
    cpu
}

fn lane_bus(imm: u32) -> TestBus {
    let mut bus = TestBus::with_memory(lane_and_call_out_image(imm));
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    bus.lazy_io_reads = true;
    bus.io_read_sequence = DEVICE_VALUES.to_vec();
    bus
}

fn lane_arm(cpu: &mut CpuGsw, ebp: u32) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_ebp(ebp);
    cpu.registers.set_esp(STACK_TOP);
    cpu.registers.set_edx(u32::from(PORT));
    cpu.registers.set_eax(0xdead_beef);
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(LANE_ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

/// A guest dword store through the ordinary data-write path, so it reaches the SMC choke with the
/// physical address already resolved -- the same path a `mov [addr], reg` takes.
fn lane_store(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32, value: u32) {
    cpu.write_memory_bus_width(
        bus,
        SegmentIndex::Ds,
        linear,
        BusWidth::Dword,
        value,
        BusAccessKind::DataWrite,
    )
    .expect("fixture patch store");
}

/// Compile and install the lane-and-call-out block, asserting it really carries both.
fn lane_and_call_out_fixture(imm: u32) -> (CpuGsw, TestBus, jit::direct::BlockId) {
    let mut cpu = lane_cpu();
    let mut bus = lane_bus(imm);
    for &linear in &lane_and_call_out_starts() {
        cpu.set_eip(linear);
        cpu.fetch_decoded(&mut bus, linear).unwrap();
    }
    let key = jit::direct::key_for(&cpu, LANE_ENTRY, true).expect("entry key");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut cpu, LANE_ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        _ => panic!("the lane-and-call-out block must compile"),
    };
    assert_eq!(compilation.span.instructions, 4, "block shape");
    assert_eq!(compilation.callout_slots, 1, "the call-out slot");
    assert_eq!(
        compilation.imm_lane_count(),
        1,
        "the ADD did not take a lane; every assertion below would be vacuous"
    );
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("the block installs");
    (cpu, bus, id)
}

#[test]
fn a_lane_patch_and_a_call_out_compose_in_one_block() {
    // The interaction the Task 1 review flagged: re-formation moved eleven doom blocks off their
    // lanes, so FORMATION can separate the two. This row is about the MECHANISMS -- when a block
    // does hold both, a guest patch of the immediate must keep the block alive AND the next native
    // entry must run the call-out with the new immediate visible.
    //
    // The reference is a block-free interpreter on the same bytes: it re-decodes whatever the
    // patch left in memory, which is the definition of what the guest should have seen.
    let patches = [0x0000_0001u32, 0x7fff_ffff, 0xffff_ffff, 0x0002_0000];
    let (mut native, mut native_bus, id) = lane_and_call_out_fixture(patches[0]);
    let mut interp = lane_cpu();
    let mut interp_bus = lane_bus(patches[0]);
    for &linear in &lane_and_call_out_starts() {
        interp.set_eip(linear);
        interp.fetch_decoded(&mut interp_bus, linear).unwrap();
    }

    for (round, &imm) in patches.iter().enumerate() {
        let context = format!("round {round} imm={imm:#010x}");
        if round != 0 {
            let accepts = native.perf_counters().smc_lane_accepts;
            lane_store(&mut native, &mut native_bus, LANE, imm);
            lane_store(&mut interp, &mut interp_bus, LANE, imm);
            assert_eq!(
                native.perf_counters().smc_lane_accepts - accepts,
                1,
                "{context}: the patch must be absorbed as a lane write"
            );
        }
        let ebp = 0x1234_5678u32.wrapping_mul(round as u32 + 1);
        lane_arm(&mut native, ebp);
        lane_arm(&mut interp, ebp);
        native_bus.trace = BusTrace::default();
        interp_bus.trace = BusTrace::default();

        let block = native
            .jit_direct
            .block(id)
            .expect("a lane write must not retire a call-out-bearing block");
        let executed = native.direct_stall_snapshot().callout_executed;
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap(),
            "{context}: the block must still be entered natively"
        );
        for _ in 0..4 {
            interp.cycle(&mut interp_bus).unwrap();
        }

        assert_eq!(
            native.direct_stall_snapshot().callout_executed - executed,
            1,
            "{context}: the call-out must run on the re-entry after the patch"
        );
        assert_eq!(
            native.registers.ebp(),
            ebp.wrapping_add(imm),
            "{context}: the lane must carry the CURRENT immediate"
        );
        assert_eq!(
            native.registers.eax() & 0xff,
            DEVICE_VALUES[round],
            "{context}: the call-out must see the current device value"
        );
        assert_eq!(native.registers, interp.registers, "{context}: registers");
        assert_eq!(
            native.pending_flags, interp.pending_flags,
            "{context}: lazy flags"
        );
        assert_eq!(native.eflags(), interp.eflags(), "{context}: EFLAGS");
        assert_eq!(native_bus.memory, interp_bus.memory, "{context}: guest RAM");
    }
    assert_eq!(
        native.perf_counters().smc_lane_accepts,
        (patches.len() - 1) as u64,
        "every patch after the first was a lane write"
    );
    assert_eq!(
        native.direct_stall_snapshot().callout_executed,
        patches.len() as u64,
        "one call-out per execution"
    );
}

#[test]
fn a_structural_write_retires_a_lane_and_call_out_block_that_a_lane_write_kept() {
    // The two halves of the same claim, on the same fixture: a write to the immediate's four bytes
    // is absorbed and the block survives; a write to the instruction's opcode bytes is structural
    // and retires it. A call-out slot changes neither answer -- which is the composition claim.
    let (mut cpu, mut bus, id) = lane_and_call_out_fixture(1);
    lane_store(&mut cpu, &mut bus, LANE, 0x0000_0020);
    assert_eq!(cpu.perf_counters().smc_lane_accepts, 1);
    assert!(
        cpu.jit_direct.block(id).is_some(),
        "a lane write must not retire a call-out-bearing block"
    );

    lane_store(&mut cpu, &mut bus, LANE_ENTRY, 0);
    assert_eq!(
        cpu.perf_counters().smc_lane_accepts,
        1,
        "a structural write is not a lane write"
    );
    assert!(
        cpu.jit_direct.block(id).is_none(),
        "a write to the block's opcode bytes must retire it, call-out or not"
    );
}

// ---------------------------------------------------------------------------------------------
// The MEMORY class: `0x60` PUSHAD and `0x61` POPAD.
//
// Every row here is mid-block against a block-free interpreter on the same bytes, and
// `compare_state` already covers guest RAM byte-for-byte -- which is the whole point for a helper
// that moves thirty-two bytes of stack.
// ---------------------------------------------------------------------------------------------

const PUSHAD: u8 = 0x60;
const POPAD: u8 = 0x61;
/// `sub cx, 4` at Word operand size -- the third member of the census's coupled prologue family,
/// forty-seven exits from PUSHAD and lowered NATIVELY rather than as a call-out.
const SUB_CX_IMM8: [u8; 4] = [0x66, 0x83, 0xe9, 0x04];

/// Put the pages a PUSHAD/POPAD frame touches into BOTH roles' fast maps.
///
/// `call_out_stack_frame_resident` refuses a frame whose pages the FastMap cannot serve, so
/// without this every row below would measure the REFUSAL rather than the mechanism. That is not a
/// fixture cheat: in production the interpreter's own direct-page path populates the map the first
/// time it touches the stack (`populate_fast_map_from_cached`), so a cold first PUSHAD refuses,
/// the interpreter executes it, and the next entry succeeds.
/// `a_pushad_whose_frame_is_not_resident_is_refused_and_left_to_the_interpreter` is the row that
/// measures the cold half deliberately.
///
/// Both roles, identically, so nothing the population does can move the differential.
fn warm_stack_frame_pages(roles: &mut Roles) {
    // One page either side of STACK_TOP, so a frame that crosses a page boundary is covered.
    warm_pages(
        roles,
        &[STACK_TOP.wrapping_sub(0x1000) & !0xfff, STACK_TOP & !0xfff],
    );
}

/// Arm the interpreter's FastMap serve gate on both roles and populate `pages` in both maps.
///
/// Population goes through REAL guest accesses rather than `FastMap::populate_write` directly, and
/// that is the whole point: `lookup_access` compares the entry's mapping epoch against
/// `data_write_pages.mapping_epoch()`, so an entry installed behind the DirectPageCache's back is
/// not servable. Driving a read and a same-value write through `read_memory_bus_width` /
/// `write_memory_bus_width` is exactly the path that populates the map in production, epochs
/// included. The write is same-value, so guest RAM does not move and no code watch can fire.
///
/// Both roles, identically, and the traces are wiped afterwards so the warm-up contributes nothing
/// to any differential.
fn warm_pages(roles: &mut Roles, pages: &[u32]) {
    for (cpu, bus) in [
        (&mut roles.native, &mut roles.native_bus),
        (&mut roles.interp, &mut roles.interp_bus),
    ] {
        cpu.set_fast_map_enabled_for_test(true);
        for &page in pages {
            let value = cpu
                .read_memory_bus_width(
                    bus,
                    SegmentIndex::Ss,
                    page,
                    BusWidth::Dword,
                    BusAccessKind::DataRead,
                )
                .expect("fixture warm read");
            cpu.write_memory_bus_width(
                bus,
                SegmentIndex::Ss,
                page,
                BusWidth::Dword,
                value,
                BusAccessKind::DataWrite,
            )
            .expect("fixture warm write");
        }
    }
    for bus in [&mut roles.native_bus, &mut roles.interp_bus] {
        bus.trace = BusTrace::default();
    }
}

/// Arm the serve gate WITHOUT populating anything, so a refusal is the residency clause rather
/// than the gate.
fn arm_fast_map_gate(roles: &mut Roles) {
    for cpu in [&mut roles.native, &mut roles.interp] {
        cpu.set_fast_map_enabled_for_test(true);
    }
}

/// Seed the eight GPRs with distinct, high-bit-bearing values on both roles, then set ESP.
///
/// Distinct values are what makes a PUSHAD that pushed the registers in the wrong ORDER visible in
/// guest RAM, and what makes a POPAD that loaded them into the wrong destinations visible in the
/// register compare. `arm` fills the file with zeroes, which would pass against both bugs.
fn seed_registers(roles: &mut Roles, esp: u32) {
    const SEED: [u32; 8] = [
        0x1111_0001,
        0x2222_0002,
        0x3333_0003,
        0x4444_0004,
        0xdead_dead, // overwritten by `esp` below; present so the array is eight wide
        0x6666_0006,
        0x7777_0007,
        0x8888_0008,
    ];
    for cpu in [&mut roles.native, &mut roles.interp] {
        cpu.registers.gpr = SEED;
        cpu.registers.set_esp(esp);
    }
}

#[test]
fn pushad_mid_block_matches_the_interpreter() {
    // The memory class's headline row. Thirty-two bytes of stack, eight registers read, ESP
    // written -- and `compare_state` compares guest RAM, so a wrong push ORDER, a wrong pushed SP
    // value (PUSHAD pushes the PRE-instruction SP, not the decremented one) or a missing store
    // fails here rather than somewhere subtle.
    let (code, starts) = program(&[&[PUSHAD]]);
    let mut roles = build(&code, &starts, |_| {}, |_| {});
    assert_eq!(
        roles.instructions, 3,
        "the block must cover all three slots"
    );
    assert_eq!(roles.callout_slots, 1);
    warm_stack_frame_pages(&mut roles);
    seed_registers(&mut roles, STACK_TOP);

    let executed = roles.native.direct_stall_snapshot().callout_executed;
    let retired = roles.native.perf_counters().jit_direct_insns;
    assert!(run_block(&mut roles, 3), "the block did not run natively");

    assert_eq!(
        roles.native.direct_stall_snapshot().callout_executed - executed,
        1,
        "the PUSHAD call-out must have run"
    );
    assert_eq!(
        roles
            .native
            .direct_stall_snapshot()
            .side_exit_callout_abnormal,
        0,
        "the frame was made resident, so the fail-closed path must not fire"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - retired,
        3,
        "all three slots, the PUSHAD included, must retire natively"
    );
    assert_eq!(
        roles.native.registers.esp(),
        STACK_TOP - 32,
        "ESP must have moved by the whole frame"
    );
    compare_state(&roles, "pushad mid-block");
}

#[test]
fn popad_mid_block_matches_the_interpreter_and_the_reload_publishes_all_eight_registers() {
    // The row the reload derivation exists for. `0xEC` wrote one byte of one register; POPAD
    // writes EIGHT plus ESP, so if the emitted slot's whole-set reload were partial this diverges
    // on `registers` -- and the frame below is seeded with eight DISTINCT values, so a reload that
    // dropped any one entry leaves a recognisable stale value rather than a zero.
    let (code, starts) = program(&[&[POPAD]]);
    let mut roles = build(&code, &starts, |_| {}, |_| {});
    assert_eq!(roles.instructions, 3);
    assert_eq!(roles.callout_slots, 1);
    warm_stack_frame_pages(&mut roles);
    // The frame POPAD will load: EDI, ESI, EBP, (discarded SP), EBX, EDX, ECX, EAX.
    const FRAME: [u32; 8] = [
        0xaaaa_0007,
        0xbbbb_0006,
        0xcccc_0005,
        0xdddd_0004,
        0xeeee_0003,
        0xffff_0002,
        0x9999_0001,
        0x8888_0000,
    ];
    let base = STACK_TOP - 32;
    for bus in [&mut roles.native_bus, &mut roles.interp_bus] {
        for (index, value) in FRAME.iter().enumerate() {
            let at = base as usize + index * 4;
            bus.memory[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
    seed_registers(&mut roles, base);

    let executed = roles.native.direct_stall_snapshot().callout_executed;
    let retired = roles.native.perf_counters().jit_direct_insns;
    assert!(run_block(&mut roles, 3), "the block did not run natively");

    assert_eq!(
        roles.native.direct_stall_snapshot().callout_executed - executed,
        1
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - retired,
        3,
        "all three slots must retire natively"
    );
    // Spelled out rather than left to the differential: this is the mutation target.
    assert_eq!(roles.native.registers.eax(), FRAME[7], "EAX");
    assert_eq!(roles.native.registers.ecx(), FRAME[6], "ECX");
    assert_eq!(roles.native.registers.edx(), FRAME[5], "EDX");
    assert_eq!(roles.native.registers.ebx(), FRAME[4], "EBX");
    assert_eq!(roles.native.registers.ebp(), FRAME[2], "EBP");
    assert_eq!(roles.native.registers.esi(), FRAME[1], "ESI");
    assert_eq!(roles.native.registers.edi(), FRAME[0], "EDI");
    assert_eq!(
        roles.native.registers.esp(),
        STACK_TOP,
        "ESP advances over the whole frame, discarded slot included"
    );
    compare_state(&roles, "popad mid-block");
}

#[test]
fn pushad_and_popad_in_one_block_match_the_interpreter() {
    // Two memory call-outs in one block, which is the shape the census's coupled family really
    // takes (a prologue and its epilogue can land in one span). Also the first row where the
    // SECOND helper reads registers the FIRST one wrote through the reload, so a reload that ran
    // only on the success path, or only once, diverges.
    let (code, starts) = program(&[&[PUSHAD], &SUB_CX_IMM8, &[POPAD]]);
    let mut roles = build(&code, &starts, |_| {}, |_| {});
    assert_eq!(roles.instructions, 5, "block shape");
    assert_eq!(roles.callout_slots, 2, "both call-outs must be admitted");
    warm_stack_frame_pages(&mut roles);
    seed_registers(&mut roles, STACK_TOP);

    let executed = roles.native.direct_stall_snapshot().callout_executed;
    assert!(run_block(&mut roles, 5), "the block did not run natively");

    assert_eq!(
        roles.native.direct_stall_snapshot().callout_executed - executed,
        2,
        "both call-outs must run"
    );
    assert_eq!(
        roles.native.registers.esp(),
        STACK_TOP,
        "PUSHAD then POPAD must leave ESP where it started"
    );
    compare_state(&roles, "pushad + word sub + popad");
}

#[test]
fn a_page_crossing_pushad_frame_matches_the_interpreter() {
    // The frame spans two pages, so the eight slots are resolved against two different fast-map
    // entries. `call_out_stack_frame_resident` does not special-case this -- it resolves each dword
    // individually -- and this row is what says that is enough rather than an oversight.
    let (code, starts) = program(&[&[PUSHAD]]);
    let mut roles = build(&code, &starts, |_| {}, |_| {});
    warm_stack_frame_pages(&mut roles);
    // Sixteen bytes into the page, so the frame's lower half lands on the page below.
    let esp = (STACK_TOP & !0xfff) + 16;
    seed_registers(&mut roles, esp);

    let executed = roles.native.direct_stall_snapshot().callout_executed;
    assert!(run_block(&mut roles, 3), "the block did not run natively");

    assert_eq!(
        roles.native.direct_stall_snapshot().callout_executed - executed,
        1
    );
    assert_eq!(
        roles
            .native
            .direct_stall_snapshot()
            .side_exit_callout_abnormal,
        0,
        "both pages were resident, so the frame must be accepted"
    );
    assert_eq!(roles.native.registers.esp(), esp - 32);
    compare_state(&roles, "page-crossing pushad");
}

#[test]
fn a_pushad_whose_frame_hits_watched_code_is_refused_and_left_to_the_interpreter() {
    // THE hazard this slice was designed around. A push whose range covers watched code would
    // reach `note_code_write_hit` with this block's native code live on the stack -- the exact
    // situation `note_code_write_inner`'s "no compiled block is mid-execution" proof rules out.
    //
    // The stack is aimed AT THE BLOCK'S OWN CODE, which is watched because the block is compiled
    // over it. The call-out must refuse (abnormal, EIP at the PUSHAD, nothing written) and the
    // interpreter must then execute the whole instruction -- which is what the twin, stepped only
    // over the ONE slot the native run completed, pins.
    let (code, starts) = program(&[&[PUSHAD]]);
    let mut roles = build(&code, &starts, |_| {}, |_| {});
    // The code page has to be servable too, or the refusal would be the residency clause rather
    // than the code-watch clause and the row would prove the wrong thing.
    warm_pages(
        &mut roles,
        &[
            STACK_TOP.wrapping_sub(0x1000) & !0xfff,
            STACK_TOP & !0xfff,
            ENTRY & !0xfff,
        ],
    );
    // ESP just past the block's last byte, 4-aligned, so the thirty-two bytes below it land ON the
    // block's own code -- which is watched precisely because the block is compiled over it.
    let esp = (ENTRY + code.len() as u32 + 3) & !3;
    seed_registers(&mut roles, esp);
    let before_memory = roles.native_bus.memory.clone();
    let before_esp = roles.native.registers.esp();

    let executed = roles.native.direct_stall_snapshot().callout_executed;
    let retired = roles.native.perf_counters().jit_direct_insns;
    // ONE interpreted step on the twin: the prefix slot. That is exactly what the run loop has
    // executed when a native run ends at a PUSHAD.
    assert!(run_block(&mut roles, 1), "the block did not run natively");

    assert_eq!(
        roles.native.direct_stall_snapshot().callout_executed - executed,
        1,
        "the helper must be entered before it can refuse"
    );
    assert_eq!(
        roles
            .native
            .direct_stall_snapshot()
            .side_exit_callout_abnormal,
        1,
        "the watched frame must take the fail-closed exit"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - retired,
        1,
        "only the prefix slot may retire"
    );
    assert_eq!(
        roles.native_bus.memory, before_memory,
        "a refused PUSHAD must not have written one byte"
    );
    assert_eq!(
        roles.native.registers.esp(),
        before_esp,
        "a refused PUSHAD must not have moved ESP"
    );
    compare_state(&roles, "watched pushad frame");
}

#[test]
fn a_pushad_whose_frame_is_not_resident_is_refused_and_left_to_the_interpreter() {
    // The cold half, deliberately measured: no `warm_stack_frame_pages`, so `lookup_access` misses
    // and the pre-check refuses on residency rather than on the code watch. Same fail-closed
    // shape, different clause -- and this is the state EVERY PUSHAD is in the first time its stack
    // page is touched, so it is the common case rather than a corner.
    let (code, starts) = program(&[&[PUSHAD]]);
    let mut roles = build(&code, &starts, |_| {}, |_| {});
    arm_fast_map_gate(&mut roles);
    seed_registers(&mut roles, STACK_TOP);
    let before_memory = roles.native_bus.memory.clone();

    let retired = roles.native.perf_counters().jit_direct_insns;
    assert!(run_block(&mut roles, 1), "the block did not run natively");

    assert_eq!(
        roles
            .native
            .direct_stall_snapshot()
            .side_exit_callout_abnormal,
        1,
        "a non-resident frame must take the fail-closed exit"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - retired,
        1,
        "only the prefix slot may retire"
    );
    assert_eq!(
        roles.native_bus.memory, before_memory,
        "a refused PUSHAD must not have written one byte"
    );
    compare_state(&roles, "non-resident pushad frame");
}

#[test]
fn a_memory_call_out_composes_with_a_port_call_out_in_one_block() {
    // The two CLASSES in one block. They are priced separately by `compute_iteration_upper` and
    // gated differently at dispatch, so a block holding both is the shape that would catch a class
    // split applied to the wrong counter.
    let (code, starts) = program(&[&[PUSHAD], &[IN_AL_DX], &[POPAD]]);
    let mut roles = build(
        &code,
        &starts,
        |bus| {
            bus.lazy_io_reads = true;
            bus.io_read_sequence = DEVICE_VALUES.to_vec();
        },
        |_| {},
    );
    assert_eq!(roles.instructions, 5);
    assert_eq!(roles.callout_slots, 3, "two memory slots and one port slot");
    warm_stack_frame_pages(&mut roles);
    seed_registers(&mut roles, STACK_TOP);

    let executed = roles.native.direct_stall_snapshot().callout_executed;
    assert!(run_block(&mut roles, 5), "the block did not run natively");

    assert_eq!(
        roles.native.direct_stall_snapshot().callout_executed - executed,
        3,
        "all three call-outs must run"
    );
    // PUSHAD saved EAX, the IN overwrote AL, and POPAD restored the saved value -- so a
    // round-trip through all three classes leaves the SEEDED EAX, not the port byte. That is the
    // sharp end of the composition claim: the memory helpers' guest RAM and the port helper's
    // register write have to interleave in the right order for this to come out.
    assert_eq!(
        roles.native.registers.eax(),
        0x1111_0001,
        "POPAD must restore the EAX that PUSHAD saved, over the port byte the IN wrote"
    );
    compare_state(&roles, "pushad + in + popad");
    compare_device_order(&roles, "pushad + in + popad");
}

#[test]
fn the_prologue_family_lands_in_one_block() {
    // The census claim, as a fixture. `0x60` PUSHAD, `0x83 /5` SUB at Word size and `0x61` POPAD
    // are one function prologue and its epilogue in doom, forty-seven exits apart at the top of
    // the rejected-row table. Lowering any one alone RELOCATES its exits onto the next
    // instruction, so this row pins that all three join one block rather than each of them
    // separately joining a block the others end.
    let (code, starts) = program(&[&SUB_CX_IMM8, &[PUSHAD], &SUB_CX_IMM8, &[POPAD]]);
    let mut roles = build(&code, &starts, |_| {}, |_| {});
    assert_eq!(
        roles.instructions, 6,
        "all four family members plus the two fillers must be one block"
    );
    assert_eq!(roles.callout_slots, 2);
    warm_stack_frame_pages(&mut roles);
    seed_registers(&mut roles, STACK_TOP);
    // A high half in ECX that the word SUB must PRESERVE: a lowering that used a 32-bit move to
    // write the result back clobbers it, and the register compare below is what sees that.
    for cpu in [&mut roles.native, &mut roles.interp] {
        cpu.registers.set_ecx(0xdead_0002);
    }

    assert!(run_block(&mut roles, 6), "the block did not run natively");

    assert_eq!(
        roles.native.registers.ecx() & 0xffff_0000,
        0xdead_0000,
        "the word SUB must preserve ECX's high half"
    );
    compare_state(&roles, "prologue family");
}
