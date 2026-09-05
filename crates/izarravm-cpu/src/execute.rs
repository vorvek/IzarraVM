// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::timing_class::TimingClass;

impl CpuGsw {
    /// Stage A executor. For the opcodes converted to the split (the whole ALU block), execute from
    /// the pre-decoded `operand`/`modrm`/`imm` (resolving the addressing-mode descriptor against the
    /// live registers). Every other opcode continues into the shared fused dispatch (which re-reads
    /// its ModRM/immediates from the post-opcode eip) so behavior is byte-for-byte unchanged.
    pub(super) fn execute_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
        work: &mut InstructionWork,
    ) -> ExecResult<CycleOutcome> {
        // Dispatch on the group `decode` already resolved and stored, so the parse side and the
        // execute side can never drift out of sync and `route_group` runs only once per instruction.
        match insn.group {
            // The whole ALU block (ADD/OR/ADC/SBB/AND/SUB/XOR/CMP, forms 0-5) runs through the
            // split executor, consuming the ModRM/immediate `decode` pre-parsed.
            DecodeGroup::Alu => self.execute_alu_decoded(insn, bus),
            // The single-byte data-movement block runs through its split executor, consuming the
            // ModRM/operand/immediate `decode` pre-parsed.
            DecodeGroup::DataMove => self.execute_datamove_decoded(insn, bus),
            // The stack block runs through its split executor, consuming the ModRM/immediate
            // `decode` pre-parsed.
            DecodeGroup::Stack => self.execute_stack_decoded(insn, bus),
            // The arithmetic /ext groups 1-4 run through their split executor, consuming the
            // ModRM (whose `reg` is the sub-op) and the conditional immediate `decode` pre-parsed.
            DecodeGroup::Group => self.execute_group_decoded(insn, bus),
            // The relative-displacement + loop control-flow block runs through its split executor,
            // consuming the relative displacement `decode` pre-parsed (eip is already at the
            // instruction end, so the eip-relative target math matches the fused path).
            DecodeGroup::Branch => self.execute_branch_decoded(insn, bus),
            // The far/indirect/RET/INT control-flow block + 0xff group 5 runs through its split
            // executor, consuming the far-pointer/imm16/imm8 `decode` pre-parsed (for 0x9a/0xea/0xc2/
            // 0xca/0xcd) or the pre-parsed ModRM/descriptor (for 0xff), and reusing the existing
            // far-call/far-jump/ret/retf/interrupt/IRET/inc_dec/push helpers verbatim.
            DecodeGroup::ControlFlow => {
                self.execute_control_flow_decoded(insn, bus, &mut work.committed)
            }
            // The flags + misc register block (TEST r/m,reg, INC/DEC reg, CBW/CWD, SAHF/LAHF, and
            // the single flag-bit ops) runs through its split executor, consuming the pre-parsed
            // ModRM/operand for TEST and running the same flag/register logic as the fused path.
            DecodeGroup::FlagsMisc => self.execute_flags_misc_decoded(insn, bus),
            // The string-operation block (MOVS/CMPS/STOS/LODS/SCAS, byte and word/dword) runs through
            // its split executor, a thin call to the existing `run_string` helper with the pre-decoded
            // `insn.prefixes` (REP/REPNE + segment override) passed through — the REP loop, ZF
            // termination, DF direction, and per-iteration clocks all stay in `run_string` unchanged.
            DecodeGroup::StringOps => self.execute_string_decoded(insn, bus, work),
            // The port I/O block (IN AL/AX/EAX, OUT AL/AX/EAX, both imm8-port and DX-port forms)
            // runs through its split executor, which calls `bus.read_io`/`bus.write_io` on the same
            // port-dispatch path as the fused arms — so `io_touched` is set exactly as before.
            DecodeGroup::PortIo => self.execute_port_io_decoded(insn, bus),
            // The two-byte bit-manipulation block (BT/BTS/BTR/BTC, BSF/BSR, SHLD/SHRD, CMPXCHG,
            // XADD) runs through its split executor, consuming the pre-decoded ModRM/operand (and
            // the pre-fetched imm8 for 0F BA/A4/AC) and reusing `bit_string_op`/`double_shift`/
            // `alu_sub`/`alu_add` verbatim so the bit-addressing and flag logic stays in one place.
            DecodeGroup::BitManip => self.execute_bitmanip_decoded(insn, bus),
            // SETcc and two-operand IMUL run through their split executor, consuming the
            // pre-decoded ModRM/operand and reusing `self.condition` and
            // `self.imul_truncated`.
            DecodeGroup::CondMove => self.execute_condmove_decoded(insn, bus),
            // The system / descriptor-table / segment block (0F 00/01/02/03/06/20/22, BOUND,
            // LES/LDS) runs through its split executor, consuming the pre-decoded ModRM/operand and
            // reusing the existing CR/segment/descriptor leaf helpers verbatim so the TLB and
            // code-cache invalidation hooks fire exactly as before.
            DecodeGroup::SystemSeg => {
                self.execute_system_seg_decoded(insn, bus, &mut work.committed)
            }
            // The x87 FPU block (0xD8-0xDF) + WAIT/FWAIT (0x9B) runs through its split executor: a
            // thin wrapper that reproduces the fused pending-#MF gate, then resolves the pre-decoded
            // ModRM/operand (for the memory forms) and calls the existing `execute_fpu_register` /
            // `execute_fpu_memory` verbatim — the entire x87 stack/control/status logic stays in
            // those leaf helpers unchanged.
            DecodeGroup::Fpu => self.execute_fpu_decoded(insn, bus),
            // The heterogeneous one-off block (BCD adjust, AAM/AAD, SALC/XLAT, TEST imm, three-
            // operand IMUL, INS/OUTS, HLT, and the no-operand 0F system/serializing/CPU-id ops,
            // and CMPXCHG8B) runs through its split executor, consuming the pre-decoded
            // ModRM/operand/immediate and reusing the existing BCD/`imul_truncated`/`run_string`/
            // CPUID/RDTSC/halt leaf logic verbatim.
            DecodeGroup::Misc => self.execute_misc_decoded(insn, bus, work),
            DecodeGroup::TwoByteFallback => {
                // Un-converted two-byte (0F) opcode. `decode` already read + charged the second
                // byte and applied the ISA gate, folding it into `insn.opcode` as 0x0F00 | second.
                // Hand the second byte to `execute_two_byte`; every opcode it still handles reads no
                // further instruction bytes (the second byte is never re-read). PUSH/POP FS/GS do
                // touch the stack, so `bus` is passed through.
                self.execute_two_byte(bus, insn.opcode as u8, insn.operand_size)
            }
            DecodeGroup::Fallback => {
                // Fallback is now a pure dead-end: after Stage A every IMPLEMENTED single-byte opcode
                // is routed to a dedicated split group, so the only opcodes that land here are the
                // genuinely-unimplemented ones (currently 0xF1 ICEBP) and, as a decode-bug guard,
                // any prefix byte `read_prefixes` failed to consume. Raise the architectural #UD
                // (vector 6); `deliver_exception` traces CS:IP/bytes/CR0/EFLAGS for it when #UD
                // tracing is enabled, so the diagnostic detail the old `UnsupportedOpcode` error
                // fields carried is not lost. `execute_two_byte` still STAYS — it is the leaf for
                // the no-operand 0F ops (`execute_misc_decoded`) and the TwoByteFallback #UD handler
                // above — but the single-byte fused dispatch is gone.
                Err(self.unsupported_single_byte_opcode())
            }
        }
    }

    /// Raise the #UD for a single-byte opcode that the decode/execute split does not implement.
    /// After Stage A every IMPLEMENTED opcode is routed by `route_group` to a dedicated split group,
    /// so the only opcodes that reach here (via the `DecodeGroup::Fallback` arm of `execute_decoded`)
    /// are the genuinely-unimplemented ones, currently 0xF1 (ICEBP), plus any prefix byte that
    /// `read_prefixes` did not consume (which would be a decode bug). All produce the same
    /// `UnsupportedOpcode` the fused path produced: `opcode` is the byte, `cs` the current selector,
    /// and `eip` the instruction's start (the byte before any ModRM/immediate would sit), matching
    /// the legacy error fields exactly.
    fn unsupported_single_byte_opcode(&self) -> InternalFault {
        undefined_opcode()
    }

    /// The entire ALU block (ADD/OR/ADC/SBB/AND/SUB/XOR/CMP across all six forms) through the
    /// decode/execute split. This is the canonical split executor: `op`/`form`/`write_back` are
    /// derived from the opcode exactly as the former fused ALU handler did, the r/m operand for
    /// forms 0-3 is resolved from the pre-decoded descriptor (so the EA is recomputed against the
    /// live registers each call), and the immediate for forms 4-5 is taken from `insn.imm` (decode
    /// already fetched and charged it, so the executor must NOT re-fetch). `self.alu` is reused
    /// verbatim so the flag logic lives in exactly one place.
    fn execute_alu_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        let opcode = insn.opcode as u8;
        let op = (opcode >> 3) & 0x07;
        let form = opcode & 0x07;
        let write_back = op != 7; // CMP computes flags only
        let operand_size = insn.operand_size;

        match form {
            0 => {
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let a = u32::from(self.read_operand_u8(bus, operand)?);
                let b = u32::from(self.read_gpr8(modrm.reg));
                let result = self.alu(op, a, b, BusWidth::Byte) as u8;
                if write_back {
                    self.write_operand_u8(bus, operand, result)?;
                }
            }
            1 => {
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let a = self.read_operand_sized(bus, operand, operand_size)?;
                let b = self.read_gpr_sized(modrm.reg, operand_size);
                let result = self.alu(op, a, b, operand_size.bus_width());
                if write_back {
                    self.write_operand_sized(bus, operand, operand_size, result)?;
                }
            }
            2 => {
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let a = u32::from(self.read_gpr8(modrm.reg));
                let b = u32::from(self.read_operand_u8(bus, operand)?);
                let result = self.alu(op, a, b, BusWidth::Byte) as u8;
                if write_back {
                    self.write_gpr8(modrm.reg, result);
                }
            }
            3 => {
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let a = self.read_gpr_sized(modrm.reg, operand_size);
                let b = self.read_operand_sized(bus, operand, operand_size)?;
                let result = self.alu(op, a, b, operand_size.bus_width());
                if write_back {
                    self.write_gpr_sized(modrm.reg, operand_size, result);
                }
            }
            4 => {
                // imm8 was fetched + charged by `decode`; consume it from the decoded instruction.
                let imm = insn.imm;
                let a = u32::from(self.read_gpr8(0));
                let result = self.alu(op, a, imm, BusWidth::Byte) as u8;
                if write_back {
                    self.write_gpr8(0, result);
                }
            }
            5 => {
                // imm16/32 was fetched + charged by `decode`; consume it from the decoded form.
                let imm = insn.imm;
                let a = self.read_gpr_sized(0, operand_size);
                let result = self.alu(op, a, imm, operand_size.bus_width());
                if write_back {
                    self.write_gpr_sized(0, operand_size, result);
                }
            }
            _ => unreachable!("alu form {form}"),
        }

        Ok(self.charge(alu_class(form, write_back, insn)))
    }

    /// The data-movement block (MOV/LEA/XCHG and their immediate/moffs/Sreg forms, plus the two-byte
    /// MOVZX/MOVSX) through the decode/execute split. Each arm mirrors the former fused handler
    /// verbatim — same operand wiring, same segment-load path for 0x8e, same XCHG read/write order,
    /// same clocks — but consumes the ModRM/operand/immediate `decode` already parsed (so the
    /// executor never re-fetches an instruction byte). Memory operands resolve from the pre-decoded
    /// descriptor, so the effective address is recomputed against the live registers each call.
    fn execute_datamove_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        let operand_size = insn.operand_size;

        // Two-byte forms first: `insn.opcode as u8` below would alias 0x0Fb6/b7/be/bf onto the
        // single-byte MOV r,imm opcodes (0xb6/b7/be/bf), so the 0F forms must be dispatched off the
        // full u16. MOVZX zero-extends, MOVSX sign-extends, an 8- or 16-bit source into the
        // destination register at the operand size; none touch flags. Same clocks (3) and operand
        // wiring as the former `execute_two_byte` arms, but from the pre-decoded operand.
        match insn.opcode {
            0x0fb6 => {
                // MOVZX r, r/m8: zero-extend the byte into the destination at the operand width.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = u32::from(self.read_operand_u8(bus, operand)?);
                self.write_gpr_sized(modrm.reg, operand_size, value);
                return Ok(self.charge(TimingClass::MovExtend));
            }
            0x0fb7 => {
                // MOVZX r, r/m16: zero-extend the word into the destination.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = self.read_operand_sized(bus, operand, OperandSize::Word)?;
                self.write_gpr_sized(modrm.reg, operand_size, value);
                return Ok(self.charge(TimingClass::MovExtend));
            }
            0x0fbe => {
                // MOVSX r, r/m8: sign-extend the byte into the destination.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = sign_extend_u8(self.read_operand_u8(bus, operand)?);
                self.write_gpr_sized(modrm.reg, operand_size, value);
                return Ok(self.charge(TimingClass::MovExtend));
            }
            0x0fbf => {
                // MOVSX r, r/m16: sign-extend the word into the destination.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value =
                    self.read_operand_sized(bus, operand, OperandSize::Word)? as i16 as i32 as u32;
                self.write_gpr_sized(modrm.reg, operand_size, value);
                return Ok(self.charge(TimingClass::MovExtend));
            }
            _ => {}
        }

        let opcode = insn.opcode as u8;

        match opcode {
            0x86 => {
                // XCHG r/m8, r8. Cross-write; the operand was resolved once in decode so the
                // displacement is not re-fetched.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let rm = self.read_operand_u8(bus, operand)?;
                let reg = self.read_gpr8(modrm.reg);
                self.write_operand_u8(bus, operand, reg)?;
                self.write_gpr8(modrm.reg, rm);
                Ok(self.charge(TimingClass::Xchg))
            }
            0x87 => {
                // XCHG r/m16/32, r16/32. Cross-write.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let rm = self.read_operand_sized(bus, operand, operand_size)?;
                let reg = self.read_gpr_sized(modrm.reg, operand_size);
                self.write_operand_sized(bus, operand, operand_size, reg)?;
                self.write_gpr_sized(modrm.reg, operand_size, rm);
                Ok(self.charge(TimingClass::Xchg))
            }
            0x88 => {
                // MOV r/m8, r8.
                let modrm = insn.modrm.expect("MOV r/m8,r8 decoded with a ModRM");
                let value = self.read_gpr8(modrm.reg);
                match insn.operand.expect("MOV r/m8,r8 decoded with an operand") {
                    DecodedOperand::Reg(index) => self.write_gpr8(index, value),
                    DecodedOperand::Mem(addr) => {
                        let memory = self.resolve_memory_addr_mode(&addr);
                        self.write_memory_u8(
                            bus,
                            memory.segment,
                            memory.offset,
                            value,
                            BusAccessKind::DataWrite,
                        )?;
                    }
                }
                Ok(self.charge(TimingClass::MovMemReg))
            }
            0x89 => {
                // MOV r/m16/32, r16/32.
                let modrm = insn.modrm.expect("MOV r/m,r decoded with a ModRM");
                let value = self.read_gpr_sized(modrm.reg, operand_size);
                match insn.operand.expect("MOV r/m,r decoded with an operand") {
                    DecodedOperand::Reg(index) => {
                        self.write_gpr_sized(index, operand_size, value);
                    }
                    DecodedOperand::Mem(addr) => {
                        let memory = self.resolve_memory_addr_mode(&addr);
                        self.write_memory_sized(
                            bus,
                            memory.segment,
                            memory.offset,
                            operand_size,
                            value,
                            BusAccessKind::DataWrite,
                        )?;
                    }
                }
                Ok(self.charge(TimingClass::MovMemReg))
            }
            0x8a => {
                // MOV r8, r/m8.
                let modrm = insn.modrm.expect("MOV r8,r/m8 decoded with a ModRM");
                let value = match insn.operand.expect("MOV r8,r/m8 decoded with an operand") {
                    DecodedOperand::Reg(index) => self.read_gpr8(index),
                    DecodedOperand::Mem(addr) => {
                        let memory = self.resolve_memory_addr_mode(&addr);
                        self.read_memory_u8(
                            bus,
                            memory.segment,
                            memory.offset,
                            BusAccessKind::DataRead,
                        )?
                    }
                };
                self.write_gpr8(modrm.reg, value);
                Ok(self.charge(TimingClass::MovRegMem))
            }
            0x8b => {
                // MOV r16/32, r/m16/32.
                let modrm = insn.modrm.expect("MOV r,r/m decoded with a ModRM");
                let value = match insn.operand.expect("MOV r,r/m decoded with an operand") {
                    DecodedOperand::Reg(index) => self.read_gpr_sized(index, operand_size),
                    DecodedOperand::Mem(addr) => {
                        let memory = self.resolve_memory_addr_mode(&addr);
                        self.read_memory_sized(
                            bus,
                            memory.segment,
                            memory.offset,
                            operand_size,
                            BusAccessKind::DataRead,
                        )?
                    }
                };
                self.write_gpr_sized(modrm.reg, operand_size, value);
                Ok(self.charge(TimingClass::MovRegMem))
            }
            0x8c => {
                // MOV r/m16, Sreg. Always a word store regardless of operand size.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = u32::from(self.segment_from_reg_field(modrm.reg).selector);
                self.write_operand_sized(bus, operand, OperandSize::Word, value)?;
                // Named rather than a literal for the reason the `0x8f` arm's charge is: the
                // memory form is an `InterpretOne` call-out row, and its budget bound
                // (`INTERPRET_ONE_MAX_CORE_CLOCKS`) and this arm must charge the same number.
                Ok(self.charge(TimingClass::MovRegSreg))
            }
            0x8d => {
                // LEA reg, m: load the effective address, not the memory it points at. mod=3 (a
                // register r/m) is an invalid encoding and faults #UD.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                match operand {
                    RmOperand::Memory(mem) => {
                        self.write_gpr_sized(modrm.reg, operand_size, mem.offset);
                        Ok(self.charge(TimingClass::Lea))
                    }
                    RmOperand::Register(_) => Err(InternalFault::Exception {
                        vector: 6,
                        error_code: None,
                    }),
                }
            }
            0x8e => {
                // MOV Sreg, r/m16. Reads a word r/m, then loads the segment register through the
                // shared segment-load path (which can fault and, in protected mode, reload the
                // descriptor). CS (reg=1) and reg>5 are invalid and #GP, matching the fused handler.
                // Loading SS this way arms the one-instruction interrupt shadow (386 PRM 11-16).
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = self.read_operand_sized(bus, operand, OperandSize::Word)?;
                let segment = match modrm.reg {
                    0 => SegmentIndex::Es,
                    2 => SegmentIndex::Ss,
                    3 => SegmentIndex::Ds,
                    4 => SegmentIndex::Fs,
                    5 => SegmentIndex::Gs,
                    _ => {
                        // Not a bad-descriptor fault (no selector to blame): the illegal
                        // encoding is the destination register field itself (CS or reg>5),
                        // so the error code is 0, not a selector index.
                        return Err(InternalFault::Exception {
                            vector: 13,
                            error_code: Some(0),
                        });
                    }
                };
                // Design review 10.1 M5: does a real guest's MOV SS leave the record alone?
                // Behind the barrier-census gate since the S4 review round, so a plain build pays
                // one predicate on an already-loaded flag rather than a segment compare and a
                // record read on every execution of this arm.
                let ss_before = (self.ss_load_census_active() && segment == SegmentIndex::Ss)
                    .then(|| self.registers.segment(SegmentIndex::Ss));
                self.load_segment_arming_ss_shadow(bus, segment, value as u16)?;
                if let Some(before) = ss_before {
                    self.note_ss_load_record(before);
                }
                // Named because FS, GS and every memory form are `InterpretOne` call-out rows:
                // their budget bound and this arm must charge the same number.
                Ok(self.charge(TimingClass::MovSregReg))
            }
            0x90 => {
                // NOP (XCHG (E)AX, (E)AX): a no-op with the same clocks as the other XCHG-acc forms.
                Ok(self.charge(TimingClass::Nop))
            }
            0x91..=0x97 => {
                // XCHG (E)AX, reg. The register index is the low 3 opcode bits.
                let reg = opcode & 7;
                let acc = self.read_gpr_sized(0, operand_size);
                let other = self.read_gpr_sized(reg, operand_size);
                self.write_gpr_sized(0, operand_size, other);
                self.write_gpr_sized(reg, operand_size, acc);
                Ok(self.charge(TimingClass::Xchg))
            }
            0xa0 => {
                // MOV AL, moffs8: byte form, ignores the operand-size prefix, flags untouched. The
                // moffs displacement was captured into `imm` by decode.
                let value = self.read_memory_u8(
                    bus,
                    insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                    insn.imm,
                    BusAccessKind::DataRead,
                )?;
                self.write_gpr8(0, value);
                Ok(self.charge(TimingClass::MovAccMoffs))
            }
            0xa1 => {
                // MOV (E)AX, moffs.
                let value = self.read_memory_sized(
                    bus,
                    insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                    insn.imm,
                    operand_size,
                    BusAccessKind::DataRead,
                )?;
                self.write_gpr_sized(0, operand_size, value);
                Ok(self.charge(TimingClass::MovAccMoffs))
            }
            0xa2 => {
                // MOV moffs8, AL.
                let value = self.read_gpr8(0);
                self.write_memory_u8(
                    bus,
                    insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                    insn.imm,
                    value,
                    BusAccessKind::DataWrite,
                )?;
                Ok(self.charge(TimingClass::MovAccMoffs))
            }
            0xa3 => {
                // MOV moffs, (E)AX.
                let value = self.read_gpr_sized(0, operand_size);
                self.write_memory_sized(
                    bus,
                    insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                    insn.imm,
                    operand_size,
                    value,
                    BusAccessKind::DataWrite,
                )?;
                Ok(self.charge(TimingClass::MovAccMoffs))
            }
            0xb0..=0xb7 => {
                // MOV r8, imm8. The immediate was captured into `imm` by decode.
                self.write_gpr8(opcode - 0xb0, insn.imm as u8);
                Ok(self.charge(TimingClass::MovImmReg))
            }
            0xb8..=0xbf => {
                // MOV r16/32, imm16/32.
                self.write_gpr_sized(opcode - 0xb8, operand_size, insn.imm);
                Ok(self.charge(TimingClass::MovImmReg))
            }
            0xc6 => {
                // MOV r/m8, imm8 (group 11). Only reg=000 is defined; decode left `operand`/`imm`
                // unparsed for any other reg field, so re-raise the identical group-opcode error.
                let modrm = insn.modrm.expect("group-11 form decoded with a ModRM");
                if modrm.reg != 0 {
                    return Err(undefined_opcode());
                }
                let (_, operand) = self.resolve_decoded_modrm_operand(insn);
                self.write_operand_u8(bus, operand, insn.imm as u8)?;
                Ok(self.charge(TimingClass::MovImmMem))
            }
            0xc7 => {
                // MOV r/m16/32, imm16/32 (group 11). Same reg=000 gate as 0xc6.
                let modrm = insn.modrm.expect("group-11 form decoded with a ModRM");
                if modrm.reg != 0 {
                    return Err(undefined_opcode());
                }
                let (_, operand) = self.resolve_decoded_modrm_operand(insn);
                self.write_operand_sized(bus, operand, operand_size, insn.imm)?;
                Ok(self.charge(TimingClass::MovImmMem))
            }
            _ => unreachable!("data-move opcode {opcode:#x}"),
        }
    }

    /// Stack-block executor: PUSH/POP reg, PUSH/POP seg, PUSH imm, POP r/m, PUSHA/POPA,
    /// PUSHF/POPF, ENTER/LEAVE.
    ///
    /// Each arm mirrors the former fused handler verbatim (same push/pop helpers, same flag
    /// masking via `check_v86_iopl` + `load_flags`, same PUSHA SP-snapshot, same ENTER
    /// nesting frame-copy, same LEAVE SP/BP semantics), but consumes the ModRM/immediate
    /// `decode` already parsed so the executor never re-fetches an instruction byte.
    fn execute_stack_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        let opcode = insn.opcode as u8;
        let operand_size = insn.operand_size;

        match opcode {
            0x06 => {
                // PUSH ES. 386 PRM: with a 32-bit operand size (D=1 code segment or a 66h
                // prefix), PUSH sreg decrements ESP by 4 and writes the 16-bit selector
                // zero-extended to a dword; with a 16-bit operand size it is the classic
                // 2-byte push. `u32::from(selector)` already zero-extends, so honoring
                // `operand_size` here (instead of hardcoding Word) covers both cases.
                self.push(
                    bus,
                    u32::from(self.registers.segment(SegmentIndex::Es).selector),
                    operand_size,
                )?;
                Ok(self.charge(TimingClass::PushSeg))
            }
            0x07 => {
                // POP ES. 386 PRM: a 32-bit operand size pops a full dword and loads the
                // low 16 bits, discarding the upper half; a 16-bit operand size pops 2 bytes.
                let value = self.pop(bus, operand_size)? as u16;
                self.load_segment(bus, SegmentIndex::Es, value)?;
                Ok(self.charge(TimingClass::PopSeg))
            }
            0x0e => {
                // PUSH CS. Same 386 PRM operand-size rule as PUSH ES above.
                self.push(
                    bus,
                    u32::from(self.registers.segment(SegmentIndex::Cs).selector),
                    operand_size,
                )?;
                Ok(self.charge(TimingClass::PushSeg))
            }
            0x16 => {
                // PUSH SS. Same 386 PRM operand-size rule as PUSH ES above.
                self.push(
                    bus,
                    u32::from(self.registers.segment(SegmentIndex::Ss).selector),
                    operand_size,
                )?;
                Ok(self.charge(TimingClass::PushSeg))
            }
            0x17 => {
                // POP SS. Arms the one-instruction interrupt shadow like MOV SS (386 PRM 11-16),
                // so a following POP (E)SP is guaranteed to run before any interrupt is taken.
                // Same 386 PRM operand-size rule as POP ES above.
                let value = self.pop(bus, operand_size)? as u16;
                // The M5 measurement's other half, behind the same gate. See the `0x8e` arm.
                let before = self
                    .ss_load_census_active()
                    .then(|| self.registers.segment(SegmentIndex::Ss));
                self.load_segment_arming_ss_shadow(bus, SegmentIndex::Ss, value)?;
                if let Some(before) = before {
                    self.note_ss_load_record(before);
                }
                // NAMED, because `POP_SS_CORE_CLOCKS` is what the block budget bound
                // (`INTERPRET_ONE_MAX_CORE_CLOCKS`) folds for this row: a literal here and a
                // constant there are two numbers that can drift apart silently.
                Ok(self.charge(TimingClass::PopSs))
            }
            0x1e => {
                // PUSH DS. Same 386 PRM operand-size rule as PUSH ES above.
                self.push(
                    bus,
                    u32::from(self.registers.segment(SegmentIndex::Ds).selector),
                    operand_size,
                )?;
                Ok(self.charge(TimingClass::PushSeg))
            }
            0x1f => {
                // POP DS. Same 386 PRM operand-size rule as POP ES above.
                let value = self.pop(bus, operand_size)? as u16;
                self.load_segment(bus, SegmentIndex::Ds, value)?;
                Ok(self.charge(TimingClass::PopSeg))
            }
            0x50..=0x57 => {
                let index = opcode - 0x50;
                let value = self.read_gpr_sized(index, operand_size);
                self.push(bus, value, operand_size)?;
                Ok(self.charge(TimingClass::PushReg))
            }
            0x58..=0x5f => {
                let index = opcode - 0x58;
                let value = self.pop(bus, operand_size)?;
                self.write_gpr_sized(index, operand_size, value);
                Ok(self.charge(TimingClass::PopReg))
            }
            0x60 => {
                self.push_all_gpr(bus, operand_size)?;
                Ok(self.charge(TimingClass::PushAll))
            }
            0x61 => {
                self.pop_all_gpr(bus, operand_size)?;
                Ok(self.charge(TimingClass::PopAll))
            }
            0x68 => {
                // PUSH imm16/32: `decode` fetched the full-width immediate into `insn.imm`.
                self.push(bus, insn.imm, operand_size)?;
                Ok(self.charge(TimingClass::PushImm))
            }
            0x6a => {
                // PUSH imm8: sign-extend the byte (stored in `insn.imm`) to the operand size.
                let value = sign_extend_u8(insn.imm as u8);
                self.push(bus, value, operand_size)?;
                Ok(self.charge(TimingClass::PushImm))
            }
            0x8f => {
                // POP r/m16/32 (group 1A). Only reg=000 is defined; other reg values are an
                // illegal encoding. `decode` left `operand` as None for any reg != 0, so
                // re-raise the identical error with the same bytes consumed.
                let modrm = insn.modrm.expect("POP r/m decoded with a ModRM");
                if modrm.reg != 0 {
                    return Err(undefined_opcode());
                }
                // The 386 PRM's POP pseudocode ("DEST <- (SS:ESP); ESP <- ESP + 4") does not
                // say when the destination EA is computed relative to the increment. The
                // modern Intel SDM is explicit: "the POP instruction computes the effective
                // address of the operand after it increments the ESP register." Real silicon
                // agrees: JEMM's DisableInts `pop [esp+4]` gadget only works if the EA is
                // resolved from the POST-increment (E)SP. `resolve_decoded_modrm_operand`
                // reads live GPRs, so it must run after `self.pop`, not before (the PUSH
                // r/m32-with-ESP-base analog -- see the PUSH SP note in the same manual -- is
                // the mirror-image caution: that source read happens before the decrement, so
                // PUSH keeps its EA-then-op order and only POP's is swapped here).
                let esp_before = self.registers.esp();
                let value = self.pop(bus, operand_size)?;
                let (_, operand) = self.resolve_decoded_modrm_operand(insn);
                if let Err(err) = self.write_operand_sized(bus, operand, operand_size, value) {
                    // The pop already advanced ESP; a faulting write must leave the
                    // instruction restartable, so undo that advance before propagating,
                    // matching the IRET esp_before fault-unwind convention.
                    self.registers.set_esp(esp_before);
                    return Err(err);
                }
                Ok(self.charge(TimingClass::PopMem))
            }
            0x9c => {
                // PUSHF / PUSHFD. The low 16 flag bits push the same in both forms. The
                // dword form additionally carries the persona's writable high flags. RF and
                // VM are masked to 0. operand_size drives whether push writes 2 or 4 bytes.
                self.check_v86_iopl()?;
                // Settle any deferred arithmetic flags so the pushed image has live CF/PF/AF/ZF/SF/OF.
                self.materialize_flags();
                let value = match operand_size {
                    OperandSize::Word => self.registers.eflags & 0xffff,
                    OperandSize::Dword => {
                        let high = match self.persona() {
                            CpuPersona::I386 => 0,
                            CpuPersona::I486 => FLAG_AC,
                            CpuPersona::I586 => FLAG_AC | FLAG_ID,
                        };
                        self.registers.eflags & (0xffff | high)
                    }
                };
                self.push(bus, value, operand_size)?;
                Ok(self.charge(TimingClass::PushFlags))
            }
            0x9d => {
                // POPF / POPFD: load the popped image through the shared flag-load.
                self.check_v86_iopl()?;
                let value = self.pop(bus, operand_size)?;
                self.load_flags(value, operand_size, false);
                Ok(self.charge(TimingClass::PopFlags))
            }
            0xc8 => {
                // ENTER imm16, imm8: build a stack frame. NestingLevel (already masked to 5
                // bits by `decode`) is taken from `insn.imm2`; frame size from `insn.imm`.
                let alloc = insn.imm as u16;
                let level = insn.imm2; // already & 0x1f from decode
                let size = operand_size.bytes();
                let frame_bp = self.read_gpr_sized(5, operand_size);
                self.push(bus, frame_bp, operand_size)?;
                // frame-ptr <- eSP (386 PRM 17-62): the saved stack pointer is read at
                // StackAddrSize (SS.B), not the operand size -- on a B=0 stack it is the
                // 16-bit SP, even for an ENTER with a 32-bit operand size.
                let frame_temp = if self.stack_is_32bit() {
                    self.registers.esp()
                } else {
                    u32::from(self.read_gpr16(4))
                };
                if level > 0 {
                    // Copy the display: the saved frame pointers of the enclosing scopes.
                    let mut bp = self.read_gpr_sized(5, operand_size);
                    for _ in 1..level {
                        bp = bp.wrapping_sub(size) & operand_size.mask();
                        self.write_gpr_sized(5, operand_size, bp);
                        let display = self.read_memory_sized(
                            bus,
                            SegmentIndex::Ss,
                            bp,
                            operand_size,
                            BusAccessKind::DataRead,
                        )?;
                        self.push(bus, display, operand_size)?;
                    }
                    self.push(bus, frame_temp, operand_size)?;
                }
                self.write_gpr_sized(5, operand_size, frame_temp);
                // The final allocation is an implicit stack reference (no memory
                // access here, just the SP/ESP update), so it follows SS.B like
                // push/pop -- not the operand size.
                if self.stack_is_32bit() {
                    let esp = self.registers.esp().wrapping_sub(u32::from(alloc));
                    self.registers.set_esp(esp);
                } else {
                    let sp = self.read_gpr16(4).wrapping_sub(alloc);
                    self.write_gpr16(4, sp);
                }
                Ok(self.charge(TimingClass::Enter))
            }
            0xc9 => {
                // LEAVE: (E)SP <- (E)BP, then (E)BP <- pop (386 PRM 17-96). Both the
                // read of BP/EBP and the write to SP/ESP are keyed on SS.B, not operand
                // size: a B=1 stack moves the FULL EBP into the FULL ESP even for a
                // 16-bit operand size (StackAddrSize=32 => ESP <- EBP, no truncation);
                // a B=0 stack moves only BP into SP and leaves ESP's high word alone.
                // The operand size only selects BP vs EBP for the popped frame pointer.
                if self.stack_is_32bit() {
                    let frame = self.read_gpr32(5);
                    self.registers.set_esp(frame);
                } else {
                    let frame = self.read_gpr16(5);
                    self.write_gpr16(4, frame);
                }
                let saved = self.pop(bus, operand_size)?;
                self.write_gpr_sized(5, operand_size, saved);
                Ok(self.charge(TimingClass::Leave))
            }
            _ => unreachable!("stack opcode {opcode:#x}"),
        }
    }

    /// The arithmetic /ext groups 1-4 (ALU r/m,imm; shift/rotate; TEST/NOT/NEG/MUL/IMUL/DIV/IDIV;
    /// INC/DEC byte) through the decode/execute split. Every opcode is a ModRM whose `reg` field
    /// selects the sub-op; `decode` already parsed the ModRM + addressing descriptor and fetched the
    /// conditional immediate, so the executor resolves the r/m operand from the pre-decoded
    /// descriptor (EA recomputed against the live registers) and reuses `self.alu`/`shift_rotate`/
    /// `mul`/`div`/`inc_dec` verbatim. Each arm mirrors its former fused handler exactly — same
    /// operand wiring, same write-back gating (CMP and TEST compute flags only), same #DE/#UD fault
    /// points, same clocks — so behavior and the bytes consumed stay byte-for-byte unchanged.
    /// The whole architectural body of `PUSHA`/`PUSHAD` (0x60), factored out of the opcode arm so
    /// the JIT's `PushAllDword` call-out slot runs THIS code rather than a copy of it. Two
    /// consumers, one body: the exact-clocks and exact-effects claims the call-out makes are then
    /// claims about a shared function, not about two implementations agreeing.
    ///
    /// Push AX, CX, DX, BX, the pre-instruction SP, BP, SI, DI. A fault on ANY of the eight pushes
    /// restores (E)SP to the pre-instruction value (386 PRM: PUSHA restores ESP so the instruction
    /// restarts whole; individual committed sub-pushes are just re-written on the restart). There
    /// is NO pre-validation of the eight-slot range: the interpreter discovers a fault by taking
    /// it, part-way, with sub-pushes already committed to memory. That is why the call-out cannot
    /// simply run this and hope — see `call_out_stack_frame_resident`, which is the pre-check the
    /// interpreter does not have and the call-out must.
    pub(crate) fn push_all_gpr<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand_size: OperandSize,
    ) -> ExecResult<()> {
        let sp_snapshot = self.read_gpr_sized(4, operand_size);
        let esp_before = self.registers.esp();
        let push_all = |cpu: &mut Self, bus: &mut B| -> ExecResult<()> {
            for index in [0u8, 1, 2, 3] {
                let value = cpu.read_gpr_sized(index, operand_size);
                cpu.push(bus, value, operand_size)?;
            }
            cpu.push(bus, sp_snapshot, operand_size)?;
            for index in [5u8, 6, 7] {
                let value = cpu.read_gpr_sized(index, operand_size);
                cpu.push(bus, value, operand_size)?;
            }
            Ok(())
        };
        if let Err(fault) = push_all(self, bus) {
            if self.stack_is_32bit() {
                self.registers.set_esp(esp_before);
            } else {
                self.write_gpr16(4, esp_before as u16);
            }
            return Err(fault);
        }
        Ok(())
    }

    /// The whole architectural body of `POPA`/`POPAD` (0x61); see `push_all_gpr` for why it is a
    /// function rather than an opcode arm.
    ///
    /// Pop DI, SI, BP, discard the SP slot, then BX, DX, CX, AX. Unlike PUSHA there is no restore
    /// on fault at all: a fault part-way leaves the registers already loaded and (E)SP already
    /// advanced, which is the architectural behaviour and another reason the call-out pre-checks
    /// instead of retrying.
    pub(crate) fn pop_all_gpr<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand_size: OperandSize,
    ) -> ExecResult<()> {
        for index in [7u8, 6, 5] {
            let value = self.pop(bus, operand_size)?;
            self.write_gpr_sized(index, operand_size, value);
        }
        let discarded = self.pop(bus, operand_size)?; // SP slot, SP advances over it
        for index in [3u8, 2, 1, 0] {
            let value = self.pop(bus, operand_size)?;
            self.write_gpr_sized(index, operand_size, value);
        }
        // On a 16-bit stack (SS.B=0), POPAD leaves SP advanced but lets the
        // discarded saved-ESP slot's high half land in ESP[31:16]. Verified
        // against the 80386 vectors; the register loads above are unaffected.
        if !self.stack_is_32bit() && matches!(operand_size, OperandSize::Dword) {
            let advanced = self.registers.esp();
            self.registers
                .set_esp((discarded & 0xffff_0000) | (advanced & 0xffff));
        }
        Ok(())
    }

    fn execute_group_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        let opcode = insn.opcode as u8;
        let operand_size = insn.operand_size;
        let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);

        match opcode {
            0x80 | 0x82 => {
                // Group 1 ALU r/m8, imm8. `reg` selects ADD/OR/ADC/SBB/AND/SUB/XOR/CMP; CMP (/7)
                // computes flags only (no write-back). The imm8 was fetched + charged by `decode`.
                let imm = insn.imm;
                let a = u32::from(self.read_operand_u8(bus, operand)?);
                let result = self.alu(modrm.reg, a, imm, BusWidth::Byte) as u8;
                if modrm.reg != 7 {
                    self.write_operand_u8(bus, operand, result)?;
                }
                Ok(self.charge(group1_class(modrm.reg, operand)))
            }
            0x81 => {
                // Group 1 ALU r/m16/32, imm16/32. Full-width immediate from `decode`.
                let imm = insn.imm;
                let a = self.read_operand_sized(bus, operand, operand_size)?;
                let result = self.alu(modrm.reg, a, imm, operand_size.bus_width());
                if modrm.reg != 7 {
                    self.write_operand_sized(bus, operand, operand_size, result)?;
                }
                Ok(self.charge(group1_class(modrm.reg, operand)))
            }
            0x83 => {
                // Group 1 ALU r/m16/32, imm8 sign-extended to the operand width. `decode` already
                // sign-extended the byte into `insn.imm`.
                let imm = insn.imm;
                let a = self.read_operand_sized(bus, operand, operand_size)?;
                let result = self.alu(modrm.reg, a, imm, operand_size.bus_width());
                if modrm.reg != 7 {
                    self.write_operand_sized(bus, operand, operand_size, result)?;
                }
                Ok(self.charge(group1_class(modrm.reg, operand)))
            }
            0xc0 | 0xc1 | 0xd0 | 0xd1 | 0xd2 | 0xd3 => {
                // Group 2 shift/rotate. `reg` selects ROL/ROR/RCL/RCR/SHL/SHR/SAL/SAR; the count
                // source is the imm8 `decode` fetched (0xc0/0xc1), the literal 1 (0xd0/0xd1), or CL
                // (0xd2/0xd3). `shift_rotate` owns every flag rule (masked count, 1-bit-vs-multi OF),
                // reused verbatim. Even-numbered opcodes are the byte form.
                let op = modrm.reg;
                let count = match opcode {
                    0xc0 | 0xc1 => insn.imm as u8,
                    0xd0 | 0xd1 => 1,
                    _ => (self.registers.ecx() & 0xff) as u8,
                };
                if opcode & 1 == 0 {
                    let value = u32::from(self.read_operand_u8(bus, operand)?);
                    let result = self.shift_rotate(op, value, count, BusWidth::Byte) as u8;
                    self.write_operand_u8(bus, operand, result)?;
                } else {
                    let value = self.read_operand_sized(bus, operand, operand_size)?;
                    let result = self.shift_rotate(op, value, count, operand_size.bus_width());
                    self.write_operand_sized(bus, operand, operand_size, result)?;
                }
                Ok(self.charge(group2_class(opcode)))
            }
            0xf6 => {
                // Group 3 byte. /0 TEST (AND-for-flags, no write-back) takes the imm8 `decode`
                // fetched for reg==0; the other sub-ops carry no immediate. NOT (/2) touches no
                // flags; NEG (/3) sets flags like 0 - operand; MUL/IMUL (/4,/5) and DIV/IDIV
                // (/6,/7) reuse `mul`/`div` (DIV raises #DE on divide-by-zero / quotient overflow).
                // reg==1 is undefined in the fused reference: it consumes no immediate and faults as
                // UnsupportedGroupOpcode, preserved here.
                let value = u32::from(self.read_operand_u8(bus, operand)?);
                match modrm.reg {
                    0 => {
                        self.alu(4, value, insn.imm, BusWidth::Byte);
                    }
                    2 => self.write_operand_u8(bus, operand, !(value as u8))?, // NOT: no flags
                    3 => {
                        // NEG: flags like 0 - operand (CF set unless operand is 0).
                        let result = self.alu_sub(0, value, 0, BusWidth::Byte) as u8;
                        self.write_operand_u8(bus, operand, result)?;
                    }
                    4 => self.mul(value, false, BusWidth::Byte), // MUL
                    5 => self.mul(value, true, BusWidth::Byte),  // IMUL
                    6 => self.div(value, false, BusWidth::Byte)?, // DIV
                    7 => self.div(value, true, BusWidth::Byte)?, // IDIV
                    _ => {
                        return Err(undefined_opcode());
                    }
                }
                Ok(self.charge(group3_class(modrm.reg, BusWidth::Byte, operand)))
            }
            0xf7 => {
                // Group 3 word/dword. Same sub-op layout as 0xf6 at the operand width.
                let value = self.read_operand_sized(bus, operand, operand_size)?;
                match modrm.reg {
                    0 => {
                        self.alu(4, value, insn.imm, operand_size.bus_width());
                    }
                    2 => {
                        // NOT: bitwise complement, no flags changed. Mask like every other
                        // write_operand_sized caller so no high bits are passed.
                        let result = !value & operand_size.mask();
                        self.write_operand_sized(bus, operand, operand_size, result)?;
                    }
                    3 => {
                        // NEG: flags like 0 - operand (CF set unless operand is 0).
                        let result = self.alu_sub(0, value, 0, operand_size.bus_width());
                        self.write_operand_sized(bus, operand, operand_size, result)?;
                    }
                    4 => self.mul(value, false, operand_size.bus_width()), // MUL
                    5 => self.mul(value, true, operand_size.bus_width()),  // IMUL
                    6 => self.div(value, false, operand_size.bus_width())?, // DIV
                    7 => self.div(value, true, operand_size.bus_width())?, // IDIV
                    _ => {
                        return Err(undefined_opcode());
                    }
                }
                // The `/2../7` Word forms are `InterpretOne` call-out rows, and their budget
                // bound (`INTERPRET_ONE_MAX_CORE_CLOCKS`) and this arm must charge the same
                // number; under epoch 1 every class `group3_class` can return charges
                // `GROUP3_CORE_CLOCKS`, so the bound still holds. Re-deriving the bound from the
                // table is slice 1 item 4.
                Ok(self.charge(group3_class(modrm.reg, operand_size.bus_width(), operand)))
            }
            0xfe => {
                // Group 4 INC/DEC byte. /0 INC, /1 DEC; any other reg is #UD (the fused reference's
                // UnsupportedGroupOpcode). INC/DEC preserve CF (handled inside `inc_dec`).
                match modrm.reg {
                    0 | 1 => {
                        let value = u32::from(self.read_operand_u8(bus, operand)?);
                        let result = self.inc_dec(value, modrm.reg == 1, BusWidth::Byte) as u8;
                        self.write_operand_u8(bus, operand, result)?;
                        // Named because the MEMORY form is an `InterpretOne` call-out row: its
                        // budget bound and this arm must charge the same number.
                        Ok(self.charge(match operand {
                            RmOperand::Register(_) => TimingClass::Reg,
                            RmOperand::Memory(_) => TimingClass::IncDecRm,
                        }))
                    }
                    _extension => Err(undefined_opcode()),
                }
            }
            _ => unreachable!("group opcode {opcode:#x}"),
        }
    }

    /// The relative-displacement + loop control-flow block (Jcc short/near, JMP short/near, CALL
    /// near, LOOP/LOOPE/LOOPNE/JCXZ) through the decode/execute split. Each arm mirrors its former
    /// fused handler verbatim — same condition/count test, same push order for CALL, same clocks —
    /// but takes the relative displacement from `insn.imm` (decode already fetched + sign-extended +
    /// charged it) instead of re-reading it. eip is already at the instruction end here (decode
    /// advanced it), so `relative_jump(disp, operand_size)` reproduces the fused eip-relative target
    /// math (16- vs 32-bit IP wrap, operand-size mask) bit-for-bit.
    fn execute_branch_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        let operand_size = insn.operand_size;
        let address_size = insn.address_size;
        // The displacement was stored sign-extended (rel8/rel16/rel32) as i32 by `decode`.
        let rel = insn.imm as i32;

        // The two-byte Jcc near (0x0F80-0x0F8F) must be matched on the FULL u16 BEFORE any `as u8`
        // narrowing — `insn.opcode as u8` would alias 0x0F8x onto the single-byte 0x8x opcodes. Both
        // the single-byte Jcc short (0x70-0x7f) and the two-byte Jcc near share the same condition
        // mapping (the low nibble), so handle them together off `insn.opcode & 0x0f`. Same clocks (3)
        // as the fused Jcc handlers.
        if matches!(insn.opcode, 0x70..=0x7f | 0x0f80..=0x0f8f) {
            if self.condition((insn.opcode & 0x0f) as u8) {
                self.relative_jump(rel, operand_size);
            }
            return Ok(self.charge(TimingClass::Jcc));
        }

        match insn.opcode as u8 {
            0xe0 | 0xe1 => {
                // LOOPNE (E0) / LOOPE (E1): decrement (E)CX, branch while non-zero and ZF matches.
                let count_nonzero = match address_size {
                    AddressSize::Word => {
                        let next = self.read_gpr16(1).wrapping_sub(1);
                        self.write_gpr16(1, next);
                        next != 0
                    }
                    AddressSize::Dword => {
                        let next = self.registers.ecx().wrapping_sub(1);
                        self.registers.set_ecx(next);
                        next != 0
                    }
                };
                let zf = self.flag(FLAG_ZF);
                let taken = count_nonzero && (if insn.opcode as u8 == 0xe1 { zf } else { !zf });
                if taken {
                    self.relative_jump(rel, operand_size);
                }
                Ok(self.charge(TimingClass::LoopCc))
            }
            0xe2 => {
                // LOOP: decrement (E)CX, branch while non-zero.
                let taken = match address_size {
                    AddressSize::Word => {
                        let next = self.read_gpr16(1).wrapping_sub(1);
                        self.write_gpr16(1, next);
                        next != 0
                    }
                    AddressSize::Dword => {
                        let next = self.registers.ecx().wrapping_sub(1);
                        self.registers.set_ecx(next);
                        next != 0
                    }
                };
                if taken {
                    self.relative_jump(rel, operand_size);
                }
                Ok(self.charge(TimingClass::Loop))
            }
            0xe3 => {
                // JCXZ / JECXZ: no decrement; branch when (E)CX is zero.
                let count_zero = match address_size {
                    AddressSize::Word => self.read_gpr16(1) == 0,
                    AddressSize::Dword => self.registers.ecx() == 0,
                };
                if count_zero {
                    self.relative_jump(rel, operand_size);
                }
                Ok(self.charge(TimingClass::Jcxz))
            }
            0xe8 => {
                // CALL near, relative. Push the return address (eip, already at the instruction
                // end) before branching — the same order the fused handler used.
                self.push(bus, self.registers.eip, operand_size)?;
                self.relative_jump(rel, operand_size);
                Ok(self.charge(TimingClass::CallJmpRel))
            }
            0xe9 => {
                // JMP near, relative.
                self.relative_jump(rel, operand_size);
                Ok(self.charge(TimingClass::CallJmpRel))
            }
            0xeb => {
                // JMP short, relative.
                self.relative_jump(rel, operand_size);
                Ok(self.charge(TimingClass::CallJmpRel))
            }
            opcode => unreachable!("branch opcode {opcode:#x}"),
        }
    }

    /// The flags + misc register block (task A7) through the decode/execute split. Each arm mirrors
    /// the former fused handler verbatim — same `alu` call for TEST (op=4, AND-for-flags, no
    /// write-back), same `inc_dec` for INC/DEC reg (CF preserved), same sign-extend logic for
    /// CBW/CWDE and CWD/CDQ, same flag-byte masking for SAHF/LAHF, same `set_flag` + `check_v86_iopl`
    /// for the flag-bit ops, and same STI interrupt shadow — but consumes the ModRM/operand
    /// `decode` pre-parsed for TEST (so the executor re-fetches nothing). The r/m operand for TEST
    /// is resolved from the pre-decoded descriptor against the live registers each call.
    fn execute_flags_misc_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        let operand_size = insn.operand_size;

        match insn.opcode as u8 {
            0x84 => {
                // TEST r/m8, reg8. AND-for-flags only; no write-back (same as op=4, write_back=false).
                let modrm = insn.modrm.expect("TEST r/m8,reg8 decoded with a ModRM");
                let value = match insn
                    .operand
                    .expect("TEST r/m8,reg8 decoded with an operand")
                {
                    DecodedOperand::Reg(index) => self.read_gpr8(index),
                    DecodedOperand::Mem(addr) => {
                        let memory = self.resolve_memory_addr_mode(&addr);
                        self.read_memory_u8(
                            bus,
                            memory.segment,
                            memory.offset,
                            BusAccessKind::DataRead,
                        )?
                    }
                };
                let reg = self.read_gpr8(modrm.reg);
                self.alu(4, u32::from(value), u32::from(reg), BusWidth::Byte);
                Ok(self.charge(test_rm_class(insn)))
            }
            0x85 => {
                // TEST r/m16/32, reg16/32. AND-for-flags only; no write-back.
                let modrm = insn.modrm.expect("TEST r/m,reg decoded with a ModRM");
                let value = match insn.operand.expect("TEST r/m,reg decoded with an operand") {
                    DecodedOperand::Reg(index) => self.read_gpr_sized(index, operand_size),
                    DecodedOperand::Mem(addr) => {
                        let memory = self.resolve_memory_addr_mode(&addr);
                        self.read_memory_sized(
                            bus,
                            memory.segment,
                            memory.offset,
                            operand_size,
                            BusAccessKind::DataRead,
                        )?
                    }
                };
                let reg = self.read_gpr_sized(modrm.reg, operand_size);
                self.alu(4, value, reg, operand_size.bus_width());
                Ok(self.charge(test_rm_class(insn)))
            }
            opcode @ 0x40..=0x4f => {
                // INC (0x40-0x47) / DEC (0x48-0x4f) register. CF is preserved by `inc_dec`.
                let index = opcode & 0x07;
                let is_dec = opcode >= 0x48;
                let value = self.read_gpr_sized(index, operand_size);
                let result = self.inc_dec(value, is_dec, operand_size.bus_width());
                self.write_gpr_sized(index, operand_size, result);
                Ok(self.charge(TimingClass::Reg))
            }
            0x98 => {
                // CBW / CWDE: sign-extend the accumulator into the next width.
                match operand_size {
                    OperandSize::Word => {
                        let ax = i16::from(self.read_gpr8(0) as i8) as u16;
                        self.write_gpr16(0, ax);
                    }
                    OperandSize::Dword => {
                        let eax = i32::from(self.read_gpr16(0) as i16) as u32;
                        self.write_gpr32(0, eax);
                    }
                }
                Ok(self.charge(TimingClass::Cbw))
            }
            0x99 => {
                // CWD / CDQ: fill (E)DX with the sign of the accumulator.
                match operand_size {
                    OperandSize::Word => {
                        let dx = if (self.read_gpr16(0) as i16) < 0 {
                            0xffff
                        } else {
                            0
                        };
                        self.write_gpr16(2, dx);
                    }
                    OperandSize::Dword => {
                        let edx = if (self.read_gpr32(0) as i32) < 0 {
                            0xffff_ffff
                        } else {
                            0
                        };
                        self.write_gpr32(2, edx);
                    }
                }
                Ok(self.charge(TimingClass::Cwd))
            }
            0x9e => {
                // SAHF: load CF/PF/AF/ZF/SF from AH; OF and the reserved bits are untouched.
                // The trailing | 0x02 keeps the always-one reserved bit set.
                // Settle deferred flags first: the read-modify-write reads registers.eflags to
                // preserve OF and control bits, so a stale descriptor would corrupt OF in the result.
                self.materialize_flags();
                let ah = u32::from(self.read_gpr8(4));
                self.registers.eflags = (self.registers.eflags & !0xd5) | (ah & 0xd5) | 0x02;
                Ok(self.charge(TimingClass::Sahf))
            }
            0x9f => {
                // LAHF: AH = low flag byte with bit1 forced 1, bits 3 and 5 forced 0.
                // Settle deferred flags so the captured low byte (CF/PF/AF/ZF/SF) is live.
                self.materialize_flags();
                let ah = ((self.registers.eflags as u8) & 0xd5) | 0x02;
                self.write_gpr8(4, ah);
                Ok(self.charge(TimingClass::Lahf))
            }
            0xf5 => {
                // CMC: complement the carry flag.
                self.set_flag(FLAG_CF, !self.flag(FLAG_CF));
                Ok(self.charge(TimingClass::FlagOp))
            }
            0xf8 => {
                // CLC: clear the carry flag.
                self.set_flag(FLAG_CF, false);
                Ok(self.charge(TimingClass::FlagOp))
            }
            0xf9 => {
                // STC: set the carry flag.
                self.set_flag(FLAG_CF, true);
                Ok(self.charge(TimingClass::FlagOp))
            }
            0xfa => {
                // CLI. IOPL-sensitive: faults to the monitor in a V86 task below IOPL 3.
                self.check_v86_iopl()?;
                self.set_flag(FLAG_IF, false);
                // Named because CLI is an `InterpretOne` call-out row: its budget bound and this
                // arm must charge the same number.
                Ok(self.charge(TimingClass::Cli))
            }
            0xfb => {
                // STI sets IF and arms the one-instruction shadow so the instruction immediately
                // after STI always executes before any interrupt is taken. The shadow is set here
                // in the executor exactly as the fused handler did.
                self.check_v86_iopl()?;
                self.set_flag(FLAG_IF, true);
                self.interrupt_shadow = true;
                Ok(self.charge(TimingClass::Sti))
            }
            0xfc => {
                // CLD: clear the direction flag.
                self.set_flag(FLAG_DF, false);
                Ok(self.charge(TimingClass::FlagOp))
            }
            0xfd => {
                // STD: set the direction flag.
                self.set_flag(FLAG_DF, true);
                Ok(self.charge(TimingClass::FlagOp))
            }
            opcode => unreachable!("flags-misc opcode {opcode:#x}"),
        }
    }

    /// The string-operation block (MOVS/CMPS/STOS/LODS/SCAS) through the decode/execute split. This
    /// is intentionally a thin wrapper: every opcode here is implicit-operand, so `decode` pre-parsed
    /// nothing, and the executor simply re-dispatches to the existing `run_string` helper VERBATIM —
    /// the same `(StringOp, BusWidth)` pairing each fused arm used, with `insn.prefixes` passed
    /// straight through. All the load-bearing semantics live in the unchanged helper and are NOT
    /// reimplemented here:
    ///   - the REP/REPNE loop and the CX/ECX==0 termination (`run_string`, keyed on `prefixes.rep`);
    ///   - the REPE-vs-REPNE ZF early-termination for CMPS/SCAS (`run_string`);
    ///   - the DF-driven SI/DI increment/decrement (`adjust_index_register`, keyed on FLAG_DF);
    ///   - the DS:SI source segment override vs the fixed ES:DI destination (`read_string_src`/
    ///     `write_string_dst`, keyed on `prefixes.segment_override`);
    ///   - the per-iteration data-access clocks (charged by the bus accesses inside `string_step`).
    ///
    /// The element width is the only thing derived here, exactly as the fused arms did: byte for the
    /// even opcodes (0xa4/0xa6/0xaa/0xac/0xae) and the operand-size width for the odd ones. The
    /// instruction-fetch clocks (prefix + opcode) were charged once in `decode`; this executor
    /// re-fetches nothing, and the returned `clocks(4)` matches each fused arm.
    fn execute_string_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
        work: &mut InstructionWork,
    ) -> ExecResult<CycleOutcome> {
        let prefixes = insn.prefixes;
        let address_size = insn.address_size;
        // The low opcode bit selects the element width: 0 = byte, 1 = operand-size (word/dword).
        let width = if insn.opcode & 1 == 0 {
            BusWidth::Byte
        } else {
            insn.operand_size.bus_width()
        };
        let op = match insn.opcode as u8 {
            0xa4 | 0xa5 => StringOp::Movs,
            0xa6 | 0xa7 => StringOp::Cmps,
            0xaa | 0xab => StringOp::Stos,
            0xac | 0xad => StringOp::Lods,
            0xae | 0xaf => StringOp::Scas,
            opcode => unreachable!("string opcode {opcode:#x}"),
        };
        if work.rep.is_none()
            && prefixes.rep.is_some()
            && matches!(op, StringOp::Movs | StringOp::Stos)
            && matches!(self.persona(), CpuPersona::I486 | CpuPersona::I586)
        {
            work.rep = Some(RepInvocation::new(RepPriceHistory {
                initial_count: self.string_count(address_size),
                startup_paid: false,
            }));
        }
        self.price_rep_invocation(work, op, address_size);
        self.run_string(bus, work, op, width, prefixes, address_size)?;
        if work.sourced_rep() {
            return Ok(clocks(0));
        }
        let mut outcome = self.charge(TimingClass::StringElem);
        if let Some(invoice) = work.rep.as_mut() {
            invoice.legacy_payment(outcome.core_clocks);
            outcome.core_clocks = 0;
        }
        Ok(outcome)
    }

    /// The port I/O block through the decode/execute split (task A9). Calls `bus.read_io` /
    /// `bus.write_io` on the same path as the former fused arms, so `io_touched` is set exactly
    /// as before. For the imm8 forms (0xe4-0xe7) `decode` pre-read the port number into `insn.imm`;
    /// for the DX forms (0xec-0xef) the port comes from the DX register (GPR index 2) at execute
    /// time. The low bit of the opcode selects the I/O direction within each pair (0 = IN, 1 = OUT
    /// only for 0xe4/0xe5 vs 0xe6/0xe7, respectively; 0 = IN, 1 = unused for the 0xec range where
    /// bit 1 distinguishes direction: see comments per arm). Every arm charges through
    /// `port_io_core_clocks`, which is epoch 1's flat 12-for-IN / 10-for-OUT byte-identically and
    /// epoch 2's four-column Intel table otherwise -- including the two `0xE5`/`0xED` arms, whose
    /// bare literal `12` this replaced.
    /// In V86 (or protected mode with CPL > IOPL), `IN`/`OUT` consult the TSS
    /// I/O-permission bitmap: the access is allowed only if every bit for ports
    /// `port..port+width` is 0. A bit at or beyond the TSS limit is treated as set
    /// (not permitted). A denied access faults `#GP(0)` to the monitor.
    pub(crate) fn check_io_permission<B: CpuBus>(
        &mut self,
        bus: &mut B,
        port: u16,
        width: BusWidth,
    ) -> ExecResult<()> {
        if !self.is_v86_mode() && self.current_privilege_level() <= self.iopl() {
            return Ok(());
        }
        let io_base = self.read_system_linear(bus, self.tr.base + 0x66, BusWidth::Word)?;
        for p in u32::from(port)..u32::from(port) + width.bytes() {
            let byte_index = io_base + p / 8;
            if byte_index > self.tr.limit {
                return Err(InternalFault::Exception {
                    vector: 13,
                    error_code: Some(0),
                });
            }
            let byte =
                self.read_system_linear(bus, self.tr.base + byte_index, BusWidth::Byte)? as u8;
            if byte & (1 << (p % 8)) != 0 {
                return Err(InternalFault::Exception {
                    vector: 13,
                    error_code: Some(0),
                });
            }
        }
        Ok(())
    }

    fn execute_port_io_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        // Falsification F1 (design section 9): any IN/OUT inside a reflected
        // trip means no read-set can certify it. Counted regardless of
        // outcome (a faulted permission check still means the trip touched a
        // port).
        #[cfg(feature = "reflected-call-diagnostic")]
        crate::reflected_call_diag::on_port_io();
        #[cfg(feature = "reflected-call-memo")]
        crate::reflected_call_memo::note_port_io(self);
        let operand_size = insn.operand_size;
        match insn.opcode as u8 {
            0xe4 => {
                // IN AL, imm8: byte port input. `decode` stored the port number in `insn.imm`.
                let port = insn.imm as u16;
                self.check_io_permission_charging(bus, port, BusWidth::Byte, TimingClass::InPort)?;
                let value = bus.read_io(
                    port,
                    BusWidth::Byte,
                    self.core_clocks_so_far,
                    self.is_ring0_protected(),
                )? as u8;
                self.write_gpr8(0, value);
                Ok(clocks(self.port_io_core_clocks(bus, false)))
            }
            0xe5 => {
                // IN AX/EAX, imm8: word/dword port input into the accumulator.
                let port = insn.imm as u16;
                self.check_io_permission_charging(
                    bus,
                    port,
                    operand_size.bus_width(),
                    TimingClass::InPortDword,
                )?;
                let value = bus.read_io(
                    port,
                    operand_size.bus_width(),
                    self.core_clocks_so_far,
                    self.is_ring0_protected(),
                )?;
                self.write_gpr_sized(0, operand_size, value);
                Ok(clocks(self.port_io_core_clocks(bus, false)))
            }
            0xe6 => {
                // OUT imm8, AL: byte port output from AL.
                let port = insn.imm as u16;
                self.check_io_permission_charging(bus, port, BusWidth::Byte, TimingClass::OutPort)?;
                bus.write_io(
                    port,
                    BusWidth::Byte,
                    u32::from(self.read_gpr8(0)),
                    self.core_clocks_so_far,
                    self.is_ring0_protected(),
                )?;
                Ok(clocks(self.port_io_core_clocks(bus, true)))
            }
            0xe7 => {
                // OUT imm8, AX/EAX: word/dword port output from the accumulator.
                let port = insn.imm as u16;
                self.check_io_permission_charging(
                    bus,
                    port,
                    operand_size.bus_width(),
                    TimingClass::OutPort,
                )?;
                bus.write_io(
                    port,
                    operand_size.bus_width(),
                    self.read_gpr_sized(0, operand_size),
                    self.core_clocks_so_far,
                    self.is_ring0_protected(),
                )?;
                Ok(clocks(self.port_io_core_clocks(bus, true)))
            }
            0xec => {
                // IN AL, DX: byte port input. Port number in DX (GPR 2).
                let port = self.read_gpr16(2);
                self.check_io_permission_charging(bus, port, BusWidth::Byte, TimingClass::InPort)?;
                let value = bus.read_io(
                    port,
                    BusWidth::Byte,
                    self.core_clocks_so_far,
                    self.is_ring0_protected(),
                )? as u8;
                self.write_gpr8(0, value);
                Ok(clocks(self.port_io_core_clocks(bus, false)))
            }
            0xed => {
                // IN AX/EAX, DX: word/dword port input addressed by DX.
                let port = self.read_gpr16(2);
                self.check_io_permission_charging(
                    bus,
                    port,
                    operand_size.bus_width(),
                    TimingClass::InPortDword,
                )?;
                let value = bus.read_io(
                    port,
                    operand_size.bus_width(),
                    self.core_clocks_so_far,
                    self.is_ring0_protected(),
                )?;
                self.write_gpr_sized(0, operand_size, value);
                Ok(clocks(self.port_io_core_clocks(bus, false)))
            }
            0xee => {
                // OUT DX, AL: byte port output addressed by DX.
                let port = self.read_gpr16(2);
                self.check_io_permission_charging(bus, port, BusWidth::Byte, TimingClass::OutPort)?;
                bus.write_io(
                    port,
                    BusWidth::Byte,
                    u32::from(self.read_gpr8(0)),
                    self.core_clocks_so_far,
                    self.is_ring0_protected(),
                )?;
                Ok(clocks(self.port_io_core_clocks(bus, true)))
            }
            0xef => {
                // OUT DX, AX/EAX: word/dword port output addressed by DX.
                let port = self.read_gpr16(2);
                self.check_io_permission_charging(
                    bus,
                    port,
                    operand_size.bus_width(),
                    TimingClass::OutPort,
                )?;
                bus.write_io(
                    port,
                    operand_size.bus_width(),
                    self.read_gpr_sized(0, operand_size),
                    self.core_clocks_so_far,
                    self.is_ring0_protected(),
                )?;
                Ok(clocks(self.port_io_core_clocks(bus, true)))
            }
            opcode => unreachable!("port-I/O opcode {opcode:#x}"),
        }
    }
}

