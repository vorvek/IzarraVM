// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Tests for the timing class table (slice 1a).
//!
//! They cover the shape of the table -- density, the epoch-1 pin, the persona
//! and epoch selection, the `Legacy` escape and the unsourced census. The proof
//! that a SITE is routed to the right class lives elsewhere, in the several
//! hundred exact-clock assertions the tree already carries; see
//! `EPOCH1_FIXTURE`'s comment.

use super::*;
use izarravm_core::CpuPersona;

/// The epoch-1 column, restated class by class as a PIN.
///
/// Each entry is the literal the class's charge sites carried at the fork point
/// (`origin/vorvek/timing-epoch2-ports`): `Ok(clocks(N))` in `execute.rs` /
/// `execute_extended.rs` / `fpu_exec.rs` / `run.rs`, or the named
/// `*_CORE_CLOCKS` constant in `lib.rs`.
///
/// What this pin is and is not. It is NOT the independent check that routing is
/// correct -- the tree already has that, in the several hundred exact-clock
/// assertions across `cpu_test.rs`, `cpu_jit_direct_timing_test.rs`,
/// `cpu_jit_interpret_one_test.rs` and the board fixtures, every one of which
/// goes red if a site is routed to a class whose epoch-1 value differs by even
/// one clock. What it IS: a guard against a later sub-slice editing an EPOCH-1
/// entry while re-solving an epoch-2 column beside it, which no per-opcode test
/// would attribute to the table. Change a value here only with the matching
/// per-opcode test change.
const EPOCH1_FIXTURE: &[(TimingClass, u16)] = &[
    (TimingClass::Reg, 2),
    (TimingClass::AluRegMem, 2),
    (TimingClass::AluMemReg, 2),
    (TimingClass::MovRegMem, 2),
    (TimingClass::MovMemReg, 2),
    (TimingClass::MovImmReg, 2),
    (TimingClass::MovImmMem, 2),
    (TimingClass::MovAccMoffs, 4),
    (TimingClass::Lea, 2),
    (TimingClass::Xchg, 3),
    (TimingClass::MovExtend, 3),
    (TimingClass::FlagOp, 2),
    (TimingClass::Cli, 3),
    (TimingClass::Sti, 3),
    (TimingClass::Sahf, 3),
    (TimingClass::Lahf, 2),
    (TimingClass::Cbw, 3),
    (TimingClass::Cwd, 2),
    (TimingClass::DecimalAdjust, 4),
    (TimingClass::Aam, 17),
    (TimingClass::Aad, 19),
    (TimingClass::Xlat, 5),
    (TimingClass::PushReg, 2),
    (TimingClass::PopReg, 4),
    (TimingClass::PushImm, 2),
    (TimingClass::PushMem, 2),
    (TimingClass::PopMem, 5),
    (TimingClass::PushSeg, 2),
    (TimingClass::PopSeg, 7),
    (TimingClass::PopSs, 7),
    (TimingClass::PushAll, 18),
    (TimingClass::PopAll, 18),
    (TimingClass::PushFlags, 3),
    (TimingClass::PopFlags, 4),
    (TimingClass::Enter, 10),
    (TimingClass::Leave, 4),
    (TimingClass::Jcc, 3),
    (TimingClass::CallJmpRel, 7),
    (TimingClass::CallJmpRm, 7),
    (TimingClass::RetNear, 10),
    (TimingClass::RetNearImm, 10),
    (TimingClass::CallFar, 17),
    (TimingClass::JmpFar, 17),
    (TimingClass::RetFar, 17),
    (TimingClass::CallJmpFarMem, 11),
    (TimingClass::FarTransferPm, 17),
    (TimingClass::FarTransferGate, 17),
    (TimingClass::FarTransferTss, 17),
    (TimingClass::Loop, 11),
    (TimingClass::LoopCc, 11),
    (TimingClass::Jcxz, 9),
    (TimingClass::Nop, 3),
    (TimingClass::IntN, 37),
    (TimingClass::IntNV86, 37),
    (TimingClass::IntNPm, 37),
    (TimingClass::Int3, 33),
    (TimingClass::IntO, 35),
    (TimingClass::IntONotTaken, 3),
    (TimingClass::Iret, 22),
    (TimingClass::IretPm, 22),
    (TimingClass::IretPmToV86, 22),
    (TimingClass::IretV86, 22),
    (TimingClass::ShiftImm, 2),
    (TimingClass::ShiftCl, 2),
    (TimingClass::DoubleShift, 3),
    (TimingClass::TestImmReg, 2),
    (TimingClass::TestImmMem, 2),
    (TimingClass::NotNegReg, 2),
    (TimingClass::NotNegMem, 2),
    (TimingClass::Mul8, 2),
    (TimingClass::Mul16, 2),
    (TimingClass::Mul32, 2),
    (TimingClass::Div8, 2),
    (TimingClass::Div16, 2),
    (TimingClass::Div32, 2),
    (TimingClass::Idiv8, 2),
    (TimingClass::Idiv16, 2),
    (TimingClass::Idiv32, 2),
    (TimingClass::IncDecRm, 2),
    (TimingClass::ImulRm, 9),
    (TimingClass::ImulImm, 14),
    (TimingClass::BitTest, 6),
    (TimingClass::BitTestModify, 6),
    (TimingClass::BitScan, 10),
    (TimingClass::Bswap, 1),
    (TimingClass::CmpXchg, 6),
    (TimingClass::CmpXchg8b, 10),
    (TimingClass::Xadd, 4),
    (TimingClass::SetCc, 4),
    (TimingClass::MovRegSreg, 2),
    (TimingClass::MovSregReg, 7),
    (TimingClass::LesLds, 7),
    (TimingClass::Lar, 11),
    (TimingClass::Lsl, 11),
    (TimingClass::VerRw, 10),
    (TimingClass::SldtStr, 2),
    (TimingClass::LldtLtr, 11),
    (TimingClass::SgdtSidt, 11),
    (TimingClass::LgdtLidt, 11),
    (TimingClass::Smsw, 2),
    (TimingClass::Lmsw, 3),
    (TimingClass::Invlpg, 12),
    (TimingClass::Clts, 2),
    (TimingClass::MovCrDr, 6),
    (TimingClass::Bound, 10),
    (TimingClass::Wrmsr, 30),
    (TimingClass::Rdtsc, 11),
    (TimingClass::Rdmsr, 11),
    (TimingClass::Invd, 4),
    (TimingClass::Wbinvd, 4),
    (TimingClass::Cpuid, 14),
    (TimingClass::StringElem, 4),
    (TimingClass::InsString, 15),
    (TimingClass::OutsString, 14),
    (TimingClass::InPort, 12),
    (TimingClass::OutPort, 10),
    (TimingClass::InPortDword, 12),
    (TimingClass::ExceptionDelivery, 59),
    (TimingClass::ExceptionDeliveryV86, 59),
    (TimingClass::HardwareInterrupt, 61),
    (TimingClass::TaskSwitch, 0),
    (TimingClass::X87Wait, 6),
    (TimingClass::X87MemArith32, 20),
    (TimingClass::X87MemArith64, 20),
    (TimingClass::X87MemDiv32, 20),
    (TimingClass::X87MemDiv64, 20),
    (TimingClass::X87MemArithInt32, 20),
    (TimingClass::X87MemArithInt16, 20),
    (TimingClass::X87MemArithIntDiv32, 20),
    (TimingClass::X87MemArithIntDiv16, 20),
    (TimingClass::X87LoadReal32, 14),
    (TimingClass::X87StoreReal32, 14),
    (TimingClass::X87LoadReal64, 14),
    (TimingClass::X87StoreReal64, 14),
    (TimingClass::X87LoadExtended80, 14),
    (TimingClass::X87StoreExtended80, 14),
    (TimingClass::X87LoadInt32, 14),
    (TimingClass::X87StoreInt32, 14),
    (TimingClass::X87LoadInt16, 14),
    (TimingClass::X87StoreInt16, 14),
    (TimingClass::X87LoadInt64, 14),
    (TimingClass::X87StoreInt64, 14),
    (TimingClass::X87LoadControl, 4),
    (TimingClass::X87StoreControl, 14),
    (TimingClass::X87StoreStatus, 14),
    (TimingClass::X87LoadEnv, 44),
    (TimingClass::X87StoreEnv, 56),
    (TimingClass::X87Restore, 75),
    (TimingClass::X87Save, 150),
    (TimingClass::X87LoadBcd, 75),
    (TimingClass::X87StoreBcd, 160),
    (TimingClass::X87RegArith, 20),
    (TimingClass::X87RegDiv, 20),
    (TimingClass::X87RegCompare, 4),
    (TimingClass::X87RegExchange, 4),
    (TimingClass::X87RegSign, 6),
    (TimingClass::X87RegConst, 8),
    (TimingClass::X87Xam, 8),
    (TimingClass::X87RegConstCheap, 4),
    (TimingClass::X87Exp, 200),
    (TimingClass::X87Transcendental, 300),
    (TimingClass::X87Sqrt, 70),
    (TimingClass::X87Rem, 100),
    (TimingClass::X87RoundInt, 20),
    (TimingClass::X87Scale, 30),
    (TimingClass::X87StackPointer, 4),
    (TimingClass::X87Control, 2),
    (TimingClass::X87Init, 3),
    (TimingClass::X87Free, 3),
    (TimingClass::X87StatusReg, 3),
    (TimingClass::X87RegStore, 3),
    (TimingClass::X87ComparePop, 5),
];

