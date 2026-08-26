// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::{Int13BufClass, int13_buf_class};

#[test]
fn buffer_segment_splits_at_a000_and_f000() {
    assert!(matches!(int13_buf_class(0x9FFF), Int13BufClass::Conv));
    assert!(matches!(int13_buf_class(0xA000), Int13BufClass::Uma));
    assert!(matches!(int13_buf_class(0xEFFF), Int13BufClass::Uma));
    assert!(matches!(int13_buf_class(0xF000), Int13BufClass::Hma));
    assert!(matches!(int13_buf_class(0xFFFF), Int13BufClass::Hma));
}
