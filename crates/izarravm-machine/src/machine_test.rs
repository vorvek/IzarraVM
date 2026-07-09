// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_core::{SbDma8, SbDma16, SbIrq};
use izarravm_firmware::I386DX25_TEST_ROM;
use izarravm_video::{VGA_MODE13H_BASE, VGA_MONO_TEXT_BASE, VGA_TEXT_BASE};
// Re-exported from cache carve (Phase 3).
use super::cache_config::{CACHE_LINE_BYTES, CACHE_TIER_DISABLED_MASK, cache_geometry};

const BIOS_TEXT_WHITE: u8 = 0x3F;

// The CacheModel tests below exercise tier IDENTITY, not the wait-state numbers:
// tier_cost is calibrated (non-zero) now, but these tests assert only that the
// model resolves L1/L2/RAM correctly per the per-mode geometry, not the specific
// costs.
#[test]
fn cache_level_config_matches_geometry() {
    for level in [
        CpuLevel::I286,
        CpuLevel::I386,
        CpuLevel::I486,
        CpuLevel::I586,
    ] {
        let g = cache_geometry(level);
        let config = cache_level_config(level);
        let l1_lines = g.l1_bytes / CACHE_LINE_BYTES;
        let l2_lines = g.l2_bytes / CACHE_LINE_BYTES;

        assert_eq!(
            config.l1_mask,
            if l1_lines == 0 {
                CACHE_TIER_DISABLED_MASK
            } else {
                l1_lines - 1
            }
        );
        assert_eq!(
            config.l2_mask,
            if l2_lines == 0 {
                CACHE_TIER_DISABLED_MASK
            } else {
                l2_lines - 1
            }
        );
    }
}

#[test]
fn cache_model_resolves_tiers_by_working_set() {
    let mut c = CacheModel::new(CpuLevel::I486);
    let warm = |c: &mut CacheModel, base: u32, len: u32| {
        for off in (0..len).step_by(64) {
            c.data_tier(CpuLevel::I486, base + off);
        }
    };
    warm(&mut c, 0x10_0000, 8 * 1024); // 8K fits 486 L1 (16K)
    assert_eq!(c.data_tier(CpuLevel::I486, 0x10_0000), Tier::L1);
    warm(&mut c, 0x20_0000, 64 * 1024); // 64K exceeds L1, fits L2 (128K)
    assert_eq!(c.data_tier(CpuLevel::I486, 0x20_0000), Tier::L2);
    warm(&mut c, 0x40_0000, 256 * 1024); // 256K exceeds 486 L2 -> RAM
    assert_eq!(c.data_tier(CpuLevel::I486, 0x40_0000), Tier::Ram);
    assert_eq!(c.data_tier(CpuLevel::I286, 0x10_0000), Tier::Ram); // 286: no cache
}

#[test]
fn cache_model_reset_goes_cold() {
    let mut c = CacheModel::new(CpuLevel::I586);
    c.data_tier(CpuLevel::I586, 0x30_0000); // installs the line
    assert_eq!(c.data_tier(CpuLevel::I586, 0x30_0000), Tier::L1); // hot
    c.reset();
    assert_ne!(c.data_tier(CpuLevel::I586, 0x30_0000), Tier::L1); // cold again
}

#[test]
fn measure_read_bandwidth_returns_a_finite_sample_in_every_mode() {
    // Small block (fits every mode's L1) and a large block (exceeds every L2).
    // Both must move bytes and take clocks; we do NOT assert tier ordering here
    // -- the tier costs are calibrated and a separate ordering test covers the
    // descending L1/L2/RAM curve.
    const TOTAL: u64 = 4 * 1024 * 1024;
    let modes = [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ];
    for mode in modes {
        let mut machine = Machine::new_boot_image(
            MachineProfile::gsw_386(24, VideoCard::Et4000Ax),
            izarravm_firmware::neurketa_image(),
        )
        .expect("boot image");
        machine.set_mode(mode);
        for block in [4 * 1024u32, 1024 * 1024] {
            let sample = machine.measure_read_bandwidth(0x10_0000, block, TOTAL);
            assert!(sample.bytes > 0, "{mode:?} block {block}: zero bytes");
            assert!(sample.clocks > 0, "{mode:?} block {block}: zero clocks");
            // bytes == block_bytes * passes, with passes = max(2, TOTAL/block).
            let passes = (TOTAL / u64::from(block)).max(2);
            assert_eq!(
                sample.bytes,
                u64::from(block) * passes,
                "{mode:?} block {block}: bytes != block * passes"
            );
        }
    }
}

#[test]
fn measure_read_bandwidth_curve_descends_per_tier() {
    // The end-to-end proof that "the three speed levels work": drive the bus over
    // block sizes chosen to land well inside each tier FOR THAT MODE (not at a
    // boundary, so the separation is clean), compute MB/s, and assert the curve
    // descends L1 > L2 > RAM. A fixed TOTAL budget amortizes the cold first pass.
    const TOTAL: u64 = 16 * 1024 * 1024;
    const BASE: u32 = 0x10_0000;

    // MB/s from a sample, mirroring the --headless-bandwidth derivation.
    fn mbps(machine: &mut Machine, mode: GswMode, block: u32) -> f64 {
        let sample = machine.measure_read_bandwidth(BASE, block, TOTAL);
        assert!(sample.clocks > 0, "{mode:?} block {block}: zero clocks");
        sample.bytes as f64 / (sample.clocks as f64 / mode.clock_hz() as f64) / 1.0e6
    }

    // A fresh machine per (mode, block) so each measurement starts cold and
    // nothing carries over, exactly like the bandwidth tool does.
    fn measure(mode: GswMode, block: u32) -> f64 {
        let mut machine = Machine::new_boot_image(
            MachineProfile::gsw_386(24, VideoCard::Et4000Ax),
            izarravm_firmware::neurketa_image(),
        )
        .expect("boot image");
        machine.set_mode(mode);
        mbps(&mut machine, mode, block)
    }

    // 586: L1 64K, L2 512K. 32K is deep in L1, 256K deep in L2, 2M is RAM.
    {
        let l1 = measure(GswMode::Gsw586, 32 * 1024);
        let l2 = measure(GswMode::Gsw586, 256 * 1024);
        let ram = measure(GswMode::Gsw586, 2 * 1024 * 1024);
        assert!(
            l1 > l2 * 1.05,
            "586: L1 {l1:.1} must exceed L2 {l2:.1} MB/s"
        );
        assert!(
            l2 > ram * 1.05,
            "586: L2 {l2:.1} must exceed RAM {ram:.1} MB/s"
        );
    }

    // 486: L1 16K, L2 128K. 8K is deep in L1, 64K deep in L2, 256K is RAM.
    {
        let l1 = measure(GswMode::Gsw486, 8 * 1024);
        let l2 = measure(GswMode::Gsw486, 64 * 1024);
        let ram = measure(GswMode::Gsw486, 256 * 1024);
        assert!(
            l1 > l2 * 1.05,
            "486: L1 {l1:.1} must exceed L2 {l2:.1} MB/s"
        );
        assert!(
            l2 > ram * 1.05,
            "486: L2 {l2:.1} must exceed RAM {ram:.1} MB/s"
        );
    }

    // 386: L2 64K, no L1. 32K is deep in L2, 1M is well into RAM. The 386 L2-vs-RAM
    // step is the narrowest, so pick a small L2 block and a large RAM block to
    // separate them cleanly and assert a >5% margin.
    {
        let l2 = measure(GswMode::Gsw386, 32 * 1024);
        let ram = measure(GswMode::Gsw386, 1024 * 1024);
        assert!(
            l2 > ram * 1.05,
            "386: L2 {l2:.1} must exceed RAM {ram:.1} MB/s"
        );
    }

    // 286: no cache. Two sizes must be roughly flat (no tier step), within 20%.
    {
        let small = measure(GswMode::Gsw386Slow, 8 * 1024);
        let large = measure(GswMode::Gsw386Slow, 1024 * 1024);
        let ratio = small / large;
        assert!(
            (0.8..=1.25).contains(&ratio),
            "286 is cacheless: {small:.1} vs {large:.1} MB/s should be flat (ratio {ratio:.3})"
        );
    }
}

#[test]
fn approximate_class_bypasses_cache_tiering_accurate_class_does_not() {
    use izarravm_core::GswMode;
    let mut machine = test_machine();

    // Accurate class (386): a conventional-RAM data access warms the tier model.
    // (read_physical_u16 routes through read_memory -> data_access_wait_states;
    // read_physical_u8 takes a raw read_phys_u8 path that never tiers.)
    machine.set_mode(GswMode::Gsw386);
    let before = machine.cache_tier_lookups();
    let _ = machine.read_physical_u16(0x2_0000);
    assert!(
        machine.cache_tier_lookups() > before,
        "386 (Accurate) must tier the access (lookups increment)"
    );

    // Approximate class (586): the same access charges the flat cost, no tiering.
    machine.set_mode(GswMode::Gsw586);
    let before = machine.cache_tier_lookups();
    let _ = machine.read_physical_u16(0x2_0000);
    assert_eq!(
        machine.cache_tier_lookups(),
        before,
        "586 (Approximate) must bypass tiering (lookups unchanged)"
    );
}

#[test]
fn cache_geometry_matches_cache_kb() {
    // The machine geometry must agree with the CPU's cache_kb readout (KB).
    for (level, (l1_kb, l2_kb)) in [
        (CpuLevel::I286, (0u16, 0u16)),
        (CpuLevel::I386, (0, 64)),
        (CpuLevel::I486, (16, 128)),
        (CpuLevel::I586, (32, 512)),
    ] {
        let g = cache_geometry(level);
        assert_eq!(g.l1_bytes / 1024, u32::from(l1_kb), "{level:?} L1");
        assert_eq!(g.l2_bytes / 1024, u32::from(l2_kb), "{level:?} L2");
        assert_eq!(level.cache_kb(), (l1_kb, l2_kb)); // mirrors cpu cache_kb
    }
}

#[test]
fn slow_post_paces_without_null_vector_runaway() {
    // Under slow POST the BIOS drives PIT channel 0 to pace the chime and the
    // RAM count-up. Those OUT edges raise IRQ0 with IF set; before INT 08h was
    // installed the timer vectored through the zeroed IVT[08h] (CS=0000) and ran
    // away through low memory. Run a slice that covers the chime and the start of
    // the count-up, then confirm the CPU never left the BIOS region and the INT
    // 08h handler advanced the BDA tick count.
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    machine.set_fast_post(false);
    let mut max_ticks = 0u32;
    for _ in 0..400 {
        let _ = machine.run_until_halt_or_cycles(50_000).unwrap();
        let cs = machine.cpu().registers.cs().selector;
        assert_ne!(cs, 0, "CPU vectored to CS=0000 (null IVT runaway)");
        let lo = u32::from(machine.read_physical_u8(0x46c));
        let hi = u32::from(machine.read_physical_u8(0x46d));
        max_ticks = max_ticks.max(lo | (hi << 8));
    }
    assert!(
        max_ticks > 3,
        "INT 08h did not advance the BDA tick (got {max_ticks})"
    );
}

#[test]
fn bios_seeds_low_exception_and_irq_vectors() {
    let mut machine = int15_machine(16);

    for vector in 0x00u32..=0x07 {
        let base = vector * 4;
        assert_eq!(
            read_u16(&mut machine, base),
            bios_int_stub_off(vector as u8)
        );
        assert_eq!(read_u16(&mut machine, base + 2), BIOS_ROM_IRET_SEG);
    }
    assert_eq!(read_u16(&mut machine, 0x08 * 4), BIOS_TIMER_ISR_ROM_OFF);
    assert_eq!(read_u16(&mut machine, 0x08 * 4 + 2), BIOS_ROM_IRET_SEG);
    for vector in [0x0Au32, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F] {
        let base = vector * 4;
        assert_eq!(read_u16(&mut machine, base), BIOS_MASTER_IRQ_ISR_ROM_OFF);
        assert_eq!(read_u16(&mut machine, base + 2), BIOS_ROM_IRET_SEG);
    }
    for vector in [0x71u32, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77] {
        let base = vector * 4;
        assert_eq!(
            read_u16(&mut machine, base),
            BIOS_SLAVE_IRQ_ISR_ADDRESS as u16
        );
        assert_eq!(read_u16(&mut machine, base + 2), 0);
    }
}

#[test]
fn default_int08_ticks_and_returns_from_irq0() {
    let code: &[u8] = &[
        0xb0, 0x11, 0xe6, 0x20, // ICW1 master
        0xb0, 0x08, 0xe6, 0x21, // ICW2 base 08h
        0xb0, 0x04, 0xe6, 0x21, // ICW3 slave on IR2
        0xb0, 0x01, 0xe6, 0x21, // ICW4 8086 mode
        0xb0, 0xfe, 0xe6, 0x21, // unmask IRQ0 only
        0xb0, 0x36, 0xe6, 0x43, // PIT channel 0 mode 3
        0xb0, 0xe8, 0xe6, 0x40, // count low 1000
        0xb0, 0x03, 0xe6, 0x40, // count high
        0xfb, 0xf4, // sti; hlt
        0xfa, 0xf4, // cli; hlt
    ];
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        boot_image_with(code),
    )
    .unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    assert_eq!(read_u16(&mut machine, 0x46c), 1);
}

// The per-batch interrupt check (Stage-1 lever 2) must not break the classic
// STI; HLT idle loop. The CPU services interrupts once at batch entry; the HLT
// ends a batch halted, and the NEXT batch's entry check must see IF set, the
// shadow already consumed by the HLT instruction, and IRQ0 pending - and take
// it. The wrong design (consuming the STI shadow at batch entry instead of per
// instruction) makes this loop spin forever and never tick.
#[test]
fn sti_hlt_idle_loop_still_takes_irq0_per_batch() {
    // PIC + PIT init as in default_int08_ticks..., then `sti; hlt; jmp $-2`:
    // STI enables interrupts (one-instruction shadow), HLT parks until IRQ0,
    // the default INT 08h handler runs and bumps the BDA tick, IRET returns to
    // the JMP which loops back to STI. The tick at 0x46c must keep advancing.
    let code: &[u8] = &[
        0xb0, 0x11, 0xe6, 0x20, // ICW1 master
        0xb0, 0x08, 0xe6, 0x21, // ICW2 base 08h
        0xb0, 0x04, 0xe6, 0x21, // ICW3 slave on IR2
        0xb0, 0x01, 0xe6, 0x21, // ICW4 8086 mode
        0xb0, 0xfe, 0xe6, 0x21, // unmask IRQ0 only
        0xb0, 0x36, 0xe6, 0x43, // PIT channel 0 mode 3
        0xb0, 0xe8, 0xe6, 0x40, // count low 1000
        0xb0, 0x03, 0xe6, 0x40, // count high
        0xfb, // sti
        0xf4, // hlt
        0xeb, 0xfc, // jmp $-2 (back to the sti)
    ];
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        boot_image_with(code),
    )
    .unwrap();

    // The loop never genuinely halts, so it exhausts the budget; the assertion
    // is that the timer ISR ran while it spun, proving the per-batch check still
    // takes the interrupt for an STI; HLT idle loop.
    let _ = machine.run_until_halt_or_cycles(2_000_000).unwrap();

    assert!(
        read_u16(&mut machine, 0x46c) >= 1,
        "the IRQ0 handler must run for an STI; HLT idle loop under per-batch \
             interrupt checking (it would spin forever if the STI shadow were \
             consumed at batch entry instead of per instruction)"
    );
}

// A run of straight-line instructions between interrupt checks must not delay
// the interrupt past the batch: `sti; nop x5; jmp $-7` keeps the CPU busy with
// no HLT and no port I/O, so a whole batch of NOPs runs through
// cycle_no_interrupt_check before the next batch entry, where IRQ0 is taken.
#[test]
fn sti_busy_loop_takes_irq0_despite_intervening_instructions() {
    let code: &[u8] = &[
        0xb0, 0x11, 0xe6, 0x20, // ICW1 master
        0xb0, 0x08, 0xe6, 0x21, // ICW2 base 08h
        0xb0, 0x04, 0xe6, 0x21, // ICW3 slave on IR2
        0xb0, 0x01, 0xe6, 0x21, // ICW4 8086 mode
        0xb0, 0xfe, 0xe6, 0x21, // unmask IRQ0 only
        0xb0, 0x36, 0xe6, 0x43, // PIT channel 0 mode 3
        0xb0, 0xe8, 0xe6, 0x40, // count low 1000
        0xb0, 0x03, 0xe6, 0x40, // count high
        0xfb, // sti
        0x90, 0x90, 0x90, 0x90, 0x90, // nop x5
        0xeb, 0xf9, // jmp $-7 (back to the sti)
    ];
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        boot_image_with(code),
    )
    .unwrap();

    let _ = machine.run_until_halt_or_cycles(2_000_000).unwrap();

    assert!(
        read_u16(&mut machine, 0x46c) >= 1,
        "the IRQ0 handler must run for a busy STI loop even with NOPs between \
             interrupt checks"
    );
}

#[test]
fn machine_accepts_256k_flash_and_shadows_top_64k() {
    // A 256 KiB flash whose top 64 KiB carries a recognizable reset far-jump
    // boots: the machine maps the top 64 KiB at 0xF0000, so the reset vector at
    // 0xFFFF0 reads the far jump.
    let mut flash = vec![0u8; 256 * 1024];
    let top = flash.len() - BIOS_ROM_SIZE;
    flash[top + 0xfff0..top + 0xfff5].copy_from_slice(&[0xea, 0x00, 0x00, 0x00, 0xf0]);
    let mut machine =
        Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), flash).unwrap();
    assert_eq!(machine.read_physical_u8(0xffff0), 0xea);
    assert_eq!(machine.read_physical_u8(0xffff4), 0xf0);
}

#[test]
fn izarra_bios_boots_into_margo_lfb_screen() {
    // POST sets the proprietary 320x240x8 Margo mode and draws its screen
    // there. Fast POST (default) skips delays so the screen is up within the
    // cycle budget.
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    // The full-screen RLE background blit writes 76800 LFB bytes through the
    // unreal FS, so the screen comes up at ~10M cycles (still ~40 ms at the
    // machine clock); give the boot loop headroom past that.
    for _ in 0..40 {
        let _ = machine.run_until_halt_or_cycles(500_000).unwrap();
        if machine.active_display() == ActiveDisplay::MargoLfb {
            break;
        }
    }
    assert_eq!(
        machine.active_display(),
        ActiveDisplay::MargoLfb,
        "POST never set the Margo LFB mode"
    );
    let (words, w, h) = machine.frame_argb();
    assert_eq!((w, h), (320, 240), "proprietary mode is 320x240");
    assert!(
        words.iter().any(|&p| p != words[0]),
        "screen is a single flat color - nothing was drawn"
    );
}

#[test]
fn izarra_bios_lfb_carries_rle_background() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    for _ in 0..40 {
        let _ = machine.run_until_halt_or_cycles(500_000).unwrap();
        if machine.active_display() == ActiveDisplay::MargoLfb {
            break;
        }
    }
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    // A top-left pixel is the cream field; the icon-strip row (y 175) carries
    // the baked-in grey icons, so it is not a single flat colour.
    let field = machine.read_physical_u8(MARGO_LFB_BASE + 4 * 320 + 4);
    let mut varied = false;
    for x in (0..320u32).step_by(7) {
        if machine.read_physical_u8(MARGO_LFB_BASE + 175 * 320 + x) != field {
            varied = true;
            break;
        }
    }
    assert!(
        varied,
        "icon-strip row is flat — RLE background did not blit"
    );
}

fn test_machine() -> Machine {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        I386DX25_TEST_ROM,
    )
    .unwrap();
    machine.set_bus_trace_detailed(true);
    machine
}

#[test]
fn predict_vga_dots_matches_the_real_advance_devices_accumulator_step() {
    // predict_dots must be textually identical arithmetic to the vga_dots
    // block it was extracted from (Task 0.3), so its output, fed through the
    // same Vga::advance + vga_dots-subtract sequence advance_devices already
    // uses, reproduces the real post-advance_devices state exactly: not just
    // numerically close, bit-for-bit (same operation order, same rounding).
    let mut expected = test_machine();
    let mut actual = test_machine();
    let clocks = 12_345u64;
    let vga_dots_before = actual.vga_dots;

    // The real, mutating step (what advance_devices does today).
    expected.advance_devices(clocks);

    // The shared pure function, applied by hand the same way advance_devices
    // applies it internally.
    let (whole, remainder) = actual.predict_dots(clocks, vga_dots_before);
    actual.video.advance(whole);
    actual.vga_dots = remainder;

    assert_eq!(
        actual.video.beam_dots(),
        expected.video.beam_dots(),
        "predict_dots's whole-dots output must move the beam identically \
             to advance_devices's real step"
    );
    assert_eq!(
        actual.vga_dots, expected.vga_dots,
        "predict_dots's fractional remainder must match the real accumulator"
    );
    assert_eq!(
        actual.video.frames_completed(),
        expected.video.frames_completed(),
        "frame-boundary bookkeeping (finalize_frame/frames) must also agree \
             when applied by hand through the same Vga::advance call"
    );
}

fn int15_machine(mem_mib: u16) -> Machine {
    Machine::new(
        MachineProfile::gsw_386(mem_mib, VideoCard::Et4000Ax),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap()
}

/// Emulate a guest `INT n` end to end for interception-contract tests: the
/// opcode acknowledge (which posts only the raw-program low-RAM vectors
/// and stashes the legacy-chain attribution), then the IVT dispatch
/// landing wherever the vector points. A default vector lands on its
/// per-vector ROM stub, whose fetch seam posts the service; a guest hook
/// lands outside the table and gets NO HLE post (the hook owns the
/// vector); the legacy shared FF00:0000 posts the stashed vector.
fn ack_and_dispatch(m: &mut Machine, vector: u8) {
    let mut bus = m.make_bus();
    bus.interrupt_acknowledge(vector, 0).unwrap();
    let base = usize::from(vector) * 4;
    let off = bus.memory.read_u16(base).unwrap();
    let seg = bus.memory.read_u16(base + 2).unwrap();
    let target = (u32::from(seg) << 4) + u32::from(off);
    bus.note_stub_fetch(target);
}

fn color_crtc_reg(machine: &mut Machine, index: u8) -> u8 {
    let mut bus = machine.make_bus();
    bus.write_io(0x3D4, BusWidth::Byte, u32::from(index), false)
        .unwrap();
    bus.read_io(0x3D5, BusWidth::Byte, 0, false).unwrap() as u8
}

#[test]
fn word_out_to_a_vga_register_pair_splits_into_two_byte_cycles() {
    // `OUT DX, AX` (16-bit) to a VGA index/data port pair is the canonical
    // mode-set idiom: the low byte (AL) selects the index at the port, the high
    // byte (AH) writes the data at port+1. The byte-only I/O bus used to reject
    // any non-byte width with WidthMismatch, halting the VM on real VGA setup
    // code (HOUSERS / TSUMERA SETUP.EXE both crash on exactly this).
    let mut m = int15_machine(1);
    {
        let mut bus = m.make_bus();
        // AX = 0x420F: CRTC index 0x0F (cursor location low), data 0x42.
        bus.write_io(0x3D4, BusWidth::Word, 0x420F, false).unwrap();
    }
    // The low byte set the index, the high byte wrote the data at 0x3D5.
    assert_eq!(color_crtc_reg(&mut m, 0x0F), 0x42);
}

#[test]
fn new_raw_program_leaves_pit_counter0_running() {
    // A directly-loaded DOS program must see PIT counter 0 ticking, the way the
    // BIOS POST leaves it; otherwise a guest that polls the timer for a delay or
    // a speed calibration spins forever (TSUMERA's setup does exactly that).
    static PROG: &[u8] = &[0xeb, 0xfe]; // JMP $ - we only need a machine to run.
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), PROG).unwrap();
    fn latched_count(m: &mut Machine) -> u16 {
        let mut bus = m.make_bus();
        bus.write_io(0x43, BusWidth::Byte, 0x00, false).unwrap(); // latch counter 0
        let lo = bus.read_io(0x40, BusWidth::Byte, 0, false).unwrap() as u16;
        let hi = bus.read_io(0x40, BusWidth::Byte, 0, false).unwrap() as u16;
        lo | (hi << 8)
    }
    let before = latched_count(&mut m);
    m.run_until_halt_or_cycles(100_000).unwrap();
    let after = latched_count(&mut m);
    assert_ne!(
        before, after,
        "PIT counter 0 must advance after new_raw_program (POST-equivalent timer setup)"
    );
}

#[test]
fn new_raw_program_runs_and_exits_via_int20() {
    let prog: &[u8] = &[0xcd, 0x20]; // int 20h
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), prog).unwrap();
    let reason = m.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
}

#[test]
fn new_raw_program_exits_with_ah4c_code() {
    let prog: &[u8] = &[0xb8, 0x2a, 0x4c, 0xcd, 0x21]; // mov ax,4c2a; int 21h
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), prog).unwrap();
    let reason = m.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0x2a });
}

#[test]
fn raw_program_profile_records_cpu_batch_phase() {
    let prog: &[u8] = &[0xb8, 0x00, 0x4c, 0xcd, 0x21]; // mov ax,4c00; int 21h
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), prog).unwrap();
    m.enable_host_profiling(1);

    let reason = m.run_until_halt_or_cycles(100_000).unwrap();

    assert_eq!(reason, StopReason::DosExit { code: 0 });
    let host = m.host_profile_snapshot();
    let cpu_batch = host
        .phases
        .iter()
        .find(|phase| phase.name == "cpu_batch")
        .expect("cpu_batch phase exists");
    assert!(cpu_batch.count > 0, "CPU batches should be counted");
    assert!(
        cpu_batch.wall_ns > 0,
        "CPU batch wall time should be measured"
    );
    let cpu = m.cpu().profile_snapshot();
    assert!(
        cpu.groups.iter().any(|bucket| bucket.instructions > 0),
        "CPU group profile should record retired instructions"
    );
}

#[test]
fn raw_program_uses_direct_page_data_and_fetch_caches() {
    let mut prog = vec![
        0xb9, 0x20, 0x00, // mov cx,32
        0xa1, 0x20, 0x01, // loop: mov ax,[0120h]
        0xa3, 0x22, 0x01, // mov [0122h],ax
        0xe2, 0xf8, // loop loop
        0xcd, 0x20, // int 20h
    ];
    prog.resize(0x20, 0);
    prog.extend_from_slice(&0xBEEFu16.to_le_bytes());
    prog.extend_from_slice(&0u16.to_le_bytes());

    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), &prog).unwrap();
    m.cpu.reset_perf_counters();
    let reason = m.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    let data_addr = (u32::from(DOS_LOAD_SEGMENT) << 4) + 0x0122;
    assert_eq!(
        m.memory.read_u16(data_addr as usize).unwrap(),
        0xBEEF,
        "the loop copied the direct-read word to the direct-write slot"
    );
    let perf = m.cpu.perf_counters();
    assert!(
        perf.direct_data_pointer_reads > 0,
        "scalar RAM reads should use cached page pointers"
    );
    assert!(
        perf.direct_data_pointer_writes > 0,
        "scalar RAM writes should use cached page pointers"
    );
    assert!(
        perf.fetch_page_hits > 0,
        "instruction decode should hit the direct fetch page"
    );
    assert_eq!(
        perf.slow_prefetch_refills, 0,
        "RAM instruction fetch should not need copied prefetch refills"
    );
}

#[test]
fn new_raw_program_prints_a_dollar_terminated_string() {
    // org 0x100: mov ah,9 / mov dx,msg / int 21h / mov ax,4c00h / int 21h
    // msg ("Hi$") placed right after the code, addressed PSP-relative.
    // Code is 12 bytes, so msg starts at offset 0x100+12 = 0x10C.
    let prog: &[u8] = &[
        0xb4, 0x09, 0xba, 0x0c, 0x01, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21, b'H', b'i', b'$',
    ];
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), prog).unwrap();
    let reason = m.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    assert_eq!(m.program_output(), b"Hi");
}

#[test]
fn new_raw_program_output_reaches_the_vga_screen() {
    // Same program as new_raw_program_prints_a_dollar_terminated_string:
    // org 0x100: mov ah,9 / mov dx,msg / int 21h / mov ax,4c00h / int 21h
    // msg ("Hi$") placed right after the code, addressed PSP-relative.
    // Code is 12 bytes, so msg starts at offset 0x100+12 = 0x10C.
    let prog: &[u8] = &[
        0xb4, 0x09, 0xba, 0x0c, 0x01, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21, b'H', b'i', b'$',
    ];
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), prog).unwrap();
    let reason = m.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    let screen = m.screen_text();
    assert!(
        screen.line_string(0).starts_with("Hi"),
        "screen line 0 was {:?}",
        screen.line_string(0)
    );
}

#[test]
fn new_raw_program_reads_typed_keys_via_ah01() {
    // org 0x100: mov ah,1 / int 21h / mov ah,1 / int 21h / mov ax,4c00h / int 21h
    let prog: &[u8] = &[
        0xb4, 0x01, 0xcd, 0x21, 0xb4, 0x01, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21,
    ];
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), prog).unwrap();
    m.set_program_stdin(b"hi");
    let reason = m.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    assert_eq!(m.program_output(), b"hi");
}

#[test]
fn new_raw_program_unknown_int21_function_sets_carry() {
    // org 0x100: mov ah,0xff / int 21h ; the unrecognized AH=FFh falls
    // into a tight loop on CF so the test can stop and inspect FLAGS
    // without the program continuing past it.
    let prog: &[u8] = &[0xb4, 0xff, 0xcd, 0x21, 0xeb, 0xfe];
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), prog).unwrap();
    m.run_until_halt_or_cycles(1_000).unwrap();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0007);
    assert_eq!(m.cpu.registers.eflags & 0x0001, 0x0001, "CF set");
}

#[test]
fn new_raw_program_seeds_env_one_paragraph_above_prog_top() {
    let prog: &[u8] = &[0xb8, 0x00, 0x4c, 0xcd, 0x21];
    let m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), prog).unwrap();
    let prog_top = m
        .memory()
        .read_u16(usize::from(DOS_LOAD_SEGMENT) * 16 + 2)
        .unwrap();
    let env_seg = m
        .memory()
        .read_u16(usize::from(DOS_LOAD_SEGMENT) * 16 + 0x2c)
        .unwrap();
    assert_eq!(env_seg, prog_top + 1);
}

fn prime_dos_int_frame(m: &mut Machine) {
    m.cpu
        .registers
        .set_segment(SegmentIndex::Ss, SegmentRegister::real(0x9000));
    m.cpu.registers.set_esp(0x0100);
    m.memory.write_u16(0x9000 * 16 + 0x0104, 0x0001).unwrap();
}

fn dos_int_flags(m: &Machine) -> u16 {
    m.memory.read_u16(0x9000 * 16 + 0x0104).unwrap()
}

#[test]
fn int15_8a_reports_extended_memory_as_dx_ax() {
    let mut m = int15_machine(24);
    m.cpu.registers.set_eax(0x8A00);
    m.handle_int15();
    // 23 MB above the first 1 MB = 23552 KB = 0x5C00 (fits in AX, DX = 0).
    assert_eq!(m.cpu.registers.eax() as u16, 0x5C00);
    assert_eq!(m.cpu.registers.edx() as u16, 0x0000);
}

#[test]
fn int15_21_post_error_log_stores_and_reads_entries() {
    let mut m = int15_machine(16);
    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x2101);
    m.cpu.registers.set_ebx(0x1234); // BH=device, BL=error
    m.handle_int15();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "write AH=0");
    assert_eq!(dos_int_flags(&m) & 1, 0, "write CF clear");

    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x2100);
    m.cpu.registers.set_edi(0xCAFE_0000);
    m.handle_int15();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "read AH=0");
    assert_eq!(m.cpu.registers.ebx() as u16, 1, "one POST record");
    let es = m.cpu.registers.segment(SegmentIndex::Es).base;
    let di = m.cpu.registers.edi() as u16;
    assert_eq!(es + u32::from(di), BIOS_POST_ERROR_LOG_ADDR);
    assert_eq!(m.read_physical_u8(BIOS_POST_ERROR_LOG_ADDR), 0x34);
    assert_eq!(m.read_physical_u8(BIOS_POST_ERROR_LOG_ADDR + 1), 0x12);
}

#[test]
fn int15_83_event_wait_sets_completion_byte() {
    let mut m = int15_machine(16);
    m.write_physical_u8(0x4_0000, 0x01);
    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x8300);
    m.cpu.registers.set_ecx(0x0000);
    m.cpu.registers.set_edx(0x0001);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int15();

    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0");
    assert_eq!(m.read_physical_u8(0x4_0000), 0x81, "completion bit set");
    assert_eq!(dos_int_flags(&m) & 1, 0, "CF clear");
}

#[test]
fn int15_84_reports_absent_joystick() {
    let mut m = int15_machine(16);
    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x84FF);
    m.cpu.registers.set_edx(0x0000);
    m.handle_int15();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0000, "switches open");
    assert_eq!(dos_int_flags(&m) & 1, 0, "switch read CF clear");

    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x8400);
    m.cpu.registers.set_ebx(0xFFFF);
    m.cpu.registers.set_ecx(0xFFFF);
    m.cpu.registers.set_edx(0x0001);
    m.handle_int15();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0000, "joy A X");
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0000, "joy A Y");
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0000, "joy B X");
    assert_eq!(m.cpu.registers.edx() as u16, 0x0000, "joy B Y");
    assert_eq!(dos_int_flags(&m) & 1, 0, "position read CF clear");
}

#[test]
fn int15_reports_absent_cassette() {
    for ah in [0x00u8, 0x01, 0x02, 0x03] {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax(u32::from(ah) << 8);
        m.handle_int15();

        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x86, "AH={ah:02X}");
        assert_eq!(dos_int_flags(&m) & 1, 1, "AH={ah:02X} CF set");
    }
}

#[test]
fn int15_keyboard_intercept_continues_scan_code() {
    let mut m = int15_machine(16);
    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x4F1E);
    m.handle_int15();

    assert_eq!(m.cpu.registers.eax() as u8, 0x1E, "scan code preserved");
    assert_eq!(dos_int_flags(&m) & 1, 1, "CF set continues processing");
}

#[test]
fn int15_os_device_hooks_succeed_as_noops() {
    for ah in [0x80u8, 0x81, 0x82] {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax((u32::from(ah) << 8) | 0x55);
        m.handle_int15();

        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH={ah:02X}");
        assert_eq!(dos_int_flags(&m) & 1, 0, "AH={ah:02X} CF clear");
    }
}

#[test]
fn int15_reports_absent_watchdog_and_pos() {
    for ax in [0xC300u32, 0xC400] {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax(ax);
        m.handle_int15();

        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x86, "AX={ax:04X}");
        assert_eq!(dos_int_flags(&m) & 1, 1, "AX={ax:04X} CF set");
    }
}

#[test]
fn int15_reports_absent_window_manager_print_and_convertible_calls() {
    for ax in [
        0x1000u32, 0x1022, 0x102D, 0xDE00, 0xDE12, 0x1100, 0x1200, 0x2000, 0x4000, 0x4400,
    ] {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax(ax);
        m.cpu.registers.set_ebx(0xFFFF);
        m.handle_int15();

        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x86, "AX={ax:04X}");
        assert_eq!(m.cpu.registers.ebx() as u16, 0x0000, "AX={ax:04X} BX");
        assert_eq!(dos_int_flags(&m) & 1, 1, "AX={ax:04X} CF set");
    }
}

#[test]
fn int15_low_bios_hooks_return_defined_status() {
    let mut m = int15_machine(16);

    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x0F02);
    m.handle_int15();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "format continues");
    assert_eq!(dos_int_flags(&m) & 1, 0, "AH=0F CF clear");

    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x8500);
    m.handle_int15();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "SysReq hook OK");
    assert_eq!(dos_int_flags(&m) & 1, 0, "AH=85 CF clear");

    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x8900);
    m.handle_int15();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x86,
        "BIOS protected-mode switch unsupported"
    );
    assert_eq!(dos_int_flags(&m) & 1, 1, "AH=89 CF set");
}

#[test]
fn int1a_09_reports_alarm_disabled() {
    let mut m = int15_machine(16);
    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x0900);
    m.cpu.registers.set_ecx(0xFFFF);
    m.cpu.registers.set_edx(0xFFFF);
    m.handle_int1a();
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0000, "alarm time");
    assert_eq!(m.cpu.registers.edx() as u16, 0x0000, "alarm disabled");
    assert_eq!(dos_int_flags(&m) & 1, 0, "CF clear");
}

#[test]
fn int1a_80_sound_multiplexor_is_iret_noop() {
    let mut m = int15_machine(16);
    prime_dos_int_frame(&mut m);
    m.memory.write_u16(0x9000 * 16 + 0x0104, 0x0241).unwrap();
    m.cpu.registers.set_eax(0x8055);
    m.cpu.registers.set_ebx(0x1234);
    m.cpu.registers.set_ecx(0x5678);
    m.cpu.registers.set_edx(0x9ABC);

    m.handle_int1a();

    assert_eq!(m.cpu.registers.eax() as u16, 0x8055, "AX preserved");
    assert_eq!(m.cpu.registers.ebx() as u16, 0x1234, "BX preserved");
    assert_eq!(m.cpu.registers.ecx() as u16, 0x5678, "CX preserved");
    assert_eq!(m.cpu.registers.edx() as u16, 0x9ABC, "DX preserved");
    assert_eq!(dos_int_flags(&m), 0x0241, "FLAGS image preserved");
}

#[test]
fn int15_e801_splits_memory_at_16m() {
    let mut m = int15_machine(24);
    m.cpu.registers.set_eax(0xE801);
    m.handle_int15();
    // 1-16 MB capped at 0x3C00 KB; 8 MB above 16 MB = 128 64KB-blocks = 0x80.
    assert_eq!(m.cpu.registers.eax() as u16, 0x3C00);
    assert_eq!(m.cpu.registers.ebx() as u16, 0x80);
    assert_eq!(m.cpu.registers.ecx() as u16, 0x3C00);
    assert_eq!(m.cpu.registers.edx() as u16, 0x80);
}

#[test]
fn int15_e820_walks_the_memory_map() {
    let mut m = int15_machine(24);
    // ES = 0, DI = 0: the descriptor lands at physical 0 in test RAM.
    let mut ebx = 0u32;
    let mut regions = Vec::new();
    loop {
        m.cpu.registers.set_eax(0xE820);
        m.cpu.registers.set_edx(0x534D_4150);
        m.cpu.registers.set_ecx(20);
        m.cpu.registers.set_ebx(ebx);
        m.handle_int15();
        assert_eq!(m.cpu.registers.eax(), 0x534D_4150);
        assert_eq!(m.cpu.registers.ecx(), 20);
        let base = m.read_guest_dword(0);
        let len = m.read_guest_dword(8);
        let kind = m.read_guest_dword(16);
        regions.push((base, len, kind));
        ebx = m.cpu.registers.ebx();
        if ebx == 0 {
            break;
        }
    }
    assert_eq!(regions.len(), 4);
    assert_eq!(regions[0], (0x0, 0x9_FC00, 1)); // 639 KB conventional (below EBDA)
    assert_eq!(regions[1], (0x9_FC00, 0x400, 2)); // 1 KB EBDA, reserved
    assert_eq!(regions[2], (0xA_0000, 0x6_0000, 2)); // reserved hole
    assert_eq!(regions[3], (0x10_0000, 23 * 0x10_0000, 1)); // extended RAM
}

#[test]
fn int15_c201_reset_reports_present_standard_mouse() {
    // C201 resets the PS/2 mouse: BH=0x00 (standard device id), BL=0xAA (the
    // reset-complete signature drivers probe for), AH=0x00, CF clear.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC201);
    m.cpu.registers.set_ebx(0xFFFF);
    m.handle_int15();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x00AA, "BH=00 BL=AA");
    assert_eq!((m.cpu.registers.eax() as u16 >> 8) as u8, 0x00, "AH=00");
}

#[test]
fn int15_c204_reports_standard_device_type() {
    // C204 get device type: BH=0x00 (standard PS/2 mouse), AH=0x00.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC204);
    m.cpu.registers.set_ebx(0xFF00);
    m.handle_int15();
    assert_eq!((m.cpu.registers.ebx() as u16 >> 8) as u8, 0x00, "BH=00");
    assert_eq!((m.cpu.registers.eax() as u16 >> 8) as u8, 0x00, "AH=00");
}

#[test]
fn int15_c206_status_describes_an_enabled_mouse() {
    // C206 BH=00 returns the three status bytes. BL bit5 = mouse enabled.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC206);
    m.cpu.registers.set_ebx(0x0000); // BH=00
    m.handle_int15();
    assert_eq!(m.cpu.registers.ebx() as u8 & 0x20, 0x20, "BL bit5 enabled");
}

#[test]
fn int15_c207_set_handler_stores_pointer_and_succeeds() {
    // C207 (set device handler) registers the ES:BX far pointer in the EBDA and
    // returns success (AH=0, CF clear). The stored pointer is the one the BIOS
    // INT 74h ISR far-calls on each completed PS/2 packet.
    let mut m = int15_machine(16);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0xABCD));
    m.cpu.registers.set_ebx(0x0042);
    m.cpu.registers.set_eax(0xC207);
    m.handle_int15();
    assert_eq!(
        (m.cpu.registers.eax() as u16 >> 8) as u8,
        0x00,
        "AH=0 success"
    );
    let base = (u32::from(EBDA_SEGMENT) << 4) + EBDA_MOUSE_HANDLER_OFF;
    assert_eq!(read_u16(&mut m, base), 0x0042);
    assert_eq!(read_u16(&mut m, base + 2), 0xABCD);
}

#[test]
fn int15_c208_still_reports_unsupported() {
    // C208 (read raw device port) has no wired path: AH=0x86 unsupported.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC208);
    m.handle_int15();
    assert_eq!(
        (m.cpu.registers.eax() as u16 >> 8) as u8,
        0x86,
        "AH=86 unsupported"
    );
}

#[test]
fn int15_e820_rejects_a_bad_smap_signature() {
    let mut m = int15_machine(24);
    m.cpu.registers.set_eax(0xE820);
    m.cpu.registers.set_edx(0); // not 'SMAP'
    m.cpu.registers.set_ecx(20);
    m.handle_int15();
    // EAX must not be rewritten to 'SMAP' when the call is rejected.
    assert_ne!(m.cpu.registers.eax(), 0x534D_4150);
}

#[test]
fn int14_status_reports_uart_registers() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0300); // AH=03h read status
    m.cpu.registers.set_edx(0); // COM1
    m.handle_int14();
    // LSR reads 0x60 (THRE|TEMT) on the idle UART; MSR reads 0x00.
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x60,
        "line status in AH"
    );
    assert_eq!(m.cpu.registers.eax() as u8, 0x00, "modem status in AL");
}

#[test]
fn int14_send_writes_a_byte_to_the_uart() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0158); // AH=01h send AL='X'
    m.cpu.registers.set_edx(0);
    m.handle_int14();
    assert_eq!(
        m.serial.output(),
        b"X",
        "byte reached the UART capture sink"
    );
    // THRE is always set, so the send succeeds with bit7 clear.
    assert_eq!((m.cpu.registers.eax() >> 8) as u8 & 0x80, 0, "no timeout");
}

#[test]
fn int14_extended_initialize_programs_uart_format_and_divisor() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0401); // AH=04h, no break
    m.cpu.registers.set_ebx(0x0201); // even parity, two stop bits
    m.cpu.registers.set_ecx(0x0308); // 8 data bits, 19200 baud
    m.cpu.registers.set_edx(0); // COM1
    m.handle_int14();

    let lcr = m.serial.read_port(0x03fb).unwrap();
    assert_eq!(lcr, 0x1f, "8E2 line format");
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x60, "LSR in AH");
    m.serial.write_port(0x03fb, lcr | 0x80); // DLAB on
    assert_eq!(m.serial.read_port(0x03f8).unwrap(), 6, "DLL for 19200");
    assert_eq!(m.serial.read_port(0x03f9).unwrap(), 0, "DLM for 19200");
    m.serial.write_port(0x03fb, lcr);
}

#[test]
fn int14_modem_control_read_write_round_trips() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0501); // AH=05h, AL=01h write MCR
    m.cpu.registers.set_ebx(0x0013); // DTR|RTS|LOOP
    m.cpu.registers.set_edx(0);
    m.handle_int14();

    assert_eq!(m.serial.read_port(0x03fc).unwrap(), 0x13);

    m.cpu.registers.set_eax(0x0500); // AH=05h, AL=00h read MCR
    m.cpu.registers.set_ebx(0xAA00);
    m.cpu.registers.set_edx(0);
    m.handle_int14();

    assert_eq!(m.cpu.registers.ebx() as u16, 0xAA13);
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00);
}

#[test]
fn int14_unwired_port_times_out() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0300);
    // INT 14h only services COM1 (DX=0); the COM2 hardware exists but the
    // BIOS service does not drive it, so DX=1 reads as a timeout.
    m.cpu.registers.set_edx(1);
    m.handle_int14();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8 & 0x80,
        0x80,
        "timeout bit set"
    );
}

#[test]
fn int14_fossil_services_use_uart_and_bios_state() {
    let mut m = int15_machine(16);

    m.cpu.registers.set_eax(0x0601);
    m.cpu.registers.set_edx(0);
    m.handle_int14();
    assert_ne!(m.serial.read_port(0x03fc).unwrap() & 0x01, 0, "DTR raised");

    m.cpu.registers.set_eax(0x0600);
    m.cpu.registers.set_edx(0);
    m.handle_int14();
    assert_eq!(m.serial.read_port(0x03fc).unwrap() & 0x01, 0, "DTR lowered");

    m.cpu.registers.set_eax(0x0400);
    m.cpu.registers.set_ebx(0x4F50);
    m.cpu.registers.set_edx(0);
    m.handle_int14();
    assert_eq!(m.cpu.registers.eax() as u16, 0x1954);
    assert_eq!(m.cpu.registers.ebx() as u16, 0x001B);
    assert_ne!(m.serial.read_port(0x03fc).unwrap() & 0x01, 0, "DTR raised");

    m.cpu.registers.set_eax(0x0B58);
    m.cpu.registers.set_edx(0);
    m.handle_int14();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0001);
    assert_eq!(m.serial.output(), b"X");

    m.write_guest_block(0x4000, b"yz");
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x400));
    m.cpu.registers.set_edi(0);
    m.cpu.registers.set_ecx(2);
    m.cpu.registers.set_eax(0x1900);
    m.cpu.registers.set_edx(0);
    m.handle_int14();
    assert_eq!(m.cpu.registers.eax() as u16, 2);
    assert_eq!(m.serial.output(), b"Xyz");

    m.serial.write_port(0x03fc, 0x10);
    m.serial.write_port(0x03f8, b'R');
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x500));
    m.cpu.registers.set_edi(0);
    m.cpu.registers.set_ecx(4);
    m.cpu.registers.set_eax(0x1800);
    m.cpu.registers.set_edx(0);
    m.handle_int14();
    assert_eq!(m.cpu.registers.eax() as u16, 1);
    assert_eq!(m.read_physical_u8(0x5000), b'R');

    m.set_program_stdin(b"k");
    m.cpu.registers.set_eax(0x0D00);
    m.handle_int14();
    assert_eq!(m.cpu.registers.eax() as u16, b'k' as u16);
}

#[test]
fn int14_fossil_screen_and_info_calls_are_minimal_but_stable() {
    let mut m = int15_machine(16);

    m.cpu.registers.set_edx(0x0407);
    m.cpu.registers.set_eax(0x1100);
    m.handle_int14();
    assert_eq!(m.read_guest_word(0x450), 0x0407);

    m.cpu.registers.set_edx(0);
    m.cpu.registers.set_eax(0x1200);
    m.handle_int14();
    assert_eq!(m.cpu.registers.edx() as u16, 0x0407);

    m.cpu.registers.set_eax(0x1541);
    m.handle_int14();
    let cell = (4 * 80 + 7) * 2;
    assert_eq!(m.video.read_u8(cell).unwrap(), b'A');

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x600));
    m.cpu.registers.set_edi(0);
    m.cpu.registers.set_ecx(21);
    m.cpu.registers.set_eax(0x1B00);
    m.cpu.registers.set_edx(0);
    m.handle_int14();
    assert_eq!(m.cpu.registers.eax() as u16, 21);
    assert_eq!(m.memory.read_u16(0x6000).unwrap(), 21);
    assert_eq!(m.read_physical_u8(0x6002), 5);
    assert_eq!(m.read_physical_u8(0x6010), 80);
    assert_eq!(m.read_physical_u8(0x6011), 25);

    m.cpu.registers.set_eax(0x7E42);
    m.cpu.registers.set_ebx(0);
    m.cpu.registers.set_edx(0x1234);
    m.handle_int14();
    assert_eq!(m.cpu.registers.eax() as u16, 0x1954);
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0042);
    assert_eq!(m.cpu.registers.edx() as u16, 0x0034);
}

#[test]
fn int17_print_captures_and_reports_ready() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0050); // AH=00h print AL='P'
    m.cpu.registers.set_edx(0); // LPT1
    m.handle_int17();
    assert_eq!(m.lpt_output(), b"P", "byte reached the LPT capture sink");
    // An always-ready printer reports 0x90: not busy, selected, no error/timeout.
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x90,
        "ready status in AH"
    );
}

#[test]
fn int17_status_reports_ready_printer() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0200); // AH=02h read status
    m.cpu.registers.set_edx(0);
    m.handle_int17();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x90,
        "ready status in AH"
    );
    assert!(m.lpt_output().is_empty(), "status query prints nothing");
}

#[test]
fn bda_seeds_serial_and_parallel_port_bases() {
    let m = int15_machine(16);
    assert_eq!(
        m.memory.read_u16(0x400).unwrap(),
        0x03f8,
        "COM1 base at 0040:0000"
    );
    assert_eq!(
        m.memory.read_u16(0x408).unwrap(),
        0x0378,
        "LPT1 base at 0040:0008"
    );
}

#[test]
fn int15_a20_status_enable_and_disable() {
    let mut m = int15_machine(16);
    // The 8042 output port defaults to A20 on, so status reads enabled.
    m.cpu.registers.set_eax(0x2402);
    m.handle_int15();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
    assert_eq!(m.cpu.registers.eax() as u8, 0x01, "A20 enabled by default");
    // AH=2400h disable.
    m.cpu.registers.set_eax(0x2400);
    m.handle_int15();
    assert!(
        !m.keyboard.a20_enabled(),
        "8042 A20 state off after disable"
    );
    m.cpu.registers.set_eax(0x2402);
    m.handle_int15();
    assert_eq!(m.cpu.registers.eax() as u8, 0x00, "status reports disabled");
    // AH=2401h enable.
    m.cpu.registers.set_eax(0x2401);
    m.handle_int15();
    assert!(m.keyboard.a20_enabled(), "8042 A20 state on after enable");
}

#[test]
fn int15_a20_query_support_reports_both_methods() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x2403);
    m.handle_int15();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
    // Bit 0 keyboard controller, bit 1 port 0x92.
    assert_eq!(
        m.cpu.registers.ebx() as u16,
        0x0003,
        "both A20 methods supported"
    );
}

#[test]
fn port_92_and_int15_a20_stay_coherent() {
    let mut m = int15_machine(16);
    // Disable A20 through the fast-A20 port; it reads back off.
    {
        let mut bus = m.make_bus();
        bus.write_io(0x0092, BusWidth::Byte, 0x00, false).unwrap();
        assert_eq!(
            bus.read_io(0x0092, BusWidth::Byte, 0, false).unwrap(),
            0x00,
            "port 0x92 A20 off"
        );
    }
    assert!(!m.keyboard.a20_enabled(), "8042 agrees A20 is off");
    m.cpu.registers.set_eax(0x2402);
    m.handle_int15();
    assert_eq!(
        m.cpu.registers.eax() as u8,
        0x00,
        "INT 15h status agrees A20 is off"
    );
    // Enable through the port again; bit 1 reads back set.
    {
        let mut bus = m.make_bus();
        bus.write_io(0x0092, BusWidth::Byte, 0x02, false).unwrap();
        assert_eq!(
            bus.read_io(0x0092, BusWidth::Byte, 0, false).unwrap(),
            0x02,
            "port 0x92 A20 on"
        );
    }
    assert!(m.keyboard.a20_enabled(), "8042 agrees A20 is on");
}

#[test]
fn a20_toggle_through_the_run_loop_invalidates_the_decode_cache() {
    // End-to-end check of the A20 -> decode-cache seam: a guest OUT to port 0x92, executed by
    // the real run loop, must advance the CPU's decode generation (so a wrap-region cached
    // decode is dropped). The control program -- identical but a NOP instead of the OUT -- must
    // not advance it, proving the bump comes from the A20 toggle and not incidental run-loop
    // activity. Both spin on JMP $ so the short run never reaches a HLT or a timer interrupt.
    fn gen_after_running(program: &[u8]) -> (bool, u32, u32) {
        let mut m =
            Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), program)
                .unwrap();
        let before = m.cpu.decode_cache_generation();
        m.run_until_halt_or_cycles(1000).unwrap();
        (
            m.keyboard.a20_enabled(),
            before,
            m.cpu.decode_cache_generation(),
        )
    }

    // MOV AL, 0; OUT 0x92, AL; JMP $  -- drives A20 off (port 0x92 bit 1 = 0).
    let (a20, before, after) = gen_after_running(&[0xb0, 0x00, 0xe6, 0x92, 0xeb, 0xfe]);
    assert!(
        !a20,
        "the guest OUT 0x92 toggled A20 off through the run loop"
    );
    assert_ne!(
        after, before,
        "the A20 toggle advanced the decode generation (note_a20_changed fired)"
    );

    // MOV AL, 0; NOP; JMP $  -- no port write, so A20 stays on and the generation is steady.
    let (a20, before, after) = gen_after_running(&[0xb0, 0x00, 0x90, 0xeb, 0xfe]);
    assert!(a20, "control: A20 stays on");
    assert_eq!(
        after, before,
        "control: no A20 toggle, so the decode generation is unchanged by the run"
    );
}

#[test]
fn a20_off_folds_the_hma_onto_low_memory() {
    let mut m = int15_machine(16);
    // A20 is on by default, so 0x0 and 0x100000 are distinct cells.
    {
        let mut bus = m.make_bus();
        bus.write_memory(0x0, BusWidth::Byte, 0xAA, BusAccessKind::DataWrite)
            .unwrap();
        bus.write_memory(0x10_0000, BusWidth::Byte, 0xBB, BusAccessKind::DataWrite)
            .unwrap();
        assert_eq!(
            bus.read_memory(0x10_0000, BusWidth::Byte, BusAccessKind::DataRead)
                .unwrap(),
            0xBB,
            "a distinct extended cell with A20 on"
        );
    }
    // Close the gate: a write to 0x100000 now folds onto 0x0.
    m.keyboard.set_a20(false);
    {
        let mut bus = m.make_bus();
        bus.write_memory(0x10_0000, BusWidth::Byte, 0xCC, BusAccessKind::DataWrite)
            .unwrap();
        assert_eq!(
            bus.read_memory(0x0, BusWidth::Byte, BusAccessKind::DataRead)
                .unwrap(),
            0xCC,
            "the HMA write reached 0x0 through the closed gate"
        );
    }
    // Reopen the gate: the real extended cell was never touched (still 0xBB).
    m.keyboard.set_a20(true);
    {
        let mut bus = m.make_bus();
        assert_eq!(
            bus.read_memory(0x10_0000, BusWidth::Byte, BusAccessKind::DataRead)
                .unwrap(),
            0xBB,
            "the aliased write left the extended cell alone"
        );
    }
}

#[test]
fn unoccupied_upper_memory_reads_open_bus() {
    // 0xC8000-0xEFFFF are the UMB-able holes above the VGA option ROM span
    // and below the system BIOS. Nothing on this machine's default boot
    // claims them, so a probe (JEMMEX and other EMS/UMB managers scan the
    // UMA for a free page frame) must see open bus, not RAM that happens
    // to hold whatever was last written there.
    let mut m = int15_machine(16);
    let mut bus = m.make_bus();
    for addr in [0xC8000u32, 0xC8001, 0xE0000, 0xEFFFF] {
        assert_eq!(
            bus.read_memory(addr, BusWidth::Byte, BusAccessKind::DataRead)
                .unwrap(),
            0xff,
            "address {addr:#08x} must read open bus"
        );
    }
    // A write finds nothing wired to receive it: read-back still 0xFF.
    bus.write_memory(0xD0000, BusWidth::Byte, 0x42, BusAccessKind::DataWrite)
        .unwrap();
    assert_eq!(
        bus.read_memory(0xD0000, BusWidth::Byte, BusAccessKind::DataRead)
            .unwrap(),
        0xff,
        "an open-bus write must not stick"
    );
    // The occupied VGA BIOS span (0xC0000-0xC7FFF) is unaffected: it is
    // genuinely backed and keeps its written content.
    bus.write_memory(0xC5000, BusWidth::Byte, 0x99, BusAccessKind::DataWrite)
        .unwrap();
    assert_eq!(
        bus.read_memory(0xC5000, BusWidth::Byte, BusAccessKind::DataRead)
            .unwrap(),
        0x99,
        "the VGA BIOS span is still flat-RAM-backed, not open bus"
    );
    // The system BIOS ROM shadow at 0xF0000 is unaffected: a write is a
    // silent no-op (ROM), not open bus 0xFF read-back of arbitrary content.
    let before = bus
        .read_memory(0xF0000, BusWidth::Byte, BusAccessKind::DataRead)
        .unwrap();
    bus.write_memory(0xF0000, BusWidth::Byte, !before, BusAccessKind::DataWrite)
        .unwrap();
    assert_eq!(
        bus.read_memory(0xF0000, BusWidth::Byte, BusAccessKind::DataRead)
            .unwrap(),
        before,
        "the BIOS ROM shadow ignores writes and keeps its ROM content"
    );
}

#[test]
fn a20_off_folds_a_split_word_in_the_hma() {
    let mut m = int15_machine(16);
    m.keyboard.set_a20(false);
    let mut bus = m.make_bus();
    // 0x100001 is odd, so the word splits; with the gate closed each byte
    // folds down by 0x100000, landing the pair at 0x1 and 0x2. (The byte just
    // below 1 MiB, 0xFFFFF, is BIOS ROM, so the genuinely straddling write is
    // not observable there; the odd HMA word proves the same split masking.)
    bus.write_memory(0x10_0001, BusWidth::Word, 0xBEEF, BusAccessKind::DataWrite)
        .unwrap();
    assert_eq!(
        bus.read_memory(0x1, BusWidth::Byte, BusAccessKind::DataRead)
            .unwrap(),
        0xEF,
        "low byte folded to 0x1"
    );
    assert_eq!(
        bus.read_memory(0x2, BusWidth::Byte, BusAccessKind::DataRead)
            .unwrap(),
        0xBE,
        "high byte folded to 0x2"
    );
    assert_eq!(
        bus.read_memory(0x10_0001, BusWidth::Word, BusAccessKind::DataRead)
            .unwrap(),
        0xBEEF,
        "the folded word reads back through the HMA alias"
    );
}

#[test]
fn a20_off_folds_a_split_dword_and_reads_back() {
    let mut m = int15_machine(16);
    m.keyboard.set_a20(false);
    let mut bus = m.make_bus();
    // 0x100001 is not 4-aligned, so the dword splits into four bytes, each
    // folding down by 0x100000 to 0x1..0x4.
    bus.write_memory(
        0x10_0001,
        BusWidth::Dword,
        0xDEAD_BEEF,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    // The read side folds too: the dword reads back through the alias.
    assert_eq!(
        bus.read_memory(0x10_0001, BusWidth::Dword, BusAccessKind::DataRead)
            .unwrap(),
        0xDEAD_BEEF,
        "the dword reads back through the HMA alias"
    );
    // The low-memory bytes hold the little-endian image.
    assert_eq!(
        bus.read_memory(0x1, BusWidth::Byte, BusAccessKind::DataRead)
            .unwrap(),
        0xEF,
        "byte 0 folded to 0x1"
    );
    assert_eq!(
        bus.read_memory(0x4, BusWidth::Byte, BusAccessKind::DataRead)
            .unwrap(),
        0xDE,
        "byte 3 folded to 0x4"
    );
}

#[test]
fn a20_on_keeps_a_split_word_in_the_hma() {
    let mut m = int15_machine(16); // A20 on by default
    let mut bus = m.make_bus();
    bus.write_memory(0x10_0001, BusWidth::Word, 0xBEEF, BusAccessKind::DataWrite)
        .unwrap();
    // Low memory is untouched; the word stays at the real HMA cells. Byte
    // 0x1 is IVT[0]'s offset high byte, seeded to the per-vector ROM stub
    // (bios_int_stub_off(0) = 0x0200 -> high byte 0x02).
    assert_eq!(
        bus.read_memory(0x1, BusWidth::Byte, BusAccessKind::DataRead)
            .unwrap(),
        u32::from(bios_int_stub_off(0) >> 8),
        "0x1 untouched with A20 on"
    );
    assert_eq!(
        bus.read_memory(0x10_0001, BusWidth::Word, BusAccessKind::DataRead)
            .unwrap(),
        0xBEEF,
        "the word stayed in the HMA"
    );
}

#[test]
fn int2f_idle_yield_reports_supported() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xCAFE_1680);

    assert!(m.handle_int2f(), "AX=1680h handled");

    assert_eq!(m.cpu.registers.eax(), 0xCAFE_1600);
}

#[test]
fn int2f_windows_install_probe_reports_plain_dos() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xCAFE_1600);
    m.cpu.registers.set_ebx(0x1111_2222);

    assert!(m.handle_int2f(), "AX=1600h handled");

    assert_eq!(m.cpu.registers.eax(), 0xCAFE_1600);
    assert_eq!(m.cpu.registers.ebx(), 0x1111_2222);
}

#[test]
fn int2f_dpmi_probes_report_absent() {
    for ax in [0x1686u16, 0x1687] {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));
        m.cpu.registers.set_ebx(0x1111_2222);

        assert!(m.handle_int2f(), "AX={ax:04X}h handled");

        assert_eq!(m.cpu.registers.eax(), 0xCAFE_0001);
        assert_eq!(m.cpu.registers.ebx(), 0x1111_2222);
    }
}

#[test]
fn int2f_dos_install_probes_report_not_installed() {
    for (ax, name) in [
        (0x0100u16, "PRINT"),
        (0x0500, "critical-error helper"),
        (0x0600, "ASSIGN"),
        (0x1000, "SHARE"),
        (0x1400, "NLSFUNC"),
        (0x2300, "DR DOS GRAFTABL"),
        (0x2E00, "Novell GRAFTABL"),
        (0x6400, "SCRNSAV2"),
        (0x7A00, "NetWare"),
        (0xAA00, "VIDCLOCK"),
        (0xAD00, "DISPLAY.SYS"),
        (0xB000, "GRAFTABL"),
        (0xB700, "APPEND"),
        (0xF700, "AUTOPARK"),
    ] {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));
        m.cpu.registers.set_ebx(0x1111_2222);
        m.cpu.registers.set_ecx(0x3333_4444);
        m.cpu.registers.set_edx(0x5555_6666);

        assert!(m.handle_int2f(), "{name} install check handled");

        assert_eq!(m.cpu.registers.eax() as u8, 0x00, "{name} not installed");
        if matches!(ax, 0x0600 | 0x2300 | 0x2E00 | 0xB700) {
            assert_eq!(
                (m.cpu.registers.eax() as u16) >> 8,
                0x00,
                "{name} also clears AH"
            );
        }
        assert_eq!(m.cpu.registers.ebx(), 0x1111_2222);
        assert_eq!(m.cpu.registers.ecx(), 0x3333_4444);
        assert_eq!(m.cpu.registers.edx(), 0x5555_6666);
    }

    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xCAFE_B800);
    m.cpu.registers.set_ebx(0x1111_2222);

    assert!(m.handle_int2f(), "network install check handled");

    assert_eq!(m.cpu.registers.eax(), 0xCAFE_0000);
    assert_eq!(m.cpu.registers.ebx(), 0x1111_0000);

    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xCAFE_0601);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x3333));

    assert!(m.handle_int2f(), "ASSIGN work-area query handled");

    assert_eq!(m.cpu.registers.segment(SegmentIndex::Es).selector, 0);

    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xCAFE_B803);
    m.cpu.registers.set_ebx(0xAAAA_5555);

    assert!(m.handle_int2f(), "network post-address read handled");

    assert_eq!(m.cpu.registers.segment(SegmentIndex::Es).selector, 0);
    assert_eq!(m.cpu.registers.ebx(), 0xAAAA_0000);

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4567));
    m.cpu.registers.set_ebx(0xBBBB_1234);
    m.cpu.registers.set_eax(0xCAFE_B804);

    assert!(m.handle_int2f(), "network post-address set handled");

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0));
    m.cpu.registers.set_ebx(0xCCCC_0000);
    m.cpu.registers.set_eax(0xCAFE_B803);

    assert!(
        m.handle_int2f(),
        "network post-address read after set handled"
    );

    assert_eq!(m.cpu.registers.segment(SegmentIndex::Es).selector, 0x4567);
    assert_eq!(m.cpu.registers.ebx(), 0xCCCC_1234);

    for ax in 0x0101u16..=0x0105 {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));

        assert!(m.handle_int2f(), "PRINT AX={ax:04X}h handled");

        assert_eq!(m.cpu.registers.eax(), 0xCAFE_0001);
        assert_ne!(dos_int_flags(&m) & 1, 0, "PRINT service sets CF");
    }

    for ax in [0x0501u16, 0x05ff] {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));

        assert!(m.handle_int2f(), "critical-error AX={ax:04X}h handled");

        assert_eq!(m.cpu.registers.eax(), 0xCAFE_0001);
        assert_ne!(
            dos_int_flags(&m) & 1,
            0,
            "critical-error helper service sets CF"
        );
    }

    for ax in [0x1401u16, 0x1402, 0x1403, 0x1404, 0x14FE, 0x14FF] {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));

        assert!(m.handle_int2f(), "NLSFUNC AX={ax:04X}h handled");

        assert_eq!(
            m.cpu.registers.eax(),
            0xCAFE_1401,
            "absent NLSFUNC service reports DOS error 1 in AL"
        );
    }

    for ax in [0xB001u16, 0x2301, 0x2E01] {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));

        assert!(m.handle_int2f(), "GRAFTABL AX={ax:04X}h handled");

        if ax == 0xB001 {
            assert_eq!(
                m.cpu.registers.eax() as u8,
                0x00,
                "MS-DOS GRAFTABL data call does not claim a font table"
            );
        } else {
            assert_eq!(
                (m.cpu.registers.eax() as u16) >> 8,
                0x00,
                "DR/Novell GRAFTABL data call reports not installed"
            );
        }
    }

    for ax in [0xB701u16, 0xB702, 0xB809, 0xF701] {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));

        assert!(m.handle_int2f(), "AX={ax:04X}h absent service handled");

        assert_eq!(m.cpu.registers.eax(), 0xCAFE_0001);
        assert_ne!(dos_int_flags(&m) & 1, 0, "absent service sets CF");
    }
}

#[test]
fn int2f_absent_redirector_calls_fail_or_noop() {
    for ax in [
        0x1101u16, 0x1102, 0x1103, 0x1104, 0x1105, 0x1106, 0x1107, 0x1108, 0x1109, 0x110A, 0x110B,
        0x110C, 0x110D, 0x110E, 0x110F, 0x1110, 0x1111, 0x1112, 0x1113, 0x1114, 0x1115, 0x1116,
        0x1117, 0x1118, 0x1119, 0x111A, 0x111B, 0x111C, 0x111E, 0x111F, 0x1121, 0x1123, 0x1124,
        0x1125, 0x1126, 0x1127, 0x1128, 0x1129, 0x112A, 0x112B, 0x112C, 0x112D, 0x112E, 0x112F,
    ] {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));
        m.cpu.registers.set_ebx(0x1111_2222);

        assert!(m.handle_int2f(), "AX={ax:04X}h handled");

        assert_eq!(m.cpu.registers.eax(), 0xCAFE_0001);
        assert_eq!(m.cpu.registers.ebx(), 0x1111_2222);
        let ss = m.cpu.registers.segment(SegmentIndex::Ss).base;
        let sp = m.cpu.registers.esp() as u16;
        let flags = m
            .memory
            .read_u16((ss + u32::from(sp.wrapping_add(4))) as usize)
            .unwrap();
        assert_ne!(flags & 0x0001, 0, "CF set");
    }

    for ax in [0x111Du16, 0x1122] {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));

        assert!(m.handle_int2f(), "AX={ax:04X}h handled");

        assert_eq!(m.cpu.registers.eax(), 0xCAFE_0000 | u32::from(ax));
        assert_ne!(dos_int_flags(&m) & 1, 0, "notify hook leaves flags alone");
    }

    let mut m = int15_machine(16);
    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0xCAFE_1120);

    assert!(m.handle_int2f(), "AX=1120h handled");

    assert_eq!(m.cpu.registers.eax(), 0xCAFE_1120);
    assert_eq!(dos_int_flags(&m) & 1, 0, "flush hook clears CF");
}

#[test]
fn int2f_disk_handler_hook_returns_previous_vectors() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xCAFE_1300);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x1111));
    m.cpu.registers.set_edx(0xAAAA_2222);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x3333));
    m.cpu.registers.set_ebx(0xBBBB_4444);

    assert!(m.handle_int2f(), "AH=13h first call handled");
    // The defaults are INT 13h's own per-vector stub (serviced by address
    // on every arrival route), not the legacy shared FF00:0000.
    assert_eq!(
        m.cpu.registers.segment(SegmentIndex::Ds).selector,
        BIOS_ROM_IRET_SEG
    );
    assert_eq!(
        m.cpu.registers.edx(),
        0xAAAA_0000 | u32::from(bios_int_stub_off(0x13))
    );
    assert_eq!(
        m.cpu.registers.segment(SegmentIndex::Es).selector,
        BIOS_ROM_IRET_SEG
    );
    assert_eq!(
        m.cpu.registers.ebx(),
        0xBBBB_0000 | u32::from(bios_int_stub_off(0x13))
    );

    m.cpu.registers.set_eax(0xCAFE_1301);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x5555));
    m.cpu.registers.set_edx(0xCCCC_6666);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x7777));
    m.cpu.registers.set_ebx(0xDDDD_8888);

    assert!(m.handle_int2f(), "AH=13h second call handled");
    assert_eq!(m.cpu.registers.segment(SegmentIndex::Ds).selector, 0x1111);
    assert_eq!(m.cpu.registers.edx(), 0xCCCC_2222);
    assert_eq!(m.cpu.registers.segment(SegmentIndex::Es).selector, 0x3333);
    assert_eq!(m.cpu.registers.ebx(), 0xDDDD_4444);
}

#[test]
fn int2f_cdrom_reserved_debug_toggles_are_noops() {
    for ax in [0x1506u16, 0x1507] {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));
        m.cpu.registers.set_ebx(0x1111_2222);
        m.cpu.registers.set_ecx(0x3333_4444);
        m.cpu.registers.set_edx(0x5555_6666);

        assert!(m.handle_int2f(), "AX={ax:04X}h handled");

        assert_eq!(m.cpu.registers.eax(), 0xCAFE_0000 | u32::from(ax));
        assert_eq!(m.cpu.registers.ebx(), 0x1111_2222);
        assert_eq!(m.cpu.registers.ecx(), 0x3333_4444);
        assert_eq!(m.cpu.registers.edx(), 0x5555_6666);
    }
}

#[test]
fn int1a_set_and_read_date_round_trips() {
    let mut m = int15_machine(16);
    // AH=05h set date: CH/CL century/year BCD, DH/DL month/day BCD -> 2021-07-15.
    m.cpu.registers.set_eax(0x0500);
    m.cpu.registers.set_ecx(0x2021);
    m.cpu.registers.set_edx(0x0715);
    m.handle_int1a();
    // AH=04h read date back.
    m.cpu.registers.set_eax(0x0400);
    m.handle_int1a();
    assert_eq!(m.cpu.registers.ecx() as u16, 0x2021);
    assert_eq!(m.cpu.registers.edx() as u16, 0x0715);
}

#[test]
fn int1a_date_persists_a_non_default_century() {
    let mut m = int15_machine(16);
    // AH=05h set date to 1999-12-31 (CH=century 0x19, CL=year 0x99).
    m.cpu.registers.set_eax(0x0500);
    m.cpu.registers.set_ecx(0x1999);
    m.cpu.registers.set_edx(0x1231);
    m.handle_int1a();
    // The century reached CMOS 0x32 (binary 19), not just the in-memory year.
    assert_eq!(m.rtc.century(), 19, "century persisted to CMOS 0x32");
    // AH=04h reads the full BCD date back through the century accessor.
    m.cpu.registers.set_eax(0x0400);
    m.handle_int1a();
    assert_eq!(
        m.cpu.registers.ecx() as u16,
        0x1999,
        "century and year round-trip"
    );
    assert_eq!(m.cpu.registers.edx() as u16, 0x1231);
}

#[test]
fn int1a_set_and_read_time_round_trips() {
    let mut m = int15_machine(16);
    // AH=03h set time: CH/CL hours/minutes BCD, DH seconds BCD -> 13:45:30.
    m.cpu.registers.set_eax(0x0300);
    m.cpu.registers.set_ecx(0x1345);
    m.cpu.registers.set_edx(0x3000);
    m.handle_int1a();
    m.cpu.registers.set_eax(0x0200);
    m.handle_int1a();
    assert_eq!(m.cpu.registers.ecx() as u16, 0x1345);
    assert_eq!((m.cpu.registers.edx() as u16) >> 8, 0x30);
}

#[test]
fn int1a_day_counter_matches_calendar() {
    let mut m = int15_machine(16);
    // 1980-01-02 is day 1 since the 1980-01-01 epoch.
    m.cpu.registers.set_eax(0x0500);
    m.cpu.registers.set_ecx(0x1980);
    m.cpu.registers.set_edx(0x0102);
    m.handle_int1a();
    m.cpu.registers.set_eax(0x0A00);
    m.handle_int1a();
    assert_eq!(m.cpu.registers.ecx() as u16, 1);
}

#[test]
fn days_since_1980_handles_leap_years() {
    assert_eq!(days_since_1980(1980, 1, 1), 0);
    assert_eq!(days_since_1980(1980, 3, 1), 60); // 1980 is a leap year (31+29)
    assert_eq!(days_since_1980(1981, 1, 1), 366);
}

#[test]
fn int1a_set_day_counter_round_trips() {
    let mut m = int15_machine(16);
    // AH=0Bh latches CX into the BDA scratch word; it reads back unchanged.
    m.cpu.registers.set_eax(0x0B00);
    m.cpu.registers.set_ecx(0x1234);
    m.handle_int1a();
    assert_eq!(m.memory.read_u16(BDA_DAY_COUNT).unwrap(), 0x1234);
    // CF clear: the call succeeded.
    let ss = m.cpu.registers.segment(SegmentIndex::Ss).base;
    let sp = m.cpu.registers.esp() as u16;
    let flags = m
        .memory
        .read_u16((ss + u32::from(sp.wrapping_add(4))) as usize)
        .unwrap();
    assert_eq!(flags & 0x0001, 0, "CF clear");
}

#[test]
fn int13_drive_parameters_report_real_floppy_count() {
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 1_474_560]).unwrap(); // 1.44 MB
    m.cpu.registers.set_eax(0x0800);
    m.cpu.registers.set_edx(0x0000); // DL=0 drive A:
    m.handle_int13();
    // One drive is mounted: DL reports 1, derived from the equipment word.
    assert_eq!(m.cpu.registers.edx() as u8, 0x01, "DL = floppy count");
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH = success");
}

#[test]
fn int13_read_over_executed_buffer_invalidates_decoded_bytes() {
    let rom = rom_with_code(&[
        0x31, 0xC0, // xor ax,ax
        0x8E, 0xD0, // mov ss,ax
        0xBC, 0x00, 0x70, // mov sp,7000h
        0x9A, 0x00, 0x7C, 0x00, 0x00, // call far 0000:7C00
        0x31, 0xC0, // xor ax,ax
        0x8E, 0xC0, // mov es,ax
        0xBB, 0x00, 0x7C, // mov bx,7C00h
        0xB8, 0x01, 0x02, // mov ax,0201h
        0xB9, 0x01, 0x00, // mov cx,0001h
        0x31, 0xD2, // xor dx,dx
        0xCD, 0x13, // int 13h
        0xEA, 0x00, 0x7C, 0x00, 0x00, // jmp far 0000:7C00
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.write_guest_block(0x7C00, &[0xB8, 0xAA, 0xAA, 0xCB]); // mov ax,AAAAh; retf

    let mut image = vec![0u8; 1_474_560];
    image[..5].copy_from_slice(&[0xFA, 0xB8, 0x34, 0x12, 0xF4]); // cli; mov ax,1234h; hlt
    machine.mount_floppy(image).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu.registers.eax() as u16, 0x1234);
}

#[test]
fn int13_drive_parameters_reject_fixed_disk() {
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 1_474_560]).unwrap();
    m.cpu.registers.set_eax(0x0800);
    m.cpu.registers.set_edx(0x0080); // DL=0x80 fixed disk, none modeled
    m.handle_int13();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x80,
        "AH = timeout/no drive"
    );
}

#[test]
fn int13_dasd_type_honors_drive_presence() {
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 1_474_560]).unwrap();
    // DL=0 with a floppy mounted: AH=01 (floppy, no change line), CF clear.
    m.cpu.registers.set_eax(0x1500);
    m.cpu.registers.set_edx(0x0000);
    m.handle_int13();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x01,
        "AH = floppy, no change line"
    );
    // DL=1 is an absent second floppy: AH=00 (no such drive).
    m.cpu.registers.set_eax(0x1500);
    m.cpu.registers.set_edx(0x0001);
    m.handle_int13();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x00,
        "AH = no such drive"
    );
}

#[test]
fn bda_seeds_serial_parallel_and_video_state() {
    let m = int15_machine(16);
    // Serial/parallel base tables: COM1 + COM2 and LPT1 + LPT2 are wired.
    assert_eq!(m.memory.read_u16(0x400).unwrap(), 0x03f8); // COM1
    assert_eq!(m.memory.read_u16(0x402).unwrap(), 0x02f8); // COM2
    assert_eq!(m.memory.read_u16(0x408).unwrap(), 0x0378); // LPT1
    assert_eq!(m.memory.read_u16(0x40a).unwrap(), 0x0278); // LPT2
    // Timeout tables across all four ports each.
    assert_eq!(m.memory.read_u8(0x47f).unwrap(), 0x01); // COM4 timeout
    assert_eq!(m.memory.read_u8(0x47b).unwrap(), 0x14); // LPT4 timeout
    // Static video-state block and the system flags.
    assert_eq!(m.memory.read_u16(0x44c).unwrap(), 0x1000); // regen page size
    assert_eq!(m.memory.read_u8(0x485).unwrap(), 16); // char cell height
    assert_eq!(m.memory.read_u8(0x487).unwrap(), 0x60); // EGA/VGA video-control byte
    assert_eq!(m.memory.read_u8(0x489).unwrap(), 0x51); // EGA/VGA mode-set control
    assert_eq!(m.memory.read_u8(0x48A).unwrap(), 0x08); // VGA colour DCC
    assert_eq!(m.memory.read_u8(0x475).unwrap(), 0); // no fixed disks
    assert_eq!(m.memory.read_u16(0x472).unwrap(), 0x1234); // warm-boot magic
}

#[test]
fn com2_scratch_round_trips_through_the_bus() {
    // A write then read of the COM2 scratch register (0x2FF) routes through the
    // serial2 port arm exactly the way COM1's (0x3FF) does.
    let mut m = int15_machine(16);
    let mut bus = m.make_bus();
    bus.write_io(0x02ff, BusWidth::Byte, 0xa5, false).unwrap();
    assert_eq!(bus.read_io(0x02ff, BusWidth::Byte, 0, false).unwrap(), 0xa5);
    // COM1 stays separate: writing COM2 did not disturb COM1's scratch.
    assert_eq!(bus.read_io(0x03ff, BusWidth::Byte, 0, false).unwrap(), 0x00);
}

#[test]
fn lpt2_data_round_trips_through_the_bus() {
    // The LPT2 data latch at 0x278 reads back through the lpt2 port arm.
    let mut m = int15_machine(16);
    let mut bus = m.make_bus();
    bus.write_io(0x0278, BusWidth::Byte, 0x42, false).unwrap();
    assert_eq!(bus.read_io(0x0278, BusWidth::Byte, 0, false).unwrap(), 0x42);
    // The LPT2 status port reports the always-ready idle byte.
    assert_eq!(bus.read_io(0x0279, BusWidth::Byte, 0, false).unwrap(), 0xdf);
}

#[test]
fn game_port_reports_no_joystick() {
    // Port 0x201: a routine joystick probe (OUT to fire the one-shots, then
    // IN) must see the absent-joystick byte -- axis bits 0-3 clear (timers
    // already expired), button bits 4-7 set (open switches, active-low) --
    // not an UnsupportedPort fault that halts the machine.
    let mut m = int15_machine(16);
    let mut bus = m.make_bus();
    assert_eq!(bus.read_io(0x0201, BusWidth::Byte, 0, false).unwrap(), 0xf0);
    bus.write_io(0x0201, BusWidth::Byte, 0xff, false).unwrap();
    assert_eq!(bus.read_io(0x0201, BusWidth::Byte, 0, false).unwrap(), 0xf0);
    // The ISA gameport decodes 0x200-0x207 as aliases of the one register;
    // TSUMERA probes 0x200. Both ends of the range answer, IN and OUT.
    for port in [0x0200, 0x0207] {
        bus.write_io(port, BusWidth::Byte, 0xff, false).unwrap();
        assert_eq!(bus.read_io(port, BusWidth::Byte, 0, false).unwrap(), 0xf0);
    }
}

#[test]
fn cms_probe_range_reads_open_bus_not_a_fault() {
    // Ports 0x280-0x28F are the C/MS Game Blaster's alternate probe base.
    // With no card there, a read must see open bus (0xFF) so a sound-detect
    // routine concludes "nothing present" -- not an UnsupportedPort fault
    // that halts the machine headless. Prince of Persia (PRINCE ADLIB) reads
    // 0x283 during its scan; regression guard for the passive-port entry.
    let mut m = int15_machine(16);
    let mut bus = m.make_bus();
    for port in [0x0280u16, 0x0283, 0x028f] {
        assert_eq!(
            bus.read_io(port, BusWidth::Byte, 0, false).unwrap(),
            0xff,
            "port {port:#06x} must read open bus"
        );
    }
    // The stub stays bounded: one past the top still faults, so genuinely
    // unclaimed ISA reads elsewhere keep surfacing as real faults.
    assert!(matches!(
        bus.read_io(0x0290, BusWidth::Byte, 0, false),
        Err(BusError::UnsupportedPort { port }) if port == 0x0290
    ));
}

#[test]
fn vmware_backdoor_probe_reads_open_bus_not_a_fault() {
    // Port 0x5658 is the VMware backdoor detection port: real VMware sets
    // EAX/EBX/ECX/EDX on `IN EAX, DX` (DX=0x5658, EAX='VMXh'); real,
    // non-VMware hardware has nothing there, so the guest must see open
    // bus (all-ones) and conclude "not VMware" -- not an UnsupportedPort
    // fault that halts the machine. JEMMEX runs this probe and used to
    // crash with CpuError("unsupported I/O port 0x5658") before this stub
    // existed; regression guard for the passive-port entry.
    let mut m = int15_machine(16);
    let mut bus = m.make_bus();
    assert_eq!(
        bus.read_io(0x5658, BusWidth::Dword, 0, false).unwrap(),
        0xffff_ffff,
        "VMware backdoor port must read open bus on a dword IN, not the VMXh response"
    );
    for port in [0x5658u16, 0x5659, 0x565a, 0x565b] {
        assert_eq!(
            bus.read_io(port, BusWidth::Byte, 0, false).unwrap(),
            0xff,
            "port {port:#06x} must read open bus"
        );
    }
    // OUT is accepted, matching every other passive stub (the generic
    // passive-port table is a plain read/write latch with no VMware
    // magic-number behavior grafted on).
    bus.write_io(0x5658, BusWidth::Dword, 0x564d_5868, false)
        .unwrap();
    // The stub stays bounded: one past the top still faults.
    assert!(matches!(
        bus.read_io(0x565c, BusWidth::Byte, 0, false),
        Err(BusError::UnsupportedPort { port }) if port == 0x565c
    ));
}

#[test]
fn int11_equipment_word_tracks_floppy_mount() {
    let mut m = int15_machine(16);
    // Mounting sets the floppy-installed bit; ejecting clears the floppy field.
    m.mount_floppy(vec![0u8; 1_474_560]).unwrap();
    m.cpu.registers.set_eax(0);
    m.handle_int11();
    assert_eq!(m.cpu.registers.eax() as u16 & 0x0001, 0x0001);
    m.eject_floppy();
    m.cpu.registers.set_eax(0);
    m.handle_int11();
    assert_eq!(m.cpu.registers.eax() as u16 & 0x00C1, 0x0000);
}

#[test]
fn int10_display_detection_tracks_color_and_mono_crtc() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x1A00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x1A); // AL = function supported
    assert_eq!(m.cpu.registers.ebx() as u8, 0x08); // BL = VGA colour DCC
    assert_eq!(m.memory.read_u8(0x48A).unwrap(), 0x08);

    m.cpu.registers.set_eax(0x1A01);
    m.cpu.registers.set_ebx(0x000A);
    m.handle_int10();
    assert_eq!(m.memory.read_u8(0x48A).unwrap(), 0x0A);
    m.cpu.registers.set_eax(0x1A00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u8, 0x0A);

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0010);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0003); // colour, 256 KiB
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0f09); // feature bits, switch setting

    m.cpu.registers.set_eax(0x0007);
    m.handle_int10();
    m.cpu.registers.set_eax(0x1A00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u8, 0x07); // BL = VGA mono DCC
    assert_eq!(m.memory.read_u8(0x48A).unwrap(), 0x07);
    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0010);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0103); // mono, 256 KiB
}

#[test]
fn int10_1232_toggles_video_addressing() {
    let mut m = int15_machine(16);
    m.write_physical_u8(VGA_TEXT_BASE, b'T');
    assert_eq!(m.read_physical_u8(VGA_TEXT_BASE), b'T');

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0032);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(m.video().video_subsystem_enabled());
    assert!(!m.video().video_memory_enabled());

    m.write_physical_u8(VGA_TEXT_BASE, b'R');
    assert_eq!(m.read_physical_u8(VGA_TEXT_BASE), b'R');
    {
        let mut bus = m.make_bus();
        assert_eq!(bus.read_io(0x3C3, BusWidth::Byte, 0, false).unwrap(), 1);
        assert_eq!(
            bus.read_io(0x3CC, BusWidth::Byte, 0, false).unwrap() & 0x02,
            0
        );
    }

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0032);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(m.video().video_subsystem_enabled());
    assert!(m.video().video_memory_enabled());
    assert_eq!(m.read_physical_u8(VGA_TEXT_BASE), b'T');

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0032);
    m.handle_int10();
    assert!(!m.video().video_memory_enabled());
    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();
    assert!(m.video().video_memory_enabled());
}

#[test]
fn int10_1230_selects_text_scanlines_on_next_mode_set() {
    let mut m = int15_machine(16);
    assert_eq!(m.read_physical_u8(0x489) & 0x90, 0x10); // POST default: 400 lines
    assert_eq!(m.read_physical_u8(0x488) & 0x0F, 0x09);

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0030);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert_eq!(m.read_physical_u8(0x489) & 0x90, 0x80);
    assert_eq!(m.read_physical_u8(0x488) & 0x0F, 0x08);
    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0010);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0f08);
    m.cpu.registers.set_eax(0x0003);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x485), 8);
    assert_eq!(m.video().raster_height(), 262);

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0030);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x489) & 0x90, 0x00);
    assert_eq!(m.read_physical_u8(0x488) & 0x0F, 0x09);
    m.cpu.registers.set_eax(0x0003);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x485), 14);
    assert_eq!(m.video().raster_width(), 720);

    m.cpu.registers.set_eax(0x1202);
    m.cpu.registers.set_ebx(0x0030);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x489) & 0x90, 0x10);
    assert_eq!(m.read_physical_u8(0x488) & 0x0F, 0x09);
    m.cpu.registers.set_eax(0x0003);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x485), 16);
    assert_eq!(m.video().raster_width(), 720);
}

#[test]
fn int10_1231_toggles_default_palette_loading_on_mode_set() {
    let mut m = int15_machine(16);
    m.video_mut().set_dac_entry(5, 1, 2, 3);
    m.video_mut().set_attr_palette_reg(1, 0x2A);
    m.video_mut().write_port(0x3C6, 0x0F);

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0031);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(!m.video().default_palette_loading_enabled());
    assert_eq!(m.read_physical_u8(0x489) & 0x08, 0x08);

    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();
    assert_eq!(m.video().dac_entry(5), [1, 2, 3]);
    assert_eq!(m.video().attr_palette_reg(1), 0x2A);
    assert_eq!(m.video_mut().read_port(0x3C6), Some(0x0F));

    m.video_mut().set_dac_entry(5, 1, 2, 3);
    m.video_mut().set_attr_palette_reg(1, 0x2A);
    m.video_mut().write_port(0x3C6, 0x0F);
    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0031);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(m.video().default_palette_loading_enabled());
    assert_eq!(m.read_physical_u8(0x489) & 0x08, 0x00);

    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();
    assert_eq!(m.video().dac_entry(5), [0x2A, 0x00, 0x2A]);
    assert_eq!(m.video().attr_palette_reg(1), 1);
    assert_eq!(m.video_mut().read_port(0x3C6), Some(0xFF));
}

#[test]
fn int10_1233_toggles_grayscale_summing_for_dac_loads() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0033);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(m.video().grayscale_summing_enabled());
    assert_eq!(m.read_physical_u8(0x489) & 0x02, 0x02);

    m.cpu.registers.set_eax(0x1010);
    m.cpu.registers.set_ebx(5);
    m.cpu.registers.set_edx(63 << 8); // DH = red
    m.cpu.registers.set_ecx(0); // CH/CL = green/blue
    m.handle_int10();
    assert_eq!(m.video().dac_entry(5), [18, 18, 18]);

    m.video_mut().write_port(0x3C8, 6);
    m.video_mut().write_port(0x3C9, 0);
    m.video_mut().write_port(0x3C9, 63);
    m.video_mut().write_port(0x3C9, 0);
    assert_eq!(m.video().dac_entry(6), [37, 37, 37]);

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0033);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(!m.video().grayscale_summing_enabled());
    assert_eq!(m.read_physical_u8(0x489) & 0x02, 0x00);

    m.cpu.registers.set_eax(0x1010);
    m.cpu.registers.set_ebx(7);
    m.cpu.registers.set_edx(0);
    m.cpu.registers.set_ecx(63 << 8);
    m.handle_int10();
    assert_eq!(m.video().dac_entry(7), [0, 63, 0]);
}

#[test]
fn int10_1234_toggles_cursor_emulation_without_disturbing_mode_set_bits() {
    let mut m = int15_machine(16);
    assert_eq!(m.read_physical_u8(0x489) & 0x01, 0x01);

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0034);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert_eq!(m.read_physical_u8(0x489) & 0x01, 0x00);

    m.cpu.registers.set_eax(0x1202);
    m.cpu.registers.set_ebx(0x0030);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x489) & 0x11, 0x10);

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0034);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert_eq!(m.read_physical_u8(0x489) & 0x11, 0x11);
}

#[test]
fn int10_1235_acknowledges_display_switch_interface() {
    let mut m = int15_machine(16);

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0035);
    m.handle_int10();

    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(m.video().display_refresh_enabled());
    assert!(m.video().video_subsystem_enabled());
}

#[test]
fn int10_01_scales_legacy_cursor_shape_when_emulation_is_enabled() {
    let mut m = int15_machine(16);
    m.write_physical_u8(0x486, 0xA5);
    m.cpu.registers.set_eax(0x0003);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x485), 16);
    assert_eq!(m.read_physical_u16(0x485), 16);
    assert_eq!(m.read_physical_u8(0x489) & 0x01, 0x01);

    m.cpu.registers.set_eax(0x0100);
    m.cpu.registers.set_ecx(0x0007);
    m.handle_int10();
    assert_eq!(m.memory.read_u16(0x460).unwrap(), 0x0007);
    assert_eq!(color_crtc_reg(&mut m, 0x0A), 0x01);
    assert_eq!(color_crtc_reg(&mut m, 0x0B), 0x0F);

    m.cpu.registers.set_eax(0x0300);
    m.cpu.registers.set_ebx(0);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0007);

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0034);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0100);
    m.cpu.registers.set_ecx(0x0007);
    m.handle_int10();
    assert_eq!(m.memory.read_u16(0x460).unwrap(), 0x0007);
    assert_eq!(color_crtc_reg(&mut m, 0x0A), 0x00);
    assert_eq!(color_crtc_reg(&mut m, 0x0B), 0x07);
}

#[test]
fn int10_1236_toggles_video_refresh() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();
    assert!(m.video_mut().planar_write_pixel(0, 0, 0x0F, false));
    let lit = m.video_mut().render_full_frame().pixels[0];
    assert_ne!(lit, 0);

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0036);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(!m.video().display_refresh_enabled());
    assert!(m.video().video_subsystem_enabled());
    assert_eq!(m.video_mut().render_full_frame().pixels[0], 0);
    assert_eq!(m.video_mut().read_status1() & 0x01, 0x01);

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0036);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(m.video().display_refresh_enabled());
    assert_eq!(m.video_mut().render_full_frame().pixels[0], lit);
}

#[test]
fn int10_04h_reports_no_light_pen() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x04ff);
    m.cpu.registers.set_ecx(0x1234);
    m.cpu.registers.set_edx(0x5678);
    m.handle_int10();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00);
    assert_eq!(m.cpu.registers.ecx() as u16, 0x1234);
    assert_eq!(m.cpu.registers.edx() as u16, 0x5678);
}

#[test]
fn int10_optional_adapter_extensions_report_absent() {
    let mut m = int15_machine(16);

    m.cpu.registers.set_eax(0x1500);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0000);

    for ax in [0x7000, 0x7100] {
        m.cpu.registers.set_eax(ax);
        m.cpu.registers.set_ebx(0x1111);
        m.cpu.registers.set_ecx(0x2222);
        m.cpu.registers.set_edx(0x3333);
        m.handle_int10();
        assert_eq!(m.cpu.registers.eax() as u16, 0x0000);
        assert_eq!(m.cpu.registers.ebx() as u16, 0x0000);
        assert_eq!(m.cpu.registers.ecx() as u16, 0x0000);
        assert_eq!(m.cpu.registers.edx() as u16, 0x0000);
    }

    m.cpu.registers.set_eax(0xBF03);
    m.cpu.registers.set_ebx(0x1111);
    m.cpu.registers.set_ecx(0x2222);
    m.cpu.registers.set_edx(0x3333);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0000);
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0000);
    assert_eq!(m.cpu.registers.edx() as u16, 0x0000);

    m.cpu.registers.set_eax(0xFA00);
    m.cpu.registers.set_ebx(0x1234);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0000);

    for ax in [0x1402, 0x4000, 0x7200, 0x8000, 0xF000, 0xFE00, 0xFF00] {
        let before_eax = 0xCAFE_0000 | ax;
        m.cpu.registers.set_eax(before_eax);
        m.cpu.registers.set_ebx(0x1111);
        m.cpu.registers.set_ecx(0x2222);
        m.cpu.registers.set_edx(0x3333);
        m.handle_int10();
        assert_eq!(m.cpu.registers.eax(), before_eax);
        assert_eq!(m.cpu.registers.ebx() as u16, 0x1111);
        assert_eq!(m.cpu.registers.ecx() as u16, 0x2222);
        assert_eq!(m.cpu.registers.edx() as u16, 0x3333);
    }
}

#[test]
fn int10_dgis_and_extended_adapter_modes_report_absent() {
    let mut m = int15_machine(16);

    m.cpu.registers.set_eax(0x6A00);
    m.cpu.registers.set_ebx(0x1111);
    m.cpu.registers.set_ecx(0x2222);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0000);
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0000);

    m.cpu.registers.set_eax(0x6A01);
    m.cpu.registers.set_ecx(0x3333);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0000);

    m.cpu.registers.set_eax(0x6A02);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x1234));
    m.cpu.registers.set_edi(0xABCD_5678);
    m.handle_int10();
    assert_eq!(m.cpu.registers.segment(SegmentIndex::Es).selector, 0);
    assert_eq!(m.cpu.registers.edi() as u16, 0);

    m.cpu.registers.set_eax(0x0003);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x449), 0x03);
    for ax in [0x0070, 0x6F05] {
        m.cpu.registers.set_eax(ax);
        m.cpu.registers.set_ebx(0x0066);
        m.handle_int10();
        assert_eq!(m.read_physical_u8(0x449), 0x03);
    }
}

#[test]
fn int10_12h_reports_vga_configuration() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0010);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0003); // color, 256 KB VRAM
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0f09); // feature bits, color switches
}

#[test]
fn int10_12h_updates_vga_policy_latches() {
    let mut m = int15_machine(16);

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0030);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert_eq!(m.read_physical_u8(0x489) & 0x90, 0x80); // 200 scan lines

    m.cpu.registers.set_eax(0x1202);
    m.cpu.registers.set_ebx(0x0030);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x489) & 0x90, 0x10); // 400 scan lines

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0031);
    m.handle_int10();
    assert_ne!(m.read_physical_u8(0x489) & 0x08, 0); // palette load disabled

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0031);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x489) & 0x08, 0); // palette load enabled

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0033);
    m.handle_int10();
    assert_ne!(m.read_physical_u8(0x489) & 0x02, 0); // gray summing enabled

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0034);
    m.handle_int10();
    assert_ne!(m.read_physical_u8(0x487) & 0x01, 0); // cursor emulation disabled

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0034);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x487) & 0x01, 0); // cursor emulation enabled
}

#[test]
fn int10_1b_fills_state_block_and_signals_vga() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004); // CGA mode so the BDA shadows are non-VGA
    m.handle_int10();
    m.cpu.registers.set_eax(0x0B00);
    m.cpu.registers.set_ebx(0x0011);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0B00);
    m.cpu.registers.set_ebx(0x0101);
    m.handle_int10();
    m.cpu.registers.set_eax(0x1A01);
    m.cpu.registers.set_ebx(0x000A);
    m.handle_int10();
    m.cpu.registers.set_eax(0x1B00); // ES:DI = 0:0 -> block at physical 0
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x1B);
    assert_eq!(m.read_physical_u16(0), 0x0000); // functionality table offset
    assert_eq!(m.read_physical_u16(2), VGA_BIOS_SEGMENT); // functionality table segment
    let table: Vec<u8> = (0..16)
        .map(|offset| m.read_physical_u8(VGA_BIOS_BASE + offset))
        .collect();
    assert_eq!(table.as_slice(), &INT10_STATIC_FUNCTIONALITY);
    assert_eq!(m.read_physical_u8(4), 0x04); // video mode at +4
    assert_eq!(m.read_physical_u16(0x07), 0x4000); // regen buffer/page size
    assert_eq!(m.read_physical_u16(0x09), 0x0000); // active page start
    assert_eq!(m.read_physical_u8(0x20), 0x0A); // CGA 3D8h shadow
    assert_eq!(m.read_physical_u8(0x21), 0x31); // CGA 3D9h shadow
    assert_eq!(m.read_physical_u16(0x23), 8); // bytes per character
    assert_eq!(m.read_physical_u8(0x25), 0x0A); // BDA display-combination code
    assert_eq!(m.read_physical_u16(0x27), 4); // CGA mode 04h colors
    assert_eq!(m.read_physical_u8(0x29), 1); // CGA graphics has one page
    assert_eq!(m.read_physical_u8(0x2A), 0x00); // 200 scan lines
}

#[test]
fn int10_exposes_video_save_pointer_and_parameter_table() {
    let mut m = int15_machine(16);
    assert_eq!(
        m.read_physical_u16(BDA_VIDEO_SAVE_POINTER as u32),
        INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET
    );
    assert_eq!(
        m.read_physical_u16((BDA_VIDEO_SAVE_POINTER + 2) as u32),
        VGA_BIOS_SEGMENT
    );

    let save_table = VGA_BIOS_BASE + u32::from(INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET);
    assert_eq!(
        m.read_physical_u16(save_table),
        INT10_VIDEO_PARAM_TABLE_OFFSET
    );
    assert_eq!(m.read_physical_u16(save_table + 2), VGA_BIOS_SEGMENT);
    let param_table = VGA_BIOS_BASE + u32::from(INT10_VIDEO_PARAM_TABLE_OFFSET);
    let mode03 = param_table + 0x18 * INT10_VIDEO_PARAM_ENTRY_LEN as u32;
    assert_eq!(m.read_physical_u8(mode03), 80);
    assert_eq!(m.read_physical_u8(mode03 + 1), 24);
    assert_eq!(m.read_physical_u8(mode03 + 2), 16);
    let mode12 = param_table + 0x1b * INT10_VIDEO_PARAM_ENTRY_LEN as u32;
    assert_eq!(m.read_physical_u8(mode12), 80);
    assert_eq!(m.read_physical_u8(mode12 + 1), 29);
    assert_eq!(m.read_physical_u8(mode12 + 2), 16);

    m.memory.write_u16(BDA_VIDEO_SAVE_POINTER, 0).unwrap();
    m.memory.write_u16(BDA_VIDEO_SAVE_POINTER + 2, 0).unwrap();
    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();
    assert_eq!(
        m.read_physical_u16(BDA_VIDEO_SAVE_POINTER as u32),
        INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET
    );
    assert_eq!(
        m.read_physical_u16((BDA_VIDEO_SAVE_POINTER + 2) as u32),
        VGA_BIOS_SEGMENT
    );
}

#[test]
fn int10_1b_reports_ega_graphics_page_count() {
    let mut m = int15_machine(16);

    for (mode, pages) in [
        (0x0D, 8),
        (0x0E, 4),
        (0x0F, 2),
        (0x10, 2),
        (0x11, 1),
        (0x12, 1),
    ] {
        m.cpu.registers.set_eax(mode);
        m.handle_int10();
        m.cpu.registers.set_eax(0x1B00);
        m.handle_int10();

        assert_eq!(m.cpu.registers.eax() as u8, 0x1B);
        assert_eq!(m.read_physical_u8(0x29), pages, "mode {mode:02X}");
    }
}

#[test]
fn timing_factors_track_the_active_mode() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(1, izarravm_core::VideoCard::Et4000Ax),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    // Boot mode is 386 @ 22 MHz: the PIT factor is PIT_INPUT_HZ / 22 MHz.
    assert_eq!(machine.active_mode(), GswMode::Gsw386);
    assert!((machine.timing.pit_per_clock - PIT_INPUT_HZ as f64 / 22_000_000.0).abs() < 1e-9);
    // Switching to 586 @ 200 MHz recomputes the factor.
    machine.set_mode(GswMode::Gsw586);
    assert_eq!(machine.active_mode(), GswMode::Gsw586);
    assert!((machine.timing.pit_per_clock - PIT_INPUT_HZ as f64 / 200_000_000.0).abs() < 1e-9);
    // 386-slow @ ~7.33 MHz.
    machine.set_mode(GswMode::Gsw386Slow);
    assert_eq!(machine.active_mode(), GswMode::Gsw386Slow);
    assert!((machine.timing.pit_per_clock - PIT_INPUT_HZ as f64 / 7_333_333.0).abs() < 1e-9);
}

#[test]
fn set_mode_drives_cpu_level_and_cache_table() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(1, izarravm_core::VideoCard::Et4000Ax),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    // The CPU boots at the full ISA so POST is never restricted, regardless of the
    // 386 boot mode, until the guest writes a Lotura mode.
    assert_eq!(machine.cpu.level(), CpuLevel::I586);

    machine.set_mode(GswMode::Gsw386Slow);
    assert_eq!(machine.cpu.level(), CpuLevel::I386);
    assert_eq!(machine.cache_config(), (0, 64));

    machine.set_mode(GswMode::Gsw386);
    assert_eq!(machine.cpu.level(), CpuLevel::I386);
    assert_eq!(machine.cache_config(), (0, 64));

    machine.set_mode(GswMode::Gsw486);
    assert_eq!(machine.cpu.level(), CpuLevel::I486);
    assert_eq!(machine.cache_config(), (16, 128));

    machine.set_mode(GswMode::Gsw586);
    assert_eq!(machine.cpu.level(), CpuLevel::I586);
    assert_eq!(machine.cache_config(), (32, 512));
}

#[test]
fn lotura_code_3_selects_386_slow_mode() {
    assert_eq!(gsw_mode_from_code(3), Some(GswMode::Gsw386Slow));
    assert_eq!(gsw_mode_code(GswMode::Gsw386Slow), 3);
    assert_eq!(cpu_level_for_mode(GswMode::Gsw386Slow), CpuLevel::I386);
}

fn rom_with_code(code: &[u8]) -> Vec<u8> {
    let mut rom = vec![0; BIOS_ROM_SIZE];
    rom[..code.len()].copy_from_slice(code);
    // The ROM IRET at offset 0xF000 (FF00:0000) the real izarra BIOS emits.
    // The host-intercepted BIOS service vectors return through it, so the
    // bare test ROM supplies it too.
    rom[0xF000] = 0xCF;
    rom[0xfff0..0xfff5].copy_from_slice(&[0xea, 0x00, 0x00, 0x00, 0xf0]);
    rom
}

#[test]
fn injected_key_is_readable_on_port_0x60_and_requests_irq1() {
    // A bare machine: inject a scancode, then read it back through the bus the
    // way the CPU would, and confirm IRQ1 became pending on the PIC.
    let profile = MachineProfile::gsw_386(1, izarravm_core::VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, vec![0u8; BIOS_ROM_SIZE]).unwrap();
    machine.inject_key_scancodes(&[0x1e]); // 'A' make
    assert_eq!(machine.read_io_port_u8(0x60), 0x1e);
    assert!(machine.irq1_pending(), "injecting a key requests IRQ1");
}

/// Run a .COM that reads one key via INT 16h AH=00h and stores AX at DS:0x200,
/// after injecting `scancodes`. Returns the value INT 16h handed the program.
/// This is the editor's keyboard path end to end: 8042 -> IRQ1 -> INT 09h ISR
/// -> BDA ring -> INT 16h read.
fn int16_read_after_with_layout(layout: u8, scancodes: &[u8]) -> u16 {
    // mov ah,0; int 16h; mov [0x200],ax; int 20h
    const PROG: [u8; 9] = [0xB4, 0x00, 0xCD, 0x16, 0xA3, 0x00, 0x02, 0xCD, 0x20];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), &PROG).unwrap();
    machine.write_physical_u8(0x0496, layout);
    machine.inject_key_scancodes(scancodes);
    machine.run_until_halt_or_cycles(3_000_000).unwrap();
    read_u16(&mut machine, (u32::from(DOS_LOAD_SEGMENT) << 4) + 0x200)
}

fn int16_read_after(scancodes: &[u8]) -> u16 {
    int16_read_after_with_layout(0, scancodes)
}

fn int16_peek_guest_exit(scancodes: &[u8], prog: &[u8]) -> StopReason {
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), prog).unwrap();
    machine.inject_key_scancodes(scancodes);
    machine.run_until_halt_or_cycles(1_000_000).unwrap()
}

#[test]
fn int16_peek_empty_sets_guest_zf() {
    // mov al,1; or al,al; mov ah,1; int 16h; jz pass; mov al,1; jmp exit;
    // pass: xor al,al; exit: store AL as the Lotura exit code.
    const PROG: [u8; 30] = [
        0xb0, 0x01, 0x08, 0xc0, 0xb4, 0x01, 0xcd, 0x16, 0x74, 0x04, 0xb0, 0x01, 0xeb, 0x02, 0x30,
        0xc0, 0x88, 0xc3, 0xb0, 0x0c, 0xe6, 0xe4, 0x88, 0xd8, 0xe6, 0xe5, 0xb0, 0x03, 0xe6, 0xe6,
    ];
    assert_eq!(
        int16_peek_guest_exit(&[], &PROG),
        StopReason::TestExit { code: 0 }
    );
}

#[test]
fn int16_peek_available_clears_guest_zf() {
    // xor ax,ax; mov ah,1; int 16h; jnz pass; mov al,1; jmp exit;
    // pass: xor al,al; exit: store AL as the Lotura exit code.
    const PROG: [u8; 28] = [
        0x31, 0xc0, 0xb4, 0x01, 0xcd, 0x16, 0x75, 0x04, 0xb0, 0x01, 0xeb, 0x02, 0x30, 0xc0, 0x88,
        0xc3, 0xb0, 0x0c, 0xe6, 0xe4, 0x88, 0xd8, 0xe6, 0xe5, 0xb0, 0x03, 0xe6, 0xe6,
    ];
    assert_eq!(
        int16_peek_guest_exit(&[0x1e, 0x9e], &PROG),
        StopReason::TestExit { code: 0 }
    );
}

#[test]
fn int16_returns_extended_scancode_for_up_arrow() {
    // Up arrow is the bare scancode 0x48 (make) / 0xC8 (break); no 0xE0 prefix.
    // The layout table has no ASCII for it, so INT 16h returns scancode 0x48
    // with ASCII 0 -- the value a full-screen editor keys arrow navigation off.
    assert_eq!(int16_read_after(&[0x48, 0xC8]), 0x4800);
}

#[test]
fn int16_emits_control_code_for_ctrl_s() {
    // Ctrl down, S, S up, Ctrl up. Holding Ctrl turns S into the DC3 control
    // code (0x13), the way a real BIOS does, so the editor reads Ctrl-S as a
    // single ring entry (scancode 0x1f, ASCII 0x13) with no modifier polling.
    assert_eq!(int16_read_after(&[0x1d, 0x1f, 0x9f, 0x9d]), 0x1f13);
}

#[test]
fn int16_numpad_honors_num_lock_for_digits() {
    // NumLock make/break toggles the BIOS flag without entering the key ring.
    // The next keypad make then returns its numeric ASCII byte.
    assert_eq!(int16_read_after(&[0x45, 0xc5, 0x48, 0xc8]), 0x4838);
    assert_eq!(int16_read_after(&[0x45, 0xc5, 0x52, 0xd2]), 0x5230);
    assert_eq!(int16_read_after(&[0x4e, 0xce]), 0x4e2b);
}

#[test]
fn int16_resident_keyboard_uses_bios_layout_byte() {
    assert_eq!(int16_read_after_with_layout(0, &[0x27, 0xa7]), 0x273b);
    assert_eq!(
        int16_read_after_with_layout(0, &[0x2a, 0x27, 0xa7, 0xaa]),
        0x273a
    );
    assert_eq!(int16_read_after_with_layout(2, &[0x27, 0xa7]), 0x27a4);
    assert_eq!(
        int16_read_after_with_layout(2, &[0x2a, 0x33, 0xb3, 0xaa]),
        0x333b
    );
}

#[test]
fn es_layout_fills_ordinal_and_iso_and_cedilla() {
    // AX = (scancode << 8) | ascii from INT 16h. Layout 2 = ES.
    // 0x29 (left of 1): º / ª / backslash
    assert_eq!(int16_read_after_with_layout(2, &[0x29, 0xa9]), 0x29a7); // º CP437 0xA7
    assert_eq!(
        int16_read_after_with_layout(2, &[0x2a, 0x29, 0xa9, 0xaa]),
        0x29a6
    ); // ª 0xA6
    assert_eq!(
        int16_read_after_with_layout(2, &[0xe0, 0x38, 0x29, 0xa9, 0xe0, 0xb8]),
        0x295c
    ); // AltGr+0x29 -> '\'
    // 0x56 ISO 102nd key: < / >
    assert_eq!(int16_read_after_with_layout(2, &[0x56, 0xd6]), 0x563c); // '<'
    assert_eq!(
        int16_read_after_with_layout(2, &[0x2a, 0x56, 0xd6, 0xaa]),
        0x563e
    ); // '>'
    // 0x2b cedilla key: c-cedilla / C-cedilla (was wrongly Greek glyphs)
    assert_eq!(int16_read_after_with_layout(2, &[0x2b, 0xab]), 0x2b87); // ç 0x87
    assert_eq!(
        int16_read_after_with_layout(2, &[0x2a, 0x2b, 0xab, 0xaa]),
        0x2b80
    ); // Ç 0x80
    // 0x0d inverted punctuation: ¡ / ¿
    assert_eq!(int16_read_after_with_layout(2, &[0x0d, 0x8d]), 0x0dad); // ¡ 0xAD
    assert_eq!(
        int16_read_after_with_layout(2, &[0x2a, 0x0d, 0x8d, 0xaa]),
        0x0da8
    ); // ¿ 0xA8
}

#[test]
fn es_dead_keys_compose_accents() {
    // Layout 2 = ES. 0x28 = acute dead key, 0x1a = grave dead key.
    // acute + a -> a-acute (CP437 0xA0), reported with a's scancode 0x1e.
    assert_eq!(
        int16_read_after_with_layout(2, &[0x28, 0xa8, 0x1e, 0x9e]),
        0x1ea0
    );
    // grave + e -> e-grave (0x8A), e scancode 0x12.
    assert_eq!(
        int16_read_after_with_layout(2, &[0x1a, 0x9a, 0x12, 0x92]),
        0x128a
    );
    // diaeresis (shift+0x28) + u -> u-diaeresis (0x81), u scancode 0x16.
    assert_eq!(
        int16_read_after_with_layout(2, &[0x2a, 0x28, 0xa8, 0xaa, 0x16, 0x96]),
        0x1681
    );
    // acute + space -> the spacing acute ' (0x27), space scancode 0x39.
    assert_eq!(
        int16_read_after_with_layout(2, &[0x28, 0xa8, 0x39, 0xb9]),
        0x3927
    );
    // acute + t (no composed form) -> first key read back is the flush ' (0x27).
    assert_eq!(
        int16_read_after_with_layout(2, &[0x28, 0xa8, 0x14, 0x94]),
        0x1427
    );
}

#[test]
fn non_es_dead_keys_use_the_per_layout_tables() {
    // The kb_deadkey routine now selects the descriptor and composition table
    // by KB_LAYOUT, so dead keys work beyond Spanish. French (layout 3): the
    // circumflex dead key (scancode 0x1a, unshifted) then 'e' (0x12) composes
    // to e-circumflex (CP850 0x88), reported with e's scancode.
    assert_eq!(
        int16_read_after_with_layout(3, &[0x1a, 0x9a, 0x12, 0x92]),
        0x1288
    );
    // German (layout 4): the dead acute (scancode 0x0d, unshifted) then 'a'
    // (0x1e) composes to a-acute (CP850 0xA0).
    assert_eq!(
        int16_read_after_with_layout(4, &[0x0d, 0x8d, 0x1e, 0x9e]),
        0x1ea0
    );
}

/// Same path as `int16_read_after`, but the program reads with AH=10h (the
/// enhanced read). Before the DOS keyboard ROM aliased AH=10h to the AH=00h
/// reader, this fell through the int16 dispatch and returned stale AX.
fn int16_enhanced_read_after(scancodes: &[u8]) -> u16 {
    // mov ah,0x10; int 16h; mov [0x200],ax; int 20h
    const PROG: [u8; 9] = [0xB4, 0x10, 0xCD, 0x16, 0xA3, 0x00, 0x02, 0xCD, 0x20];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), &PROG).unwrap();
    machine.inject_key_scancodes(scancodes);
    machine.run_until_halt_or_cycles(3_000_000).unwrap();
    read_u16(&mut machine, (u32::from(DOS_LOAD_SEGMENT) << 4) + 0x200)
}

#[test]
fn int16_enhanced_read_matches_plain_read() {
    // AH=10h must hand a DOS program the same ring entry AH=00h does. Up
    // arrow gives scancode 0x48 / ASCII 0, the editor-navigation case.
    assert_eq!(int16_enhanced_read_after(&[0x48, 0xC8]), 0x4800);
    assert_eq!(
        int16_enhanced_read_after(&[0x48, 0xC8]),
        int16_read_after(&[0x48, 0xC8]),
    );
}

#[test]
fn int16_keyclick_call_returns_to_caller() {
    // mov ax,0401h; int 16h; mov word [0200h],1234h; int 20h
    const PROG: [u8; 13] = [
        0xb8, 0x01, 0x04, 0xcd, 0x16, 0xc7, 0x06, 0x00, 0x02, 0x34, 0x12, 0xcd, 0x20,
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), &PROG).unwrap();
    machine.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(
        read_u16(&mut machine, (u32::from(DOS_LOAD_SEGMENT) << 4) + 0x200),
        0x1234,
        "INT 16h AH=04h returned to the caller"
    );
}

#[test]
fn io_port_reports_last_post_write() {
    // mov al,0x42; out 0x80,al; hlt
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        rom_with_code(&[0xb0, 0x42, 0xe6, 0x80, 0xf4]),
    )
    .unwrap();
    machine.run_until_halt_or_cycles(10_000).unwrap();
    assert_eq!(machine.io_port(0x80), Some(0x42));
    assert_eq!(machine.io_port(0x0100), None); // outside the passive port map
}

fn read_u16(machine: &mut Machine, addr: u32) -> u16 {
    u16::from(machine.read_physical_u8(addr)) | (u16::from(machine.read_physical_u8(addr + 1)) << 8)
}

fn read_u32(machine: &mut Machine, addr: u32) -> u32 {
    u32::from(read_u16(machine, addr)) | (u32::from(read_u16(machine, addr + 2)) << 16)
}

#[test]
fn rejects_non_64k_roms() {
    let err = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), [0u8; 8]).unwrap_err();

    assert!(matches!(err, MachineError::InvalidRomSize(8)));
}

#[test]
fn first_instruction_fetch_uses_386_reset_vector() {
    let mut machine = test_machine();
    let reason = machine.run_cycles(32).unwrap();

    assert_ne!(reason, StopReason::Halted);
    assert_eq!(
        machine.bus_trace().cycles()[0].kind,
        BusAccessKind::InstructionPrefetch
    );
    assert_eq!(machine.bus_trace().cycles()[0].address, 0xffff_fff0);
}

#[test]
fn unaligned_dword_splits_into_byte_bus_cycles() {
    let mut machine = test_machine();
    {
        let mut bus = machine.make_bus();
        bus.write_memory(
            0x101,
            BusWidth::Dword,
            0x1234_5678,
            BusAccessKind::DataWrite,
        )
        .unwrap();
    }

    let writes = machine
        .bus_trace()
        .cycles()
        .iter()
        .filter(|cycle| cycle.kind == BusAccessKind::DataWrite)
        .count();
    assert_eq!(writes, 4);
}

#[test]
fn test_rom_reaches_deterministic_text_screen() {
    let mut machine = test_machine();
    let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
    let frame = machine.screen_text();

    assert_eq!(reason, StopReason::Halted);
    assert_eq!(frame.line_string(0), "RESET VECTOR + BIOS INT10 PASS");
    assert_eq!(frame.line_string(1), "B8000 DIRECT TEXT PASS");
    assert_eq!(frame.line_string(2), "PROTECTED MODE FLAT SEGMENTS PASS");
    assert_eq!(frame.line_string(3), "PAGING + B8000 ALIAS PASS");
    assert_eq!(frame.line_string(4), "RING0 PAGE FAULT HANDLER PASS");
    assert!(
        machine
            .bus_trace()
            .cycles()
            .iter()
            .any(|cycle| cycle.kind == BusAccessKind::PageWalkRead)
    );
    assert!(machine.cpu().is_protected_mode());
    assert!(machine.cpu().is_paging_enabled());
}

#[test]
fn int10_mode13h_routes_a000_through_chain4() {
    let rom = rom_with_code(&[
        0xb8, 0x13, 0x00, // mov ax, 0013h
        0xcd, 0x10, // int 10h
        0xb8, 0x00, 0xa0, // mov ax, a000h
        0x8e, 0xc0, // mov es, ax
        0xbf, 0x7b, 0x00, // mov di, 007bh
        0xb0, 0x2a, // mov al, 2ah
        0xaa, // stosb
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.set_bus_trace_detailed(true);

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    // Chain-4 routes the A0000 byte at offset 0x7B to plane 0x7B & 3 = 3 at
    // plane offset 0x7B >> 2 = 30.
    assert_eq!(machine.video().plane_byte(3, 30), 0x2a);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    assert!(machine.is_graphics_mode());
    assert!(machine.bus_trace().cycles().iter().any(|cycle| {
        cycle.kind == BusAccessKind::InterruptAcknowledge && cycle.address == 0x10
    }));
}

#[test]
fn unittester_exit_command_stops_with_the_guest_code() {
    // index=REG_EXIT; data=42; command=CMD_EXIT.
    let rom = rom_with_code(&[
        0xB0, 0x0C, 0xE6, 0xE4, // mov al,12; out 0E4h,al  (index = REG_EXIT)
        0xB0, 0x2A, 0xE6, 0xE5, // mov al,42; out 0E5h,al  (exit code 42)
        0xB0, 0x03, 0xE6, 0xE6, // mov al,3;  out 0E6h,al  (CMD_EXIT)
        0xF4, // hlt (not reached)
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::TestExit { code: 42 });
}

#[test]
fn unittester_crc_command_matches_the_rust_helper() {
    // Program a 2x2 rectangle and issue CMD_CRC; the run loop computes it and
    // stores it at REG_CRC, where the guest (here, a bus read) can read it.
    let rom = rom_with_code(&[
        0xB0, 0x00, 0xE6, 0xE4, // index = REG_X (0)
        0xB0, 0x00, 0xE6, 0xE5, // X lo
        0xB0, 0x00, 0xE6, 0xE5, // X hi
        0xB0, 0x00, 0xE6, 0xE5, // Y lo
        0xB0, 0x00, 0xE6, 0xE5, // Y hi
        0xB0, 0x02, 0xE6, 0xE5, // W lo = 2
        0xB0, 0x00, 0xE6, 0xE5, // W hi
        0xB0, 0x02, 0xE6, 0xE5, // H lo = 2
        0xB0, 0x00, 0xE6, 0xE5, // H hi
        0xB0, 0x01, 0xE6, 0xE6, // CMD_CRC
        0xF4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.run_until_halt_or_cycles(1_000_000).unwrap();

    let reported = with_bus(&mut machine, |bus| {
        bus.write_io(0xE4, BusWidth::Byte, 8, false).unwrap(); // index = REG_CRC
        let mut crc = [0u8; 4];
        for byte in &mut crc {
            *byte = bus.read_io(0xE5, BusWidth::Byte, 0, false).unwrap() as u8;
        }
        u32::from_le_bytes(crc)
    });
    assert_eq!(reported, machine.screen_crc32(0, 0, 2, 2));
}

#[test]
fn int10_ah0f_reports_mode_after_set() {
    // Set mode 13h, then AH=0Fh returns AL=mode, AH=columns.
    let rom = rom_with_code(&[
        0xB8, 0x13, 0x00, 0xCD, 0x10, // mov ax,0013h; int 10h (set mode 13h)
        0xB4, 0x0F, 0xCD, 0x10, // mov ah,0Fh; int 10h (get mode)
        0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let ax = machine.cpu().registers.eax() as u16;
    assert_eq!(ax & 0xff, 0x13, "AL = current mode");
    assert_eq!(ax >> 8, 40, "AH = column count for mode 13h");
}

#[test]
fn int10_00_returns_vgabios_mode_class_code() {
    let mut m = int15_machine(16);

    for (mode, returned_al) in [
        (0x00u8, 0x30u8),
        (0x04, 0x30),
        (0x06, 0x3F),
        (0x0D, 0x20),
        (0x13, 0x20),
        (0x84, 0x30),
    ] {
        m.cpu.registers.set_eax(u32::from(mode));
        m.handle_int10();

        assert_eq!(m.cpu.registers.eax() as u8, returned_al, "mode {mode:02X}");
    }
    assert_eq!(m.read_physical_u8(0x449), 0x84);
}

#[test]
fn int10_00_tracks_no_clear_in_bda_video_control() {
    let mut m = int15_machine(16);

    m.cpu.registers.set_eax(0x008D);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x449), 0x8D);
    assert_eq!(m.read_physical_u8(0x487), 0xE0);

    m.cpu.registers.set_eax(0x0093);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x449), 0x93);
    assert_eq!(m.read_physical_u8(0x487), 0xE0);

    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x449), 0x0D);
    assert_eq!(m.read_physical_u8(0x487), 0x60);
}

#[test]
fn boot_image_starts_at_bios_loaded_boot_sector() {
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::X86_BOOT_TEST_IMAGE,
    )
    .unwrap();
    machine.set_bus_trace_detailed(true);

    let reason = machine.run_cycles(16).unwrap();

    assert_ne!(reason, StopReason::Halted);
    assert_eq!(
        machine.bus_trace().cycles()[0].address,
        BOOT_SECTOR_ADDRESS as u32
    );
}

#[test]
fn boot_image_emits_serial_records_and_result_block() {
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::X86_BOOT_TEST_IMAGE,
    )
    .unwrap();

    // The budget covers the timer test's idle (ten ticks of about 11932 PIT
    // clocks, near 2.5M CPU clocks) plus the setup, matching the headless runner.
    let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
    let serial = machine.serial_text();
    let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();

    assert_eq!(reason, StopReason::Halted);
    assert!(serial.contains("PASS boot.stage2"));
    assert!(serial.contains("PASS video.cga_graphics"));
    assert!(serial.contains("PASS video.ega_planar"));
    assert!(serial.contains("PASS video.vga_mode13h"));
    assert_eq!(
        usize::from(results.declared_record_count),
        results.records.len()
    );
    assert!(results.records.iter().any(|record| {
        record.status == izarravm_firmware::SuiteRecordStatus::Pass
            && record.name == "video.vga_text"
    }));
    assert!(results.records.iter().any(|record| {
        record.status == izarravm_firmware::SuiteRecordStatus::Pass
            && record.name == "video.vga_mode13h"
    }));
    for name in ["video.cga_graphics", "video.ega_planar"] {
        assert!(results.records.iter().any(|record| {
            record.status == izarravm_firmware::SuiteRecordStatus::Pass && record.name == name
        }));
    }
    // Chain-4 routes the linear byte at offset N to plane N & 3 at plane
    // offset N >> 2, so the boot image's three drawn pixels land as:
    // 0 -> plane 0 @ 0, 319 -> plane 3 @ 79, 63680 -> plane 0 @ 15920.
    assert_eq!(machine.video().plane_byte(0, 0), 0x2a);
    assert_eq!(machine.video().plane_byte(3, 79), 0x13);
    assert_eq!(machine.video().plane_byte(0, 15920), 0x7f);
    assert!(results.records.iter().any(|record| {
        record.status == izarravm_firmware::SuiteRecordStatus::Pass
            && record.name == "sound.sb_16bit_dma"
    }));
}

#[test]
fn boot_suite_timer_passes_at_native_200mhz() {
    // The boot suite is wall-time-bound: the timer test waits for ten IRQ0
    // edges and the PIT runs at a fixed rate regardless of the CPU clock. At
    // the 200 MHz native default the cycle budget must scale (clock_hz / 5,
    // about 200 ms) or the timer test never reaches its tick target.
    let profile = MachineProfile {
        cpu: GswMode::Gsw586,
        clock_hz: GswMode::Gsw586.clock_hz(),
        memory_mib: 16,
        video: VideoCard::Et4000Ax,
        sound_blaster: SoundBlasterConfig::default(),
        wss: WssConfig::default(),
        wait_states: WaitStateProfile::default(),
        address_pipelining: false,
        cache_enabled: false,
    };
    let budget = profile.clock_hz / 5;
    let mut machine =
        Machine::new_boot_image(profile, izarravm_firmware::X86_BOOT_TEST_IMAGE).unwrap();

    let reason = machine.run_until_halt_or_cycles(budget).unwrap();
    let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();

    assert_eq!(reason, StopReason::Halted);
    let timer = results
        .records
        .iter()
        .find(|record| record.name == "timer.irq0")
        .expect("timer.irq0 record present");
    assert_eq!(
        timer.status,
        izarravm_firmware::SuiteRecordStatus::Pass,
        "timer.irq0 must pass at 200 MHz with the scaled budget"
    );
}

#[test]
fn margo_apertures_route_through_the_bus() {
    let mut machine = test_machine();

    // LFB: write a byte at the aperture base + 5, read it back.
    let lfb = MARGO_LFB_BASE + 5;
    machine.write_physical_u8(lfb, 0x9c);
    assert_eq!(machine.read_physical_u8(lfb), 0x9c);

    // MMIO: the ID register reads the Margo magic.
    let id = u32::from(machine.read_physical_u8(MARGO_MMIO_BASE))
        | (u32::from(machine.read_physical_u8(MARGO_MMIO_BASE + 1)) << 8)
        | (u32::from(machine.read_physical_u8(MARGO_MMIO_BASE + 2)) << 16)
        | (u32::from(machine.read_physical_u8(MARGO_MMIO_BASE + 3)) << 24);
    assert_eq!(id, MARGO_ID_VALUE);
}

#[test]
fn vga_mode_set_clears_a_latched_margo_display() {
    let rom = rom_with_code(&[
        0xb8, 0x13, 0x00, // mov ax, 0013h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    // Host path latches Margo as the active display.
    machine.set_margo_mode_640x480x8();
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);

    // A guest VGA mode-set must hand the display back to VGA.
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
}

#[test]
fn int42_relocated_video_handler_uses_int10_service() {
    let rom = rom_with_code(&[
        0xb8, 0x13, 0x00, // mov ax, 0013h
        0xcd, 0x42, // int 42h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.memory.read_u8(0x449).unwrap(), 0x13);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
}

#[test]
fn host_mode_set_selects_margo_lfb() {
    let mut machine = test_machine();
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);

    machine.set_margo_mode_640x480x8();

    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    assert_eq!(machine.margo().display().width, 640);
    assert_eq!(machine.margo().display().height, 480);
}

#[test]
fn int13_read_places_sector_in_memory() {
    // A 720 KB image whose first sector starts with a recognizable marker.
    let mut img = vec![0u8; 737_280];
    img[0] = 0xEB;
    img[1] = 0x55;
    // Stub: ES=0, BX=0x2000, read 1 sector at CHS(0,0,1) of drive 0 via INT 13h,
    // then halt. AX=0x0201 (AH=02 read, AL=01 sector), CX=0x0001 (cyl 0,
    // sector 1), DX=0x0000 (head 0, drive A:). The buffer sits well clear of
    // the IRET stub the BIOS keeps near 0x0600.
    let rom = rom_with_code(&[
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xC0, // mov es, ax
        0xBB, 0x00, 0x20, // mov bx, 0x2000
        0xB8, 0x01, 0x02, // mov ax, 0x0201
        0xB9, 0x01, 0x00, // mov cx, 0x0001
        0xBA, 0x00, 0x00, // mov dx, 0x0000
        0xCD, 0x13, // int 13h
        0xF4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.mount_floppy(img).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // The sector bytes landed at physical 0x2000.
    assert_eq!(machine.read_physical_u8(0x2000), 0xEB);
    assert_eq!(machine.read_physical_u8(0x2001), 0x55);
    // AH cleared, AL reports one sector read, CF clear on success.
    let ax = machine.cpu().registers.eax() as u16;
    assert_eq!(ax >> 8, 0x00);
    assert_eq!(ax & 0xff, 0x01);
    let flags = machine.cpu().registers.eflags;
    assert_eq!(flags & 0x0001, 0, "CF must be clear after a good read");
}

#[test]
fn int40_relocated_floppy_handler_uses_disk_service() {
    let mut img = vec![0u8; 737_280];
    img[0] = 0xEB;
    img[1] = 0x40;
    let rom = rom_with_code(&[
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xC0, // mov es, ax
        0xBB, 0x00, 0x20, // mov bx, 0x2000
        0xB8, 0x01, 0x02, // mov ax, 0x0201
        0xB9, 0x01, 0x00, // mov cx, 0x0001
        0xBA, 0x00, 0x00, // mov dx, 0x0000
        0xCD, 0x40, // int 40h
        0xF4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.mount_floppy(img).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.read_physical_u8(0x2000), 0xEB);
    assert_eq!(machine.read_physical_u8(0x2001), 0x40);
    assert_eq!(machine.cpu().registers.eflags & 0x0001, 0);
}

#[test]
fn int10_pixel_write_read_round_trips_in_mode13h() {
    let mut m = int15_machine(16);
    m.video_mut().set_mode13h();
    // AH=0Ch write pixel: AL=colour 0x43 (bit7 clear = plain write), CX=col 5,
    // DX=row 2 -> framebuffer offset 2*320+5.
    m.cpu.registers.set_eax(0x0C43);
    m.cpu.registers.set_ecx(5);
    m.cpu.registers.set_edx(2);
    m.handle_int10();
    // AH=0Dh read the same pixel back into AL.
    m.cpu.registers.set_eax(0x0D00);
    m.cpu.registers.set_ecx(5);
    m.cpu.registers.set_edx(2);
    m.handle_int10();
    assert_eq!(
        m.cpu.registers.eax() as u8,
        0x43,
        "pixel reads back its colour"
    );
    // Mode 13h is a 256-color mode: AL is the full 8-bit colour, bit 7 included,
    // with no XOR. Writing 0x8F stores colour 0x8F (143), not an XOR.
    m.cpu.registers.set_eax(0x0C8F); // colour 0x8F, bit7 part of the value
    m.cpu.registers.set_ecx(5);
    m.cpu.registers.set_edx(2);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0D00);
    m.cpu.registers.set_ecx(5);
    m.cpu.registers.set_edx(2);
    m.handle_int10();
    assert_eq!(
        m.cpu.registers.eax() as u8,
        0x8F,
        "high colours write directly, no bit-7 XOR in 256-colour mode"
    );
}

#[test]
fn int10_pixel_write_read_round_trips_in_cga_graphics() {
    let mut m = int15_machine(16);
    m.video_mut().set_cga_mode(0x04);

    // Mode 04h packs four 2-bit pixels per byte. Pixel (2,1) lives in the odd
    // bank at B800:2000 bits 3:2.
    m.cpu.registers.set_eax(0x0C03);
    m.cpu.registers.set_ecx(2);
    m.cpu.registers.set_edx(1);
    m.handle_int10();
    assert_eq!(m.video().cga_read(0x2000), 0x0C);

    m.cpu.registers.set_eax(0x0D00);
    m.cpu.registers.set_ecx(2);
    m.cpu.registers.set_edx(1);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 3);

    // In CGA modes AL bit 7 means XOR the low colour bits with the existing
    // pixel, so 3 xor 1 becomes 2.
    m.cpu.registers.set_eax(0x0C81);
    m.cpu.registers.set_ecx(2);
    m.cpu.registers.set_edx(1);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0D00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 2);

    m.video_mut().set_cga_mode(0x06);
    m.cpu.registers.set_eax(0x0C01);
    m.cpu.registers.set_ecx(9);
    m.cpu.registers.set_edx(0);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0D00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 1);
}

#[test]
fn int10_pixel_write_read_round_trips_in_ega_planar() {
    let mut m = int15_machine(16);
    assert!(m.video_mut().set_mode(0x0D));

    m.cpu.registers.set_eax(0x0C0B);
    m.cpu.registers.set_ecx(9);
    m.cpu.registers.set_edx(3);
    m.handle_int10();
    assert_eq!(m.video().planar_read_pixel(9, 3), 0x0B);

    m.cpu.registers.set_eax(0x0D00);
    m.cpu.registers.set_ecx(9);
    m.cpu.registers.set_edx(3);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x0B);
    assert_eq!(m.video().render_active_row(6)[9], 0x13);

    m.cpu.registers.set_eax(0x0C82);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0D00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x09);
}

#[test]
fn int10_pixel_read_write_uses_ega_graphics_page() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();

    m.cpu.registers.set_eax(0x0C0B);
    m.cpu.registers.set_ebx(0x0100);
    m.cpu.registers.set_ecx(9);
    m.cpu.registers.set_edx(3);
    m.handle_int10();

    assert_eq!(m.video().planar_read_pixel(9, 3), 0x00);
    assert_eq!(m.video().planar_read_pixel_at(0x2000, 9, 3), 0x0B);

    m.cpu.registers.set_eax(0x0D00);
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x00);

    m.cpu.registers.set_eax(0x0D00);
    m.cpu.registers.set_ebx(0x0100);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x0B);
}

#[test]
fn ega_graphics_brown_and_bright_colors_render_through_the_dac() {
    // End-to-end guard for the EGA/CGA palette: a guest draws brown (color 6)
    // and the bright eight in an EGA graphics mode, and the composited frame
    // (the same pixels the unit-tester CRC hashes) must show the real RGB, not
    // the 256-color palette3 gray/color ramps that brown and 0x38-0x3F land on.
    // The boot-suite video checks only touch safe colors, so they missed this.
    //
    // Per pixel: AH=0Ch AL=color BH=0 (page) CX=col DX=row, then INT 10h.
    fn draw(code: &mut Vec<u8>, color: u8, col: u16, row: u16) {
        code.extend_from_slice(&[0xB8, color, 0x0C]); // mov ax, 0x0C00 | color
        code.extend_from_slice(&[0xBB, 0x00, 0x00]); // mov bx, 0 (page 0)
        code.push(0xB9);
        code.extend_from_slice(&col.to_le_bytes()); // mov cx, col
        code.push(0xBA);
        code.extend_from_slice(&row.to_le_bytes()); // mov dx, row
        code.extend_from_slice(&[0xCD, 0x10]); // int 0x10
    }

    // Color number -> expected 0x00RRGGBB, the same in both modes: brown, dark
    // gray, bright blue, yellow, bright white, and light gray as a control that
    // was already correct (it never used a remapped DAC entry).
    let samples: [(u8, u32); 6] = [
        (6, 0x00AA_5500),
        (8, 0x0055_5555),
        (9, 0x0055_55FF),
        (14, 0x00FF_FF55),
        (15, 0x00FF_FFFF),
        (7, 0x00AA_AAAA),
    ];

    // Mode 10h (640x350, palette2 via the EGA attribute remap) is 1:1; mode 0Dh
    // (320x200, palette1, the Monkey Island mode) is double-scanned, so source
    // row R lands on output raster rows 2R and 2R+1.
    for (mode, row, scan) in [(0x10u8, 100u16, 1usize), (0x0Du8, 50u16, 2usize)] {
        let mut code = vec![0xB8, mode, 0x00, 0xCD, 0x10]; // mov ax,00<mode>h; int 10h
        for (i, (color, _)) in samples.iter().enumerate() {
            draw(&mut code, *color, 10 + i as u16 * 10, row);
        }
        code.push(0xF4); // hlt

        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            rom_with_code(&code),
        )
        .unwrap();
        assert_eq!(
            machine.run_until_halt_or_cycles(5_000_000).unwrap(),
            StopReason::Halted,
            "mode {mode:#04x} guest ran to hlt"
        );
        // Present two whole frames so the final render is a clean full frame of
        // the drawn VRAM (advance only resets the scanline cursor past one frame).
        let dots = machine.video_mut().frame_dots();
        machine.video_mut().advance(dots * 2);

        let (frame, width, _height) = machine.frame_argb();
        let raster_row = row as usize * scan;
        for (i, (color, want)) in samples.iter().enumerate() {
            let col = 10 + i * 10;
            let got = frame[raster_row * width + col];
            assert_eq!(
                got, *want,
                "mode {mode:#04x} color {color}: got {got:#08x}, want {want:#08x}"
            );
        }
    }
}

#[test]
fn int10_mode_set_bit7_preserves_cga_framebuffer() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();
    m.video_mut().cga_write(0, 0b01_10_11_00);
    assert!(m.video_mut().write_port(0x3D9, 0x31));

    m.cpu.registers.set_eax(0x0084);
    m.handle_int10();

    assert_eq!(m.video().active_mode(), VideoMode::Cga);
    assert_eq!(m.video().cga_read(0), 0b01_10_11_00);
    assert_eq!(m.video().cga_color_select(), 0x00);
    assert_eq!(m.memory.read_u8(0x449).unwrap(), 0x84);
    assert_eq!(m.memory.read_u16(0x44c).unwrap(), 0x4000);

    m.cpu.registers.set_eax(0x0F00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x84);

    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();
    assert_eq!(m.video().cga_read(0), 0);
}

#[test]
fn int10_09_draws_and_xors_font_glyphs_in_cga_graphics() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    m.cpu.registers.set_eax(0x09DB);
    m.cpu.registers.set_ebx(0x0003);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();
    assert_eq!(m.video().cga_read_pixel(0, 0), 3);
    assert_eq!(m.video().cga_read_pixel(7, 7), 3);

    m.cpu.registers.set_eax(0x09DB);
    m.cpu.registers.set_ebx(0x0081);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();
    assert_eq!(m.video().cga_read_pixel(0, 0), 2);
    assert_eq!(m.memory.read_u16(0x450).unwrap(), 0);
}

#[test]
fn int10_09_space_erases_cga_graphics_cell() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    for y in 0..8u16 {
        for x in 0..8u16 {
            assert!(m.video_mut().cga_write_pixel(x, y, 3, false));
        }
    }

    m.cpu.registers.set_eax(0x0920);
    m.cpu.registers.set_ebx(0x0002);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();

    assert_eq!(m.video().cga_read_pixel(0, 0), 0);
    assert_eq!(m.video().cga_read_pixel(7, 7), 0);

    m.cpu.registers.set_eax(0x0800);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0020);
}

#[test]
fn int10_08_recognizes_white_cga_graphics_font_patterns() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    m.cpu.registers.set_eax(0x09DB);
    m.cpu.registers.set_ebx(0x0003);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0800);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x03DB);

    m.video_mut().set_cga_mode(0x04);
    m.cpu.registers.set_eax(0x09DB);
    m.cpu.registers.set_ebx(0x0002);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0800);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x02DB);

    m.video_mut().set_cga_mode(0x06);
    m.cpu.registers.set_eax(0x09DB);
    m.cpu.registers.set_ebx(0x0001);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0800);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x01DB);
}

#[test]
fn int10_cga_graphics_uses_int1f_font_for_high_chars() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    m.write_guest_block(0x40000, &[0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01]);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
    m.cpu.registers.set_ebp(0);
    m.cpu.registers.set_eax(0x1120);
    m.handle_int10();
    assert_eq!(m.read_physical_u16(0x1F * 4), 0);
    assert_eq!(m.read_physical_u16(0x1F * 4 + 2), 0x4000);

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x1234));
    m.cpu.registers.set_ebp(0xFFFF);
    m.cpu.registers.set_ecx(0);
    m.cpu.registers.set_edx(0);
    m.cpu.registers.set_eax(0x1130);
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int10();
    assert_eq!(m.cpu.registers.segment(SegmentIndex::Es).selector, 0x4000);
    assert_eq!(m.cpu.registers.ebp() as u16, 0);
    assert_eq!(m.cpu.registers.ecx() as u16, 8);
    assert_eq!(m.cpu.registers.edx() as u8, 24);

    m.cpu.registers.set_eax(0x0980);
    m.cpu.registers.set_ebx(0x0002);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();

    assert_eq!(m.video().cga_read_pixel(0, 0), 2);
    assert_eq!(m.video().cga_read_pixel(1, 0), 0);
    assert_eq!(m.video().cga_read_pixel(1, 1), 2);

    m.cpu.registers.set_eax(0x0800);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0280);
}

#[test]
fn int10_1130_returns_readable_font_info_pointers() {
    let mut m = int15_machine(16);

    m.cpu.registers.set_eax(0x1130);
    m.cpu.registers.set_ebx(0x0100);
    m.handle_int10();
    assert_eq!(
        m.cpu.registers.segment(SegmentIndex::Es).selector,
        VGA_BIOS_SEGMENT
    );
    assert_eq!(m.cpu.registers.ebp() as u16, VGA_BIOS_FONT_TABLE_OFF);

    m.cpu.registers.set_eax(0x0010);
    m.handle_int10();
    m.cpu.registers.set_eax(0x1130);
    m.cpu.registers.set_ebx(0x0600);
    m.cpu.registers.set_ecx(0xBEEF);
    m.cpu.registers.set_edx(0xAB00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ecx() as u16, 14);
    assert_eq!(m.cpu.registers.edx() as u8, 24);
    let ptr = (u32::from(BIOS_ROM_SEGMENT) << 4) + u32::from(BIOS_FONT_8X16_ROM_OFFSET);
    assert_eq!(
        m.read_physical_u8(ptr + u32::from(b'A') * 16 + 7),
        font::VGAFONT_8X16[usize::from(b'A') * 16 + 7]
    );

    m.cpu.registers.set_eax(0x1130);
    m.cpu.registers.set_ebx(0x0200);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebp() as u16, BIOS_FONT_8X14_ROM_OFFSET);
    let ptr = (u32::from(BIOS_ROM_SEGMENT) << 4) + u32::from(BIOS_FONT_8X14_ROM_OFFSET);
    assert_eq!(
        m.read_physical_u8(ptr + u32::from(b'A') * 14 + 6),
        font::VGAFONT_8X14[usize::from(b'A') * 14 + 6]
    );

    m.cpu.registers.set_eax(0x1130);
    m.cpu.registers.set_ebx(0x0400);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebp() as u16, BIOS_FONT_8X8_HIGH_ROM_OFFSET);
    let ptr = (u32::from(BIOS_ROM_SEGMENT) << 4) + u32::from(BIOS_FONT_8X8_HIGH_ROM_OFFSET);
    assert_eq!(m.read_physical_u8(ptr), font::VGAFONT_8X8[128 * 8]);
}

#[test]
fn int10_04_reports_cga_light_pen_latch() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    let line_dots = m.video().frame_dots() / u64::from(m.video().raster_height());
    m.video_mut().advance(line_dots * 16 + 80);
    assert_eq!(m.video_mut().read_port(0x3DC), Some(0xFF));

    m.cpu.registers.set_eax(0x0400);
    m.handle_int10();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 1);
    assert_eq!(m.cpu.registers.ebx() as u16, 80);
    assert_eq!(m.cpu.registers.ecx() as u16, 0x1000);
    assert_eq!(m.cpu.registers.edx() as u16, 0x020A);

    assert_eq!(m.video_mut().read_port(0x3DB), Some(0xFF));
    m.cpu.registers.set_eax(0x0400);
    m.handle_int10();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0);
}

#[test]
fn int10_teletype_draws_and_scrolls_cga_graphics_text() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    m.cpu.registers.set_eax(0x0EDB);
    m.cpu.registers.set_ebx(0x0002);
    m.handle_int10();
    assert_eq!(m.video().cga_read_pixel(0, 0), 2);
    assert_eq!(m.memory.read_u16(0x450).unwrap(), 1);

    m.video_mut().cga_write_pixel(0, 8, 3, false);
    m.cpu.registers.set_eax(0x0200);
    m.cpu.registers.set_edx(24 << 8);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0E0A);
    m.handle_int10();
    assert_eq!(m.video().cga_read_pixel(0, 0), 3);
    assert_eq!(m.video().cga_read_pixel(0, 192), 0);
    assert_eq!(m.memory.read_u16(0x450).unwrap(), 24 << 8);
}

#[test]
fn int10_scroll_window_moves_cga_graphics_pixels_by_character_rows() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    assert!(m.video_mut().cga_write_pixel(8, 16, 2, false)); // window row 2, col 1
    assert!(m.video_mut().cga_write_pixel(0, 16, 1, false)); // outside window
    m.cpu.registers.set_eax(0x0601);
    m.cpu.registers.set_ebx(0x0300);
    m.cpu.registers.set_ecx((1 << 8) | 1);
    m.cpu.registers.set_edx((3 << 8) | 2);
    m.handle_int10();

    assert_eq!(m.video().cga_read_pixel(8, 8), 2);
    assert_eq!(m.video().cga_read_pixel(8, 24), 3);
    assert_eq!(m.video().cga_read_pixel(0, 16), 1);

    m.video_mut().set_cga_mode(0x04);
    assert!(m.video_mut().cga_write_pixel(8, 16, 2, false));
    m.cpu.registers.set_eax(0x0701);
    m.cpu.registers.set_ebx(0x0100);
    m.cpu.registers.set_ecx((1 << 8) | 1);
    m.cpu.registers.set_edx((3 << 8) | 2);
    m.handle_int10();

    assert_eq!(m.video().cga_read_pixel(8, 24), 2);
    assert_eq!(m.video().cga_read_pixel(8, 8), 1);
}

#[test]
fn int10_scroll_window_clear_fills_cga_graphics_window_only() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    assert!(m.video_mut().cga_write_pixel(8, 8, 1, false));
    assert!(m.video_mut().cga_write_pixel(0, 8, 3, false));
    m.cpu.registers.set_eax(0x0600);
    m.cpu.registers.set_ebx(0x0200);
    m.cpu.registers.set_ecx((1 << 8) | 1);
    m.cpu.registers.set_edx((2 << 8) | 2);
    m.handle_int10();

    assert_eq!(m.video().cga_read_pixel(8, 8), 2);
    assert_eq!(m.video().cga_read_pixel(16, 16), 2);
    assert_eq!(m.video().cga_read_pixel(0, 8), 3);
}

#[test]
fn int10_13_draws_attributed_string_in_cga_graphics() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();
    m.write_guest_block(0x4000, &[0xDB, 0x01, 0xDB, 0x02]);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebp(0x4000);
    m.cpu.registers.set_eax(0x1303);
    m.cpu.registers.set_ecx(2);
    m.cpu.registers.set_edx(0);
    m.handle_int10();

    assert_eq!(m.video().cga_read_pixel(0, 0), 1);
    assert_eq!(m.video().cga_read_pixel(8, 0), 2);
    assert_eq!(m.memory.read_u16(0x450).unwrap(), 2);
}

#[test]
fn int10_ega_graphics_text_services_draw_visible_planar_glyphs() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0012);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x484), 29);
    assert_eq!(m.read_physical_u8(0x485), 16);

    m.cpu.registers.set_eax(0x09DB);
    m.cpu.registers.set_ebx(0x000C);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();
    assert_eq!(m.video().planar_read_pixel(0, 0), 0x0C);

    m.cpu.registers.set_eax(0x0800);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0CDB);

    m.write_guest_block(0x6000, &[0xDB, 0x05]);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebp(0x6000);
    m.cpu.registers.set_eax(0x1303);
    m.cpu.registers.set_ecx(1);
    m.cpu.registers.set_edx((1 << 8) | 1);
    m.handle_int10();
    assert_eq!(m.video().planar_read_pixel(8, 16), 0x05);
    assert_eq!(m.memory.read_u16(0x450).unwrap(), (1 << 8) | 2);

    assert!(m.video_mut().planar_write_pixel(0, 16, 3, false));
    m.cpu.registers.set_eax(0x0200);
    m.cpu.registers.set_edx(29 << 8);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0E0A);
    m.handle_int10();
    assert_eq!(m.video().planar_read_pixel(0, 0), 3);
    assert_eq!(m.video().planar_read_pixel(0, 29 * 16), 0);
    assert_eq!(m.memory.read_u16(0x450).unwrap(), 29 << 8);
}

#[test]
fn int10_ega_graphics_text_services_use_bh_page() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();

    m.cpu.registers.set_eax(0x0200);
    m.cpu.registers.set_ebx(0x0100);
    m.cpu.registers.set_edx(0);
    m.handle_int10();

    m.cpu.registers.set_eax(0x09DB);
    m.cpu.registers.set_ebx(0x0105);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();

    assert_eq!(m.video().planar_read_pixel(0, 0), 0);
    assert_eq!(m.video().planar_read_pixel_at(0x2000, 0, 0), 5);

    m.cpu.registers.set_eax(0x0800);
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0020);

    m.cpu.registers.set_eax(0x0800);
    m.cpu.registers.set_ebx(0x0100);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x05DB);
}

#[test]
fn int10_ega_graphics_font_services_feed_planar_text_output() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0012);
    m.handle_int10();

    let mut font = vec![0u8; 256 * 16];
    font[usize::from(b'A') * 16] = 0x80;
    m.write_guest_block(0x7000, &font);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebp(0x7000);
    m.cpu.registers.set_eax(0x1121);
    m.cpu.registers.set_ebx(0x0000);
    m.cpu.registers.set_ecx(16);
    m.cpu.registers.set_edx(30);
    m.handle_int10();

    assert_eq!(m.read_physical_u8(0x484), 29);
    assert_eq!(m.read_physical_u8(0x485), 16);
    m.cpu.registers.set_eax(0x0941);
    m.cpu.registers.set_ebx(0x0007);
    m.cpu.registers.set_ecx(1);
    m.handle_int10();
    assert_eq!(m.video().planar_read_pixel(0, 0), 7);
    assert_eq!(m.video().planar_read_pixel(1, 0), 0);

    m.cpu.registers.set_eax(0x1123);
    m.cpu.registers.set_ebx(0x0003);
    m.cpu.registers.set_edx(0);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x484), 42);
    assert_eq!(m.read_physical_u8(0x485), 8);

    m.cpu.registers.set_eax(0x0200);
    m.cpu.registers.set_edx(42 << 8);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0EDB);
    m.cpu.registers.set_ebx(0x0002);
    m.handle_int10();
    assert_eq!(m.video().planar_read_pixel(0, 42 * 8), 2);

    m.cpu.registers.set_eax(0x1122);
    m.cpu.registers.set_ebx(0x0002);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x484), 24);
    assert_eq!(m.read_physical_u8(0x485), 14);

    m.cpu.registers.set_eax(0x1124);
    m.cpu.registers.set_ebx(0x0000);
    m.cpu.registers.set_edx(30);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x484), 29);
    assert_eq!(m.read_physical_u8(0x485), 16);
}

#[test]
fn int10_write_string_places_chars_and_attr_in_text_buffer() {
    let mut m = int15_machine(16);
    m.video_mut().set_text_mode();
    // Place a 3-char string "Hi!" at ES:BP = 0x0000:0x4000 (physical 0x4000).
    m.write_physical_u8(0x4000, b'H');
    m.write_physical_u8(0x4001, b'i');
    m.write_physical_u8(0x4002, b'!');
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebp(0x4000);
    // AH=13h AL=01 (advance cursor, no attr bytes), BL=attr 0x1E, CX=3,
    // DH=row 4, DL=col 10.
    m.cpu.registers.set_eax(0x1301);
    m.cpu.registers.set_ebx(0x001E);
    m.cpu.registers.set_ecx(3);
    m.cpu.registers.set_edx((4 << 8) | 10);
    m.handle_int10();
    // The chars and attribute landed at row 4, col 10.. of the text buffer.
    let base = (4 * 80 + 10) * 2;
    assert_eq!(m.video().read_u8(base).unwrap(), b'H');
    assert_eq!(m.video().read_u8(base + 1).unwrap(), 0x1E);
    assert_eq!(m.video().read_u8(base + 2).unwrap(), b'i');
    assert_eq!(m.video().read_u8(base + 4).unwrap(), b'!');
    // AL bit 0 set leaves the BDA cursor at the end of the string (col 13).
    assert_eq!(m.memory.read_u16(0x450).unwrap(), (4 << 8) | 13);
}

#[test]
fn int10_write_string_honors_interleaved_attribute_bytes() {
    let mut m = int15_machine(16);
    m.video_mut().set_text_mode();
    // AL bit 1 set: the source is char,attr,char,attr. "Ab" with attrs 0x12,0x34.
    m.write_physical_u8(0x5000, b'A');
    m.write_physical_u8(0x5001, 0x12);
    m.write_physical_u8(0x5002, b'b');
    m.write_physical_u8(0x5003, 0x34);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebp(0x5000);
    m.cpu.registers.set_eax(0x1302); // AL bit1 = interleaved attrs, bit0 clear
    m.cpu.registers.set_ebx(0x0000);
    m.cpu.registers.set_ecx(2);
    m.cpu.registers.set_edx(0); // row 0, col 0
    m.handle_int10();
    assert_eq!(m.video().read_u8(0).unwrap(), b'A');
    assert_eq!(m.video().read_u8(1).unwrap(), 0x12);
    assert_eq!(m.video().read_u8(2).unwrap(), b'b');
    assert_eq!(m.video().read_u8(3).unwrap(), 0x34);
}

#[test]
fn int10_save_restore_state_round_trips_the_bda_block() {
    let mut m = int15_machine(16);
    // AL=00 reports the buffer size in 64-byte blocks (99 bytes -> 2 blocks).
    m.cpu.registers.set_eax(0x1C00);
    m.cpu.registers.set_ecx(0x0002);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 2, "two 64-byte blocks");
    assert_eq!(m.cpu.registers.eax() as u8, 0x1C);
    m.cpu.registers.set_eax(0x1C00);
    m.cpu.registers.set_ecx(0x0007);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 15, "full state block count");
    // Mark the BDA edge bytes, save into ES:BX, change them, then restore.
    let _ = m.memory.write_u8(0x449, 0x12);
    let _ = m.memory.write_u16(BDA_VIDEO_SAVE_POINTER, 0x1234);
    let _ = m.memory.write_u16(BDA_VIDEO_SAVE_POINTER + 2, 0xabcd);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebx(0x6000);
    m.cpu.registers.set_eax(0x1C01); // save
    m.cpu.registers.set_ecx(0x0002);
    m.handle_int10();
    // Corrupt the live BDA, then restore it from the saved buffer.
    let _ = m.memory.write_u8(0x449, 0x99);
    let _ = m.memory.write_u16(BDA_VIDEO_SAVE_POINTER, 0);
    let _ = m.memory.write_u16(BDA_VIDEO_SAVE_POINTER + 2, 0);
    m.cpu.registers.set_ebx(0x6000);
    m.cpu.registers.set_eax(0x1C02); // restore
    m.handle_int10();
    assert_eq!(m.memory.read_u8(0x449).unwrap(), 0x12, "BDA mode restored");
    assert_eq!(
        m.memory.read_u16(BDA_VIDEO_SAVE_POINTER).unwrap(),
        0x1234,
        "video-save pointer offset restored"
    );
    assert_eq!(
        m.memory.read_u16(BDA_VIDEO_SAVE_POINTER + 2).unwrap(),
        0xabcd,
        "video-save pointer segment restored"
    );
}

#[test]
fn int10_save_restore_state_round_trips_hardware_registers() {
    let mut m = int15_machine(16);
    m.video_mut().write_port(0x3C4, 0x02);
    m.video_mut().write_port(0x3C5, 0x05);
    m.video_mut().write_port(0x3D4, 0x0A);
    m.video_mut().write_port(0x3D5, 0x12);
    m.video_mut().write_port(0x3CE, 0x08);
    m.video_mut().write_port(0x3CF, 0xA5);
    m.video_mut().set_attr_register(0x12, 0x06);
    m.video_mut().write_port(0x3DA, 0x77);

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebx(0x6000);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_eax(0x1C01);
    m.handle_int10();

    m.video_mut().write_port(0x3C4, 0x02);
    m.video_mut().write_port(0x3C5, 0x0F);
    m.video_mut().write_port(0x3D4, 0x0A);
    m.video_mut().write_port(0x3D5, 0x01);
    m.video_mut().write_port(0x3CE, 0x08);
    m.video_mut().write_port(0x3CF, 0x5A);
    m.video_mut().set_attr_register(0x12, 0x00);
    m.video_mut().write_port(0x3DA, 0x11);

    m.cpu.registers.set_ebx(0x6000);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_eax(0x1C02);
    m.handle_int10();

    m.video_mut().write_port(0x3C4, 0x02);
    assert_eq!(m.video_mut().read_port(0x3C5), Some(0x05));
    assert_eq!(color_crtc_reg(&mut m, 0x0A), 0x12);
    m.video_mut().write_port(0x3CE, 0x08);
    assert_eq!(m.video_mut().read_port(0x3CF), Some(0xA5));
    assert_eq!(m.video().attr_register(0x12), 0x06);
    assert_eq!(m.video_mut().read_port(0x3CA), Some(0x77));
}

#[test]
fn int10_save_restore_state_round_trips_cga_output_only_registers() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    m.video_mut().write_port(0x3D8, 0x0A);
    m.video_mut().write_port(0x3D9, 0x35);
    for (index, value) in [(0x01, 0x20), (0x09, 0x01), (0x0A, 0x06), (0x0B, 0x07)] {
        m.video_mut().write_port(0x3D4, index);
        m.video_mut().write_port(0x3D5, value);
    }

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebx(0x6000);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_eax(0x1C01);
    m.handle_int10();

    m.video_mut().write_port(0x3D8, 0x1A);
    m.video_mut().write_port(0x3D9, 0x00);
    for (index, value) in [(0x01, 0x28), (0x09, 0x07), (0x0A, 0x01), (0x0B, 0x02)] {
        m.video_mut().write_port(0x3D4, index);
        m.video_mut().write_port(0x3D5, value);
    }

    m.cpu.registers.set_ebx(0x6000);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_eax(0x1C02);
    m.handle_int10();

    assert_eq!(m.video().active_mode(), VideoMode::Cga);
    assert_eq!(m.video().cga_mode_control(), 0x0A);
    assert_eq!(m.video().cga_color_select(), 0x35);
    assert_eq!(m.video().crtc_register_latch(0x01), 0x20);
    assert_eq!(m.video().crtc_register_latch(0x09), 0x01);
    assert_eq!(m.video().crtc_register_latch(0x0A), 0x06);
    assert_eq!(m.video().crtc_register_latch(0x0B), 0x07);
    assert_eq!(m.video().crtc_index_latch(), 0x0B);
    assert_eq!(m.video().raster_width(), 256);
    m.video_mut().write_port(0x3D4, 0x01);
    assert_eq!(m.video_mut().read_port(0x3D5), None);
}

#[test]
fn int10_save_restore_state_reenters_cga_text_from_planar_mode() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0002);
    m.handle_int10();
    m.video_mut().write_port(0x3D9, 0x15);

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebx(0x6400);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_eax(0x1C01);
    m.handle_int10();

    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();
    assert_eq!(m.video().active_mode(), VideoMode::Planar);

    m.cpu.registers.set_ebx(0x6400);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_eax(0x1C02);
    m.handle_int10();

    assert_eq!(m.video().active_mode(), VideoMode::Text);
    assert!(m.video().is_cga_personality());
    assert_eq!(m.video().cga_mode_control(), 0x2D);
    assert_eq!(m.video().cga_color_select(), 0x15);
    assert_eq!(m.video().raster_width(), 640);
    m.video_mut().write_port(0x3D4, 0x01);
    assert_eq!(m.video_mut().read_port(0x3D5), None);
}

#[test]
fn int10_save_restore_state_round_trips_dac_without_grayscale_summing() {
    let mut m = int15_machine(16);
    m.video_mut().set_grayscale_summing_enabled(false);
    m.video_mut().set_dac_entry(5, 1, 2, 3);
    m.video_mut().write_port(0x3C6, 0x0F);
    m.video_mut().set_attr_register(0x14, 0x0C);
    m.video_mut().write_port(0x3C8, 0x22);
    m.video_mut().set_grayscale_summing_enabled(true);

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebx(0x7000);
    m.cpu.registers.set_ecx(0x0004);
    m.cpu.registers.set_eax(0x1C01);
    m.handle_int10();

    m.video_mut().set_dac_entry(5, 63, 0, 0);
    m.video_mut().write_port(0x3C6, 0xFF);
    m.video_mut().set_attr_register(0x14, 0x00);
    m.video_mut().write_port(0x3C8, 0x00);

    m.cpu.registers.set_ebx(0x7000);
    m.cpu.registers.set_ecx(0x0004);
    m.cpu.registers.set_eax(0x1C02);
    m.handle_int10();

    assert_eq!(m.video().dac_entry(5), [1, 2, 3]);
    assert_eq!(m.video_mut().read_port(0x3C6), Some(0x0F));
    assert_eq!(m.video().attr_register(0x14), 0x0C);
    assert_eq!(m.video_mut().read_port(0x3C8), Some(0x22));
    assert!(m.video().grayscale_summing_enabled());
}

#[test]
fn int15_c0_reports_honest_feature_byte() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC000);
    m.handle_int15();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x00,
        "AH = 00 on success"
    );
    // ES:BX points at the seeded config table.
    let es = m.cpu.registers.segment(SegmentIndex::Es).base;
    let bx = m.cpu.registers.ebx() as u16;
    let addr = es + u32::from(bx);
    let len = m.read_guest_word(addr);
    assert_eq!(len, 8, "table reports 8 bytes following");
    assert_eq!(m.read_physical_u8(addr + 2), 0xFC, "AT-class model byte");
    let feature1 = m.read_physical_u8(addr + 5);
    assert_eq!(feature1 & 0x40, 0x40, "second PIC present");
    assert_eq!(feature1 & 0x20, 0x20, "RTC present");
    assert_eq!(feature1 & 0x04, 0x04, "EBDA allocated");
    assert_eq!(
        feature1 & 0x10,
        0x00,
        "no AH=4Fh keyboard-intercept callout"
    );
    assert_eq!(feature1 & 0x08, 0x00, "wait-for-event not supported");
    assert_eq!(feature1 & 0x02, 0x00, "ISA bus, not Micro Channel");
}

#[test]
fn int15_c1_returns_ebda_segment_and_size_byte() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC100);
    m.handle_int15();
    assert_eq!(
        m.cpu.registers.segment(SegmentIndex::Es).selector,
        0x9FC0,
        "ES = EBDA segment"
    );
    // The EBDA size byte at 0x9FC00 reports 1 KB, and INT 12h dropped to 639.
    assert_eq!(m.memory.read_u8(0x9FC00).unwrap(), 1, "EBDA size = 1 KB");
    assert_eq!(
        m.memory.read_u16(0x413).unwrap(),
        639,
        "conventional lowered"
    );
}

#[test]
fn int13_ah05_format_track_fills_with_f6() {
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 737_280]).unwrap(); // 720 KB, 9 spt
    // AH=05 AL=9 sectors, CH=3 (track 3), DH=1 (head 1), DL=0 (A:).
    m.cpu.registers.set_eax(0x0509);
    m.cpu.registers.set_ecx(0x0300); // CH=3, CL=0
    m.cpu.registers.set_edx(0x0100); // DH=1, DL=0
    m.handle_int13();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x00,
        "AH = 00 on success"
    );
    // The BDA last-disk-status byte records success. (CF rides the IRET frame,
    // which a direct handler call has no real stack for; AH and 0040:0041 carry
    // the result either way.)
    assert_eq!(
        m.memory.read_u8(0x441).unwrap(),
        0x00,
        "disk status = success"
    );
    // A CHS read of that track returns the 0xF6 filler.
    let sector = m
        .floppy
        .as_ref()
        .unwrap()
        .read_sector(3, 1, 1)
        .unwrap()
        .to_vec();
    assert_eq!(sector[0], 0xF6);
    assert_eq!(sector[511], 0xF6);
}

#[test]
fn int13_ah05_format_track_rejects_bad_track_and_fixed_disk() {
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 737_280]).unwrap(); // 80 cylinders, 2 heads
    // Track 80 is off an 80-cylinder disk: AH=0Ch bad track.
    m.cpu.registers.set_eax(0x0509);
    m.cpu.registers.set_ecx(0x5000); // CH=0x50 = 80
    m.cpu.registers.set_edx(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x0C, "bad-track error");
    assert_eq!(m.memory.read_u8(0x441).unwrap(), 0x0C, "status = bad track");
    // The track was not formatted: its first sector is still zero, not 0xF6.
    assert_eq!(
        m.floppy.as_ref().unwrap().read_sector(0, 0, 1).unwrap()[0],
        0x00
    );
    // A fixed-disk unit (DL>=0x80) reports no such drive (AH=0x80).
    m.cpu.registers.set_eax(0x0509);
    m.cpu.registers.set_ecx(0x0000);
    m.cpu.registers.set_edx(0x0080); // DL = 0x80
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x80, "no fixed disk");
    assert_eq!(m.memory.read_u8(0x441).unwrap(), 0x80, "status = no drive");
}

#[test]
fn int13_ah16_reports_floppy_not_changed() {
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 1_474_560]).unwrap();
    prime_dos_int_frame(&mut m);

    m.cpu.registers.set_eax(0x1600);
    m.cpu.registers.set_edx(0x0000);
    m.handle_int13();

    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0");
    assert_eq!(m.memory.read_u8(0x441).unwrap(), 0x00, "status = success");
    assert_eq!(dos_int_flags(&m) & 0x0001, 0, "CF clear");
}

#[test]
fn int13_ah17_validates_floppy_format_class() {
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 737_280]).unwrap(); // 720 KB

    m.cpu.registers.set_eax(0x1704);
    m.cpu.registers.set_edx(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "720 KB accepted");

    m.cpu.registers.set_eax(0x1703);
    m.cpu.registers.set_edx(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x0c, "1.2 MB rejected");
    assert_eq!(
        m.memory.read_u8(0x441).unwrap(),
        0x0c,
        "status = unsupported media"
    );
}

#[test]
fn int13_ah18_returns_diskette_parameter_table_for_current_media() {
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 1_474_560]).unwrap(); // 80 cyl, 18 spt
    prime_dos_int_frame(&mut m);

    m.cpu.registers.set_eax(0x1800);
    m.cpu.registers.set_ecx(0x4f12); // max cylinder 79, 18 sectors
    m.cpu.registers.set_edx(0x0000);
    m.cpu.registers.set_edi(0xCAFE_1234);
    m.handle_int13();

    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0");
    assert_eq!(dos_int_flags(&m) & 0x0001, 0, "CF clear");
    let es = m.cpu.registers.segment(SegmentIndex::Es).base;
    let di = m.cpu.registers.edi() as u16;
    assert_eq!(
        es + u32::from(di),
        BIOS_DISKETTE_PARAMETER_TABLE_ADDR,
        "ES:DI points at the DPT"
    );
}

/// A small hard-disk image whose first byte per sector marks the LBA, plus an
/// otherwise-zero machine with the disk mounted as C:.
fn machine_with_hdd(sectors: usize) -> Machine {
    let mut bytes = vec![0u8; sectors * 512];
    for s in 0..sectors {
        bytes[s * 512] = (s as u8).wrapping_add(0x10);
    }
    let mut m = int15_machine(16);
    m.mount_hdd(bytes);
    m
}

#[test]
fn mount_hdd_seeds_the_bda_fixed_disk_count() {
    let m = machine_with_hdd(64);
    assert_eq!(m.memory.read_u8(0x475).unwrap(), 1, "one fixed disk");
}

#[test]
fn apply_overrides_replaces_by_name_and_appends_new() {
    let mut base = vec![
        ("AUTOEXEC.BAT".to_string(), b"old".to_vec()),
        ("KERNEL.SYS".to_string(), b"k".to_vec()),
    ];
    apply_overrides(
        &mut base,
        vec![
            ("autoexec.bat".to_string(), b"new".to_vec()), // case-insensitive replace
            ("RUNNER.COM".to_string(), b"r".to_vec()),     // append
        ],
    );
    let autoexec = base
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("autoexec.bat"))
        .unwrap();
    assert_eq!(autoexec.1, b"new");
    // A replace updates bytes in place but keeps the original key's case
    // (KateaTreeVolume folds names case-insensitively, so the stored case is
    // cosmetic — pinned here so the intent is explicit).
    assert_eq!(
        autoexec.0, "AUTOEXEC.BAT",
        "original key case preserved on replace"
    );
    assert!(base.iter().any(|(n, b)| n == "KERNEL.SYS" && b == b"k"));
    assert!(base.iter().any(|(n, b)| n == "RUNNER.COM" && b == b"r"));
    assert_eq!(base.len(), 3, "one replace + one append");
}

#[test]
fn ensure_user_config_seeds_missing_files_only() {
    let dir = std::env::temp_dir().join(format!("katea_cfg_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // A user-owned AUTOEXEC stays; a missing CONFIG.SYS is seeded.
    std::fs::write(dir.join("AUTOEXEC.BAT"), b"@ECHO OFF\r\nMYGAME\r\n").unwrap();
    super::ensure_user_config(&dir, b"FILES=40\r\n", b"@ECHO OFF\r\nDEFAULT\r\n").unwrap();
    assert_eq!(
        std::fs::read(dir.join("AUTOEXEC.BAT")).unwrap(),
        b"@ECHO OFF\r\nMYGAME\r\n",
        "the user's AUTOEXEC must not be overwritten"
    );
    assert_eq!(
        std::fs::read(dir.join("CONFIG.SYS")).unwrap(),
        b"FILES=40\r\n",
        "a missing CONFIG.SYS is seeded with the default"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn user_folder_overlay_keeps_binaries_drops_config() {
    let payload = vec![
        ("KERNEL.SYS".to_string(), vec![1u8]),
        ("COMMAND.COM".to_string(), vec![2u8]),
        ("CONFIG.SYS".to_string(), vec![3u8]),
        ("AUTOEXEC.BAT".to_string(), vec![4u8]),
        ("HELLO.TXT".to_string(), vec![5u8]),
        ("LICENSE.TXT".to_string(), vec![6u8]),
        ("TOKAMOUS.COM".to_string(), vec![7u8]),
    ];
    let names: Vec<String> = super::user_folder_overlay(payload)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(names.contains(&"KERNEL.SYS".to_string()));
    assert!(names.contains(&"TOKAMOUS.COM".to_string()));
    assert!(names.contains(&"LICENSE.TXT".to_string()));
    assert!(
        !names.contains(&"CONFIG.SYS".to_string()),
        "config is the user's"
    );
    assert!(
        !names.contains(&"AUTOEXEC.BAT".to_string()),
        "autoexec is the user's"
    );
    assert!(
        !names.contains(&"HELLO.TXT".to_string()),
        "demo file dropped"
    );
}

#[test]
fn flush_hdd_folder_runs_a_final_reconcile() {
    // Mount a temp folder, then flush; confirm flush is callable and a no-op on
    // an unwritten folder (creates nothing beyond the config mount seeds). The
    // end-to-end create/overwrite/grow is covered by the e2e smoke test; this
    // only proves the plumbing exists.
    let dir = std::env::temp_dir().join(format!("katea_flush_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut m = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    m.mount_hdd_folder(&dir).unwrap();
    // mount_hdd_folder seeds the user-owned CONFIG.SYS/AUTOEXEC.BAT.
    let listing = |dir: &std::path::Path| -> std::collections::BTreeSet<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    };
    let after_mount = listing(&dir);
    assert!(
        after_mount.contains("CONFIG.SYS") && after_mount.contains("AUTOEXEC.BAT"),
        "mount seeds the user-owned config"
    );
    m.flush_hdd_folder();
    // With nothing written by the guest, flush creates nothing new.
    assert_eq!(
        listing(&dir),
        after_mount,
        "flush on an unwritten folder creates nothing beyond the seed"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn mount_hdd_publishes_int41_fixed_disk_parameter_table() {
    let mut m = machine_with_hdd(4032); // 4 cylinders, 16 heads, 63 spt
    let off = read_u16(&mut m, 0x41 * 4);
    let seg = read_u16(&mut m, 0x41 * 4 + 2);
    let table = (u32::from(seg) << 4) + u32::from(off);
    assert_eq!(table, BIOS_FIXED_DISK_PARAMETER_TABLE_ADDR);
    assert_eq!(read_u16(&mut m, table), 4, "cylinder count");
    assert_eq!(m.read_physical_u8(table + 2), 16, "head count");
    assert_eq!(read_u16(&mut m, table + 3), 0, "no XT reduced current");
    assert_eq!(read_u16(&mut m, table + 5), 0, "no write precomp");
    assert_eq!(m.read_physical_u8(table + 8), 0x08, "more-than-8-heads bit");
    assert_eq!(read_u16(&mut m, table + 12), 4, "landing zone");
    assert_eq!(m.read_physical_u8(table + 14), 63, "sectors per track");

    let bytes = m.eject_hdd().unwrap();
    assert_eq!(bytes.len(), 4032 * 512);
    assert_eq!(m.memory.read_u8(0x475).unwrap(), 0, "no fixed disks");
    assert_eq!(read_u16(&mut m, 0x41 * 4), 0, "INT 41h offset cleared");
    assert_eq!(read_u16(&mut m, 0x41 * 4 + 2), 0, "INT 41h segment cleared");
}

#[test]
fn int46_secondary_fixed_disk_parameter_table_is_absent() {
    let mut m = machine_with_hdd(4032);
    assert_eq!(read_u16(&mut m, 0x46 * 4), 0, "INT 46h offset absent");
    assert_eq!(read_u16(&mut m, 0x46 * 4 + 2), 0, "INT 46h segment absent");

    let bytes = m.eject_hdd().unwrap();
    assert_eq!(bytes.len(), 4032 * 512);
    assert_eq!(
        read_u16(&mut m, 0x46 * 4),
        0,
        "INT 46h offset remains absent"
    );
    assert_eq!(
        read_u16(&mut m, 0x46 * 4 + 2),
        0,
        "INT 46h segment remains absent"
    );
}

#[test]
fn int13_ah02_reads_a_hard_disk_sector_through_es_bx() {
    let mut m = machine_with_hdd(4032); // 16*63 = one cylinder of 1008, 4 cyls
    // Read LBA 63 (CHS cyl 0, head 1, sector 1). AL=1, CH=0, CL=1 (sector),
    // DH=1 (head), DL=0x80 (C:), ES:BX = 4000:0000 (physical 0x40000).
    m.cpu.registers.set_eax(0x0201);
    m.cpu.registers.set_ecx(0x0001); // CH=0, CL=1
    m.cpu.registers.set_edx(0x0180); // DH=1, DL=0x80
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
    assert_eq!(m.cpu.registers.eax() as u8, 0x01, "AL=1 sector moved");
    // The marker for LBA 63 is 63 + 0x10.
    assert_eq!(m.read_physical_u8(0x4_0000), 63u8.wrapping_add(0x10));
}

#[test]
fn int13_ah03_write_then_ah02_read_round_trips() {
    let mut m = machine_with_hdd(64);
    // Seed a pattern in a guest buffer at ES:BX = 2000:0000 (0x20000).
    for i in 0..512u32 {
        m.write_physical_u8(0x2_0000 + i, (i & 0xff) as u8 ^ 0x5A);
    }
    // Write LBA 0 (CHS 0,0,1): AH=03 AL=1, CH=0 CL=1, DH=0 DL=0x80.
    m.cpu.registers.set_eax(0x0301);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_edx(0x0080);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x2000));
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "write AH=0");
    assert!(m.hdd_dirty(), "the write marked the image dirty");

    // Read it back into a fresh buffer at 3000:0000.
    m.cpu.registers.set_eax(0x0201);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_edx(0x0080);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x3000));
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "read AH=0");
    for i in 0..512u32 {
        assert_eq!(m.read_physical_u8(0x3_0000 + i), (i & 0xff) as u8 ^ 0x5A);
    }
}

#[test]
fn int13_ah0a_read_long_includes_synthetic_ecc_bytes() {
    let mut m = machine_with_hdd(64);
    for i in 0..516u32 {
        m.write_physical_u8(0x4_0000 + i, 0xAA);
    }

    m.cpu.registers.set_eax(0x0A01);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_edx(0x0080);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int13();

    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0");
    assert_eq!(m.cpu.registers.eax() as u8, 0x01, "AL=1 sector moved");
    assert_eq!(m.read_physical_u8(0x4_0000), 0x10, "sector data copied");
    for i in 0..4u32 {
        assert_eq!(
            m.read_physical_u8(0x4_0000 + 512 + i),
            0x00,
            "synthetic ECC byte {i}"
        );
    }
}

#[test]
fn int13_ah0b_write_long_ignores_ecc_bytes() {
    let mut m = machine_with_hdd(64);
    for i in 0..512u32 {
        m.write_physical_u8(0x2_0000 + i, (i as u8).wrapping_mul(3));
    }
    for i in 0..4u32 {
        m.write_physical_u8(0x2_0000 + 512 + i, 0xE0 + i as u8);
    }

    m.cpu.registers.set_eax(0x0B01);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_edx(0x0080);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x2000));
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "write long AH=0");

    m.cpu.registers.set_eax(0x0201);
    m.cpu.registers.set_ecx(0x0001);
    m.cpu.registers.set_edx(0x0080);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x3000));
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "read AH=0");
    for i in 0..512u32 {
        assert_eq!(m.read_physical_u8(0x3_0000 + i), (i as u8).wrapping_mul(3));
    }
}

#[test]
fn int13_ah08_reports_hard_disk_geometry() {
    let mut m = machine_with_hdd(4032); // 4 cylinders, 16 heads, 63 spt
    m.cpu.registers.set_eax(0x0800);
    m.cpu.registers.set_edx(0x0080); // DL = C:
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
    let cx = m.cpu.registers.ecx() as u16;
    let dx = m.cpu.registers.edx() as u16;
    let cl = cx as u8;
    let ch = (cx >> 8) as u8;
    let sectors = cl & 0x3f;
    let max_cyl = u16::from(ch) | (u16::from(cl & 0xc0) << 2);
    assert_eq!(sectors, 63, "63 sectors per track");
    assert_eq!(max_cyl, 3, "max cylinder index = 4 - 1");
    assert_eq!((dx >> 8) as u8, 15, "max head index = 16 - 1");
    assert_eq!(dx as u8, 1, "one fixed disk in DL");
}

#[test]
fn int13_ah15_reports_fixed_disk_dasd_and_capacity() {
    let mut m = machine_with_hdd(4032);
    m.cpu.registers.set_eax(0x1500);
    m.cpu.registers.set_edx(0x0080);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x03, "AH=03 fixed disk");
    let cx = m.cpu.registers.ecx() as u16;
    let dx = m.cpu.registers.edx() as u16;
    let total = (u32::from(cx) << 16) | u32::from(dx);
    assert_eq!(total, 4032, "CX:DX = total sectors");
}

#[test]
fn int13_hard_disk_read_past_end_sets_carry() {
    let mut m = machine_with_hdd(8); // 8 sectors, all on cylinder 0
    // Read at CHS that maps past the image (cyl 1 does not exist).
    m.cpu.registers.set_eax(0x0201);
    m.cpu.registers.set_ecx(0x0001); // sector 1
    m.cpu.registers.set_edx(0x0180); // head 1, DL=0x80
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int13();
    // head 1 * 63 spt = LBA 63, past an 8-sector disk: sector-not-found.
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x04, "AH=04 not found");
    assert_eq!(m.memory.read_u8(0x474).unwrap(), 0x04, "fixed-disk status");
}

#[test]
fn int13_ah41_edd_install_check() {
    let mut m = machine_with_hdd(64);
    m.cpu.registers.set_eax(0x4100);
    m.cpu.registers.set_ebx(0x55AA); // the documented input magic
    m.cpu.registers.set_edx(0x0080);
    m.handle_int13();
    assert_eq!(m.cpu.registers.ebx() as u16, 0xAA55, "BX=0xAA55 present");
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x30, "EDD version 3.0");
    assert_eq!(m.cpu.registers.ecx() as u16 & 0x0001, 0x0001, "ext access");
}

#[test]
fn int13_legacy_fixed_disk_controls_report_status() {
    let mut m = machine_with_hdd(64);

    m.cpu.registers.set_eax(0x12ff);
    m.cpu.registers.set_edx(0x0080);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=12 success");
    assert_eq!(m.cpu.registers.eax() as u8, 0x00, "AL=0 diagnostic code");

    m.cpu.registers.set_eax(0x1300);
    m.cpu.registers.set_edx(0x0080);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=13 success");

    m.cpu.registers.set_eax(0x1900);
    m.cpu.registers.set_edx(0x0080);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=19 success");

    m.cpu.registers.set_eax(0x0600);
    m.cpu.registers.set_edx(0x0080);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x01, "format rejected");
    assert_eq!(m.memory.read_u8(0x474).unwrap(), 0x01, "fixed status");
}

#[test]
fn int13_ah42_extended_read_via_disk_address_packet() {
    let mut m = machine_with_hdd(64);
    // Build a Disk Address Packet at DS:SI = 5000:0000 (physical 0x50000):
    // size 16, reserved 0, blocks 1, reserved 0, buffer 6000:0000, LBA 7.
    let dap = 0x5_0000u32;
    m.write_physical_u8(dap, 16); // packet size
    m.write_physical_u8(dap + 2, 1); // block count
    // buffer offset (0) at 4-5, segment 0x6000 at 6-7.
    m.write_physical_u8(dap + 6, 0x00);
    m.write_physical_u8(dap + 7, 0x60);
    m.write_physical_u8(dap + 8, 7); // LBA low byte = 7
    m.cpu.registers.set_eax(0x4200);
    m.cpu.registers.set_edx(0x0080);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x5000));
    m.cpu.registers.set_esi(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
    // The buffer at 0x60000 holds the LBA 7 marker (7 + 0x10).
    assert_eq!(m.read_physical_u8(0x6_0000), 7u8.wrapping_add(0x10));
    // The packet's block count was rewritten to 1 (sectors moved).
    assert_eq!(m.read_physical_u8(dap + 2), 1);
}

#[test]
fn int13_ah48_extended_drive_params() {
    let mut m = machine_with_hdd(4032);
    let buf = 0x5_0000u32;
    m.cpu.registers.set_eax(0x4800);
    m.cpu.registers.set_edx(0x0080);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x5000));
    m.cpu.registers.set_esi(0x0000);
    m.handle_int13();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
    let total = (0..8u32).fold(0u64, |acc, i| {
        acc | (u64::from(m.read_physical_u8(buf + 16 + i)) << (i * 8))
    });
    assert_eq!(total, 4032, "qword total sectors");
    let bps =
        u16::from(m.read_physical_u8(buf + 24)) | (u16::from(m.read_physical_u8(buf + 25)) << 8);
    assert_eq!(bps, 512, "bytes per sector");
}

#[test]
fn primary_channel_ports_read_open_bus_when_empty() {
    // With no disk mounted, the primary channel reads 0xFF (open bus) so a
    // probe sees no device, and a write is harmlessly dropped.
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x1F2, BusWidth::Byte, 0x55, false).unwrap();
        let v = bus.read_io(0x1F7, BusWidth::Byte, 0, false).unwrap();
        assert_eq!(v, 0xFF, "empty channel reads open bus");
    });
}

#[test]
fn primary_channel_identify_runs_through_the_bus() {
    let mut machine = int15_machine(16);
    machine.mount_hdd(vec![0u8; 4032 * 512]);
    with_bus(&mut machine, |bus| {
        // IDENTIFY DEVICE on the command port, then drain word 0 of the block.
        bus.write_io(0x1F7, BusWidth::Byte, 0xEC, false).unwrap();
        let lo = bus.read_io(0x1F0, BusWidth::Byte, 0, false).unwrap();
        let hi = bus.read_io(0x1F0, BusWidth::Byte, 0, false).unwrap();
        let word0 = u16::from(lo as u8) | (u16::from(hi as u8) << 8);
        assert_eq!(word0, 0x0040, "fixed ATA device general config");
    });
}

#[test]
fn booter_inert_stands_down_dos_vectors_but_keeps_the_bios() {
    let mut m = int15_machine(16);

    // The Rust DOS kernel that used to service INT 21h/25h/26h/27h/29h/2Ah/2Eh
    // was retired in SP-3, so those pure-DOS vectors are no longer intercepted
    // in EITHER mode; they always pass straight through to the guest's IVT.
    ack_and_dispatch(&mut m, 0x21);
    assert_eq!(
        m.pending_soft_int, None,
        "INT 21h is never intercepted (the DOS kernel was retired)"
    );

    // The DOS multiplex vector (INT 2Fh) IS intercepted by default. INT 67h is
    // not intercepted at all any more: the TOKAEMM guest driver owns the EMS
    // API (SP-4b M2).
    ack_and_dispatch(&mut m, 0x2f);
    assert_eq!(
        m.pending_soft_int,
        Some(0x2f),
        "INT 2Fh (multiplex) is intercepted by default"
    );
    m.pending_soft_int = None;
    ack_and_dispatch(&mut m, 0x67);
    assert_eq!(
        m.pending_soft_int, None,
        "INT 67h (EMS) is never intercepted (the guest driver owns it)"
    );

    // Booter-inert mode stands the multiplex vector down so the guest's own
    // handlers run through the IVT.
    m.set_booter_inert(true);
    assert!(m.booter_inert());
    ack_and_dispatch(&mut m, 0x21);
    assert_eq!(
        m.pending_soft_int, None,
        "INT 21h is still not intercepted in booter mode"
    );
    ack_and_dispatch(&mut m, 0x2f);
    assert_eq!(
        m.pending_soft_int, None,
        "INT 2Fh (multiplex) stands down too"
    );

    // The BIOS hardware services stay intercepted even in booter mode.
    ack_and_dispatch(&mut m, 0x10);
    assert_eq!(
        m.pending_soft_int,
        Some(0x10),
        "INT 10h (BIOS video) stays intercepted"
    );
    m.pending_soft_int = None;
    ack_and_dispatch(&mut m, 0x13);
    assert_eq!(
        m.pending_soft_int,
        Some(0x13),
        "INT 13h (BIOS disk) stays intercepted"
    );
    m.pending_soft_int = None;
    ack_and_dispatch(&mut m, 0x40);
    assert_eq!(
        m.pending_soft_int,
        Some(0x40),
        "INT 40h (relocated floppy) stays intercepted"
    );
    m.pending_soft_int = None;
    ack_and_dispatch(&mut m, 0x42);
    assert_eq!(
        m.pending_soft_int,
        Some(0x42),
        "INT 42h (relocated video) stays intercepted"
    );

    // A vector the HLE never intercepts is recorded in neither mode.
    m.pending_soft_int = None;
    ack_and_dispatch(&mut m, 0x80);
    assert_eq!(
        m.pending_soft_int, None,
        "an un-intercepted vector is ignored"
    );
}

#[test]
fn int2f_stands_down_when_a_guest_dpmi_host_hooks_the_vector() {
    let mut m = int15_machine(16);

    // Default boot: IVT[0x2F] is still the ROM IRET stub, so the multiplex
    // HLE intercepts as always.
    ack_and_dispatch(&mut m, 0x2f);
    assert_eq!(
        m.pending_soft_int,
        Some(0x2f),
        "default boot: INT 2Fh is intercepted (no guest hook present)"
    );
    m.pending_soft_int = None;

    // Simulate a guest DPMI host (e.g. JEMMEX) hooking IVT[0x2F] to point at
    // its own handler in guest RAM instead of the ROM IRET stub.
    {
        let bus = m.make_bus();
        bus.memory.write_u16(0x2f * 4, 0x128e).unwrap();
        bus.memory.write_u16(0x2f * 4 + 2, 0x00d8).unwrap();
    }
    ack_and_dispatch(&mut m, 0x2f);
    assert_eq!(
        m.pending_soft_int, None,
        "guest-hooked INT 2Fh: the HLE stands down so the guest's own handler runs \
             (this is what lets a real DPMI host answer AX=1686h/1687h instead of the \
             HLE's stale \"no host\" answer)"
    );
}

#[test]
fn program_runtime_reintercepts_dos_vectors_for_the_raw_program_loader() {
    // The raw-program runtime (new_raw_program) still services INT 20h/21h/27h
    // itself (terminate + minimal console I/O), so interrupt_acknowledge must
    // record those vectors when program_runtime is set — even though the
    // retired HLE no longer intercepts them for a normal boot. This pins the
    // exact branch the SP-3 seam deletion added to interrupt_acknowledge.
    let prog: &[u8] = &[0xcd, 0x20]; // int 20h
    let mut raw =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), prog).unwrap();
    for vector in [0x20u8, 0x21, 0x27] {
        raw.pending_soft_int = None;
        ack_and_dispatch(&mut raw, vector);
        assert_eq!(
            raw.pending_soft_int,
            Some(vector),
            "INT {vector:02X}h is intercepted for the raw-program runtime"
        );
    }

    // A normal (non-program-runtime) machine passes them straight through.
    let mut boot = int15_machine(16);
    ack_and_dispatch(&mut boot, 0x21);
    assert_eq!(
        boot.pending_soft_int, None,
        "INT 21h passes through for a normal boot (no raw-program runtime)"
    );
}

#[test]
fn absent_resident_api_vectors_intercept_only_default_iret() {
    let mut m = int15_machine(16);

    for vector in [0x5C, 0x60, 0x68, 0x6F, 0x7A, 0x86, 0xE4] {
        ack_and_dispatch(&mut m, vector);
        assert_eq!(m.pending_soft_int, Some(vector), "INT {vector:02X}h");
        m.pending_soft_int = None;
    }

    m.memory.write_u16(0x60 * 4, 0x1234).unwrap();
    m.memory.write_u16(0x60 * 4 + 2, 0x5678).unwrap();
    ack_and_dispatch(&mut m, 0x60);

    assert_eq!(
        m.pending_soft_int, None,
        "guest-owned INT 60h is not stolen"
    );
}

#[test]
fn absent_resident_api_vectors_report_not_installed() {
    let mut m = int15_machine(16);

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x3000));
    m.cpu.registers.set_ebx(0x0020);
    m.write_physical_u8(0x30020 + 1, 0);
    m.handle_absent_resident_api(0x5C);
    assert_eq!(m.cpu.registers.eax() as u8, 0xFB);
    assert_eq!(m.read_physical_u8(0x30020 + 1), 0xFB);

    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0xCAFE_01FF);
    m.handle_absent_resident_api(0x60);
    assert_eq!(m.cpu.registers.eax(), 0xCAFE_01FF);
    assert_eq!(dos_int_flags(&m) & 1, 0, "driver-info clears CF");

    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0xCAFE_0400);
    m.cpu.registers.set_edx(0x1111_2222);
    m.handle_absent_resident_api(0x60);
    assert_eq!((m.cpu.registers.edx() >> 8) as u8, 0x0B);
    assert_ne!(dos_int_flags(&m) & 1, 0, "packet send sets CF");

    m.cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x4000));
    m.cpu.registers.set_edx(0x0100);
    m.cpu.registers.set_eax(0x0500);
    m.write_guest_block(0x40100, &[0; 0x18]);
    m.handle_absent_resident_api(0x68);
    assert_eq!(&m.read_guest_block(0x40114, 4), &[0xF0, 0x01, 0x00, 0x00]);

    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x0200);
    m.handle_absent_resident_api(0x6F);
    assert_eq!(m.cpu.registers.eax() as u16, 0x08FF);
    assert_ne!(dos_int_flags(&m) & 1, 0, "10NET node status sets CF");

    m.cpu.registers.set_eax(0x0001);
    m.cpu.registers.set_ebx(0x1111_2222);
    m.cpu.registers.set_ecx(0x3333_4444);
    m.cpu.registers.set_edx(0x5555_6666);
    m.handle_absent_resident_api(0x7A);
    assert_eq!(m.cpu.registers.eax() as u16, 0);
    assert_eq!(m.cpu.registers.ebx() as u16, 0);
    assert_eq!(m.cpu.registers.ecx() as u16, 0);
    assert_eq!(m.cpu.registers.edx() as u16, 0);

    m.cpu.registers.set_eax(0);
    m.cpu.registers.set_ebx(0x0010);
    m.handle_absent_resident_api(0x7A);
    assert_eq!(m.cpu.registers.eax() as u8, 0xF0);
}

#[test]
fn int19_floppy_boot_marks_the_machine_booter_inert() {
    // Booting any floppy hands the machine to the disk's own sector-0 code,
    // so the HLE Toka-DOS stands down the way it would on real hardware:
    // whatever is in the boot sector is the OS now, not the HLE.
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 1_474_560]).unwrap(); // 1.44 MB, readable sector 0
    assert!(!m.booter_inert(), "booter-inert defaults off");
    m.handle_int19();
    assert!(
        m.booter_inert(),
        "a floppy boot stands the HLE down so the disk owns the DOS interrupts"
    );
    assert_eq!(
        m.cpu.registers.edx() as u8,
        0x00,
        "DL=00h: the floppy branch ran"
    );
}

#[test]
fn int19_boots_from_ata_when_no_floppy() {
    // Booting from a fixed disk (ATA primary master) hands the machine to the
    // disk's own sector-0 code, so the HLE Toka-DOS stands down exactly the
    // same way the floppy path does. DL=0x80 signals the first fixed disk.
    let mut m = int15_machine(16);
    // Build a minimal 4-sector image with the 0x55AA boot signature.
    let mut img = vec![0u8; 512 * 4];
    img[0] = 0xEB; // recognisable first byte
    img[510] = 0x55;
    img[511] = 0xAA;
    m.mount_hdd(img);
    assert!(!m.booter_inert(), "booter-inert defaults off");
    m.handle_int19();
    assert!(
        m.booter_inert(),
        "an ATA boot stands the HLE down so the disk owns the DOS interrupts"
    );
    assert_eq!(
        m.cpu.registers.edx() as u8,
        0x80,
        "DL=80h: the ATA fixed-disk branch ran"
    );
    assert_eq!(
        m.read_physical_u8(BOOT_SECTOR_ADDRESS as u32),
        0xEB,
        "sector 0 byte 0 must land at 0x7C00"
    );
}

#[test]
fn int19_skips_ata_without_boot_signature() {
    // An ATA disk whose LBA 0 lacks the 0x55AA signature is not bootable: the
    // ATA branch must fall through (to the C: HLE / int18 path) without copying
    // sector 0 or standing the HLE down. Tasks 3-5 rely on this fall-through.
    let mut m = int15_machine(16);
    let mut img = vec![0u8; 512 * 4];
    img[0] = 0xEB; // sentinel first byte, but NO 0x55AA signature
    m.mount_hdd(img);
    m.handle_int19();
    assert!(
        !m.booter_inert(),
        "an unsigned ATA disk must not stand the HLE down"
    );
    assert_ne!(
        m.read_physical_u8(BOOT_SECTOR_ADDRESS as u32),
        0xEB,
        "an unsigned ATA disk's sector 0 must not be copied to 0x7C00"
    );
}

#[test]
fn floppy_booted_machine_stands_dos_down_at_interrupt_ack() {
    // The end-to-end guarantee: after a floppy boot the next INT 21h must
    // stand down so the disk's own handler runs, not the HLE. This catches a
    // stale booter-inert snapshot in the per-interrupt bus.
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 1_474_560]).unwrap();
    m.handle_int19();
    ack_and_dispatch(&mut m, 0x21);
    assert_eq!(
        m.pending_soft_int, None,
        "INT 21h stands down once a floppy has booted"
    );
}

#[test]
fn int11_returns_equipment_word() {
    // Stub: INT 11h then halt. AX must hold the seeded BDA equipment word.
    // The BIOS service vectors return through the ROM IRET at offset 0xF000
    // that rom_with_code supplies, matching the real izarra BIOS.
    let rom = rom_with_code(&[
        0xCD, 0x11, // int 11h
        0xF4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let ax = machine.cpu().registers.eax() as u16;
    assert_eq!(ax, BIOS_EQUIPMENT_WORD);
    // Bits 11-9 = 010b: two serial ports advertised (COM1 + COM2).
    assert_eq!((ax >> 9) & 0x07, 2, "two serial ports advertised");
    // Bits 15-14 = 10b: two parallel printer ports advertised (LPT1 + LPT2).
    assert_eq!((ax >> 14) & 0x03, 2, "two parallel ports advertised");
    // Bit 1 (80x87 coprocessor) stays clear: the Izarra 3000 has no FPU.
    assert_eq!(ax & 0x0002, 0, "no coprocessor advertised");
}

#[test]
fn int12_returns_conventional_memory_kib() {
    // Stub: INT 12h then halt. AX must hold the conventional memory size. The
    // 1 KB EBDA reserved at POST drops the reported size from 640 to 639 KB.
    let rom = rom_with_code(&[
        0xCD, 0x12, // int 12h
        0xF4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let ax = machine.cpu().registers.eax() as u16;
    assert_eq!(ax, BIOS_BASE_MEMORY_KIB - 1);
    assert_eq!(ax, 639);
}

#[test]
fn int1a_ah00_reads_bda_tick() {
    // Seed the BDA tick to 0x00012345, then INT 1Ah AH=00h returns CX:DX.
    let rom = rom_with_code(&[
        0xB4, 0x00, // mov ah, 0
        0xCD, 0x1A, // int 1Ah
        0xF4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.write_physical_u8(0x46c, 0x45);
    machine.write_physical_u8(0x46d, 0x23);
    machine.write_physical_u8(0x46e, 0x01);
    machine.write_physical_u8(0x46f, 0x00);
    machine.write_physical_u8(0x470, 0x00); // no rollover
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let cx = machine.cpu().registers.ecx() as u16;
    let dx = machine.cpu().registers.edx() as u16;
    assert_eq!(cx, 0x0001, "CX = high word of tick");
    assert_eq!(dx, 0x2345, "DX = low word of tick");
    assert_eq!(
        machine.cpu().registers.eax() as u8,
        0x00,
        "AL = rollover count"
    );
}

#[test]
fn int1a_ah02_ah04_return_bcd_clock() {
    // AH=04h clobbers CX/DX, so the AH=02h time result must be stashed to
    // memory before the date call overwrites it. Set DS=0, run AH=02h, store
    // CX/DX into BIOS scratch at 0:0500h, then run AH=04h and HLT. The date
    // result stays live in CX/DX; the time result is read back from scratch.
    let rom = rom_with_code(&[
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xD8, // mov ds, ax (DS = 0)
        0xB4, 0x02, 0xCD, 0x1A, // int 1Ah AH=02h (time)
        0x89, 0x0E, 0x00, 0x05, // mov [0500h], cx
        0x89, 0x16, 0x02, 0x05, // mov [0502h], dx
        0xB4, 0x04, 0xCD, 0x1A, // int 1Ah AH=04h (date)
        0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.seed_rtc(2026, 6, 21, 1, 13, 45, 30); // helper forwards to rtc.seed
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // After AH=04h: CH=century 0x20, CL=year 0x26, DH=month 0x06, DL=day 0x21.
    let cx = machine.cpu().registers.ecx() as u16;
    let dx = machine.cpu().registers.edx() as u16;
    assert_eq!(cx, 0x2026);
    assert_eq!(dx, 0x0621);
    // AH=02h stashed time: CH=hour 0x13, CL=minute 0x45, DH=second 0x30, DL=0.
    let time_cx = u16::from(machine.read_physical_u8(0x0500))
        | (u16::from(machine.read_physical_u8(0x0501)) << 8);
    let time_dx = u16::from(machine.read_physical_u8(0x0502))
        | (u16::from(machine.read_physical_u8(0x0503)) << 8);
    assert_eq!(time_cx, 0x1345, "CH=hour BCD, CL=minute BCD");
    assert_eq!(time_dx, 0x3000, "DH=second BCD, DL=0");
}

#[test]
fn int15_ah87_block_move_across_1mb() {
    // Build a GDT in low RAM with source = 0x20000, dest = 0x30000, move 4 words.
    let rom = rom_with_code(&[
        0xB4, 0x87, // mov ah,87h
        0xB9, 0x04, 0x00, // mov cx,4 (words)
        0xBE, 0x00, 0x10, // mov si,1000h (GDT offset)
        0xCD, 0x15, 0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    // ES = 0 so the GDT sits at linear 0x1000. Descriptors at +0x10 (src), +0x18 (dst).
    let gdt = 0x1000u32;
    let write_desc = |m: &mut Machine, at: u32, base: u32| {
        m.write_physical_u8(at, 0xFF); // limit low
        m.write_physical_u8(at + 1, 0xFF);
        m.write_physical_u8(at + 2, base as u8); // base 0..7
        m.write_physical_u8(at + 3, (base >> 8) as u8); // base 8..15
        m.write_physical_u8(at + 4, (base >> 16) as u8); // base 16..23
        m.write_physical_u8(at + 5, 0x93); // access
        m.write_physical_u8(at + 6, 0x00);
        m.write_physical_u8(at + 7, (base >> 24) as u8); // base 24..31
    };
    write_desc(&mut machine, gdt + 0x10, 0x20000);
    write_desc(&mut machine, gdt + 0x18, 0x30000);
    for i in 0..8u32 {
        machine.write_physical_u8(0x20000 + i, 0xA0 + i as u8);
    }
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    for i in 0..8u32 {
        assert_eq!(machine.read_physical_u8(0x30000 + i), 0xA0 + i as u8);
    }
    assert_eq!(
        (machine.cpu().registers.eax() as u16 >> 8) as u8,
        0x00,
        "AH=0 success"
    );
}

#[test]
fn int15_ah86_wait_advances_guest_clock() {
    let rom = rom_with_code(&[
        0xB4, 0x86, 0xB9, 0x00, 0x00, // CX=0
        0xBA, 0x40, 0x42, // DX=0x4240 -> with CX=0 that is 16960 us
        0xCD, 0x15, 0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    let before = machine.elapsed_clocks();
    let reason = machine.run_until_halt_or_cycles(10_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // CX:DX = 0x00004240 = 16960 microseconds. stall_for converts that to guest
    // clocks at the active mode's rate, so the elapsed-clock jump must dwarf the
    // handful of setup-instruction clocks. Require at least half the expected
    // stall to leave margin for the rounding in stall_for.
    let wait_secs = 16_960.0 / 1_000_000.0;
    let expected_stall = (wait_secs * machine.active_mode().clock_hz() as f64) as u64;
    let advanced = machine.elapsed_clocks() - before;
    assert!(
        advanced >= expected_stall / 2,
        "AH=86h stall too small: advanced {advanced} clocks, expected ~{expected_stall}"
    );
    let flags = machine.cpu().registers.eflags;
    assert_eq!(flags & 0x0001, 0, "CF clear after WAIT");
}

#[test]
fn device_fill_never_moves_the_master_clock() {
    // The GUI's Approximate-class stall fill relies on this: stall_for already
    // advanced elapsed_clocks by the stall, so the device catch-up must not
    // advance it again or the audio pump gains a cumulative lead over wall time.
    let mut machine = test_machine();
    let before = machine.elapsed_clocks();
    machine.advance_devices_clocks(1000);
    assert_eq!(
        machine.elapsed_clocks(),
        before,
        "advance_devices_clocks must advance device time only, never the master clock"
    );
}

#[test]
fn wall_shortfall_advances_devices_and_master_clock_together() {
    // The GUI's Approximate-class wall-clock top-up relies on this: when the
    // host could not execute the full budget, the unrun remainder must move
    // BOTH device time and the master clock, so the audio pump (which paces
    // off elapsed_clocks deltas) keeps tracking wall time. Contrast with
    // device_fill_never_moves_the_master_clock above: that path fills a gap
    // the master clock already jumped over; this one creates the time.
    let mut machine = test_machine();
    fn latched_count(m: &mut Machine) -> u16 {
        let mut bus = m.make_bus();
        bus.write_io(0x43, BusWidth::Byte, 0x00, false).unwrap(); // latch counter 0
        let lo = bus.read_io(0x40, BusWidth::Byte, 0, false).unwrap() as u16;
        let hi = bus.read_io(0x40, BusWidth::Byte, 0, false).unwrap() as u16;
        lo | (hi << 8)
    }
    {
        // Program PIT counter 0 (mode 3, reload 0 = 65536) so it counts; the
        // test ROM machine never ran the POST timer setup.
        let mut bus = machine.make_bus();
        bus.write_io(0x43, BusWidth::Byte, 0x36, false).unwrap();
        bus.write_io(0x40, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x40, BusWidth::Byte, 0x00, false).unwrap();
    }
    let before = machine.elapsed_clocks();
    let pit_before = latched_count(&mut machine);
    // 100_000 clocks at the 386 mode rate is ~4.5ms, thousands of PIT ticks,
    // and well short of the first vretrace start edge (the boot text-mode
    // beam sits at dot 0; the edge is ~289k clocks away), so the span has no
    // edge and must be consumed in full.
    let consumed = machine.advance_wall_shortfall(100_000);
    assert_eq!(
        consumed, 100_000,
        "a span with no intervening vretrace edge is consumed in full"
    );
    assert_eq!(
        machine.elapsed_clocks(),
        before + 100_000,
        "advance_wall_shortfall must advance the master clock by exactly the consumed clocks"
    );
    assert_ne!(
        latched_count(&mut machine),
        pit_before,
        "advance_wall_shortfall must advance device time (PIT counter 0 moved)"
    );
}

#[test]
fn wall_shortfall_stops_at_a_vretrace_start_edge_and_then_makes_progress() {
    // The P4d clamp: a top-up spanning a vretrace start edge must stop AT the
    // edge (vretrace bit 3 already readable) and report the shorter consume,
    // so the GUI can grant a polling guest an execution quantum there instead
    // of sweeping the whole window past it unobserved.
    let mut machine = test_machine();
    let clock_hz = machine.active_mode().clock_hz();
    let before = machine.elapsed_clocks();
    // A full guest second: dozens of frames, so an edge is guaranteed inside.
    let consumed = machine.advance_wall_shortfall(clock_hz);
    assert!(
        consumed < clock_hz,
        "a span crossing a vretrace start edge must stop early (consumed {consumed})"
    );
    assert!(consumed > 0, "the stop must still make progress");
    assert_eq!(
        machine.elapsed_clocks(),
        before + consumed,
        "the master clock advances by exactly the consumed clocks"
    );
    assert_ne!(
        machine.video_mut().read_status1() & 0x08,
        0,
        "the beam must land inside the vretrace window (bit 3 set at the stop)"
    );

    // Termination pin: with the beam ON the edge (inside the window), the
    // next call must not return 0. A short span still inside the window has
    // no NEXT start edge within it (that edge is a full frame ahead), so it
    // is consumed in full.
    let consumed_inside = machine.advance_wall_shortfall(10);
    assert_eq!(
        consumed_inside, 10,
        "on-edge/inside-window spans consume fully instead of stalling"
    );

    // And a long span from inside the window stops at the NEXT frame's edge,
    // roughly one frame period away, never zero.
    let consumed_next = machine.advance_wall_shortfall(clock_hz);
    assert!(consumed_next > 0 && consumed_next < clock_hz);
    assert_ne!(
        machine.video_mut().read_status1() & 0x08,
        0,
        "each stop lands inside the vretrace window"
    );
}

#[test]
fn paced_wall_topup_lets_a_polling_guest_catch_vretrace_windows() {
    // Permanent port of the P4d investigation repro. A mode-13h guest
    // double-polling port 0x3DA (wait for vretrace to clear, then wait for
    // it to set) is driven with the GUI's Approximate-class pacing pattern
    // at a 1/8 execution share: run 1/8 of each ~1ms quantum, then top the
    // remainder up wall-style. Unfixed (single unclamped top-up per
    // quantum), the guest caught 12.8-18.9 percent of the vretrace windows,
    // because a top-up sweeps the whole 2-scanline window past it with zero
    // instructions executing. With the edge clamp + peek it must catch
    // nearly all of them. Window count derives from beam geometry (frames
    // completed; each frame crosses exactly one vretrace start edge), so
    // the test is host-speed-independent and deterministic.
    let code = [
        0xB8, 0x13, 0x00, // mov ax, 0x0013 (mode 13h)
        0xCD, 0x10, // int 0x10
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x00, 0x70, 0x00, 0x00, // mov word [0x7000], 0 (catch counter)
        0xBA, 0xDA, 0x03, // mov dx, 0x03DA
        // wait_clear (0x12): spin while the vretrace bit is set
        0xEC, // in al, dx
        0xA8, 0x08, // test al, 0x08
        0x75, 0xFB, // jnz wait_clear
        // wait_set (0x17): spin until the vretrace bit sets
        0xEC, // in al, dx
        0xA8, 0x08, // test al, 0x08
        0x74, 0xFB, // jz wait_set
        0xFF, 0x06, 0x00, 0x70, // inc word [0x7000] (window caught)
        0xEB, 0xF0, // jmp wait_clear
    ];
    let mut machine = Machine::new(
        MachineProfile::gsw_386(4, VideoCard::Et4000Ax),
        rom_with_code(&code),
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw486); // Approximate class, 66 MHz
    let clock_hz = machine.active_mode().clock_hz();
    let quantum = clock_hz / 1000; // the GUI's ~1ms sub-slice

    // Warm up at full speed until the guest set mode 13h and is inside the
    // poll loop, then baseline the counters.
    machine.run_cycles(quantum).unwrap();
    let counter = |m: &Machine| m.memory.read_u16(0x7000).unwrap();
    let counter_base = u64::from(counter(&machine));
    let frames_base = machine.video().frames_completed();

    // One guest second of the paced pattern: ~70 mode-13h frames, plenty of
    // statistical power for a 90 percent threshold against a 13-19 percent
    // unfixed baseline, at half the runtime of a two-second run.
    for _ in 0..1000 {
        let before = machine.elapsed_clocks();
        machine.run_cycles(quantum / 8).unwrap();
        let ran = machine.elapsed_clocks().saturating_sub(before);
        let mut remaining = quantum.saturating_sub(ran);
        let mut stops = 0u32;
        while remaining > 0 {
            let consumed = machine.advance_wall_shortfall(remaining);
            assert!(consumed > 0, "termination: every call must make progress");
            remaining = remaining.saturating_sub(consumed);
            if remaining == 0 {
                break;
            }
            // Stopped at a vretrace start edge: grant the peek so the
            // polling guest observes the window, exactly like the GUI.
            stops += 1;
            assert!(
                stops <= 4,
                "termination: at most one edge fits in a 1ms quantum (plus slack)"
            );
            machine.run_cycles(VRETRACE_PEEK_CLOCKS).unwrap();
        }
    }

    let windows_opened = machine.video().frames_completed() - frames_base;
    let caught = u64::from(counter(&machine)) - counter_base;
    assert!(
        windows_opened >= 60,
        "geometry sanity: expected ~70 frames in 1 guest second, saw {windows_opened}"
    );
    assert!(
        caught <= windows_opened + 1,
        "sanity: cannot catch more windows than opened ({caught} vs {windows_opened})"
    );
    assert!(
        caught * 10 >= windows_opened * 9,
        "guest caught {caught} of {windows_opened} vretrace windows (< 90 percent); \
             unfixed baseline was 12.8-18.9 percent"
    );
}

#[test]
fn mouse_movement_requests_irq12_after_enable() {
    // Bring up the PS/2 mouse the way a driver does (command byte bit 1 set
    // for the mouse interrupt, then 0xF4 enable reporting via the 0xD4 path),
    // then inject a host move and confirm IRQ12 is pending on the PIC and the
    // three-byte packet is readable on port 0x60 with the AUX status bit set.
    let profile = MachineProfile::gsw_386(1, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, vec![0u8; BIOS_ROM_SIZE]).unwrap();
    // Drive the controller through the bus the way the CPU would.
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x64, BusWidth::Byte, 0x60, false).unwrap(); // write command byte
        bus.write_io(0x60, BusWidth::Byte, 0x03, false).unwrap(); // IRQ1 + IRQ12 enabled
        bus.write_io(0x64, BusWidth::Byte, 0xD4, false).unwrap(); // next byte to aux
        bus.write_io(0x60, BusWidth::Byte, 0xF4, false).unwrap(); // enable data reporting
        assert_eq!(bus.read_io(0x60, BusWidth::Byte, 0, false).unwrap(), 0xFA); // mouse ACK
    }
    // The ACK read armed the keyboard controller's aux settle window (see
    // AUX_BYTE_SETTLE_US in keyboard.rs); advance past it -- comfortably
    // more than 1ms regardless of the active GSW clock rate -- so the
    // movement packet below latches without an unrelated pacing delay.
    machine.advance_devices_clocks(1_000_000);
    // Move right 4, down 2, left button down.
    machine.inject_mouse(4, 2, 0x01);
    assert!(machine.irq12_pending(), "movement requests IRQ12");
    // The packet is on port 0x60 and the status reports an AUX byte.
    assert_eq!(machine.read_io_port_u8(0x64) & 0x20, 0x20, "AUX status bit");
    let b0 = machine.read_io_port_u8(0x60);
    assert_eq!(b0 & 0x08, 0x08, "always-one bit");
    assert_eq!(b0 & 0x01, 0x01, "left button");
    assert_eq!(b0 & 0x10, 0x00, "X positive");
    assert_eq!(b0 & 0x20, 0x20, "Y sign set (screen-down move)");
    machine.advance_devices_clocks(1_000_000); // pace the next aux byte
    assert_eq!(machine.read_io_port_u8(0x60), 4, "dx byte");
    machine.advance_devices_clocks(1_000_000); // pace the next aux byte
    assert_eq!(machine.read_io_port_u8(0x60) as i8 as i32, -2, "dy byte");
}

#[test]
fn bios_aux_enable_then_packet_reads_back_with_no_stray_keyboard_byte() {
    // Drive the exact sequence the BIOS bootbox menu runs (izbios-bootbox.inc
    // bx2_aux_init): read the controller command byte, set the IRQ1+IRQ12
    // enable bits, then enable AUX reporting via the 0xD4 prefix and drain the
    // mouse ACK. The two things this guards that the menu has no automated
    // coverage for: the injected packet reads back on 0x60 with the AUX status
    // bit set, AND the enable handshake never drops a stray byte into the
    // keyboard scancode ring (which the keyboard ISR reads unconditionally).
    let profile = MachineProfile::gsw_386(1, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, vec![0u8; BIOS_ROM_SIZE]).unwrap();
    {
        let mut bus = machine.make_bus();
        // Read CCB (0x20) -> 0x60, OR in IRQ1 (bit0) + IRQ12 (bit1), write back.
        bus.write_io(0x64, BusWidth::Byte, 0x20, false).unwrap();
        let ccb = bus.read_io(0x60, BusWidth::Byte, 0, false).unwrap() as u8;
        let new_ccb = ccb | 0x01 | 0x02;
        bus.write_io(0x64, BusWidth::Byte, 0x60, false).unwrap();
        bus.write_io(0x60, BusWidth::Byte, new_ccb as u32, false)
            .unwrap();
    }
    // Drain the IRQ1 edge the CCB read above itself arms in
    // respond_immediately (a pre-existing quirk unrelated to AUX enable:
    // it fires for any controller-command response while command-byte
    // bit0 is set, which it is by default), then acknowledge it the way
    // the CPU eventually would so it doesn't linger as a pending PIC
    // request. This keeps the assertion below honestly testing whether
    // the AUX-enable sequence, not this earlier CCB read, arms IRQ1.
    machine.advance_devices_clocks(1_000_000);
    machine.pic.acknowledge();
    {
        let mut bus = machine.make_bus();
        // Enable AUX data reporting: 0xD4 routes 0xF4 to the mouse.
        bus.write_io(0x64, BusWidth::Byte, 0xD4, false).unwrap();
        bus.write_io(0x60, BusWidth::Byte, 0xF4, false).unwrap();
        // Drain the AUX ACK (0xFA): it must arrive flagged as an AUX byte.
        let status = bus.read_io(0x64, BusWidth::Byte, 0, false).unwrap() as u8;
        assert_eq!(status & 0x01, 0x01, "ACK waiting (OBF)");
        assert_eq!(status & 0x20, 0x20, "ACK is an AUX byte, not a key");
        assert_eq!(
            bus.read_io(0x60, BusWidth::Byte, 0, false).unwrap(),
            0xFA,
            "mouse ACK"
        );
    }
    // The AUX-enable sequence itself must not arm IRQ1. The ACK read also
    // armed the keyboard controller's aux settle window (see
    // AUX_BYTE_SETTLE_US in keyboard.rs); advance past it too, 1,000,000
    // clocks being far more than 1ms regardless of the active GSW clock
    // rate.
    machine.advance_devices_clocks(1_000_000);
    assert!(
        !machine.irq1_pending(),
        "AUX enable must not arm the keyboard interrupt"
    );
    assert_eq!(
        machine.read_io_port_u8(0x64) & 0x01,
        0,
        "no byte left in the output buffer after the ACK drain"
    );

    // Now a host move queues a three-byte packet, flagged AUX, with IRQ12.
    machine.inject_mouse(6, -3, 0x01); // right 6, up 3, left button down
    assert!(machine.irq12_pending(), "movement requests IRQ12");
    assert_eq!(
        machine.read_io_port_u8(0x64) & 0x20,
        0x20,
        "packet byte is flagged AUX"
    );
    let b0 = machine.read_io_port_u8(0x60);
    assert_eq!(b0 & 0x08, 0x08, "sync bit");
    assert_eq!(b0 & 0x01, 0x01, "left button");
    machine.advance_devices_clocks(1_000_000); // pace the next aux byte
    assert_eq!(machine.read_io_port_u8(0x60), 6, "dx byte");
    machine.advance_devices_clocks(1_000_000); // pace the next aux byte
    assert_eq!(
        machine.read_io_port_u8(0x60),
        3,
        "dy byte (screen up -> +3)"
    );
    // The packet drained cleanly: nothing left, and still no keyboard IRQ.
    assert_eq!(
        machine.read_io_port_u8(0x64) & 0x01,
        0,
        "output buffer empty after the packet"
    );
    assert!(
        !machine.irq1_pending(),
        "the AUX packet never touched the keyboard interrupt"
    );
}

#[test]
fn c200_enable_arms_irq12_in_the_command_byte_itself() {
    // Without any manual command-byte setup: a C200 enable must arm IRQ12 on
    // its own, the way a real PS/2 BIOS does, so the MOUSE.COM install path
    // (which only issues INT 15h C205/C207/C200) gets working interrupts. The
    // injected packet then raises IRQ12 with no separate command-byte write.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC200);
    m.cpu.registers.set_ebx(0x0100); // BH=1 enable
    m.handle_int15();
    m.inject_mouse(4, -2, 0x01);
    assert!(
        m.irq12_pending(),
        "C200 enable alone arms IRQ12 (no separate command-byte write needed)"
    );
}

#[test]
fn c205_initialize_arms_irq12_in_the_command_byte() {
    // C205 is MOUSE.COM's first BIOS call. Like C200 enable, it must arm IRQ12
    // on its own with no prior command-byte setup, so an injected packet raises
    // the interrupt.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC205);
    m.handle_int15();
    m.inject_mouse(4, -2, 0x01);
    assert!(
        m.irq12_pending(),
        "C205 initialize alone arms IRQ12 (no separate command-byte write needed)"
    );
}

#[test]
fn c200_disable_leaves_no_irq12_pending() {
    // The BIOS-level mirror of the keyboard-level edge-clear test: enabling
    // then disabling the pointing device through C200 leaves a disabled mouse
    // that raises no IRQ12 (C200 disable both turns reporting off and clears
    // the command-byte IRQ12 bit). The keyboard unit test
    // disable_clears_a_pending_irq12_edge covers the already-latched-edge case.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC200);
    m.cpu.registers.set_ebx(0x0100); // BH=1 enable
    m.handle_int15();
    m.cpu.registers.set_eax(0xC200);
    m.cpu.registers.set_ebx(0x0000); // BH=0 disable
    m.handle_int15();
    m.inject_mouse(4, -2, 0x01);
    assert!(
        !m.irq12_pending(),
        "a disabled pointing device raises no IRQ12"
    );
}

#[test]
fn bios_irq12_preserves_interrupted_cx_dx() {
    // IRQ12 can interrupt any game code, even when the game never calls INT 33h.
    // The BIOS mouse ISR's dispatch helper uses CX/DX while assembling a packet,
    // so the outer ISR has to save them before IRET returns to the interrupted
    // instruction stream.
    const PROGRAM: &[u8] = &[
        0xb9, 0x34, 0x12, // mov cx,1234h
        0xba, 0x78, 0x56, // mov dx,5678h
        0xfb, // sti
        0xbb, 0xff, 0xff, // mov bx,ffffh
        0x4b, // dec bx
        0x75, 0xfd, // jnz $-3
        0x89, 0x0e, 0x00, 0x70, // mov [7000h],cx
        0x89, 0x16, 0x02, 0x70, // mov [7002h],dx
        0xfa, // cli
        0xf4, // hlt
    ];

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    let _ = machine.run_until_halt_or_cycles(20_000_000).unwrap();
    for (offset, byte) in PROGRAM.iter().copied().enumerate() {
        machine.write_physical_u8(0x8000 + offset as u32, byte);
    }

    machine.register_mouse_handler_for_test(0, 0); // null handler still exercises dispatch
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x21, BusWidth::Byte, 0xfb, false).unwrap(); // master: IRQ2 only
        bus.write_io(0xa1, BusWidth::Byte, 0xef, false).unwrap(); // slave: IRQ12 only
    }

    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::real(0));
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0));
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Ss, SegmentRegister::real(0));
    machine.cpu.registers.eip = 0x8000;
    machine.cpu.registers.eflags = 0x0002;
    machine.cpu.registers.set_esp(0x9000);

    machine.inject_mouse(7, 0, 0);
    let reason = machine.run_until_halt_or_cycles(10_000_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.read_physical_u16(0x7000), 0x1234, "CX survived");
    assert_eq!(machine.read_physical_u16(0x7002), 0x5678, "DX survived");
}

#[test]
fn set_mouse_absolute_synthesizes_relative_deltas() {
    let mut m = int15_machine(16);
    m.enable_8042_irq12();
    m.cpu.registers.set_eax(0xC205);
    m.handle_int15(); // initialize enables reporting
    m.seed_mouse_origin(100, 100);
    m.set_mouse_absolute(110, 97, 0x00); // +10 / -3 screen delta
    assert!(
        m.irq12_pending(),
        "synthesized motion reaches the aux device"
    );
}

#[test]
fn bios_service_vectors_survive_low_memory_wipe() {
    // A booter that zeroes low RAM (including the 0x600 RAM IRET stub) must not
    // strand INT 11h/12h: their IVT targets point at the ROM IRET, so the
    // service still returns. Stub: zero 0x600, then INT 11h, then halt.
    // rom_with_code supplies the ROM IRET at FF00:0000 that survives the wipe.
    let rom = rom_with_code(&[
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x00, 0x06, 0x00, 0x00, // mov word [0x600], 0
        0xCD, 0x11, // int 11h
        0xF4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, BIOS_EQUIPMENT_WORD);
}

#[test]
fn vbe_set_mode_selects_a_margo_mode() {
    let rom = rom_with_code(&[
        0xb8, 0x02, 0x4f, // mov ax, 4F02h
        0xbb, 0x01, 0x41, // mov bx, 0101h | 4000h (LFB)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

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
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

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
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

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
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);
    assert_eq!(machine.cpu().registers.ebx() as u16, 0x0101);
}

#[test]
fn passive_target_ports_allow_capability_probes_to_fail_cleanly() {
    // 0x226 is the SB DSP reset port: still an unimplemented passive port
    // (0x224/0x225 are now the CT1745 mixer, 0x388 the OPL chip).
    let mut machine = test_machine();
    let value = with_bus(&mut machine, |bus| {
        bus.read_io(0x0226, BusWidth::Byte, 0, false).unwrap()
    });

    assert_eq!(value, 0xff);
    assert!(
        machine
            .bus_trace()
            .cycles()
            .iter()
            .any(|cycle| cycle.kind == BusAccessKind::IoRead && cycle.address == 0x0226)
    );
}

#[test]
fn mixer_index_port_decodes_instead_of_falling_through_passive() {
    // 0x224 used to read 0xFF as a passive port; it is now the CT1745 mixer
    // index register, whose read returns the latched index (0 at reset).
    let mut machine = test_machine();
    let index_read = with_bus(&mut machine, |bus| {
        bus.read_io(0x0224, BusWidth::Byte, 0, false).unwrap()
    });
    assert_eq!(index_read, 0x00, "0x224 returns the latched mixer index");
    // Programming register 0x80 (IRQ7) round-trips through 0x225.
    with_bus(&mut machine, |bus| {
        bus.write_io(0x224, BusWidth::Byte, 0x80, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x04, false).unwrap();
    });
    let routed = with_bus(&mut machine, |bus| {
        bus.write_io(0x224, BusWidth::Byte, 0x80, false).unwrap();
        bus.read_io(0x225, BusWidth::Byte, 0, false).unwrap()
    });
    assert_eq!(routed, 0x04, "IRQ7 latched in mixer register 0x80");
}

#[test]
fn dma_channel_one_transfers_from_memory_through_the_bus() {
    let mut machine = test_machine();
    // Seed memory at physical 0x01_0010 (page 0x01, offset 0x0010).
    machine.write_physical_u8(0x0001_0010, 0x77);
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap(); // mode ch1: single, read
        bus.write_io(0x02, BusWidth::Byte, 0x10, false).unwrap(); // address LSB
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap(); // address MSB -> 0x0010
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap(); // count LSB
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap(); // count MSB -> 0 (1 transfer)
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap(); // page -> 0x01_0010
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap(); // unmask channel 1
    });
    let byte = machine.dma_read_byte(1).expect("a byte from channel 1");
    assert_eq!(byte, 0x77);
}

#[test]
fn sb_dsp_reset_handshake_through_the_bus() {
    let mut machine = test_machine();
    // Reset: write 1, then 0 to the DSP reset port 0x226.
    with_bus(&mut machine, |bus| {
        bus.write_io(0x226, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x226, BusWidth::Byte, 0x00, false).unwrap();
    });
    // Advance emulated time past the ~100us DSP settle window.
    machine.advance_dsp_micros(200);
    let status = with_bus(&mut machine, |bus| {
        u8::try_from(bus.read_io(0x22E, BusWidth::Byte, 0, false).unwrap()).unwrap()
    });
    assert_eq!(status & 0x80, 0x80, "data available after reset");
    let ack = with_bus(&mut machine, |bus| {
        u8::try_from(bus.read_io(0x22A, BusWidth::Byte, 0, false).unwrap()).unwrap()
    });
    assert_eq!(ack, 0xAA);
}

#[test]
fn sb_dsp_status_read_sets_io_touched_in_every_cpu_mode() {
    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        for port in [0x22Eu16, 0x22F] {
            with_bus(&mut machine, |bus| {
                *bus.io_touched = false;
                let _ = bus.read_io(port, BusWidth::Byte, 0, false).unwrap();
                assert!(
                    *bus.io_touched,
                    "{mode:?}: DSP status port {port:#06X} must end the batch"
                );
            });
        }
    }
}

#[test]
fn sb_dsp_version_and_status_route_through_the_bus() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x22C, BusWidth::Byte, 0xE1, false).unwrap(); // read version
    });
    let hi = with_bus(&mut machine, |bus| {
        u8::try_from(bus.read_io(0x22A, BusWidth::Byte, 0, false).unwrap()).unwrap()
    });
    let lo = with_bus(&mut machine, |bus| {
        u8::try_from(bus.read_io(0x22A, BusWidth::Byte, 0, false).unwrap()).unwrap()
    });
    assert_eq!([hi, lo], [4, 5]);
}

#[test]
fn sb_dsp_write_status_does_not_read_as_open_bus() {
    let mut machine = test_machine();
    let status = with_bus(&mut machine, |bus| {
        u8::try_from(bus.read_io(0x22C, BusWidth::Byte, 0, false).unwrap()).unwrap()
    });
    assert_eq!(
        status & 0x80,
        0x00,
        "DSP write-status bit 7 clear means ready for commands"
    );
}

#[test]
fn sb_dma_irq5_fires_from_the_cpu_clock_without_host_audio_pull() {
    let mut machine = test_machine();
    // 8-bit ramp at 0x01_0000; arm DMA ch1 + DSP exactly like the playback golden.
    for (i, b) in (0..16u8).map(|i| i * 16).enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        for &b in &[0x41u8, 0x2B, 0x11, 0x14, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    let before = with_bus(&mut machine, |bus| bus.interrupt_pending());
    assert!(!before, "no IRQ pending before time advances");
    // Advance CPU time for well over the 16-sample block (single-cycle -> end IRQ).
    machine.advance_devices_clocks(200_000);
    let after = with_bus(&mut machine, |bus| bus.interrupt_pending());
    assert!(
        after,
        "IRQ5 must be raised by the per-clock sample advance, not the host render path"
    );
}

#[test]
fn sb16_8bit_dma_command_c0_plays_and_raises_irq5() {
    let mut machine = test_machine();
    for (i, b) in (0..16u8).map(|i| i * 16).enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        // SB16 8-bit single-cycle output: command 0xC0, unsigned mono,
        // count 15 -> 16 DMA bytes. Doom-style SB16 drivers may use this
        // command family after detecting a DSP 4.x card.
        for &b in &[0x41u8, 0x2B, 0x11, 0xC0, 0x00, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    machine.advance_devices_clocks(200_000);
    let after = with_bus(&mut machine, |bus| bus.interrupt_pending());
    assert!(after, "SB16 0xC0 DMA playback must raise IRQ5");
    let out = machine.render_dsp_audio(16);
    assert_eq!(out.len(), 16, "SB16 0xC0 playback drained DMA bytes");
}

#[test]
fn sb_mixer_selects_irq7_and_routes_the_dma_irq() {
    let mut machine = test_machine();
    // 8-bit ramp at 0x01_0000 (DMA ch1, the mixer's default 8-bit channel).
    for (i, b) in (0..16u8).map(|i| i * 16).enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        // Route the DSP IRQ on IRQ7 (mixer register 0x80 = 0x04).
        bus.write_io(0x224, BusWidth::Byte, 0x80, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x04, false).unwrap();
        // PIC base 0x08 so IRQ7 -> vector 0x0F; mask everything except IR7.
        bus.write_io(0x20, BusWidth::Byte, 0x11, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x08, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x7F, false).unwrap();
        // DMA ch1 + DSP 8-bit single-cycle, exactly like the IRQ5 golden.
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        for &b in &[0x41u8, 0x2B, 0x11, 0x14, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    machine.advance_devices_clocks(200_000);
    let vector = with_bus(&mut machine, |bus| bus.acknowledge_interrupt());
    assert_eq!(vector, Some(0x0F), "the DMA IRQ must land on line 7, not 5");
}

#[test]
fn sb_mixer_selects_dma_channel_3() {
    let mut machine = test_machine();
    let bytes: Vec<u8> = (0..16).map(|i| (i * 16) as u8).collect();
    for (i, &b) in bytes.iter().enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        // Route the 8-bit DMA through DMA3 (mixer register 0x81 = 0x08).
        bus.write_io(0x224, BusWidth::Byte, 0x81, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x08, false).unwrap();
        // DMA ch3: page 0x82, byte addr 0, count 15 (16 bytes), single read.
        bus.write_io(0x0B, BusWidth::Byte, 0x4B, false).unwrap();
        bus.write_io(0x06, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x06, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x07, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x07, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x82, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x03, false).unwrap();
        // DSP: 11025 Hz, block 16, single-cycle 8-bit DMA output.
        for &b in &[0x41u8, 0x2B, 0x11, 0x14, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    let out = {
        machine.advance_devices_clocks(200_000);
        machine.render_dsp_audio(16)
    };
    assert_eq!(out.len(), 16, "buffer drained via DMA channel 3");
    assert!(out.iter().any(|&(l, _)| l < 0), "expected negative samples");
    assert!(
        out.iter().all(|&(l, r)| l == r),
        "8-bit mono duplicated L/R"
    );
    // Single mode masks channel 3 at terminal count, proving the producer
    // drew from channel 3 (channel 1 stayed masked and untouched).
    assert_eq!(machine.dma_read_byte(3), None, "ch3 reached TC");
}

#[test]
fn sb_mixer_reset_restores_irq5_default() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        // Route the IRQ on IRQ7, then reset the mixer (any value to 0x00).
        bus.write_io(0x224, BusWidth::Byte, 0x80, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x224, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x01, false).unwrap();
        // A guest reset restores the hardware IRQ5 default, not the host config.
        bus.write_io(0x224, BusWidth::Byte, 0x80, false).unwrap();
        let byte = bus.read_io(0x225, BusWidth::Byte, 0, false).unwrap();
        assert_eq!(byte, 0x02);
    });
}

#[test]
fn machine_applies_host_sound_blaster_config_at_boot() {
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    profile.sound_blaster = SoundBlasterConfig {
        enabled: true,
        irq: SbIrq::I7,
        dma: SbDma8::D3,
        high_dma: SbDma16::D6,
    };
    let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();
    // The mixer boots on the configured routing, not the hardware IRQ5/DMA1/DMA5.
    assert_eq!(machine.sb_selected_irq(), 7);
    let (irq_byte, dma_byte) = with_bus(&mut machine, |bus| {
        bus.write_io(0x224, BusWidth::Byte, 0x80, false).unwrap();
        let irq = u8::try_from(bus.read_io(0x225, BusWidth::Byte, 0, false).unwrap()).unwrap();
        bus.write_io(0x224, BusWidth::Byte, 0x81, false).unwrap();
        let dma = u8::try_from(bus.read_io(0x225, BusWidth::Byte, 0, false).unwrap()).unwrap();
        (irq, dma)
    });
    assert_eq!(irq_byte, 0x04, "register 0x80 boots on IRQ7");
    assert_eq!(dma_byte, 0x48, "register 0x81 boots on DMA3 | DMA6");
}

#[test]
fn sb_8bit_dma_plays_a_buffer_through_the_dsp() {
    let mut machine = test_machine();
    // A 16-byte unsigned ramp in conventional memory at 0x01_0000.
    let bytes: Vec<u8> = (0..16).map(|i| (i * 16) as u8).collect();
    for (i, &b) in bytes.iter().enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        // DMA ch1: address 0x0000, page 0x01, count 15, single read.
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap(); // mode ch1
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap(); // page -> 0x01_0000
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap(); // unmask ch1
        // DSP: 11025 Hz, block 16, single 8-bit DMA output.
        for &b in &[0x41u8, 0x2B, 0x11, 0x14, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    let out = {
        // Playback is now clock-driven: advance CPU time for well over the
        // 16-sample block (single-cycle -> end IRQ), then drain the ring.
        machine.advance_devices_clocks(200_000);
        machine.render_dsp_audio(16)
    };
    assert_eq!(out.len(), 16);
    // Unsigned 0x00 maps to a centered negative sample; mono is duplicated L/R.
    assert!(out.iter().any(|&(l, _)| l < 0), "expected negative samples");
    assert!(
        out.iter().all(|&(l, r)| l == r),
        "8-bit mono duplicated L/R"
    );
    // Single mode masks channel 1 at terminal count.
    assert_eq!(machine.dma_read_byte(1), None);
}

#[test]
fn sb_pro_8bit_stereo_deinterleaves_two_bytes_per_frame_at_the_halved_rate() {
    let mut machine = test_machine();
    // A 16-byte unsigned interleaved L/R pattern in conventional memory:
    // bytes 0,16,32,... so each frame's left byte differs from its right.
    let bytes: Vec<u8> = (0..16).map(|i| (i * 16) as u8).collect();
    for (i, &b) in bytes.iter().enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        // DMA ch1: address 0x0000, page 0x01, count 15, single read.
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap(); // mode ch1
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap(); // page -> 0x01_0000
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap(); // unmask ch1
        // Mixer register 0x0E bit1: SB Pro stereo.
        bus.write_io(0x224, BusWidth::Byte, 0x0E, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x02, false).unwrap();
        // Voice volume to unity so the decoded L/R samples survive the mixer.
        bus.write_io(0x224, BusWidth::Byte, 0x32, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x1F, false).unwrap();
        bus.write_io(0x224, BusWidth::Byte, 0x33, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x1F, false).unwrap();
        // DSP: set the interleaved byte rate via the 0x40 TIME CONSTANT
        // (tc 0xD3 -> 1_000_000/45 = 22_222 byte/s; SB Pro stereo halves it
        // to the per-channel frame rate), block 16, single 8-bit DMA output.
        for &b in &[0x40u8, 0xD3, 0x14, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    // Advance well past the 16-byte block (8 stereo frames at 2 bytes/frame).
    machine.advance_devices_clocks(200_000);
    // Drain the raw producer ring: each frame must carry DISTINCT L/R pulled
    // from two interleaved DMA bytes (left = even byte, right = odd byte).
    let raw = machine.render_dsp_audio(8);
    assert_eq!(raw.len(), 8, "8 stereo frames from a 16-byte block");
    // Frame 0: left byte 0 (= -32768), right byte 16 (= -28672); distinct.
    assert_ne!(raw[0].0, raw[0].1, "frame 0 de-interleaves distinct L/R");
    assert!(
        raw.iter().any(|&(l, r)| l != r),
        "stereo de-interleave yields a per-channel L != R through the DMA path"
    );
    // Single mode masks channel 1 at terminal count.
    assert_eq!(machine.dma_read_byte(1), None);
    // And the resampler runs at the HALVED per-channel rate: byte rate 22_222
    // (1_000_000/45) -> 11_111 Hz.
    let out = machine.render_audio(OPL_NATIVE_HZ as usize / 50);
    assert!(!out.is_empty(), "SB Pro stereo produces output");
    assert_eq!(
        machine.dsp_rate_hz, 11_111,
        "DSP resampler configured at the halved per-channel rate"
    );
}

#[test]
fn sb_16bit_dma_plays_a_signed_stereo_buffer_through_the_dsp() {
    let mut machine = test_machine();
    // 8 signed-LE stereo frames (32 bytes). The slave 8237A (channel 5)
    // word-addresses its transfers, so page 0x01 at word addr 0 drives byte
    // base (0x01 << 17) = 0x2_0000 (page in A23-A17, A0 tied low). Each frame
    // is L = -1 (0xFFFF) then R = +1 (0x0001).
    let frame: [u8; 4] = [0xFF, 0xFF, 0x01, 0x00];
    for i in 0..8 {
        for (j, &b) in frame.iter().enumerate() {
            machine.write_physical_u8(0x2_0000 + (i * 4 + j) as u32, b);
        }
    }
    with_bus(&mut machine, |bus| {
        // Slave ch5 (local ch1): word addr 0, page 0x8B=0x01, count 15 (16
        // words), auto-init read.
        bus.write_io(0xD6, BusWidth::Byte, 0x59, false).unwrap(); // slave ch1 mode: auto-init, read
        bus.write_io(0xC4, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0xC4, BusWidth::Byte, 0x00, false).unwrap(); // word addr 0
        bus.write_io(0xC6, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0xC6, BusWidth::Byte, 0x00, false).unwrap(); // count 15 -> 16 words
        bus.write_io(0x8B, BusWidth::Byte, 0x01, false).unwrap(); // page -> byte base 0x2_0000
        bus.write_io(0xD4, BusWidth::Byte, 0x01, false).unwrap(); // unmask slave ch1
        // Voice volume to unity (0 dB) so the exact -1/+1 samples survive the
        // CT1745 voice attenuation and the test stays about 16-bit decoding.
        bus.write_io(0x224, BusWidth::Byte, 0x32, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x1F, false).unwrap();
        bus.write_io(0x224, BusWidth::Byte, 0x33, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x1F, false).unwrap();
        // DSP: 22050 Hz, 16-bit auto-init output, signed, stereo, count 15.
        for &b in &[0x41u8, 0x56, 0x22, 0xB6, 0x30, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    let out = {
        // Playback is now clock-driven: advance CPU time for well over the
        // 8-frame stereo buffer (auto-init keeps feeding), then drain the ring.
        machine.advance_devices_clocks(200_000);
        machine.render_dsp_audio(8)
    };
    assert_eq!(out.len(), 8);
    assert_eq!(out[0].0, -1, "left channel is signed -1");
    assert_eq!(out[0].1, 1, "right channel is signed +1");
    assert!(
        out.iter().all(|&(l, r)| l == -1 && r == 1),
        "every stereo frame decodes L=-1, R=+1"
    );
    // Auto-init: channel 5 (the mixer's default 16-bit channel) still feeds.
    assert!(
        machine.dma_read_word(5).is_some(),
        "auto-init keeps feeding"
    );
}

// ---- AD1848 / Windows Sound System integration ------------------------

// The default WSS board: config region at 0x530, codec direct registers at
// 0x534-0x537 (base+4), IRQ7, byte-wide DMA channel 0.
const WSS_CODEC: u16 = 0x534; // R0 Index
const WSS_DATA: u16 = 0x535; // R1 Indexed Data

/// Write one AD1848 indirect register through the codec's R0 (index) + R1
/// (data) direct ports on the machine bus.
fn wss_write_indirect(bus: &mut MachineBus, index: u8, value: u8) {
    bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(index), false)
        .unwrap();
    bus.write_io(WSS_DATA, BusWidth::Byte, u32::from(value), false)
        .unwrap();
}

#[test]
fn wss_16bit_stereo_dma_plays_and_irqs_through_the_machine() {
    let mut machine = test_machine();
    // 8 signed-LE 16-bit stereo frames (32 bytes) at byte base 0x01_0000 over
    // the WSS byte-wide DMA channel 0. Each frame is asymmetric: L = +1
    // (0x0001), R = -2 (0xFFFE), so a real de-interleave yields L != R and the
    // codec's left-before-right ordering is observable.
    let frame: [u8; 4] = [0x01, 0x00, 0xFE, 0xFF];
    for i in 0..8u32 {
        for (j, &b) in frame.iter().enumerate() {
            machine.write_physical_u8(0x1_0000 + i * 4 + j as u32, b);
        }
    }
    with_bus(&mut machine, |bus| {
        // DMA ch0 (the WSS default): byte addr 0x0000, page 0x01 -> 0x01_0000,
        // count 31 (32 bytes), single read. Channel 0 ports: addr 0x00,
        // count 0x01, mode 0x0B, page 0x87, single-mask 0x0A.
        bus.write_io(0x0B, BusWidth::Byte, 0x48, false).unwrap(); // mode ch0: single, read
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap(); // addr 0x0000
        bus.write_io(0x01, BusWidth::Byte, 0x1F, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x00, false).unwrap(); // count 31 -> 32 bytes
        bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap(); // ch0 page -> 0x01_0000
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap(); // unmask ch0

        // Program the codec for 16-bit signed stereo at 48000 Hz (XTAL1 CFS6).
        // I8 = FMT(0x40) | S/M(0x10) | CFS6(0x0C) -> 0x5C, MCE-gated.
        bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08), false)
            .unwrap(); // R0: MCE | index 8
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x5C, false).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
            .unwrap(); // clear MCE
        // Enable the external INT pin (I10 IEN, bit1) so terminal count forwards.
        wss_write_indirect(bus, 10, 0x02);
        // Base count 7 -> underflow at frame 8 (N+1 cadence).
        wss_write_indirect(bus, 15, 0x07); // I15 lower count
        wss_write_indirect(bus, 14, 0x00); // I14 upper count (loads current)
        // Arm playback: I9 PEN (bit0) + ACAL (bit3).
        wss_write_indirect(bus, 9, 0x09);
        // Unmute both DACs at 0 dB so the decoded samples pass through.
        wss_write_indirect(bus, 6, 0x00);
        wss_write_indirect(bus, 7, 0x00);
    });

    // Drain the codec ring directly to prove de-interleave (render_audio mixes
    // OPL in, which would mask the exact values).
    machine.advance_devices_clocks(200_000);
    let mut frames = Vec::new();
    while let Some(f) = machine.wss.drain_frame() {
        frames.push(f);
    }
    assert!(!frames.is_empty(), "WSS produced rendered frames");
    assert_eq!(
        frames[0],
        (1, -2),
        "16-bit LE de-interleave, left before right"
    );
    assert!(
        frames.iter().any(|&(l, r)| l != r),
        "asymmetric L/R proves a real stereo de-interleave (not a mono dup)"
    );

    // The terminal-count interrupt reached the PIC on the WSS line (IRQ7).
    assert!(
        machine.pic.irr_bit(7),
        "WSS terminal count raised its IRQ on the configured line"
    );

    // render_audio still produces a full mix here (this drained the WSS ring
    // above, so the WSS contribution is proven through render_audio separately
    // in wss_stream_reaches_the_mixed_render_output_through_render_audio; this
    // only checks the OPL/DSP/speaker mix is not truncated by the WSS path).
    let mixed = machine.render_audio(64);
    assert!(
        !mixed.is_empty(),
        "render_audio still mixes the other streams after the WSS ring is drained"
    );
}

#[test]
fn wss_coexists_with_sb16_and_opl_without_cross_talk() {
    // With WSS enabled, the SB16 DSP + OPL must still function and there must
    // be no port/IRQ/DMA cross-talk: WSS uses base 0x530 / IRQ7 / DMA0, the
    // SB16 uses 0x220-0x22F / IRQ5 / DMA1, the OPL uses 0x388/9.
    let mut machine = test_machine();

    // SB16 8-bit mono playback on DMA ch1 (the SB default), exactly like the
    // standalone DSP golden, at byte base 0x02_0000.
    let bytes: Vec<u8> = (0..16).map(|i| (i * 16) as u8).collect();
    for (i, &b) in bytes.iter().enumerate() {
        machine.write_physical_u8(0x2_0000 + i as u32, b);
    }
    // A distinct WSS 8-bit mono buffer on DMA ch0 at byte base 0x01_0000:
    // a constant near-full-positive value so the WSS stream is unmistakable.
    for i in 0..16u32 {
        machine.write_physical_u8(0x1_0000 + i, 0xFF);
    }

    with_bus(&mut machine, |bus| {
        // --- SB16 DMA ch1 + DSP (IRQ5/DMA1, ports 0x220-0x22F) ---
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap(); // mode ch1: single, read
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x02, false).unwrap(); // ch1 page -> 0x02_0000
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap(); // unmask ch1
        for &b in &[0x41u8, 0x2B, 0x11, 0x14, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
        // OPL: key a full sustained tone on channel 0 (modulator + carrier +
        // key-on) so the OPL stream is genuinely audible, not just touched.
        program_tone(bus, 0x388, 0x389);

        // --- WSS DMA ch0 + codec (IRQ7/DMA0, ports 0x530-0x537) ---
        bus.write_io(0x0B, BusWidth::Byte, 0x48, false).unwrap(); // mode ch0: single, read
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap(); // ch0 page -> 0x01_0000
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap(); // unmask ch0
        // Codec: 8-bit unsigned PCM mono at 48000 Hz (I8 = CFS6 only -> 0x0C).
        bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08), false)
            .unwrap();
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x0C, false).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
            .unwrap();
        wss_write_indirect(bus, 10, 0x02); // IEN
        wss_write_indirect(bus, 15, 0x07); // count low
        wss_write_indirect(bus, 14, 0x00); // count high
        wss_write_indirect(bus, 9, 0x09); // PEN | ACAL
        wss_write_indirect(bus, 6, 0x00);
        wss_write_indirect(bus, 7, 0x00);
    });

    machine.advance_devices_clocks(200_000);

    // Both producers filled their own rings independently.
    let mut wss_frames = Vec::new();
    while let Some(f) = machine.wss.drain_frame() {
        wss_frames.push(f);
    }
    assert!(!wss_frames.is_empty(), "WSS still plays alongside the SB16");
    assert!(
        wss_frames.iter().all(|&(l, r)| l == r && l > 0),
        "WSS 8-bit unsigned 0xFF -> near-full-positive mono dup, undisturbed"
    );
    let dsp_out = machine.render_dsp_audio(16);
    assert_eq!(dsp_out.len(), 16, "SB16 DSP still plays its own buffer");

    // No IRQ cross-talk: WSS fired IRQ7, the SB16 fired its mixer-selected
    // IRQ5, and neither stepped on the other.
    assert_eq!(
        machine.sb_selected_irq(),
        5,
        "SB16 default IRQ unchanged by WSS"
    );
    assert!(machine.pic.irr_bit(7), "WSS raised IRQ7");
    assert!(
        machine.pic.irr_bit(machine.sb_selected_irq()),
        "SB16 raised its own (IRQ5) line"
    );

    // No DMA cross-talk: the WSS drew from ch0, the SB16 from ch1; both single
    // channels reached terminal count on their own.
    assert_eq!(machine.dma_read_byte(0), None, "WSS ch0 reached TC");
    assert_eq!(machine.dma_read_byte(1), None, "SB16 ch1 reached TC");

    // The OPL really produces output (the keyed note is audible), not just a
    // non-empty render_audio that the speaker/WSS streams alone would satisfy.
    // Render the OPL in isolation and assert a non-zero sample magnitude.
    let opl_nonsilent = (0..512)
        .map(|_| machine.opl.render_sample())
        .any(|(l, r)| l != 0 || r != 0);
    assert!(
        opl_nonsilent,
        "OPL still synthesizes its keyed note alongside the WSS and SB16"
    );

    // The full mix is non-empty (OPL + SB16 + WSS all summed).
    let mixed = machine.render_audio(64);
    assert!(!mixed.is_empty());
}

#[test]
fn sb_dsp_auto_init_edges_forward_within_their_advance_and_rearm_after_ack() {
    // Multi-edge contract (see advance_devices): every block edge reaches
    // the PIC inside the advance whose sample tick produced it, and edges
    // that land while the previous request is still latched coalesce in the
    // 8259's IRR. The CPU does not execute during advance_devices, so the
    // guest cannot acknowledge between intra-step edges; the IRR absorbing
    // the extra requests is exactly the hardware's still-asserted-line
    // behavior, not a loss. After the guest-style ack sequence the NEXT
    // step's edges must raise the line again: nothing may stay parked in
    // the DSP's take_irq latch across steps.
    let mut machine = test_machine();
    // 16-byte auto-init DMA loop on ch1 feeding an 8-sample auto-init DSP
    // block: one 200k-clock advance at 22 MHz (~9 ms, ~99 frames at
    // 11025 Hz) spans MANY half/end block edges.
    for (i, b) in (0..16u8).map(|i| i * 16).enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        // PIC base 0x08 (IRQ5 -> vector 0x0D), all lines unmasked.
        bus.write_io(0x20, BusWidth::Byte, 0x11, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x08, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x00, false).unwrap();
        // DMA ch1: auto-init read of 16 bytes at 0x1_0000 (mode 0x59).
        bus.write_io(0x0B, BusWidth::Byte, 0x59, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        // DSP: rate 11025 Hz, block size 8, 8-bit auto-init output.
        for &b in &[0x41u8, 0x2B, 0x11, 0x48, 0x07, 0x00, 0x1C] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    machine.advance_devices_clocks(200_000);
    assert!(
        machine.dsp.is_playing(),
        "auto-init keeps the block looping"
    );
    assert!(
        machine.pic.irr_bit(5),
        "the block edges latched IRR5 within their own step"
    );
    assert!(
        !machine.dsp.take_irq(),
        "no edge stays parked in the DSP latch after the step"
    );
    // Guest ISR: PIC acknowledge, device ack (0x22E read), then EOI.
    with_bus(&mut machine, |bus| {
        assert_eq!(bus.acknowledge_interrupt(), Some(0x0D));
        bus.read_io(0x22E, BusWidth::Byte, 0, false).unwrap();
        bus.write_io(0x20, BusWidth::Byte, 0x20, false).unwrap(); // OCW2 EOI
    });
    assert!(!machine.pic.irr_bit(5), "IRR clear after the acknowledge");
    // Later edges arrive in a later advance and re-request the line: the
    // edge stream survives the ack instead of being lost with it.
    machine.advance_devices_clocks(200_000);
    assert!(
        machine.pic.irr_bit(5),
        "the next step's block edges re-request IRQ5"
    );
}

#[test]
fn wss_terminal_count_edge_forwards_within_its_advance() {
    // Same multi-edge contract as the DSP loop: the codec's terminal-count
    // edge is drained per output frame inside the producer loop, so it
    // reaches the PIC within the advance whose frame produced it and never
    // parks in the take_irq latch across steps.
    let mut machine = test_machine();
    for i in 0..64u32 {
        machine.write_physical_u8(0x1_0000 + i, 0x80);
    }
    with_bus(&mut machine, |bus| {
        wss_arm_8bit_mono(bus, 31); // TC after 32 frames (~0.67 ms at 48 kHz)
    });
    // One advance spanning the whole 32-frame window plus slack.
    machine.advance_devices_clocks(200_000);
    assert!(
        machine.pic.irr_bit(7),
        "the terminal-count edge latched IRR7 within the step"
    );
    assert!(
        !machine.wss.take_irq(),
        "no edge stays parked in the codec latch after the step"
    );
}

#[test]
fn pit_channel0_multi_edge_advance_latches_irq0() {
    // Channel 0 is the per-edge exemplar (a request per OUT rising edge,
    // issued inside the producer): several mode-2 periods inside ONE
    // advance coalesce in the IRR, and the request is present at step end.
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0x34, false).unwrap(); // ch0 mode 2
        bus.write_io(0x40, BusWidth::Byte, 100, false).unwrap();
        bus.write_io(0x40, BusWidth::Byte, 0, false).unwrap();
    });
    // ~325 PIT ticks at 22 MHz: three-plus full periods in one advance.
    machine.advance_devices_clocks(6_000);
    assert!(
        machine.pic.irr_bit(0),
        "channel-0 edges latched IRQ0 within the multi-period step"
    );
}

#[test]
fn approx_batch_cap_tracks_the_next_device_event() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw586);
    let clock_hz = machine.active_mode().clock_hz();
    let ceiling = clock_hz / 1000;
    let floor = machine.timing.clocks_per_audio_sample;

    // Nothing scheduled: the ~1 ms latency ceiling binds. The always-running
    // channel-1 refresh heartbeat (mode 2, reload 18, ~15 us) must NOT
    // bind, or this cap could never exceed the Accurate DAC-sample cap.
    assert_eq!(machine.approx_batch_cap(u64::MAX), ceiling);

    // The remaining-deadline clamp wins when nearer.
    assert_eq!(machine.approx_batch_cap(123), 123);

    // A running channel-0 (IRQ0) counter binds the cap to its next OUT rise.
    with_bus(&mut machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0x34, false).unwrap(); // ch0 mode 2
        bus.write_io(0x40, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x40, BusWidth::Byte, 0x04, false).unwrap(); // reload 0x0400
    });
    let ticks = machine.pit.clocks_until_out_rise(0).unwrap();
    let expected =
        ((u128::from(ticks) * u128::from(clock_hz)).div_ceil(u128::from(PIT_INPUT_HZ))) as u64;
    assert!(expected < ceiling && expected > floor);
    assert_eq!(machine.approx_batch_cap(u64::MAX), expected);

    // A sub-sample edge floors at the DAC-sample cap: an Approximate batch
    // is never SHORTER than an Accurate one.
    with_bus(&mut machine, |bus| {
        bus.write_io(0x40, BusWidth::Byte, 0x08, false).unwrap();
        bus.write_io(0x40, BusWidth::Byte, 0x00, false).unwrap(); // reload 8 (~7 us)
    });
    assert_eq!(machine.approx_batch_cap(u64::MAX), floor);
}

#[test]
fn approx_batch_cap_ends_at_the_next_dsp_block_edge() {
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw586);
    let clock_hz = machine.active_mode().clock_hz();
    for (i, b) in (0..16u8).map(|i| i * 16).enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        // The 16-frame 8-bit single-cycle golden: at 11025 Hz the half edge
        // (8 frames, ~726 us) is the next due event, under the ~1 ms
        // ceiling and above the DAC-sample floor.
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        for &b in &[0x41u8, 0x2B, 0x11, 0x14, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    let expected = machine
        .dsp
        .clocks_until_next_irq(machine.dsp.rate_hz(), clock_hz)
        .unwrap();
    assert!(expected < clock_hz / 1000, "half edge under the ceiling");
    assert!(expected > machine.timing.clocks_per_audio_sample);
    assert_eq!(machine.approx_batch_cap(u64::MAX), expected);
}

#[test]
fn approximate_class_delivers_pit_irq0_during_long_compute_stretches() {
    // P4c end to end: in the Approximate class a guest that computes for
    // many milliseconds without any port I/O still sees IRQ0 at the
    // programmed cadence. Each edge is requested by advance_devices in the
    // batch that spans it, the cap ends that batch at (about) the edge
    // instant, and the next batch entry services the interrupt.
    let code = [
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x00, 0x70, 0x00, 0x00, // mov word [0x7000], 0
        0xB0, 0x11, 0xE6, 0x20, // ICW1
        0xB0, 0x08, 0xE6, 0x21, // ICW2: vector base 0x08
        0xB0, 0x04, 0xE6, 0x21, // ICW3
        0xB0, 0x01, 0xE6, 0x21, // ICW4
        0xB0, 0x00, 0xE6, 0x21, // unmask all lines
        0xB0, 0x34, 0xE6, 0x43, // PIT ch0 mode 2, LSB/MSB
        0xB0, 0x00, 0xE6, 0x40, // reload low 0x00
        0xB0, 0x10, 0xE6, 0x40, // reload high 0x10 -> 4096 ticks (~3.43 ms)
        0xFB, // sti
        0xEB, 0xFE, // jmp $ (pure compute, no port I/O)
    ];
    let mut machine = Machine::new(
        MachineProfile::gsw_386(4, VideoCard::Et4000Ax),
        rom_with_code(&code),
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw586); // Approximate class, 200 MHz
    // IRQ0 handler at 0x0700: inc word [0x7000]; mov al,0x20; out 0x20,al; iret.
    let handler: [u8; 9] = [0xff, 0x06, 0x00, 0x70, 0xb0, 0x20, 0xe6, 0x20, 0xcf];
    for (i, &b) in handler.iter().enumerate() {
        machine.write_physical_u8(0x0700 + i as u32, b);
    }
    // IVT[0x08] (IRQ0 at PIC base 0x08) -> 0000:0700.
    machine.write_physical_u8(0x20, 0x00);
    machine.write_physical_u8(0x21, 0x07);
    machine.write_physical_u8(0x22, 0x00);
    machine.write_physical_u8(0x23, 0x00);
    // ~20 periods of 4096 PIT ticks at 200 MHz (~686.6k clocks each).
    machine.run_cycles(14_000_000).unwrap();
    let ticks = u16::from(machine.read_physical_u8(0x7000))
        | (u16::from(machine.read_physical_u8(0x7001)) << 8);
    assert!(
        (15..=22).contains(&ticks),
        "expected ~20 IRQ0 ticks across the compute stretch, saw {ticks}"
    );
}

/// Program DMA channel 0 (the WSS default) for a single-cycle 8-bit read of
/// `count + 1` bytes at physical `0x01_0000`, then arm the AD1848 codec for
/// 8-bit unsigned mono at 48000 Hz with IEN set and `count` base count.
fn wss_arm_8bit_mono(bus: &mut MachineBus, count: u8) {
    // DMA ch0: mode single+read, addr 0x0000, count, page 0x01, unmask.
    bus.write_io(0x0B, BusWidth::Byte, 0x48, false).unwrap();
    bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
    bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
    bus.write_io(0x01, BusWidth::Byte, u32::from(count), false)
        .unwrap();
    bus.write_io(0x01, BusWidth::Byte, 0x00, false).unwrap();
    bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap();
    bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap();
    // Codec: 8-bit unsigned PCM mono at 48000 Hz (I8 = CFS6 -> 0x0C), MCE-gated.
    bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08), false)
        .unwrap();
    bus.write_io(WSS_DATA, BusWidth::Byte, 0x0C, false).unwrap();
    bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
        .unwrap(); // clear MCE
    wss_write_indirect(bus, 10, 0x02); // I10 IEN
    wss_write_indirect(bus, 15, count); // I15 lower count
    wss_write_indirect(bus, 14, 0x00); // I14 upper count (loads current)
    wss_write_indirect(bus, 9, 0x09); // I9 PEN | ACAL
    wss_write_indirect(bus, 6, 0x00);
    wss_write_indirect(bus, 7, 0x00);
}

#[test]
fn wss_irq7_wakes_a_halted_cpu_via_fast_forward() {
    // Mirror sb_dma_irq5_wakes_a_halted_cpu_via_fast_forward for the WSS wake
    // branch in next_device_wake: a guest arms WSS playback with IEN set and
    // IRQ7 unmasked, then sti;hlt. The run loop must fast-forward across the
    // codec's terminal-count window and deliver IRQ7 -- proving the wss_wake
    // estimator drives the machine, not just the wss.rs unit test.
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        // mov ax,0; mov ds,ax; sti; hlt; cli; hlt
        rom_with_code(&[0xb8, 0x00, 0x00, 0x8e, 0xd8, 0xfb, 0xf4, 0xfa, 0xf4]),
    )
    .unwrap();
    // 64-byte buffer at 0x01_0000 (DMA ch0 page 0x01) so the codec never
    // underruns before terminal count.
    for i in 0..64u32 {
        machine.write_physical_u8(0x1_0000 + i, 0x80);
    }
    // IRQ7 handler at 0x0700: inc word [0x0610]; mov al,0x20; out 0x20,al; iret.
    let handler: [u8; 9] = [0xff, 0x06, 0x10, 0x06, 0xb0, 0x20, 0xe6, 0x20, 0xcf];
    for (i, &b) in handler.iter().enumerate() {
        machine.write_physical_u8(0x0700 + i as u32, b);
    }
    // IVT[0x0F] (IRQ7 with PIC base 0x08) -> 0x0000:0x0700; clear the counter.
    machine.write_physical_u8(0x3C, 0x00);
    machine.write_physical_u8(0x3D, 0x07);
    machine.write_physical_u8(0x3E, 0x00);
    machine.write_physical_u8(0x3F, 0x00);
    machine.write_physical_u8(0x0610, 0x00);
    machine.write_physical_u8(0x0611, 0x00);
    with_bus(&mut machine, |bus| {
        // PIC base 0x08 so IRQ7 -> vector 0x0F; all IRQs unmasked.
        bus.write_io(0x20, BusWidth::Byte, 0x11, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x08, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x00, false).unwrap(); // unmask all
        // Base count 31 -> terminal count after 32 frames.
        wss_arm_8bit_mono(bus, 31);
    });
    let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let ticks = u16::from(machine.read_physical_u8(0x0610))
        | (u16::from(machine.read_physical_u8(0x0611)) << 8);
    assert!(ticks >= 1, "the WSS IRQ7 handler should have run");
    // The fast-forward crossed a real sample window (32 frames at 48 kHz ~=
    // 14.6k CPU clocks at 22 MHz), not a no-op halt.
    assert!(
        machine.elapsed_clocks() > 10_000,
        "the fast-forward should advance emulated time across the WSS window"
    );
}

#[test]
fn wss_honors_a_configured_slave_irq11_end_to_end() {
    // The default integration tests only exercise IRQ7 (master). Prove a machine
    // configured with a slave line (IRQ11) actually raises THAT line: wss_irq is
    // taken from profile.wss.irq.line() and fed to pic.request(wss_irq), so a
    // transposed or hardcoded line would route the terminal-count IRQ to the
    // wrong pin. Arm the codec and advance device time across its window; the
    // configured line must latch in the PIC's IRR and IRQ7 must stay clear.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    profile.wss = WssConfig {
        irq: izarravm_core::WssIrq::I11,
        ..WssConfig::default()
    };
    let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();
    for i in 0..64u32 {
        machine.write_physical_u8(0x1_0000 + i, 0x80);
    }
    with_bus(&mut machine, |bus| {
        wss_arm_8bit_mono(bus, 31); // base count 31 -> TC after 32 frames
    });
    machine.advance_devices_clocks(200_000);
    assert!(
        machine.pic.irr_bit(11),
        "the codec raised its configured IRQ11 (slave line)"
    );
    assert!(
        !machine.pic.irr_bit(7),
        "the default IRQ7 was NOT raised when IRQ11 is configured"
    );
}

#[test]
fn wss_masked_irq7_does_not_wake_the_cpu() {
    // The IRQ-masked path: with IRQ7 masked, next_device_wake's wss branch must
    // be None (it gates on pic.deliverable), so sti;hlt is a genuine halt the
    // codec cannot wake. We mask EVERY line (IMR = 0xFF) so the WSS is the only
    // armed device and no other source can confound the wake -- the run loop
    // therefore halts at the first hlt and the handler never runs, even with
    // interrupts enabled. (The sticky Status INT bit is proven separately in
    // wss_masked_ien_clear_sets_sticky_status_without_a_pic_edge.)
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        // mov ax,0; mov ds,ax; sti; hlt; cli; hlt
        rom_with_code(&[0xb8, 0x00, 0x00, 0x8e, 0xd8, 0xfb, 0xf4, 0xfa, 0xf4]),
    )
    .unwrap();
    for i in 0..64u32 {
        machine.write_physical_u8(0x1_0000 + i, 0x80);
    }
    // IRQ7 handler that bumps a counter, so we can prove it never runs.
    let handler: [u8; 9] = [0xff, 0x06, 0x10, 0x06, 0xb0, 0x20, 0xe6, 0x20, 0xcf];
    for (i, &b) in handler.iter().enumerate() {
        machine.write_physical_u8(0x0700 + i as u32, b);
    }
    machine.write_physical_u8(0x3C, 0x00);
    machine.write_physical_u8(0x3D, 0x07);
    machine.write_physical_u8(0x0610, 0x00);
    machine.write_physical_u8(0x0611, 0x00);
    with_bus(&mut machine, |bus| {
        // PIC base 0x08, then mask ALL lines (IMR = 0xFF) so only the codec is
        // armed and nothing can wake the CPU.
        bus.write_io(0x20, BusWidth::Byte, 0x11, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x08, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0xFF, false).unwrap(); // mask every line
        wss_arm_8bit_mono(bus, 31);
    });
    // Run long enough that, were the WSS line a wake source, the codec would
    // fast-forward to terminal count and the CPU would advance well past the
    // window. A genuine halt makes no progress at the first hlt.
    let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let ticks = u16::from(machine.read_physical_u8(0x0610))
        | (u16::from(machine.read_physical_u8(0x0611)) << 8);
    assert_eq!(ticks, 0, "masked IRQ7 must not deliver the WSS interrupt");
    assert!(
        !machine.pic.irq_unmasked(7),
        "IRQ7 stayed masked for the duration"
    );
    // The codec did NOT wake the CPU: a masked WSS line is not a wake source,
    // so the run loop genuinely halted instead of fast-forwarding across the
    // ~32-frame codec window (which would have advanced emulated time by 10k+
    // clocks, as the unmasked twin test asserts).
    assert!(
        machine.elapsed_clocks() < 5_000,
        "a genuine halt does not fast-forward across the masked codec window"
    );
}

#[test]
fn wss_masked_ien_clear_sets_sticky_status_without_a_pic_edge() {
    // Underflow sets the codec's *internal* sticky Status INT bit regardless of
    // IEN (datasheet: the internal INT bit becomes one on counter underflow even
    // if IEN is zero), but the external INT *pin* -- and hence the PIC forward in
    // advance_devices -- is gated by IEN. Arm with IEN CLEAR and drive the codec
    // to terminal count directly (advance_devices, no CPU); the sticky bit must
    // be set while no edge ever reaches the PIC, proving the two are distinct.
    const R2_INT: u8 = 0x01;
    let mut machine = test_machine();
    for i in 0..64u32 {
        machine.write_physical_u8(0x1_0000 + i, 0x80);
    }
    with_bus(&mut machine, |bus| {
        // DMA ch0 for 32 bytes at 0x01_0000, 8-bit unsigned mono at 48 kHz, but
        // with IEN CLEAR (I10 = 0) so the underflow forwards no pin edge.
        bus.write_io(0x0B, BusWidth::Byte, 0x48, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x1F, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08), false)
            .unwrap();
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x0C, false).unwrap();
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
            .unwrap();
        wss_write_indirect(bus, 10, 0x00); // IEN CLEAR
        wss_write_indirect(bus, 15, 0x1F); // base count 31 -> TC after 32 frames
        wss_write_indirect(bus, 14, 0x00);
        wss_write_indirect(bus, 9, 0x09); // PEN | ACAL
        wss_write_indirect(bus, 6, 0x00);
        wss_write_indirect(bus, 7, 0x00);
    });
    // Advance device time across the full ~32-frame window at 48 kHz (~14.6k CPU
    // clocks at 22 MHz; use a generous budget so terminal count is reached).
    machine.advance_devices_clocks(200_000);
    assert_ne!(
        machine.wss.status() & R2_INT,
        0,
        "underflow sets the internal sticky Status INT bit even with IEN clear"
    );
    assert!(
        !machine.pic.irr_bit(7),
        "IEN clear forwards no pin edge, so the PIC line stays clear"
    );
}

#[test]
fn wss_disabled_leaves_its_ports_undecoded() {
    // With the codec disabled, its config/codec ports must NOT decode at all:
    // 0x530-0x537 is not in known_passive_ports(), so the bus must return
    // Err(UnsupportedPort) for both reads and writes -- not a swallowed error
    // and not a stale latched value. Contrast with the enabled machine, which
    // answers the I12 revision read with 0x0A, so the test proves the gate
    // toggles real decode rather than relying on an error either way.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    profile.wss = WssConfig {
        enabled: false,
        ..WssConfig::default()
    };
    let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();
    with_bus(&mut machine, |bus| {
        // A write to the index port must not decode.
        assert!(
            matches!(
                bus.write_io(WSS_CODEC, BusWidth::Byte, 0x0C, false),
                Err(BusError::UnsupportedPort { port }) if port == WSS_CODEC
            ),
            "disabled WSS index write does not decode"
        );
        // A read of the data port (where an enabled codec would surface the
        // I12 revision) must not decode either.
        assert!(
            matches!(
                bus.read_io(WSS_DATA, BusWidth::Byte, 0, false),
                Err(BusError::UnsupportedPort { port }) if port == WSS_DATA
            ),
            "disabled WSS data read does not decode"
        );
        // The window edges (base+7) are likewise undecoded.
        assert!(
            matches!(
                bus.read_io(0x537, BusWidth::Byte, 0, false),
                Err(BusError::UnsupportedPort { port }) if port == 0x537
            ),
            "disabled WSS upper window edge does not decode"
        );
    });

    // The same index-12 read on an ENABLED machine DOES decode to 0x0A, so the
    // disabled assertions above are a genuine contrast, not a vacuous pass.
    let mut enabled = test_machine();
    with_bus(&mut enabled, |bus| {
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x0C, false)
            .unwrap(); // select I12
        assert_eq!(
            bus.read_io(WSS_DATA, BusWidth::Byte, 0, false).unwrap(),
            0x0A,
            "enabled WSS answers the I12 revision read"
        );
    });
}

#[test]
fn wss_disabled_advance_and_render_run_cleanly_and_stay_silent() {
    // The disabled-codec branches in the producer loop (`if self.wss_enabled`)
    // and in render_audio (the `} else { Vec::new() }` WSS arm) are never reached
    // by the port-decode disabled test, which exits before any audio work. Run a
    // disabled machine through advance_devices AND render_audio to prove those
    // branches execute cleanly (no panic) and the WSS contributes silence. We
    // arm DMA/codec ports first to confirm a disabled codec ignores them entirely
    // -- but the ports do not decode, so the writes that would land on the codec
    // are skipped; only the DMA programming (separate decoder) is set up.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    profile.wss = WssConfig {
        enabled: false,
        ..WssConfig::default()
    };
    let mut machine = Machine::new(profile, I386DX25_TEST_ROM).unwrap();
    for i in 0..64u32 {
        machine.write_physical_u8(0x1_0000 + i, 0xFF);
    }
    with_bus(&mut machine, |bus| {
        // Program DMA ch0 (a separate decoder, still live) so the producer loop,
        // had it run the codec, would have data to read. The codec ports do not
        // decode while disabled, so we do not touch them here.
        bus.write_io(0x0B, BusWidth::Byte, 0x48, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x3F, false).unwrap();
        bus.write_io(0x01, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap();
    });
    // The disabled producer branch must be a no-op: this runs cleanly and queues
    // no codec frames.
    machine.advance_devices_clocks(200_000);
    assert!(
        machine.wss.drain_frame().is_none(),
        "a disabled codec renders no frames"
    );
    // The disabled render_audio arm (Vec::new()) must contribute nothing; with
    // OPL/DSP idle and the speaker silent the whole mix is silence.
    let mixed = machine.render_audio(64);
    assert!(
        mixed.iter().all(|&(l, r)| l == 0 && r == 0),
        "a disabled WSS adds nothing; idle OPL/DSP/speaker leave silence"
    );
}

/// Program DMA channel 0 and the codec for 16-bit signed stereo at 48 kHz with
/// IEN set, drawing `frames` frames (4 bytes each) at physical 0x01_0000.
fn wss_arm_16bit_stereo(bus: &mut MachineBus, frames: u8) {
    let byte_count = u16::from(frames) * 4 - 1; // count is bytes-1
    bus.write_io(0x0B, BusWidth::Byte, 0x48, false).unwrap(); // mode ch0: single, read
    bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
    bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
    bus.write_io(0x01, BusWidth::Byte, u32::from(byte_count & 0xFF), false)
        .unwrap();
    bus.write_io(0x01, BusWidth::Byte, u32::from(byte_count >> 8), false)
        .unwrap();
    bus.write_io(0x87, BusWidth::Byte, 0x01, false).unwrap();
    bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap();
    // I8 = FMT(0x40) | S/M(0x10) | CFS6(0x0C) -> 0x5C, MCE-gated.
    bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08), false)
        .unwrap();
    bus.write_io(WSS_DATA, BusWidth::Byte, 0x5C, false).unwrap();
    bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
        .unwrap(); // clear MCE
    wss_write_indirect(bus, 10, 0x02); // IEN
    let count = u16::from(frames) - 1;
    wss_write_indirect(bus, 15, (count & 0xFF) as u8);
    wss_write_indirect(bus, 14, (count >> 8) as u8);
    wss_write_indirect(bus, 9, 0x09); // PEN | ACAL
    wss_write_indirect(bus, 6, 0x00); // left DAC 0 dB
    wss_write_indirect(bus, 7, 0x00); // right DAC 0 dB
}

/// Load `frames` asymmetric 16-bit LE stereo frames at 0x01_0000: L = +0x4000,
/// R = -0x4000, so the de-interleaved, mixed output carries L > 0 and R < 0.
fn load_asymmetric_stereo(machine: &mut Machine, frames: u32) {
    // L = 0x4000 (+16384) -> bytes 0x00,0x40; R = 0xC000 (-16384) -> 0x00,0xC0.
    let frame: [u8; 4] = [0x00, 0x40, 0x00, 0xC0];
    for i in 0..frames {
        for (j, &b) in frame.iter().enumerate() {
            machine.write_physical_u8(0x1_0000 + i * 4 + j as u32, b);
        }
    }
}

#[test]
fn wss_stream_reaches_the_mixed_render_output_through_render_audio() {
    // Finding: the de-interleave smoke test pre-drains the ring before calling
    // render_audio, so the resampler + L/R summation path is never proven to
    // carry WSS audio. Here we arm an asymmetric stereo buffer, advance devices,
    // and call render_audio WITHOUT draining -- with OPL/DSP idle and the speaker
    // silent, the only possible signal is the WSS stream, so the mixed output
    // must show the codec's L>0 / R<0 sign pattern. Disabling WSS for the same
    // buffer must then yield silence, proving the contribution came from the
    // WSS mix path and not from some other stream.
    let mut machine = test_machine();
    load_asymmetric_stereo(&mut machine, 64);
    with_bus(&mut machine, |bus| wss_arm_16bit_stereo(bus, 64));
    machine.advance_devices_clocks(200_000);
    let mixed = machine.render_audio(64);
    assert!(
        mixed.iter().any(|&(l, r)| l > 0 && r < 0),
        "the WSS stream reaches the mixed L/R output with its asymmetric sign \
             pattern (left positive, right negative)"
    );

    // The identical buffer with WSS disabled produces silence: nothing else is
    // sounding, so the signal above was the WSS mix path, not a stray stream.
    let mut silent_profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    silent_profile.wss = WssConfig {
        enabled: false,
        ..WssConfig::default()
    };
    let mut silent = Machine::new(silent_profile, I386DX25_TEST_ROM).unwrap();
    load_asymmetric_stereo(&mut silent, 64);
    silent.advance_devices_clocks(200_000);
    let quiet = silent.render_audio(64);
    assert!(
        quiet.iter().all(|&(l, r)| l == 0 && r == 0),
        "with WSS disabled the same buffer mixes to silence"
    );
}

#[test]
fn wss_is_summed_raw_bypassing_the_ct1745_master_gain() {
    // Design contract: the WSS stream is summed into render_audio WITHOUT the
    // CT1745 master/voice/outgain scaling that OPL and the SB16 DSP take. Prove
    // it by HARD-MUTING the CT1745 master (level 0 = exactly 0.0 gain): an OPL
    // tone is silenced, but the WSS stream must still reach the output unchanged.
    let mut machine = test_machine();
    load_asymmetric_stereo(&mut machine, 64);
    with_bus(&mut machine, |bus| {
        // Hard-mute the CT1745 master (0x30/0x31 = 0) -- this scales OPL+DSP to
        // exactly zero but, per the contract, must NOT touch the WSS stream.
        bus.write_io(0x224, BusWidth::Byte, 0x30, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x224, BusWidth::Byte, 0x31, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x00, false).unwrap();
        // Key a loud OPL tone: with the master muted it must contribute zero.
        program_tone(bus, 0x388, 0x389);
        wss_arm_16bit_stereo(bus, 64);
    });
    machine.advance_devices_clocks(200_000);
    let mixed = machine.render_audio(64);
    assert!(
        mixed.iter().any(|&(l, r)| l > 0 && r < 0),
        "the WSS stream survives a hard-muted CT1745 master (summed raw)"
    );

    // Control: the SAME muted master with the OPL tone but WSS disabled yields
    // silence -- confirming the master mute really does zero the OPL/DSP path,
    // so the non-silence above is the raw WSS sum and not a leaking OPL tone.
    let mut control = test_machine();
    with_bus(&mut control, |bus| {
        bus.write_io(0x224, BusWidth::Byte, 0x30, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x224, BusWidth::Byte, 0x31, false).unwrap();
        bus.write_io(0x225, BusWidth::Byte, 0x00, false).unwrap();
        program_tone(bus, 0x388, 0x389);
    });
    control.advance_devices_clocks(200_000);
    let muted = control.render_audio(64);
    assert!(
        muted.iter().all(|&(l, r)| l == 0 && r == 0),
        "a hard-muted master zeroes the OPL/DSP path (proving the mute is live)"
    );
}

#[test]
fn wss_autocal_window_drains_even_under_an_invalid_sample_rate() {
    // The autocal converter clock retires the ~128-sample ACI window regardless
    // of the programmed sample rate. If the producer only advanced the autocal
    // when rate_hz() > 0, a guest that cleared MCE while I8 selects one of the
    // two unsupported XTAL1 rates (rate_hz() == 0) would leave ACI asserted
    // forever. Arm an invalid rate (XTAL1 CFS4 -> 0 Hz), trigger autocal by an
    // MCE clear, and advance device time: ACI must retire on the fallback clock.
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        // Select an UNSUPPORTED rate: I8 = CFS4 (bits3:1 = 4 -> 0x08), CSS=0
        // (XTAL1). rate_hz() decodes this to 0. Set MCE to latch I8, then clear
        // MCE to assert ACI.
        bus.write_io(WSS_CODEC, BusWidth::Byte, u32::from(0x40u8 | 0x08), false)
            .unwrap(); // MCE | index 8
        bus.write_io(WSS_DATA, BusWidth::Byte, 0x08, false).unwrap(); // CFS4, XTAL1 -> 0 Hz
        bus.write_io(WSS_CODEC, BusWidth::Byte, 0x08, false)
            .unwrap(); // clear MCE -> ACI
    });
    assert!(
        machine.wss.autocal_active(),
        "clearing MCE asserts the ACI autocal window"
    );
    assert_eq!(
        machine.wss.rate_hz(),
        0,
        "the selected rate is one of the unsupported (0 Hz) XTAL1 cells"
    );
    // Advance well past the 128-sample window at the 8000 Hz fallback cadence
    // (~16 ms; 200k clocks at 22 MHz is ~9 ms, so use a larger budget).
    machine.advance_devices_clocks(1_000_000);
    assert!(
        !machine.wss.autocal_active(),
        "the ACI window retires on the fallback converter clock despite rate 0"
    );
}

#[test]
fn wss_port_window_edges_and_config_region_decode_through_the_bus() {
    // Pin the wss_offset window math (`port.checked_sub(base).filter(|o| o < 8)`)
    // at its boundaries through the machine bus, plus the config-region readback
    // the decode comment promises does not overlap the SB16/mixer/OPL ranges:
    //   base+1 (0x531) -> IRQ7/DMA0 jumper byte 0x70,
    //   base+7 (0x537) -> decodes (Ok),
    //   base+8 (0x538) and base-1 (0x52F) -> Err(UnsupportedPort).
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        assert_eq!(
            bus.read_io(0x531, BusWidth::Byte, 0, false).unwrap(),
            0x70,
            "config region reads the IRQ7/DMA0 jumper byte"
        );
        assert!(
            bus.read_io(0x537, BusWidth::Byte, 0, false).is_ok(),
            "base+7 is the last decoded WSS port"
        );
        assert!(
            matches!(
                bus.read_io(0x538, BusWidth::Byte, 0, false),
                Err(BusError::UnsupportedPort { port }) if port == 0x538
            ),
            "base+8 is past the 8-port window"
        );
        assert!(
            matches!(
                bus.read_io(0x52F, BusWidth::Byte, 0, false),
                Err(BusError::UnsupportedPort { port }) if port == 0x52F
            ),
            "base-1 is below the window"
        );
    });
}

#[test]
fn dma_software_request_drives_a_mem_to_mem_block_copy() {
    // Program the 8237A through the ports for a memory-to-memory copy, then
    // arm it with a software DREQ on channel 0 (a write to the request
    // register) and confirm the destination block in guest memory matches the
    // source. The machine fires the burst on that request-register write.
    let mut machine = test_machine();
    const SRC: u32 = 0x1000;
    const DST: u32 = 0x1100;
    let src = [0xDE, 0xAD, 0xBE, 0xEFu8];
    for (i, &b) in src.iter().enumerate() {
        machine.write_physical_u8(SRC + i as u32, b);
    }
    with_bus(&mut machine, |bus| {
        // Channel 0 source address 0x1000, channel 1 dest address 0x1100.
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap(); // ch0 addr LSB
        bus.write_io(0x00, BusWidth::Byte, 0x10, false).unwrap(); // ch0 addr MSB -> 0x1000
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap(); // ch1 addr LSB
        bus.write_io(0x02, BusWidth::Byte, 0x11, false).unwrap(); // ch1 addr MSB -> 0x1100
        bus.write_io(0x03, BusWidth::Byte, 0x03, false).unwrap(); // ch1 count LSB
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap(); // ch1 count MSB -> 3 (4 bytes)
        bus.write_io(0x87, BusWidth::Byte, 0x00, false).unwrap(); // ch0 page 0
        bus.write_io(0x83, BusWidth::Byte, 0x00, false).unwrap(); // ch1 page 0
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap(); // unmask ch0 (the requester)
        bus.write_io(0x08, BusWidth::Byte, 0x01, false).unwrap(); // command: mem-to-mem enable
        // Arm the software DREQ on channel 0: bit2 set, channel bits 0-1 = 0.
        // This write triggers the block copy.
        bus.write_io(0x09, BusWidth::Byte, 0x04, false).unwrap();
    });
    for (i, &b) in src.iter().enumerate() {
        assert_eq!(
            machine.read_physical_u8(DST + i as u32),
            b,
            "dest byte {i} copied from the source block"
        );
    }
}

#[test]
fn dma_software_request_without_mem_to_mem_enable_does_nothing() {
    // The same request-register write, but with mem-to-mem disabled (command
    // bit0 clear), must not move any memory: the destination stays zero.
    let mut machine = test_machine();
    const SRC: u32 = 0x1000;
    const DST: u32 = 0x1100;
    for i in 0..4 {
        machine.write_physical_u8(SRC + i, 0xAB);
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0x00, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x00, BusWidth::Byte, 0x10, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x11, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x03, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x00, false).unwrap(); // unmask ch0
        bus.write_io(0x09, BusWidth::Byte, 0x04, false).unwrap(); // arm, but command bit0 not set
    });
    for i in 0..4 {
        assert_eq!(
            machine.read_physical_u8(DST + i),
            0x00,
            "no copy when mem-to-mem is disabled"
        );
    }
}

#[test]
fn machine_bus_snapshots_batch_entry_state() {
    // Run the machine forward a bit first so elapsed_clocks/vga_dots/beam/
    // bus_rem are not all trivially zero, then check that a freshly-built
    // MachineBus's five batch-entry snapshot fields equal the live machine
    // state at the moment the bus is constructed (P4a Slice 1 Task 1.1:
    // dev_docs/2026-07-02-p4a-lazy-port-device-time-plan.md). Nothing
    // consumes these fields yet; this only pins the wiring.
    let mut machine = test_machine();
    machine.run_cycles(5_000).unwrap();
    let expected_elapsed = machine.elapsed_clocks;
    let expected_vga_dots = machine.vga_dots;
    let expected_beam = machine.video.beam_dots();
    let expected_trace_elapsed = machine.trace.elapsed_clocks();
    let expected_bus_rem = machine.bus_rem;
    with_bus(&mut machine, |bus| {
        assert_eq!(
            bus.elapsed_clocks_at_batch_start, expected_elapsed,
            "elapsed_clocks_at_batch_start must mirror Machine::elapsed_clocks at construction"
        );
        assert_eq!(
            bus.vga_dots_at_batch_start, expected_vga_dots,
            "vga_dots_at_batch_start must mirror Machine::vga_dots at construction"
        );
        assert_eq!(
            bus.beam_at_batch_start, expected_beam,
            "beam_at_batch_start must mirror the VGA beam dot counter at construction"
        );
        assert_eq!(
            bus.trace_elapsed_at_batch_start, expected_trace_elapsed,
            "trace_elapsed_at_batch_start must mirror BusTrace::elapsed_clocks at construction"
        );
        assert_eq!(
            bus.bus_rem_at_batch_start, expected_bus_rem,
            "bus_rem_at_batch_start must mirror Machine::bus_rem at construction"
        );
    });
}

#[test]
fn predicted_beam_at_batch_start_equals_the_unmutated_beam() {
    // At core_clocks_so_far = 0 with zero in-batch bus clocks (the very first
    // instruction of a batch, before any fetch/data access has been recorded
    // into the trace this batch), the lazy formula must degenerate to exactly
    // the batch-entry beam: no in-batch advance has happened yet. This pins the
    // P4a Slice 1 peek's first-instruction safety argument as a test.
    let mut machine = test_machine();
    machine.run_cycles(5_000).unwrap();
    let expected_beam = machine.video.beam_dots();
    with_bus(&mut machine, |bus| {
        // core_clocks_so_far and prior_runs_core_clocks default to 0 (no
        // read_io call has run yet on this bus, no prior run this batch) and
        // trace.elapsed_clocks() at this instant equals
        // trace_elapsed_at_batch_start (nothing has been recorded since
        // construction), so in-batch clocks are zero on all terms.
        assert_eq!(
            bus.predicted_beam(),
            expected_beam,
            "zero in-batch clocks must predict exactly the batch-entry beam"
        );
    });
}

#[test]
fn predicted_beam_after_n_clocks_matches_a_real_advance_devices_of_the_same_n() {
    // Differential no-time-travel test: build two identically-driven machines
    // (the established pattern, see
    // predict_vga_dots_matches_the_real_advance_devices_accumulator_step). Run
    // both forward an odd cycle count first so vga_dots is fractional and
    // bus_rem is nonzero at batch entry (the Task 1.1 shape: vga_dots
    // ~0.4397, bus_rem 24 after 5000 cycles). Snapshot one into a MachineBus
    // and compute predicted_beam for a given in-batch clock total; call
    // advance_devices for real on the other with the same total (expressed in
    // the same core+scaled-bus units predicted_beam consumes) and assert the
    // beam positions agree exactly. The sweep covers: the trivial zero path,
    // small deltas inside one scanline, larger multi-scanline ones, a
    // 450_000-core case whose dot total exceeds the ~404k-dot frame so the
    // modulo wrap REALLY happens (asserted below via frames_completed), and
    // nonzero prior_runs_core_clocks values so the batch-scoped core term
    // (prior runs of the same batch) is exercised, not just the run-scoped
    // one. Task 1.3's lazy-read tests will drive the prior-runs seam
    // end-to-end through read_io; here the field is set directly, paired with
    // the batch-loop pin test below
    // (batch_loop_publishes_prior_runs_core_clocks_before_every_run).
    let mut any_wrap = false;
    for prior_runs_core_clocks in [0u64, 61, 33_000] {
        for core_clocks_so_far in [0u64, 100, 12_345, 450_000] {
            for fetch_count in [0u32, 1, 4_096] {
                let mut predicted_machine = test_machine();
                predicted_machine.run_cycles(5_000).unwrap();
                let mut real_machine = test_machine();
                real_machine.run_cycles(5_000).unwrap();
                assert_eq!(predicted_machine.vga_dots, real_machine.vga_dots);
                assert_eq!(predicted_machine.bus_rem, real_machine.bus_rem);
                assert_eq!(
                    predicted_machine.video.beam_dots(),
                    real_machine.video.beam_dots()
                );

                let (predicted, raw_bus_clocks) = with_bus(&mut predicted_machine, |bus| {
                    // Simulate fetch_count bytes' worth of bus traffic having
                    // been recorded into the trace since batch entry (prior
                    // instructions of this straight-line run, or this
                    // instruction's own fetch), at zero wait-states, then read
                    // back the actual raw clocks the trace charged for it so
                    // the real-machine side below combines the exact same
                    // total (not an assumed one) --
                    // record_instruction_fetch_run's per-byte cost is an
                    // internal BusCycle detail this test must not hardcode.
                    let before = bus.trace.elapsed_clocks();
                    if fetch_count > 0 {
                        bus.trace.record_instruction_fetch_run(0, fetch_count, 0);
                    }
                    let raw_bus_clocks = bus.trace.elapsed_clocks() - before;
                    bus.prior_runs_core_clocks = prior_runs_core_clocks;
                    bus.core_clocks_so_far = core_clocks_so_far;
                    (bus.predicted_beam(), raw_bus_clocks)
                });

                // The real batch-end step (run_until_clock / advance_devices):
                // core is the batch total (prior runs + the current run's
                // clocks), bus_clocks is what the trace recorded since batch
                // entry (mirrored here by raw_bus_clocks), scaled through
                // scale_bus's exact carry arithmetic.
                let step = prior_runs_core_clocks
                    + core_clocks_so_far
                    + real_machine.scale_bus(raw_bus_clocks);
                // Compute whether this step wraps the frame, from the same
                // pure formula, BEFORE the mutating advance: the prediction
                // only claims position, but the wrap cases must be shown to
                // really wrap (frames_completed bumps) or the coverage claim
                // above is hollow.
                let (whole_dots, _) = real_machine.predict_dots(step, real_machine.vga_dots);
                let frame = real_machine.video.frame_dots();
                let wraps = frame > 0 && real_machine.video.beam_dots() + whole_dots >= frame;
                let frames_before = real_machine.video.frames_completed();
                real_machine.advance_devices(step);

                assert_eq!(
                    predicted,
                    real_machine.video.beam_dots(),
                    "predicted_beam(prior={prior_runs_core_clocks}, \
                         core={core_clocks_so_far}, fetch_count={fetch_count}) must match a \
                         real advance_devices of the same core+scaled-bus clock total"
                );
                if wraps {
                    any_wrap = true;
                    assert!(
                        real_machine.video.frames_completed() > frames_before,
                        "a wrapping step must bump the real machine's frame counter \
                             (prior={prior_runs_core_clocks}, core={core_clocks_so_far}, \
                             fetch_count={fetch_count})"
                    );
                }
            }
        }
    }
    assert!(
        any_wrap,
        "the sweep must include at least one case that crosses a frame boundary, \
             or the wrap coverage this test claims is not exercised"
    );
}

#[test]
fn batch_loop_publishes_prior_runs_core_clocks_before_every_run() {
    // Pins the run_until_clock batch loop's prior_runs_core_clocks updates
    // through the cfg(test) push logs: before every run_straight_line call the
    // loop must republish the batch-scoped core accumulator (interrupt-service
    // charge + prior runs) into the bus, so a mid-run lazy prediction sees a
    // clock total that is monotone across run boundaries and bounded by the
    // core total the batch-end step later consumes. Nothing reads the field
    // from read_io yet (Task 1.3 wires that end-to-end); this pins the
    // loop-update mechanics directly: per batch, pushes are non-decreasing
    // prefix sums of the final batch core total, they reset at batch entry,
    // and real ROM execution produces multi-run batches where a later run
    // observes a NONZERO prior-runs value (the case the run-scoped
    // core_clocks_so_far alone would get wrong).
    let mut machine = test_machine();
    machine.run_cycles(300_000).unwrap();
    assert_eq!(
        machine.test_prior_core_pushes.len(),
        machine.test_batch_core_totals.len(),
        "one push log and one core total per completed batch"
    );
    assert!(
        !machine.test_prior_core_pushes.is_empty(),
        "the run must have executed at least one batch"
    );
    let mut saw_multi_run_nonzero_prior = false;
    for (batch, (pushes, total)) in machine
        .test_prior_core_pushes
        .iter()
        .zip(&machine.test_batch_core_totals)
        .enumerate()
    {
        let mut prev = 0u64;
        for &push in pushes {
            assert!(
                push >= prev,
                "batch {batch}: prior_runs_core_clocks pushes must be non-decreasing \
                     (a later run saw a smaller prior-core total: {push} after {prev})"
            );
            assert!(
                push <= *total,
                "batch {batch}: a push ({push}) exceeded the final batch core total \
                     ({total}) that fed the batch-end step"
            );
            prev = push;
        }
        if pushes.len() >= 2 && *pushes.last().unwrap() > 0 {
            saw_multi_run_nonzero_prior = true;
        }
    }
    assert!(
        saw_multi_run_nonzero_prior,
        "the boot run must contain at least one multi-run batch whose later run \
             saw a nonzero prior-runs core total; if this stops holding, drive the \
             machine differently rather than weakening the assert"
    );
}

#[test]
fn lazy_3da_read_does_not_set_io_touched_in_approximate_class_but_does_in_accurate() {
    // The P4a Task 1.3 behavior change: in the Approximate class (486/586) a
    // 0x3DA/0x3BA/0x3C2 read must NOT end the batch (io_touched stays false),
    // while the Accurate class (286/386) keeps the exact prior behavior
    // (io_touched set on every status-port read). Covers all three ports.
    for port in [0x3DAu16, 0x3BA, 0x3C2] {
        // Accurate class: unchanged behavior, io_touched set.
        let mut accurate = test_machine(); // Gsw386 by construction
        with_bus(&mut accurate, |bus| {
            let _ = bus.read_io(port, BusWidth::Byte, 0, false).unwrap();
            assert!(
                *bus.io_touched,
                "port {port:#06X}: the Accurate class must still set io_touched \
                     on a status-port read"
            );
        });

        // Approximate class: the new lazy behavior, io_touched stays false.
        let mut approximate = test_machine();
        approximate.set_mode(GswMode::Gsw486);
        with_bus(&mut approximate, |bus| {
            let _ = bus.read_io(port, BusWidth::Byte, 0, false).unwrap();
            assert!(
                !*bus.io_touched,
                "port {port:#06X}: the Approximate class must NOT set io_touched \
                     on a status-port read (the lazy path)"
            );
        });
    }
}

#[test]
fn ring0_monitor_port_access_does_not_set_io_touched_in_approximate_class() {
    // V86 trap tax, Part 1: a port access made by the ring-0 monitor
    // (cpu_is_ring0_pm = true, the TOKAEMM vec13 discriminator's PIC OCW3
    // probe being the motivating case) must NOT end the batch in the
    // Approximate class (486/586) -- the io_touched flag stays false on
    // both the read AND the write half of the OCW3 select-then-read idiom.
    // A guest (non-monitor) access to the same port keeps the old
    // unconditional-set behavior, both timing classes.
    let mut approximate = test_machine();
    approximate.set_mode(GswMode::Gsw486);
    with_bus(&mut approximate, |bus| {
        // OCW3: select ISR readback (0x0B) on the master PIC. Monitor access.
        bus.write_io(0x20, BusWidth::Byte, 0x0B, true).unwrap();
        assert!(
            !*bus.io_touched,
            "a ring-0-monitor OCW3 select write must NOT set io_touched \
                 in the Approximate class"
        );
        let _ = bus.read_io(0x20, BusWidth::Byte, 0, true).unwrap();
        assert!(
            !*bus.io_touched,
            "a ring-0-monitor PIC read must NOT set io_touched in the \
                 Approximate class"
        );
    });
}

#[test]
fn ring0_monitor_port_access_still_sets_io_touched_in_accurate_class() {
    // The Accurate class (286/386) keeps byte-identical batch semantics:
    // the ring-0-monitor exemption is Approximate-only, matching every
    // other P4a lazy gate in read_io/write_io.
    let mut accurate = test_machine(); // Gsw386 by construction
    with_bus(&mut accurate, |bus| {
        bus.write_io(0x20, BusWidth::Byte, 0x0B, true).unwrap();
        assert!(
            *bus.io_touched,
            "a ring-0-monitor OCW3 select write must still set io_touched \
                 in the Accurate class"
        );
        *bus.io_touched = false;
        let _ = bus.read_io(0x20, BusWidth::Byte, 0, true).unwrap();
        assert!(
            *bus.io_touched,
            "a ring-0-monitor PIC read must still set io_touched in the \
                 Accurate class"
        );
    });
}

#[test]
fn guest_port_access_still_sets_io_touched_regardless_of_ring0_pm_flag() {
    // A false cpu_is_ring0_pm (the ordinary guest/V86 case) must keep the
    // exact pre-Part-1 behavior in BOTH timing classes -- the exemption is
    // opt-in per access, never a global relaxation.
    for mode in [GswMode::Gsw386, GswMode::Gsw486] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        with_bus(&mut machine, |bus| {
            bus.write_io(0x20, BusWidth::Byte, 0x0B, false).unwrap();
            assert!(
                *bus.io_touched,
                "{mode:?}: a guest OCW3 select write must set io_touched"
            );
            *bus.io_touched = false;
            let _ = bus.read_io(0x20, BusWidth::Byte, 0, false).unwrap();
            assert!(
                *bus.io_touched,
                "{mode:?}: a guest PIC read must set io_touched"
            );
        });
    }
}

#[test]
fn ring0_monitor_wide_port_access_stays_lazy_across_byte_decomposition() {
    // The width != Byte decomposition path in both read_io and write_io
    // recurses per byte; cpu_is_ring0_pm must survive that recursion so a
    // (hypothetical) wide ring-0-monitor access stays exempt on every byte,
    // not just the first.
    let mut approximate = test_machine();
    approximate.set_mode(GswMode::Gsw486);
    with_bus(&mut approximate, |bus| {
        bus.write_io(0x20, BusWidth::Word, 0x0B0B, true).unwrap();
        assert!(
            !*bus.io_touched,
            "a wide ring-0-monitor write must NOT set io_touched in the \
                 Approximate class, on any decomposed byte"
        );
    });
}

#[test]
fn lazy_3da_read_still_resets_the_attribute_flip_flop_and_calls_catch_up() {
    // A lazy 0x3DA read must perform the exact same guest-visible side effects
    // as the non-lazy read (catch_up + the Attribute Controller address/data
    // flip-flop reset), even though io_touched stays false. `Attribute`'s
    // flip_flop_data field is pub(crate) to izarravm-video, not reachable
    // directly from this crate, so this observes the flip-flop indirectly
    // through 0x3C0's own read-back semantics: a first 0x3C0 write always sets
    // the index (armed as pending data); if the flip-flop is still "data"
    // after the 3DA read, a second 0x3C0 write would be consumed as a data
    // write to the FIRST index rather than a new index, and 0x3C0's own
    // read-back (`Some(attr.index | pas<<5)`) would show the stale value.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486); // Approximate class: the lazy path
    with_bus(&mut machine, |bus| {
        // ONE 0x3C0 write: consumed in the index phase (sets index = 0x05)
        // and leaves the flip-flop armed in the DATA phase. Exactly one
        // write, deliberately: `write_attr` toggles the flip-flop on EVERY
        // write, so a second "re-arm" write would itself consume the data
        // phase and put the flip-flop back at "index" regardless of whether
        // the 3DA reset fires -- which would make this test pass even with
        // the reset deleted (a mutation the spec review actually ran).
        bus.write_io(0x3C0, BusWidth::Byte, 0x05, false).unwrap();
        // Reading 0x3C0 returns `attr.index | pas << 5` and does NOT touch
        // the flip-flop, so this sanity check leaves the data phase armed.
        assert_eq!(
            bus.read_io(0x3C0, BusWidth::Byte, 0, false).unwrap(),
            0x05,
            "sanity: the index write took effect"
        );
        // The setup write above is an ordinary (non-lazy) port write and
        // unconditionally sets io_touched; clear it so the sanity check below
        // observes only the upcoming 3DA read's own effect on the flag.
        *bus.io_touched = false;

        // The lazy 3DA read: must reset the flip-flop to "index" despite not
        // setting io_touched.
        let _ = bus.read_io(0x3DA, BusWidth::Byte, 0, false).unwrap();
        assert!(
            !*bus.io_touched,
            "sanity: this is the lazy path (Approximate class)"
        );

        // A second 0x3C0 write with a DIFFERENT value. If the 3DA read reset
        // the flip-flop to "index", this is an index write (index becomes
        // 0x0A) and the read-back shows 0x0A. If the reset did NOT fire, the
        // flip-flop is still in the data phase, so this write lands as DATA
        // for the stale index 0x05 (palette[5] = 0x0A) and the read-back
        // still shows 0x05, failing the assertion. Mutation-verified: with
        // `flip_flop_data = false` deleted from status1_side_effects this
        // assertion fails; restored, it passes.
        bus.write_io(0x3C0, BusWidth::Byte, 0x0A, false).unwrap();
        assert_eq!(
            bus.read_io(0x3C0, BusWidth::Byte, 0, false).unwrap(),
            0x0A,
            "the 3DA read must have reset the attribute flip-flop to \"index\", \
                 so the next 0x3C0 write is treated as a new index (0x0A), not a \
                 data write to the stale index 0x05"
        );
    });
}

#[test]
fn lazy_3da_read_returns_the_same_bits_a_non_lazy_read_would_at_batch_start() {
    // At batch start (zero in-batch clocks, predicted_beam degenerates to the
    // batch-entry beam exactly, per
    // predicted_beam_at_batch_start_equals_the_unmutated_beam), the lazy
    // status1 bits must be byte-identical to what the pre-Task-1.3
    // read_status1 would have returned for the same live beam. Compared
    // within a SINGLE Approximate-class machine (not across two differently
    // clocked machines, whose beams would drift apart independently of this
    // task's change): clone the live Vga state before either read touches
    // it, compute the accurate read_status1() on the clone, then compute the
    // lazy value through the real bus, both starting from the identical
    // device state.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486); // Approximate class: the lazy path
    machine.run_cycles(5_000).unwrap();

    let mut accurate_clone = machine.video.clone();
    let expected = accurate_clone.read_status1();

    let (lazy_value, io_touched) = with_bus(&mut machine, |bus| {
        let value = bus.read_io(0x3DA, BusWidth::Byte, 0, false).unwrap();
        (value, *bus.io_touched)
    });

    assert!(
        !io_touched,
        "sanity: this is the lazy path (Approximate class)"
    );
    assert_eq!(
        lazy_value,
        u32::from(expected),
        "a lazy 3DA read at batch start must return byte-identical bits to a \
             non-lazy read of the same live beam"
    );
}

#[test]
fn lazy_reads_chain_into_far_fewer_batches_than_poll_iterations_with_monotone_observations() {
    // End-to-end no-time-travel test: a real mode-13h guest tightly polls
    // 0x3DA in a loop (the same port the P4d cadence test polls) and
    // maintains, in guest memory, a running sample count and a toggle count
    // of the vretrace bit (0x08) across every sample it has ever taken --
    // not just a bounded ring, so the toggle observation cannot be an
    // artifact of a capture window that happens to miss an edge. Asserts (a)
    // the Approximate-class run collapses many poll iterations into far
    // fewer `run_straight_line` calls (each 0x3DA IN no longer ends the
    // batch), and (b) the vretrace bit toggled at least once across the
    // whole run -- proving the lazy per-read prediction actually tracked
    // beam motion across many samples rather than reading a frozen value.
    //
    // Guest memory layout: [0x7000] sample count, [0x7004] toggle count,
    // [0x7006] last-observed vretrace bit (byte).
    let code = [
        0xB8, 0x13, 0x00, // 0: mov ax, 0x0013 (mode 13h)
        0xCD, 0x10, // 3: int 0x10
        0x31, 0xC0, // 5: xor ax, ax
        0x8E, 0xD8, // 7: mov ds, ax
        0xC7, 0x06, 0x00, 0x70, 0x00, 0x00, // 9: mov word [0x7000], 0 (sample count)
        0xC7, 0x06, 0x04, 0x70, 0x00, 0x00, // 15: mov word [0x7004], 0 (toggle count)
        0xC6, 0x06, 0x06, 0x70, 0xFF, // 21: mov byte [0x7006], 0xFF (no prior sample)
        0xBA, 0xDA, 0x03, // 26: mov dx, 0x03DA
        // poll (29): read status, isolate the vretrace bit, compare against
        // the last-observed bit, bump the toggle count on a change, stash
        // the new last-observed bit, bump the sample count, loop forever.
        0xEC, // 29: in al, dx
        0x24, 0x08, // 30: and al, 0x08 (isolate the vretrace bit)
        0x3A, 0x06, 0x06, 0x70, // 32: cmp al, [0x7006]
        0x74, 0x04, // 36: jz same (+4: skip the toggle bump)
        0xFF, 0x06, 0x04, 0x70, // 38: inc word [0x7004] (toggle count)
        // same (42):
        0xA2, 0x06, 0x70, // 42: mov [0x7006], al
        0xFF, 0x06, 0x00, 0x70, // 45: inc word [0x7000] (sample count)
        0xEB, 0xEA, // 49: jmp poll (displacement -22: poll=29, jmp ends at 51)
    ];
    let mut machine = Machine::new(
        MachineProfile::gsw_386(4, VideoCard::Et4000Ax),
        rom_with_code(&code),
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw486); // Approximate class: the lazy path

    // Warm up until mode 13h is set and the guest is inside the poll loop.
    machine.run_cycles(50_000).unwrap();
    machine.cpu.reset_perf_counters();
    let sample_count_before = machine.memory.read_u16(0x7000).unwrap();

    // Run enough guest clocks to complete several frames' worth of polling.
    let clock_hz = machine.active_mode().clock_hz();
    machine.run_cycles(clock_hz / 20).unwrap(); // 50ms of guest time

    // `straight_line_runs` counts every `run_straight_line` call (opening a
    // new run OR chaining continuations); each poll iteration is one IN, so
    // without continuation-chaining this would grow roughly 1:1 with the
    // sample count. With lazy reads admitted as continuations, many samples
    // land inside a single run.
    let runs = machine.cpu.perf_counters().straight_line_runs;
    let sample_count_after = machine.memory.read_u16(0x7000).unwrap();
    let samples_taken = sample_count_after.wrapping_sub(sample_count_before);
    let toggles = machine.memory.read_u16(0x7004).unwrap();

    assert!(
        samples_taken > 1000,
        "sanity: the poll loop must have run many iterations in 50ms of \
             guest time, saw {samples_taken}"
    );
    assert!(
        runs < u64::from(samples_taken) / 4,
        "lazy reads must chain many poll iterations per run_straight_line \
             call: saw {runs} runs for {samples_taken} samples (expected far \
             fewer runs than samples)"
    );
    assert!(
        toggles > 0,
        "the vretrace bit must have toggled at least once across the whole \
             run's samples in 50ms of guest time (multiple frames), or the lazy \
             prediction never actually tracked beam motion; saw {toggles} \
             toggles across {samples_taken} samples"
    );
    // Upper bound (spec-review hardening): a prediction jittering BACKWARD
    // across the vretrace edge would inflate the toggle count and still
    // satisfy toggles > 0, so bound it by the physically possible edge
    // count. Derivation: the measured window is 50ms of guest time; mode
    // 13h runs ~70 frames/s, so ~3.5 frames, and the vretrace bit toggles
    // exactly twice per frame (set at retrace start, clear at its end) =
    // ~7 toggles. Plus the 50_000-clock warm-up (< 1ms, at most one edge
    // pair -- the counter accumulates from boot) and +1 from the guest's
    // 0xFF last-bit sentinel mismatching the first real sample. Total
    // expected <= ~10; 20 leaves generous slack while still failing on any
    // per-read jitter (which would produce hundreds of spurious toggles
    // across >1000 samples).
    assert!(
        toggles < 20,
        "the vretrace bit toggled {toggles} times across {samples_taken} \
             samples in ~3.5 frames of guest time; more than ~2 per frame (+ \
             slack) means the lazy prediction is jittering back and forth \
             across the retrace edge instead of advancing monotonically"
    );
}

#[test]
fn lazy_read_after_an_interrupt_service_charge_sees_the_batch_scoped_total() {
    // Carried-forward review note: the first lazy read of a batch that opened
    // with an interrupt-service charge (the once-per-batch IRQ dispatch cost
    // added to batch_core before the first run_straight_line call) must see a
    // clock total that includes that charge -- prior_runs_core_clocks is
    // republished from batch_core before every run, and the very first
    // publish (before run 1) already carries the service charge. Observable
    // via the cfg(test) log seam: the FIRST prior-runs push of a batch that
    // serviced an interrupt must be nonzero.
    //
    // Reuses approximate_class_delivers_pit_irq0_during_long_compute_stretches'
    // exact setup (a pure `sti; jmp $` compute loop after arming the PIC/PIT
    // for ~3.43ms IRQ0 ticks) so an interrupt is serviced at a KNOWN, reliable
    // cadence rather than depending on incidental BIOS/POST timing.
    let code = [
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xD8, // mov ds, ax
        0xB0, 0x11, 0xE6, 0x20, // ICW1
        0xB0, 0x08, 0xE6, 0x21, // ICW2: vector base 0x08
        0xB0, 0x04, 0xE6, 0x21, // ICW3
        0xB0, 0x01, 0xE6, 0x21, // ICW4
        0xB0, 0x00, 0xE6, 0x21, // unmask all lines
        0xB0, 0x34, 0xE6, 0x43, // PIT ch0 mode 2, LSB/MSB
        0xB0, 0x00, 0xE6, 0x40, // reload low 0x00
        0xB0, 0x10, 0xE6, 0x40, // reload high 0x10 -> 4096 ticks (~3.43 ms)
        0xFB, // sti
        0xEB, 0xFE, // jmp $ (pure compute, no port I/O)
    ];
    let mut machine = Machine::new(
        MachineProfile::gsw_386(4, VideoCard::Et4000Ax),
        rom_with_code(&code),
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw486); // Approximate class: the lazy path
    // IRQ0 handler at 0x0700: mov al,0x20; out 0x20,al; iret (EOI only, no
    // guest-visible port I/O beyond that, which keeps the batch shape simple).
    let handler: [u8; 5] = [0xb0, 0x20, 0xe6, 0x20, 0xcf];
    for (i, &b) in handler.iter().enumerate() {
        machine.write_physical_u8(0x0700 + i as u32, b);
    }
    // IVT[0x08] (IRQ0 at PIC base 0x08) -> 0000:0700.
    machine.write_physical_u8(0x20, 0x00);
    machine.write_physical_u8(0x21, 0x07);
    machine.write_physical_u8(0x22, 0x00);
    machine.write_physical_u8(0x23, 0x00);
    // A few periods of 4096 PIT ticks at the Gsw486 clock rate, comfortably
    // enough for several IRQ0 edges (and thus several interrupt-opened
    // batches) to land.
    machine.run_cycles(5_000_000).unwrap();

    assert!(
        !machine.test_prior_core_pushes.is_empty(),
        "the run must have executed at least one batch"
    );
    let saw_batch_with_serviced_interrupt_charge = machine
        .test_prior_core_pushes
        .iter()
        .any(|pushes| pushes.first().is_some_and(|&first| first > 0));
    assert!(
        saw_batch_with_serviced_interrupt_charge,
        "at least one batch's FIRST prior-runs publish (before its first \
             run_straight_line call) must be nonzero, proving an interrupt- \
             service charge from batch entry is visible to the first lazy read \
             of that batch's first run, not just to later runs"
    );
}

#[test]
fn lazy_61_read_does_not_set_io_touched_in_approximate_class_but_does_in_accurate() {
    // The P4a Task 2.3 behavior change, mirroring the 3DA/3BA/3C2 case: in
    // the Approximate class (486/586) a port 0x61 read must NOT end the
    // batch (io_touched stays false), while the Accurate class (286/386)
    // keeps the exact prior behavior (io_touched set).
    let mut accurate = test_machine(); // Gsw386 by construction
    with_bus(&mut accurate, |bus| {
        let _ = bus.read_io(0x61, BusWidth::Byte, 0, false).unwrap();
        assert!(
            *bus.io_touched,
            "the Accurate class must still set io_touched on a port 0x61 read"
        );
    });

    let mut approximate = test_machine();
    approximate.set_mode(GswMode::Gsw486);
    with_bus(&mut approximate, |bus| {
        let _ = bus.read_io(0x61, BusWidth::Byte, 0, false).unwrap();
        assert!(
            !*bus.io_touched,
            "the Approximate class must NOT set io_touched on a port 0x61 \
                 read (the lazy path)"
        );
    });
}

#[test]
fn lazy_61_read_returns_the_same_bits_a_non_lazy_read_would_at_batch_start() {
    // At batch start (zero in-batch clocks, predicted_pit_out degenerates to
    // the batch-entry live channel_out exactly, the PIT counterpart of
    // predicted_beam_at_batch_start_equals_the_unmutated_beam), the lazy 0x61
    // byte must be byte-identical to what the pre-Task-2.3 read would have
    // returned for the same live PIT/speaker state.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486); // Approximate class: the lazy path
    machine.run_cycles(5_000).unwrap();

    let expected = (machine.speaker.control_bits() & 0x03)
        | (u8::from(machine.pit.channel_out(1)) << 4)
        | (u8::from(machine.pit.channel_out(2)) << 5);

    let (lazy_value, io_touched) = with_bus(&mut machine, |bus| {
        let value = bus.read_io(0x61, BusWidth::Byte, 0, false).unwrap();
        (value, *bus.io_touched)
    });

    assert!(
        !io_touched,
        "sanity: this is the lazy path (Approximate class)"
    );
    assert_eq!(
        lazy_value,
        u32::from(expected),
        "the lazy 0x61 byte must equal the non-lazy read at batch start \
             (zero in-batch clocks)"
    );
}

#[test]
fn predicted_pit_out_after_n_clocks_matches_a_real_advance_devices_of_the_same_n() {
    // Differential no-time-travel test, the PIT counterpart of
    // predicted_beam_after_n_clocks_matches_a_real_advance_devices_of_the_same_n:
    // build two identically-driven machines, snapshot one into a MachineBus,
    // compute predicted_pit_out for a given in-batch clock total, and call
    // advance_devices for real on the other with the same total (expressed
    // in the same core+scaled-bus units) -- the two must agree exactly.
    // Mode-2 (channel 1, the AT refresh timer, pre-seeded at power-on) and
    // mode-3 (channel 2, PC speaker) channels are both covered, including
    // totals crossing several OUT edges, so the sweep exercises both this
    // slice's channels at both the periods the real machine actually uses.
    for prior_runs_core_clocks in [0u64, 61, 33_000] {
        for core_clocks_so_far in [0u64, 100, 12_345, 450_000] {
            for channel in [1usize, 2] {
                let mut predicted_machine = test_machine();
                predicted_machine.set_mode(GswMode::Gsw486);
                if channel == 2 {
                    // Arm channel 2 in mode 3 (square wave) with a short
                    // divisor so several OUT edges land inside the swept
                    // clock range; GATE2 comes from port 0x61 bit 0.
                    with_bus(&mut predicted_machine, |bus| {
                        bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap(); // ch2, lo/hi, mode 3
                        bus.write_io(0x42, BusWidth::Byte, 0x10, false).unwrap(); // divisor low
                        bus.write_io(0x42, BusWidth::Byte, 0x00, false).unwrap(); // divisor high (16)
                        bus.write_io(0x61, BusWidth::Byte, 0x03, false).unwrap(); // GATE2 + data
                    });
                }
                predicted_machine.run_cycles(5_000).unwrap();
                let mut real_machine = test_machine();
                real_machine.set_mode(GswMode::Gsw486);
                if channel == 2 {
                    with_bus(&mut real_machine, |bus| {
                        bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap();
                        bus.write_io(0x42, BusWidth::Byte, 0x10, false).unwrap();
                        bus.write_io(0x42, BusWidth::Byte, 0x00, false).unwrap();
                        bus.write_io(0x61, BusWidth::Byte, 0x03, false).unwrap();
                    });
                }
                real_machine.run_cycles(5_000).unwrap();
                assert_eq!(predicted_machine.pit_clocks, real_machine.pit_clocks);
                assert_eq!(
                    predicted_machine.pit.channel_out(channel),
                    real_machine.pit.channel_out(channel)
                );

                let (predicted, raw_bus_clocks) = with_bus(&mut predicted_machine, |bus| {
                    let before = bus.trace.elapsed_clocks();
                    if core_clocks_so_far > 0 {
                        // A cheap stand-in for real bus traffic: any nonzero
                        // fetch count exercises the scaled-bus term the same
                        // way predicted_beam's twin test does.
                        bus.trace.record_instruction_fetch_run(0, 1, 0);
                    }
                    let raw_bus_clocks = bus.trace.elapsed_clocks() - before;
                    bus.prior_runs_core_clocks = prior_runs_core_clocks;
                    bus.core_clocks_so_far = core_clocks_so_far;
                    (bus.predicted_pit_out(channel), raw_bus_clocks)
                });

                let step = prior_runs_core_clocks
                    + core_clocks_so_far
                    + real_machine.scale_bus(raw_bus_clocks);
                real_machine.advance_devices(step);

                assert_eq!(
                    predicted,
                    Some(real_machine.pit.channel_out(channel)),
                    "predicted_pit_out(channel={channel}, prior={prior_runs_core_clocks}, \
                         core={core_clocks_so_far}) must match a real advance_devices \
                         of the same core+scaled-bus clock total"
                );
            }
        }
    }
}

#[test]
fn lazy_61_read_falls_back_to_the_non_lazy_path_for_a_bcd_counter() {
    // BCD fallback (P4a Task 2.3): out_after conservatively declines for a
    // BCD-programmed counter, so the lazy 0x61 arm must fall all the way
    // back to the exact non-lazy path -- io_touched set, today's live read
    // -- rather than a second implementation of the bit composition.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486); // Approximate class: the lazy path would
    // otherwise apply.
    with_bus(&mut machine, |bus| {
        // Program channel 1 as BCD, mode 2: SC=01, RW=11, mode=010, BCD=1.
        bus.write_io(0x43, BusWidth::Byte, 0x75, false).unwrap();
        bus.write_io(0x41, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x41, BusWidth::Byte, 0x01, false).unwrap();
        *bus.io_touched = false; // clear the setup writes' own effect

        let expected = (bus.speaker.control_bits() & 0x03)
            | (u8::from(bus.pit.channel_out(1)) << 4)
            | (u8::from(bus.pit.channel_out(2)) << 5);
        let value = bus.read_io(0x61, BusWidth::Byte, 0, false).unwrap();
        assert!(
            *bus.io_touched,
            "a BCD-programmed channel must fall back to the non-lazy path, \
                 which sets io_touched"
        );
        assert_eq!(
            value,
            u32::from(expected),
            "the BCD fallback must return exactly today's live read"
        );
    });
}

#[test]
fn lazy_pit_conversion_honors_the_batch_entry_fractional_carry() {
    // Carry-pinning differential (the Slice 2 review's FIX 2): the sweep test
    // above passes even with `elapsed_pit_clocks`' carry zeroed, because its
    // (T, carry) pairs rarely land where the carry decides the floor. This
    // test CONSTRUCTS such a pair: seed the fractional accumulator near 1.0
    // on both machines, pick an elapsed-PIT-clock count `k` sitting exactly
    // on a channel-2 OUT toggle edge, then pick a T whose product crosses
    // the k-th integer only WITH the carry (floor(carry + T*rate) == k but
    // floor(0 + T*rate) == k-1). The lazy byte's bit 5 then flips iff the
    // carry is honored. Mutation-verified: with `elapsed_pit_clocks` passing
    // 0.0 instead of pit_clocks_at_batch_start this fails; restored, passes.
    let carry = 0.999_f64;
    let mut predicted_machine = test_machine();
    predicted_machine.set_mode(GswMode::Gsw486); // Approximate: the lazy path
    let mut real_machine = test_machine();
    real_machine.set_mode(GswMode::Gsw486);
    for machine in [&mut predicted_machine, &mut real_machine] {
        with_bus(machine, |bus| {
            // Channel 2, mode 3 (square wave), divisor 16: OUT toggles every
            // 8 PIT input clocks, so toggle edges are dense in the probe
            // range. GATE2 + data enable via port 0x61 bits 0/1.
            bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap();
            bus.write_io(0x42, BusWidth::Byte, 0x10, false).unwrap();
            bus.write_io(0x42, BusWidth::Byte, 0x00, false).unwrap();
            bus.write_io(0x61, BusWidth::Byte, 0x03, false).unwrap();
        });
        machine.run_cycles(5_000).unwrap();
        machine.pit_clocks = carry; // the deliberate batch-entry carry seed
    }
    assert_eq!(predicted_machine.pit, real_machine.pit, "identical drive");

    // The smallest elapsed-PIT-clock count sitting on an OUT toggle edge.
    let k = (1..=64u64)
        .find(|&k| {
            predicted_machine.pit.out_after(2, k) != predicted_machine.pit.out_after(2, k - 1)
        })
        .expect("a mode-3 divisor-16 counter toggles within 64 input clocks");
    let out_with_carry = predicted_machine.pit.out_after(2, k).unwrap();
    let out_without_carry = predicted_machine.pit.out_after(2, k - 1).unwrap();
    assert_ne!(out_with_carry, out_without_carry, "k is a toggle edge");

    // A core-clock total T whose elapsed-PIT-clock floor lands on k only
    // WITH the seeded carry, computed with the exact shared formula.
    let rate = predicted_machine.timing.pit_per_clock;
    let t = (1..=200_000u64)
        .find(|&t| {
            advance_fractional(carry, t, rate).0 == k && advance_fractional(0.0, t, rate).0 == k - 1
        })
        .expect("a carry-deciding T exists (the carry spans ~55 core clocks at 486)");

    let (lazy_value, lazy_elapsed, io_touched) = with_bus(&mut predicted_machine, |bus| {
        bus.core_clocks_so_far = t;
        let elapsed = bus.elapsed_pit_clocks();
        let value = bus.read_io(0x61, BusWidth::Byte, t, false).unwrap();
        (value as u8, elapsed, *bus.io_touched)
    });
    assert!(!io_touched, "sanity: the lazy path");
    assert_eq!(
        lazy_elapsed, k,
        "the lazy conversion must honor the batch-entry carry: elapsed must \
             be k (carry crosses the integer), not k-1 (carry dropped)"
    );
    assert_eq!(
        (lazy_value >> 5) & 1,
        u8::from(out_with_carry),
        "bit 5 must be the OUT level at k (carry honored), which differs \
             from the level at k-1 (carry dropped)"
    );

    // The ground truth: a real advance_devices of the same T, then the
    // non-lazy composition, must agree with the lazy byte bit for bit.
    real_machine.advance_devices(t);
    let real_value = (real_machine.speaker.control_bits() & 0x03)
        | (u8::from(real_machine.pit.channel_out(1)) << 4)
        | (u8::from(real_machine.pit.channel_out(2)) << 5);
    assert_eq!(
        lazy_value, real_value,
        "lazy at T == a real advance_devices(T) then read, on the \
             carry-deciding (T, carry) pair"
    );
}

#[test]
fn opl_status_read_sets_io_touched_in_every_cpu_mode() {
    // AdLib detection is a timer probe, so every OPL status read must end
    // the current CPU batch even in approximate 486/586 modes. Covers every
    // alias `opl_port` maps to a status read: the native 0x388/0x38A and the
    // SB16 mirrors 0x220/0x222/0x228.
    let status_ports = [0x388u16, 0x38a, 0x220, 0x222, 0x228];

    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        for &port in &status_ports {
            with_bus(&mut machine, |bus| {
                *bus.io_touched = false;
                let _ = bus.read_io(port, BusWidth::Byte, 0, false).unwrap();
                assert!(
                    *bus.io_touched,
                    "mode {mode:?}, port {port:#06X}: OPL status reads \
                         must set io_touched"
                );
            });
        }
    }
}

#[test]
fn opl_status_read_returns_the_live_status_byte_in_approximate_mode() {
    // 486/586 still use exact OPL status reads. Pin the byte value as well
    // as the batch-ending behavior on an active timer.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486);
    with_bus(&mut machine, |bus| {
        bus.write_io(0x388, BusWidth::Byte, 0x04, false).unwrap(); // latch reg 0x04
        bus.write_io(0x389, BusWidth::Byte, 0x80, false).unwrap(); // reset IRQ flags
        bus.write_io(0x388, BusWidth::Byte, 0x02, false).unwrap(); // latch reg 0x02
        bus.write_io(0x389, BusWidth::Byte, 0xff, false).unwrap(); // timer 1 preset
        bus.write_io(0x388, BusWidth::Byte, 0x04, false).unwrap(); // latch reg 0x04
        bus.write_io(0x389, BusWidth::Byte, 0x01, false).unwrap(); // start timer 1
    });
    machine.run_cycles(5_000).unwrap();

    let expected = machine.opl.status();

    let (lazy_value, io_touched) = with_bus(&mut machine, |bus| {
        let value = bus.read_io(0x388, BusWidth::Byte, 0, false).unwrap();
        (value, *bus.io_touched)
    });

    assert!(
        io_touched,
        "486-mode OPL status reads must stay batch-ending"
    );
    assert_eq!(
        lazy_value,
        u32::from(expected),
        "the OPL status byte must equal the live device status"
    );
}

#[test]
fn adlib_detection_idiom_ends_the_batch_on_status_reads() {
    // The AdLib detection idiom is one address-port write followed by
    // status-port polling. Both the write and the reads must end the CPU
    // batch so the OPL timers advance between polls in approximate modes.
    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw486);

    with_bus(&mut machine, |bus| {
        *bus.io_touched = false;
        let _ = bus.write_io(0x388, BusWidth::Byte, 0x04, false); // address write
        assert!(
            *bus.io_touched,
            "the address-port write must still set io_touched (writes \
                 stay batch-ending)"
        );

        for _ in 0..6 {
            *bus.io_touched = false;
            let _ = bus.read_io(0x388, BusWidth::Byte, 0, false).unwrap();
            assert!(
                *bus.io_touched,
                "status-port reads must set io_touched in the \
                     Approximate class"
            );
        }
    });
}

#[test]
fn opl_status_poll_charges_isa_bus_time_only_in_approximate_class() {
    // A fast CPU retires a tight IN loop so quickly that the 80 us OPL timer
    // AdLib detection waits on never overflows, so Doom disables FM music. The
    // fix charges each OPL status read one ISA bus period (~1 us), folded into
    // the batch's device advance, so the poll cannot outrun the timer. The
    // Approximate class (486/586) accrues it; the Accurate class (286/386) must
    // not, keeping its byte-identical batch cadence (its slower clock already
    // spans the window).
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        machine.isa_io_batch_clocks = 0;
        with_bus(&mut machine, |bus| {
            let _ = bus.read_io(0x388, BusWidth::Byte, 0, false).unwrap();
        });
        let expected = (mode.clock_hz() / 1_000_000).max(1);
        assert_eq!(
            machine.isa_io_batch_clocks, expected,
            "{mode:?}: one OPL status poll charges one ISA bus period"
        );
    }

    let mut machine = test_machine();
    machine.set_mode(GswMode::Gsw386);
    machine.isa_io_batch_clocks = 0;
    with_bus(&mut machine, |bus| {
        let _ = bus.read_io(0x388, BusWidth::Byte, 0, false).unwrap();
    });
    assert_eq!(
        machine.isa_io_batch_clocks, 0,
        "the Accurate class must not charge ISA I/O time (byte-identical cadence)"
    );
}

// Run one closure against a freshly-borrowed bus over the whole machine.
fn with_bus<R>(machine: &mut Machine, f: impl FnOnce(&mut MachineBus) -> R) -> R {
    // Captured before the struct literal below since video/trace are also
    // mutably borrowed by other fields in that same literal.
    let beam_at_batch_start = machine.video.beam_dots();
    let trace_elapsed_at_batch_start = machine.trace.elapsed_clocks();
    let (bus_num_at_batch_start, bus_den_at_batch_start) = bus_timing(machine.cpu.level());
    let mut bus = MachineBus {
        memory: &mut machine.memory,
        ram_lookup: &mut machine.ram_lookup,
        video: &mut machine.video,
        margo: &mut machine.margo,
        distira: &mut machine.distira,
        pci: &mut machine.pci,
        rom: &machine.rom,
        serial: &mut machine.serial,
        serial2: &mut machine.serial2,
        lpt: &mut machine.lpt,
        lpt2: &mut machine.lpt2,
        device_ports: &mut machine.device_ports,
        pic: &mut machine.pic,
        pit: &mut machine.pit,
        keyboard: &mut machine.keyboard,
        speaker: &mut machine.speaker,
        rtc: &mut machine.rtc,
        dma: &mut machine.dma,
        fdc: &mut machine.fdc,
        floppy: &mut machine.floppy,
        opl: &mut machine.opl,
        dsp: &mut machine.dsp,
        mixer: &mut machine.mixer,
        wss: &mut machine.wss,
        wss_base: machine.wss_base,
        wss_enabled: machine.wss_enabled,
        ide: &mut machine.ide,
        ata: &mut machine.ata,
        trace: &mut machine.trace,
        pending_soft_int: &mut machine.pending_soft_int,
        last_int_vector: &mut machine.last_int_vector,
        active_mode: machine.active_mode,
        pending_mode: &mut machine.pending_mode,
        fast_post: machine.fast_post,
        booter_inert: machine.booter_inert,
        program_runtime: machine.program_runtime,
        pending_toka_service: &mut machine.pending_toka_service,
        toka_service_status: machine.toka_service_status,
        unittester: &mut machine.unittester,
        wait_states: machine.profile.wait_states,
        cache: &mut machine.cache_model,
        flat_data_cost: matches!(machine.active_mode.timing_class(), TimingClass::Approximate),
        lazy_port_reads: matches!(machine.active_mode.timing_class(), TimingClass::Approximate),
        io_touched: &mut machine.io_touched,
        isa_io_clocks: &mut machine.isa_io_batch_clocks,
        device_wrote_memory: &mut machine.device_wrote_memory,
        direct_map_changed: &mut machine.direct_map_changed,
        core_clocks_so_far: 0,
        prior_runs_core_clocks: 0,
        elapsed_clocks_at_batch_start: machine.elapsed_clocks,
        vga_dots_at_batch_start: machine.vga_dots,
        beam_at_batch_start,
        trace_elapsed_at_batch_start,
        bus_rem_at_batch_start: machine.bus_rem,
        inv_clock_at_batch_start: machine.timing.inv_clock,
        bus_num_at_batch_start,
        bus_den_at_batch_start,
        pit_clocks_at_batch_start: machine.pit_clocks,
        pit_per_clock_at_batch_start: machine.timing.pit_per_clock,
    };
    f(&mut bus)
}

#[test]
fn instruction_fetch_run_fast_path_stops_at_the_video_aperture() {
    // Pins the `end < 0xA0000` guard in charge_instruction_fetch_run: a run whose
    // last byte is 0x9FFFF takes the conventional-RAM fast path (one collapsed
    // I-cache access at the per-mode code-fetch constant), while a run straddling
    // 0xA0000 must fall through to the full classification, which sees the VGA
    // window's wait-states, goes non-uniform, and charges per byte.
    use izarravm_bus::BusCycle;
    let mut machine = test_machine();
    // Preconditions for the straddle case: the A0000 window decodes as a device
    // window, and its wait-states differ from the code-fetch constant (otherwise
    // the uniform arm legitimately collapses the run and the paths are
    // charge-identical by design).
    assert!(machine.video.video_memory_enabled());
    let code_ws = machine.cache_model.code_fetch_wait_states();
    let video_ws = machine.profile.wait_states.video;
    assert_ne!(
        code_ws, video_ws,
        "test needs distinct RAM/video wait-states"
    );

    with_bus(&mut machine, |bus| {
        // Fast path: 4 bytes ending exactly at 0x9FFFF -> one I-cache access.
        let before = bus.trace.elapsed_clocks();
        bus.charge_instruction_fetch_run(0x0009_FFFC, 4).unwrap();
        assert_eq!(
            bus.trace.elapsed_clocks() - before,
            u64::from(BusCycle::clocks_for(BusWidth::Byte, code_ws)),
            "run ending at 0x9FFFF charges a single I-cache access"
        );
        // Slow path: 4 bytes straddling 0xA0000 -> non-uniform (RAM then VGA
        // window), charged per byte: two at the code-fetch constant, two at
        // the video cost.
        let before = bus.trace.elapsed_clocks();
        bus.charge_instruction_fetch_run(0x0009_FFFE, 4).unwrap();
        assert_eq!(
            bus.trace.elapsed_clocks() - before,
            2 * u64::from(BusCycle::clocks_for(BusWidth::Byte, code_ws))
                + 2 * u64::from(BusCycle::clocks_for(BusWidth::Byte, video_ws)),
            "run straddling 0xA0000 keeps the per-byte classification"
        );
    });
}

#[test]
fn ram_lookup_rebuilds_when_distira_bar_moves_over_ram() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(24, VideoCard::Distira),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    const RAM_ADDR: u32 = 0x0100_0000;
    machine.memory.write_u8(RAM_ADDR as usize, 0x5a).unwrap();

    with_bus(&mut machine, |bus| {
        assert!(
            bus.direct_page(RAM_ADDR, BusAccessKind::DataRead)
                .unwrap()
                .is_some(),
            "extended RAM starts direct"
        );
        let config_addr = 0x8000_0000 | (u32::from(DISTIRA_PCI_SLOT) << 11) | 0x10;
        bus.write_io(PCI_CONFIG_ADDRESS_PORT, BusWidth::Dword, config_addr, false)
            .unwrap();
        bus.write_io(PCI_CONFIG_DATA_PORT, BusWidth::Dword, RAM_ADDR, false)
            .unwrap();
        assert!(
            bus.direct_page(RAM_ADDR, BusAccessKind::DataRead)
                .unwrap()
                .is_none(),
            "Distira BAR overlap removes the direct page"
        );
        assert!(
            *bus.direct_map_changed,
            "BAR relocation marks CPU direct caches stale"
        );
        bus.write_memory(RAM_ADDR, BusWidth::Byte, 0xa5, BusAccessKind::DataWrite)
            .unwrap();
    });

    assert_eq!(
        machine.memory.read_u8(RAM_ADDR as usize).unwrap(),
        0x5a,
        "Distira BAR relocation must invalidate direct-RAM lookup entries"
    );
}

#[test]
fn direct_memory_helpers_accept_only_page_local_ram() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(24, VideoCard::Et4000Ax),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    machine.memory.write_u32(0x2000, 0x1234_5678).unwrap();

    with_bus(&mut machine, |bus| {
        let read_page = bus
            .direct_page(0x2000, BusAccessKind::DataRead)
            .unwrap()
            .expect("ordinary RAM page is direct");
        assert_eq!(read_page.physical_page, 0x2000);
        assert_eq!(read_page.len, RAM_LOOKUP_PAGE_SIZE);
        assert!(!read_page.ptr.is_null());
        assert!(!read_page.writable, "read lookup is not a write grant");
        assert!(
            bus.direct_page(0x2000, BusAccessKind::DataWrite)
                .unwrap()
                .expect("ordinary RAM write page is direct")
                .writable,
            "write lookup grants writes"
        );
        let ram = bus
            .read_memory_direct(0x2000, BusWidth::Dword, BusAccessKind::DataRead)
            .unwrap();
        assert!(ram.direct, "ordinary RAM is direct");
        assert_eq!(ram.value, 0x1234_5678);
        assert!(
            bus.write_memory_direct(
                0x2004,
                BusWidth::Dword,
                0xDEAD_BEEF,
                BusAccessKind::DataWrite
            )
            .unwrap()
            .direct,
            "ordinary RAM writes are direct"
        );
        assert_eq!(
            bus.direct_memory_bytes(0x2ff0, 16, BusWidth::Byte),
            16,
            "same-page RAM span is direct"
        );

        assert_eq!(
            bus.direct_memory_bytes(0x2fff, 2, BusWidth::Byte),
            0,
            "cross-page spans fall back"
        );
        assert_eq!(
            bus.direct_memory_bytes(0x2001, 2, BusWidth::Word),
            0,
            "split word spans fall back"
        );
        assert!(
            !bus.read_memory_direct(LOW_BIOS_BASE, BusWidth::Dword, BusAccessKind::DataRead)
                .unwrap()
                .direct,
            "ROM falls back"
        );
        assert!(
            bus.direct_page(LOW_BIOS_BASE, BusAccessKind::InstructionPrefetch)
                .unwrap()
                .is_none(),
            "ROM has no direct page"
        );
        assert!(
            !bus.write_memory_direct(
                VGA_TEXT_BASE,
                BusWidth::Byte,
                b'X'.into(),
                BusAccessKind::DataWrite
            )
            .unwrap()
            .direct,
            "VGA memory falls back"
        );
        assert!(
            bus.direct_page(VGA_TEXT_BASE, BusAccessKind::DataWrite)
                .unwrap()
                .is_none(),
            "VGA memory has no direct page"
        );
        assert_eq!(
            bus.direct_memory_bytes(0x0E_0000, 4, BusWidth::Dword),
            0,
            "upper-memory window falls back"
        );
        assert!(
            bus.direct_page(0x0E_0000, BusAccessKind::DataRead)
                .unwrap()
                .is_none(),
            "the upper-memory window has no direct page"
        );
    });

    machine.keyboard.set_a20(false);
    with_bus(&mut machine, |bus| {
        assert!(
            !bus.read_memory_direct(0x10_0000, BusWidth::Byte, BusAccessKind::DataRead)
                .unwrap()
                .direct,
            "A20-folded accesses fall back"
        );
        assert!(
            bus.direct_page(0x10_0000, BusAccessKind::DataRead)
                .unwrap()
                .is_none(),
            "A20-folded pages are not direct"
        );
    });
}

#[test]
fn ram_lookup_does_not_expose_partial_final_pages_as_full_pages() {
    let pci = PciConfig::new(false);
    let lookup = RamPageLookup::new(RAM_LOOKUP_PAGE_SIZE + 17, &pci);
    assert!(lookup.direct_bytes(0, RAM_LOOKUP_PAGE_SIZE).is_some());
    assert!(
        lookup
            .direct_bytes(RAM_LOOKUP_PAGE_SIZE as u32, RAM_LOOKUP_PAGE_SIZE)
            .is_none(),
        "a final partial page cannot back a full direct-page pointer"
    );
}

// Profiling probe for the RAM page lookup. Not a correctness test; run with:
// cargo test --release -p izarravm-machine ram_lookup_profile -- --ignored --nocapture
#[test]
#[ignore]
fn ram_lookup_profile() {
    let iters = std::env::var("IZARRAVM_PROFILE_ITERS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(5_000_000);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(24, VideoCard::Et4000Ax),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();

    for i in 0..1024u32 {
        machine.write_physical_u8(0x2000 + i, i as u8);
        machine.write_physical_u8(0x10_0000 + i, i as u8);
    }

    fn report(label: &str, iters: u32, mut body: impl FnMut(u32) -> u32) -> u32 {
        let t = std::time::Instant::now();
        let mut checksum = 0u32;
        for i in 0..iters {
            checksum = checksum
                .wrapping_add(std::hint::black_box(body(i)).rotate_left(i & 31))
                .wrapping_add(i);
        }
        let secs = t.elapsed().as_secs_f64();
        let ns = secs * 1.0e9 / f64::from(iters);
        println!(
            "{label:<32} {ns:>8.2} ns/op  {:>8.1} Mops/s  checksum={checksum:#010x}",
            f64::from(iters) / secs / 1.0e6
        );
        checksum
    }

    with_bus(&mut machine, |bus| {
        println!("ram_lookup_profile: {iters} iterations");
        assert!(bus.direct_ram_bytes(0x10_0000, 4).is_some());
        assert!(bus.direct_ram_bytes(LOW_BIOS_BASE, 4).is_none());

        let low = report("lookup low RAM", iters, |i| {
            let (start, _) = bus.direct_ram_bytes(0x2000 + ((i & 0xff) << 2), 4).unwrap();
            start as u32
        });
        let high = report("lookup extended RAM", iters, |i| {
            let (start, _) = bus
                .direct_ram_bytes(0x10_0000 + ((i & 0xff) << 2), 4)
                .unwrap();
            start as u32
        });
        let slow = report("lookup ROM miss", iters, |i| {
            u32::from(
                bus.direct_ram_bytes(LOW_BIOS_BASE + ((i & 0xff) << 2), 4)
                    .is_some(),
            )
        });
        let read_low = report("bus read low RAM", iters, |i| {
            bus.read_memory(
                0x2000 + ((i & 0xff) << 2),
                BusWidth::Dword,
                BusAccessKind::DataRead,
            )
            .unwrap()
        });
        let read_high = report("bus read extended RAM", iters, |i| {
            bus.read_memory(
                0x10_0000 + ((i & 0xff) << 2),
                BusWidth::Dword,
                BusAccessKind::DataRead,
            )
            .unwrap()
        });
        let read_rom = report("bus read ROM", iters, |i| {
            bus.read_memory(
                LOW_BIOS_BASE + ((i & 0xff) << 2),
                BusWidth::Dword,
                BusAccessKind::DataRead,
            )
            .unwrap()
        });

        std::hint::black_box((low, high, slow, read_low, read_high, read_rom));
    });
}

#[test]
fn rtc_ports_round_trip_through_the_bus() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x70, BusWidth::Byte, 0x00, false).unwrap(); // select seconds
        bus.write_io(0x71, BusWidth::Byte, 42, false).unwrap();
        bus.write_io(0x70, BusWidth::Byte, 0x00, false).unwrap();
        let secs = bus.read_io(0x70 + 1, BusWidth::Byte, 0, false).unwrap();
        assert_eq!(secs, 42);
    });
}

#[test]
fn rtc_advances_seconds_on_the_machine_clock() {
    let mut machine = test_machine();
    machine.seed_rtc(2026, 6, 20, 6, 12, 0, 0);
    // Step roughly three seconds of emulated time, in ~10 ms chunks so the
    // sub-second accumulator carries the way it does during a real run.
    let clock_hz = machine.profile.clock_hz;
    let chunk = clock_hz / 100; // ~10 ms
    for _ in 0..300 {
        machine.advance_devices_clocks(chunk);
    }
    let bytes = machine.cmos_bytes();
    // Seconds register (0x00) should have advanced to about 3.
    assert!(
        (2..=4).contains(&bytes[0x00]),
        "expected the seconds register near 3, got {}",
        bytes[0x00]
    );
}

#[test]
fn cmos_persists_and_reloads_via_bytes() {
    let mut machine = test_machine();
    // Guest writes a layout byte and a boot-order byte, then refreshes the
    // checksum the way the setup page would.
    with_bus(&mut machine, |bus| {
        bus.write_io(0x70, BusWidth::Byte, 0x10, false).unwrap();
        bus.write_io(0x71, BusWidth::Byte, 3, false).unwrap(); // FR layout
        bus.write_io(0x70, BusWidth::Byte, 0x11, false).unwrap();
        bus.write_io(0x71, BusWidth::Byte, 1, false).unwrap(); // disk-first
    });
    assert!(
        machine.take_cmos_dirty(),
        "an NVRAM write should mark dirty"
    );
    let saved = machine.cmos_bytes();

    // A fresh machine loads the saved image and reads the same bytes back.
    let mut other = test_machine();
    other.load_cmos(&saved);
    assert_eq!(other.cmos_bytes()[0x10], 3);
    assert_eq!(other.cmos_bytes()[0x11], 1);
}

#[test]
fn pc_speaker_renders_a_square_wave() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap(); // ch2, lo/hi, mode 3
        bus.write_io(0x42, BusWidth::Byte, 0x00, false).unwrap(); // divisor low
        bus.write_io(0x42, BusWidth::Byte, 0x04, false).unwrap(); // divisor high (0x0400)
        bus.write_io(0x61, BusWidth::Byte, 0x03, false).unwrap(); // GATE2 + data enable
    });
    let clock_hz = machine.profile.clock_hz;
    let chunk = clock_hz / 100_000; // ~10 us, mimicking per-instruction advance
    for _ in 0..2_000 {
        machine.advance_devices_clocks(chunk); // ~20 ms total
    }
    let pcm = machine.render_audio(OPL_NATIVE_HZ as usize / 50);
    assert!(
        pcm.iter().any(|&(l, _)| l > 0) && pcm.iter().any(|&(l, _)| l < 0),
        "a toggling speaker tone should produce both polarities"
    );
}

#[test]
fn pc_speaker_ultrasonic_square_wave_averages_quietly() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap(); // ch2, lo/hi, mode 3
        bus.write_io(0x42, BusWidth::Byte, 0x02, false).unwrap(); // divisor low
        bus.write_io(0x42, BusWidth::Byte, 0x00, false).unwrap(); // divisor high
        bus.write_io(0x61, BusWidth::Byte, 0x03, false).unwrap(); // GATE2 + data enable
    });
    let clock_hz = machine.profile.clock_hz;
    let chunk = clock_hz / 100_000; // ~10 us, mimicking per-instruction advance
    for _ in 0..2_000 {
        machine.advance_devices_clocks(chunk); // ~20 ms total
    }
    let pcm = machine.render_audio(OPL_NATIVE_HZ as usize / 50);
    let peak = pcm
        .iter()
        .map(|&(l, r)| i32::from(l).abs().max(i32::from(r).abs()))
        .max()
        .unwrap_or(0);
    assert!(
        peak < 1_200,
        "an ultrasonic PIT2 square wave should average down instead of aliasing at full scale, peak {peak}"
    );
}

#[test]
fn port_61_reports_out_gate_enable_and_refresh() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0xb6, false).unwrap();
        bus.write_io(0x42, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x42, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x61, BusWidth::Byte, 0x03, false).unwrap();
    });
    let clock_hz = machine.profile.clock_hz;
    machine.advance_devices_clocks(clock_hz / 100_000); // ~10 us
    let b = with_bus(&mut machine, |bus| {
        bus.read_io(0x61, BusWidth::Byte, 0, false).unwrap() as u8
    });
    assert_eq!(
        (b >> 5) & 1,
        u8::from(machine.pit.channel_out(2)),
        "bit 5 = ch2 OUT"
    );
    assert_eq!(b & 0x03, 0x03, "bits 0,1 read back GATE2 + data enable");

    // Bit 4 is now PIT channel 1 OUT (the AT DRAM-refresh timer, mode 2),
    // pre-seeded at power-on. This guest never programmed channel 1, yet the
    // bit must still toggle. Mode 2 pulses OUT low for one input clock per
    // refresh period, so over a couple of periods sampled finely bit 4 reads
    // both high (the bulk) and low (the short pulse).
    let mut saw_high = false;
    let mut saw_low = false;
    // Advance one PIT input clock at a time; one CPU step worth of clocks is
    // clock_hz / PIT_INPUT_HZ, so step that to move roughly one PIT tick.
    let per_pit_clock = (clock_hz / u64::from(PIT_INPUT_HZ)).max(1);
    for _ in 0..40 {
        machine.advance_devices_clocks(per_pit_clock);
        let bit4 = with_bus(&mut machine, |bus| {
            (bus.read_io(0x61, BusWidth::Byte, 0, false).unwrap() as u8 >> 4) & 1
        });
        if bit4 == 1 {
            saw_high = true;
        } else {
            saw_low = true;
        }
    }
    assert!(
        saw_high,
        "refresh bit (4) reads high for the bulk of a period"
    );
    assert!(
        saw_low,
        "refresh bit (4) pulses low once per refresh period"
    );
}

// Program channel 0 as a keyed sine tone through the given OPL address/data
// port pair (so the same routine can drive the native and aliased ports).
fn program_tone(bus: &mut MachineBus, addr: u16, data: u16) {
    let mut write = |reg: u8, value: u8| {
        bus.write_io(addr, BusWidth::Byte, u32::from(reg), false)
            .unwrap();
        bus.write_io(data, BusWidth::Byte, u32::from(value), false)
            .unwrap();
    };
    write(0x20, 0x01); // modulator: multiple x1
    write(0x40, 0x3f); // modulator muted
    write(0x60, 0xf0); // modulator instant attack
    write(0x80, 0x00);
    write(0x23, 0x21); // carrier: sustained, multiple x1
    write(0x43, 0x00); // carrier loud
    write(0x63, 0xf0); // carrier instant attack
    write(0x83, 0x00);
    write(0xc0, 0x01); // additive
    write(0xa0, 0x00); // f-number low
    write(0xb0, 0x20 | (4 << 2) | 0x02); // key-on, block 4, fnum 0x200
}

fn boot_image_with(code: &[u8]) -> Vec<u8> {
    let mut image = vec![0; BOOT_IMAGE_SIZE];
    image[..code.len()].copy_from_slice(code);
    image[510] = 0x55;
    image[511] = 0xaa;
    image
}

// SP-4b M0 Task 2 (increment 1): the standalone V86 spike boots, enters V86 via
// the real-mode -> PM+paging -> IRETD-into-V86 transition, and the V86 stub signals
// exit code 0xA5 through the unit-tester port. Proves the transition in isolation.
#[test]
fn v86spike_enters_v86_and_signals() {
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        boot_image_with(izarravm_firmware::V86SPIKE_BIN),
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(50_000_000).unwrap();
    assert_eq!(
        reason,
        StopReason::TestExit { code: 0xA5 },
        "V86 spike did not reach the V86 stub and signal (stop={reason:?})"
    );
}

#[test]
fn hlt_wakes_on_pit_timer_tick() {
    // Boot code: init the PIC, unmask IRQ0, program channel 0 (mode 3, count 1000),
    // install IVT[8] -> a handler that bumps [0x0500] and EOIs, sti, hlt, then
    // cli, hlt. The run loop must fast-forward to the IRQ0 edge and wake the CPU.
    // The count is large enough that the handler finishes long before the next
    // tick, so the cli after hlt runs and the program reaches a genuine halt.
    let code: &[u8] = &[
        0xb0, 0x11, 0xe6, 0x20, 0xb0, 0x08, 0xe6, 0x21, 0xb0, 0x04, 0xe6, 0x21, 0xb0, 0x01, 0xe6,
        0x21, 0xb0, 0xfe, 0xe6, 0x21, 0xb0, 0x36, 0xe6, 0x43, 0xb0, 0xe8, 0xe6, 0x40, 0xb0, 0x03,
        0xe6, 0x40, 0xc7, 0x06, 0x20, 0x00, 0x30, 0x7c, 0xc7, 0x06, 0x22, 0x00, 0x00, 0x00, 0xfb,
        0xf4, 0xfa, 0xf4, 0xff, 0x06, 0x00, 0x05, 0xb0, 0x20, 0xe6, 0x20, 0xcf,
    ];
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        boot_image_with(code),
    )
    .unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    let tick = u16::from_le_bytes([
        machine.memory().as_slice()[0x0500],
        machine.memory().as_slice()[0x0501],
    ]);
    assert_eq!(tick, 1, "the IRQ0 handler should have run once");
    // One tick is about 1000 PIT clocks, near 18000 CPU clocks at 22 MHz, so a
    // real fast-forward clears this slack floor while a no-op halt would not.
    assert!(
        machine.elapsed_clocks() > 10_000,
        "the fast-forward should have advanced emulated time across the tick interval"
    );
}

#[test]
fn boot_suite_reports_timer_irq0_pass() {
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::X86_BOOT_TEST_IMAGE,
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);

    let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
    assert!(
        results.records.iter().any(|record| {
            record.status == izarravm_firmware::SuiteRecordStatus::Pass
                && record.name == "timer.irq0"
        }),
        "boot suite should report PASS timer.irq0"
    );
    // The timer idle genuinely advanced emulated time (ten ticks of ~11932
    // input clocks each), not spun instantly.
    assert!(machine.elapsed_clocks() > 1_500_000);
}

#[test]
fn boot_suite_reports_sb_dsp_reset_pass() {
    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = Machine::new_boot_image(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::X86_BOOT_TEST_IMAGE,
        )
        .unwrap();
        machine.set_mode(mode);
        let reason = machine
            .run_until_halt_or_cycles(mode.clock_hz() / 4)
            .unwrap();
        assert_eq!(reason, StopReason::Halted);
        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        assert!(
            results.records.iter().any(|record| {
                record.status == izarravm_firmware::SuiteRecordStatus::Pass
                    && record.name == "sound.sb_dsp_reset"
            }),
            "boot suite should report PASS sound.sb_dsp_reset in {mode:?}"
        );
    }
}

#[test]
fn boot_suite_reports_opl3_pass() {
    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = Machine::new_boot_image(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::X86_BOOT_TEST_IMAGE,
        )
        .unwrap();
        machine.set_mode(mode);
        let reason = machine
            .run_until_halt_or_cycles(mode.clock_hz() / 4)
            .unwrap();
        assert_eq!(reason, StopReason::Halted);
        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        assert!(
            results.records.iter().any(|record| {
                record.status == izarravm_firmware::SuiteRecordStatus::Pass
                    && record.name == "sound.opl3"
            }),
            "boot suite should report PASS sound.opl3 in {mode:?}"
        );
    }
}

#[test]
fn boot_suite_reports_opl2_pass() {
    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = Machine::new_boot_image(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::X86_BOOT_TEST_IMAGE,
        )
        .unwrap();
        machine.set_mode(mode);
        let reason = machine
            .run_until_halt_or_cycles(mode.clock_hz() / 4)
            .unwrap();
        assert_eq!(reason, StopReason::Halted);
        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        assert!(
            results.records.iter().any(|record| {
                record.status == izarravm_firmware::SuiteRecordStatus::Pass
                    && record.name == "sound.opl2"
            }),
            "boot suite should report PASS sound.opl2 in {mode:?}"
        );
    }
}

#[test]
fn boot_suite_reports_sb_8bit_dma_pass() {
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::X86_BOOT_TEST_IMAGE,
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
    assert!(
        results.records.iter().any(|record| {
            record.status == izarravm_firmware::SuiteRecordStatus::Pass
                && record.name == "sound.sb_8bit_dma"
        }),
        "boot suite should report PASS sound.sb_8bit_dma (clock-driven single-cycle DMA + IRQ5)"
    );
}

#[test]
fn boot_suite_reports_sb_16bit_dma_pass() {
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::X86_BOOT_TEST_IMAGE,
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
    assert!(
        results.records.iter().any(|record| {
            record.status == izarravm_firmware::SuiteRecordStatus::Pass
                && record.name == "sound.sb_16bit_dma"
        }),
        "boot suite should report PASS sound.sb_16bit_dma (clock-driven auto-init DMA + IRQ5)"
    );
}

#[test]
fn sb_dma_irq5_wakes_a_halted_cpu_via_fast_forward() {
    // A guest arms 8-bit single-cycle DMA + IRQ5, then `sti;hlt`. The run loop
    // must fast-forward across the DSP sample window (the new IRQ5 wake) and
    // deliver the half-buffer IRQ5, so the handler runs and real emulated time
    // advances -- not a genuine no-wake halt. Setup mirrors the 8-bit probe.
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        // mov ax,0; mov ds,ax; sti; hlt; cli; hlt
        rom_with_code(&[0xb8, 0x00, 0x00, 0x8e, 0xd8, 0xfb, 0xf4, 0xfa, 0xf4]),
    )
    .unwrap();
    // 16-byte unsigned ramp at 0x01_0000 (DMA page 0x01, byte addr 0).
    for (i, b) in (0..16u8).map(|i| i * 16).enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }
    // IRQ5 handler at 0x0700: inc word [0x0610]; mov al,0x20; out 0x20,al; iret.
    let handler: [u8; 9] = [0xff, 0x06, 0x10, 0x06, 0xb0, 0x20, 0xe6, 0x20, 0xcf];
    for (i, &b) in handler.iter().enumerate() {
        machine.write_physical_u8(0x0700 + i as u32, b);
    }
    // IVT[0x0D] -> 0x0000:0x0700; clear the tick counter.
    machine.write_physical_u8(0x34, 0x00);
    machine.write_physical_u8(0x35, 0x07);
    machine.write_physical_u8(0x36, 0x00);
    machine.write_physical_u8(0x37, 0x00);
    machine.write_physical_u8(0x0610, 0x00);
    machine.write_physical_u8(0x0611, 0x00);
    with_bus(&mut machine, |bus| {
        // PIC base 0x08 (ICW1..ICW4) so IRQ5 -> vector 0x0D; all IRQs unmasked.
        bus.write_io(0x20, BusWidth::Byte, 0x11, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x08, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x04, false).unwrap();
        bus.write_io(0x21, BusWidth::Byte, 0x01, false).unwrap();
        // DMA ch1: page 0x01, byte addr 0, count 15 (16 bytes), single read.
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        // DSP: 11025 Hz, block 16, single-cycle 8-bit DMA output.
        for &b in &[0x41u8, 0x2B, 0x11, 0x14, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    let reason = machine.run_until_halt_or_cycles(5_000_000).unwrap();
    // The handler ran (after the cli the second hlt is genuine).
    assert_eq!(reason, StopReason::Halted);
    let ticks = u16::from(machine.read_physical_u8(0x0610))
        | (u16::from(machine.read_physical_u8(0x0611)) << 8);
    assert!(ticks >= 1, "the IRQ5 handler should have run");
    // The fast-forward crossed a real sample window (half-buffer at 8 samples
    // ~= 16k CPU clocks at 22 MHz), not a no-op halt.
    assert!(
        machine.elapsed_clocks() > 15_000,
        "the fast-forward should advance emulated time across the DSP sample window"
    );
}

#[test]
fn sb16_creative_adpcm_decodes_over_dma_and_raises_irq5() {
    // End-to-end SB16 Creative ADPCM: a guest arms 4-bit ADPCM-with-reference
    // (DSP command 0x75) over 8-bit DMA channel 1, the clock-driven producer
    // pulls the encoded bytes, decodes them through the DSP, and raises the
    // 8-bit IRQ (IRQ5, the mixer default) at terminal count. Exercises the
    // real DMA -> DSP decode -> PIC path, not just the codec in isolation.
    let mut machine = test_machine();
    // 16 encoded DMA bytes at 0x01_0000 (page 0x01): a reference seed (0x80)
    // followed by 15 non-zero code bytes so the decoded output is audible.
    machine.write_physical_u8(0x1_0000, 0x80);
    for i in 1..16u32 {
        machine.write_physical_u8(0x1_0000 + i, 0x50);
    }
    with_bus(&mut machine, |bus| {
        // DMA ch1: page 0x01, byte addr 0, count 15 (16 bytes), single read.
        bus.write_io(0x0B, BusWidth::Byte, 0x49, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x0F, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        // DSP: 11025 Hz, then 0x75 (4-bit ADPCM + reference), length 0x000F ->
        // 16 encoded bytes (the block counts DMA bytes, reference included).
        for &b in &[0x41u8, 0x2B, 0x11, 0x75, 0x0F, 0x00] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    // Drain the whole 16-byte block at 11025 Hz (~2.9 ms); 400k clocks spans it.
    machine.advance_devices_clocks(400_000);
    // The terminal-count IRQ latched on the SB16's default line (IRQ5).
    assert!(
        machine.pic.irr_bit(5),
        "Creative ADPCM block raised the 8-bit IRQ5 at terminal count"
    );
    // Single-cycle playback stopped at the end of the block.
    assert!(!machine.dsp.is_playing(), "single-cycle ADPCM halted at TC");
    // The decoder produced audible (non-silent) frames on the DSP ring: the
    // reference byte seeded 0x80 and the 0x50 code bytes moved it off center.
    let mut audible = false;
    while let Some((l, _)) = machine.dsp.drain_frame() {
        if l != 0 {
            audible = true;
        }
    }
    assert!(audible, "decoded ADPCM is audible, not flat silence");
}

#[test]
fn cli_hlt_is_a_genuine_halt() {
    // With interrupts off, HLT must still halt immediately, not spin.
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        rom_with_code(&[0xfa, 0xf4]), // cli; hlt
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
}

#[test]
fn pit_channel0_raises_irq0_while_running() {
    // cli; jmp $ keeps the CPU spinning with interrupts off, so advance_devices
    // ticks the PIT but the raised IRQ0 stays pending (never acknowledged).
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        rom_with_code(&[0xfa, 0xeb, 0xfe]),
    )
    .unwrap();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x43, BusWidth::Byte, 0x36, false).unwrap(); // counter 0, mode 3
        bus.write_io(0x40, BusWidth::Byte, 0x04, false).unwrap(); // count low
        bus.write_io(0x40, BusWidth::Byte, 0x00, false).unwrap(); // count high -> 4
    });
    machine.run_cycles(4000).unwrap();
    let pending = with_bus(&mut machine, |bus| bus.interrupt_pending());
    assert!(
        pending,
        "channel 0 should have raised IRQ0 over 4000 cycles"
    );
}

// Throughput probe for the run-loop batching (item 2.3). Not a correctness
// test; run with: cargo test --release -- --ignored --nocapture batch_throughput
#[test]
#[ignore]
fn batch_throughput() {
    // cli; jmp $ — a tight interrupt-free loop with no port I/O, the case the
    // batch fully amortizes (one bus build + device fan-out per ~thousands of
    // instructions instead of per instruction).
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        rom_with_code(&[0xfa, 0xeb, 0xfe]),
    )
    .unwrap();
    let budget = 2_000_000_000u64;
    let t = std::time::Instant::now();
    machine.run_cycles(budget).unwrap();
    let secs = t.elapsed().as_secs_f64();
    println!(
        "batch_throughput: {budget} guest clocks in {secs:.3}s = {:.1} M guest-clocks/s",
        budget as f64 / secs / 1.0e6
    );
}

#[test]
fn audio_sample_cap_is_one_dac_sample_and_never_zero() {
    // The run-loop batch services devices once per cap clocks; the cap must be
    // exactly one 44.1 kHz DAC sample so the DSP/CD producers never alias, and
    // never 0 (which would stall the batch). Checked at the live 200 MHz
    // default and a pathologically slow clock where the floor division would
    // otherwise be 0.
    assert_eq!(
        TimingFactors::for_clock(200_000_000).clocks_per_audio_sample,
        200_000_000 / u64::from(DAC_HZ)
    );
    assert_eq!(
        TimingFactors::for_clock(40_000).clocks_per_audio_sample,
        1,
        "a clock below the DAC rate must floor to 1, not 0"
    );
}

#[test]
fn fdc_read_data_streams_a_sector_into_memory_over_dma_channel_2() {
    // A guest that programs the FDC directly: arm DMA channel 2 for a
    // device->memory write, then issue READ DATA. The sector bytes must land
    // in the guest buffer through the channel-2 datapath, and the controller
    // must raise IRQ6 and present its result phase.
    let mut machine = test_machine();

    // 720 KB image (9 sectors/track, 2 heads). Seed CHS(2,0,3) with a marker.
    // LBA = (cyl*heads + head)*spt + (sector-1) = (2*2+0)*9 + 2 = 38.
    let mut img = vec![0u8; 737_280];
    // LBA for CHS(2,0,3) on a 9-spt, 2-head disk: (2*2 + 0)*9 + (3-1) = 38.
    let lba = 38usize;
    let off = lba * 512;
    for (i, slot) in img[off..off + 512].iter_mut().enumerate() {
        *slot = (0xA0 + (i & 0x0F)) as u8;
    }
    machine.mount_floppy(img).unwrap();

    // Guest DMA target buffer at physical 0x2000 (512 bytes).
    const BUF: u16 = 0x2000;

    with_bus(&mut machine, |bus| {
        // --- Program DMA channel 2: device->memory (write), single, count 512.
        bus.write_io(0x0B, BusWidth::Byte, 0x46, false).unwrap(); // mode ch2: single, write
        bus.write_io(0x0C, BusWidth::Byte, 0x00, false).unwrap(); // clear the flip-flop
        bus.write_io(0x04, BusWidth::Byte, u32::from(BUF & 0xFF), false)
            .unwrap();
        bus.write_io(0x04, BusWidth::Byte, u32::from(BUF >> 8), false)
            .unwrap();
        bus.write_io(0x81, BusWidth::Byte, 0x00, false).unwrap(); // page (A16-A23) = 0
        bus.write_io(0x05, BusWidth::Byte, 0xFF, false).unwrap(); // count low (511)
        bus.write_io(0x05, BusWidth::Byte, 0x01, false).unwrap(); // count high -> 0x01FF
        bus.write_io(0x0A, BusWidth::Byte, 0x02, false).unwrap(); // unmask channel 2

        // --- Drive the FDC.
        bus.write_io(0x3F2, BusWidth::Byte, 0x1C, false).unwrap(); // DOR: motor A, gate, out of reset, drive 0
        bus.write_io(0x3F5, BusWidth::Byte, 0x08, false).unwrap(); // SENSE INT (clear power-up irq)
        while bus.read_io(0x3F4, BusWidth::Byte, 0, false).unwrap() & 0x40 != 0 {
            bus.read_io(0x3F5, BusWidth::Byte, 0, false).unwrap();
        }
        // READ DATA: HDS+DS=0, C=2, H=0, R=3, N=2(512), EOT=3, GPL=0x1B, DTL=0xFF.
        for &b in &[0xE6u8, 0x00, 0x02, 0x00, 0x03, 0x02, 0x03, 0x1B, 0xFF] {
            bus.write_io(0x3F5, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });

    // The sector landed in the guest buffer over channel 2.
    for i in 0..512usize {
        let got = machine.read_physical_u8(u32::from(BUF) + i as u32);
        let want = (0xA0 + (i & 0x0F)) as u8;
        assert_eq!(got, want, "byte {i} of the sector in memory");
    }

    // The disk->memory DMA transfer flagged a device memory write, so the run loop will tell
    // the CPU to drop its prefetch + decode cache (the staged bytes could be re-entered by a
    // near branch that would not otherwise invalidate). The flag->invalidation step itself is
    // covered end-to-end by a20_toggle_through_the_run_loop, which shares the seam.
    assert!(
        machine.device_wrote_memory,
        "an FDC disk->memory DMA transfer must flag a device memory write"
    );

    // The completion interrupt is IRQ6 (the controller raised it; advance the
    // device pump so the bus collects it into the PIC).
    machine.advance_devices(1);
    let pending = with_bus(&mut machine, |bus| bus.interrupt_pending());
    assert!(pending, "FDC completion raised IRQ6");

    // The result phase is seven status bytes ending at sector 3.
    let result = with_bus(&mut machine, |bus| {
        let mut out = Vec::new();
        while bus.read_io(0x3F4, BusWidth::Byte, 0, false).unwrap() & 0x40 != 0 {
            out.push(bus.read_io(0x3F5, BusWidth::Byte, 0, false).unwrap() as u8);
        }
        out
    });
    assert_eq!(result.len(), 7, "ST0..N result phase");
    assert_eq!(result[0] & 0xC0, 0x00, "normal termination");
    assert_eq!(result[3], 2, "ending cylinder 2");
    assert_eq!(result[5], 3, "ending sector 3");
}

#[test]
fn pic_command_and_data_ports_route_to_the_master() {
    let mut machine = test_machine();
    let mask = with_bus(&mut machine, |bus| {
        // ICW1..ICW4 init, then OCW1 sets the mask to a recognizable value.
        for (port, value) in [
            (0x20u16, 0x11u32),
            (0x21, 0x08),
            (0x21, 0x04),
            (0x21, 0x01),
            (0x21, 0xab),
        ] {
            bus.write_io(port, BusWidth::Byte, value, false).unwrap();
        }
        // The data port reads back the mask, not the passive 0xff stub.
        bus.read_io(0x21, BusWidth::Byte, 0, false).unwrap()
    });
    assert_eq!(mask, 0xab);
}

#[test]
fn machine_bus_acknowledges_a_pic_interrupt() {
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        for (port, value) in [(0x20u16, 0x11u32), (0x21, 0x08), (0x21, 0x04), (0x21, 0x01)] {
            bus.write_io(port, BusWidth::Byte, value, false).unwrap();
        }
    });
    machine.request_irq(0);

    let (pending, vector) = with_bus(&mut machine, |bus| {
        (bus.interrupt_pending(), bus.acknowledge_interrupt())
    });
    assert!(pending);
    assert_eq!(vector, Some(0x08));
}

#[test]
fn opl_sounds_through_the_adlib_ports() {
    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        with_bus(&mut machine, |bus| program_tone(bus, 0x0388, 0x0389));
        let pcm = machine.render_audio(2000);
        assert!(
            pcm.iter().any(|&(l, _)| l != 0),
            "the OPL should produce audio via the AdLib ports in {mode:?}"
        );
    }
}

#[test]
fn opl_sounds_through_the_sound_blaster_aliases() {
    // 0x220/0x221 mirror the OPL3 primary-bank address/data ports.
    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine = test_machine();
        machine.set_mode(mode);
        with_bus(&mut machine, |bus| program_tone(bus, 0x0220, 0x0221));
        let pcm = machine.render_audio(2000);
        assert!(
            pcm.iter().any(|&(l, _)| l != 0),
            "the OPL should produce audio via the SB base aliases in {mode:?}"
        );
    }
}

/// Build a CD image with one data sector and a stretch of loud audio frames,
/// for the CD-audio mixing test.
fn audio_cd(frames: u32) -> CdImage {
    let cue = "TRACK 01 MODE1/2048\nINDEX 01 00:00:00\n\
                   TRACK 02 AUDIO\nINDEX 01 00:00:01\n";
    let mut bin = vec![0u8; cdimage::DATA_SECTOR + frames as usize * cdimage::RAW_SECTOR];
    // Fill the audio region with a loud constant so the mix is clearly nonzero.
    for chunk in bin[cdimage::DATA_SECTOR..].chunks_exact_mut(2) {
        chunk.copy_from_slice(&8000i16.to_le_bytes());
    }
    CdImage::from_cue(cue, bin).unwrap()
}

fn iso_dir_record(lba: u32, len: u32, flags: u8, name: &[u8]) -> Vec<u8> {
    let pad = usize::from(name.len() % 2 == 0);
    let mut record = vec![0u8; 33 + name.len() + pad];
    record[0] = record.len() as u8;
    record[2..6].copy_from_slice(&lba.to_le_bytes());
    record[6..10].copy_from_slice(&lba.to_be_bytes());
    record[10..14].copy_from_slice(&len.to_le_bytes());
    record[14..18].copy_from_slice(&len.to_be_bytes());
    record[18..25].copy_from_slice(&[126, 1, 1, 0, 0, 0, 0]);
    record[25] = flags;
    record[28..30].copy_from_slice(&1u16.to_le_bytes());
    record[30..32].copy_from_slice(&1u16.to_be_bytes());
    record[32] = name.len() as u8;
    record[33..33 + name.len()].copy_from_slice(name);
    record
}

#[test]
fn play_audio_mixes_cd_audio_into_render_audio() {
    let mut machine = test_machine();
    machine.mount_cd(audio_cd(20));
    // Open the CD volume to full (5-bit registers 0x36/0x37) via the mixer.
    with_bus(&mut machine, |bus| {
        for (index, value) in [(0x36u32, 31u32), (0x37, 31)] {
            bus.write_io(0x224, BusWidth::Byte, index, false).unwrap();
            bus.write_io(0x225, BusWidth::Byte, value, false).unwrap();
        }
    });
    // Issue PLAY AUDIO(10) over the secondary-channel ATAPI ports: PACKET
    // command, then the 12-byte CDB. Play from LBA 1 (audio start) for 16
    // frames.
    with_bus(&mut machine, |bus| {
        bus.write_io(0x177, BusWidth::Byte, 0xA0, false).unwrap(); // PACKET command
        let mut cdb = [0u8; 12];
        cdb[0] = 0x45; // PLAY AUDIO(10)
        cdb[5] = 1; // starting LBA 1
        cdb[8] = 16; // 16 frames
        for b in cdb {
            bus.write_io(0x170, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    assert!(machine.cd_loaded());
    let pcm = machine.render_audio(2000);
    assert!(
        pcm.iter().any(|&(l, r)| l != 0 || r != 0),
        "PLAY AUDIO should mix nonzero CD audio into the DAC output"
    );
}

#[test]
fn cd_audio_is_silent_with_the_volume_muted() {
    let mut machine = test_machine();
    machine.mount_cd(audio_cd(20));
    // Leave CD volume at its muted default (0). Start playback.
    with_bus(&mut machine, |bus| {
        bus.write_io(0x177, BusWidth::Byte, 0xA0, false).unwrap();
        let mut cdb = [0u8; 12];
        cdb[0] = 0x45;
        cdb[5] = 1;
        cdb[8] = 16;
        for b in cdb {
            bus.write_io(0x170, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    let pcm = machine.render_audio(2000);
    assert!(
        pcm.iter().all(|&(l, r)| l == 0 && r == 0),
        "a muted CD volume yields silence even while playing"
    );
}

#[test]
fn icdex_install_check_reports_installed() {
    let mut machine = test_machine();
    // The probe pushes DADAh, then the INT pushed IP, CS, FLAGS over it, so
    // the marker sits at SS:SP+6. Stand in for that frame here.
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Ss, SegmentRegister::real(0x9000));
    machine.cpu.registers.set_esp(0x0100);
    let marker_addr = 0x9000 * 16 + (0x0100 + 6);
    machine.memory.write_u16(marker_addr, 0xDADA).unwrap();
    machine.cpu.registers.set_eax(0x1100);
    assert!(machine.handle_int2f());
    // AL = FFh means installed.
    assert_eq!(machine.cpu.registers.eax() as u8, 0xFF);
    // The pushed marker is rewritten to ADADh so a strict probe sees the
    // word change (RBIL INTERRUP.K, INT 2F/AX=1100h).
    assert_eq!(machine.memory.read_u16(marker_addr).unwrap(), 0xADAD);
}

#[test]
fn int2f_redirector_install_check_reports_not_installed_without_dada_marker() {
    let mut machine = test_machine();
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Ss, SegmentRegister::real(0x9000));
    machine.cpu.registers.set_esp(0x0100);
    let marker_addr = 0x9000 * 16 + (0x0100 + 6);
    // A pushed word other than DADAh is the plain redirector install check.
    // It must not claim IZCDEX installed or touch the stack word.
    machine.memory.write_u16(marker_addr, 0x1234).unwrap();
    machine.cpu.registers.set_eax(0xCAFE_1100);
    assert!(machine.handle_int2f());
    assert_eq!(machine.cpu.registers.eax(), 0xCAFE_1100);
    assert_eq!(machine.memory.read_u16(marker_addr).unwrap(), 0x1234);
}

#[test]
fn icdex_drive_check_reports_the_cd_drive() {
    let mut machine = test_machine();
    machine.mount_cd(audio_cd(4));
    // AX=1500: BX = drive count, CX = first drive letter (D: = 3).
    machine.cpu.registers.set_eax(0x1500);
    assert!(machine.handle_int2f());
    assert_eq!(machine.cpu.registers.ebx() as u16, 1);
    assert_eq!(
        machine.cpu.registers.ecx() as u16,
        u16::from(CD_DRIVE_NUMBER)
    );
    // AX=150B drive check for D:: BX = ADADh, AX nonzero (supported).
    machine.cpu.registers.set_eax(0x150B);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    assert!(machine.handle_int2f());
    assert_eq!(machine.cpu.registers.ebx() as u16, 0xADAD);
    assert_ne!(machine.cpu.registers.eax() as u16, 0);
}

#[test]
fn icdex_direct_calls_cover_metadata_vtoc_absolute_read_and_preferences() {
    let mut machine = test_machine();
    let mut bytes = vec![0u8; 20 * cdimage::DATA_SECTOR];
    bytes[0] = 0x42; // LBA 0 marker for AX=1508h.
    let pvd = 16 * cdimage::DATA_SECTOR;
    bytes[pvd] = 0x01; // primary volume descriptor
    bytes[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
    let root = iso_dir_record(18, cdimage::DATA_SECTOR as u32, 0x02, &[0]);
    bytes[pvd + 156..pvd + 156 + root.len()].copy_from_slice(&root);
    let file = iso_dir_record(19, 1234, 0x00, b"README.TXT;1");
    let root_sector = 18 * cdimage::DATA_SECTOR;
    bytes[root_sector..root_sector + root.len()].copy_from_slice(&root);
    bytes[root_sector + root.len()..root_sector + root.len() + file.len()].copy_from_slice(&file);
    let term = 17 * cdimage::DATA_SECTOR;
    bytes[term] = 0xff; // volume descriptor set terminator
    machine.mount_cd(CdImage::from_iso(bytes).unwrap());

    machine.write_guest_block(0x5000, &[0xaa; 38]);
    prime_dos_int_frame(&mut machine);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0));
    machine.cpu.registers.set_ebx(0x5000);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_eax(0x1502);
    assert!(machine.handle_int2f(), "AX=1502h handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "metadata clears CF");
    assert_eq!(machine.read_guest_block(0x5000, 38), vec![0; 38]);

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0x6000);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_edx(0);
    machine.cpu.registers.set_eax(0x1505);
    assert!(machine.handle_int2f(), "AX=1505h handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "VTOC clears CF");
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0001);
    assert_eq!(machine.read_physical_u8(0x6000), 0x01);
    assert_eq!(&machine.read_guest_block(0x6001, 5), b"CD001");

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0x7000);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_esi(0);
    machine.cpu.registers.set_edi(0);
    machine.cpu.registers.set_edx(1);
    machine.cpu.registers.set_eax(0x1508);
    assert!(machine.handle_int2f(), "AX=1508h handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "absolute read clears CF");
    assert_eq!(machine.read_physical_u8(0x7000), 0x42);

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_edx(0);
    machine.cpu.registers.set_eax(0x150E);
    assert!(machine.handle_int2f(), "AX=150Eh get handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "preference get clears CF");
    assert_eq!(machine.cpu.registers.edx() as u16, 0x0100);

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(1);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_edx(0x0201);
    machine.cpu.registers.set_eax(0x150E);
    assert!(machine.handle_int2f(), "AX=150Eh set handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "preference set clears CF");

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_edx(0);
    machine.cpu.registers.set_eax(0x150E);
    assert!(machine.handle_int2f(), "AX=150Eh get-after-set handled");
    assert_eq!(machine.cpu.registers.edx() as u16, 0x0201);

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_eax(0x150A);
    assert!(machine.handle_int2f(), "AX=150Ah handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "reserved call clears CF");

    machine.write_guest_block(0x5100, b"\\README.TXT\0");
    prime_dos_int_frame(&mut machine);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0));
    machine.cpu.registers.set_ebx(0x5100);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_esi(0x3000);
    machine.cpu.registers.set_edi(0x0100);
    machine.cpu.registers.set_eax(0x150F);
    assert!(machine.handle_int2f(), "AX=150Fh direct handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "directory entry clears CF");
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0001);
    let direct = machine.read_guest_block((0x3000u32 << 4) + 0x0100, file.len());
    assert_eq!(direct[0], file.len() as u8);
    assert_eq!(&direct[2..6], &19u32.to_le_bytes());
    assert_eq!(&direct[10..14], &1234u32.to_le_bytes());
    assert_eq!(&direct[33..45], b"README.TXT;1");

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ebx(0x5100);
    machine
        .cpu
        .registers
        .set_ecx((1u32 << 8) | u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_esi(0x3100);
    machine.cpu.registers.set_edi(0);
    machine.cpu.registers.set_eax(0x150F);
    assert!(machine.handle_int2f(), "AX=150Fh canonical handled");
    assert_eq!(dos_int_flags(&machine) & 1, 0, "canonical entry clears CF");
    let canonical = machine.read_guest_block(0x3100u32 << 4, 0x41);
    assert_eq!(&canonical[1..5], &19u32.to_le_bytes());
    assert_eq!(&canonical[5..7], &1u16.to_le_bytes());
    assert_eq!(&canonical[7..11], &1234u32.to_le_bytes());
    assert_eq!(canonical[0x17], 10);
    assert_eq!(&canonical[0x18..0x22], b"README.TXT");
    assert_eq!(&canonical[0x3e..0x40], &1u16.to_le_bytes());

    prime_dos_int_frame(&mut machine);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    machine.cpu.registers.set_eax(0x1509);
    assert!(machine.handle_int2f(), "AX=1509h handled");
    assert_ne!(dos_int_flags(&machine) & 1, 0, "write sets CF");
    assert_eq!(machine.cpu.registers.eax() as u16, 0x0001);

    prime_dos_int_frame(&mut machine);
    machine
        .cpu
        .registers
        .set_ecx(u32::from(CD_DRIVE_NUMBER) + 1);
    machine.cpu.registers.set_eax(0x1508);
    assert!(machine.handle_int2f(), "AX=1508h invalid drive handled");
    assert_ne!(dos_int_flags(&machine) & 1, 0, "invalid drive sets CF");
    assert_eq!(machine.cpu.registers.eax() as u16, 0x000f);

    let mut empty = test_machine();
    prime_dos_int_frame(&mut empty);
    empty
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0));
    empty.cpu.registers.set_ebx(0x8000);
    empty.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    empty.cpu.registers.set_edx(0);
    empty.cpu.registers.set_eax(0x1505);
    assert!(empty.handle_int2f(), "AX=1505h no-media handled");
    assert_ne!(dos_int_flags(&empty) & 1, 0, "no media sets CF");
    assert_eq!(empty.cpu.registers.eax() as u16, 0x0015);
}

#[test]
fn icdex_send_request_read_long_loads_a_sector() {
    let mut machine = test_machine();
    // A small data ISO with a marker per sector.
    let mut bytes = vec![0u8; 4 * cdimage::DATA_SECTOR];
    bytes[2 * cdimage::DATA_SECTOR] = 0x99; // LBA 2 marker
    machine.mount_cd(CdImage::from_iso(bytes).unwrap());

    // Build a READ LONG (0x80) device request header at linear 0x2000, with a
    // transfer buffer at 0x4000. ES:BX -> header via ES base 0, BX = 0x2000.
    let header = 0x2000u32;
    let xfer = 0x4000u32;
    machine.write_physical_u8(header + 2, 0x80); // command READ LONG
    machine.write_physical_u8(header + 0x0D, 0x00); // HSG addressing
    // transfer address dword at 0x0E
    for (i, b) in xfer.to_le_bytes().iter().enumerate() {
        machine.write_physical_u8(header + 0x0E + i as u32, *b);
    }
    // sector count (1) at 0x12
    machine.write_physical_u8(header + 0x12, 1);
    machine.write_physical_u8(header + 0x13, 0);
    // starting sector (LBA 2) dword at 0x14
    for (i, b) in 2u32.to_le_bytes().iter().enumerate() {
        machine.write_physical_u8(header + 0x14 + i as u32, *b);
    }

    machine.cpu.registers.set_eax(0x1510);
    machine.cpu.registers.set_ebx(header); // ES base 0, BX = header
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    assert!(machine.handle_int2f());

    // The sector landed at the transfer address.
    assert_eq!(machine.read_physical_u8(xfer), 0x99);
    // Status word (offset 3) has the done bit set, no error.
    let status = machine.read_guest_word(header + 3);
    assert_eq!(status & 0x8000, 0, "no error bit");
    assert_ne!(status & 0x0100, 0, "done bit set");
}

#[test]
fn atapi_read10_on_a_folder_mount_returns_a_known_files_bytes() {
    // Mount a small host folder as a CD (the ISO9660 metadata is
    // synthesized in memory, file bytes stay on the host), then drive a
    // real READ(10) through the ATAPI PACKET port handshake exactly like
    // ide.rs's packet_read10 helper, proving the lazy folder backing works
    // through the same device path a real driver uses.
    let dir = tempfile::tempdir().unwrap();
    let content = b"hello from a folder-mounted CD-ROM, izarra style";
    std::fs::write(dir.path().join("HELLO.TXT"), content).unwrap();

    let built = crate::iso9660::build(dir.path()).unwrap();
    let image = CdImage::from_folder(built).unwrap();
    let mut machine = test_machine();
    machine.mount_cd(image);

    // Walk the root directory (through read_data_sector, the same path
    // the ATAPI device serves) to find HELLO.TXT;1's extent LBA.
    let pvd = machine
        .ide
        .device()
        .image()
        .unwrap()
        .read_data_sector(16)
        .unwrap();
    let root_lba = u32::from_le_bytes(pvd[156 + 2..156 + 6].try_into().unwrap());
    let root_sector = machine
        .ide
        .device()
        .image()
        .unwrap()
        .read_data_sector(root_lba)
        .unwrap();
    let mut offset = 0usize;
    let mut file_lba = None;
    while offset < root_sector.len() {
        let len = usize::from(root_sector[offset]);
        if len == 0 {
            break;
        }
        let name_len = usize::from(root_sector[offset + 32]);
        let name = &root_sector[offset + 33..offset + 33 + name_len];
        if name == b"HELLO.TXT;1" {
            file_lba = Some(u32::from_le_bytes(
                root_sector[offset + 2..offset + 6].try_into().unwrap(),
            ));
        }
        offset += len;
    }
    let file_lba = file_lba.expect("HELLO.TXT;1 must be in the root directory");

    // Clear the post-mount unit attention with a TEST UNIT READY packet.
    with_bus(&mut machine, |bus| {
        bus.write_io(0x177, BusWidth::Byte, 0xA0, false).unwrap();
        for b in [0u8; 12] {
            bus.write_io(0x170, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });

    // READ(10) one sector at the file's LBA over the real ATAPI packet
    // ports, then drain the data-in phase.
    let sector = with_bus(&mut machine, |bus| {
        bus.write_io(0x177, BusWidth::Byte, 0xA0, false).unwrap(); // PACKET
        let mut cdb = [0u8; 12];
        cdb[0] = 0x28; // READ(10)
        cdb[2..6].copy_from_slice(&file_lba.to_be_bytes());
        cdb[8] = 1; // one sector
        for b in cdb {
            bus.write_io(0x170, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
        let mut out = Vec::with_capacity(cdimage::DATA_SECTOR);
        for _ in 0..cdimage::DATA_SECTOR {
            out.push(bus.read_io(0x170, BusWidth::Byte, 0, false).unwrap() as u8);
        }
        out
    });

    assert_eq!(&sector[..content.len()], &content[..]);
    assert!(
        sector[content.len()..].iter().all(|&b| b == 0),
        "the rest of the sector is zero-padded"
    );
}

#[test]
fn icdex_send_request_play_audio_starts_playback() {
    let mut machine = test_machine();
    machine.mount_cd(audio_cd(40));
    let header = 0x2000u32;
    machine.write_physical_u8(header + 2, 0x84); // PLAY AUDIO
    machine.write_physical_u8(header + 0x0D, 0x00); // HSG
    // start sector (LBA 1, the audio track) dword at 0x0E
    for (i, b) in 1u32.to_le_bytes().iter().enumerate() {
        machine.write_physical_u8(header + 0x0E + i as u32, *b);
    }
    // play count (8 frames) dword at 0x12
    for (i, b) in 8u32.to_le_bytes().iter().enumerate() {
        machine.write_physical_u8(header + 0x12 + i as u32, *b);
    }
    machine.cpu.registers.set_eax(0x1510);
    machine.cpu.registers.set_ebx(header);
    machine.cpu.registers.set_ecx(u32::from(CD_DRIVE_NUMBER));
    assert!(machine.handle_int2f());
    assert!(machine.ide.device().playback().playing);
}

#[test]
fn render_audio_outputs_at_the_dac_rate() {
    let mut machine = test_machine();
    let pcm = machine.render_audio(OPL_NATIVE_HZ as usize); // one second of OPL time
    assert!(
        (pcm.len() as i32 - DAC_HZ as i32).abs() < 50,
        "expected ~{DAC_HZ} frames, got {}",
        pcm.len()
    );
}

#[test]
fn render_audio_passes_through_when_the_dsp_is_idle() {
    // No DMA playback armed: the DSP produces nothing, so render_audio must
    // return the OPL-only output at the DAC rate (the existing contract).
    let mut machine = test_machine();
    let pcm = machine.render_audio(OPL_NATIVE_HZ as usize);
    assert!(
        (pcm.len() as i32 - DAC_HZ as i32).abs() < 50,
        "idle DSP must not truncate the OPL stream, got {} frames",
        pcm.len()
    );
}

#[test]
fn render_audio_mixes_the_dsp_dc_level_with_the_opl() {
    let mut machine = test_machine();
    // A constant 256-byte DMA buffer; 0x40 maps to sample_u8(0x40) = -16384.
    // The default CT1745 volume attenuates it by voice (0x32=24, ~-14 dB)
    // and master (0x30=24, ~-14 dB): -16384 * 0.19953^2 ~= -652.
    const BYTE: u8 = 0x40;
    let expected: i32 = (-16384.0f32 * 10f32.powf(-14.0 / 20.0) * 10f32.powf(-14.0 / 20.0)) as i32;
    for i in 0..256u32 {
        machine.write_physical_u8(0x1_0000 + i, BYTE);
    }
    with_bus(&mut machine, |bus| {
        // DMA ch1: page 0x01, address 0, count 255, auto-init read.
        bus.write_io(0x0B, BusWidth::Byte, 0x59, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0xFF, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        // DSP: 11025 Hz, block 256, auto-init 8-bit output.
        for &b in &[0x41u8, 0x2B, 0x11, 0x48, 0xFF, 0x00, 0x1C] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });
    // The OPL is silent (no voices keyed), so the steady output is the DSP DC
    // level after the resampler warmup. Playback is clock-driven now, so
    // advance CPU time to let the per-clock producer fill the ring, then
    // render plenty of OPL-native time for the host drainer + resampler.
    machine.advance_devices_clocks(2_500_000);
    let out = machine.render_audio(4_000);
    assert!(!out.is_empty());
    let mid = &out[out.len() / 3..out.len() * 2 / 3];
    let (min_l, max_l) = mid
        .iter()
        .map(|f| f.0)
        .fold((i16::MAX, i16::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let center = (i32::from(min_l) + i32::from(max_l)) / 2;
    assert!(
        (center - expected).abs() < 400,
        "DSP DC center {center}, expected ~{expected}"
    );
    // Mono is duplicated to both channels.
    assert!(mid.iter().all(|f| f.0 == f.1), "DSP mono duplicated L/R");
}

#[test]
fn sb_mixer_voice_and_master_volume_attenuate_output() {
    let mut machine = test_machine();
    // Constant 256-byte DC buffer: sample_u8(0x40) = -16384, auto-init.
    for i in 0..256u32 {
        machine.write_physical_u8(0x1_0000 + i, 0x40);
    }
    with_bus(&mut machine, |bus| {
        bus.write_io(0x0B, BusWidth::Byte, 0x59, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x02, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0xFF, false).unwrap();
        bus.write_io(0x03, BusWidth::Byte, 0x00, false).unwrap();
        bus.write_io(0x83, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x0A, BusWidth::Byte, 0x01, false).unwrap();
        for &b in &[0x41u8, 0x2B, 0x11, 0x48, 0xFF, 0x00, 0x1C] {
            bus.write_io(0x22C, BusWidth::Byte, u32::from(b), false)
                .unwrap();
        }
    });

    fn set_reg(machine: &mut Machine, index: u8, value: u8) {
        with_bus(machine, |bus| {
            bus.write_io(0x224, BusWidth::Byte, u32::from(index), false)
                .unwrap();
            bus.write_io(0x225, BusWidth::Byte, u32::from(value), false)
                .unwrap();
        });
    }
    // Refill the clock-driven ring, then render a window of mixed output.
    fn render(machine: &mut Machine) -> Vec<(i16, i16)> {
        machine.advance_devices_clocks(2_500_000);
        machine.render_audio(4_000)
    }
    fn mid_quiet(out: &[(i16, i16)]) -> bool {
        let mid = &out[out.len() / 3..out.len() * 2 / 3];
        mid.iter().all(|&(l, r)| l.abs() <= 50 && r.abs() <= 50)
    }

    // Voice mute (0x32/0x33 = 0) silences the DSP path regardless of master.
    set_reg(&mut machine, 0x32, 0x00);
    set_reg(&mut machine, 0x33, 0x00);
    assert!(
        mid_quiet(&render(&mut machine)),
        "voice mute silences the DSP output"
    );

    // Master mute (0x30/0x31 = 0) silences the whole mix even at full voice.
    set_reg(&mut machine, 0x32, 0x1F);
    set_reg(&mut machine, 0x33, 0x1F);
    set_reg(&mut machine, 0x30, 0x00);
    set_reg(&mut machine, 0x31, 0x00);
    assert!(
        mid_quiet(&render(&mut machine)),
        "master mute silences the summed output"
    );

    // Defaults (master/voice 24 => -14 dB each) return the attenuated DC level.
    for (idx, val) in [(0x30u8, 24u8), (0x31, 24), (0x32, 24), (0x33, 24)] {
        set_reg(&mut machine, idx, val);
    }
    let restored = render(&mut machine);
    let mid = &restored[restored.len() / 3..restored.len() * 2 / 3];
    let (min_l, max_l) = mid
        .iter()
        .map(|f| f.0)
        .fold((i16::MAX, i16::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let center = (i32::from(min_l) + i32::from(max_l)) / 2;
    let expected = (-16384.0f32 * 10f32.powf(-14.0 / 20.0) * 10f32.powf(-14.0 / 20.0)) as i32;
    assert!(
        (center - expected).abs() < 200,
        "restored DC center {center}, expected ~{expected}"
    );
}

#[test]
fn opl_timers_advance_with_machine_clocks() {
    // AdLib detection: arm timer 1 to overflow in one 80us step, let machine
    // time pass, and confirm the status port reports the overflow + IRQ.
    let mut machine = test_machine();
    with_bus(&mut machine, |bus| {
        let mut write = |reg: u8, value: u8| {
            bus.write_io(0x0388, BusWidth::Byte, u32::from(reg), false)
                .unwrap();
            bus.write_io(0x0389, BusWidth::Byte, u32::from(value), false)
                .unwrap();
        };
        write(0x04, 0x60); // mask both timers
        write(0x04, 0x80); // reset the overflow flags
        write(0x02, 0xff); // timer 1 preset: overflow in one step
        write(0x04, 0x21); // start timer 1 (unmasked), mask timer 2
    });

    // 100 us of CPU time (clock_hz/10000 clocks) covers the 80 us timer step.
    machine.advance_devices(machine.profile().clock_hz / 10_000);

    let status = with_bus(&mut machine, |bus| {
        bus.read_io(0x0388, BusWidth::Byte, 0, false).unwrap()
    });
    assert_eq!(
        status & 0xe0,
        0xc0,
        "timer 1 overflow raises IRQ + timer-1 flag"
    );
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
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, 0x004f);

    let base = 0x40000;
    assert_eq!(read_u16(&mut machine, base + 0x10), 640); // BytesPerScanLine
    assert_eq!(read_u16(&mut machine, base + 0x12), 640); // XResolution
    assert_eq!(read_u16(&mut machine, base + 0x14), 480); // YResolution
    assert_eq!(machine.read_physical_u8(base + 0x19), 8); // BitsPerPixel
    assert_eq!(read_u32(&mut machine, base + 0x28), MARGO_LFB_BASE); // PhysBasePtr
}

#[test]
fn vbe_controller_info_fills_the_block() {
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbf, 0x00, 0x00, // mov di, 0
        0xb8, 0x00, 0x4f, // mov ax, 4F00h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

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
    // OemStringPtr and Capabilities are intentionally left zero.
    assert_eq!(read_u32(&mut machine, base + 0x06), 0); // OemStringPtr
    assert_eq!(read_u32(&mut machine, base + 0x0a), 0); // Capabilities

    // VideoModePtr (seg:off) must point at the mode list, which lists every
    // entry in MARGO_VBE_MODES (8bpp then hi-color then true-color) and ends
    // with the 0xffff terminator.
    let ptr = read_u32(&mut machine, base + 0x0e);
    let list = (((ptr >> 16) & 0xffff) << 4) + (ptr & 0xffff);
    let expected = [
        0x0100, 0x0101, 0x0150, 0x0103, 0x0105, 0x0110, 0x0111, 0x0113, 0x0114, 0x0116, 0x0117,
        0x014a, 0x014c, 0x014e, 0xffff,
    ];
    for (i, &mode) in expected.iter().enumerate() {
        assert_eq!(read_u16(&mut machine, list + (i * 2) as u32), mode);
    }
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
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, 0x014f);
}

// Write/read a 32-bit Margo register through the MMIO aperture.
fn write_mmio_reg(machine: &mut Machine, offset: u32, value: u32) {
    for (i, b) in value.to_le_bytes().into_iter().enumerate() {
        machine.write_physical_u8(MARGO_MMIO_BASE + offset + i as u32, b);
    }
}

fn read_mmio_reg(machine: &mut Machine, offset: u32) -> u32 {
    let mut value = 0u32;
    for i in 0..4 {
        value |= u32::from(machine.read_physical_u8(MARGO_MMIO_BASE + offset + i)) << (8 * i);
    }
    value
}

#[test]
fn copy_through_the_mmio_aperture_moves_vram_and_times_busy() {
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
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com).unwrap();
    let reason = machine.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    assert_eq!(machine.program_output(), b"Hi");
}

#[test]
fn dos_com_exit_code_is_carried_through() {
    // org 0x100: mov ax,4c07; int 21
    let com: &[u8] = &[0xb8, 0x07, 0x4c, 0xcd, 0x21];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com).unwrap();
    let reason = machine.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 7 });
    assert!(machine.program_output().is_empty());
}

#[test]
fn fill_through_the_mmio_aperture_writes_vram_and_times_busy() {
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
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
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
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
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
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com).unwrap();
    available.set_program_stdin(b"X");
    assert_eq!(
        available.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );
    assert_eq!(available.program_output(), b"X"); // char path taken, AL echoed

    let mut empty =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com).unwrap();
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
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com).unwrap();
    machine.set_program_stdin(b"hi");
    assert_eq!(
        machine.run_until_halt_or_cycles(100_000).unwrap(),
        StopReason::DosExit { code: 0 }
    );
    assert_eq!(machine.program_output(), b"hi");
}

#[test]
fn color_expand_data_through_the_mmio_aperture_draws_a_glyph_and_times_busy() {
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
fn line_through_the_mmio_aperture_draws_and_times_busy() {
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
fn pattern_fill_through_the_mmio_aperture_tiles_and_times_busy() {
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
fn clipped_xor_fill_through_the_mmio_aperture() {
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
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
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
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
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
fn hardware_cursor_composites_through_the_apertures() {
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

#[test]
fn machine_advances_the_vga_beam_with_cpu_clocks() {
    let mut machine = test_machine();
    machine.set_vga_mode_0dh();
    let before = machine.video().beam_dots();
    // 10 000 CPU clocks at 22 MHz with a 25.175 MHz dot clock advances
    // roughly 11 443 dots, well above zero.
    machine.advance_devices(10_000);
    assert!(machine.video().beam_dots() != before || machine.video().frames_completed() > 0);
}

#[test]
fn display_refresh_matches_the_vga_mode() {
    let mut machine = test_machine();
    // Mode 0Dh is a ~359 200-dot frame at the 25.175 MHz dot clock, i.e.
    // ~70 Hz, the classic VGA graphics refresh.
    machine.set_vga_mode_0dh();
    let hz = machine.display_refresh_hz();
    assert!((hz - 70.0).abs() < 1.0, "expected ~70 Hz, got {hz}");
    // Mode 12h (640x480, 525 lines) is the 60 Hz timing.
    machine.set_vga_mode(0x12);
    let hz = machine.display_refresh_hz();
    assert!((hz - 60.0).abs() < 1.0, "expected ~60 Hz, got {hz}");
}

#[test]
fn display_refresh_uses_misc_output_clock_select() {
    let mut machine = test_machine();
    machine.set_vga_mode_0dh();
    let clock25 = machine.display_refresh_hz();
    assert!(machine.video_mut().write_port(0x3C2, 0x04));
    let clock28 = machine.display_refresh_hz();

    assert!(clock28 > clock25);
    assert!(
        (clock28 / clock25 - 28_322_000.0 / 25_175_000.0).abs() < 0.01,
        "expected refresh ratio to follow Misc Output clock select"
    );
}

#[test]
fn planar_mode_presents_a_vga_raster() {
    let mut machine = test_machine();
    machine.set_vga_mode_0dh();
    // Mode 0Dh frame is ~359 200 dots; 600 000 CPU clocks at 22 MHz yields
    // ~686 600 dot clocks, enough to complete at least one full frame.
    machine.advance_devices(600_000);
    assert!(matches!(machine.active_display(), ActiveDisplay::VgaRaster));
    assert!(machine.vga_raster().is_some());
}

#[test]
fn text_mode_scanout_through_the_machine() {
    let mut machine = test_machine();
    // A CP437 cell at B8000:0 (the solid block 0xDB) with a white-on-black
    // attribute, written through the bus so it routes to text_memory.
    machine.write_physical_u8(VGA_TEXT_BASE, 0xDB);
    machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
    // Mode 03h maps white text through DAC index 0x3F.
    machine.video_mut().set_dac_entry(0x3F, 63, 0, 0);
    // Enough CPU time to finalize at least one frame.
    machine.advance_devices(600_000);
    assert!(matches!(machine.active_display(), ActiveDisplay::VgaRaster));
    let raster = machine.vga_raster().expect("text presents a VgaRaster");
    assert_eq!(raster.width, 720);
    assert_eq!(raster.pixels[0], 0x3F);
    assert_eq!(machine.palette_argb()[0x3F], 0x00FF_0000);
}

#[test]
fn video_subsystem_enable_gates_legacy_apertures_through_the_machine() {
    let mut machine = test_machine();

    machine.write_physical_u8(VGA_TEXT_BASE, b'T');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'T');
    assert!(machine.video_mut().write_port(0x3C3, 0x00));
    machine.write_physical_u8(VGA_TEXT_BASE, b'R');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'R');
    assert!(machine.video_mut().write_port(0x3C3, 0x01));
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'T');

    machine.video_mut().set_mode13h();
    machine.write_physical_u8(VGA_MODE13H_BASE, 0x12);
    assert_eq!(machine.read_physical_u8(VGA_MODE13H_BASE), 0x12);
    assert!(machine.video_mut().write_port(0x3C3, 0x00));
    machine.write_physical_u8(VGA_MODE13H_BASE, 0x34);
    assert_eq!(machine.read_physical_u8(VGA_MODE13H_BASE), 0x34);
    assert!(machine.video_mut().write_port(0x3C3, 0x01));
    assert_eq!(machine.read_physical_u8(VGA_MODE13H_BASE), 0x12);
}

#[test]
fn misc_output_ram_enable_gates_legacy_apertures_through_the_machine() {
    let mut machine = test_machine();

    machine.write_physical_u8(VGA_TEXT_BASE, b'T');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'T');
    let misc = machine.video_mut().read_port(0x3CC).unwrap();
    assert!(machine.video_mut().write_port(0x3C2, misc & !0x02));
    assert!(machine.video().video_subsystem_enabled());
    assert!(!machine.video().video_memory_enabled());
    machine.write_physical_u8(VGA_TEXT_BASE, b'R');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'R');
    {
        let mut bus = machine.make_bus();
        assert_eq!(bus.read_io(0x3C3, BusWidth::Byte, 0, false).unwrap(), 1);
        assert_eq!(
            bus.read_io(0x3CC, BusWidth::Byte, 0, false).unwrap() & 0x02,
            0
        );
    }
    let misc = machine.video_mut().read_port(0x3CC).unwrap();
    assert!(machine.video_mut().write_port(0x3C2, misc | 0x02));
    assert!(machine.video().video_memory_enabled());
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'T');

    machine.video_mut().set_mode13h();
    machine.write_physical_u8(VGA_MODE13H_BASE, 0x12);
    assert_eq!(machine.read_physical_u8(VGA_MODE13H_BASE), 0x12);
    let misc = machine.video_mut().read_port(0x3CC).unwrap();
    assert!(machine.video_mut().write_port(0x3C2, misc & !0x02));
    machine.write_physical_u8(VGA_MODE13H_BASE, 0x34);
    assert_eq!(machine.read_physical_u8(VGA_MODE13H_BASE), 0x34);
    let misc = machine.video_mut().read_port(0x3CC).unwrap();
    assert!(machine.video_mut().write_port(0x3C2, misc | 0x02));
    assert_eq!(machine.read_physical_u8(VGA_MODE13H_BASE), 0x12);
}

#[test]
fn mode7_routes_b000_text_window_through_the_machine() {
    // mov ax,0007h; int 10h; hlt
    let rom = rom_with_code(&[0xb8, 0x07, 0x00, 0xcd, 0x10, 0xf4]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    assert_eq!(machine.video().active_mode(), VideoMode::Text);
    assert_eq!(machine.video().raster_width(), 720);
    assert_eq!(machine.video().raster_height(), 449);
    assert_eq!(machine.read_physical_u8(0x449), 0x07);
    assert_eq!(machine.read_physical_u16(0x463), 0x03B4);
    assert_eq!(machine.read_physical_u8(0x485), 14);

    machine.write_physical_u8(VGA_MONO_TEXT_BASE, 0xDB);
    machine.write_physical_u8(VGA_MONO_TEXT_BASE + 1, 0x0F);
    assert_eq!(machine.read_physical_u8(VGA_MONO_TEXT_BASE), 0xDB);
    assert_eq!(machine.read_physical_u8(VGA_MONO_TEXT_BASE + 1), 0x0F);

    machine.advance_devices(600_000);
    let raster = machine.vga_raster().expect("mode 7 presents a VgaRaster");
    assert_eq!(raster.pixels[0], 0x0F);
}

#[test]
fn cga_graphics_routes_b800_to_the_framebuffer() {
    let mut machine = test_machine();
    // Enter CGA mode 04h (320x200x4) the way INT 10h AH=00 AL=04 would.
    machine.video_mut().set_cga_mode(0x04);
    assert_eq!(machine.video().active_mode(), VideoMode::Cga);
    // A byte written to B800:0000 lands in the CGA framebuffer, not the text
    // buffer. 0b00_01_10_11 decodes to bg/green/red/brown on the default
    // palette (green=2, red=4, brown=6).
    machine.write_physical_u8(VGA_TEXT_BASE, 0b00_01_10_11);
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), 0b00_01_10_11);
    let raster = machine.video_mut().render_full_frame();
    assert_eq!(raster.width, 320);
    assert_eq!(raster.height, 262);
    // The first four pixels of scanline 0.
    assert_eq!(&raster.pixels[0..4], &[0, 2, 4, 6]);
}

#[test]
fn cga_odd_scanline_reads_the_high_bank_through_the_machine() {
    let mut machine = test_machine();
    machine.video_mut().set_cga_mode(0x04);
    // Scanline 1 of a CGA frame reads framebuffer offset 0x2000 (the odd bank).
    // Write there through the B800 aperture and confirm it scans out on line 1.
    machine.write_physical_u8(VGA_TEXT_BASE + 0x2000, 0b01_01_01_01);
    let raster = machine.video_mut().render_full_frame();
    // Row 1 starts at offset width*1.
    let row1 = &raster.pixels[320..320 + 4];
    assert_eq!(row1, &[2, 2, 2, 2]); // value 1 -> green(2)
}

#[test]
fn cga_mode_control_switches_b800_routing_through_the_machine() {
    let mut machine = test_machine();
    assert!(machine.video_mut().set_cga_text_mode(0x01));
    machine.write_physical_u8(VGA_TEXT_BASE, b'T');
    machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);

    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3D8, BusWidth::Byte, 0x0A, false).unwrap();
    }
    assert_eq!(machine.video().active_mode(), VideoMode::Cga);
    assert_eq!(machine.video().render_cga_row(0)[0], 2);
    machine.write_physical_u8(VGA_TEXT_BASE, 0b01_01_01_01);
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), 0b01_01_01_01);

    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3D8, BusWidth::Byte, 0x28, false).unwrap();
    }
    assert_eq!(machine.video().active_mode(), VideoMode::Text);
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), 0b01_01_01_01);
}

#[test]
fn cga_mode_and_color_select_ports_are_output_only_through_the_bus() {
    let mut machine = test_machine();
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3D8, BusWidth::Byte, 0x0A, false).unwrap();
        bus.write_io(0x3D9, BusWidth::Byte, 0x35, false).unwrap();
        assert_eq!(bus.read_io(0x3D8, BusWidth::Byte, 0, false).unwrap(), 0xFF);
        assert_eq!(bus.read_io(0x3D9, BusWidth::Byte, 0, false).unwrap(), 0xFF);
    }

    assert_eq!(machine.video().active_mode(), VideoMode::Cga);
    assert_eq!(machine.video().cga_color_select(), 0x35);
}

#[test]
fn cga_crtc_alias_ports_route_through_video_bus() {
    let mut machine = test_machine();
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3D8, BusWidth::Byte, 0x0A, false).unwrap();
        bus.write_io(0x3D0, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x3D1, BusWidth::Byte, 0x20, false).unwrap();
        assert_eq!(bus.read_io(0x3D2, BusWidth::Byte, 0, false).unwrap(), 0xFF);
        assert_eq!(bus.read_io(0x3D3, BusWidth::Byte, 0, false).unwrap(), 0xFF);

        bus.write_io(0x3D6, BusWidth::Byte, 0x0A, false).unwrap();
        bus.write_io(0x3D7, BusWidth::Byte, 0x06, false).unwrap();
        assert_eq!(bus.read_io(0x3D4, BusWidth::Byte, 0, false).unwrap(), 0xFF);
        assert_eq!(bus.read_io(0x3D5, BusWidth::Byte, 0, false).unwrap(), 0xFF);

        bus.write_io(0x3D4, BusWidth::Byte, 0x0E, false).unwrap();
        bus.write_io(0x3D5, BusWidth::Byte, 0x12, false).unwrap();
        assert_eq!(bus.read_io(0x3D5, BusWidth::Byte, 0, false).unwrap(), 0x12);
    }

    assert_eq!(machine.video().active_mode(), VideoMode::Cga);
    assert_eq!(machine.video().raster_width(), 256);
}

#[test]
fn cga_text_b800_window_mirrors_16kb_through_the_machine() {
    let mut machine = test_machine();
    assert!(machine.video_mut().set_cga_text_mode(0x01));
    machine.write_physical_u8(VGA_TEXT_BASE, b'A');
    machine.write_physical_u8(VGA_TEXT_BASE + CGA_FB_SIZE as u32, b'B');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'B');
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE + CGA_FB_SIZE as u32),
        b'B'
    );

    machine.video_mut().set_text_mode();
    machine.write_physical_u8(VGA_TEXT_BASE, b'A');
    machine.write_physical_u8(VGA_TEXT_BASE + CGA_FB_SIZE as u32, b'V');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'A');
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE + CGA_FB_SIZE as u32),
        b'V'
    );
}

#[test]
fn hercules_graphics_routes_b0000_and_b8000_through_the_machine() {
    // Real Hercules software sets BIOS mode 07h (MDA-compatible text) and
    // then bangs ports 3B8h/3BFh directly: there is no INT 10h graphics
    // mode number for it.
    let mut machine = test_machine();
    machine.video_mut().set_mono_text_mode();

    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3BF, BusWidth::Byte, 0x01, false).unwrap(); // allow graphics
        bus.write_io(0x3B8, BusWidth::Byte, 0x0A, false).unwrap(); // GRPH + video enable
    }
    assert_eq!(machine.video().active_mode(), VideoMode::Hercules);
    assert_eq!(machine.video().raster_width(), 720);
    assert_eq!(machine.video().raster_height(), 370);

    // Page 0 lives at B0000 and is always addressable.
    machine.write_physical_u8(VGA_MONO_TEXT_BASE, 0b1000_0000);
    assert_eq!(machine.read_physical_u8(VGA_MONO_TEXT_BASE), 0b1000_0000);
    let raster = machine.video_mut().render_full_frame();
    assert_eq!(raster.pixels[0], 1);

    // Page 1 (B8000) is not yet paged in: a write there does not land in
    // the Hercules framebuffer (falls through to the flat RAM array
    // underneath, like any other unclaimed MMIO window in this bus), so
    // it is invisible to the Hercules scanout.
    assert!(!machine.video().hgc_page1_addressable());
    machine.write_physical_u8(VGA_MONO_TEXT_BASE + HGC_FB_SIZE as u32, 0xFF);
    assert_eq!(machine.video_mut().hgc_read(HGC_FB_SIZE), 0);

    // Page in the second bank through 3BFh and flip Mode Control's page
    // select (bit 7): the CRTC now scans out B8000 instead of B0000.
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3BF, BusWidth::Byte, 0x03, false).unwrap(); // allow graphics + page 1
        bus.write_io(0x3B8, BusWidth::Byte, 0x8A, false).unwrap(); // GRPH + video + page select
    }
    machine.write_physical_u8(VGA_MONO_TEXT_BASE + HGC_FB_SIZE as u32, 0b0100_0000);
    assert_eq!(
        machine.read_physical_u8(VGA_MONO_TEXT_BASE + HGC_FB_SIZE as u32),
        0b0100_0000
    );
    let raster = machine.video_mut().render_full_frame();
    assert_eq!(raster.pixels[0], 0); // page 0's bit no longer scanned out
    assert_eq!(raster.pixels[1], 1); // page 1's bit shows instead
}

#[test]
fn hercules_config_switch_refuses_graphics_through_the_machine() {
    let mut machine = test_machine();
    machine.video_mut().set_mono_text_mode();

    // 3B8h GRPH with no 3BFh unlock: the card stays in text mode, and the
    // Hercules 64K graphics window does not decode (falls through to the
    // ordinary mono text B0000 aperture instead).
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3B8, BusWidth::Byte, 0x0A, false).unwrap();
    }
    assert_eq!(machine.video().active_mode(), VideoMode::Text);
    machine.write_physical_u8(VGA_MONO_TEXT_BASE, 0xDB);
    assert_eq!(machine.read_physical_u8(VGA_MONO_TEXT_BASE), 0xDB);
}

#[test]
fn hercules_detection_status_port_survives_the_machine_bus() {
    let mut machine = test_machine();
    machine.video_mut().set_mono_text_mode();
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3BF, BusWidth::Byte, 0x01, false).unwrap();
        bus.write_io(0x3B8, BusWidth::Byte, 0x0A, false).unwrap();
    }
    assert_eq!(machine.video().active_mode(), VideoMode::Hercules);

    let mut bus = machine.make_bus();
    let outside_vsync = bus.read_io(0x3BA, BusWidth::Byte, 0, false).unwrap() & 0x80;
    assert_eq!(outside_vsync, 0x80);
}

#[test]
fn int10_11h_loads_user_font() {
    // A 2-glyph user font (two solid 8x16 blocks) at ES:BP = 4000h:0,
    // overwriting 'A' and 'B'. AL=00 loads it; BH=16 bytes/char, BL=0
    // (table 0), CX=2, DX=41h.
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbd, 0x00, 0x00, // mov bp, 0
        0xb9, 0x02, 0x00, // mov cx, 2
        0xba, 0x41, 0x00, // mov dx, 41h (first char 'A')
        0xbb, 0x00, 0x10, // mov bx, 1000h (BH=16, BL=0)
        0xb8, 0x00, 0x11, // mov ax, 1100h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.write_guest_block(0x40000, &[0xFF; 32]); // two solid glyphs
    // Display cell 0 = 'A', white on black.
    machine.write_physical_u8(VGA_TEXT_BASE, 0x41);
    machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    // The custom 'A' is solid, so its top row scans out as the foreground.
    // The stock 'A' would be blank on the top row (background), so this
    // confirms the user font loaded and renders.
    assert_eq!(machine.video().render_text_row(0)[0], BIOS_TEXT_WHITE);
}

#[test]
fn int43_points_at_current_font_table() {
    let mut machine = int15_machine(16);
    let off = read_u16(&mut machine, 0x43 * 4);
    let seg = read_u16(&mut machine, 0x43 * 4 + 2);
    let table = (u32::from(seg) << 4) + u32::from(off);
    assert_eq!(seg, (VGA_BIOS_BASE >> 4) as u16);
    assert_eq!(off, VGA_BIOS_FONT_TABLE_OFF);
    assert_eq!(table, VGA_BIOS_INT43_FONT_ADDR);
    assert_eq!(
        machine.read_physical_u8(table + 0x41 * 16 + 7),
        izarravm_video::font::VGAFONT_8X16[0x41 * 16 + 7]
    );

    machine.cpu.registers.set_eax(0x1130);
    machine.cpu.registers.set_ebx(0x0100); // BH=01h: INT 43h pointer
    machine.handle_int10();
    assert_eq!(
        machine.cpu.registers.segment(SegmentIndex::Es).selector,
        (VGA_BIOS_BASE >> 4) as u16
    );
    assert_eq!(machine.cpu.registers.ebp() as u16, VGA_BIOS_FONT_TABLE_OFF);
    assert_eq!(machine.cpu.registers.ecx() as u16, 16);
    assert_eq!(machine.cpu.registers.edx() as u8, 24);

    machine.write_guest_block(0x40000, &[0xFF; 16]);
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
    machine.cpu.registers.set_ebp(0);
    machine.cpu.registers.set_ecx(1);
    machine.cpu.registers.set_edx(0x41);
    machine.cpu.registers.set_ebx(0x1000); // BH=16, BL=0
    machine.cpu.registers.set_eax(0x1100);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(table + 0x41 * 16), 0xFF);
}

#[test]
fn bios_table_vectors_1d_1e_1f_point_at_seeded_tables() {
    let mut machine = int15_machine(16);

    let int1d = {
        let off = read_u16(&mut machine, 0x1d * 4);
        let seg = read_u16(&mut machine, 0x1d * 4 + 2);
        (u32::from(seg) << 4) + u32::from(off)
    };
    assert_eq!(int1d, VGA_BIOS_INT1D_VIDEO_TABLE_ADDR);
    assert_eq!(
        machine.read_physical_u8(int1d + 2 * 16 + 1),
        0x50,
        "INT 1Dh mode 02h table is 80-column text"
    );

    let int1e = {
        let off = read_u16(&mut machine, 0x1e * 4);
        let seg = read_u16(&mut machine, 0x1e * 4 + 2);
        (u32::from(seg) << 4) + u32::from(off)
    };
    assert_eq!(int1e, BIOS_DISKETTE_PARAMETER_TABLE_ADDR);
    assert_eq!(
        machine.read_physical_u8(int1e + 4),
        0x12,
        "default diskette table describes 18 sectors per track"
    );

    let int1f = {
        let off = read_u16(&mut machine, 0x1f * 4);
        let seg = read_u16(&mut machine, 0x1f * 4 + 2);
        (u32::from(seg) << 4) + u32::from(off)
    };
    assert_eq!(int1f, VGA_BIOS_INT1F_FONT_ADDR);
    assert_eq!(
        machine.read_physical_u8(int1f + (0xc4 - 0x80) * 8),
        izarravm_video::font::VGAFONT_8X8[0xc4 * 8]
    );
}

#[test]
fn int44_points_at_rom_8x8_font_table() {
    let mut machine = int15_machine(16);
    let off = read_u16(&mut machine, 0x44 * 4);
    let seg = read_u16(&mut machine, 0x44 * 4 + 2);
    let table = (u32::from(seg) << 4) + u32::from(off);
    assert_eq!(seg, (VGA_BIOS_BASE >> 4) as u16);
    assert_eq!(off, VGA_BIOS_INT44_FONT_OFF);
    assert_eq!(table, VGA_BIOS_INT44_FONT_ADDR);
    assert_eq!(
        machine.read_physical_u8(table + 0x41 * 8 + 4),
        izarravm_video::font::VGAFONT_8X8[0x41 * 8 + 4]
    );
}

#[test]
fn int10_11h_loads_rom_8x16() {
    // First a custom load blanks glyph 0xDB (AL=00); then AL=04 reloads the
    // ROM 8x16 font, restoring the solid full block.
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbd, 0x00, 0x00, // mov bp, 0
        0xb9, 0x01, 0x00, // mov cx, 1
        0xba, 0xdb, 0x00, // mov dx, 0DBh (full block)
        0xbb, 0x00, 0x10, // mov bx, 1000h (BH=16, BL=0)
        0xb8, 0x00, 0x11, // mov ax, 1100h (user font)
        0xcd, 0x10, // int 10h
        0xbb, 0x00, 0x10, // mov bx, 1000h
        0xb8, 0x04, 0x11, // mov ax, 1104h (ROM 8x16)
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.write_guest_block(0x40000, &[0x00; 16]); // a blank glyph for 0xDB
    machine.write_physical_u8(VGA_TEXT_BASE, 0xDB);
    machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
    assert_eq!(
        machine.run_until_halt_or_cycles(1_000_000).unwrap(),
        StopReason::Halted
    );
    // The ROM reload restored the solid full block; without it the custom
    // blank load would leave the top row as the background (0).
    assert_eq!(machine.video().render_text_row(0)[0], BIOS_TEXT_WHITE);
}

#[test]
fn int10_11h_caps_a_pathological_glyph_count() {
    // CX = 0xFFFF with BH = 16 would read ~16 MB byte-at-a-time. The handler
    // caps the read at 256 glyphs (codes fold modulo 256), so the call still
    // loads the first glyph and returns promptly without stalling or
    // over-allocating.
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x40, // mov ax, 4000h
        0x8e, 0xc0, // mov es, ax
        0xbd, 0x00, 0x00, // mov bp, 0
        0xb9, 0xff, 0xff, // mov cx, 0FFFFh
        0xba, 0x41, 0x00, // mov dx, 41h ('A')
        0xbb, 0x00, 0x10, // mov bx, 1000h (BH=16, BL=0)
        0xb8, 0x00, 0x11, // mov ax, 1100h
        0xcd, 0x10, // int 10h
        0xf4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    // A solid glyph for 'A' at the first 16 bytes; the rest of the 64 KB
    // page stays zero, so capping the read also proves only the real glyph
    // data is consulted.
    machine.write_guest_block(0x40000, &[0xFF; 16]);
    machine.write_physical_u8(VGA_TEXT_BASE, 0x41);
    machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
    let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // The first glyph (solid) loaded and renders as the foreground.
    assert_eq!(machine.video().render_text_row(0)[0], BIOS_TEXT_WHITE);
}

#[test]
fn int10_teletype_and_cursor() {
    let rom = rom_with_code(&[
        0xB8, 0x03, 0x00, 0xCD, 0x10, // set text mode 03h (homes cursor)
        0xB4, 0x0E, 0xB0, b'H', 0xCD, 0x10, // AH=0Eh teletype 'H'
        0xB4, 0x0E, 0xB0, b'i', 0xCD, 0x10, // AH=0Eh teletype 'i'
        0xB4, 0x03, 0xB7, 0x00, 0xCD, 0x10, // AH=03h get cursor (page 0)
        0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // 'H' then 'i' landed at row 0 cols 0,1; cursor now at row 0 col 2.
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'H');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE + 2), b'i');
    let dx = machine.cpu().registers.edx() as u16;
    assert_eq!(dx, 0x0002, "DH=row 0, DL=col 2");
}

#[test]
fn int10_01_updates_cga_hardware_cursor_shape() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x0002);
    machine.handle_int10();
    machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
    assert_eq!(machine.video().render_text_row(0)[0], 0);

    machine.cpu.registers.set_eax(0x0100);
    machine.cpu.registers.set_ecx(0x0007);
    machine.handle_int10();
    assert_eq!(machine.video().render_text_row(0)[0], 15);

    machine.cpu.registers.set_eax(0x0300);
    machine.cpu.registers.set_ebx(0);
    machine.handle_int10();
    assert_eq!(machine.cpu.registers.ecx() as u16, 0x0007);
}

#[test]
fn int10_text_services_use_40_column_mode_stride() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0001);
    m.handle_int10();
    assert_eq!(m.video().frame().columns, 40);
    assert_eq!(m.video_mut().render_full_frame().width, 320);
    assert_eq!(m.memory.read_u16(0x44c).unwrap(), 0x0800);

    m.write_guest_block(0x4000, b"ABCD");
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x0000));
    m.cpu.registers.set_ebp(0x4000);
    m.cpu.registers.set_eax(0x1301);
    m.cpu.registers.set_ebx(0x001E);
    m.cpu.registers.set_ecx(4);
    m.cpu.registers.set_edx(38);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(VGA_TEXT_BASE + 38 * 2), b'A');
    assert_eq!(m.read_physical_u8(VGA_TEXT_BASE + 39 * 2), b'B');
    assert_eq!(m.read_physical_u8(VGA_TEXT_BASE + 40 * 2), b'C');
    assert_eq!(m.read_physical_u8(VGA_TEXT_BASE + 41 * 2), b'D');
    assert_eq!(m.memory.read_u16(0x450).unwrap(), 0x0102);

    m.cpu.registers.set_eax(0x0200);
    m.cpu.registers.set_edx(39);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0E5A);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(VGA_TEXT_BASE + 39 * 2), b'Z');
    assert_eq!(m.memory.read_u16(0x450).unwrap(), 0x0100);
    assert_eq!(m.video().frame().cursor_offset, 40);

    m.cpu.registers.set_eax(0x0F00);
    m.handle_int10();
    assert_eq!((m.cpu.registers.eax() as u16) >> 8, 40);
}

#[test]
fn int10_mode02_uses_cga_80_text_geometry_and_mode03_stays_vga() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0002);
    m.handle_int10();
    assert_eq!(m.video().frame().columns, 80);
    assert_eq!(m.video_mut().render_full_frame().width, 640);
    assert_eq!(m.memory.read_u16(0x44c).unwrap(), 0x1000);
    assert_eq!(m.read_physical_u8(0x485), 8);

    m.cpu.registers.set_eax(0x0003);
    m.handle_int10();
    assert_eq!(m.video().frame().columns, 80);
    assert_eq!(m.video_mut().render_full_frame().width, 720);
    assert_eq!(m.memory.read_u16(0x44c).unwrap(), 0x1000);
    assert_eq!(m.read_physical_u8(0x485), 16);

    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();
    assert_eq!(m.memory.read_u16(0x44c).unwrap(), 0x4000);
    assert_eq!(m.read_physical_u8(0x485), 8);
}

#[test]
fn int10_scroll_window_up_blanks_bottom() {
    // No mode set here: setting a text mode clears the framebuffer, which
    // would wipe the marker the host seeds below before the scroll runs.
    let rom = rom_with_code(&[
        0xB8, 0x01, 0x06, // mov ax,0601h (AH=06h scroll up 1 line)
        0xB7, 0x07, // mov bh,07h (fill attr)
        0xB9, 0x00, 0x00, // mov cx,0000h (top-left 0,0)
        0xBA, 0x4F, 0x18, // mov dx,184Fh (bottom-right row 24 col 79)
        0xCD, 0x10, 0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    // Put a non-space at row 1 col 0; after scroll-up by 1 it lands at row 0.
    machine.write_physical_u8(VGA_TEXT_BASE + 80 * 2, b'X');
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE),
        b'X',
        "row 1 scrolled to row 0"
    );
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE + 24 * 80 * 2),
        b' ',
        "bottom row blanked"
    );
}

#[test]
fn int10_scroll_window_down_blanks_top() {
    // No mode set here: setting a text mode clears the framebuffer, which
    // would wipe the marker the host seeds below before the scroll runs.
    let rom = rom_with_code(&[
        0xB8, 0x01, 0x07, // mov ax,0701h (AH=07h scroll down 1 line)
        0xB7, 0x07, // mov bh,07h (fill attr)
        0xB9, 0x00, 0x00, // mov cx,0000h (top-left 0,0)
        0xBA, 0x4F, 0x18, // mov dx,184Fh (bottom-right row 24 col 79)
        0xCD, 0x10, 0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    // Put a non-space at row 0 col 0; after scroll-down by 1 it lands at row 1.
    machine.write_physical_u8(VGA_TEXT_BASE, b'Y');
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE + 80 * 2),
        b'Y',
        "row 0 scrolled to row 1"
    );
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE),
        b' ',
        "top row blanked"
    );
}

#[test]
fn int10_scroll_subwindow_up() {
    // No mode set here: setting a text mode clears the framebuffer, which
    // would wipe the marker the host seeds below before the scroll runs.
    // CX = top-left, DX = bottom-right; for each, the high byte is the row
    // and the low byte is the column: CX=(row<<8)|col, DX=(row<<8)|col.
    let rom = rom_with_code(&[
        0xB8, 0x01, 0x06, // mov ax,0601h (AH=06h scroll up 1 line)
        0xB7, 0x07, // mov bh,07h (fill attr)
        0xB9, 0x04, 0x01, // mov cx,0104h (top-left row 1 col 4)
        0xBA, 0x0A, 0x03, // mov dx,030Ah (bottom-right row 3 col 10)
        0xCD, 0x10, 0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    // Marker inside the window at row 2 col 5; after scroll-up by 1 it lands
    // at row 1 col 5.
    machine.write_physical_u8(VGA_TEXT_BASE + ((2 * 80) + 5) * 2, b'W');
    // Sentinels in cells outside the window (the framebuffer is otherwise
    // pre-blanked with spaces, so seed distinct bytes to prove the scroll's
    // row and column clamping never wrote here): row 0 col 0 is above the
    // window, row 2 col 0 is left of the col-4 left edge.
    machine.write_physical_u8(VGA_TEXT_BASE, b'A');
    machine.write_physical_u8(VGA_TEXT_BASE + (2 * 80) * 2, b'B');
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE + (80 + 5) * 2),
        b'W',
        "row 2 col 5 scrolled to row 1 col 5"
    );
    // A cell above the window (row 0 col 0) is untouched.
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE),
        b'A',
        "row 0 col 0 outside window left untouched"
    );
    // A cell to the left of the window (row 2 col 0, left edge is col 4) is
    // untouched.
    assert_eq!(
        machine.read_physical_u8(VGA_TEXT_BASE + (2 * 80) * 2),
        b'B',
        "row 2 col 0 left of window left untouched"
    );
}

#[test]
fn a0000_writes_route_to_the_planar_datapath_in_mode_0dh() {
    let mut machine = test_machine();
    machine.set_vga_mode_0dh();
    // Enable plane 0 only, copy write mode, full bit mask, via the VGA ports.
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
    machine.video_mut().write_port(0x3CE, 0x05);
    machine.video_mut().write_port(0x3CF, 0x00); // write mode 0
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
    // Write a byte to A0000 through the machine memory path.
    machine.write_physical_u8(0x000A_0000, 0xFF);
    // Plane 0 byte 0 should now be 0xFF (planar datapath), confirming routing.
    assert_eq!(machine.video().plane_byte(0, 0), 0xFF);
}

#[test]
fn copper_bar_split_through_the_machine() {
    let mut machine = test_machine();
    machine.set_vga_mode_0dh();
    // Set up so A0000 writes fill plane 0 (attribute index 1) with a full bit
    // mask. Write mode 0 is the reset default.
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
    // Fill the visible region of plane 0 (offset 0..8000 covers 200 lines * 40
    // bytes) through the machine memory path — exercises the A0000 routing.
    for off in 0..8000u32 {
        machine.write_physical_u8(0x000A_0000 + off, 0xFF);
    }
    // Identity attribute palette so index 1 -> DAC 1. Reading 3DA resets the
    // flip-flop to "index" first; each entry is an index write then a value
    // write, so after 16 entries the flip-flop is back in "index" mode.
    machine.video_mut().read_status1(); // reset attr flip-flop
    for i in 0..16u8 {
        machine.video_mut().write_port(0x3C0, 0x20 | i); // index, PAS on
        machine.video_mut().write_port(0x3C0, i); // value: palette[i] = i
    }
    // Advance to roughly counter line 50, change palette[1] -> 9, then finish
    // the frame. dots = clocks * VGA_DOT_HZ / clock_hz (~1.007 dots/clock);
    // 39_700 clocks ≈ 39_980 dots ≈ counter line 49 (htotal 800).
    machine.advance_devices(39_700);
    // The flip-flop is in "index" mode here (even number of writes above).
    machine.video_mut().write_port(0x3C0, 0x21); // attr index 1, PAS on
    machine.video_mut().write_port(0x3C0, 9); // palette[1] = 9
    machine.advance_devices(400_000); // complete the frame
    let raster = machine.vga_raster().expect("a frame presented");
    let w = raster.width as usize;
    // The principle: a contiguous top region uses the old palette (DAC 1) and a
    // lower region uses the new palette (DAC 9), separated by the beam row at
    // the time of the palette change. Scan for that transition rather than
    // hard-coding the split row, so the test survives small timing drift.
    assert_eq!(raster.pixels[0], 1, "top of frame uses the old palette");
    let height = raster.height as usize;
    let mut split = None;
    for row in 0..height {
        let p = raster.pixels[row * w];
        if p == 9 {
            split = Some(row);
            break;
        }
        assert_eq!(p, 1, "row {row} above the split must use the old palette");
    }
    let split = split.expect("a row using the new palette exists below the split");
    // The split must land in the active region (200 raster rows of content),
    // not at the very top or beyond the visible area.
    assert!(
        (1..200).contains(&split),
        "split row {split} should fall inside the active picture"
    );
    // Every active row at or below the split uses the new palette.
    for row in split..200 {
        assert_eq!(
            raster.pixels[row * w],
            9,
            "row {row} below the split must use the new palette"
        );
    }
}

#[test]
fn line_compare_split_through_the_machine() {
    let mut machine = test_machine();
    machine.set_vga_mode_0dh(); // double-scanned byte mode
    // A0000 writes fill plane 0 with a full bit mask, write mode 0 (reset default).
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
    // Mark the top of VRAM (plane 0 offset 0) with bit 7 only: pixel 0 set, the rest
    // clear. The split region reads this; a non-uniform byte also detects a
    // wrongly-applied pel-pan below the split.
    machine.write_physical_u8(0x000A_0000, 0x80);
    // Identity attribute palette so index 1 -> DAC 1. read_status1 resets the
    // flip-flop to "index"; 16 entries * 2 writes leaves it in "index" mode.
    machine.video_mut().read_status1();
    for i in 0..16u8 {
        machine.video_mut().write_port(0x3C0, 0x20 | i); // index, PAS on
        machine.video_mut().write_port(0x3C0, i); // value: palette[i] = i
    }
    // Lock pel-pan below the split (Attribute Mode Control 10h bit 5) and pan the
    // top by 4. The flip-flop is in "index" mode here.
    machine.video_mut().write_port(0x3C0, 0x30); // attr index 0x10, PAS on
    machine.video_mut().write_port(0x3C0, 0x20); // bit 5: pel-pan up to line compare
    machine.video_mut().write_port(0x3C0, 0x33); // attr index 0x13, PAS on
    machine.video_mut().write_port(0x3C0, 0x04); // pan 4
    // Program a split at scan-counter line 100. The mode default line compare is
    // 0x3FF, so the overflow (07h) bit 8 and max-scan (09h) bit 9 must be cleared.
    // The 09h write touches only line compare bit 9, not the double-scan bit.
    machine.video_mut().write_port(0x3D4, 0x07);
    machine.video_mut().write_port(0x3D5, 0x00); // line compare bit 8 = 0
    machine.video_mut().write_port(0x3D4, 0x09);
    machine.video_mut().write_port(0x3D5, 0x00); // line compare bit 9 = 0
    machine.video_mut().write_port(0x3D4, 0x18);
    machine.video_mut().write_port(0x3D5, 0x64); // line compare low 8 bits = 100
    // Scroll the top region to a cleared area of VRAM (start address 0x4000),
    // buffered until the next vertical retrace.
    machine.video_mut().write_port(0x3D4, 0x0C);
    machine.video_mut().write_port(0x3D5, 0x40); // start address high
    machine.video_mut().write_port(0x3D4, 0x0D);
    machine.video_mut().write_port(0x3D5, 0x00); // start address low
    // First frame latches the buffered start address; the second renders with it.
    machine.advance_devices(400_000);
    machine.advance_devices(400_000);
    let raster = machine.vga_raster().expect("a frame presented");
    let w = raster.width as usize; // 320
    // A top scanline (50 < 100) reads the scrolled, cleared region: index 0.
    assert_eq!(
        raster.pixels[50 * w],
        0,
        "top region is scrolled to cleared VRAM"
    );
    assert_eq!(
        raster.pixels[101 * w],
        0,
        "EGA keeps two extra scanlines in the top region"
    );
    // The first EGA split scanline (103 = line_compare + 3) reads offset 0
    // (the marked byte), with pel-pan forced to 0 below the split.
    assert_eq!(
        raster.pixels[103 * w],
        1,
        "split region reads offset 0 with pel-pan forced to 0"
    );
}

#[test]
fn display_address_wrap_seam_through_the_machine() {
    let mut machine = test_machine();
    machine.set_vga_mode_0dh(); // byte mode
    // Plane 0 datapath: map mask plane 0, full bit mask, write mode 0 (reset default).
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x01);
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF);
    // Mark the top of VRAM: plane 0 offset 0 = 0xFF (pixels 0..7 -> attribute index 1).
    machine.write_physical_u8(0x000A_0000, 0xFF);
    // Identity palette so index 1 -> DAC 1.
    machine.video_mut().read_status1(); // reset attr flip-flop to index
    for i in 0..16u8 {
        machine.video_mut().write_port(0x3C0, 0x20 | i);
        machine.video_mut().write_port(0x3C0, i);
    }
    // Set start_address = 0xFFF8 through the CRTC ports (buffered until vretrace).
    machine.video_mut().write_port(0x3D4, 0x0C); // start address high
    machine.video_mut().write_port(0x3D5, 0xFF);
    machine.video_mut().write_port(0x3D4, 0x0D); // start address low
    machine.video_mut().write_port(0x3D5, 0xF8);
    // First frame latches the buffered start address; the second renders with it.
    machine.advance_devices(400_000);
    machine.advance_devices(400_000);
    let raster = machine.vga_raster().expect("a frame presented");
    let w = raster.width as usize; // 320
    // Row 0: pixels 0..63 read 0xFFF8..0xFFFF (clear), pixels 64..71 wrap to offset 0.
    assert_eq!(raster.pixels[0], 0, "pre-wrap pixel reads the cleared tail");
    assert_eq!(
        raster.pixels[64], 1,
        "wrapped scanout pixel equals the top-of-VRAM pixel (no tear)"
    );
    // Sanity: still on row 0 of the active area.
    assert!(w >= 72);
}

#[test]
fn frame_generation_tracks_graphics_writes() {
    let mut machine = test_machine();

    // Text mode (the power-up default) is never memoized: cursor/attribute blink
    // toggles with no guest write, so the gen must be None so the GUI keeps
    // re-rendering.
    assert_eq!(machine.video().active_mode(), VideoMode::Text);
    assert_eq!(
        machine.frame_generation(),
        None,
        "text mode is not memoizable (blink)"
    );

    // A graphics mode (mode 13h) is a pure function of guest writes, so it gets a
    // generation key.
    machine.video_mut().set_mode13h();
    assert_eq!(machine.video().active_mode(), VideoMode::Mode13h);
    let gen0 = machine
        .frame_generation()
        .expect("graphics mode has a generation");

    // Stable across repeated calls with no intervening writes (so a static screen
    // stays a cache hit).
    assert_eq!(
        machine.frame_generation(),
        Some(gen0),
        "no write -> same generation"
    );
    assert_eq!(
        machine.frame_generation(),
        Some(gen0),
        "still stable on a third call"
    );

    // A write into the VGA memory aperture changes the key (the framebuffer moved).
    machine.write_physical_u8(0xA0000, 0x2A);
    let gen1 = machine.frame_generation().expect("still graphics");
    assert_ne!(gen1, gen0, "a VRAM write bumps the generation");

    // ...and is stable again afterward.
    assert_eq!(
        machine.frame_generation(),
        Some(gen1),
        "stable after the VRAM write"
    );

    // A VGA register / DAC port write (a palette change is the classic graphics-mode
    // output change with no VRAM write) changes the key. 0x3C8/0x3C9 are the DAC
    // write-index / data ports.
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3C8, BusWidth::Byte, 0x00, false).unwrap(); // DAC write index 0
        bus.write_io(0x3C9, BusWidth::Byte, 0x3F, false).unwrap(); // red component
    }
    let gen2 = machine.frame_generation().expect("still graphics");
    assert_ne!(gen2, gen1, "a VGA port write bumps the generation");

    // A mode / resolution change always moves the key (the raster dims are folded
    // into the key).
    assert!(machine.set_vga_mode(0x12)); // 640x480 planar (raster 640x525)
    assert_eq!(machine.video().active_mode(), VideoMode::Planar);
    let gen3 = machine.frame_generation().expect("planar is graphics");
    assert_ne!(gen3, gen2, "a resolution change moves the generation");

    // Returning to text mode drops back to None.
    machine.video_mut().set_text_mode();
    assert_eq!(machine.video().active_mode(), VideoMode::Text);
    assert_eq!(
        machine.frame_generation(),
        None,
        "back in text mode -> not memoizable"
    );
}

// Regression: the HLE BIOS INT 10h graphics services mutate `self.video` directly
// (bypassing the CPU bus), so the content generation must live inside the Vga
// mutators, not on the bus, or a BIOS-drawing program would be frozen by the cache.
// Each sub-case stays in an ALREADY-established graphics mode (same dims before and
// after the BIOS call) so the dims fold cannot mask a missing bump.
#[test]
fn frame_generation_tracks_same_dims_mode_switch() {
    // Mode 13h and mode 0Dh are both 320x449 raster, so the dimension fold in
    // frame_generation cannot tell them apart. A program switching between them
    // (INT 10h AH=00h, no intervening VRAM write) must still move the key, or the
    // host frame cache would freeze on the switch. The mode-set helpers bump the
    // content gen to cover this.
    let mut machine = int15_machine(16);
    machine.video_mut().set_mode13h();
    assert_eq!(machine.video().active_mode(), VideoMode::Mode13h);
    let dims_before = (
        machine.video().raster_width(),
        machine.video().raster_height(),
    );
    let before = machine
        .frame_generation()
        .expect("mode 13h is a graphics mode");

    assert!(machine.video_mut().set_mode(0x0D)); // 320x200x16 planar, same raster dims
    let dims_after = (
        machine.video().raster_width(),
        machine.video().raster_height(),
    );
    assert_eq!(
        dims_before, dims_after,
        "13h and 0Dh share raster dims, so the dims fold cannot move the key"
    );
    let after = machine
        .frame_generation()
        .expect("mode 0Dh is a graphics mode");
    assert_ne!(
        after, before,
        "a same-dims graphics-to-graphics mode switch must still bump the generation"
    );
}

#[test]
fn frame_generation_tracks_hle_bios_graphics_writes() {
    // Mode 13h, INT 10h AH=0Ch WRITE PIXEL (AL=color, CX=col, DX=row, BH=page).
    let mut machine = int15_machine(16);
    machine.video_mut().set_mode13h();
    assert_eq!(machine.video().active_mode(), VideoMode::Mode13h);
    let before = machine
        .frame_generation()
        .expect("mode 13h is a graphics mode");

    machine.cpu.registers.set_eax(0x0C2A); // AH=0Ch, AL=0x2A
    machine.cpu.registers.set_ebx(0x0000); // BH=page 0
    machine.cpu.registers.set_ecx(10); // column
    machine.cpu.registers.set_edx(20); // row
    machine.handle_int10();
    let after = machine.frame_generation().expect("still mode 13h");
    assert_ne!(
        after, before,
        "INT 10h AH=0Ch write-pixel must bump the generation (HLE bypasses the bus)"
    );

    // CGA graphics (mode 04h), INT 10h AH=0Eh TELETYPE — draws a glyph as pixels.
    let mut cga = int15_machine(16);
    cga.cpu.registers.set_eax(0x0004); // set CGA graphics mode 04h
    cga.handle_int10();
    assert_eq!(cga.video().active_mode(), VideoMode::Cga);
    let dims_before = (cga.video().raster_width(), cga.video().raster_height());
    let before = cga
        .frame_generation()
        .expect("CGA graphics has a generation");

    cga.cpu.registers.set_eax(0x0E41); // AH=0Eh TTY, AL='A'
    cga.cpu.registers.set_ebx(0x0002); // BH=page 0, BL=color 2
    cga.handle_int10();
    let dims_after = (cga.video().raster_width(), cga.video().raster_height());
    assert_eq!(
        dims_before, dims_after,
        "dims unchanged, so the dims fold can't mask the bump"
    );
    let after = cga.frame_generation().expect("still CGA graphics");
    assert_ne!(
        after, before,
        "INT 10h AH=0Eh teletype in CGA graphics must bump the generation"
    );

    // A palette change via INT 10h AH=10h AL=10h (set one DAC register) in mode 13h
    // — the classic graphics output change with no VRAM write — must bump too.
    let mut pal = int15_machine(16);
    pal.video_mut().set_mode13h();
    let before = pal.frame_generation().expect("mode 13h graphics");
    pal.cpu.registers.set_eax(0x1010); // AH=10h, AL=10h set DAC register
    pal.cpu.registers.set_ebx(0x0005); // BX = DAC index 5
    pal.cpu.registers.set_ecx(0x3F00); // CH=green, CL=blue
    pal.cpu.registers.set_edx(0x3F00); // DH=red
    pal.handle_int10();
    let after = pal.frame_generation().expect("still mode 13h");
    assert_ne!(
        after, before,
        "INT 10h AH=10h palette write must bump the generation"
    );
}

#[test]
fn set_vga_mode_selects_graphics_geometry_per_number() {
    let mut machine = test_machine();

    assert!(machine.set_vga_mode(0x0E));
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    assert_eq!(machine.video().raster_width(), 640);
    assert_eq!(machine.video().raster_height(), 449);

    assert!(machine.set_vga_mode(0x12));
    assert_eq!(machine.video().raster_width(), 640);
    assert_eq!(machine.video().raster_height(), 525);

    assert!(machine.set_vga_mode(0x13));
    assert_eq!(machine.video().active_mode(), VideoMode::Mode13h);
    assert_eq!(machine.video().raster_width(), 320);
    assert_eq!(machine.video().raster_height(), 449);

    assert!(!machine.set_vga_mode(0x99));
}

#[test]
fn int10_paradise_special_mode_selects_existing_mode() {
    let mut machine = int15_machine(16);

    machine.cpu.registers.set_eax(0x007E);
    machine.cpu.registers.set_ebx(320);
    machine.cpu.registers.set_ecx(200);
    machine.cpu.registers.set_edx(256);
    machine.handle_int10();
    assert_eq!((machine.cpu.registers.eax() as u16) & 0x00FF, 0x007E);
    assert_eq!(((machine.cpu.registers.ebx() as u16) >> 8) as u8, 0x7E);
    assert_eq!(machine.video().active_mode(), VideoMode::Mode13h);
    assert_eq!(machine.read_physical_u8(0x449), 0x13);

    machine.cpu.registers.set_eax(0x007E);
    machine.cpu.registers.set_ebx(800);
    machine.cpu.registers.set_ecx(600);
    machine.cpu.registers.set_edx(16);
    machine.handle_int10();
    assert_eq!(((machine.cpu.registers.ebx() as u16) >> 8) as u8, 0x00);
}

#[test]
fn int10_paradise_extended_status_and_registers() {
    let mut machine = int15_machine(16);

    machine.cpu.registers.set_eax(0x007F);
    machine.cpu.registers.set_ebx(0x0A5A);
    machine.handle_int10();
    assert_eq!(((machine.cpu.registers.ebx() as u16) >> 8) as u8, 0x7F);

    machine.cpu.registers.set_eax(0x007F);
    machine.cpu.registers.set_ebx(0x1A00);
    machine.handle_int10();
    assert_eq!(((machine.cpu.registers.ebx() as u16) >> 8) as u8, 0x7F);
    assert_eq!((machine.cpu.registers.ebx() as u16) & 0x00FF, 0x005A);

    machine.cpu.registers.set_eax(0x007F);
    machine.cpu.registers.set_ebx(0x0100);
    machine.handle_int10();
    machine.cpu.registers.set_eax(0x007F);
    machine.cpu.registers.set_ebx(0x0200);
    machine.handle_int10();
    assert_eq!(((machine.cpu.registers.ebx() as u16) >> 8) as u8, 0x7F);
    assert_eq!((machine.cpu.registers.ebx() as u16) & 0x00FF, 0x0001);
    assert_eq!(machine.cpu.registers.ecx() as u16, 0x0401);

    machine.cpu.registers.set_eax(0x007F);
    machine.cpu.registers.set_ebx(0x0700);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(0x449), 0x03);
    machine.cpu.registers.set_eax(0x007F);
    machine.cpu.registers.set_ebx(0x0200);
    machine.handle_int10();
    assert_eq!((machine.cpu.registers.ebx() as u16) & 0x00FF, 0x0000);

    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
    machine.cpu.registers.set_edi(0x0100);
    machine.write_physical_u8(0x40100, 0xFF);
    machine.cpu.registers.set_eax(0x007F);
    machine.cpu.registers.set_ebx(0x6100);
    machine.handle_int10();
    assert_eq!(((machine.cpu.registers.ebx() as u16) >> 8) as u8, 0x7F);
    assert_eq!(machine.read_physical_u8(0x40100), 0x00);
}

#[test]
fn int10_ega_modes_publish_bda_geometry() {
    let mut machine = int15_machine(16);

    for (mode, height, page_size) in [
        (0x0D, 8, 0x2000),
        (0x0E, 8, 0x4000),
        (0x0F, 14, 0x8000),
        (0x10, 14, 0x8000),
        (0x11, 16, 0x0000),
        (0x12, 16, 0x0000),
    ] {
        machine.cpu.registers.set_eax(mode);
        machine.handle_int10();
        assert_eq!(machine.read_physical_u8(0x485), height, "mode {mode:02X}");
        assert_eq!(
            machine.read_physical_u16(0x44C),
            page_size,
            "mode {mode:02X}"
        );
    }
}

#[test]
fn int10_sets_mode_12h_then_draws_and_presents_640x480() {
    // mov ax, 0012h; int 10h; hlt
    let rom = rom_with_code(&[0xb8, 0x12, 0x00, 0xcd, 0x10, 0xf4]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    assert_eq!(machine.video().raster_width(), 640);
    assert_eq!(machine.video().raster_height(), 525);

    // Draw attribute index 1 into the first byte of plane 0 (first 8 pixels of
    // the top row) through the A0000 datapath, with an identity palette.
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
    machine.write_physical_u8(0x000A_0000, 0xFF);
    machine.video_mut().read_status1(); // reset attr flip-flop to index
    for i in 0..16u8 {
        machine.video_mut().write_port(0x3C0, 0x20 | i); // index, PAS on
        machine.video_mut().write_port(0x3C0, i); // palette[i] = i
    }

    // A 12h frame is 800 * 525 = 420 000 dots; 600 000 clocks (~604 000 dots)
    // completes at least one frame.
    machine.advance_devices(600_000);
    let raster = machine.vga_raster().expect("a frame presented");
    assert_eq!(raster.width, 640);
    assert_eq!(raster.height, 525);
    assert_eq!(raster.pixels[0], 1, "top-left pixel is attribute index 1");
}

#[test]
fn int10_sets_ega_mode_0fh_through_planar_dispatch() {
    // mov ax,000Fh; int 10h; hlt
    let rom = rom_with_code(&[0xb8, 0x0f, 0x00, 0xcd, 0x10, 0xf4]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    assert_eq!(machine.video().active_mode(), VideoMode::Planar);
    assert_eq!(machine.video().raster_width(), 640);
    assert_eq!(machine.video().raster_height(), 449);
    assert_eq!(machine.read_physical_u8(0x449), 0x0f);
    assert_eq!(machine.read_physical_u16(0x463), 0x03B4);

    {
        let mut bus = machine.make_bus();
        bus.write_io(0x3B4, BusWidth::Byte, 0x0C, false).unwrap();
        bus.write_io(0x3B5, BusWidth::Byte, 0x12, false).unwrap();
        bus.write_io(0x3B4, BusWidth::Byte, 0x0D, false).unwrap();
        bus.write_io(0x3B5, BusWidth::Byte, 0x34, false).unwrap();
        assert!(bus.read_io(0x3BA, BusWidth::Byte, 0, false).is_ok());
    }
    assert_eq!(machine.video().pending_start_address(), Some(0x1234));
}

#[test]
fn int10_vga_graphics_modes_honor_clear_and_preserve_flag() {
    let mut machine = int15_machine(16);

    machine.cpu.registers.set_eax(0x0013);
    machine.handle_int10();
    machine.write_physical_u8(VGA_MODE13H_BASE, 0x5a);
    machine.cpu.registers.set_eax(0x0093);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(VGA_MODE13H_BASE), 0x5a);
    machine.cpu.registers.set_eax(0x0013);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(VGA_MODE13H_BASE), 0x00);

    machine.cpu.registers.set_eax(0x0010);
    machine.handle_int10();
    machine.video_mut().cpu_write(0, 0xa5);
    assert_eq!(machine.video().plane_byte(0, 0), 0xa5);
    machine.cpu.registers.set_eax(0x0090);
    machine.handle_int10();
    assert_eq!(machine.video().plane_byte(0, 0), 0xa5);
    machine.cpu.registers.set_eax(0x0010);
    machine.handle_int10();
    assert_eq!(machine.video().plane_byte(0, 0), 0x00);
}

#[test]
fn int10_returns_to_text_mode() {
    // mov ax,0013h; int 10h; mov ax,0003h; int 10h; hlt
    let rom = rom_with_code(&[
        0xb8, 0x13, 0x00, 0xcd, 0x10, 0xb8, 0x03, 0x00, 0xcd, 0x10, 0xf4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    // Stamp a recognizable pattern into the text buffer before the toggles.
    machine.video_mut().write_u8(0, b'X').unwrap();
    machine.video_mut().write_u8(1, 0x4e).unwrap();
    machine
        .video_mut()
        .write_u8(VGA_TEXT_MEMORY_SIZE - 2, b'Y')
        .unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // Returning to text hands the display back to the VGA core text path
    // (now a raster) and clears the Margo latch.
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    assert_eq!(machine.video().active_mode(), VideoMode::Text);
    // set_text_mode blanks the buffer to spaces with the 0x07 attribute.
    assert_eq!(machine.video().read_u8(0).unwrap(), b' ');
    assert_eq!(machine.video().read_u8(1).unwrap(), 0x07);
    assert_eq!(
        machine.video().read_u8(VGA_TEXT_MEMORY_SIZE - 2).unwrap(),
        b' '
    );
}

#[test]
fn int10_0bh_sets_border_overscan() {
    // mov ax,0b00h; mov bx,0005h; int 10h; hlt  (AH=0Bh, BH=0 border, BL=5)
    let rom = rom_with_code(&[0xb8, 0x00, 0x0b, 0xbb, 0x05, 0x00, 0xcd, 0x10, 0xf4]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.video().overscan(), 5);
}

#[test]
fn int10_0bh_sets_cga_background_and_palette() {
    // mode 04h; AH=0Bh/BH=0 background blue + high intensity; AH=0Bh/BH=1 palette 1.
    let rom = rom_with_code(&[
        0xb8, 0x04, 0x00, 0xcd, 0x10, 0xb8, 0x00, 0x0b, 0xbb, 0x11, 0x00, 0xcd, 0x10, 0xb8, 0x00,
        0x0b, 0xbb, 0x01, 0x01, 0xcd, 0x10, 0xf4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.video().active_mode(), VideoMode::Cga);
    assert_eq!(machine.memory.read_u16(0x44c).unwrap(), 0x4000);
    machine.write_physical_u8(VGA_TEXT_BASE, 0b00_01_10_11);
    let raster = machine.video_mut().render_full_frame();
    assert_eq!(&raster.pixels[0..4], &[1, 11, 13, 15]);
}

#[test]
fn int10_1003_toggles_cga_text_blink_bit() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x0001);
    machine.handle_int10();
    assert_ne!(machine.video().cga_mode_control() & 0x20, 0);

    machine.cpu.registers.set_eax(0x1003);
    machine.cpu.registers.set_ebx(0x0000);
    machine.handle_int10();
    assert_eq!(machine.video().cga_mode_control() & 0x20, 0);

    machine.cpu.registers.set_eax(0x1003);
    machine.cpu.registers.set_ebx(0x0001);
    machine.handle_int10();
    assert_ne!(machine.video().cga_mode_control() & 0x20, 0);
}

#[test]
fn int10_cga_bda_latches_track_bios_control_writes() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x0006);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(0x465), 0x1A);
    assert_eq!(machine.read_physical_u8(0x466), 0x0F);

    machine.cpu.registers.set_eax(0x0B00);
    machine.cpu.registers.set_ebx(0x0011);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(0x466), 0x11);

    machine.cpu.registers.set_eax(0x0B00);
    machine.cpu.registers.set_ebx(0x0101);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(0x466), 0x31);

    machine.cpu.registers.set_eax(0x0002);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(0x465), 0x2D);

    machine.cpu.registers.set_eax(0x1003);
    machine.cpu.registers.set_ebx(0x0000);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(0x465), 0x0D);
}

#[test]
fn int10_non_cga_mode_set_clears_cga_bda_latches() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x0006);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(0x465), 0x1A);
    assert_eq!(machine.read_physical_u8(0x466), 0x0F);

    machine.cpu.registers.set_eax(0x000D);
    machine.handle_int10();

    assert_eq!(machine.read_physical_u8(0x465), 0);
    assert_eq!(machine.read_physical_u8(0x466), 0);
    machine.cpu.registers.set_eax(0x1B00);
    machine.handle_int10();
    assert_eq!(machine.read_physical_u8(0x20), 0);
    assert_eq!(machine.read_physical_u8(0x21), 0);
}

#[test]
fn int10_ah05_sets_the_text_page_via_start_address() {
    // mov ax,0501h; int 10h; hlt  (AH=05h, AL=1 -> display page 1)
    let rom = rom_with_code(&[0xb8, 0x01, 0x05, 0xcd, 0x10, 0xf4]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // Page 1 sits at byte 4096 = cell 2048. AH=05h routes through
    // set_start_address (the vretrace latch), so the value is buffered in
    // pending_start before the next frame boundary applies it.
    assert_eq!(
        machine.video().pending_start_address(),
        Some(2048),
        "AH=05h page 1 buffers start address 2048 (cell)"
    );
    assert_eq!(
        machine.video().crtc_start_address(),
        0,
        "start address applies at the next vretrace, not mid-frame"
    );
}

#[test]
fn int10_ah05_uses_40_column_page_stride() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x0001);
    machine.handle_int10();
    machine.cpu.registers.set_eax(0x0501);
    machine.handle_int10();

    assert_eq!(machine.video().pending_start_address(), Some(1024));
    assert_eq!(machine.read_physical_u8(0x462), 1);
    assert_eq!(machine.memory.read_u16(0x44e).unwrap(), 2048);

    machine.cpu.registers.set_eax(0x0F00);
    machine.cpu.registers.set_ebx(0);
    machine.handle_int10();
    assert_eq!((machine.cpu.registers.ebx() >> 8) as u8, 1);
}

#[test]
fn int10_text_services_use_the_selected_cga_text_page() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x0001);
    machine.handle_int10();
    machine.cpu.registers.set_eax(0x0501);
    machine.handle_int10();

    machine.cpu.registers.set_eax(0x0200); // cursor page 1, row 0 col 0
    machine.cpu.registers.set_ebx(0x0100);
    machine.cpu.registers.set_edx(0);
    machine.handle_int10();
    machine.cpu.registers.set_eax(0x0950); // write 'P'/attr 1E on page 1
    machine.cpu.registers.set_ebx(0x011E);
    machine.cpu.registers.set_ecx(1);
    machine.handle_int10();

    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b' ');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE + 2048), b'P');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE + 2049), 0x1E);

    machine.cpu.registers.set_eax(0x0800);
    machine.cpu.registers.set_ebx(0x0100);
    machine.handle_int10();
    assert_eq!(machine.cpu.registers.eax() as u16, 0x1E50);

    machine.cpu.registers.set_eax(0x0300);
    machine.cpu.registers.set_ebx(0x0100);
    machine.handle_int10();
    assert_eq!(machine.cpu.registers.edx() as u16, 0);
}

#[test]
fn int10_mode02_wraps_display_pages_at_the_cga_16kb_window() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x0002);
    machine.handle_int10();

    machine.cpu.registers.set_eax(0x0503);
    machine.handle_int10();
    assert_eq!(machine.video().pending_start_address(), Some(0x1800));
    assert_eq!(machine.read_physical_u8(0x462), 3);
    assert_eq!(machine.memory.read_u16(0x44e).unwrap(), 0x3000);

    machine.cpu.registers.set_eax(0x0504);
    machine.handle_int10();
    assert_eq!(machine.video().pending_start_address(), Some(0));
    assert_eq!(machine.read_physical_u8(0x462), 0);
    assert_eq!(machine.memory.read_u16(0x44e).unwrap(), 0);
}

#[test]
fn int10_ah05_ignores_cga_graphics_single_page() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x0004);
    machine.handle_int10();
    machine.video_mut().cga_write(0, 0b01_01_01_01);

    machine.cpu.registers.set_eax(0x0501);
    machine.handle_int10();

    assert_eq!(machine.video().pending_start_address(), None);
    assert_eq!(machine.video().crtc_start_address(), 0);
    assert_eq!(machine.read_physical_u8(0x462), 0);
    assert_eq!(&machine.video().render_cga_row(0)[0..4], &[2, 2, 2, 2]);
}

#[test]
fn int10_ah05_selects_ega_graphics_display_page() {
    let mut machine = int15_machine(16);
    machine.cpu.registers.set_eax(0x000D);
    machine.handle_int10();
    machine.write_physical_u8(0x000A_0000 + 0x2000, 0x80);

    machine.cpu.registers.set_eax(0x0501);
    machine.handle_int10();

    assert_eq!(machine.video().pending_start_address(), Some(0x2000));
    assert_eq!(machine.read_physical_u8(0x462), 1);
    assert_eq!(machine.memory.read_u16(0x44e).unwrap(), 0x2000);

    machine.advance_devices(600_000);
    let raster = machine.video_mut().render_full_frame();
    assert_eq!(raster.pixels[0], 0x17);

    machine.cpu.registers.set_eax(0x0012);
    machine.handle_int10();
    machine.cpu.registers.set_eax(0x0501);
    machine.handle_int10();

    assert_eq!(machine.video().pending_start_address(), Some(0));
    assert_eq!(machine.read_physical_u8(0x462), 0);
    assert_eq!(machine.memory.read_u16(0x44e).unwrap(), 0);
}

#[test]
fn int10_ah05_page_flip_scrolls_through_the_machine() {
    // Drive a full AH=05h page flip end-to-end: pre-seed page 0 and page 1
    // with distinct glyphs, call the BIOS service for page 1, run a frame
    // so the latch applies, and confirm the pixel scanout reads page 1.
    //   mov ax,0501h ; AH=05h, AL=1 (display page 1)
    //   int 10h
    //   hlt
    let rom = rom_with_code(&[0xb8, 0x01, 0x05, 0xcd, 0x10, 0xf4]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    // Page 0 cell 0 = 'A'; page 1 cell 0 (cell 2048, byte 4096) = 'Z'.
    let video = machine.video_mut();
    video.write_u8(0, b'A').unwrap();
    video.write_u8(1, 0x0F).unwrap();
    video.write_u8(4096, b'Z').unwrap();
    video.write_u8(4097, 0x0F).unwrap();

    machine.run_until_halt_or_cycles(1_000_000).unwrap();

    // The latch is buffered; the start address has not applied yet.
    let video = machine.video_mut();
    assert_eq!(
        video.frame().cells[0].character,
        b'A',
        "before vretrace the displayed page is still 0"
    );
    // Advance one frame so finalize_frame applies the buffered start address.
    let dots = video.frame_dots();
    video.advance(dots);
    assert_eq!(
        video.frame().cells[0].character,
        b'Z',
        "after vretrace the displayed page scrolls to page 1"
    );
}

#[test]
fn int10_10h_sets_palette_register() {
    // mov ax,1000h; mov bx,0901h; int 10h; hlt  (AH=10h AL=00, BL=1, BH=9)
    let rom = rom_with_code(&[0xb8, 0x00, 0x10, 0xbb, 0x01, 0x09, 0xcd, 0x10, 0xf4]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.video().attr_palette_reg(1), 9);
}

#[test]
fn int10_10h_sets_individual_dac() {
    // mov ax,1010h; mov bx,0028h; mov dx,3f00h; mov cx,0000h; int 10h; hlt
    // (AH=10h AL=10, BX=40, DH=63 R, CH=0 G, CL=0 B)
    let rom = rom_with_code(&[
        0xb8, 0x10, 0x10, 0xbb, 0x28, 0x00, 0xba, 0x00, 0x3f, 0xb9, 0x00, 0x00, 0xcd, 0x10, 0xf4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.video().dac_entry(40), [63, 0, 0]);
}

#[test]
fn int10_10h_sets_dac_block() {
    // ES:DX -> a 3-triple buffer at 1000:0000 (physical 0x10000).
    // mov ax,1000h; mov es,ax; mov dx,0; mov ax,1012h; mov bx,000ah; mov cx,3; int 10h; hlt
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x10, 0x8e, 0xc0, 0xba, 0x00, 0x00, 0xb8, 0x12, 0x10, 0xbb, 0x0a, 0x00, 0xb9,
        0x03, 0x00, 0xcd, 0x10, 0xf4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    // The three triples at 0x10000: red, green, blue.
    for (i, &b) in [63u8, 0, 0, 0, 63, 0, 0, 0, 63].iter().enumerate() {
        machine.write_physical_u8(0x1_0000 + i as u32, b);
    }

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.video().dac_entry(10), [63, 0, 0]);
    assert_eq!(machine.video().dac_entry(11), [0, 63, 0]);
    assert_eq!(machine.video().dac_entry(12), [0, 0, 63]);
}

#[test]
fn int10_10h_gets_dac_block() {
    // AL=17 reads CX DAC entries starting at BX into ES:DX.
    // mov ax,1000h; mov es,ax; mov dx,0; mov ax,1017h; mov bx,000ah; mov cx,3; int 10h; hlt
    let rom = rom_with_code(&[
        0xb8, 0x00, 0x10, 0x8e, 0xc0, 0xba, 0x00, 0x00, 0xb8, 0x17, 0x10, 0xbb, 0x0a, 0x00, 0xb9,
        0x03, 0x00, 0xcd, 0x10, 0xf4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();

    // Seed DAC entries 10/11/12 with known values, then let the readback run.
    machine.video_mut().set_dac_entry(10, 12, 34, 56);
    machine.video_mut().set_dac_entry(11, 1, 2, 3);
    machine.video_mut().set_dac_entry(12, 63, 63, 63);

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // The handler wrote CX*3 bytes to 0x10000.
    assert_eq!(machine.read_physical_u8(0x1_0000), 12);
    assert_eq!(machine.read_physical_u8(0x1_0001), 34);
    assert_eq!(machine.read_physical_u8(0x1_0002), 56);
    assert_eq!(machine.read_physical_u8(0x1_0006), 63);
    assert_eq!(machine.read_physical_u8(0x1_0007), 63);
    assert_eq!(machine.read_physical_u8(0x1_0008), 63);
}

#[test]
fn int10_10h_reads_overscan() {
    // AL=01 sets the overscan to BH=0x2A, then AL=08 reads it back into BH.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x1001);
    m.cpu.registers.set_ebx(0x2A00); // BH = 0x2A
    m.handle_int10();
    m.cpu.registers.set_eax(0x1008);
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int10();
    assert_eq!((m.cpu.registers.ebx() as u16 >> 8) as u8, 0x2A);
}

#[test]
fn int10_1001_sets_cga_graphics_intensity() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    m.cpu.registers.set_eax(0x1001);
    m.cpu.registers.set_ebx(0x1100);
    m.handle_int10();

    m.write_physical_u8(VGA_TEXT_BASE, 0b00_01_10_11);
    let raster = m.video_mut().render_full_frame();
    assert_eq!(&raster.pixels[0..4], &[1, 10, 12, 14]);
}

#[test]
fn int10_1000_11_sets_cga_overscan_register() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();

    m.cpu.registers.set_eax(0x1000);
    m.cpu.registers.set_ebx(0x1111);
    m.handle_int10();

    m.cpu.registers.set_eax(0x1007);
    m.cpu.registers.set_ebx(0x0011);
    m.handle_int10();
    assert_eq!((m.cpu.registers.ebx() >> 8) as u8, 0x11);

    m.write_physical_u8(VGA_TEXT_BASE, 0b00_01_10_11);
    let raster = m.video_mut().render_full_frame();
    assert_eq!(&raster.pixels[0..4], &[1, 10, 12, 14]);
}

#[test]
fn int10_10h_reads_cga_color_select_low_bits() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004);
    m.handle_int10();
    assert!(m.video_mut().write_port(0x3D9, 0x3F));

    m.cpu.registers.set_eax(0x1008);
    m.cpu.registers.set_ebx(0);
    m.handle_int10();
    assert_eq!((m.cpu.registers.ebx() >> 8) as u8, 0x1F);

    m.cpu.registers.set_eax(0x1009);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x1000));
    m.cpu.registers.set_edx(0);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x1_0010), 0x1F);
}

#[test]
fn int10_10h_reads_all_palette_registers() {
    // AL=09 writes the 16 palette registers + overscan to ES:DX. Mode 03h
    // starts from the VGABios text Attribute Controller table, followed by
    // overscan 0.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x1009);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x1000));
    m.cpu.registers.set_edx(0x0000);
    m.handle_int10();
    let expected = [
        0x00u8, 0x01, 0x02, 0x03, 0x04, 0x05, 0x14, 0x07, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E,
        0x3F,
    ];
    for (i, value) in expected.into_iter().enumerate() {
        assert_eq!(m.read_physical_u8(0x1_0000 + i as u32), value);
    }
    assert_eq!(
        m.read_physical_u8(0x1_0010),
        0,
        "overscan trails the 16 regs"
    );
}

#[test]
fn int10_10h_sums_dac_block_to_gray() {
    // AL=1B sums BX..BX+CX DAC entries to gray with NTSC luma weights.
    let mut m = int15_machine(16);
    m.video_mut().set_dac_entry(5, 63, 0, 0); // pure red
    m.video_mut().set_dac_entry(6, 0, 63, 0); // pure green
    m.cpu.registers.set_eax(0x101B);
    m.cpu.registers.set_ebx(0x0005); // start at index 5
    m.cpu.registers.set_ecx(0x0002); // two entries
    m.handle_int10();
    // Red gray = 63*77>>8 = 18; green gray = 63*151>>8 = 37. Each entry is now
    // an equal-component gray.
    let [r5, g5, b5] = m.video().dac_entry(5);
    assert_eq!((r5, g5, b5), (18, 18, 18));
    let [r6, g6, b6] = m.video().dac_entry(6);
    assert_eq!((r6, g6, b6), (37, 37, 37));
}

#[test]
fn int10_10h_reads_dac_page_state_default() {
    // AL=1A reports the power-up DAC paging state: mode 0 (BL), page 0 (BH).
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x101A);
    m.cpu.registers.set_ebx(0xFFFF);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0000);
}

#[test]
fn int10_10h_sets_and_reads_pel_mask() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x1018);
    m.cpu.registers.set_ebx(0x120F);
    m.handle_int10();
    assert_eq!(m.video_mut().read_port(0x3C6), Some(0x0F));

    m.cpu.registers.set_eax(0x1019);
    m.cpu.registers.set_ebx(0xAB00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0xAB0F);
}

#[test]
fn int10_10h_selects_and_reports_dac_color_pages() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();

    // Attribute palette register 1 selects DAC low bits 5, then a pixel with
    // colour 1 scans out through the colour-page state below.
    m.cpu.registers.set_eax(0x1000);
    m.cpu.registers.set_ebx(0x0501);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0C01);
    m.cpu.registers.set_ebx(0);
    m.cpu.registers.set_ecx(0);
    m.cpu.registers.set_edx(0);
    m.handle_int10();

    // Mode 0: four 64-colour pages. Page 3 supplies DAC bits 7-6.
    m.cpu.registers.set_eax(0x1013);
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int10();
    m.cpu.registers.set_eax(0x1013);
    m.cpu.registers.set_ebx(0x0301);
    m.handle_int10();
    m.cpu.registers.set_eax(0x101A);
    m.cpu.registers.set_ebx(0xFFFF);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0300);
    assert_eq!(m.video_mut().render_full_frame().pixels[0], 0xC5);

    // Mode 1: sixteen 16-colour pages. Page 6 supplies DAC bits 7-4.
    m.cpu.registers.set_eax(0x1013);
    m.cpu.registers.set_ebx(0x0100);
    m.handle_int10();
    m.cpu.registers.set_eax(0x1013);
    m.cpu.registers.set_ebx(0x0601);
    m.handle_int10();
    m.cpu.registers.set_eax(0x101A);
    m.cpu.registers.set_ebx(0);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0601);
    assert_eq!(m.video_mut().render_full_frame().pixels[0], 0x65);
}

#[test]
fn overlay_color_key_gates_on_the_primary_pixel() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x14a); // 640x480x32, pitch 2560
    // Primary at (10, 20) holds the key; (11, 20) holds an occluding window pixel.
    let key = 0x0011_2233u32;
    let occluder = 0x0044_5566u32;
    let p0 = 20 * 2560 + 10 * 4;
    let p1 = 20 * 2560 + 11 * 4;
    for (i, b) in key.to_le_bytes().into_iter().enumerate() {
        machine.write_physical_u8(MARGO_LFB_BASE + p0 + i as u32, b);
    }
    for (i, b) in occluder.to_le_bytes().into_iter().enumerate() {
        machine.write_physical_u8(MARGO_LFB_BASE + p1 + i as u32, b);
    }
    // YUY2 source: Y0=235 (white), Y1=16 (black).
    let src = 0x0020_0000u32;
    machine.write_physical_u8(MARGO_LFB_BASE + src, 235);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 1, 128);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 2, 16);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 3, 128);

    write_mmio_reg(&mut machine, 0x44, src);
    write_mmio_reg(&mut machine, 0x48, 4);
    write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 2);
    write_mmio_reg(&mut machine, 0x58, (20 << 16) | 10);
    write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 2);
    write_mmio_reg(&mut machine, 0x60, key); // OVL_COLORKEY
    write_mmio_reg(&mut machine, 0x40, 1 | (1 << 3)); // ENABLE + KEY_EN, FORMAT YUY2

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // Where the primary equals the key, the overlay shows (white).
    assert_eq!(argb[20 * 640 + 10], 0x00ff_ffff);
    // Where another value occludes the key, the overlay is hidden and the
    // decoded primary pixel (0x00445566 in X8R8G8B8) remains.
    assert_eq!(argb[20 * 640 + 11], 0x0044_5566);
}

#[test]
fn overlay_yuy2_composites_through_the_apertures() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x14a); // 640x480x32
    // One YUY2 group offscreen (2 MiB in, past the 32bpp visible surface):
    // Y0=235 (white), U=128, Y1=16 (black), V=128. Byte order Y0, U, Y1, V.
    let src = 0x0020_0000u32;
    machine.write_physical_u8(MARGO_LFB_BASE + src, 235);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 1, 128);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 2, 16);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 3, 128);

    write_mmio_reg(&mut machine, 0x44, src); // OVL_SRC_Y (packed surface)
    write_mmio_reg(&mut machine, 0x48, 4); // OVL_SRC_PITCH
    write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 2); // OVL_SRC_DIM: w=2, h=1
    write_mmio_reg(&mut machine, 0x58, (20 << 16) | 10); // OVL_DST_XY: x=10, y=20
    write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 2); // OVL_DST_DIM: w=2, h=1 (1:1)
    write_mmio_reg(&mut machine, 0x40, 1); // OVL_CTRL: ENABLE, FORMAT YUY2, no key

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    assert_eq!(argb[20 * 640 + 10], 0x00ff_ffff); // Y0 -> white
    assert_eq!(argb[20 * 640 + 11], 0x0000_0000); // Y1 -> black
}

#[test]
fn overlay_scales_by_point_sampling() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x14a);
    // The same one YUY2 group, scaled 2x horizontally: dst width 4.
    let src = 0x0020_0000u32;
    machine.write_physical_u8(MARGO_LFB_BASE + src, 235);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 1, 128);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 2, 16);
    machine.write_physical_u8(MARGO_LFB_BASE + src + 3, 128);

    write_mmio_reg(&mut machine, 0x44, src);
    write_mmio_reg(&mut machine, 0x48, 4);
    write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 2); // src w=2, h=1
    write_mmio_reg(&mut machine, 0x58, (20 << 16) | 10);
    write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 4); // dst w=4, h=1 (2x)
    write_mmio_reg(&mut machine, 0x40, 1);

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // sx = dx * src_w / dst_w = dx * 2 / 4 = dx / 2:
    // dst 0,1 sample src pixel 0 (white); dst 2,3 sample src pixel 1 (black).
    assert_eq!(argb[20 * 640 + 10], 0x00ff_ffff);
    assert_eq!(argb[20 * 640 + 11], 0x00ff_ffff);
    assert_eq!(argb[20 * 640 + 12], 0x0000_0000);
    assert_eq!(argb[20 * 640 + 13], 0x0000_0000);
}

#[test]
fn overlay_yv12_upsamples_chroma_through_the_apertures() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x14a); // 640x480x32
    // YV12 source, 2x2. Y plane (pitch 2): [16, 235; 16, 235]. A single shared
    // chroma sample (U=128, V=255) covers the whole 2x2 block (4:2:0 upsample).
    let yp = 0x0020_0000u32;
    let up = 0x0020_1000u32;
    let vp = 0x0020_2000u32;
    machine.write_physical_u8(MARGO_LFB_BASE + yp, 16); // (0,0)
    machine.write_physical_u8(MARGO_LFB_BASE + yp + 1, 235); // (1,0)
    machine.write_physical_u8(MARGO_LFB_BASE + yp + 2, 16); // (0,1)
    machine.write_physical_u8(MARGO_LFB_BASE + yp + 3, 235); // (1,1)
    machine.write_physical_u8(MARGO_LFB_BASE + up, 128); // U plane
    machine.write_physical_u8(MARGO_LFB_BASE + vp, 255); // V plane

    write_mmio_reg(&mut machine, 0x44, yp); // OVL_SRC_Y
    write_mmio_reg(&mut machine, 0x48, 2); // OVL_SRC_PITCH (Y plane)
    write_mmio_reg(&mut machine, 0x4c, (2 << 16) | 2); // OVL_SRC_DIM: 2x2
    write_mmio_reg(&mut machine, 0x50, up); // OVL_SRC_U
    write_mmio_reg(&mut machine, 0x54, vp); // OVL_SRC_V
    write_mmio_reg(&mut machine, 0x58, (20 << 16) | 10); // OVL_DST_XY
    write_mmio_reg(&mut machine, 0x5c, (2 << 16) | 2); // OVL_DST_DIM: 2x2 (1:1)
    write_mmio_reg(&mut machine, 0x40, 1 | (1 << 1)); // ENABLE + FORMAT YV12

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // Y=16 with (U=128, V=255) -> 0x00cb0000; Y=235 -> 0x00ff98ff. The same
    // chroma sample applies across the 2x2 block.
    assert_eq!(argb[20 * 640 + 10], 0x00cb_0000);
    assert_eq!(argb[20 * 640 + 11], 0x00ff_98ff);
    assert_eq!(argb[21 * 640 + 10], 0x00cb_0000);
    assert_eq!(argb[21 * 640 + 11], 0x00ff_98ff);
}

#[test]
fn overlay_yv12_chroma_traversal_addresses_each_cell() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x14a); // 640x480x32
    // 4x4 YV12 source with a flat Y of 128, so each output pixel's color is set
    // solely by which 2x2 chroma cell it samples. The 2x2 chroma grid (chroma
    // pitch = Y pitch / 2 = 2) holds a distinct (U, V) per cell, so this proves
    // cx = sx/2, cy = sy/2, and the chroma-plane stride, which the 2x2 test (only
    // cell 0,0) does not exercise.
    let yp = 0x0020_0000u32;
    let up = 0x0020_1000u32;
    let vp = 0x0020_2000u32;
    for i in 0..16u32 {
        machine.write_physical_u8(MARGO_LFB_BASE + yp + i, 128);
    }
    // Chroma cells indexed cy * 2 + cx.
    let us = [128u8, 128, 255, 255];
    let vs = [128u8, 255, 128, 255];
    for i in 0..4u32 {
        machine.write_physical_u8(MARGO_LFB_BASE + up + i, us[i as usize]);
        machine.write_physical_u8(MARGO_LFB_BASE + vp + i, vs[i as usize]);
    }

    write_mmio_reg(&mut machine, 0x44, yp); // OVL_SRC_Y
    write_mmio_reg(&mut machine, 0x48, 4); // OVL_SRC_PITCH (Y plane)
    write_mmio_reg(&mut machine, 0x4c, (4 << 16) | 4); // OVL_SRC_DIM: 4x4
    write_mmio_reg(&mut machine, 0x50, up); // OVL_SRC_U
    write_mmio_reg(&mut machine, 0x54, vp); // OVL_SRC_V
    write_mmio_reg(&mut machine, 0x58, (20 << 16) | 10); // OVL_DST_XY
    write_mmio_reg(&mut machine, 0x5c, (4 << 16) | 4); // OVL_DST_DIM: 4x4 (1:1)
    write_mmio_reg(&mut machine, 0x40, 1 | (1 << 1)); // ENABLE + FORMAT YV12

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // Cell (0,0) U=128 V=128 -> gray; two pixels in the same cell share it.
    assert_eq!(argb[20 * 640 + 10], 0x0082_8282);
    assert_eq!(argb[21 * 640 + 11], 0x0082_8282);
    // Cell (1,0) U=128 V=255.
    assert_eq!(argb[20 * 640 + 12], 0x00ff_1b82);
    // Cell (0,1) U=255 V=128.
    assert_eq!(argb[22 * 640 + 10], 0x0082_51ff);
    // Cell (1,1) U=255 V=255.
    assert_eq!(argb[22 * 640 + 12], 0x00ff_00ff);
}

#[test]
fn pusher_runs_a_fill_packet_from_the_ring() {
    let mut machine = test_machine();
    // A command ring in system RAM that issues one FILL: a 2x2 rect of 0xAB at
    // (x=1, y=1) on a depth-1 surface, pitch 8, base 0. Mirrors the guide's
    // fill_via_pusher: header words are (count << 16) | method.
    let ring_base = 0x0001_0000u32;
    let ring: [u32; 16] = [
        (3 << 16) | 0x0100,
        0, // DST_BASE = 0
        8, // DST_PITCH = 8
        0, // SRC_BASE = 0 (unused by FILL)
        (1 << 16) | 0x0110,
        1, // DEPTH = 1
        (1 << 16) | 0x0114,
        (1 << 16) | 1, // DST_XY: y=1, x=1
        (1 << 16) | 0x011c,
        (2 << 16) | 2, // DIM: h=2, w=2
        (1 << 16) | 0x0120,
        0xab, // FG_COLOR = 0xAB
        (1 << 16) | 0x0128,
        0xf0, // ROP = PATCOPY
        (1 << 16) | 0x0150,
        0x01, // COMMAND = FILL
    ];
    for (i, word) in ring.iter().enumerate() {
        for (b, byte) in word.to_le_bytes().into_iter().enumerate() {
            machine.write_physical_u8(ring_base + (i * 4 + b) as u32, byte);
        }
    }
    let put = (ring.len() * 4) as u32; // 64

    write_mmio_reg(&mut machine, 0x84, ring_base); // PUSH_BASE
    write_mmio_reg(&mut machine, 0x88, 0x1000); // PUSH_SIZE (4 KiB, power of two)
    write_mmio_reg(&mut machine, 0x80, 1); // PUSH_CTRL = ENABLE
    write_mmio_reg(&mut machine, 0x8c, put); // PUSH_PUT = doorbell

    // One device tick drives the pump; the FILL applies immediately.
    machine.advance_devices(1);

    // The fill landed in VRAM (read back through the LFB).
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 8 + 1), 0xab); // (1,1)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2 * 8 + 2), 0xab); // (2,2)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE), 0x00); // (0,0) untouched
    // The ring drained: GET reached PUT.
    assert_eq!(read_mmio_reg(&mut machine, 0x90), put);
}

#[test]
fn pusher_does_not_spin_on_a_malformed_ring() {
    let mut machine = test_machine();
    // A non-power-of-two size with a PUT that the (get + 4) % size orbit never
    // reaches, over zeroed RAM (every header decodes to method 0, count 0, so no
    // COMMAND ever sets busy_ns). Without the word budget this would spin forever.
    write_mmio_reg(&mut machine, 0x84, 0x0001_0000); // PUSH_BASE
    write_mmio_reg(&mut machine, 0x88, 10); // PUSH_SIZE: not a multiple of 4
    write_mmio_reg(&mut machine, 0x80, 1); // PUSH_CTRL = ENABLE
    write_mmio_reg(&mut machine, 0x8c, 1); // PUSH_PUT = 1 (never on the orbit)

    // Must return rather than hang. GET stays within the ring.
    machine.advance_devices(1);
    assert!(read_mmio_reg(&mut machine, 0x90) < 10);
}

#[test]
fn pusher_get_trails_put_until_commands_complete() {
    let mut machine = test_machine();
    // Two single-pixel FILLs in the ring. Common setup (DST_BASE, DST_PITCH,
    // DEPTH, ROP) first, then per-fill DST_XY, DIM, FG_COLOR, COMMAND: 0xAA at
    // (1,1) and 0xBB at (3,3). Header words are (count << 16) | method.
    let ring_base = 0x0001_0000u32;
    let ring: [u32; 23] = [
        // Common setup: 7 words.
        (2 << 16) | 0x0100,
        0, // DST_BASE = 0
        8, // DST_PITCH = 8
        (1 << 16) | 0x0110,
        1, // DEPTH = 1
        (1 << 16) | 0x0128,
        0xf0, // ROP = PATCOPY
        // Fill 1: 8 words (cumulative 15 words = 60 bytes after this).
        (1 << 16) | 0x0114,
        (1 << 16) | 1, // DST_XY: y=1, x=1
        (1 << 16) | 0x011c,
        (1 << 16) | 1, // DIM: h=1, w=1
        (1 << 16) | 0x0120,
        0xaa, // FG_COLOR = 0xAA
        (1 << 16) | 0x0150,
        0x01, // COMMAND = FILL
        // Fill 2: 8 words (cumulative 23 words = 92 bytes = PUT).
        (1 << 16) | 0x0114,
        (3 << 16) | 3, // DST_XY: y=3, x=3
        (1 << 16) | 0x011c,
        (1 << 16) | 1, // DIM: h=1, w=1
        (1 << 16) | 0x0120,
        0xbb, // FG_COLOR = 0xBB
        (1 << 16) | 0x0150,
        0x01, // COMMAND = FILL
    ];
    for (i, word) in ring.iter().enumerate() {
        for (b, byte) in word.to_le_bytes().into_iter().enumerate() {
            machine.write_physical_u8(ring_base + (i * 4 + b) as u32, byte);
        }
    }
    let put = (ring.len() * 4) as u32; // 92
    let after_fill1 = 15 * 4u32; // 60: offset just past fill 1's COMMAND packet

    write_mmio_reg(&mut machine, 0x84, ring_base); // PUSH_BASE
    write_mmio_reg(&mut machine, 0x88, 0x1000); // PUSH_SIZE
    write_mmio_reg(&mut machine, 0x80, 1); // PUSH_CTRL = ENABLE
    write_mmio_reg(&mut machine, 0x8c, put); // PUSH_PUT = doorbell

    // One tick: the pump consumes the setup plus fill 1, which sets busy_ns and
    // stalls the pump. GET trails PUT, fill 1 landed, fill 2 has not run yet.
    machine.advance_devices(1);
    assert_eq!(read_mmio_reg(&mut machine, 0x90), after_fill1); // GET lags PUT
    assert_ne!(read_mmio_reg(&mut machine, 0x90), put);
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 8 + 1), 0xaa); // (1,1)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 3 * 8 + 3), 0x00); // (3,3) not yet

    // Enough ticks to drain fill 1's busy_ns (a 1-pixel fill is 105 ns; 10
    // clocks at 22 MHz = ~454 ns), letting the pump consume fill 2.
    machine.advance_devices(10);
    assert_eq!(read_mmio_reg(&mut machine, 0x90), put); // GET caught up
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 3 * 8 + 3), 0xbb); // (3,3) now
}

#[test]
fn pusher_streams_color_expand_data_through_the_ring() {
    let mut machine = test_machine();
    // The pusher arms COLOR_EXPAND_DATA and then streams its MONO_DATA words from
    // the ring. This works only because the pump gates on busy_ns (arming leaves
    // busy_ns at 0, so the pump keeps feeding the stream) rather than STATUS.BUSY.
    // An 8x2 glyph at (0,0), depth 1, pitch 8, FG 0xAB, BG 0x00, ROP SRCCOPY: row
    // 0 bits 0xA0 (x=0,2 set), row 1 bits 0x50 (x=1,3 set); MONO_DATA is MSB-first
    // in the high byte. Each MONO_DATA word is its own packet (the port is a single
    // register at 0x0160, so a count>1 run would scatter to 0x0164 and beyond).
    let ring_base = 0x0001_0000u32;
    let ring: [u32; 22] = [
        (2 << 16) | 0x0100,
        0, // DST_BASE = 0
        8, // DST_PITCH = 8
        (1 << 16) | 0x0110,
        1, // DEPTH = 1
        (1 << 16) | 0x0114,
        0, // DST_XY = (0, 0)
        (1 << 16) | 0x011c,
        (2 << 16) | 8, // DIM: h=2, w=8
        (2 << 16) | 0x0120,
        0xab, // FG_COLOR
        0x00, // BG_COLOR
        (1 << 16) | 0x0128,
        0xcc, // ROP = SRCCOPY (S = expanded pixel)
        (1 << 16) | 0x0130,
        0, // FLAGS = 0 (clear bits painted with BG)
        (1 << 16) | 0x0150,
        0x03, // COMMAND = COLOR_EXPAND_DATA (arms the stream; no busy_ns yet)
        (1 << 16) | 0x0160,
        0xa000_0000, // MONO_DATA row 0: bits 0xA0 in the high byte
        (1 << 16) | 0x0160,
        0x5000_0000, // MONO_DATA row 1: bits 0x50 in the high byte
    ];
    for (i, word) in ring.iter().enumerate() {
        for (b, byte) in word.to_le_bytes().into_iter().enumerate() {
            machine.write_physical_u8(ring_base + (i * 4 + b) as u32, byte);
        }
    }
    let put = (ring.len() * 4) as u32; // 88

    write_mmio_reg(&mut machine, 0x84, ring_base); // PUSH_BASE
    write_mmio_reg(&mut machine, 0x88, 0x1000); // PUSH_SIZE
    write_mmio_reg(&mut machine, 0x80, 1); // PUSH_CTRL = ENABLE
    write_mmio_reg(&mut machine, 0x8c, put); // PUSH_PUT = doorbell

    machine.advance_devices(1);

    // Row 0: set bits at x=0,2 -> 0xAB; clear bits -> 0x00 (BG).
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE), 0xab); // (0,0)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 1), 0x00); // (1,0)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 2), 0xab); // (2,0)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 3), 0x00); // (3,0)
    // Row 1: set bits at x=1,3 -> 0xAB.
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 8), 0x00); // (0,1)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 9), 0xab); // (1,1)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 10), 0x00); // (2,1)
    assert_eq!(machine.read_physical_u8(MARGO_LFB_BASE + 11), 0xab); // (3,1)
    // The whole ring drained.
    assert_eq!(read_mmio_reg(&mut machine, 0x90), put);
}

#[test]
fn mode_x_a0000_writes_route_to_the_planar_datapath() {
    let mut machine = test_machine();
    // Mode 13h then unchained (chain-4 off).
    machine.video_mut().set_mode13h();
    machine.video_mut().write_port(0x3C4, 0x04);
    machine.video_mut().write_port(0x3C5, 0x06);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    // Map mask = plane 2, full bit mask, write mode 0 (reset default).
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x04); // plane 2
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
    machine.write_physical_u8(0x000A_0000 + 5, 0x9C);
    assert_eq!(machine.video().plane_byte(2, 5), 0x9C);
    // An offset past the old 64000-byte mode-13h cap is reachable in the 64 KB
    // unchained planar window.
    machine.write_physical_u8(0x000A_0000 + 0xFB00, 0x3C);
    assert_eq!(machine.video().plane_byte(2, 0xFB00), 0x3C);
    // Read back through the bus read path: select plane 2 as the read-map source,
    // then the A0000 reads return the bytes written above (proving cpu_read routes
    // through the 64 KB window too, including past the old 64000-byte cap).
    machine.video_mut().write_port(0x3CE, 0x04); // GC Read Map Select
    machine.video_mut().write_port(0x3CF, 0x02); // plane 2
    assert_eq!(machine.read_physical_u8(0x000A_0000 + 5), 0x9C);
    assert_eq!(machine.read_physical_u8(0x000A_0000 + 0xFB00), 0x3C);
}

#[test]
fn gc06_moved_aperture_routes_graphics_access_to_the_vga() {
    // Mode 13h programs GC06 to the standard 64 KB A0000 graphics window, so
    // an A0000 write lands in the chain-4 plane (offset 6 -> plane 2,
    // plane-offset 1). Then move the aperture to the 32 KB B8000 window and
    // confirm a B8000 access now routes to the VGA, while the default A0000
    // path stays exactly as it was.
    let mut machine = test_machine();
    machine.video_mut().set_mode13h();
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);

    // Default aperture: A0000 access routes to the chain-4 datapath unchanged.
    machine.write_physical_u8(0x000A_0000 + 6, 0xA5);
    assert_eq!(
        machine.video().plane_byte(2, 1),
        0xA5,
        "default A0000 window still routes to the VGA"
    );

    // Move the aperture to B8000 (GC06 memory map select = 0b11, a 32 KB
    // window): write index 06h then value 0b1100.
    machine.video_mut().write_port(0x3CE, 0x06);
    machine.video_mut().write_port(0x3CF, 0b1100);
    let ap = machine.video().gfx_aperture();
    assert_eq!((ap.base, ap.length), (0x000B_8000, 0x0000_8000));

    // A B8000 access in the moved window routes to the VGA chain-4 datapath.
    // Offset 10 -> plane 10 & 3 = 2, plane-offset 10 >> 2 = 2.
    machine.write_physical_u8(0x000B_8000 + 10, 0x7E);
    assert_eq!(
        machine.video().plane_byte(2, 2),
        0x7E,
        "the moved B8000 window routes to the VGA, not the text buffer"
    );
    // Read-back through the moved window returns the byte from the plane.
    assert_eq!(machine.read_physical_u8(0x000B_8000 + 10), 0x7E);
}

#[test]
fn gc06_map_select_00_routes_the_128kb_graphics_aperture() {
    let mut machine = test_machine();
    machine.video_mut().set_mode13h();
    machine.video_mut().write_port(0x3CE, 0x06);
    machine.video_mut().write_port(0x3CF, 0x01); // graphics, A0000-BFFFF

    machine.write_physical_u8(VGA_TEXT_BASE + 10, 0x6D);

    let mirrored_offset = 0x8000 + 10;
    assert_eq!(
        machine
            .video()
            .plane_byte(mirrored_offset & 3, mirrored_offset >> 2),
        0x6D,
        "B8000 in map-select 00 routes through the mirrored VGA graphics window"
    );
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE + 10), 0x6D);
    assert_eq!(
        machine.read_physical_u8(VGA_MODE13H_BASE + mirrored_offset as u32),
        0x6D,
        "the second 64 KB host half mirrors the same plane window"
    );
}

#[test]
fn gc06_default_aperture_keeps_text_routing_at_b8000() {
    // In text mode the B8000 window is the character buffer regardless of GC06;
    // the moved-aperture routing only applies to graphics modes. Writing a
    // char/attr pair at B8000 must reach the text buffer, not a VGA plane.
    let mut machine = test_machine();
    machine.write_physical_u8(VGA_TEXT_BASE, b'Z');
    machine.write_physical_u8(VGA_TEXT_BASE + 1, 0x0F);
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE), b'Z');
    assert_eq!(machine.read_physical_u8(VGA_TEXT_BASE + 1), 0x0F);
}

#[test]
fn mode_x_320x240_through_the_machine() {
    let mut machine = test_machine();
    // Mode 13h, then unchained mode X.
    machine.video_mut().set_mode13h();
    machine.video_mut().write_port(0x3C4, 0x04);
    machine.video_mut().write_port(0x3C5, 0x06);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    // Abrash's 320x240 vertical timing through the CRTC ports.
    for (idx, val) in [
        (0x06u8, 0x0Du8),
        (0x07, 0x3E),
        (0x09, 0x41),
        (0x10, 0xEA),
        (0x11, 0xAC),
        (0x12, 0xDF),
        (0x15, 0xE7),
        (0x16, 0x06),
    ] {
        machine.video_mut().write_port(0x3D4, idx);
        machine.video_mut().write_port(0x3D5, val);
    }
    // Draw a pixel at column 6: plane 6 & 3 = 2, plane offset 6 >> 2 = 1.
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x04); // map mask = plane 2
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
    machine.write_physical_u8(0x000A_0000 + 1, 0xC2); // plane 2, offset 1; bits 6-7 set prove no 6-bit mask
    // Complete a frame (mode-X 320x240 frame is ~421 600 dots; 500 000 clocks is
    // ~503 500 dots, enough to cross one frame and present).
    machine.advance_devices(500_000);
    let raster = machine.vga_raster().expect("a frame presented");
    assert_eq!(raster.width, 320);
    assert_eq!(raster.height, 527, "320x240 vertical total");
    // Column 6 of row 0 scans out the drawn 0xC2, as the 8-bit DAC index directly.
    assert_eq!(
        raster.pixels[6], 0xC2,
        "mode-X pixel scans out at its column with its full 8-bit value"
    );
}

#[test]
fn mode_x_line_compare_split_through_the_machine() {
    let mut machine = test_machine();
    // Mode 13h, then unchained mode X.
    machine.video_mut().set_mode13h();
    machine.video_mut().write_port(0x3C4, 0x04);
    machine.video_mut().write_port(0x3C5, 0x06);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    // Abrash's 320x240 vertical timing through the CRTC ports (Black Book Listing
    // 47.1): double-scanned, 240 source rows over 480 scanlines.
    for (idx, val) in [
        (0x06u8, 0x0Du8),
        (0x07, 0x3E),
        (0x09, 0x41),
        (0x10, 0xEA),
        (0x11, 0xAC),
        (0x12, 0xDF),
        (0x15, 0xE7),
        (0x16, 0x06),
    ] {
        machine.video_mut().write_port(0x3D4, idx);
        machine.video_mut().write_port(0x3D5, val);
    }
    // Program a split at scan-counter line 200. The 320x240 bang sets 07h bit 4
    // (line-compare bit 8) and 09h bit 6 (line-compare bit 9); rewrite both with
    // their other overflow / max-scan bits intact but those two line-compare bits
    // clear, then the low byte. The kept bits reproduce vtotal 527, vdisp_end 480
    // and keep double-scan on; only line-compare bits 8 and 9 are forced to 0.
    machine.video_mut().write_port(0x3D4, 0x07);
    machine.video_mut().write_port(0x3D5, 0x2E); // overflow minus line-compare bit 8
    machine.video_mut().write_port(0x3D4, 0x09);
    machine.video_mut().write_port(0x3D5, 0x01); // max scan 1 (double-scan), bit 6 clear
    machine.video_mut().write_port(0x3D4, 0x18);
    machine.video_mut().write_port(0x3D5, 0xC8); // line compare low 8 = 200
    // Mark the status panel: plane 0, offset 0 (pixel 0 of any scanline reading
    // offset 0). 0xC2 has bits above 0x3F set, proving the 8-bit DAC index is read
    // directly with no attribute 6-bit mask.
    machine.video_mut().write_port(0x3C4, 0x02);
    machine.video_mut().write_port(0x3C5, 0x01); // map mask = plane 0
    machine.video_mut().write_port(0x3CE, 0x08);
    machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF
    machine.write_physical_u8(0x000A_0000, 0xC2);
    // Scroll the top region to cleared VRAM, buffered until the next vertical
    // retrace. Two frame periods: the first latches the start address, the second
    // renders with it (the vretrace latch is exercised the same way as the 16-color
    // split test).
    machine.video_mut().write_port(0x3D4, 0x0C);
    machine.video_mut().write_port(0x3D5, 0x40); // start address high = 0x40
    machine.video_mut().write_port(0x3D4, 0x0D);
    machine.video_mut().write_port(0x3D5, 0x00); // start address low = 0x00 -> 0x4000
    machine.advance_devices(500_000);
    machine.advance_devices(500_000);
    let raster = machine.vga_raster().expect("a frame presented");
    assert_eq!(raster.width, 320, "mode-X width");
    let w = raster.width as usize;
    // A top scanline (50 < 200) reads the scrolled, cleared region: 0.
    assert_eq!(
        raster.pixels[50 * w],
        0,
        "top region is scrolled to cleared VRAM"
    );
    // The first split scanline (201 = line_compare + 1) reads offset 0 (the marked
    // status panel), as the full 8-bit DAC index.
    assert_eq!(
        raster.pixels[201 * w],
        0xC2,
        "split region reads offset 0 at the full 8-bit value"
    );
}

#[test]
fn mode_x_pel_pan_smooth_scroll_through_the_machine() {
    let mut machine = test_machine();
    // Mode 13h, then unchained mode X.
    machine.video_mut().set_mode13h();
    machine.video_mut().write_port(0x3C4, 0x04);
    machine.video_mut().write_port(0x3C5, 0x06);
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    // Abrash's 320x240 vertical timing through the CRTC ports (Black Book
    // Listing 47.1): double-scanned, 240 source rows over 480 scanlines.
    for (idx, val) in [
        (0x06u8, 0x0Du8),
        (0x07, 0x3E),
        (0x09, 0x41),
        (0x10, 0xEA),
        (0x11, 0xAC),
        (0x12, 0xDF),
        (0x15, 0xE7),
        (0x16, 0x06),
    ] {
        machine.video_mut().write_port(0x3D4, idx);
        machine.video_mut().write_port(0x3D5, val);
    }
    // Distinct bytes per plane at plane offset 0 (values above 0x3F prove the
    // 8-bit-direct DAC index is scanned out, not masked to 6 bits).
    let plane_byte: [u8; 4] = [0x40, 0x50, 0x60, 0x70];
    for (plane, &val) in plane_byte.iter().enumerate() {
        machine.video_mut().write_port(0x3C4, 0x02);
        machine.video_mut().write_port(0x3C5, 1u8 << plane); // map mask = this plane
        machine.video_mut().write_port(0x3CE, 0x08);
        machine.video_mut().write_port(0x3CF, 0xFF); // bit mask 0xFF, write mode 0
        machine.write_physical_u8(0x000A_0000, val);
    }
    // For each pel-pan 1..3, reset the attribute flip-flop, write AC index 0x13
    // then the pan value, run two frame periods, and assert the leftmost column
    // scans out plane `pan` at plane offset 0: the fine-shifted pixel, not plane 0.
    for pan in 1u8..=3 {
        machine.video_mut().read_status1(); // reset attr flip-flop to index mode
        machine.video_mut().write_port(0x3C0, 0x33); // attr index 0x13, PAS on
        machine.video_mut().write_port(0x3C0, pan); // pel-pan value
        // Pel-pan is live (not latched): it takes effect at the scanline of the
        // write, so the in-progress frame's early rows still hold the prior pan.
        // Two frame periods flush that frame and then render a clean one whose row
        // zero is scanned after the write.
        machine.advance_devices(500_000); // flush the in-progress (mixed-pan) frame
        machine.advance_devices(500_000); // render a full frame with the new pan
        let raster = machine.vga_raster().expect("a frame presented");
        assert_eq!(
            raster.pixels[0], plane_byte[pan as usize],
            "pel-pan {pan} scans out plane {pan} at the leftmost column"
        );
    }
}

#[test]
fn mode13h_320x200_through_the_machine() {
    let mut machine = test_machine();
    // INT 10h AH=00h AL=13h installs chained mode 13h; set_mode13h is its
    // programmatic equivalent (the INT path is proven by
    // int10_mode13h_routes_a000_through_chain4).
    machine.video_mut().set_mode13h();
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    // Chain-4 routes the A0000 byte at offset 6 to plane 6 & 3 = 2 at plane
    // offset 6 >> 2 = 1. 0xC2 has bits above 0x3F, proving no 6-bit mask.
    machine.write_physical_u8(0x000A_0000 + 6, 0xC2);
    // Complete a frame (the standard mode-13h frame is ~359 200 dots; 500 000
    // clocks is ~503 500 dots, enough to cross one frame and present).
    machine.advance_devices(500_000);
    let raster = machine.vga_raster().expect("a frame presented");
    assert_eq!(raster.width, 320);
    assert_eq!(raster.height, 449, "mode-13h vertical total");
    // Column 6 of row 0 scans out the written 0xC2, as the 8-bit DAC index
    // directly.
    assert_eq!(
        raster.pixels[6], 0xC2,
        "mode-13h pixel scans out at its column with its full 8-bit value"
    );
}

#[test]
fn mode13h_pel_pan_smooth_scroll_through_the_machine() {
    let mut machine = test_machine();
    machine.video_mut().set_mode13h();
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    // Chain-4 writes the byte at A0000 offset p straight to plane p at plane
    // offset 0, so four writes at offsets 0..3 mark one distinct byte per plane
    // there (values above 0x3F prove the 8-bit-direct DAC index is scanned out,
    // not masked to 6 bits).
    let plane_byte: [u8; 4] = [0x40, 0x50, 0x60, 0x70];
    for (plane, &val) in plane_byte.iter().enumerate() {
        machine.write_physical_u8(0x000A_0000 + plane as u32, val);
    }
    // For each pel-pan 1..3, reset the attribute flip-flop, write AC index 0x13
    // then the pan value, run two frame periods, and assert the leftmost column
    // scans out plane `pan` at plane offset 0: the fine-shifted pixel.
    for pan in 1u8..=3 {
        machine.video_mut().read_status1(); // reset attr flip-flop to index mode
        machine.video_mut().write_port(0x3C0, 0x33); // attr index 0x13, PAS on
        machine.video_mut().write_port(0x3C0, pan); // pel-pan value
        // Pel-pan is live (not latched): it takes effect at the scanline of the
        // write, so the in-progress frame's early rows still hold the prior pan.
        // Two frame periods flush that frame and then render a clean one whose row
        // zero is scanned after the write.
        machine.advance_devices(500_000); // flush the in-progress (mixed-pan) frame
        machine.advance_devices(500_000); // render a full frame with the new pan
        let raster = machine.vga_raster().expect("a frame presented");
        assert_eq!(
            raster.pixels[0], plane_byte[pan as usize],
            "pel-pan {pan} scans out plane {pan} at the leftmost column"
        );
    }
}

#[test]
fn mode13h_line_compare_split_through_the_machine() {
    let mut machine = test_machine();
    machine.video_mut().set_mode13h();
    assert_eq!(machine.active_display(), ActiveDisplay::VgaRaster);
    // A split at scan-counter line 200, well inside the 400 active scanlines.
    // Preserve the other vertical-timing bits in 07h/09h while clearing the
    // line-compare high bits; those registers are live timing on VGA hardware.
    machine.video_mut().write_port(0x3D4, 0x07);
    let r07 = machine.video_mut().read_port(0x3D5).unwrap_or(0);
    machine.video_mut().write_port(0x3D5, r07 & !0x10); // clear line-compare bit 8
    machine.video_mut().write_port(0x3D4, 0x09);
    let r09 = machine.video_mut().read_port(0x3D5).unwrap_or(0);
    machine.video_mut().write_port(0x3D5, r09 & !0x40); // clear line-compare bit 9
    machine.video_mut().write_port(0x3D4, 0x18);
    machine.video_mut().write_port(0x3D5, 200); // line compare low byte = 200
    // Mark plane 0, offset 0 (pixel 0 of any scanline reading offset 0). 0xC2
    // has bits above 0x3F, proving the 8-bit DAC index is read directly.
    machine.write_physical_u8(0x000A_0000, 0xC2); // chain-4: plane 0, offset 0
    // Scroll the top region to cleared VRAM, buffered until the next vertical
    // retrace. Two frame periods: the first latches the start address, the second
    // renders with it.
    machine.video_mut().write_port(0x3D4, 0x0C);
    machine.video_mut().write_port(0x3D5, 0x40); // start address high = 0x40
    machine.video_mut().write_port(0x3D4, 0x0D);
    machine.video_mut().write_port(0x3D5, 0x00); // start address low -> 0x4000
    machine.advance_devices(500_000);
    machine.advance_devices(500_000);
    let raster = machine.vga_raster().expect("a frame presented");
    assert_eq!(raster.width, 320, "mode-13h width");
    let w = raster.width as usize;
    // A top scanline (50 < 200) reads the scrolled, cleared region: 0.
    assert_eq!(
        raster.pixels[50 * w],
        0,
        "top region is scrolled to cleared VRAM"
    );
    // The first split scanline (201 = line_compare + 1) reads offset 0 (the
    // marked byte), as the full 8-bit DAC index.
    assert_eq!(
        raster.pixels[201 * w],
        0xC2,
        "split region reads offset 0 at the full 8-bit value"
    );
}

#[test]
fn overlay_quantizes_to_16bpp_display_without_dither() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x111); // 640x480x16 (R5G6B5)
    // A uniform gray YUY2 source (Y=130, U=128, V=128 -> yuv_to_argb = 0x858585),
    // 4 pixels (2 packed groups: Y0,U,Y1,V), offscreen at 1 MiB.
    let src = 0x0010_0000u32;
    for g in 0..2u32 {
        let base = src + g * 4;
        machine.write_physical_u8(MARGO_LFB_BASE + base, 130); // Y0
        machine.write_physical_u8(MARGO_LFB_BASE + base + 1, 128); // U
        machine.write_physical_u8(MARGO_LFB_BASE + base + 2, 130); // Y1
        machine.write_physical_u8(MARGO_LFB_BASE + base + 3, 128); // V
    }
    write_mmio_reg(&mut machine, 0x44, src); // OVL_SRC_Y
    write_mmio_reg(&mut machine, 0x48, 8); // OVL_SRC_PITCH
    write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 4); // OVL_SRC_DIM: 4x1
    write_mmio_reg(&mut machine, 0x58, 0); // OVL_DST_XY: (0, 0)
    write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 4); // OVL_DST_DIM: 4x1 (1:1)
    write_mmio_reg(&mut machine, 0x0c, 0); // CONTROL: DITHER_EN off
    write_mmio_reg(&mut machine, 0x40, 1); // OVL_CTRL: ENABLE, YUY2

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // On a 16bpp display the overlay is reduced to R5G6B5 and bit-expanded back:
    // 0x858585 -> 0x848684 (R/B truncate to 0x84, G to 0x86), uniform (no dither).
    for (x, &pixel) in argb.iter().enumerate().take(4) {
        assert_eq!(pixel, 0x0084_8684, "pixel {x}");
    }
}

#[test]
fn overlay_orders_dither_on_a_16bpp_display() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x111); // 640x480x16
    let src = 0x0010_0000u32;
    for g in 0..2u32 {
        let base = src + g * 4;
        machine.write_physical_u8(MARGO_LFB_BASE + base, 130);
        machine.write_physical_u8(MARGO_LFB_BASE + base + 1, 128);
        machine.write_physical_u8(MARGO_LFB_BASE + base + 2, 130);
        machine.write_physical_u8(MARGO_LFB_BASE + base + 3, 128);
    }
    write_mmio_reg(&mut machine, 0x44, src);
    write_mmio_reg(&mut machine, 0x48, 8);
    write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 4);
    write_mmio_reg(&mut machine, 0x58, 0);
    write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 4);
    write_mmio_reg(&mut machine, 0x0c, 0x2); // CONTROL: DITHER_EN on
    write_mmio_reg(&mut machine, 0x40, 1); // OVL_CTRL: ENABLE, YUY2

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // Row 0 Bayer cells are 0, 8, 2, 10. For gray 0x858585 the R/B (5-bit) jump
    // a step where the cell offset pushes 133 past the 17th code: cells 8 and 10
    // dither up to 0x8C, cells 0 and 2 stay at 0x84. G (6-bit) stays 0x86.
    assert_eq!(argb[0], 0x0084_8684); // cell 0
    assert_eq!(argb[1], 0x008c_868c); // cell 8
    assert_eq!(argb[2], 0x0084_8684); // cell 2
    assert_eq!(argb[3], 0x008c_868c); // cell 10
}

#[test]
fn overlay_dithers_on_a_15bpp_display() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x110); // 640x480x15 (X1R5G5B5): all channels 5-bit
    let src = 0x0010_0000u32;
    for g in 0..2u32 {
        let base = src + g * 4;
        machine.write_physical_u8(MARGO_LFB_BASE + base, 130); // Y0
        machine.write_physical_u8(MARGO_LFB_BASE + base + 1, 128); // U
        machine.write_physical_u8(MARGO_LFB_BASE + base + 2, 130); // Y1
        machine.write_physical_u8(MARGO_LFB_BASE + base + 3, 128); // V
    }
    write_mmio_reg(&mut machine, 0x44, src);
    write_mmio_reg(&mut machine, 0x48, 8);
    write_mmio_reg(&mut machine, 0x4c, (1 << 16) | 4);
    write_mmio_reg(&mut machine, 0x58, 0);
    write_mmio_reg(&mut machine, 0x5c, (1 << 16) | 4);
    write_mmio_reg(&mut machine, 0x0c, 0x2); // CONTROL: DITHER_EN on
    write_mmio_reg(&mut machine, 0x40, 1); // OVL_CTRL: ENABLE, YUY2

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // 15bpp makes G 5-bit too (unlike 16bpp's 6-bit G), so a dithered-up pixel is
    // gray 0x8C8C8C, not 0x8C868C. Row 0 cells 0, 8, 2, 10 -> 0x84, 0x8C, 0x84, 0x8C.
    assert_eq!(argb[0], 0x0084_8484); // cell 0: truncated gray
    assert_eq!(argb[1], 0x008c_8c8c); // cell 8: dithered up
    assert_eq!(argb[2], 0x0084_8484); // cell 2
    assert_eq!(argb[3], 0x008c_8c8c); // cell 10
}

#[test]
fn overlay_dither_is_locked_to_screen_position() {
    let mut machine = test_machine();
    machine.margo_mut().set_mode(0x111); // 640x480x16
    // Uniform gray YUY2 source, 4x4 (4 rows x 2 packed groups = 8 groups), offscreen.
    let src = 0x0010_0000u32;
    for g in 0..8u32 {
        let base = src + g * 4;
        machine.write_physical_u8(MARGO_LFB_BASE + base, 130); // Y0
        machine.write_physical_u8(MARGO_LFB_BASE + base + 1, 128); // U
        machine.write_physical_u8(MARGO_LFB_BASE + base + 2, 130); // Y1
        machine.write_physical_u8(MARGO_LFB_BASE + base + 3, 128); // V
    }
    write_mmio_reg(&mut machine, 0x44, src);
    write_mmio_reg(&mut machine, 0x48, 8); // src pitch: 2 groups per row
    write_mmio_reg(&mut machine, 0x4c, (4 << 16) | 4); // OVL_SRC_DIM: 4x4
    write_mmio_reg(&mut machine, 0x58, (2 << 16) | 1); // OVL_DST_XY: x=1, y=2 (non-aligned)
    write_mmio_reg(&mut machine, 0x5c, (4 << 16) | 4); // OVL_DST_DIM: 4x4 (1:1)
    write_mmio_reg(&mut machine, 0x0c, 0x2); // CONTROL: DITHER_EN on
    write_mmio_reg(&mut machine, 0x40, 1); // OVL_CTRL: ENABLE, YUY2

    let palette = machine.palette_argb();
    let argb = machine.margo().scanout_argb(&palette);
    // The dither cell is BAYER[screen_y & 3][screen_x & 3] in ABSOLUTE screen
    // coordinates, not destination-relative. If it were dst-relative, screen (1,2)
    // would be cell 0 (0x848684); screen-locked it is BAYER[2][1] = 11.
    assert_eq!(argb[2 * 640 + 1], 0x008c_868c); // screen (1,2): cell 11
    assert_eq!(argb[2 * 640 + 4], 0x0084_8684); // screen (4,2): cell 3
    assert_eq!(argb[5 * 640 + 2], 0x008c_8a8c); // screen (2,5): cell 14
}

// The EXEC integration fixtures are nasm-assembled .COM programs (nasm 3.01,
// -f bin, org 0x100). Their source is in the comment above each const so the
// bytes are auditable without re-running the assembler.
const PMIRQ5_COM: &[u8] = include_bytes!("../tests/fixtures/pmirq5.com");
const VCPIPIC_COM: &[u8] = include_bytes!("../tests/fixtures/vcpipic.com");

// --- BLASTER environment seeding ---

/// Walk the env block at `seg` back into (KEY, VALUE) pairs, the way a DOS
/// game scans the segment named by PSP:0x2C.
fn parse_env_block(machine: &Machine, seg: u16) -> Vec<(String, String)> {
    let mem = machine.memory();
    let base = usize::from(seg) * 16;
    let mut entries = Vec::new();
    let mut offset = 0usize;
    loop {
        let mut bytes = Vec::new();
        loop {
            let byte = mem.read_u8(base + offset).unwrap();
            offset += 1;
            if byte == 0 {
                break;
            }
            bytes.push(byte);
        }
        if bytes.is_empty() {
            break; // the terminating empty string
        }
        let entry = String::from_utf8(bytes).unwrap();
        let (key, value) = entry.split_once('=').expect("KEY=VALUE");
        entries.push((key.to_string(), value.to_string()));
    }
    entries
}

/// The env-segment pointer the loader wrote into PSP:0x2C, or 0 if unset.
fn psp_env_segment(machine: &Machine) -> u16 {
    machine
        .memory()
        .read_u16(usize::from(DOS_LOAD_SEGMENT) * 16 + 0x2c)
        .unwrap()
}

#[test]
fn sound_blaster_env_entries_default_config() {
    let entries = sound_blaster_env_entries(&SoundBlasterConfig::default());
    assert_eq!(
        entries,
        vec![
            ("BLASTER".to_string(), "A220 I5 D1 H5 T6".to_string()),
            ("SETSOUND".to_string(), "A220 I5 D1 H5 T6".to_string()),
        ]
    );
}

#[test]
fn sound_blaster_env_entries_non_default_routing() {
    let config = SoundBlasterConfig {
        enabled: true,
        irq: SbIrq::I7,
        dma: SbDma8::D3,
        high_dma: SbDma16::D5,
    };
    assert_eq!(
        sound_blaster_env_entries(&config),
        vec![
            ("BLASTER".to_string(), "A220 I7 D3 H5 T6".to_string()),
            ("SETSOUND".to_string(), "A220 I7 D3 H5 T6".to_string()),
        ]
    );
}

#[test]
fn sound_blaster_env_entries_disabled_omits_the_string() {
    let config = SoundBlasterConfig {
        enabled: false,
        ..SoundBlasterConfig::default()
    };
    assert!(sound_blaster_env_entries(&config).is_empty());
}

#[test]
fn new_raw_program_seeds_psp_env_pointer_with_blaster() {
    // A trivial exit-only program is enough: the env is seeded at load.
    let com: &[u8] = &[0xb8, 0x00, 0x4c, 0xcd, 0x21];
    let machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com).unwrap();
    let env_seg = psp_env_segment(&machine);
    assert_ne!(env_seg, 0, "PSP:0x2C must name the env segment");
    // The env data sits one paragraph above the 64 KiB .COM program block
    // (PSP:0x02), past the env block's reserved MCB header.
    let prog_top = machine
        .memory()
        .read_u16(usize::from(DOS_LOAD_SEGMENT) * 16 + 2)
        .unwrap();
    assert_eq!(env_seg, prog_top + 1);
    assert_eq!(
        parse_env_block(&machine, env_seg),
        vec![
            ("BLASTER".to_string(), "A220 I5 D1 H5 T6".to_string()),
            ("SETSOUND".to_string(), "A220 I5 D1 H5 T6".to_string()),
        ]
    );
}

#[test]
fn dos_env_block_carries_the_configured_routing() {
    // A non-default routing (IRQ7 / DMA3) flows from the host config through
    // the loader into the env block a guest scans via PSP:0x2C.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    profile.sound_blaster = SoundBlasterConfig {
        enabled: true,
        irq: SbIrq::I7,
        dma: SbDma8::D3,
        high_dma: SbDma16::D5,
    };
    let machine = Machine::new_raw_program(profile, &[0xb8, 0x00, 0x4c, 0xcd, 0x21]).unwrap();
    let env_seg = psp_env_segment(&machine);
    assert_ne!(env_seg, 0, "PSP:0x2C must name the env segment");
    assert_eq!(
        parse_env_block(&machine, env_seg),
        vec![
            ("BLASTER".to_string(), "A220 I7 D3 H5 T6".to_string()),
            ("SETSOUND".to_string(), "A220 I7 D3 H5 T6".to_string()),
        ]
    );
}

#[test]
fn keyboard_rom_echoes_injected_keys_to_the_screen() {
    let profile = MachineProfile::gsw_386(1, izarravm_core::VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::kbd_bios()).unwrap();
    // Let the ROM run its init (install vectors, unmask IRQ1, STI, enter loop).
    machine.run_until_halt_or_cycles(200_000).unwrap();
    // Inject 'h' then 'i' (Set 1 make+break for H=0x23, I=0x17).
    machine.inject_key_scancodes(&[0x23, 0xa3, 0x17, 0x97]);
    machine.run_until_halt_or_cycles(2_000_000).unwrap();
    let screen = machine.screen_text();
    assert!(
        screen.line_string(0).starts_with("hi"),
        "screen line 0 was {:?}",
        screen.line_string(0)
    );
}

#[test]
fn dos_machine_routes_irq1_to_the_keyboard_isr() {
    // A do-nothing program that just spins (jmp $) so the machine keeps running.
    // org 0x100: jmp $  (EB FE)
    let com: &[u8] = &[0xeb, 0xfe];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com).unwrap();
    machine.inject_key_scancodes(&[0x1e, 0x9e]); // 'a' make + break
    machine.run_until_halt_or_cycles(200_000).unwrap();
    // The real INT 09h ISR should have moved 'a' into the BDA ring.
    let head = machine.memory_read_u16_for_test(0x41a);
    let tail = machine.memory_read_u16_for_test(0x41c);
    assert_ne!(head, tail, "ISR enqueued a key into the BDA ring");
}

#[test]
fn dos_program_reads_typed_keys_through_int21() {
    // org 0x100: read two chars with AH=01 (each echoes to stdout), then exit.
    //   mov ah,1 / int 21h / mov ah,1 / int 21h / mov ax,4c00h / int 21h
    let com: &[u8] = &[
        0xb4, 0x01, 0xcd, 0x21, 0xb4, 0x01, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21,
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), com).unwrap();
    // Type 'h' then 'i' as Set 1 make+break (H=0x23, I=0x17).
    machine.inject_key_scancodes(&[0x23, 0xa3, 0x17, 0x97]);
    let reason = machine.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    assert_eq!(machine.program_output(), b"hi");
}

#[test]
fn tokados_sndtst_delivers_sb_irq5_under_v86() {
    let dir = std::env::temp_dir().join(format!("katea_sndtst_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    machine.set_cmos_byte(0x11, 1); // disk-first
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "SNDTST.COM".to_string(),
                    izarravm_firmware::sndtst_com().to_vec(),
                ),
                (
                    "AUTOEXEC.BAT".to_string(),
                    b"@ECHO OFF\r\nSNDTST\r\n".to_vec(),
                ),
            ],
        )
        .unwrap();

    let reason = machine.run_until_halt_or_cycles(250_000_000).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        reason,
        StopReason::TestExit { code: 0xA5 },
        "SNDTST.COM should complete under TOKAEMM V86, got {reason:?}"
    );
}

#[test]
fn tokados_vcpi_de0b_remaps_sb_irq5_vector() {
    let dir = std::env::temp_dir().join(format!("katea_vcpipic_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    machine.set_cmos_byte(0x11, 1); // disk-first
    machine
        .mount_hdd_folder_with(
            &dir,
            vec![
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                ("VCPIPIC.COM".to_string(), VCPIPIC_COM.to_vec()),
                (
                    "AUTOEXEC.BAT".to_string(),
                    b"@ECHO OFF\r\nVCPIPIC\r\n".to_vec(),
                ),
            ],
        )
        .unwrap();

    let reason = machine.run_until_halt_or_cycles(250_000_000).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        reason,
        StopReason::TestExit { code: 0xA5 },
        "VCPIPIC.COM should receive SB IRQ5 on remapped vector 25h, got {reason:?}"
    );
}

#[test]
fn protected_mode_sb_dma_irq5_reaches_client_idt() {
    for mode in [GswMode::Gsw386, GswMode::Gsw486, GswMode::Gsw586] {
        let mut machine =
            Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), PMIRQ5_COM)
                .unwrap();
        machine.set_mode(mode);
        let reason = machine
            .run_until_halt_or_cycles(mode.clock_hz() / 4)
            .unwrap();
        assert!(
            matches!(reason, StopReason::TestExit { code: 0xA5 }),
            "{mode:?}: protected-mode SB IRQ5 fixture stopped with {reason:?}"
        );
    }
}

#[test]
fn lotura_reports_id_and_switches_mode_live() {
    // org 0x100: mov al,2; out 0xe1,al; mov ax,4c00h; int 21h
    let com: &[u8] = &[0xb0, 0x02, 0xe6, 0xe1, 0xb8, 0x00, 0x4c, 0xcd, 0x21];
    let mut machine = Machine::new_raw_program(
        MachineProfile::gsw_386(16, izarravm_core::VideoCard::Et4000Ax),
        com,
    )
    .unwrap();
    assert_eq!(machine.active_mode(), GswMode::Gsw386); // boot mode
    let id = with_bus(&mut machine, |bus| {
        bus.read_io(0x00e0, BusWidth::Byte, 0, false).unwrap() as u8
    });
    assert_eq!(id, LOTURA_ID_VALUE);
    let code = with_bus(&mut machine, |bus| {
        bus.read_io(0x00e1, BusWidth::Byte, 0, false).unwrap() as u8
    });
    assert_eq!(code, 0);
    // An out-of-range write records no pending switch.
    with_bus(&mut machine, |bus| {
        bus.write_io(0x00e1, BusWidth::Byte, 9, false).unwrap()
    });
    assert!(machine.pending_mode.is_none());
    assert_eq!(machine.active_mode(), GswMode::Gsw386);
    // Running the program writes 2 to 0xE1; the run loop applies the live switch.
    machine.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(machine.active_mode(), GswMode::Gsw586);
    let code = with_bus(&mut machine, |bus| {
        bus.read_io(0x00e1, BusWidth::Byte, 0, false).unwrap() as u8
    });
    assert_eq!(code, 2);
}

// --- Izarra 3000 BIOS foundation ---------------------------------------

#[test]
fn izarra_bios_post_publishes_result_block() {
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    // The full-screen RLE background blit delays the POST step loop to ~10M
    // cycles, so the result block fills out later than the old mode-13h screen.
    let reason = machine.run_until_halt_or_cycles(20_000_000).unwrap();
    // POST completes and the BIOS idles (it keeps running, not halting).
    assert!(matches!(reason, StopReason::CycleLimit { .. }));
    let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
    // The live result builder owns the header: declared count must match the
    // parsed records and the additive checksum must validate (parse succeeded).
    assert_eq!(
        usize::from(results.declared_record_count),
        results.records.len()
    );
    // The suite opens with a BEGIN record and the foundation reference step.
    assert_eq!(
        results.records[0].status,
        izarravm_firmware::SuiteRecordStatus::Begin
    );
    assert_eq!(results.records[0].name, "suite.izarra");
    assert!(results.records.iter().any(|record| {
        record.status == izarravm_firmware::SuiteRecordStatus::Pass
            && record.name == "self.framework"
    }));
    // self.extaccess proves the unreal-mode >1 MiB helpers work in the live BIOS.
    assert!(results.records.iter().any(|record| {
        record.status == izarravm_firmware::SuiteRecordStatus::Pass
            && record.name == "self.extaccess"
    }));
    assert!(results.records.iter().any(|record| {
        record.status == izarravm_firmware::SuiteRecordStatus::Pass
            && record.name == "component.optical_atapi"
    }));
    let cpu = results
        .records
        .iter()
        .position(|record| record.name == "component.cpu_gsw")
        .unwrap();
    let memory = results
        .records
        .iter()
        .position(|record| record.name == "memory.ramtest")
        .unwrap();
    let video = results
        .records
        .iter()
        .position(|record| record.name == "component.video_margo")
        .unwrap();
    assert!(cpu < memory, "CPU POST should run before RAM");
    assert!(video < memory, "VGA POST should run before RAM");
}

#[test]
fn izarra_bios_slow_post_continues_after_ramtest() {
    let profile = MachineProfile::gsw_386(2, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.set_fast_post(false);
    let mut results = None;
    for _ in 0..30 {
        machine.run_until_halt_or_cycles(10_000_000).unwrap();
        let parsed = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        let complete = parsed
            .records
            .iter()
            .any(|record| record.name == "component.optical_atapi");
        results = Some(parsed);
        if complete {
            break;
        }
    }
    let results = results.unwrap();
    assert!(
        results
            .records
            .iter()
            .any(|record| record.name == "component.optical_atapi"),
        "{:?}",
        results.records
    );
}

#[test]
fn izarra_bios_ramtest_esc_skips_and_continues_post() {
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.set_fast_post(false);
    for _ in 0..40 {
        machine.run_until_halt_or_cycles(1_000_000).unwrap();
        let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
        if results
            .records
            .iter()
            .any(|record| record.name == "component.cpu_gsw")
        {
            break;
        }
    }
    machine.inject_key_scancodes(&[0x01]);
    let reason = machine.run_until_halt_or_cycles(100_000_000).unwrap();
    assert!(matches!(reason, StopReason::CycleLimit { .. }));
    let results = izarravm_firmware::parse_result_block(machine.memory().as_slice()).unwrap();
    assert!(
        results
            .records
            .iter()
            .any(|record| record.name == "component.cpu_lotura"),
        "{:?}",
        results.records
    );
}

#[test]
fn izarra_bios_tab_before_ramtest_wins_over_later_del() {
    let profile = MachineProfile::gsw_386(2, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.set_fast_post(false);
    for _ in 0..40 {
        machine.run_until_halt_or_cycles(1_000_000).unwrap();
        if let Ok(results) = izarravm_firmware::parse_result_block(machine.memory().as_slice()) {
            if results
                .records
                .iter()
                .any(|record| record.name == "video.margo_caps")
            {
                break;
            }
        }
    }

    machine.inject_key_scancodes(&[0x0f, 0x8f, 0x53, 0xd3]); // Tab, then Del.

    let mut red = 0;
    for _ in 0..40 {
        machine.run_until_halt_or_cycles(5_000_000).unwrap();
        red = (64..72u32)
            .flat_map(|y| (28..130u32).map(move |x| (x, y)))
            .filter(|&(x, y)| machine.read_physical_u8(MARGO_LFB_BASE + y * 320 + x) == 24)
            .count();
        if red > 20 {
            break;
        }
    }
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    assert!(
        red > 20,
        "Tab should open the boot menu; found {red} red title pixels"
    );
}

#[test]
fn izarra_bios_draws_art_post_screen() {
    // The POST screen is the RLE art (izbios-art.inc): a cream field with the
    // wordmark, mascot and grey component icons baked into the background, plus
    // the top-left header text drawn over it by lfb_text. Pixels are read as raw
    // palette indices from the LFB VRAM at MARGO_LFB_BASE + y*320 + x. The
    // full-screen RLE blit is heavy, so POST needs a large cycle budget.
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    // A clear top-left spot is the cream field; the screen is not monochrome.
    let field = machine.read_physical_u8(MARGO_LFB_BASE + 4 * 320 + 4);
    // The wordmark sits top-right in the art (x 213..303, y 11..60): non-field
    // pixels there prove the background RLE blitted, not just a flat clear.
    let mut wordmark = 0;
    for y in 11..60u32 {
        for x in 213..303u32 {
            if machine.read_physical_u8(MARGO_LFB_BASE + y * 320 + x) != field {
                wordmark += 1;
            }
        }
    }
    assert!(
        wordmark > 100,
        "expected the baked-in wordmark in the background, found {wordmark} non-field pixels"
    );
    // The version line "Izarra-BIOS v3.01 - 1997" renders top-left (y 12..20)
    // via lfb_text; any non-field pixels there are glyphs, guarding the LFB
    // glyph path on the art.
    let mut header = 0;
    for y in 12..20u32 {
        for x in 8..200u32 {
            if machine.read_physical_u8(MARGO_LFB_BASE + y * 320 + x) != field {
                header += 1;
            }
        }
    }
    assert!(
        header > 60,
        "expected the top-left version line, found {header} non-field pixels"
    );
    // The DEL/TAB key hints render in the gap above the icon row (y 134..154,
    // x 8..200), telling the user how to reach setup and the boot menu.
    let mut hints = 0;
    for y in 134..154u32 {
        for x in 8..200u32 {
            if machine.read_physical_u8(MARGO_LFB_BASE + y * 320 + x) != field {
                hints += 1;
            }
        }
    }
    assert!(
        hints > 60,
        "expected the DEL/TAB key hints, found {hints} non-field pixels"
    );
}

#[test]
fn izarra_bios_post_lights_component_icons() {
    // As each wired probe passes, console_step_line blits the colour icon sprite
    // over its grey background icon. The VEGA monitor sprite (cell x 42..66,
    // y 166..192) carries saturated colour bars once lit, whereas the grey icon
    // is near-monochrome. A saturated pixel in the cell after a full POST sweep
    // proves component.video_margo passed and the grey->colour reveal fired.
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
    let (words, _w, _h) = machine.frame_argb();
    let saturated = |x: u32, y: u32| {
        let p = words[(y * 320 + x) as usize];
        let (r, g, b) = ((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff);
        r.max(g).max(b).saturating_sub(r.min(g).min(b)) > 60
    };
    let lit = (42..66u32).any(|x| (166..192u32).any(|y| saturated(x, y)));
    assert!(
        lit,
        "VEGA icon cell never lit to colour — the reveal did not fire"
    );
}

#[test]
fn serial_tx_is_captured_and_lsr_reports_empty() {
    // A write to the COM1 transmit register (0x3F8) with DLAB clear appends to
    // the text serial_text() surfaces, and the line status register (0x3FD)
    // always reports transmitter empty (THRE|TEMT) so a poll loop never stalls.
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    with_bus(&mut machine, |bus| {
        bus.write_io(0x03f8, BusWidth::Byte, u32::from(b'H'), false)
            .unwrap();
        bus.write_io(0x03f8, BusWidth::Byte, u32::from(b'i'), false)
            .unwrap();
    });
    assert!(machine.serial_text().ends_with("Hi"));
    let lsr = machine.read_io_port_u8(0x03fd);
    assert_ne!(lsr & 0x20, 0, "THRE set");
    assert_ne!(lsr & 0x40, 0, "TEMT set");
}

#[test]
fn izarra_bios_mirrors_post_log_to_com1() {
    // POST initializes COM1 and writes each step's status and name to 0x3F8.
    // After a full POST run the serial log carries the header and the
    // foundation reference step, proving the mirror is live.
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    // The RLE background blit delays com1_init/the step loop to ~10M cycles.
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    let serial = machine.serial_text();
    assert!(
        serial.contains("Izarra 3000 POST"),
        "COM1 log missing the POST header: {serial:?}"
    );
    assert!(
        serial.contains("PASS self.framework"),
        "COM1 log missing the framework step line: {serial:?}"
    );
    // MEASURE steps must carry their value: this 16 MB machine reports 16384 KiB
    // detected, so the COM1 line ends with the eight-digit value, not a bare name.
    assert!(
        serial.contains("MEASURE memory.detected_kib 00016384"),
        "COM1 MEASURE line missing its value: {serial:?}"
    );
}

#[test]
fn fast_post_port_reflects_the_flag() {
    // Port 0xE2 is the Lotura POST-pacing flag the BIOS reads before the
    // cosmetic RAM count-up. It defaults to fast (1) so headless runs and
    // tests skip the ~8 s pacing; the GUI clears it for the full experience.
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    let fast = with_bus(&mut machine, |bus| {
        bus.read_io(0x00e2, BusWidth::Byte, 0, false).unwrap() as u8
    });
    assert_eq!(fast, 1, "fast POST is the default");
    machine.set_fast_post(false);
    let full = with_bus(&mut machine, |bus| {
        bus.read_io(0x00e2, BusWidth::Byte, 0, false).unwrap() as u8
    });
    assert_eq!(full, 0, "clearing the flag selects the full-pacing path");
}

#[test]
fn izarra_bios_int19_boots_floppy_sector_zero() {
    // INT 19h must load sector 0 of the mounted floppy to 0000:7C00 and far
    // jump there with no signature check. The boot sector writes a sentinel
    // and halts; if the sentinel lands, the bootstrap loaded and jumped.
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();

    let mut img = vec![0u8; 737_280];
    // Boot sector at 0000:7C00: mov bx,0x0500; mov al,0x99; mov [bx],al; hlt.
    // boot_entry enters with DS=0, so [bx] addresses 0000:0500.
    let boot = [0xBB, 0x00, 0x05, 0xB0, 0x99, 0x88, 0x07, 0xF4];
    img[..boot.len()].copy_from_slice(&boot);
    machine.mount_floppy(img).unwrap();

    machine.run_until_halt_or_cycles(50_000_000).unwrap();
    assert_eq!(
        machine.read_physical_u8(0x0500),
        0x99,
        "the boot sector ran from 0000:7C00, so INT 19h loaded and jumped"
    );
}

#[test]
fn floppy_booter_owns_int21_through_its_ivt_handler() {
    // QuickDOS-style self-booting disks provide their own DOS personality.
    // After INT 19h boots A:, INT 21h must run through the disk's IVT handler
    // rather than the Toka-DOS HLE. The boot sector installs INT 21h at
    // 0000:7C1E, calls AH=4Ch, then writes a post-return marker and halts. If
    // HLE owns the call, AH=4Ch reports StopReason::DosExit before either
    // marker lands.
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();

    let mut img = vec![0u8; 737_280];
    let boot = [
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x84, 0x00, 0x1E, 0x7C, // mov word [0084h], 7C1Eh
        0xC7, 0x06, 0x86, 0x00, 0x00, 0x00, // mov word [0086h], 0000h
        0xB8, 0x2A, 0x4C, // mov ax, 4C2Ah
        0xCD, 0x21, // int 21h
        0xBB, 0x01, 0x05, // mov bx, 0501h
        0xB0, 0x7E, // mov al, 7Eh
        0x88, 0x07, // mov [bx], al
        0xFA, // cli
        0xF4, // hlt
        // INT 21h handler at 0000:7C1E.
        0xBB, 0x00, 0x05, // mov bx, 0500h
        0xB0, 0x21, // mov al, 21h
        0x88, 0x07, // mov [bx], al
        0xCF, // iret
    ];
    img[..boot.len()].copy_from_slice(&boot);
    machine.mount_floppy(img).unwrap();

    let reason = machine.run_until_halt_or_cycles(50_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.read_physical_u8(0x0500),
        0x21,
        "the boot sector's INT 21h handler ran instead of Toka-DOS HLE"
    );
    assert_eq!(
        machine.read_physical_u8(0x0501),
        0x7E,
        "the boot sector returned from its INT 21h handler and kept running"
    );
}

#[test]
fn int13_through_ff00_0000_returns_to_caller() {
    // Period PC booters (e.g. Wizardry III) repoint IVT[0x13] to FF00:0000 to
    // chain disk calls through the ROM-BIOS handler, then issue INT 13h. The
    // host intercepts the INT 13h instruction by vector number regardless of
    // the IVT target, so it still services the read; the redirected vector at
    // FF00:0000 only needs a valid IRET to land on. This test proves control
    // returns to the caller (no reset, no runaway) and the disk read happened.
    let mut img = vec![0u8; 737_280];
    img[0] = 0xEB;
    img[1] = 0x55;
    let rom = rom_with_code(&[
        // Point IVT[0x13] (at 0000:004C) to FF00:0000.
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x4C, 0x00, 0x00, 0x00, // mov word [0x004C], 0x0000 (offset)
        0xC7, 0x06, 0x4E, 0x00, 0x00, 0xFF, // mov word [0x004E], 0xFF00 (segment)
        // Read 1 sector at CHS(0,0,1) of drive 0 into ES:BX = 0000:2000.
        0x8E, 0xC0, // mov es, ax
        0xBB, 0x00, 0x20, // mov bx, 0x2000
        0xB8, 0x01, 0x02, // mov ax, 0x0201
        0xB9, 0x01, 0x00, // mov cx, 0x0001
        0xBA, 0x00, 0x00, // mov dx, 0x0000
        0xCD, 0x13, // int 13h  -> vector now targets FF00:0000
        // If the IRET at FF00:0000 returned cleanly, we reach this marker.
        0xBB, 0x00, 0x05, // mov bx, 0x0500
        0xB0, 0x42, // mov al, 0x42
        0x88, 0x07, // mov [bx], al   (DS=0, so writes 0000:0500)
        0xF4, // hlt
    ]);
    // The Izarra BIOS emits an IRET at ROM offset 0xF000 (FF00:0000); the
    // synthetic test ROM gets the same stub so the redirected vector lands on
    // a clean return point.
    let mut rom = rom;
    rom[0xF000] = 0xCF; // iret
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.mount_floppy(img).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // The INT 13h read still placed the sector bytes at 0x2000.
    assert_eq!(machine.read_physical_u8(0x2000), 0xEB);
    assert_eq!(machine.read_physical_u8(0x2001), 0x55);
    // The IRET at FF00:0000 returned to the caller, which ran the marker store.
    assert_eq!(
        machine.read_physical_u8(0x0500),
        0x42,
        "control returned past the redirected INT 13h vector"
    );
    let flags = machine.cpu().registers.eflags;
    assert_eq!(flags & 0x0001, 0, "CF must be clear after a good read");
}

#[test]
fn int13_ah01_returns_last_status() {
    // A failed read (drive B:, unbacked) sets the last status; AH=01h reads it back.
    let rom = rom_with_code(&[
        0xB4, 0x02, 0xB0, 0x01, // AH=02h read, AL=1 sector
        0xB5, 0x00, 0xB1, 0x01, // CH=0 cyl, CL=1 sector
        0xB6, 0x00, 0xB2, 0x01, // DH=0 head, DL=1 (drive B:, unbacked)
        0xCD, 0x13, 0xB4, 0x01, 0xCD, 0x13, // AH=01h get last status
        0xF4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    // Mount media in A: so handle_int13 runs; the read targets B:, which is unbacked.
    machine.mount_floppy(vec![0u8; 737_280]).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    // Drive B: is unbacked: the transfer reported AH=0x80 (timeout); AH=01h returns it
    // in AH (the documented register) and mirrors it into AL for PS/2 compatibility.
    let ax = machine.cpu().registers.eax() as u16;
    assert_eq!(ax as u8, 0x80, "AL = last disk status");
    assert_eq!((ax >> 8) as u8, 0x80, "AH = last disk status");
}

#[test]
fn simulated_int_dispatch_through_the_ivt_services_the_hle() {
    // The Quake-under-CWSDPMI mechanism in miniature: a DPMI host services
    // a real-mode interrupt request by PUSHF + far CALL through the IVT,
    // never executing an INT opcode. The per-vector stub's fetch seam must
    // service the HLE anyway. Here: a simulated INT 10h AX=0013 mode set.
    let code: &[u8] = &[
        0xb8, 0x13, 0x00, // mov ax, 0x0013
        0x31, 0xdb, // xor bx, bx
        0x8e, 0xdb, // mov ds, bx
        0x9c, // pushf
        0xff, 0x1e, 0x40, 0x00, // call far [0x0040]  (IVT[0x10])
        0xfa, 0xf4, // cli; hlt
    ];
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        boot_image_with(code),
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.read_physical_u8(0x449),
        0x13,
        "the simulated INT 10h mode set must reach the HLE (BDA current mode)"
    );
}

#[test]
fn int_opcode_dispatch_services_exactly_once() {
    // INT 10h AH=0Eh teletype 'A' via the opcode path: the opcode arm
    // stands down for a default vector (the stub fetch posts instead), so
    // the character must appear exactly once (a double service advances
    // the BDA cursor column twice).
    let code: &[u8] = &[
        0xb8, 0x41, 0x0e, // mov ax, 0x0e41 ('A' teletype)
        0xcd, 0x10, // int 0x10
        0xfa, 0xf4, // cli; hlt
    ];
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        boot_image_with(code),
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.read_physical_u8(0x450),
        1,
        "one teletype call advances the cursor column exactly once"
    );
}

#[test]
fn hook_chaining_to_the_saved_default_services_exactly_once() {
    // Reviewer reproducer (finding 1): a guest hooks an intercepted vector
    // and chains to the saved default (the per-vector stub). The hook gets
    // no post at the opcode (it owns the vector); the chain landing posts
    // exactly one service. A double poster advances the BDA cursor column
    // twice.
    let mut image_code = vec![
        0x31, 0xc0, // xor ax, ax
        0x8e, 0xd8, // mov ds, ax
        // Save IVT[0x10] (the default per-vector stub) at 0000:7D80.
        0xa1, 0x40, 0x00, // mov ax, [0x0040]
        0xa3, 0x80, 0x7d, // mov [0x7D80], ax
        0xa1, 0x42, 0x00, // mov ax, [0x0042]
        0xa3, 0x82, 0x7d, // mov [0x7D82], ax
        // Hook IVT[0x10] = 0000:7D00.
        0xc7, 0x06, 0x40, 0x00, 0x00, 0x7d, // mov word [0x0040], 0x7D00
        0xc7, 0x06, 0x42, 0x00, 0x00, 0x00, // mov word [0x0042], 0x0000
        0xb8, 0x41, 0x0e, // mov ax, 0x0e41 ('A' teletype)
        0xcd, 0x10, // int 0x10
        0xfa, 0xf4, // cli; hlt
    ];
    image_code.resize(0x100, 0x90);
    // The hook body at 0000:7D00 (boot-sector offset 0x100): chain to the
    // saved default with a far jump through the saved pointer.
    image_code.extend_from_slice(&[0xff, 0x2e, 0x80, 0x7d]); // jmp far [0x7D80]
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        boot_image_with(&image_code),
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.read_physical_u8(0x450),
        1,
        "hook-then-chain must service the teletype exactly once (column 1, not 2)"
    );
}

#[test]
fn copied_vector_services_once_as_the_landed_vector() {
    // Reviewer reproducer (finding 2): a guest copies one intercepted
    // vector's IVT entry over another (IVT[0x42] <- IVT[0x10]) and issues
    // the copy. Real hardware runs the landed handler exactly once; a
    // dispatch that posts at both the opcode (as 0x42) and the landing
    // (as 0x10) services twice.
    let code: &[u8] = &[
        0x31, 0xc0, // xor ax, ax
        0x8e, 0xd8, // mov ds, ax
        0xa1, 0x40, 0x00, // mov ax, [0x0040]  (IVT[0x10] offset)
        0xa3, 0x08, 0x01, // mov [0x0108], ax  (IVT[0x42] offset)
        0xa1, 0x42, 0x00, // mov ax, [0x0042]  (IVT[0x10] segment)
        0xa3, 0x0a, 0x01, // mov [0x010A], ax  (IVT[0x42] segment)
        0xb8, 0x41, 0x0e, // mov ax, 0x0e41 ('A' teletype)
        0xcd, 0x42, // int 0x42
        0xfa, 0xf4, // cli; hlt
    ];
    let mut machine = Machine::new_boot_image(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        boot_image_with(code),
    )
    .unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(
        machine.read_physical_u8(0x450),
        1,
        "a copied vector services once, as the landed vector (column 1, not 2)"
    );
}

#[test]
fn hook_chain_to_legacy_iret_survives_an_uninterceded_stub_landing() {
    // Round-2 review finding 1 (deterministic stand-in for the timer-tick
    // race): a guest hooks INT 13h and its hook body dispatches a
    // NON-intercepted interrupt (INT 1Ch here, exactly what the machine's
    // own timer ISR chains every tick) before chaining to the hardcoded
    // legacy FF00:0000. The 1Ch stub landing must NOT disarm the live
    // 0x13 legacy stash, or the chained disk service is silently dropped.
    let rom = rom_with_code(&[
        0x31, 0xc0, // xor ax, ax
        0x8e, 0xd8, // mov ds, ax
        // IVT[0x13] = F000:0023 (the hook below, in this ROM).
        0xc7, 0x06, 0x4c, 0x00, 0x23, 0x00, // mov word [0x4c], 0x0023
        0xc7, 0x06, 0x4e, 0x00, 0x00, 0xf0, // mov word [0x4e], 0xf000
        // A failing read on unbacked drive B sets the last status...
        0xb4, 0x02, 0xb0, 0x01, // AH=02h read, AL=1 sector
        0xb5, 0x00, 0xb1, 0x01, // CH=0, CL=1
        0xb6, 0x00, 0xb2, 0x01, // DH=0, DL=1 (drive B:, unbacked)
        0xcd, 0x13, // int 0x13
        0xb4, 0x01, 0xcd, 0x13, // ...and AH=01h reads it back.
        0xf4, // hlt (offset 0x22)
        // hook (offset 0x23): tick-chain stand-in, then legacy chain.
        0xcd, 0x1c, // int 0x1c   (lands stub 0x1C, not intercepted)
        0xea, 0x00, 0x00, 0x00, 0xff, // jmp far FF00:0000
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.mount_floppy(vec![0u8; 737_280]).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let ax = machine.cpu().registers.eax() as u16;
    assert_eq!(
        (ax >> 8) as u8,
        0x80,
        "the hook-chained INT 13h must survive an interleaved non-intercepted \
             stub landing (AH = last status)"
    );
}

#[test]
fn booter_hardcoded_legacy_iret_keeps_int13_serviced() {
    // Period booters repoint IVT[0x13] at the legacy shared chain target
    // FF00:0000 (not the per-vector stub) and then issue INT 13h. That
    // address is shared by every vector, so the fetch seam attributes the
    // landing through the vector the INT opcode stashed (last_int_vector).
    let rom = rom_with_code(&[
        // IVT[0x13] = FF00:0000, the hardcoded legacy chain target.
        0x31, 0xc0, // xor ax, ax
        0x8e, 0xd8, // mov ds, ax
        0xc7, 0x06, 0x4c, 0x00, 0x00, 0x00, // mov word [0x4c], 0x0000
        0xc7, 0x06, 0x4e, 0x00, 0x00, 0xff, // mov word [0x4e], 0xff00
        // A failing read on unbacked drive B: sets the last status...
        0xb4, 0x02, 0xb0, 0x01, // AH=02h read, AL=1 sector
        0xb5, 0x00, 0xb1, 0x01, // CH=0, CL=1
        0xb6, 0x00, 0xb2, 0x01, // DH=0, DL=1 (drive B:, unbacked)
        0xcd, 0x13, // int 0x13
        0xb4, 0x01, 0xcd, 0x13, // ...and AH=01h reads it back.
        0xf4,
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), rom).unwrap();
    machine.mount_floppy(vec![0u8; 737_280]).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    let ax = machine.cpu().registers.eax() as u16;
    assert_eq!(
        (ax >> 8) as u8,
        0x80,
        "the hardcoded-legacy-vector INT 13h was still serviced (AH = last status)"
    );
}

#[test]
fn izarra_bios_isr_enqueues_injected_key() {
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    // Run POST so the BIOS reaches its idle loop (past the setup hotkey window,
    // which would otherwise drain the key). Then inject a key: IRQ1 reaches the
    // installed INT 09h, which enqueues it into the BDA ring. The idle loop does
    // not consume keys, so it stays there. The budget tracks POST's length: the
    // setup-page incremental-redraw work (f56c0197) pushed POST past the old
    // 5M-cycle budget, which parked this test inside the hotkey window.
    machine.run_until_halt_or_cycles(10_000_000).unwrap();
    machine.inject_key_scancodes(&[0x1e, 0x9e]);
    machine.run_until_halt_or_cycles(2_000_000).unwrap();
    let head = machine.memory_read_u16_for_test(0x41a);
    let tail = machine.memory_read_u16_for_test(0x41c);
    assert_ne!(head, tail, "the installed INT 09h enqueued the key");
}

#[test]
fn izarra_setup_saves_a_changed_value_to_cmos() {
    // Drive the Del setup page end to end: enter it during POST, change the
    // keyboard layout (CMOS 0x10, default 0 = en-US) to the next entry, save,
    // and confirm the persisted CMOS byte changed. The setup menu blocks on a
    // keyboard read between keystrokes, so each key is injected then run.
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    assert_eq!(
        machine.cmos_byte(0x10),
        0,
        "the keyboard-layout NVRAM byte starts at en-US (0)"
    );

    // Queue Del before POST reaches the hotkey window so the window finds it.
    // Make + break; only the make enqueues into the BDA ring (0x53 = Del).
    machine.inject_key_scancodes(&[0x53, 0xd3]);
    // Run past POST. The window consumes Del and enters the menu, which then
    // blocks on a keyboard read, so the rest of the budget just spins there.
    // The full-screen RLE POST background pushes the hotkey window to ~15M
    // cycles, so this budget must clear it.
    machine.run_until_halt_or_cycles(20_000_000).unwrap();

    // Down moves the highlight from Time (row 0) to Keyboard (row 1). Each
    // keystroke repaints the whole page (title + boxed menu + help footer) on
    // the Margo LFB; the per-pixel unreal-mode box/fill primitives cost more
    // guest cycles than the old mode-13h gfx_clear + gfx_text redraw, so these
    // budgets are larger than the pre-LFB page needed.
    machine.inject_key_scancodes(&[0x50, 0xd0]); // Down
    machine.run_until_halt_or_cycles(3_000_000).unwrap();
    // Right cycles the keyboard layout forward (en-US -> UK).
    machine.inject_key_scancodes(&[0x4d, 0xcd]); // Right
    machine.run_until_halt_or_cycles(3_000_000).unwrap();
    // F10 saves: writes CMOS 0x10/0x12, refreshes the checksum, and exits.
    machine.inject_key_scancodes(&[0x44, 0xc4]); // F10
    machine.run_until_halt_or_cycles(4_000_000).unwrap();

    assert_eq!(
        machine.cmos_byte(0x10),
        1,
        "saving the setup page persisted the new keyboard layout to CMOS 0x10"
    );
    // The save also refreshes the NVRAM checksum, so a reload validates.
    let saved = machine.cmos_bytes();
    let mut reloaded = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    assert!(
        reloaded.load_cmos(&saved),
        "the saved CMOS image carries a valid checksum"
    );
    assert_eq!(reloaded.cmos_byte(0x10), 1);
}

#[test]
fn boot_menu_marks_one_speed_row_on_the_lfb() {
    // Open the LFB boot menu (focus seeds on the Floppy device row, so every
    // speed row is unfocused) and confirm exactly one speed row shows the marker
    // diamond. The marker sits at x 172 on a speed row; an unfocused marked row
    // paints an ink diamond (index ART_INK_INDEX = 0) on the cream field, so ink
    // pixels in that column flag the mark. This guards the full-repaint render
    // (a stale or missing marker would change the count).
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.inject_key_scancodes(&[0x0f, 0x8f]); // Tab opens the menu.
    machine.run_until_halt_or_cycles(25_000_000).unwrap();
    assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);

    // Speed rows top at y 144 + row*12; the marker glyph is at +2, x 172..180.
    let marker_inked = |m: &mut Machine, row: u32| -> bool {
        let y0 = 144 + row * 12 + 2;
        (y0..y0 + 8)
            .any(|y| (172..180u32).any(|x| m.read_physical_u8(MARGO_LFB_BASE + y * 320 + x) == 0))
    };
    let marked = (0..4u32)
        .filter(|&row| marker_inked(&mut machine, row))
        .count();
    assert_eq!(marked, 1, "exactly one speed row shows the marker diamond");
}

#[test]
fn int1b_and_int1c_vectors_point_at_valid_iret_handlers() {
    // Use a ROM that carries the IRET byte at FF00:0000, the way the real BIOS
    // does, so the seeded vector lands on a genuine IRET.
    let mut m = Machine::new(
        MachineProfile::gsw_386(4, VideoCard::Et4000Ax),
        rom_with_code(&[]),
    )
    .unwrap();
    for vector in [0x1bu32, 0x1c] {
        let off = read_u16(&mut m, vector * 4);
        let seg = read_u16(&mut m, vector * 4 + 2);
        assert_eq!(
            seg, BIOS_ROM_IRET_SEG,
            "INT {vector:02X}h targets the ROM IRET segment"
        );
        let target = (u32::from(seg) << 4) + u32::from(off);
        assert_eq!(
            m.read_physical_u8(target),
            0x90,
            "INT {vector:02X}h target is its per-vector stub's NOP"
        );
        assert_eq!(
            m.read_physical_u8(target + 1),
            0xcf,
            "INT {vector:02X}h stub ends in an IRET"
        );
    }
}

#[test]
fn dos_reserved_vectors_point_at_a_valid_iret_handler() {
    let mut m = Machine::new(
        MachineProfile::gsw_386(4, VideoCard::Et4000Ax),
        rom_with_code(&[]),
    )
    .unwrap();

    for vector in [
        0x2bu32, 0x2c, 0x2d, 0x2e, 0x32, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c,
        0x3d, 0x3e, 0x3f, 0x45, 0x48, 0x49, 0x4a, 0x59, 0x5a, 0x5b, 0x5c, 0x60, 0x61, 0x62, 0x63,
        0x64, 0x65, 0x68, 0x69, 0x6a, 0x6b, 0x6c, 0x6d, 0x6e, 0x6f, 0x78, 0x79, 0x7a, 0x7b, 0x7c,
        0x7d, 0x7e, 0x7f, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0xe0, 0xe4, 0xef, 0xf0, 0xf1,
        0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd, 0xfe, 0xff,
    ] {
        let off = read_u16(&mut m, vector * 4);
        let seg = read_u16(&mut m, vector * 4 + 2);
        assert_eq!(seg, BIOS_ROM_IRET_SEG, "INT {vector:02X}h IRET segment");
        let target = (u32::from(seg) << 4) + u32::from(off);
        assert_eq!(
            m.read_physical_u8(target),
            0x90,
            "INT {vector:02X}h target is its per-vector stub's NOP"
        );
        assert_eq!(
            m.read_physical_u8(target + 1),
            0xcf,
            "INT {vector:02X}h stub ends in an IRET"
        );
    }
}

#[test]
fn int70_vector_points_at_the_rtc_isr_stub() {
    let mut m = int15_machine(4);
    let off = read_u16(&mut m, 0x70 * 4);
    let seg = read_u16(&mut m, 0x70 * 4 + 2);
    assert_eq!(seg, 0);
    assert_eq!(off, BIOS_RTC_ISR_ADDRESS as u16);
    // The stub starts with PUSH AX and ends with IRET.
    assert_eq!(m.read_physical_u8(BIOS_RTC_ISR_ADDRESS as u32), 0x50);
    assert_eq!(m.read_physical_u8(BIOS_RTC_ISR_ADDRESS as u32 + 14), 0xcf);
}

#[test]
fn slave_irq_vectors_point_at_the_eoi_stub() {
    let mut m = int15_machine(4);
    for vector in [0x74u32, 0x75, 0x76] {
        let off = read_u16(&mut m, vector * 4);
        let seg = read_u16(&mut m, vector * 4 + 2);
        assert_eq!(seg, 0, "INT {vector:02X}h segment");
        assert_eq!(
            off, BIOS_SLAVE_IRQ_ISR_ADDRESS as u16,
            "INT {vector:02X}h offset"
        );
    }
    assert_eq!(m.read_physical_u8(BIOS_SLAVE_IRQ_ISR_ADDRESS as u32), 0x50);
    assert_eq!(
        m.read_physical_u8(BIOS_SLAVE_IRQ_ISR_ADDRESS as u32 + 8),
        0xcf
    );
}

#[test]
fn enabled_rtc_periodic_interrupt_requests_irq8() {
    let mut m = int15_machine(4);
    // Enable the periodic interrupt (select Reg B, set PIE bit 6).
    m.rtc.write_port(0x70, 0x0b);
    m.rtc.write_port(0x71, 0x40);
    // Advance enough clocks for at least one whole RTC second to elapse.
    let one_second = m.active_mode.clock_hz();
    m.advance_devices(one_second + 1);
    assert!(m.pic.irr_bit(8), "IRQ8 became pending");
}

#[test]
fn c207_stores_the_mouse_handler_far_pointer_in_the_ebda() {
    let mut m = int15_machine(4);
    // ES:BX = 1234:5678, the handler the guest installs.
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x1234));
    m.cpu.registers.set_ebx(0x5678);
    m.cpu.registers.set_eax(0xC207);
    m.handle_int15();
    // CF clear, AH=0: success.
    let flags_carry = {
        let ss = m.cpu.registers.segment(SegmentIndex::Ss).base;
        let sp = m.cpu.registers.esp() as u16;
        read_u16(&mut m, ss + u32::from(sp.wrapping_add(4))) & 1
    };
    assert_eq!(flags_carry, 0, "C207 returns CF clear");
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00);
    // The EBDA holds the far pointer: offset word then segment word.
    let base = (u32::from(EBDA_SEGMENT) << 4) + EBDA_MOUSE_HANDLER_OFF;
    assert_eq!(read_u16(&mut m, base), 0x5678, "offset stored");
    assert_eq!(read_u16(&mut m, base + 2), 0x1234, "segment stored");
}

#[test]
fn c205_init_enables_intellimouse_wheel_and_sets_ebda_packet_size() {
    let mut m = int15_machine(4);
    // The aux device powers up as a standard 3-byte mouse.
    assert!(!m.keyboard.mouse_wheel_enabled(), "starts in 3-byte mode");
    // INT 15h AX=C205, BH=3 (the standard init the driver issues at startup).
    m.cpu.registers.set_eax(0xC205);
    m.cpu.registers.set_ebx(0x0300);
    m.handle_int15();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x00,
        "C205 returns AH=0"
    );
    // The platform enables wheel mode at mouse-enable: the device is now 4-byte,
    assert!(
        m.keyboard.mouse_wheel_enabled(),
        "device in IntelliMouse mode"
    );
    // and the BIOS-visible EBDA packet size is 4 so int74 assembles the Z byte.
    let pkt_size = (u32::from(EBDA_SEGMENT) << 4) + EBDA_MOUSE_PKT_SIZE_OFF;
    assert_eq!(m.read_physical_u8(pkt_size), 4, "EBDA packet size is 4");
}

#[test]
fn c202_sample_rate_is_reported_by_c206_status() {
    let mut m = int15_machine(4);
    m.cpu.registers.set_eax(0xC202);
    m.cpu.registers.set_ebx(0x0600); // BIOS rate code 6 = 200 Hz
    m.handle_int15();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00);

    m.cpu.registers.set_eax(0xC206);
    m.cpu.registers.set_ebx(0x0000); // BH=0 status
    m.handle_int15();
    assert_eq!(m.cpu.registers.edx() as u8, 200);
}

#[test]
fn c200_enable_turns_on_the_wheel_and_disable_leaves_it() {
    let mut m = int15_machine(4);
    // C200 enable (BH=1) flips on IntelliMouse 4-byte mode and packet size 4.
    m.cpu.registers.set_eax(0xC200);
    m.cpu.registers.set_ebx(0x0100);
    m.handle_int15();
    assert!(
        m.keyboard.mouse_wheel_enabled(),
        "enable turns on the wheel"
    );
    let pkt_size = (u32::from(EBDA_SEGMENT) << 4) + EBDA_MOUSE_PKT_SIZE_OFF;
    assert_eq!(m.read_physical_u8(pkt_size), 4, "enable sets packet size 4");
    // C200 disable (BH=0) stops reporting but leaves the wheel mode and the EBDA
    // packet size as-is (the known no-resize ceiling).
    m.cpu.registers.set_eax(0xC200);
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int15();
    assert!(
        m.keyboard.mouse_wheel_enabled(),
        "disable leaves wheel mode untouched"
    );
    assert_eq!(
        m.read_physical_u8(pkt_size),
        4,
        "disable leaves the packet size untouched"
    );
}

#[test]
fn int19_floppy_boot_loads_sector_and_jumps_to_7c00() {
    let mut m = int15_machine(4);
    // A 360 KB image with a marker byte at the start of sector 0.
    let mut image = vec![0u8; 368_640];
    image[0] = 0xeb; // a plausible boot-sector first byte (JMP short)
    image[1] = 0x3c;
    m.mount_floppy(image).unwrap();
    m.handle_int19();
    // Boot sector copied to 0000:7C00, DL = 0 (floppy), CS:IP = 0000:7C00.
    assert_eq!(m.read_physical_u8(BOOT_SECTOR_ADDRESS as u32), 0xeb);
    assert_eq!(m.read_physical_u8(BOOT_SECTOR_ADDRESS as u32 + 1), 0x3c);
    assert_eq!(m.cpu.registers.edx() as u8, 0x00);
    assert_eq!(m.cpu.registers.segment(SegmentIndex::Cs).selector, 0x0000);
    assert_eq!(m.cpu.registers.eip, BOOT_SECTOR_ADDRESS as u32);
}

#[test]
fn int19_without_bootable_media_falls_to_int18_halt_stub() {
    let mut m = int15_machine(4);
    // No floppy and no Toka-DOS install: INT 19h must reach the INT 18h halt.
    m.handle_int19();
    assert_eq!(
        m.cpu.registers.segment(SegmentIndex::Cs).selector,
        0x0000,
        "CS points at the low-RAM halt stub"
    );
    assert_eq!(m.cpu.registers.eip, BIOS_HALT_STUB_ADDRESS as u32);
    // The stub is CLI;HLT, which halts the machine for good.
    assert_eq!(m.read_physical_u8(BIOS_HALT_STUB_ADDRESS as u32), 0xfa);
    assert_eq!(m.read_physical_u8(BIOS_HALT_STUB_ADDRESS as u32 + 1), 0xf4);
}

#[test]
fn int18_halt_stub_actually_stops_the_machine() {
    let mut m = int15_machine(4);
    m.handle_int18();
    // Run from the halt stub: CLI then HLT, with IF cleared, gives a genuine stop.
    let reason = m.run_until_halt_or_cycles(10_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
}

#[test]
fn lotura_e7_banks_a_codepage_font_page_into_the_window() {
    // mov al,3 ; out 0E7h,al ; int 20h -> bank CP850 8x16 (cp=1, size=0) into 0xC4000.
    // sel=3: cp=3/3=1, size_index=3%3=0 => CP850, 8x16 block.
    const PROG: [u8; 6] = [0xB0, 0x03, 0xE6, 0xE7, 0xCD, 0x20];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Et4000Ax), &PROG).unwrap();
    machine.run_until_halt_or_cycles(1_000_000).unwrap();
    // CP850 8x16 block is CODEPAGE_FONTS[9728 .. 9728+4096]; it must now be at 0xC4000.
    for k in [0u32, 1, 0x41 * 16 + 2, 4095] {
        assert_eq!(
            machine.read_physical_u8(0xC4000 + k),
            izarravm_firmware::CODEPAGE_FONTS[(9728 + k) as usize],
            "byte {k} mismatch"
        );
    }
}

// Boot the BIOS with the given CMOS 0x11 code-page index to its idle loop, then
// return `rows` font bytes for `glyph` from the VGA character generator (table 0).
// Mirrors the boot-to-idle pattern from izarra_kbd_layouts.rs.
fn boot_and_read_font_rows(cmos_codepage: u8, glyph: u8, rows: usize) -> Vec<u8> {
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios()).unwrap();
    machine.set_cmos_byte(0x13, cmos_codepage);
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    (0..rows)
        .map(|r| machine.video().active_font_glyph_row(glyph, r))
        .collect()
}

#[test]
fn boot_codepage_byte_loads_font_into_generator() {
    // CP850 8x16 block is CODEPAGE_FONTS[9728 .. 9728+4096]. Glyph 0xB5 there is
    // A-acute; under CP437 it is a box-drawing piece. Booting with CMOS 0x13 = 1
    // must leave the VGA font generator holding the CP850 glyph.
    let want: Vec<u8> = (0..16)
        .map(|r| izarravm_firmware::CODEPAGE_FONTS[9728 + 0xB5 * 16 + r])
        .collect();
    let got = boot_and_read_font_rows(1, 0xB5, 16);
    assert_eq!(got, want);
    // CP437 (cmos 0) keeps the box-drawing glyph.
    let want437: Vec<u8> = (0..16)
        .map(|r| izarravm_firmware::CODEPAGE_FONTS[0xB5 * 16 + r])
        .collect();
    assert_eq!(boot_and_read_font_rows(0, 0xB5, 16), want437);
}
