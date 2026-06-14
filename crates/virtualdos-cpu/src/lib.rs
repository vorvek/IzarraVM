use thiserror::Error;
use virtualdos_bus::{BusAccessKind, BusError, BusWidth, CpuBus};

const FLAG_CF: u32 = 0x0000_0001;
const FLAG_PF: u32 = 0x0000_0004;
const FLAG_AF: u32 = 0x0000_0010;
const FLAG_ZF: u32 = 0x0000_0040;
const FLAG_SF: u32 = 0x0000_0080;
const FLAG_TF: u32 = 0x0000_0100;
const FLAG_IF: u32 = 0x0000_0200;
const FLAG_DF: u32 = 0x0000_0400;
const FLAG_OF: u32 = 0x0000_0800;

const CR0_PE: u32 = 0x0000_0001;
const CR0_PG: u32 = 0x8000_0000;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CpuError {
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error("unsupported opcode {opcode:#04x} at CS:EIP {cs:#06x}:{eip:#010x}")]
    UnsupportedOpcode { opcode: u8, cs: u16, eip: u32 },
    #[error("unsupported 0f opcode {opcode:#04x} at CS:EIP {cs:#06x}:{eip:#010x}")]
    UnsupportedTwoByteOpcode { opcode: u8, cs: u16, eip: u32 },
    #[error("unsupported group opcode extension /{extension} for opcode {opcode:#04x}")]
    UnsupportedGroupOpcode { opcode: u8, extension: u8 },
    #[error("segment limit violation in {segment:?}: offset {offset:#010x}, width {width}")]
    SegmentLimit {
        segment: SegmentIndex,
        offset: u32,
        width: u32,
    },
    #[error("general protection fault while loading selector {selector:#06x}")]
    GeneralProtection { selector: u16 },
    #[error("IDT vector {vector} is outside IDTR limit")]
    IdtLimit { vector: u8 },
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

