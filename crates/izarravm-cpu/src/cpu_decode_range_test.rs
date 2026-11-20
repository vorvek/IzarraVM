// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn scalar_write(cache: &mut DecodeCache, physical: u32, width: u32) -> Option<u32> {
    let mut killed = 0;
    for byte in (0..width).map(|i| physical.wrapping_add(i)) {
        if !cache.is_code_byte(byte) {
            continue;
        }
        let info = *cache.code_page_lin.get(&(byte >> 12))?;
        if info.aliased {
            return None;
        }
        let linear = (info.lin_page << 12) | (byte & 0xfff);
        for candidate in linear.saturating_sub(14)..=linear {
            let index = (candidate & cache.mask) as usize;
            let line = &cache.lines[index];
            // Live means the current generation or the generation of an occupied ring slot.
            let live = line.generation == cache.generation
                || cache
                    .contexts
                    .iter()
                    .flatten()
                    .any(|c| c.1 == line.generation);
            if live && line.tag == candidate {
                let len = line.insn.map_or(0, |insn| u32::from(insn.len));
                if line.phys_start <= byte && byte < line.phys_start.wrapping_add(len) {
                    cache.kill_line_at(index);
                    killed += 1;
                }
            }
        }
    }
    Some(killed)
}

fn instruction(len: u8) -> DecodedInsn {
    let (mut cpu, memory) = real_mode_cpu(&[0x90], 0x20);
    let mut insn = cpu.decode(&mut TestBus::with_memory(memory)).unwrap();
    insn.len = len;
    insn
}

fn filled(physical: u32, linear_page: u32, slots: usize) -> DecodeCache {
    let mut cache = DecodeCache::new(slots);
    let offset = physical & 0xfff;
    let mut insn = instruction(1);
    for start in offset.saturating_sub(16)..=offset.saturating_add(8).min(0xfff) {
        insn.len = (1 + start % 15).min(4096 - start) as u8;
        assert!(
            cache
                .put(
                    (linear_page << 12) | start,
                    insn,
                    true,
                    (physical & !0xfff) | start
                )
                .inserted
        );
    }
    cache
}

fn same_cache(left: &DecodeCache, right: &DecodeCache) {
    assert_eq!(format!("{:?}", left.lines), format!("{:?}", right.lines));
    assert_eq!(left.generation, right.generation);
    assert_eq!(left.next_generation, right.next_generation);
    assert_eq!(left.code_bytes, right.code_bytes);
    assert_eq!(left.code_pages, right.code_pages);
    assert_eq!(left.translation_pages, right.translation_pages);
    assert_eq!(
        left.translation_pages_marked,
        right.translation_pages_marked
    );
    assert_eq!(left.dirty_byte_words, right.dirty_byte_words);
    assert_eq!(left.dirty_page_words, right.dirty_page_words);
    assert_eq!(left.contexts, right.contexts);
    assert_eq!(left.ring_seeded, right.ring_seeded);
    let map = |cache: &DecodeCache| {
        let mut rows: Vec<_> = cache
            .code_page_lin
            .iter()
            .map(|(&page, info)| (page, info.lin_page, info.aliased))
            .collect();
        rows.sort_unstable();
        rows
    };
    assert_eq!(map(left), map(right));
    #[cfg(feature = "jit")]
    {
        assert_eq!(format!("{:?}", left.packs), format!("{:?}", right.packs));
        assert_eq!(left.poll_neg_gens, right.poll_neg_gens);
        assert_eq!(left.poll_neg, right.poll_neg);
        left.assert_packs_consistent();
        right.assert_packs_consistent();
    }
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    {
        left.assert_native_watch_consistent();
        right.assert_native_watch_consistent();
    }
}

#[test]
fn decode_range_matches_scalar_across_widths_mappings_and_boundaries() {
    for physical in [
        0,
        1,
        62,
        63,
        64,
        4093,
        4094,
        4095,
        SMC_BYTE_COVERAGE - 4,
        SMC_BYTE_COVERAGE,
        u32::MAX - 4,
        u32::MAX,
    ] {
        for linear_page in [physical >> 12, 0xfffff] {
            for width in [0, 1, 2, 3, 4, 8] {
                for slots in [8, 64] {
                    let mut scalar = filled(physical, linear_page, slots);
                    let mut range = filled(physical, linear_page, slots);
                    assert_eq!(
                        range.narrow_invalidate_write(physical, width),
                        scalar_write(&mut scalar, physical, width),
                        "physical={physical:x} linear={linear_page:x} width={width} slots={slots}"
                    );
                    same_cache(&range, &scalar);
                }
            }
        }
    }
}

