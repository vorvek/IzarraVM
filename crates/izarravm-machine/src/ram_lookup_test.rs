// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const MEMORY_LEN: usize = 2 * 1024 * 1024;

#[test]
fn consistency_checks_length_page_count_and_every_page_base() {
    let vega = Vega::default();
    let mut lookup = RamPageLookup::new(MEMORY_LEN, &vega);
    assert!(lookup.is_consistent(MEMORY_LEN, &vega));

    lookup.memory_len -= 1;
    assert!(!lookup.is_consistent(MEMORY_LEN, &vega));
    lookup.memory_len = MEMORY_LEN;

    let expected_pages = MEMORY_LEN.div_ceil(RAM_LOOKUP_PAGE_SIZE);
    lookup.page_bases = vec![RAM_LOOKUP_SLOW; expected_pages - 1].into_boxed_slice();
    assert!(!lookup.is_consistent(MEMORY_LEN, &vega));

    lookup = RamPageLookup::new(MEMORY_LEN, &vega);
    lookup.page_bases[0] = RAM_LOOKUP_SLOW;
    assert!(!lookup.is_consistent(MEMORY_LEN, &vega));

    lookup = RamPageLookup::new(MEMORY_LEN, &vega);
    let extended_page = 0x0010_0000 / RAM_LOOKUP_PAGE_SIZE;
    lookup.page_bases[extended_page] = RAM_LOOKUP_SLOW;
    assert!(!lookup.is_consistent(MEMORY_LEN, &vega));
}