/// Every table-backed class must have a pinned epoch-1 value, and the pin must
/// be what `EPOCH1` charges. See `EPOCH1_FIXTURE`'s doc comment for what this
/// does and does not prove.
#[test]
fn epoch_one_charges_the_pinned_literal_for_every_class() {
    assert_eq!(
        EPOCH1_FIXTURE.len(),
        N_CLASSES,
        "every class needs an epoch-1 pin; the fixture and the enum disagree"
    );
    for (class, literal) in EPOCH1_FIXTURE {
        assert_eq!(
            EPOCH1.raw(*class),
            u32::from(*literal),
            "{} charges the wrong epoch-1 literal",
            class.name()
        );
    }
    // And the pins are in table order, so a class inserted in the middle of the
    // list cannot silently shift every pin below it onto its neighbour.
    for (index, (class, _)) in EPOCH1_FIXTURE.iter().enumerate() {
        assert_eq!(
            class.index(),
            index,
            "{} sits at table index {} but is pinned at {index}",
            class.name(),
            class.index()
        );
    }
}

/// The mutation this catches: a class added to the enum and forgotten in a
/// persona column, which the macro makes impossible for the SHAPE but not for
/// the VALUE -- a `0` entry would silently make an instruction free.
#[test]
fn every_class_charges_something_in_every_persona_table() {
    for table in [&EPOCH1, &EPOCH2_I486, &EPOCH2_I586] {
        for class in TimingClass::ALL {
            // `TaskSwitch` is the one class whose epoch-1 entry is not a literal
            // it replaced: no task-switch term existed at all before slice 8
            // (the census scores that as under by 100x or more), and inventing
            // one at epoch 1 would break the knob-unset identity bar. Its
            // epoch-2 columns are real and are asserted instead.
            if *class == TimingClass::TaskSwitch {
                assert_eq!(EPOCH1.raw(*class), 0);
                assert!(EPOCH2_I486.raw(*class) > 0 && EPOCH2_I586.raw(*class) > 0);
                continue;
            }
            assert!(
                table.raw(*class) > 0,
                "{} charges zero clocks",
                class.name()
            );
        }
        assert_eq!(table.entries().len(), N_CLASSES);
    }
}

