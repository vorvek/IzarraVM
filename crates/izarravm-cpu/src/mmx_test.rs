// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn padd_w_wraps_per_lane() {
    // lanes: 0xffff+1=0, 1+1=2, 0x7fff+1=0x8000, 0+0=0
    let a = from_words([0xffff, 1, 0x7fff, 0]);
    let b = from_words([1, 1, 1, 0]);
    assert_eq!(words(padd_w(a, b)), [0, 2, 0x8000, 0]);
}

#[test]
fn padds_w_saturates_signed() {
    let a = from_words([0x7fff, 0x8000, 0, 0]);
    let b = from_words([1, 0xffff, 0, 0]); // +1, -1
    assert_eq!(words(padds_w(a, b)), [0x7fff, 0x8000, 0, 0]);
}

#[test]
fn paddus_b_saturates_unsigned() {
    let a = u64::from_le_bytes([250, 10, 0, 0, 0, 0, 0, 0]);
    let b = u64::from_le_bytes([10, 10, 0, 0, 0, 0, 0, 0]);
    assert_eq!(paddus_b(a, b).to_le_bytes(), [255, 20, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn pcmpgt_w_is_signed() {
    let a = from_words([0, 5, 0xffff, 0]); // 0, 5, -1
    let b = from_words([0xffff, 4, 0, 0]); // -1, 4, 0
    assert_eq!(words(pcmpgt_w(a, b)), [0xffff, 0xffff, 0, 0]);
}

#[test]
fn punpcklbw_interleaves_low_bytes() {
    let a = u64::from_le_bytes([1, 2, 3, 4, 0, 0, 0, 0]);
    let b = u64::from_le_bytes([0x11, 0x22, 0x33, 0x44, 0, 0, 0, 0]);
    assert_eq!(
        punpcklbw(a, b).to_le_bytes(),
        [1, 0x11, 2, 0x22, 3, 0x33, 4, 0x44]
    );
}

#[test]
fn packsswb_saturates_to_signed_bytes() {
    let a = from_words([0x7fff, 0x8000, 5, 0xfffb]); // 32767, -32768, 5, -5
    assert_eq!(packsswb(a, 0).to_le_bytes(), [127, 128, 5, 251, 0, 0, 0, 0]);
}

#[test]
fn pmaddwd_multiplies_and_adds_pairs() {
    let a = from_words([2, 3, 4, 5]);
    let b = from_words([10, 20, 30, 40]);
    // lo = 2*10 + 3*20 = 80; hi = 4*30 + 5*40 = 320
    assert_eq!(dwords(pmaddwd(a, b)), [80, 320]);
}

#[test]
fn psllq_shifts_the_whole_register() {
    assert_eq!(psllq(1, 4), 16);
    assert_eq!(psllq(1, 64), 0);
}

#[test]
fn psraw_replicates_sign() {
    let a = from_words([0x8000, 0x4000, 0, 0]); // -32768, 16384
    assert_eq!(words(psraw(a, 1)), [0xc000, 0x2000, 0, 0]);
}
