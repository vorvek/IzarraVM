// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The device-armed ATA/ATAPI clock skip, `IZARRAVM_ATA_POLL_SKIP`.
//!
//! **EVERY FIXTURE STATES ITS ARM IN BOTH DIRECTIONS.** The gate is per-machine
//! state read once at construction (not a process-wide `OnceLock` like the CPU
//! lane knobs), so `set_ata_poll_skip_enabled` is the whole override mechanism
//! and there is no ambient reading to inherit. The OFF fixtures state OFF for
//! the same reason the lane families' refusal fixtures do: they must keep
//! meaning what they say the day the default flips.
//!
//! The actuation fixtures call `Machine::actuate_ata_poll_skip` -- the
//! production method the run loop calls, not a re-derivation of its decision
//! tree -- after arming through the real bus path. Three fixtures (1, 4, 12)
//! run a guest poll loop end to end instead, because the wiring between the arm
//! site, the batch-entry clear and the actuation is exactly what they exist to
//! pin.

use super::*;

const STATUS_BSY: u8 = 0x80;
/// One master tick per microsecond of guest time.
const TICKS_PER_US: u64 = izarravm_core::MASTER_CLOCK_HZ / 1_000_000;
/// The real GUI quantum: `FAST_EMU_QUANTUM_TICKS = MASTER_CLOCK_HZ / 1000`
/// (`gui_session.rs`), applied whenever the mode is approximate -- which is
/// precisely and only the class this lever is scoped to.
const GUI_SLICE_TICKS: u64 = izarravm_core::MASTER_CLOCK_HZ / 1000;

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

fn data_disc(sectors: u32) -> CdImage {
    let mut bytes = vec![0u8; sectors as usize * cdimage::DATA_SECTOR];
    for sector in 0..sectors as usize {
        bytes[sector * cdimage::DATA_SECTOR] = 0x60u8.wrapping_add(sector as u8);
    }
    CdImage::from_iso(bytes).unwrap()
}

/// A 586 machine whose guest program is the shape TOKACD's `wait_not_busy` is:
/// poll the alt-status port and spin while BSY is set.
///
/// ```text
///   mov dx, 0x376
/// .loop:
///   in  al, dx
///   test al, 0x80
///   jnz .loop
///   hlt
/// ```
///
/// 586 rather than 386 because the arm is Approximate-class only: on the
/// Accurate class an alt-status read already sets `io_touched` and ends its own
/// batch, so the run counter can never exceed 1 and the arm is unreachable by
/// construction. That is recorded as owed, not as a bug.
fn poll_machine(enabled: bool) -> Machine {
    const PROGRAM: &[u8] = &[
        0xba, 0x76, 0x03, // mov dx,376h
        0xec, // in al,dx
        0xa8, 0x80, // test al,BSY
        0x75, 0xfb, // jnz -5
        0xf4, // hlt
    ];
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw586;
    let mut machine = Machine::new_raw_program(profile, PROGRAM).unwrap();
    machine.mount_cd(data_disc(16));
    machine.set_ata_poll_skip_enabled(enabled);
    machine
}

/// A bare 586 machine with a disc but no guest program to run: for the fixtures
/// that drive the bus arm directly and then call the actuation.
fn bus_machine(enabled: bool) -> Machine {
    let mut machine = int15_machine(16);
    machine.set_mode(GswMode::Gsw586);
    machine.mount_cd(data_disc(16));
    machine.set_ata_poll_skip_enabled(enabled);
    machine
}

/// Schedule an IDENTIFY PACKET DEVICE: one pending command, 100 us of
/// `COMMAND_LATENCY_TICKS`, comfortably above the 20 us floor and with no data
/// the guest has to drain first.
fn schedule_identify(machine: &mut Machine) -> u64 {
    out(machine, ide::SECONDARY_CMD_BASE + 7, 0xa1);
    machine
        .ide
        .ticks_until_completion()
        .expect("IDENTIFY PACKET DEVICE schedules a pending command")
}

/// A packet READ(10) of the FARTHEST sector on the disc, so the scheduled
/// boundary is `media_delay + sector_transfer` -- tens of milliseconds rather
/// than the 100 us command latency. Returns the pending deadline.
///
/// Needed wherever the fixture has to out-wait something else: a bus-master DMA
/// transfer (~115 us) or the 1 ms GUI slice.
fn schedule_far_read(machine: &mut Machine) -> u64 {
    let send = |machine: &mut Machine, cdb: [u8; 12]| {
        out(machine, ide::SECONDARY_CMD_BASE + 7, 0xa0);
        let accept = machine.ide.ticks_until_completion().unwrap();
        machine.advance_devices_ticks(accept);
        let _ = input(machine, ide::SECONDARY_CMD_BASE + 7);
        for byte in cdb {
            out(machine, ide::SECONDARY_CMD_BASE, byte);
        }
        let latency = machine.ide.ticks_until_completion().unwrap();
        machine.advance_devices_ticks(latency);
    };
    // TEST UNIT READY first: the power-on unit attention would fail the read.
    send(machine, [0u8; 12]);
    let _ = input(machine, ide::SECONDARY_CMD_BASE + 7);

    let mut cdb = [0u8; 12];
    cdb[0] = 0x28; // READ(10)
    cdb[2..6].copy_from_slice(&15u32.to_be_bytes());
    cdb[8] = 1;
    send(machine, cdb);
    machine
        .ide
        .ticks_until_completion()
        .expect("the read schedules its mechanical boundary")
}

/// Drive `count` alt-status reads through the real bus path, as guest V86 code
/// does (`cpu_is_ring0_pm == false`).
///
/// `io_touched` is cleared first because the run loop clears it at batch entry
/// and the arm predicate carries `!io_touched_before_read` -- a read that
/// follows an access which already ended the batch is not part of a poll run.
/// A fixture that skipped this would be measuring the port write it just did.
fn poll_alt_status(machine: &mut Machine, count: u32) {
    machine.io_touched = false;
    for _ in 0..count {
        let _ = input(machine, ide::SECONDARY_CTRL);
    }
}

/// Arm PIT channel 0 at the DOS default reload, mode 2: an OUT rise 54.8 ms out.
///
/// **This is what makes a no-pending-command fixture non-vacuous.** The hazard
/// the mandatory ATA precondition exists for is the target FALLING THROUGH to an
/// unrelated edge, and a machine with no unrelated edge armed has nothing to
/// fall through to -- its `next_device_edge_ticks()` is `None`, so a mutant that
/// drops the precondition stalls for zero ticks and the fixture passes anyway.
/// `18.2 Hz` is exactly the "up to 54.9 ms away" the design's B1 names.
fn arm_far_pit_edge(machine: &mut Machine) {
    out(machine, 0x43, 0x34); // channel 0, lobyte/hibyte, mode 2
    out(machine, 0x40, 0); // reload 65536 -> 18.2 Hz
    out(machine, 0x40, 0);
}

