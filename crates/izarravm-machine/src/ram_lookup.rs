// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! RAM page lookup for direct memory access, including conventional and UMA
//! bypasses.

use crate::vega::Vega;
use crate::video_params::RAM_LOOKUP_PAGE_BITS;
use crate::video_params::RAM_LOOKUP_PAGE_MASK;
use crate::video_params::RAM_LOOKUP_PAGE_SIZE;
use crate::video_params::RAM_LOOKUP_SLOW;

#[derive(Debug)]
pub(crate) struct RamPageLookup {
    page_bases: Box<[usize]>,
    memory_len: usize,
}

impl RamPageLookup {
    pub(crate) fn new(memory_len: usize, vega: &Vega) -> Self {
        let page_count = memory_len.div_ceil(RAM_LOOKUP_PAGE_SIZE);
        let mut lookup = Self {
            page_bases: vec![RAM_LOOKUP_SLOW; page_count].into_boxed_slice(),
            memory_len,
        };
        lookup.rebuild(memory_len, vega);
        lookup
    }

    pub(crate) fn rebuild(&mut self, memory_len: usize, vega: &Vega) {
        self.memory_len = memory_len;
        let page_count = memory_len.div_ceil(RAM_LOOKUP_PAGE_SIZE);
        if self.page_bases.len() != page_count {
            self.page_bases = vec![RAM_LOOKUP_SLOW; page_count].into_boxed_slice();
        } else {
            self.page_bases.fill(RAM_LOOKUP_SLOW);
        }

        for (page, base) in self.page_bases.iter_mut().enumerate() {
            let start = page * RAM_LOOKUP_PAGE_SIZE;
            let end = (start + RAM_LOOKUP_PAGE_SIZE).min(memory_len);
            *base = ram_lookup_page_base(start, end, vega);
        }
    }

    /// Checks that this acceleration table is the exact derivation of the
    /// authoritative RAM length and live video-memory decode.
    ///
    /// Canonical capture excludes the table itself, but a stale direct entry
    /// could route a later CPU access to RAM instead of a device. Keep this
    /// allocation-free and share the same page derivation as `rebuild`.
    pub(crate) fn is_consistent(&self, memory_len: usize, vega: &Vega) -> bool {
        let page_count = memory_len.div_ceil(RAM_LOOKUP_PAGE_SIZE);
        self.memory_len == memory_len
            && self.page_bases.len() == page_count
            && self.page_bases.iter().enumerate().all(|(page, &base)| {
                let start = page * RAM_LOOKUP_PAGE_SIZE;
                let end = (start + RAM_LOOKUP_PAGE_SIZE).min(memory_len);
                base == ram_lookup_page_base(start, end, vega)
            })
    }

    #[inline]
    pub(crate) fn direct_bytes(&self, address: u32, bytes: usize) -> Option<(usize, usize)> {
        let start = address as usize;
        let end = start.checked_add(bytes)?;
        if bytes == 0 || end > self.memory_len {
            return None;
        }
        let first_page = start >> RAM_LOOKUP_PAGE_BITS;
        let last_page = (end - 1) >> RAM_LOOKUP_PAGE_BITS;
        let first_base = self.page_bases.get(first_page).copied()?;
        if first_base == RAM_LOOKUP_SLOW {
            return None;
        }
        if first_page == last_page {
            let mapped_start = first_base + (start & RAM_LOOKUP_PAGE_MASK);
            return Some((mapped_start, mapped_start + bytes));
        }
        for page in first_page..=last_page {
            if self.page_bases.get(page).copied()? == RAM_LOOKUP_SLOW {
                return None;
            }
        }
        let mapped_start = first_base + (start & RAM_LOOKUP_PAGE_MASK);
        Some((mapped_start, mapped_start + bytes))
    }
}

fn ram_lookup_page_base(start: usize, end: usize, vega: &Vega) -> usize {
    if ram_lookup_page_is_direct(start, end, vega) {
        start
    } else {
        RAM_LOOKUP_SLOW
    }
}

fn ram_lookup_page_is_direct(start: usize, end: usize, vega: &Vega) -> bool {
    if end <= 0x000A_0000 {
        return true;
    }
    if start < 0x0010_0000 {
        return false;
    }
    !vega.memory_bar_overlaps(start, end)
}

#[cfg(test)]
mod tests {
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
}
