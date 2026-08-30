// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_video::{
    DistiraCensus, DistiraCensusKey, ModeCensus, ModeCensusKey, VideoMode, bits_per_pixel,
};

fn mode_x_256() -> ModeCensusKey {
    ModeCensusKey {
        mode: VideoMode::ModeX,
        hdisp_end: 256,
        vdisp_end: 480,
        vtotal: 525,
        double_scan: false,
        line_compare_active: false,
        bpp: bits_per_pixel(VideoMode::ModeX),
    }
}

#[test]
fn an_empty_census_renders_two_empty_lists_rather_than_nothing() {
    // A missing section reads as "not measured". An empty list reads as
    // "measured, and the guest never went there". They are different facts and
    // the board grades on the difference, so the shape must not collapse.
    let json = mode_census_json(
        &ModeCensus::default(),
        &DistiraCensus::default(),
        None,
        &[],
        &[],
        Default::default(),
        0,
    );
    assert_eq!(json["schema"], "izarravm-mode-census-v1");
    assert_eq!(json["vga"].as_array().expect("vga is a list").len(), 0);
    assert_eq!(
        json["distira"].as_array().expect("distira is a list").len(),
        0
    );
}

#[test]
fn a_vga_row_carries_every_key_field_and_its_count() {
    let mut vga = ModeCensus::default();
    vga.record(mode_x_256());
    vga.record(mode_x_256());

    let json = mode_census_json(
        &vga,
        &DistiraCensus::default(),
        None,
        &[],
        &[],
        Default::default(),
        0,
    );
    let row = &json["vga"][0];
    assert_eq!(row["mode"], "ModeX");
    assert_eq!(row["hdisp_end"], 256);
    assert_eq!(row["vdisp_end"], 480);
    assert_eq!(
        row["source_lines"], 480,
        "not double scanned, so they are equal"
    );
    assert_eq!(row["vtotal"], 525);
    assert_eq!(row["double_scan"], false);
    assert_eq!(row["line_compare_active"], false);
    assert_eq!(row["bpp"], 8);
    assert_eq!(row["entries"], 2);
}

#[test]
fn two_line_counts_at_one_pixel_height_render_as_two_rows() {
    // The rendering must not collapse what the key separates. 200 lines double
    // scanned and 199 lines single scanned both present 400 raster lines.
    let mut vga = ModeCensus::default();
    let mut standard = mode_x_256();
    standard.mode = VideoMode::Mode13h;
    standard.hdisp_end = 320;
    standard.vdisp_end = 200;
    standard.double_scan = true;
    let mut jazz = standard;
    jazz.vdisp_end = 199;
    jazz.double_scan = false;
    vga.record(standard);
    vga.record(jazz);

    let json = mode_census_json(
        &vga,
        &DistiraCensus::default(),
        None,
        &[],
        &[],
        Default::default(),
        0,
    );
    let rows = json["vga"].as_array().expect("vga is a list");
    assert_eq!(rows.len(), 2);
    // MEASURED on Psycho Pinball 2026-08-29: standard mode 13h reports
    // vdisp_end 400 with double_scan set, NOT 200. vdisp_end counts raster
    // lines. source_lines is what a game's own resolution means, and it is the
    // field an acceptance rule has to read.
    let raster: Vec<_> = rows.iter().map(|row| row["vdisp_end"].as_u64()).collect();
    assert!(raster.contains(&Some(199)));
    assert!(raster.contains(&Some(200)));
    let source: Vec<_> = rows
        .iter()
        .map(|row| row["source_lines"].as_u64())
        .collect();
    assert!(
        source.contains(&Some(199)),
        "199 single scanned is 199 source rows"
    );
    assert!(
        source.contains(&Some(100)),
        "200 double scanned is 100 source rows"
    );
}

#[test]
fn source_lines_halves_a_double_scanned_mode() {
    // The case that made this field necessary. Standard mode 13h is 200 source
    // rows double scanned to 400 raster lines; an aspect-defeating 199-line
    // mode is 199 source rows single scanned to 199. Both are "mode 13h" and
    // only source_lines tells them apart at a glance.
    let mut vga = ModeCensus::default();
    let mut standard = mode_x_256();
    standard.mode = VideoMode::Mode13h;
    standard.hdisp_end = 320;
    standard.vdisp_end = 400;
    standard.double_scan = true;
    vga.record(standard);

    let json = mode_census_json(
        &vga,
        &DistiraCensus::default(),
        None,
        &[],
        &[],
        Default::default(),
        0,
    );
    assert_eq!(json["vga"][0]["vdisp_end"], 400);
    assert_eq!(json["vga"][0]["source_lines"], 200);
}

#[test]
fn a_distira_row_carries_its_size_and_count() {
    let mut distira = DistiraCensus::default();
    distira.record(DistiraCensusKey {
        width: 640,
        height: 480,
    });

    let json = mode_census_json(
        &ModeCensus::default(),
        &distira,
        None,
        &[],
        &[],
        Default::default(),
        0,
    );
    let row = &json["distira"][0];
    assert_eq!(row["width"], 640);
    assert_eq!(row["height"], 480);
    assert_eq!(row["entries"], 1);
}