fn counters(machine: &Machine) -> AtaPollSkipCounters {
    machine.ata_poll_skip_counters()
}

// ---------------------------------------------------------------------------
// 1. The target lands ON the completion.
// ---------------------------------------------------------------------------

/// The landing instant, pinned exactly at the actuation itself.
///
/// Separate from the end-to-end fixture below because after the skip the guest
/// still executes the few instructions that leave the loop, and the batch that
/// runs them advances the timeline past the completion for an ordinary reason.
/// Asserting the landing here keeps that tail out of the assertion.
#[test]
fn ata_poll_skip_lands_exactly_on_the_atapi_completion() {
    let mut machine = bus_machine(true);
    schedule_identify(&mut machine);
    poll_alt_status(&mut machine, ide::ATA_POLL_RUN);
    assert!(machine.ata_poll_skip_armed, "the arm fired");

    let before = machine.master_ticks();
    let remaining = machine.ide.ticks_until_completion().unwrap();
    machine.actuate_ata_poll_skip(before + remaining * 8, false);

    assert_eq!(
        machine.master_ticks() - before,
        remaining,
        "the skip lands ON the completion instant, neither short of it nor past it"
    );
    assert_eq!(
        machine.ide.ticks_until_completion(),
        None,
        "the command completed at the landing, through the ordinary device fan-out"
    );
    assert_eq!(
        input(&mut machine, ide::SECONDARY_CTRL) & STATUS_BSY,
        0,
        "the byte the guest would read after the skip is the byte it would have read spinning"
    );
    let c = counters(&machine);
    assert_eq!(c.spans, 1);
    assert_eq!(c.ticks, remaining, "and the whole wait was skipped");
    assert_eq!(c.blocks, 0);
    assert_eq!(c.declines_deadline_clamped, 0);
    assert_eq!(c.monitor_exempt, 0);
    assert_eq!(
        machine.io_stall_ticks(),
        remaining,
        "charged as I/O stall -- a spinning guest is not idle"
    );
    assert_eq!(
        machine.halted_ticks(),
        0,
        "halted_ticks stays the 'guest asked to be parked' metric"
    );
}

/// The same thing end to end, through the real arm site and the real run loop,
/// with a guest poll loop shaped like TOKACD's `wait_not_busy`.
#[test]
fn ata_poll_skip_carries_a_guest_poll_loop_through_the_run_loop() {
    let mut machine = poll_machine(true);
    let start = machine.master_ticks();
    let deadline = schedule_identify(&mut machine);

    let stop = machine.run_until_halt_or_cycles(50_000_000).unwrap();

    assert_eq!(
        stop,
        StopReason::Halted,
        "the guest leaves its poll loop and halts once BSY clears"
    );
    let elapsed = machine.master_ticks() - start;
    assert!(
        elapsed >= deadline && elapsed < deadline + TICKS_PER_US,
        "the guest waited the modelled wait and no more: the only time past the completion \
         is the handful of instructions it ran on leaving the loop. elapsed={elapsed} \
         deadline={deadline}"
    );
    assert_eq!(
        input(&mut machine, ide::SECONDARY_CTRL) & STATUS_BSY,
        0,
        "the byte the guest read after the skip is the byte it would have read having spun"
    );
    let c = counters(&machine);
    assert_eq!(c.spans, 1, "one committed skip covers the whole wait");
    assert!(c.arms >= 1, "the arm fired: {c:?}");
    assert_eq!(c.blocks, 0, "nothing truncated it, so nothing latched");
    assert_eq!(c.declines_deadline_clamped, 0);
    assert_eq!(c.monitor_exempt, 0);
    assert!(
        machine.io_stall_ticks() >= c.ticks,
        "the skipped span is charged as I/O stall, not as halted time"
    );
    assert_eq!(
        machine.halted_ticks(),
        0,
        "a spinning guest is not idle -- halted_ticks stays the 'guest asked to be parked' metric"
    );
}

/// The OFF arm is the A/B base, and it must be inert in BOTH senses: no skip,
/// and no counter movement at all.
#[test]
fn ata_poll_skip_off_arm_spins_the_wait_out_and_moves_no_counter() {
    let mut machine = poll_machine(false);
    let start = machine.master_ticks();
    let deadline = schedule_identify(&mut machine);

    let stop = machine.run_until_halt_or_cycles(50_000_000).unwrap();

    assert_eq!(stop, StopReason::Halted);
    assert!(
        machine.master_ticks() >= start + deadline,
        "the guest spun the whole wait out"
    );
    assert_eq!(
        counters(&machine),
        AtaPollSkipCounters::default(),
        "with the gate off the mechanism is entirely absent, counters included"
    );
    assert_eq!(machine.io_stall_ticks(), 0, "no stall was charged");
}

// ---------------------------------------------------------------------------
// 2. An earlier device edge stops the skip, and the guest re-arms after it.
// ---------------------------------------------------------------------------

#[test]
fn ata_poll_skip_stops_at_an_earlier_pit_edge() {
    let mut machine = poll_machine(true);
    // PIT channel 0, mode 2, a short reload so its OUT rise lands well inside
    // the 100 us ATA window but well above the 20 us floor.
    out(&mut machine, 0x43, 0x34);
    out(&mut machine, 0x40, 60);
    out(&mut machine, 0x40, 0);

    let start = machine.master_ticks();
    let deadline = schedule_identify(&mut machine);
    let edge = machine
        .next_device_edge_ticks()
        .expect("a PIT edge is armed");
    assert!(
        edge < deadline,
        "the fixture is vacuous unless the PIT edge is inside the ATA window: \
         edge={edge} deadline={deadline}"
    );
    assert!(edge >= machine.ata_poll_floor_ticks, "and above the floor");

    let stop = machine.run_until_halt_or_cycles(50_000_000).unwrap();

    assert_eq!(stop, StopReason::Halted);
    let elapsed = machine.master_ticks() - start;
    assert!(
        elapsed >= deadline && elapsed < deadline + TICKS_PER_US,
        "no device edge is crossed inside a skip, and the total is still the ATA window plus \
         only the instructions the guest ran on leaving the loop: elapsed={elapsed} \
         deadline={deadline}"
    );
    let c = counters(&machine);
    assert!(
        c.spans >= 2,
        "the first skip stops on the PIT edge and the guest re-arms for the rest: {c:?}"
    );
    assert_eq!(
        c.blocks, 0,
        "a truncating edge ABOVE the floor is not the pathology the latch exists for"
    );
    assert!(
        machine.pic.irr_bit(0),
        "IRQ0 was raised in the ordinary batch-end position, not swallowed by the skip"
    );
}

