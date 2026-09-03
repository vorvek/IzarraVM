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
    (TimingClass::Loop, 11),
    (TimingClass::LoopCc, 11),
    (TimingClass::Jcxz, 9),
    (TimingClass::Nop, 3),
    (TimingClass::IntN, 37),
    (TimingClass::Int3, 33),
    (TimingClass::IntO, 35),
    (TimingClass::IntONotTaken, 3),
    (TimingClass::Iret, 22),
    (TimingClass::ShiftImm, 2),
    (TimingClass::ShiftOne, 2),
    (TimingClass::ShiftCl, 2),
    (TimingClass::DoubleShift, 3),
    (TimingClass::Group3Unsplit, 2),
    (TimingClass::IncDecRm, 2),
    (TimingClass::ImulRm, 9),
    (TimingClass::ImulImm, 14),
    (TimingClass::BitTest, 6),
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
    (TimingClass::SgdtSidt, 2),
    (TimingClass::LgdtLidt, 11),
    (TimingClass::Smsw, 3),
    (TimingClass::Lmsw, 11),
    (TimingClass::Invlpg, 12),
    (TimingClass::Clts, 2),
    (TimingClass::MovCrDr, 6),
    (TimingClass::Bound, 10),
    (TimingClass::Wrmsr, 30),
    (TimingClass::Rdtsc, 11),
    (TimingClass::Rdmsr, 11),
    (TimingClass::InvdWbinvd, 4),
    (TimingClass::Cpuid, 14),
    (TimingClass::StringElem, 4),
    (TimingClass::InsString, 15),
    (TimingClass::OutsString, 14),
    (TimingClass::InPort, 12),
    (TimingClass::OutPort, 10),
    (TimingClass::InPortDword, 12),
    (TimingClass::X87Wait, 6),
    (TimingClass::X87MemArith32, 20),
    (TimingClass::X87MemArith64, 20),
    (TimingClass::X87MemArithInt32, 20),
    (TimingClass::X87MemArithInt16, 20),
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
    (TimingClass::X87RegCompare, 5),
    (TimingClass::X87RegExchange, 4),
    (TimingClass::X87RegSign, 6),
    (TimingClass::X87RegConst, 8),
    (TimingClass::X87RegConstCheap, 4),
    (TimingClass::X87Exp, 200),
    (TimingClass::X87Transcendental, 300),
    (TimingClass::X87Sqrt, 70),
    (TimingClass::X87Rem, 100),
    (TimingClass::X87RoundInt, 20),
    (TimingClass::X87Scale, 30),
    (TimingClass::X87StackPointer, 4),
    (TimingClass::X87Control, 2),
    (TimingClass::X87StatusReg, 3),
    (TimingClass::X87RegStore, 4),
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

/// The unsourced census, pinned.
///
/// A class whose provenance says `UNSOURCED x12` has NO reference count behind
/// its epoch-2 entry: it is the epoch-1 literal times twelve, which preserves
/// today's relative cost and errs slow. That is a legitimate default under the
/// owner's 12:15 ruling and an illegitimate place to stop, so the count is
/// pinned: a later sub-slice that sources a row lowers this number, and one that
/// adds an unsourced row has to say so in the diff.
///
/// `PLACEHOLDER` is the stronger admission: the row is not merely unsourced, its
/// SHAPE is wrong (`Group3Unsplit` fuses `TEST` with `DIV`; `StringElem` fuses a
/// per-element cost with a setup cost) and a later sub-slice must split it.
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
        (78, 2),
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
/// The declared blends, all of them half- or third-clock:
/// * I586 `Jcc` raw 16 (1.33 clk, a third of a clock) -- design section 3.4: Intel's 1-clock
///   predicted cost plus an amortized mispredict, not any single count.
/// * I586 `Loop` / `LoopCc` / `Jcxz` raw 66 / 90 / 66 (5.5 / 7.5 / 5.5) --
///   section 3.2's midpoints of Intel's taken/not-taken ranges (5-6, 7-8, 6-5),
///   which the interpreter's one-arm charge cannot separate.
/// * I486 `ImulRm` / `ImulImm` raw 234 (19.5 clk) -- the audit's own midpoint of
///   the i486's 13-26 multiply range, which it prints as `~234` and tells us to
///   record as a range.
const DECLARED_BLENDS: &[(&str, &str)] = &[
    ("I486", "ImulRm"),
    ("I486", "ImulImm"),
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