/// The charge class for one `execute_alu_decoded` form.
///
/// The interpreter serves all six ALU forms from one arm and one
/// `Ok(clocks(2))`, so the class has to be recovered here from the form number
/// and the decoded operand. Both references split them three ways
/// (`dev_docs/2026-09-05-86box-pentium-timing-comparison.md` section 3 rows 1,
/// 3 and 4): a register-only form is 1 clock, a memory SOURCE is the 2-clock
/// load form, and a memory DESTINATION that writes back is the 3-clock
/// read/modify/write. `CMP`/`TEST` against memory write nothing, so they are a
/// load however the operand is addressed.
///
/// Forms 4 and 5 are the accumulator/immediate encodings and carry no ModRM at
/// all, so `insn.operand` is `None` and they fall out as register-only.
pub(crate) fn alu_class(form: u8, write_back: bool, insn: &DecodedInsn) -> TimingClass {
    if !matches!(insn.operand, Some(DecodedOperand::Mem(_))) {
        return TimingClass::Reg;
    }
    match form {
        0 | 1 if write_back => TimingClass::AluMemReg,
        0..=3 => TimingClass::AluRegMem,
        _ => TimingClass::Reg,
    }
}

/// The charge class for a group-1 `ALU r/m, imm` (`0x80`..`0x83`).
///
/// Same three-way split as `alu_class`, decided by the resolved r/m operand and
/// the sub-opcode: `/7` is `CMP`, which computes flags without writing back and
/// is therefore a load rather than a read/modify/write.
pub(crate) fn group1_class(sub_opcode: u8, operand: RmOperand) -> TimingClass {
    match operand {
        RmOperand::Register(_) => TimingClass::Reg,
        RmOperand::Memory(_) if sub_opcode == 7 => TimingClass::AluRegMem,
        RmOperand::Memory(_) => TimingClass::AluMemReg,
    }
}