// ---------------------------------------------------------------------------
// 3. nIEN. The single sharpest hazard in the design.
// ---------------------------------------------------------------------------

#[test]
fn ata_poll_skip_ignores_nien() {
    let mut machine = poll_machine(true);
    // TOKACD writes 0x02 (nIEN) to the control port before EVERY command
    // (`tokacd.asm:1464-1466`). Do the same, before scheduling, since a control
    // write clears the run and latches the block.
    out(&mut machine, ide::SECONDARY_CTRL, 0x02);
    let start = machine.master_ticks();
    let deadline = schedule_identify(&mut machine);

    assert!(
        !machine.ide.irq_enabled(),
        "nIEN is set, which is the precondition for this hazard"
    );
    // THE HAZARD, pinned at its own predicate: `next_timer_wake`'s ATAPI term is
    // gated on IRQ deliverability, so with nIEN set it does NOT see this
    // channel's completion and a skip built on it would run straight past.
    let wake = machine.next_timer_wake(start + deadline * 4);
    let ata_wake = machine
        .timeline
        .cpu_clocks_for_master_ticks_ceil(deadline)
        .max(1);
    assert!(
        wake != Some(ata_wake),
        "next_timer_wake must NOT be offering the ATAPI completion here -- that is why the \
         skip target is a separate function from the halt target"
    );
    // The skip target reads the channel directly and takes the ungated
    // `next_ata_deadline` chain, so it stops at the completion regardless.
    assert_eq!(
        machine.next_device_edge_ticks(),
        Some(deadline),
        "the skip's own edge set carries the ATAPI completion with nIEN set"
    );

    let stop = machine.run_until_halt_or_cycles(50_000_000).unwrap();

    assert_eq!(stop, StopReason::Halted);
    let elapsed = machine.master_ticks() - start;
    assert!(
        elapsed >= deadline && elapsed < deadline + TICKS_PER_US,
        "the skip stopped at the completion, not past it: elapsed={elapsed} deadline={deadline}"
    );
    assert_eq!(counters(&machine).spans, 1);
}

// ---------------------------------------------------------------------------
// 4. Skipping and spinning reach the same device state.
// ---------------------------------------------------------------------------

/// The weakened analogue of `assert_poll_machine_boundary_eq`.
///
/// **THIS DELIBERATELY DOES NOT COMPARE REGISTERS OR RETIRED INSTRUCTIONS.**
/// The elided iterations are not executed, so the register file at a boundary
/// and the instruction count both legitimately differ -- by construction, not
/// by accident. What IS claimed, and what this compares, is that at a common
/// guest instant every device is in the same state and the drive delivered the
/// same bytes.
#[test]
fn ata_poll_skip_and_spinning_reach_the_same_device_state() {
    let mut skipped = poll_machine(true);
    let mut spinning = poll_machine(false);
    let deadline = {
        let d = schedule_identify(&mut skipped);
        assert_eq!(d, schedule_identify(&mut spinning));
        d
    };

    for machine in [&mut skipped, &mut spinning] {
        assert_eq!(
            machine.run_until_halt_or_cycles(50_000_000).unwrap(),
            StopReason::Halted
        );
    }
    assert!(
        skipped.master_ticks() <= spinning.master_ticks(),
        "the skip reaches the completion no later than the spin does"
    );

    // Normalise both to one common guest instant, well past the completion, so
    // "same state" is a statement about the devices and not about where each
    // machine happened to stop.
    let target = spinning.master_ticks().max(skipped.master_ticks()) + deadline * 4;
    for machine in [&mut skipped, &mut spinning] {
        let delta = target - machine.master_ticks();
        machine.advance_devices_ticks(delta);
    }

    assert_eq!(
        skipped.ide.transport_state_snapshot(),
        spinning.ide.transport_state_snapshot(),
        "the whole ATAPI transport projection is identical"
    );
    assert_eq!(skipped.pit, spinning.pit, "PIT");
    assert_eq!(skipped.pic, spinning.pic, "PIC");
    assert_eq!(
        skipped.cd_pio_byte_count(),
        spinning.cd_pio_byte_count(),
        "cd_pio_bytes is the fidelity falsifier and it is byte-exact"
    );
    assert_eq!(
        skipped.atapi_packet_command_count(),
        spinning.atapi_packet_command_count(),
        "the same commands ran"
    );
    assert_eq!(
        skipped.master_ticks(),
        spinning.master_ticks(),
        "the normalisation is exact"
    );
}

// ---------------------------------------------------------------------------
// 5. The two reset edges.
// ---------------------------------------------------------------------------

#[test]
fn ata_poll_skip_declines_while_a_command_is_not_pending() {
    let mut machine = bus_machine(true);
    assert!(machine.ide.ticks_until_completion().is_none());

    // TOKACD reads 0x376 outside any loop at `tokacd.asm:1506`, immediately
    // after `wait_not_busy` returns. Without the explicit zeroing arm those
    // reads would carry a run across a completion boundary.
    poll_alt_status(&mut machine, 64);

    assert!(
        !machine.ata_poll_skip_armed,
        "nothing pending, nothing armed"
    );
    assert_eq!(
        machine.ide.alt_status_run_for_test(),
        0,
        "and zeroed, not frozen"
    );
    assert_eq!(counters(&machine).arms, 0);
}

#[test]
fn ata_poll_skip_resets_on_a_control_write() {
    let mut machine = bus_machine(true);
    schedule_identify(&mut machine);
    poll_alt_status(&mut machine, ide::ATA_POLL_RUN - 1);
    assert_eq!(
        machine.ide.alt_status_run_for_test(),
        ide::ATA_POLL_RUN - 1,
        "one read short of the threshold"
    );

    // A control write is not a poll. It clears the run AND latches the block,
    // because it can trigger an SRST that clears `pending_command` mid-batch.
    out(&mut machine, ide::SECONDARY_CTRL, 0x02);

    assert_eq!(machine.ide.alt_status_run_for_test(), 0);
    assert!(machine.ide.poll_skip_blocked());
    poll_alt_status(&mut machine, ide::ATA_POLL_RUN * 2);
    assert!(
        !machine.ata_poll_skip_armed,
        "an armed flag raised before a control write cannot be honoured after it"
    );
    assert_eq!(counters(&machine).arms, 0);
}

/// Any OTHER read on the channel breaks the run: an arm means "N alt-status
/// reads with no other I/O to the channel", which is much tighter than "N reads
/// eventually".
#[test]
fn ata_poll_skip_resets_on_any_other_channel_read() {
    let mut machine = bus_machine(true);
    schedule_identify(&mut machine);
    poll_alt_status(&mut machine, ide::ATA_POLL_RUN - 1);
    let _ = input(&mut machine, ide::SECONDARY_CMD_BASE + 1); // error register
    assert_eq!(machine.ide.alt_status_run_for_test(), 0);
    assert_eq!(counters(&machine).arms, 0);
}

