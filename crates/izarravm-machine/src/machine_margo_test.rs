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
fn vbe_set_mode_clears_display_memory_unless_bit15_says_keep() {
    // VBE 4F02, BX bit 15: 0 = clear display memory (the default every mode
    // set relies on), 1 = preserve it. The graphical POST leaves its frame in
    // Margo VRAM; without the clear, Descent II's VESA menu scans out stale
    // POST pixels around its own drawing (seen as re-tinted POST bands in the
    // stage-0 sweep screens). The legacy INT 10h path already models its
    // equivalent (mode number bit 7); this pins the VBE side.
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.vega.set_vbe_mode(0x0101));
    machine.vega.margo_mut().vram_mut()[..4].copy_from_slice(&[1, 2, 3, 4]);

    // Bit 15 set: memory survives the mode set.
    assert!(machine.vega.set_vbe_mode(0x8105));
    assert_eq!(&machine.vega.margo_mut().vram()[..4], &[1, 2, 3, 4]);

    // Bit 15 clear: memory is cleared.
    assert!(machine.vega.set_vbe_mode(0x0101));
    assert!(
        machine.vega.margo_mut().vram()[..4].iter().all(|&b| b == 0),
        "4F02 without BX bit 15 must clear display memory"
    );
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

/// T6 -- THE PROBE/COPY AGREEMENT INVARIANT, which the whole mechanism rests on.
///
/// `direct_vga_bytes` decides `bulk_direct`; `direct_write_page` produces the
/// pointer. If the first says yes where the second says no, every REP run builds a
/// 4 KiB buffer, issues a full bulk source read, then abandons -- and because the
/// loop re-enters at L-1 the waste repeats at L, L-1, ..., 1. QUADRATIC. That is
/// what makes partial admission unshippable rather than merely wasteful, and
/// nothing pinned it before this slice.
#[test]
fn banked_probe_and_copy_agree_for_every_page_of_the_window() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.vega.set_vbe_mode(0x0101)); // banked
    assert!(machine.vega.margo_banked_window_key().is_some());

    let mut checked = 0;
    for page in (0..0x1_0000u32).step_by(0x1000) {
        let address = 0x000a_0000 + page;
        let probe = with_bus(&mut machine, |bus| {
            bus.direct_memory_bytes(address, 4, BusWidth::Dword, BusAccessKind::DataWrite)
        });
        let pointer = machine.vega.direct_write_page(address).is_some();
        assert_eq!(
            probe == 4,
            pointer,
            "probe and copy disagree at {address:#x}: probe said {probe}, pointer {pointer}"
        );
        checked += 1;
    }
    assert_eq!(checked, 16, "the banked window is 16 pages");
    assert!(
        with_bus(&mut machine, |bus| bus.direct_memory_bytes(
            0x000a_0000,
            4,
            BusWidth::Dword,
            BusAccessKind::DataWrite
        )) == 4,
        "non-vacuous: the window must actually be ADMITTED, or the equality above \
         is two nos agreeing"
    );
}

/// T2 -- a bank switch must move which BYTES the pointer means.
///
/// The previous version of this test asserted `margo_banked_window_key()` changed,
/// which re-derives the very arithmetic under test: making the key ignore the
/// offset entirely -- every bank aliasing bank 0 -- passed it. That is the single
/// most important property the slice has, and it was unpinned.
///
/// This writes THROUGH the pointer and reads the frame store back, so the only way
/// to pass is to point at the right bytes.
#[test]
fn the_banked_direct_page_points_at_the_bank_it_names() {
    const BANK: u16 = 3;
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.vega.set_vbe_mode(0x0101)); // banked

    // Bank 0: write 0xAA through the host pointer at the window base.
    let p0 = machine
        .vega
        .margo_banked_direct_page(0x000a_0000)
        .expect("bank 0 page");
    unsafe { *p0 = 0xAA };

    assert_eq!(machine.vega.vbe_window_control(0x0000, BANK), Ok(BANK));

    // Bank 3: the SAME physical page must now reach a different frame-store offset.
    let p3 = machine
        .vega
        .margo_banked_direct_page(0x000a_0000)
        .expect("bank 3 page");
    unsafe { *p3 = 0x55 };

    let vram = machine.vega.margo().vram();
    let bank_bytes = 0x1_0000usize;
    assert_eq!(
        vram[0], 0xAA,
        "bank 0's byte must still be at frame-store 0"
    );
    assert_eq!(
        vram[BANK as usize * bank_bytes],
        0x55,
        "bank {BANK}'s byte must land {BANK} windows in, not aliased onto bank 0"
    );
    assert_ne!(
        p0, p3,
        "and the two pointers must differ -- equality here is the aliasing bug"
    );
}

