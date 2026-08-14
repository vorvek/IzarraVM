// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Tests for the per-window entry-target tally.
//!
//! `truncation_keeps_distinct_whole` is the one that matters: the top-N list is the only lossy
//! part of this instrument, and the failure that would make the v2 trace lie is `distinct` being
//! computed from the TRUNCATED list rather than the map, which reads as "this window re-enters
//! eight blocks" for a window that re-enters thousands.

use super::*;

#[test]
fn counts_repeats_and_orders_by_weight() {
    let mut tally = IpeEntryTally::default();
    for _ in 0..5 {
        tally.note_entry(0x1000);
    }
    for _ in 0..9 {
        tally.note_entry(0x2000);
    }
    tally.note_entry(0x3000);
    let snap = tally.snapshot(8);
    assert_eq!(snap.distinct, 3);
    assert_eq!(snap.total, 15);
    assert_eq!(snap.top, vec![(0x2000, 9), (0x1000, 5), (0x3000, 1)]);
    // The counts can never exceed the entries the window observed.
    assert_eq!(snap.top.iter().map(|(_, c)| c).sum::<u64>(), snap.total);
}

#[test]
fn truncation_keeps_distinct_whole() {
    let mut tally = IpeEntryTally::default();
    // 40 targets, weight descending, so the top-8 cut is unambiguous.
    for i in 0..40u32 {
        for _ in 0..(40 - i) {
            tally.note_entry(0x4000 + i * 16);
        }
    }
    let snap = tally.snapshot(8);
    assert_eq!(snap.top.len(), 8, "the list is cut at top_n");
    assert_eq!(
        snap.distinct, 40,
        "truncation must not touch the distinct count"
    );
    assert_eq!(snap.total, (1..=40u64).sum::<u64>());
    assert_eq!(snap.top[0], (0x4000, 40));
    assert!(
        snap.top.iter().map(|(_, c)| c).sum::<u64>() < snap.total,
        "the truncated tail is still inside total"
    );
}

#[test]
fn equal_counts_break_ties_by_linear() {
    let mut tally = IpeEntryTally::default();
    for linear in [0x30u32, 0x10, 0x20] {
        tally.note_entry(linear);
    }
    assert_eq!(tally.snapshot(8).top, vec![(0x10, 1), (0x20, 1), (0x30, 1)]);
}

#[test]
fn reset_starts_a_fresh_window() {
    let mut tally = IpeEntryTally::default();
    tally.note_entry(0x1000);
    tally.reset();
    assert_eq!(tally.snapshot(8), IpeEntryTargets::default());
    tally.note_entry(0x2000);
    let snap = tally.snapshot(8);
    assert_eq!(snap.distinct, 1);
    assert_eq!(snap.total, 1);
    assert_eq!(snap.top, vec![(0x2000, 1)]);
}
