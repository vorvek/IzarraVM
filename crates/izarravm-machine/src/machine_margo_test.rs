// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn vbe_set_mode_selects_a_margo_mode() {
    let rom = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x41, // mov bx, 0101h | 4000h (LFB)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    assert_eq!(machine.margo().display().width, 640);
    assert_eq!(machine.margo().display().height, 480);
}

#[test]
fn vbe_set_mode_then_vga_mode_follows_the_display() {
    let rom = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x41, // mov bx, 0101h | 4000h
        0xcd, 0x10, // int 10h
        0xb8, 0x13, 0x00, // mov ax, 0013h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // The VGA mode-set hands the display back to VGA, but the 4F02 call must
    // still have set the Margo mode (width stays set; only margo_active clears).
    assert_eq!(machine.margo().display().width, 640);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
}

#[test]
fn vbe_set_mode_accepts_hi_color_modes() {
    let rom = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x11, 0x41, // mov bx, 0111h | 4000h (640x480x16, linear frame buffer)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    assert_eq!(machine.margo().display().bpp, 16);
}

#[test]
fn vbe_current_mode_returns_the_set_mode() {
    let rom = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x41, // mov bx, 0101h | 4000h
        0xcd, 0x10, // int 10h
        0xb8, 0x03, 0x4f, // mov ax, 4F03h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
    assert_eq!(machine.cpu().registers.ebx() as u16, 0x4101);
}

/// Sample 0x3DA through the real port path every `sample_ticks` of machine time
/// for `window_ticks`, and report the mean master-tick period between rising
/// edges of bit 3 (vertical retrace), plus the number of edges seen.
///
/// Deliberately drives the guest-visible port rather than the device, so a fix
/// that only reaches the device API would not pass.
fn vretrace_period_ticks(
    machine: &mut Machine,
    sample_ticks: u64,
    window_ticks: u64,
) -> (u64, u32) {
    let mut previous = machine.read_io_port_u8(0x3da) & 0x08 != 0;
    let (mut first, mut last, mut edges) = (None, 0u64, 0u32);
    let mut elapsed = 0u64;
    while elapsed < window_ticks {
        machine.advance_devices_ticks(sample_ticks);
        elapsed += sample_ticks;
        let now = machine.read_io_port_u8(0x3da) & 0x08 != 0;
        if now && !previous {
            edges += 1;
            first.get_or_insert(elapsed);
            last = elapsed;
        }
        previous = now;
    }
    let first = first.expect("at least one vertical-retrace edge in the window");
    assert!(
        edges >= 2,
        "need two edges to measure a period, saw {edges}"
    );
    ((last - first) / u64::from(edges - 1), edges)
}

/// THE BUG THIS SLICE FIXES. `4F02h` never touched the legacy VGA CRTC, so while
/// a Margo mode was on screen 0x3DA reported the STALE legacy mode's retrace
/// rate while `Margo::advance_frames` latched page flips at 60 Hz. A guest that
/// paced on 0x3DA and flipped with `4F07h` -- which both VESA fixtures do -- ran
/// two display clocks well over 10% apart.
///
/// The test measures the rate a guest can actually observe, before and after the
/// mode set, on ONE machine. The "before" leg is not decoration: it is what makes
/// the "after" assertion non-vacuous, by showing the legacy clock this port used
/// to report is a different number that the fix had to stop reporting.
#[test]
fn margo_3da_reports_the_margo_frame_rate_not_the_stale_legacy_mode() {
    const SAMPLE_TICKS: u64 = 50_000; // ~7 samples inside a 2-line retrace window
    const WINDOW_TICKS: u64 = izarravm_core::MASTER_CLOCK_HZ / 4; // 0.25 s of machine time
    let margo_period = izarravm_core::MASTER_CLOCK_HZ / 60;

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.set_vga_mode(0x13));
    let (legacy_period, _) = vretrace_period_ticks(&mut machine, SAMPLE_TICKS, WINDOW_TICKS);
    assert!(
        legacy_period.abs_diff(margo_period) > margo_period / 100,
        "the legacy VGA mode must run at a MEASURABLY different rate from Margo \
         for this test to prove anything (legacy {legacy_period}, margo {margo_period})"
    );

    // Two modes with very different frame geometry (420,000 dots against
    // 1,083,264). Both must land on the SAME 60 Hz, which is what says the rate
    // comes from the frame clock and not from a dot count that happens to
    // resemble the legacy mode's.
    for mode in [0x0101u16, 0x0105] {
        assert!(machine.vega.set_vbe_mode(mode));
        assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
        let (vbe_period, edges) = vretrace_period_ticks(&mut machine, SAMPLE_TICKS, WINDOW_TICKS);
        assert!(
            edges >= 14,
            "0.25 s at 60 Hz owes about 15 edges, saw {edges} in mode {mode:#06x}"
        );
        assert!(
            vbe_period.abs_diff(margo_period) <= margo_period / 500,
            "0x3DA in VBE mode {mode:#06x} must report Margo's frame rate: measured \
         period {vbe_period} ticks against Margo's {margo_period}"
        );
    }
}

/// The other half of the same contract: 0x3DA's retrace and the frame boundary
/// that latches `DISP_START` are ONE clock, in the right order.
///
/// A guest queues a page flip during vertical blanking and expects it to take
/// effect at the start of the next frame -- so from the instant 0x3DA first
/// reports retrace, the latch must land STRICTLY INSIDE the remaining blanking,
/// never a whole frame later and never before the retrace it was queued in.
/// Nothing enforced that before this slice, because the retrace edge came off
/// the VGA CRTC and the latch off a 60 Hz phase that had no fixed relationship
/// to it.
#[test]
fn margo_display_start_latches_inside_the_blanking_the_guest_polled_for() {
    const SAMPLE_TICKS: u64 = 20_000;
    let frame_ticks = izarravm_core::MASTER_CLOCK_HZ / 60;

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.vega.set_vbe_mode(0x0101));

    // Advance to the first rising edge of the retrace bit, exactly as a guest
    // pacing on 0x3DA would.
    let mut previous = machine.read_io_port_u8(0x3da) & 0x08 != 0;
    let mut waited = 0u64;
    loop {
        machine.advance_devices_ticks(SAMPLE_TICKS);
        waited += SAMPLE_TICKS;
        let now = machine.read_io_port_u8(0x3da) & 0x08 != 0;
        if now && !previous {
            break;
        }
        previous = now;
        assert!(
            waited < 2 * frame_ticks,
            "no retrace edge within two frames"
        );
    }

    // Queue a flip from inside that blanking interval.
    assert!(machine.vega.program_display_start(640 * 480));
    assert!(machine.margo().display_start_pending());

    let mut to_latch = 0u64;
    while machine.margo().display_start_pending() {
        machine.advance_devices_ticks(SAMPLE_TICKS);
        to_latch += SAMPLE_TICKS;
        assert!(
            to_latch <= frame_ticks,
            "a flip queued at the retrace edge must latch within the same frame, \
             not a whole frame later"
        );
    }
    assert_eq!(machine.margo().display().start, 640 * 480);

    // 640x480 blanks for 45 of its 525 lines and starts retrace 10 lines in, so
    // 35 lines of blanking remain after the edge: 6.67% of a frame. The sampling
    // grain and the up-to-one-sample lateness of the observed edge bound the
    // error, so this asserts the ORDER OF MAGNITUDE of the gap, which is what
    // distinguishes "same clock" from "unrelated clocks that happen to agree on
    // rate".
    let expected = frame_ticks * 35 / 525;
    assert!(
        to_latch.abs_diff(expected) <= 4 * SAMPLE_TICKS,
        "the latch must fall {expected} ticks after the retrace edge (the remaining \
         blanking), measured {to_latch}"
    );
}

/// The JIT's poll-skip path reads `Vega::status1_bits` off a predicted beam and
/// skips guest iterations right up to the edge it computes from
/// `dots_until_status1_bit_change_from`. If that path and the port a guest
/// actually reads ever disagreed about which clock 0x3DA runs on, the JIT would
/// sleep through the retrace the guest was waiting for. Both arms are asked at
/// the SAME beam, so this pins the agreement itself rather than the peek offset.
#[cfg(feature = "jit")]
#[test]
fn margo_poll_skip_status_bits_agree_with_the_port_read() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.set_vga_mode(0x13));
    assert!(machine.vega.set_vbe_mode(0x0101));

    let mut saw_retrace = false;
    let mut saw_active = false;
    for _ in 0..4_000 {
        machine.advance_devices_ticks(50_000);
        let beam = machine.scanout_beam_dots();
        let poll_skip = machine.vega.status1_bits(beam);
        let port = machine
            .vega
            .read_status_port_lazy(0x3da, beam)
            .expect("0x3DA is the active status1 alias in a color setup");
        assert_eq!(
            poll_skip, port,
            "the poll-skip status bits and the port read must agree at beam {beam}"
        );
        saw_retrace |= port & 0x08 != 0;
        saw_active |= port & 0x01 == 0;
    }
    assert!(
        saw_retrace && saw_active,
        "the sweep must observe both retrace and active display, or it pins nothing"
    );
}

