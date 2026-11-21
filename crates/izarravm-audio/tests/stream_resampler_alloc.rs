// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The audio callback must not allocate. This test counts heap allocations on the
//! calling thread while the streaming resampler runs. It lives in its own test
//! binary because the counting allocator needs `unsafe`, which the library crate
//! forbids.

use izarravm_audio::StreamResampler;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: every call forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|count| count.set(count.get() + 1));
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|count| count.set(count.get() + 1));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

#[test]
fn push_and_pull_do_not_allocate_after_new() {
    for out_hz in [22_050u32, 48_000, 96_000, 192_000] {
        let mut stream = StreamResampler::new(44_100, out_hz);
        let mut n = 0i32;
        let before = ALLOCS.with(Cell::get);
        let mut sum = 0i64;
        for _ in 0..out_hz {
            let frame = loop {
                if let Some(frame) = stream.pull() {
                    break frame;
                }
                n = n.wrapping_add(331);
                stream.push((n % 20_000, -(n % 20_000)));
            };
            sum += i64::from(frame.0);
        }
        let after = ALLOCS.with(Cell::get);
        assert_eq!(after, before, "{out_hz} Hz allocated while streaming");
        assert!(sum != i64::MAX);
    }
}