/// Epoch 1 is byte-identical for every persona BY CONSTRUCTION: one table, no
/// persona arm. This is the property the knob-unset identity fixture rests on,
/// stated as a test so a future persona arm cannot be added without noticing.
#[test]
fn epoch_one_resolves_to_one_table_for_every_persona() {
    for persona in [CpuPersona::I386, CpuPersona::I486, CpuPersona::I586] {
        assert!(
            std::ptr::eq(class_table(persona, 1), &EPOCH1),
            "{persona:?} must resolve to the epoch-1 table under epoch 1"
        );
    }
}

/// The 386 is out of the recalibration's scope (design section 9.9), so it must
/// stay on the epoch-1 column under epoch 2 as well -- otherwise every 386 board
/// row re-pins for a persona nobody re-solved.
#[test]
fn the_386_never_leaves_the_epoch_one_column() {
    for epoch in [1u32, 2, 3] {
        assert!(
            std::ptr::eq(class_table(CpuPersona::I386, epoch), &EPOCH1),
            "the 386 moved under epoch {epoch}"
        );
    }
}

/// The two epoch-2 columns are selected by persona, and an epoch above 2
/// resolves like 2 rather than silently falling back to epoch 1 (the knob parser
/// is the one place a spelling is refused).
#[test]
fn epoch_two_selects_the_persona_column() {
    assert!(std::ptr::eq(class_table(CpuPersona::I486, 2), &EPOCH2_I486));
    assert!(std::ptr::eq(class_table(CpuPersona::I586, 2), &EPOCH2_I586));
    assert!(std::ptr::eq(class_table(CpuPersona::I586, 7), &EPOCH2_I586));
}

/// The `Legacy` escape charges its own literal under every table, which is what
/// lets routing proceed site by site without an epoch-1 charge moving.
#[test]
fn legacy_charges_its_own_literal_under_every_table() {
    for table in [&EPOCH1, &EPOCH2_I486, &EPOCH2_I586] {
        for literal in [0u16, 1, 2, 37, 300, u16::MAX] {
            assert_eq!(table.raw(TimingClass::Legacy(literal)), u32::from(literal));
        }
    }
}

/// Indices are dense, unique, and cover exactly `0..N_CLASSES`.
#[test]
fn class_indices_are_a_dense_permutation() {
    let mut seen = [false; N_CLASSES];
    for class in TimingClass::ALL {
        let index = class.index();
        assert!(index < N_CLASSES, "{} is out of range", class.name());
        assert!(!seen[index], "{} collides at index {index}", class.name());
        seen[index] = true;
    }
    assert!(seen.iter().all(|hit| *hit));
}

/// Names are unique, so the class histogram (design section 9.1) and every
/// failure message above name exactly one row.
#[test]
fn class_names_are_unique() {
    let mut names: Vec<&str> = TimingClass::ALL.iter().map(|class| class.name()).collect();
    names.sort_unstable();
    let count = names.len();
    names.dedup();
    assert_eq!(names.len(), count, "two classes share a name");
}

/// The unsourced census, pinned -- and it is now ZERO.
///
/// A class whose provenance says `UNSOURCED x12` has NO reference count behind
/// its epoch-2 entry: it is the epoch-1 literal times twelve, a default that
/// preserves today's relative cost and errs slow. Slice 1a shipped 78 of them.
/// `dev_docs/2026-09-05-class-table-sources.md` sourced every one against
/// Intel's Pentium Table F-2/F-3/F-5, the Optimization Manual's Table A-1 for
/// the pairing letters, and the i486 DX2 Data Book's Tables 10.1-10.3, so the
/// count is now 0 and a row that reappears has to say why in the diff.
///
/// `PLACEHOLDER` is the stronger admission: the row is not merely unsourced, its
/// SHAPE is wrong. One remains -- `StringElem`, which still fuses a per-element
/// cost with a setup cost. The sources doc supplies both terms for all seven
/// string families; spending them is slice 1's REP item
/// (`RepLimitPlan::compute`), not this commit's.
#[test]
fn the_unsourced_and_placeholder_census_is_pinned() {
    let unsourced = TimingClass::ALL
        .iter()
        .filter(|class| class.provenance().contains("UNSOURCED"))
        .count();
    let placeholder = TimingClass::ALL
        .iter()
        .filter(|class| class.provenance().contains("PLACEHOLDER"))
        .count();
    assert_eq!(
        (unsourced, placeholder),
        (0, 1),
        "the unsourced/placeholder census moved; update the pin and say why"
    );
    for class in TimingClass::ALL {
        assert!(
            !class.provenance().is_empty(),
            "{} has no provenance",
            class.name()
        );
    }
}

