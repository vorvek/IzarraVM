// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Slice 9C-pre (`dev_docs/2026-09-05-device-timing-slice9-design.md` §6): the
//! primary-channel (fixed-disk) analogue of `machine_atapi_poll_skip_test.rs`.
//!
//! **THE BAR THIS FILE CERTIFIES.** 9C-pre is "pure plumbing": the whole
//! mechanism must be INERT while `ata::COMMAND_LATENCY_TICKS` stays 0 (its
//! shipped value) and while `DeviceTimingProfile::ata` stays unarmed (its
//! default). Every fixture below states which of those two facts it is
//! pinning, the same discipline the ATAPI file's every-fixture-states-its-arm
//! rule follows.

use super::*;

const STATUS_BSY: u8 = 0x80;
const TICKS_PER_US: u64 = izarravm_core::MASTER_CLOCK_HZ / 1_000_000;

fn out(machine: &mut Machine, port: u16, value: u8) {
    with_bus(machine, |bus| {
        bus.write_io(port, BusWidth::Byte, u32::from(value), false)
            .unwrap();
    });
}

fn input(machine: &mut Machine, port: u16) -> u8 {
    with_bus(machine, |bus| {
        bus.read_io(port, BusWidth::Byte, 0, false).unwrap() as u8
    })
}

/// A machine with a small disk mounted, the `ata` device-timing family armed
/// or not.
fn hdd_machine(ata_family_armed: bool) -> Machine {
    let mut machine = machine_with_hdd(16);
    machine.set_mode(GswMode::Gsw586);
    machine.set_device_timing_ata_for_test(ata_family_armed);
    machine
}

/// Drive `count` alt-status reads through the real bus path, the same shape
/// `poll_alt_status` uses in the ATAPI file.
fn poll_alt_status(machine: &mut Machine, count: u32) {
    machine.io_touched = false;
    for _ in 0..count {
        let _ = input(machine, ata::PRIMARY_CTRL);
    }
}

fn counters(machine: &Machine) -> AtaHddPollSkipCounters {
    machine.ata_hdd_poll_skip_counters()
}

// ---------------------------------------------------------------------------
// 1. INERT while `COMMAND_LATENCY_TICKS` stays 0, even with the family armed.
// ---------------------------------------------------------------------------

/// A real ATA command (EXECUTE DEVICE DIAGNOSTIC) schedules through the
/// production `COMMAND_LATENCY_TICKS = 0` path -- `schedule()` floors it to
/// one master tick, far under the 20 us floor -- so the hook must never arm
/// no matter how long the guest polls.
#[test]
fn ata_hdd_poll_skip_never_fires_at_command_latency_zero() {
    let mut machine = hdd_machine(true);
    out(&mut machine, ata::PRIMARY_CMD_BASE + 7, 0x90); // EXECUTE DEVICE DIAGNOSTIC
    let deadline = machine
        .ata
        .as_ref()
        .and_then(ata::AtaDisk::ticks_until_completion)
        .expect("the diagnostic command schedules a pending completion");
    assert!(
        deadline < ata::ATA_POLL_FLOOR_TICKS,
        "VACUOUS unless COMMAND_LATENCY_TICKS=0 keeps the deadline under the floor: \
         deadline={deadline} floor={}",
        ata::ATA_POLL_FLOOR_TICKS
    );

    poll_alt_status(&mut machine, ata::ATA_POLL_RUN * 4);

    assert!(
        !machine.ata_hdd_poll_skip_armed,
        "under the floor, the arm must never fire"
    );
    assert_eq!(
        counters(&machine),
        AtaHddPollSkipCounters::default(),
        "and no counter moves"
    );
}

/// The same command, but the `ata` family is left at its default (unarmed):
/// the bus arm site must decline before it even counts, regardless of how
/// long a latency the channel is later given.
#[test]
fn ata_hdd_poll_skip_never_fires_with_the_family_unarmed() {
    let mut machine = hdd_machine(false);
    out(&mut machine, ata::PRIMARY_CMD_BASE + 7, 0x90);
    // Overwrite the schedule with a large test-injected latency, comfortably
    // above the floor, so this fixture is not vacuous the way the
    // latency-zero one above already covers.
    machine
        .ata
        .as_mut()
        .unwrap()
        .schedule_test_pending(500 * TICKS_PER_US);

    poll_alt_status(&mut machine, ata::ATA_POLL_RUN * 4);

    assert!(
        !machine.ata_hdd_poll_skip_armed,
        "the family flag is the whole gate: unarmed must mean absent, not merely below floor"
    );
    assert_eq!(counters(&machine), AtaHddPollSkipCounters::default());
}