/// The charge class for a group-2 shift/rotate (`0xc0`..`0xd3`), decided by the
/// count SOURCE, which is what both references price: an immediate or a literal
/// 1 issues in one clock on a P5 where a `CL` count is unpairable and costs four
/// (comparison section 3 row 11).
///
/// The by-1 forms (`0xd0`/`0xd1`) share `ShiftImm` with the immediate forms
/// rather than taking a class of their own, because the JIT cannot separate
/// them: `0xd1` and `0xc1` with an immediate of 1 produce the same
/// `DirectKind::Shift` and `DirectInsn` carries no opcode. Splitting here alone
/// would make a compiled block and an interpreted one charge the same
/// instruction differently on the 486, which is the divergence the arm-equality
/// bar exists to stop. See `TimingClass::ShiftImm`.
///
/// `RCL`/`RCR` by `CL` is more expensive again on both parts (486 8-30, P5 7-24)
/// and wants its own class; the sub-opcode that would separate it also separates
/// nothing else here, and splitting it one-sidedly would break native/interpreted
/// equality the same way the group-3 split would. It rides `ShiftCl`, an
/// under-charge recorded on `TimingClass::ShiftCl`.
pub(crate) fn group2_class(opcode: u8) -> TimingClass {
    match opcode {
        0xc0 | 0xc1 | 0xd0 | 0xd1 => TimingClass::ShiftImm,
        _ => TimingClass::ShiftCl,
    }
}