/// Every epoch-2 entry is an exact multiple of twelve, because design section
/// 3.1 fixes the unit: `level_timing` stays `(1, 12)` and one raw clock is one
/// twelfth of a core clock, so every Intel count lands on a twelfth boundary. A
/// non-multiple is a transcription slip, not a finer measurement -- UNLESS it is
/// a declared blend, and the blend list is pinned exactly so a fabricated value
/// cannot hide behind the word.
///
/// The declared blends, and there are now only two families of them:
/// * I586 `Jcc` raw 16 (1.33 clk, a third of a clock) -- design section 3.4:
///   Intel's 1-clock predicted cost plus an amortized mispredict, not any single
///   count.
/// * I586 `Loop` / `LoopCc` / `Jcxz` raw 66 / 90 / 66 (5.5 / 7.5 / 5.5) --
///   section 3.2's midpoints of Intel's taken/not-taken ranges (5-6, 7-8, 6-5),
///   which the interpreter's one-arm charge cannot separate.
/// * I486 `X87MemArithIntDiv32` raw 1026 (85.5 clk) -- Intel's OWN printed
///   average of the DX2's 84-86 range for `FIDIV m32` (Table 10.3's "Cache Hit"
///   column is an average), not a midpoint we chose.
///
/// The multiply midpoints that used to be here are gone: the manual sourcing
/// replaced `Mul8`/`Mul16`/`Mul32` and `ImulRm`/`ImulImm`'s invented 486
/// "midpoints" with the slow end of Intel's own MN/MX ranges, which are whole
/// clocks.
const DECLARED_BLENDS: &[(&str, &str)] = &[
    ("I486", "X87MemArithIntDiv32"),
    ("I586", "Jcc"),
    ("I586", "Loop"),
    ("I586", "LoopCc"),
    ("I586", "Jcxz"),
];

#[test]
fn epoch_two_entries_are_whole_twelfths_except_the_declared_blends() {
    let mut fractional: Vec<(&str, &str)> = Vec::new();
    for (name, table) in [("I486", &EPOCH2_I486), ("I586", &EPOCH2_I586)] {
        for class in TimingClass::ALL {
            let raw = table.raw(*class);
            if raw % 12 != 0 {
                fractional.push((name, class.name()));
            }
        }
    }
    fractional.sort_unstable();
    let mut declared = DECLARED_BLENDS.to_vec();
    declared.sort_unstable();
    assert_eq!(
        fractional, declared,
        "the blended-entry list moved; a fractional epoch-2 entry needs a documented reason"
    );
}

/// The seam, end to end on a real CPU: the epoch and the persona pick the
/// column, `charge` reads it, and epoch 1 charges the same number for all three
/// personas.
///
/// This is the live half of the identity argument. The dead half -- that every
/// ROUTED site picks the right class -- is carried by the tree's existing
/// exact-clock assertions, which all still hold with 131 sites routed.
#[test]
fn the_cpu_charges_the_epoch_and_persona_column() {
    use crate::CpuGsw;
    use izarravm_core::GswMode;

    for (mode, persona) in [
        (GswMode::Gsw386, CpuPersona::I386),
        (GswMode::Gsw486, CpuPersona::I486),
        (GswMode::Gsw586, CpuPersona::I586),
    ] {
        let mut cpu = CpuGsw::default();
        cpu.set_mode(mode);
        assert_eq!(cpu.timing_epoch(), 1, "a fresh CPU is epoch 1");
        assert_eq!(cpu.persona(), persona);
        // Epoch 1: today's literal, the same for every persona.
        assert_eq!(cpu.charge(TimingClass::Reg).core_clocks, 2);
        assert_eq!(cpu.charge(TimingClass::Div32).core_clocks, 2);
        assert_eq!(cpu.charge(TimingClass::Legacy(37)).core_clocks, 37);

        cpu.set_timing_epoch(2);
        assert_eq!(cpu.timing_epoch(), 2);
        let expected = class_table(persona, 2);
        assert_eq!(
            cpu.charge(TimingClass::Reg).core_clocks,
            expected.raw(TimingClass::Reg)
        );
        // The 386 stays on the epoch-1 column; the other two move.
        let moved = cpu.charge(TimingClass::Reg).core_clocks != 2;
        assert_eq!(moved, persona != CpuPersona::I386, "{persona:?}");

        // And the epoch survives a later mode switch, which re-resolves the
        // table rather than dropping back to epoch 1.
        cpu.set_mode(GswMode::Gsw586);
        assert_eq!(cpu.timing_epoch(), 2);
        assert_eq!(
            cpu.charge(TimingClass::Reg).core_clocks,
            EPOCH2_I586.raw(TimingClass::Reg)
        );
    }
}

/// The classifier sites, pinned.
///
/// Sixteen charge sites do not name a class outright: they call `alu_class`,
/// `group1_class`, `group2_class` or `test_rm_class`, which pick from several
/// classes using the decoded operand or the opcode. Every one of those sites
/// carried the literal `2` before the routing, so epoch-1 identity holds only if
/// EVERY class those four functions can return is pinned at 2. A later
/// sub-slice that re-solves one of these epoch-2 entries and touches its epoch-1
/// entry by accident would silently move a charge on the hottest arms in the
/// interpreter; this catches it.
#[test]
fn every_class_a_classifier_can_return_is_pinned_at_the_sites_literal() {
    for class in [
        TimingClass::Reg,
        TimingClass::AluRegMem,
        TimingClass::AluMemReg,
        TimingClass::ShiftImm,
        TimingClass::ShiftCl,
        // ... and `group3_class`, whose thirteen classes all replace the single
        // `Ok(clocks(GROUP3_CORE_CLOCKS))` the `0xf6`/`0xf7` arms returned.
        TimingClass::TestImmReg,
        TimingClass::TestImmMem,
        TimingClass::NotNegReg,
        TimingClass::NotNegMem,
        TimingClass::Mul8,
        TimingClass::Mul16,
        TimingClass::Mul32,
        TimingClass::Div8,
        TimingClass::Div16,
        TimingClass::Div32,
        TimingClass::Idiv8,
        TimingClass::Idiv16,
        TimingClass::Idiv32,
    ] {
        assert_eq!(
            EPOCH1.raw(class),
            2,
            "{} is reachable from a classifier site whose literal was 2",
            class.name()
        );
    }
}