// ---------------------------------------------------------------------------
// 6. A bus-master DMA deadline bounds the skip.
// ---------------------------------------------------------------------------

#[test]
fn ata_poll_skip_never_crosses_a_bmide_deadline() {
    let mut machine = machine_with_hdd(16);
    machine.set_mode(GswMode::Gsw586);
    machine.mount_cd(data_disc(16));
    machine.set_ata_poll_skip_enabled(true);

    // Set up the secondary-channel wait FIRST -- a far-seek packet read, tens
    // of milliseconds -- because arming it advances the timeline and would eat
    // into a DMA transfer armed earlier. The skip is armed by the SECONDARY
    // channel; the bound has to come from the primary's DMA all the same.
    schedule_far_read(&mut machine);
    const PRD: u32 = 0x1000;
    machine.write_physical_u32(PRD, 0x2000);
    machine.write_physical_u32(PRD + 4, 0x8000_0200);
    with_bus(&mut machine, |bus| {
        bus.write_io(0xf004, BusWidth::Dword, PRD, false).unwrap();
        bus.write_io(0xf000, BusWidth::Byte, 0x09, false).unwrap();
    });
    for (port, value) in [
        (ata::PRIMARY_CMD_BASE + 2, 1u32),
        (ata::PRIMARY_CMD_BASE + 3, 2),
        (ata::PRIMARY_CMD_BASE + 4, 0),
        (ata::PRIMARY_CMD_BASE + 5, 0),
        (ata::PRIMARY_CMD_BASE + 6, 0x40),
        (ata::PRIMARY_CMD_BASE + 7, 0xc8),
    ] {
        with_bus(&mut machine, |bus| {
            bus.write_io(port, BusWidth::Byte, value, false).unwrap();
        });
    }
    let dma = machine
        .bmide
        .ticks_until_completion()
        .expect("a bus-master transfer is in flight");
    let ata = machine
        .ide
        .ticks_until_completion()
        .expect("the packet read is still pending");
    assert!(
        dma < ata,
        "the fixture is vacuous unless the DMA deadline is nearer: dma={dma} ata={ata}"
    );
    assert!(dma >= machine.ata_poll_floor_ticks);

    poll_alt_status(&mut machine, ide::ATA_POLL_RUN);
    assert!(machine.ata_poll_skip_armed);
    let before = machine.master_ticks();
    machine.actuate_ata_poll_skip(before + ata * 8, false);

    assert_eq!(
        machine.master_ticks() - before,
        dma,
        "the skip stopped on the DMA boundary, not on its own channel's completion"
    );
    assert_eq!(counters(&machine).spans, 1);
    assert_eq!(counters(&machine).blocks, 0);
}

// ---------------------------------------------------------------------------
// 7. The mandatory ATA precondition, ordinary route.
// ---------------------------------------------------------------------------

/// The arm fires, then the batch's OWN end-of-batch advance crosses the
/// completion before the actuation runs.
///
/// Without the mandatory `ticks_until_completion()` precondition the target
/// would fall through the optional ATA term inside `next_device_edge_ticks` to
/// an unrelated edge -- PIT channel 0 at 18.2 Hz is up to 54.9 ms away -- and
/// the machine would grant the guest that whole span with the drive READY,
/// charged as `io_stall_clocks`: invisible to `cd_pio_bytes`, invisible to the
/// frame anchor, and visible only as wall no ladder could attribute.
#[test]
fn ata_poll_skip_declines_when_the_command_completed_in_the_same_batch() {
    let mut machine = bus_machine(true);
    // Drop the floor so a nearly-elapsed command still passes the ARM-time
    // check; the completion is what this fixture is about, not the floor.
    machine.set_ata_poll_skip_tuning(ide::ATA_POLL_RUN, 1);
    let deadline = schedule_identify(&mut machine);
    machine.advance_devices_ticks(deadline - 2 * TICKS_PER_US);

    poll_alt_status(&mut machine, ide::ATA_POLL_RUN);
    assert!(
        machine.ata_poll_skip_armed,
        "the arm fired with time to spare"
    );

    // Now let the completion pass, exactly as the batch's own `advance_cpu_work`
    // would, and only THEN actuate.
    machine.advance_devices_ticks(2 * TICKS_PER_US);
    assert!(machine.ide.ticks_until_completion().is_none());
    let before = machine.master_ticks();
    let stalls_before = machine.io_stall_ticks();
    machine.actuate_ata_poll_skip(before + 1_000 * TICKS_PER_US, false);

    let c = counters(&machine);
    assert_eq!(c.declines_not_pending, 1, "{c:?}");
    assert_eq!(c.spans, 0);
    assert_eq!(machine.master_ticks(), before, "master_ticks did not move");
    assert_eq!(
        machine.io_stall_ticks(),
        stalls_before,
        "nothing was stalled"
    );
    assert!(
        !machine.ata_poll_skip_armed,
        "the flag is TAKEN unconditionally so it can never be stranded"
    );
}

/// **THE FIXTURE THAT ACTUALLY KILLS THE MUTANT.** Same route as the one above,
/// but on a machine with a far unrelated edge armed.
///
/// The fixture above asserts the right thing and still cannot fail against a
/// mutant that deletes the precondition: `bus_machine` arms no device, so its
/// `next_device_edge_ticks()` is `None` and there is nothing to fall through TO.
/// Its `master_ticks() == before` is then satisfied by the ABSENCE of the hazard
/// rather than by the guard -- the `fixtures-that-cannot-fail` class.
///
/// With PIT channel 0 running at the DOS default the fall-through target exists
/// and is 54.8 ms away. A machine that granted the guest that span with the
/// drive READY would charge it as `io_stall_clocks`: invisible to
/// `cd_pio_bytes`, invisible to the frame anchor because guest time still
/// advances, and visible only as wall no ladder could attribute. That is the
/// sharpest failure mode in the design, and this is the only fixture in the file
/// that would see it.
#[test]
fn ata_poll_skip_declines_with_a_far_unrelated_edge_and_no_pending_command() {
    let mut machine = bus_machine(true);
    arm_far_pit_edge(&mut machine);

    let deadline = schedule_identify(&mut machine);
    poll_alt_status(&mut machine, ide::ATA_POLL_RUN);
    assert!(machine.ata_poll_skip_armed, "the arm fired");

    // Route 1 to the no-pending state: the batch's own advance crosses it.
    machine.advance_devices_ticks(deadline);
    assert!(machine.ide.ticks_until_completion().is_none());

    let far = machine
        .next_device_edge_ticks()
        .expect("a far PIT edge is armed");
    assert!(
        far > machine.ata_poll_floor_ticks * 100,
        "VACUOUS unless the fall-through edge is far: {far} ticks"
    );

    let before = machine.master_ticks();
    let stalls_before = machine.io_stall_ticks();
    machine.actuate_ata_poll_skip(before + far * 8, false);

    let c = counters(&machine);
    assert_eq!(c.declines_not_pending, 1, "{c:?}");
    assert_eq!(c.spans, 0);
    assert_eq!(
        machine.master_ticks(),
        before,
        "with no pending command the machine must not grant the guest the unrelated PIT \
         edge's whole span with the drive READY"
    );
    assert_eq!(
        machine.io_stall_ticks(),
        stalls_before,
        "and must charge nothing as I/O stall, which is where such a grant would hide"
    );
}