/// The wall-pacing path (`advance_wall_shortfall`) stops the machine at each
/// vertical-retrace start edge so a guest polling for it cannot miss a window
/// that opens and closes inside one pacing top-up. That edge has to be Margo's
/// while Margo is scanning out, or the pacing lands on the legacy raster's edges
/// and the guest misses every one of Margo's.
#[test]
fn margo_owns_the_vretrace_edge_the_wall_pacing_stops_at() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.set_vga_mode(0x13));
    assert!(machine.vega.set_vbe_mode(0x0105));

    let beam = machine.scanout_beam_dots();
    let scan = machine
        .vega
        .margo_scanout()
        .expect("a VBE mode must be active");
    assert_eq!(
        machine.vega.dots_until_vretrace_start(beam),
        scan.dots_until_vretrace_start(beam)
    );
    // Non-vacuous: 1024x768 and mode 13h do not share an edge distance, so the
    // equality above is a routing claim and not a coincidence.
    assert_ne!(
        machine.vega.dots_until_vretrace_start(beam),
        machine.vega.legacy().dots_until_vretrace_start()
    );
}

/// GUARANTEE 1 of the section 9 contract, exact-edge form: a queued `DISP_START`
/// is applied AT the frame boundary and at no earlier instant.
///
/// The existing `vbe_display_start_latches_on_the_next_margo_frame` shows the
/// flip is deferred and lands within a frame; it does not show WHERE. This walks
/// the machine to one master tick short of the boundary and requires the flip to
/// still be queued there, which is the only form that distinguishes "latched at
/// the boundary" from "latched somewhere in the neighbourhood".
#[test]
fn margo_display_start_is_applied_at_the_frame_boundary_and_not_one_tick_before() {
    let frame_ticks = izarravm_core::MASTER_CLOCK_HZ / 60;
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.vega.set_vbe_mode(0x0101));

    // Land somewhere mid-frame, so the boundary this measures is a real edge and
    // not the phase the machine happens to boot on.
    machine.advance_devices_ticks(frame_ticks / 3);
    assert!(machine.vega.program_display_start(640 * 480));
    assert!(machine.margo().display_start_pending());

    let to_edge = machine
        .timeline
        .master_ticks_until(
            crate::timeline::DeviceClock::MargoFrame,
            1,
            crate::timeline::MARGO_FRAME_HZ,
        )
        .expect("the frame clock is running");
    assert!(
        to_edge > 1 && to_edge <= frame_ticks,
        "the next boundary must be inside one frame and not already due, got {to_edge}"
    );

    machine.advance_devices_ticks(to_edge - 1);
    assert!(
        machine.margo().display_start_pending(),
        "one master tick short of the frame boundary the flip must still be QUEUED"
    );
    assert_eq!(
        machine.margo().display().start,
        0,
        "and the scanned-out origin must not have moved yet"
    );

    machine.advance_devices_ticks(1);
    assert!(!machine.margo().display_start_pending());
    assert_eq!(machine.margo().display().start, 640 * 480);
}

/// The same guarantee for `4F07h BL=80h`, which additionally STALLS the caller to
/// that boundary. The stall must be exactly the distance to the next frame edge:
/// shorter and the caller returns before its flip is live, longer and the BIOS
/// is burning guest time it did not owe.
#[test]
fn margo_display_start_wait_stalls_exactly_to_the_frame_boundary() {
    let frame_ticks = izarravm_core::MASTER_CLOCK_HZ / 60;
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.vega.set_vbe_mode(0x0101));
    machine.advance_devices_ticks(frame_ticks / 3);
    assert!(machine.vega.program_display_start(640 * 480));

    let to_edge = machine
        .timeline
        .master_ticks_until(
            crate::timeline::DeviceClock::MargoFrame,
            1,
            crate::timeline::MARGO_FRAME_HZ,
        )
        .expect("the frame clock is running");
    let stalled_before = machine.io_stall_ticks();
    machine.stall_until_margo_frame();

    assert_eq!(
        machine.io_stall_ticks() - stalled_before,
        to_edge,
        "the retrace wait must stall exactly to the next frame boundary"
    );
    assert!(to_edge < frame_ticks, "and never a whole frame");
    assert!(
        !machine.margo().display_start_pending(),
        "the stall must carry the machine THROUGH the boundary, so the flip is \
         live when the caller returns"
    );
    assert_eq!(machine.margo().display().start, 640 * 480);
}

/// The batch cap owes the same edge. `Machine::vega_edge_ticks` arms a deadline
/// on a pending `DISP_START` so no batch can run past the boundary that applies
/// it -- otherwise a guest that flips and then polls would see the origin move
/// up to a whole batch cap late, which on the coarse 1 ms cap is a sixteenth of
/// a frame.
#[test]
fn margo_pending_display_start_caps_the_batch_at_the_frame_boundary() {
    let frame_ticks = izarravm_core::MASTER_CLOCK_HZ / 60;
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.vega.set_vbe_mode(0x0101));

    // Walk up to within ~17 us of the boundary. The term only BINDS when the
    // frame edge is nearer than the other caps: a whole 16.67 ms frame is far
    // longer than the ~1 ms coarse cap, so a mid-frame test would assert
    // nothing about this deadline and pass no matter what it did.
    let to_edge = machine
        .timeline
        .master_ticks_until(
            crate::timeline::DeviceClock::MargoFrame,
            1,
            crate::timeline::MARGO_FRAME_HZ,
        )
        .expect("the frame clock is running");
    machine.advance_devices_ticks(to_edge - frame_ticks / 1_000);

    // Nothing queued: the cap comes from some other, longer term.
    let idle_cap = machine.event_batch_cap(u64::MAX);
    assert!(machine.vega.program_display_start(640 * 480));
    let edge_clocks = machine
        .timeline
        .cpu_clocks_until(
            crate::timeline::DeviceClock::MargoFrame,
            1,
            crate::timeline::MARGO_FRAME_HZ,
        )
        .expect("the frame clock is running");
    assert!(
        edge_clocks < idle_cap,
        "the frame edge must be shorter than the idle cap for this to be \
         distinguishable (edge {edge_clocks}, idle {idle_cap})"
    );
    assert_eq!(
        machine.event_batch_cap(u64::MAX),
        edge_clocks,
        "a queued flip must cap the batch at the frame boundary"
    );
}

/// GUARANTEE 2: the palette is sampled ONCE per scanout, so a frame is decoded
/// entirely under one DAC state -- no tearing between the top and bottom of the
/// screen, and no stale palette carried across frames.
///
/// BE PRECISE ABOUT WHAT THIS CAN AND CANNOT SHOW. The "once per scanout" half
/// is enforced by the TYPE, not by this test: `Margo::scanout_argb(&palette)`
/// receives one immutable snapshot the caller sampled before the call, so there
/// is no seam through which a second DAC state could enter, and nothing a test
/// can do from outside will mutate the palette part-way down the surface.
///
/// What the assertions actually pin is the pair of properties that WOULD break
/// if that structure were dismantled: every pixel of a capture decodes through
/// the correct entry for its own index -- so a scanout that re-mapped part of
/// the surface through anything else is caught -- and the next capture picks up
/// a palette change, so a cached or stale snapshot is caught. Mutations N5 and
/// N6 are the two shapes, and both fail here.
#[test]
fn margo_scanout_decodes_a_whole_frame_under_one_palette_snapshot() {
    const RED: u32 = 0x00ff_0000;
    const GREEN: u32 = 0x0000_ff00;
    const BLUE: u32 = 0x0000_00ff;

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.vega.set_vbe_mode(0x0101));

    // Index 0 on the top half of the surface, index 1 on the bottom half, so a
    // scanout that changed palettes part-way down would split along the same
    // axis and be caught by the per-half colour sets below.
    for y in 0..480usize {
        let index = u8::from(y >= 240);
        for x in 0..640usize {
            machine.vega.margo_mut().write_vram_u8(y * 640 + x, index);
        }
    }
    // 6-bit DAC components; 0x3F bit-replicates to 0xFF in the ARGB the scanout emits.
    machine.video_mut().set_dac_entry(0, 0x3f, 0x00, 0x00);
    machine.video_mut().set_dac_entry(1, 0x00, 0x3f, 0x00);

    fn halves(machine: &mut Machine) -> (Vec<u32>, Vec<u32>) {
        let (words, width, height) = machine.frame_argb();
        assert_eq!((width, height), (640, 480));
        let mut top: Vec<u32> = words[..width * 240].to_vec();
        let mut bottom: Vec<u32> = words[width * 240..].to_vec();
        top.sort_unstable();
        top.dedup();
        bottom.sort_unstable();
        bottom.dedup();
        (top, bottom)
    }

    assert_eq!(
        halves(&mut machine),
        (vec![RED], vec![GREEN]),
        "each half must be a single colour: one palette snapshot, no tearing"
    );

    machine.video_mut().set_dac_entry(0, 0x00, 0x00, 0x3f);
    assert_eq!(
        halves(&mut machine),
        (vec![BLUE], vec![GREEN]),
        "the next scanout must sample the CURRENT palette, not a cached one"
    );
}