/// T5a -- the BULK COPY into the banked window must land in the bank it names.
///
/// WHY THIS TEST CHANGED, because the previous version proved less than it read
/// as. It probed admission with `direct_memory_bytes` -- which returns a COUNT
/// and never dereferences anything -- and then wrote its bytes with
/// `write_physical_u8`. That is the ORDINARY path, and it does not touch the
/// granted pointer at all: `bus.rs`'s `write_memory_byte_recorded` routes the
/// byte to `Vega::write_memory_u8`, which derives the bank offset for itself in
/// `margo_banked_window_offset` -- an INDEPENDENT second computation of
/// `bank * 64K + offset`. So the bytes landed in the right bank by a route the
/// aliasing mutation cannot reach, and S6 (`margo_banked_direct_page` drops the
/// bank base, every bank aliasing bank 0) survived it.
///
/// `write_memory_bytes_direct` is the function the REP string path actually
/// calls (`strings.rs`, both the MOVS and the STOS arm) and the only one that
/// copies through the pointer `direct_write_page` grants. Its return value is
/// the non-vacuity guard: a refusal returns 0 rather than writing anywhere else.
#[test]
fn a_bulk_copy_into_the_banked_window_lands_in_the_named_bank() {
    const BANK: u16 = 1;
    const COUNT: usize = 256;
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.vega.set_vbe_mode(0x0101));
    assert_eq!(machine.vega.vbe_window_control(0x0000, BANK), Ok(BANK));

    let payload: Vec<u8> = (0..COUNT).map(|i| (i & 0xff) as u8).collect();
    let put = with_bus(&mut machine, |bus| {
        bus.write_memory_bytes_direct(
            0x000a_0000,
            &payload,
            BusWidth::Byte,
            BusAccessKind::DataWrite,
        )
    })
    .unwrap();
    assert_eq!(
        put, COUNT,
        "the banked window must be admitted for the bulk copy, or the slice's \
         whole mechanism is inert -- and the placement assertions below would be \
         asserting about bytes nobody wrote"
    );

    let vram = machine.vega.margo().vram();
    let bank_bytes = 0x1_0000usize;
    for i in 0..COUNT {
        assert_eq!(
            vram[BANK as usize * bank_bytes + i],
            payload[i],
            "byte {i} must land in bank {BANK} of the frame store"
        );
    }
    assert!(
        vram[..COUNT].iter().all(|&byte| byte == 0),
        "and bank 0 must be UNTOUCHED -- every bank aliasing bank 0 is the \
         mutation this test exists for, and it lands its bytes exactly here"
    );
}

#[test]
fn a_bulk_copy_into_the_linear_framebuffer_takes_the_direct_path() {
    const OFFSET: usize = 0x1100;
    const COUNT: usize = 0x800;
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.vega.set_vbe_mode(0x4101));

    let payload: Vec<u8> = (0..COUNT)
        .map(|i| (i.wrapping_mul(37) & 0xff) as u8)
        .collect();
    let address = MARGO_LFB_BASE + OFFSET as u32;
    let admitted = with_bus(&mut machine, |bus| {
        bus.direct_memory_bytes(
            address,
            payload.len(),
            BusWidth::Byte,
            BusAccessKind::DataWrite,
        )
    });
    assert_eq!(admitted, payload.len());

    let written = with_bus(&mut machine, |bus| {
        bus.write_memory_bytes_direct(address, &payload, BusWidth::Byte, BusAccessKind::DataWrite)
    })
    .unwrap();
    assert_eq!(written, payload.len());
    assert_eq!(
        &machine.vega.margo().vram()[OFFSET..OFFSET + COUNT],
        payload.as_slice()
    );
    let metrics = machine.video_host_metrics();
    assert_eq!(metrics.margo_lfb_direct_write_bytes, COUNT as u64);
    assert_eq!(metrics.margo_lfb_slow_write_bytes, 0);
}

/// A scanline copy that CROSSES a 4 KiB boundary must still take the direct
/// path.
///
/// Row 6 of a 640-byte-pitch mode starts at 0xF00 and ends at 0x1180, and one
/// row in seven lands like this; at 1024 bytes per row it is one in two. Margo's
/// frame store is a single contiguous allocation and the aperture probe bounds-
/// checks the whole run against it, so a page boundary inside the run means
/// nothing here -- the page-crossing clause that guards the RAM and legacy-VGA
/// probes would have sent the common case back to the per-byte path.
#[test]
fn a_page_crossing_scanline_copy_still_takes_the_direct_path() {
    const PITCH: usize = 640;
    const OFFSET: usize = 6 * PITCH;
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.vega.set_vbe_mode(0x4101));
    const _: () = assert!(
        OFFSET / 0x1000 != (OFFSET + PITCH - 1) / 0x1000,
        "the fixture is only discriminating while the run crosses a page"
    );

    let payload: Vec<u8> = (0..PITCH)
        .map(|i| (i.wrapping_mul(29) & 0xff) as u8)
        .collect();
    let address = MARGO_LFB_BASE + OFFSET as u32;
    let admitted = with_bus(&mut machine, |bus| {
        bus.direct_memory_bytes(
            address,
            payload.len(),
            BusWidth::Byte,
            BusAccessKind::DataWrite,
        )
    });
    assert_eq!(
        admitted,
        payload.len(),
        "the whole row, not the page remnant"
    );

    let written = with_bus(&mut machine, |bus| {
        bus.write_memory_bytes_direct(address, &payload, BusWidth::Byte, BusAccessKind::DataWrite)
    })
    .unwrap();
    assert_eq!(written, payload.len());
    assert_eq!(
        &machine.vega.margo().vram()[OFFSET..OFFSET + PITCH],
        payload.as_slice()
    );

    let mut read_back = vec![0u8; PITCH];
    let read = with_bus(&mut machine, |bus| {
        bus.read_memory_bytes_direct(
            address,
            &mut read_back,
            BusWidth::Byte,
            BusAccessKind::DataRead,
        )
    })
    .unwrap();
    assert_eq!(read, payload.len());
    assert_eq!(read_back, payload);

    let metrics = machine.video_host_metrics();
    assert_eq!(metrics.margo_lfb_direct_write_bytes, PITCH as u64);
    assert_eq!(metrics.margo_lfb_direct_read_bytes, PITCH as u64);
    assert_eq!(metrics.margo_lfb_slow_write_bytes, 0);
    assert_eq!(metrics.margo_lfb_slow_read_bytes, 0);
}

