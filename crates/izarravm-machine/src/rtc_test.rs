// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_core::{
    CanonicalSectionId, CanonicalSectionRequirement, CanonicalSectionVersion, CanonicalStateView,
    CanonicalStateWriter, MASTER_CLOCK_HZ,
};

const RTC_PAYLOAD_LEN: usize = 82;

fn canonical_payload(rtc: &Rtc) -> Vec<u8> {
    let projection = rtc.canonical_projection();
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_0007).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| projection.write_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn assert_only_offsets_changed(before: &[u8], after: &[u8], expected: &[usize]) {
    let actual: Vec<_> = before
        .iter()
        .zip(after)
        .enumerate()
        .filter_map(|(offset, (left, right))| (left != right).then_some(offset))
        .collect();
    assert_eq!(actual, expected);
}

fn assert_time_offset(
    baseline: &Rtc,
    expected: &[u8],
    offset: usize,
    change: impl FnOnce(&mut Time),
) {
    let mut changed = baseline.clone();
    change(&mut changed.time);
    assert_only_offsets_changed(expected, &canonical_payload(&changed), &[offset]);
}

#[test]
fn canonical_payload_layout_is_exact() {
    let mut rtc = Rtc::new();
    for (offset, byte) in rtc.ram.iter_mut().enumerate() {
        *byte = (offset as u8).wrapping_mul(3).wrapping_add(1);
    }
    rtc.index = 0x4c;
    rtc.nmi_disabled = true;
    rtc.time = Time {
        year: 0x1234,
        month: 5,
        day: 6,
        weekday: 7,
        hour: 8,
        minute: 9,
        second: 10,
    };
    rtc.seeded = true;
    rtc.nvram_dirty = true;
    rtc.periodic_phase = RatePhase::with_remainder(0x0012_3456);

    let mut expected = rtc.ram.to_vec();
    expected.extend_from_slice(&[
        0x4c, 1, 0x34, 0x12, 5, 6, 7, 8, 9, 10, 0x56, 0x34, 0x12, 0, 0, 0, 0, 0,
    ]);

    let payload = canonical_payload(&rtc);
    assert_eq!(payload.len(), RTC_PAYLOAD_LEN);
    assert_eq!(payload, expected);
}

#[test]
fn canonical_payload_pins_every_behavioral_field_offset() {
    let baseline = Rtc::new();
    let expected = canonical_payload(&baseline);

    for offset in 0..baseline.ram.len() {
        let mut changed = baseline.clone();
        changed.ram[offset] ^= 0x80;
        assert_only_offsets_changed(&expected, &canonical_payload(&changed), &[offset]);
    }

    let mut changed = baseline.clone();
    changed.index = 0x40;
    assert_only_offsets_changed(&expected, &canonical_payload(&changed), &[64]);

    let mut changed = baseline.clone();
    changed.nmi_disabled = true;
    assert_only_offsets_changed(&expected, &canonical_payload(&changed), &[65]);

    assert_time_offset(&baseline, &expected, 66, |time| time.year ^= 0x0001);
    assert_time_offset(&baseline, &expected, 67, |time| time.year ^= 0x0100);
    assert_time_offset(&baseline, &expected, 68, |time| {
        time.month = time.month.wrapping_add(1);
    });
    assert_time_offset(&baseline, &expected, 69, |time| {
        time.day = time.day.wrapping_add(1);
    });
    assert_time_offset(&baseline, &expected, 70, |time| {
        time.weekday = time.weekday.wrapping_add(1);
    });
    assert_time_offset(&baseline, &expected, 71, |time| {
        time.hour = time.hour.wrapping_add(1);
    });
    assert_time_offset(&baseline, &expected, 72, |time| {
        time.minute = time.minute.wrapping_add(1);
    });
    assert_time_offset(&baseline, &expected, 73, |time| {
        time.second = time.second.wrapping_add(1);
    });

    for byte in 0..=4 {
        let mut changed = baseline.clone();
        changed.periodic_phase = RatePhase::with_remainder(1 << (byte * 8));
        assert_only_offsets_changed(&expected, &canonical_payload(&changed), &[74 + byte]);
    }
    assert_eq!(&expected[79..82], &[0, 0, 0]);
}

