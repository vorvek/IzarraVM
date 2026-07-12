// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn physical_cache_covers_all_registers_without_overlap() {
    for physical in 0..8 {
        assert_eq!(physical_cache(physical), Xmm(4 + physical));
    }
}

#[test]
fn logical_indices_wrap_from_every_top() {
    for top in 0..8 {
        for logical in 0..8 {
            assert_eq!(physical(top, logical), top.wrapping_add(logical) & 7);
        }
    }
}
