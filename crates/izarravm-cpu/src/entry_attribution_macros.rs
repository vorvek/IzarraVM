// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The entry-attribution observer's stamp macros, and nothing else.
//!
//! They live at the CRATE ROOT rather than beside the instrument (`jit::direct`) for one reason:
//! `jit::direct` is compiled only under `feature = "jit"`, while the call sites in `run.rs` are
//! not all inside a `jit` gate -- the `ea_begin!` that opens a traversal and the P13 pair
//! (`ea_mark!(Phase::InterpretFallback)` + `ea_end!(Population::Fallback)`) sit in
//! `run_budgeted_inner`'s interpreted arm, which every build compiles. With the macros defined
//! down there, `--no-default-features` could not resolve them and the crate did not build.
//!
//! Defined here they are always in scope and always expand to NOTHING unless
//! `direct-entry-attribution` is selected -- which implies `jit`, so the armed bodies can name
//! `jit::direct` freely.
//!
//! `unused_macros` is allowed for the module: most of the call sites are inside `run.rs`'s own
//! `jit` gates, so a `--no-default-features` build defines all ten and invokes three.

#![allow(unused_macros)]

/// Close one phase against the cursor. Expands to nothing without the feature.
///
/// FULL arm only: `IZARRAVM_DIRECT_ENTRY_ATTRIBUTION=2` (COARSE) skips these entirely, leaving the
/// four marks `ea_begin!` / `ea_mark_coarse!` / `ea_end!` produce.
macro_rules! ea_mark {
    ($phase:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::mark($phase);
        }
    };
}

/// Close one phase against the cursor in BOTH armed modes.
///
/// The two native-window boundaries in `run_direct_block` use this -- `Phase::NativePreamble`
/// immediately before the `entry(..)` call and `Phase::NativeBody` immediately after it. They are
/// what makes COARSE's four-mark total comparable with FULL's (A6): on an entered traversal COARSE
/// stamps exactly `ea_begin!`, those two, and `ea_end!`.
///
/// The `BlockProbe::Compile` arm in `try_direct_continuation` is COARSE-inclusive too (B3, see the
/// comment at its `ea_mark_coarse!(Phase::Probe)`), because P2 is what BOUNDS P14 and P14 has to be
/// subtractable from `total_entered` in both arms. That arm is ~2.5% of traversals on the loader,
/// so the four-mark COARSE shape holds for the other ~97.5%.
macro_rules! ea_mark_coarse {
    ($phase:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::mark_coarse($phase);
        }
    };
}

/// Anchor the cursor for one dispatcher traversal and take the sample decision.
macro_rules! ea_begin {
    ($d:expr, $v86:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::begin($d, $v86);
        }
    };
}

/// Record the traversal's whole span against one population.
macro_rules! ea_end {
    ($population:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::end($population);
        }
    };
}

/// Bump the `refusal_site` histogram. Every early return in the measured path carries one, which
/// is what makes refusals no `perf` key counts countable without adding a production counter.
///
/// In `run_direct_block` exactly three refusals have no `self.perf` bump at all:
/// `site::NATIVE_FETCH_TRACE`, `site::DATA_SEGMENT` and `site::BLOCK_REGENERATED_NONE`. Two more are counted but not DISTINGUISHED:
/// `jit_direct_deferred_short` is bumped from two different sites, which the histogram separates as
/// `site::DISPATCH_DEFERRED_SHORT` vs `site::ENTRY_DEFERRED_SHORT`, and
/// `note_reject_callout_privileged` lands in `DirectStallTally`, not in `perf`, so it is absent
/// from the perf JSON the boards are graded on.
///
/// (The two `run.rs` LINE citations this note used to carry were wrong when they were written --
/// at the commit that added them one pointed into a doc comment and the other at the
/// `jit_direct_deferred_short` bump itself. Named sites cannot drift; do not reintroduce line
/// numbers here.)
macro_rules! ea_refusal {
    ($site:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::note_refusal($site);
        }
    };
}

/// Bump the `compile_site` histogram (the seven exits of the `BlockProbe::Compile` arm).
macro_rules! ea_compile_site {
    ($site:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::note_compile_site($site);
        }
    };
}

/// Write the interpreted-fallback site tag (declined vs skipped). Not a mark: H3-R.
macro_rules! ea_fallback_tag {
    ($tag:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::set_fallback_tag($tag);
        }
    };
}

/// Record one native window into the §6 regression bins.
macro_rules! ea_native_sample {
    ($insns:expr, $hops:expr, $self_loop:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::note_native($insns, $hops, $self_loop);
        }
    };
}

/// The fall-through at the end of `try_direct_continuation`'s `BlockProbe` match, which two arms
/// reach.
///
/// `BlockProbe::Ready` arrives here having taken no mark since `mark(P1)` (`Phase::Key`, stamped
/// just before the probe), so it owes `mark(P2)` (`Phase::Probe`). `BlockProbe::Compile` already
/// took `mark(P2)` at its arm head and owes the seventh of that arm's exits, `mark(P14)`
/// (`Phase::Compile`) with `compile_site = INSTALLED_FALL_THROUGH` (B3-R). One macro so the two
/// cannot drift apart.
macro_rules! ea_mark_probe_tail {
    ($from_compile:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            if $from_compile {
                // COARSE-inclusive: see the `mark(P2)` at the arm head.
                $crate::jit::direct::mark_coarse($crate::jit::direct::Phase::Compile);
                $crate::jit::direct::note_compile_site(
                    $crate::jit::direct::compile_site::INSTALLED_FALL_THROUGH,
                );
            } else {
                $crate::jit::direct::mark($crate::jit::direct::Phase::Probe);
            }
        }
    };
}

/// H9's pin: the block's own `mode_key` bit 0 must equal the lane bit `begin()` latched.
macro_rules! ea_pin_lane_bit0 {
    ($bit:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::pin_lane_bit0($bit);
        }
    };
}