// ---------------------------------------------------------------------------
// 2. The target lands exactly on the completion, given an injected latency.
// ---------------------------------------------------------------------------

/// A test-injected latency stands in for the non-zero `COMMAND_LATENCY_TICKS`
/// slice 9C will one day ship, so this fixture proves the mechanism itself
/// -- not merely its inertness -- ports correctly.
#[test]
fn ata_hdd_poll_skip_lands_exactly_on_the_completion() {
    let mut machine = hdd_machine(true);
    let injected = 100 * TICKS_PER_US;
    machine
        .ata
        .as_mut()
        .unwrap()
        .schedule_test_pending(injected);
    assert!(injected >= ata::ATA_POLL_FLOOR_TICKS);

    poll_alt_status(&mut machine, ata::ATA_POLL_RUN);
    assert!(machine.ata_hdd_poll_skip_armed, "the arm fired");

    let before = machine.master_ticks();
    let remaining = machine
        .ata
        .as_ref()
        .and_then(ata::AtaDisk::ticks_until_completion)
        .unwrap();
    assert_eq!(
        remaining, injected,
        "nothing consumed the wait before the actuation"
    );
    machine.actuate_ata_hdd_poll_skip(before + remaining * 8, false);

    assert_eq!(
        machine.master_ticks() - before,
        remaining,
        "the skip lands ON the completion instant, neither short of it nor past it"
    );
    assert_eq!(
        machine
            .ata
            .as_ref()
            .and_then(ata::AtaDisk::ticks_until_completion),
        None,
        "the command completed at the landing, through the ordinary device fan-out"
    );
    assert_eq!(
        input(&mut machine, ata::PRIMARY_CTRL) & STATUS_BSY,
        0,
        "the byte the guest would read after the skip is the byte it would have read spinning"
    );
    let c = counters(&machine);
    assert_eq!(c.skips, 1);
    assert_eq!(c.skipped_ticks, remaining, "and the whole wait was skipped");
    assert_eq!(
        machine.io_stall_ticks(),
        remaining,
        "charged as I/O stall -- a spinning guest is not idle"
    );
    assert_eq!(machine.halted_ticks(), 0);
}

/// N alt-status reads elide to exactly ONE projection, and the projected
/// advance equals the deadline exactly -- the same contract the ATAPI
/// mechanism's `spans`/`ticks` pair certifies.
#[test]
fn ata_hdd_poll_skip_n_polls_elide_to_one_projection_of_exactly_the_deadline() {
    let mut machine = hdd_machine(true);
    let injected = 250 * TICKS_PER_US;
    machine
        .ata
        .as_mut()
        .unwrap()
        .schedule_test_pending(injected);

    // More than the threshold: the extra reads before the arm fires must not
    // change the shape of the result -- one committed span, exactly the
    // deadline's worth of ticks.
    poll_alt_status(&mut machine, ata::ATA_POLL_RUN + 40);
    assert!(machine.ata_hdd_poll_skip_armed);

    let before = machine.master_ticks();
    machine.actuate_ata_hdd_poll_skip(before + injected * 8, false);

    let c = counters(&machine);
    assert_eq!(c.skips, 1, "N reads elide to exactly one committed span");
    assert_eq!(
        c.skipped_ticks, injected,
        "the projected advance equals the deadline exactly"
    );
    assert_eq!(machine.master_ticks() - before, injected);
}

// ---------------------------------------------------------------------------
// 3. The two reset edges, mirroring the ATAPI file's §5.
// ---------------------------------------------------------------------------

#[test]
fn ata_hdd_poll_skip_declines_while_a_command_is_not_pending() {
    let mut machine = hdd_machine(true);
    assert!(
        machine
            .ata
            .as_ref()
            .and_then(ata::AtaDisk::ticks_until_completion)
            .is_none()
    );

    poll_alt_status(&mut machine, 64);

    assert!(
        !machine.ata_hdd_poll_skip_armed,
        "nothing pending, nothing armed"
    );
    assert_eq!(counters(&machine), AtaHddPollSkipCounters::default());
}

