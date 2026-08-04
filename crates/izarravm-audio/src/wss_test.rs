// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

// Direct-port offsets (codec = base+4, so config=0..3, codec regs=4..7).
const R0_INDEX: u16 = 4;
const R1_DATA: u16 = 5;
const R2_STATUS: u16 = 6;

/// Write an indirect register via R0 (index) + R1 (data).
fn write_indirect(dev: &mut Ad1848, index: u8, value: u8) {
    dev.write_port(R0_INDEX, index);
    dev.write_port(R1_DATA, value);
}

/// Read an indirect register via R0 + R1.
fn read_indirect(dev: &mut Ad1848, index: u8) -> u8 {
    dev.write_port(R0_INDEX, index);
    dev.read_port(R1_DATA)
}

#[test]
fn r0_latches_index_and_mce_trd_bits() {
    let mut dev = Ad1848::default();
    // MCE | TRD | index 8.
    dev.write_port(R0_INDEX, R0_MCE | R0_TRD | 0x08);
    let v = dev.read_port(R0_INDEX);
    assert_eq!(v & R0_INDEX_MASK, 0x08, "index latched");
    assert_ne!(v & R0_MCE, 0, "MCE latched");
    assert_ne!(v & R0_TRD, 0, "TRD latched");
    assert_eq!(v & R0_INIT, 0, "INIT reads clear (always ready)");
}

#[test]
fn indirect_register_round_trips_via_r0_r1() {
    let mut dev = Ad1848::default();
    // I0 (Left Input Control) is a plain stored register.
    write_indirect(&mut dev, 0, 0x5A);
    assert_eq!(read_indirect(&mut dev, 0), 0x5A);
    // I13 (Digital Mix) also round-trips.
    write_indirect(&mut dev, 13, 0xA5);
    assert_eq!(read_indirect(&mut dev, 13), 0xA5);
}

#[test]
fn i8_write_is_gated_by_mce() {
    let mut dev = Ad1848::default();
    // Without MCE the format write is ignored.
    write_indirect(&mut dev, IDX_FORMAT as u8, 0x40);
    assert_eq!(
        read_indirect(&mut dev, IDX_FORMAT as u8),
        0x00,
        "I8 inert without MCE"
    );
    // Set MCE via R0, then the write sticks.
    dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
    dev.write_port(R1_DATA, 0x40);
    assert_eq!(
        dev.read_port(R1_DATA) & I8_FMT,
        I8_FMT,
        "I8 honored under MCE"
    );
}

#[test]
fn clearing_mce_asserts_then_clears_aci_across_autocal_window() {
    let mut dev = Ad1848::default();
    // Enter MCE, change I8, then clear MCE.
    dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
    dev.write_port(R1_DATA, 0x40);
    // Clear MCE (index stays 11 to poll ACI).
    dev.write_port(R0_INDEX, IDX_TEST_INIT as u8);
    // ACI asserts immediately on MCE exit.
    assert_ne!(dev.read_port(R1_DATA) & I11_ACI, 0, "ACI set on MCE exit");
    // Drive the autocal countdown directly. ACI counts output sample periods;
    // the per-frame coupling (advance_autocal alongside tick_sample) is
    // verified at the machine-integration layer, out of scope for this core.
    for _ in 0..(AUTOCAL_SAMPLES - 1) {
        dev.advance_autocal();
    }
    assert_ne!(
        dev.read_port(R1_DATA) & I11_ACI,
        0,
        "ACI still set just before window end"
    );
    dev.advance_autocal();
    assert_eq!(
        dev.read_port(R1_DATA) & I11_ACI,
        0,
        "ACI clears after the autocal window"
    );
}

#[test]
fn rate_table_decodes_representative_freq_crystal_combos() {
    let mut dev = Ad1848::default();
    let set_i8 = |dev: &mut Ad1848, cfs: u8, css: u8| {
        let v = ((cfs & 0x07) << I8_CFS_SHIFT) | (css & 1);
        dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
        dev.write_port(R1_DATA, v);
    };
    // XTAL1 (CSS=0): CFS0 -> 8000, CFS6 -> 48000, CFS7 -> 9600.
    set_i8(&mut dev, 0, 0);
    assert_eq!(dev.rate_hz(), 8000);
    set_i8(&mut dev, 6, 0);
    assert_eq!(dev.rate_hz(), 48000);
    set_i8(&mut dev, 7, 0);
    assert_eq!(dev.rate_hz(), 9600);
    // XTAL2 (CSS=1): CFS0 -> 5512, CFS5 -> 44100, CFS3 -> 22050.
    set_i8(&mut dev, 0, 1);
    assert_eq!(dev.rate_hz(), 5512);
    set_i8(&mut dev, 5, 1);
    assert_eq!(dev.rate_hz(), 44100);
    set_i8(&mut dev, 3, 1);
    assert_eq!(dev.rate_hz(), 22050);
    // XTAL1 CFS4/CFS5 are "Not Supported" -> 0.
    set_i8(&mut dev, 4, 0);
    assert_eq!(dev.rate_hz(), 0, "XTAL1 CFS4 unsupported");
}

