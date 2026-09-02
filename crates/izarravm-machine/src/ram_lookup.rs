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

    /// `direct_bytes` for a caller that has ALREADY proved the range is non-empty and lies inside
    /// one lookup page.
    ///
    /// `direct_page_ram_bytes` and its unaligned sibling both prove exactly that before they get
    /// here, and the general form then re-proved it: a second `bytes == 0` test, a `checked_add`
    /// for an overflow that page-locality rules out, a `last_page` derivation and a multi-page
    /// loop that this caller can never enter. What is left is the one thing that has to be asked,
    /// which is whether the page is direct at all.
    #[inline]
    pub(crate) fn direct_bytes_page_local(
        &self,
        address: u32,
        bytes: usize,
    ) -> Option<(usize, usize)> {
        debug_assert!(bytes != 0, "a page-local range has at least one byte");
        debug_assert!(
            (address as usize & RAM_LOOKUP_PAGE_MASK) + bytes <= RAM_LOOKUP_PAGE_SIZE,
            "a page-local range does not leave its lookup page"
        );
        let start = address as usize;
        // `start` is a 32-bit address widened to `usize` and `bytes` is at most one page, so this
        // cannot overflow on any host this builds for.
        let end = start + bytes;
        if end > self.memory_len {
            return None;
        }
        let base = self
            .page_bases
            .get(start >> RAM_LOOKUP_PAGE_BITS)
            .copied()?;
        if base == RAM_LOOKUP_SLOW {
            return None;
        }
        let mapped_start = base + (start & RAM_LOOKUP_PAGE_MASK);
        Some((mapped_start, mapped_start + bytes))
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
#[path = "ram_lookup_test.rs"]
mod tests;
