// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

// The one primitive the entry-attribution line-table generator needs, `include!`d rather than
// depended on as a crate: `build.rs` is a SEPARATE compilation from `izarravm-cpu` (Cargo builds
// and runs it before the crate exists as a dependency graph node), so it cannot `use` anything
// out of `src/`. Textual `include!` gets the same function into both `build.rs` (which calls it
// against the real `run.rs`) and `jit/direct/entry_attribution/armed_test.rs` (which calls the
// SAME copy against a synthetic snippet whose true line the test also knows independently, via
// `line!()`) without either one depending on the other.
//
// Plain `//` throughout, not `//!`: this file is spliced into the MIDDLE of another file's item
// list by `include!`, where an inner doc comment is not legal syntax.

/// Scan forward from `start_line` (1-indexed, inclusive) across the next `window` lines of
/// `text` for the first one containing `needle`, and return its 1-indexed line number.
///
/// This is the whole derivation: every `ea_refusal!`/`ea_compile_site!` call site is immediately
/// followed by its `return` a fixed, small number of lines down (the intervening lines are the
/// bookkeeping macros -- `ea_end!`, `ea_mark_coarse!` -- that always sit between the two). Rather
/// than hard-code that distance, this looks for the `return` itself, so it survives a line being
/// added or removed between the macro call and the return it belongs to.
fn scan_forward(text: &str, start_line: usize, window: usize, needle: &str) -> Option<u32> {
    text.lines()
        .enumerate()
        .skip(start_line - 1)
        .take(window)
        .find(|(_, line)| line.contains(needle))
        .map(|(index, _)| index as u32 + 1)
}