/// Pins the recon instrument behind `IZARRAVM_VBE_TRACE`: the counters must
/// separate a LINEAR 4F02 request (bit 0x4000) from a banked one, and must
/// count only ACCEPTED mode sets. A rejected mode leaves both alone -- otherwise
/// a guest that probes the mode list would inflate whichever column it probed
/// with, and the answer this instrument exists to give ("does GP2 run the LFB
/// linearly or banked") would be read off noise.
#[test]
fn vbe_mode_set_window_counters_separate_linear_from_banked() {
    let rom = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x41, // mov bx, 0101h | 4000h (linear)
        0xcd, 0x10, // int 10h
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x03, 0x01, // mov bx, 0103h (banked)
        0xcd, 0x10, // int 10h
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0xff, 0x41, // mov bx, 01FFh | 4000h -- not in the table, rejected
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.cpu().registers.eax() as u16,
        0x014f,
        "the third mode set must have been REJECTED for this test to be about \
         accepted-only counting"
    );
    assert_eq!(machine.vega.vbe_mode_set_window_counts(), (1, 1));
}

#[test]
fn vbe_banked_window_set_get_and_boundary_round_trip_in_guest() {
    let rom = rom_with_code(&[
        0x31, 0xc0, // xor ax, ax
        0x8e, 0xd8, // mov ds, ax
        0xb8, 0x13, 0x00, // mov ax, 0013h
        0xcd, 0x10, // int 10h
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x01, // mov bx, 0101h (banked)
        0xcd, 0x10, // int 10h
        0xb8, 0x00, 0xa0, // mov ax, A000h
        0x8e, 0xc0, // mov es, ax
        0xb8, 0x05, 0x4f, // mov ax, 4F05h
        0x31, 0xdb, // xor bx, bx (set window A)
        0x31, 0xd2, // xor dx, dx (bank 0)
        0xcd, 0x10, // int 10h
        0x26, 0xc6, 0x06, 0xff, 0xff, 0x11, // mov byte [es:FFFFh], 11h
        0xb8, 0x05, 0x4f, // mov ax, 4F05h
        0x31, 0xdb, // xor bx, bx
        0xba, 0x01, 0x00, // mov dx, 1
        0xcd, 0x10, // int 10h
        0x26, 0xc6, 0x06, 0x00, 0x00, 0x22, // mov byte [es:0000h], 22h
        0x26, 0xa0, 0x00, 0x00, // mov al, [es:0000h]
        0xa2, 0x00, 0x05, // mov [0500h], al
        0xb8, 0x05, 0x4f, // mov ax, 4F05h
        0xbb, 0x00, 0x01, // mov bx, 0100h (get window A)
        0xcd, 0x10, // int 10h
        0x89, 0x16, 0x02, 0x05, // mov [0502h], dx
        0xb8, 0x05, 0x4f, // mov ax, 4F05h
        0x31, 0xdb, // xor bx, bx
        0x31, 0xd2, // xor dx, dx
        0xcd, 0x10, // int 10h
        0x26, 0xa0, 0xff, 0xff, // mov al, [es:FFFFh]
        0xa2, 0x01, 0x05, // mov [0501h], al
        0xb8, 0x03, 0x4f, // mov ax, 4F03h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.read_physical_u8(0x0500), 0x22);
    assert_eq!(machine.read_physical_u8(0x0501), 0x11);
    assert_eq!(read_u16(&mut machine, 0x0502), 1);
    assert_eq!(machine.margo().read_vram_u8(0xffff), 0x11);
    assert_eq!(machine.margo().read_vram_u8(0x1_0000), 0x22);
    assert_eq!(machine.video().cpu_read_chain4(0), 0);
    assert_eq!(machine.video().cpu_read_chain4(0xffff), 0);
    assert_eq!(machine.cpu().registers.ebx() as u16, 0x0101);
}

#[test]
fn vbe_banked_window_outside_vram_reads_open_bus() {
    let rom = rom_with_code(&[
        0x31, 0xc0, // xor ax, ax
        0x8e, 0xd8, // mov ds, ax
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x01, // mov bx, 0101h (banked)
        0xcd, 0x10, // int 10h
        0xb8, 0x00, 0xa0, // mov ax, A000h
        0x8e, 0xc0, // mov es, ax
        0xb8, 0x05, 0x4f, // mov ax, 4F05h
        0x31, 0xdb, // xor bx, bx
        0xba, 0x40, 0x00, // mov dx, 64 (one bank past 4 MiB)
        0xcd, 0x10, // int 10h
        0x26, 0xc6, 0x06, 0x00, 0x00, 0x66, // mov byte [es:0000h], 66h
        0x26, 0xa0, 0x00, 0x00, // mov al, [es:0000h]
        0xa2, 0x00, 0x05, // mov [0500h], al
        0xb8, 0x05, 0x4f, // mov ax, 4F05h
        0xbb, 0x00, 0x01, // mov bx, 0100h (get window A)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
    assert_eq!(machine.cpu().registers.edx() as u16, 64);
    assert_eq!(machine.read_physical_u8(0x0500), 0xff);
    assert_eq!(machine.margo().read_vram_u8(0), 0);
}

#[test]
fn vbe_linear_mode_keeps_a000_on_vga_and_the_lfb_on_margo() {
    let rom = rom_with_code(&[
        0xb8, 0x13, 0x00, // mov ax, 0013h
        0xcd, 0x10, // int 10h
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x41, // mov bx, 0101h | 4000h (linear)
        0xcd, 0x10, // int 10h
        0xb8, 0x00, 0xa0, // mov ax, A000h
        0x8e, 0xc0, // mov es, ax
        0x26, 0xc6, 0x06, 0x00, 0x00, 0x77, // mov byte [es:0000h], 77h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.video().cpu_read_chain4(0), 0x77);
    assert_eq!(machine.margo().read_vram_u8(0), 0);
    machine.write_physical_u8(MARGO_LFB_BASE, 0x88);
    assert_eq!(machine.margo().read_vram_u8(0), 0x88);
}

#[test]
fn vbe_display_start_latches_on_the_next_margo_frame() {
    let rom = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x41, // mov bx, 0101h | 4000h
        0xcd, 0x10, // int 10h
        0xb8, 0x07, 0x4f, // mov ax, 4F07h
        0xbb, 0x00, 0x00, // mov bx, 0000h (set without waiting)
        0xb9, 0x00, 0x00, // mov cx, 0
        0xba, 0xe0, 0x01, // mov dx, 480 (second page)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
    assert_eq!(read_mmio_reg(&mut machine, 0x24), 640 * 480);
    assert_eq!(machine.margo().display().start, 0);
    assert!(machine.margo().display_start_pending());

    machine.advance_devices_ticks(izarravm_core::MASTER_CLOCK_HZ / 60);
    assert_eq!(machine.margo().display().start, 640 * 480);
    assert!(!machine.margo().display_start_pending());
}

#[test]
fn vbe_display_start_retrace_wait_returns_the_active_coordinates() {
    let rom = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x41, // mov bx, 0101h | 4000h
        0xcd, 0x10, // int 10h
        0xb8, 0x07, 0x4f, // mov ax, 4F07h
        0xbb, 0x80, 0x00, // mov bx, 0080h (set during vertical retrace)
        0xb9, 0x00, 0x00, // mov cx, 0
        0xba, 0xe0, 0x01, // mov dx, 480
        0xcd, 0x10, // int 10h
        0xb8, 0x07, 0x4f, // mov ax, 4F07h
        0xbb, 0x01, 0x00, // mov bx, 0001h (get active start)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
    assert_eq!(machine.cpu().registers.ebx() as u16, 0x0001);
    assert_eq!(machine.cpu().registers.ecx() as u16, 0);
    assert_eq!(machine.cpu().registers.edx() as u16, 480);
    assert_eq!(machine.margo().display().start, 640 * 480);
    assert!(machine.io_stall_ticks() > 0);
}

#[test]
fn vbe_eight_bit_palette_round_trips_at_vertical_retrace() {
    let rom = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x41, // mov bx, 0101h | 4000h (8bpp LFB)
        0xcd, 0x10, // int 10h
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbf, 0x00, 0x00, // mov di, 0
        0xb8, 0x08, 0x4f, // mov ax, 4F08h
        0xbb, 0x00, 0x08, // mov bx, 0800h (set 8-bit DAC)
        0xcd, 0x10, // int 10h
        0xb8, 0x09, 0x4f, // mov ax, 4F09h
        0xbb, 0x80, 0x00, // mov bx, 0080h (set during vertical retrace)
        0xb9, 0x02, 0x00, // mov cx, 2 entries
        0xba, 0x0a, 0x00, // mov dx, first index 10
        0xcd, 0x10, // int 10h
        0xbf, 0x20, 0x00, // mov di, 0020h
        0xb8, 0x09, 0x4f, // mov ax, 4F09h
        0xbb, 0x01, 0x00, // mov bx, 0001h (get palette)
        0xcd, 0x10, // int 10h
        0xb8, 0x08, 0x4f, // mov ax, 4F08h
        0xbb, 0x01, 0x00, // mov bx, 0001h (get DAC width)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    let input = [0x56, 0x34, 0x12, 0, 0xef, 0xcd, 0xab, 0]; // B,G,R,alignment
    for (offset, value) in input.into_iter().enumerate() {
        machine.write_physical_u8(0x40000 + offset as u32, value);
    }

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
    assert_eq!(machine.cpu().registers.ebx() as u16, 0x0801);
    assert_eq!(machine.video().dac_component_bits(), 8);
    assert_eq!(machine.video().dac_entry(10), [0x12, 0x34, 0x56]);
    assert_eq!(machine.video().dac_entry(11), [0xab, 0xcd, 0xef]);
    assert!(machine.io_stall_ticks() > 0);
    for (offset, expected) in input.into_iter().enumerate() {
        assert_eq!(machine.read_physical_u8(0x40020 + offset as u32), expected);
    }
}

#[test]
fn vbe_dac_format_rejects_direct_color_modes() {
    let rom = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x11, 0x41, // mov bx, 0111h | 4000h (16bpp LFB)
        0xcd, 0x10, // int 10h
        0xb8, 0x08, 0x4f, // mov ax, 4F08h
        0xbb, 0x00, 0x08, // mov bx, 0800h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.cpu().registers.eax() as u16, 0x034f);
    assert_eq!(machine.video().dac_component_bits(), 6);
}

#[test]
fn vbe_mode_info_fills_the_block() {
    // ES = 0x4000 -> physical 0x40000, DI = 0.
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbf, 0x00, 0x00, // mov di, 0
        0xb8, 0x01, 0x4f, // mov ax, 4F01h
        0xb9, 0x01, 0x01, // mov cx, 0101h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);

    let base = 0x40000;
    assert_eq!(machine.read_physical_u8(base + 0x02), 0x07); // WinA: present, read/write
    assert_eq!(machine.read_physical_u8(base + 0x03), 0); // WinB absent
    assert_eq!(read_u16(&mut machine, base + 0x04), 64); // WinGranularity, KiB
    assert_eq!(read_u16(&mut machine, base + 0x06), 64); // WinSize, KiB
    assert_eq!(read_u16(&mut machine, base + 0x08), 0xa000); // WinASegment
    assert_eq!(read_u16(&mut machine, base + 0x0a), 0); // WinBSegment
    assert_eq!(read_u32(&mut machine, base + 0x0c), 0); // no direct ROM bank thunk
    assert_eq!(read_u16(&mut machine, base + 0x10), 640); // BytesPerScanLine
    assert_eq!(read_u16(&mut machine, base + 0x12), 640); // XResolution
    assert_eq!(read_u16(&mut machine, base + 0x14), 480); // YResolution
    assert_eq!(machine.read_physical_u8(base + 0x19), 8); // BitsPerPixel
    assert_eq!(read_u32(&mut machine, base + 0x28), MARGO_LFB_BASE); // PhysBasePtr
}

#[test]
fn tomb_shaped_vbe_granularity_divisor_is_nonzero() {
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbf, 0x00, 0x00, // mov di, 0
        0xb8, 0x01, 0x4f, // mov ax, 4F01h
        0xb9, 0x01, 0x01, // mov cx, 0101h
        0xcd, 0x10, // int 10h
        0x26, 0x8b, 0x5d, 0x04, // mov bx, [es:di+WinGranularity]
        0xb8, 0x00, 0x01, // mov ax, 256
        0x31, 0xd2, // xor dx, dx
        0xf7, 0xf3, // div bx
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.cpu().registers.ebx() as u16, 64);
    assert_eq!(machine.cpu().registers.eax() as u16, 4);
    assert_eq!(machine.cpu().registers.edx() as u16, 0);
}

#[test]
fn vbe_controller_info_fills_the_block() {
    let rom = izbios_rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbf, 0x00, 0x00, // mov di, 0
        0xb8, 0x00, 0x4f, // mov ax, 4F00h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);

    let base = 0x40000;
    assert_eq!(machine.read_physical_u8(base), b'V');
    assert_eq!(machine.read_physical_u8(base + 1), b'E');
    assert_eq!(machine.read_physical_u8(base + 2), b'S');
    assert_eq!(machine.read_physical_u8(base + 3), b'A');
    assert_eq!(read_u16(&mut machine, base + 0x04), 0x0200); // VbeVersion
    assert_eq!(read_u16(&mut machine, base + 0x12), 64); // TotalMemory (64 KB units)
    // Capabilities bit 0 advertises 6/8-bit DAC switching.
    assert_eq!(read_u32(&mut machine, base + 0x0a), 1); // Capabilities

    // OemStringPtr points into the ROM at a real NUL-terminated string.
    let oem = read_u32(&mut machine, base + 0x06);
    assert_eq!(oem >> 16, u32::from(izarravm_firmware::IZARRA_BIOS_SEG));
    let oem_linear = ((oem >> 16) << 4) + (oem & 0xffff);
    let text: Vec<u8> = (0..26)
        .map(|i| machine.read_physical_u8(oem_linear + i))
        .collect();
    assert_eq!(&text, b"Izarra 3000 VEGA/Margo VBE");
    assert_eq!(machine.read_physical_u8(oem_linear + 26), 0);

    // The three VBE 2.0 OEM pointers at 0x16/0x1A/0x1E stay null. They are only
    // interesting because the mode list used to start at 0x14 and fill them with
    // mode numbers, which a VBE2 client would have followed as far pointers.
    assert_eq!(read_u32(&mut machine, base + 0x16), 0); // OemVendorNamePtr
    assert_eq!(read_u32(&mut machine, base + 0x1a), 0); // OemProductNamePtr
    assert_eq!(read_u32(&mut machine, base + 0x1e), 0); // OemProductRevPtr

    // VideoModePtr (seg:off) must point at the mode list. Modes are listed in
    // ascending numeric order, VESA-defined first and the OEM 320x240 mode
    // (0x150) last, as a real VBE BIOS does -- then the 0xffff terminator.
    let ptr = read_u32(&mut machine, base + 0x0e);
    let list = (((ptr >> 16) & 0xffff) << 4) + (ptr & 0xffff);
    let expected = [
        0x0100, 0x0101, 0x0103, 0x0105, 0x0110, 0x0111, 0x0113, 0x0114, 0x0116, 0x0117, 0x014a,
        0x014c, 0x014e, 0x0150, 0xffff,
    ];
    for (i, &mode) in expected.iter().enumerate() {
        assert_eq!(read_u16(&mut machine, list + (i * 2) as u32), mode);
    }
}

/// A real VBE BIOS enumerates modes in ascending numeric order. Pinning the
/// property (not just the current list) means a mode added out of order fails
/// here rather than silently shifting every later guest-visible index.
#[test]
fn vbe_mode_list_is_strictly_ascending() {
    let numbers: Vec<u16> = izarravm_video::MARGO_VBE_MODES
        .iter()
        .map(|mode| mode.number)
        .collect();
    assert!(
        numbers.windows(2).all(|pair| pair[0] < pair[1]),
        "MARGO_VBE_MODES must be sorted ascending, got {numbers:04x?}"
    );
}

#[test]
fn vbe_mode_info_rejects_unknown_modes() {
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbf, 0x00, 0x00, // mov di, 0
        0xb8, 0x01, 0x4f, // mov ax, 4F01h
        0xb9, 0x12, 0x01, // mov cx, 0112h (640x480x24, packed 24-bit not provided)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, 0x014f);
}

#[test]
pub(super) fn copy_through_the_mmio_aperture_moves_vram_and_times_busy() {
    let mut machine = test_machine();
    // Seed a 2x2 source rectangle at (0, 0), pitch 640, depth 1, through the LFB.
    machine.write_physical_u8(MARGO_LFB_BASE, 0xa1); // (0,0)
    machine.write_physical_u8(MARGO_LFB_BASE + 1, 0xa2); // (1,0)
    machine.write_physical_u8(MARGO_LFB_BASE + 640, 0xa3); // (0,1)
    machine.write_physical_u8(MARGO_LFB_BASE + 641, 0xa4); // (1,1)

    // Copy it to (10, 10) on the same surface (no overlap).
    write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
    write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
    write_mmio_reg(&mut machine, 0x108, 0); // SRC_BASE
    write_mmio_reg(&mut machine, 0x10c, 640); // SRC_PITCH
    write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
    write_mmio_reg(&mut machine, 0x114, (10 << 16) | 10); // DST_XY: y=10, x=10
    write_mmio_reg(&mut machine, 0x118, 0); // SRC_XY: (0,0)
    write_mmio_reg(&mut machine, 0x11c, (2 << 16) | 2); // DIM: h=2, w=2
    write_mmio_reg(&mut machine, 0x128, 0xcc); // ROP: SRCCOPY
    write_mmio_reg(&mut machine, 0x130, 0); // FLAGS: none
    write_mmio_reg(&mut machine, 0x150, 0x02); // COMMAND: COPY

    // Destination corners hold the source bytes (read back through the LFB).
    assert_eq!(
        machine.read_physical_u8(MARGO_LFB_BASE + 10 * 640 + 10),
        0xa1
    );
    assert_eq!(
        machine.read_physical_u8(MARGO_LFB_BASE + 11 * 640 + 11),
        0xa4
    );
    // BUSY is set right after the command.
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);

    // 4 pixels -> busy_ns = 100 + 4*10 = 140 ns. At 22 MHz (45.4545 ns/clock),
    // three clocks (136 ns drained) leave it busy; the fourth clears it.
    machine.advance_devices(3);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    machine.advance_devices(1);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
}

#[test]
fn dos_com_prints_string_and_exits() {
    // org 0x100: mov ah,9; mov dx,0x010c; int 21; mov ax,4c00; int 21; db 'Hi$'
    let com: &[u8] = &[
        0xb4, 0x09, 0xba, 0x0c, 0x01, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21, b'H', b'i', b'$',
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), com).unwrap();
    let reason = machine.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    assert_eq!(machine.program_output(), b"Hi");
}

#[test]
fn dos_com_exit_code_is_carried_through() {
    // org 0x100: mov ax,4c07; int 21
    let com: &[u8] = &[0xb8, 0x07, 0x4c, 0xcd, 0x21];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), com).unwrap();
    let reason = machine.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 7 });
    assert!(machine.program_output().is_empty());
}

