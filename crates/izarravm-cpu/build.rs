// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Generates the entry-attribution line tables (`REFUSAL_SITES`, `COMPILE_SITES`,
//! `P0_MARK_LINE`) that `jit/direct.rs` `include!`s under `direct-entry-attribution`.
//!
//! Why: those three tables used to be hand-typed literals citing `run.rs` line numbers. Four
//! times in one week (twice before, then #835 and #837) an unrelated edit above one of the
//! cited lines shifted everything below it without anyone touching the tables, and the
//! `armed_tests::the_refusal_and_compile_tables_name_the_lines_they_claim` /
//! `the_p0_mark_line_is_where_the_p0_mark_is` fixtures (which read `run.rs` back at test time
//! and check the cited line really is what the table says) went red on a change that had
//! nothing to do with entry attribution. See commit 148d8d71 for the fourth fix and the note
//! that a generator was the right fix but out of scope that day.
//!
//! This IS that generator. It reads `run.rs` and `jit/direct.rs` at build time, finds every
//! `ea_refusal!`/`ea_compile_site!` call site and the `mark(P0)` call, and writes their real,
//! current line numbers to an `OUT_DIR` file that `jit/direct.rs` splices in with `include!`.
//! There is no longer a literal for an edit to shift out from under -- the table is recomputed
//! on every build that touches either file.
//!
//! Runs (and re-parses both files) on every build, feature or no feature: the cost is a couple
//! of linear text scans over two source files, and running it unconditionally means the plain
//! build's `OUT_DIR` never lacks the generated file. Nothing this script writes is READ unless
//! `direct-entry-attribution` is selected -- `jit/direct.rs`'s `include!` of the generated file
//! sits behind that `#[cfg]` -- so a plain build's compiled output is exactly as if this script
//! did not exist.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::PathBuf;

include!("build/site_line_scan.rs");

/// One line of `run.rs`, split at its first `needle`, or `None` if `needle` never occurs.
fn find_line(text: &str, needle: &str) -> Option<u32> {
    text.lines()
        .enumerate()
        .find(|(_, line)| line.contains(needle))
        .map(|(index, _)| index as u32 + 1)
}

fn count_occurrences(text: &str, needle: &str) -> usize {
    text.lines().filter(|line| line.contains(needle)).count()
}

/// Parse `pub(crate) const NAME: usize = N;` lines out of the named `pub(crate) mod MODULE { ...
/// }` block in `direct_rs`, returning `{lowercased NAME: N}`. This is the authority for which
/// array INDEX a site's name owns -- `run.rs` addresses the perf histograms by `site::NAME` /
/// `compile_site::NAME`, so the generated table has to place each site at the same index the
/// `mod` block already assigns it, not at wherever the scan happens to encounter it.
fn parse_index_module(direct_rs: &str, module: &str) -> HashMap<String, usize> {
    let open = format!("pub(crate) mod {module} {{");
    let start = direct_rs
        .find(&open)
        .unwrap_or_else(|| panic!("jit/direct.rs has no `{open}` block"));
    let close = direct_rs[start..]
        .find('}')
        .unwrap_or_else(|| panic!("`{open}` block in jit/direct.rs is never closed"));
    let block = &direct_rs[start..start + close];
    let mut out = HashMap::new();
    for line in block.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("pub(crate) const ") else {
            continue;
        };
        let Some((name, rest)) = rest.split_once(':') else {
            continue;
        };
        let rest = rest.trim();
        let Some(rest) = rest.strip_prefix("usize = ") else {
            continue;
        };
        let Some(value) = rest.strip_suffix(';') else {
            continue;
        };
        let index: usize = value
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("`{module}::{name}` value {value:?} is not a usize: {e}"));
        out.insert(name.to_ascii_lowercase(), index);
    }
    out
}

/// Every `macro!(module::NAME)` call in `run_rs`, as `{lowercased NAME: first call's 1-indexed
/// line}`. A site with two call sites (`probe_rejected` has one) is recorded at its FIRST
/// occurrence, matching the fixture's own stated rule ("the first is cited").
fn find_calls(run_rs: &str, macro_name: &str, module: &str) -> HashMap<String, u32> {
    let prefix = format!("{macro_name}!({module}::");
    let mut out = HashMap::new();
    for (index, line) in run_rs.lines().enumerate() {
        let Some(at) = line.find(&prefix) else {
            continue;
        };
        let rest = &line[at + prefix.len()..];
        let Some(end) = rest.find(')') else {
            continue;
        };
        let name = rest[..end].to_ascii_lowercase();
        out.entry(name).or_insert(index as u32 + 1);
    }
    out
}