// ---------------------------------------------------------------------------
// 8. The mandatory ATA precondition, ring-0 SRST route.
// ---------------------------------------------------------------------------

/// A ring-0-protected-mode write to 0x376 in the Approximate class does NOT end
/// the batch: the write-side carve-out at the top of `write_io` names
/// 0x60/0x64/0x92/0xE7 and the PCI config window, and 0x376 is in none of them.
/// So an SRST can clear `pending_command` mid-batch with the skip already armed.
///
/// The first draft of the design argued this away with the wrong citation
/// (`read_io`'s unconditional set, not `write_io`'s conditional one) and the
/// wrong conclusion ("it cannot happen"). It is HANDLED instead, and this pins
/// the sufficient guard.
#[test]
fn ata_poll_skip_declines_after_a_ring0_soft_reset_inside_the_batch() {
    let mut machine = bus_machine(true);
    // The far edge for the same reason as route 1: without something to fall
    // through TO, the assertion below cannot fail against a mutant that drops
    // the precondition.
    arm_far_pit_edge(&mut machine);
    schedule_identify(&mut machine);
    poll_alt_status(&mut machine, ide::ATA_POLL_RUN);
    assert!(machine.ata_poll_skip_armed);

    machine.io_touched = false;
    with_bus(&mut machine, |bus| {
        // SRST from ring-0 protected mode, the exact hazard geometry.
        bus.write_io(ide::SECONDARY_CTRL, BusWidth::Byte, 0x04, true)
            .unwrap();
    });
    assert!(
        !machine.io_touched,
        "0x376 is not in the write-side carve-out, so this write did NOT end the batch -- \
         which is what makes the hazard real"
    );
    assert!(
        machine.exempt_io_touched,
        "it signalled through the exempt flag instead"
    );
    assert!(
        machine.ide.ticks_until_completion().is_none(),
        "soft_reset cleared the pending command mid-batch"
    );

    let far = machine
        .next_device_edge_ticks()
        .expect("a far PIT edge is armed");
    assert!(
        far > machine.ata_poll_floor_ticks * 100,
        "VACUOUS unless the fall-through edge is far: {far} ticks"
    );

    let before = machine.master_ticks();
    machine.actuate_ata_poll_skip(before + far * 8, false);

    let c = counters(&machine);
    assert_eq!(c.declines_not_pending, 1, "guard 2 declines it: {c:?}");
    assert_eq!(c.spans, 0);
    assert_eq!(
        machine.master_ticks(),
        before,
        "the armed flag produced no stall -- not even the far PIT edge's span"
    );
    assert!(machine.ide.poll_skip_blocked(), "guard 1 latched as well");
}

// ---------------------------------------------------------------------------
// 9. The ring-0 monitor port exemption is preserved.
// ---------------------------------------------------------------------------

/// **THE THIRD ASSERTION IS THE DISCRIMINATOR.** An earlier draft credited the
/// exclusion of the monitor's own reads to `!io_touched_before_read`. That is
/// false: the snapshot is taken before the general `io_touched` set, and under
/// the exemption nothing sets the flag, so it reads `false` and every term of
/// the predicate was satisfied in exactly the case it claimed to exclude. The
/// arm would have fired silently while `monitor_exempt` read 0 as apparent
/// evidence the case never arose.
#[test]
fn ata_poll_skip_preserves_the_ring0_monitor_port_exemption() {
    let mut machine = bus_machine(true);
    schedule_identify(&mut machine);
    machine.io_touched = false;
    machine.exempt_io_touched = false;

    with_bus(&mut machine, |bus| {
        for _ in 0..ide::ATA_POLL_RUN * 4 {
            // cpu_is_ring0_pm = true: the TOKAEMM monitor's own read.
            bus.read_io(ide::SECONDARY_CTRL, BusWidth::Byte, 0, true)
                .unwrap();
        }
    });

    assert!(
        !machine.io_touched,
        "the documented V86-trap-tax contract holds: the monitor's poke does not end the batch"
    );
    assert!(
        machine.exempt_io_touched,
        "it signals through exempt_io_touched, as it always has"
    );
    let c = counters(&machine);
    assert!(
        c.monitor_exempt > 0,
        "the decline is real and counted: {c:?}"
    );
    assert_eq!(
        c.arms, 0,
        "and the arm did NOT fire -- this is the assertion that fails against a predicate \
         with no skip_io_touched term"
    );
    assert!(!machine.ata_poll_skip_armed);
}

// ---------------------------------------------------------------------------
// 10. The floor, the latch, and what clears it.
// ---------------------------------------------------------------------------

#[test]
fn ata_poll_skip_declines_below_the_floor_and_blocks_one_command() {
    let mut machine = bus_machine(true);
    let deadline = schedule_identify(&mut machine);
    // Leave a device-bounded target under the floor by consuming almost all of
    // the ATA window. This is the DEVICE cause, and it is the one that latches.
    machine.advance_devices_ticks(deadline - machine.ata_poll_floor_ticks / 2);

    // The arm-time floor also declines here and does so WITHOUT latching, so
    // drop the arm-time floor to isolate the batch-end check that does.
    machine.set_ata_poll_skip_tuning(ide::ATA_POLL_RUN, 1);
    poll_alt_status(&mut machine, ide::ATA_POLL_RUN);
    assert!(machine.ata_poll_skip_armed);
    // Restore the shipped floor for the batch-end decision.
    machine.set_ata_poll_skip_tuning(ide::ATA_POLL_RUN, ide::ATA_POLL_FLOOR_TICKS);

    let before = machine.master_ticks();
    machine.actuate_ata_poll_skip(before + 1_000 * TICKS_PER_US, false);

    let c = counters(&machine);
    assert_eq!(c.declines_below_floor, 1, "{c:?}");
    assert_eq!(c.blocks, 1, "and it is the ONLY decline that latches");
    assert_eq!(
        c.declines_deadline_clamped, 0,
        "the R2-B split has not regressed"
    );
    assert_eq!(c.spans, 0);
    assert_eq!(machine.master_ticks(), before);
    assert!(machine.ide.poll_skip_blocked());

    // A second full run of reads does not arm while the latch is set: at most
    // ONE wasted batch break per pending command.
    poll_alt_status(&mut machine, ide::ATA_POLL_RUN * 3);
    assert!(!machine.ata_poll_skip_armed);
    assert_eq!(counters(&machine).arms, c.arms, "no further arm");
    assert_eq!(
        machine.ide.alt_status_run_for_test(),
        0,
        "zeroed rather than frozen, so a later clear cannot arm on a stale count"
    );

    // The next command clears it: each of TOKACD's three per-sector phases is
    // its own `schedule`, so each gets its own chance. Let this command finish
    // and drain its data block so the channel is back at Phase::Idle and will
    // accept another.
    machine.advance_devices_ticks(machine.ide.ticks_until_completion().unwrap_or(0));
    let _ = input(&mut machine, ide::SECONDARY_CMD_BASE + 7);
    for _ in 0..512 {
        let _ = input(&mut machine, ide::SECONDARY_CMD_BASE);
    }
    assert!(
        machine.ide.poll_skip_blocked(),
        "still latched before the new command"
    );
    schedule_identify(&mut machine);
    assert!(
        !machine.ide.poll_skip_blocked(),
        "schedule() cleared the latch"
    );
    poll_alt_status(&mut machine, ide::ATA_POLL_RUN);
    assert!(machine.ata_poll_skip_armed, "and the guest re-arms");
}

