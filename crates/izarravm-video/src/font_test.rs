// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// The ROM glyph tables are byte-for-byte from the LGPL VGABios `vgafonts.h`.
/// Anchor them on glyphs whose rows are unambiguous: 0xDB is the solid full
/// block (all-ones rows), and 0xC0 is a box-drawing corner whose row pattern
/// is distinctive. A regression here means the generator or the table moved.
#[test]
fn cp437_rom_glyphs_match_the_reference() {
    // 0xDB (full block): every row is 0xFF in all three ROM sizes.
    let db8 = &VGAFONT_8X8[0xDB * 8..0xDB * 8 + 8];
    let db14 = &VGAFONT_8X14[0xDB * 14..0xDB * 14 + 14];
    let db16 = &VGAFONT_8X16[0xDB * 16..0xDB * 16 + 16];
    assert!(db8.iter().all(|&b| b == 0xFF), "8x8 0xDB is a full block");
    assert!(db14.iter().all(|&b| b == 0xFF), "8x14 0xDB is a full block");
    assert!(db16.iter().all(|&b| b == 0xFF), "8x16 0xDB is a full block");

    // 0xC0 (box-drawing bottom-left corner): the distinctive row pattern.
    let c0_16 = &VGAFONT_8X16[0xC0 * 16..0xC0 * 16 + 16];
    assert_eq!(
        c0_16,
        &[
            0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x1F, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ],
        "8x16 0xC0 corner glyph matches the source byte-for-byte"
    );

    // 'A' (0x41): a normal glyph with the top bar set (row 7 = 0xFE).
    let a = &VGAFONT_8X16[0x41 * 16..0x41 * 16 + 16];
    assert_eq!(a[7], 0xFE, "8x16 'A' has its crossbar at font row 7");
}
