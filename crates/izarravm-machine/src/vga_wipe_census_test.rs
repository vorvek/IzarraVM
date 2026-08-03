// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn armed() -> VgaWipeCensus {
    VgaWipeCensus {
        enabled: true,
        ..VgaWipeCensus::default()
    }
}

#[test]
fn snapshot_is_none_when_the_gate_is_off() {
    let mut census = VgaWipeCensus {
        enabled: false,
        ..VgaWipeCensus::default()
    };
    census.record_token_change(0x3C5, 0x02, 0x04, 1, 4);
    assert_eq!(census.snapshot(), None);
}

#[test]
fn identical_writes_share_one_row_and_distinct_ones_do_not() {
    let mut census = armed();
    census.record_token_change(0x3C5, 0x02, 0x04, 1, 4);
    census.record_token_change(0x3C5, 0x02, 0x04, 4, 1);
    census.record_token_change(0x3C5, 0x02, 0x08, 1, 5);
    // Same port and value, different index register in force: a different row.
    census.record_token_change(0x3C5, 0x04, 0x04, 1, 0);
    let snapshot = census.snapshot().expect("armed");
    assert_eq!(snapshot.events, 4);
    assert_eq!(snapshot.rows.len(), 3);
    assert_eq!(snapshot.rows[0].count, 2);
    assert_eq!(snapshot.rows[0].port, 0x3C5);
    assert_eq!(snapshot.rows[0].selector, 0x02);
    assert_eq!(snapshot.rows[0].value, 0x04);
    assert_eq!(snapshot.key_overflow, 0);
}

#[test]
fn transitions_are_counted_by_before_and_after_token() {
    let mut census = armed();
    census.record_token_change(0x3C5, 0x02, 0x04, 1, 4);
    census.record_token_change(0x3C5, 0x02, 0x04, 1, 4);
    census.record_token_change(0x3C5, 0x02, 0x01, 4, 1);
    let snapshot = census.snapshot().expect("armed");
    assert_eq!(snapshot.transitions[1][4], 2);
    assert_eq!(snapshot.transitions[4][1], 1);
    assert_eq!(snapshot.transitions[0][0], 0);
}

#[test]
fn applies_same_token_counts_only_a_repeat_of_the_previous_application() {
    let mut census = armed();
    census.record_apply(1, 100);
    census.record_apply(0, 200);
    census.record_apply(0, 400);
    census.record_apply(1, 500);
    let snapshot = census.snapshot().expect("armed");
    assert_eq!(snapshot.applies, 4);
    assert_eq!(snapshot.applies_same_token, 1);
}

#[test]
fn a_first_application_with_token_zero_is_not_a_repeat() {
    let mut census = armed();
    census.record_apply(0, 10);
    let snapshot = census.snapshot().expect("armed");
    assert_eq!(snapshot.applies, 1);
    assert_eq!(snapshot.applies_same_token, 0);
    // The first application has no predecessor, so it contributes no gap.
    assert_eq!(snapshot.gap_buckets.iter().sum::<u64>(), 0);
}

#[test]
fn gap_buckets_are_log2_of_the_instruction_distance() {
    let mut census = armed();
    census.record_apply(1, 0);
    census.record_apply(0, 1); // gap 1 -> bucket 0
    census.record_apply(1, 3); // gap 2 -> bucket 1
    census.record_apply(0, 8); // gap 5 -> bucket 2
    census.record_apply(1, 1032); // gap 1024 -> bucket 10
    let snapshot = census.snapshot().expect("armed");
    assert_eq!(snapshot.gap_buckets[0], 1);
    assert_eq!(snapshot.gap_buckets[1], 1);
    assert_eq!(snapshot.gap_buckets[2], 1);
    assert_eq!(snapshot.gap_buckets[10], 1);
    assert_eq!(snapshot.gap_buckets.iter().sum::<u64>(), 4);
}

#[test]
fn the_histogram_reports_keys_it_could_not_hold_rather_than_dropping_them_silently() {
    let mut census = armed();
    for slot in 0..KEY_SLOTS {
        let port = u16::try_from(slot).expect("fits");
        census.record_token_change(port, 0, 0, 1, 0);
    }
    census.record_token_change(0xFFFF, 0, 0, 1, 0);
    let snapshot = census.snapshot().expect("armed");
    assert_eq!(snapshot.rows.len(), KEY_SLOTS);
    assert_eq!(snapshot.key_overflow, 1);
    assert_eq!(snapshot.events, KEY_SLOTS as u64 + 1);
}