/// Arm playback: 8-bit mono, base count `count`, PEN set.
fn arm_8bit_mono(dev: &mut Ad1848, count: u16) {
    // I8 = 8-bit unsigned PCM, mono (all format bits clear), needs MCE.
    dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
    dev.write_port(R1_DATA, 0x00);
    dev.write_port(R0_INDEX, IDX_FORMAT as u8); // clear MCE
    // Enable the external INT pin (IEN) so terminal-count IRQs forward.
    write_indirect(dev, IDX_PIN_CONTROL as u8, I10_IEN);
    write_indirect(dev, IDX_LOWER_COUNT as u8, (count & 0xFF) as u8);
    write_indirect(dev, IDX_UPPER_COUNT as u8, (count >> 8) as u8);
    write_indirect(dev, IDX_IFACE_CONFIG as u8, I9_ACAL | I9_PEN);
    // Unmute both DACs at 0 dB so the render values pass through.
    write_indirect(dev, IDX_LEFT_DAC as u8, 0x00);
    write_indirect(dev, IDX_RIGHT_DAC as u8, 0x00);
}

#[test]
fn format_8bit_unsigned_decodes_and_duplicates_mono() {
    let mut dev = Ad1848::default();
    arm_8bit_mono(&mut dev, 4);
    // 0x80 -> silence on both channels.
    let f = dev.render_frame(|| Some(0x80));
    assert_eq!(f, Some((0, 0)));
    let f = dev.render_frame(|| Some(0xFF));
    assert_eq!(
        f,
        Some((32_512, 32_512)),
        "0xFF near full positive, mono dup"
    );
}

#[test]
fn format_mulaw_and_alaw_known_points() {
    // mu-law: arm 8-bit companded mu-law (L/C set, FMT clear), mono.
    let mut dev = Ad1848::default();
    dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
    dev.write_port(R1_DATA, I8_LC); // companded + mu-law
    dev.write_port(R0_INDEX, IDX_FORMAT as u8);
    write_indirect(&mut dev, IDX_LOWER_COUNT as u8, 0x10);
    write_indirect(&mut dev, IDX_UPPER_COUNT as u8, 0x00);
    write_indirect(&mut dev, IDX_IFACE_CONFIG as u8, I9_ACAL | I9_PEN);
    write_indirect(&mut dev, IDX_LEFT_DAC as u8, 0x00);
    write_indirect(&mut dev, IDX_RIGHT_DAC as u8, 0x00);
    // mu-law 0xFF is digital silence.
    assert_eq!(
        dev.render_frame(|| Some(0xFF)),
        Some((0, 0)),
        "mu-law 0xFF -> 0"
    );

    // A-law: FMT set + L/C set.
    let mut dev = Ad1848::default();
    dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
    dev.write_port(R1_DATA, I8_LC | I8_FMT);
    dev.write_port(R0_INDEX, IDX_FORMAT as u8);
    write_indirect(&mut dev, IDX_LOWER_COUNT as u8, 0x10);
    write_indirect(&mut dev, IDX_UPPER_COUNT as u8, 0x00);
    write_indirect(&mut dev, IDX_IFACE_CONFIG as u8, I9_ACAL | I9_PEN);
    write_indirect(&mut dev, IDX_LEFT_DAC as u8, 0x00);
    write_indirect(&mut dev, IDX_RIGHT_DAC as u8, 0x00);
    // A-law full-scale positive = 0xAA.
    assert_eq!(
        dev.render_frame(|| Some(0xAA)),
        Some((32_256, 32_256)),
        "A-law 0xAA full scale"
    );
}