/// The four classifiers agree with the shapes their doc comments claim, checked
/// against the enum rather than against prose.
#[test]
fn the_classifiers_pick_the_documented_shapes() {
    use crate::execute::{group2_class, group3_class};
    use izarravm_bus::BusWidth;

    assert_eq!(group2_class(0xc0), TimingClass::ShiftImm);
    assert_eq!(group2_class(0xc1), TimingClass::ShiftImm);
    assert_eq!(group2_class(0xd0), TimingClass::ShiftImm);
    assert_eq!(group2_class(0xd1), TimingClass::ShiftImm);
    assert_eq!(group2_class(0xd2), TimingClass::ShiftCl);
    assert_eq!(group2_class(0xd3), TimingClass::ShiftCl);

    // Group 3: the sub-opcode picks the family, the width picks the row, and the
    // operand shape picks load-vs-RMW for the two families that have both.
    let reg = crate::RmOperand::Register(0);
    let mem = crate::RmOperand::Memory(crate::MemoryOperand {
        segment: crate::SegmentIndex::Ds,
        offset: 0,
    });
    for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
        assert_eq!(group3_class(0, width, reg), TimingClass::TestImmReg);
        assert_eq!(group3_class(1, width, reg), TimingClass::TestImmReg);
        assert_eq!(group3_class(0, width, mem), TimingClass::TestImmMem);
        assert_eq!(group3_class(2, width, reg), TimingClass::NotNegReg);
        assert_eq!(group3_class(3, width, mem), TimingClass::NotNegMem);
    }
    for (sub, byte, word, dword) in [
        (4, TimingClass::Mul8, TimingClass::Mul16, TimingClass::Mul32),
        (5, TimingClass::Mul8, TimingClass::Mul16, TimingClass::Mul32),
        (6, TimingClass::Div8, TimingClass::Div16, TimingClass::Div32),
        (
            7,
            TimingClass::Idiv8,
            TimingClass::Idiv16,
            TimingClass::Idiv32,
        ),
    ] {
        assert_eq!(group3_class(sub, BusWidth::Byte, reg), byte);
        assert_eq!(group3_class(sub, BusWidth::Word, reg), word);
        assert_eq!(group3_class(sub, BusWidth::Dword, mem), dword);
    }

    // And the 246x row itself: `DIV r/m32` charges 41 P5 clocks under epoch 2
    // where it charged 1/6 of a clock under epoch 1.
    assert_eq!(EPOCH1.raw(TimingClass::Div32), 2);
    assert_eq!(EPOCH2_I586.raw(TimingClass::Div32), 492);
    assert_eq!(EPOCH2_I486.raw(TimingClass::Div32), 480);
}

/// `max_raw` is what the budget bound's per-slot term reads (review B3), so it
/// must be the maximum over the WHOLE table, not over some subset that happens
/// to be the largest today.
#[test]
fn max_raw_is_the_largest_entry_in_the_table() {
    for (name, table) in [
        ("EPOCH1", &EPOCH1),
        ("I486", &EPOCH2_I486),
        ("I586", &EPOCH2_I586),
    ] {
        let expected = TimingClass::ALL
            .iter()
            .map(|class| table.raw(*class))
            .max()
            .expect("the table is not empty");
        assert_eq!(table.max_raw(), expected, "{name}");
        // And it dominates every class, which is the property the bound needs.
        for class in TimingClass::ALL {
            assert!(
                table.raw(*class) <= table.max_raw(),
                "{name} {}",
                class.name()
            );
        }
    }
    // The old literal the bound carried was 4, and it was ALREADY an under-bound
    // at epoch 1: `RetFar` charges 17. That is the finding, pinned.
    assert_eq!(EPOCH1.raw(TimingClass::RetFar), 17);
    assert!(EPOCH1.max_raw() > 4);
}

/// The `InterpretOne` budget term is a maximum over an allowlist, and the
/// allowlist has to contain the group-3 classes: `0xF7 /2../7` at Word and
/// `0xF6 /2../7` are both call-out rows, and `Idiv32` charges 552 raw under
/// epoch 2 where the epoch-1 constant is 7.
///
/// Without this the chain quota's DIVISOR would price a group-3 call-out at
/// 7/12ths of a clock while the slot charged 46 -- an under-budget of 78x, in
/// the release builds the campaign measures.
#[test]
fn the_interpret_one_budget_term_covers_the_group_three_rows() {
    let allowlist = crate::run::INTERPRET_ONE_CLASSES;
    for class in [
        TimingClass::Div32,
        TimingClass::Idiv32,
        TimingClass::Mul32,
        TimingClass::NotNegMem,
        TimingClass::TestImmMem,
    ] {
        assert!(
            allowlist.contains(&class),
            "{} is a call-out row and must be in the budget allowlist",
            class.name()
        );
    }
    let max_at = |table: &ClassTable| {
        allowlist
            .iter()
            .map(|class| table.raw(*class))
            .max()
            .expect("the allowlist is not empty")
    };
    // Epoch 1: the fold equals the constant it replaces, which the compile-time
    // tripwire in `timing_class.rs` also asserts.
    assert_eq!(max_at(&EPOCH1), crate::INTERPRET_ONE_MAX_CORE_CLOCKS);
    assert_eq!(max_at(&EPOCH1), 7);
    // Epoch 2: it moves, and it moves to the divide.
    assert_eq!(max_at(&EPOCH2_I586), EPOCH2_I586.raw(TimingClass::Idiv32));
    assert_eq!(max_at(&EPOCH2_I586), 552);
    assert_eq!(max_at(&EPOCH2_I486), EPOCH2_I486.raw(TimingClass::Idiv32));
    assert_eq!(max_at(&EPOCH2_I486), 528);
}

