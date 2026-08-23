// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Observer-build attribution of the Direct backend's per-entry cost.
//!
//! Implements `dev_docs/specs/2026-08-23-sixteen-bit-entry-attribution-design.md` (rev 3, plus the
//! third review pass's M-R4 / M-R5 / M-R6 corrections). The whole instrument is behind the
//! `direct-entry-attribution` feature, which is excluded from `default` and from any
//! `--all-features` path used for perf grading: without the feature the `ea_*!` macros below
//! expand to NOTHING, so the plain build is byte-identical in behaviour and carries no symbol.
//!
//! Shape: `begin()` at the dispatcher seam anchors a cursor and takes the sample decision;
//! `mark(phase)` closes one phase against that cursor with a raw `rdtsc` delta and no per-mark
//! subtraction; `end(population)` records the whole traversal against one of four populations.
//! State lives in a `thread_local!` `UnsafeCell` with a `const` initialiser and no `Drop`, so a
//! mark is a segment-relative load and a few adds — deliberately NOT a `CpuGsw` or `JitState`
//! field, because `DirectStallTally` sits ahead of `pending_flags` at an offset emitted code bakes
//! (`run.rs:2898-2901`) and a cfg'd field there would move baked offsets in the observer build
//! only.

/// Close one phase against the cursor. Expands to nothing without the feature.
///
/// FULL arm only: `IZARRAVM_DIRECT_ENTRY_ATTRIBUTION=2` (COARSE) skips these entirely, leaving the
/// four marks `ea_begin!` / `ea_mark_coarse!` / `ea_end!` produce.
macro_rules! ea_mark {
    ($phase:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::entry_attribution::mark($phase);
        }
    };
}

/// Close one phase against the cursor in BOTH armed modes. Only the two native-window boundaries
/// (`run.rs:2597` in, `run.rs:2605` out) use this: they are what makes COARSE's four-mark total
/// comparable with FULL's (A6).
macro_rules! ea_mark_coarse {
    ($phase:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::entry_attribution::mark_coarse($phase);
        }
    };
}

/// Anchor the cursor for one dispatcher traversal and take the sample decision.
macro_rules! ea_begin {
    ($d:expr, $v86:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::entry_attribution::begin($d, $v86);
        }
    };
}

/// Record the traversal's whole span against one population.
macro_rules! ea_end {
    ($population:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::entry_attribution::end($population);
        }
    };
}

/// Bump the `refusal_site` histogram. Every early return in the measured path carries one, which
/// is what makes the four refusals production counts nowhere (`run.rs:2294`, `run.rs:2566`,
/// `jit_direct_deferred_short`, `note_reject_callout_privileged`) countable without touching a
/// production counter.
macro_rules! ea_refusal {
    ($site:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::entry_attribution::note_refusal($site);
        }
    };
}

/// Bump the `compile_site` histogram (the seven exits of the `BlockProbe::Compile` arm).
macro_rules! ea_compile_site {
    ($site:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::entry_attribution::note_compile_site($site);
        }
    };
}

/// Write the interpreted-fallback site tag (declined vs skipped). Not a mark: H3-R.
macro_rules! ea_fallback_tag {
    ($tag:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::entry_attribution::set_fallback_tag($tag);
        }
    };
}

/// Record one native window into the §6 regression bins.
macro_rules! ea_native_sample {
    ($insns:expr, $hops:expr, $self_loop:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::entry_attribution::note_native($insns, $hops, $self_loop);
        }
    };
}

/// The `run.rs:1633` fall-through, which two arms reach.
///
/// `BlockProbe::Ready` arrives here having taken no mark since `mark(P1)` at `run.rs:1426`, so it
/// owes `mark(P2)`. `BlockProbe::Compile` took `mark(P2)` at `run.rs:1487` and owes the seventh of
/// the arm's exits, `mark(P14)` with `compile_site = installed_fall_through` (B3-R). One macro so
/// the two cannot drift apart.
macro_rules! ea_mark_probe_tail {
    ($from_compile:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            if $from_compile {
                // COARSE-inclusive: see the `mark(P2)` at the arm head.
                $crate::jit::direct::entry_attribution::mark_coarse(
                    $crate::jit::direct::entry_attribution::Phase::Compile,
                );
                $crate::jit::direct::entry_attribution::note_compile_site(
                    $crate::jit::direct::entry_attribution::compile_site::INSTALLED_FALL_THROUGH,
                );
            } else {
                $crate::jit::direct::entry_attribution::mark(
                    $crate::jit::direct::entry_attribution::Phase::Probe,
                );
            }
        }
    };
}

/// H9's pin: the block's own `mode_key` bit 0 must equal the lane bit `begin()` latched.
macro_rules! ea_pin_lane_bit0 {
    ($bit:expr) => {
        #[cfg(feature = "direct-entry-attribution")]
        {
            $crate::jit::direct::entry_attribution::pin_lane_bit0($bit);
        }
    };
}

pub(crate) use {
    ea_begin, ea_compile_site, ea_end, ea_fallback_tag, ea_mark, ea_mark_coarse,
    ea_mark_probe_tail, ea_native_sample, ea_pin_lane_bit0, ea_refusal,
};

#[cfg(feature = "direct-entry-attribution")]
mod armed;
#[cfg(feature = "direct-entry-attribution")]
pub use armed::*;
