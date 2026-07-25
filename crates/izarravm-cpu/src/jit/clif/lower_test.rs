// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::MemFlagsData;

/// C1e mandated regression guard (design section 3.2, review finding M1): the
/// operand-table loads (`UnitBuilder::flags()`, which returns
/// `MemFlagsData::trusted()` verbatim) must NEVER carry `readonly`. Historical note:
/// the design first argued non-readonly flags alone made a no-generation-bump restamp
/// sound; the C1e in-flight battery FALSIFIED that on cranelift 0.133.1 (a trusted()
/// load was still reordered across the x87 call-out), so a restamp now bumps the
/// generation (see `invalidate_physical_range`'s reversal note). This guard stays as
/// defense-in-depth: a readonly operand table would additionally license folding
/// lane loads across RESTAMPS OBSERVED BY FRESH ENTRIES, a strictly worse hazard the
/// generation latch does not cover (nothing exits a unit that was entered AFTER the
/// patch). If a perf tweak ever wants readonly here, the restamp design must be
/// re-opened first.
#[test]
fn operand_table_loads_are_never_readonly() {
    assert!(
        !MemFlagsData::trusted().readonly(),
        "trusted() must stay notrap|aligned WITHOUT readonly (design 3.2)"
    );
}