// ---------------------------------------------------------------------------
// 11. The armed flag does not survive a batch.
// ---------------------------------------------------------------------------

#[test]
fn ata_poll_skip_armed_flag_does_not_survive_a_batch() {
    let mut machine = poll_machine(true);
    schedule_identify(&mut machine);
    poll_alt_status(&mut machine, ide::ATA_POLL_RUN);
    assert!(
        machine.ata_poll_skip_armed,
        "armed outside the run loop, which is the state a batch must not inherit"
    );
    let arms_before = counters(&machine).arms;

    // Entering the run loop clears the flag AND the run counter at batch entry.
    machine.cpu.registers.eip = 0x108; // the HLT: one batch, no polling
    let before = machine.master_ticks();
    let _ = machine.run_master_ticks(TICKS_PER_US).unwrap();

    let c = counters(&machine);
    assert_eq!(
        c.spans, 0,
        "no phantom skip fired behind a stale arm: {c:?}"
    );
    assert_eq!(c.declines_not_pending, 0);
    assert_eq!(c.declines_below_floor, 0);
    assert_eq!(c.declines_deadline_clamped, 0);
    assert_eq!(c.arms, arms_before, "and no new arm was manufactured");
    assert_eq!(
        c.declines_halted, 0,
        "the stale arm was cleared at batch ENTRY, so the halt arm never saw it either -- \
         stated rather than left to the incidental fact that a halted slice returns before \
         the actuation: {c:?}"
    );
    assert!(!machine.ata_poll_skip_armed);
    assert_eq!(
        machine.io_stall_ticks(),
        0,
        "nothing was stalled: master_ticks moved only by the batch's own work"
    );
    assert!(machine.master_ticks() >= before);
}

/// **THE CERTIFICATE, pinned.** An arm must mean "N alt-status reads inside ONE
/// CPU batch with no other I/O to the channel", not the much weaker "N reads
/// eventually".
///
/// The batch-entry `reset_alt_status_run()` is the sole mitigation for design
/// §7 risk 2 -- "a guest that polls while doing useful work would be robbed of
/// it" -- and it is the one architectural hazard the design accepts rather than
/// eliminates. Nothing on the board can see it, because under TOKACD the run
/// reaches the threshold inside the first batch anyway; without a fixture, the
/// line is deletable with every other test green.
///
/// So: get one read short of the threshold, cross a batch boundary by a route
/// that touches no channel port, and take one more read. It must NOT arm, and
/// the run must read 1 -- the reads on either side of the boundary must not
/// accumulate.
#[test]
fn ata_poll_skip_run_does_not_accumulate_across_a_batch_boundary() {
    let mut machine = poll_machine(true);
    let deadline = schedule_identify(&mut machine);
    poll_alt_status(&mut machine, ide::ATA_POLL_RUN - 1);
    assert_eq!(
        machine.ide.alt_status_run_for_test(),
        ide::ATA_POLL_RUN - 1,
        "one read short of the threshold"
    );
    assert!(!machine.ata_poll_skip_armed);

    // One batch through the real run loop, by a route that touches no channel
    // port: the guest's HLT. This is the batch boundary the certificate is about.
    //
    // The slice is deliberately ABOVE the floor. A sub-floor slice would leave
    // `ata_poll_skip_slice_too_short` set, the arm predicate would decline
    // before counting, and the final read below would not increment the run --
    // which would make this fixture pass against the very mutant it exists to
    // kill. The two vacuity guards after the run call are what keep that honest.
    machine.cpu.registers.eip = 0x108;
    let _ = machine
        .run_master_ticks(machine.ata_poll_floor_ticks * 4)
        .unwrap();
    assert!(
        machine.ide.ticks_until_completion().is_some(),
        "VACUOUS if the command completed across the boundary: the run would then reset on \
         the no-pending arm instead of on the batch-entry one. deadline was {deadline}"
    );
    assert!(
        !machine.ata_poll_skip_slice_too_short,
        "VACUOUS if the last batch entered a sub-floor slice: the read below would then be \
         refused by the slice test rather than counted into a fresh run"
    );

    // The read that would have been the sixteenth of an unbroken run.
    poll_alt_status(&mut machine, 1);

    let c = counters(&machine);
    assert_eq!(
        c.arms, 0,
        "reads in different batches must not accumulate into an arm: {c:?}"
    );
    assert_eq!(
        machine.ide.alt_status_run_for_test(),
        1,
        "the run restarted at the batch boundary and this read is its first"
    );
    assert!(!machine.ata_poll_skip_armed);
    assert_eq!(c.spans, 0);
}

// ---------------------------------------------------------------------------
// 12. The run deadline truncates WITHOUT latching. The interactive-path
//     regression, and the one no headless leg would ever catch.
// ---------------------------------------------------------------------------

