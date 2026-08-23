// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::{AddressSize, DecodeGroup, ModRm, OperandSize, Prefixes};

/// `addl ebp, imm32` as Doom's renderer patches it: 0x81 /0 with mod=3, a four-byte immediate and
/// no displacement, so the immediate occupies bytes 2..6 of a six-byte instruction.
fn add_ebp_imm32() -> DecodedInsn {
    DecodedInsn {
        len: 6,
        prefixes: Prefixes::default(),
        opcode: 0x81,
        operand_size: OperandSize::Dword,
        address_size: AddressSize::Dword,
        modrm: Some(ModRm {
            mode: 3,
            reg: 0,
            rm: 5,
        }),
        operand: None,
        imm: 0x1234_5678,
        imm2: 0,
        group: DecodeGroup::Group,
        continuable: true,
        disp_len: 0,
        imm_len: 4,
    }
}

#[test]
fn dword_patch_of_the_immediate_classifies_as_immediate_only() {
    let insn = add_ebp_imm32();
    assert_eq!(
        SmcTrace::classify(0x1_0002, 4, 0x1_0000, &insn),
        SmcFieldClass::ImmediateOnly
    );
}

#[test]
fn partial_patches_inside_the_immediate_still_classify_as_immediate_only() {
    let insn = add_ebp_imm32();
    for (offset, width) in [(2, 1), (3, 1), (2, 2), (4, 2), (5, 1)] {
        assert_eq!(
            SmcTrace::classify(0x1_0000 + offset, width, 0x1_0000, &insn),
            SmcFieldClass::ImmediateOnly,
            "offset {offset} width {width}"
        );
    }
}

#[test]
fn a_write_touching_opcode_or_modrm_is_structural() {
    let insn = add_ebp_imm32();
    for (offset, width) in [(0, 1), (1, 1), (0, 4), (1, 4)] {
        assert_eq!(
            SmcTrace::classify(0x1_0000 + offset, width, 0x1_0000, &insn),
            SmcFieldClass::Structural,
            "offset {offset} width {width}"
        );
    }
}

#[test]
fn a_write_running_off_the_end_is_structural() {
    let insn = add_ebp_imm32();
    assert_eq!(
        SmcTrace::classify(0x1_0004, 4, 0x1_0000, &insn),
        SmcFieldClass::Structural
    );
}

#[test]
fn a_displacement_only_write_is_reported_separately() {
    let mut insn = add_ebp_imm32();
    // `addl disp32(ebp), imm32`: mod=2 with a four-byte displacement before the immediate.
    insn.len = 10;
    insn.disp_len = 4;
    insn.modrm = Some(ModRm {
        mode: 2,
        reg: 0,
        rm: 5,
    });
    assert_eq!(
        SmcTrace::classify(0x1_0002, 4, 0x1_0000, &insn),
        SmcFieldClass::DisplacementOnly
    );
    assert_eq!(
        SmcTrace::classify(0x1_0006, 4, 0x1_0000, &insn),
        SmcFieldClass::ImmediateOnly
    );
    // Straddling the two fields is neither.
    assert_eq!(
        SmcTrace::classify(0x1_0004, 4, 0x1_0000, &insn),
        SmcFieldClass::Structural
    );
}

#[test]
fn a_write_with_no_covering_line_is_recorded_and_reported() {
    let mut trace = SmcTrace::default();
    trace.record(
        0x1_0002,
        4,
        SmcTracePre::new(None, 100),
        SmcTraceAction::default(),
    );
    let lines = trace.report_lines();
    assert!(lines[0].contains("events=1"), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|line| line.contains("smc_class no-line events=1")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("smc_site 0 1 no-line")),
        "{lines:?}"
    );
}