#[test]
fn margo_frame_publication_reports_stable_generation_and_row_damage() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.vega.set_vbe_mode(0x4101));

    let generation = machine
        .presented_frame_generation()
        .expect("Margo graphics output has a generation");
    let first = machine.presented_frame_update().expect("Margo frame");
    assert_eq!(
        first.changed_rows,
        std::iter::once(0..480).collect::<Vec<_>>()
    );
    assert_eq!(
        machine.video_host_metrics().margo_scanout_rows_converted,
        480
    );
    assert_eq!(machine.presented_frame_generation(), Some(generation));

    with_bus(&mut machine, |bus| {
        bus.write_memory(
            MARGO_LFB_BASE + 3 * 640 + 17,
            BusWidth::Byte,
            0x2a,
            BusAccessKind::DataWrite,
        )
    })
    .unwrap();
    let second = machine
        .presented_frame_update()
        .expect("updated Margo frame");
    assert_eq!(
        second.changed_rows,
        std::iter::once(3..4).collect::<Vec<_>>()
    );
    assert_eq!(
        machine.video_host_metrics().margo_scanout_rows_converted,
        481
    );
    assert_eq!(second.words[3 * 640 + 17], machine.palette_argb()[0x2a]);
    assert_ne!(machine.presented_frame_generation(), Some(generation));
}

/// T5b -- and a real REP MOVS must REACH that bulk pair.
///
/// T5a proves the copy lands correctly once someone calls it. This proves the
/// string path is who calls it, which is the slice's actual claim: 99.97% of
/// nascar's aperture traffic is a REP MOVS, and before the slice
/// `direct_vga_bytes` refused on the zeroed token, `bulk_direct` went false, and
/// the run fell back to one bus round-trip per iteration.
///
/// NON-VACUITY, which matters more here than usual: the bytes land in the right
/// bank under the per-iteration fallback TOO (that path recomputes the offset in
/// `Vega::write_memory_u8`), so placement alone cannot tell the two mechanisms
/// apart. `data_slow_writes` is what does: it is incremented once per non-direct
/// sized access, the REP is the only memory write this program makes, and the
/// bulk arm scores `data_direct_writes` instead.
#[test]
fn a_rep_movs_into_the_banked_window_takes_the_bulk_path() {
    const SRC: u32 = 0x2_0000;
    const BANK: u16 = 1;
    const COUNT: usize = 256;
    let rom = rom_with_code(&[
        0xfc, // cld
        0xb8, 0x00, 0x20, // mov ax, 2000h
        0x8e, 0xd8, // mov ds, ax        (source at 0x20000)
        0xb8, 0x00, 0xa0, // mov ax, A000h
        0x8e, 0xc0, // mov es, ax        (the banked window)
        0x31, 0xf6, // xor si, si
        0x31, 0xff, // xor di, di
        0xb9, 0x00, 0x01, // mov cx, 100h
        0xf3, 0xa4, // rep movsb
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    assert!(machine.vega.set_vbe_mode(0x0101));
    assert_eq!(machine.vega.vbe_window_control(0x0000, BANK), Ok(BANK));
    for i in 0..COUNT {
        machine.write_physical_u8(SRC + i as u32, (i & 0xff) as u8);
    }

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );

    let perf = machine.cpu().perf_counters().clone();
    assert_eq!(
        perf.data_slow_writes, 0,
        "the REP is this program's only memory write, so a single slow write \
         means it took the per-iteration fallback the slice exists to remove"
    );
    assert!(
        perf.data_direct_writes >= COUNT as u64,
        "and the bulk arm must have scored all {COUNT} iterations, not {}",
        perf.data_direct_writes
    );

    let vram = machine.vega.margo().vram();
    let bank_bytes = 0x1_0000usize;
    for i in 0..COUNT {
        assert_eq!(
            vram[BANK as usize * bank_bytes + i],
            (i & 0xff) as u8,
            "byte {i} must land in bank {BANK} of the frame store"
        );
    }
    assert!(
        vram[..COUNT].iter().all(|&byte| byte == 0),
        "and bank 0 must be UNTOUCHED"
    );
}

/// T3 -- THE C1 REGRESSION. Banked VESA, then `INT 10h AX=0003h`, must move the
/// compared identity.
///
/// `select_legacy` clears `margo_active` and touches nothing else. The token is 0
/// before and 0 after (the VGA reports 0 for text), and bank and linear do not
/// move -- so the design's original `(token, bank, linear)` tuple was INVARIANT
/// across a transition that flips the mapping, leaving a live pointer into Margo
/// VRAM serving legacy VGA writes. That is the shutdown path of both measured
/// fixtures.
#[test]
fn leaving_a_banked_mode_for_text_moves_the_direct_write_identity() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.vega.set_vbe_mode(0x0101));
    let banked = machine.vega.direct_write_identity();
    assert!(machine.vega.margo_banked_direct_page(0x000a_0000).is_some());

    machine.vega.select_legacy();

    assert!(
        machine.vega.margo_banked_direct_page(0x000a_0000).is_none(),
        "the grant must be revoked"
    );
    assert_ne!(
        banked,
        machine.vega.direct_write_identity(),
        "and the IDENTITY must move with it -- this is the assertion the \
         (token, bank, linear) tuple failed"
    );
}

/// T4a -- banked from TEXT. Without the Margo arm this trips
/// `debug_assert_ne!(direct_write_token(), 0)` at `vga.rs:747`.
#[test]
fn a_banked_write_from_text_does_not_notify_the_vga() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.vega.set_vbe_mode(0x0101));
    machine.vega.note_direct_write(0x000a_0100, 4);
    machine.vega.note_direct_write_pages(0x0001);
    assert!(!machine.vega.legacy().mode13_linear_authoritative());
}

