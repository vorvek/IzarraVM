// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn cpu_mode_label_preserves_fractional_clock_rates() {
    assert_eq!(
        cpu_mode_label(GswMode::Gsw386Slow),
        "GSW-586 - 386-slow mode - 7.33 MHz"
    );
    assert_eq!(
        cpu_mode_label(GswMode::Gsw586),
        "GSW-586 - 586 mode - 200 MHz"
    );
}

#[test]
fn volume_gain_is_cubic_and_clamped() {
    // Endpoints are exact: silence at 0, unity at full.
    assert_eq!(volume_gain(0.0), 0.0);
    assert_eq!(volume_gain(1.0), 1.0);
    // Halfway on the slider is 0.5^3 = 0.125 of linear gain.
    assert!((volume_gain(0.5) - 0.125).abs() < 1e-6);
    // 0.8 (the default) cubes to 0.512.
    assert!((volume_gain(0.8) - 0.512).abs() < 1e-6);
    // Out-of-range input is clamped before cubing.
    assert_eq!(volume_gain(-1.0), 0.0);
    assert_eq!(volume_gain(2.0), 1.0);
}

#[test]
fn refill_credit_clamps_a_stall() {
    let cap = MASTER_CLOCK_HZ / 20;
    // From empty, a normal ~15 ms slice yields its full wall-time worth.
    assert_eq!(
        refill_credit(0, Duration::from_millis(15), cap),
        (MASTER_CLOCK_HZ * 15 / 1000) as i64
    );
    // A long stall is clamped to the cap, so the backlog is forgiven, not banked.
    assert_eq!(
        refill_credit(0, Duration::from_millis(500), cap),
        cap as i64
    );
}

#[test]
fn disk_overshoot_holds_the_guest() {
    let cap = MASTER_CLOCK_HZ / 20;
    // A read that ran ~190 ms past its budget leaves credit deep in debt.
    let mut credit: i64 = -(MASTER_CLOCK_HZ as i64) / 5;
    // One short slice cannot lift it out of debt, so the guest's budget stays
    // zero: it waits in wall-clock time.
    credit = refill_credit(credit, Duration::from_millis(1), cap);
    assert!(credit < 0);
    assert_eq!(credit.max(0) as u64, 0, "no budget while in disk debt");
    // After enough wall time the debt clears and the guest runs again.
    credit = refill_credit(credit, Duration::from_millis(500), cap);
    assert!(credit > 0, "debt repaid once wall-clock catches up");
}

#[test]
fn top_up_escape_hatch_caps_edge_stops_for_a_pathological_crtc() {
    // A guest-programmed CRTC with kHz-rate frames would put hundreds of
    // vretrace edges inside one slice's shortfall; the defensive cap must
    // bound the peek work and consume the remainder unclamped instead of
    // livelocking the emulate thread.
    // ROM: reset far-jumps to F000:0000 which spins forever (jmp $), so the
    // peeks execute real instructions and never stop early.
    let mut rom = vec![0u8; izarravm_machine::BIOS_ROM_SIZE];
    rom[0] = 0xEB; // jmp $
    rom[1] = 0xFE;
    rom[0xFFF0..0xFFF5].copy_from_slice(&[0xEA, 0x00, 0x00, 0x00, 0xF0]);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(1, izarravm_core::VideoCard::Vega),
        rom,
    )
    .unwrap();
    // Mode 13h, then shrink the frame to 20 scanlines (~0.64ms, ~1570
    // edges/s): unprotect + vretrace end 14, overflow 0, vtotal 18+2,
    // vdisp end 9+1, vretrace start 12.
    let vga = machine.video_mut();
    assert!(vga.set_mode(0x13));
    for (index, value) in [
        (0x11u8, 0x0Eu8),
        (0x07, 0x00),
        (0x06, 18),
        (0x12, 9),
        (0x10, 12),
    ] {
        vga.write_port(0x3D4, index);
        vga.write_port(0x3D5, value);
    }
    let asked = MASTER_CLOCK_HZ / 10; // 100ms of shortfall: ~157 edges, cap is 12
    let work_before = machine.elapsed_clocks();
    let ticks_before = machine.master_ticks();
    let top_up = top_up_shortfall(&mut machine, asked);
    assert_eq!(
        top_up.topped_up_ticks, asked,
        "the escape hatch must still consume the full shortfall"
    );
    assert_eq!(
        machine.elapsed_clocks() - work_before,
        machine
            .active_mode()
            .clock_rate()
            .clocks_for_master_ticks_floor(top_up.peeked_ticks),
        "only executed peeks count as CPU work"
    );
    assert_eq!(
        machine.master_ticks() - ticks_before,
        asked + top_up.peeked_ticks,
        "the timeline advances by the shortfall plus executed peeks"
    );
    // The cap allows asked/10ms + 2 = 12 stops; each peek runs about
    // VRETRACE_PEEK_CLOCKS (allow 2x for run-loop batch overshoot). An
    // uncapped loop would have peeked ~157 times.
    let max_stops = asked / (MASTER_CLOCK_HZ / 100) + 2;
    let max_peek_ticks = machine
        .active_mode()
        .clock_rate()
        .master_ticks_for_clocks_ceil(max_stops * VRETRACE_PEEK_CLOCKS * 2);
    assert!(
        top_up.peeked_ticks <= max_peek_ticks,
        "peek work must be bounded by the stop cap (peeked {} over {} stops max)",
        top_up.peeked_ticks,
        max_stops
    );
}

