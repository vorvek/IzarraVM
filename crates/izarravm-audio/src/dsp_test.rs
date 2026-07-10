// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn reset_handshake_yields_0xaa() {
    let mut dsp = SbDsp::default();
    dsp.write_port(0x226, 0x01);
    dsp.write_port(0x226, 0x00);
    dsp.advance_micros(120); // > the ~100us the DSP needs to respond
    // 0x22E bit7 = data available.
    assert_eq!(dsp.read_port(0x22E), Some(0x80));
    assert_eq!(dsp.read_port(0x22A), Some(0xAA));
    assert_eq!(dsp.read_port(0x22E), Some(0x00), "data consumed");
}

#[test]
fn empty_read_data_does_not_fake_a_reset_ack() {
    // With nothing queued and no reset, the read-data port returns the idle
    // bus value, not the 0xAA the DSP only emits after a real reset.
    let mut dsp = SbDsp::default();
    assert_eq!(dsp.read_port(0x22A), Some(0xFF));
}

#[test]
fn dsp_claims_only_its_own_ports() {
    let mut dsp = SbDsp::default();
    assert!(
        !dsp.write_port(0x224, 0x00),
        "mixer (0x224) stays out of scope"
    );
    assert!(dsp.write_port(0x226, 0x00), "reset is a DSP port");
}

#[test]
fn write_status_port_reports_ready() {
    let mut dsp = SbDsp::default();
    assert_eq!(
        dsp.read_port(0x22C),
        Some(0x00),
        "bit 7 clear means command/data writes may proceed"
    );
}

fn write_cmd(dsp: &mut SbDsp, bytes: &[u8]) {
    for &b in bytes {
        dsp.write_port(0x22C, b);
    }
}

#[test]
fn version_command_returns_sb16_4_5() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0xE1]);
    assert_eq!(dsp.read_port(0x22A), Some(DSP_VERSION_HI));
    assert_eq!(dsp.read_port(0x22A), Some(DSP_VERSION_LO));
}

#[test]
fn test_register_write_then_read_round_trips() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0xE4, 0x5A]);
    write_cmd(&mut dsp, &[0xE8]);
    assert_eq!(dsp.read_port(0x22A), Some(0x5A));
}

#[test]
fn direct_dac_command_latches_one_byte() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x10, 0x80]);
    assert_eq!(dsp.direct_dac_byte(), Some(0x80));
}

#[test]
fn reset_clears_the_16bit_mode_latch_so_direct_dac_works_again() {
    let mut dsp = SbDsp::default();
    // Arm a 16-bit signed stereo auto-init playback (0xB6, mode 0x30).
    write_cmd(&mut dsp, &[0xB6, 0x30, 0x07, 0x00]);
    // Game resets the DSP (halt playback), then falls back to direct DAC.
    dsp.write_port(0x226, 0x01);
    dsp.write_port(0x226, 0x00);
    write_cmd(&mut dsp, &[0x10, 0x80]);
    let frame = dsp.render_frame(|| None, || None);
    assert_eq!(
        frame,
        Some((0, 0)),
        "direct DAC byte 0x80 (midpoint) must render after a reset"
    );
}

#[test]
fn time_constant_sets_the_playback_rate() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x40, 0x9C]); // tc 0x9C -> 1e6/(256-156)=1e6/100 = 10000 Hz
    assert_eq!(dsp.rate_hz(), 10_000);
}

#[test]
fn sb16_rate_command_programs_hz_directly() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x41, 0x2B, 0x11]); // 0x2B11 = 11025 Hz, high byte then low byte
    assert_eq!(dsp.rate_hz(), 11_025);
}

#[test]
fn dma_single_output_arms_with_inline_length() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x14, 0xFF, 0x00]); // length 0x00FF -> 256 samples
    assert!(dsp.is_playing());
    assert!(!dsp.is_auto_init());
    assert_eq!(dsp.block_remaining(), 256);
}

#[test]
fn auto_init_command_marks_the_mode() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x48, 0x00, 0x01]); // 0x0100 -> 256
    write_cmd(&mut dsp, &[0x1C]); // 8-bit auto-init
    assert!(dsp.is_playing() && dsp.is_auto_init());
}