/// T4b -- banked from MODE 13h, which is the branch that matters and which the
/// previous single test never reached: `for pre_mode in [0x03, 0x13]` panicked on
/// the text state first, so this one never ran.
///
/// Here `Vga::direct_write_token()` is 1, so the assert PASSES and `vga.rs:777-781`
/// sets `mode13_linear_authoritative` from Margo offsets instead. Silent
/// corruption, not a loud assert -- and the old test's only assertion
/// (`key.is_some()`) could not have seen it either way.
#[test]
fn a_banked_write_from_mode13_does_not_make_the_vga_authoritative() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.set_vga_mode(0x13));
    assert!(machine.vega.set_vbe_mode(0x0101));

    machine.vega.note_direct_write(0x000a_0100, 4);
    machine.vega.note_direct_write_pages(0x0001);

    assert!(
        !machine.vega.legacy().mode13_linear_authoritative(),
        "Margo's writes must not make the VGA's mode13 surface authoritative"
    );
}

/// R-4a -- the identity guard must fire through the REAL `INT 10h` entry point.
///
/// T2 and T3 call `Vega` methods directly, so reverting the `handle_int10`
/// token-compare wrapper to the bare token survives them. For 4F05 that revert
/// is a live bug: the banked token is `0xff` before and after, so `0xff == 0xff`
/// compares equal and the cached-pointer invalidation never fires.
///
/// This pins the `video.rs` wrapper ONLY. The port-write wrapper in `bus.rs` is
/// a second, independent comparison of the same identity, and no INT 10h reaches
/// it -- see `a_bank_move_through_the_segsel_port_invalidates_the_data_map`,
/// which is the test that pins it. Both reverts were run against this test: it
/// catches the `video.rs` one and does NOT catch the `bus.rs` one, which is
/// exactly the coverage boundary a reader should not have to guess at.
#[test]
fn a_guest_bank_switch_through_int10_invalidates_the_data_map() {
    let rom = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x01, // mov bx, 0101h (banked)
        0xcd, 0x10, // int 10h
        0xb8, 0x05, 0x4f, // mov ax, 4F05h
        0x31, 0xdb, // xor bx, bx (set window A)
        0xba, 0x02, 0x00, // mov dx, 2 (bank 2)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    // The 4F02 alone already invalidates (the identity moves legacy -> banked), so
    // an ABSOLUTE count proves nothing about the 4F05. Measure the DELTA against a
    // run that stops after the mode set.
    let mode_set_only = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x01, // mov bx, 0101h (banked)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut base =
        Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), mode_set_only).unwrap();
    assert_eq!(
        base.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    let without_4f05 = base.cpu().perf_counters().direct_map_invalidations;

    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.vega.margo_banked_window_key(), Some(2 * 0x1_0000));
    assert!(
        machine.cpu().perf_counters().direct_map_invalidations > without_4f05,
        "a 4F05 through INT 10h must reach the data-map invalidation, or a cached \
         pointer keeps serving the old bank"
    );
}

/// R-4b / S7 -- the OTHER identity wrapper, the one on the port-write path.
///
/// `bus.rs` compares `direct_write_identity` across every accepted device-port
/// write, and that is a separate comparison from `handle_int10`'s. A guest moves
/// the Margo window through the SEGSEL extension registers as well as through
/// 4F05 -- `izbios-vbepm.inc`'s protected-mode stub uses exactly this pair -- so
/// this path carries live bank moves and reverting it to the bare token is the
/// same live bug as reverting the other one.
///
/// WHAT MAKES THIS DISTINGUISHING, which the INT 10h delta test could not be:
/// the observation has to separate "the wrapper compared the BANK" from "the
/// wrapper compared only the token". A bank move is precisely the transition
/// where the bare token does NOT move (`0xff` while banked, whatever the bank),
/// so the test asserts BOTH halves at once -- the key moved, the bare token did
/// not, and the data map was still marked. Under the revert the third assertion
/// is the only one that can fail, and it must.
///
/// The flag is cleared after the mode set for the reason the INT 10h test needed
/// a delta: a 4F02 raises it on its own, so a bare `assert!(flag)` afterwards
/// would pass on the mode set alone and say nothing about the bank move.
#[test]
fn a_bank_move_through_the_segsel_port_invalidates_the_data_map() {
    const MARGO_EXT_INDEX: u16 = 0x03cb;
    const MARGO_EXT_DATA: u16 = 0x03cd;
    const SEGSEL_LO: u32 = 0x00;
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert!(machine.vega.set_vbe_mode(0x0101)); // banked
    assert_eq!(machine.vega.margo_banked_window_key(), Some(0));

    let token_before = machine.vega.direct_write_token();
    machine.direct_data_map_changed = false;
    with_bus(&mut machine, |bus| {
        bus.write_io(MARGO_EXT_INDEX, BusWidth::Byte, SEGSEL_LO, false)
            .unwrap();
        bus.write_io(MARGO_EXT_DATA, BusWidth::Byte, 2, false)
            .unwrap();
    });

    assert_eq!(
        machine.vega.margo_banked_window_key(),
        Some(2 * 0x1_0000),
        "non-vacuous: the port write must actually have moved the window, or the \
         invalidation assertion below is about nothing"
    );
    assert_eq!(
        machine.vega.direct_write_token(),
        token_before,
        "and the BARE token must NOT have moved -- that is why comparing it \
         instead of the identity is a live bug rather than a stylistic one"
    );
    assert!(
        machine.direct_data_map_changed,
        "a bank move through the SEGSEL port must mark the direct data map \
         changed, or a cached pointer keeps serving the old bank"
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
    let text: Vec<u8> = (0..25)
        .map(|i| machine.read_physical_u8(oem_linear + i))
        .collect();
    assert_eq!(&text, b"Izarra3000 VEGA/Margo VBE");
    assert_eq!(machine.read_physical_u8(oem_linear + 25), 0);

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

/// Physical frames the fixture's two upper-memory pages are mapped to, and the
/// physical address the caller's block therefore occupies. A page-walking
/// handler writes there; the broken one wrote at the identity address
/// 000C8C60h, which is the unbacked upper-memory hole.
pub(super) const UMB_FRAME_LOW: u32 = 0x0011_0000;
pub(super) const UMB_FRAME_HIGH: u32 = 0x0012_0000;
pub(super) const UMB_BUFFER_PHYSICAL: u32 = UMB_FRAME_LOW + 0x0c60;

/// A V86 machine whose upper-memory pages 000C8000h and 000C9000h are mapped to
/// `UMB_FRAME_LOW` and `UMB_FRAME_HIGH`, with ES = C8C6h so ES:DI addresses a
/// caller block inside them: the shape TOKAEMM leaves a DPMI transfer buffer in.
/// The two frames are deliberately not adjacent, so a block that spans the page
/// boundary cannot be written correctly by one translation. Everything else in
/// the first 4 MB is identity mapped, so nothing in a fixture faults for an
/// unrelated reason.
pub(super) fn umb_paged_machine() -> Machine {
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &[0xf4]).unwrap();
    install_umb_paging(&mut machine);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0xc8c6));
    machine
}

