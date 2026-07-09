// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

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
    // Power-on Register B has every enable clear.
    assert!(!r.tick_interrupts(1));
    r.write_port(0x70, REG_C);
    assert_eq!(r.read_port(0x71), Some(0));
}

#[test]
fn enabled_periodic_interrupt_sets_pf_and_irqf_then_clears_on_read() {
    let mut r = Rtc::new();
    // Enable the periodic interrupt (PIE, bit 6).
    r.write_port(0x70, REG_B);
    r.write_port(0x71, REG_B_PIE);
    assert!(r.tick_interrupts(1)); // rising edge of IRQF
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
    assert!(r.tick_interrupts(1)); // first edge
    assert!(!r.tick_interrupts(1)); // still pending, no new edge
    // Ack by reading Register C, then a tick edges again.
    r.write_port(0x70, REG_C);
    let _ = r.read_port(0x71);
    assert!(r.tick_interrupts(1));
}

#[test]
fn alarm_fires_only_on_a_time_match() {
    let mut r = Rtc::new();
    r.seed(2026, 6, 22, 1, 10, 30, 45);
    // Enable the alarm and set it for 10:30:45.
    r.write_port(0x70, REG_B);
    r.write_port(0x71, REG_B_AIE);
    r.write_port(0x70, REG_SECONDS_ALARM);
    r.write_port(0x71, 45);
    r.write_port(0x70, REG_MINUTES_ALARM);
    r.write_port(0x71, 30);
    r.write_port(0x70, REG_HOURS_ALARM);
    r.write_port(0x71, 10);
    assert!(r.tick_interrupts(1));
    r.write_port(0x70, REG_C);
    assert_ne!(r.read_port(0x71).unwrap() & REG_C_AF, 0);

    // Move the clock off the alarm time: no match, no flag.
    r.seed(2026, 6, 22, 1, 10, 30, 46);
    assert!(!r.tick_interrupts(1));
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
    assert!(r.tick_interrupts(1));
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