#[test]
fn render_sample_raises_one_irq_per_completed_auto_init_block() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x41, 0x2B, 0x11]); // 11025 Hz
    write_cmd(&mut dsp, &[0x48, 0x07, 0x00]); // block size 8
    write_cmd(&mut dsp, &[0x1C]); // 8-bit auto-init
    let pattern = [0x00u8, 0x40, 0x80, 0xC0, 0x00, 0x40, 0x80, 0xC0];
    let mut irq_at: Vec<usize> = Vec::new();
    for i in 1..=16 {
        let byte = pattern[(i - 1) % pattern.len()];
        let _ = dsp.render_sample(move || Some(byte));
        if dsp.take_irq() {
            irq_at.push(i);
        }
    }
    assert_eq!(irq_at, vec![8, 16], "one IRQ at each block completion");
    // Auto-init reloads and keeps playing across programmed block completion.
    assert!(dsp.is_playing(), "auto-init keeps playing past TC");
}

#[test]
fn single_mode_stops_at_end_of_block() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x41, 0x2B, 0x11, 0x14, 0x01, 0x00]); // block 2, single
    let _ = dsp.render_sample(|| Some(0x80));
    let _ = dsp.render_sample(|| Some(0x80)); // TC -> single stops
    assert!(!dsp.is_playing(), "single mode halts after the block");
}

#[test]
fn halt_continue_and_exit_auto_init_commands_control_playback() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x48, 0x07, 0x00, 0x1C]); // auto-init, block 8
    assert!(dsp.is_playing() && dsp.is_auto_init());
    write_cmd(&mut dsp, &[0xD0]); // halt
    assert!(!dsp.is_playing());
    write_cmd(&mut dsp, &[0xD4]); // continue
    assert!(dsp.is_playing());
    write_cmd(&mut dsp, &[0xDA]); // exit auto-init
    assert!(!dsp.is_auto_init(), "exit-auto-init clears the mode");
}

#[test]
fn sb16_16bit_auto_init_command_arms_with_mode_and_count() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x41, 0x2B, 0x11]); // 11025 Hz
    // 0xB6 = 16-bit auto-init output; mode 0x30 = signed, stereo; count 7 -> 8 samples.
    write_cmd(&mut dsp, &[0xB6, 0x30, 0x07, 0x00]);
    assert!(dsp.is_playing() && dsp.is_auto_init());
    assert!(dsp.is_16bit());
    assert!(dsp.is_stereo());
    assert_eq!(dsp.block_remaining(), 8);
}

#[test]
fn sb16_16bit_single_command_arms_non_auto_init() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0xB0, 0x00, 0x01, 0x00]); // single, mono, unsigned, count 2
    assert!(dsp.is_16bit());
    assert!(!dsp.is_stereo());
    assert!(!dsp.is_auto_init());
    assert_eq!(dsp.block_remaining(), 2);
}

#[test]
fn sb16_16bit_input_command_arms_nothing() {
    // 0xB8 is the 16-bit A/D (input) command; ADC is out of scope, so it must
    // not arm playback even with well-formed arguments.
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0xB8, 0x30, 0x07, 0x00]);
    assert!(!dsp.is_playing());
    assert!(!dsp.is_16bit());
}

#[test]
fn sb16_8bit_single_command_arms_with_mode_and_count() {
    let mut dsp = SbDsp::default();
    // 0xC0 = SB16 8-bit single-cycle output; mode 0x10 = signed mono.
    write_cmd(&mut dsp, &[0xC0, 0x10, 0x01, 0x00]);
    assert!(dsp.is_playing());
    assert!(!dsp.is_auto_init());
    assert!(!dsp.is_16bit());
    assert_eq!(dsp.block_remaining(), 2);

    let f = dsp.render_frame(|| Some(0xFF), || panic!("word fetch unused"));
    assert_eq!(f, Some((-256, -256)), "signed 8-bit mono");
}

