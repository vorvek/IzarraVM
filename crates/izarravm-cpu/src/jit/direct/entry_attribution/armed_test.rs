// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Fixtures for the armed half of the entry-attribution observer (moved out of `armed.rs`
//! to satisfy the *_test.rs layout policy).

use super::*;

/// The tables are indexed by enum discriminant, so a phase added without a name -- or named
/// out of order -- mis-labels every export from then on. Checking `len()` against the constant
/// beside it proves nothing (the arrays are DECLARED at that length); checking that each name
/// carries its own index does.
#[test]
fn phase_names_are_in_discriminant_order() {
    for (index, name) in PHASE_NAMES.iter().enumerate() {
        let prefix = name
            .split('_')
            .next()
            .expect("a phase name has a P<n> prefix");
        assert_eq!(prefix, format!("P{index}"), "{name} sits at index {index}");
    }
    // And the enum agrees with the table it is used to index.
    assert_eq!(Phase::DispatchGates as usize, 0);
    assert_eq!(Phase::NativePreamble as usize, 8);
    assert_eq!(Phase::NativeBody as usize, 9);
    assert_eq!(Phase::Compile as usize, 14);
    assert_eq!(Phase::Outliers as usize, N_PHASES - 1);
    assert_eq!(Population::Compile as usize, N_POPULATIONS - 1);
    assert_eq!(FallbackTag::Skipped as usize, N_FALLBACK_TAGS - 1);
}

/// Every refusal and compile site must name a DISTINCT `run.rs` line, and the lines must be
/// non-decreasing down each table -- the tables are read as source order by the report and by
/// `PRE_P0_REFUSAL_SITES`.
#[test]
fn site_tables_are_distinct_and_in_source_order() {
    let mut previous = 0;
    for (label, line) in REFUSAL_SITES {
        assert!(line > previous, "{label} at {line} is out of source order");
        previous = line;
    }
    let mut previous = 0;
    for (label, line) in COMPILE_SITES {
        assert!(line > previous, "{label} at {line} is out of source order");
        previous = line;
    }
}

/// `run.rs` as the test harness sees it. The tables below quote line numbers in it, and a
/// line number is the one kind of citation that rots silently: every edit above a site moves
/// it, nothing fails, and the export goes on labelling refusals with lines that now hold
/// something else. This is the fixture that makes that impossible.
const RUN_RS: &str = include_str!("../../../run.rs");

fn run_rs_line(line: u32) -> &'static str {
    RUN_RS
        .lines()
        .nth(line as usize - 1)
        .unwrap_or_else(|| panic!("run.rs has no line {line}"))
}

/// Every cited line must really carry the return it claims, AND the macro call naming that
/// site must sit immediately above it. Both halves matter: the first catches a table that
/// drifted off its site, the second catches two sites that swapped labels while both still
/// pointed at some `return`.
#[test]
fn the_refusal_and_compile_tables_name_the_lines_they_claim() {
    for (index, (label, line)) in REFUSAL_SITES.iter().enumerate() {
        let text = run_rs_line(*line);
        assert!(
            text.contains("return "),
            "refusal site {label} cites run.rs:{line}, which reads {text:?}"
        );
        let call = format!("ea_refusal!(site::{})", label.to_ascii_uppercase());
        let window: String = RUN_RS
            .lines()
            .skip(*line as usize - 5)
            .take(5)
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        assert!(
            window.contains(&call),
            "refusal site {index} ({label}) cites run.rs:{line}, but {call} is not above it"
        );
    }
    for (label, line) in COMPILE_SITES {
        if label == "installed_fall_through" {
            // The seventh exit is a fall-through, not a return: `ea_mark_probe_tail!` decides
            // between P2 and P14 there.
            let window: String = RUN_RS
                .lines()
                .skip(line as usize - 3)
                .take(3)
                .collect::<Vec<_>>()
                .join(
                    "
",
                );
            assert!(
                window.contains("ea_mark_probe_tail!"),
                "compile site {label} cites run.rs:{line} without the tail macro above it"
            );
            continue;
        }
        let text = run_rs_line(line);
        assert!(
            text.contains("return "),
            "compile site {label} cites run.rs:{line}, which reads {text:?}"
        );
        let call = format!(
            "ea_compile_site!(compile_site::{})",
            label.to_ascii_uppercase()
        );
        let window: String = RUN_RS
            .lines()
            .skip(line as usize - 5)
            .take(5)
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        assert!(
            window.contains(&call),
            "compile site {label} cites run.rs:{line}, but {call} is not above it"
        );
    }
}