impl Reg16 {
    const fn index(self) -> usize {
        match self {
            Self::Ax => 0,
            Self::Cx => 1,
            Self::Dx => 2,
            Self::Bx => 3,
            Self::Sp => 4,
            Self::Bp => 5,
            Self::Si => 6,
            Self::Di => 7,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentIndex {
    Es,
    Cs,
    Ss,
    Ds,
    Fs,
    Gs,
}

impl SegmentIndex {
    const fn index(self) -> usize {
        match self {
            Self::Es => 0,
            Self::Cs => 1,
            Self::Ss => 2,
            Self::Ds => 3,
            Self::Fs => 4,
            Self::Gs => 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentRegister {
    pub selector: u16,
    pub base: u32,
    pub limit: u32,
    pub access: u8,
    pub default_size_32: bool,
}

impl SegmentRegister {
    pub const fn real(selector: u16) -> Self {
        Self {
            selector,
            base: (selector as u32) << 4,
            limit: 0x0000_ffff,
            access: 0x93,
            default_size_32: false,
        }
    }

    pub const fn reset_cs() -> Self {
        Self {
            selector: 0xf000,
            base: 0xffff_0000,
            limit: 0x0000_ffff,
            access: 0x9b,
            default_size_32: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DescriptorTable {
    pub base: u32,
    pub limit: u16,
}

impl Default for DescriptorTable {
    fn default() -> Self {
        Self {
            base: 0,
            limit: 0x03ff,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registers {
    gpr: [u32; 8],
    segments: [SegmentRegister; 6],
    pub eip: u32,
    pub eflags: u32,
}

impl Default for Registers {
    fn default() -> Self {
        let zero = SegmentRegister::real(0);
        Self {
            gpr: [0; 8],
            segments: [zero, SegmentRegister::reset_cs(), zero, zero, zero, zero],
            eip: 0x0000_fff0,
            eflags: 0x0000_0002,
        }
    }
}

impl Registers {
    pub fn eax(&self) -> u32 {
        self.gpr[0]
    }

    pub fn ecx(&self) -> u32 {
        self.gpr[1]
    }

    pub fn edx(&self) -> u32 {
        self.gpr[2]
    }

    pub fn ebx(&self) -> u32 {
        self.gpr[3]
    }

    pub fn esp(&self) -> u32 {
        self.gpr[4]
    }

    pub fn ebp(&self) -> u32 {
        self.gpr[5]
    }

    pub fn esi(&self) -> u32 {
        self.gpr[6]
    }

    pub fn edi(&self) -> u32 {
        self.gpr[7]
    }

    pub fn set_eax(&mut self, value: u32) {
        self.gpr[0] = value;
    }

    pub fn set_ecx(&mut self, value: u32) {
        self.gpr[1] = value;
    }

    pub fn set_edx(&mut self, value: u32) {
        self.gpr[2] = value;
    }

    pub fn set_ebx(&mut self, value: u32) {
        self.gpr[3] = value;
    }

    pub fn set_esp(&mut self, value: u32) {
        self.gpr[4] = value;
    }

    pub fn set_ebp(&mut self, value: u32) {
        self.gpr[5] = value;
    }

    pub fn set_esi(&mut self, value: u32) {
        self.gpr[6] = value;
    }

    pub fn set_edi(&mut self, value: u32) {
        self.gpr[7] = value;
    }

    pub fn cs(&self) -> SegmentRegister {
        self.segment(SegmentIndex::Cs)
    }

    pub fn segment(&self, segment: SegmentIndex) -> SegmentRegister {
        self.segments[segment.index()]
    }

    pub fn set_segment(&mut self, segment: SegmentIndex, value: SegmentRegister) {
        self.segments[segment.index()] = value;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlRegisters {
    pub cr0: u32,
    pub cr2: u32,
    pub cr3: u32,
}

impl Default for ControlRegisters {
    fn default() -> Self {
        Self {
            cr0: 0x0000_0010,
            cr2: 0,
            cr3: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cpu386 {
    pub registers: Registers,
    pub control: ControlRegisters,
    pub gdtr: DescriptorTable,
    pub idtr: DescriptorTable,
    pub elapsed_clocks: u64,
    pub halted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleOutcome {
    pub core_clocks: u32,
    pub halted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperandSize {
    Word,
    Dword,
}

impl OperandSize {
    const fn bytes(self) -> u32 {
        match self {
            Self::Word => 2,
            Self::Dword => 4,
        }
    }

    const fn bus_width(self) -> BusWidth {
        match self {
            Self::Word => BusWidth::Word,
            Self::Dword => BusWidth::Dword,
        }
    }

    const fn mask(self) -> u32 {
        match self {
            Self::Word => 0x0000_ffff,
            Self::Dword => 0xffff_ffff,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddressSize {
    Word,
    Dword,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Prefixes {
    operand_size_override: bool,
    address_size_override: bool,
    rep: bool,
    segment_override: Option<SegmentIndex>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ModRm {
    mode: u8,
    reg: u8,
    rm: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MemoryOperand {
    segment: SegmentIndex,
    offset: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RmOperand {
    Register(u8),
    Memory(MemoryOperand),
}

#[derive(Debug, Error)]
enum InternalFault {
    #[error(transparent)]
    Cpu(#[from] CpuError),
    #[error("processor exception {vector}")]
    Exception { vector: u8, error_code: Option<u32> },
}

impl From<BusError> for InternalFault {
    fn from(value: BusError) -> Self {
        CpuError::Bus(value).into()
    }
}

type ExecResult<T> = Result<T, InternalFault>;

impl Cpu386 {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn is_protected_mode(&self) -> bool {
        self.control.cr0 & CR0_PE != 0
    }

    pub fn is_paging_enabled(&self) -> bool {
        self.control.cr0 & CR0_PG != 0
    }

    pub fn linear_eip(&self) -> u32 {
        self.registers.cs().base.wrapping_add(self.registers.eip)
    }

    pub fn read_reg16(&self, reg: Reg16) -> u16 {
        self.read_gpr16(reg.index() as u8)
    }

    pub fn write_reg16(&mut self, reg: Reg16, value: u16) {
        self.write_gpr16(reg.index() as u8, value);
    }

    pub fn cycle<B: CpuBus>(&mut self, bus: &mut B) -> Result<CycleOutcome, CpuError> {
        if self.halted {
            return Ok(CycleOutcome {
                core_clocks: 1,
                halted: true,
            });
        }

        let start_eip = self.registers.eip;
        let start_cs = self.registers.cs().selector;
        let result = self.execute_instruction(bus);
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(InternalFault::Exception { vector, error_code }) => {
                self.registers.eip = start_eip;
                if self.registers.cs().selector != start_cs {
                    self.load_segment_real(SegmentIndex::Cs, start_cs);
                }
                self.deliver_exception(bus, vector, error_code)
                    .map_err(|fault| match fault {
                        InternalFault::Cpu(error) => error,
                        InternalFault::Exception { vector, .. } => CpuError::IdtLimit { vector },
                    })?;
                CycleOutcome {
                    core_clocks: 59,
                    halted: false,
                }
            }
            Err(InternalFault::Cpu(error)) => return Err(error),
        };

        self.elapsed_clocks += u64::from(outcome.core_clocks);
        Ok(outcome)
    }

    fn execute_instruction<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<CycleOutcome> {
        let instruction_eip = self.registers.eip;
        let prefixes = self.read_prefixes(bus)?;
        let opcode = self.fetch_u8(bus)?;
        let operand_size = self.operand_size(prefixes);
        let address_size = self.address_size(prefixes);

        match opcode {
            opcode if opcode < 0x40 && (opcode & 0x07) < 6 => {
                self.execute_alu_block(bus, opcode, prefixes, address_size, operand_size)
            }
            0x06 => {
                self.push(
                    bus,
                    u32::from(self.registers.segment(SegmentIndex::Es).selector),
                    OperandSize::Word,
                )?;
                Ok(clocks(2))
            }
            0x07 => {
                let value = self.pop(bus, OperandSize::Word)? as u16;
                self.load_segment(bus, SegmentIndex::Es, value)?;
                Ok(clocks(7))
            }
            0x0e => {
                self.push(
                    bus,
                    u32::from(self.registers.segment(SegmentIndex::Cs).selector),
                    OperandSize::Word,
                )?;
                Ok(clocks(2))
            }
            0x16 => {
                self.push(
                    bus,
                    u32::from(self.registers.segment(SegmentIndex::Ss).selector),
                    OperandSize::Word,
                )?;
                Ok(clocks(2))
            }
            0x17 => {
                let value = self.pop(bus, OperandSize::Word)? as u16;
                self.load_segment(bus, SegmentIndex::Ss, value)?;
                Ok(clocks(7))
            }
            0x1e => {
                self.push(
                    bus,
                    u32::from(self.registers.segment(SegmentIndex::Ds).selector),
                    OperandSize::Word,
                )?;
                Ok(clocks(2))
            }
            0x1f => {
                let value = self.pop(bus, OperandSize::Word)? as u16;
                self.load_segment(bus, SegmentIndex::Ds, value)?;
                Ok(clocks(7))
            }
            0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 | 0x67 | 0xf3 => {
                Err(CpuError::UnsupportedOpcode {
                    opcode,
                    cs: self.registers.cs().selector,
                    eip: instruction_eip,
                }
                .into())
            }
            0x50..=0x57 => {
                let index = opcode - 0x50;
                let value = self.read_gpr_sized(index, operand_size);
                self.push(bus, value, operand_size)?;
                Ok(clocks(2))
            }
            0x58..=0x5f => {
                let index = opcode - 0x58;
                let value = self.pop(bus, operand_size)?;
                self.write_gpr_sized(index, operand_size, value);
                Ok(clocks(4))
            }
            0x68 => {
                let value = self.fetch_immediate(bus, operand_size)?;
                self.push(bus, value, operand_size)?;
                Ok(clocks(2))
            }
            0x70..=0x7f => {
                let rel = self.fetch_i8(bus)? as i32;
                if self.condition(opcode & 0x0f) {
                    self.relative_jump(rel, operand_size);
                }
                Ok(clocks(3))
            }
            0x80 => {
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let imm = u32::from(self.fetch_u8(bus)?);
                let a = u32::from(self.read_operand_u8(bus, operand)?);
                let result = self.alu(modrm.reg, a, imm, BusWidth::Byte) as u8;
                if modrm.reg != 7 {
                    self.write_operand_u8(bus, operand, result)?;
                }
                Ok(clocks(2))
            }
            0x81 => {
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let imm = self.fetch_immediate(bus, operand_size)?;
                let a = self.read_operand_sized(bus, operand, operand_size)?;
                let result = self.alu(modrm.reg, a, imm, operand_size.bus_width());
                if modrm.reg != 7 {
                    self.write_operand_sized(bus, operand, operand_size, result)?;
                }
                Ok(clocks(2))
            }
            0x83 => {
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let imm = sign_extend_u8(self.fetch_u8(bus)?);
                let a = self.read_operand_sized(bus, operand, operand_size)?;
                let result = self.alu(modrm.reg, a, imm, operand_size.bus_width());
                if modrm.reg != 7 {
                    self.write_operand_sized(bus, operand, operand_size, result)?;
                }
                Ok(clocks(2))
            }
            0x84 => {
                let modrm = self.fetch_modrm(bus)?;
                let value = self.read_rm_u8(bus, prefixes, address_size, modrm)?;
                let reg = self.read_gpr8(modrm.reg);
                self.alu(4, u32::from(value), u32::from(reg), BusWidth::Byte);
                Ok(clocks(2))
            }
            0x85 => {
                let modrm = self.fetch_modrm(bus)?;
                let value = self.read_rm_sized(bus, prefixes, address_size, operand_size, modrm)?;
                let reg = self.read_gpr_sized(modrm.reg, operand_size);
                self.alu(4, value, reg, operand_size.bus_width());
                Ok(clocks(2))
            }
            0x88 => {
                let modrm = self.fetch_modrm(bus)?;
                let value = self.read_gpr8(modrm.reg);
                self.write_rm_u8(bus, prefixes, address_size, modrm, value)?;
                Ok(clocks(2))
            }
            0x89 => {
                let modrm = self.fetch_modrm(bus)?;
                let value = self.read_gpr_sized(modrm.reg, operand_size);
                self.write_rm_sized(bus, prefixes, address_size, operand_size, modrm, value)?;
                Ok(clocks(2))
            }
            0x8a => {
                let modrm = self.fetch_modrm(bus)?;
                let value = self.read_rm_u8(bus, prefixes, address_size, modrm)?;
                self.write_gpr8(modrm.reg, value);
                Ok(clocks(2))
            }
            0x8b => {
                let modrm = self.fetch_modrm(bus)?;
                let value = self.read_rm_sized(bus, prefixes, address_size, operand_size, modrm)?;
                self.write_gpr_sized(modrm.reg, operand_size, value);
                Ok(clocks(2))
            }
            0x8c => {
                let modrm = self.fetch_modrm(bus)?;
                let value = u32::from(self.segment_from_reg_field(modrm.reg).selector);
                self.write_rm_sized(bus, prefixes, address_size, OperandSize::Word, modrm, value)?;
                Ok(clocks(2))
            }
            0x8e => {
                let modrm = self.fetch_modrm(bus)?;
                let value =
                    self.read_rm_sized(bus, prefixes, address_size, OperandSize::Word, modrm)?;
                let segment = match modrm.reg {
                    0 => SegmentIndex::Es,
                    2 => SegmentIndex::Ss,
                    3 => SegmentIndex::Ds,
                    4 => SegmentIndex::Fs,
                    5 => SegmentIndex::Gs,
                    _ => {
                        return Err(CpuError::GeneralProtection {
                            selector: value as u16,
                        }
                        .into());
                    }
                };
                self.load_segment(bus, segment, value as u16)?;
                Ok(clocks(7))
            }
            0x90 => Ok(clocks(3)),
            0xa1 => {
                let offset = self.fetch_moffs(bus, address_size)?;
                let value = self.read_memory_sized(
                    bus,
                    prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                    offset,
                    operand_size,
                    BusAccessKind::DataRead,
                )?;
                self.write_gpr_sized(0, operand_size, value);
                Ok(clocks(4))
            }
            0xa3 => {
                let offset = self.fetch_moffs(bus, address_size)?;
                let value = self.read_gpr_sized(0, operand_size);
                self.write_memory_sized(
                    bus,
                    prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                    offset,
                    operand_size,
                    value,
                    BusAccessKind::DataWrite,
                )?;
                Ok(clocks(4))
            }
            0xa8 => {
                let imm = self.fetch_u8(bus)?;
                let al = self.read_gpr8(0);
                self.alu(4, u32::from(al), u32::from(imm), BusWidth::Byte);
                Ok(clocks(2))
            }
            0xa9 => {
                let imm = self.fetch_immediate(bus, operand_size)?;
                let acc = self.read_gpr_sized(0, operand_size);
                self.alu(4, acc, imm, operand_size.bus_width());
                Ok(clocks(2))
            }
            0xaa => {
                self.stos_u8(bus, address_size)?;
                Ok(clocks(4))
            }
            0xac => {
                self.lods(bus, prefixes, address_size, BusWidth::Byte)?;
                Ok(clocks(5))
            }
            0xab => {
                self.stos(bus, address_size, operand_size)?;
                Ok(clocks(4))
            }
            0xb0..=0xb7 => {
                let value = self.fetch_u8(bus)?;
                self.write_gpr8(opcode - 0xb0, value);
                Ok(clocks(2))
            }
            0xb8..=0xbf => {
                let value = self.fetch_immediate(bus, operand_size)?;
                self.write_gpr_sized(opcode - 0xb8, operand_size, value);
                Ok(clocks(2))
            }
            0xc3 => {
                let target = self.pop(bus, operand_size)?;
                self.registers.eip = target & operand_size.mask();
                Ok(clocks(10))
            }
            0xc6 => {
                let modrm = self.fetch_modrm(bus)?;
                if modrm.reg != 0 {
                    return Err(CpuError::UnsupportedGroupOpcode {
                        opcode,
                        extension: modrm.reg,
                    }
                    .into());
                }
                let value = self.fetch_u8(bus)?;
                self.write_rm_u8(bus, prefixes, address_size, modrm, value)?;
                Ok(clocks(2))
            }
            0xc7 => {
                let modrm = self.fetch_modrm(bus)?;
                if modrm.reg != 0 {
                    return Err(CpuError::UnsupportedGroupOpcode {
                        opcode,
                        extension: modrm.reg,
                    }
                    .into());
                }
                let value = self.fetch_immediate(bus, operand_size)?;
                self.write_rm_sized(bus, prefixes, address_size, operand_size, modrm, value)?;
                Ok(clocks(2))
            }
            0xcd => {
                let vector = self.fetch_u8(bus)?;
                self.software_interrupt(bus, vector)?;
                Ok(clocks(37))
            }
            0xcf => {
                self.iret(bus, operand_size)?;
                Ok(clocks(22))
            }
            0xe2 => {
                let rel = self.fetch_i8(bus)? as i32;
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
                Ok(clocks(11))
            }
            0xe4 => {
                let port = u16::from(self.fetch_u8(bus)?);
                let value = bus.read_io(port, BusWidth::Byte)? as u8;
                self.write_gpr8(0, value);
                Ok(clocks(12))
            }
            0xe6 => {
                let port = u16::from(self.fetch_u8(bus)?);
                bus.write_io(port, BusWidth::Byte, u32::from(self.read_gpr8(0)))?;
                Ok(clocks(10))
            }
            0xec => {
                let port = self.read_gpr16(2);
                let value = bus.read_io(port, BusWidth::Byte)? as u8;
                self.write_gpr8(0, value);
                Ok(clocks(12))
            }
            0xee => {
                let port = self.read_gpr16(2);
                bus.write_io(port, BusWidth::Byte, u32::from(self.read_gpr8(0)))?;
                Ok(clocks(10))
            }
            0xe8 => {
                let rel = self.fetch_relative(bus, operand_size)?;
                self.push(bus, self.registers.eip, operand_size)?;
                self.relative_jump(rel, operand_size);
                Ok(clocks(7))
            }
            0xe9 => {
                let rel = self.fetch_relative(bus, operand_size)?;
                self.relative_jump(rel, operand_size);
                Ok(clocks(7))
            }
            0xeb => {
                let rel = self.fetch_i8(bus)? as i32;
                self.relative_jump(rel, operand_size);
                Ok(clocks(7))
            }
            0xea => {
                let offset = match operand_size {
                    OperandSize::Word => u32::from(self.fetch_u16(bus)?),
                    OperandSize::Dword => self.fetch_u32(bus)?,
                };
                let selector = self.fetch_u16(bus)?;
                self.far_jump(bus, selector, offset, operand_size)?;
                Ok(clocks(17))
            }
            0xf4 => {
                self.halted = true;
                Ok(CycleOutcome {
                    core_clocks: 5,
                    halted: true,
                })
            }
            0xf6 => {
                let modrm = self.fetch_modrm(bus)?;
                match modrm.reg {
                    0 => {
                        let value = self.read_rm_u8(bus, prefixes, address_size, modrm)?;
                        let imm = self.fetch_u8(bus)?;
                        self.alu(4, u32::from(value), u32::from(imm), BusWidth::Byte);
                        Ok(clocks(2))
                    }
                    _ => Err(CpuError::UnsupportedGroupOpcode {
                        opcode: 0xf6,
                        extension: modrm.reg,
                    }
                    .into()),
                }
            }
            0xf7 => {
                let modrm = self.fetch_modrm(bus)?;
                match modrm.reg {
                    0 => {
                        let value =
                            self.read_rm_sized(bus, prefixes, address_size, operand_size, modrm)?;
                        let imm = self.fetch_immediate(bus, operand_size)?;
                        self.alu(4, value, imm, operand_size.bus_width());
                        Ok(clocks(2))
                    }
                    _ => Err(CpuError::UnsupportedGroupOpcode {
                        opcode: 0xf7,
                        extension: modrm.reg,
                    }
                    .into()),
                }
            }
            0xf8 => {
                self.set_flag(FLAG_CF, false);
                Ok(clocks(2))
            }
            0xfa => {
                self.set_flag(FLAG_IF, false);
                Ok(clocks(3))
            }
            0xfc => {
                self.set_flag(FLAG_DF, false);
                Ok(clocks(2))
            }
            0xfd => {
                self.set_flag(FLAG_DF, true);
                Ok(clocks(2))
            }
            0xfe => {
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                match modrm.reg {
                    0 | 1 => {
                        let value = u32::from(self.read_operand_u8(bus, operand)?);
                        let result = self.inc_dec(value, modrm.reg == 1, BusWidth::Byte) as u8;
                        self.write_operand_u8(bus, operand, result)?;
                        Ok(clocks(2))
                    }
                    extension => Err(CpuError::UnsupportedGroupOpcode {
                        opcode: 0xfe,
                        extension,
                    }
                    .into()),
                }
            }
            0x40..=0x4f => {
                let index = opcode & 0x07;
                let is_dec = opcode >= 0x48;
                let value = self.read_gpr_sized(index, operand_size);
                let result = self.inc_dec(value, is_dec, operand_size.bus_width());
                self.write_gpr_sized(index, operand_size, result);
                Ok(clocks(2))
            }
            0x0f => self.execute_two_byte(bus, prefixes, address_size, operand_size),
            _ => Err(CpuError::UnsupportedOpcode {
                opcode,
                cs: self.registers.cs().selector,
                eip: instruction_eip,
            }
            .into()),
        }
    }

    fn execute_two_byte<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        operand_size: OperandSize,
    ) -> ExecResult<CycleOutcome> {
        let opcode = self.fetch_u8(bus)?;
        match opcode {
            0x01 => {
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let memory = match operand {
                    RmOperand::Memory(memory) => memory,
                    RmOperand::Register(_) => {
                        return Err(CpuError::UnsupportedGroupOpcode {
                            opcode,
                            extension: modrm.reg,
                        }
                        .into());
                    }
                };
                let limit = self.read_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    OperandSize::Word,
                    BusAccessKind::DataRead,
                )? as u16;
                let base_low = self.read_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset + 2,
                    OperandSize::Dword,
                    BusAccessKind::DataRead,
                )?;
                match modrm.reg {
                    2 => {
                        self.gdtr = DescriptorTable {
                            base: base_low,
                            limit,
                        }
                    }
                    3 => {
                        self.idtr = DescriptorTable {
                            base: base_low,
                            limit,
                        }
                    }
                    _ => {
                        return Err(CpuError::UnsupportedGroupOpcode {
                            opcode,
                            extension: modrm.reg,
                        }
                        .into());
                    }
                }
                Ok(clocks(11))
            }
            0x20 => {
                let modrm = self.fetch_modrm(bus)?;
                if modrm.mode != 3 {
                    return Err(CpuError::UnsupportedTwoByteOpcode {
                        opcode,
                        cs: self.registers.cs().selector,
                        eip: self.registers.eip,
                    }
                    .into());
                }
                let value = match modrm.reg {
                    0 => self.control.cr0,
                    2 => self.control.cr2,
                    3 => self.control.cr3,
                    _ => 0,
                };
                self.write_gpr32(modrm.rm, value);
                Ok(clocks(6))
            }
            0x22 => {
                let modrm = self.fetch_modrm(bus)?;
                if modrm.mode != 3 {
                    return Err(CpuError::UnsupportedTwoByteOpcode {
                        opcode,
                        cs: self.registers.cs().selector,
                        eip: self.registers.eip,
                    }
                    .into());
                }
                let value = self.read_gpr32(modrm.rm);
                match modrm.reg {
                    0 => self.control.cr0 = value | 0x10,
                    2 => self.control.cr2 = value,
                    3 => self.control.cr3 = value & 0xffff_f000,
                    _ => {}
                }
                Ok(clocks(6))
            }
            0x80..=0x8f => {
                let rel = self.fetch_relative(bus, operand_size)?;
                if self.condition(opcode & 0x0f) {
                    self.relative_jump(rel, operand_size);
                }
                Ok(clocks(3))
            }
            _ => Err(CpuError::UnsupportedTwoByteOpcode {
                opcode,
                cs: self.registers.cs().selector,
                eip: self.registers.eip,
            }
            .into()),
        }
    }

    fn execute_alu_block<B: CpuBus>(
        &mut self,
        bus: &mut B,
        opcode: u8,
        prefixes: Prefixes,
        address_size: AddressSize,
        operand_size: OperandSize,
    ) -> ExecResult<CycleOutcome> {
        let op = (opcode >> 3) & 0x07;
        let form = opcode & 0x07;
        let write_back = op != 7; // CMP computes flags only

        match form {
            0 => {
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let a = u32::from(self.read_operand_u8(bus, operand)?);
                let b = u32::from(self.read_gpr8(modrm.reg));
                let result = self.alu(op, a, b, BusWidth::Byte) as u8;
                if write_back {
                    self.write_operand_u8(bus, operand, result)?;
                }
            }
            1 => {
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let a = self.read_operand_sized(bus, operand, operand_size)?;
                let b = self.read_gpr_sized(modrm.reg, operand_size);
                let result = self.alu(op, a, b, operand_size.bus_width());
                if write_back {
                    self.write_operand_sized(bus, operand, operand_size, result)?;
                }
            }
            2 => {
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let a = u32::from(self.read_gpr8(modrm.reg));
                let b = u32::from(self.read_operand_u8(bus, operand)?);
                let result = self.alu(op, a, b, BusWidth::Byte) as u8;
                if write_back {
                    self.write_gpr8(modrm.reg, result);
                }
            }
            3 => {
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let a = self.read_gpr_sized(modrm.reg, operand_size);
                let b = self.read_operand_sized(bus, operand, operand_size)?;
                let result = self.alu(op, a, b, operand_size.bus_width());
                if write_back {
                    self.write_gpr_sized(modrm.reg, operand_size, result);
                }
            }
            4 => {
                let imm = u32::from(self.fetch_u8(bus)?);
                let a = u32::from(self.read_gpr8(0));
                let result = self.alu(op, a, imm, BusWidth::Byte) as u8;
                if write_back {
                    self.write_gpr8(0, result);
                }
            }
            5 => {
                let imm = self.fetch_immediate(bus, operand_size)?;
                let a = self.read_gpr_sized(0, operand_size);
                let result = self.alu(op, a, imm, operand_size.bus_width());
                if write_back {
                    self.write_gpr_sized(0, operand_size, result);
                }
            }
            _ => unreachable!("alu form {form}"),
        }

        Ok(clocks(2))
    }

    fn read_prefixes<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<Prefixes> {
        let mut prefixes = Prefixes::default();
        loop {
            let eip = self.registers.eip;
            let byte = self.fetch_u8(bus)?;
            match byte {
                0x26 => prefixes.segment_override = Some(SegmentIndex::Es),
                0x2e => prefixes.segment_override = Some(SegmentIndex::Cs),
                0x36 => prefixes.segment_override = Some(SegmentIndex::Ss),
                0x3e => prefixes.segment_override = Some(SegmentIndex::Ds),
                0x64 => prefixes.segment_override = Some(SegmentIndex::Fs),
                0x65 => prefixes.segment_override = Some(SegmentIndex::Gs),
                0x66 => prefixes.operand_size_override = !prefixes.operand_size_override,
                0x67 => prefixes.address_size_override = !prefixes.address_size_override,
                0xf3 => prefixes.rep = true,
                _ => {
                    self.registers.eip = eip;
                    return Ok(prefixes);
                }
            }
        }
    }

    fn operand_size(&self, prefixes: Prefixes) -> OperandSize {
        let default_32 = self.registers.cs().default_size_32;
        if default_32 ^ prefixes.operand_size_override {
            OperandSize::Dword
        } else {
            OperandSize::Word
        }
    }

    fn address_size(&self, prefixes: Prefixes) -> AddressSize {
        let default_32 = self.registers.cs().default_size_32;
        if default_32 ^ prefixes.address_size_override {
            AddressSize::Dword
        } else {
            AddressSize::Word
        }
    }

    fn fetch_u8<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<u8> {
        let offset = self.registers.eip;
        let value = self.read_memory_u8(
            bus,
            SegmentIndex::Cs,
            offset,
            BusAccessKind::InstructionPrefetch,
        )?;
        self.registers.eip = self.registers.eip.wrapping_add(1);
        Ok(value)
    }

    fn fetch_i8<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<i8> {
        Ok(self.fetch_u8(bus)? as i8)
    }

    fn fetch_u16<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<u16> {
        let low = self.fetch_u8(bus)?;
        let high = self.fetch_u8(bus)?;
        Ok(u16::from_le_bytes([low, high]))
    }

    fn fetch_u32<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<u32> {
        let b0 = self.fetch_u8(bus)?;
        let b1 = self.fetch_u8(bus)?;
        let b2 = self.fetch_u8(bus)?;
        let b3 = self.fetch_u8(bus)?;
        Ok(u32::from_le_bytes([b0, b1, b2, b3]))
    }

    fn fetch_immediate<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand_size: OperandSize,
    ) -> ExecResult<u32> {
        match operand_size {
            OperandSize::Word => Ok(u32::from(self.fetch_u16(bus)?)),
            OperandSize::Dword => self.fetch_u32(bus),
        }
    }

    fn fetch_relative<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand_size: OperandSize,
    ) -> ExecResult<i32> {
        match operand_size {
            OperandSize::Word => Ok(i32::from(self.fetch_u16(bus)? as i16)),
            OperandSize::Dword => Ok(self.fetch_u32(bus)? as i32),
        }
    }

    fn fetch_moffs<B: CpuBus>(
        &mut self,
        bus: &mut B,
        address_size: AddressSize,
    ) -> ExecResult<u32> {
        match address_size {
            AddressSize::Word => Ok(u32::from(self.fetch_u16(bus)?)),
            AddressSize::Dword => self.fetch_u32(bus),
        }
    }

    fn fetch_modrm<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<ModRm> {
        let value = self.fetch_u8(bus)?;
        Ok(ModRm {
            mode: value >> 6,
            reg: (value >> 3) & 0x07,
            rm: value & 0x07,
        })
    }

    fn decode_rm_operand<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        modrm: ModRm,
    ) -> ExecResult<RmOperand> {
        if modrm.mode == 3 {
            return Ok(RmOperand::Register(modrm.rm));
        }

        let (default_segment, offset) = match address_size {
            AddressSize::Word => self.decode_16bit_address(bus, modrm)?,
            AddressSize::Dword => self.decode_32bit_address(bus, modrm)?,
        };

        Ok(RmOperand::Memory(MemoryOperand {
            segment: prefixes.segment_override.unwrap_or(default_segment),
            offset,
        }))
    }

    fn decode_16bit_address<B: CpuBus>(
        &mut self,
        bus: &mut B,
        modrm: ModRm,
    ) -> ExecResult<(SegmentIndex, u32)> {
        let bx = u32::from(self.read_gpr16(3));
        let bp = u32::from(self.read_gpr16(5));
        let si = u32::from(self.read_gpr16(6));
        let di = u32::from(self.read_gpr16(7));
        let mut uses_bp = false;

        let base = match modrm.rm {
            0 => bx.wrapping_add(si),
            1 => bx.wrapping_add(di),
            2 => {
                uses_bp = true;
                bp.wrapping_add(si)
            }
            3 => {
                uses_bp = true;
                bp.wrapping_add(di)
            }
            4 => si,
            5 => di,
            6 if modrm.mode == 0 => u32::from(self.fetch_u16(bus)?),
            6 => {
                uses_bp = true;
                bp
            }
            _ => bx,
        };

        let displacement = match modrm.mode {
            0 => 0,
            1 => self.fetch_i8(bus)? as i32,
            2 => i32::from(self.fetch_u16(bus)? as i16),
            _ => 0,
        };

        let offset = ((base as i32).wrapping_add(displacement) as u16) as u32;
        let segment = if uses_bp {
            SegmentIndex::Ss
        } else {
            SegmentIndex::Ds
        };
        Ok((segment, offset))
    }

    fn decode_32bit_address<B: CpuBus>(
        &mut self,
        bus: &mut B,
        modrm: ModRm,
    ) -> ExecResult<(SegmentIndex, u32)> {
        let mut base_reg = None;
        let mut index = 0u32;
        let mut scale = 1u32;

        if modrm.rm == 4 {
            let sib = self.fetch_u8(bus)?;
            scale = 1 << (sib >> 6);
            let index_reg = (sib >> 3) & 0x07;
            if index_reg != 4 {
                index = self.read_gpr32(index_reg);
            }
            let base = sib & 0x07;
            if !(modrm.mode == 0 && base == 5) {
                base_reg = Some(base);
            }
        } else if !(modrm.mode == 0 && modrm.rm == 5) {
            base_reg = Some(modrm.rm);
        }

        let base = base_reg.map_or(0, |reg| self.read_gpr32(reg));
        let displacement = match modrm.mode {
            0 if base_reg.is_none() => self.fetch_u32(bus)? as i32,
            0 => 0,
            1 => self.fetch_i8(bus)? as i32,
            2 => self.fetch_u32(bus)? as i32,
            _ => 0,
        };
        let offset = base
            .wrapping_add(index.wrapping_mul(scale))
            .wrapping_add(displacement as u32);
        let segment = if matches!(base_reg, Some(4 | 5)) {
            SegmentIndex::Ss
        } else {
            SegmentIndex::Ds
        };
        Ok((segment, offset))
    }

    fn read_rm_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        modrm: ModRm,
    ) -> ExecResult<u8> {
        match self.decode_rm_operand(bus, prefixes, address_size, modrm)? {
            RmOperand::Register(index) => Ok(self.read_gpr8(index)),
            RmOperand::Memory(memory) => {
                self.read_memory_u8(bus, memory.segment, memory.offset, BusAccessKind::DataRead)
            }
        }
    }

    fn write_rm_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        modrm: ModRm,
        value: u8,
    ) -> ExecResult<()> {
        match self.decode_rm_operand(bus, prefixes, address_size, modrm)? {
            RmOperand::Register(index) => self.write_gpr8(index, value),
            RmOperand::Memory(memory) => self.write_memory_u8(
                bus,
                memory.segment,
                memory.offset,
                value,
                BusAccessKind::DataWrite,
            )?,
        }
        Ok(())
    }

    fn read_rm_sized<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        operand_size: OperandSize,
        modrm: ModRm,
    ) -> ExecResult<u32> {
        match self.decode_rm_operand(bus, prefixes, address_size, modrm)? {
            RmOperand::Register(index) => Ok(self.read_gpr_sized(index, operand_size)),
            RmOperand::Memory(memory) => self.read_memory_sized(
                bus,
                memory.segment,
                memory.offset,
                operand_size,
                BusAccessKind::DataRead,
            ),
        }
    }

    fn write_rm_sized<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        operand_size: OperandSize,
        modrm: ModRm,
        value: u32,
    ) -> ExecResult<()> {
        match self.decode_rm_operand(bus, prefixes, address_size, modrm)? {
            RmOperand::Register(index) => self.write_gpr_sized(index, operand_size, value),
            RmOperand::Memory(memory) => self.write_memory_sized(
                bus,
                memory.segment,
                memory.offset,
                operand_size,
                value,
                BusAccessKind::DataWrite,
            )?,
        }
        Ok(())
    }

    fn read_operand_u8<B: CpuBus>(&mut self, bus: &mut B, operand: RmOperand) -> ExecResult<u8> {
        match operand {
            RmOperand::Register(index) => Ok(self.read_gpr8(index)),
            RmOperand::Memory(memory) => {
                self.read_memory_u8(bus, memory.segment, memory.offset, BusAccessKind::DataRead)
            }
        }
    }

    fn write_operand_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand: RmOperand,
        value: u8,
    ) -> ExecResult<()> {
        match operand {
            RmOperand::Register(index) => {
                self.write_gpr8(index, value);
                Ok(())
            }
            RmOperand::Memory(memory) => self.write_memory_u8(
                bus,
                memory.segment,
                memory.offset,
                value,
                BusAccessKind::DataWrite,
            ),
        }
    }

    fn read_operand_sized<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand: RmOperand,
        size: OperandSize,
    ) -> ExecResult<u32> {
        match operand {
            RmOperand::Register(index) => Ok(self.read_gpr_sized(index, size)),
            RmOperand::Memory(memory) => self.read_memory_sized(
                bus,
                memory.segment,
                memory.offset,
                size,
                BusAccessKind::DataRead,
            ),
        }
    }

    fn write_operand_sized<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand: RmOperand,
        size: OperandSize,
        value: u32,
    ) -> ExecResult<()> {
        match operand {
            RmOperand::Register(index) => {
                self.write_gpr_sized(index, size, value);
                Ok(())
            }
            RmOperand::Memory(memory) => self.write_memory_sized(
                bus,
                memory.segment,
                memory.offset,
                size,
                value,
                BusAccessKind::DataWrite,
            ),
        }
    }

    fn read_memory_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        kind: BusAccessKind,
    ) -> ExecResult<u8> {
        let physical = self.translate_segmented(bus, segment, offset, 1, kind, false)?;
        Ok(bus.read_memory(physical, BusWidth::Byte, kind)? as u8)
    }

    fn write_memory_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        value: u8,
        kind: BusAccessKind,
    ) -> ExecResult<()> {
        let physical = self.translate_segmented(bus, segment, offset, 1, kind, true)?;
        bus.write_memory(physical, BusWidth::Byte, u32::from(value), kind)?;
        Ok(())
    }

    fn read_memory_sized<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        size: OperandSize,
        kind: BusAccessKind,
    ) -> ExecResult<u32> {
        let physical = self.translate_segmented(bus, segment, offset, size.bytes(), kind, false)?;
        Ok(bus.read_memory(physical, size.bus_width(), kind)?)
    }

    fn write_memory_sized<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        size: OperandSize,
        value: u32,
        kind: BusAccessKind,
    ) -> ExecResult<()> {
        let physical = self.translate_segmented(bus, segment, offset, size.bytes(), kind, true)?;
        bus.write_memory(physical, size.bus_width(), value, kind)?;
        Ok(())
    }

    fn translate_segmented<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        width: u32,
        kind: BusAccessKind,
        write: bool,
    ) -> ExecResult<u32> {
        let descriptor = self.registers.segment(segment);
        if offset > descriptor.limit
            || offset.saturating_add(width.saturating_sub(1)) > descriptor.limit
        {
            return Err(CpuError::SegmentLimit {
                segment,
                offset,
                width,
            }
            .into());
        }

        let linear = descriptor.base.wrapping_add(offset);
        self.translate_linear(
            bus,
            linear,
            write,
            matches!(kind, BusAccessKind::InstructionPrefetch),
        )
    }

    fn translate_linear<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        write: bool,
        instruction_fetch: bool,
    ) -> ExecResult<u32> {
        if !self.is_paging_enabled() {
            return Ok(linear);
        }

        let directory = self.control.cr3 & 0xffff_f000;
        let directory_address = directory + (((linear >> 22) & 0x03ff) * 4);
        let mut pde = bus.read_memory(
            directory_address,
            BusWidth::Dword,
            BusAccessKind::PageWalkRead,
        )?;
        if pde & 1 == 0 {
            self.control.cr2 = linear;
            return Err(InternalFault::Exception {
                vector: 14,
                error_code: Some(page_fault_code(false, write, instruction_fetch)),
            });
        }
        if pde & 0x20 == 0 {
            pde |= 0x20;
            bus.write_memory(
                directory_address,
                BusWidth::Dword,
                pde,
                BusAccessKind::PageWalkWrite,
            )?;
        }

        let table_address = (pde & 0xffff_f000) + (((linear >> 12) & 0x03ff) * 4);
        let mut pte =
            bus.read_memory(table_address, BusWidth::Dword, BusAccessKind::PageWalkRead)?;
        if pte & 1 == 0 {
            self.control.cr2 = linear;
            return Err(InternalFault::Exception {
                vector: 14,
                error_code: Some(page_fault_code(false, write, instruction_fetch)),
            });
        }

        let dirty = if write { 0x40 } else { 0 };
        let accessed_dirty = 0x20 | dirty;
        if pte & accessed_dirty != accessed_dirty {
            pte |= accessed_dirty;
            bus.write_memory(
                table_address,
                BusWidth::Dword,
                pte,
                BusAccessKind::PageWalkWrite,
            )?;
        }

        Ok((pte & 0xffff_f000) | (linear & 0x0000_0fff))
    }

    fn push<B: CpuBus>(
        &mut self,
        bus: &mut B,
        value: u32,
        operand_size: OperandSize,
    ) -> ExecResult<()> {
        match operand_size {
            OperandSize::Word => {
                let sp = self.read_gpr16(4).wrapping_sub(2);
                self.write_gpr16(4, sp);
                self.write_memory_sized(
                    bus,
                    SegmentIndex::Ss,
                    u32::from(sp),
                    OperandSize::Word,
                    value,
                    BusAccessKind::DataWrite,
                )
            }
            OperandSize::Dword => {
                let esp = self.registers.esp().wrapping_sub(4);
                self.registers.set_esp(esp);
                self.write_memory_sized(
                    bus,
                    SegmentIndex::Ss,
                    esp,
                    OperandSize::Dword,
                    value,
                    BusAccessKind::DataWrite,
                )
            }
        }
    }

    fn pop<B: CpuBus>(&mut self, bus: &mut B, operand_size: OperandSize) -> ExecResult<u32> {
        match operand_size {
            OperandSize::Word => {
                let sp = self.read_gpr16(4);
                let value = self.read_memory_sized(
                    bus,
                    SegmentIndex::Ss,
                    u32::from(sp),
                    OperandSize::Word,
                    BusAccessKind::DataRead,
                )?;
                self.write_gpr16(4, sp.wrapping_add(2));
                Ok(value)
            }
            OperandSize::Dword => {
                let esp = self.registers.esp();
                let value = self.read_memory_sized(
                    bus,
                    SegmentIndex::Ss,
                    esp,
                    OperandSize::Dword,
                    BusAccessKind::DataRead,
                )?;
                self.registers.set_esp(esp.wrapping_add(4));
                Ok(value)
            }
        }
    }

    fn lods<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        width: BusWidth,
    ) -> ExecResult<()> {
        let segment = prefixes.segment_override.unwrap_or(SegmentIndex::Ds);
        let increment = width.bytes();
        let offset = match address_size {
            AddressSize::Word => u32::from(self.read_gpr16(6)),
            AddressSize::Dword => self.registers.esi(),
        };
        let physical = self.translate_segmented(
            bus,
            segment,
            offset,
            increment,
            BusAccessKind::DataRead,
            false,
        )?;
        let value = bus.read_memory(physical, width, BusAccessKind::DataRead)?;
        match width {
            BusWidth::Byte => self.write_gpr8(0, value as u8),
            BusWidth::Word => self.write_gpr16(0, value as u16),
            BusWidth::Dword => self.write_gpr32(0, value),
        }
        self.adjust_index_register(6, address_size, increment);
        Ok(())
    }

    fn stos<B: CpuBus>(
        &mut self,
        bus: &mut B,
        address_size: AddressSize,
        operand_size: OperandSize,
    ) -> ExecResult<()> {
        let offset = match address_size {
            AddressSize::Word => u32::from(self.read_gpr16(7)),
            AddressSize::Dword => self.registers.edi(),
        };
        let value = self.read_gpr_sized(0, operand_size);
        self.write_memory_sized(
            bus,
            SegmentIndex::Es,
            offset,
            operand_size,
            value,
            BusAccessKind::DataWrite,
        )?;
        self.adjust_index_register(7, address_size, operand_size.bytes());
        Ok(())
    }

    fn stos_u8<B: CpuBus>(&mut self, bus: &mut B, address_size: AddressSize) -> ExecResult<()> {
        let offset = match address_size {
            AddressSize::Word => u32::from(self.read_gpr16(7)),
            AddressSize::Dword => self.registers.edi(),
        };
        self.write_memory_u8(
            bus,
            SegmentIndex::Es,
            offset,
            self.read_gpr8(0),
            BusAccessKind::DataWrite,
        )?;
        self.adjust_index_register(7, address_size, 1);
        Ok(())
    }

    fn adjust_index_register(&mut self, index: u8, address_size: AddressSize, amount: u32) {
        let delta = if self.flag(FLAG_DF) {
            0u32.wrapping_sub(amount)
        } else {
            amount
        };

        match address_size {
            AddressSize::Word => {
                let value = self.read_gpr16(index).wrapping_add(delta as u16);
                self.write_gpr16(index, value);
            }
            AddressSize::Dword => {
                let value = self.read_gpr32(index).wrapping_add(delta);
                self.write_gpr32(index, value);
            }
        }
    }

    fn software_interrupt<B: CpuBus>(&mut self, bus: &mut B, vector: u8) -> ExecResult<()> {
        bus.interrupt_acknowledge(vector, self.read_gpr16(0))?;
        if self.is_protected_mode() {
            self.deliver_exception(bus, vector, None)
        } else {
            self.push(bus, self.registers.eflags as u16 as u32, OperandSize::Word)?;
            self.push(
                bus,
                u32::from(self.registers.cs().selector),
                OperandSize::Word,
            )?;
            self.push(bus, self.registers.eip as u16 as u32, OperandSize::Word)?;
            self.set_flag(FLAG_IF | FLAG_TF, false);
            let vector_address = u32::from(vector) * 4;
            let ip =
                bus.read_memory(vector_address, BusWidth::Word, BusAccessKind::DataRead)? as u16;
            let cs = bus.read_memory(vector_address + 2, BusWidth::Word, BusAccessKind::DataRead)?
                as u16;
            self.load_segment_real(SegmentIndex::Cs, cs);
            self.registers.eip = u32::from(ip);
            Ok(())
        }
    }

    fn deliver_exception<B: CpuBus>(
        &mut self,
        bus: &mut B,
        vector: u8,
        error_code: Option<u32>,
    ) -> ExecResult<()> {
        if !self.is_protected_mode() {
            return self.software_interrupt(bus, vector);
        }

        let gate_address = self.idtr.base + u32::from(vector) * 8;
        if u32::from(self.idtr.limit) < u32::from(vector) * 8 + 7 {
            return Err(CpuError::IdtLimit { vector }.into());
        }

        let gate_low = self.read_system_linear_u32(bus, gate_address)?;
        let gate_high = self.read_system_linear_u32(bus, gate_address + 4)?;
        let selector = ((gate_low >> 16) & 0xffff) as u16;
        let offset = (gate_low & 0x0000_ffff) | (gate_high & 0xffff_0000);

        self.push(bus, self.registers.eflags, OperandSize::Dword)?;
        self.push(
            bus,
            u32::from(self.registers.cs().selector),
            OperandSize::Dword,
        )?;
        self.push(bus, self.registers.eip, OperandSize::Dword)?;
        if let Some(error_code) = error_code {
            self.push(bus, error_code, OperandSize::Dword)?;
        }
        self.set_flag(FLAG_IF | FLAG_TF, false);
        self.load_segment(bus, SegmentIndex::Cs, selector)?;
        self.registers.eip = offset;
        Ok(())
    }

    fn read_system_linear_u32<B: CpuBus>(&mut self, bus: &mut B, linear: u32) -> ExecResult<u32> {
        let physical = self.translate_linear(bus, linear, false, false)?;
        Ok(bus.read_memory(physical, BusWidth::Dword, BusAccessKind::DataRead)?)
    }

    fn iret<B: CpuBus>(&mut self, bus: &mut B, operand_size: OperandSize) -> ExecResult<()> {
        match operand_size {
            OperandSize::Word => {
                let ip = self.pop(bus, OperandSize::Word)?;
                let cs = self.pop(bus, OperandSize::Word)? as u16;
                let flags = self.pop(bus, OperandSize::Word)?;
                self.load_segment(bus, SegmentIndex::Cs, cs)?;
                self.registers.eip = ip & 0xffff;
                self.registers.eflags =
                    (self.registers.eflags & 0xffff_0000) | (flags & 0xffff) | 0x2;
            }
            OperandSize::Dword => {
                let eip = self.pop(bus, OperandSize::Dword)?;
                let cs = self.pop(bus, OperandSize::Dword)? as u16;
                let flags = self.pop(bus, OperandSize::Dword)?;
                self.load_segment(bus, SegmentIndex::Cs, cs)?;
                self.registers.eip = eip;
                self.registers.eflags = flags | 0x2;
            }
        }
        Ok(())
    }

    fn far_jump<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
        offset: u32,
        operand_size: OperandSize,
    ) -> ExecResult<()> {
        self.load_segment(bus, SegmentIndex::Cs, selector)?;
        self.registers.eip = offset & operand_size.mask();
        Ok(())
    }

    fn relative_jump(&mut self, relative: i32, operand_size: OperandSize) {
        self.registers.eip = self.registers.eip.wrapping_add(relative as u32) & operand_size.mask();
    }

    fn load_segment<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        selector: u16,
    ) -> ExecResult<()> {
        if self.is_protected_mode() {
            let register = self.load_protected_segment(bus, selector)?;
            self.registers.set_segment(segment, register);
        } else {
            self.load_segment_real(segment, selector);
        }
        Ok(())
    }

    fn load_segment_real(&mut self, segment: SegmentIndex, selector: u16) {
        self.registers
            .set_segment(segment, SegmentRegister::real(selector));
    }

    fn load_protected_segment<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
    ) -> ExecResult<SegmentRegister> {
        let index = u32::from(selector & !0x7);
        if index == 0 || index + 7 > u32::from(self.gdtr.limit) {
            return Err(CpuError::GeneralProtection { selector }.into());
        }
        let descriptor_address = self.gdtr.base + index;
        let low = bus.read_memory(descriptor_address, BusWidth::Dword, BusAccessKind::DataRead)?;
        let high = bus.read_memory(
            descriptor_address + 4,
            BusWidth::Dword,
            BusAccessKind::DataRead,
        )?;
        let access = ((high >> 8) & 0xff) as u8;
        if access & 0x80 == 0 {
            return Err(CpuError::GeneralProtection { selector }.into());
        }

        let base = ((low >> 16) & 0xffff) | ((high & 0x0000_00ff) << 16) | (high & 0xff00_0000);
        let mut limit = (low & 0xffff) | (high & 0x000f_0000);
        if high & 0x0080_0000 != 0 {
            limit = (limit << 12) | 0x0fff;
        }

        Ok(SegmentRegister {
            selector,
            base,
            limit,
            access,
            default_size_32: high & 0x0040_0000 != 0,
        })
    }

    fn segment_from_reg_field(&self, reg: u8) -> SegmentRegister {
        match reg {
            0 => self.registers.segment(SegmentIndex::Es),
            1 => self.registers.segment(SegmentIndex::Cs),
            2 => self.registers.segment(SegmentIndex::Ss),
            3 => self.registers.segment(SegmentIndex::Ds),
            4 => self.registers.segment(SegmentIndex::Fs),
            _ => self.registers.segment(SegmentIndex::Gs),
        }
    }

    fn read_gpr32(&self, index: u8) -> u32 {
        self.registers.gpr[usize::from(index & 7)]
    }

    fn write_gpr32(&mut self, index: u8, value: u32) {
        self.registers.gpr[usize::from(index & 7)] = value;
    }

    fn read_gpr16(&self, index: u8) -> u16 {
        self.registers.gpr[usize::from(index & 7)] as u16
    }

    fn write_gpr16(&mut self, index: u8, value: u16) {
        let slot = &mut self.registers.gpr[usize::from(index & 7)];
        *slot = (*slot & 0xffff_0000) | u32::from(value);
    }

    fn read_gpr8(&self, index: u8) -> u8 {
        let reg = usize::from(index & 3);
        if index < 4 {
            self.registers.gpr[reg] as u8
        } else {
            (self.registers.gpr[reg] >> 8) as u8
        }
    }

    fn write_gpr8(&mut self, index: u8, value: u8) {
        let reg = usize::from(index & 3);
        if index < 4 {
            self.registers.gpr[reg] = (self.registers.gpr[reg] & !0xff) | u32::from(value);
        } else {
            self.registers.gpr[reg] = (self.registers.gpr[reg] & !0xff00) | (u32::from(value) << 8);
        }
    }

    fn read_gpr_sized(&self, index: u8, operand_size: OperandSize) -> u32 {
        match operand_size {
            OperandSize::Word => u32::from(self.read_gpr16(index)),
            OperandSize::Dword => self.read_gpr32(index),
        }
    }

    fn write_gpr_sized(&mut self, index: u8, operand_size: OperandSize, value: u32) {
        match operand_size {
            OperandSize::Word => self.write_gpr16(index, value as u16),
            OperandSize::Dword => self.write_gpr32(index, value),
        }
    }

    fn flag(&self, flag: u32) -> bool {
        self.registers.eflags & flag != 0
    }

    fn set_flag(&mut self, flag: u32, enabled: bool) {
        if enabled {
            self.registers.eflags |= flag;
        } else {
            self.registers.eflags &= !flag;
        }
        self.registers.eflags |= 0x2;
    }

    fn alu(&mut self, op: u8, a: u32, b: u32, width: BusWidth) -> u32 {
        let mask = width_mask(width);
        let cf_in = u32::from(self.flag(FLAG_CF));
        match op {
            0 => self.alu_add(a, b, 0, width),
            2 => self.alu_add(a, b, cf_in, width),
            3 => self.alu_sub(a, b, cf_in, width),
            5 | 7 => self.alu_sub(a, b, 0, width),
            1 => {
                let result = (a | b) & mask;
                self.set_flag(FLAG_CF | FLAG_OF, false);
                self.set_szp(result, width);
                result
            }
            4 => {
                let result = (a & b) & mask;
                self.set_flag(FLAG_CF | FLAG_OF, false);
                self.set_szp(result, width);
                result
            }
            6 => {
                let result = (a ^ b) & mask;
                self.set_flag(FLAG_CF | FLAG_OF, false);
                self.set_szp(result, width);
                result
            }
            _ => unreachable!("alu op {op}"),
        }
    }

    fn inc_dec(&mut self, value: u32, is_dec: bool, width: BusWidth) -> u32 {
        // INC/DEC affect OF/SF/ZF/AF/PF exactly like ADD/SUB by 1, but leave CF.
        let carry = self.flag(FLAG_CF);
        let result = if is_dec {
            self.alu_sub(value, 1, 0, width)
        } else {
            self.alu_add(value, 1, 0, width)
        };
        self.set_flag(FLAG_CF, carry);
        result
    }

    fn alu_add(&mut self, a: u32, b: u32, carry: u32, width: BusWidth) -> u32 {
        let mask = width_mask(width);
        let sign = width_sign(width);
        let a = a & mask;
        let b = b & mask;
        let full = u64::from(a) + u64::from(b) + u64::from(carry);
        let result = (full as u32) & mask;
        self.set_flag(FLAG_CF, full > u64::from(mask));
        self.set_flag(FLAG_OF, ((a ^ result) & (b ^ result) & sign) != 0);
        self.set_flag(FLAG_AF, ((a ^ b ^ result) & 0x10) != 0);
        self.set_szp(result, width);
        result
    }

    fn alu_sub(&mut self, a: u32, b: u32, borrow: u32, width: BusWidth) -> u32 {
        let mask = width_mask(width);
        let sign = width_sign(width);
        let a = a & mask;
        let b = b & mask;
        let rhs = u64::from(b) + u64::from(borrow);
        let result = (u64::from(a).wrapping_sub(rhs) as u32) & mask;
        self.set_flag(FLAG_CF, u64::from(a) < rhs);
        self.set_flag(FLAG_OF, ((a ^ b) & (a ^ result) & sign) != 0);
        self.set_flag(FLAG_AF, ((a ^ b ^ result) & 0x10) != 0);
        self.set_szp(result, width);
        result
    }

    fn set_szp(&mut self, result: u32, width: BusWidth) {
        let mask = width_mask(width);
        let sign = width_sign(width);
        self.set_flag(FLAG_ZF, result & mask == 0);
        self.set_flag(FLAG_SF, result & sign != 0);
        self.set_flag(FLAG_PF, parity(result as u8));
    }

    fn condition(&self, condition: u8) -> bool {
        match condition {
            0x0 => self.flag(FLAG_OF),
            0x1 => !self.flag(FLAG_OF),
            0x2 => self.flag(FLAG_CF),
            0x3 => !self.flag(FLAG_CF),
            0x4 => self.flag(FLAG_ZF),
            0x5 => !self.flag(FLAG_ZF),
            0x6 => self.flag(FLAG_CF) || self.flag(FLAG_ZF),
            0x7 => !self.flag(FLAG_CF) && !self.flag(FLAG_ZF),
            0x8 => self.flag(FLAG_SF),
            0x9 => !self.flag(FLAG_SF),
            0xa => self.flag(FLAG_PF),
            0xb => !self.flag(FLAG_PF),
            0xc => self.flag(FLAG_SF) != self.flag(FLAG_OF),
            0xd => self.flag(FLAG_SF) == self.flag(FLAG_OF),
            0xe => self.flag(FLAG_ZF) || (self.flag(FLAG_SF) != self.flag(FLAG_OF)),
            _ => !self.flag(FLAG_ZF) && (self.flag(FLAG_SF) == self.flag(FLAG_OF)),
        }
    }
}

pub fn linear_address(segment: u16, offset: u16) -> usize {
    (usize::from(segment) << 4) + usize::from(offset)
}

fn clocks(core_clocks: u32) -> CycleOutcome {
    CycleOutcome {
        core_clocks,
        halted: false,
    }
}

fn sign_extend_u8(value: u8) -> u32 {
    value as i8 as i32 as u32
}

const fn width_mask(width: BusWidth) -> u32 {
    match width {
        BusWidth::Byte => 0x0000_00ff,
        BusWidth::Word => 0x0000_ffff,
        BusWidth::Dword => 0xffff_ffff,
    }
}

const fn width_sign(width: BusWidth) -> u32 {
    match width {
        BusWidth::Byte => 0x0000_0080,
        BusWidth::Word => 0x0000_8000,
        BusWidth::Dword => 0x8000_0000,
    }
}

fn parity(value: u8) -> bool {
    value.count_ones() % 2 == 0
}

fn page_fault_code(present: bool, write: bool, instruction_fetch: bool) -> u32 {
    u32::from(present) | (u32::from(write) << 1) | (u32::from(instruction_fetch) << 4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use virtualdos_bus::{BusCycle, BusTrace, BusWidth};

    #[derive(Default)]
    struct TestBus {
        memory: Vec<u8>,
        trace: BusTrace,
    }

    impl TestBus {
        fn with_memory(memory: Vec<u8>) -> Self {
            Self {
                memory,
                trace: BusTrace::default(),
            }
        }
    }

    impl CpuBus for TestBus {
        fn read_memory(
            &mut self,
            address: u32,
            width: BusWidth,
            kind: BusAccessKind,
        ) -> Result<u32, BusError> {
            self.trace.push(BusCycle::new(kind, address, width, 0));
            let address = address as usize;
            Ok(match width {
                BusWidth::Byte => u32::from(self.memory[address]),
                BusWidth::Word => u32::from(u16::from_le_bytes([
                    self.memory[address],
                    self.memory[address + 1],
                ])),
                BusWidth::Dword => u32::from_le_bytes([
                    self.memory[address],
                    self.memory[address + 1],
                    self.memory[address + 2],
                    self.memory[address + 3],
                ]),
            })
        }

        fn write_memory(
            &mut self,
            address: u32,
            width: BusWidth,
            value: u32,
            kind: BusAccessKind,
        ) -> Result<(), BusError> {
            self.trace.push(BusCycle::new(kind, address, width, 0));
            let address = address as usize;
            match width {
                BusWidth::Byte => self.memory[address] = value as u8,
                BusWidth::Word => {
                    self.memory[address..address + 2].copy_from_slice(&(value as u16).to_le_bytes())
                }
                BusWidth::Dword => {
                    self.memory[address..address + 4].copy_from_slice(&value.to_le_bytes())
                }
            }
            Ok(())
        }

        fn read_io(&mut self, port: u16, width: BusWidth) -> Result<u32, BusError> {
            self.trace.push(BusCycle::new(
                BusAccessKind::IoRead,
                u32::from(port),
                width,
                0,
            ));
            Ok(0)
        }

        fn write_io(&mut self, port: u16, width: BusWidth, _value: u32) -> Result<(), BusError> {
            self.trace.push(BusCycle::new(
                BusAccessKind::IoWrite,
                u32::from(port),
                width,
                0,
            ));
            Ok(())
        }

        fn interrupt_acknowledge(&mut self, vector: u8, _ax: u16) -> Result<(), BusError> {
            self.trace.push(BusCycle::new(
                BusAccessKind::InterruptAcknowledge,
                u32::from(vector),
                BusWidth::Byte,
                0,
            ));
            Ok(())
        }
    }

    #[test]
    fn reset_state_starts_at_386_reset_vector() {
        let cpu = Cpu386::default();

        assert_eq!(cpu.registers.cs().selector, 0xf000);
        assert_eq!(cpu.registers.cs().base, 0xffff_0000);
        assert_eq!(cpu.registers.eip, 0xfff0);
        assert_eq!(cpu.linear_eip(), 0xffff_fff0);
    }

    #[test]
    fn register_aliasing_updates_low_parts() {
        let mut cpu = Cpu386::default();
        cpu.registers.set_eax(0x1234_5678);

        cpu.write_reg16(Reg16::Ax, 0xabcd);
        cpu.write_gpr8(4, 0xef);

        assert_eq!(cpu.registers.eax(), 0x1234_efcd);
        assert_eq!(cpu.read_reg16(Reg16::Ax), 0xefcd);
    }

    #[test]
    fn operand_prefix_allows_32bit_mov_in_real_mode() {
        let mut memory = vec![0; 32];
        memory[0..6].copy_from_slice(&[0x66, 0xb8, 0x78, 0x56, 0x34, 0x12]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eax(), 0x1234_5678);
        assert_eq!(cpu.registers.eip, 6);
    }

    #[test]
    fn modrm_direct_address_can_store_ax() {
        let mut memory = vec![0; 1024];
        memory[0..5].copy_from_slice(&[0x89, 0x06, 0x00, 0x02, 0xf4]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x4f56);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(
            u16::from_le_bytes([bus.memory[0x200], bus.memory[0x201]]),
            0x4f56
        );
    }

    #[test]
    fn page_translation_reads_identity_mapped_memory() {
        let mut memory = vec![0; 0x4000];
        memory[0x1000..0x1004].copy_from_slice(&0x0000_2003u32.to_le_bytes());
        memory[0x2000..0x2004].copy_from_slice(&0x0000_3003u32.to_le_bytes());
        memory[0x3000] = 0x90;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.control.cr3 = 0x1000;
        cpu.control.cr0 |= CR0_PG;
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eip, 1);
    }

    #[test]
    fn stosb_writes_al_to_es_di() {
        let mut memory = vec![0; 1024];
        memory[0] = 0xaa;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_edi(0x200);
        cpu.write_gpr8(0, b'S');
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(bus.memory[0x200], b'S');
        assert_eq!(cpu.registers.edi(), 0x201);
    }

    #[test]
    fn out_dx_al_uses_dx_port() {
        let mut memory = vec![0; 16];
        memory[0..6].copy_from_slice(&[0xba, 0xf8, 0x03, 0xb0, b'X', 0xee]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();
        cpu.cycle(&mut bus).unwrap();
        cpu.cycle(&mut bus).unwrap();

        assert!(
            bus.trace
                .cycles()
                .iter()
                .any(|cycle| { cycle.kind == BusAccessKind::IoWrite && cycle.address == 0x03f8 })
        );
    }

    #[test]
    fn test_byte_sets_sign_flag() {
        // test al, al with al = 0x80  (0x84 modrm 0xc0). SF must reflect bit 7.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0x84, 0xc0]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr8(0, 0x80);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_SF));
        assert!(!cpu.flag(FLAG_ZF));
        assert!(!cpu.flag(FLAG_CF));
    }

    #[test]
    fn test_word_immediate_group_f7() {
        // test bx, 0x0001  (0xf7 /0, modrm 0xc3, imm 0x0001). bx=0x0002 -> ZF set.
        let mut memory = vec![0; 16];
        memory[0..4].copy_from_slice(&[0xf7, 0xc3, 0x01, 0x00]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x0002);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_ZF));
    }

    #[test]
    fn group81_add_memory_with_displacement_and_immediate() {
        // add word [bx+0x10], 0x0102  (0x81 /0, modrm 0x47, disp 0x10, imm 0x0102)
        let mut memory = vec![0; 1024];
        memory[0..6].copy_from_slice(&[0x81, 0x47, 0x10, 0x02, 0x01, 0xf4]);
        memory[0x210..0x212].copy_from_slice(&0x0003u16.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x200);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(
            u16::from_le_bytes([bus.memory[0x210], bus.memory[0x211]]),
            0x0105
        );
        assert_eq!(cpu.registers.eip, 5); // opcode + modrm + disp8 + imm16
    }

    #[test]
    fn group83_sign_extends_immediate() {
        // sub bx, -1  (0x83 /5, modrm 0xeb, imm 0xff -> -1)
        let mut memory = vec![0; 16];
        memory[0..3].copy_from_slice(&[0x83, 0xeb, 0xff]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x0005);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Bx), 0x0006); // 5 - (-1) = 6
    }