#[test]
fn repeated_patches_aggregate_hits_values_and_intervals() {
    let mut trace = SmcTrace::default();
    let action = SmcTraceAction {
        blocks_killed: 1,
        narrow_kills: 2,
        wholesale: false,
        newly_hot: 0,
    };
    let mut insn = add_ebp_imm32();
    for (index, instructions) in [1_000u64, 1_400, 2_600].into_iter().enumerate() {
        insn.imm = 0x100 + index as u32;
        trace.record(
            0x1_0002,
            4,
            SmcTracePre::new(Some((0x1_0000, insn)), instructions),
            action,
        );
    }
    // The same immediate twice must not inflate the distinct-value count.
    trace.record(
        0x1_0002,
        4,
        SmcTracePre::new(Some((0x1_0000, insn)), 2_700),
        action,
    );
    let lines = trace.report_lines();
    let site = lines
        .iter()
        .find(|line| line.starts_with("smc_site 0 "))
        .expect("the single site ranks first");
    let fields: Vec<&str> = site.split_whitespace().collect();
    // smc_site rank hits class write_phys width insn_phys insn_len opcode modrm operand_bytes
    // imm_len disp_len field_offset distinct_values ...
    assert_eq!(fields[2], "4", "hits");
    assert_eq!(fields[3], "imm", "class");
    assert_eq!(fields[8], "0x0081", "opcode");
    assert_eq!(fields[9], "mod3/reg0/rm5", "modrm");
    assert_eq!(fields[13], "2", "field_offset");
    assert_eq!(fields[14], "3", "distinct_values");
    assert_eq!(fields[15], "4", "blocks_killed");
    assert_eq!(fields[16], "8", "narrow_kills");
    assert_eq!(fields[19], "100", "min_interval");
    assert_eq!(fields[20], "1200", "max_interval");
    assert!(
        lines
            .iter()
            .any(|line| line.contains("smc_class imm events=4")),
        "{lines:?}"
    );
}

/// `mov [disp32], reg` at `opcode`, mod 0 reg 3 rm 5: six bytes, disp32 at offset 2, no
/// immediate. The shape all three `disp_store` census rows are read from.
fn mov_disp32(opcode: u16) -> DecodedInsn {
    DecodedInsn {
        len: 6,
        prefixes: Prefixes::default(),
        opcode,
        operand_size: OperandSize::Dword,
        address_size: AddressSize::Dword,
        modrm: Some(ModRm {
            mode: 0,
            reg: 3,
            rm: 5,
        }),
        operand: None,
        imm: 0,
        imm2: 0,
        group: DecodeGroup::Group,
        continuable: true,
        disp_len: 4,
        imm_len: 0,
    }
}

/// THE `disp_store` CENSUS ROW: per (opcode, modrm reg) events, block kills, narrow kills and --
/// the reason it exists -- `newly_hot` PER OPCODE.
///
/// Capture has until now been estimated as joined un-laned-disp crossings over the whole run's
/// `smc_heat_chunks_hot`, a denominator an already-laned family inflated 22% between two legs with
/// no change to the numerator. Attributing crossings per opcode removes that estimate, and this
/// fixture is what says the attribution is per opcode rather than per class.
#[test]
fn disp_store_rows_attribute_crossings_per_opcode() {
    let mut trace = SmcTrace::default();
    let action = SmcTraceAction {
        blocks_killed: 1,
        narrow_kills: 2,
        wholesale: false,
        newly_hot: 3,
    };
    // Two `0x89` writes and one `0x8B`, all displacement-field patches, plus one `0x81`
    // immediate patch that must appear in NO `disp_store` row.
    for (index, opcode) in [0x89u16, 0x89, 0x8b].into_iter().enumerate() {
        let base = 0x2_0000 + (index as u32) * 0x100;
        trace.record(
            base + 2,
            4,
            SmcTracePre::new(Some((base, mov_disp32(opcode))), 1_000),
            action,
        );
    }
    trace.record(
        0x3_0002,
        4,
        SmcTracePre::new(Some((0x3_0000, add_ebp_imm32())), 1_000),
        action,
    );
    let lines = trace.report_lines();
    let rows: Vec<&String> = lines
        .iter()
        .filter(|line| line.starts_with("smc_disp_store "))
        .collect();
    // The header plus exactly two data rows: `0x89` and `0x8B`. The `0x81` immediate patch is a
    // different class and must not have produced one.
    assert_eq!(rows.len(), 3, "{lines:?}");
    assert!(rows[0].contains("rank arm opcode"), "{rows:?}");
    // smc_disp_store rank arm opcode modrm_reg disp_len prefixes admissible events
    // blocks_killed narrow_kills newly_hot
    let first: Vec<&str> = rows[1].split_whitespace().collect();
    assert_eq!(first[2], "store", "the 0x89 row names the store arm");
    assert_eq!(first[3], "0x0089", "opcode");
    assert_eq!(first[4], "3", "modrm_reg, the smc_shape join key");
    assert_eq!(
        first[5], "4",
        "disp_len, in the key so disp8 gets its own cell"
    );
    assert_eq!(first[6], "none", "prefixes");
    assert_eq!(first[7], "yes", "admissible");
    assert_eq!(first[8], "2", "events");
    assert_eq!(first[9], "2", "blocks_killed");
    assert_eq!(first[10], "4", "narrow_kills");
    assert_eq!(
        first[11], "6",
        "newly_hot, the capture numerator, PER OPCODE"
    );
    let second: Vec<&str> = rows[2].split_whitespace().collect();
    assert_eq!(second[2], "load_widen");
    assert_eq!(second[3], "0x008b");
    assert_eq!(second[8], "1", "events");
    assert_eq!(second[11], "3", "newly_hot");
}