#[test]
fn decode_range_preserves_marks_context_and_mapping_refusals() {
    for variant in 0..8 {
        for contexts in 0..=2 {
            let prepare = || {
                let mut cache = filled(0x1240, 0x31, 64);
                // The real ring never gives two slots one generation: slot 1 owns a fresh one.
                let other = cache.allocate_generation();
                let unowned = cache.allocate_generation();
                for index in 0..contexts {
                    let generation = if index == 0 { cache.generation } else { other };
                    cache.contexts[index] = Some((index as u32 * 0x1000, generation));
                }
                match variant {
                    0 => {}
                    1 => {
                        cache.code_page_lin.remove(&1);
                    }
                    2 => {
                        cache.code_page_lin.get_mut(&1).unwrap().aliased = true;
                    }
                    3 => {
                        cache.code_bytes[0x1241 >> 6] &= !(1 << (0x1241 & 63));
                    }
                    4 => {
                        cache.code_bytes[0x1240 >> 6] = 0;
                    }
                    // The line carries the other slot's generation: it dies when that slot is
                    // occupied and survives when it is not.
                    5 => {
                        let slot = 0x31240 & cache.mask;
                        cache.lines[slot as usize].generation = other;
                        #[cfg(feature = "jit")]
                        {
                            cache.packs[slot as usize].generation = other;
                        }
                    }
                    // The line carries a generation no slot holds: it always survives.
                    7 => {
                        let slot = 0x31240 & cache.mask;
                        cache.lines[slot as usize].generation = unowned;
                        #[cfg(feature = "jit")]
                        {
                            cache.packs[slot as usize].generation = unowned;
                        }
                    }
                    6 => {
                        let slot = 0x31240 & cache.mask;
                        cache.lines[slot as usize].tag ^= 0x1000;
                        #[cfg(feature = "jit")]
                        {
                            cache.packs[slot as usize].tag = cache.lines[slot as usize].tag;
                        }
                    }
                    _ => unreachable!(),
                }
                cache
            };
            let mut scalar = prepare();
            let mut range = prepare();
            assert_eq!(
                range.narrow_invalidate_write(0x1240, 4),
                scalar_write(&mut scalar, 0x1240, 4)
            );
            same_cache(&range, &scalar);
        }
    }
}

#[test]
fn decode_range_counts_a_long_overlap_once_and_retires_tail_starts() {
    let prepare = || {
        let mut cache = DecodeCache::new(64);
        assert!(cache.put(0x1230, instruction(15), true, 0x1230).inserted);
        for start in 0x1239..=0x123c {
            assert!(cache.put(start, instruction(2), true, start).inserted);
        }
        cache
    };
    let mut scalar = prepare();
    let mut range = prepare();
    assert_eq!(range.narrow_invalidate_write(0x1238, 4), Some(4));
    assert_eq!(scalar_write(&mut scalar, 0x1238, 4), Some(4));
    same_cache(&range, &scalar);
}

#[test]
fn decode_range_fallback_keeps_partial_kills_before_unknown_page() {
    let prepare = || {
        let mut cache = DecodeCache::new(64);
        assert!(cache.put(0xfff, instruction(1), true, 0xfff).inserted);
        cache.mark_code_range(0x1000, 1);
        cache
    };
    let mut scalar = prepare();
    let mut range = prepare();
    assert_eq!(range.narrow_invalidate_write(0xfff, 2), None);
    assert_eq!(scalar_write(&mut scalar, 0xfff, 2), None);
    assert_eq!(range.lines[(0xfff & range.mask) as usize].generation, 0);
    same_cache(&range, &scalar);
}