#[test]
fn ata_hdd_poll_skip_resets_on_a_control_write() {
    let mut machine = hdd_machine(true);
    machine
        .ata
        .as_mut()
        .unwrap()
        .schedule_test_pending(200 * TICKS_PER_US);
    poll_alt_status(&mut machine, ata::ATA_POLL_RUN - 1);
    assert_eq!(
        machine.ata.as_ref().unwrap().alt_status_run_for_test(),
        ata::ATA_POLL_RUN - 1,
        "one read short of the threshold"
    );

    // A control write is not a poll: it clears the run AND latches the
    // block, mirroring the ATAPI channel's same two guards.
    out(&mut machine, ata::PRIMARY_CTRL, 0x02);
    assert_eq!(machine.ata.as_ref().unwrap().alt_status_run_for_test(), 0);
    assert!(machine.ata.as_ref().unwrap().poll_skip_blocked());

    poll_alt_status(&mut machine, ata::ATA_POLL_RUN * 2);
    assert!(
        !machine.ata_hdd_poll_skip_armed,
        "an armed flag raised before a control write cannot be honoured after it"
    );
    assert_eq!(counters(&machine), AtaHddPollSkipCounters::default());
}

/// The DEVICE-bounded target under the floor is the only decline that
/// latches, and a new `schedule()` clears the latch again -- mirroring the
/// ATAPI file's floor-and-latch fixture.
#[test]
fn ata_hdd_poll_skip_declines_below_the_floor_and_blocks_one_command() {
    // Schedule comfortably above the floor, then let most of it elapse before
    // polling, so the arm-time check (evaluated against the REMAINING
    // deadline) still passes while the device-bounded target at actuation
    // time is sub-floor -- the DEVICE cause, and the one that latches.
    let mut machine = hdd_machine(true);
    let deadline = ata::ATA_POLL_FLOOR_TICKS * 4;
    machine
        .ata
        .as_mut()
        .unwrap()
        .schedule_test_pending(deadline);
    machine.advance_devices_ticks(deadline - ata::ATA_POLL_FLOOR_TICKS / 2);

    // The arm-time floor also declines here and does so WITHOUT latching, so
    // drop it to isolate the batch-end check that does -- mirroring the
    // ATAPI fixture this ports.
    machine
        .ata
        .as_mut()
        .unwrap()
        .configure_poll_skip_for_test(ata::ATA_POLL_RUN, 1);
    poll_alt_status(&mut machine, ata::ATA_POLL_RUN);
    assert!(
        machine.ata_hdd_poll_skip_armed,
        "armed with time to spare above the arm-time floor"
    );
    // Restore the shipped floor for the batch-end decision.
    machine
        .ata
        .as_mut()
        .unwrap()
        .configure_poll_skip_for_test(ata::ATA_POLL_RUN, ata::ATA_POLL_FLOOR_TICKS);

    let before = machine.master_ticks();
    machine.actuate_ata_hdd_poll_skip(before + 1_000 * TICKS_PER_US, false);

    let c = counters(&machine);
    assert_eq!(
        c.skips, 0,
        "the device-bounded target was under the floor: {c:?}"
    );
    assert_eq!(machine.master_ticks(), before);
    assert!(
        machine.ata.as_ref().unwrap().poll_skip_blocked(),
        "the latch is set"
    );

    poll_alt_status(&mut machine, ata::ATA_POLL_RUN * 3);
    assert!(
        !machine.ata_hdd_poll_skip_armed,
        "at most ONE wasted batch break per pending command"
    );

    // Let the still-pending command finish and drain its completion so the
    // channel is back at Phase::Idle and will accept a new command.
    let remaining = machine
        .ata
        .as_ref()
        .and_then(ata::AtaDisk::ticks_until_completion)
        .unwrap_or(0);
    machine.advance_devices_ticks(remaining);
    let _ = input(&mut machine, ata::PRIMARY_CMD_BASE + 7);
    assert!(
        machine.ata.as_ref().unwrap().poll_skip_blocked(),
        "still latched before the new command"
    );

    // The next command clears the latch.
    out(&mut machine, ata::PRIMARY_CMD_BASE + 7, 0x90);
    assert!(
        !machine.ata.as_ref().unwrap().poll_skip_blocked(),
        "a fresh schedule() cleared the latch"
    );
}

