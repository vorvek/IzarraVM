// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// C1e: the restamp re-read must reproduce the DECODER's extension semantics
/// byte-for-byte (design section 2.1's correctness cliff). The Word/SignWord arms are
/// currently unreachable through admission (no word-immediate or 16-bit-addressing
/// form classifies) but are pinned here so a future admission widening cannot
/// silently misextend.
#[test]
fn extend_bytes_reproduces_the_decoder_rules() {
    assert_eq!(
        ClifUnitCache::extend_bytes(&[0xf0], ImmExtend::ZeroByte),
        0x0000_00f0
    );
    // sign_extend_u8 extends to the FULL 32 bits regardless of operand size.
    assert_eq!(
        ClifUnitCache::extend_bytes(&[0xf0], ImmExtend::SignByte),
        0xffff_fff0
    );
    assert_eq!(
        ClifUnitCache::extend_bytes(&[0x34, 0xf0], ImmExtend::Word),
        0x0000_f034
    );
    assert_eq!(
        ClifUnitCache::extend_bytes(&[0x34, 0xf0], ImmExtend::SignWord),
        0xffff_f034
    );
    assert_eq!(
        ClifUnitCache::extend_bytes(&[0x78, 0x56, 0x34, 0xf2], ImmExtend::Dword),
        0xf234_5678
    );
    assert_eq!(ClifUnitCache::extend_bytes(&[], ImmExtend::None), 0);
}