#[test]
fn decode_range_preserves_current_generation_empty_entries() {
    for empty in [true, false] {
        let prepare = || {
            let mut cache = filled(0x1240, 1, 64);
            let index = (0x1241 & cache.mask) as usize;
            cache.lines[index].insn = if empty { None } else { Some(instruction(0)) };
            #[cfg(feature = "jit")]
            {
                cache.packs[index].len = 0;
            }
            cache
        };
        let mut scalar = prepare();
        let mut range = prepare();
        assert_eq!(
            range.narrow_invalidate_write(0x1240, 4),
            scalar_write(&mut scalar, 0x1240, 4)
        );
        let index = (0x1241 & range.mask) as usize;
        assert_eq!(range.lines[index].generation, range.generation);
        assert_eq!(format!("{:?}", range.lines), format!("{:?}", scalar.lines));
    }
}

/// A small deterministic generator for the property test below.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 32) as u32
    }
}

/// `(linear page, physical page)` pairs per context. Physical page 5 sits under the same linear
/// page in both contexts. Linear page 6 maps to different physical pages. Physical page 9 is
/// reached through two linear pages, so it becomes aliased on first use from both contexts.
const CONTEXT_MAPS: [[(u32, u32); 4]; 2] = [
    [(5, 5), (6, 6), (9, 9), (10, 10)],
    [(5, 5), (6, 7), (8, 9), (10, 10)],
];

#[test]
fn narrow_kill_handles_both_contexts_under_random_traffic() {
    for seed in 1..=6u64 {
        let mut rng = XorShift(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut cache = DecodeCache::new(256);
        let generations = [cache.generation, cache.allocate_generation()];
        cache.contexts = [Some((0, generations[0])), Some((0x1000, generations[1]))];
        for _ in 0..200 {
            for _ in 0..3 {
                let context = (rng.next() & 1) as usize;
                let (linear_page, physical_page) = CONTEXT_MAPS[context][(rng.next() & 3) as usize];
                let offset = rng.next() % 48;
                let len = 1 + (rng.next() % 15) as u8;
                // Switch the current generation the way `select_context` does.
                cache.generation = generations[context];
                let put = cache.put(
                    (linear_page << 12) | offset,
                    instruction(len),
                    true,
                    (physical_page << 12) | offset,
                );
                assert!(put.inserted);
            }
            cache.generation = generations[(rng.next() & 1) as usize];

            let (_, physical_page) =
                CONTEXT_MAPS[(rng.next() & 1) as usize][(rng.next() & 3) as usize];
            let physical = (physical_page << 12) | (rng.next() % 64);
            let width = [1u32, 2, 4][(rng.next() % 3) as usize];
            let live = cache.live_generations();
            let before: Vec<DecodeLine> = cache.lines.to_vec();

            match cache.narrow_invalidate_write(physical, width) {
                Some(kills) => {
                    let mut expected = 0;
                    for (index, old) in before.iter().enumerate() {
                        let len = old.insn.map_or(0, |insn| u32::from(insn.len));
                        let overlaps = live.contains(&old.generation)
                            && len != 0
                            && old.phys_start < physical + width
                            && physical < old.phys_start + len;
                        let now = &cache.lines[index];
                        if overlaps {
                            expected += 1;
                            assert_eq!(
                                now.generation, 0,
                                "seed {seed}: slot {index} overlaps the write and must die"
                            );
                        } else {
                            assert_eq!(
                                format!("{old:?}"),
                                format!("{now:?}"),
                                "seed {seed}: slot {index} does not overlap the write"
                            );
                        }
                    }
                    assert_eq!(kills, expected, "seed {seed}: kill count");
                    #[cfg(feature = "jit")]
                    cache.assert_packs_consistent();
                    #[cfg(all(
                        feature = "jit",
                        target_arch = "x86_64",
                        any(target_os = "windows", target_os = "linux")
                    ))]
                    cache.assert_native_watch_consistent();
                }
                None => {
                    let refused = (0..width).map(|i| physical + i).any(|byte| {
                        cache.is_code_byte(byte)
                            && cache
                                .code_page_lin
                                .get(&(byte >> 12))
                                .is_none_or(|info| info.aliased)
                    });
                    assert!(
                        refused,
                        "seed {seed}: None needs an unknown or aliased page"
                    );
                }
            }
        }
    }
}