#[test]
fn sb16_8bit_auto_init_command_arms_and_stereo_deinterleaves() {
    let mut dsp = SbDsp::default();
    // 0xC6 = SB16 8-bit auto-init output; bit1/FIFO is accepted and ignored.
    // Mode 0x20 = unsigned stereo.
    write_cmd(&mut dsp, &[0xC6, 0x20, 0x03, 0x00]);
    assert!(dsp.is_playing() && dsp.is_auto_init());
    assert!(!dsp.is_16bit());
    assert!(dsp.is_stereo());

    let bytes = [0x00u8, 0x80, 0xFF, 0x40];
    let mut i = 0usize;
    let f = dsp.render_frame(
        || {
            let byte = bytes[i];
            i += 1;
            Some(byte)
        },
        || panic!("word fetch unused"),
    );
    assert_eq!(f, Some((-32_768, 0)), "unsigned 8-bit stereo L/R");
}

#[test]
fn sb16_8bit_input_command_arms_nothing() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0xC8, 0x00, 0x07, 0x00]);
    assert!(!dsp.is_playing(), "ADC/input command is out of scope");
}

#[test]
fn render_frame_16bit_signed_stereo_consumes_two_words() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x41, 0x2B, 0x11]); // 11025 Hz
    // auto-init, signed, stereo, count 7 -> 8 samples = 4 stereo frames.
    write_cmd(&mut dsp, &[0xB6, 0x30, 0x07, 0x00]);
    let words = [
        0x0001u16, 0xFFFE, 0x7FFF, 0x8000, 0x0001, 0xFFFE, 0x7FFF, 0x8000,
    ];
    let mut i = 0;
    let mut frames = Vec::new();
    for _ in 0..4 {
        let f = dsp.render_frame(
            || panic!("8-bit fetch unused in 16-bit mode"),
            || {
                let w = words[i % words.len()];
                i += 1;
                Some(w)
            },
        );
        frames.push(f);
    }
    assert_eq!(frames[0], Some((1, -2)), "signed little-endian L,R");
    assert!(dsp.is_playing(), "auto-init continues past TC");
}

#[test]
fn render_frame_16bit_mono_duplicates_one_word_to_both_channels() {
    let mut dsp = SbDsp::default();
    // single, mono, signed: 0xB0 with mode 0x10 (bit4 = signed, bit5 clear = mono).
    write_cmd(&mut dsp, &[0xB0, 0x10, 0x01, 0x00]); // count 2 words
    let f = dsp.render_frame(
        || panic!("8-bit fetch unused in 16-bit mode"),
        || Some(0x7FFF),
    );
    assert_eq!(f, Some((32_767, 32_767)), "mono duplicates the word L/R");
}

#[test]
fn render_frame_8bit_mono_duplicated_to_both_channels() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x41, 0x2B, 0x11, 0x14, 0x01, 0x00]); // 8-bit mono single
    let f = dsp.render_frame(|| Some(0x80), || panic!("word fetch unused in 8-bit mode"));
    assert_eq!(f, Some((0, 0)), "0x80 -> silence on both channels");
}

#[test]
fn high_speed_auto_init_command_0x90_arms_auto_init() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x48, 0x07, 0x00]); // block size 8
    write_cmd(&mut dsp, &[0x90]); // SB Pro high-speed 8-bit auto-init
    assert!(dsp.is_playing() && dsp.is_auto_init());
    assert!(!dsp.is_16bit(), "high-speed 0x90 is an 8-bit mode");
}

#[test]
fn high_speed_single_command_0x91_arms_single() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x48, 0x07, 0x00]); // block size 8
    write_cmd(&mut dsp, &[0x91]); // SB Pro high-speed 8-bit single
    assert!(dsp.is_playing());
    assert!(!dsp.is_auto_init(), "high-speed 0x91 is single-cycle");
}

#[test]
fn command_f2_raises_the_8bit_irq_immediately() {
    let mut dsp = SbDsp::default();
    assert!(!dsp.take_irq(), "no IRQ pending before the command");
    dsp.write_command_byte(0xF2);
    assert!(dsp.take_irq(), "F2 requests the 8-bit interrupt");
    assert!(!dsp.take_irq(), "take_irq clears the pending state");
}