/// The paging half of `umb_paged_machine`, on its own, so a fixture that must
/// start from a fully furnished machine (a mounted disc, a booted BIOS) can be
/// put into the same V86-with-paging shape afterwards. The page directory and
/// table occupy physical 1000h and 2000h.
pub(super) fn install_umb_paging(machine: &mut Machine) {
    const PD: u32 = 0x1000;
    const PT: u32 = 0x2000;

    machine.write_physical_u32(PD, PT | 7);
    for page in 0u32..1024 {
        let pte = match page {
            0xc8 => UMB_FRAME_LOW | 7,
            0xc9 => UMB_FRAME_HIGH | 7,
            _ => (page << 12) | 7,
        };
        machine.write_physical_u32(PT + page * 4, pte);
    }
    machine.cpu.control.cr3 = PD;
    machine.cpu.control.cr0 |= 0x8000_0001;
    machine.cpu.registers.eflags |= 0x0002_0000; // VM: a V86 task, as under TOKAEMM
}

/// Corpus row MonikaTT (eXoDOS "Monika's Tic Tac Toe", stage-1 pass B): a DJGPP
/// program that asks for 640x480 in 65536 colours -- VBE mode 111h -- and gave
/// up with "The video card isn't handling 640x480x16, or haven't VESA 1.2
/// support" before it ever issued a 4F02h. The row was filed as a refused mode;
/// mode 111h has been in the Margo table all along. What failed is one address.
///
/// The program is a DPMI client. CWSDPMI takes its transfer buffer from DOS,
/// TOKAEMM supplies upper memory out of extended memory, and the buffer landed
/// in a UMB at segment C8C6h. Guest linear 000C8C60h is not physical 000C8C60h
/// there -- the measured run mapped it to 00110C60h -- and the VBE handler
/// deposited the 4F00h controller block at the linear address as though it were
/// physical. That address is the unbacked upper-memory hole, so the write went
/// nowhere and the program read back 0xFF where the "VESA" signature belongs.
/// Every mode the card offers was invisible to it.
///
/// The fixture cannot pass by accident: the destination frame is asserted clear
/// first, so the bytes can only have arrived through the page walk.
#[test]
fn vbe_controller_info_lands_in_a_non_identity_mapped_caller_buffer() {
    let mut machine = umb_paged_machine();

    assert_eq!(
        machine.read_physical_u32(UMB_BUFFER_PHYSICAL),
        0,
        "precondition: the mapped frame must be clear, or this test cannot tell \
         a page-walking write from an identity-assuming one"
    );

    machine.cpu.registers.set_edi(0);
    machine.cpu.registers.set_eax(0x4f00);
    machine.handle_int10();

    assert_eq!(machine.cpu.registers.eax() as u16, 0x004f);
    assert_eq!(
        machine.read_guest_block(UMB_BUFFER_PHYSICAL, 4),
        b"VESA".to_vec(),
        "the signature must arrive at the frame the caller's page is mapped to"
    );
    assert_eq!(read_u16(&mut machine, UMB_BUFFER_PHYSICAL + 0x04), 0x0200);

    // VideoModePtr is a far pointer built from the caller's own ES:DI, so it
    // still reads back as C8C6:0022 and the mode list follows it at the same
    // frame. 111h is what the row asked for.
    assert_eq!(
        read_u32(&mut machine, UMB_BUFFER_PHYSICAL + 0x0e),
        (0xc8c6u32 << 16) | 0x22
    );
    let mut modes = Vec::new();
    let mut at = UMB_BUFFER_PHYSICAL + 0x22;
    loop {
        let mode = read_u16(&mut machine, at);
        if mode == 0xffff {
            break;
        }
        modes.push(mode);
        at += 2;
    }
    assert!(
        modes.contains(&0x111),
        "the enumeration must offer 640x480x16, got {modes:#06x?}"
    );
}

