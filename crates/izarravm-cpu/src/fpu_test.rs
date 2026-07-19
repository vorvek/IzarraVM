// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_core::{
    CanonicalSectionId, CanonicalSectionRequirement, CanonicalSectionVersion, CanonicalStateView,
    CanonicalStateWriter,
};

const X87_PAYLOAD_LEN: usize = 134;

fn x87_payload(fpu: &X87) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(1).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| fpu.write_canonical_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn sentinel_x87() -> X87 {
    X87 {
        st: [
            f64::from_bits(0x0000_0000_0000_0000),
            f64::from_bits(0x8000_0000_0000_0000),
            f64::from_bits(0x7ff8_0000_0000_0042),
            f64::from_bits(0x7ff0_0000_0000_0043),
            f64::from_bits(0x3ff8_0000_0000_0000),
            f64::from_bits(0xc004_0000_0000_0000),
            f64::from_bits(0x0000_0000_0000_0001),
            f64::from_bits(0x7ff0_0000_0000_0000),
        ],
        control: 0x1234,
        status: 0xa941,
        tag: 0x5678,
        mm: [
            0x0102_0304_0506_0708,
            0x1112_1314_1516_1718,
            0x2122_2324_2526_2728,
            0x3132_3334_3536_3738,
            0x4142_4344_4546_4748,
            0x5152_5354_5556_5758,
            0x6162_6364_6566_6768,
            0x7172_7374_7576_7778,
        ],
    }
}

fn assert_only_span_changes<F>(fpu: &X87, span: core::ops::Range<usize>, mutate: F)
where
    F: FnOnce(&mut X87),
{
    let before = x87_payload(fpu);
    let mut changed = fpu.clone();
    mutate(&mut changed);
    let after = x87_payload(&changed);
    let changed_offsets: Vec<_> = before
        .iter()
        .zip(&after)
        .enumerate()
        .filter_map(|(index, (left, right))| (left != right).then_some(index))
        .collect();
    assert!(
        !changed_offsets.is_empty(),
        "mutation did not change payload"
    );
    assert!(
        changed_offsets.iter().all(|offset| span.contains(offset)),
        "changed offsets {changed_offsets:?} escaped {span:?}"
    );
}

#[test]
fn canonical_payload_has_exact_golden_bytes() {
    let fpu = sentinel_x87();
    let mut expected = Vec::new();
    expected.extend_from_slice(&fpu.control.to_le_bytes());
    expected.extend_from_slice(&fpu.status.to_le_bytes());
    expected.extend_from_slice(&fpu.tag.to_le_bytes());
    for value in fpu.st {
        expected.extend_from_slice(&value.to_bits().to_le_bytes());
    }
    for value in fpu.mm {
        expected.extend_from_slice(&value.to_le_bytes());
    }
    assert_eq!(expected.len(), X87_PAYLOAD_LEN);
    assert_eq!(x87_payload(&fpu), expected);
}

#[test]
fn canonical_payload_assigns_each_field_one_span() {
    let fpu = sentinel_x87();
    assert_only_span_changes(&fpu, 0..2, |changed| changed.control ^= 1);
    assert_only_span_changes(&fpu, 2..4, |changed| changed.status ^= 1);
    assert_only_span_changes(&fpu, 4..6, |changed| changed.tag ^= 1);
    for index in 0..8 {
        let start = 6 + index * 8;
        assert_only_span_changes(&fpu, start..start + 8, |changed| {
            changed.st[index] = f64::from_bits(changed.st[index].to_bits() ^ 1);
        });
    }
    for index in 0..8 {
        let start = 70 + index * 8;
        assert_only_span_changes(&fpu, start..start + 8, |changed| {
            changed.mm[index] ^= 1;
        });
    }
}

#[test]
fn canonical_payload_keeps_physical_order_across_top_wrap() {
    let mut fpu = sentinel_x87();
    fpu.set_top(7);
    let before = x87_payload(&fpu);
    fpu.inc_top();
    let after = x87_payload(&fpu);

    assert_eq!(fpu.top(), 0);
    assert_ne!(&before[2..4], &after[2..4]);
    assert_eq!(&before[..2], &after[..2]);
    assert_eq!(&before[4..], &after[4..]);
}

#[test]
fn canonical_payload_keeps_mmx_bits_when_emms_changes_tags() {
    let mut fpu = sentinel_x87();
    let before = x87_payload(&fpu);
    fpu.emms();
    let after = x87_payload(&fpu);

    assert_ne!(&before[4..6], &after[4..6]);
    assert_eq!(&before[..4], &after[..4]);
    assert_eq!(&before[6..], &after[6..]);
}

#[test]
fn canonical_payload_does_not_mutate_fpu_state() {
    let fpu = sentinel_x87();
    let before = fpu.clone();
    let _ = x87_payload(&fpu);
    assert_eq!(fpu, before);
}

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