#[test]
fn format_16bit_assembles_two_bytes_le_and_orders_stereo() {
    let mut dev = Ad1848::default();
    // I8 = 16-bit linear PCM (FMT set), stereo (S/M set).
    dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
    dev.write_port(R1_DATA, I8_FMT | I8_SM);
    dev.write_port(R0_INDEX, IDX_FORMAT as u8);
    // base count 8 bytes = 2 stereo frames (4 bytes each).
    write_indirect(&mut dev, IDX_LOWER_COUNT as u8, 8);
    write_indirect(&mut dev, IDX_UPPER_COUNT as u8, 0);
    write_indirect(&mut dev, IDX_IFACE_CONFIG as u8, I9_ACAL | I9_PEN);
    write_indirect(&mut dev, IDX_LEFT_DAC as u8, 0x00);
    write_indirect(&mut dev, IDX_RIGHT_DAC as u8, 0x00);
    // Stream: L = 0x0001 (lo=01, hi=00), R = 0xFFFE (lo=FE, hi=FF).
    let bytes = [0x01u8, 0x00, 0xFE, 0xFF];
    let mut i = 0;
    let f = dev.render_frame(|| {
        let b = bytes[i % bytes.len()];
        i += 1;
        Some(b)
    });
    assert_eq!(f, Some((1, -2)), "LE assembly + left-before-right");
}

#[test]
fn base_count_terminal_sets_int_raises_irq_and_auto_reloads() {
    let mut dev = Ad1848::default();
    // 8-bit mono, base count 4. Datasheet: the counter decrements each sample
    // period until zero, and the NEXT period after zero underflows and fires
    // the interrupt -> INT after N+1 = 5 frames, then every 5 thereafter.
    arm_8bit_mono(&mut dev, 4);
    let mut irqs = Vec::new();
    for i in 1..=10 {
        let _ = dev.render_frame(|| Some(0x80));
        if dev.take_irq() {
            irqs.push(i);
        }
    }
    // Underflow at frame 5 (INT + reload), again at frame 10 (5 + 5).
    assert_eq!(irqs, vec![5, 10], "INT/IRQ at each underflow (N+1 cadence)");
    assert_ne!(dev.status() & R2_INT, 0, "Status INT sticky after TC");
    assert!(dev.is_playing(), "auto-reload keeps playback armed");
    // Frame 10 reloaded count to base (4); no further decrement this loop.
    assert_eq!(dev.current_count(), 4, "count reloaded from base");
}

#[test]
fn writing_status_clears_int() {
    let mut dev = Ad1848::default();
    // Base count 1 -> underflow after N+1 = 2 sample periods.
    arm_8bit_mono(&mut dev, 1);
    let _ = dev.render_frame(|| Some(0x80)); // count 1 -> 0, no INT yet
    assert_eq!(dev.status() & R2_INT, 0, "no INT before underflow");
    let _ = dev.render_frame(|| Some(0x80)); // underflow -> INT set
    assert_ne!(dev.status() & R2_INT, 0, "INT set at TC");
    dev.write_port(R2_STATUS, 0x00); // any write to R2 acks INT
    assert_eq!(dev.status() & R2_INT, 0, "INT cleared by Status write");
}

#[test]
fn i12_revision_reads_k_grade_pattern() {
    let mut dev = Ad1848::default();
    let rev = read_indirect(&mut dev, IDX_MISC_INFO as u8);
    assert_eq!(rev & 0x0F, 0b1010, "I12 ID3:0 = K-grade 1010");
}

#[test]
fn config_region_reports_id_version_and_irq_dma_jumpers() {
    let mut dev = Ad1848::new(Ad1848Config { irq: 7, dma: 0 });
    assert_eq!(dev.read_port(0), 0x04, "config region board/version ID");
    // High nibble IRQ, low nibble DMA (IRQ7, DMA0 -> 0x70).
    assert_eq!(dev.read_port(1), 0x70, "IRQ/DMA jumper readback");
    dev.set_config(Ad1848Config { irq: 9, dma: 3 });
    assert_eq!(dev.read_port(1), (9 << 4) | 3, "config setter reflected");
}

#[test]
fn writing_the_config_region_repoints_resources_and_reinitialises_the_codec() {
    // ReSonique 2's config register is writable, so a guest can move the codec
    // without restarting the machine. Selecting resources must also quiesce it:
    // a transfer left running would keep driving the channel the board just gave
    // up. The board ID and the mirror offsets stay read-only.
    let mut dev = Ad1848::new(Ad1848Config { irq: 11, dma: 0 });
    assert_eq!(dev.read_port(1), 0xB0, "IRQ11/DMA0 readback");

    // Arm a transfer so there is something live to interrupt.
    write_indirect(&mut dev, IDX_LOWER_COUNT as u8, 8);
    write_indirect(&mut dev, IDX_IFACE_CONFIG as u8, I9_ACAL | I9_PEN);
    assert!(dev.is_playing(), "codec armed before the re-point");

    dev.write_port(1, (7 << 4) | 3);
    assert_eq!(dev.read_port(1), (7 << 4) | 3, "write selects IRQ7/DMA3");
    assert!(
        !dev.is_playing(),
        "re-pointing re-initialises: playback stopped"
    );
    assert_eq!(dev.status() & 1, 0, "sticky INT cleared by the re-init");

    let id = dev.read_port(0);
    dev.write_port(0, 0xFF);
    assert_eq!(dev.read_port(0), id, "board ID stays read-only");
}

