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
//!    `u64::MAX`, which is the one way a default-off instrument turns itself on. It also asserts
//!    the CPU-side entry tally is absent, which is the v2 way for a default-off instrument to
//!    turn itself on -- and the expensive way.
//!  * `armed_short_run_records_windows` additionally pins the v2 columns: `retired` must SUM to
//!    the machine's own `perf.instructions` (so deleting the retired capture, or capturing it
//!    once and never advancing the window start, fails), `coverage` must equal
//!    `direct_insns / retired` and land in [0, 1], and every window must name at least one entry
//!    target whose counts do not exceed the window's entries.
//!  * `render_truncates_top_targets_without_losing_distinct` is the mutation guard for the one
//!    lossy column: `distinct_targets` is computed from the whole map, never from the truncated
//!    list, so a window re-entering thousands of blocks cannot read as re-entering eight.

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

/// One parsed v2 row. A struct rather than a tuple now that the row is nine columns wide: the
/// v1 tuple would have made every assertion below a positional index.
struct Row {
    index: u64,
    entries: u64,
    insns: u64,
    ipe: f64,
    retired: u64,
    coverage: f64,
    distinct_targets: u64,
    top_targets: Vec<(u32, u64)>,
    kind: String,
}

/// Parse `top_targets` the way a consumer must: strip the quotes, split on `;`, split each pair
/// on `:`. An empty field is an empty list, not a one-element list of nothing.
fn parse_top_targets(field: &str) -> Vec<(u32, u64)> {
    let inner = field
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or_else(|| panic!("top_targets must be quoted: {field}"));
    if inner.is_empty() {
        return Vec::new();
    }
    inner
        .split(';')
        .map(|pair| {
            let (linear, count) = pair.split_once(':').expect("pair is linear:count");
            let linear = linear.strip_prefix("0x").expect("linear is 0x-prefixed");
            (
                u32::from_str_radix(linear, 16).expect("linear hex"),
                count.parse().expect("count"),
            )
        })
        .collect()
}

fn parse_rows(text: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.starts_with("window_index") {
            continue;
        }
        let fields: Vec<&str> = line.split(',').collect();
        assert_eq!(fields.len(), 9, "row must have nine fields: {line}");
        rows.push(Row {
            index: fields[0].parse().expect("index"),
            entries: fields[1].parse().expect("entries"),
            insns: fields[2].parse().expect("insns"),
            ipe: fields[3].parse().expect("ipe"),
            retired: fields[4].parse().expect("retired"),
            coverage: fields[5].parse().expect("coverage"),
            distinct_targets: fields[6].parse().expect("distinct_targets"),
            top_targets: parse_top_targets(fields[7]),
            kind: fields[8].to_string(),
        });
    }
    rows
}

