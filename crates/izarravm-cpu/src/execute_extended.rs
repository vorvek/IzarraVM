// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::timing_class::TimingClass;

/// The SECOND word of a far-pointer memory operand -- LES/LDS/LSS/LFS/LGS's selector, which sits
/// `operand_size_bytes` past the offset word this instruction family reads first.
///
/// WRAPS MOD 64K at `AddressSize::Word`, rather than adding into a flat `u32` offset. 86Box's
/// `opLDS_w_a16` (`src/cpu/x86_ops_mov_seg.h`) is the citation: `cpu_state.eaa16[0]` is a
/// `uint16_t`, so `cpu_state.eaa16[0] += 2` wraps in C's unsigned 16-bit arithmetic, and the
/// following `CHECK_READ` bounds it with `& 0xffff` too. A 16-bit EA of `0xFFFE` therefore reads
/// its selector back at `0x0000`, not at the linear `0x10000` a bare `u32` add would reach --
/// `resolve_memory_addr_mode`'s own Word arm masks the FIRST word's EA the same way
/// (`(sum as u16) as u32`), so the second word has to be masked too or the two halves of one
/// instruction disagree about how their segment wraps. `AddressSize::Dword`'s 32-bit forms
/// (`opLDS_w_a32` et al.) add without any such mask, because their EA register (`eaaddr`) is a
/// `u32` and the whole segment is flat -- which is why this only special-cases `Word`.
///
/// The far CALL/JMP-through-memory arm (`0xFF /3`, `/5`) already got this right, independently,
/// against a SingleStepTests 80386 conformance vector (FF.3 "call far [ds:bx+di]" with
/// `bx=di=0xffff`; `far_call_via_memory_wraps_selector_offset_at_64k` pins it). This function
/// dedupes what was a second hand-written copy of that same arithmetic and gives LES/LDS/LSS/
/// LFS/LGS the fix that arm already had.
fn far_pointer_second_word_offset(
    address_size: AddressSize,
    first_word_offset: u32,
    operand_size_bytes: u32,
) -> u32 {
    match address_size {
        AddressSize::Word => {
            u32::from((first_word_offset as u16).wrapping_add(operand_size_bytes as u16))
        }
        AddressSize::Dword => first_word_offset.wrapping_add(operand_size_bytes),
    }
}

fn bit_op(op: u8, value: u32, bit: u32) -> (bool, u32) {
    // op: 0=BT, 1=BTS, 2=BTR, 3=BTC. `bit` is already reduced to 0..bits-1 (the caller
    // masks to the operand width, so 0..15 for a word and 0..31 for a dword).
    let mask = 1u32 << bit;
    let cf = value & mask != 0;
    let new = match op {
        0 => value,         // BT: read-only
        1 => value | mask,  // BTS
        2 => value & !mask, // BTR
        3 => value ^ mask,  // BTC
        _ => unreachable!("bit op {op}"),
    };
    (cf, new)
}

