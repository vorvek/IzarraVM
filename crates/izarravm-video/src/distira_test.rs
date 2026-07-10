// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn model_always_reports_two_tmus() {
    assert_eq!(Distira::new().tmu_count(), 2);
}