#[test]
fn top_up_aborts_when_the_peek_halts() {
    // ROM: reset far-jumps to F000:0000 which runs CLI; HLT, so the very
    // first vretrace-edge peek halts the guest. The top-up must break
    // instead of continuing to create time against a halted machine (the
    // next slice's own Halted handling takes over).
    let mut rom = vec![0u8; izarravm_machine::BIOS_ROM_SIZE];
    rom[0] = 0xFA; // cli
    rom[1] = 0xF4; // hlt
    rom[0xFFF0..0xFFF5].copy_from_slice(&[0xEA, 0x00, 0x00, 0x00, 0xF0]);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(1, izarravm_core::VideoCard::Vega),
        rom,
    )
    .unwrap();
    let asked = MASTER_CLOCK_HZ; // one guest second: dozens of frames
    let top_up = top_up_shortfall(&mut machine, asked);
    assert!(
        top_up.topped_up_ticks < asked / 2,
        "a halted peek must abort the top-up (topped up {} of {asked})",
        top_up.topped_up_ticks
    );
    assert!(
        top_up.peeked_ticks > 0,
        "the aborting peek itself executed guest time"
    );
    assert!(
        matches!(tick_machine(&mut machine, 1_000), Some(StopReason::Halted)),
        "the machine is halted after the aborted top-up"
    );
}

#[test]
fn live_mode_switch_debits_credit_in_master_ticks() {
    let mut rom = vec![0u8; izarravm_machine::BIOS_ROM_SIZE];
    rom[..15].copy_from_slice(&[
        0xB0, 0x01, 0xE6, 0xE1, // 486
        0xB0, 0x00, 0xE6, 0xE1, // 386
        0xB0, 0x03, 0xE6, 0xE1, // 386-slow
        0xFA, 0xEB, 0xFE, // cli; jmp $
    ]);
    rom[0xFFF0..0xFFF5].copy_from_slice(&[0xEA, 0x00, 0x00, 0x00, 0xF0]);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(1, izarravm_core::VideoCard::Vega),
        rom,
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw586);
    let budget = MASTER_CLOCK_HZ / 1000;
    let mut credit = budget as i64;
    let before = machine.master_ticks();

    assert_eq!(tick_machine_ticks(&mut machine, budget), None);
    let ran = machine.master_ticks() - before;
    credit -= i64::try_from(ran).unwrap();

    assert_eq!(machine.active_mode(), GswMode::Gsw386Slow);
    assert!(credit <= 0, "the full fixed-time budget was debited");
    assert!(
        credit > -(100 * 900),
        "credit debt is only final-instruction overshoot, not mixed clock units"
    );
}

#[test]
fn logo_recolor_maps_background_to_beige_and_keeps_ink() {
    // One pure-background pixel and one pure-black-ink pixel, both opaque.
    let raw = [236u8, 230, 223, 255, 0, 0, 0, 255];
    let out = recolor_logo(&raw, PANEL_FACE_F32);
    // Background becomes the exact panel beige.
    assert_eq!(&out[0..4], &[205u8, 195, 164, 255]);
    // Ink is untouched (background coverage is zero).
    assert_eq!(&out[4..8], &[0u8, 0, 0, 255]);
}

#[test]
fn palette_maps_indices_to_words() {
    let pixels = [0u8, 1, 0, 1];
    let mut palette = [0u32; 256];
    palette[1] = 0x00AB_CDEF;
    let words = palette_words(&pixels, &palette);
    assert_eq!(words.len(), 4);
    assert_eq!(words[1], 0x00AB_CDEF);
    let rgba = words_to_rgba(&words, 2, 2);
    assert_eq!(rgba.len(), 16);
    // Pixel 1 is 0x00ABCDEF -> R=AB, G=CD, B=EF, A=FF.
    assert_eq!(
        (rgba[4], rgba[5], rgba[6], rgba[7]),
        (0xAB, 0xCD, 0xEF, 0xFF)
    );
}

#[test]
fn star_icon_is_red_in_the_centre_and_clear_in_the_corner() {
    let size = 64u32;
    let rgba = render_star_icon(size, [0xC7, 0x44, 0x46]);
    assert_eq!(rgba.len(), (size * size * 4) as usize);
    let center = ((size / 2 * size + size / 2) * 4) as usize;
    assert_eq!(&rgba[center..center + 4], &[0xC7u8, 0x44, 0x46, 0xFF]);
    // Top-left corner is outside the star, fully transparent.
    assert_eq!(&rgba[0..4], &[0u8, 0, 0, 0]);
}