#[test]
fn armed_short_run_records_windows() {
    let dir = trace_test_dir("ipe-window-trace");
    let path = dir.join("ipe.csv");
    let mut machine = loop_machine();
    // Read BEFORE arming: the windows measure retired instructions from the arming point, so the
    // sum below has to be compared against the same origin.
    let retired_at_arm = machine.cpu().perf_counters().instructions;
    // One entry per window: a short deterministic run cannot retire 2^22 entries, and the
    // window ARITHMETIC is what this test pins, not the shipped size.
    machine.arm_ipe_window_trace(1);
    machine
        .run_until_halt_or_cycles(200_000_000)
        .expect("run loop program");

    let entries = machine.cpu().perf_counters().jit_direct_entries;
    let insns = machine.cpu().perf_counters().jit_direct_insns;
    let retired = machine.cpu().perf_counters().instructions - retired_at_arm;
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
    assert!(text.starts_with("# izarravm-ipe-window-trace-v2 window_entries=1"));
    let rows = parse_rows(&text);
    let tail = machine.ipe_window_tail();
    assert_eq!(
        rows.len(),
        machine.ipe_windows().len() + usize::from(tail.is_some()),
        "one row per closed window plus the tail"
    );

    let mut summed_entries = 0u64;
    let mut summed_insns = 0u64;
    let mut summed_retired = 0u64;
    for (position, row) in rows.iter().enumerate() {
        assert_eq!(
            row.index, position as u64,
            "window indices must be monotone"
        );
        let last = position + 1 == rows.len();
        // Only the trailing row may be entry-free: a closed window closes ON an entry.
        assert!(
            row.entries > 0 || last,
            "a closed window must carry entries"
        );
        let expected = if row.entries == 0 {
            0.0
        } else {
            row.insns as f64 / row.entries as f64
        };
        assert!(
            (row.ipe - expected).abs() < 1e-3,
            "ipe must be insns/entries: {} vs {expected}",
            row.ipe
        );
        // v2: coverage is the direct share of everything the window retired, and a share is in
        // [0, 1]. Anything above 1 means `instructions` and `jit_direct_insns` have diverged.
        assert!(
            row.retired >= row.insns,
            "a window cannot run more native instructions than it retired: {} vs {}",
            row.insns,
            row.retired
        );
        let expected_coverage = if row.retired == 0 {
            0.0
        } else {
            row.insns as f64 / row.retired as f64
        };
        assert!(
            (row.coverage - expected_coverage).abs() < 1e-3,
            "coverage must be insns/retired: {} vs {expected_coverage}",
            row.coverage
        );
        assert!(
            (0.0..=1.0).contains(&row.coverage),
            "coverage must be a share: {}",
            row.coverage
        );
        // v2: every window that took an entry must NAME at least one target, and the named
        // counts can never exceed the entries the window saw.
        if row.entries > 0 {
            assert!(
                row.distinct_targets >= 1,
                "an entry-carrying window must name a target"
            );
            assert!(!row.top_targets.is_empty(), "top_targets must not be empty");
        }
        assert!(
            row.top_targets.len() as u64 <= row.distinct_targets,
            "the top list cannot be longer than the distinct count"
        );
        assert!(
            row.top_targets.iter().map(|(_, c)| c).sum::<u64>() <= row.entries,
            "target counts must fit inside the window's entries"
        );
        assert_eq!(
            row.kind == "partial",
            last && tail.is_some(),
            "only a trailing tail row is marked partial"
        );
        summed_entries += row.entries;
        summed_insns += row.insns;
        summed_retired += row.retired;
    }
    // The rows must account for the whole run, which is what makes the assertions above
    // non-vacuous: a hook that fired once and then stalled would leave this short.
    assert_eq!(summed_entries, entries);
    assert_eq!(summed_insns, insns);
    // v2 mutation guard: dropping the per-window retired capture, or capturing it once without
    // advancing the window start, both leave this sum wrong.
    assert_eq!(
        summed_retired, retired,
        "per-window retired must sum to the run's retired instructions"
    );
    assert!(
        summed_retired > summed_insns,
        "the fixture must retire some interpreted work, or coverage is untested"
    );
    // The same invariant read off the ARTIFACT rather than the rows, which is the form a reader
    // with only the file has: the totals line must carry the retired sum too.
    assert!(
        text.contains(&format!(" retired={retired} ")),
        "the totals line must carry the retired sum: {retired}"
    );

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
    // v2: the entry-target tally is the only part of this instrument that costs anything, and
    // this is the assertion that it never allocates itself into existence on a normal run. The
    // hot-path seam it hangs off (`run_direct_block`, at the `jit_direct_entries` increment) is
    // one `Option` null test when this is `None`.
    assert!(
        machine.cpu().ipe_entry_targets(8).is_none(),
        "a disarmed run must not build an entry-target tally"
    );
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
            retired: 8_388_608,
            distinct_targets: 3,
            top_targets: vec![(0x1234, 4_000_000), (0x0002_0000, 194_304)],
            partial: false,
        },
        IpeWindow {
            index: 1,
            entries: 4_194_400,
            direct_insns: 4_194_400,
            retired: 8_388_800,
            distinct_targets: 1,
            top_targets: vec![(0xF000_0010, 4_194_400)],
            partial: false,
        },
    ];
    let tail = IpeWindow {
        index: 2,
        entries: 100,
        direct_insns: 250,
        retired: 1_000,
        distinct_targets: 0,
        top_targets: Vec::new(),
        partial: true,
    };
    let text = render(1 << 22, &windows, Some(tail));
    let rows = parse_rows(&text);
    assert_eq!(rows.len(), 3);
    assert!((rows[0].ipe - 2.0).abs() < 1e-9);
    assert!((rows[1].ipe - 1.0).abs() < 1e-9);
    assert!((rows[2].ipe - 2.5).abs() < 1e-9);
    // Coverage: full, exactly half, and a quarter.
    assert!((rows[0].coverage - 1.0).abs() < 1e-9);
    assert!((rows[1].coverage - 0.5).abs() < 1e-9);
    assert!((rows[2].coverage - 0.25).abs() < 1e-9);
    assert_eq!(rows[0].retired, 8_388_608);
    assert_eq!(rows[0].distinct_targets, 3);
    assert_eq!(
        rows[0].top_targets,
        vec![(0x1234, 4_000_000), (0x0002_0000, 194_304)]
    );
    // The list is shorter than the distinct count and that is NOT a defect: it is what
    // truncation looks like from the reader's side, and `distinct_targets` still carries the
    // whole answer.
    assert!(rows[0].top_targets.len() < rows[0].distinct_targets as usize);
    assert!(text.contains(",\"0x00001234:4000000;0x00020000:194304\","));
    // An empty list round-trips as an empty quoted field, never as a stray column.
    assert!(rows[2].top_targets.is_empty());
    assert!(text.contains(",\"\",partial"));
    assert_eq!(rows[2].kind, "partial");
    // retired = 8_388_608 + 8_388_800 + 1_000; coverage = 12_583_258 / 16_778_408.
    assert!(text.contains(
        "# totals windows=3 entries=8388804 direct_insns=12583258 ipe=1.500006 \
         retired=16778408 coverage=0.749967"
    ));
    // A window with no entries reads as 0.0 rather than NaN, so a min-IPE consumer never sees
    // a value it cannot order. Same for coverage against zero retired.
    let empty = IpeWindow {
        partial: true,
        ..IpeWindow::default()
    };
    assert_eq!(empty.ipe(), 0.0);
    assert_eq!(empty.coverage(), 0.0);
    assert!(render(4, &[], Some(empty)).contains("0,0,0,0.000000,0,0.000000,0,\"\",partial"));
}

/// The v2 mutation guard for the one lossy column, exercised end to end rather than only at the
/// tally: a window that re-enters far more targets than the top list holds must still report the
/// true `distinct_targets`, and its top list must be a PREFIX of the truth by weight.
#[test]
fn render_truncates_top_targets_without_losing_distinct() {
    let top: Vec<(u32, u64)> = (0..izarravm_machine::IPE_TOP_TARGETS as u32)
        .map(|i| (0x1000 + i * 16, 1_000 - u64::from(i)))
        .collect();
    let window = IpeWindow {
        index: 0,
        entries: 500_000,
        direct_insns: 1_000_000,
        retired: 2_000_000,
        distinct_targets: 12_345,
        top_targets: top.clone(),
        partial: false,
    };
    let text = render(1 << 22, std::slice::from_ref(&window), None);
    let rows = parse_rows(&text);
    assert_eq!(rows[0].distinct_targets, 12_345);
    assert_eq!(rows[0].top_targets, top);
    assert_eq!(rows[0].top_targets.len(), izarravm_machine::IPE_TOP_TARGETS);
    assert!(
        rows[0].top_targets.iter().map(|(_, c)| c).sum::<u64>() < rows[0].entries,
        "the truncated tail is still inside the window's entries"
    );
}
