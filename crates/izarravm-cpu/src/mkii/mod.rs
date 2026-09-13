// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

mod native;
mod ops;
mod runtime;

use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub(crate) struct State {
    engine: runtime::Engine,
    enabled: Option<bool>,
    census: Option<Box<[u64; 2048]>>,
    dirty_writes: Vec<(u32, u32)>,
    full_flush: bool,
    sources: BTreeMap<u32, u64>,
    pub(super) code_dirty: bool,
    pub(super) mapping_dirty: bool,
}

impl Clone for State {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for State {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for State {}

impl State {
    fn add_source(&mut self, physical: u32, len: u32) {
        for (mut start, mut end) in physical_ranges(physical, len).into_iter().flatten() {
            if let Some((&previous, &previous_end)) = self.sources.range(..=start).next_back()
                && previous_end >= u64::from(start)
            {
                start = previous;
                end = end.max(previous_end);
            }
            let mut remove = Vec::new();
            for (&next, &next_end) in self.sources.range(start..) {
                if u64::from(next) > end {
                    break;
                }
                end = end.max(next_end);
                remove.push(next);
            }
            for key in remove {
                self.sources.remove(&key);
            }
            self.sources.insert(start, end);
        }
    }

    pub(super) fn note_write(&mut self, physical: u32, len: u32) -> bool {
        let hit = physical_ranges(physical, len)
            .into_iter()
            .flatten()
            .any(|(start, end)| {
                self.sources
                    .range(..=(end - 1) as u32)
                    .next_back()
                    .is_some_and(|(_, &source_end)| source_end > u64::from(start))
            });
        self.code_dirty |= hit;
        if hit && !self.full_flush {
            if self.dirty_writes.len() == 32 {
                self.invalidate_code();
            } else {
                self.dirty_writes.push((physical, len));
            }
        }
        hit
    }

    pub(super) fn invalidate_code(&mut self) {
        self.code_dirty = true;
        self.mapping_dirty = true;
        self.full_flush = true;
        self.dirty_writes.clear();
    }
}

fn physical_ranges(physical: u32, len: u32) -> [Option<(u32, u64)>; 2] {
    if len == 0 {
        return [None, None];
    }
    const END: u64 = 1 << 32;
    let end = u64::from(physical) + u64::from(len);
    [
        Some((physical, end.min(END))),
        (end > END).then_some((0, end.saturating_sub(END))),
    ]
}

impl crate::CpuGsw {
    fn mkii_watch_source(&mut self, physical: u32, len: u32) {
        self.jit_direct.acquire_mkii_source(physical, len);
        self.jit_direct.mkii.add_source(physical, len);
        self.sweep_block_watch_edges();
    }
}

#[cfg(test)]
#[path = "watch_test.rs"]
mod watch_tests;