#[test]
pub(super) fn fill_through_the_mmio_aperture_writes_vram_and_times_busy() {
    let mut machine = test_machine();
    // Latch a 5x4 fill at (3, 2), pitch 640, depth 1, color 0xAB, solid.
    write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
    write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
    write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
    write_mmio_reg(&mut machine, 0x114, (2 << 16) | 3); // DST_XY: y=2, x=3
    write_mmio_reg(&mut machine, 0x11c, (4 << 16) | 5); // DIM: h=4, w=5
    write_mmio_reg(&mut machine, 0x120, 0xab); // FG_COLOR
    write_mmio_reg(&mut machine, 0x128, 0xf0); // ROP: PATCOPY
    write_mmio_reg(&mut machine, 0x150, 0x01); // COMMAND: FILL

    // VRAM filled (read the top-left filled pixel back through the LFB).
    let pixel = MARGO_LFB_BASE + 2 * 640 + 3;
    assert_eq!(machine.read_physical_u8(pixel), 0xab);
    // BUSY is set right after the command.
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);

    // 20 pixels -> busy_ns = 100 + 20*5 = 200 ns. At 22 MHz (45.4545 ns/clock),
    // four clocks (181 ns drained) leave it busy; the fifth clears it.
    machine.advance_devices(4);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    machine.advance_devices(1);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
}