/// The GUI drives `run_master_ticks(execution_budget(credit, approximate))`, and
/// `execution_budget` clamps to `FAST_EMU_QUANTUM_TICKS = MASTER_CLOCK_HZ/1000`
/// = **1 ms** whenever the mode is approximate -- i.e. exactly the class this
/// lever is scoped to. That is SHORTER than the 1.111 ms `sector_transfer_ticks`
/// this lever's largest win is, so the slice boundary lands inside the wait
/// essentially every time.
///
/// If a deadline-clamped sub-floor tail latched `poll_skip_blocked`, nothing
/// would clear it until the sector completed and the guest would spin out the
/// remaining ~1.1 ms interpreted -- systematically, in the GUI only, invisible
/// to every headless leg on the board.
#[test]
fn ata_poll_skip_does_not_block_when_the_run_deadline_truncates() {
    let mut machine = poll_machine(true);
    // A wait LONGER THAN ONE SLICE is the whole geometry: on the shipped
    // workload it is the 1.111 ms `sector_transfer_ticks` against the 1 ms
    // quantum. A far-seek packet read gives the same shape with room to spare.
    let deadline = schedule_far_read(&mut machine);
    assert!(
        deadline > GUI_SLICE_TICKS,
        "the wait must outlast one slice for this to bite: deadline={deadline} \
         slice={GUI_SLICE_TICKS}"
    );

    // PHASE A -- the real GUI geometry. Each 1 ms slice arms, clamps its target
    // to the remaining slice, and STILL COMMITS because the clamped residue
    // clears the floor. This is "the win largely survives on the interactive
    // path": most of each slice is skipped and the next slice takes the rest.
    const SLICES: u32 = 6;
    let mut slices_run = 0u32;
    for _ in 0..SLICES {
        slices_run += 1;
        if machine.run_master_ticks(GUI_SLICE_TICKS).unwrap() == StopReason::Halted {
            break;
        }
        let c = counters(&machine);
        assert!(
            !machine.ide.poll_skip_blocked(),
            "the CALLER's deadline is not the device-edge pathology and must NEVER latch: {c:?}"
        );
        assert_eq!(
            c.declines_below_floor, 0,
            "and it must not be miscounted as the device cause"
        );
        assert_eq!(
            c.blocks, 0,
            "ata_poll_skip_blocks must not track _deadline_clamped -- if it does, the split \
             separating the two truncation causes has regressed"
        );
    }
    let after_a = counters(&machine);
    assert_eq!(
        after_a.declines_deadline_clamped, 0,
        "AT THE REAL GUI QUANTUM THE CLAMP DOES NOT EVEN FIRE. A 1 ms slice against a longer \
         wait arms ~2 us in, so the clamped target is ~1 ms -- far above the floor -- and it \
         COMMITS, landing exactly on the slice deadline. The sub-floor branch needs a slice \
         whose remainder at the arm instant is already under 20 us, which phase B builds \
         deliberately: {after_a:?}"
    );
    assert!(
        after_a.spans >= 2,
        "slice after slice arms and commits: the mechanism is not wedged, which is exactly \
         what a false latch would have done: {after_a:?}"
    );
    assert!(
        machine.ide.ticks_until_completion().is_some(),
        "the wait is still running, so the following phase is not vacuous"
    );

    // PHASE B -- THE SUB-FLOOR SLICE, after the interactive mitigation.
    //
    // This phase used to build a clamped decline deliberately and assert that it
    // did not latch. Since the mitigation, a batch entered with less than the
    // floor remaining does not arm AT ALL, so there is no decline to count: the
    // waste is removed rather than merely made harmless. What is asserted here
    // is that stronger property.
    //
    // The clamped decline's own disposition -- declines, does NOT latch -- is
    // still pinned, twice: at the actuation by
    // `ata_poll_skip_blocks_when_a_device_edge_truncates_but_not_when_the_deadline_does`,
    // which arms outside the run loop and calls `actuate_ata_poll_skip` with a
    // sub-floor deadline directly; and at scale by the interactive confirmation
    // run, where 364,753 real clamped declines moved `blocks` by exactly zero.
    // The mitigation is not an elimination -- a batch entered just above the
    // floor can still arm and clamp -- so that disposition still matters.
    let short_slice = machine.ata_poll_floor_ticks / 2;
    let _ = machine.run_master_ticks(short_slice).unwrap();
    let after_b = counters(&machine);
    assert_eq!(
        after_b.arms, after_a.arms,
        "a sub-floor slice must not arm: {after_b:?}"
    );
    assert_eq!(
        after_b.declines_deadline_clamped, 0,
        "and so must not produce a clamped decline either -- one counted here would mean          the arm fired and then declined, which is the forced batch break the mitigation          removes: {after_b:?}"
    );
    assert_eq!(
        after_b.declines_below_floor, 0,
        "the DEVICE cause did not fire: {after_b:?}"
    );
    assert_eq!(after_b.blocks, 0, "and nothing latched: {after_b:?}");
    assert!(
        !machine.ide.poll_skip_blocked(),
        "THE REGRESSION THIS FIXTURE EXISTS FOR: had a run-deadline truncation ever latched,          nothing would clear it until the command completed and the guest would spin out the          remainder at full interpreted cost -- in the GUI only, invisible to every headless          leg on the board"
    );
    let _ = slices_run;

    // And the next full slice arms and commits again, so the decline really was
    // benign rather than a quiet wedge.
    let _ = machine.run_master_ticks(GUI_SLICE_TICKS).unwrap();
    let after_c = counters(&machine);
    assert!(
        after_c.spans > after_b.spans,
        "the following slice committed a skip: {after_c:?}"
    );
}

/// The device-edge variant of the same geometry, so the two causes are pinned
/// AGAINST EACH OTHER rather than one in isolation. Same sub-floor remainder,
/// different cause, opposite disposition.
#[test]
fn ata_poll_skip_blocks_when_a_device_edge_truncates_but_not_when_the_deadline_does() {
    // Cause A: the caller's deadline. Declines, does not latch.
    let mut by_deadline = bus_machine(true);
    let ata = schedule_identify(&mut by_deadline);
    by_deadline.set_ata_poll_skip_tuning(ide::ATA_POLL_RUN, 1);
    poll_alt_status(&mut by_deadline, ide::ATA_POLL_RUN);
    by_deadline.set_ata_poll_skip_tuning(ide::ATA_POLL_RUN, ide::ATA_POLL_FLOOR_TICKS);
    assert!(by_deadline.ata_poll_skip_armed);
    let now = by_deadline.master_ticks();
    by_deadline.actuate_ata_poll_skip(now + by_deadline.ata_poll_floor_ticks / 2, false);
    let a = counters(&by_deadline);
    assert_eq!(a.declines_deadline_clamped, 1, "{a:?}");
    assert_eq!(a.declines_below_floor, 0);
    assert_eq!(a.blocks, 0);
    assert!(!by_deadline.ide.poll_skip_blocked(), "no latch");

    // Cause B: a device edge. Declines AND latches. Same remainder size.
    let mut by_edge = bus_machine(true);
    let ata_b = schedule_identify(&mut by_edge);
    assert_eq!(ata, ata_b);
    by_edge.advance_devices_ticks(ata_b - by_edge.ata_poll_floor_ticks / 2);
    by_edge.set_ata_poll_skip_tuning(ide::ATA_POLL_RUN, 1);
    poll_alt_status(&mut by_edge, ide::ATA_POLL_RUN);
    by_edge.set_ata_poll_skip_tuning(ide::ATA_POLL_RUN, ide::ATA_POLL_FLOOR_TICKS);
    assert!(by_edge.ata_poll_skip_armed);
    let now = by_edge.master_ticks();
    by_edge.actuate_ata_poll_skip(now + ata_b * 8, false);
    let b = counters(&by_edge);
    assert_eq!(b.declines_below_floor, 1, "{b:?}");
    assert_eq!(b.blocks, 1);
    assert_eq!(b.declines_deadline_clamped, 0);
    assert!(
        by_edge.ide.poll_skip_blocked(),
        "the latch IS the point here"
    );
}