#[test]
fn pen_arms_only_with_nonzero_base_count() {
    let mut dev = Ad1848::default();
    // PEN set but base count still zero -> not armed.
    write_indirect(&mut dev, IDX_IFACE_CONFIG as u8, I9_ACAL | I9_PEN);
    assert!(!dev.is_playing(), "PEN without count does not arm");
    // Now load a count; arming re-evaluates on the upper-byte write.
    write_indirect(&mut dev, IDX_LOWER_COUNT as u8, 4);
    write_indirect(&mut dev, IDX_UPPER_COUNT as u8, 0);
    assert!(dev.is_playing(), "count + PEN arms playback");
    assert_eq!(dev.current_count(), 4);
}

#[test]
fn dac_mute_silences_and_attenuation_scales() {
    let mut dev = Ad1848::default();
    arm_8bit_mono(&mut dev, 8);
    // Mute the right DAC; left at 0 dB.
    write_indirect(&mut dev, IDX_LEFT_DAC as u8, 0x00);
    write_indirect(&mut dev, IDX_RIGHT_DAC as u8, DAC_MUTE);
    let f = dev.render_frame(|| Some(0xFF)).unwrap();
    assert_eq!(f.0, 32_512, "left passes at 0 dB");
    assert_eq!(f.1, 0, "right muted");
}

/// Arm playback in an arbitrary I8 format byte (MCE-gated write), base count,
/// PEN set, both DACs unmuted at 0 dB.
fn arm_format(dev: &mut Ad1848, i8_format: u8, count: u16) {
    dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
    dev.write_port(R1_DATA, i8_format);
    dev.write_port(R0_INDEX, IDX_FORMAT as u8); // clear MCE
    // Enable the external INT pin (IEN) so terminal-count IRQs forward.
    write_indirect(dev, IDX_PIN_CONTROL as u8, I10_IEN);
    write_indirect(dev, IDX_LOWER_COUNT as u8, (count & 0xFF) as u8);
    write_indirect(dev, IDX_UPPER_COUNT as u8, (count >> 8) as u8);
    write_indirect(dev, IDX_IFACE_CONFIG as u8, I9_ACAL | I9_PEN);
    write_indirect(dev, IDX_LEFT_DAC as u8, 0x00);
    write_indirect(dev, IDX_RIGHT_DAC as u8, 0x00);
}

#[test]
fn count_is_in_sample_periods_not_bytes_for_16bit_stereo() {
    // 16-bit stereo consumes 4 bytes per frame but the Current Count is in
    // sample periods: base count N must fire after N+1 *frames*, independent
    // of width/channels. Base count 3 -> underflow at frame 4, then 8.
    let mut dev = Ad1848::default();
    arm_format(&mut dev, I8_FMT | I8_SM, 3);
    let mut irqs = Vec::new();
    for i in 1..=8 {
        // Each frame pulls 4 bytes (L lo/hi, R lo/hi); value is irrelevant.
        let _ = dev.render_frame(|| Some(0x00));
        if dev.take_irq() {
            irqs.push(i);
        }
    }
    assert_eq!(
        irqs,
        vec![4, 8],
        "16-bit stereo: INT counts sample periods (N+1=4), not bytes"
    );
}

#[test]
fn count_terminal_even_and_odd_base_16bit() {
    // Odd base count cannot overshoot zero now that the count decrements by
    // one sample period per frame. Even base behaves identically.
    for base in [4u16, 5u16] {
        let mut dev = Ad1848::default();
        arm_format(&mut dev, I8_FMT, base); // 16-bit mono (2 bytes/frame)
        let mut first_irq = None;
        for i in 1..=(2 * (base as u32 + 1)) {
            let _ = dev.render_frame(|| Some(0x00));
            if dev.take_irq() && first_irq.is_none() {
                first_irq = Some(i);
            }
        }
        assert_eq!(
            first_irq,
            Some(base as u32 + 1),
            "16-bit base {base}: INT at frame N+1 exactly"
        );
        assert_eq!(
            dev.current_count(),
            base as u32,
            "post-reload count equals base {base}"
        );
    }
}

