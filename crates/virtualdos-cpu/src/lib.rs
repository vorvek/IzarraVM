use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CpuError {
    #[error("instruction fetch at {address:#x} is outside memory")]
    FetchOutOfBounds { address: usize },
    #[error("unsupported opcode {opcode:#04x} at {address:#x}")]
    UnsupportedOpcode { opcode: u8, address: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reg16 {
    Ax,
    Cx,
    Dx,
    Bx,
    Sp,
    Bp,
    Si,
    Di,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instruction {
    Nop,
    MovRegImm16 { reg: Reg16, imm: u16 },
    AddAxImm16 { imm: u16 },
    Int { vector: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedInstruction {
    pub instruction: Instruction,
    pub length: u16,
    pub clocks_386: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepTrap {
    SoftwareInterrupt(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepOutcome {
    pub decoded: DecodedInstruction,
    pub trap: Option<StepTrap>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registers {
    pub ax: u16,
    pub cx: u16,
    pub dx: u16,
    pub bx: u16,
    pub sp: u16,
    pub bp: u16,
    pub si: u16,
    pub di: u16,
    pub cs: u16,
    pub ds: u16,
    pub es: u16,
    pub ss: u16,
    pub ip: u16,
    pub flags: u16,
}

impl Default for Registers {
    fn default() -> Self {
        Self {
            ax: 0,
            cx: 0,
            dx: 0,
            bx: 0,
            sp: 0xfffe,
            bp: 0,
            si: 0,
            di: 0,
            cs: 0,
            ds: 0,
            es: 0,
            ss: 0,
            ip: 0,
            flags: 0x0002,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cpu386 {
    pub registers: Registers,
    pub elapsed_clocks: u64,
}

impl Cpu386 {
    pub fn linear_ip(&self) -> usize {
        linear_address(self.registers.cs, self.registers.ip)
    }

    pub fn step(&mut self, memory: &[u8]) -> Result<StepOutcome, CpuError> {
        let address = self.linear_ip();
        let decoded = decode_at(memory, address)?;
        self.registers.ip = self.registers.ip.wrapping_add(decoded.length);
        self.elapsed_clocks += u64::from(decoded.clocks_386);

        let trap = match decoded.instruction {
            Instruction::Nop => None,
            Instruction::MovRegImm16 { reg, imm } => {
                self.write_reg16(reg, imm);
                None
            }
            Instruction::AddAxImm16 { imm } => {
                let (value, carry) = self.registers.ax.overflowing_add(imm);
                self.registers.ax = value;
                self.set_carry(carry);
                self.set_zero(value == 0);
                None
            }
            Instruction::Int { vector } => Some(StepTrap::SoftwareInterrupt(vector)),
        };

        Ok(StepOutcome { decoded, trap })
    }

    pub fn read_reg16(&self, reg: Reg16) -> u16 {
        match reg {
            Reg16::Ax => self.registers.ax,
            Reg16::Cx => self.registers.cx,
            Reg16::Dx => self.registers.dx,
            Reg16::Bx => self.registers.bx,
            Reg16::Sp => self.registers.sp,
            Reg16::Bp => self.registers.bp,
            Reg16::Si => self.registers.si,
            Reg16::Di => self.registers.di,
        }
    }

    pub fn write_reg16(&mut self, reg: Reg16, value: u16) {
        match reg {
            Reg16::Ax => self.registers.ax = value,
            Reg16::Cx => self.registers.cx = value,
            Reg16::Dx => self.registers.dx = value,
            Reg16::Bx => self.registers.bx = value,
            Reg16::Sp => self.registers.sp = value,
            Reg16::Bp => self.registers.bp = value,
            Reg16::Si => self.registers.si = value,
            Reg16::Di => self.registers.di = value,
        }
    }

    fn set_carry(&mut self, enabled: bool) {
        set_flag(&mut self.registers.flags, 0x0001, enabled);
    }

    fn set_zero(&mut self, enabled: bool) {
        set_flag(&mut self.registers.flags, 0x0040, enabled);
    }
}

pub fn decode_at(memory: &[u8], address: usize) -> Result<DecodedInstruction, CpuError> {
    let opcode = *memory
        .get(address)
        .ok_or(CpuError::FetchOutOfBounds { address })?;

    match opcode {
        0x90 => Ok(DecodedInstruction {
            instruction: Instruction::Nop,
            length: 1,
            clocks_386: 3,
        }),
        0xb8..=0xbf => {
            let imm = read_u16(memory, address + 1)?;
            Ok(DecodedInstruction {
                instruction: Instruction::MovRegImm16 {
                    reg: reg16_from_low_opcode_bits(opcode),
                    imm,
                },
                length: 3,
                clocks_386: 2,
            })
        }
        0x05 => {
            let imm = read_u16(memory, address + 1)?;
            Ok(DecodedInstruction {
                instruction: Instruction::AddAxImm16 { imm },
                length: 3,
                clocks_386: 2,
            })
        }
        0xcd => {
            let vector_address = address + 1;
            let vector = *memory
                .get(vector_address)
                .ok_or(CpuError::FetchOutOfBounds {
                    address: vector_address,
                })?;
            Ok(DecodedInstruction {
                instruction: Instruction::Int { vector },
                length: 2,
                clocks_386: 37,
            })
        }
        _ => Err(CpuError::UnsupportedOpcode { opcode, address }),
    }
}

pub fn linear_address(segment: u16, offset: u16) -> usize {
    (usize::from(segment) << 4) + usize::from(offset)
}

fn read_u16(memory: &[u8], address: usize) -> Result<u16, CpuError> {
    let low = *memory
        .get(address)
        .ok_or(CpuError::FetchOutOfBounds { address })?;
    let high_address = address + 1;
    let high = *memory.get(high_address).ok_or(CpuError::FetchOutOfBounds {
        address: high_address,
    })?;
    Ok(u16::from_le_bytes([low, high]))
}

fn reg16_from_low_opcode_bits(opcode: u8) -> Reg16 {
    match opcode & 0x07 {
        0 => Reg16::Ax,
        1 => Reg16::Cx,
        2 => Reg16::Dx,
        3 => Reg16::Bx,
        4 => Reg16::Sp,
        5 => Reg16::Bp,
        6 => Reg16::Si,
        _ => Reg16::Di,
    }
}

fn set_flag(flags: &mut u16, mask: u16, enabled: bool) {
    if enabled {
        *flags |= mask;
    } else {
        *flags &= !mask;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_nop_with_386_timing() {
        let decoded = decode_at(&[0x90], 0).unwrap();
        assert_eq!(decoded.instruction, Instruction::Nop);
        assert_eq!(decoded.length, 1);
        assert_eq!(decoded.clocks_386, 3);
    }

    #[test]
    fn executes_mov_reg_imm16() {
        let mut cpu = Cpu386::default();
        let outcome = cpu.step(&[0xbb, 0x34, 0x12]).unwrap();

        assert_eq!(
            outcome.decoded.instruction,
            Instruction::MovRegImm16 {
                reg: Reg16::Bx,
                imm: 0x1234
            }
        );
        assert_eq!(cpu.registers.bx, 0x1234);
        assert_eq!(cpu.registers.ip, 3);
        assert_eq!(cpu.elapsed_clocks, 2);
    }

    #[test]
    fn executes_add_ax_imm16_and_sets_flags() {
        let mut cpu = Cpu386::default();
        cpu.registers.ax = 0xffff;

        cpu.step(&[0x05, 0x01, 0x00]).unwrap();

        assert_eq!(cpu.registers.ax, 0);
        assert_ne!(cpu.registers.flags & 0x0001, 0);
        assert_ne!(cpu.registers.flags & 0x0040, 0);
    }

    #[test]
    fn int_instruction_surfaces_trap() {
        let mut cpu = Cpu386::default();
        let outcome = cpu.step(&[0xcd, 0x21]).unwrap();

        assert_eq!(outcome.trap, Some(StepTrap::SoftwareInterrupt(0x21)));
        assert_eq!(cpu.registers.ip, 2);
    }

    #[test]
    fn segmented_ip_fetches_from_linear_address() {
        let mut cpu = Cpu386::default();
        cpu.registers.cs = 0x1000;
        cpu.registers.ip = 0x0010;
        let mut memory = vec![0; linear_address(0x1000, 0x0010) + 1];
        memory[linear_address(0x1000, 0x0010)] = 0x90;

        cpu.step(&memory).unwrap();

        assert_eq!(cpu.registers.ip, 0x0011);
    }
}