/// Build one `(label, line)` table, in index order, from a name->index authority (the `mod
/// site`/`mod compile_site` block) and a name->call-line map (the `run.rs` scan). Panics with a
/// specific, actionable message the moment the two disagree about which sites exist -- that is
/// the one drift class this generator cannot self-heal, because it means a site was added or
/// removed on only one side.
fn build_table(
    module: &str,
    indices: &HashMap<String, usize>,
    calls: &HashMap<String, u32>,
    site_line: impl Fn(&str, u32) -> u32,
) -> Vec<(String, u32)> {
    let indexed: HashSet<&str> = indices.keys().map(String::as_str).collect();
    let called: HashSet<&str> = calls.keys().map(String::as_str).collect();
    let mut missing: Vec<&str> = indexed.difference(&called).copied().collect();
    missing.sort_unstable();
    if let Some(&first) = missing.first() {
        panic!(
            "`{module}::{}` is declared in jit/direct.rs but run.rs has no call site for it \
             ({} more site(s) also missing: {missing:?})",
            first.to_ascii_uppercase(),
            missing.len() - 1
        );
    }
    let mut extra: Vec<&str> = called.difference(&indexed).copied().collect();
    extra.sort_unstable();
    if let Some(&first) = extra.first() {
        panic!(
            "run.rs calls `{module}::{}`, which jit/direct.rs's `mod {module}` does not declare \
             ({} more such call(s): {extra:?})",
            first.to_ascii_uppercase(),
            extra.len() - 1
        );
    }
    let mut table: Vec<Option<(String, u32)>> = vec![None; indices.len()];
    for (name, &index) in indices {
        let call_line = calls[name];
        let line = site_line(name, call_line);
        table[index] = Some((name.clone(), line));
    }
    table
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            entry.unwrap_or_else(|| panic!("`{module}` index {index} was never filled"))
        })
        .collect()
}

fn render_table(const_name: &str, count_name: &str, entries: &[(String, u32)]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "pub const {count_name}: usize = {};\n",
        entries.len()
    ));
    out.push_str(&format!(
        "pub const {const_name}: [(&str, u32); {count_name}] = [\n"
    ));
    for (label, line) in entries {
        out.push_str(&format!("    ({label:?}, {line}),\n"));
    }
    out.push_str("];\n");
    out
}

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let run_rs_path = manifest_dir.join("src/run.rs");
    let direct_rs_path = manifest_dir.join("src/jit/direct.rs");
    println!("cargo::rerun-if-changed={}", run_rs_path.display());
    println!("cargo::rerun-if-changed={}", direct_rs_path.display());
    println!("cargo::rerun-if-changed=build/site_line_scan.rs");

    let run_rs = fs::read_to_string(&run_rs_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", run_rs_path.display()));
    let direct_rs = fs::read_to_string(&direct_rs_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", direct_rs_path.display()));

    // Every `ea_refusal!`/`ea_compile_site!` call is immediately followed (within a handful of
    // bookkeeping lines) by the `return` it belongs to; that `return`'s line is what the table
    // cites. `RETURN_WINDOW` is generous on purpose -- wide enough that a bookkeeping line added
    // between the macro and its `return` does not itself require widening this constant.
    const RETURN_WINDOW: usize = 12;

    let refusal_indices = parse_index_module(&direct_rs, "site");
    let refusal_calls = find_calls(&run_rs, "ea_refusal", "site");
    let refusal_table = build_table(
        "site",
        &refusal_indices,
        &refusal_calls,
        |name, call_line| {
            scan_forward(&run_rs, call_line as usize, RETURN_WINDOW, "return ").unwrap_or_else(
                || {
                    panic!(
                        "refusal site {name} (ea_refusal! at run.rs:{call_line}) has no `return` \
                         within {RETURN_WINDOW} lines below it"
                    )
                },
            )
        },
    );

    let compile_indices = parse_index_module(&direct_rs, "compile_site");
    // `installed_fall_through` is the one compile site with no `ea_compile_site!` call of its
    // own in run.rs -- it is bumped INSIDE the `ea_mark_probe_tail!` macro, so its cited line is
    // that macro's own call site in run.rs, not a scanned-for `return`.
    let fall_through_line = find_line(&run_rs, "ea_mark_probe_tail!(").unwrap_or_else(|| {
        panic!("run.rs has no `ea_mark_probe_tail!` call for `installed_fall_through`")
    });
    let mut compile_calls = find_calls(&run_rs, "ea_compile_site", "compile_site");
    compile_calls
        .entry("installed_fall_through".to_string())
        .or_insert(fall_through_line);
    let compile_table = build_table(
        "compile_site",
        &compile_indices,
        &compile_calls,
        |name, call_line| {
            if name == "installed_fall_through" {
                return call_line;
            }
            scan_forward(&run_rs, call_line as usize, RETURN_WINDOW, "return ").unwrap_or_else(
                || {
                    panic!(
                        "compile site {name} (ea_compile_site! at run.rs:{call_line}) has no \
                         `return` within {RETURN_WINDOW} lines below it"
                    )
                },
            )
        },
    );

    let dispatch_gates_calls = count_occurrences(&run_rs, "ea_mark!(Phase::DispatchGates)");
    assert_eq!(
        dispatch_gates_calls, 1,
        "run.rs must call `ea_mark!(Phase::DispatchGates)` exactly once (found {dispatch_gates_calls}); \
         P0_MARK_LINE cannot name a single line otherwise"
    );
    let p0_mark_line = find_line(&run_rs, "ea_mark!(Phase::DispatchGates)")
        .expect("just counted exactly one occurrence");

    let mut generated = String::new();
    generated.push_str(
        "// Generated by build.rs from the ea_refusal!/ea_compile_site!/ea_mark! call sites in \
         run.rs and jit/direct.rs's `mod site`/`mod compile_site`. Do not hand-edit.\n\n",
    );
    generated.push_str(&render_table(
        "REFUSAL_SITES",
        "N_REFUSAL_SITES",
        &refusal_table,
    ));
    generated.push('\n');
    generated.push_str(&format!("pub const P0_MARK_LINE: u32 = {p0_mark_line};\n"));
    generated.push('\n');
    generated.push_str(&render_table(
        "COMPILE_SITES",
        "N_COMPILE_SITES",
        &compile_table,
    ));

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out_dir.join("entry_attribution_lines.rs"), generated)
        .expect("writing generated entry-attribution line table");
}