#[test]
fn dos_com_runs_the_committed_hello_fixture() {
    let mut machine = Machine::new_raw_program(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::HELLO_COM,
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    assert_eq!(machine.program_output(), b"Hello, world!\r\n");
}

#[test]
fn dos_exe_runs_with_relocation_applied() {
    // The committed .EXE loads DS from a relocated segment reference, then
    // prints via AH=09h. Correct output is only possible if load_exe applied
    // the relocation (otherwise DS is the link-time base and the bytes
    // diverge), so this doubles as the end-to-end relocation check.
    let mut machine = Machine::new_raw_program(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::EXEHELLO_EXE,
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    assert_eq!(
        machine.program_output(),
        b"Hello from a relocated .EXE!\r\n"
    );
}

#[test]
fn dos_com_ah06_zf_reaches_the_guest() {
    // org 0x100: AH=06h DL=0xFF; INT 21h; JZ empty; echo AL via AH=02h; else '!'
    // Proves ZF returned by AH=06h survives the IRET (it is written to the pushed
    // FLAGS image, not just live eflags which the IRET would discard).
    let com: &[u8] = &[
        0xb4, 0x06, 0xb2, 0xff, 0xcd, 0x21, 0x74, 0x08, 0x88, 0xc2, 0xb4, 0x02, 0xcd, 0x21, 0xeb,
        0x06, 0xb2, 0x21, 0xb4, 0x02, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21,
    ];

    let mut available =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), com).unwrap();
    available.set_program_stdin(b"X");
    assert_eq!(
        available.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );
    assert_eq!(available.program_output(), b"X"); // char path taken, AL echoed

    let mut empty =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), com).unwrap();
    assert_eq!(
        empty.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );
    assert_eq!(empty.program_output(), b"!"); // empty path taken (ZF=1)
}

#[test]
fn dos_com_echoes_input() {
    // org 0x100: AH=01h; INT 21h (x2, each echoes); AH=4Ch exit
    let com: &[u8] = &[
        0xb4, 0x01, 0xcd, 0x21, 0xb4, 0x01, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21,
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), com).unwrap();
    machine.set_program_stdin(b"hi");
    assert_eq!(
        machine.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );
    assert_eq!(machine.program_output(), b"hi");
}

