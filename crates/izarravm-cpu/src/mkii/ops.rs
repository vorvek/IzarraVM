// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use crate::timing_class::{ClassTable, TimingClass};
use crate::{AddrMode, BusWidth, DecodeGroup, DecodedInsn, DecodedOperand, SegmentIndex};
use izarravm_bus::CompiledBusDelta;

#[derive(Debug, Clone, Copy)]
pub(super) enum Input {
    Reg(u8),
    Immediate(u32),
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Pure {
    Mov {
        dst: u8,
        src: Input,
        width: BusWidth,
    },
    Alu {
        op: u8,
        dst: u8,
        src: Input,
        width: BusWidth,
        store: bool,
    },
    Nop,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Read {
    pub address: AddrMode,
    pub width: BusWidth,
    pub dst: u8,
    pub alu: Option<u8>,
    pub class: TimingClass,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct RegionCost {
    pub raw_core: u64,
    pub delta: CompiledBusDelta,
    pub reads: u64,
    pub carry_ops: u64,
}

#[derive(Debug)]
pub(super) struct Region {
    pub prefixes: Box<[RegionCost]>,
    pub segments: u8,
}

impl Region {
    pub fn build(table: &ClassTable, operations: &[Operation]) -> Self {
        let mut cost = RegionCost::default();
        let mut prefixes = vec![cost];
        let mut segments = 0;
        for op in operations {
            cost.carry_ops += u64::from(op.needs_carry_zero());
            cost.raw_core += u64::from(table.raw(op.region_class()));
            cost.delta.add_instruction_fetches(1);
            if let Some(read) = op.read {
                cost.delta.add_ram_accesses(read.width, 1);
                cost.reads += 1;
                segments |= 1 << read.address.segment.index();
            }
            prefixes.push(cost);
        }
        Self {
            prefixes: prefixes.into_boxed_slice(),
            segments,
        }
    }
}

#[derive(Debug)]
pub(super) struct Operation {
    pub eip: u32,
    pub physical: u32,
    pub insn: DecodedInsn,
    pub helper: usize,
    pub pure: Option<(Pure, TimingClass)>,
    pub guarded_pure: Option<(Pure, TimingClass)>,
    pub span_len: usize,
    pub memory_cmp_branch: bool,
    pub read: Option<Read>,
    pub region_len: usize,
    pub region: Option<Region>,
}

impl Operation {
    pub fn lower(eip: u32, physical: u32, insn: DecodedInsn) -> Option<Self> {
        if insn.prefixes.lock || insn.prefixes.rep.is_some() {
            return None;
        }
        let lowered = lower_pure(&insn);
        let (pure, guarded_pure) = if matches!(lowered, Some((Pure::Alu { op: 2 | 3, .. }, _))) {
            (None, lowered)
        } else {
            (lowered, None)
        };
        let helper = if pure.is_some() {
            0
        } else {
            match insn.group {
                DecodeGroup::Alu => 1,
                DecodeGroup::DataMove => 2,
                DecodeGroup::Stack => 3,
                DecodeGroup::Group => 4,
                DecodeGroup::Branch => 5,
                DecodeGroup::FlagsMisc => 6,
                DecodeGroup::PortIo if insn.opcode == 0xec => 6,
                DecodeGroup::SystemSeg => 7,
                DecodeGroup::ControlFlow => 8,
                DecodeGroup::BitManip => 9,
                DecodeGroup::CondMove => 10,
                _ => return None,
            }
        };
        Some(Self {
            eip,
            physical,
            insn,
            helper,
            pure,
            guarded_pure,
            span_len: 1,
            memory_cmp_branch: false,
            read: lower_read(&insn),
            region_len: 0,
            region: None,
        })
    }

    pub fn region_class(&self) -> TimingClass {
        self.region_pure()
            .map(|(_, class)| class)
            .or(self.read.map(|read| read.class))
            .unwrap_or(TimingClass::Jcc)
    }

    pub fn region_pure(&self) -> Option<(Pure, TimingClass)> {
        self.pure.or(self.guarded_pure)
    }

    pub fn needs_carry_zero(&self) -> bool {
        self.guarded_pure.is_some()
            || self
                .read
                .is_some_and(|read| matches!(read.alu, Some(2 | 3)))
    }

    pub fn branch_alu(&self) -> Option<(u8, BusWidth)> {
        if let Some(read) = self.read
            && let Some(alu) = read.alu
        {
            return Some((alu, read.width));
        }
        if let Some((Pure::Alu { op, width, .. }, _)) = self.region_pure() {
            return Some((op, width));
        }
        None
    }
}

fn lower_read(insn: &DecodedInsn) -> Option<Read> {
    let opcode = insn.opcode;
    let (dst, alu, class) = match opcode {
        0x8a | 0x8b => (insn.modrm?.reg, None, TimingClass::MovRegMem),
        0xa0 | 0xa1 => (0, None, TimingClass::MovAccMoffs),
        0x02 | 0x03 | 0x12 | 0x13 | 0x1a | 0x1b | 0x2a | 0x2b | 0x3a | 0x3b => (
            insn.modrm?.reg,
            Some((opcode as u8 >> 3) & 7),
            TimingClass::AluRegMem,
        ),
        _ => return None,
    };
    let address = if matches!(opcode, 0xa0 | 0xa1) {
        AddrMode {
            segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
            base: None,
            index: None,
            scale: 1,
            disp: insn.imm as i32,
            address_size: insn.address_size,
        }
    } else if let Some(DecodedOperand::Mem(address)) = insn.operand {
        address
    } else {
        return None;
    };
    Some(Read {
        address,
        dst,
        alu,
        class,
        width: if opcode & 1 == 0 {
            BusWidth::Byte
        } else {
            insn.operand_size.bus_width()
        },
    })
}

fn lower_pure(insn: &DecodedInsn) -> Option<(Pure, TimingClass)> {
    // Poll recognition still needs these TESTs and their following branches in the decode cache.
    if insn.opcode == 0xa8
        || (insn.opcode == 0x84
            && insn.modrm.is_some_and(|modrm| modrm.reg == 4)
            && matches!(insn.operand, Some(DecodedOperand::Reg(0))))
    {
        return None;
    }
    let sized = insn.operand_size.bus_width();
    let reg = match insn.operand {
        Some(DecodedOperand::Reg(reg)) => Some(reg),
        _ => None,
    };
    let opcode = insn.opcode;
    let mov = match opcode {
        0xb0..=0xb7 => Some((
            opcode as u8 - 0xb0,
            Input::Immediate(insn.imm),
            BusWidth::Byte,
            TimingClass::MovImmReg,
        )),
        0xb8..=0xbf => Some((
            opcode as u8 - 0xb8,
            Input::Immediate(insn.imm),
            sized,
            TimingClass::MovImmReg,
        )),
        0x88 | 0x89 => Some((
            reg?,
            Input::Reg(insn.modrm?.reg),
            if opcode == 0x88 {
                BusWidth::Byte
            } else {
                sized
            },
            TimingClass::MovMemReg,
        )),
        0x8a | 0x8b => Some((
            insn.modrm?.reg,
            Input::Reg(reg?),
            if opcode == 0x8a {
                BusWidth::Byte
            } else {
                sized
            },
            TimingClass::MovRegMem,
        )),
        0xc6 | 0xc7 if insn.modrm?.reg == 0 => Some((
            reg?,
            Input::Immediate(insn.imm),
            if opcode == 0xc6 {
                BusWidth::Byte
            } else {
                sized
            },
            TimingClass::MovImmMem,
        )),
        _ => None,
    };
    if let Some((dst, src, width, class)) = mov {
        return Some((Pure::Mov { dst, src, width }, class));
    }
    if opcode == 0x90 {
        return Some((Pure::Nop, TimingClass::Nop));
    }
    let (op, dst, src, width, store, class) = if opcode < 0x40 && opcode & 7 < 6 {
        let op = (opcode as u8 >> 3) & 7;
        let (dst, src) = match opcode & 7 {
            0 | 1 => (reg?, Input::Reg(insn.modrm?.reg)),
            2 | 3 => (insn.modrm?.reg, Input::Reg(reg?)),
            4 | 5 => (0, Input::Immediate(insn.imm)),
            _ => return None,
        };
        (
            op,
            dst,
            src,
            if opcode & 1 == 0 {
                BusWidth::Byte
            } else {
                sized
            },
            op != 7,
            TimingClass::Reg,
        )
    } else if matches!(opcode, 0x80..=0x83) {
        let op = insn.modrm?.reg;
        (
            op,
            reg?,
            Input::Immediate(insn.imm),
            if matches!(opcode, 0x80 | 0x82) {
                BusWidth::Byte
            } else {
                sized
            },
            op != 7,
            TimingClass::Reg,
        )
    } else if matches!(opcode, 0x84 | 0x85 | 0xa8 | 0xa9) {
        let (dst, src) = if opcode < 0x90 {
            (reg?, Input::Reg(insn.modrm?.reg))
        } else {
            (0, Input::Immediate(insn.imm))
        };
        (
            4,
            dst,
            src,
            if opcode & 1 == 0 {
                BusWidth::Byte
            } else {
                sized
            },
            false,
            if opcode < 0x90 {
                TimingClass::Reg
            } else {
                TimingClass::TestImmReg
            },
        )
    } else {
        return None;
    };
    Some((
        Pure::Alu {
            op,
            dst,
            src,
            width,
            store,
        },
        class,
    ))
}