#[test]
fn raw_clock_bytes_and_authoritative_time_are_independent() {
    let baseline = Rtc::new();
    let expected = canonical_payload(&baseline);

    let mut raw_changed = baseline.clone();
    raw_changed.set_nvram(usize::from(REG_SECONDS), 0x5a);
    assert_eq!(raw_changed.clock(), baseline.clock());
    assert_only_offsets_changed(&expected, &canonical_payload(&raw_changed), &[0]);

    let mut time_changed = baseline.clone();
    time_changed.time.second = 17;
    assert_eq!(time_changed.ram, baseline.ram);
    assert_only_offsets_changed(&expected, &canonical_payload(&time_changed), &[73]);
}

#[test]
fn host_bookkeeping_is_normalized_without_being_consumed() {
    let mut baseline = Rtc::new();
    baseline.write_port(0x70, REG_B);
    baseline.write_port(0x71, REG_B_PIE);
    baseline.advance_master_ticks(123_457, 0);

    let expected = canonical_payload(&baseline);
    for (seeded, dirty) in [(true, false), (false, true), (true, true)] {
        let mut changed = baseline.clone();
        changed.seeded = seeded;
        changed.nvram_dirty = dirty;
        assert_eq!(canonical_payload(&changed), expected);
        assert_eq!(
            changed.ticks_until_periodic_irq(),
            baseline.ticks_until_periodic_irq()
        );
        let delta = changed.ticks_until_periodic_irq().unwrap();
        let mut reference = baseline.clone();
        assert_eq!(
            changed.advance_master_ticks(delta, 0),
            reference.advance_master_ticks(delta, 0)
        );
        assert_eq!(changed.clock(), reference.clock());
        assert_eq!(changed.ram, reference.ram);
        assert_eq!(canonical_payload(&changed), canonical_payload(&reference));
    }

    let mut dirty = baseline.clone();
    dirty.seeded = true;
    dirty.nvram_dirty = true;
    let before_take = canonical_payload(&dirty);
    assert_eq!(canonical_payload(&dirty), before_take);
    assert!(dirty.is_seeded());
    assert!(dirty.take_nvram_dirty());
    assert_eq!(canonical_payload(&dirty), before_take);
    assert!(!dirty.take_nvram_dirty());
}

#[test]
fn canonical_capture_preserves_register_c_and_full_index_latches() {
    let mut rtc = Rtc::new();
    rtc.write_port(0x70, REG_B);
    rtc.write_port(0x71, REG_B_PIE);
    let deadline = rtc.ticks_until_periodic_irq().unwrap();
    assert!(rtc.advance_master_ticks(deadline, 0));
    rtc.write_port(0x70, 0x80 | 0x4c);

    let first = canonical_payload(&rtc);
    let second = canonical_payload(&rtc);
    assert_eq!(first, second);
    assert_eq!(first[64], 0x4c);
    assert_eq!(first[65], 1);
    assert_eq!(first[usize::from(REG_C)] & (REG_C_IRQF | REG_C_PF), 0xc0);
    assert_eq!(rtc.read_port(0x70), Some(0xcc));

    let aliased = rtc.read_port(0x71).unwrap();
    assert_eq!(aliased & (REG_C_IRQF | REG_C_PF), 0xc0);
    assert_eq!(canonical_payload(&rtc)[usize::from(REG_C)] & 0xc0, 0xc0);

    rtc.write_port(0x70, REG_C);
    assert_eq!(rtc.read_port(0x71).unwrap() & 0xc0, 0xc0);
    assert_eq!(canonical_payload(&rtc)[usize::from(REG_C)], 0);
}

#[test]
fn periodic_phase_reset_and_preservation_rules_are_canonical() {
    let mut rtc = Rtc::new();
    rtc.advance_master_ticks(123_457, 0);
    let partial = canonical_payload(&rtc);
    assert_ne!(&partial[74..82], &[0; 8]);

    rtc.write_port(0x70, REG_A);
    rtc.write_port(0x71, REG_A_DEFAULT);
    assert_eq!(&canonical_payload(&rtc)[74..82], &partial[74..82]);

    rtc.write_port(0x70, REG_B);
    rtc.write_port(0x71, REG_B_PIE);
    assert_eq!(&canonical_payload(&rtc)[74..82], &partial[74..82]);

    rtc.write_port(0x70, REG_A);
    rtc.write_port(0x71, 0x2f);
    assert_eq!(&canonical_payload(&rtc)[74..82], &[0; 8]);

    rtc.advance_master_ticks(7, 0);
    let image = rtc.nvram();
    rtc.load_nvram(&image);
    assert_eq!(&canonical_payload(&rtc)[74..82], &[0; 8]);
}