impl CpuGsw {
    fn near_return<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand_size: OperandSize,
        release: u16,
    ) -> ExecResult<()> {
        let stack_offset = if self.stack_is_32bit() {
            self.registers.esp()
        } else {
            u32::from(self.read_gpr16(4))
        };
        let target = self.read_memory_sized(
            bus,
            SegmentIndex::Ss,
            stack_offset,
            operand_size,
            BusAccessKind::DataRead,
        )? & operand_size.mask();
        if target > self.registers.cs().limit {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(0),
            });
        }
        self.release_stack(operand_size.bytes() as u16);
        self.set_eip(target);
        self.release_stack(release);
        Ok(())
    }

    /// The two-byte bit-manipulation block (BT/BTS/BTR/BTC reg+imm8, BSF/BSR, SHLD/SHRD, CMPXCHG,
    /// XADD) through the decode/execute split. Each arm mirrors the former `execute_two_byte`
    /// handler verbatim — same operand wiring, same read/write order, same clocks — but consumes the
    /// ModRM/operand `decode` pre-parsed and the imm8 `decode` pre-fetched (for 0F BA/A4/AC). Memory
    /// operands resolve from the pre-decoded descriptor, so the effective address is recomputed
    /// against the live registers each call; for the BT-memory reg form the live reg bit index can
    /// walk the address past the operand width, which `bit_string_op` handles unchanged. Dispatch is
    /// off the FULL u16 `insn.opcode` because the `as u8` low byte of 0x0Fa4/a5/b0/b1/c0/c1 aliases
    /// single-byte opcodes; the second 0F byte is never re-read and the ISA gate is never re-applied
    /// (both already done once in `decode`).
    pub(super) fn execute_bitmanip_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        let operand_size = insn.operand_size;
        let address_size = insn.address_size;

        match insn.opcode {
            0x0fbc => {
                // BSF: index of the lowest set bit. Source 0 -> ZF=1, destination unchanged
                // (386 silicon; Intel documents the destination as undefined).
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let src =
                    self.read_operand_sized(bus, operand, operand_size)? & operand_size.mask();
                if src == 0 {
                    self.set_flag(FLAG_ZF, true);
                } else {
                    self.set_flag(FLAG_ZF, false);
                    self.write_gpr_sized(modrm.reg, operand_size, src.trailing_zeros());
                }
                Ok(self.charge(TimingClass::BitScan))
            }
            0x0fbd => {
                // BSR: index of the highest set bit. Source 0 -> ZF=1, destination unchanged
                // (386 silicon; Intel documents the destination as undefined).
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let src =
                    self.read_operand_sized(bus, operand, operand_size)? & operand_size.mask();
                if src == 0 {
                    self.set_flag(FLAG_ZF, true);
                } else {
                    self.set_flag(FLAG_ZF, false);
                    self.write_gpr_sized(modrm.reg, operand_size, 31 - src.leading_zeros());
                }
                Ok(self.charge(TimingClass::BitScan))
            }
            0x0fa3 | 0x0fab | 0x0fb3 | 0x0fbb => {
                // BT/BTS/BTR/BTC r/m, r. The opcodes are 8 apart: A3=BT, AB=BTS, B3=BTR, BB=BTC.
                // The bit index in the reg operand is signed for a memory operand; the adjusted
                // address is computed inside `bit_string_op` from the live reg index (register_index
                // = true), never pre-resolved at decode.
                let op = ((insn.opcode as u8) - 0xa3) / 8;
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let index = self.read_gpr_sized(modrm.reg, operand_size);
                self.bit_string_op(bus, op, operand, index, operand_size, address_size, true)?;
                Ok(self.charge(bit_string_class(op)))
            }
            0x0fba => {
                // BT/BTS/BTR/BTC r/m, imm8: /4=BT, /5=BTS, /6=BTR, /7=BTC. The imm8 was fetched by
                // `decode` (after the ModRM+displacement) into `insn.imm`. /0../3 are not defined
                // bit-test ops and #UD before the operation runs (matching the fused handler, which
                // resolved the operand and read the imm8 first, then faulted on the bad /ext).
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                if modrm.reg < 4 {
                    return Err(InternalFault::Exception {
                        vector: 6,
                        error_code: None,
                    });
                }
                let op = modrm.reg - 4;
                self.bit_string_op(
                    bus,
                    op,
                    operand,
                    insn.imm,
                    operand_size,
                    address_size,
                    false,
                )?;
                Ok(self.charge(bit_string_class(op)))
            }
            0x0fa4 | 0x0fac => {
                // SHLD (A4) / SHRD (AC) r/m, r, imm8. The imm8 count was fetched by `decode` into
                // `insn.imm`. Read order (src reg, then dest r/m) matches the fused handler.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let src = self.read_gpr_sized(modrm.reg, operand_size);
                let count = insn.imm as u8;
                let dest = self.read_operand_sized(bus, operand, operand_size)?;
                let result =
                    self.double_shift(insn.opcode == 0x0fa4, dest, src, count, operand_size);
                self.write_operand_sized(bus, operand, operand_size, result)?;
                Ok(self.charge(TimingClass::DoubleShift))
            }
            0x0fa5 | 0x0fad => {
                // SHLD (A5) / SHRD (AD) r/m, r, CL. No immediate — the count is the low byte of CL.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let src = self.read_gpr_sized(modrm.reg, operand_size);
                let count = (self.registers.ecx() & 0xff) as u8;
                let dest = self.read_operand_sized(bus, operand, operand_size)?;
                let result =
                    self.double_shift(insn.opcode == 0x0fa5, dest, src, count, operand_size);
                self.write_operand_sized(bus, operand, operand_size, result)?;
                Ok(self.charge(TimingClass::DoubleShift))
            }
            0x0fb0 | 0x0fb1 => {
                // CMPXCHG r/m, r. B0 is the byte form, B1 the word/dword form. Compare the
                // accumulator (AL/AX/EAX) with the destination exactly like CMP (acc - dest),
                // setting every ALU flag from that subtraction. If they are equal (ZF set after
                // the compare) the source register is stored into the destination; otherwise the
                // destination value is loaded into the accumulator. Either way the destination is
                // written once, which is what makes the LOCK form meaningful.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let size = if insn.opcode == 0x0fb0 {
                    None
                } else {
                    Some(operand_size)
                };
                match size {
                    None => {
                        let dest = self.read_operand_u8(bus, operand)?;
                        let acc = self.read_gpr8(0);
                        self.alu_sub(u32::from(acc), u32::from(dest), 0, BusWidth::Byte);
                        if self.flag(FLAG_ZF) {
                            let src = self.read_gpr8(modrm.reg);
                            self.write_operand_u8(bus, operand, src)?;
                        } else {
                            self.write_gpr8(0, dest);
                            // Re-write the destination with its own value so the bus sees a write
                            // even on the unequal branch, matching the architectural read-modify-
                            // write of CMPXCHG.
                            self.write_operand_u8(bus, operand, dest)?;
                        }
                    }
                    Some(size) => {
                        let dest = self.read_operand_sized(bus, operand, size)?;
                        let acc = self.read_gpr_sized(0, size);
                        self.alu_sub(acc, dest, 0, size.bus_width());
                        if self.flag(FLAG_ZF) {
                            let src = self.read_gpr_sized(modrm.reg, size);
                            self.write_operand_sized(bus, operand, size, src)?;
                        } else {
                            self.write_gpr_sized(0, size, dest);
                            self.write_operand_sized(bus, operand, size, dest)?;
                        }
                    }
                }
                Ok(self.charge(TimingClass::CmpXchg))
            }
            0x0fc0 | 0x0fc1 => {
                // XADD r/m, r. C0 is the byte form, C1 the word/dword form. The exchange-and-add
                // first saves the destination, then writes dest + src back to the destination and
                // copies the saved destination into the source register. The flags come out
                // exactly like ADD of the two operands (reuse alu_add).
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                if insn.opcode == 0x0fc0 {
                    let dest = self.read_operand_u8(bus, operand)?;
                    let src = self.read_gpr8(modrm.reg);
                    let sum =
                        self.alu_add(u32::from(dest), u32::from(src), 0, BusWidth::Byte) as u8;
                    self.write_operand_u8(bus, operand, sum)?;
                    self.write_gpr8(modrm.reg, dest);
                } else {
                    let dest = self.read_operand_sized(bus, operand, operand_size)?;
                    let src = self.read_gpr_sized(modrm.reg, operand_size);
                    let sum = self.alu_add(dest, src, 0, operand_size.bus_width());
                    self.write_operand_sized(bus, operand, operand_size, sum)?;
                    self.write_gpr_sized(modrm.reg, operand_size, dest);
                }
                Ok(self.charge(TimingClass::Xadd))
            }
            opcode => unreachable!("bit-manipulation opcode {opcode:#x}"),
        }
    }

    /// The SETcc / two-operand IMUL block through the decode/execute split. Integer CMOVcc is a
    /// P6 instruction and is rejected by the generation gate before routing reaches this block.
    pub(super) fn execute_condmove_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        let operand_size = insn.operand_size;

        match insn.opcode {
            0x0f90..=0x0f9f => {
                // SETcc r/m8: set the byte operand to 1 when the condition holds, else 0. Always
                // byte-wide regardless of the operand-size prefix. The condition code is the low
                // nibble of the second byte (insn.opcode & 0x0f). Touches no flags.
                let (_, operand) = self.resolve_decoded_modrm_operand(insn);
                let set = self.condition((insn.opcode & 0x0f) as u8);
                self.write_operand_u8(bus, operand, u8::from(set))?;
                Ok(self.charge(TimingClass::SetCc))
            }
            0x0faf => {
                // IMUL reg, r/m: two-operand signed multiply into the reg destination. The full
                // product's high half is discarded; CF/OF are set when the result does not fit in
                // the operand size (the truncated result does not sign-extend back to the full
                // product). Reuses `imul_truncated` verbatim.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let src = self.read_operand_sized(bus, operand, operand_size)?;
                let dst = self.read_gpr_sized(modrm.reg, operand_size);
                let result = self.imul_truncated(dst, src, operand_size);
                self.write_gpr_sized(modrm.reg, operand_size, result);
                Ok(self.charge(TimingClass::ImulRm))
            }
            opcode => unreachable!("condmove opcode {opcode:#x}"),
        }
    }

    /// The system / descriptor-table / segment-load block (task A12) through the decode/execute
    /// split. Each arm mirrors the former fused handler verbatim — the same /ext dispatch off
    /// `modrm.reg`, the same privilege (`require_cpl0`) and protected-mode gates, the same descriptor
    /// loads and TLB/code-cache flushes, the same #BR/#UD faults, and the same clocks — but consumes
    /// the ModRM/operand pre-decoded by `decode` instead of re-fetching. Crucially the state-changing
    /// leaf helpers (`load_segment`, `load_ldtr`, `load_tr`, `verify_segment`, `store_descriptor_table`,
    /// `flush_tlb_and_code_caches`, `try_read_descriptor`/`descriptor_accessible`) are reused
    /// UNCHANGED, so the invalidation hooks Stage B depends on still fire exactly as before. The
    /// far pointer for LES/LDS is read FROM MEMORY here (against live registers), never at decode.
    /// Dispatches off the FULL u16 `insn.opcode` (0x0F00/01/02/03/06/20/22 plus single-byte
    /// 0x62/0x63/0xc4/0xc5) so the `as u8` narrowing can never alias a 0F opcode onto a single-byte
    /// one.
    pub(super) fn execute_system_seg_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
        _committed: &mut CommittedCore,
    ) -> ExecResult<CycleOutcome> {
        let operand_size = insn.operand_size;
        match insn.opcode {
            0x0f00 => {
                // Group 6 (SLDT/STR/LLDT/LTR/VERR/VERW). The whole group is invalid outside
                // protected mode, exactly as the fused handler gated it.
                if !self.is_protected_mode() {
                    return Err(InternalFault::Exception {
                        vector: 6,
                        error_code: None,
                    });
                }
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                match modrm.reg {
                    0 => {
                        // SLDT r/m16: store the LDTR selector.
                        let selector = u32::from(self.ldtr.selector);
                        self.write_operand_sized(bus, operand, OperandSize::Word, selector)?;
                        Ok(self.charge(TimingClass::SldtStr))
                    }
                    1 => {
                        // STR r/m16: store the task-register selector.
                        let selector = u32::from(self.tr.selector);
                        self.write_operand_sized(bus, operand, OperandSize::Word, selector)?;
                        Ok(self.charge(TimingClass::SldtStr))
                    }
                    2 => {
                        // LLDT r/m16: load the local descriptor table register. Privileged.
                        self.require_cpl0()?;
                        let selector =
                            self.read_operand_sized(bus, operand, OperandSize::Word)? as u16;
                        self.load_ldtr(bus, selector)?;
                        Ok(self.charge(TimingClass::LldtLtr))
                    }
                    3 => {
                        // LTR r/m16: load the task register. Privileged.
                        self.require_cpl0()?;
                        let selector =
                            self.read_operand_sized(bus, operand, OperandSize::Word)? as u16;
                        self.load_tr(bus, selector)?;
                        Ok(self.charge(TimingClass::LldtLtr))
                    }
                    4 | 5 => {
                        // VERR (/4) / VERW (/5): set ZF if the segment is readable / writable.
                        let selector =
                            self.read_operand_sized(bus, operand, OperandSize::Word)? as u16;
                        let ok = self.verify_segment(bus, selector, modrm.reg == 5)?;
                        self.set_flag(FLAG_ZF, ok);
                        Ok(self.charge(TimingClass::VerRw))
                    }
                    _reg => Err(undefined_opcode()),
                }
            }
            0x0f01 => {
                // Group 7 (SGDT/SIDT/LGDT/LIDT/SMSW/LMSW/INVLPG).
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                match modrm.reg {
                    4 => {
                        // SMSW r/m16: store the machine status word (low 16 bits of CR0).
                        let msw = self.control.cr0 as u16;
                        self.write_operand_sized(bus, operand, OperandSize::Word, u32::from(msw))?;
                        Ok(self.charge(TimingClass::Smsw))
                    }
                    6 => {
                        // LMSW r/m16: load MP/EM/TS; PE can be set but not cleared. Privileged.
                        self.require_cpl0()?;
                        let msw = self.read_operand_sized(bus, operand, OperandSize::Word)?;
                        let switchable = CR0_MP | CR0_EM | CR0_TS;
                        let mut cr0 = (self.control.cr0 & !switchable) | (msw & switchable);
                        if msw & CR0_PE != 0 {
                            cr0 |= CR0_PE;
                        }
                        if self.control.cr0 != cr0 {
                            // Captured BEFORE the assignment: the flush predicate needs the old
                            // value, and the flush itself must run after the new one is in place.
                            let old_cr0 = self.control.cr0;
                            self.control.cr0 = cr0;
                            self.recompute_alignment_armed();
                            // LMSW's switchable set is MP|EM|TS plus an optional PE SET, so it can
                            // never reach PG or WP and the predicate is always false here. The
                            // gated call is still the right shape: gating on what actually moves
                            // the map is stronger than special-casing this opcode.
                            self.flush_tlb_for_cr0_write(old_cr0, cr0);
                            // LMSW can only set PE (never clear it, masked out of
                            // `switchable` above), and require_cpl0 above already forced
                            // cpl == 0 -- entering protected mode this way starts at ring 0
                            // per the PRM, and cpl was already 0, so no assignment needed.
                        }
                        Ok(self.charge(TimingClass::Lmsw))
                    }
                    reg => {
                        // SGDT/SIDT/LGDT/LIDT/INVLPG all require a memory operand.
                        let memory = match operand {
                            RmOperand::Memory(memory) => memory,
                            RmOperand::Register(_) => {
                                return Err(InternalFault::Exception {
                                    vector: 6,
                                    error_code: None,
                                });
                            }
                        };
                        match reg {
                            0 => {
                                // SGDT m: store the GDTR pseudo-descriptor.
                                self.store_descriptor_table(bus, memory, self.gdtr)?;
                                Ok(self.charge(TimingClass::SgdtSidt))
                            }
                            1 => {
                                // SIDT m: store the IDTR pseudo-descriptor.
                                self.store_descriptor_table(bus, memory, self.idtr)?;
                                Ok(self.charge(TimingClass::SgdtSidt))
                            }
                            2 | 3 => {
                                // LGDT (/2) / LIDT (/3): load the GDTR/IDTR from a 6-byte image.
                                // 386 PRM 5.1 ("Privilege Levels"): LGDT/LIDT reload the
                                // descriptor-table base/limit registers that the whole
                                // protection model rests on, so like LLDT/LTR/LMSW/CLTS above
                                // they are privileged instructions -- #GP(0) outside CPL 0.
                                // Real mode has no protection, so CPL is always 0 there and
                                // this gate is a no-op for real-mode boot code.
                                self.require_cpl0()?;
                                let limit = self.read_memory_sized(
                                    bus,
                                    memory.segment,
                                    memory.offset,
                                    OperandSize::Word,
                                    BusAccessKind::DataRead,
                                )? as u16;
                                let base = self.read_memory_sized(
                                    bus,
                                    memory.segment,
                                    memory.offset + 2,
                                    OperandSize::Dword,
                                    BusAccessKind::DataRead,
                                )?;
                                let table = DescriptorTable { base, limit };
                                if reg == 2 {
                                    self.gdtr = table;
                                } else {
                                    self.idtr = table;
                                }
                                Ok(self.charge(TimingClass::LgdtLidt))
                            }
                            7 => {
                                // INVLPG m: privileged on the 486. The operand supplies an address;
                                // its memory contents are not read.
                                self.require_isa_generation(IsaGeneration::I486)?;
                                if self.current_privilege_level() != 0 {
                                    return Err(InternalFault::Exception {
                                        vector: 6,
                                        error_code: None,
                                    });
                                }
                                let linear =
                                    self.segment_linear_byte(memory.segment, memory.offset, false)?;
                                // The reflected-call memo's control-effect journal
                                // (slice1 plan R2.5): recorded BEFORE the effect runs,
                                // in trip order, so an answered trip can reproduce the
                                // same invalidation through `apply_invlpg` below.
                                #[cfg(feature = "reflected-call-memo")]
                                crate::reflected_call_memo::note_invlpg(self, linear);
                                self.apply_invlpg(linear);
                                Ok(self.charge(TimingClass::Invlpg))
                            }
                            _ => Err(undefined_opcode()),
                        }
                    }
                }
            }
            0x0f02 => {
                // LAR reg, r/m16: read the descriptor access-rights byte(s). Protected mode only.
                if !self.is_protected_mode() {
                    return Err(InternalFault::Exception {
                        vector: 6,
                        error_code: None,
                    });
                }
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let selector = self.read_operand_sized(bus, operand, OperandSize::Word)? as u16;
                match self.try_read_descriptor(bus, selector)? {
                    Some((_, high)) if self.descriptor_accessible(selector, high, false) => {
                        let mask = match operand_size {
                            OperandSize::Word => 0x0000_ff00,
                            OperandSize::Dword => 0x00f0_ff00,
                        };
                        self.write_gpr_sized(modrm.reg, operand_size, high & mask);
                        self.set_flag(FLAG_ZF, true);
                    }
                    _ => self.set_flag(FLAG_ZF, false),
                }
                Ok(self.charge(TimingClass::Lar))
            }
            0x0f03 => {
                // LSL reg, r/m16: read the descriptor segment limit. Protected mode only.
                if !self.is_protected_mode() {
                    return Err(InternalFault::Exception {
                        vector: 6,
                        error_code: None,
                    });
                }
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let selector = self.read_operand_sized(bus, operand, OperandSize::Word)? as u16;
                match self.try_read_descriptor(bus, selector)? {
                    Some((low, high)) if self.descriptor_accessible(selector, high, true) => {
                        let mut limit = (low & 0xffff) | (high & 0x000f_0000);
                        if high & 0x0080_0000 != 0 {
                            limit = (limit << 12) | 0x0fff;
                        }
                        self.write_gpr_sized(modrm.reg, operand_size, limit);
                        self.set_flag(FLAG_ZF, true);
                    }
                    _ => self.set_flag(FLAG_ZF, false),
                }
                Ok(self.charge(TimingClass::Lsl))
            }
            0x0f06 => {
                // CLTS: clear the task-switched flag. Privileged.
                self.require_cpl0()?;
                self.control.cr0 &= !CR0_TS;
                Ok(self.charge(TimingClass::Clts))
            }
            0x0f20 => {
                // MOV reg, CR: whole-32-bit read of the selected control register. The ModRM is a
                // register form (`mode == 3`); any other `mode` is an invalid encoding (#UD). The
                // `reg` field is the CR number, `rm` the destination GPR.
                //
                // Privileged, like every other 0F 00/01 system-register op (LLDT/LTR/LMSW/CLTS
                // all gate on require_cpl0 above). This was missing the gate: a CPL-3 guest
                // (including a V86 task, which is architecturally always CPL 3) could read CR0
                // straight through. #GP(0) outside CPL 0.
                let modrm = insn.modrm.expect("MOV reg,CR decoded with a ModRM");
                if modrm.mode != 3 {
                    return Err(undefined_opcode());
                }
                if modrm.reg == 4 {
                    self.require_isa_generation(IsaGeneration::P55c)?;
                }
                self.require_cpl0()?;
                // 386 PRM 12.2.4 / table 12-1: only CR0, CR2, CR3 (and, on the P55C
                // persona, CR4) are architecturally defined. CR1/CR5/CR6/CR7 have no backing
                // register at all -- referencing one is an invalid encoding (#UD), not a
                // silent read of 0.
                let value = match modrm.reg {
                    0 => self.control.cr0,
                    2 => self.control.cr2,
                    3 => self.control.cr3,
                    4 => self.control.cr4,
                    _ => return Err(undefined_opcode()),
                };
                self.write_gpr32(modrm.rm, value);
                Ok(self.charge(TimingClass::MovCrDr))
            }
            0x0f22 => {
                // MOV CR, reg: whole-32-bit write of the selected control register. CR0 (paging
                // enable / WP) and CR3 (page-table base) change translations, so flush the TLB
                // (and code caches) via the unchanged helper; CR2/CR4 do not.
                //
                // Privileged (same require_cpl0 gate as LLDT/LTR/LMSW/CLTS). This was the
                // prerequisite gap the owner flagged for VCPI work: without it, a ring-3 V86
                // guest could silently write CR0 (e.g. flip PE/PG) or CR3 (repoint the page
                // tables), which is a guest-fidelity and monitor-security hole. #GP(0) outside
                // CPL 0.
                let modrm = insn.modrm.expect("MOV CR,reg decoded with a ModRM");
                if modrm.mode != 3 {
                    return Err(undefined_opcode());
                }
                // 386 PRM 12.2.4 / table 12-1: same undefined-register check as MOV reg,CR
                // above -- CR1/CR5/CR6/CR7 have no backing store, so writing one is #UD, not
                // a silent no-op.
                if !matches!(modrm.reg, 0 | 2 | 3 | 4) {
                    return Err(undefined_opcode());
                }
                if modrm.reg == 4 {
                    self.require_isa_generation(IsaGeneration::P55c)?;
                }
                self.require_cpl0()?;
                let value = self.read_gpr32(modrm.rm);
                match modrm.reg {
                    0 => {
                        // 386 PRM 5.2.1 / 12.3.1: PG (bit 31) requires PE (bit 0) -- paged
                        // linear addressing only makes sense once protection (and with it
                        // segment/privilege checking) is active. Setting PG while PE is (or
                        // would remain) clear is an invalid combination -- #GP(0), the
                        // register is left unmodified. This also rejects the "set both PE
                        // and PG at once with PE=0 in the new value" case, since PE is taken
                        // from the value being written, not the old CR0.
                        if value & CR0_PG != 0 && value & CR0_PE == 0 {
                            return Err(InternalFault::Exception {
                                vector: 13,
                                error_code: Some(0),
                            });
                        }
                        // Captured BEFORE the assignment; see `flush_tlb_for_cr0_write`.
                        let old_cr0 = self.control.cr0;
                        self.control.cr0 = value;
                        self.recompute_alignment_armed();
                        self.flush_tlb_for_cr0_write(old_cr0, value);
                    }
                    2 => self.control.cr2 = value,
                    3 => {
                        // CR3 holds the page-directory base in bits 31:12. The 486 and P55C
                        // also retain PWT/PCD in bits 3:4; those bits remain reserved on the 386.
                        let mask = match self.persona() {
                            CpuPersona::I386 => 0xffff_f000,
                            CpuPersona::I486 | CpuPersona::I586 => 0xffff_f018,
                        };
                        // Ring-gated (design `2026-09-02-cr3-code-cache-gate-design.md`): the
                        // function reads the OLD register value to seed/select the ring, so it
                        // owns the assignment -- do not set `self.control.cr3` here.
                        self.flush_tlb_and_code_caches_for_cr3_write(value & mask);
                    }
                    4 => {
                        // CR4 is present only on the P55C persona. TSD has a modeled effect;
                        // the other P55C-defined bits are inert storage. A set reserved bit
                        // faults instead of being silently dropped.
                        if value & !CR4_DEFINED_MASK != 0 {
                            return Err(InternalFault::Exception {
                                vector: 13,
                                error_code: Some(0),
                            });
                        }
                        self.control.cr4 = value;
                    }
                    _ => unreachable!("undefined CR numbers are rejected by the check above"),
                }
                Ok(self.charge(TimingClass::MovCrDr))
            }
            0x0f21 => {
                // MOV reg, DR: whole-32-bit read of the selected debug register. Same shape as
                // MOV reg,CR above -- ModRM register form only (`mode == 3`; any other mode is
                // #UD), privileged (386 PRM ch12: debug-register access is CPL-0-only, #GP(0)
                // otherwise), `reg` selects the DR number, `rm` the destination GPR.
                //
                // DR4/DR5 alias DR6/DR7 by default (CR4.DE clear, which this core never sets
                // behaviorally -- see CR4_TSD/CR4_DEFINED_MASK above) per 386 PRM ch12 and the
                // 486/586 successors; a guest that references DR4/DR5 expecting DR6/DR7 (as
                // DOS/32A's exception reporter does) gets the alias instead of #UD.
                //
                // Storage only: no breakpoint matching or #DB generation is modeled (ledger
                // row 26, deferred). This just stops MOV DR6/DR7 from raising #UD, which is
                // what DOS/32A's VCPI init and exception reporter need.
                self.require_cpl0()?;
                let modrm = insn.modrm.expect("MOV reg,DR decoded with a ModRM");
                if modrm.mode != 3 {
                    return Err(undefined_opcode());
                }
                let value = match modrm.reg {
                    0..=3 => self.control.dr0_3[modrm.reg as usize],
                    4 => self.control.dr6,
                    5 => self.control.dr7,
                    6 => self.control.dr6,
                    7 => self.control.dr7,
                    _ => return Err(undefined_opcode()),
                };
                self.write_gpr32(modrm.rm, value);
                Ok(self.charge(TimingClass::MovCrDr))
            }
            0x0f23 => {
                // MOV DR, reg: whole-32-bit write of the selected debug register. Same shape as
                // MOV CR,reg above; see 0x0f21 for the privilege/aliasing rationale.
                //
                // Reserved-bit handling per 386 PRM ch12: DR7 bit 10 is hardwired to 1 (it is
                // not settable by the guest); this core does not model LE/GE cycle-exactness or
                // the L/G breakpoint enables beyond plain storage, so every other bit is stored
                // as written. DR6 has no core-enforced reserved bits either (this is storage
                // only, not breakpoint matching), so it round-trips whatever is written.
                self.require_cpl0()?;
                let modrm = insn.modrm.expect("MOV DR,reg decoded with a ModRM");
                if modrm.mode != 3 {
                    return Err(undefined_opcode());
                }
                let value = self.read_gpr32(modrm.rm);
                match modrm.reg {
                    0..=3 => self.control.dr0_3[modrm.reg as usize] = value,
                    4 | 6 => self.control.dr6 = value,
                    5 | 7 => self.control.dr7 = (value & !DR7_FIXED_ONE) | DR7_FIXED_ONE,
                    _ => return Err(undefined_opcode()),
                }
                Ok(self.charge(TimingClass::MovCrDr))
            }
            0x63 => {
                // ARPL r/m16,r16 raises a selector's requested privilege level when the source
                // RPL is more restrictive. It exists only in protected mode and always operates
                // on 16-bit selectors, regardless of the operand-size attribute.
                if !self.is_protected_mode() || self.is_v86_mode() {
                    return Err(undefined_opcode());
                }
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let destination = self.read_operand_sized(bus, operand, OperandSize::Word)? as u16;
                let source_rpl = self.read_gpr16(modrm.reg) & 3;
                let adjusted = destination & 3 < source_rpl;
                if adjusted {
                    let selector = (destination & !3) | source_rpl;
                    self.write_operand_sized(bus, operand, OperandSize::Word, u32::from(selector))?;
                }
                self.set_flag(FLAG_ZF, adjusted);
                // ARPL is the ONE charge site in the tree that was already
                // persona-keyed before the class table existed, so it cannot be a
                // class: a class holds one epoch-1 value for all three personas
                // and this arm holds three. It takes the `Legacy` escape, which
                // charges the literal handed to it under every epoch -- exactly
                // today's behaviour, and an epoch-2 under-charge recorded here
                // rather than guessed at. Folding it in needs a per-persona
                // epoch-1 column, which is a table shape change, not a routing
                // one.
                let clocks_used: u16 = match self.persona() {
                    CpuPersona::I386 => 20,
                    CpuPersona::I486 => 9,
                    CpuPersona::I586 => 7,
                };
                Ok(self.charge(TimingClass::Legacy(clocks_used)))
            }
            0x62 => {
                // BOUND r, m: the memory operand holds the signed lower and upper array bounds;
                // if the register is outside [lower, upper] raise #BR (vector 5). mod=3 -> #UD.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let memory = match operand {
                    RmOperand::Memory(memory) => memory,
                    RmOperand::Register(_) => {
                        return Err(InternalFault::Exception {
                            vector: 6,
                            error_code: None,
                        });
                    }
                };
                let size = operand_size.bytes();
                let lower = self.read_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    operand_size,
                    BusAccessKind::DataRead,
                )?;
                let upper = self.read_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset + size,
                    operand_size,
                    BusAccessKind::DataRead,
                )?;
                let index = self.read_gpr_sized(modrm.reg, operand_size);
                let (index, lower, upper) = match operand_size {
                    OperandSize::Word => (
                        i32::from(index as u16 as i16),
                        i32::from(lower as u16 as i16),
                        i32::from(upper as u16 as i16),
                    ),
                    OperandSize::Dword => (index as i32, lower as i32, upper as i32),
                };
                if index < lower || index > upper {
                    return Err(InternalFault::Exception {
                        vector: 5,
                        error_code: None,
                    });
                }
                Ok(self.charge(TimingClass::Bound))
            }
            0xc4 | 0xc5 => {
                // LES (0xc4) / LDS (0xc5): load a far pointer from memory. The low half (operand
                // size) goes into the reg operand and the next word into ES (0xc4) or DS (0xc5).
                // The far pointer is read here against the LIVE registers; the segment is loaded
                // through the unchanged `load_segment`. mod=3 (a register r/m) is #UD.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let mem = match operand {
                    RmOperand::Memory(mem) => mem,
                    RmOperand::Register(_) => {
                        return Err(InternalFault::Exception {
                            vector: 6,
                            error_code: None,
                        });
                    }
                };
                let offset = self.read_memory_sized(
                    bus,
                    mem.segment,
                    mem.offset,
                    operand_size,
                    BusAccessKind::DataRead,
                )?;
                let selector_offset = far_pointer_second_word_offset(
                    insn.address_size,
                    mem.offset,
                    operand_size.bytes(),
                );
                let selector = self.read_memory_sized(
                    bus,
                    mem.segment,
                    selector_offset,
                    OperandSize::Word,
                    BusAccessKind::DataRead,
                )? as u16;
                let segment = if insn.opcode == 0xc4 {
                    SegmentIndex::Es
                } else {
                    SegmentIndex::Ds
                };
                self.load_segment(bus, segment, selector)?;
                self.write_gpr_sized(modrm.reg, operand_size, offset);
                Ok(self.charge(TimingClass::LesLds))
            }
            0x0fb2 | 0x0fb4 | 0x0fb5 => {
                // LSS (0F B2) / LFS (0F B4) / LGS (0F B5): 386 PRM 17-56 -- same far-pointer
                // shape as LES/LDS above (mod=3 is #UD, the offset is read first, then the
                // selector word right after it, both against the LIVE registers), loading the
                // offset into the reg operand and the selector into SS/FS/GS through the
                // unchanged `load_segment` (so the existing null-selector/#GP/#NP rules and the
                // SS.B cache refresh all apply exactly as they do for any other segment load).
                // LSS additionally arms the one-instruction interrupt shadow via
                // `load_segment_arming_ss_shadow`, exactly like MOV SS/POP SS: 386 PRM 11-16
                // treats "load SS, then load (E)SP" as one atomic unit against interrupts, NMI,
                // and single-step, and LSS is that idiom in a single instruction.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let mem = match operand {
                    RmOperand::Memory(mem) => mem,
                    RmOperand::Register(_) => {
                        return Err(InternalFault::Exception {
                            vector: 6,
                            error_code: None,
                        });
                    }
                };
                let offset = self.read_memory_sized(
                    bus,
                    mem.segment,
                    mem.offset,
                    operand_size,
                    BusAccessKind::DataRead,
                )?;
                let selector_offset = far_pointer_second_word_offset(
                    insn.address_size,
                    mem.offset,
                    operand_size.bytes(),
                );
                let selector = self.read_memory_sized(
                    bus,
                    mem.segment,
                    selector_offset,
                    OperandSize::Word,
                    BusAccessKind::DataRead,
                )? as u16;
                let segment = match insn.opcode {
                    0x0fb2 => SegmentIndex::Ss,
                    0x0fb4 => SegmentIndex::Fs,
                    _ => SegmentIndex::Gs,
                };
                if segment == SegmentIndex::Ss {
                    self.load_segment_arming_ss_shadow(bus, segment, selector)?;
                } else {
                    self.load_segment(bus, segment, selector)?;
                }
                self.write_gpr_sized(modrm.reg, operand_size, offset);
                Ok(self.charge(TimingClass::LesLds))
            }
            opcode => unreachable!("system/segment opcode {opcode:#x}"),
        }
    }

    /// The far/indirect/RET/INT control-flow block + 0xff group 5 through the decode/execute split.
    /// Each arm mirrors the former fused handler verbatim — same far-pointer reconstruction, same
    /// ret/retf and interrupt/IRET delivery, same FF sub-op dispatch off `modrm.reg`, same clocks —
    /// but consumes what `decode` pre-parsed (the far-pointer offset/selector in `imm`/`imm2`, the
    /// imm16 release in `imm`, the imm8 vector in `imm`, or the ModRM/descriptor) so the executor
    /// re-fetches no instruction byte. The protected-mode descriptor loads, gates, faults, the
    /// V86 IOPL check, the interrupt-shadow/IF semantics, and the FF indirect target read all stay in
    /// the unchanged helpers, so behavior is byte-for-byte identical to the fused path.
    pub(super) fn execute_control_flow_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
        committed: &mut CommittedCore,
    ) -> ExecResult<CycleOutcome> {
        let operand_size = insn.operand_size;
        let address_size = insn.address_size;

        match insn.opcode as u8 {
            0x9a => {
                // CALL far direct. `decode` fetched the far pointer (offset into `imm`, selector into
                // `imm2`); reconstruct it and deliver through the unchanged far-call helper.
                let offset = insn.imm;
                let selector = insn.imm2 as u16;
                self.far_call(bus, selector, offset, operand_size, committed)?;
                #[cfg(feature = "reflected-call-diagnostic")]
                crate::reflected_call_diag::on_far_transfer_boundary(self, bus);
                #[cfg(feature = "reflected-call-memo")]
                crate::reflected_call_memo::on_far_transfer(self, bus);
                {
                    let class = self.far_transfer_class(TimingClass::CallFar);
                    Ok(self.charge(class))
                }
            }
            0xea => {
                // JMP far direct. Same far-pointer reconstruction, via the far-jump helper.
                let offset = insn.imm;
                let selector = insn.imm2 as u16;
                self.far_jump(bus, selector, offset, operand_size, committed)?;
                #[cfg(feature = "reflected-call-diagnostic")]
                crate::reflected_call_diag::on_far_transfer_boundary(self, bus);
                #[cfg(feature = "reflected-call-memo")]
                crate::reflected_call_memo::on_far_transfer(self, bus);
                {
                    let class = self.far_transfer_class(TimingClass::JmpFar);
                    Ok(self.charge(class))
                }
            }
            0xc2 => {
                // RET near, release imm16 bytes of arguments. `decode` fetched the release count into
                // `imm`; validate the return offset before committing either stack adjustment.
                self.near_return(bus, operand_size, insn.imm as u16)?;
                Ok(self.charge(TimingClass::RetNearImm))
            }
            0xc3 => {
                self.near_return(bus, operand_size, 0)?;
                Ok(self.charge(TimingClass::RetNear))
            }
            0xca => {
                // RETF, release imm16 bytes. `decode` fetched the count into `imm`; pop CS:IP via the
                // far-return helper THEN release.
                let release = insn.imm as u16;
                #[cfg(feature = "retf-arity-census")]
                let site = self.retf_census_site();
                self.return_far(bus, operand_size, release)?;
                #[cfg(feature = "retf-arity-census")]
                self.note_retf_target(site);
                self.release_stack(release);
                {
                    let class = self.far_transfer_class(TimingClass::RetFar);
                    Ok(self.charge(class))
                }
            }
            0xcb => {
                #[cfg(feature = "retf-arity-census")]
                let site = self.retf_census_site();
                self.return_far(bus, operand_size, 0)?;
                #[cfg(feature = "retf-arity-census")]
                self.note_retf_target(site);
                {
                    let class = self.far_transfer_class(TimingClass::RetFar);
                    Ok(self.charge(class))
                }
            }
            0xcc => {
                // INT 3: one-byte breakpoint trap to vector 3, via the shared delivery path.
                self.software_interrupt(bus, 3, committed)?;
                Ok(self.charge(TimingClass::Int3))
            }
            0xcd => {
                // INT n. IOPL-sensitive in V86 (checked here, exactly as the fused handler did,
                // before the delivery). `decode` fetched the vector into `imm`.
                let vector = insn.imm as u8;
                #[cfg(feature = "int-trace")]
                if crate::int_trace::is_traced(vector) {
                    crate::int_trace::on_entry(
                        vector,
                        self.registers.cs().selector,
                        self.registers.eip,
                        self.pushad_image(),
                        self.registers.segment(SegmentIndex::Ds).selector,
                        self.registers.segment(SegmentIndex::Es).selector,
                        self.is_v86_mode(),
                        self.iopl(),
                    );
                }
                // In V86 a below-IOPL `INT n` faults to the monitor, but the emulator's HLE
                // BIOS/DOS services (INT 10h video, INT 13h disk, …) are driven from
                // `interrupt_acknowledge`, which the fault path would otherwise skip — so the
                // guest's console output would never render under a V86 monitor. Notify the bus
                // first, exactly as real-mode `software_interrupt` does, then raise the #GP.
                if self.is_v86_mode() && self.iopl() < 3 {
                    bus.interrupt_acknowledge(vector, self.read_gpr16(0))?;
                    self.check_v86_iopl()?;
                }
                // Read BEFORE delivery: `software_interrupt` loads the handler's
                // CS and can leave V86, and Intel prices the gate by the mode the
                // INSTRUCTION executed in.
                let mode = int_n_class(self.control.cr0 & CR0_PE != 0, self.is_v86_mode());
                self.software_interrupt(bus, vector, committed)?;
                Ok(self.charge(mode))
            }
            0xce => {
                // INTO: trap to vector 4 only when OF is set; otherwise a no-op.
                if self.flag(FLAG_OF) {
                    self.software_interrupt(bus, 4, committed)?;
                    Ok(self.charge(TimingClass::IntO))
                } else {
                    Ok(self.charge(TimingClass::IntONotTaken))
                }
            }
            0xcf => {
                // IRET is IOPL-sensitive in V86 (386 PRM): #GP(0) below IOPL 3.
                // TOKAEMM runs its guests at real IOPL 3, so this waves them through
                // and the pop reaches real EFLAGS; the gate still serves any IOPL-0
                // V86 configuration. Mirrors CLI/STI/PUSHF/POPF.
                self.check_v86_iopl()?;
                // The MODE is read BEFORE the return, deliberately: `iret` can
                // switch mode (a protected-mode return into V86 sets VM), and
                // Intel prices the transfer by where it STARTED and where it
                // landed, not by where it ended up alone.
                let from_v86 = self.is_v86_mode();
                let from_cpl = self.current_privilege_level();
                let protected = self.control.cr0 & CR0_PE != 0;
                self.iret(bus, operand_size, committed)?;
                Ok(self.charge(iret_class(
                    protected,
                    from_v86,
                    from_cpl,
                    self.is_v86_mode(),
                    self.current_privilege_level(),
                )))
            }
            0xff => {
                // Group 5. The /ext is `modrm.reg`. `decode` pre-parsed the ModRM + descriptor; the
                // r/m operand resolves against the live registers here. The indirect CALL/JMP read
                // their target FROM MEMORY now, mirroring the fused handler's read order exactly.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                match modrm.reg {
                    0 | 1 => {
                        let value = self.read_operand_sized(bus, operand, operand_size)?;
                        let result = self.inc_dec(value, modrm.reg == 1, operand_size.bus_width());
                        self.write_operand_sized(bus, operand, operand_size, result)?;
                        Ok(self.charge(match operand {
                            RmOperand::Register(_) => TimingClass::Reg,
                            RmOperand::Memory(_) => TimingClass::IncDecRm,
                        }))
                    }
                    2 => {
                        let target = self.read_operand_sized(bus, operand, operand_size)?;
                        self.push(bus, self.registers.eip, operand_size)?;
                        self.set_eip(target & operand_size.mask());
                        Ok(self.charge(TimingClass::CallJmpRm))
                    }
                    4 => {
                        let target = self.read_operand_sized(bus, operand, operand_size)?;
                        self.set_eip(target & operand_size.mask());
                        Ok(self.charge(TimingClass::CallJmpRm))
                    }
                    6 => {
                        let value = self.read_operand_sized(bus, operand, operand_size)?;
                        self.push(bus, value, operand_size)?;
                        // Named because the Word memory form is an `InterpretOne` call-out row:
                        // its budget bound and this arm must charge the same number.
                        Ok(self.charge(TimingClass::PushMem))
                    }
                    3 | 5 => {
                        // Far CALL (/3) and far JMP (/5) via memory. The operand must be memory;
                        // mod=3 is an invalid encoding and faults as #UD.
                        let memory = match operand {
                            RmOperand::Memory(memory) => memory,
                            RmOperand::Register(_) => {
                                return Err(InternalFault::Exception {
                                    vector: 6,
                                    error_code: None,
                                });
                            }
                        };
                        let offset = self.read_memory_sized(
                            bus,
                            memory.segment,
                            memory.offset,
                            operand_size,
                            BusAccessKind::DataRead,
                        )?;
                        // The selector follows the offset in memory. Its address is computed in the
                        // address-size space, so on a 16-bit real-mode segment it wraps at 0xffff
                        // (offset 0xfffe puts the selector at 0x0000, not past the limit), matching
                        // the 80386 (SingleStepTests FF.3 "call far [ds:bx+di]" with bx=di=0xffff;
                        // `far_call_via_memory_wraps_selector_offset_at_64k` pins it). LES/LDS/LSS/
                        // LFS/LGS share the exact same shape and the same wrap, through
                        // `far_pointer_second_word_offset`.
                        let selector_offset = far_pointer_second_word_offset(
                            address_size,
                            memory.offset,
                            operand_size.bytes(),
                        );
                        let selector = self.read_memory_sized(
                            bus,
                            memory.segment,
                            selector_offset,
                            OperandSize::Word,
                            BusAccessKind::DataRead,
                        )? as u16;
                        if modrm.reg == 3 {
                            self.far_call(bus, selector, offset, operand_size, committed)?;
                        } else {
                            self.far_jump(bus, selector, offset, operand_size, committed)?;
                        }
                        // `FF /3` and `FF /5`, the indirect far forms review N3 named:
                        // a DPMI host's return through a saved far pointer is exactly
                        // this shape, and the direct-form hooks at `0x9A`/`0xEA` miss it.
                        #[cfg(feature = "reflected-call-diagnostic")]
                        crate::reflected_call_diag::on_far_transfer_boundary(self, bus);
                        #[cfg(feature = "reflected-call-memo")]
                        crate::reflected_call_memo::on_far_transfer(self, bus);
                        Ok(self.charge(TimingClass::CallJmpFarMem))
                    }
                    _extension => Err(undefined_opcode()),
                }
            }
            opcode => unreachable!("control-flow opcode {opcode:#x}"),
        }
    }

    /// Execute a two-byte (0F) opcode that has no dedicated split group. `opcode` is the second
    /// opcode byte that `decode` already read + charged and gated; this never re-reads it. Reached
    /// two ways: the `TwoByteFallback` arm of `execute_decoded` (which #UDs the unimplemented bytes),
    /// and as a leaf call from `execute_misc_decoded` for the no-operand 0F members. The converted 0F
    /// groups (MOVZX/MOVSX and the rest) bypass this entirely via `route_group`/`execute_decoded`.
    ///
    /// Most opcodes handled here re-read no further instruction bytes, so the heterogeneous
    /// `Misc` group (task A14) also leaf-calls this for its 0F members
    /// (INVD/WBINVD/WRMSR/RDTSC/RDMSR/CPUID/BSWAP) rather than duplicating them.
    /// PUSH/POP FS/GS (0F A0/A1/A8/A9) are Misc members too: like their one-byte ES/SS/DS
    /// counterparts in `execute_stack_decoded`, they touch the stack, so `bus` is threaded
    /// through. The genuinely unimplemented 0F bytes still fall through to the
    /// `UnsupportedTwoByteOpcode` arm and #UD.
    pub(super) fn execute_two_byte<B: CpuBus>(
        &mut self,
        bus: &mut B,
        opcode: u8,
        operand_size: OperandSize,
    ) -> ExecResult<CycleOutcome> {
        match opcode {
            // The MMX integer-SIMD encodings are not handled anywhere: the GSW-586 carries no SIMD
            // extension, so `two_byte_isa_generation` marks them `IsaGeneration::Never` and the
            // shared decode gate #UDs them on every persona before execute is reached.
            // 0F 00 (group 6: SLDT/STR/LLDT/LTR/VERR/VERW), 0F 01 (group 7: SGDT/SIDT/LGDT/LIDT/
            // SMSW/LMSW/INVLPG), 0F 02 (LAR), 0F 03 (LSL), and 0F 06 (CLTS) are converted to the
            // decode/execute split (task A12): `route_group` classifies them as
            // `DecodeGroup::SystemSeg` and `execute_system_seg_decoded` runs them (the ModRM + /ext
            // dispatch and the descriptor/CR/TLB leaf helpers are reused unchanged). Not handled here.
            0x30 => {
                // WRMSR: write EDX:EAX into the model-specific register selected by ECX.
                // Privileged (#GP(0) outside CPL 0). An undefined MSR selector also #GP(0)s.
                self.require_cpl0()?;
                let value = self.read_edx_eax();
                match self.read_gpr32(1) {
                    MSR_MCAR => self.msr.mcar = value,
                    MSR_MCTR => self.msr.mctr = value,
                    // Rebase the counter against elapsed instruction clocks.
                    // Future machine-time advances are added to the new offset.
                    MSR_TSC => self.msr.tsc_offset = value.wrapping_sub(self.elapsed_clocks),
                    _ => {
                        return Err(InternalFault::Exception {
                            vector: 13,
                            error_code: Some(0),
                        });
                    }
                }
                Ok(self.charge(TimingClass::Wrmsr))
            }
            0x31 => {
                // RDTSC: read the time-stamp counter into EDX:EAX. When CR4.TSD is set the
                // instruction is privileged and #GP(0)s outside CPL 0; with TSD clear (the
                // default) it runs at any level.
                if self.control.cr4 & CR4_TSD != 0 && self.current_privilege_level() != 0 {
                    return Err(InternalFault::Exception {
                        vector: 13,
                        error_code: Some(0),
                    });
                }
                let tsc = self.time_stamp_counter();
                self.set_edx_eax(tsc);
                #[cfg(feature = "reflected-call-diagnostic")]
                crate::reflected_call_diag::on_rdtsc_or_rdmsr_tsc();
                #[cfg(feature = "reflected-call-memo")]
                crate::reflected_call_memo::note_rdtsc_or_rdmsr(self);
                Ok(self.charge(TimingClass::Rdtsc))
            }
            0x32 => {
                // RDMSR: read the model-specific register selected by ECX into EDX:EAX.
                // Privileged; an undefined selector #GP(0)s.
                self.require_cpl0()?;
                #[cfg(feature = "reflected-call-diagnostic")]
                if self.read_gpr32(1) == MSR_TSC {
                    crate::reflected_call_diag::on_rdtsc_or_rdmsr_tsc();
                }
                if self.read_gpr32(1) == MSR_TSC {
                    #[cfg(feature = "reflected-call-memo")]
                    crate::reflected_call_memo::note_rdtsc_or_rdmsr(self);
                }
                let value = match self.read_gpr32(1) {
                    MSR_MCAR => self.msr.mcar,
                    MSR_MCTR => self.msr.mctr,
                    MSR_TSC => self.time_stamp_counter(),
                    _ => {
                        return Err(InternalFault::Exception {
                            vector: 13,
                            error_code: Some(0),
                        });
                    }
                };
                self.set_edx_eax(value);
                Ok(self.charge(TimingClass::Rdmsr))
            }
            // 0F 20 (MOV reg,CR) and 0F 22 (MOV CR,reg) are converted to the decode/execute split
            // (task A12): `route_group` classifies them as `DecodeGroup::SystemSeg` and
            // `execute_system_seg_decoded` runs them (the register-form ModRM is parsed in `decode`;
            // the whole-32-bit CR read/write, the `mode != 3` #UD, and the CR0/CR3 TLB flush via the
            // unchanged `flush_tlb_and_code_caches` stay in the executor). Not handled here. 0F 21/23
            // (MOV reg,DR / MOV DR,reg) remain unimplemented and #UD as `UnsupportedTwoByteOpcode`.
            // SETcc (0x90-0x9F) and IMUL reg,r/m (0xAF) use the CondMove split group.
            // P6 CMOVcc remains undefined on P55C.
            // 0x80-0x8f (Jcc near, rel16/32) are converted to the decode/execute split: `decode`
            // folds them into `insn.opcode` as 0x0F80-0x0F8F, `route_group` classifies them as
            // `DecodeGroup::Branch`, and `execute_branch_decoded` runs them. Not handled here.
            // MOVZX/MOVSX (0F B6/B7/BE/BF) are converted to the decode/execute split; they route
            // through `DecodeGroup::DataMove` and `execute_datamove_decoded`, never reaching here.
            // BSF/BSR (0xbc/0xbd), BT/BTS/BTR/BTC reg (0xa3/0xab/0xb3/0xbb) and imm8 (0xba), and
            // SHLD/SHRD (0xa4/0xac imm8, 0xa5/0xad CL) are converted to the decode/execute split
            // (task A10): `route_group` classifies them as `DecodeGroup::BitManip` and
            // `execute_bitmanip_decoded` runs them. Not handled here.
            0x08 | 0x09 => {
                // INVD (08) / WBINVD (09): flush the internal caches. Both are privileged and
                // raise #UD outside CPL 0. We model no PRICED cache on the Approximate class, so
                // there is no guest-visible timing effect here either way; WBINVD differs from
                // INVD only by writing dirty lines back first, which has no observable effect on
                // a write-through part. `note_cache_flush` still fires for both: it flushes the
                // diagnostic shadow L1 probe (`izarravm_machine::shadow_cache`), which is a real
                // (if unpriced) tag array and must not carry state across a guest-issued flush.
                if self.current_privilege_level() != 0 {
                    return Err(InternalFault::Exception {
                        vector: 6,
                        error_code: None,
                    });
                }
                bus.note_cache_flush();
                // Sourced apart, not together: `INVD` is 15 P5 clocks and
                // `WBINVD` is Intel's printed "2000+" (a floor, not a count) --
                // a 133x spread that shared one `clocks(4)` until the manual
                // pass. The 486's own 4-vs-5 could never have shown it.
                Ok(self.charge(if opcode & 1 == 0 {
                    TimingClass::Invd
                } else {
                    TimingClass::Wbinvd
                }))
            }
            // CMPXCHG (0xb0/0xb1) and XADD (0xc0/0xc1) are converted to the decode/execute split
            // (task A10): `route_group` classifies them as `DecodeGroup::BitManip` and
            // `execute_bitmanip_decoded` runs them. Not handled here.
            0xa2 => {
                // CPUID (0F A2). Not privileged: it runs at any CPL. The leaf selector is in
                // EAX. The result registers are EAX, EBX, ECX, EDX (full 32-bit writes). We
                // model the two basic P55C leaves. The shared decode gate has already rejected
                // CPUID on the 386 and 486 personas. Any other leaf returns all zeros.
                let leaf = self.registers.eax();
                let (eax, ebx, ecx, edx) = match leaf {
                    0 => (
                        CPUID_MAX_BASIC_LEAF,
                        CPUID_VENDOR_EBX,
                        CPUID_VENDOR_ECX,
                        CPUID_VENDOR_EDX,
                    ),
                    1 => (
                        CPUID_VERSION_EAX,
                        CPUID_LEAF1_EBX,
                        CPUID_LEAF1_ECX,
                        CPUID_FEATURES_EDX,
                    ),
                    _ => (0, 0, 0, 0),
                };
                self.write_gpr32(0, eax); // EAX
                self.write_gpr32(3, ebx); // EBX
                self.write_gpr32(1, ecx); // ECX
                self.write_gpr32(2, edx); // EDX
                Ok(self.charge(TimingClass::Cpuid))
            }
            // CMPXCHG8B m64 (0F C7 /1) is converted to the decode/execute split (task A14):
            // `route_group` classifies it as `DecodeGroup::Misc` and `execute_misc_decoded` runs it
            // (the ModRM + addressing descriptor is parsed in `decode`; the register form / wrong
            // /ext #UD and the read-modify-write stay in the executor). Not handled here.
            0xc8..=0xcf => {
                // BSWAP r32 (0F C8+r): reverse the byte order of a 32-bit register. The low
                // three bits of the opcode pick the register. The 16-bit-operand form is
                // architecturally undefined; we follow the documented Intel note and the common
                // emulator choice of leaving the register contents undefined-but-unchanged, so a
                // 66h-prefixed BSWAP here is a no-op rather than corrupting the value.
                let reg = opcode & 0x07;
                if matches!(operand_size, OperandSize::Dword) {
                    let value = self.read_gpr32(reg);
                    self.write_gpr32(reg, value.swap_bytes());
                }
                Ok(self.charge(TimingClass::Bswap))
            }
            // PUSH FS / PUSH GS (0F A0 / 0F A8): 386+ additions, otherwise identical to the
            // one-byte PUSH ES/CS/SS/DS handlers in `execute_stack_decoded` (0x06/0x0e/0x16/
            // 0x1e). 386 PRM: PUSH sreg with a 32-bit operand size (66h prefix or D=1 code
            // segment) decrements ESP by 4 and writes the 16-bit selector zero-extended to a
            // dword; with a 16-bit operand size it is the classic 2-byte push. Honor
            // `operand_size` here instead of hardcoding Word, matching the ES/SS/DS fix.
            // Same clock cost (2) as those.
            0xa0 => {
                self.push(
                    bus,
                    u32::from(self.registers.segment(SegmentIndex::Fs).selector),
                    operand_size,
                )?;
                Ok(self.charge(TimingClass::PushSeg))
            }
            0xa8 => {
                self.push(
                    bus,
                    u32::from(self.registers.segment(SegmentIndex::Gs).selector),
                    operand_size,
                )?;
                Ok(self.charge(TimingClass::PushSeg))
            }
            // POP FS / POP GS (0F A1 / 0F A9): mirrors POP ES/SS/DS (0x07/0x17/0x1f) -- pop a
            // selector off the stack, then run it through the same `load_segment`
            // descriptor-load path (which raises the identical #GP/#SS a bad or null selector
            // would on POP DS). 386 PRM: a 32-bit operand size pops a full dword and loads the
            // low 16 bits, discarding the upper half; a 16-bit operand size pops 2 bytes.
            // Same clock cost (7) as those.
            0xa1 => {
                let value = self.pop(bus, operand_size)? as u16;
                self.load_segment(bus, SegmentIndex::Fs, value)?;
                Ok(self.charge(TimingClass::PopSeg))
            }
            0xa9 => {
                let value = self.pop(bus, operand_size)? as u16;
                self.load_segment(bus, SegmentIndex::Gs, value)?;
                Ok(self.charge(TimingClass::PopSeg))
            }
            _ => Err(undefined_opcode()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn bit_string_op<B: CpuBus>(
        &mut self,
        bus: &mut B,
        op: u8,
        operand: RmOperand,
        raw_index: u32,
        operand_size: OperandSize,
        address_size: AddressSize,
        register_index: bool,
    ) -> ExecResult<()> {
        let bits = operand_size.bytes() * 8; // 16 or 32
        match operand {
            RmOperand::Register(index) => {
                let bit = raw_index & (bits - 1);
                let value = self.read_gpr_sized(index, operand_size);
                let (cf, new) = bit_op(op, value, bit);
                self.set_flag(FLAG_CF, cf);
                if op != 0 {
                    self.write_gpr_sized(index, operand_size, new);
                }
                Ok(())
            }
            RmOperand::Memory(mem) => {
                let (offset, bit) = if register_index {
                    // Signed bit-addressing: an index past the operand width walks to an
                    // adjacent operand in the bit string. div_euclid/rem_euclid give the
                    // floor block and the non-negative bit within it.
                    let signed = match operand_size {
                        OperandSize::Word => i32::from(raw_index as u16 as i16),
                        OperandSize::Dword => raw_index as i32,
                    };
                    let block = signed.div_euclid(bits as i32);
                    let bit = signed.rem_euclid(bits as i32) as u32;
                    let bytes = operand_size.bytes() as i32;
                    let offset = (mem.offset as i32).wrapping_add(block * bytes) as u32;
                    let offset = match address_size {
                        AddressSize::Word => offset & 0xffff,
                        AddressSize::Dword => offset,
                    };
                    (offset, bit)
                } else {
                    (mem.offset, raw_index & (bits - 1))
                };
                let value = self.read_memory_sized(
                    bus,
                    mem.segment,
                    offset,
                    operand_size,
                    BusAccessKind::DataRead,
                )?;
                let (cf, new) = bit_op(op, value, bit);
                self.set_flag(FLAG_CF, cf);
                if op != 0 {
                    self.write_memory_sized(
                        bus,
                        mem.segment,
                        offset,
                        operand_size,
                        new,
                        BusAccessKind::DataWrite,
                    )?;
                }
                Ok(())
            }
        }
    }
    /// The heterogeneous one-off block (task A14) through the decode/execute split. Each arm mirrors
    /// the former fused handler verbatim — same flag effects, same memory access, same clocks — but
    /// consumes the ModRM/operand/immediate `decode` pre-parsed (so the executor never re-fetches an
    /// instruction byte). CMPXCHG8B resolves its pre-decoded ModRM here; the
    /// no-operand 0F system/serializing/CPU-id ops (INVD/WBINVD/WRMSR/RDTSC/RDMSR/
    /// CPUID/BSWAP) read no instruction bytes, so they reuse the existing `execute_two_byte` leaf
    /// logic verbatim (it re-reads nothing for them). Dispatch is off the FULL u16 `insn.opcode`
    /// so a 0F low byte can never alias a single-byte opcode.
    pub(super) fn execute_misc_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
        work: &mut InstructionWork,
    ) -> ExecResult<CycleOutcome> {
        let operand_size = insn.operand_size;
        let address_size = insn.address_size;
        match insn.opcode {
            0x27 => {
                // DAA: decimal adjust AL after addition. OF is left undefined.
                let old_al = self.read_gpr8(0);
                let old_cf = self.flag(FLAG_CF);
                let mut al = old_al;
                self.set_flag(FLAG_CF, false);
                if (al & 0x0f) > 9 || self.flag(FLAG_AF) {
                    let (sum, carry) = al.overflowing_add(6);
                    al = sum;
                    self.set_flag(FLAG_CF, old_cf || carry);
                    self.set_flag(FLAG_AF, true);
                } else {
                    self.set_flag(FLAG_AF, false);
                }
                if old_al > 0x99 || old_cf {
                    al = al.wrapping_add(0x60);
                    self.set_flag(FLAG_CF, true); // the high correction always sets CF
                }
                self.write_gpr8(0, al);
                self.set_szp(u32::from(al), BusWidth::Byte);
                Ok(self.charge(TimingClass::DecimalAdjust))
            }
            0x2f => {
                // DAS: decimal adjust AL after subtraction. OF is left undefined.
                let old_al = self.read_gpr8(0);
                let old_cf = self.flag(FLAG_CF);
                let mut al = old_al;
                self.set_flag(FLAG_CF, false);
                if (al & 0x0f) > 9 || self.flag(FLAG_AF) {
                    let (diff, borrow) = al.overflowing_sub(6);
                    al = diff;
                    self.set_flag(FLAG_CF, old_cf || borrow);
                    self.set_flag(FLAG_AF, true);
                } else {
                    self.set_flag(FLAG_AF, false);
                }
                if old_al > 0x99 || old_cf {
                    al = al.wrapping_sub(0x60);
                    self.set_flag(FLAG_CF, true); // the high correction always sets CF
                }
                self.write_gpr8(0, al);
                self.set_szp(u32::from(al), BusWidth::Byte);
                Ok(self.charge(TimingClass::DecimalAdjust))
            }
            0x37 => {
                // AAA: ASCII adjust AL after addition. OF/SF/ZF/PF are left undefined.
                if (self.read_gpr8(0) & 0x0f) > 9 || self.flag(FLAG_AF) {
                    let ax = self.read_gpr16(0).wrapping_add(0x106);
                    self.write_gpr16(0, ax);
                    self.set_flag(FLAG_AF, true);
                    self.set_flag(FLAG_CF, true);
                } else {
                    self.set_flag(FLAG_AF, false);
                    self.set_flag(FLAG_CF, false);
                }
                let al = self.read_gpr8(0) & 0x0f;
                self.write_gpr8(0, al);
                Ok(self.charge(TimingClass::DecimalAdjust))
            }
            0x3f => {
                // AAS: ASCII adjust AL after subtraction. OF/SF/ZF/PF are left undefined.
                if (self.read_gpr8(0) & 0x0f) > 9 || self.flag(FLAG_AF) {
                    let ax = self.read_gpr16(0).wrapping_sub(6);
                    self.write_gpr16(0, ax.wrapping_sub(0x100));
                    self.set_flag(FLAG_AF, true);
                    self.set_flag(FLAG_CF, true);
                } else {
                    self.set_flag(FLAG_AF, false);
                    self.set_flag(FLAG_CF, false);
                }
                let al = self.read_gpr8(0) & 0x0f;
                self.write_gpr8(0, al);
                Ok(self.charge(TimingClass::DecimalAdjust))
            }
            0x69 => {
                // IMUL r, r/m, imm16/32: signed multiply of r/m by a full-width immediate.
                // `decode` parsed the ModRM/operand and fetched the immediate into `insn.imm`.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let src = self.read_operand_sized(bus, operand, operand_size)?;
                let result = self.imul_truncated(src, insn.imm, operand_size);
                self.write_gpr_sized(modrm.reg, operand_size, result);
                Ok(self.charge(TimingClass::ImulImm))
            }
            0x6b => {
                // IMUL r, r/m, imm8: signed multiply of r/m by a sign-extended byte immediate.
                // `decode` parsed the ModRM/operand and sign-extended the imm8 into `insn.imm`.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let src = self.read_operand_sized(bus, operand, operand_size)?;
                let result = self.imul_truncated(src, insn.imm, operand_size);
                self.write_gpr_sized(modrm.reg, operand_size, result);
                Ok(self.charge(TimingClass::ImulImm))
            }
            0x6c => {
                self.run_string(
                    bus,
                    work,
                    StringOp::Ins,
                    BusWidth::Byte,
                    insn.prefixes,
                    address_size,
                )?;
                // P3: the SETUP charge only. Under epoch 2 a `REP` form's per-element cost is
                // charged inside `string_step` (see `charge_string_port_element_core` for why it
                // cannot ride this return value); epoch 1 returns the flat 15 unchanged.
                Ok(clocks(self.string_port_setup_core_clocks(
                    bus,
                    false,
                    insn.prefixes.rep.is_some(),
                )))
            }
            0x6d => {
                self.run_string(
                    bus,
                    work,
                    StringOp::Ins,
                    operand_size.bus_width(),
                    insn.prefixes,
                    address_size,
                )?;
                // P3: the SETUP charge only. Under epoch 2 a `REP` form's per-element cost is
                // charged inside `string_step` (see `charge_string_port_element_core` for why it
                // cannot ride this return value); epoch 1 returns the flat 15 unchanged.
                Ok(clocks(self.string_port_setup_core_clocks(
                    bus,
                    false,
                    insn.prefixes.rep.is_some(),
                )))
            }
            0x6e => {
                self.run_string(
                    bus,
                    work,
                    StringOp::Outs,
                    BusWidth::Byte,
                    insn.prefixes,
                    address_size,
                )?;
                // P3: the SETUP charge only. Under epoch 2 a `REP` form's per-element cost is
                // charged inside `string_step` (see `charge_string_port_element_core` for why it
                // cannot ride this return value); epoch 1 returns the flat 14 unchanged.
                Ok(clocks(self.string_port_setup_core_clocks(
                    bus,
                    true,
                    insn.prefixes.rep.is_some(),
                )))
            }
            0x6f => {
                self.run_string(
                    bus,
                    work,
                    StringOp::Outs,
                    operand_size.bus_width(),
                    insn.prefixes,
                    address_size,
                )?;
                // P3: the SETUP charge only. Under epoch 2 a `REP` form's per-element cost is
                // charged inside `string_step` (see `charge_string_port_element_core` for why it
                // cannot ride this return value); epoch 1 returns the flat 14 unchanged.
                Ok(clocks(self.string_port_setup_core_clocks(
                    bus,
                    true,
                    insn.prefixes.rep.is_some(),
                )))
            }
            0xa8 => {
                // TEST AL, imm8: AND-for-flags, no write-back. `decode` fetched the imm8.
                let al = self.read_gpr8(0);
                self.alu(4, u32::from(al), insn.imm, BusWidth::Byte);
                Ok(self.charge(TimingClass::TestImmReg))
            }
            0xa9 => {
                // TEST AX/EAX, imm: AND-for-flags, no write-back. `decode` fetched the immediate.
                let acc = self.read_gpr_sized(0, operand_size);
                self.alu(4, acc, insn.imm, operand_size.bus_width());
                Ok(self.charge(TimingClass::TestImmReg))
            }
            0xd4 => {
                // AAM: AH = AL / imm8, AL = AL % imm8. OF/AF/CF undefined; SF/ZF/PF from AL.
                // `decode` fetched the imm8 base into `insn.imm`; a base of 0 raises #DE.
                let divisor = insn.imm as u8;
                if divisor == 0 {
                    return Err(divide_error());
                }
                let al = self.read_gpr8(0);
                self.write_gpr8(4, al / divisor);
                let rem = al % divisor;
                self.write_gpr8(0, rem);
                self.set_szp(u32::from(rem), BusWidth::Byte);
                Ok(self.charge(TimingClass::Aam))
            }
            0xd5 => {
                // AAD: AL = (AL + AH*imm8) & 0xff, AH = 0. OF/AF/CF undefined; SF/ZF/PF from AL.
                let multiplier = insn.imm as u8;
                let al = self.read_gpr8(0);
                let ah = self.read_gpr8(4);
                let result = al.wrapping_add(ah.wrapping_mul(multiplier));
                self.write_gpr8(0, result);
                self.write_gpr8(4, 0);
                self.set_szp(u32::from(result), BusWidth::Byte);
                Ok(self.charge(TimingClass::Aad))
            }
            0xd6 => {
                // SALC/SETALC (undocumented): AL = CF ? 0xFF : 0x00. Flags unaffected.
                let value = if self.flag(FLAG_CF) { 0xff } else { 0x00 };
                self.write_gpr8(0, value);
                Ok(self.charge(TimingClass::FlagOp))
            }
            0xd7 => {
                // XLAT: AL = [segment:(B)X + AL]. DS is the default, overridable; the 16-bit base
                // plus AL wraps inside the segment. Read from live registers at execute time.
                let segment = insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds);
                let al = u32::from(self.read_gpr8(0));
                let offset = match address_size {
                    AddressSize::Word => u32::from(self.read_gpr16(3).wrapping_add(al as u16)),
                    AddressSize::Dword => self.read_gpr32(3).wrapping_add(al),
                };
                let value = self.read_memory_u8(bus, segment, offset, BusAccessKind::DataRead)?;
                self.write_gpr8(0, value);
                Ok(self.charge(TimingClass::Xlat))
            }
            0xf4 => {
                // HLT: privileged on real 386+ (#GP(0) at CPL != 0). A V86 task is
                // always CPL 3, so a guest HLT under a V86 monitor faults here instead
                // of halting the whole machine; the monitor (if any) is responsible for
                // emulating the guest's halt semantics on the resulting #GP.
                self.require_cpl0()?;
                self.halted = true;
                Ok(CycleOutcome {
                    core_clocks: 5,
                    halted: true,
                })
            }
            // CMPXCHG8B (0F C7 /1): the ModRM was pre-parsed; resolve the m64 operand here and reuse
            // the same compare/store/load-and-set-ZF logic as the former fused arm. The register
            // form and any other group-7 /ext are #UD.
            0x0fc7 => {
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let mem = match operand {
                    RmOperand::Memory(mem) if modrm.reg == 1 => mem,
                    _ => {
                        return Err(InternalFault::Exception {
                            vector: 6,
                            error_code: None,
                        });
                    }
                };
                let current = self.read_qword(bus, mem)?;
                if current == self.read_edx_eax() {
                    let source =
                        (u64::from(self.read_gpr32(1)) << 32) | u64::from(self.read_gpr32(3));
                    self.write_qword(bus, mem, source)?;
                    self.set_flag(FLAG_ZF, true);
                } else {
                    self.set_edx_eax(current);
                    // Re-write the destination with its own value so the bus still sees a write on
                    // the unequal branch, matching the locked read-modify-write.
                    self.write_qword(bus, mem, current)?;
                    self.set_flag(FLAG_ZF, false);
                }
                Ok(self.charge(TimingClass::CmpXchg8b))
            }
            // The remaining 0F system/serializing/CPU-id/stack ops re-read no further instruction
            // bytes in `execute_two_byte`, so reuse that leaf logic verbatim: INVD/WBINVD (08/09),
            // WRMSR/RDTSC/RDMSR (30/31/32), PUSH FS/GS (A0/A8), CPUID
            // (A2), POP FS/GS (A1/A9), BSWAP (C8-CF). `decode` already read + gated the second
            // byte; this never re-reads it.
            0x0f08
            | 0x0f09
            | 0x0f30
            | 0x0f31
            | 0x0f32
            | 0x0fa0
            | 0x0fa1
            | 0x0fa2
            | 0x0fa8
            | 0x0fa9
            | 0x0fc8..=0x0fcf => self.execute_two_byte(bus, insn.opcode as u8, insn.operand_size),
            opcode => unreachable!("misc opcode {opcode:#x}"),
        }
    }
}