/// The four width sites (review B4), checked at the values that would have
/// truncated.
///
/// `DIV r/m32` is raw 492 on the 586 and 480 on the 486; `FSQRT` is raw 840.
/// Both exceed a `u8`, and 840 exceeds it by more than three times. Before slice
/// 1d, `jit/native_x87.rs`'s per-instruction `raw_clocks` was a `u8` and
/// `StaticAccounting`'s accumulator was a `u16` fed by a truncating cast and an
/// unchecked `+=`. This walks the values through every stage that used to
/// narrow them.
#[test]
fn the_widest_epoch_two_charges_survive_every_stage() {
    // Stage 1: the table itself. `u16`, so both fit with room.
    assert_eq!(EPOCH2_I586.raw(TimingClass::Div32), 492);
    assert_eq!(EPOCH2_I486.raw(TimingClass::Div32), 480);
    assert_eq!(EPOCH2_I586.raw(TimingClass::X87Sqrt), 840);
    assert_eq!(EPOCH2_I486.raw(TimingClass::X87Sqrt), 840);
    for class in [TimingClass::Div32, TimingClass::X87Sqrt] {
        for (name, table) in [("I486", &EPOCH2_I486), ("I586", &EPOCH2_I586)] {
            assert!(
                table.raw(class) > u32::from(u8::MAX),
                "{name} {} would have fitted a u8, so this test proves nothing",
                class.name()
            );
        }
    }

    // Stage 2: a full block of the widest class the JIT can lower natively.
    // `CompiledBlock::raw_clocks` is still a `u16` and still refuses rather than
    // truncating, so the worst native block has to fit it.
    #[cfg(feature = "jit")]
    {
        let worst_native = EPOCH2_I586.raw(TimingClass::Idiv32);
        let worst_block = worst_native * crate::jit::direct::MAX_BLOCK_INSTRUCTIONS as u32;
        assert!(
            worst_block <= u32::from(u16::MAX),
            "a full block of the widest native class ({worst_native} raw x \
         {} slots = {worst_block}) no longer fits the block sum's u16; either the sum widens \
         or the install refusal becomes reachable on ordinary code",
            crate::jit::direct::MAX_BLOCK_INSTRUCTIONS
        );

        // Stage 3: the static accounting accumulator, now u32. The widest entry in
        // the table is `WBINVD`'s printed floor, which is not a native slot but is
        // the number the accumulator has to be safe against if one ever is.
        let widest = EPOCH2_I586.max_raw();
        assert_eq!(widest, EPOCH2_I586.raw(TimingClass::Wbinvd));
        let worst_accumulation =
            u64::from(widest) * crate::jit::direct::MAX_BLOCK_INSTRUCTIONS as u64;
        assert!(
            worst_accumulation <= u64::from(u32::MAX),
            "the static accounting accumulator would saturate at {worst_accumulation}"
        );
        assert!(
            worst_accumulation > u64::from(u16::MAX),
            "the u16 this accumulator used to be would have WRAPPED at {worst_accumulation}, which \
         is the defect slice 1d fixed; if this stops being true the test is no longer a proof"
        );
    }
}

/// Native entries form an exact diagnostic partition without assigning a
/// repeated or linked entry to a guessed block vector.
#[cfg(feature = "timing-class-histogram")]
fn native_metadata(
    vector: &[u8],
    span_instructions: usize,
    self_loop: bool,
) -> NativeClassMetadata<'_> {
    NativeClassMetadata {
        self_loop,
        span_instructions,
        vector,
    }
}

#[cfg(feature = "timing-class-histogram")]
#[test]
fn native_histogram_partitions_only_a_proven_single_block_prefix() {
    let mut hist = TimingHistogram::default();
    let vector = [
        TimingClass::Reg.index() as u8,
        TimingClass::AluRegMem.index() as u8,
        TimingClass::Jcc.index() as u8,
    ];
    hist.record_native_entry(
        2,
        30,
        7,
        0,
        Some(native_metadata(&vector, vector.len(), false)),
    );
    let snapshot = hist.snapshot(&EPOCH1, 11);
    let known: std::collections::HashMap<_, _> =
        snapshot.native_known_class_counts.into_iter().collect();
    assert_eq!(known["Reg"], 1);
    assert_eq!(known["AluRegMem"], 1);
    assert!(!known.contains_key("Jcc"));
    assert_eq!(snapshot.native_entries, 1);
    assert_eq!(snapshot.native_instructions, 2);
    assert_eq!(snapshot.native_observed_raw_core, 30);
    assert_eq!(snapshot.native_observed_weighted_fp, 7);
    assert_eq!(snapshot.native_partition_residual, 0);
    assert_eq!(snapshot.instruction_minus_native, 9);

    hist.record_native_entry(
        3,
        0,
        0,
        1,
        Some(native_metadata(&vector, vector.len(), true)),
    );
    hist.record_native_entry(
        4,
        0,
        0,
        0,
        Some(native_metadata(&vector, vector.len(), false)),
    );
    let snapshot = hist.snapshot(&EPOCH1, 11);
    assert_eq!(snapshot.native_unresolved_instructions.linked_entry, 3);
    assert_eq!(snapshot.native_unresolved_entries.linked_entry, 1);
    assert_eq!(
        snapshot
            .native_unresolved_instructions
            .repeated_unlinked_entry,
        4
    );
    assert_eq!(
        snapshot.native_unresolved_entries.repeated_unlinked_entry,
        1
    );
    assert_eq!(snapshot.native_partition_residual, 0);
}

