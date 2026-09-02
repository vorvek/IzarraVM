// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Merge-review nit 7: the D5 (`dev_docs/2026-09-04-reflected-call-slice0b-
//! review.md` §2) real/IF-edge tag, `run.rs`'s `BatchBoundaryRealTag`, tested
//! on its own -- it defaults to "real" every iteration and is marked "not
//! real" ONLY by the trip's own IF-enable edge (`can_take_before` false, then
//! true). A wrong default, or a mark that leaks across iterations, would
//! silently restore the 100%-tautological `batch_straddle_trips` 0c exists to
//! fix, and CI only `cargo check`s this crate under the feature -- it does
//! not run these tests there today (see `ci.yml`'s quartet), so this is the
//! ONE place that mutation would be caught locally before a human reads the
//! diff.

use super::BatchBoundaryRealTag;

/// The default-true half: a fresh tag, and a tag that was just taken without
/// an intervening `mark_if_edge`, both report "real" -- this is what makes
/// ordinary cap-ended batches count.
///
/// **Mutation bite**: change `BatchBoundaryRealTag::new` to `Self(false)` and
/// this test's first assertion goes red.
#[test]
fn defaults_to_real_every_iteration_with_no_if_edge_mark() {
    let mut tag = BatchBoundaryRealTag::new();
    assert!(
        tag.take_and_reset(),
        "the very first batch has no prior batch to blame"
    );
    // No `mark_if_edge` call between these two reads: both must be real.
    assert!(
        tag.take_and_reset(),
        "an ordinary (non-IF-edge) batch end must leave the NEXT read real"
    );
    assert!(
        tag.take_and_reset(),
        "the default must keep holding across further untouched iterations"
    );
}

/// The mark-false half: `mark_if_edge` flips exactly the NEXT read, and only
/// that one -- the reset inside `take_and_reset` restores the default
/// immediately after, so a mark does not leak into the iteration after the
/// one it was meant for.
///
/// **Mutation bite**: delete the `self.0 = true;` line inside
/// `take_and_reset` and this test's third assertion goes red (the mark would
/// leak forever instead of being consumed once).
#[test]
fn mark_if_edge_flips_only_the_next_read_and_does_not_leak() {
    let mut tag = BatchBoundaryRealTag::new();
    tag.mark_if_edge();
    assert!(
        !tag.take_and_reset(),
        "the read immediately after an IF-edge mark must report false"
    );
    assert!(
        tag.take_and_reset(),
        "the mark must not survive past the one read it was for"
    );
    tag.mark_if_edge();
    tag.mark_if_edge(); // a second, redundant mark inside the same batch
    assert!(
        !tag.take_and_reset(),
        "repeated marks inside one batch still report false exactly once, then reset"
    );
    assert!(tag.take_and_reset());
}