#[test]
fn stereo_8bit_orders_left_before_right_and_counts_one_period() {
    // 8-bit stereo: distinct L/R bytes confirm channel order and that one
    // sample period (not two bytes) is consumed per frame.
    let mut dev = Ad1848::default();
    arm_format(&mut dev, I8_SM, 4); // 8-bit linear, stereo
    let before = dev.current_count();
    // L = 0xFF (near +full), R = 0x00 (full negative).
    let bytes = [0xFFu8, 0x00];
    let mut i = 0;
    let f = dev.render_frame(|| {
        let b = bytes[i % bytes.len()];
        i += 1;
        Some(b)
    });
    assert_eq!(f, Some((32_512, -32_768)), "8-bit stereo: left then right");
    assert_eq!(
        before - dev.current_count(),
        1,
        "one sample period consumed per stereo frame"
    );
}

#[test]
fn rate_table_decodes_every_cfs_css_cell() {
    // Data-driven over all 16 (cfs, css) combinations so every table cell --
    // including both XTAL1 "Not Supported" codes (CFS4 and CFS5) -- is pinned.
    const XTAL1: [u32; 8] = [8000, 16000, 27429, 32000, 0, 0, 48000, 9600];
    const XTAL2: [u32; 8] = [5512, 11025, 18900, 22050, 37800, 44100, 33075, 6615];
    let mut dev = Ad1848::default();
    for css in 0u8..=1 {
        for cfs in 0u8..=7 {
            let v = (cfs << I8_CFS_SHIFT) | css;
            dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
            dev.write_port(R1_DATA, v);
            let expected = if css == 0 {
                XTAL1[cfs as usize]
            } else {
                XTAL2[cfs as usize]
            };
            assert_eq!(dev.rate_hz(), expected, "rate cell cfs={cfs} css={css}");
        }
    }
}

#[test]
fn attenuation_follows_log_curve_with_sign_mask_and_mute() {
    // apply_atten selects a -1.5 dB-per-step logarithmic gain (10^(-1.5n/20)).
    // Step 0 is unity, so the input passes through unchanged.
    assert_eq!(apply_atten(1000, 0), 1000, "step 0 is unity gain");
    // Step 10 -> 10^(-15/20) = 0.17783: 1000 * 0.17783 ~= 178 (round).
    let n10 = apply_atten(1000, 10);
    let expected_10 = (1000.0 * 10f32.powf(-15.0 / 20.0)).round() as i16;
    assert_eq!(n10, expected_10, "step 10 matches the log law");
    assert!((n10 - 178).abs() <= 1, "step 10 ~= input * 0.1778 ({n10})");
    // The curve decreases monotonically across the 64 steps.
    let mut prev = apply_atten(30_000, 0);
    for n in 1u8..64 {
        let cur = apply_atten(30_000, n);
        assert!(cur <= prev, "step {n} must not be louder than {}", n - 1);
        prev = cur;
    }
    // Negative input keeps its sign under attenuation.
    assert_eq!(apply_atten(-1000, 10), -n10, "negative input keeps sign");
    // Mask: only the low 6 bits select attenuation; bit6 (0x40) is ignored.
    assert_eq!(
        apply_atten(1000, 0x40 | 10),
        apply_atten(1000, 10),
        "atten field is masked to 6 bits"
    );
    // Mute (bit7) silences the channel regardless of the attenuate field.
    assert_eq!(apply_atten(1000, DAC_MUTE | 10), 0, "mute -> 0");
}

#[test]
fn i9_nonmce_write_passes_pen_but_preserves_acal_sdc() {
    let mut dev = Ad1848::default();
    // Set ACAL (and SDC) under MCE so they are latched in I9.
    dev.write_port(R0_INDEX, R0_MCE | IDX_IFACE_CONFIG as u8);
    dev.write_port(R1_DATA, I9_ACAL | I9_SDC);
    // Now without MCE, write I9 with PEN set and ACAL/SDC clear in the value.
    dev.write_port(R0_INDEX, IDX_IFACE_CONFIG as u8); // clears MCE
    dev.write_port(R1_DATA, I9_PEN);
    let i9 = read_indirect(&mut dev, IDX_IFACE_CONFIG as u8);
    assert_ne!(i9 & I9_ACAL, 0, "ACAL preserved across non-MCE I9 write");
    assert_ne!(i9 & I9_SDC, 0, "SDC preserved across non-MCE I9 write");
    assert_ne!(i9 & I9_PEN, 0, "PEN took effect on-the-fly");
}