/// `BT` against `BTS`/`BTR`/`BTC`, by the sub-operation both encodings decode to
/// (`0` = `BT`).
///
/// `BT` reads and writes nothing, so it costs 8-9 clocks on both parts; the other
/// three are read/modify/writes that lock their memory form and cost 13. They
/// shared one `clocks(BIT_STRING_CORE_CLOCKS)` until the manual sourcing
/// separated them.
pub(crate) fn bit_string_class(op: u8) -> TimingClass {
    if op == 0 {
        TimingClass::BitTest
    } else {
        TimingClass::BitTestModify
    }
}

/// The charge class for `IRET`, by the mode it left and the mode it reached.
///
/// One flat `clocks(22)` covered all four rows before slice 8, which the census
/// scores as 4.4x under at real mode and **14.7x** under on a
/// different-privilege protected-mode return. Intel prices them apart: real 8,
/// protected same-level 10, protected different-level and V86 27 on a P5.
///
/// Real mode (`CR0.PE` clear) keeps `Iret`. Inside V86 the instruction is
/// `IretV86`. A protected-mode return that LANDS in V86, or that drops to a
/// lower privilege level, is `IretPmToV86` -- Intel gives those the same count.
/// Everything else is `IretPm`.
pub(crate) fn iret_class(
    protected: bool,
    from_v86: bool,
    from_cpl: u8,
    to_v86: bool,
    to_cpl: u8,
) -> TimingClass {
    if !protected {
        return TimingClass::Iret;
    }
    if from_v86 {
        return TimingClass::IretV86;
    }
    if to_v86 || to_cpl > from_cpl {
        return TimingClass::IretPmToV86;
    }
    TimingClass::IretPm
}