#[test]
pub(super) fn color_expand_data_through_the_mmio_aperture_draws_a_glyph_and_times_busy() {
    let mut machine = test_machine();
    // draw_glyph_8x8: an 8x8 glyph expanded at (10, 5), pitch 640, depth 1,
    // FG 0xAB, EXPAND_TRANSPARENT so clear bits leave the zeroed background.
    // Row 0 = 0x80 (only the leftmost pixel), row 1 = 0x01 (only the rightmost),
    // proving MSB-first ordering; the rest are blank.
    let glyph: [u8; 8] = [0x80, 0x01, 0, 0, 0, 0, 0, 0];

    write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
    write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
    write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
    write_mmio_reg(&mut machine, 0x114, (5 << 16) | 10); // DST_XY: y=5, x=10
    write_mmio_reg(&mut machine, 0x11c, (8 << 16) | 8); // DIM: 8x8
    write_mmio_reg(&mut machine, 0x120, 0xab); // FG_COLOR
    write_mmio_reg(&mut machine, 0x130, 0x04); // FLAGS: EXPAND_TRANSPARENT
    write_mmio_reg(&mut machine, 0x128, 0xcc); // ROP: SRCCOPY (S = expanded pixel)
    write_mmio_reg(&mut machine, 0x150, 0x03); // COMMAND: COLOR_EXPAND_DATA

    // Armed: BUSY set before any data, nothing drawn yet.
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    assert_eq!(
        machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + 10),
        0x00
    );

    // Stream the eight rows; the bits go in the high byte, MSB first.
    for (row, &bits) in glyph.iter().enumerate() {
        write_mmio_reg(&mut machine, 0x160, u32::from(bits) << 24); // MONO_DATA
        if row < 7 {
            // Still armed until the final word arrives.
            assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
        }
    }

    // Set bits painted FG; clear bits left untouched over the zeroed background.
    assert_eq!(
        machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + 10),
        0xab
    ); // row 0, col 0
    assert_eq!(
        machine.read_physical_u8(MARGO_LFB_BASE + 6 * 640 + 17),
        0xab
    ); // row 1, col 7
    assert_eq!(
        machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + 11),
        0x00
    ); // row 0, col 1 clear
    assert_eq!(
        machine.read_physical_u8(MARGO_LFB_BASE + 6 * 640 + 10),
        0x00
    ); // row 1, col 0 clear

    // 2 pixels written -> busy_ns = 100 + 2*5 = 110 ns. At 22 MHz (45.4545 ns/clock),
    // two clocks (90 ns drained) leave it busy; the third clears it.
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    machine.advance_devices(2);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    machine.advance_devices(1);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
}

#[test]
pub(super) fn line_through_the_mmio_aperture_draws_and_times_busy() {
    let mut machine = test_machine();
    // draw_line: a horizontal 5-pixel line at y=5 from x=10 to x=14, pitch 640,
    // depth 1, FG 0xAB. ROP 0xF0 (PATCOPY) draws solid; LINE has no source, so
    // the pattern (FG) is the right input, not SRCCOPY.
    write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
    write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
    write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
    write_mmio_reg(&mut machine, 0x13c, (5 << 16) | 10); // LINE_START: (10,5)
    write_mmio_reg(&mut machine, 0x140, (5 << 16) | 14); // LINE_END: (14,5)
    write_mmio_reg(&mut machine, 0x120, 0xab); // FG_COLOR
    write_mmio_reg(&mut machine, 0x128, 0xf0); // ROP: PATCOPY (solid; LINE has no source)
    write_mmio_reg(&mut machine, 0x150, 0x05); // COMMAND: LINE

    // The five pixels (x=10..14, y=5) are set; the pixel just left is not.
    for x in 10u32..=14 {
        assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + x), 0xab);
    }
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + 9), 0x00);
    // BUSY set right after the command.
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);

    // 5 pixels -> busy_ns = 100 + 5*10 = 150 ns. At 22 MHz (45.4545 ns/clock),
    // three clocks (136 ns drained) leave it busy; the fourth clears it.
    machine.advance_devices(3);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    machine.advance_devices(1);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
}

#[test]
pub(super) fn pattern_fill_through_the_mmio_aperture_tiles_and_times_busy() {
    let mut machine = test_machine();
    // Seed an 8x8 tile in offscreen VRAM (offset 0x10000, clear of the
    // destination) through the LFB: cell (r, c) = r*8 + c + 1, depth 1.
    let pat_base = 0x1_0000u32;
    for r in 0..8u32 {
        for c in 0..8u32 {
            machine.write_physical_u8(MARGO_LFB_BASE + pat_base + r * 8 + c, (r * 8 + c + 1) as u8);
        }
    }
    write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
    write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
    write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
    write_mmio_reg(&mut machine, 0x144, pat_base); // PAT_BASE
    write_mmio_reg(&mut machine, 0x114, (2 << 16) | 3); // DST_XY: (x=3, y=2)
    write_mmio_reg(&mut machine, 0x11c, (4 << 16) | 4); // DIM: 4x4
    write_mmio_reg(&mut machine, 0x128, 0xf0); // ROP: PATCOPY (P = pattern, no source)
    write_mmio_reg(&mut machine, 0x150, 0x06); // COMMAND: PATTERN_FILL

    // Absolute-phase tiling: dst (x, y) -> tile[y & 7][x & 7] = (y & 7)*8 + (x & 7) + 1.
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2 * 640 + 3), 20); // (3,2) tile[2][3]
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2 * 640 + 6), 23); // (6,2) tile[2][6]
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 5 * 640 + 3), 44); // (3,5) tile[5][3]
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2 * 640 + 2), 0); // left of the rect
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1); // BUSY set

    // 16 pixels -> busy_ns = 100 + 16*5 = 180 ns. At 22 MHz (45.4545 ns/clock),
    // three clocks (136 ns drained) leave it busy; the fourth clears it.
    machine.advance_devices(3);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    machine.advance_devices(1);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
}

#[test]
pub(super) fn clipped_xor_fill_through_the_mmio_aperture() {
    let mut machine = test_machine();
    // Seed x=0..3 at y=0 with 0xFF through the LFB.
    for x in 0u32..4 {
        machine.write_physical_u8(MARGO_LFB_BASE + x, 0xff);
    }
    // FILL the 4x1 row with FG 0x0F through ROP 0x5A (PATINVERT: D ^ P), but clip
    // to x in [0, 3): x=0,1,2 are XORed, x=3 is left alone.
    write_mmio_reg(&mut machine, 0x100, 0); // DST_BASE
    write_mmio_reg(&mut machine, 0x104, 640); // DST_PITCH
    write_mmio_reg(&mut machine, 0x110, 1); // DEPTH
    write_mmio_reg(&mut machine, 0x114, 0); // DST_XY: (0,0)
    write_mmio_reg(&mut machine, 0x11c, (1 << 16) | 4); // DIM: 4x1
    write_mmio_reg(&mut machine, 0x120, 0x0f); // FG_COLOR
    write_mmio_reg(&mut machine, 0x128, 0x5a); // ROP: PATINVERT
    write_mmio_reg(&mut machine, 0x134, 0); // CLIP_TL: (0,0)
    write_mmio_reg(&mut machine, 0x138, (1 << 16) | 3); // CLIP_BR: (3,1) exclusive
    write_mmio_reg(&mut machine, 0x130, 0x2); // FLAGS: CLIP_EN
    write_mmio_reg(&mut machine, 0x150, 0x01); // COMMAND: FILL

    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE), 0xf0); // 0xff ^ 0x0f
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 1), 0xf0);
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2), 0xf0);
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 3), 0xff); // clipped, untouched
    // 3 pixels written -> busy_ns = 100 + 3*5 = 115 ns. At 40 ns/clock, two clocks
    // (80 ns) leave it busy; the third clears it.
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    machine.advance_devices(2);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 1);
    machine.advance_devices(1);
    assert_eq!(read_mmio_reg(&mut machine, 0x008) & 1, 0);
}

#[test]
fn vbe_mode_info_reports_hicolor_masks() {
    // ES = 0x4000 -> physical 0x40000, DI = 0, mode 0x0111 (R5G6B5).
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbf, 0x00, 0x00, // mov di, 0
        0xb8, 0x01, 0x4f, // mov ax, 4F01h
        0xb9, 0x11, 0x01, // mov cx, 0111h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);

    let base = 0x40000;
    assert_eq!(read_u16(&mut machine, base + 0x10), 1280); // BytesPerScanLine = 640 * 2
    assert_eq!(machine.read_physical_u8(base + 0x19), 16); // BitsPerPixel
    assert_eq!(machine.read_physical_u8(base + 0x1f), 5); // RedMaskSize
    assert_eq!(machine.read_physical_u8(base + 0x20), 11); // RedFieldPosition
    assert_eq!(machine.read_physical_u8(base + 0x21), 6); // GreenMaskSize
    assert_eq!(machine.read_physical_u8(base + 0x22), 5); // GreenFieldPosition
    assert_eq!(machine.read_physical_u8(base + 0x23), 5); // BlueMaskSize
    assert_eq!(machine.read_physical_u8(base + 0x24), 0); // BlueFieldPosition
    assert_eq!(machine.read_physical_u8(base + 0x25), 0); // RsvdMaskSize (R5G6B5 has none)
}