#[test]
fn r0_reads_40h_mce_set_after_reset() {
    // Datasheet: R0 reads "0100 0000 (40h)" once the codec leaves INIT.
    let dev = Ad1848::default();
    assert_eq!(dev.read_index(), R0_MCE, "post-reset R0 = 0x40 (MCE set)");
}

#[test]
fn drain_frame_pops_pushed_frames() {
    let mut dev = Ad1848::default();
    arm_8bit_mono(&mut dev, 8);
    dev.tick_sample(|| Some(0xFF));
    dev.tick_sample(|| Some(0x80));
    assert_eq!(dev.drain_frame(), Some((32_512, 32_512)));
    assert_eq!(dev.drain_frame(), Some((0, 0)));
    assert_eq!(dev.drain_frame(), None, "ring drained");
}

#[test]
fn tick_n_samples_counts_complete_multibyte_frames_and_stops_dry() {
    let mut dev = Ad1848::default();
    arm_format(&mut dev, I8_FMT | I8_SM, 4); // 16-bit stereo, 4 bytes per frame
    assert_eq!(dev.bytes_per_frame(), 4);
    let before = dev.current_count();
    let mut bytes = [
        0x01, 0x00, 0xFE, 0xFF, // L=1, R=-2
        0x02, 0x00, 0xFD, 0xFF, // L=2, R=-3
    ]
    .into_iter();

    let produced = dev.tick_n_samples(5, || bytes.next());

    assert_eq!(produced, 2);
    assert_eq!(dev.current_count(), before - 2);
    assert_eq!(dev.drain_frame(), Some((1, -2)));
    assert_eq!(dev.drain_frame(), Some((2, -3)));
    assert_eq!(dev.drain_frame(), None);
}

#[test]
fn tick_sample_returns_false_without_a_complete_frame() {
    let mut dev = Ad1848::default();
    arm_format(&mut dev, I8_FMT | I8_SM, 4);
    let before = dev.current_count();

    assert!(!dev.tick_sample(|| None));
    assert_eq!(dev.current_count(), before);
    assert_eq!(dev.drain_frame(), None);
}

#[test]
fn ien_clear_sets_status_int_but_does_not_forward_irq_pin() {
    // Datasheet: the internal INT status bit becomes one on underflow even
    // when IEN is zero, but the external INT pin (the PIC forward) stays
    // inactive. Arm playback WITHOUT setting I10 IEN.
    let mut dev = Ad1848::default();
    dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
    dev.write_port(R1_DATA, 0x00); // 8-bit mono
    dev.write_port(R0_INDEX, IDX_FORMAT as u8); // clear MCE
    // Deliberately leave I10 (Pin Control) IEN clear.
    write_indirect(&mut dev, IDX_LOWER_COUNT as u8, 1);
    write_indirect(&mut dev, IDX_UPPER_COUNT as u8, 0);
    write_indirect(&mut dev, IDX_IFACE_CONFIG as u8, I9_ACAL | I9_PEN);
    write_indirect(&mut dev, IDX_LEFT_DAC as u8, 0x00);
    write_indirect(&mut dev, IDX_RIGHT_DAC as u8, 0x00);
    // Base count 1 -> underflow after N+1 = 2 sample periods.
    let _ = dev.render_frame(|| Some(0x80)); // count 1 -> 0
    let _ = dev.render_frame(|| Some(0x80)); // underflow
    assert_ne!(
        dev.status() & R2_INT,
        0,
        "internal INT status set on underflow regardless of IEN"
    );
    assert!(
        !dev.take_irq(),
        "external INT pin not forwarded while IEN clear"
    );
}

#[test]
fn trd_holds_count_until_int_acked() {
    // Datasheet: the Current Count Register does not decrement while both TRD
    // and the sticky INT bit are set. Arm with TRD set; after the underflow
    // sets INT the count must hold (no back-to-back re-underflow) until the
    // host acks INT via an R2 write.
    let mut dev = Ad1848::default();
    arm_8bit_mono(&mut dev, 1);
    // Latch TRD via a final R0 write (TRD is set with the index on R0 writes;
    // the subsequent renders/reads never touch R0, so the latch persists).
    dev.write_port(R0_INDEX, R0_TRD);
    // base 1 -> underflow on the 2nd frame, count then reloads to 1.
    let _ = dev.render_frame(|| Some(0x80)); // count 1 -> 0
    let _ = dev.render_frame(|| Some(0x80)); // underflow: INT set, reload 1
    assert!(dev.take_irq(), "first underflow forwards the IRQ edge");
    let held = dev.current_count();
    // Further frames must NOT decrement or re-underflow while TRD+INT hold.
    for _ in 0..5 {
        let _ = dev.render_frame(|| Some(0x80));
        assert_eq!(dev.current_count(), held, "count holds while TRD && INT");
        assert!(!dev.take_irq(), "no further IRQ while count is held");
    }
    // Ack INT (R2 write) -> transfers resume, count decrements again.
    dev.write_port(R2_STATUS, 0x00);
    let _ = dev.render_frame(|| Some(0x80));
    assert_eq!(
        dev.current_count(),
        held - 1,
        "count resumes decrementing once INT is acked"
    );
}