/// The 4F01h half of the same address defect: the caller asks for mode 111h's
/// ModeInfoBlock into a non-identity page and must get it, with the
/// direct-colour fields a VBE 1.2 client checks before it commits to a mode.
#[test]
fn vbe_mode_info_for_111h_lands_in_a_non_identity_mapped_caller_buffer() {
    let mut machine = umb_paged_machine();
    machine.cpu.registers.set_edi(0);
    machine.cpu.registers.set_eax(0x4f01);
    machine.cpu.registers.set_ecx(0x0111);
    machine.handle_int10();

    assert_eq!(machine.cpu.registers.eax() as u16, 0x004f);
    assert_eq!(read_u16(&mut machine, UMB_BUFFER_PHYSICAL + 0x10), 1280); // BytesPerScanLine
    assert_eq!(read_u16(&mut machine, UMB_BUFFER_PHYSICAL + 0x12), 640); // XResolution
    assert_eq!(read_u16(&mut machine, UMB_BUFFER_PHYSICAL + 0x14), 480); // YResolution
    assert_eq!(machine.read_physical_u8(UMB_BUFFER_PHYSICAL + 0x19), 16); // BitsPerPixel
    assert_eq!(machine.read_physical_u8(UMB_BUFFER_PHYSICAL + 0x1b), 6); // MemoryModel
    assert_eq!(machine.read_physical_u8(UMB_BUFFER_PHYSICAL + 0x1f), 5); // RedMaskSize
    assert_eq!(machine.read_physical_u8(UMB_BUFFER_PHYSICAL + 0x20), 11); // RedFieldPosition
    assert_eq!(machine.read_physical_u8(UMB_BUFFER_PHYSICAL + 0x21), 6); // GreenMaskSize
    assert_eq!(machine.read_physical_u8(UMB_BUFFER_PHYSICAL + 0x23), 5); // BlueMaskSize
}

/// The 256-byte block is longer than the distance from ES:DI to the end of its
/// page whenever the caller's buffer sits near a page boundary, and the two
/// pages need not be mapped to adjacent frames. One translation of the block's
/// first byte would scatter the tail; the per-page split is what keeps it whole.
///
/// ES:DI = C8C6:0370 puts the split at block offset 30h, inside the mode list:
/// the signature and the first seven mode numbers are in the low frame, the
/// eighth onwards in the high one.
#[test]
fn vbe_controller_info_spanning_a_page_boundary_follows_both_mappings() {
    let mut machine = umb_paged_machine();
    machine.cpu.registers.set_edi(0x0370);
    machine.cpu.registers.set_eax(0x4f00);
    machine.handle_int10();

    assert_eq!(machine.cpu.registers.eax() as u16, 0x004f);
    let head = UMB_FRAME_LOW + 0x0fd0; // linear 000C8FD0
    assert_eq!(machine.read_guest_block(head, 4), b"VESA".to_vec());
    assert_eq!(read_u16(&mut machine, head + 0x22), 0x100);
    // Block offset 30h is the first byte of the next page, mapped elsewhere.
    assert_eq!(
        read_u16(&mut machine, UMB_FRAME_HIGH),
        0x114,
        "the tail of the mode list must follow the second page's mapping"
    );
}

/// A VBE 1.2 client reads MemoryModel to tell an indexed mode from a
/// direct-colour one; the RGB mask fields are only defined for model 06h. The
/// handler reported 04h (packed pixel) for every mode, including the 15/16/32bpp
/// ones, which is the answer a 256-colour mode gives. Pinned per depth so the two
/// families cannot be given one answer again.
#[test]
fn vbe_mode_info_reports_the_memory_model_for_the_depth() {
    for (mode, expected) in [
        (0x0101u16, 4u8), // 640x480x8, packed pixel
        (0x0110, 6),      // 640x480x15, direct colour
        (0x0111, 6),      // 640x480x16, direct colour
        (0x014a, 6),      // 640x480x32, direct colour
    ] {
        let mut code = vec![
            0xb8, 0x00, 0x40, // mov ax, 4000h
            0x8e, 0xc0, // mov es, ax
            0xbf, 0x00, 0x00, // mov di, 0
            0xb8, 0x01, 0x4f, // mov ax, 4F01h
            0xb9, 0x00, 0x00, // mov cx, mode (patched below)
            0xcd, 0x10, // int 10h
            0xf4, // hlt
        ];
        code[12] = mode as u8;
        code[13] = (mode >> 8) as u8;
        let rom = rom_with_code(&code);
        let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
        assert_eq!(
            machine.run_until_halt_or_cycles(1_000_000).unwrap(),
            StopReason::Halted
        );
        assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
        assert_eq!(
            machine.read_physical_u8(0x40000 + 0x1b),
            expected,
            "MemoryModel for mode {mode:#06x}"
        );
    }
}

/// The row's own request, end to end in real mode: 4F02h with BX=0111h must be
/// accepted and must leave the display 640x480 at 16bpp.
#[test]
fn vbe_set_mode_accepts_111h_and_selects_640x480x16() {
    let rom = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x11, 0x01, // mov bx, 0111h (banked window, clear memory)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
    let display = machine.margo().display();
    assert_eq!(display.mode, 0x111);
    assert_eq!((display.width, display.height, display.bpp), (640, 480, 16));
    assert_eq!(display.pitch, 1280);
}

// --- The same address defect in the rest of the INT 10h HLE -----------------
//
// 4F00h/4F01h/4F09h were corrected in isolation, but every other INT 10h
// service that deposits a block at a caller's `ES:`-relative pointer made the
// same mistake: the segment base plus the offset went onto the bus as a
// physical address. `run.rs` dispatches these services for any caller that is
// not in ring-0 protected mode, which includes a V86 task under TOKAEMM with
// paging on, so the caller's buffer can be wherever its page tables say.

/// The identity address the fixture's caller buffer occupies -- the unbacked
/// upper-memory hole a handler that ignores paging writes to and reads from.
const UMB_BUFFER_IDENTITY: u32 = 0x000c_8c60;

