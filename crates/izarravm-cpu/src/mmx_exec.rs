// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl CpuGsw {
    // ================================ MMX ================================
    // 0F-extended integer SIMD on the eight 64-bit MMX registers (mmx.rs holds the
    // lane math). EMMS takes no operand; the shift-by-immediate forms (0F 71/72/73)
    // and MOVD/MOVQ have their own operand shapes; everything else is the regular
    // Pxxx mm, mm/m64 form.

    /// The MMX integer-SIMD block through the decode/execute split (task A14). `decode` already
    /// fetched the ModRM (for every opcode except EMMS, which has none) and, for the shift-by-imm
    /// forms (0F 71/72/73), the trailing imm8 into `insn.imm`; this never re-reads an instruction
    /// byte. The r/m operand resolves from the pre-decoded descriptor against the live registers.
    /// The lane math (`mmx::*`) and the #UD for an unmapped sub-op (`fpu_unsupported`) are reused
    /// verbatim, so the only change from the former `execute_mmx` is WHERE the ModRM/imm came from.
    pub(super) fn execute_mmx_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        let opcode = insn.opcode as u8;
        if opcode == 0x77 {
            self.fpu.emms();
            return Ok(clocks(6));
        }
        let modrm = insn.modrm.expect("an MMX opcode decoded with a ModRM");

        if matches!(opcode, 0x71..=0x73) {
            // Shift mm by an immediate; modrm.reg selects the shift, modrm.rm the register.
            // `decode` fetched the imm8 count into `insn.imm`.
            let count = u64::from(insn.imm);
            let target = modrm.rm;
            let a = self.fpu.mm(target);
            let result = match (opcode, modrm.reg) {
                (0x71, 2) => mmx::psrlw(a, count),
                (0x71, 4) => mmx::psraw(a, count),
                (0x71, 6) => mmx::psllw(a, count),
                (0x72, 2) => mmx::psrld(a, count),
                (0x72, 4) => mmx::psrad(a, count),
                (0x72, 6) => mmx::pslld(a, count),
                (0x73, 2) => mmx::psrlq(a, count),
                (0x73, 6) => mmx::psllq(a, count),
                _ => return self.fpu_unsupported(opcode),
            };
            self.fpu.set_mm(target, result);
            return Ok(clocks(6));
        }

        let (_, operand) = self.resolve_decoded_modrm_operand(insn);
        let dest = modrm.reg;
        match opcode {
            0x6e => {
                // MOVD mm, r/m32: zero-extend a dword into the register.
                let v = self.read_operand_sized(bus, operand, OperandSize::Dword)?;
                self.fpu.set_mm(dest, u64::from(v));
                return Ok(clocks(4));
            }
            0x7e => {
                // MOVD r/m32, mm: store the low dword.
                let v = self.fpu.mm(dest) as u32;
                self.write_operand_sized(bus, operand, OperandSize::Dword, v)?;
                return Ok(clocks(4));
            }
            0x6f => {
                // MOVQ mm, mm/m64.
                let v = self.read_mmx_operand(bus, operand)?;
                self.fpu.set_mm(dest, v);
                return Ok(clocks(4));
            }
            0x7f => {
                // MOVQ mm/m64, mm.
                let v = self.fpu.mm(dest);
                self.write_mmx_operand(bus, operand, v)?;
                return Ok(clocks(4));
            }
            _ => {}
        }

        let src = self.read_mmx_operand(bus, operand)?;
        let a = self.fpu.mm(dest);
        let result = match opcode {
            0x60 => mmx::punpcklbw(a, src),
            0x61 => mmx::punpcklwd(a, src),
            0x62 => mmx::punpckldq(a, src),
            0x63 => mmx::packsswb(a, src),
            0x64 => mmx::pcmpgt_b(a, src),
            0x65 => mmx::pcmpgt_w(a, src),
            0x66 => mmx::pcmpgt_d(a, src),
            0x67 => mmx::packuswb(a, src),
            0x68 => mmx::punpckhbw(a, src),
            0x69 => mmx::punpckhwd(a, src),
            0x6a => mmx::punpckhdq(a, src),
            0x6b => mmx::packssdw(a, src),
            0x74 => mmx::pcmpeq_b(a, src),
            0x75 => mmx::pcmpeq_w(a, src),
            0x76 => mmx::pcmpeq_d(a, src),
            0xd1 => mmx::psrlw(a, src),
            0xd2 => mmx::psrld(a, src),
            0xd3 => mmx::psrlq(a, src),
            0xd5 => mmx::pmullw(a, src),
            0xd8 => mmx::psubus_b(a, src),
            0xd9 => mmx::psubus_w(a, src),
            0xdb => mmx::pand(a, src),
            0xdc => mmx::paddus_b(a, src),
            0xdd => mmx::paddus_w(a, src),
            0xdf => mmx::pandn(a, src),
            0xe1 => mmx::psraw(a, src),
            0xe2 => mmx::psrad(a, src),
            0xe5 => mmx::pmulhw(a, src),
            0xe8 => mmx::psubs_b(a, src),
            0xe9 => mmx::psubs_w(a, src),
            0xeb => mmx::por(a, src),
            0xec => mmx::padds_b(a, src),
            0xed => mmx::padds_w(a, src),
            0xef => mmx::pxor(a, src),
            0xf1 => mmx::psllw(a, src),
            0xf2 => mmx::pslld(a, src),
            0xf3 => mmx::psllq(a, src),
            0xf5 => mmx::pmaddwd(a, src),
            0xf8 => mmx::psub_b(a, src),
            0xf9 => mmx::psub_w(a, src),
            0xfa => mmx::psub_d(a, src),
            0xfc => mmx::padd_b(a, src),
            0xfd => mmx::padd_w(a, src),
            0xfe => mmx::padd_d(a, src),
            _ => return self.fpu_unsupported(opcode),
        };
        self.fpu.set_mm(dest, result);
        Ok(clocks(4))
    }

    /// Read an MMX r/m operand (a register MMX value or an m64 from memory) from the pre-resolved
    /// operand `decode` produced. The register form indexes the MMX file by `rm`; the memory form
    /// reads the 64-bit value against the live effective address.
    fn read_mmx_operand<B: CpuBus>(&mut self, bus: &mut B, operand: RmOperand) -> ExecResult<u64> {
        match operand {
            RmOperand::Register(index) => Ok(self.fpu.mm(index)),
            RmOperand::Memory(mem) => self.read_qword(bus, mem),
        }
    }

    fn write_mmx_operand<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand: RmOperand,
        value: u64,
    ) -> ExecResult<()> {
        match operand {
            RmOperand::Register(index) => {
                self.fpu.set_mm(index, value);
                Ok(())
            }
            RmOperand::Memory(mem) => self.write_qword(bus, mem, value),
        }
    }
}
