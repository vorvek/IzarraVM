// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Tests for the windowed IPE trace.
//!
//! What each one would catch, since a measurement instrument that silently records nothing is
//! the classic fixture-that-cannot-fail:
//!  * `armed_short_run_records_windows` FAILS if the run-loop hook never fires (it asserts a
//!    non-empty window list AND that the recorded entries sum to the machine's own counter), so
//!    deleting the `close_ipe_window` call site cannot pass.
//!  * `render_is_self_consistent` pins the file format that the sweep script parses.
//!  * `disarmed_run_records_nothing` FAILS if the sentinel is ever initialised to anything but
//!    `u64::MAX`, which is the one way a default-off instrument turns itself on.

use super::*;
use izarravm_core::{GswMode, VideoCard};
use izarravm_machine::{IpeWindow, Machine, MachineProfile};

/// A tight 16-bit loop the direct backend admits, then a DOS exit. `dec cx; jnz` repeated
/// 0x8000 times with an outer repeat, so the run retires enough entries to close several
/// one-entry windows.
const LOOP_PROGRAM: &[u8] = &[
    0xB8, 0x00, 0x08, // mov ax, 0x0800   ; outer count
    0xB9, 0xFF, 0x7F, // mov cx, 0x7FFF   ; inner count
    0x49, // dec cx
    0x75, 0xFD, // jnz -3
    0x48, // dec ax
    0x75, 0xF6, // jnz -10 (reload cx, spin again)
    0xB8, 0x00, 0x4C, // mov ax, 0x4c00
    0xCD, 0x21, // int 21h
];

/// A fresh temp directory for one test's trace file, named per process and nanosecond so
/// parallel test threads never share one.
fn trace_test_dir(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "izarravm-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn loop_machine() -> Machine {
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw486;
    Machine::new_raw_program(profile, LOOP_PROGRAM).expect("build raw machine")
}

fn parse_rows(text: &str) -> Vec<(u64, u64, u64, f64, String)> {
    let mut rows = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.starts_with("window_index") {
            continue;
        }
        let fields: Vec<&str> = line.split(',').collect();
        assert_eq!(fields.len(), 5, "row must have five fields: {line}");
        rows.push((
            fields[0].parse().expect("index"),
            fields[1].parse().expect("entries"),
            fields[2].parse().expect("insns"),
            fields[3].parse().expect("ipe"),
            fields[4].to_string(),
        ));
    }
    rows
}

#[test]
fn armed_short_run_records_windows() {
    let dir = trace_test_dir("ipe-window-trace");
    let path = dir.join("ipe.csv");
    let mut machine = loop_machine();
    // One entry per window: a short deterministic run cannot retire 2^22 entries, and the
    // window ARITHMETIC is what this test pins, not the shipped size.
    machine.arm_ipe_window_trace(1);
    machine
        .run_until_halt_or_cycles(200_000_000)
        .expect("run loop program");

    let entries = machine.cpu().perf_counters().jit_direct_entries;
    let insns = machine.cpu().perf_counters().jit_direct_insns;
    assert!(
        entries > 1,
        "the fixture must retire direct entries: {entries}"
    );
    assert!(
        !machine.ipe_windows().is_empty(),
        "an armed run must close at least one window"
    );

    write_trace(&path, &machine).expect("write trace");
    let text = std::fs::read_to_string(&path).expect("read trace");
    assert!(text.starts_with("# izarravm-ipe-window-trace-v1 window_entries=1"));
    let rows = parse_rows(&text);
    let tail = machine.ipe_window_tail();
    assert_eq!(
        rows.len(),
        machine.ipe_windows().len() + usize::from(tail.is_some()),
        "one row per closed window plus the tail"
    );

    let mut summed_entries = 0u64;
    let mut summed_insns = 0u64;
    for (position, (index, row_entries, row_insns, ipe, kind)) in rows.iter().enumerate() {
        assert_eq!(*index, position as u64, "window indices must be monotone");
        assert!(*row_entries > 0, "a recorded window must carry entries");
        let expected = *row_insns as f64 / *row_entries as f64;
        assert!(
            (ipe - expected).abs() < 1e-3,
            "ipe must be insns/entries: {ipe} vs {expected}"
        );
        let last = position + 1 == rows.len();
        assert_eq!(
            kind == "partial",
            last && tail.is_some(),
            "only a trailing tail row is marked partial"
        );
        summed_entries += row_entries;
        summed_insns += row_insns;
    }
    // The rows must account for the whole run, which is what makes the assertions above
    // non-vacuous: a hook that fired once and then stalled would leave this short.
    assert_eq!(summed_entries, entries);
    assert_eq!(summed_insns, insns);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn disarmed_run_records_nothing() {
    let mut machine = loop_machine();
    machine
        .run_until_halt_or_cycles(200_000_000)
        .expect("run loop program");
    assert!(machine.ipe_windows().is_empty());
    assert!(machine.ipe_window_tail().is_none());
    assert_eq!(machine.ipe_window_size(), 0);
    assert!(
        machine.cpu().perf_counters().jit_direct_entries > 1,
        "the disarmed run must still be the same workload"
    );
}

#[test]
fn render_is_self_consistent() {
    let windows = [
        IpeWindow {
            index: 0,
            entries: 4_194_304,
            direct_insns: 8_388_608,
            partial: false,
        },
        IpeWindow {
            index: 1,
            entries: 4_194_400,
            direct_insns: 4_194_400,
            partial: false,
        },
    ];
    let tail = IpeWindow {
        index: 2,
        entries: 100,
        direct_insns: 250,
        partial: true,
    };
    let text = render(1 << 22, &windows, Some(tail));
    let rows = parse_rows(&text);
    assert_eq!(rows.len(), 3);
    assert!((rows[0].3 - 2.0).abs() < 1e-9);
    assert!((rows[1].3 - 1.0).abs() < 1e-9);
    assert!((rows[2].3 - 2.5).abs() < 1e-9);
    assert_eq!(rows[2].4, "partial");
    assert!(text.contains("# totals windows=3 entries=8388804 direct_insns=12583258"));
    // A window with no entries reads as 0.0 rather than NaN, so a min-IPE consumer never sees
    // a value it cannot order.
    let empty = IpeWindow {
        index: 0,
        entries: 0,
        direct_insns: 0,
        partial: true,
    };
    assert_eq!(empty.ipe(), 0.0);
    assert!(render(4, &[], Some(empty)).contains(",0.000000,partial"));
}