/// The charge class for `INT n`, by the mode it is taken in.
///
/// One flat `clocks(INT_IMM8_CORE_CLOCKS)` covered all three rows, which the
/// census scores at 5.2x under on its own units. Intel resolves the symbolic
/// `INT` of Table F-2 through the Interrupt Clock Counts Table, and the three
/// rows are far apart: real mode 11, a protected-mode trap gate to a different
/// level 40, and V86 through a trap gate to a different level **54 plus 12 on a
/// cache miss** -- the row
/// `dev_docs/2026-09-05-v86-port-io-timing-research.md` section 1.2 re-anchored
/// when it found the reflected-trip figure of 45 was taken from the unreachable
/// real-mode row.
pub(crate) fn int_n_class(protected: bool, v86: bool) -> TimingClass {
    if v86 {
        TimingClass::IntNV86
    } else if protected {
        TimingClass::IntNPm
    } else {
        TimingClass::IntN
    }
}

impl CpuGsw {
    /// The class a far transfer earns, given the real-mode default its opcode
    /// would otherwise charge.
    ///
    /// `far_system_transfer` records a gate / TSS / protected class when the
    /// transfer went through a system descriptor; a real-mode transfer records
    /// nothing and keeps its own row. Taking rather than peeking is what stops a
    /// gate transfer's class from being read a second time by the next
    /// real-mode `RETF`.
    fn far_transfer_class(&mut self, real_mode_default: TimingClass) -> TimingClass {
        self.pending_transfer_class
            .take()
            .unwrap_or(real_mode_default)
    }
}