#[test]
fn reset_during_active_playback_clears_playing_and_auto_init() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x48, 0x07, 0x00, 0x90]); // block 8, high-speed auto-init
    assert!(dsp.is_playing() && dsp.is_auto_init());
    // Render part of the block and leave a command partially assembled so
    // `pending` is non-empty.
    for _ in 0..5 {
        let _ = dsp.render_sample(|| Some(0x80));
    }
    // Re-establish a pending IRQ and a partial command to prove reset clears them.
    dsp.irq_pending = true;
    dsp.write_command_byte(0x48); // arity-2 command, no args yet -> pending set
    assert!(dsp.pending.is_some(), "partial command queued before reset");
    // A DSP reset (write 0 to 0x226) halts playback, the way a game exits
    // high-speed mode.
    dsp.write_port(0x226, 0x00);
    assert!(!dsp.is_playing(), "reset halts playback");
    assert!(!dsp.is_auto_init(), "reset clears the auto-init latch");
    assert_eq!(dsp.block_remaining(), 0);
    assert!(dsp.pending.is_none(), "reset drops the partial command");
    // Real hardware clears the interrupt latch on reset, so a pre-reset
    // pending IRQ does not fire on the next re-armed playback.
    assert!(!dsp.take_irq(), "reset clears the pending IRQ latch");
    // rate/block-size are intentionally preserved across the halt-on-reset.
    assert_eq!(
        dsp.block_size, 8,
        "reset preserves the programmed block size"
    );
}

#[test]
fn sbpro_8bit_stereo_consumes_two_bytes_and_yields_distinct_l_r() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x14, 0x03, 0x00]); // block 4 bytes, 8-bit single
    assert!(!dsp.is_sbpro_stereo(), "SB Pro stereo off by default");
    dsp.set_sbpro_stereo(true);
    assert!(dsp.is_sbpro_stereo(), "set_sbpro_stereo(true) latches");
    // Left 0xFF (near full positive), right 0x00 (full negative) interleaved.
    let pattern = [0xFFu8, 0x00];
    let mut i = 0;
    let f = dsp.render_frame(
        || {
            let b = pattern[i % pattern.len()];
            i += 1;
            Some(b)
        },
        || panic!("word fetch unused in 8-bit stereo"),
    );
    assert_eq!(i, 2, "two bytes consumed per stereo frame");
    let (l, r) = f.expect("a stereo frame");
    assert!(
        l > 0 && r < 0,
        "distinct L/R from the byte pattern: {l},{r}"
    );
    assert_eq!(dsp.block_remaining(), 2, "block advanced by both bytes");
}

#[test]
fn sbpro_8bit_stereo_raises_irq_only_at_block_completion() {
    let mut dsp = SbDsp::default();
    // Block 4 bytes, 8-bit single, SB Pro stereo: advance_block(2) per frame,
    // so the block drains in 2 frames. Only block completion after frame 2 raises
    // the IRQ, then single mode stops.
    write_cmd(&mut dsp, &[0x14, 0x03, 0x00]); // block 4
    dsp.set_sbpro_stereo(true);
    let mut feed = || Some(0x80u8);
    // Frame 1: remaining 4 -> 2, no IRQ.
    assert!(dsp.render_frame(&mut feed, || panic!("no words")).is_some());
    assert_eq!(dsp.block_remaining(), 2);
    assert!(!dsp.take_irq(), "no IRQ before block completion");
    // Frame 2: remaining 2 -> 0, end IRQ, single mode stops.
    assert!(dsp.render_frame(&mut feed, || panic!("no words")).is_some());
    assert!(dsp.take_irq(), "block-completion IRQ after frame 2");
    assert!(!dsp.is_playing(), "single mode stops at end of block");
}

#[test]
fn high_speed_0x90_clears_stale_16bit_stereo_signed_latches() {
    let mut dsp = SbDsp::default();
    // First arm a 16-bit signed stereo auto-init mode (0xB6, mode 0x30).
    write_cmd(&mut dsp, &[0xB6, 0x30, 0x07, 0x00]);
    assert!(dsp.is_16bit() && dsp.is_stereo() && dsp.sample_signed);
    // A high-speed 8-bit command must reset those latches to the 8-bit
    // defaults; arm_dma clears them. The render path then pulls bytes.
    write_cmd(&mut dsp, &[0x48, 0x03, 0x00]); // block 4
    write_cmd(&mut dsp, &[0x90]); // high-speed auto-init 8-bit
    assert!(!dsp.is_16bit(), "0x90 clears the 16-bit latch");
    assert!(!dsp.is_stereo(), "0x90 clears the 16-bit stereo latch");
    assert!(!dsp.sample_signed, "0x90 clears the signed latch");
    // The 8-bit render path must run (pull a byte, never a word).
    let f = dsp.render_frame(|| Some(0x80), || panic!("word fetch unused in 8-bit mode"));
    assert_eq!(f, Some((0, 0)), "8-bit render path taken after 0x90");
}