/// Recovered self-loop counts overlap the partition that already owns the same instructions.
#[cfg(feature = "timing-class-histogram")]
#[test]
fn native_histogram_recovers_only_proven_self_loop_multiplicities() {
    let mut hist = TimingHistogram::default();
    let vector = [
        TimingClass::Reg.index() as u8,
        TimingClass::AluRegMem.index() as u8,
        TimingClass::Jcc.index() as u8,
    ];
    hist.record_native_entry(
        8,
        0,
        0,
        0,
        Some(native_metadata(&vector, vector.len(), true)),
    );
    let snapshot = hist.snapshot(&EPOCH1, 8);
    let known: std::collections::HashMap<_, _> =
        snapshot.native_known_class_counts.iter().copied().collect();
    assert_eq!(known["Reg"], 3);
    assert_eq!(known["AluRegMem"], 3);
    assert_eq!(known["Jcc"], 2);
    assert_eq!(snapshot.self_loop_recovered_entries, 1);
    assert_eq!(snapshot.self_loop_recovered_instructions, 8);
    assert_eq!(
        snapshot
            .native_unresolved_instructions
            .repeated_unlinked_entry,
        0
    );
    assert_eq!(snapshot.native_partition_residual, 0);
    assert_eq!(
        snapshot.native_instructions + snapshot.self_loop_recovered_instructions,
        16,
        "recovered coverage overlaps native partition instructions and must not be summed into it"
    );

    let mut non_loop = TimingHistogram::default();
    non_loop.record_native_entry(
        8,
        0,
        0,
        0,
        Some(native_metadata(&vector, vector.len(), false)),
    );
    let non_loop_snapshot = non_loop.snapshot(&EPOCH1, 8);
    assert_eq!(
        non_loop_snapshot
            .native_unresolved_instructions
            .repeated_unlinked_entry,
        8
    );
    assert_eq!(non_loop_snapshot.self_loop_recovered_entries, 0);

    let mut short_or_zero = TimingHistogram::default();
    short_or_zero.record_native_entry(
        0,
        0,
        0,
        0,
        Some(native_metadata(&vector, vector.len(), true)),
    );
    short_or_zero.record_native_entry(
        2,
        0,
        0,
        0,
        Some(native_metadata(&vector, vector.len(), true)),
    );
    let short_or_zero_snapshot = short_or_zero.snapshot(&EPOCH1, 2);
    assert_eq!(short_or_zero_snapshot.self_loop_recovered_entries, 0);
    assert_eq!(short_or_zero_snapshot.self_loop_recovered_instructions, 0);
    assert_eq!(short_or_zero_snapshot.native_partition_residual, 0);
}

/// Call-outs and unknown slots remain visible and do not join interpreter charge
/// events or native class counts.
#[cfg(feature = "timing-class-histogram")]
#[test]
fn native_histogram_keeps_helper_and_unknown_slots_unresolved() {
    let mut hist = TimingHistogram::default();
    let vector = [
        TimingClass::Reg.index() as u8,
        CALL_OUT_SLOT,
        UNKNOWN_SLOT,
        TimingClass::X87RegArith.index() as u8,
    ];
    hist.record_native_entry(
        4,
        0,
        0,
        0,
        Some(native_metadata(&vector, vector.len(), false)),
    );
    hist.record(TimingClass::Reg);
    hist.record(TimingClass::Legacy(9));
    let snapshot = hist.snapshot(&EPOCH2_I586, 5);
    let known: std::collections::HashMap<_, _> =
        snapshot.native_known_class_counts.into_iter().collect();
    assert_eq!(known["Reg"], 1);
    assert_eq!(known["X87RegArith"], 1);
    assert_eq!(snapshot.native_unresolved_instructions.callout_slot, 1);
    assert_eq!(snapshot.native_unresolved_instructions.unknown_slot, 1);
    assert_eq!(snapshot.interpreter_unknown_class_events, 1);
    assert_eq!(snapshot.instruction_minus_native, 1);
    assert_eq!(snapshot.native_partition_residual, 0);
}

#[cfg(feature = "timing-class-histogram")]
#[test]
fn native_histogram_marks_missing_and_invalid_vectors_without_panicking() {
    let mut hist = TimingHistogram::default();
    hist.record_native_entry(0, 0, 0, 0, None);
    let short = [TimingClass::Reg.index() as u8];
    hist.record_native_entry(3, 0, 0, 0, Some(native_metadata(&short, 3, false)));
    let invalid = [0xfe, 0xfd];
    hist.record_native_entry(
        2,
        0,
        0,
        0,
        Some(native_metadata(&invalid, invalid.len(), false)),
    );
    let snapshot = hist.snapshot(&EPOCH1, 5);
    assert_eq!(snapshot.native_unresolved_entries.missing_vector, 1);
    assert_eq!(snapshot.native_unresolved_instructions.missing_vector, 0);
    assert_eq!(snapshot.native_unresolved_instructions.invalid_vector, 5);
    assert_eq!(snapshot.native_unresolved_entries.invalid_vector, 2);
    assert_eq!(snapshot.native_partition_residual, 0);
}

#[cfg(feature = "timing-class-histogram")]
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "native timing-class partition")]
fn native_histogram_partition_assertion_rejects_a_dropped_slot() {
    let hist = TimingHistogram {
        native_instructions: 1,
        ..Default::default()
    };
    hist.assert_native_partition();
}