/// Lay a decoy over the identity-addressed range under the caller's buffer, so
/// a read-side handler that treats the caller's pointer as physical returns
/// something recognisably wrong whether or not that range happens to be backed.
/// Without this, a fixture that plants a block and reads it back could pass on
/// the broken code by using the same wrong address twice.
fn poison_umb_identity_range(machine: &mut Machine, len: usize) {
    for offset in 0..len {
        machine.write_physical_u8(UMB_BUFFER_IDENTITY + offset as u32, 0x5a);
    }
}

/// Assert the fixture's destination frames are clear before a write-side
/// service runs, so bytes found there afterwards can only have arrived through
/// the page walk.
fn assert_umb_frames_clear(machine: &mut Machine) {
    assert_eq!(
        machine.read_physical_u32(UMB_BUFFER_PHYSICAL),
        0,
        "precondition: the mapped frame must be clear"
    );
    assert_eq!(
        machine.read_physical_u32(UMB_FRAME_HIGH),
        0,
        "precondition: the second mapped frame must be clear"
    );
}

/// INT 10h AH=1Bh is the highest-value of the remaining sites: detection code
/// calls it right beside 4F00h to confirm a VGA BIOS, and a DPMI client's
/// transfer buffer is the same UMB for both. The block must reach the frame the
/// caller's page is mapped to.
#[test]
fn int10_state_info_lands_in_a_non_identity_mapped_caller_buffer() {
    let mut machine = umb_paged_machine();
    assert_umb_frames_clear(&mut machine);
    machine.write_physical_u8(0x449, 0x12); // BDA video mode, echoed at block+4

    machine.cpu.registers.set_edi(0);
    machine.cpu.registers.set_eax(0x1b00);
    machine.handle_int10();

    assert_eq!(machine.cpu.registers.eax() as u8, 0x1b);
    assert_eq!(
        read_u16(&mut machine, UMB_BUFFER_PHYSICAL),
        crate::firmware_contract::address::INT10_FUNCTIONALITY_TABLE_OFFSET,
        "the static functionality table pointer must arrive at the mapped frame"
    );
    assert_eq!(
        read_u16(&mut machine, UMB_BUFFER_PHYSICAL + 2),
        crate::firmware_contract::address::VGA_BIOS_SEGMENT
    );
    assert_eq!(
        machine.read_physical_u8(UMB_BUFFER_PHYSICAL + 4),
        0x12,
        "the live video mode must arrive at the mapped frame"
    );
}

/// INT 10h AH=1Ch AL=01h with all three state bits saves more than 900 bytes at
/// ES:BX, which is longer than the distance from the fixture's buffer to the end
/// of its page: the DAC tail belongs to the second frame, which is deliberately
/// not adjacent to the first. One translation of the block's first byte would
/// scatter that tail.
#[test]
fn int10_save_video_state_lands_in_the_non_identity_mapped_caller_buffer() {
    const HARDWARE_LEN: u32 = crate::video_params::INT10_STATE_HARDWARE_LEN as u32;
    const BDA_LEN: u32 = crate::video_params::INT10_STATE_BDA_LEN as u32;

    let mut machine = umb_paged_machine();
    assert_umb_frames_clear(&mut machine);
    prime_dos_int_frame(&mut machine);
    machine.write_physical_u8(0x449, 0x34); // first BDA byte the save copies
    machine.vega.legacy_mut().set_dac_entry(253, 11, 22, 33);

    machine.cpu.registers.set_ebx(0);
    machine.cpu.registers.set_ecx(0x0007);
    machine.cpu.registers.set_eax(0x1c01);
    machine.handle_int10();

    assert_eq!(machine.cpu.registers.eax() as u8, 0x1c);
    assert_eq!(
        machine.read_physical_u8(UMB_BUFFER_PHYSICAL + HARDWARE_LEN),
        0x34,
        "the BDA copy must follow the first page's mapping"
    );
    // DAC block layout: three port bytes, then 256 RGB triples. Entry 253 falls
    // past the end of the caller's first page, in the frame mapped elsewhere.
    let dac_block = UMB_BUFFER_IDENTITY + HARDWARE_LEN + BDA_LEN;
    let entry_253 = dac_block + 3 + 253 * 3;
    assert!(
        entry_253 >= 0x000c_9000,
        "fixture must place entry 253 in the second page, got {entry_253:#x}"
    );
    let tail = UMB_FRAME_HIGH + (entry_253 - 0x000c_9000);
    assert_eq!(
        machine.read_guest_block(tail, 3),
        vec![11, 22, 33],
        "the DAC tail must follow the second page's mapping"
    );
}

/// The read direction of the same service. The saved block is planted in the
/// mapped frame only and the identity range under it is poisoned, so a restore
/// that ignores paging cannot recover the values by accident.
#[test]
fn int10_restore_video_state_reads_the_non_identity_mapped_caller_buffer() {
    const BDA_LEN: usize = crate::video_params::INT10_STATE_BDA_LEN;
    const DAC_LEN: usize = crate::video_params::INT10_STATE_DAC_LEN;

    let mut machine = umb_paged_machine();
    prime_dos_int_frame(&mut machine);

    // CX=0006h: BDA then DAC, skipping the hardware-register block so the
    // fixture drives no CRTC/ATC programming it would then have to model.
    let mut block = vec![0u8; BDA_LEN + DAC_LEN];
    block[0] = 0x56; // BDA 0449h, the live video mode
    block[BDA_LEN + 3] = 0x0a; // DAC entry 0: red
    block[BDA_LEN + 4] = 0x14; // green
    block[BDA_LEN + 5] = 0x1e; // blue
    machine.write_guest_block(UMB_BUFFER_PHYSICAL, &block);
    poison_umb_identity_range(&mut machine, block.len());

    machine.cpu.registers.set_ebx(0);
    machine.cpu.registers.set_ecx(0x0006);
    machine.cpu.registers.set_eax(0x1c02);
    machine.handle_int10();

    assert_eq!(machine.cpu.registers.eax() as u8, 0x1c);
    assert_eq!(
        machine.read_physical_u8(0x449),
        0x56,
        "the BDA block must be restored from the mapped frame"
    );
    assert_eq!(
        machine.video().dac_entry(0),
        [0x0a, 0x14, 0x1e],
        "the DAC block must be restored from the mapped frame"
    );
}

