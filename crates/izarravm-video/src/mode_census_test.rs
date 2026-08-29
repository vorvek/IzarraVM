// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::VideoMode;

fn key(mode: VideoMode, hdisp_end: u32, vdisp_end: u32) -> ModeCensusKey {
    ModeCensusKey {
        mode,
        hdisp_end,
        vdisp_end,
        vtotal: vdisp_end + 45,
        double_scan: false,
        line_compare_active: false,
        bpp: bits_per_pixel(mode),
    }
}

#[test]
fn a_repeated_geometry_counts_up_rather_than_adding_a_row() {
    let mut census = ModeCensus::default();
    let mode_x = key(VideoMode::ModeX, 256, 480);
    census.record(mode_x);
    census.record(mode_x);
    census.record(mode_x);

    let rows: Vec<_> = census.entries().collect();
    assert_eq!(rows.len(), 1, "one geometry is one row");
    assert_eq!(*rows[0].1, 3, "the count is the number of times it was set");
}

#[test]
fn two_line_counts_at_one_pixel_height_are_different_rows() {
    // THE POINT OF THE KEY. Standard mode 13h is 200 visible lines double
    // scanned; the aspect-defeating variant is 199 lines. Both present 400
    // raster lines, so a presented pixel count cannot tell them apart.
    let mut census = ModeCensus::default();
    let mut standard = key(VideoMode::Mode13h, 320, 200);
    standard.double_scan = true;
    let jazz = key(VideoMode::Mode13h, 320, 199);
    census.record(standard);
    census.record(jazz);

    assert_eq!(census.entries().count(), 2);
}

#[test]
fn a_split_screen_is_a_different_row_from_a_full_screen() {
    let mut census = ModeCensus::default();
    let full = key(VideoMode::Planar, 320, 400);
    let mut split = full;
    split.line_compare_active = true;
    census.record(full);
    census.record(split);

    assert_eq!(census.entries().count(), 2);
}

#[test]
fn entries_come_back_in_a_stable_order() {
    // The census gets PINNED, so two runs that record the same geometries in a
    // different order must produce the same list.
    let a = key(VideoMode::Cga, 320, 200);
    let b = key(VideoMode::ModeX, 256, 480);

    let mut forward = ModeCensus::default();
    forward.record(a);
    forward.record(b);
    let mut backward = ModeCensus::default();
    backward.record(b);
    backward.record(a);

    let forward: Vec<_> = forward.entries().map(|(key, _)| *key).collect();
    let backward: Vec<_> = backward.entries().map(|(key, _)| *key).collect();
    assert_eq!(forward, backward);
}

#[test]
fn every_mode_has_a_bit_depth() {
    assert_eq!(bits_per_pixel(VideoMode::Text), 4);
    assert_eq!(bits_per_pixel(VideoMode::Planar), 4);
    assert_eq!(bits_per_pixel(VideoMode::Mode13h), 8);
    assert_eq!(bits_per_pixel(VideoMode::ModeX), 8);
    assert_eq!(bits_per_pixel(VideoMode::Cga), 2);
    assert_eq!(bits_per_pixel(VideoMode::Hercules), 1);
}

#[test]
fn a_fresh_census_is_empty() {
    assert!(ModeCensus::default().entries().next().is_none());
}

#[test]
fn a_distira_geometry_counts_the_same_way() {
    let mut census = DistiraCensus::default();
    census.record(DistiraCensusKey {
        width: 640,
        height: 480,
    });
    census.record(DistiraCensusKey {
        width: 640,
        height: 480,
    });
    census.record(DistiraCensusKey {
        width: 512,
        height: 384,
    });

    let rows: Vec<_> = census.entries().collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0.width, 512, "sorted, so the smaller width leads");
    assert_eq!(*rows[1].1, 2);
}