// ---------------------------------------------------------------------------
// 14. A slice that cannot pay for a skip does not arm at all.
// ---------------------------------------------------------------------------

/// **THE INTERACTIVE MITIGATION, and it was measured rather than modelled.**
///
/// The design assumed the GUI slice is `FAST_EMU_QUANTUM_TICKS` = 1 ms. The
/// interactive confirmation run says the real median is ~13 µs, because
/// `execution_budget` is `min(credit, quantum)` and in a paced window the
/// CREDIT binds: a machine on time refills it in tiny wall-clock increments and
/// spends it immediately. Most interactive slices are therefore BELOW the 20 µs
/// floor, so a skip armed in one can never commit — 364,753 `_deadline_clamped`
/// declines in a single 328 s window, each costing a forced batch break and a
/// device-edge-cache invalidation for nothing.
///
/// So a batch whose remaining deadline is already sub-floor must not arm. The
/// discriminator is that `_deadline_clamped` does **not** move: a decline that
/// counted there would mean the arm fired and then declined, which is the waste
/// this exists to remove.
#[test]
fn ata_poll_skip_does_not_arm_into_a_sub_floor_slice() {
    let mut machine = poll_machine(true);
    let deadline = schedule_far_read(&mut machine);
    assert!(deadline > machine.ata_poll_floor_ticks * 100);

    // A slice far shorter than the floor: the interactive regime, at its median.
    let short = machine.ata_poll_floor_ticks / 2;
    for _ in 0..8 {
        let _ = machine.run_master_ticks(short).unwrap();
    }

    let c = counters(&machine);
    assert_eq!(
        c.arms, 0,
        "a sub-floor slice cannot pay for a skip, so it must not arm: {c:?}"
    );
    assert_eq!(
        c.declines_deadline_clamped, 0,
        "AND THIS IS THE DISCRIMINATOR: a decline counted here would mean the arm fired \
         and then declined -- the forced batch break this mitigation exists to remove. \
         Zero means it never armed. {c:?}"
    );
    assert_eq!(c.spans, 0);
    assert_eq!(c.blocks, 0, "and nothing latched: {c:?}");
    assert!(!machine.ide.poll_skip_blocked());
    assert!(
        machine.ide.ticks_until_completion().is_some(),
        "VACUOUS if the command completed: the arm would have declined on the no-pending \
         arm instead of on the slice test"
    );

    // A slice that CAN pay still arms and still commits, so the mitigation is
    // scoped to the case it names and has not disarmed the mechanism.
    let _ = machine.run_master_ticks(GUI_SLICE_TICKS).unwrap();
    let c = counters(&machine);
    assert!(
        c.spans >= 1,
        "a slice above the floor arms and commits as before: {c:?}"
    );
}

// ---------------------------------------------------------------------------
// The gate itself.
// ---------------------------------------------------------------------------

/// The spelling table, and THE DEFAULT PIN, two-sided.
///
/// `IZARRAVM_ATA_POLL_SKIP` is OFF by default in this landing. THE DAY THE
/// DEFAULT FLIPS this assertion is what forces the flip commit to say so, and
/// the empty-string row is what stops a PowerShell `$env:X = $null` leg from
/// silently measuring the wrong arm -- the trap that voided three earlier
/// evidence directories.
#[test]
fn ata_poll_skip_env_spelling_table_and_default() {
    use std::env::VarError;
    assert!(
        !run::parse_ata_poll_skip_arm_for_test(Err(VarError::NotPresent)),
        "IZARRAVM_ATA_POLL_SKIP must default OFF in this landing; flipping it is a separate \
         commit that must move this assertion"
    );
    for off in ["", "0", "off", "OFF", "  0  "] {
        assert!(
            !run::parse_ata_poll_skip_arm_for_test(Ok(off.to_string())),
            "{off:?} names the OFF arm"
        );
    }
    for on in ["1", "on", "ON", " on "] {
        assert!(
            run::parse_ata_poll_skip_arm_for_test(Ok(on.to_string())),
            "{on:?} names the ON arm"
        );
    }
}

/// The SWEEP knobs panic on a typo too, and this is not symmetry for its own
/// sake: their first real use will be a sweep, which is exactly the run a silent
/// fallback poisons. `IZARRAVM_ATA_POLL_RUN=sixteen` used to run 16 without a
/// word, and a mistyped sweep leg would have read as "that threshold changed
/// nothing" -- the same trap the main gate already refuses.
#[test]
fn ata_poll_skip_sweep_knobs_panic_on_a_typo() {
    use std::env::VarError;
    assert_eq!(
        run::sweep_knob_for_test("X", Err(VarError::NotPresent)),
        None,
        "unset is a spelling of 'the default'"
    );
    for empty in ["", "   "] {
        assert_eq!(
            run::sweep_knob_for_test("X", Ok(empty.to_string())),
            None,
            "AND SO IS EMPTY, which is not a convenience: PowerShell's \
             SetEnvironmentVariable(name, $null) -- how every harness here clears a variable \
             -- leaves it PRESENT AND EMPTY. Making empty panic killed this branch's own \
             re-ladder on its first run, six legs in 60 ms each. A numeric knob has no 'off' \
             value, so empty cannot mean anything but the default."
        );
    }
    assert_eq!(
        run::sweep_knob_for_test("X", Ok("  24  ".to_string())),
        Some(24),
        "a number still parses, trimmed"
    );
    for typo in ["sixteen", "20us", "-1", "1.5"] {
        let result =
            std::panic::catch_unwind(|| run::sweep_knob_for_test("X", Ok(typo.to_string())));
        assert!(result.is_err(), "sweep knob {typo:?} must panic");
    }
}

/// A typo must PANIC rather than silently run the default: a mistyped ladder leg
/// that fell through would be read as "the arm I asked for changed nothing".
#[test]
fn ata_poll_skip_env_typo_panics() {
    for typo in ["yes", "true", "enabled", "2"] {
        let result = std::panic::catch_unwind(|| {
            run::parse_ata_poll_skip_arm_for_test(Ok(typo.to_string()))
        });
        assert!(
            result.is_err(),
            "IZARRAVM_ATA_POLL_SKIP={typo:?} must panic"
        );
    }
}