#[test]
fn dma_underrun_midframe_does_not_advance_count_or_set_int() {
    // 16-bit mono: a frame pulls lo then hi. A fetch that yields the lo byte
    // then None must drop the frame WITHOUT advancing the count, setting INT,
    // or latching an IRQ.
    let mut dev = Ad1848::default();
    arm_format(&mut dev, I8_FMT, 4); // 16-bit mono
    let before = dev.current_count();
    let mut calls = 0;
    let frame = dev.render_frame(|| {
        calls += 1;
        if calls == 1 { Some(0x34) } else { None }
    });
    assert_eq!(frame, None, "partial 16-bit frame dropped");
    assert_eq!(
        dev.current_count(),
        before,
        "count unchanged on mid-frame underrun"
    );
    assert_eq!(dev.status() & R2_INT, 0, "no INT on underrun");
    assert!(!dev.take_irq(), "no IRQ on underrun");

    // 8-bit mono: the very first fetch returns None.
    let mut dev = Ad1848::default();
    arm_8bit_mono(&mut dev, 4);
    let before = dev.current_count();
    let frame = dev.render_frame(|| None);
    assert_eq!(frame, None, "8-bit dry fetch yields no frame");
    assert_eq!(dev.current_count(), before, "count unchanged on dry fetch");
    assert_eq!(dev.status() & R2_INT, 0, "no INT on dry fetch");
    assert!(!dev.take_irq(), "no IRQ on dry fetch");
}

#[test]
fn take_irq_is_one_shot_edge_independent_of_sticky_status() {
    // The sticky Status INT bit (acked by an R2 write) and the irq_pending
    // edge (consumed by take_irq for the PIC forward) are independent.
    let mut dev = Ad1848::default();
    arm_8bit_mono(&mut dev, 1); // underflow after 2 frames
    let _ = dev.render_frame(|| Some(0x80)); // count 1 -> 0
    let _ = dev.render_frame(|| Some(0x80)); // underflow
    assert_ne!(dev.status() & R2_INT, 0, "Status INT set at underflow");
    // take_irq is a one-shot edge: true once, then false, while INT stays set.
    assert!(dev.take_irq(), "first take_irq returns the edge");
    assert!(!dev.take_irq(), "edge is one-shot");
    assert_ne!(
        dev.status() & R2_INT,
        0,
        "Status INT still sticky after take_irq"
    );
    // Acking via an R2 write clears Status INT but does not by itself fire a
    // new edge; a fresh underflow latches a new independent edge.
    dev.write_port(R2_STATUS, 0x00);
    assert_eq!(dev.status() & R2_INT, 0, "R2 write clears Status INT");
    assert!(!dev.take_irq(), "no edge from a bare Status ack");
    // Drive to the next underflow (count reloaded to 1 -> 2 more frames).
    let _ = dev.render_frame(|| Some(0x80));
    let _ = dev.render_frame(|| Some(0x80));
    assert!(dev.take_irq(), "fresh underflow latches a new edge");
}

#[test]
fn auto_reload_disarms_when_base_count_rewritten_to_zero() {
    // advance_count's else-branch: an underflow whose base count is now zero
    // disarms playback instead of re-arming.
    let mut dev = Ad1848::default();
    arm_8bit_mono(&mut dev, 1); // base 1: arms with current_count = 1
    let _ = dev.render_frame(|| Some(0x80)); // count 1 -> 0, still armed
    assert!(dev.is_playing(), "still armed after first frame");
    // Zero the base by writing only the LOWER byte (upper was already 0):
    // base() now reads 0, but current_count stays 0 (no upper-byte reload).
    write_indirect(&mut dev, IDX_LOWER_COUNT as u8, 0);
    // current_count == 0, base == 0, still playing -> the next frame enters
    // the underflow period and the zero-base reload branch disarms playback.
    let _ = dev.render_frame(|| Some(0x80));
    assert!(!dev.is_playing(), "zero-base reload disarms playback");
    assert_ne!(dev.status() & R2_INT, 0, "INT still set at the underflow");
}