#[test]
fn output_frame_rate_halves_for_8bit_stereo_only() {
    let mut dsp = SbDsp::default();
    // 0x40 time constant programs the interleaved BYTE rate. tc for ~22.05k
    // byte rate: 256 - 1_000_000/22_050 = 256 - 45 = 211 (0xD3), giving
    // 1_000_000 / 45 = 22_222.
    write_cmd(&mut dsp, &[0x40, 0xD3]);
    let byte_rate = dsp.rate_hz();
    // 8-bit mono: per-channel rate is the programmed rate.
    write_cmd(&mut dsp, &[0x14, 0x00, 0x00]);
    assert_eq!(dsp.output_frame_rate(), byte_rate, "8-bit mono is unhalved");
    // 8-bit stereo: the time constant is the byte rate, so each channel halves.
    dsp.set_sbpro_stereo(true);
    assert_eq!(
        dsp.output_frame_rate(),
        byte_rate / 2,
        "8-bit stereo halves a time-constant (byte) rate"
    );
    // 16-bit stereo: the rate command programs the per-channel rate already,
    // so the SB Pro byte-interleave halving must not apply.
    write_cmd(&mut dsp, &[0xB6, 0x30, 0x07, 0x00]); // 16-bit signed stereo
    assert_eq!(dsp.rate_hz(), byte_rate, "16-bit stereo unchanged");
    assert_eq!(
        dsp.output_frame_rate(),
        byte_rate,
        "16-bit stereo unchanged"
    );
}

#[test]
fn output_frame_rate_does_not_halve_a_0x41_rate_for_8bit_stereo() {
    // Per the SB16 guide, 0x41 programs the per-channel rate directly (no
    // channel-count pre-multiply), so SB Pro stereo must NOT halve it.
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x41, 0x2B, 0x11]); // 0x2B11 = 11025 Hz, per-channel
    write_cmd(&mut dsp, &[0x14, 0x00, 0x00]); // 8-bit single
    dsp.set_sbpro_stereo(true);
    assert_eq!(
        dsp.output_frame_rate(),
        11_025,
        "a 0x41 per-channel rate is not halved for SB Pro stereo"
    );
}

#[test]
fn frames_until_next_irq_counts_stereo_dma_units() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0xC6, 0x20, 0x0F, 0x00]);
    assert_eq!(dsp.frames_until_next_irq(), Some(8));

    let mut bytes = [0x80; 8].into_iter();
    assert_eq!(dsp.tick_n_samples(3, || bytes.next(), || None), 3);
    assert_eq!(dsp.frames_until_next_irq(), Some(5));
}

#[test]
fn sbpro_stereo_does_not_change_16bit_mono_irq_units() {
    let mut dsp = SbDsp::default();
    dsp.set_sbpro_stereo(true);
    write_cmd(&mut dsp, &[0xB0, 0x00, 0x07, 0x00]);
    assert_eq!(dsp.frames_until_next_irq(), Some(8));
}

#[test]
fn tick_n_samples_reports_frames_and_stops_when_dma_is_dry() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x14, 0x03, 0x00]);
    let mut bytes = [0x80].into_iter();

    let produced = dsp.tick_n_samples(4, || bytes.next(), || None);

    assert_eq!(produced, 1);
    assert_eq!(dsp.block_remaining(), 3);
    assert_eq!(dsp.drain_frame(), Some((0, 0)));
    assert_eq!(dsp.drain_frame(), None);
}