/// INT 10h AH=10h AL=09h and AL=17h read the palette and the DAC out to ES:DX.
#[test]
fn int10_palette_block_reads_land_in_the_non_identity_mapped_caller_buffer() {
    let mut machine = umb_paged_machine();
    assert_umb_frames_clear(&mut machine);
    for index in 0..16u8 {
        machine
            .vega
            .legacy_mut()
            .set_attr_palette_reg(index, index + 0x20);
    }
    machine.vega.legacy_mut().set_overscan(0x39);
    machine.vega.legacy_mut().set_dac_entry(5, 1, 2, 3);
    machine.vega.legacy_mut().set_dac_entry(6, 4, 5, 6);

    // AL=09h: sixteen palette registers plus overscan at ES:DX.
    machine.cpu.registers.set_edx(0);
    machine.cpu.registers.set_eax(0x1009);
    machine.handle_int10();
    let expected: Vec<u8> = (0..16u8).map(|i| i + 0x20).chain([0x39]).collect();
    assert_eq!(
        machine.read_guest_block(UMB_BUFFER_PHYSICAL, 17),
        expected,
        "the palette block must reach the mapped frame"
    );

    // AL=17h: a DAC run at ES:DX, placed elsewhere in the same page.
    machine.cpu.registers.set_edx(0x0100);
    machine.cpu.registers.set_ebx(5);
    machine.cpu.registers.set_ecx(2);
    machine.cpu.registers.set_eax(0x1017);
    machine.handle_int10();
    assert_eq!(
        machine.read_guest_block(UMB_BUFFER_PHYSICAL + 0x100, 6),
        vec![1, 2, 3, 4, 5, 6],
        "the DAC run must reach the mapped frame"
    );
}

/// INT 10h AH=10h AL=02h and AL=12h take the palette and the DAC in from ES:DX.
#[test]
fn int10_palette_block_writes_come_from_the_non_identity_mapped_caller_buffer() {
    let mut machine = umb_paged_machine();

    let mut palette: Vec<u8> = (0..16u8).map(|i| i + 0x11).collect();
    palette.push(0x2f); // overscan
    machine.write_guest_block(UMB_BUFFER_PHYSICAL, &palette);
    machine.write_guest_block(UMB_BUFFER_PHYSICAL + 0x100, &[7, 8, 9, 10, 11, 12]);
    poison_umb_identity_range(&mut machine, 0x106);

    machine.cpu.registers.set_edx(0);
    machine.cpu.registers.set_eax(0x1002);
    machine.handle_int10();
    for index in 0..16u8 {
        assert_eq!(
            machine.video().attr_palette_reg(index),
            index + 0x11,
            "palette register {index} must come from the mapped frame"
        );
    }
    assert_eq!(machine.video().overscan(), 0x2f);

    machine.cpu.registers.set_edx(0x0100);
    machine.cpu.registers.set_ebx(200);
    machine.cpu.registers.set_ecx(2);
    machine.cpu.registers.set_eax(0x1012);
    machine.handle_int10();
    assert_eq!(machine.video().dac_entry(200), [7, 8, 9]);
    assert_eq!(machine.video().dac_entry(201), [10, 11, 12]);
}

/// INT 10h AH=11h AL=10h (user text font) and AL=21h (user graphics font) both
/// take their glyph bytes from ES:BP.
#[test]
fn int10_font_load_reads_the_non_identity_mapped_caller_buffer() {
    let mut machine = umb_paged_machine();

    // AL=10h: one 16-row glyph for 'A'.
    let glyph: Vec<u8> = (0..16u8).map(|row| row | 0x80).collect();
    machine.write_guest_block(UMB_BUFFER_PHYSICAL, &glyph);
    poison_umb_identity_range(&mut machine, 0x200);

    machine.cpu.registers.set_ebp(0);
    machine.cpu.registers.set_ebx(0x1000); // BH=16 bytes/char, BL=0 block
    machine.cpu.registers.set_ecx(1);
    machine.cpu.registers.set_edx(u32::from(b'A'));
    machine.cpu.registers.set_eax(0x1110);
    machine.handle_int10();
    for (row, expected) in glyph.iter().enumerate() {
        assert_eq!(
            machine.video().active_font_glyph_row(b'A', row),
            *expected,
            "text-font row {row} must come from the mapped frame"
        );
    }

    // AL=21h: a one-row-per-character graphics font, 256 bytes at ES:BP.
    let graphics: Vec<u8> = (0..=255u8).collect();
    machine.write_guest_block(UMB_BUFFER_PHYSICAL, &graphics);
    machine.cpu.registers.set_ebp(0);
    machine.cpu.registers.set_ebx(0);
    machine.cpu.registers.set_ecx(1);
    machine.cpu.registers.set_edx(0);
    machine.cpu.registers.set_eax(0x1121);
    machine.handle_int10();
    assert_eq!(
        machine.video().active_font_glyph_row(b'A', 0),
        b'A',
        "graphics-font glyph must come from the mapped frame"
    );
}
