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
//! (`run.rs:3100-3103`) and a cfg'd field there would move baked offsets in the observer build
//! only.

#[cfg(feature = "direct-entry-attribution")]
mod armed;
#[cfg(feature = "direct-entry-attribution")]
pub use armed::*;
