// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn bridge<const RETAIN: bool>(helper: CallOutHelper) -> Vec<u8> {
    let mut e = Encoder::new();
    let abnormal = e.label();
    let step_break = e.label();
    emit_call_out::<RETAIN>(
        &mut e,
        helper,
        0,
        3,
        abnormal,
        step_break,
        matches!(helper, CallOutHelper::PortReadAlDx).then_some(7),
        None,
        false,
    );
    e.place(abnormal);
    e.place(step_break);
    e.finish()
}

#[test]
fn callout_retained_homes_remove_only_the_four_in_reloads() {
    let legacy = bridge::<false>(CallOutHelper::PortReadAlDx);
    let candidate = bridge::<true>(CallOutHelper::PortReadAlDx);
    let mut stores = Encoder::new();
    emit_store_homes(&mut stores);
    let stores = stores.finish();
    assert!(legacy.starts_with(&stores));
    assert!(candidate.starts_with(&stores));
    let mut removed = 0;
    for (index, home) in GUEST_HOMES.into_iter().enumerate() {
        let mut e = Encoder::new();
        e.load_r32_disp32(home, Reg::R15, gpr_offset(index));
        let load = e.finish();
        let occurrences = |code: &[u8]| code.windows(load.len()).filter(|w| *w == load).count();
        assert_eq!(occurrences(&legacy), 1, "legacy home {index}");
        assert_eq!(
            occurrences(&candidate),
            usize::from(index < 4),
            "candidate home {index}"
        );
        if index >= 4 {
            removed += load.len();
        }
    }
    assert_eq!(legacy.len() - candidate.len(), removed);
}

#[test]
fn callout_retained_homes_leave_stack_helpers_byte_identical() {
    for helper in [CallOutHelper::PushAllDword, CallOutHelper::PopAllDword] {
        assert_eq!(bridge::<false>(helper), bridge::<true>(helper));
    }
}