/// `P0_MARK_LINE` partitions the refusal table, so it has to be the line the P0 mark is on.
#[test]
fn the_p0_mark_line_is_where_the_p0_mark_is() {
    assert!(
        run_rs_line(P0_MARK_LINE).contains("ea_mark!(Phase::DispatchGates)"),
        "P0_MARK_LINE = {P0_MARK_LINE} reads {:?}",
        run_rs_line(P0_MARK_LINE)
    );
}

#[test]
fn pre_p0_sites_are_exactly_the_returns_above_the_p0_mark() {
    // Every refusal site with a line below `P0_MARK_LINE` must be in the pre-P0 set, and no
    // site at or after it may be.
    for (index, (label, line)) in REFUSAL_SITES.iter().enumerate() {
        let listed = PRE_P0_REFUSAL_SITES.contains(&index);
        assert_eq!(
            listed,
            *line < P0_MARK_LINE,
            "{label} at {line} is on the wrong side of the P0 mark"
        );
    }
}

/// `native_bin_index` and `native_bin_parts` must be exact inverses over the whole domain,
/// and the index must stay inside the array it addresses. The exporter decodes with
/// `native_bin_parts`, so a packing change that misses one of the two silently re-labels every
/// bin in the JSON.
#[test]
fn the_native_bin_index_and_its_inverse_round_trip() {
    let mut seen = std::collections::HashSet::new();
    for self_loop in [false, true] {
        for hops in 0..3u32 {
            for insns in 1..=N_INSN_BINS as u64 {
                let bin = native_bin_index(insns, hops, self_loop);
                assert!(bin < N_BINS);
                assert!(seen.insert(bin), "bin {bin} was produced twice");
                assert_eq!(
                    native_bin_parts(bin),
                    (insns as usize, hops as usize, self_loop)
                );
            }
        }
    }
    assert_eq!(seen.len(), N_BINS);
}

/// The saturating edges: zero instructions folds up into bin 1, anything past 32 folds down
/// into bin 32, and three-or-more hops folds into the `2+` class. Without this a 100-insn
/// chain would index out of the array.
#[test]
fn the_native_bin_index_saturates_at_both_edges() {
    assert_eq!(native_bin_index(0, 0, false), native_bin_index(1, 0, false));
    assert_eq!(
        native_bin_index(1_000_000, 0, false),
        native_bin_index(N_INSN_BINS as u64, 0, false)
    );
    assert_eq!(
        native_bin_index(4, 99, false),
        native_bin_index(4, 2, false)
    );
    assert!(native_bin_index(u64::MAX, u32::MAX, true) < N_BINS);
}

/// The calibration must measure a MARK, not the measurement. Both halves are asserted: an
/// empty bracket is a couple of `rdtsc` latencies, a bracketed real mark is strictly more, and
/// the difference is the resolution floor. The first version timed a `Vec::push` and returned
/// 42 ticks against an in-situ cost near 16, which drove ten of twelve phases negative.
#[test]
fn calibration_measures_a_mark_and_not_the_bracket() {
    let (bracket, mark, overhead) = calibrate();
    assert!(bracket > 0, "rdtsc did not advance across an empty bracket");
    assert!(
        mark > bracket,
        "a bracketed real mark ({mark}) must cost more than an empty bracket ({bracket})"
    );
    assert_eq!(overhead, mark - bracket);
    assert!(
        overhead < 10_000,
        "calibration difference {overhead} is not a mark cost"
    );
}