/// Slice 8's mode-keyed system rows. Every one of them was a single flat
/// literal that covered two to four modes Intel prices apart, and every one of
/// them still charges that literal at epoch 1.
#[test]
fn the_system_event_rows_are_mode_keyed_and_epoch_one_flat() {
    use crate::execute_extended::{int_n_class, iret_class};

    // At epoch 1 every mode row carries the literal its one flat arm charged, so
    // the split is invisible to a knob-unset run. That is the merge bar.
    for class in [TimingClass::IntN, TimingClass::IntNPm, TimingClass::IntNV86] {
        assert_eq!(EPOCH1.raw(class), crate::INT_IMM8_CORE_CLOCKS);
    }
    for class in [
        TimingClass::Iret,
        TimingClass::IretPm,
        TimingClass::IretV86,
        TimingClass::IretPmToV86,
    ] {
        assert_eq!(EPOCH1.raw(class), 22);
    }
    for class in [
        TimingClass::CallFar,
        TimingClass::JmpFar,
        TimingClass::RetFar,
        TimingClass::FarTransferPm,
        TimingClass::FarTransferGate,
        TimingClass::FarTransferTss,
    ] {
        assert_eq!(EPOCH1.raw(class), 17);
    }
    assert_eq!(EPOCH1.raw(TimingClass::ExceptionDelivery), 59);
    assert_eq!(EPOCH1.raw(TimingClass::ExceptionDeliveryV86), 59);
    assert_eq!(EPOCH1.raw(TimingClass::HardwareInterrupt), 61);

    // At epoch 2 they separate, and in the direction the census measured.
    assert!(EPOCH2_I586.raw(TimingClass::IntNV86) > EPOCH2_I586.raw(TimingClass::IntN));
    assert!(EPOCH2_I586.raw(TimingClass::IretPmToV86) > EPOCH2_I586.raw(TimingClass::Iret));
    assert!(
        EPOCH2_I586.raw(TimingClass::FarTransferTss)
            > 7 * EPOCH2_I586.raw(TimingClass::FarTransferPm),
        "the census scores the TSS row at ~122x the flat 17 it rode"
    );
    // Census row 7: the V86 monitor trip, 16.7x under at epoch 1.
    let trip = f64::from(EPOCH2_I586.raw(TimingClass::ExceptionDeliveryV86))
        / f64::from(EPOCH1.raw(TimingClass::ExceptionDeliveryV86));
    assert!(
        (13.0..=14.0).contains(&trip),
        "the V86 trip moved by {trip:.1}x; the census predicted ~16.7x against a 59 that also \
         replaced the faulting instruction's charge, and slice 8 adds that charge back separately"
    );

    // The classifiers pick the documented rows.
    assert_eq!(int_n_class(false, false), TimingClass::IntN);
    assert_eq!(int_n_class(true, false), TimingClass::IntNPm);
    assert_eq!(int_n_class(true, true), TimingClass::IntNV86);
    assert_eq!(iret_class(false, false, 0, false, 0), TimingClass::Iret);
    assert_eq!(iret_class(true, true, 3, true, 3), TimingClass::IretV86);
    assert_eq!(
        iret_class(true, false, 0, true, 3),
        TimingClass::IretPmToV86
    );
    assert_eq!(
        iret_class(true, false, 0, false, 3),
        TimingClass::IretPmToV86
    );
    assert_eq!(iret_class(true, false, 0, false, 0), TimingClass::IretPm);
    assert_eq!(iret_class(true, false, 3, false, 3), TimingClass::IretPm);
}

/// The task-switch term is the one class with NO epoch-1 literal, because there
/// was no term at all: a switch rode whichever of 17 / 22 / 37 / 59 delivered
/// it. Charging zero at epoch 1 is what keeps the knob-unset identity bar.
#[test]
fn the_task_switch_term_is_new_and_free_at_epoch_one() {
    assert_eq!(EPOCH1.raw(TimingClass::TaskSwitch), 0);
    assert_eq!(EPOCH2_I586.raw(TimingClass::TaskSwitch), 2076);
    assert_eq!(EPOCH2_I486.raw(TimingClass::TaskSwitch), 2388);
    // It is the ONLY zero in the epoch-1 column; anything else at zero is a
    // class that lost its literal.
    let zeros: Vec<_> = TimingClass::ALL
        .iter()
        .filter(|class| EPOCH1.raw(**class) == 0)
        .map(|class| class.name())
        .collect();
    assert_eq!(zeros, vec!["TaskSwitch"]);
}

/// System events remain outside interpreter charge events and native entries.
#[cfg(feature = "timing-class-histogram")]
#[test]
fn system_events_do_not_enter_the_retire_counts() {
    let mut hist = TimingHistogram::default();
    hist.record(TimingClass::Reg);
    hist.record_system_event(TimingClass::ExceptionDeliveryV86);
    hist.record_system_event(TimingClass::ExceptionDeliveryV86);
    hist.record_system_event(TimingClass::TaskSwitch);

    let snapshot = hist.snapshot(&EPOCH2_I586, 1);
    assert_eq!(
        snapshot.interpreter_charge_counts,
        vec![("Reg", 1)],
        "a delivery is not an interpreter charge event"
    );
    assert_eq!(
        hist.class_clocks(&EPOCH2_I586),
        12,
        "one Reg, and nothing else"
    );
    let events: std::collections::HashMap<_, _> =
        snapshot.system_event_counts.into_iter().collect();
    assert_eq!(events["ExceptionDeliveryV86"], 2);
    assert_eq!(events["TaskSwitch"], 1);
    assert_eq!(
        snapshot.system_event_modeled_raw_clocks,
        2 * 792 + 2076,
        "two V86 trips and a task switch"
    );
}
