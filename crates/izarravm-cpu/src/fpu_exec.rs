// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl CpuGsw {
    // ============================ x87 FPU ============================
    // Escape opcodes 0xD8-0xDF. Registers are f64 (see fpu.rs for the precision
    // ceiling). Coverage now spans arithmetic (all D8/DC/DA/DE forms), the
    // transcendentals, 80-bit-extended and BCD memory operands, and the
    // environment/state save-restore set. Known limits are
    // documented at each site: precision control is ignored (everything computes in
    // f64), stores ignore RC (FIST/FRNDINT/FBSTP honor it), stack over/underflow
    // does not fault, and the env image's instruction/data pointers store as zero.

    /// Execute an x87 escape or FWAIT from the decoded instruction. The shared
    /// entry applies #NM and pending-#MF gates before the leaf operation runs.
    pub(super) fn execute_fpu_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        let opcode = insn.opcode as u8;
        if opcode == 0x9b {
            // WAIT checks task switching only when CR0.MP requests that behavior.
            // CR0.EM and physical FPU presence do not make WAIT an x87 escape.
            if self.control.cr0 & (CR0_MP | CR0_TS) == (CR0_MP | CR0_TS) {
                return Err(InternalFault::Exception {
                    vector: 7,
                    error_code: None,
                });
            }
            // WAIT/FWAIT: trap with #MF if the x87 has a pending unmasked exception (gated on
            // CR0.NE; otherwise the FERR#/IRQ13 path the PC uses applies and is not modeled). With
            // nothing pending it retires as a no-op. Identical to the former fused 0x9b arm.
            if self.control.cr0 & CR0_NE != 0 && self.fpu.pending_unmasked_exception() {
                return Err(InternalFault::Exception {
                    vector: 16,
                    error_code: None,
                });
            }
            // Fall through to the single tail so scale_fp_clocks is applied uniformly.
            let raw = clocks(6);
            return Ok(CycleOutcome {
                core_clocks: self.scale_fp_clocks(raw.core_clocks, FpOpClass::Wait),
                halted: raw.halted,
            });
        }

        // Every x87 escape raises #NM before touching FPU or memory state when
        // this fixed persona has no unit, emulation is requested, or a task switch
        // has left the FPU unavailable.
        if !self.persona().has_fpu() || self.control.cr0 & (CR0_EM | CR0_TS) != 0 {
            return Err(InternalFault::Exception {
                vector: 7,
                error_code: None,
            });
        }

        let modrm = insn
            .modrm
            .expect("an FPU escape opcode decoded with a ModRM");
        // A pending unmasked exception traps with #MF on the next waiting FPU
        // instruction. The no-wait control ops (FNINIT, FNCLEX) are exempt so they can
        // clear the state. Gated on CR0.NE; with NE clear the part would drive FERR#
        // and IRQ13 instead, which is not modeled.
        let is_no_wait_clear =
            opcode == 0xdb && modrm.mode == 3 && modrm.reg == 4 && matches!(modrm.rm, 2 | 3);
        if !is_no_wait_clear
            && self.control.cr0 & CR0_NE != 0
            && self.fpu.pending_unmasked_exception()
        {
            return Err(InternalFault::Exception {
                vector: 16,
                error_code: None,
            });
        }
        // Single tail: apply the per-mode, per-class FP-timing factor to every x87
        // op's raw clocks (WAIT handled above). Class derivation mirrors the census
        // profiler's view: register forms vs the three memory-form families.
        let fp_class = if modrm.mode == 3 {
            FpOpClass::Register
        } else {
            match opcode {
                0xdb => FpOpClass::IntConvert32,
                // DF int16/m64/BCD loads/stores, DE int16 arith, DA int32 arith.
                0xda | 0xde | 0xdf => FpOpClass::IntConvert16,
                0xd8 | 0xd9 => FpOpClass::F32Mem,
                _ => FpOpClass::F64Mem, // 0xdc | 0xdd
            }
        };
        let outcome = if modrm.mode == 3 {
            self.execute_fpu_register(opcode, modrm)?
        } else {
            // `decode` parsed the addressing descriptor for the memory forms; resolve it against
            // the live registers now (no instruction bytes are re-read).
            let mem = match self.resolve_decoded_modrm_operand(insn) {
                (_, RmOperand::Memory(memory)) => memory,
                (_, RmOperand::Register(_)) => {
                    unreachable!("mode != 3 decodes to a memory operand")
                }
            };
            self.execute_fpu_memory(bus, opcode, modrm.reg, mem, insn.operand_size)?
        };
        Ok(CycleOutcome {
            core_clocks: self.scale_fp_clocks(outcome.core_clocks, fp_class),
            halted: outcome.halted,
        })
    }
    fn execute_fpu_memory<B: CpuBus>(
        &mut self,
        bus: &mut B,
        opcode: u8,
        reg: u8,
        mem: MemoryOperand,
        operand_size: OperandSize,
    ) -> ExecResult<CycleOutcome> {
        match opcode {
            0xd8 => {
                let operand = self.read_real32(bus, mem)?;
                self.fpu_mem_arith(reg, operand);
                Ok(clocks(20))
            }
            0xdc => {
                let operand = self.read_real64(bus, mem)?;
                self.fpu_mem_arith(reg, operand);
                Ok(clocks(20))
            }
            0xd9 => match reg {
                0 => {
                    let v = self.read_real32(bus, mem)?;
                    self.fpu.push(v);
                    Ok(clocks(14))
                }
                2 | 3 => {
                    let v = self.fpu.get(0);
                    self.write_real32(bus, mem, v)?;
                    if reg == 3 {
                        self.fpu.pop();
                    }
                    Ok(clocks(14))
                }
                5 => {
                    let cw = self.read_memory_sized(
                        bus,
                        mem.segment,
                        mem.offset,
                        OperandSize::Word,
                        BusAccessKind::DataRead,
                    )?;
                    self.fpu.control = cw as u16;
                    Ok(clocks(4))
                }
                7 => {
                    let cw = u32::from(self.fpu.control);
                    self.write_memory_sized(
                        bus,
                        mem.segment,
                        mem.offset,
                        OperandSize::Word,
                        cw,
                        BusAccessKind::DataWrite,
                    )?;
                    Ok(clocks(14))
                }
                4 => {
                    // FLDENV: load the control, status, and tag words.
                    self.fpu_load_environment(bus, mem.segment, mem.offset, operand_size)?;
                    Ok(clocks(44))
                }
                6 => {
                    // FNSTENV: store the FPU environment.
                    self.fpu_store_environment(bus, mem.segment, mem.offset, operand_size)?;
                    Ok(clocks(56))
                }
                _ => self.fpu_unsupported(opcode),
            },
            0xdd => match reg {
                0 => {
                    let v = self.read_real64(bus, mem)?;
                    self.fpu.push(v);
                    Ok(clocks(14))
                }
                2 | 3 => {
                    let v = self.fpu.get(0);
                    self.write_real64(bus, mem, v)?;
                    if reg == 3 {
                        self.fpu.pop();
                    }
                    Ok(clocks(14))
                }
                7 => {
                    let sw = u32::from(self.fpu.status);
                    self.write_memory_sized(
                        bus,
                        mem.segment,
                        mem.offset,
                        OperandSize::Word,
                        sw,
                        BusAccessKind::DataWrite,
                    )?;
                    Ok(clocks(14))
                }
                4 => {
                    // FRSTOR: restore the environment and all eight registers.
                    self.fpu_restore_state(bus, mem.segment, mem.offset, operand_size)?;
                    Ok(clocks(75))
                }
                6 => {
                    // FNSAVE: store the environment and registers, then reinitialize.
                    self.fpu_save_state(bus, mem.segment, mem.offset, operand_size)?;
                    Ok(clocks(150))
                }
                _ => self.fpu_unsupported(opcode),
            },
            0xdb => match reg {
                0 => {
                    let v = self.read_int32(bus, mem)?;
                    self.fpu.push(v);
                    Ok(clocks(14))
                }
                2 | 3 => {
                    let v = self.fpu.get(0);
                    self.write_int32(bus, mem, v)?;
                    if reg == 3 {
                        self.fpu.pop();
                    }
                    Ok(clocks(14))
                }
                5 => {
                    // FLD m80: load an 80-bit extended-precision real.
                    let v = self.read_extended80(bus, mem)?;
                    self.fpu.push(v);
                    Ok(clocks(14))
                }
                7 => {
                    // FSTP m80: store ST(0) as 80-bit extended, then pop.
                    let v = self.fpu.get(0);
                    self.write_extended80(bus, mem, v)?;
                    self.fpu.pop();
                    Ok(clocks(14))
                }
                _ => self.fpu_unsupported(opcode),
            },
            0xda => {
                // FIADD/FIMUL/FICOM/FICOMP/FISUB/FISUBR/FIDIV/FIDIVR m32int.
                let operand = self.read_int32(bus, mem)?;
                self.fpu_mem_arith(reg, operand);
                Ok(clocks(20))
            }
            0xde => {
                // Integer-operand arithmetic with an m16 source.
                let operand = self.read_int16(bus, mem)?;
                self.fpu_mem_arith(reg, operand);
                Ok(clocks(20))
            }
            0xdf => match reg {
                0 => {
                    let v = self.read_int16(bus, mem)?;
                    self.fpu.push(v);
                    Ok(clocks(14))
                }
                2 | 3 => {
                    let v = self.fpu.get(0);
                    self.write_int16(bus, mem, v)?;
                    if reg == 3 {
                        self.fpu.pop();
                    }
                    Ok(clocks(14))
                }
                5 => {
                    let v = self.read_int64(bus, mem)?;
                    self.fpu.push(v);
                    Ok(clocks(14))
                }
                7 => {
                    let v = self.fpu.get(0);
                    self.write_int64(bus, mem, v)?;
                    self.fpu.pop();
                    Ok(clocks(14))
                }
                4 => {
                    // FBLD: load an 80-bit packed-BCD integer.
                    let v = self.read_bcd80(bus, mem.segment, mem.offset)?;
                    self.fpu.push(v);
                    Ok(clocks(75))
                }
                6 => {
                    // FBSTP: store ST(0) as 80-bit packed BCD, then pop.
                    let v = self.fpu.get(0);
                    self.write_bcd80(bus, mem.segment, mem.offset, v)?;
                    self.fpu.pop();
                    Ok(clocks(160))
                }
                _ => self.fpu_unsupported(opcode),
            },
            _ => self.fpu_unsupported(opcode),
        }
    }

    fn execute_fpu_register(&mut self, opcode: u8, modrm: ModRm) -> ExecResult<CycleOutcome> {
        let reg = modrm.reg;
        let i = modrm.rm;
        let byte = 0xc0 | (reg << 3) | modrm.rm;
        match opcode {
            0xd8 => self.fpu_reg_arith_st0(reg, i),
            0xdc => self.fpu_reg_arith_sti(reg, i, false),
            0xde => {
                if byte == 0xd9 {
                    // FCOMPP: compare ST(0) with ST(1), then pop both.
                    let a = self.fpu.get(0);
                    let b = self.fpu.get(1);
                    self.fpu_compare(a, b);
                    self.fpu.pop();
                    self.fpu.pop();
                    Ok(clocks(5))
                } else {
                    self.fpu_reg_arith_sti(reg, i, true)
                }
            }
            0xda => match byte {
                0xe9 => {
                    // FUCOMPP: unordered compare ST(0) with ST(1), then pop both.
                    let a = self.fpu.get(0);
                    let b = self.fpu.get(1);
                    self.fpu_compare(a, b);
                    self.fpu.pop();
                    self.fpu.pop();
                    Ok(clocks(5))
                }
                _ => self.fpu_unsupported(opcode),
            },
            0xd9 => self.fpu_d9_register(byte, i),
            0xdb => self.fpu_db_register(byte),
            0xdd => self.fpu_dd_register(reg, i),
            0xdf => match byte {
                0xe0 => {
                    // FNSTSW AX.
                    let sw = self.fpu.status;
                    self.write_gpr16(0, sw);
                    Ok(clocks(3))
                }
                _ => self.fpu_unsupported(opcode),
            },
            _ => self.fpu_unsupported(opcode),
        }
    }

    fn fpu_mem_arith(&mut self, reg: u8, operand: f64) {
        let a = self.fpu.get(0);
        match reg {
            2 => self.fpu_compare(a, operand),
            3 => {
                self.fpu_compare(a, operand);
                self.fpu.pop();
            }
            op => {
                let r = fpu_arith(op, a, operand);
                self.fpu_record_exceptions(op, a, operand, r);
                self.fpu.set(0, r);
            }
        }
    }

    fn fpu_reg_arith_st0(&mut self, reg: u8, i: u8) -> ExecResult<CycleOutcome> {
        let a = self.fpu.get(0);
        let b = self.fpu.get(i);
        match reg {
            2 => self.fpu_compare(a, b),
            3 => {
                self.fpu_compare(a, b);
                self.fpu.pop();
            }
            op => {
                let r = fpu_arith(op, a, b);
                self.fpu_record_exceptions(op, a, b, r);
                self.fpu.set(0, r);
            }
        }
        Ok(clocks(20))
    }

    /// Set the IE (invalid) and ZE (divide-by-zero) status flags after an arithmetic
    /// op. Limit: only these two classes are detected from the f64 result; overflow,
    /// underflow, denormal, and precision are not (f64's wider range rarely trips them).
    fn fpu_record_exceptions(&mut self, op: u8, a: f64, b: f64, result: f64) {
        if matches!(op, 6 | 7) {
            let (dividend, divisor) = if op == 6 { (a, b) } else { (b, a) };
            if divisor == 0.0 && dividend != 0.0 && dividend.is_finite() {
                self.fpu.raise_exception(0x04); // ZE
                return;
            }
        }
        if result.is_nan() && !a.is_nan() && !b.is_nan() {
            self.fpu.raise_exception(0x01); // IE
        }
    }

    fn fpu_reg_arith_sti(&mut self, reg: u8, i: u8, pop: bool) -> ExecResult<CycleOutcome> {
        if reg == 2 || reg == 3 {
            return self.fpu_unsupported(if pop { 0xde } else { 0xdc });
        }
        // The DC/DE register encodings swap sub<->reverse-sub and div<->reverse-div
        // relative to the D8 forms; the destination is ST(i) and the source ST(0).
        let op = match reg {
            4 => 5,
            5 => 4,
            6 => 7,
            7 => 6,
            other => other,
        };
        let a = self.fpu.get(i);
        let b = self.fpu.get(0);
        let r = fpu_arith(op, a, b);
        self.fpu_record_exceptions(op, a, b, r);
        self.fpu.set(i, r);
        if pop {
            self.fpu.pop();
        }
        Ok(clocks(20))
    }

    fn fpu_d9_register(&mut self, byte: u8, i: u8) -> ExecResult<CycleOutcome> {
        match byte {
            0xc0..=0xc7 => {
                // FLD ST(i): push a copy.
                let v = self.fpu.get(i);
                self.fpu.push(v);
                Ok(clocks(4))
            }
            0xc8..=0xcf => {
                self.fpu.exchange(i);
                Ok(clocks(4))
            }
            0xd0 => Ok(clocks(4)), // FNOP
            0xe0 => {
                let v = -self.fpu.get(0);
                self.fpu.set(0, v);
                Ok(clocks(6))
            }
            0xe1 => {
                let v = self.fpu.get(0).abs();
                self.fpu.set(0, v);
                Ok(clocks(6))
            }
            0xe4 => {
                let a = self.fpu.get(0);
                self.fpu_compare(a, 0.0);
                Ok(clocks(4))
            }
            0xe5 => {
                self.fpu_examine();
                Ok(clocks(8))
            }
            0xe8 => {
                self.fpu.push(1.0);
                Ok(clocks(4))
            }
            0xe9 => {
                self.fpu.push(std::f64::consts::LOG2_10);
                Ok(clocks(8))
            }
            0xea => {
                self.fpu.push(std::f64::consts::LOG2_E);
                Ok(clocks(8))
            }
            0xeb => {
                self.fpu.push(std::f64::consts::PI);
                Ok(clocks(8))
            }
            0xec => {
                self.fpu.push(std::f64::consts::LOG10_2);
                Ok(clocks(8))
            }
            0xed => {
                self.fpu.push(std::f64::consts::LN_2);
                Ok(clocks(8))
            }
            0xee => {
                self.fpu.push(0.0);
                Ok(clocks(4))
            }
            0xfa => {
                let operand = self.fpu.get(0);
                let v = operand.sqrt();
                if v.is_nan() && !operand.is_nan() {
                    self.fpu.raise_exception(0x01); // IE: sqrt of a negative
                }
                self.fpu.set(0, v);
                Ok(clocks(70))
            }
            0xfc => {
                // FRNDINT: round to integer per the control word's RC field.
                let v = fpu_round_rc(self.fpu.control, self.fpu.get(0));
                self.fpu.set(0, v);
                Ok(clocks(20))
            }
            0xf6 => {
                self.fpu.dec_top();
                Ok(clocks(4))
            }
            0xf7 => {
                self.fpu.inc_top();
                Ok(clocks(4))
            }
            0xf0 => {
                // F2XM1: ST0 = 2^ST0 - 1.
                let v = self.fpu.get(0);
                self.fpu.set(0, v.exp2() - 1.0);
                Ok(clocks(200))
            }
            0xf1 => {
                // FYL2X: ST1 = ST1 * log2(ST0), then pop.
                let x = self.fpu.get(0);
                let y = self.fpu.get(1);
                self.fpu.set(1, y * x.log2());
                self.fpu.pop();
                Ok(clocks(300))
            }
            0xf2 => {
                // FPTAN: ST0 = tan(ST0), then push 1.0. C2 cleared (reduction complete).
                let v = self.fpu.get(0);
                self.fpu.set(0, v.tan());
                self.fpu.push(1.0);
                self.fpu.set_condition(false, false, false, false);
                Ok(clocks(300))
            }
            0xf3 => {
                // FPATAN: ST1 = atan2(ST1, ST0), then pop.
                let x = self.fpu.get(0);
                let y = self.fpu.get(1);
                self.fpu.set(1, y.atan2(x));
                self.fpu.pop();
                Ok(clocks(300))
            }
            0xf4 => {
                // FXTRACT: ST0 = unbiased exponent, then push the significand (in [1,2)).
                let v = self.fpu.get(0);
                if v == 0.0 || !v.is_finite() {
                    let exponent = if v == 0.0 { f64::NEG_INFINITY } else { v };
                    self.fpu.set(0, exponent);
                    self.fpu.push(v);
                } else {
                    let exponent = v.abs().log2().floor();
                    let significand = v / exponent.exp2();
                    self.fpu.set(0, exponent);
                    self.fpu.push(significand);
                }
                Ok(clocks(70))
            }
            0xf5 => {
                // FPREM1: IEEE partial remainder (round-to-nearest quotient).
                self.fpu_partial_remainder(true);
                Ok(clocks(100))
            }
            0xf8 => {
                // FPREM: 8087-style partial remainder (truncated quotient).
                self.fpu_partial_remainder(false);
                Ok(clocks(100))
            }
            0xf9 => {
                // FYL2XP1: ST1 = ST1 * log2(ST0 + 1), then pop.
                let x = self.fpu.get(0);
                let y = self.fpu.get(1);
                self.fpu.set(1, y * (x.ln_1p() / std::f64::consts::LN_2));
                self.fpu.pop();
                Ok(clocks(300))
            }
            0xfb => {
                // FSINCOS: ST0 = sin, then push cos. Result: ST1 = sin, ST0 = cos.
                let v = self.fpu.get(0);
                self.fpu.set(0, v.sin());
                self.fpu.push(v.cos());
                self.fpu.set_condition(false, false, false, false);
                Ok(clocks(300))
            }
            0xfd => {
                // FSCALE: ST0 = ST0 * 2^trunc(ST1).
                let st0 = self.fpu.get(0);
                let st1 = self.fpu.get(1);
                self.fpu.set(0, st0 * st1.trunc().exp2());
                Ok(clocks(30))
            }
            0xfe => {
                // FSIN.
                let v = self.fpu.get(0);
                self.fpu.set(0, v.sin());
                self.fpu.set_condition(false, false, false, false);
                Ok(clocks(300))
            }
            0xff => {
                // FCOS.
                let v = self.fpu.get(0);
                self.fpu.set(0, v.cos());
                self.fpu.set_condition(false, false, false, false);
                Ok(clocks(300))
            }
            _ => self.fpu_unsupported(0xd9),
        }
    }

    fn fpu_partial_remainder(&mut self, round_nearest: bool) {
        // Limit: computes the full remainder in one step and reports reduction
        // complete (C2=0). Real hardware reduces partially and may take several FPREM
        // iterations for large quotients; FPU exceptions are not modeled yet.
        let dividend = self.fpu.get(0);
        let divisor = self.fpu.get(1);
        if divisor == 0.0 || !dividend.is_finite() || !divisor.is_finite() {
            self.fpu.set_condition(false, false, false, false);
            return;
        }
        let ratio = dividend / divisor;
        let quotient = if round_nearest {
            ratio.round_ties_even()
        } else {
            ratio.trunc()
        };
        let remainder = dividend - divisor * quotient;
        self.fpu.set(0, remainder);
        let q = quotient as i64;
        let c0 = (q >> 2) & 1 != 0;
        let c3 = (q >> 1) & 1 != 0;
        let c1 = q & 1 != 0;
        self.fpu.set_condition(c3, false, c1, c0);
    }

    fn fpu_db_register(&mut self, byte: u8) -> ExecResult<CycleOutcome> {
        match byte {
            0xe0 | 0xe1 | 0xe4 => Ok(clocks(2)), // FNENI / FNDISI / FNSETPM: 387 no-ops
            0xe2 => {
                self.fpu.clear_exceptions();
                Ok(clocks(2))
            }
            0xe3 => {
                self.fpu.finit();
                Ok(clocks(3))
            }
            _ => self.fpu_unsupported(0xdb),
        }
    }

    fn fpu_dd_register(&mut self, reg: u8, i: u8) -> ExecResult<CycleOutcome> {
        match reg {
            0 => {
                self.fpu.free(i);
                Ok(clocks(3))
            }
            2 | 3 => {
                let v = self.fpu.get(0);
                self.fpu.set(i, v);
                if reg == 3 {
                    self.fpu.pop();
                }
                Ok(clocks(3))
            }
            4 | 5 => {
                // FUCOM / FUCOMP. Limit: treated like FCOM/FCOMP; the unordered-vs-
                // signaling NaN distinction is not yet modeled (no exceptions).
                let a = self.fpu.get(0);
                let b = self.fpu.get(i);
                self.fpu_compare(a, b);
                if reg == 5 {
                    self.fpu.pop();
                }
                Ok(clocks(4))
            }
            _ => self.fpu_unsupported(0xdd),
        }
    }

    fn fpu_compare(&mut self, a: f64, b: f64) {
        let (c3, c2, c0) = if a.is_nan() || b.is_nan() {
            (true, true, true) // unordered
        } else if a < b {
            (false, false, true)
        } else if a > b {
            (false, false, false)
        } else {
            (true, false, false) // equal
        };
        self.fpu.set_condition(c3, c2, false, c0);
    }

    fn fpu_examine(&mut self) {
        // FXAM: classify ST(0) into C3/C2/C0, with C1 = sign. Denormals are not
        // distinguished from normals here.
        let v = self.fpu.get(0);
        let sign = v.is_sign_negative();
        let (c3, c2, c0) = if self.fpu.is_empty(0) {
            (true, false, true)
        } else if v.is_nan() {
            (false, false, true)
        } else if v.is_infinite() {
            (false, true, true)
        } else if v == 0.0 {
            (true, false, false)
        } else {
            (false, true, false)
        };
        self.fpu.set_condition(c3, c2, sign, c0);
    }

    pub(super) fn fpu_unsupported(&self, _opcode: u8) -> ExecResult<CycleOutcome> {
        Err(undefined_opcode())
    }

    fn read_real32<B: CpuBus>(&mut self, bus: &mut B, mem: MemoryOperand) -> ExecResult<f64> {
        let bits = self.read_memory_sized(
            bus,
            mem.segment,
            mem.offset,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )?;
        Ok(f64::from(f32::from_bits(bits)))
    }

    /// Read eight bytes as a little-endian u64. The one primitive behind FLD m64,
    /// FILD m64, the FLD m80 mantissa, and FNSAVE/FRSTOR.
    pub(super) fn read_qword<B: CpuBus>(
        &mut self,
        bus: &mut B,
        mem: MemoryOperand,
    ) -> ExecResult<u64> {
        let lo = self.read_memory_sized(
            bus,
            mem.segment,
            mem.offset,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )?;
        let hi = self.read_memory_sized(
            bus,
            mem.segment,
            mem.offset.wrapping_add(4),
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )?;
        Ok((u64::from(hi) << 32) | u64::from(lo))
    }

    pub(super) fn write_qword<B: CpuBus>(
        &mut self,
        bus: &mut B,
        mem: MemoryOperand,
        value: u64,
    ) -> ExecResult<()> {
        self.write_memory_sized(
            bus,
            mem.segment,
            mem.offset,
            OperandSize::Dword,
            value as u32,
            BusAccessKind::DataWrite,
        )?;
        self.write_memory_sized(
            bus,
            mem.segment,
            mem.offset.wrapping_add(4),
            OperandSize::Dword,
            (value >> 32) as u32,
            BusAccessKind::DataWrite,
        )
    }

    fn read_real64<B: CpuBus>(&mut self, bus: &mut B, mem: MemoryOperand) -> ExecResult<f64> {
        Ok(f64::from_bits(self.read_qword(bus, mem)?))
    }

    fn write_real32<B: CpuBus>(
        &mut self,
        bus: &mut B,
        mem: MemoryOperand,
        value: f64,
    ) -> ExecResult<()> {
        let bits = (value as f32).to_bits();
        self.write_memory_sized(
            bus,
            mem.segment,
            mem.offset,
            OperandSize::Dword,
            bits,
            BusAccessKind::DataWrite,
        )
    }

    fn write_real64<B: CpuBus>(
        &mut self,
        bus: &mut B,
        mem: MemoryOperand,
        value: f64,
    ) -> ExecResult<()> {
        self.write_qword(bus, mem, value.to_bits())
    }

    fn read_int16<B: CpuBus>(&mut self, bus: &mut B, mem: MemoryOperand) -> ExecResult<f64> {
        let v = self.read_memory_sized(
            bus,
            mem.segment,
            mem.offset,
            OperandSize::Word,
            BusAccessKind::DataRead,
        )?;
        Ok(f64::from(v as u16 as i16))
    }

    fn read_int32<B: CpuBus>(&mut self, bus: &mut B, mem: MemoryOperand) -> ExecResult<f64> {
        let v = self.read_memory_sized(
            bus,
            mem.segment,
            mem.offset,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )?;
        Ok(f64::from(v as i32))
    }

    fn read_int64<B: CpuBus>(&mut self, bus: &mut B, mem: MemoryOperand) -> ExecResult<f64> {
        Ok(self.read_qword(bus, mem)? as i64 as f64)
    }

    fn write_int16<B: CpuBus>(
        &mut self,
        bus: &mut B,
        mem: MemoryOperand,
        value: f64,
    ) -> ExecResult<()> {
        let r = fpu_round_rc(self.fpu.control, value);
        let v = if r.is_nan() || !(-32768.0..=32767.0).contains(&r) {
            // Out of range: the masked #IA response is the integer indefinite.
            self.fpu.raise_exception(0x01);
            0x8000u16
        } else {
            r as i16 as u16
        };
        self.write_memory_sized(
            bus,
            mem.segment,
            mem.offset,
            OperandSize::Word,
            u32::from(v),
            BusAccessKind::DataWrite,
        )
    }

    fn write_int32<B: CpuBus>(
        &mut self,
        bus: &mut B,
        mem: MemoryOperand,
        value: f64,
    ) -> ExecResult<()> {
        let r = fpu_round_rc(self.fpu.control, value);
        let v = if r.is_nan() || !(-2147483648.0..=2147483647.0).contains(&r) {
            self.fpu.raise_exception(0x01);
            0x8000_0000u32
        } else {
            r as i32 as u32
        };
        self.write_memory_sized(
            bus,
            mem.segment,
            mem.offset,
            OperandSize::Dword,
            v,
            BusAccessKind::DataWrite,
        )
    }

    fn write_int64<B: CpuBus>(
        &mut self,
        bus: &mut B,
        mem: MemoryOperand,
        value: f64,
    ) -> ExecResult<()> {
        let r = fpu_round_rc(self.fpu.control, value);
        // 2^63 is exactly representable; anything at or beyond it (or NaN) is out of
        // range for i64 and stores the integer indefinite.
        let v = if r.is_nan()
            || !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&r)
        {
            self.fpu.raise_exception(0x01);
            0x8000_0000_0000_0000u64
        } else {
            r as i64 as u64
        };
        self.write_qword(bus, mem, v)
    }

    fn read_extended80<B: CpuBus>(&mut self, bus: &mut B, mem: MemoryOperand) -> ExecResult<f64> {
        let mantissa = self.read_qword(bus, mem)?;
        let se = self.read_memory_sized(
            bus,
            mem.segment,
            mem.offset.wrapping_add(8),
            OperandSize::Word,
            BusAccessKind::DataRead,
        )?;
        let sign = (se >> 15) & 1 == 1;
        let exponent = (se & 0x7fff) as i32;
        let value = if exponent == 0 && mantissa == 0 {
            0.0
        } else if exponent == 0x7fff {
            // Integer bit set with an empty fraction is infinity; otherwise NaN.
            if mantissa == 0x8000_0000_0000_0000 {
                f64::INFINITY
            } else {
                f64::NAN
            }
        } else {
            // value = mantissa * 2^(exponent - bias - 63), where the 64-bit mantissa
            // carries the explicit integer bit. f64 keeps 53 of those bits.
            (mantissa as f64) * 2.0f64.powi(exponent - 16383 - 63)
        };
        Ok(if sign { -value } else { value })
    }

    fn write_extended80<B: CpuBus>(
        &mut self,
        bus: &mut B,
        mem: MemoryOperand,
        value: f64,
    ) -> ExecResult<()> {
        let sign = value.is_sign_negative();
        let (mantissa, exponent) = if value == 0.0 {
            (0u64, 0u16)
        } else if value.is_nan() {
            (0xc000_0000_0000_0000, 0x7fff)
        } else if value.is_infinite() {
            (0x8000_0000_0000_0000, 0x7fff)
        } else {
            let bits = value.abs().to_bits();
            let biased = ((bits >> 52) & 0x7ff) as i32;
            let fraction = bits & 0x000f_ffff_ffff_ffff;
            if biased == 0 {
                // Subnormal f64. Limit: rare path normalized by scaling, not exact bits.
                let e = value.abs().log2().floor() as i32;
                let m = (value.abs() / 2.0f64.powi(e) * 2.0f64.powi(63)) as u64;
                (m, (e + 16383) as u16)
            } else {
                // Move the implicit integer bit out and shift the 52-bit fraction up.
                (
                    (1u64 << 63) | (fraction << 11),
                    (biased - 1023 + 16383) as u16,
                )
            }
        };
        let se = (u16::from(sign) << 15) | exponent;
        self.write_qword(bus, mem, mantissa)?;
        self.write_memory_sized(
            bus,
            mem.segment,
            mem.offset.wrapping_add(8),
            OperandSize::Word,
            u32::from(se),
            BusAccessKind::DataWrite,
        )
    }

    fn fpu_store_environment<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        operand_size: OperandSize,
    ) -> ExecResult<u32> {
        // Limit: the instruction and data pointers (the last four env slots) are
        // written as zero; the core does not track the last FPU instruction or
        // operand address yet. Control/status/tag are exact. Returns the env size.
        let control = u32::from(self.fpu.control);
        let status = u32::from(self.fpu.status);
        let tag = u32::from(self.fpu.tag);
        let (size, step) = match operand_size {
            OperandSize::Word => (OperandSize::Word, 2u32),
            OperandSize::Dword => (OperandSize::Dword, 4u32),
        };
        for (slot, value) in [control, status, tag].into_iter().enumerate() {
            let at = offset + step * slot as u32;
            self.write_memory_sized(bus, segment, at, size, value, BusAccessKind::DataWrite)?;
        }
        for slot in 3..7 {
            let at = offset + step * slot;
            self.write_memory_sized(bus, segment, at, size, 0, BusAccessKind::DataWrite)?;
        }
        Ok(step * 7)
    }

    fn fpu_load_environment<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        operand_size: OperandSize,
    ) -> ExecResult<u32> {
        let (size, step) = match operand_size {
            OperandSize::Word => (OperandSize::Word, 2u32),
            OperandSize::Dword => (OperandSize::Dword, 4u32),
        };
        let control =
            self.read_memory_sized(bus, segment, offset, size, BusAccessKind::DataRead)?;
        let status =
            self.read_memory_sized(bus, segment, offset + step, size, BusAccessKind::DataRead)?;
        let tag = self.read_memory_sized(
            bus,
            segment,
            offset + step * 2,
            size,
            BusAccessKind::DataRead,
        )?;
        self.fpu.control = control as u16;
        self.fpu.status = status as u16;
        self.fpu.tag = tag as u16;
        Ok(step * 7)
    }

    fn fpu_save_state<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        operand_size: OperandSize,
    ) -> ExecResult<()> {
        // Limit: registers are saved in stack order ST(0)..ST(7); hardware uses
        // physical order R0..R7. This round-trips with fpu_restore_state, which is the
        // common use; software that hand-parses the image would see a different order.
        let env = self.fpu_store_environment(bus, segment, offset, operand_size)?;
        for i in 0..8u32 {
            let value = self.fpu.get(i as u8);
            let mem = MemoryOperand {
                segment,
                offset: offset + env + i * 10,
            };
            self.write_extended80(bus, mem, value)?;
        }
        self.fpu.finit();
        Ok(())
    }

    fn fpu_restore_state<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        operand_size: OperandSize,
    ) -> ExecResult<()> {
        let env = self.fpu_load_environment(bus, segment, offset, operand_size)?;
        let saved_tag = self.fpu.tag;
        for i in 0..8u32 {
            let mem = MemoryOperand {
                segment,
                offset: offset + env + i * 10,
            };
            let value = self.read_extended80(bus, mem)?;
            self.fpu.set(i as u8, value);
        }
        // set() recomputed tags from the values; the saved tag word is authoritative.
        self.fpu.tag = saved_tag;
        Ok(())
    }

    fn read_bcd80<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
    ) -> ExecResult<f64> {
        let d0 = self.read_memory_sized(
            bus,
            segment,
            offset,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )?;
        let d1 = self.read_memory_sized(
            bus,
            segment,
            offset + 4,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )?;
        let w = self.read_memory_sized(
            bus,
            segment,
            offset + 8,
            OperandSize::Word,
            BusAccessKind::DataRead,
        )?;
        // Nine magnitude bytes (18 packed digits), least significant first; the sign is
        // bit 7 of the tenth byte.
        let raw = [
            d0 as u8,
            (d0 >> 8) as u8,
            (d0 >> 16) as u8,
            (d0 >> 24) as u8,
            d1 as u8,
            (d1 >> 8) as u8,
            (d1 >> 16) as u8,
            (d1 >> 24) as u8,
            w as u8,
        ];
        let negative = (w >> 8) & 0x80 != 0;
        let mut value: i64 = 0;
        for &b in raw.iter().rev() {
            value = value * 100 + i64::from((b >> 4) * 10 + (b & 0x0f));
        }
        let v = value as f64;
        Ok(if negative { -v } else { v })
    }

    fn write_bcd80<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        value: f64,
    ) -> ExecResult<()> {
        let rounded = fpu_round_rc(self.fpu.control, value);
        let negative = rounded.is_sign_negative();
        // BCD overflow past 18 digits is not detected because the current game
        // corpus never exercises FBSTP. Add the packed-BCD indefinite if needed.
        let mut magnitude = rounded.abs() as u64;
        let mut raw = [0u8; 9];
        for slot in raw.iter_mut() {
            let lo = (magnitude % 10) as u8;
            magnitude /= 10;
            let hi = (magnitude % 10) as u8;
            magnitude /= 10;
            *slot = (hi << 4) | lo;
        }
        let d0 = u32::from(raw[0])
            | (u32::from(raw[1]) << 8)
            | (u32::from(raw[2]) << 16)
            | (u32::from(raw[3]) << 24);
        let d1 = u32::from(raw[4])
            | (u32::from(raw[5]) << 8)
            | (u32::from(raw[6]) << 16)
            | (u32::from(raw[7]) << 24);
        let w = u32::from(raw[8]) | if negative { 0x8000 } else { 0 };
        self.write_memory_sized(
            bus,
            segment,
            offset,
            OperandSize::Dword,
            d0,
            BusAccessKind::DataWrite,
        )?;
        self.write_memory_sized(
            bus,
            segment,
            offset + 4,
            OperandSize::Dword,
            d1,
            BusAccessKind::DataWrite,
        )?;
        self.write_memory_sized(
            bus,
            segment,
            offset + 8,
            OperandSize::Word,
            w,
            BusAccessKind::DataWrite,
        )
    }
}