    #[test]
    fn add_rm_reg_byte_writes_memory_with_displacement() {
        // add [bx+0x10], al   (opcode 0x00, modrm 0x47, disp 0x10)
        let mut memory = vec![0; 1024];
        memory[0..4].copy_from_slice(&[0x00, 0x47, 0x10, 0xf4]);
        memory[0x210] = 0x01; // [bx+0x10] initial
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x200);
        cpu.write_gpr8(0, 0x05); // al
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(bus.memory[0x210], 0x06);
        assert_eq!(cpu.registers.eip, 3); // opcode + modrm + disp8, no double-fetch
    }

    #[test]
    fn sub_reg_rm_sets_flags() {
        // sub al, bl  (opcode 0x2a, modrm 0xc3)
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0x2a, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr8(0, 0x05); // al
        cpu.write_gpr8(3, 0x05); // bl
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_gpr8(0), 0x00);
        assert!(cpu.flag(FLAG_ZF));
        assert!(!cpu.flag(FLAG_CF));
    }

    #[test]
    fn cmp_does_not_write_back() {
        // cmp al, 0x10 is form via 0x3c (AL, imm8)
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0x3c, 0x10]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr8(0, 0x10);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_gpr8(0), 0x10); // unchanged
        assert!(cpu.flag(FLAG_ZF));
    }

    #[test]
    fn alu_add_byte_sets_carry_zero_and_aux() {
        let mut cpu = Cpu386::default();
        let result = cpu.alu(0, 0xff, 0x01, BusWidth::Byte);
        assert_eq!(result, 0x00);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_ZF));
        assert!(cpu.flag(FLAG_AF));
        assert!(!cpu.flag(FLAG_OF));
    }

    #[test]
    fn alu_adc_uses_carry_in() {
        let mut cpu = Cpu386::default();
        cpu.set_flag(FLAG_CF, true);
        let result = cpu.alu(2, 0x01, 0x01, BusWidth::Word); // ADC 1,1 with CF=1 -> 3
        assert_eq!(result, 0x0003);
        assert!(!cpu.flag(FLAG_CF));
    }

    #[test]
    fn alu_sub_byte_sets_borrow_and_sign() {
        let mut cpu = Cpu386::default();
        let result = cpu.alu(5, 0x00, 0x01, BusWidth::Byte); // 0 - 1 = 0xff
        assert_eq!(result, 0xff);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_SF));
        assert!(cpu.flag(FLAG_AF));
    }

    #[test]
    fn alu_sbb_uses_borrow_in() {
        let mut cpu = Cpu386::default();
        cpu.set_flag(FLAG_CF, true);
        let result = cpu.alu(3, 0x05, 0x02, BusWidth::Word); // 5 - 2 - 1 = 2
        assert_eq!(result, 0x0002);
        assert!(!cpu.flag(FLAG_CF));
    }

    #[test]
    fn alu_logic_clears_carry_overflow_leaves_aux() {
        let mut cpu = Cpu386::default();
        cpu.set_flag(FLAG_AF, true);
        let result = cpu.alu(4, 0xf0, 0x0f, BusWidth::Byte); // AND -> 0
        assert_eq!(result, 0x00);
        assert!(!cpu.flag(FLAG_CF));
        assert!(!cpu.flag(FLAG_OF));
        assert!(cpu.flag(FLAG_ZF));
        assert!(cpu.flag(FLAG_AF)); // AND leaves AF untouched (undefined)
    }

    #[test]
    fn alu_add_byte_overflow_without_carry() {
        let mut cpu = Cpu386::default();
        let result = cpu.alu(0, 0x7f, 0x01, BusWidth::Byte); // 127 + 1 -> 0x80
        assert_eq!(result, 0x80);
        assert!(cpu.flag(FLAG_OF)); // signed overflow, isolated from carry
        assert!(!cpu.flag(FLAG_CF)); // no unsigned carry
        assert!(cpu.flag(FLAG_SF));
        assert!(cpu.flag(FLAG_AF));
    }

    #[test]
    fn alu_sbb_borrow_in_with_max_subtrahend() {
        let mut cpu = Cpu386::default();
        cpu.set_flag(FLAG_CF, true); // borrow in
        let result = cpu.alu(3, 0x00, 0xff, BusWidth::Byte); // 0 - 0xff - 1
        assert_eq!(result, 0x00);
        assert!(cpu.flag(FLAG_CF)); // b + borrow must not wrap to 0 and clear CF
        assert!(cpu.flag(FLAG_ZF));
    }

    #[test]
    fn alu_parity_uses_low_byte_only() {
        let mut cpu = Cpu386::default();
        let result = cpu.alu(0, 0x00ff, 0x0001, BusWidth::Word); // -> 0x0100
        assert_eq!(result, 0x0100);
        assert!(cpu.flag(FLAG_PF)); // low byte 0x00 is even parity; full word would be odd
    }

    #[test]
    fn alu_sign_flag_word_uses_bit15() {
        let mut cpu = Cpu386::default();
        let result = cpu.alu(0, 0x8000, 0x0000, BusWidth::Word);
        assert_eq!(result, 0x8000);
        assert!(cpu.flag(FLAG_SF));
    }

    #[test]
    fn inc_reg_preserves_carry_flag() {
        // inc ax (0x40) with CF set: AX increments, CF stays set, AF set by 0xff+1.
        let mut memory = vec![0; 16];
        memory[0] = 0x40;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_CF, true);
        cpu.write_reg16(Reg16::Ax, 0x00ff);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0100);
        assert!(cpu.flag(FLAG_CF)); // INC must not touch CF
        assert!(cpu.flag(FLAG_AF));
    }

    #[test]
    fn dec_reg_sets_zero_and_keeps_carry_clear() {
        // dec ax (0x48) with CF clear: AX -> 0, ZF set, CF still clear.
        let mut memory = vec![0; 16];
        memory[0] = 0x48;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_CF, false);
        cpu.write_reg16(Reg16::Ax, 0x0001);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0000);
        assert!(cpu.flag(FLAG_ZF));
        assert!(!cpu.flag(FLAG_CF));
    }

    #[test]
    fn inc_byte_memory_with_displacement() {
        // inc byte [bx+0x10]  (0xfe /0, modrm 0x47, disp 0x10). 0x7f -> 0x80.
        let mut memory = vec![0; 1024];
        memory[0..3].copy_from_slice(&[0xfe, 0x47, 0x10]);
        memory[0x210] = 0x7f;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x200);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(bus.memory[0x210], 0x80);
        assert!(cpu.flag(FLAG_SF));
        assert!(cpu.flag(FLAG_OF)); // 0x7f + 1 byte overflow
        assert_eq!(cpu.registers.eip, 3); // opcode + modrm + disp8
    }

    #[test]
    fn inc_word_overflow_sets_of_and_sf() {
        // inc ax (0x40) on 0x7fff: -> 0x8000, OF and SF set, CF preserved.
        let mut memory = vec![0; 16];
        memory[0] = 0x40;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_CF, true);
        cpu.write_reg16(Reg16::Ax, 0x7fff);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
        assert!(cpu.flag(FLAG_OF));
        assert!(cpu.flag(FLAG_SF));
        assert!(cpu.flag(FLAG_CF)); // preserved
    }

    #[test]
    fn cmp_memory_form_issues_no_write() {
        // cmp [bx], al  (0x38 modrm 0x07). Equal operands -> ZF, and no write cycle.
        let mut memory = vec![0; 1024];
        memory[0..3].copy_from_slice(&[0x38, 0x07, 0xf4]);
        memory[0x200] = 0x42;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x200);
        cpu.write_gpr8(0, 0x42); // al
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_ZF));
        assert_eq!(bus.memory[0x200], 0x42); // unchanged
        assert!(
            !bus.trace
                .cycles()
                .iter()
                .any(|cycle| cycle.kind == BusAccessKind::DataWrite)
        );
    }
}