#[test]
fn odd_stereo_auto_init_block_carries_a_unit_into_the_reloaded_block() {
    let mut dsp = SbDsp::default();
    // Three DMA bytes per auto-init block, two bytes per stereo frame.
    write_cmd(&mut dsp, &[0xC6, 0x20, 0x02, 0x00]);
    let mut bytes = [0x00, 0x40, 0x80, 0xC0].into_iter();

    assert_eq!(dsp.tick_n_samples(2, || bytes.next(), || None), 2);
    assert_eq!(
        dsp.block_remaining(),
        2,
        "the fourth byte is the first unit consumed from the reloaded block"
    );
    assert!(dsp.is_playing());
    assert!(dsp.take_irq(), "the crossed block boundary raised an IRQ");
}

#[test]
fn dry_8bit_stereo_dma_preserves_the_left_sample_for_refill() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0xC6, 0x20, 0x02, 0x00]);
    let mut left_only = [0x00].into_iter();

    assert_eq!(dsp.tick_n_samples(1, || left_only.next(), || None), 0);
    assert_eq!(dsp.block_remaining(), 2);
    assert_eq!(dsp.frames_until_next_irq(), Some(2));

    let mut right_only = [0xFF].into_iter();
    assert_eq!(dsp.tick_n_samples(1, || right_only.next(), || None), 1);
    assert_eq!(dsp.drain_frame(), Some((-32_768, 32_512)));
    assert_eq!(dsp.block_remaining(), 1);
}

#[test]
fn dry_16bit_stereo_dma_preserves_the_left_sample_for_refill() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0xB6, 0x30, 0x02, 0x00]);
    let mut left_only = [0xFFFF].into_iter();

    assert_eq!(dsp.tick_n_samples(1, || None, || left_only.next()), 0);
    assert_eq!(dsp.block_remaining(), 2);

    let mut right_only = [0x0001].into_iter();
    assert_eq!(dsp.tick_n_samples(1, || None, || right_only.next()), 1);
    assert_eq!(dsp.drain_frame(), Some((-1, 1)));
    assert_eq!(dsp.block_remaining(), 1);
}

#[test]
fn odd_single_8bit_stereo_block_does_not_read_past_completion() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0xC0, 0x20, 0x00, 0x00]);
    let mut reads = 0;

    let frame = dsp.render_frame(
        || {
            reads += 1;
            Some(0x80)
        },
        || None,
    );

    assert_eq!(frame, None);
    assert_eq!(reads, 1, "the one-unit block consumes one DMA byte");
    assert!(!dsp.is_playing());
    assert!(dsp.take_irq());
}

#[test]
fn odd_single_16bit_stereo_block_does_not_read_past_completion() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0xB0, 0x20, 0x00, 0x00]);
    let mut reads = 0;

    let frame = dsp.render_frame(
        || None,
        || {
            reads += 1;
            Some(0x8000)
        },
    );

    assert_eq!(frame, None);
    assert_eq!(reads, 1, "the one-unit block consumes one DMA word");
    assert!(!dsp.is_playing());
    assert!(dsp.take_irq());
}

#[test]
fn reading_0x22f_acks_the_16bit_irq() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x41, 0x2B, 0x11, 0xB6, 0x30, 0x00, 0x00]); // count 1
    let mut w = 0u16;
    let _ = dsp.render_frame(
        || None,
        || {
            w = w.wrapping_add(1);
            Some(w)
        },
    );
    // end-of-buffer IRQ pending; 0x22F acks it.
    dsp.read_port(0x22F);
    assert!(!dsp.take_irq(), "0x22F cleared the pending IRQ");
}

// ---- Creative ADPCM ----------------------------------------------------

#[test]
fn creative_adpcm4_silence_holds_the_reference() {
    // Reference 0x80 with all-zero 4-bit codes stays at 0x80 (centered
    // silence): code 0 adds scaleMap[0]=0 and adjustMap[0]=0, so neither the
    // reference nor the step index moves.
    let mut st = AdpcmState::new(AdpcmMode::Bits4, false);
    st.reference = 0x80;
    for _ in 0..4 {
        st.decode_byte(0x00);
    }
    assert!(
        st.buf.iter().all(|&s| s == 0x80),
        "0x80 + zero codes stays silent: {:?}",
        st.buf
    );
}

