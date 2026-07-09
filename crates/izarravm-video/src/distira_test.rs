// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn model_always_reports_two_tmus() {
    assert_eq!(Distira::new().tmu_count(), 2);
}

#[test]
fn render_threads_default_to_86box_choices() {
    let mut distira = Distira::new();

    assert_eq!(distira.render_threads(), 2);
    distira.set_render_threads(4);
    assert_eq!(distira.render_threads(), 4);
    distira.set_render_threads(3);
    assert_eq!(distira.render_threads(), 2);
}