#[test]
fn vbe_mode_info_reports_15bpp_masks() {
    // Mode 0x0110 (X1R5G5B5): five-bit channels plus a one-bit reserved field.
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbf, 0x00, 0x00, // mov di, 0
        0xb8, 0x01, 0x4f, // mov ax, 4F01h
        0xb9, 0x10, 0x01, // mov cx, 0110h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);

    let base = 0x40000;
    assert_eq!(read_u16(&mut machine, base + 0x10), 1280); // BytesPerScanLine = 640 * 2
    assert_eq!(machine.read_physical_u8(base + 0x19), 15); // BitsPerPixel
    assert_eq!(machine.read_physical_u8(base + 0x1f), 5); // RedMaskSize
    assert_eq!(machine.read_physical_u8(base + 0x20), 10); // RedFieldPosition
    assert_eq!(machine.read_physical_u8(base + 0x21), 5); // GreenMaskSize
    assert_eq!(machine.read_physical_u8(base + 0x22), 5); // GreenFieldPosition
    assert_eq!(machine.read_physical_u8(base + 0x23), 5); // BlueMaskSize
    assert_eq!(machine.read_physical_u8(base + 0x24), 0); // BlueFieldPosition
    assert_eq!(machine.read_physical_u8(base + 0x25), 1); // RsvdMaskSize (the X bit)
    assert_eq!(machine.read_physical_u8(base + 0x26), 15); // RsvdFieldPosition
}

#[test]
fn hicolor_scanout_decodes_through_the_lfb_aperture() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x111); // 640x480x16, pitch 1280
    // Red pixel (0xf800) at (3, 2): offset 2*1280 + 3*2 = 2566.
    machine.write_physical_u8(MARGO_LFB_BASE + 2566, 0x00);
    machine.write_physical_u8(MARGO_LFB_BASE + 2567, 0xf8);

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    assert_eq!(argb[2 * 640 + 3], 0x00ff_0000);
}

#[test]
pub(super) fn hardware_cursor_composites_through_the_apertures() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x111); // 640x480x16 (R5G6B5)
    // Seed the cursor planes offscreen (1 MiB in, past the 16bpp visible surface)
    // through the LFB. FG pixel at cursor (0,0): XOR plane byte 0 bit 0x80, AND clear.
    let addr = 0x10_0000u32;
    machine.write_physical_u8(MARGO_LFB_BASE + addr + 512, 0x80);
    write_mmio_reg(&mut machine, 0x2c, addr); // CURSOR_ADDR
    write_mmio_reg(&mut machine, 0x30, (5 << 16) | 3); // CURSOR_POS: (x=3, y=5)
    write_mmio_reg(&mut machine, 0x34, 0xf800); // CURSOR_FG = pure red
    write_mmio_reg(&mut machine, 0x38, 0x0000); // CURSOR_BG
    write_mmio_reg(&mut machine, 0x28, 1); // CURSOR_CTRL = ENABLE

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // Cursor pixel (0,0) lands at the positioned screen pixel (3, 5), proving the
    // packed CURSOR_POS encoding routes through the aperture.
    assert_eq!(argb[5 * 640 + 3], 0x00ff_0000); // FG decoded as red at (3,5)
    assert_eq!(argb[0], 0x0000_0000); // the origin is outside the cursor: black surface
}

/// Emit the index-then-data pair a guest uses to reach one Margo extension
/// register. The protected-mode stub in `izbios-vbepm.inc` emits exactly this
/// sequence, so these tests exercise the decode the stub depends on.
fn push_margo_ext(code: &mut Vec<u8>, index: u8, value: u8) {
    code.extend_from_slice(&[0xba, 0xcb, 0x03]); // mov dx, 3CBh (index)
    code.extend_from_slice(&[0xb0, index]); // mov al, index
    code.push(0xee); // out dx, al
    code.extend_from_slice(&[0xba, 0xcd, 0x03]); // mov dx, 3CDh (data)
    code.extend_from_slice(&[0xb0, value]); // mov al, value
    code.push(0xee); // out dx, al
}

#[test]
fn margo_ext_segsel_and_int10_window_are_one_register() {
    // Both directions plus the physical effect. A test that only echoed the
    // register back would pass against a private shadow copy that never moved
    // the window, which is the whole failure this aliasing exists to prevent.
    let mut code = vec![
        0x31, 0xc0, // xor ax, ax
        0x8e, 0xd8, // mov ds, ax
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x01, // mov bx, 0101h (banked, no LFB)
        0xcd, 0x10, // int 10h
        0xb8, 0x00, 0xa0, // mov ax, A000h
        0x8e, 0xc0, // mov es, ax
    ];
    // Bank 2 through the extension registers, then store through the window.
    push_margo_ext(&mut code, 0x00, 2); // SEGSEL_LO
    push_margo_ext(&mut code, 0x01, 0); // SEGSEL_HI
    code.extend_from_slice(&[0x26, 0xc6, 0x06, 0x00, 0x00, 0xaa]); // mov byte [es:0], AAh
    code.extend_from_slice(&[
        0xb8, 0x05, 0x4f, // mov ax, 4F05h
        0xbb, 0x00, 0x01, // mov bx, 0100h (get window A)
        0xcd, 0x10, // int 10h
        0x89, 0x16, 0x02, 0x05, // mov [0502h], dx
        // Now the other direction: INT 10h sets the bank, the port reads it.
        0xb8, 0x05, 0x4f, // mov ax, 4F05h
        0x31, 0xdb, // xor bx, bx (set window A)
        0xba, 0x01, 0x00, // mov dx, 1
        0xcd, 0x10, // int 10h
        0xba, 0xcb, 0x03, // mov dx, 3CBh
        0xb0, 0x00, // mov al, 0 (SEGSEL_LO)
        0xee, // out dx, al
        0xba, 0xcd, 0x03, // mov dx, 3CDh
        0xec, // in al, dx
        0xa2, 0x00, 0x05, // mov [0500h], al
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        rom_with_code(&code),
    )
    .unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    // The port write moved the real window: bank 2 is VRAM offset 0x20000.
    assert_eq!(machine.margo().read_vram_u8(0x2_0000), 0xaa);
    // ... and INT 10h reports the bank the port set.
    assert_eq!(read_u16(&mut machine, 0x0502), 2);
    // ... and the port reports the bank INT 10h set.
    assert_eq!(machine.read_physical_u8(0x0500), 1);
}

#[test]
fn margo_ext_dispctl_latches_the_same_start_as_int10() {
    let mut code = vec![
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x41, // mov bx, 0101h | 4000h (LFB)
        0xcd, 0x10, // int 10h
    ];
    push_margo_ext(&mut code, 0x02, 0x00); // DISPX_LO = 0
    push_margo_ext(&mut code, 0x03, 0x00); // DISPX_HI
    push_margo_ext(&mut code, 0x04, 0xe0); // DISPY_LO = 480
    push_margo_ext(&mut code, 0x05, 0x01); // DISPY_HI
    push_margo_ext(&mut code, 0x06, 0x01); // DISPCTL: latch
    code.push(0xf4); // hlt
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        rom_with_code(&code),
    )
    .unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    // Identical expectations to vbe_display_start_latches_on_the_next_margo_frame:
    // same latch, reached through the port pair instead of INT 10h.
    assert_eq!(machine.margo().display().start, 0);
    assert!(machine.margo().display_start_pending());
    machine.advance_devices_ticks(izarravm_core::MASTER_CLOCK_HZ / 60);
    assert_eq!(machine.margo().display().start, 640 * 480);
}

#[test]
fn margo_ext_dispctl_without_bit0_does_not_latch() {
    // The strobe condition has to be a real gate. Without this the DISPCTL arm
    // could latch on any write and both of the tests above would still pass.
    let mut code = vec![
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x41, // mov bx, 4101h
        0xcd, 0x10, // int 10h
    ];
    push_margo_ext(&mut code, 0x04, 0xe0); // DISPY_LO = 480
    push_margo_ext(&mut code, 0x05, 0x01); // DISPY_HI
    push_margo_ext(&mut code, 0x06, 0x80); // DISPCTL: retrace bit only, no latch
    code.push(0xf4); // hlt
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        rom_with_code(&code),
    )
    .unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert!(!machine.margo().display_start_pending());
    machine.advance_devices_ticks(izarravm_core::MASTER_CLOCK_HZ / 60);
    assert_eq!(machine.margo().display().start, 0);
}

/// A test ROM that keeps the real BIOS image and only replaces its reset entry
/// with `code`. `rom_with_code`'s all-zero image cannot be used by anything that
/// reads a fixed ROM structure -- the VBE 2.0 protected-mode block lives at
/// 0xF100 and would be 176 bytes of zeros there.
fn izbios_rom_with_code(code: &[u8]) -> Vec<u8> {
    let mut rom = izarravm_firmware::IZARRA_BIOS.to_vec();
    assert!(
        code.len() < 0xf000,
        "test code would overwrite the ROM tail"
    );
    rom[..code.len()].copy_from_slice(code);
    rom
}