/// THE SUPERSET TRAP the key exists to close: a `0x89 /3` the store arm CANNOT admit — a disp8
/// form, and a `0x66`-prefixed disp32 one — must land in their own cells, marked inadmissible,
/// rather than inflating the disp32 cell's `newly_hot`.
///
/// Keyed on (opcode, modrm reg) alone all three writes below share one row and its capture
/// numerator reads 9 instead of the 3 the arm can actually take.
#[test]
fn inadmissible_disp_shapes_do_not_share_a_cell_with_the_admitted_one() {
    let mut trace = SmcTrace::default();
    let action = SmcTraceAction {
        blocks_killed: 0,
        narrow_kills: 0,
        wholesale: false,
        newly_hot: 3,
    };
    // The admitted shape.
    trace.record(
        0x5_0002,
        4,
        SmcTracePre::new(Some((0x5_0000, mov_disp32(0x89))), 1_000),
        action,
    );
    // Same opcode and same modrm reg, disp8: three bytes, the displacement last.
    let mut disp8 = mov_disp32(0x89);
    disp8.len = 3;
    disp8.disp_len = 1;
    disp8.modrm = Some(ModRm {
        mode: 1,
        reg: 3,
        rm: 5,
    });
    trace.record(
        0x5_0102,
        1,
        SmcTracePre::new(Some((0x5_0100, disp8)), 1_000),
        action,
    );
    // Same opcode, same modrm reg, disp32 — but behind a `0x66`.
    let mut prefixed = mov_disp32(0x89);
    prefixed.len = 7;
    prefixed.prefixes = Prefixes {
        operand_size_override: true,
        ..Prefixes::default()
    };
    trace.record(
        0x5_0203,
        4,
        SmcTracePre::new(Some((0x5_0200, prefixed)), 1_000),
        action,
    );
    let lines = trace.report_lines();
    let rows: Vec<Vec<&str>> = lines
        .iter()
        .filter(|line| line.starts_with("smc_disp_store ") && !line.contains("rank arm"))
        .map(|line| line.split_whitespace().collect())
        .collect();
    assert_eq!(rows.len(), 3, "three cells, not one: {lines:?}");
    let admitted: Vec<&Vec<&str>> = rows.iter().filter(|r| r[7] == "yes").collect();
    assert_eq!(
        admitted.len(),
        1,
        "exactly one cell is admissible: {rows:?}"
    );
    assert_eq!(admitted[0][5], "4", "the admissible cell is the disp32 one");
    assert_eq!(admitted[0][6], "none");
    assert_eq!(
        admitted[0][11], "3",
        "the capture numerator counts ONLY the shape the arm can take"
    );
    for row in rows.iter().filter(|r| r[7] == "no") {
        assert!(
            row[5] == "1" || row[6] == "other",
            "an inadmissible cell is the disp8 one or the prefixed one: {row:?}"
        );
        assert_eq!(row[11], "3");
    }
}

/// The shipped `0x8A` lane class rides in the same table as its own row, because its kill rate is
/// the control the two new arms have to be compared against. A row that could only see the new
/// arms could not make that comparison.
#[test]
fn the_shipped_8a_lane_class_has_its_own_disp_store_row() {
    let mut trace = SmcTrace::default();
    trace.record(
        0x4_0002,
        4,
        SmcTracePre::new(Some((0x4_0000, mov_disp32(0x8a))), 1_000),
        SmcTraceAction::default(),
    );
    assert!(
        trace
            .report_lines()
            .iter()
            .any(|line| line.starts_with("smc_disp_store 0 laned_8a 0x008a 3 4 none yes 1 ")),
        "{:?}",
        trace.report_lines()
    );
}

#[test]
fn a_disabled_trace_slot_is_transparent_to_cpu_equality_and_clone() {
    let enabled = SmcTraceSlot(Some(Box::new(SmcTrace::default())));
    let disabled = SmcTraceSlot::default();
    assert_eq!(enabled, disabled);
    assert!(enabled.clone().0.is_none(), "a clone starts disabled");
    assert!(format!("{disabled:?}").contains("enabled: false"));
}
