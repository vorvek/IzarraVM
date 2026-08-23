// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Unit tests for the data-segment reject governor's census fold.
//!
//! A child module of `retire_governor` rather than a row in `direct_test.rs`, because
//! `layout_fingerprint` is private to that file and the property under test is a property of
//! exactly one line inside it. The file-policy check bars a `#[test]` body in a non-`_test.rs`
//! source file, so the test lives here and is attached with `#[path]`.

use super::{census_mask, layout_fingerprint};
use crate::SegmentRegister;
use crate::jit::direct::BAKES_CS_BIT;

fn live_records() -> [SegmentRegister; 6] {
    [
        SegmentRegister::real(0x1000),
        SegmentRegister::real(0x2000),
        SegmentRegister::real(0x3000),
        SegmentRegister::real(0x4000),
        SegmentRegister::real(0x5000),
        SegmentRegister::real(0x6000),
    ]
}

/// F20. Two blocks that differ ONLY in whether they bake CS's selector -- i.e. whose `used`
/// bytes differ only in `BAKES_CS_BIT` -- must produce the same layout fingerprint, so the OFF
/// arm's `distinct_layouts` and `data_segment_layout_histogram` are untouched by the bit.
///
/// This asserts the MASKED value the reject site hands in, through `census_mask` -- the one line
/// the reject site calls, so the fixture tests the production expression rather than a copy of
/// it. That is the exact edit mutant M24 deletes. Asserted here rather than through
/// `data_segment_retire_record_for_test`, because `distinct_layouts` is per BlockKey and two
/// blocks differing by a `push cs` have different linears -- two keys, two records, each reading
/// 1 under both arms. The record accessor cannot see the fingerprint at all (`layouts` is
/// private), so a fixture written that way could not fail.
#[test]
fn the_layout_fingerprint_ignores_the_cs_bake_bit() {
    let live = live_records();
    // ES, SS and DS pinned; the second block additionally bakes CS's selector.
    let plain = 0b0000_1101u8;
    let bakes_cs = plain | BAKES_CS_BIT;
    assert_ne!(plain, bakes_cs, "the two `used` bytes must actually differ");
    assert_eq!(
        layout_fingerprint(census_mask(plain), &live),
        layout_fingerprint(census_mask(bakes_cs), &live),
        "BAKES_CS_BIT must not reach the census fold; drop the `& SEGMENT_MASK_BITS` at the \
         reject site and the OFF arm's data-segment layout census shifts on every `push cs` block"
    );
    // And the fold really is mask-sensitive, so the assertion above is not vacuous: a bit that
    // NAMES a segment changes the fingerprint.
    assert_ne!(
        layout_fingerprint(census_mask(plain), &live),
        layout_fingerprint(census_mask(plain | 0b0001_0000), &live),
        "the fold must still discriminate two different segment masks"
    );
}