/// Any OTHER read on the channel breaks the run.
#[test]
fn ata_hdd_poll_skip_resets_on_any_other_channel_read() {
    let mut machine = hdd_machine(true);
    machine
        .ata
        .as_mut()
        .unwrap()
        .schedule_test_pending(200 * TICKS_PER_US);
    poll_alt_status(&mut machine, ata::ATA_POLL_RUN - 1);
    let _ = input(&mut machine, ata::PRIMARY_CMD_BASE + 1); // error register
    poll_alt_status(&mut machine, 1);
    assert!(
        !machine.ata_hdd_poll_skip_armed,
        "the run restarted at the intervening read and one more read is not the threshold"
    );
}

// ---------------------------------------------------------------------------
// 4. Skipping and spinning reach the same device state.
// ---------------------------------------------------------------------------

/// The weakened analogue of the ATAPI file's own version: retired instruction
/// counts and the register file at a boundary are NOT compared (the elided
/// iterations are not executed, by construction); what IS compared is that
/// at a common guest instant the channel and the timeline agree.
#[test]
fn ata_hdd_poll_skip_and_spinning_reach_the_same_device_state() {
    let injected = 300 * TICKS_PER_US;
    let mut skipped = hdd_machine(true);
    skipped
        .ata
        .as_mut()
        .unwrap()
        .schedule_test_pending(injected);
    let mut spinning = hdd_machine(false);
    spinning
        .ata
        .as_mut()
        .unwrap()
        .schedule_test_pending(injected);

    poll_alt_status(&mut skipped, ata::ATA_POLL_RUN);
    assert!(skipped.ata_hdd_poll_skip_armed);
    let before = skipped.master_ticks();
    skipped.actuate_ata_hdd_poll_skip(before + injected * 8, false);

    spinning.advance_devices_ticks(injected);

    assert_eq!(
        skipped
            .ata
            .as_ref()
            .and_then(ata::AtaDisk::ticks_until_completion),
        None
    );
    assert_eq!(
        spinning
            .ata
            .as_ref()
            .and_then(ata::AtaDisk::ticks_until_completion),
        None
    );
    assert_eq!(skipped.master_ticks(), spinning.master_ticks());
    assert_eq!(
        input(&mut skipped, ata::PRIMARY_CTRL),
        input(&mut spinning, ata::PRIMARY_CTRL),
        "the byte the guest reads is identical either way"
    );
}

// ---------------------------------------------------------------------------
// 5. The armed flag does not survive a batch (the run-loop wiring).
// ---------------------------------------------------------------------------

/// Guest program: poll the primary alt-status port and spin while BSY is set,
/// then halt -- the fixed-disk analogue of the ATAPI file's `poll_machine`
/// guest loop.
fn poll_program_machine() -> Machine {
    const PROGRAM: &[u8] = &[
        0xba, 0xf6, 0x03, // mov dx,3F6h
        0xec, // in al,dx
        0xa8, 0x80, // test al,BSY
        0x75, 0xfb, // jnz -5
        0xf4, // hlt
    ];
    let mut bytes = vec![0u8; 16 * 512];
    for s in 0..16 {
        bytes[s * 512] = (s as u8).wrapping_add(0x10);
    }
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let mut machine = Machine::new_raw_program(profile, PROGRAM).unwrap();
    machine.mount_hdd(bytes);
    machine.set_device_timing_ata_for_test(true);
    machine
}

#[test]
fn ata_hdd_poll_skip_carries_a_guest_poll_loop_through_the_run_loop() {
    let mut machine = poll_program_machine();
    let injected = 150 * TICKS_PER_US;
    machine
        .ata
        .as_mut()
        .unwrap()
        .schedule_test_pending(injected);
    let start = machine.master_ticks();

    let stop = machine.run_until_halt_or_cycles(50_000_000).unwrap();

    assert_eq!(
        stop,
        StopReason::Halted,
        "the guest leaves its poll loop and halts once BSY clears"
    );
    let elapsed = machine.master_ticks() - start;
    assert!(
        elapsed >= injected && elapsed < injected + TICKS_PER_US,
        "the guest waited the modelled wait and no more: elapsed={elapsed} injected={injected}"
    );
    let c = counters(&machine);
    assert!(
        c.skips >= 1,
        "the mechanism actually fired end to end: {c:?}"
    );
    assert_eq!(
        input(&mut machine, ata::PRIMARY_CTRL) & STATUS_BSY,
        0,
        "the byte the guest read after the skip is the byte it would have read having spun"
    );
}