#[test]
fn i11_and_i12_writes_are_ignored_read_only() {
    let mut dev = Ad1848::default();
    // I12 revision is read-only: a garbage write must not change the read.
    write_indirect(&mut dev, IDX_MISC_INFO as u8, 0xFF);
    assert_eq!(
        read_indirect(&mut dev, IDX_MISC_INFO as u8) & 0x0F,
        0b1010,
        "I12 revision unchanged by write"
    );
    // I11 ACI (bit5) cannot be forced on via a register write while no
    // autocal window is active. Select index 11 first (this clears the
    // power-on MCE and opens the ~128-sample autocal window), then exhaust
    // the window so aci_remaining is None before attempting the spoof write.
    let mut dev = Ad1848::default();
    dev.write_port(R0_INDEX, IDX_TEST_INIT as u8); // clears MCE -> ACI window
    for _ in 0..AUTOCAL_SAMPLES {
        dev.advance_autocal();
    }
    assert_eq!(
        dev.read_port(R1_DATA) & I11_ACI,
        0,
        "autocal window elapsed: ACI clear"
    );
    // Index is still 11 and MCE already clear, so this write neither
    // re-opens the window nor stores into the read-only ACI bit.
    dev.write_port(R1_DATA, I11_ACI);
    assert_eq!(
        dev.read_port(R1_DATA) & I11_ACI,
        0,
        "ACI cannot be spoofed on via an I11 write"
    );
}

#[test]
fn format_dispatch_is_format_sensitive_with_sign_bearing_codes() {
    // The same input byte must decode differently under mu-law vs A-law,
    // proving format() routes to distinct decoders (not one swapped for the
    // other). Also pin a negative-polarity code for each.
    let render_one = |i8_format: u8, byte: u8| -> (i16, i16) {
        let mut dev = Ad1848::default();
        arm_format(&mut dev, i8_format, 16);
        dev.render_frame(move || Some(byte)).unwrap()
    };
    // Non-extreme byte under mu-law vs A-law -> different decoded values.
    let mu = render_one(I8_LC, 0x40);
    let al = render_one(I8_LC | I8_FMT, 0x40);
    assert_ne!(
        mu, al,
        "mu-law and A-law decode the same byte differently (dispatch is format-sensitive)"
    );
    // Negative-polarity codes: mu-law 0x70 (high bit clear -> negative),
    // A-law 0x2A (toggled sign clear -> negative).
    assert!(render_one(I8_LC, 0x70).0 < 0, "mu-law 0x70 is negative");
    assert!(
        render_one(I8_LC | I8_FMT, 0x2A).0 < 0,
        "A-law 0x2A is negative"
    );
}

#[test]
fn frames_until_next_irq_tracks_count_and_gates() {
    // Idle codec: nothing can wake the CPU.
    let mut dev = Ad1848::default();
    assert_eq!(dev.frames_until_next_irq(), None);

    // Armed 8-bit mono with IEN set, base count 4 -> N+1 = 5 frames to the
    // first underflow.
    arm_8bit_mono(&mut dev, 4);
    assert_eq!(dev.frames_until_next_irq(), Some(5));

    // Same arming but with IEN clear: the external pin never forwards, so no
    // wake. arm_format leaves IEN set, so write I10 back to 0 explicitly.
    let mut dev = Ad1848::default();
    arm_8bit_mono(&mut dev, 4);
    write_indirect(&mut dev, IDX_PIN_CONTROL as u8, 0);
    assert_eq!(
        dev.frames_until_next_irq(),
        None,
        "IEN clear cannot wake the CPU"
    );

    // TRD count-gate: once TRD is set AND the sticky INT bit is pending,
    // advance_count freezes the count, so no further underflow is generated
    // until the host acks INT. The estimator must mirror that and return None,
    // not a finite estimate the producer will never honor.
    let mut dev = Ad1848::default();
    arm_8bit_mono(&mut dev, 1);
    dev.write_port(R0_INDEX, R0_TRD); // latch TRD
    let _ = dev.render_frame(|| Some(0x80)); // count 1 -> 0
    let _ = dev.render_frame(|| Some(0x80)); // underflow: INT set, count held
    assert!(dev.take_irq(), "the underflow forwarded the first edge");
    assert_eq!(
        dev.frames_until_next_irq(),
        None,
        "TRD + sticky INT freezes the count, so no further wake is generated"
    );
    // Acking INT (R2 write) clears the gate; the estimator returns finite again.
    dev.write_port(R2_STATUS, 0x00);
    assert!(
        dev.frames_until_next_irq().is_some(),
        "acking INT releases the TRD gate so the codec can wake again"
    );
}