#[test]
fn current_format_bits_and_uip_model_are_pinned() {
    let mut rtc = Rtc::new();
    rtc.write_port(0x70, REG_A);
    rtc.write_port(0x71, 0xff);
    assert_eq!(rtc.nvram_byte(usize::from(REG_A)) & 0x80, 0);

    rtc.write_port(0x70, REG_B);
    rtc.write_port(0x71, 0x80);
    let register_b = rtc.nvram_byte(usize::from(REG_B));
    assert_eq!(register_b & 0x80, 0x80, "SET remains a stored bit");
    assert_eq!(register_b & REG_B_DEFAULT, REG_B_DEFAULT);
}

#[test]
fn rtc_register_round_trip() {
    let mut r = Rtc::new();
    r.write_port(0x70, 0x00); // select seconds
    r.write_port(0x71, 30);
    r.write_port(0x70, 0x00);
    assert_eq!(r.read_port(0x71), Some(30));
}

#[test]
fn rtc_seconds_advance_and_carry() {
    let mut r = Rtc::new();
    r.seed(2026, 6, 20, 6, 23, 59, 58);
    r.tick_seconds(3);
    r.write_port(0x70, 0x00);
    assert_eq!(r.read_port(0x71), Some(1)); // 58 -> 01
    r.write_port(0x70, 0x02);
    assert_eq!(r.read_port(0x71), Some(0)); // minutes 59 -> 00
    r.write_port(0x70, 0x04);
    assert_eq!(r.read_port(0x71), Some(0)); // hours 23 -> 00
}

#[test]
fn day_carries_across_month_boundary() {
    let mut r = Rtc::new();
    // 30 June 23:59:59, plus 2 seconds -> 1 July 00:00:01.
    r.seed(2026, 6, 30, 3, 23, 59, 59);
    r.tick_seconds(2);
    r.write_port(0x70, REG_DAY);
    assert_eq!(r.read_port(0x71), Some(1));
    r.write_port(0x70, REG_MONTH);
    assert_eq!(r.read_port(0x71), Some(7));
    r.write_port(0x70, REG_HOURS);
    assert_eq!(r.read_port(0x71), Some(0));
}

#[test]
fn leap_day_is_honored() {
    let mut r = Rtc::new();
    // 28 Feb 2024 (leap) 23:59:59 + 1s -> 29 Feb.
    r.seed(2024, 2, 28, 4, 23, 59, 59);
    r.tick_seconds(1);
    r.write_port(0x70, REG_DAY);
    assert_eq!(r.read_port(0x71), Some(29));
    r.write_port(0x70, REG_MONTH);
    assert_eq!(r.read_port(0x71), Some(2));
}

#[test]
fn cmos_checksum_round_trips_via_bytes() {
    let mut r = Rtc::new();
    r.set_nvram(0x10, 3); // FR layout
    r.refresh_checksum();
    let saved = r.nvram();
    let mut r2 = Rtc::new();
    r2.load_nvram(&saved);
    assert_eq!(r2.nvram_byte(0x10), 3);
    assert!(r2.checksum_valid());
}

#[test]
fn bad_checksum_is_detected() {
    let mut r = Rtc::new();
    r.set_nvram(0x11, 1);
    // No refresh: the stored checksum is now stale.
    assert!(!r.checksum_valid());
}

#[test]
fn register_b_reports_binary_24h() {
    let mut r = Rtc::new();
    r.write_port(0x70, REG_B);
    let b = r.read_port(0x71).unwrap();
    assert_ne!(b & 0x04, 0); // DM = 1 (binary)
    assert_ne!(b & 0x02, 0); // 24/12 = 1 (24-hour)
}

#[test]
fn register_d_reports_vrt() {
    let mut r = Rtc::new();
    r.write_port(0x70, REG_D);
    assert_eq!(r.read_port(0x71).unwrap() & 0x80, 0x80);
}

#[test]
fn index_port_round_trips_nmi_bit() {
    let mut r = Rtc::new();
    r.write_port(0x70, 0x80 | 0x0a); // NMI disabled, index = Reg A
    assert_eq!(r.read_port(0x70), Some(0x8a));
}

#[test]
fn year_write_keeps_century() {
    let mut r = Rtc::new();
    r.seed(2026, 6, 20, 6, 12, 0, 0);
    r.write_port(0x70, REG_YEAR);
    r.write_port(0x71, 30); // guest writes "30"
    r.write_port(0x70, REG_YEAR);
    assert_eq!(r.read_port(0x71), Some(30));
}

#[test]
fn fresh_device_has_clear_diagnostic_byte() {
    let r = Rtc::new();
    assert_eq!(r.nvram_byte(REG_DIAGNOSTIC), 0);
}