/// The charge class for `TEST r/m, r` (`0x84`/`0x85`), which reads its r/m
/// operand and writes nothing: the memory form is Intel's 2-clock load shape,
/// the register form its 1-clock ALU shape.
pub(crate) fn test_rm_class(insn: &DecodedInsn) -> TimingClass {
    match insn.operand {
        Some(DecodedOperand::Mem(_)) => TimingClass::AluRegMem,
        _ => TimingClass::Reg,
    }
}

/// The charge class for one group-3 sub-opcode (`0xf6`/`0xf7`), which the
/// interpreter serves from a single arm per opcode.
///
/// This is design section 9.1's headline row. One `clocks(2)` used to cover
/// `TEST`, `NOT`, `NEG`, `MUL`, `IMUL`, `DIV` and `IDIV` at every width, which
/// gave `DIV EAX, ECX` the cost of `MOV EAX, EBX` -- 0.167 guest clocks against
/// Intel's 41, a 246x under-charge and the largest single error in the old
/// table.
///
/// `MUL` and `IMUL` share a class per width because both references price them
/// together (comparison section 3 row 13; audit section 5's `Mul` row);
/// `DIV` and `IDIV` do not, because both references price them apart.
/// `TEST` (`/0`) reads without writing back, so its memory form is a load;
/// `NOT`/`NEG` write back, so theirs is a read/modify/write.
///
/// The JIT reaches the same classes from `DirectKind`, which `classify` already
/// splits into `TestImmReg`/`TestImmMem`/`NegReg`/`MulReg`/`MulMemAcc`/
/// `ImulRegAcc`/`ImulMemAcc`/`DivReg`/`DivMem` -- all admitted at Dword only --
/// so the two arms agree instruction for instruction.
pub(crate) fn group3_class(sub_opcode: u8, width: BusWidth, operand: RmOperand) -> TimingClass {
    let memory = matches!(operand, RmOperand::Memory(_));
    match sub_opcode {
        0 | 1 if memory => TimingClass::TestImmMem,
        0 | 1 => TimingClass::TestImmReg,
        2 | 3 if memory => TimingClass::NotNegMem,
        2 | 3 => TimingClass::NotNegReg,
        4 | 5 => match width {
            BusWidth::Byte => TimingClass::Mul8,
            BusWidth::Word => TimingClass::Mul16,
            _ => TimingClass::Mul32,
        },
        6 => match width {
            BusWidth::Byte => TimingClass::Div8,
            BusWidth::Word => TimingClass::Div16,
            _ => TimingClass::Div32,
        },
        _ => match width {
            BusWidth::Byte => TimingClass::Idiv8,
            BusWidth::Word => TimingClass::Idiv16,
            _ => TimingClass::Idiv32,
        },
    }
}

