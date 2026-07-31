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

#[test]
fn a_disabled_trace_slot_is_transparent_to_cpu_equality_and_clone() {
    let enabled = SmcTraceSlot(Some(Box::new(SmcTrace::default())));
    let disabled = SmcTraceSlot::default();
    assert_eq!(enabled, disabled);
    assert!(enabled.clone().0.is_none(), "a clone starts disabled");
    assert!(format!("{disabled:?}").contains("enabled: false"));
}