#[test]
fn creative_adpcm4_decode_matches_reference_arithmetic() {
    // Byte 0x50 after reference 0x80 (step 0): hi nibble 5, lo nibble 0.
    //  code 5, step 0: samp 5, scaleMap[5]=5  -> ref 0x85 (133); step += 16.
    //  code 0, step 16: samp 16, scaleMap[16]=1 -> ref 134; adjustMap[16]=240
    //   wraps the step index (16+240)&0xff back to 0.
    let mut st = AdpcmState::new(AdpcmMode::Bits4, false);
    st.reference = 0x80;
    st.decode_byte(0x50);
    assert_eq!(st.buf.pop_front(), Some(133));
    assert_eq!(st.buf.pop_front(), Some(134));
    assert_eq!(st.step, 0, "adjustMap[16]=240 wraps step 16 to 0");
}

#[test]
fn adpcm4_reference_command_seeds_predictor_and_counts_encoded_bytes() {
    // 0x75 = 4-bit ADPCM with reference, inline length. Length 0x0002 means
    // 3 DMA bytes: the reference seed plus two encoded bytes.
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x75, 0x02, 0x00]);
    assert!(dsp.is_playing());
    assert_eq!(dsp.block_remaining(), 3, "block counts encoded DMA bytes");
    // Stream: reference 0x80 then two silence bytes.
    let stream = [0x80u8, 0x00, 0x00];
    let mut i = 0;
    let mut fetch = || {
        let b = stream.get(i).copied();
        i += 1;
        b
    };
    // First frame consumes the reference seed (no sample) then decodes the
    // first encoded byte: 0x80 -> centered silence, so both channels are 0.
    let f = dsp.render_frame(&mut fetch, || None);
    assert_eq!(f, Some((0, 0)), "0x80 decodes to centered silence");
    assert_eq!(
        dsp.block_remaining(),
        1,
        "reference seed + one encoded byte both drained the counter"
    );
}

#[test]
fn adpcm_tick_count_tracks_decoded_frames_and_drains_the_final_fifo() {
    let mut dsp = SbDsp::default();
    // Reference plus two encoded bytes. Each encoded 4-bit byte yields two
    // frames, so the three DMA bytes produce four frames in total.
    write_cmd(&mut dsp, &[0x75, 0x02, 0x00]);
    let mut bytes = [0x80, 0x00, 0x00].into_iter();
    let produced = dsp.tick_n_samples(10, || bytes.next(), || None);

    assert_eq!(produced, 4, "two encoded bytes expand to four frames");
    assert!(
        !dsp.is_playing(),
        "single-cycle transfer stopped at its byte count"
    );
    assert_eq!(
        std::iter::from_fn(|| dsp.drain_frame()).count(),
        4,
        "decoded samples buffered at block completion are still rendered"
    );
}

#[test]
fn adpcm4_auto_init_uses_the_0x48_block_size() {
    // 0x7D = 4-bit auto-init ADPCM: no inline length, block size from 0x48.
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x48, 0x03, 0x00]); // 4 encoded bytes
    write_cmd(&mut dsp, &[0x7D]);
    assert!(dsp.is_playing() && dsp.is_auto_init());
    assert_eq!(dsp.block_remaining(), 4);
}

#[test]
fn pcm_arm_clears_a_prior_adpcm_transfer() {
    // Arming a plain PCM transfer after an ADPCM one must drop the decode
    // state, or the raw-byte path stays stuck decoding.
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x75, 0x02, 0x00]);
    assert!(dsp.adpcm.is_some(), "ADPCM armed");
    write_cmd(&mut dsp, &[0x14, 0x01, 0x00]); // 8-bit PCM single
    assert!(dsp.adpcm.is_none(), "PCM arm dropped ADPCM state");
    let f = dsp.render_frame(|| Some(0x80), || None);
    assert_eq!(f, Some((0, 0)), "raw 0x80 PCM byte -> silence");
}

#[test]
fn dsp_reset_clears_adpcm_state() {
    let mut dsp = SbDsp::default();
    write_cmd(&mut dsp, &[0x75, 0x02, 0x00]);
    assert!(dsp.adpcm.is_some());
    dsp.write_port(0x226, 0x00); // reset settle
    assert!(dsp.adpcm.is_none(), "reset drops ADPCM state");
}