#[test]
fn clean_image_load_leaves_diagnostic_clear() {
    let mut r = Rtc::new();
    r.set_nvram(0x12, 7);
    r.refresh_checksum();
    let saved = r.nvram();
    let mut r2 = Rtc::new();
    assert!(r2.load_nvram(&saved));
    assert_eq!(r2.nvram_byte(REG_DIAGNOSTIC), 0);
}

#[test]
fn tampered_image_sets_diagnostic_bad_checksum_bit() {
    let mut r = Rtc::new();
    r.set_nvram(0x12, 7);
    r.refresh_checksum();
    let mut saved = r.nvram();
    // Flip a checksummed byte without updating the stored checksum.
    saved[0x13] ^= 0xff;
    let mut r2 = Rtc::new();
    assert!(!r2.load_nvram(&saved));
    assert_ne!(r2.nvram_byte(REG_DIAGNOSTIC) & DIAG_BAD_CHECKSUM, 0);
}

#[test]
fn power_lost_image_sets_diagnostic_power_bit() {
    let r = Rtc::new();
    let mut saved = r.nvram();
    // Clear Register D VRT to mark a dead battery; keep the checksum valid.
    saved[usize::from(REG_D)] &= !0x80;
    let mut r2 = Rtc::new();
    assert!(r2.load_nvram(&saved));
    assert_ne!(r2.nvram_byte(REG_DIAGNOSTIC) & DIAG_POWER_LOST, 0);
}

#[test]
fn century_default_is_2000s() {
    let r = Rtc::new();
    assert_eq!(r.nvram_byte(REG_CENTURY), 0x20);
    assert_eq!(r.century(), 20);
    let (year, ..) = r.clock();
    assert_eq!(year / 100, 20);
}

#[test]
fn century_byte_drives_the_year_on_load() {
    let mut r = Rtc::new();
    r.seed(2095, 6, 20, 6, 12, 0, 0);
    // Force the 1900s century into the saved image.
    r.set_nvram(REG_CENTURY, 0x19);
    r.refresh_checksum();
    let saved = r.nvram();
    let mut r2 = Rtc::new();
    r2.load_nvram(&saved);
    let (year, ..) = r2.clock();
    assert_eq!(year, 1995);

    // The default 0x20 century resolves the same two-digit year as 20xx.
    let mut r3 = Rtc::new();
    r3.seed(2095, 6, 20, 6, 12, 0, 0);
    let saved2 = r3.nvram();
    assert_eq!(saved2[REG_CENTURY], 0x20);
    let mut r4 = Rtc::new();
    r4.load_nvram(&saved2);
    let (year2, ..) = r4.clock();
    assert_eq!(year2, 2095);
}

#[test]
fn set_century_rolls_the_year_and_mirrors_alt_slot() {
    let mut r = Rtc::new();
    r.seed(2026, 6, 20, 6, 12, 0, 0);
    r.set_century(19);
    assert_eq!(r.century(), 19);
    assert_eq!(r.nvram_byte(REG_CENTURY), 0x19);
    assert_eq!(r.nvram_byte(REG_CENTURY_ALT), 0x19);
    let (year, ..) = r.clock();
    assert_eq!(year, 1926);
}

#[test]
fn disabled_periodic_interrupt_latches_nothing() {
    let mut r = Rtc::new();
    let deadline = r
        .periodic_phase
        .ticks_until(1, r.periodic_rate_hz())
        .unwrap();
    assert!(!r.advance_master_ticks(deadline, 0));
    r.write_port(0x70, REG_C);
    let c = r.read_port(0x71).unwrap();
    assert_ne!(c & REG_C_PF, 0, "raw PF still latches");
    assert_eq!(c & REG_C_IRQF, 0, "PIE gates IRQF");
}

#[test]
fn enabled_periodic_interrupt_sets_pf_and_irqf_then_clears_on_read() {
    let mut r = Rtc::new();
    // Enable the periodic interrupt (PIE, bit 6).
    r.write_port(0x70, REG_B);
    r.write_port(0x71, REG_B_PIE);
    assert_eq!(r.periodic_rate_hz(), 1024);
    let deadline = r.ticks_until_periodic_irq().unwrap();
    assert!(!r.advance_master_ticks(deadline - 1, 0));
    assert!(r.advance_master_ticks(1, 0));
    r.write_port(0x70, REG_C);
    let c = r.read_port(0x71).unwrap();
    assert_ne!(c & REG_C_PF, 0, "PF set");
    assert_ne!(c & REG_C_IRQF, 0, "IRQF set");
    // Reading Register C cleared the flags.
    r.write_port(0x70, REG_C);
    assert_eq!(r.read_port(0x71), Some(0));
}