impl CpuGsw {
    /// `check_io_permission`, naming the class the FAULTING INSTRUCTION would
    /// have charged had it completed.
    ///
    /// Slice 8, and the reason the wrapper exists at all: the exception arm in
    /// `finish_instruction` REPLACES the faulting instruction's charge with the
    /// delivery cost, so a reflected V86 `IN` used to cost 4.92 guest clocks
    /// where a real one costs the trap gate PLUS the `IN` -- census row 7. The
    /// class is stashed only on the path that is about to return `Err`, so the
    /// success path pays nothing and no stale value can survive: the Err arm
    /// `take`s it, and nothing else writes it.
    ///
    /// SCOPE, stated rather than implied: port I/O is the only family wired up.
    /// It is the one the census names and the one the reflected-call memo
    /// replays. Every other fault still charges delivery alone, which is the
    /// pre-slice behaviour and is recorded as such on
    /// `TimingClass::ExceptionDelivery`.
    fn check_io_permission_charging<B: CpuBus>(
        &mut self,
        bus: &mut B,
        port: u16,
        width: BusWidth,
        class: TimingClass,
    ) -> ExecResult<()> {
        match self.check_io_permission(bus, port, width) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.pending_faulting_class = Some(class);
                Err(error)
            }
        }
    }
}
