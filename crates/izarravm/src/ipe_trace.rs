// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Windowed instructions-per-entry (IPE) trace: the host side of
//! `Machine::arm_ipe_window_trace`.
//!
//! MEASUREMENT ONLY. It exists because run-wide averages hide phases: a fixture whose whole-run
//! IPE is healthy can still spend tens of guest seconds in a load phase whose IPE is far lower,
//! and any admission policy keyed on IPE has to be judged against the WORST window, not the mean.
//!
//! Off unless `IZARRAVM_IPE_WINDOW_TRACE` names a file. When it is armed, the machine records one
//! window per `WINDOW_ENTRIES` direct-JIT entries (see `arm_ipe_window_trace` for why the boundary
//! is approximate) and this module renders the recorded windows AFTER the run returns. Nothing is
//! written from inside the run loop and no `run_until_*` call boundary moves, so an armed run
//! executes the same guest instruction stream as a disarmed one.
//!
//! FORMAT v2 adds three things v1's IPE column could not answer, one per consumer:
//!
//!   * `retired` and `coverage` -- total guest instructions retired in the window and the share
//!     of them the direct backend ran. A low-IPE window that is 30% covered and one that is 99%
//!     covered are different defects; v1 reported both as one number.
//!   * `distinct_targets` and `top_targets` -- how many different entry linears the dispatcher
//!     re-entered in the window, and the heaviest eight by name. This is the only part of the
//!     instrument that costs anything while armed; `izarravm_cpu::IpeEntryTargets` states the
//!     price, and an armed run is a MAP of a workload rather than a timing of one.
//!
//! An armed run is still guest-neutral: the tally observes and records, and changes no admission,
//! no schedule and no counter the guest can see.

use izarravm_machine::{IpeWindow, Machine};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Window size in direct-JIT entries: 2^22, the size the B2 admission-governor design uses for
/// its own windowed IPE arm, so the trace and the governor read the same quantity.
pub(crate) const WINDOW_ENTRIES: u64 = 1 << 22;

/// The requested trace path, or None when the instrument is disarmed.
pub(crate) fn requested_path() -> Option<PathBuf> {
    std::env::var_os("IZARRAVM_IPE_WINDOW_TRACE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Render one window's top entry targets as a single CSV field: `"0xLINEAR:count;0xLINEAR:count"`,
/// heaviest first, empty (`""`) when the tally recorded nothing.
///
/// QUOTED, and the quotes are belt-and-braces rather than load-bearing: the field's alphabet is
/// `[0-9a-fx:;]`, so it can contain neither a comma nor a quote and needs no escaping. It is
/// quoted anyway so that a reader using a real CSV parser and one using `split(',')` -- the sweep
/// script does the latter -- agree on the column count, and so that a future widening of the
/// separator cannot silently shift every column to its right.
fn render_top_targets(top: &[(u32, u64)]) -> String {
    let mut field = String::from("\"");
    for (position, (linear, count)) in top.iter().enumerate() {
        if position > 0 {
            field.push(';');
        }
        let _ = write!(field, "{linear:#010x}:{count}");
    }
    field.push('"');
    field
}

/// Render the trace as CSV with `#` comment lines: a header naming the armed window size, one row
/// per window, and an end-of-run totals line. The totals are summed from the rows rather than read
/// from the counters, so a reader can check the file against itself.
pub(crate) fn render(
    window_entries: u64,
    windows: &[IpeWindow],
    tail: Option<IpeWindow>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# izarravm-ipe-window-trace-v2 window_entries={window_entries}"
    );
    out.push_str(
        "window_index,entries,direct_insns,ipe,retired,coverage,distinct_targets,top_targets,kind\n",
    );
    let mut total_entries = 0u64;
    let mut total_insns = 0u64;
    let mut total_retired = 0u64;
    for window in windows.iter().chain(tail.iter()) {
        total_entries = total_entries.saturating_add(window.entries);
        total_insns = total_insns.saturating_add(window.direct_insns);
        total_retired = total_retired.saturating_add(window.retired);
        let kind = if window.partial { "partial" } else { "full" };
        let _ = writeln!(
            out,
            "{},{},{},{:.6},{},{:.6},{},{},{kind}",
            window.index,
            window.entries,
            window.direct_insns,
            window.ipe(),
            window.retired,
            window.coverage(),
            window.distinct_targets,
            render_top_targets(&window.top_targets)
        );
    }
    let total_ipe = if total_entries == 0 {
        0.0
    } else {
        total_insns as f64 / total_entries as f64
    };
    let total_coverage = if total_retired == 0 {
        0.0
    } else {
        total_insns as f64 / total_retired as f64
    };
    // `retired` carries on the totals line so the file's central v2 invariant is checkable from
    // the ARTIFACT alone: this figure must equal the run's `perf.instructions` less whatever had
    // already retired when the trace was armed. A reader who cannot reproduce the run can still
    // tell a trace that dropped a window from one that did not.
    let _ = writeln!(
        out,
        "# totals windows={} entries={total_entries} direct_insns={total_insns} ipe={total_ipe:.6} retired={total_retired} coverage={total_coverage:.6}",
        windows.len() + usize::from(tail.is_some())
    );
    out
}

/// Write the machine's recorded windows to `path`. Call AFTER the run returns: the trailing
/// partial window is computed from the live counters at this moment.
pub(crate) fn write_trace(path: &Path, machine: &Machine) -> std::io::Result<()> {
    let text = render(
        machine.ipe_window_size(),
        machine.ipe_windows(),
        machine.ipe_window_tail(),
    );
    std::fs::write(path, text)
}

#[cfg(test)]
#[path = "ipe_trace_test.rs"]
mod tests;