#[test]
fn pending_flag_reports_no_new_edge_until_acked() {
    let mut r = Rtc::new();
    r.write_port(0x70, REG_B);
    r.write_port(0x71, REG_B_UIE);
    assert!(r.advance_master_ticks(MASTER_CLOCK_HZ, 1));
    assert!(!r.advance_master_ticks(MASTER_CLOCK_HZ, 1));
    // Ack by reading Register C, then a tick edges again.
    r.write_port(0x70, REG_C);
    let _ = r.read_port(0x71);
    assert!(r.advance_master_ticks(MASTER_CLOCK_HZ, 1));
}

#[test]
fn alarm_fires_only_on_a_time_match() {
    let mut r = Rtc::new();
    r.seed(2026, 6, 22, 1, 10, 30, 44);
    // Enable the alarm and set it for 10:30:45.
    r.write_port(0x70, REG_B);
    r.write_port(0x71, REG_B_AIE);
    r.write_port(0x70, REG_SECONDS_ALARM);
    r.write_port(0x71, 45);
    r.write_port(0x70, REG_MINUTES_ALARM);
    r.write_port(0x71, 30);
    r.write_port(0x70, REG_HOURS_ALARM);
    r.write_port(0x71, 10);
    assert!(r.advance_master_ticks(MASTER_CLOCK_HZ, 1));
    r.write_port(0x70, REG_C);
    assert_ne!(r.read_port(0x71).unwrap() & REG_C_AF, 0);

    // Move the clock off the alarm time: no match, no flag.
    r.seed(2026, 6, 22, 1, 10, 30, 46);
    assert!(!r.advance_master_ticks(MASTER_CLOCK_HZ, 1));
}

#[test]
fn alarm_wildcard_byte_matches_any_value() {
    let mut r = Rtc::new();
    r.seed(2026, 6, 22, 1, 7, 15, 3);
    r.write_port(0x70, REG_B);
    r.write_port(0x71, REG_B_AIE);
    // 0xFF seconds/minutes/hours are all "don't care": the alarm fires every
    // second.
    r.write_port(0x70, REG_SECONDS_ALARM);
    r.write_port(0x71, 0xff);
    r.write_port(0x70, REG_MINUTES_ALARM);
    r.write_port(0x71, 0xff);
    r.write_port(0x70, REG_HOURS_ALARM);
    r.write_port(0x71, 0xff);
    assert!(r.advance_master_ticks(MASTER_CLOCK_HZ, 1));
}

#[test]
fn rate_select_and_periodic_phase_are_batch_invariant() {
    let mut whole = Rtc::new();
    let mut split = Rtc::new();
    for rtc in [&mut whole, &mut split] {
        rtc.write_port(0x70, REG_A);
        rtc.write_port(0x71, 0x2f); // 2 Hz
        rtc.write_port(0x70, REG_B);
        rtc.write_port(0x71, REG_B_PIE);
    }
    assert_eq!(whole.periodic_rate_hz(), 2);
    let deadline = whole.ticks_until_periodic_irq().unwrap();
    whole.advance_master_ticks(deadline, 0);
    split.advance_master_ticks(deadline / 3, 0);
    split.advance_master_ticks(deadline - deadline / 3, 0);
    assert_eq!(whole.ram[usize::from(REG_C)], split.ram[usize::from(REG_C)]);
    assert_eq!(whole.periodic_phase, split.periodic_phase);
}

#[test]
fn legacy_periodic_rate_aliases_match_the_rtc_table() {
    let mut rtc = Rtc::new();
    for (selector, expected) in [(1, 256), (2, 128), (3, 8192), (6, 1024), (15, 2)] {
        rtc.write_port(0x70, REG_A);
        rtc.write_port(0x71, 0x20 | selector);
        assert_eq!(rtc.periodic_rate_hz(), expected, "selector {selector}");
    }
}

#[test]
fn writing_register_b_keeps_format_bits_forced() {
    let mut r = Rtc::new();
    // Try to clear the format bits and set PIE; the format bits stay set.
    r.write_port(0x70, REG_B);
    r.write_port(0x71, REG_B_PIE);
    r.write_port(0x70, REG_B);
    let b = r.read_port(0x71).unwrap();
    assert_ne!(b & REG_B_PIE, 0); // enable took
    assert_ne!(b & 0x04, 0); // DM still set (binary)
    assert_ne!(b & 0x02, 0); // 24-hour still set
}