#[test]
fn vbe_pm_interface_returns_the_rom_block() {
    let rom = izbios_rom_with_code(&[
        0xb8, 0x0a, 0x4f, // mov ax, 4F0Ah
        0x31, 0xdb, // xor bx, bx (subfunction 0)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
    assert_eq!(
        machine.cpu().registers.segment(SegmentIndex::Es).selector,
        izarravm_firmware::IZARRA_BIOS_SEG
    );
    assert_eq!(
        machine.cpu().registers.edi() as u16,
        izarravm_firmware::IZARRA_BIOS_VBE_PM_OFFSET
    );
    let len = izarravm_firmware::izarra_bios_vbe_pm_len();
    assert_eq!(machine.cpu().registers.ecx() as u16, len);
    // The block has to be a real object, not just a plausible pointer: four
    // in-range header offsets, all distinct, all below the reported length.
    let base = (u32::from(izarravm_firmware::IZARRA_BIOS_SEG) << 4)
        + u32::from(izarravm_firmware::IZARRA_BIOS_VBE_PM_OFFSET);
    let header: Vec<u16> = (0..4)
        .map(|i| read_u16(&mut machine, base + i * 2))
        .collect();
    for &offset in &header {
        assert!(offset >= 8 && offset < len, "header offset {offset:#x}");
    }
    assert_eq!(
        header
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        4
    );
}

#[test]
fn vbe_pm_interface_rejects_unknown_subfunctions() {
    let rom = izbios_rom_with_code(&[
        0xb8, 0x0a, 0x4f, // mov ax, 4F0Ah
        0xbb, 0x01, 0x00, // mov bx, 1 (undefined subfunction)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.cpu().registers.eax() as u16, 0x014f);
}

#[test]
fn vbe_pm_stub_drives_the_margo_registers() {
    // The point of 4F0Ah is that the client CALLS the returned code instead of
    // going back through INT 10h. So this test does exactly that: it asks for
    // the block, far-calls SetWindow out of it, and then checks the real window
    // moved by storing through A000h and reading Margo's VRAM. Nothing here is
    // emulated on the host side -- if the assembled stub or the port decode is
    // wrong, the byte lands in the wrong bank.
    let rom = izbios_rom_with_code(&[
        0x31, 0xc0, // xor ax, ax
        0x8e, 0xd8, // mov ds, ax
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x01, // mov bx, 0101h (banked, no LFB)
        0xcd, 0x10, // int 10h
        0xb8, 0x0a, 0x4f, // mov ax, 4F0Ah
        0x31, 0xdb, // xor bx, bx
        0xcd, 0x10, // int 10h
        // Build a far pointer to block + [block+0] (the SetWindow offset).
        0x89, 0xfe, // mov si, di
        0x26, 0x8b, 0x04, // mov ax, [es:si]
        0x01, 0xf8, // add ax, di
        0xa3, 0x10, 0x05, // mov [0510h], ax
        0x8c, 0xc0, // mov ax, es
        0xa3, 0x12, 0x05, // mov [0512h], ax
        // BH=0 set, BL=0 window A, DX=bank 5.
        0x31, 0xdb, // xor bx, bx
        0xba, 0x05, 0x00, // mov dx, 5
        0xff, 0x1e, 0x10, 0x05, // call far [0510h]
        0xb8, 0x00, 0xa0, // mov ax, A000h
        0x8e, 0xc0, // mov es, ax
        0x26, 0xc6, 0x06, 0x00, 0x00, 0x5a, // mov byte [es:0], 5Ah
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(2_000_000).unwrap(),
        StopReason::Halted
    );
    // Bank 5 is VRAM offset 5 * 64 KB. Bank 0 would put it at 0.
    assert_eq!(machine.margo().read_vram_u8(5 * 0x1_0000), 0x5a);
    assert_eq!(machine.margo().read_vram_u8(0), 0);
}

#[test]
fn vbe_pm_stub_set_display_start_latches_through_the_ports() {
    let rom = izbios_rom_with_code(&[
        0x31, 0xc0, // xor ax, ax
        0x8e, 0xd8, // mov ds, ax
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x41, // mov bx, 4101h (LFB)
        0xcd, 0x10, // int 10h
        0xb8, 0x0a, 0x4f, // mov ax, 4F0Ah
        0x31, 0xdb, // xor bx, bx
        0xcd, 0x10, // int 10h
        // SetDisplayStart is the SECOND header word, at block+2.
        0x89, 0xfe, // mov si, di
        0x26, 0x8b, 0x44, 0x02, // mov ax, [es:si+2]
        0x01, 0xf8, // add ax, di
        0xa3, 0x10, 0x05, // mov [0510h], ax
        0x8c, 0xc0, // mov ax, es
        0xa3, 0x12, 0x05, // mov [0512h], ax
        0x31, 0xdb, // xor bx, bx (set now)
        0x31, 0xc9, // xor cx, cx (pixel x = 0)
        0xba, 0xe0, 0x01, // mov dx, 480 (scan line)
        0xff, 0x1e, 0x10, 0x05, // call far [0510h]
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(2_000_000).unwrap(),
        StopReason::Halted
    );
    assert!(machine.margo().display_start_pending());
    machine.advance_devices_ticks(izarravm_core::MASTER_CLOCK_HZ / 60);
    assert_eq!(machine.margo().display().start, 640 * 480);
}

#[test]
fn vbe_pm_stub_set_palette_writes_the_dac() {
    // The third routine in the block, and the one an 8bpp game leans on hardest.
    // Entry order is blue, green, red, pad -- the reverse of the DAC's own write
    // order -- so a stub that loaded the three bytes sequentially would produce
    // a plausible palette with red and blue swapped, which no smoke test would
    // notice. Hence distinct values per channel.
    let mut code = vec![
        0x31, 0xc0, // xor ax, ax
        0x8e, 0xd8, // mov ds, ax
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x41, // mov bx, 4101h (LFB, 8bpp)
        0xcd, 0x10, // int 10h
        0xb8, 0x0a, 0x4f, // mov ax, 4F0Ah
        0x31, 0xdb, // xor bx, bx
        0xcd, 0x10, // int 10h
        // SetPrimaryPalette is the THIRD header word, at block+4.
        0x89, 0xfe, // mov si, di
        0x26, 0x8b, 0x44, 0x04, // mov ax, [es:si+4]
        0x01, 0xf8, // add ax, di
        0xa3, 0x10, 0x05, // mov [0510h], ax
        0x8c, 0xc0, // mov ax, es (still the ROM segment)
        0xa3, 0x12, 0x05, // mov [0512h], ax
    ];
    // Two entries at 0000:2000, each blue, green, red, pad.
    for (offset, byte) in [0x01u8, 0x02, 0x03, 0x00, 0x04, 0x05, 0x06, 0x00]
        .into_iter()
        .enumerate()
    {
        let addr = 0x2000u16 + offset as u16;
        code.extend_from_slice(&[0xc6, 0x06, addr as u8, (addr >> 8) as u8, byte]);
    }
    code.extend_from_slice(&[
        0x31, 0xc0, // xor ax, ax
        0x8e, 0xc0, // mov es, ax (ES:DI now points at the table)
        0xbf, 0x00, 0x20, // mov di, 2000h
        0x31, 0xdb, // xor bx, bx (BL=0: set now)
        0xb9, 0x02, 0x00, // mov cx, 2 (two entries)
        0xba, 0x05, 0x00, // mov dx, 5 (first DAC index)
        0xff, 0x1e, 0x10, 0x05, // call far [0510h]
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izbios_rom_with_code(&code),
    )
    .unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(2_000_000).unwrap(),
        StopReason::Halted
    );
    // dac_entry is [r, g, b], the table is blue-first: entry 0 is B=1 G=2 R=3.
    assert_eq!(machine.video().dac_entry(5), [3, 2, 1]);
    assert_eq!(machine.video().dac_entry(6), [6, 5, 4]);
    // The entry below the start index keeps the BIOS default palette, where 4 is
    // red at 6-bit full scale. This is the check that DX is honoured: a stub
    // that ignored it and always began at index 0 would still pass both
    // assertions above while quietly overwriting the low entries.
    assert_eq!(machine.video().dac_entry(4), [42, 0, 0]);
}
