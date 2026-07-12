// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn finit_sets_documented_reset_state() {
    let fpu = X87::default();
    assert_eq!(fpu.control, 0x037f);
    assert_eq!(fpu.status, 0);
    assert_eq!(fpu.tag, 0xffff);
    assert_eq!(fpu.top(), 0);
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn native_layout_matches_the_live_x87_state() {
    let fpu = X87::default();
    let layout = X87::native_layout();
    let base = core::ptr::addr_of!(fpu) as usize;

    assert_eq!(layout.st, core::ptr::addr_of!(fpu.st) as usize - base);
    assert_eq!(
        layout.control,
        core::ptr::addr_of!(fpu.control) as usize - base
    );
    assert_eq!(
        layout.status,
        core::ptr::addr_of!(fpu.status) as usize - base
    );
    assert_eq!(layout.tag, core::ptr::addr_of!(fpu.tag) as usize - base);
    assert_eq!(layout.st_stride, core::mem::size_of::<f64>());
}

#[test]
fn push_decrements_top_and_fills_st0() {
    let mut fpu = X87::default();
    fpu.push(1.5);
    assert_eq!(fpu.top(), 7);
    assert_eq!(fpu.get(0), 1.5);
    assert!(!fpu.is_empty(0));
    assert!(fpu.is_empty(1));
}

#[test]
fn push_then_pop_restores_top_and_empties() {
    let mut fpu = X87::default();
    fpu.push(3.0);
    fpu.push(4.0);
    assert_eq!(fpu.get(0), 4.0);
    assert_eq!(fpu.get(1), 3.0);
    fpu.pop();
    assert_eq!(fpu.top(), 7);
    assert_eq!(fpu.get(0), 3.0);
}

#[test]
fn top_is_reflected_in_the_status_word() {
    let mut fpu = X87::default();
    fpu.push(0.0);
    // TOP=7 lands in status bits 11-13.
    assert_eq!((fpu.status & TOP_MASK) >> TOP_SHIFT, 7);
}
