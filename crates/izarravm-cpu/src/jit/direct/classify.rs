// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

pub(super) fn classify(insn: &DecodedInsn, lin: u32, entry_lin: u32) -> Option<DirectKind> {
    if insn.group == DecodeGroup::Fpu {
        if insn.operand_size != OperandSize::Dword {
            return None;
        }
        let native = NativeX87Insn::classify(insn)?;
        let addr = match native {
            NativeX87Insn::BinaryMemory { addr, .. }
            | NativeX87Insn::LoadF32 { addr }
            | NativeX87Insn::StoreF32 { addr, .. }
            | NativeX87Insn::LoadI32 { addr }
            | NativeX87Insn::StoreI32 { addr } => Some(direct_addr(addr)?),
            _ => None,
        };
        return Some(DirectKind::X87 { insn: native, addr });
    }
    let operand_width = match insn.operand_size {
        OperandSize::Word => MemoryWidth::Word,
        OperandSize::Dword => MemoryWidth::Dword,
    };
    if insn.operand_size == OperandSize::Word
        && !matches!(insn.opcode, 0x39 | 0x3b | 0x40..=0x4f | 0x89 | 0x8b | 0xff)
    {
        return None;
    }
    // IMUL r32, r/m32, register form only. Must stay below the Word-size gate above: a
    // 66-prefixed IMUL decodes with OperandSize::Word and is not in that gate's allowlist, so it
    // already falls through to `None` there. Moving this arm above the gate, or adding 0x0faf to
    // the allowlist, would silently lower a 16-bit IMUL as a 32-bit multiply instead: the
    // destination's high 16 bits would be clobbered rather than preserved, and CF/OF would be
    // computed against the wrong width.
    if insn.opcode == 0x0faf {
        // Keyed on the full u16 opcode rather than the u8 truncation further down: that
        // truncation (`u8::try_from(insn.opcode).ok()`) returns None for every two-byte opcode,
        // so the u8 arms below are unreachable for 0x0faf regardless. Matching the full u16 here
        // keeps that explicit and local instead of relying on the truncation's behavior.
        let m = insn.modrm?;
        let DecodedOperand::Reg(src) = insn.operand? else {
            return None;
        };
        return Some(DirectKind::Imul { dst: m.reg, src });
    }
    if matches!(insn.opcode, 0x0fa4 | 0x0fa5 | 0x0fac | 0x0fad) {
        let m = insn.modrm?;
        let count = if matches!(insn.opcode, 0x0fa4 | 0x0fac) {
            ShiftCount::Immediate(insn.imm as u8)
        } else {
            ShiftCount::Cl
        };
        let left = matches!(insn.opcode, 0x0fa4 | 0x0fa5);
        return match insn.operand? {
            DecodedOperand::Reg(dst) => Some(DirectKind::DoubleShiftReg {
                left,
                dst,
                src: m.reg,
                count,
            }),
            DecodedOperand::Mem(addr) => Some(DirectKind::DoubleShiftMem {
                left,
                src: m.reg,
                count,
                addr: direct_addr(addr)?,
            }),
        };
    }
    let opcode = u8::try_from(insn.opcode).ok();
    if let Some(opcode) = opcode {
        if opcode < 0x40 {
            let op = (opcode >> 3) & 7;
            let form = opcode & 7;
            match form {
                1 => {
                    let m = insn.modrm?;
                    return match insn.operand? {
                        DecodedOperand::Reg(dst) => Some(DirectKind::AluReg {
                            op,
                            dst,
                            src: m.reg,
                            width: operand_width,
                        }),
                        DecodedOperand::Mem(addr) => Some(DirectKind::AluMemDest {
                            op,
                            source: StoreSource::Reg(m.reg),
                            width: operand_width,
                            addr: direct_addr(addr)?,
                        }),
                    };
                }
                3 => {
                    let m = insn.modrm?;
                    return match insn.operand? {
                        DecodedOperand::Reg(src) => Some(DirectKind::AluReg {
                            op,
                            dst: m.reg,
                            src,
                            width: operand_width,
                        }),
                        DecodedOperand::Mem(addr) => Some(DirectKind::AluMemSource {
                            op,
                            dst: m.reg,
                            width: operand_width,
                            addr: direct_addr(addr)?,
                        }),
                    };
                }
                5 => {
                    return Some(DirectKind::AluImm {
                        op,
                        dst: 0,
                        imm: insn.imm,
                    });
                }
                _ => {}
            }
        }
        match opcode {
            0x40..=0x4f => {
                return Some(DirectKind::IncDecReg {
                    dst: opcode & 7,
                    is_dec: opcode >= 0x48,
                    width: operand_width,
                });
            }
            0x50..=0x57 => {
                return Some(DirectKind::Push {
                    source: StoreSource::Reg(opcode - 0x50),
                });
            }
            0x58..=0x5f => {
                return Some(DirectKind::Pop { dst: opcode - 0x58 });
            }
            0x68 => {
                return Some(DirectKind::Push {
                    source: StoreSource::Imm(insn.imm),
                });
            }
            0x6a => {
                return Some(DirectKind::Push {
                    source: StoreSource::Imm(crate::sign_extend_u8(insn.imm as u8)),
                });
            }
            0x80 => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::AluByteImm {
                        op: m.reg,
                        dst,
                        imm: insn.imm as u8,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::AluMemDest {
                        op: m.reg,
                        source: StoreSource::Imm(insn.imm),
                        width: MemoryWidth::Byte,
                        addr: direct_addr(addr)?,
                    }),
                };
            }
            0x81 | 0x83 => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::AluImm {
                        op: m.reg,
                        dst,
                        imm: insn.imm,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::AluMemDest {
                        op: m.reg,
                        source: StoreSource::Imm(insn.imm),
                        width: MemoryWidth::Dword,
                        addr: direct_addr(addr)?,
                    }),
                };
            }
            0x84 => {
                let m = insn.modrm?;
                let DecodedOperand::Reg(a) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::TestByte { a, b: m.reg });
            }
            0x85 => {
                let m = insn.modrm?;
                let DecodedOperand::Reg(a) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::Test { a, b: m.reg });
            }
            0xa8 => {
                return Some(DirectKind::TestImmReg {
                    dst: 0,
                    imm: insn.imm,
                    width: MemoryWidth::Byte,
                });
            }
            0xa9 => {
                return Some(DirectKind::TestImmReg {
                    dst: 0,
                    imm: insn.imm,
                    width: MemoryWidth::Dword,
                });
            }
            0x88 => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::MovRegByte { dst, src: m.reg }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Store {
                        source: StoreSource::Reg(m.reg),
                        width: MemoryWidth::Byte,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0x89 => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::MovReg {
                        dst,
                        src: m.reg,
                        width: operand_width,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Store {
                        source: StoreSource::Reg(m.reg),
                        width: operand_width,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0x8a => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(src) => Some(DirectKind::MovRegByte { dst: m.reg, src }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Load {
                        dst: m.reg,
                        width: MemoryWidth::Byte,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0x8b => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(src) => Some(DirectKind::MovReg {
                        dst: m.reg,
                        src,
                        width: operand_width,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Load {
                        dst: m.reg,
                        width: operand_width,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0x8d => {
                let m = insn.modrm?;
                let DecodedOperand::Mem(addr) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::Lea {
                    dst: m.reg,
                    addr: direct_addr(addr)?,
                });
            }
            0xa0 => {
                return Some(DirectKind::Load {
                    dst: 0,
                    width: MemoryWidth::Byte,
                    addr: DirectAddr {
                        segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                        base: None,
                        index: None,
                        scale: 1,
                        disp: insn.imm,
                    },
                    raw_clocks: 4,
                });
            }
            0xa1 => {
                return Some(DirectKind::Load {
                    dst: 0,
                    width: MemoryWidth::Dword,
                    addr: DirectAddr {
                        segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                        base: None,
                        index: None,
                        scale: 1,
                        disp: insn.imm,
                    },
                    raw_clocks: 4,
                });
            }
            0xa2 => {
                return Some(DirectKind::Store {
                    source: StoreSource::Reg(0),
                    width: MemoryWidth::Byte,
                    addr: DirectAddr {
                        segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                        base: None,
                        index: None,
                        scale: 1,
                        disp: insn.imm,
                    },
                    raw_clocks: 4,
                });
            }
            0xa3 => {
                return Some(DirectKind::Store {
                    source: StoreSource::Reg(0),
                    width: MemoryWidth::Dword,
                    addr: DirectAddr {
                        segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                        base: None,
                        index: None,
                        scale: 1,
                        disp: insn.imm,
                    },
                    raw_clocks: 4,
                });
            }
            0xb0..=0xb7 => {
                return Some(DirectKind::MovImmByte {
                    dst: opcode - 0xb0,
                    imm: insn.imm as u8,
                });
            }
            0xb8..=0xbf => {
                return Some(DirectKind::MovImm {
                    dst: opcode - 0xb8,
                    imm: insn.imm,
                });
            }
            0xc6 | 0xc7 => {
                let m = insn.modrm?;
                if m.reg != 0 {
                    return None;
                }
                let width = if opcode == 0xc6 {
                    MemoryWidth::Byte
                } else {
                    MemoryWidth::Dword
                };
                return match insn.operand? {
                    DecodedOperand::Reg(dst) if opcode == 0xc6 => Some(DirectKind::MovImmByte {
                        dst,
                        imm: insn.imm as u8,
                    }),
                    DecodedOperand::Reg(dst) => Some(DirectKind::MovImm { dst, imm: insn.imm }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Store {
                        source: StoreSource::Imm(insn.imm),
                        width,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0xc1 | 0xd1 => {
                let m = insn.modrm?;
                // reg 1 is ROR and MUST be admitted by this guard, not appended after it: reg 1
                // fails `matches!(m.reg, 4..=7)`, so a rotate arm placed below the guard would be
                // unreachable and the whole lowering would be dead code that no negative test
                // could detect. ROL (/0), RCL (/2) and RCR (/3) stay out. The refreshed attribution
                // measures zero rejects for all three, and RCL and RCR additionally take the
                // incoming CF as a rotate INPUT (`shift_rotate` seeds `cf` from `flag(FLAG_CF)`
                // before its loop), which would need the flags loaded into the host before the
                // rotate rather than only captured after it.
                if !matches!(m.reg, 1 | 4..=7) {
                    return None;
                }
                let DecodedOperand::Reg(dst) = insn.operand? else {
                    return None;
                };
                // The RAW immediate, unmasked, matching what the Shift arm has always stored. The
                // architectural five-bit mask is applied in the emitter.
                let count = if opcode == 0xd1 { 1 } else { insn.imm as u8 };
                if m.reg == 1 {
                    return Some(DirectKind::RotateRightReg { dst, count });
                }
                return Some(DirectKind::Shift {
                    op: m.reg,
                    dst,
                    count,
                });
            }
            0xf6 | 0xf7 => {
                let m = insn.modrm?;
                // NEG r/m32, register form. Deliberately carries NO width field: this arm sits
                // below the OperandSize::Word gate at the top of `classify`, and that allowlist
                // (which does not contain 0xf7) is the ONLY thing stopping a 586-mode `66 F7 /3`
                // from reaching here, since the persona gate admits word ops on 586 and
                // `prefixes_supported` accepts the operand-size override. A `width` field would
                // invite a future edit to pass `operand_width`, which is MemoryWidth::Word in
                // exactly that case, and a 16-bit NEG would then be lowered as a 32-bit one,
                // clobbering the destination's high half. Same hazard the 0x0faf comment above
                // describes. In a 16-bit segment the unprefixed form is Word (gated the same way)
                // and the 66-prefixed form is rejected earlier for carrying a prefix at all, so
                // NEG is simply never lowered there.
                if opcode == 0xf7 && m.reg == 3 {
                    let DecodedOperand::Reg(dst) = insn.operand? else {
                        return None;
                    };
                    return Some(DirectKind::NegReg { dst });
                }
                if m.reg != 0 {
                    return None;
                }
                let width = if opcode == 0xf6 {
                    MemoryWidth::Byte
                } else {
                    MemoryWidth::Dword
                };
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::TestImmReg {
                        dst,
                        imm: insn.imm,
                        width,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::TestImmMem {
                        imm: insn.imm,
                        width,
                        addr: direct_addr(addr)?,
                    }),
                };
            }
            0xc2 | 0xc3 => {
                if !matches!(operand_width, MemoryWidth::Dword) {
                    return None;
                }
                return Some(DirectKind::Ret {
                    release: if opcode == 0xc2 { insn.imm as u16 } else { 0 },
                });
            }
            0xff => {
                let m = insn.modrm?;
                if !matches!(m.reg, 0 | 1) {
                    return None;
                }
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::IncDecReg {
                        dst,
                        is_dec: m.reg == 1,
                        width: operand_width,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::RmwIncDec {
                        is_dec: m.reg == 1,
                        width: operand_width,
                        addr: direct_addr(addr)?,
                    }),
                };
            }
            0x70..=0x7f if insn.group == DecodeGroup::Branch => {
                let end_delta = lin
                    .wrapping_add(u32::from(insn.len))
                    .wrapping_sub(entry_lin);
                return Some(DirectKind::Jcc {
                    condition: opcode & 0x0f,
                    taken_delta: end_delta.wrapping_add(insn.imm),
                });
            }
            0xe8 if insn.group == DecodeGroup::Branch => {
                let return_delta = lin
                    .wrapping_add(u32::from(insn.len))
                    .wrapping_sub(entry_lin);
                return Some(DirectKind::Call {
                    return_delta,
                    target_delta: return_delta.wrapping_add(insn.imm),
                });
            }
            0xe9 | 0xeb if insn.group == DecodeGroup::Branch => {
                let end_delta = lin
                    .wrapping_add(u32::from(insn.len))
                    .wrapping_sub(entry_lin);
                return Some(DirectKind::Jmp {
                    target_delta: end_delta.wrapping_add(insn.imm),
                });
            }
            _ => {}
        }
    }
    if matches!(insn.opcode, 0x0f80..=0x0f8f) && insn.group == DecodeGroup::Branch {
        let end_delta = lin
            .wrapping_add(u32::from(insn.len))
            .wrapping_sub(entry_lin);
        return Some(DirectKind::Jcc {
            condition: (insn.opcode & 0x0f) as u8,
            taken_delta: end_delta.wrapping_add(insn.imm),
        });
    }
    None
}

fn direct_addr(addr: crate::AddrMode) -> Option<DirectAddr> {
    if addr.address_size != AddressSize::Dword || !matches!(addr.scale, 1 | 2 | 4 | 8) {
        return None;
    }
    Some(DirectAddr {
        segment: addr.segment,
        base: addr.base,
        index: addr.index,
        scale: addr.scale,
        disp: addr.disp as u32,
    })
}
