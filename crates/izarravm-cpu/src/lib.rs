use izarravm_bus::{BusAccessKind, BusError, BusWidth, CpuBus};
use thiserror::Error;

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
    #[error("divide error (#DE): divide by zero or quotient overflow")]
    DivideError,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepKind {
    Repe,
    Repne,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Prefixes {
    operand_size_override: bool,
    address_size_override: bool,
    rep: Option<RepKind>,
    segment_override: Option<SegmentIndex>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringOp {
    Movs,
    Cmps,
    Scas,
    Stos,
    Lods,
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
        // Stack code also uses this as a proxy for the SS B-bit (stack width): real
        // mode always has a 16-bit stack. Replace with a real SS.B check when 32-bit
        // protected-mode stacks land.
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
            0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 | 0x67 => {
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
            0x60 => {
                // PUSHA / PUSHAD: push AX, CX, DX, BX, the pre-instruction SP, BP, SI, DI.
                let sp_snapshot = self.read_gpr_sized(4, operand_size);
                for index in [0u8, 1, 2, 3] {
                    let value = self.read_gpr_sized(index, operand_size);
                    self.push(bus, value, operand_size)?;
                }
                self.push(bus, sp_snapshot, operand_size)?;
                for index in [5u8, 6, 7] {
                    let value = self.read_gpr_sized(index, operand_size);
                    self.push(bus, value, operand_size)?;
                }
                Ok(clocks(18))
            }
            0x61 => {
                // POPA / POPAD: pop DI, SI, BP, discard the SP slot, then BX, DX, CX, AX.
                for index in [7u8, 6, 5] {
                    let value = self.pop(bus, operand_size)?;
                    self.write_gpr_sized(index, operand_size, value);
                }
                let discarded = self.pop(bus, operand_size)?; // SP slot, SP advances over it
                for index in [3u8, 2, 1, 0] {
                    let value = self.pop(bus, operand_size)?;
                    self.write_gpr_sized(index, operand_size, value);
                }
                // On a 16-bit stack, POPAD leaves SP advanced but lets the discarded
                // saved-ESP slot's high half land in ESP[31:16]. Verified against the
                // 80386 vectors; the register loads above are unaffected.
                if !self.is_protected_mode() && matches!(operand_size, OperandSize::Dword) {
                    let advanced = self.registers.esp();
                    self.registers
                        .set_esp((discarded & 0xffff_0000) | (advanced & 0xffff));
                }
                Ok(clocks(18))
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
            0x86 => {
                // XCHG r/m8, r8. Decode the operand once, then cross-write; decoding twice would
                // re-fetch the displacement and over-advance eip for a memory operand.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let rm = self.read_operand_u8(bus, operand)?;
                let reg = self.read_gpr8(modrm.reg);
                self.write_operand_u8(bus, operand, reg)?;
                self.write_gpr8(modrm.reg, rm);
                Ok(clocks(3))
            }
            0x87 => {
                // XCHG r/m16/32, r16/32. Decode the operand once, then cross-write.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let rm = self.read_operand_sized(bus, operand, operand_size)?;
                let reg = self.read_gpr_sized(modrm.reg, operand_size);
                self.write_operand_sized(bus, operand, operand_size, reg)?;
                self.write_gpr_sized(modrm.reg, operand_size, rm);
                Ok(clocks(3))
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
            0x8d => {
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                match operand {
                    RmOperand::Memory(mem) => {
                        // LEA loads the effective address, not the memory it points at.
                        self.write_gpr_sized(modrm.reg, operand_size, mem.offset);
                        Ok(clocks(2))
                    }
                    // LEA requires a memory operand; mod=3 is an invalid encoding.
                    RmOperand::Register(_) => Err(InternalFault::Exception {
                        vector: 6,
                        error_code: None,
                    }),
                }
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
            0x91..=0x97 => {
                // XCHG AX/EAX, reg. The register index is the low 3 opcode bits; 0x90 (index 0,
                // XCHG AX,AX) is the existing NOP arm.
                let reg = opcode & 7;
                let acc = self.read_gpr_sized(0, operand_size);
                let other = self.read_gpr_sized(reg, operand_size);
                self.write_gpr_sized(0, operand_size, other);
                self.write_gpr_sized(reg, operand_size, acc);
                Ok(clocks(3))
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
                Ok(clocks(3))
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
                Ok(clocks(2))
            }
            0x9c => {
                // PUSHF / PUSHFD. The 386 EFLAGS defines no bits above 15 except RF and
                // VM, both of which PUSHFD clears in the pushed image, so both forms push
                // the identical low 16 flag bits; the dword form just zero-extends to 32.
                // Confirmed against the 669C vectors. operand_size still drives whether
                // push writes 2 or 4 bytes.
                let value = self.registers.eflags & 0xffff;
                self.push(bus, value, operand_size)?;
                Ok(clocks(3))
            }
            0x9d => {
                // POPF / POPFD: load the popped image through the shared flag-load.
                let value = self.pop(bus, operand_size)?;
                self.load_flags(value, operand_size);
                Ok(clocks(4))
            }
            0x9e => {
                // SAHF: load CF/PF/AF/ZF/SF from AH; OF and the reserved bits are untouched.
                // The trailing | 0x02 keeps the always-one reserved bit set, matching
                // set_flag and load_flags.
                let ah = u32::from(self.read_gpr8(4));
                self.registers.eflags = (self.registers.eflags & !0xd5) | (ah & 0xd5) | 0x02;
                Ok(clocks(3))
            }
            0x9f => {
                // LAHF: AH = low flag byte with bit1 forced 1, bits 3 and 5 forced 0.
                let ah = ((self.registers.eflags as u8) & 0xd5) | 0x02;
                self.write_gpr8(4, ah);
                Ok(clocks(2))
            }
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
            0xa4 => {
                self.run_string(bus, StringOp::Movs, BusWidth::Byte, prefixes, address_size)?;
                Ok(clocks(4))
            }
            0xa5 => {
                self.run_string(
                    bus,
                    StringOp::Movs,
                    operand_size.bus_width(),
                    prefixes,
                    address_size,
                )?;
                Ok(clocks(4))
            }
            0xa6 => {
                self.run_string(bus, StringOp::Cmps, BusWidth::Byte, prefixes, address_size)?;
                Ok(clocks(4))
            }
            0xa7 => {
                self.run_string(
                    bus,
                    StringOp::Cmps,
                    operand_size.bus_width(),
                    prefixes,
                    address_size,
                )?;
                Ok(clocks(4))
            }
            0xae => {
                self.run_string(bus, StringOp::Scas, BusWidth::Byte, prefixes, address_size)?;
                Ok(clocks(4))
            }
            0xaf => {
                self.run_string(
                    bus,
                    StringOp::Scas,
                    operand_size.bus_width(),
                    prefixes,
                    address_size,
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
                self.run_string(bus, StringOp::Stos, BusWidth::Byte, prefixes, address_size)?;
                Ok(clocks(4))
            }
            0xab => {
                self.run_string(
                    bus,
                    StringOp::Stos,
                    operand_size.bus_width(),
                    prefixes,
                    address_size,
                )?;
                Ok(clocks(4))
            }
            0xac => {
                self.run_string(bus, StringOp::Lods, BusWidth::Byte, prefixes, address_size)?;
                Ok(clocks(4))
            }
            0xad => {
                self.run_string(
                    bus,
                    StringOp::Lods,
                    operand_size.bus_width(),
                    prefixes,
                    address_size,
                )?;
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
            0xc2 => {
                // RET near, release imm16 bytes of arguments. The release count is always a
                // 16-bit immediate fetched from the instruction stream before the return
                // address is popped; the operand size only selects the offset pop width.
                let release = self.fetch_u16(bus)?;
                let target = self.pop(bus, operand_size)?;
                self.registers.eip = target & operand_size.mask();
                self.release_stack(release);
                Ok(clocks(10))
            }
            0xc3 => {
                let target = self.pop(bus, operand_size)?;
                self.registers.eip = target & operand_size.mask();
                Ok(clocks(10))
            }
            0xca => {
                // RETF, release imm16 bytes. The count is part of the instruction stream, so
                // it is fetched before the pops.
                let release = self.fetch_u16(bus)?;
                self.return_far(bus, operand_size)?;
                self.release_stack(release);
                Ok(clocks(17))
            }
            0xcb => {
                self.return_far(bus, operand_size)?;
                Ok(clocks(17))
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
            0xc9 => {
                // LEAVE: (E)SP <- (E)BP, then (E)BP <- pop. The stack-pointer move
                // follows the stack width: a 16-bit real-mode stack moves only SP and
                // keeps ESP[31:16]; a 32-bit stack moves the full ESP. The operand size
                // still selects BP vs EBP for the popped frame pointer.
                let frame = self.read_gpr_sized(5, operand_size);
                if self.is_protected_mode() {
                    self.write_gpr_sized(4, operand_size, frame);
                } else {
                    self.write_gpr16(4, frame as u16);
                }
                let saved = self.pop(bus, operand_size)?;
                self.write_gpr_sized(5, operand_size, saved);
                Ok(clocks(4))
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
            0xe0 | 0xe1 => {
                // LOOPNE (E0) / LOOPE (E1): decrement (E)CX, branch while non-zero and ZF matches.
                let rel = self.fetch_i8(bus)? as i32;
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
                let taken = count_nonzero && (if opcode == 0xe1 { zf } else { !zf });
                if taken {
                    self.relative_jump(rel, operand_size);
                }
                Ok(clocks(11))
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
            0xe3 => {
                // JCXZ / JECXZ: no decrement; branch when (E)CX is zero.
                let rel = self.fetch_i8(bus)? as i32;
                let count_zero = match address_size {
                    AddressSize::Word => self.read_gpr16(1) == 0,
                    AddressSize::Dword => self.registers.ecx() == 0,
                };
                if count_zero {
                    self.relative_jump(rel, operand_size);
                }
                Ok(clocks(9))
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
            0x9a => {
                let offset = match operand_size {
                    OperandSize::Word => u32::from(self.fetch_u16(bus)?),
                    OperandSize::Dword => self.fetch_u32(bus)?,
                };
                let selector = self.fetch_u16(bus)?;
                self.far_call(bus, selector, offset, operand_size)?;
                Ok(clocks(17))
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
                if modrm.reg == 0 {
                    // TEST r/m8, imm8 (unchanged)
                    let value = self.read_rm_u8(bus, prefixes, address_size, modrm)?;
                    let imm = self.fetch_u8(bus)?;
                    self.alu(4, u32::from(value), u32::from(imm), BusWidth::Byte);
                    return Ok(clocks(2));
                }
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let value = u32::from(self.read_operand_u8(bus, operand)?);
                match modrm.reg {
                    2 => self.write_operand_u8(bus, operand, !(value as u8))?, // NOT: no flags changed
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
                        return Err(CpuError::UnsupportedGroupOpcode {
                            opcode: 0xf6,
                            extension: modrm.reg,
                        }
                        .into());
                    }
                }
                Ok(clocks(2))
            }
            0xf7 => {
                let modrm = self.fetch_modrm(bus)?;
                if modrm.reg == 0 {
                    // TEST r/m, imm (unchanged)
                    let value =
                        self.read_rm_sized(bus, prefixes, address_size, operand_size, modrm)?;
                    let imm = self.fetch_immediate(bus, operand_size)?;
                    self.alu(4, value, imm, operand_size.bus_width());
                    return Ok(clocks(2));
                }
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let value = self.read_operand_sized(bus, operand, operand_size)?;
                match modrm.reg {
                    2 => {
                        // NOT: bitwise complement, no flags changed. Mask like every
                        // other write_operand_sized caller so no high bits are passed.
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
                        return Err(CpuError::UnsupportedGroupOpcode {
                            opcode: 0xf7,
                            extension: modrm.reg,
                        }
                        .into());
                    }
                }
                Ok(clocks(2))
            }
            0xf5 => {
                // CMC: complement the carry flag.
                self.set_flag(FLAG_CF, !self.flag(FLAG_CF));
                Ok(clocks(2))
            }
            0xf8 => {
                self.set_flag(FLAG_CF, false);
                Ok(clocks(2))
            }
            0xf9 => {
                // STC: set the carry flag.
                self.set_flag(FLAG_CF, true);
                Ok(clocks(2))
            }
            0xfa => {
                self.set_flag(FLAG_IF, false);
                Ok(clocks(3))
            }
            0xfb => {
                // STI sets IF. The one-instruction interrupt shadow is irrelevant until
                // interrupt delivery exists.
                self.set_flag(FLAG_IF, true);
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
            0xff => {
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                match modrm.reg {
                    0 | 1 => {
                        let value = self.read_operand_sized(bus, operand, operand_size)?;
                        let result = self.inc_dec(value, modrm.reg == 1, operand_size.bus_width());
                        self.write_operand_sized(bus, operand, operand_size, result)?;
                        Ok(clocks(2))
                    }
                    2 => {
                        let target = self.read_operand_sized(bus, operand, operand_size)?;
                        self.push(bus, self.registers.eip, operand_size)?;
                        self.registers.eip = target & operand_size.mask();
                        Ok(clocks(7))
                    }
                    4 => {
                        let target = self.read_operand_sized(bus, operand, operand_size)?;
                        self.registers.eip = target & operand_size.mask();
                        Ok(clocks(7))
                    }
                    6 => {
                        let value = self.read_operand_sized(bus, operand, operand_size)?;
                        self.push(bus, value, operand_size)?;
                        Ok(clocks(2))
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
                        // The selector follows the offset in memory. Its address is
                        // computed in the address-size space, so on a 16-bit real-mode
                        // segment it wraps at 0xffff (offset 0xfffe puts the selector at
                        // 0x0000, not past the limit), matching the 80386.
                        let selector_offset = match address_size {
                            AddressSize::Word => u32::from(
                                (memory.offset as u16).wrapping_add(operand_size.bytes() as u16),
                            ),
                            AddressSize::Dword => memory.offset.wrapping_add(operand_size.bytes()),
                        };
                        let selector = self.read_memory_sized(
                            bus,
                            memory.segment,
                            selector_offset,
                            OperandSize::Word,
                            BusAccessKind::DataRead,
                        )? as u16;
                        if modrm.reg == 3 {
                            self.far_call(bus, selector, offset, operand_size)?;
                        } else {
                            self.far_jump(bus, selector, offset, operand_size)?;
                        }
                        Ok(clocks(11))
                    }
                    extension => Err(CpuError::UnsupportedGroupOpcode {
                        opcode: 0xff,
                        extension,
                    }
                    .into()),
                }
            }
            0xc0 | 0xc1 | 0xd0 | 0xd1 | 0xd2 | 0xd3 => {
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let op = modrm.reg;
                let count = match opcode {
                    0xc0 | 0xc1 => self.fetch_u8(bus)?,
                    0xd0 | 0xd1 => 1,
                    _ => (self.registers.ecx() & 0xff) as u8,
                };
                if opcode & 1 == 0 {
                    // byte forms 0xc0/0xd0/0xd2
                    let value = u32::from(self.read_operand_u8(bus, operand)?);
                    let result = self.shift_rotate(op, value, count, BusWidth::Byte) as u8;
                    self.write_operand_u8(bus, operand, result)?;
                } else {
                    let value = self.read_operand_sized(bus, operand, operand_size)?;
                    let result = self.shift_rotate(op, value, count, operand_size.bus_width());
                    self.write_operand_sized(bus, operand, operand_size, result)?;
                }
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
            0xd7 => {
                // XLAT: AL = [segment:(B)X + AL]. DS is the default, overridable; the 16-bit base
                // plus AL wraps inside the segment.
                let segment = prefixes.segment_override.unwrap_or(SegmentIndex::Ds);
                let al = u32::from(self.read_gpr8(0));
                let offset = match address_size {
                    AddressSize::Word => u32::from(self.read_gpr16(3).wrapping_add(al as u16)),
                    AddressSize::Dword => self.read_gpr32(3).wrapping_add(al),
                };
                let value = self.read_memory_u8(bus, segment, offset, BusAccessKind::DataRead)?;
                self.write_gpr8(0, value);
                Ok(clocks(5))
            }
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
                Ok(clocks(4))
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
                Ok(clocks(4))
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
                Ok(clocks(4))
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
                Ok(clocks(4))
            }
            0xd4 => {
                // AAM: AH = AL / imm8, AL = AL % imm8. OF/AF/CF undefined; SF/ZF/PF from AL.
                let divisor = self.fetch_u8(bus)?;
                if divisor == 0 {
                    return Err(CpuError::DivideError.into());
                }
                let al = self.read_gpr8(0);
                self.write_gpr8(4, al / divisor);
                let rem = al % divisor;
                self.write_gpr8(0, rem);
                self.set_szp(u32::from(rem), BusWidth::Byte);
                Ok(clocks(17))
            }
            0xd5 => {
                // AAD: AL = (AL + AH*imm8) & 0xff, AH = 0. OF/AF/CF undefined; SF/ZF/PF from AL.
                let multiplier = self.fetch_u8(bus)?;
                let al = self.read_gpr8(0);
                let ah = self.read_gpr8(4);
                let result = al.wrapping_add(ah.wrapping_mul(multiplier));
                self.write_gpr8(0, result);
                self.write_gpr8(4, 0);
                self.set_szp(u32::from(result), BusWidth::Byte);
                Ok(clocks(19))
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
            0x90..=0x9f => {
                // SETcc r/m8: set the byte to 1 when the condition holds, else 0. The condition
                // code is the low nibble of the opcode; SETcc is always byte-wide and touches no
                // flags. The ModRM reg field is not used.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let set = self.condition(opcode & 0x0f);
                self.write_operand_u8(bus, operand, u8::from(set))?;
                Ok(clocks(4))
            }
            0xb6 => {
                // MOVZX r, r/m8: zero-extend the byte into the destination at the operand width.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let value = u32::from(self.read_operand_u8(bus, operand)?);
                self.write_gpr_sized(modrm.reg, operand_size, value);
                Ok(clocks(3))
            }
            0xb7 => {
                // MOVZX r, r/m16: zero-extend the word into the destination.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let value = self.read_operand_sized(bus, operand, OperandSize::Word)?;
                self.write_gpr_sized(modrm.reg, operand_size, value);
                Ok(clocks(3))
            }
            0xbe => {
                // MOVSX r, r/m8: sign-extend the byte into the destination.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let value = sign_extend_u8(self.read_operand_u8(bus, operand)?);
                self.write_gpr_sized(modrm.reg, operand_size, value);
                Ok(clocks(3))
            }
            0xbf => {
                // MOVSX r, r/m16: sign-extend the word into the destination.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let value =
                    self.read_operand_sized(bus, operand, OperandSize::Word)? as i16 as i32 as u32;
                self.write_gpr_sized(modrm.reg, operand_size, value);
                Ok(clocks(3))
            }
            0xaf => {
                // IMUL r, r/m: two-operand signed multiply into the reg destination.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let src = self.read_operand_sized(bus, operand, operand_size)?;
                let dst = self.read_gpr_sized(modrm.reg, operand_size);
                let result = self.imul_truncated(dst, src, operand_size);
                self.write_gpr_sized(modrm.reg, operand_size, result);
                Ok(clocks(9))
            }
            0xbc => {
                // BSF: index of the lowest set bit. Source 0 -> ZF=1, destination unchanged
                // (386 silicon; Intel documents the destination as undefined).
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let src =
                    self.read_operand_sized(bus, operand, operand_size)? & operand_size.mask();
                if src == 0 {
                    self.set_flag(FLAG_ZF, true);
                } else {
                    self.set_flag(FLAG_ZF, false);
                    self.write_gpr_sized(modrm.reg, operand_size, src.trailing_zeros());
                }
                Ok(clocks(10))
            }
            0xbd => {
                // BSR: index of the highest set bit. Source 0 -> ZF=1, destination unchanged
                // (386 silicon; Intel documents the destination as undefined).
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let src =
                    self.read_operand_sized(bus, operand, operand_size)? & operand_size.mask();
                if src == 0 {
                    self.set_flag(FLAG_ZF, true);
                } else {
                    self.set_flag(FLAG_ZF, false);
                    self.write_gpr_sized(modrm.reg, operand_size, 31 - src.leading_zeros());
                }
                Ok(clocks(10))
            }
            0xa3 | 0xab | 0xb3 | 0xbb => {
                // BT/BTS/BTR/BTC r/m, r. The opcodes are 8 apart: A3=BT, AB=BTS, B3=BTR, BB=BTC.
                // The bit index in the reg operand is signed for a memory operand.
                let op = (opcode - 0xa3) / 8;
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let index = self.read_gpr_sized(modrm.reg, operand_size);
                self.bit_string_op(bus, op, operand, index, operand_size, address_size, true)?;
                Ok(clocks(6))
            }
            0xba => {
                // BT/BTS/BTR/BTC r/m, imm8: /4=BT, /5=BTS, /6=BTR, /7=BTC. The imm8 follows the
                // ModRM and displacement. /0../3 are not defined bit-test ops.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let imm = self.fetch_u8(bus)?;
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
                    u32::from(imm),
                    operand_size,
                    address_size,
                    false,
                )?;
                Ok(clocks(6))
            }
            0xa4 | 0xac => {
                // SHLD (A4) / SHRD (AC) r/m, r, imm8. The imm8 follows the ModRM and displacement.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let src = self.read_gpr_sized(modrm.reg, operand_size);
                let count = self.fetch_u8(bus)?;
                let dest = self.read_operand_sized(bus, operand, operand_size)?;
                let result = self.double_shift(opcode == 0xa4, dest, src, count, operand_size);
                self.write_operand_sized(bus, operand, operand_size, result)?;
                Ok(clocks(3))
            }
            0xa5 | 0xad => {
                // SHLD (A5) / SHRD (AD) r/m, r, CL.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let src = self.read_gpr_sized(modrm.reg, operand_size);
                let count = (self.registers.ecx() & 0xff) as u8;
                let dest = self.read_operand_sized(bus, operand, operand_size)?;
                let result = self.double_shift(opcode == 0xa5, dest, src, count, operand_size);
                self.write_operand_sized(bus, operand, operand_size, result)?;
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
                0xf3 => prefixes.rep = Some(RepKind::Repe),
                0xf2 => prefixes.rep = Some(RepKind::Repne),
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
                if self.is_protected_mode() {
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
                } else {
                    // A real-mode stack is 16 bits: the address comes from SP, only SP
                    // advances, and ESP[31:16] is preserved. The SS B-bit-accurate
                    // version arrives with 32-bit protected-mode stacks.
                    let sp = self.read_gpr16(4).wrapping_sub(4);
                    self.write_gpr16(4, sp);
                    self.write_memory_sized(
                        bus,
                        SegmentIndex::Ss,
                        u32::from(sp),
                        OperandSize::Dword,
                        value,
                        BusAccessKind::DataWrite,
                    )
                }
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
                if self.is_protected_mode() {
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
                } else {
                    // Real-mode 16-bit stack: read from SP and advance only SP.
                    let sp = self.read_gpr16(4);
                    let value = self.read_memory_sized(
                        bus,
                        SegmentIndex::Ss,
                        u32::from(sp),
                        OperandSize::Dword,
                        BusAccessKind::DataRead,
                    )?;
                    self.write_gpr16(4, sp.wrapping_add(4));
                    Ok(value)
                }
            }
        }
    }

    fn index_offset(&self, index: u8, address_size: AddressSize) -> u32 {
        match address_size {
            AddressSize::Word => u32::from(self.read_gpr16(index)),
            AddressSize::Dword => self.read_gpr32(index),
        }
    }

    fn string_count(&self, address_size: AddressSize) -> u32 {
        self.index_offset(1, address_size) // CX / ECX
    }

    fn decrement_string_count(&mut self, address_size: AddressSize) {
        match address_size {
            AddressSize::Word => {
                let cx = self.read_gpr16(1).wrapping_sub(1);
                self.write_gpr16(1, cx);
            }
            AddressSize::Dword => {
                let ecx = self.read_gpr32(1).wrapping_sub(1);
                self.write_gpr32(1, ecx);
            }
        }
    }

    fn read_string_src<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        width: BusWidth,
    ) -> ExecResult<u32> {
        let segment = prefixes.segment_override.unwrap_or(SegmentIndex::Ds);
        let offset = self.index_offset(6, address_size); // SI / ESI
        let physical = self.translate_segmented(
            bus,
            segment,
            offset,
            width.bytes(),
            BusAccessKind::DataRead,
            false,
        )?;
        Ok(bus.read_memory(physical, width, BusAccessKind::DataRead)?)
    }

    fn read_string_dst<B: CpuBus>(
        &mut self,
        bus: &mut B,
        address_size: AddressSize,
        width: BusWidth,
    ) -> ExecResult<u32> {
        let offset = self.index_offset(7, address_size); // DI / EDI
        let physical = self.translate_segmented(
            bus,
            SegmentIndex::Es,
            offset,
            width.bytes(),
            BusAccessKind::DataRead,
            false,
        )?;
        Ok(bus.read_memory(physical, width, BusAccessKind::DataRead)?)
    }

    fn acc_read(&self, width: BusWidth) -> u32 {
        match width {
            BusWidth::Byte => u32::from(self.read_gpr8(0)),
            BusWidth::Word => u32::from(self.read_gpr16(0)),
            BusWidth::Dword => self.read_gpr32(0),
        }
    }

    fn acc_write(&mut self, width: BusWidth, value: u32) {
        match width {
            BusWidth::Byte => self.write_gpr8(0, value as u8),
            BusWidth::Word => self.write_gpr16(0, value as u16),
            BusWidth::Dword => self.write_gpr32(0, value),
        }
    }

    fn write_string_dst<B: CpuBus>(
        &mut self,
        bus: &mut B,
        address_size: AddressSize,
        width: BusWidth,
        value: u32,
    ) -> ExecResult<()> {
        let offset = self.index_offset(7, address_size); // DI / EDI
        let physical = self.translate_segmented(
            bus,
            SegmentIndex::Es,
            offset,
            width.bytes(),
            BusAccessKind::DataWrite,
            true,
        )?;
        bus.write_memory(physical, width, value, BusAccessKind::DataWrite)?;
        Ok(())
    }

    fn string_step<B: CpuBus>(
        &mut self,
        bus: &mut B,
        op: StringOp,
        width: BusWidth,
        prefixes: Prefixes,
        address_size: AddressSize,
    ) -> ExecResult<()> {
        let bytes = width.bytes();
        match op {
            StringOp::Movs => {
                let value = self.read_string_src(bus, prefixes, address_size, width)?;
                self.write_string_dst(bus, address_size, width, value)?;
                self.adjust_index_register(6, address_size, bytes);
                self.adjust_index_register(7, address_size, bytes);
            }
            StringOp::Cmps => {
                let a = self.read_string_src(bus, prefixes, address_size, width)?;
                let b = self.read_string_dst(bus, address_size, width)?;
                self.alu_sub(a, b, 0, width); // flags only: [DS:SI] - [ES:DI]
                self.adjust_index_register(6, address_size, bytes);
                self.adjust_index_register(7, address_size, bytes);
            }
            StringOp::Scas => {
                let a = self.acc_read(width);
                let b = self.read_string_dst(bus, address_size, width)?;
                self.alu_sub(a, b, 0, width); // flags only: accumulator - [ES:DI]
                self.adjust_index_register(7, address_size, bytes);
            }
            StringOp::Stos => {
                let value = self.acc_read(width);
                self.write_string_dst(bus, address_size, width, value)?;
                self.adjust_index_register(7, address_size, bytes);
            }
            StringOp::Lods => {
                let value = self.read_string_src(bus, prefixes, address_size, width)?;
                self.acc_write(width, value);
                self.adjust_index_register(6, address_size, bytes);
            }
        }
        Ok(())
    }

    fn run_string<B: CpuBus>(
        &mut self,
        bus: &mut B,
        op: StringOp,
        width: BusWidth,
        prefixes: Prefixes,
        address_size: AddressSize,
    ) -> ExecResult<()> {
        match prefixes.rep {
            None => self.string_step(bus, op, width, prefixes, address_size)?,
            Some(kind) => loop {
                if self.string_count(address_size) == 0 {
                    break;
                }
                self.string_step(bus, op, width, prefixes, address_size)?;
                self.decrement_string_count(address_size);
                // CMPS/SCAS also end the repeat on the ZF condition. REPE continues while
                // ZF is set; REPNE continues while ZF is clear. MOVS/STOS/LODS ignore ZF.
                if matches!(op, StringOp::Cmps | StringOp::Scas) {
                    let zf = self.flag(FLAG_ZF);
                    let again = match kind {
                        RepKind::Repe => zf,
                        RepKind::Repne => !zf,
                    };
                    if !again {
                        break;
                    }
                }
            },
        }
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

    fn load_flags(&mut self, value: u32, operand_size: OperandSize) {
        match operand_size {
            OperandSize::Word => {
                self.registers.eflags =
                    (self.registers.eflags & 0xffff_0000) | (value & 0xffff) | 0x2;
            }
            OperandSize::Dword => {
                self.registers.eflags = value | 0x2;
            }
        }
    }

    fn iret<B: CpuBus>(&mut self, bus: &mut B, operand_size: OperandSize) -> ExecResult<()> {
        match operand_size {
            OperandSize::Word => {
                let ip = self.pop(bus, OperandSize::Word)?;
                let cs = self.pop(bus, OperandSize::Word)? as u16;
                let flags = self.pop(bus, OperandSize::Word)?;
                self.load_segment(bus, SegmentIndex::Cs, cs)?;
                self.registers.eip = ip & 0xffff;
                self.load_flags(flags, OperandSize::Word);
            }
            OperandSize::Dword => {
                let eip = self.pop(bus, OperandSize::Dword)?;
                let cs = self.pop(bus, OperandSize::Dword)? as u16;
                let flags = self.pop(bus, OperandSize::Dword)?;
                self.load_segment(bus, SegmentIndex::Cs, cs)?;
                self.registers.eip = eip;
                self.load_flags(flags, OperandSize::Dword);
            }
        }
        Ok(())
    }

    fn far_call<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
        offset: u32,
        operand_size: OperandSize,
    ) -> ExecResult<()> {
        // Push CS first (higher stack address), then the return offset (lower
        // address). RETF pops in the opposite order: offset first, then CS.
        // self.registers.eip already points past the instruction, because the
        // far pointer operands have been fetched.
        self.push(bus, u32::from(self.registers.cs().selector), operand_size)?;
        self.push(bus, self.registers.eip, operand_size)?;
        self.load_segment(bus, SegmentIndex::Cs, selector)?;
        self.registers.eip = offset & operand_size.mask();
        Ok(())
    }

    fn return_far<B: CpuBus>(&mut self, bus: &mut B, operand_size: OperandSize) -> ExecResult<()> {
        // Pop the offset then CS, mirroring iret's pop order. On the 32-bit form CS
        // occupies four stack bytes; the high two are discarded on load.
        let offset = self.pop(bus, operand_size)?;
        let selector = self.pop(bus, operand_size)? as u16;
        self.load_segment(bus, SegmentIndex::Cs, selector)?;
        self.registers.eip = offset & operand_size.mask();
        Ok(())
    }

    fn release_stack(&mut self, count: u16) {
        // The immediate return forms release `count` bytes of arguments after the
        // pop. The stack pointer width follows the stack, not the operand size: a
        // real-mode 16-bit stack moves only SP and preserves ESP[31:16]. The SS
        // B-bit-accurate version arrives with 32-bit protected-mode stacks.
        if self.is_protected_mode() {
            let esp = self.registers.esp().wrapping_add(u32::from(count));
            self.registers.set_esp(esp);
        } else {
            let sp = self.read_gpr16(4).wrapping_add(count);
            self.write_gpr16(4, sp);
        }
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

    fn double_shift(
        &mut self,
        left: bool,
        dest: u32,
        src: u32,
        raw_count: u8,
        operand_size: OperandSize,
    ) -> u32 {
        // The 386 masks the count to 5 bits. A count of 0 is a no-op that touches no flags.
        let count = u32::from(raw_count) & 0x1f;
        if count == 0 {
            return dest;
        }
        let bits = operand_size.bytes() * 8;
        let mask = operand_size.mask();
        let msb = 1u32 << (bits - 1);
        let mut d = dest & mask;
        let mut s = src & mask;
        // A masked count past the operand width is undefined per Intel, but the 386
        // leaves the destination as the source rotated by the count modulo the width:
        // SHLD rotates left, SHRD rotates right. A 5-bit count never exceeds a 32-bit
        // width, so this only applies to the 16-bit forms. The flags are undefined here
        // and the conformance harness masks them. Derived from the SingleStepTests
        // vectors.
        if count > bits {
            let n = count % bits;
            let result = if left {
                ((s << n) | (s >> (bits - n))) & mask
            } else {
                ((s >> n) | (s << (bits - n))) & mask
            };
            self.set_szp(result, operand_size.bus_width());
            return result;
        }
        // A nonzero count always overwrites cf on the first iteration; unlike the
        // rotate-through-carry shifts there is no carry-in to seed.
        let mut cf = false;
        for _ in 0..count {
            if left {
                // SHLD: dest shifts left, the vacated low bit takes src's high bit.
                cf = d & msb != 0;
                d = ((d << 1) | ((s & msb) >> (bits - 1))) & mask;
                s = (s << 1) & mask;
            } else {
                // SHRD: dest shifts right, the vacated high bit takes src's low bit.
                cf = d & 1 != 0;
                d = (d >> 1) | ((s & 1) << (bits - 1));
                s >>= 1;
            }
        }
        self.set_flag(FLAG_CF, cf);
        if count == 1 {
            // OF is defined only for a single-bit count: set when the sign bit changed.
            self.set_flag(FLAG_OF, (dest ^ d) & msb != 0);
        }
        self.set_szp(d, operand_size.bus_width());
        d & mask
    }

    fn shift_rotate(&mut self, op: u8, value: u32, raw_count: u8, width: BusWidth) -> u32 {
        // The 386 masks the count to 5 bits, then performs that many
        // single-bit steps. A single-bit loop (<=31 iterations) matches silicon
        // step for step and avoids every closed-form edge case (a `>> bits` shift
        // at a full rotation, the RCL/RCR rotate-through-carry modulus). Switch to
        // a closed form only if this ever shows up on a profile.
        let count = u32::from(raw_count) & 0x1f;
        if count == 0 {
            return value; // a zero count affects no flags at all
        }
        let mask = width_mask(width);
        let msb = width_sign(width);
        let bits = width.bytes() * 8;
        let mut v = value & mask;
        let mut cf = self.flag(FLAG_CF); // seed for RCL/RCR
        for _ in 0..count {
            match op {
                0 => {
                    // ROL
                    let bit = (v & msb) != 0;
                    v = ((v << 1) | u32::from(bit)) & mask;
                    cf = bit;
                }
                1 => {
                    // ROR
                    let bit = (v & 1) != 0;
                    v = (v >> 1) | (u32::from(bit) << (bits - 1));
                    cf = bit;
                }
                2 => {
                    // RCL (rotate left through carry)
                    let bit = (v & msb) != 0;
                    v = ((v << 1) | u32::from(cf)) & mask;
                    cf = bit;
                }
                3 => {
                    // RCR (rotate right through carry)
                    let bit = (v & 1) != 0;
                    v = (v >> 1) | (u32::from(cf) << (bits - 1));
                    cf = bit;
                }
                4 | 6 => {
                    // SHL (/6 aliases SHL)
                    cf = (v & msb) != 0;
                    v = (v << 1) & mask;
                }
                5 => {
                    // SHR (logical)
                    cf = (v & 1) != 0;
                    v >>= 1;
                }
                7 => {
                    // SAR (arithmetic, sign preserved)
                    cf = (v & 1) != 0;
                    v = (v >> 1) | (v & msb);
                }
                _ => unreachable!("shift/rotate op {op}"),
            }
        }
        self.set_flag(FLAG_CF, cf);
        if count == 1 {
            // OF is defined only for a single-bit count.
            let top = (v & msb) != 0;
            let of = match op {
                0 | 2 => top ^ cf,                      // ROL, RCL: top bit XOR carry out
                1 | 3 => top ^ ((v & (msb >> 1)) != 0), // ROR, RCR: top two bits XORed
                4 | 6 => top ^ cf,                      // SHL: top bit of result XOR carry out
                5 => (value & msb) != 0,                // SHR: most-significant bit of the original
                7 => false,                             // SAR never overflows
                _ => unreachable!("shift/rotate op {op}"),
            };
            self.set_flag(FLAG_OF, of);
        }
        // Shifts set SF/ZF/PF; rotates leave them (and AF) untouched.
        if matches!(op, 4..=7) {
            self.set_szp(v, width);
        }
        v & mask
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

    fn mul(&mut self, operand: u32, signed: bool, width: BusWidth) {
        // Multiply the implicit accumulator (AL/AX/EAX) by the operand and store the
        // wide product split across AH:AL / DX:AX / EDX:EAX. CF and OF are set when the
        // high half is significant (unsigned: nonzero; signed: not the sign extension
        // of the low half); SF/ZF/AF/PF are left untouched (undefined on the 386).
        let significant = match (width, signed) {
            (BusWidth::Byte, false) => {
                let product = u16::from(self.read_gpr8(0)) * u16::from(operand as u8);
                self.write_gpr16(0, product);
                product & 0xff00 != 0
            }
            (BusWidth::Byte, true) => {
                let product = i16::from(self.read_gpr8(0) as i8) * i16::from(operand as u8 as i8);
                self.write_gpr16(0, product as u16);
                product != i16::from(product as u8 as i8)
            }
            (BusWidth::Word, false) => {
                let product = u32::from(self.read_gpr16(0)) * u32::from(operand as u16);
                self.write_gpr16(0, product as u16);
                self.write_gpr16(2, (product >> 16) as u16);
                product >> 16 != 0
            }
            (BusWidth::Word, true) => {
                let product =
                    i32::from(self.read_gpr16(0) as i16) * i32::from(operand as u16 as i16);
                self.write_gpr16(0, product as u16);
                self.write_gpr16(2, (product >> 16) as u16);
                product != i32::from(product as u16 as i16)
            }
            (BusWidth::Dword, false) => {
                let product = u64::from(self.read_gpr32(0)) * u64::from(operand);
                self.write_gpr32(0, product as u32);
                self.write_gpr32(2, (product >> 32) as u32);
                product >> 32 != 0
            }
            (BusWidth::Dword, true) => {
                let product = i64::from(self.read_gpr32(0) as i32) * i64::from(operand as i32);
                self.write_gpr32(0, product as u32);
                self.write_gpr32(2, (product >> 32) as u32);
                product != i64::from(product as u32 as i32)
            }
        };
        self.set_flag(FLAG_CF | FLAG_OF, significant);
    }

    fn imul_truncated(&mut self, a: u32, b: u32, operand_size: OperandSize) -> u32 {
        // Two-operand signed multiply: the low-half product truncated to the operand size.
        // CF/OF are set when the full product does not sign-extend back from the truncation
        // (the result does not fit). SF/ZF/AF/PF are left undefined, matching the 386.
        let (result, significant) = match operand_size {
            OperandSize::Word => {
                let p = i32::from(a as u16 as i16) * i32::from(b as u16 as i16);
                (p as u16 as u32, p != i32::from(p as u16 as i16))
            }
            OperandSize::Dword => {
                let p = i64::from(a as i32) * i64::from(b as i32);
                (p as u32, p != i64::from(p as u32 as i32))
            }
        };
        self.set_flag(FLAG_CF | FLAG_OF, significant);
        result
    }

    fn div(&mut self, operand: u32, signed: bool, width: BusWidth) -> ExecResult<()> {
        // Divide the implicit dividend (AX / DX:AX / EDX:EAX) by the operand, writing the
        // quotient to AL/AX/EAX and the remainder to AH/DX/EDX. Divide-by-zero and
        // quotient overflow are checked BEFORE any register write and return DivideError
        // (real-mode #DE delivery is deferred). Arithmetic flags are left undefined.
        if operand & width_mask(width) == 0 {
            return Err(CpuError::DivideError.into());
        }
        match (width, signed) {
            (BusWidth::Byte, false) => {
                let dividend = u32::from(self.read_gpr16(0));
                let divisor = u32::from(operand as u8);
                let quotient = dividend / divisor;
                if quotient > 0xff {
                    return Err(CpuError::DivideError.into());
                }
                self.write_gpr8(0, quotient as u8);
                self.write_gpr8(4, (dividend % divisor) as u8);
            }
            (BusWidth::Byte, true) => {
                let dividend = i32::from(self.read_gpr16(0) as i16);
                let divisor = i32::from(operand as u8 as i8);
                let (Some(quotient), Some(remainder)) =
                    (dividend.checked_div(divisor), dividend.checked_rem(divisor))
                else {
                    return Err(CpuError::DivideError.into());
                };
                if !(i32::from(i8::MIN)..=i32::from(i8::MAX)).contains(&quotient) {
                    return Err(CpuError::DivideError.into());
                }
                self.write_gpr8(0, quotient as u8);
                self.write_gpr8(4, remainder as u8);
            }
            (BusWidth::Word, false) => {
                let dividend =
                    (u32::from(self.read_gpr16(2)) << 16) | u32::from(self.read_gpr16(0));
                let divisor = u32::from(operand as u16);
                let quotient = dividend / divisor;
                if quotient > 0xffff {
                    return Err(CpuError::DivideError.into());
                }
                self.write_gpr16(0, quotient as u16);
                self.write_gpr16(2, (dividend % divisor) as u16);
            }
            (BusWidth::Word, true) => {
                let dividend =
                    ((u32::from(self.read_gpr16(2)) << 16) | u32::from(self.read_gpr16(0))) as i32;
                let divisor = i32::from(operand as u16 as i16);
                let (Some(quotient), Some(remainder)) =
                    (dividend.checked_div(divisor), dividend.checked_rem(divisor))
                else {
                    return Err(CpuError::DivideError.into());
                };
                if !(i32::from(i16::MIN)..=i32::from(i16::MAX)).contains(&quotient) {
                    return Err(CpuError::DivideError.into());
                }
                self.write_gpr16(0, quotient as u16);
                self.write_gpr16(2, remainder as u16);
            }
            (BusWidth::Dword, false) => {
                let dividend =
                    (u64::from(self.read_gpr32(2)) << 32) | u64::from(self.read_gpr32(0));
                let divisor = u64::from(operand);
                let quotient = dividend / divisor;
                if quotient > 0xffff_ffff {
                    return Err(CpuError::DivideError.into());
                }
                self.write_gpr32(0, quotient as u32);
                self.write_gpr32(2, (dividend % divisor) as u32);
            }
            (BusWidth::Dword, true) => {
                let dividend =
                    ((u64::from(self.read_gpr32(2)) << 32) | u64::from(self.read_gpr32(0))) as i64;
                let divisor = i64::from(operand as i32);
                let (Some(quotient), Some(remainder)) =
                    (dividend.checked_div(divisor), dividend.checked_rem(divisor))
                else {
                    return Err(CpuError::DivideError.into());
                };
                if !(i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&quotient) {
                    return Err(CpuError::DivideError.into());
                }
                self.write_gpr32(0, quotient as u32);
                self.write_gpr32(2, remainder as u32);
            }
        }
        Ok(())
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
    use izarravm_bus::{BusCycle, BusTrace, BusWidth};

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
    fn rep_stosb_fills_es_di() {
        // rep stosb (0xf3 0xaa), cx=3, al=0xee. Fills 3 bytes at es:di, cx -> 0, di += 3.
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0xf3, 0xaa]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_DF, false);
        cpu.write_gpr8(0, 0xee);
        cpu.registers.set_edi(0x300);
        cpu.registers.set_ecx(3);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(&bus.memory[0x300..0x303], &[0xee, 0xee, 0xee]);
        assert_eq!(cpu.registers.edi(), 0x303);
        assert_eq!(cpu.registers.ecx(), 0);
    }

    #[test]
    fn lodsw_loads_ax_and_advances_si() {
        // lodsw (0xad). [ds:si]=0x1234 (LE) -> ax; si += 2.
        let mut memory = vec![0; 1024];
        memory[0] = 0xad;
        memory[0x100..0x102].copy_from_slice(&0x1234u16.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_DF, false);
        cpu.registers.set_esi(0x100);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1234);
        assert_eq!(cpu.registers.esi(), 0x102);
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
    fn inc_word_memory_via_ff_group() {
        // inc word [bx]  (0xff /0, modrm 0x07). 0x00ff -> 0x0100.
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0xff, 0x07]);
        memory[0x200..0x202].copy_from_slice(&0x00ffu16.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x200);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(
            u16::from_le_bytes([bus.memory[0x200], bus.memory[0x201]]),
            0x0100
        );
        assert!(!cpu.flag(FLAG_ZF));
    }

    #[test]
    fn call_near_indirect_register_pushes_return_and_jumps() {
        // call ax  (0xff /2, modrm 0xd0). Pushes return eip (2), jumps to ax.
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0xff, 0xd0]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Sp, 0x100);
        cpu.write_reg16(Reg16::Ax, 0x0050);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eip, 0x0050);
        assert_eq!(cpu.read_reg16(Reg16::Sp), 0x00fe);
        assert_eq!(
            u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
            0x0002
        );
    }

    #[test]
    fn jmp_near_indirect_sets_eip_without_push() {
        // jmp bx  (0xff /4, modrm 0xe3).
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0xff, 0xe3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Sp, 0x100);
        cpu.write_reg16(Reg16::Bx, 0x0030);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eip, 0x0030);
        assert_eq!(cpu.read_reg16(Reg16::Sp), 0x0100); // no push
    }

    #[test]
    fn push_rm_writes_value_and_decrements_sp() {
        // push cx  (0xff /6, modrm 0xf1).
        let mut memory = vec![0; 256];
        memory[0..2].copy_from_slice(&[0xff, 0xf1]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Sp, 0x100);
        cpu.write_reg16(Reg16::Cx, 0xbeef);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Sp), 0x00fe);
        assert_eq!(
            u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
            0xbeef
        );
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

    #[test]
    fn incdec_preserve_carry_both_directions() {
        // DEC with CF set leaves CF set.
        let mut memory = vec![0; 16];
        memory[0] = 0x48; // dec ax
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_CF, true);
        cpu.write_reg16(Reg16::Ax, 0x0005);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0004);
        assert!(cpu.flag(FLAG_CF));

        // INC with CF clear leaves CF clear.
        let mut memory = vec![0; 16];
        memory[0] = 0x40; // inc ax
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_CF, false);
        cpu.write_reg16(Reg16::Ax, 0x0005);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0006);
        assert!(!cpu.flag(FLAG_CF));
    }

    #[test]
    fn dec_word_overflow_sets_of() {
        // dec ax (0x48) on 0x8000 -> 0x7fff: OF set, SF clear.
        let mut memory = vec![0; 16];
        memory[0] = 0x48;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x8000);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x7fff);
        assert!(cpu.flag(FLAG_OF));
        assert!(!cpu.flag(FLAG_SF));
    }

    #[test]
    fn call_near_indirect_memory_displacement_return_addr() {
        // call [bx+0x10] (0xff /2, modrm 0x57, disp 0x10): 3-byte instruction,
        // return address must be computed after the displacement fetch.
        let mut memory = vec![0; 1024];
        memory[0..3].copy_from_slice(&[0xff, 0x57, 0x10]);
        memory[0x210..0x212].copy_from_slice(&0x0080u16.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Sp, 0x100);
        cpu.write_reg16(Reg16::Bx, 0x200);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.eip, 0x0080);
        assert_eq!(
            u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
            0x0003
        );
    }

    #[test]
    fn push_sp_uses_pre_decrement_value() {
        // push sp (0xff /6, modrm 0xf4): the 386 pushes SP before the decrement.
        let mut memory = vec![0; 256];
        memory[0..2].copy_from_slice(&[0xff, 0xf4]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Sp, 0x100);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.read_reg16(Reg16::Sp), 0x00fe);
        assert_eq!(
            u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
            0x0100
        );
    }

    #[test]
    fn inc_dword_uses_32bit_width() {
        // 0x66 0x40 = inc eax (32-bit operand): 0x0000ffff -> 0x00010000.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0x66, 0x40]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_eax(0x0000_ffff);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.eax(), 0x0001_0000);
        assert!(!cpu.flag(FLAG_ZF));
        assert!(!cpu.flag(FLAG_SF));
    }

    #[test]
    fn shl_word_by_one_sets_of_and_clears_cf() {
        // shl ax,1 (0xd1 /4, modrm 0xe0). 0x4000 -> 0x8000, CF=0 (old bit15), OF=1, SF=1.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xd1, 0xe0]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x4000);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
        assert!(!cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_OF));
        assert!(cpu.flag(FLAG_SF));
    }

    #[test]
    fn shr_word_by_one_sets_cf_and_of() {
        // shr ax,1 (0xd1 /5, modrm 0xe8). 0x8001 -> 0x4000, CF=1, OF=msb(orig)=1.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xd1, 0xe8]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x8001);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x4000);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_OF));
        assert!(!cpu.flag(FLAG_SF)); // result 0x4000 is positive
    }

    #[test]
    fn shl_dword_by_one_via_operand_size_prefix() {
        // shl eax,1 (0x66 0xd1 /4, modrm 0xe0). 0x4000_0000 -> 0x8000_0000, CF=0, OF=1, SF=1.
        let mut memory = vec![0; 16];
        memory[0..3].copy_from_slice(&[0x66, 0xd1, 0xe0]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_eax(0x4000_0000);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eax(), 0x8000_0000);
        assert!(!cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_OF));
        assert!(cpu.flag(FLAG_SF));
        assert_eq!(cpu.registers.eip, 3); // prefix + opcode + modrm
    }

    #[test]
    fn sar_word_by_one_preserves_sign_and_clears_of() {
        // sar ax,1 (0xd1 /7, modrm 0xf8). 0x8001 -> 0xc000, CF=1, OF=0, SF=1.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xd1, 0xf8]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x8001);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0xc000);
        assert!(cpu.flag(FLAG_CF));
        assert!(!cpu.flag(FLAG_OF));
        assert!(cpu.flag(FLAG_SF));
    }

    #[test]
    fn shl_byte_via_c0_imm_only_touches_low_byte() {
        // shl al,1 (0xc0 /4, modrm 0xe0, imm 0x01). ax=0xff81 -> al 0x81<<1=0x02, ah preserved.
        let mut memory = vec![0; 16];
        memory[0..3].copy_from_slice(&[0xc0, 0xe0, 0x01]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0xff81);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0xff02);
        assert!(cpu.flag(FLAG_CF)); // old bit7 of 0x81
        assert_eq!(cpu.registers.eip, 3); // opcode + modrm + imm8
    }

    #[test]
    fn shl_word_by_imm_count() {
        // shl ax,4 (0xc1 /4, modrm 0xe0, imm 0x04). 0x0001 -> 0x0010.
        let mut memory = vec![0; 16];
        memory[0..3].copy_from_slice(&[0xc1, 0xe0, 0x04]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0001);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0010);
        assert_eq!(cpu.registers.eip, 3);
    }

    #[test]
    fn shift_count_masked_to_five_bits() {
        // shl ax,cl with cl=33 (0xd3 /4, modrm 0xe0). 33 & 0x1f == 1, so one shift.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xd3, 0xe0]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x4000);
        cpu.write_reg16(Reg16::Cx, 33);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
    }

    #[test]
    fn shift_count_zero_touches_no_flags() {
        // shl ax,cl with cl=32 (0xd3 /4). 32 & 0x1f == 0: operand and flags unchanged.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xd3, 0xe0]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x1234);
        cpu.write_reg16(Reg16::Cx, 32);
        cpu.set_flag(FLAG_CF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1234);
        assert!(cpu.flag(FLAG_CF)); // unchanged: a zero count touches no flags
    }

    #[test]
    fn rol_word_by_one() {
        // rol ax,1 (0xd1 /0, modrm 0xc0). 0x8000 -> 0x0001, CF=1, OF=msb^cf=0^1=1.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xd1, 0xc0]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x8000);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0001);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_OF));
    }

    #[test]
    fn ror_word_by_one() {
        // ror ax,1 (0xd1 /1, modrm 0xc8). 0x0001 -> 0x8000, CF=1, OF=msb^next=1^0=1.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xd1, 0xc8]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0001);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_OF));
    }

    #[test]
    fn rcl_word_rotates_through_carry() {
        // rcl ax,1 (0xd1 /2, modrm 0xd0). ax=0x0000, CF=1 -> 0x0001, CF=0 (old msb=0).
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xd1, 0xd0]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0000);
        cpu.set_flag(FLAG_CF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0001); // carry rotated into bit 0
        assert!(!cpu.flag(FLAG_CF)); // old msb (0) rotated out
        assert!(!cpu.flag(FLAG_OF)); // result_msb(0) ^ cf(0)
    }

    #[test]
    fn rcr_word_rotates_through_carry() {
        // rcr ax,1 (0xd1 /3, modrm 0xd8). ax=0x0000, CF=1 -> 0x8000, CF=0 (old bit0=0).
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xd1, 0xd8]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0000);
        cpu.set_flag(FLAG_CF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000); // carry rotated into bit 15
        assert!(!cpu.flag(FLAG_CF)); // old bit0 (0) rotated out
        assert!(cpu.flag(FLAG_OF)); // result_msb(1) ^ result_bit14(0)
    }

    #[test]
    fn rotate_leaves_sign_zero_parity_untouched() {
        // rol ax,1: rotates touch only CF/OF, never SF/ZF/PF. Set ZF first, then
        // rotate to a nonzero result and confirm ZF survives.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xd1, 0xc0]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x8000);
        cpu.set_flag(FLAG_ZF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0001);
        assert!(cpu.flag(FLAG_ZF)); // unchanged by a rotate
    }

    #[test]
    fn ror_byte_by_cl_multi_bit() {
        // ror al,cl with cl=3 (0xd2 /1, modrm 0xc8). Exercises the byte width
        // (msb 0x80, shift by bits-1=7) and a multi-bit count. al 0x01 ror 3 = 0x20.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xd2, 0xc8]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0001);
        cpu.write_reg16(Reg16::Cx, 3);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0020); // ah preserved, al rotated
        assert!(!cpu.flag(FLAG_CF)); // last bit out is 0
    }

    #[test]
    fn not_byte_leaves_flags_untouched() {
        // not bl (0xf6 /2, modrm 0xd3). 0x0f -> 0xf0; NOT affects no flags.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xf6, 0xd3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x000f);
        cpu.set_flag(FLAG_CF, true);
        cpu.set_flag(FLAG_ZF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Bx) & 0xff, 0xf0);
        assert!(cpu.flag(FLAG_CF)); // unchanged
        assert!(cpu.flag(FLAG_ZF)); // unchanged
    }

    #[test]
    fn neg_byte_sets_carry_and_sign() {
        // neg bl (0xf6 /3, modrm 0xdb). 0x01 -> 0xff; CF set, SF set, ZF clear.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xf6, 0xdb]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x0001);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Bx) & 0xff, 0xff);
        assert!(cpu.flag(FLAG_CF)); // operand nonzero
        assert!(cpu.flag(FLAG_SF));
        assert!(!cpu.flag(FLAG_ZF));
    }

    #[test]
    fn neg_zero_clears_carry_and_sets_zero() {
        // neg bl of 0x00 -> 0x00; CF clear, ZF set.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xf6, 0xdb]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x0000);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Bx) & 0xff, 0x00);
        assert!(!cpu.flag(FLAG_CF)); // operand zero
        assert!(cpu.flag(FLAG_ZF));
    }

    #[test]
    fn neg_byte_overflow_at_0x80() {
        // neg bl of 0x80 -> 0x80; OF set (only value that negates to itself), CF and SF set.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xf6, 0xdb]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x0080);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Bx) & 0xff, 0x80);
        assert!(cpu.flag(FLAG_OF));
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_SF));
    }

    #[test]
    fn not_word_via_f7_complements() {
        // not bx (0xf7 /2, modrm 0xd3). 0x0ff0 -> 0xf00f; flags unchanged.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xf7, 0xd3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x0ff0);
        cpu.set_flag(FLAG_CF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Bx), 0xf00f);
        assert!(cpu.flag(FLAG_CF)); // NOT touches no flags
    }

    #[test]
    fn mul_byte_sets_carry_when_high_nonzero() {
        // mul bl (0xf6 /4, modrm 0xe3). al=0x10, bl=0x10 -> ax=0x0100; CF/OF set (ah != 0).
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xf6, 0xe3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0010);
        cpu.write_reg16(Reg16::Bx, 0x0010);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0100);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_OF));
    }

    #[test]
    fn mul_byte_clears_carry_when_high_zero() {
        // mul bl. al=0x05, bl=0x03 -> ax=0x000f; CF/OF clear.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xf6, 0xe3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0005);
        cpu.write_reg16(Reg16::Bx, 0x0003);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x000f);
        assert!(!cpu.flag(FLAG_CF));
        assert!(!cpu.flag(FLAG_OF));
    }

    #[test]
    fn mul_word_writes_dx_ax_preserving_high_halves() {
        // mul bx (0xf7 /4, modrm 0xe3). ax=0x1000, bx=0x0010 -> product 0x0010_0000:
        // ax=0x0000, dx=0x0001; CF/OF set. High 16 bits of EAX/EDX must survive.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xf7, 0xe3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_eax(0xaaaa_1000);
        cpu.registers.set_edx(0xbbbb_0000);
        cpu.registers.set_ebx(0x0000_0010);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eax(), 0xaaaa_0000); // ax=0, high preserved
        assert_eq!(cpu.registers.edx(), 0xbbbb_0001); // dx=1, high preserved
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_OF));
    }

    #[test]
    fn imul_byte_clears_carry_when_result_fits() {
        // imul bl (0xf6 /5, modrm 0xeb). al=0xff(-1), bl=0x02(+2) -> ax=0xfffe(-2);
        // CF/OF clear because the high half is the sign extension of the low half.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xf6, 0xeb]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x00ff);
        cpu.write_reg16(Reg16::Bx, 0x0002);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0xfffe);
        assert!(!cpu.flag(FLAG_CF));
        assert!(!cpu.flag(FLAG_OF));
    }

    #[test]
    fn imul_byte_sets_carry_when_result_overflows() {
        // imul bl. al=0x10(+16), bl=0x10(+16) -> ax=0x0100(+256); the low byte is 0x00,
        // its sign extension is 0x0000 != 0x0100, so CF/OF set.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xf6, 0xeb]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0010);
        cpu.write_reg16(Reg16::Bx, 0x0010);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0100);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_OF));
    }

    #[test]
    fn mul_dword_writes_edx_eax() {
        // mul ebx (0x66 0xf7 /4, modrm 0xe3). eax=0x0001_0000 * ebx=0x0001_0000
        // = 0x1_0000_0000 -> eax=0, edx=1; CF/OF set. Exercises the u64 dword path.
        let mut memory = vec![0; 16];
        memory[0..3].copy_from_slice(&[0x66, 0xf7, 0xe3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_eax(0x0001_0000);
        cpu.registers.set_ebx(0x0001_0000);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eax(), 0x0000_0000);
        assert_eq!(cpu.registers.edx(), 0x0000_0001);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_OF));
    }

    #[test]
    fn div_byte_writes_quotient_and_remainder() {
        // div bl (0xf6 /6, modrm 0xf3). ax=0x0011(17), bl=0x05 -> al=3, ah=2.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xf6, 0xf3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0011);
        cpu.write_reg16(Reg16::Bx, 0x0005);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0203); // ah=2 (rem), al=3 (quot)
    }

    #[test]
    fn div_word_writes_ax_and_dx() {
        // div bx (0xf7 /6, modrm 0xf3). dx:ax = 0x0000:0x0011 (17), bx=5 -> ax=3 (quot), dx=2 (rem).
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xf7, 0xf3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Dx, 0x0000);
        cpu.write_reg16(Reg16::Ax, 0x0011);
        cpu.write_reg16(Reg16::Bx, 0x0005);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0003);
        assert_eq!(cpu.read_reg16(Reg16::Dx), 0x0002);
    }

    #[test]
    fn idiv_byte_negative_dividend_truncates_toward_zero() {
        // idiv bl (0xf6 /7, modrm 0xfb). ax=-17=0xffef, bl=+5 -> quot=-3 (0xfd), rem=-2 (0xfe).
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xf6, 0xfb]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0xffef);
        cpu.write_reg16(Reg16::Bx, 0x0005);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0xfd); // al = -3
        assert_eq!((cpu.read_reg16(Reg16::Ax) >> 8) & 0xff, 0xfe); // ah = -2
    }

    #[test]
    fn div_by_zero_returns_error_without_writes() {
        // div bl with bl=0 -> DivideError; ax unchanged.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xf6, 0xf3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x1234);
        cpu.write_reg16(Reg16::Bx, 0x0000);
        let mut bus = TestBus::with_memory(memory);

        assert_eq!(cpu.cycle(&mut bus).unwrap_err(), CpuError::DivideError);
        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1234); // no writes
    }

    #[test]
    fn div_quotient_overflow_returns_error() {
        // div bl: ax=0xffff, bl=0x01 -> quotient 0xffff > 0xff -> DivideError.
        let mut memory = vec![0; 16];
        memory[0..2].copy_from_slice(&[0xf6, 0xf3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0xffff);
        cpu.write_reg16(Reg16::Bx, 0x0001);
        let mut bus = TestBus::with_memory(memory);

        assert_eq!(cpu.cycle(&mut bus).unwrap_err(), CpuError::DivideError);
    }

    #[test]
    fn div_dword_writes_eax_edx() {
        // div ebx (0x66 0xf7 /6, modrm 0xf3). edx:eax = 0x1_0000_0005, ebx=2
        // -> quot=0x8000_0002, rem=1. Exercises the u64 dword path.
        let mut memory = vec![0; 16];
        memory[0..3].copy_from_slice(&[0x66, 0xf7, 0xf3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_edx(0x0000_0001);
        cpu.registers.set_eax(0x0000_0005);
        cpu.registers.set_ebx(0x0000_0002);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eax(), 0x8000_0002); // quotient
        assert_eq!(cpu.registers.edx(), 0x0000_0001); // remainder
    }

    #[test]
    fn idiv_dword_min_over_negative_one_is_divide_error() {
        // idiv ebx (0x66 0xf7 /7, modrm 0xfb). edx:eax = i64::MIN, ebx = -1.
        // checked_div catches the overflow so this is #DE, not a panic.
        let mut memory = vec![0; 16];
        memory[0..3].copy_from_slice(&[0x66, 0xf7, 0xfb]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_edx(0x8000_0000);
        cpu.registers.set_eax(0x0000_0000);
        cpu.registers.set_ebx(0xffff_ffff);
        let mut bus = TestBus::with_memory(memory);

        assert_eq!(cpu.cycle(&mut bus).unwrap_err(), CpuError::DivideError);
    }

    #[test]
    fn movsb_copies_and_increments_when_df_clear() {
        // movsb (0xa4). [ds:si]=0x42 -> [es:di]; si and di increment (DF=0).
        let mut memory = vec![0; 1024];
        memory[0] = 0xa4;
        memory[0x100] = 0x42;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_DF, false);
        cpu.registers.set_esi(0x100);
        cpu.registers.set_edi(0x200);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(bus.memory[0x200], 0x42);
        assert_eq!(cpu.registers.esi(), 0x101);
        assert_eq!(cpu.registers.edi(), 0x201);
    }

    #[test]
    fn movsb_decrements_when_df_set() {
        // movsb with DF=1: si and di decrement.
        let mut memory = vec![0; 1024];
        memory[0] = 0xa4;
        memory[0x100] = 0x42;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_DF, true);
        cpu.registers.set_esi(0x100);
        cpu.registers.set_edi(0x200);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(bus.memory[0x200], 0x42);
        assert_eq!(cpu.registers.esi(), 0x0ff);
        assert_eq!(cpu.registers.edi(), 0x1ff);
    }

    #[test]
    fn rep_movsb_copies_cx_bytes() {
        // rep movsb (0xf3 0xa4) with cx=3 copies 3 bytes, leaves cx=0, advances si/di by 3.
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0xf3, 0xa4]);
        memory[0x100..0x103].copy_from_slice(&[1, 2, 3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_DF, false);
        cpu.registers.set_esi(0x100);
        cpu.registers.set_edi(0x200);
        cpu.registers.set_ecx(3);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(&bus.memory[0x200..0x203], &[1, 2, 3]);
        assert_eq!(cpu.registers.esi(), 0x103);
        assert_eq!(cpu.registers.edi(), 0x203);
        assert_eq!(cpu.registers.ecx(), 0);
    }

    #[test]
    fn rep_movsb_with_zero_count_does_nothing() {
        // rep movsb with cx=0 performs no access and leaves si/di/cx unchanged.
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0xf3, 0xa4]);
        memory[0x100] = 0x42;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esi(0x100);
        cpu.registers.set_edi(0x200);
        cpu.registers.set_ecx(0);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(bus.memory[0x200], 0); // no write
        assert_eq!(cpu.registers.esi(), 0x100);
        assert_eq!(cpu.registers.edi(), 0x200);
        assert_eq!(cpu.registers.ecx(), 0);
    }

    #[test]
    fn cmpsb_equal_sets_zero_flag() {
        // cmpsb (0xa6). [ds:si]=0x55, [es:di]=0x55 -> equal, ZF set.
        let mut memory = vec![0; 1024];
        memory[0] = 0xa6;
        memory[0x100] = 0x55;
        memory[0x200] = 0x55;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_DF, false);
        cpu.registers.set_esi(0x100);
        cpu.registers.set_edi(0x200);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_ZF));
        assert_eq!(cpu.registers.esi(), 0x101);
        assert_eq!(cpu.registers.edi(), 0x201);
    }

    #[test]
    fn cmpsb_unequal_clears_zero_flag() {
        // cmpsb. [ds:si]=0x10, [es:di]=0x20 -> 0x10-0x20 borrows: ZF clear, CF set.
        let mut memory = vec![0; 1024];
        memory[0] = 0xa6;
        memory[0x100] = 0x10;
        memory[0x200] = 0x20;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_DF, false);
        cpu.registers.set_esi(0x100);
        cpu.registers.set_edi(0x200);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(!cpu.flag(FLAG_ZF));
        assert!(cpu.flag(FLAG_CF)); // 0x10 < 0x20
        assert_eq!(cpu.registers.esi(), 0x101); // si advances even when unequal
        assert_eq!(cpu.registers.edi(), 0x201);
    }

    #[test]
    fn scasb_compares_al_with_es_di() {
        // scasb (0xae). al=0x41, [es:di]=0x41 -> ZF set; di increments, si untouched.
        let mut memory = vec![0; 1024];
        memory[0] = 0xae;
        memory[0x200] = 0x41;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr8(0, 0x41);
        cpu.registers.set_esi(0x100);
        cpu.registers.set_edi(0x200);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_ZF));
        assert_eq!(cpu.registers.edi(), 0x201);
        assert_eq!(cpu.registers.esi(), 0x100); // SCAS does not touch SI
    }

    #[test]
    fn repe_cmpsb_stops_on_first_mismatch() {
        // repe cmpsb (0xf3 0xa6), cx=4. Source "AABB" vs dest "AACC": the third byte
        // (index 2) is the B/C mismatch, so the repeat stops there with ZF clear after
        // 3 iterations; cx counts 4 -> 3 -> 2 -> 1, si/di advance by 3.
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0xf3, 0xa6]);
        memory[0x100..0x104].copy_from_slice(b"AABB");
        memory[0x200..0x204].copy_from_slice(b"AACC");
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_DF, false);
        cpu.registers.set_esi(0x100);
        cpu.registers.set_edi(0x200);
        cpu.registers.set_ecx(4);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(!cpu.flag(FLAG_ZF)); // stopped on the index-2 mismatch (B != C)
        assert_eq!(cpu.registers.ecx(), 1); // 4 -> 3 -> 2 -> 1, then ZF clear stops
        assert_eq!(cpu.registers.esi(), 0x103);
        assert_eq!(cpu.registers.edi(), 0x203);
    }

    #[test]
    fn repne_scasb_stops_on_match() {
        // repne scasb (0xf2 0xae), cx=4, al='C'. Dest "AACA": scans until the match at
        // index 2, stopping with ZF set after 3 iterations; cx counts 4 -> 3 -> 2 -> 1,
        // di advances by 3.
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0xf2, 0xae]);
        memory[0x200..0x204].copy_from_slice(b"AACA");
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_DF, false);
        cpu.write_gpr8(0, b'C');
        cpu.registers.set_edi(0x200);
        cpu.registers.set_ecx(4);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_ZF)); // matched 'C' at index 2
        assert_eq!(cpu.registers.ecx(), 1); // 4 -> 3 -> 2 -> 1, match stops
        assert_eq!(cpu.registers.edi(), 0x203);
    }

    #[test]
    fn movsb_honors_source_segment_override() {
        // es: movsb (0x26 0xa4). With ds=0 and es base 0x200, the override reads the
        // source from es:si (0x210), not ds:si (0x10); the destination stays es:di (0x230).
        let mut memory = vec![0; 0x400];
        memory[0..2].copy_from_slice(&[0x26, 0xa4]);
        memory[0x210] = 0x99;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0x20); // base 0x200
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_DF, false);
        cpu.registers.set_esi(0x10);
        cpu.registers.set_edi(0x30);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(bus.memory[0x230], 0x99); // es:di destination
        assert_eq!(bus.memory[0x10], 0); // ds:si source was not used
    }

    #[test]
    fn lea_loads_effective_address() {
        // lea bx, [si+0x10]  (0x8d 0x5c 0x10). bx <- si + 0x10, no memory access:
        // the byte at the computed address must not be loaded.
        let mut memory = vec![0; 1024];
        memory[0..3].copy_from_slice(&[0x8d, 0x5c, 0x10]);
        memory[0x110] = 0x99;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esi(0x0100);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Bx), 0x0110);
    }

    #[test]
    fn lea_with_register_operand_delivers_ud() {
        // lea ax, ax  (0x8d 0xc0, mod=3) is an invalid encoding -> #UD (vector 6).
        // IVT[6] at 0x18 points to IP 0x00ee, CS 0; the CPU vectors there and clears IF.
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0x8d, 0xc0]);
        memory[0x18] = 0xee; // vector 6 IP low byte (IP = 0x00ee)
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        cpu.set_flag(FLAG_IF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eip, 0x00ee);
        assert!(!cpu.flag(FLAG_IF));
    }

    #[test]
    fn cbw_sign_extends_al_into_ax() {
        // cbw (0x98): al = 0x80 (-128) -> ax = 0xff80.
        let mut memory = vec![0; 64];
        memory[0] = 0x98;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr8(0, 0x80);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0xff80);
    }

    #[test]
    fn cwde_sign_extends_ax_into_eax() {
        // 0x66 0x98 (CWDE): ax = 0x8000 -> eax = 0xffff_8000.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0x66, 0x98]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_eax(0x0000_8000);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eax(), 0xffff_8000);
    }

    #[test]
    fn cwd_fills_dx_from_ax_sign() {
        // cwd (0x99): ax = 0x8000 (negative) -> dx = 0xffff, ax unchanged.
        let mut memory = vec![0; 64];
        memory[0] = 0x99;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x8000);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Dx), 0xffff);
        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
    }

    #[test]
    fn cwd_clears_dx_for_positive_ax() {
        // cwd (0x99): ax = 0x0001 (positive) -> dx = 0.
        let mut memory = vec![0; 64];
        memory[0] = 0x99;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0001);
        cpu.write_reg16(Reg16::Dx, 0xaaaa);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Dx), 0x0000);
    }

    #[test]
    fn cdq_fills_edx_from_eax_sign() {
        // 0x66 0x99 (CDQ): eax = 0x8000_0000 -> edx = 0xffff_ffff.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0x66, 0x99]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_eax(0x8000_0000);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.edx(), 0xffff_ffff);
    }

    #[test]
    fn sti_sets_interrupt_flag() {
        // sti (0xfb) sets IF.
        let mut memory = vec![0; 64];
        memory[0] = 0xfb;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_IF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_IF));
    }

    #[test]
    fn lahf_loads_flag_byte_into_ah() {
        // lahf (0x9f). CF=PF=AF=ZF=SF=1 -> AH = 0xD5 | 0x02 = 0xD7; AL unchanged.
        let mut memory = vec![0; 64];
        memory[0] = 0x9f;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0000);
        cpu.set_flag(FLAG_CF, true);
        cpu.set_flag(FLAG_PF, true);
        cpu.set_flag(FLAG_AF, true);
        cpu.set_flag(FLAG_ZF, true);
        cpu.set_flag(FLAG_SF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0xd7);
        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x00);
    }

    #[test]
    fn sahf_loads_flags_from_ah_leaving_overflow() {
        // sahf (0x9e). AH=0xD7 -> CF=PF=AF=ZF=SF=1; OF untouched (a set OF survives).
        let mut memory = vec![0; 64];
        memory[0] = 0x9e;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0xd700); // AH=0xD7, AL=0
        cpu.set_flag(FLAG_CF, false);
        cpu.set_flag(FLAG_PF, false);
        cpu.set_flag(FLAG_AF, false);
        cpu.set_flag(FLAG_ZF, false);
        cpu.set_flag(FLAG_SF, false);
        cpu.set_flag(FLAG_OF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_PF));
        assert!(cpu.flag(FLAG_AF));
        assert!(cpu.flag(FLAG_ZF));
        assert!(cpu.flag(FLAG_SF));
        assert!(cpu.flag(FLAG_OF));
    }

    #[test]
    fn stc_sets_carry_and_cmc_toggles_it() {
        // stc (0xf9) sets CF; cmc (0xf5) toggles it back to 0.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0xf9, 0xf5]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_CF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap(); // stc
        assert!(cpu.flag(FLAG_CF));

        cpu.cycle(&mut bus).unwrap(); // cmc
        assert!(!cpu.flag(FLAG_CF));
    }

    #[test]
    fn pushf_then_popf_restores_flags() {
        // pushf (0x9c) ; popf (0x9d). pushf saves CF=1; CF is perturbed by hand;
        // popf restores it and reserved bit 1 stays set.
        let mut memory = vec![0; 1024];
        memory[0] = 0x9c;
        memory[1] = 0x9d;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        cpu.set_flag(FLAG_CF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap(); // pushf
        cpu.set_flag(FLAG_CF, false); // perturb after the value is on the stack
        cpu.cycle(&mut bus).unwrap(); // popf

        assert!(cpu.flag(FLAG_CF));
        assert_eq!(cpu.registers.eflags & 0x2, 0x2);
    }

    #[test]
    fn leave_restores_sp_and_bp() {
        // leave (0xc9): sp <- bp; bp <- pop. bp = 0x0200, [ss:0x0200] = 0x1234.
        // Result: bp = 0x1234, sp = 0x0202 (0x0200 then +2 from the pop).
        let mut memory = vec![0; 1024];
        memory[0] = 0xc9;
        memory[0x200..0x202].copy_from_slice(&0x1234u16.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0080);
        cpu.write_gpr16(5, 0x0200); // BP
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_gpr16(5), 0x1234);
        assert_eq!(cpu.read_gpr16(4), 0x0202);
    }

    #[test]
    fn pusha_then_popa_round_trips_and_saves_original_sp() {
        // pusha (0x60) ; popa (0x61). All GPRs round-trip; the SP slot holds the
        // pre-pusha SP and popa discards it, so SP returns to its starting value.
        let mut memory = vec![0; 1024];
        memory[0] = 0x60;
        memory[1] = 0x61;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        cpu.write_gpr16(0, 0x1111);
        cpu.write_gpr16(1, 0x2222);
        cpu.write_gpr16(2, 0x3333);
        cpu.write_gpr16(3, 0x4444);
        cpu.write_gpr16(5, 0x6666);
        cpu.write_gpr16(6, 0x7777);
        cpu.write_gpr16(7, 0x8888);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap(); // pusha: 8 words, sp 0x0100 -> 0x00f0
        assert_eq!(cpu.read_gpr16(4), 0x00f0);
        // the 5th push (the SP slot) lands at 0x0100 - 2*5 = 0x00f6 and holds 0x0100
        assert_eq!(
            u16::from_le_bytes([bus.memory[0xf6], bus.memory[0xf7]]),
            0x0100
        );

        cpu.cycle(&mut bus).unwrap(); // popa
        assert_eq!(cpu.read_gpr16(0), 0x1111);
        assert_eq!(cpu.read_gpr16(1), 0x2222);
        assert_eq!(cpu.read_gpr16(2), 0x3333);
        assert_eq!(cpu.read_gpr16(3), 0x4444);
        assert_eq!(cpu.read_gpr16(5), 0x6666);
        assert_eq!(cpu.read_gpr16(6), 0x7777);
        assert_eq!(cpu.read_gpr16(7), 0x8888);
        assert_eq!(cpu.read_gpr16(4), 0x0100);
    }

    #[test]
    fn pushfd_pushes_only_defined_eflags_bits() {
        // 0x66 0x9c PUSHFD. EFLAGS carries garbage in the high bits; the 386 pushes
        // only the defined low 16, so the dword on the stack is 0x0000_0493.
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0x66, 0x9c]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        cpu.registers.eflags = 0xfffc_0493;
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        let pushed = u32::from_le_bytes([
            bus.memory[0xfc],
            bus.memory[0xfd],
            bus.memory[0xfe],
            bus.memory[0xff],
        ]);
        assert_eq!(pushed, 0x0000_0493);
        assert_eq!(cpu.registers.esp(), 0x0000_00fc);
    }

    #[test]
    fn pushad_uses_16bit_sp_and_preserves_high_esp() {
        // 0x66 0x60 PUSHAD on a 16-bit stack: SP wraps within the segment and ESP[31:16]
        // is preserved. ESP = 0x0001_0010 -> SP 0x10 - 32 wraps to 0xfff0.
        let mut memory = vec![0; 0x2_0000];
        memory[0..2].copy_from_slice(&[0x66, 0x60]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0001_0010);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.esp(), 0x0001_fff0);
    }

    #[test]
    fn popad_leaks_discarded_esp_high_half_on_16bit_stack() {
        // 0x66 0x61 POPAD on a 16-bit stack: the discarded saved-ESP slot's high half
        // lands in ESP[31:16] while SP keeps the advanced value (a 386 quirk).
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0x66, 0x61]);
        // The discard is the 4th dword, at SP + 12 = 0x20c.
        memory[0x20c..0x210].copy_from_slice(&0x5a04_6b18u32.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0200);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        // SP 0x200 + 32 = 0x220; high half from the discarded slot = 0x5a04.
        assert_eq!(cpu.registers.esp(), 0x5a04_0220);
    }

    #[test]
    fn retf_pops_offset_then_segment() {
        // retf (0xcb). Stack at ss:0x0100 holds ip 0x0100 then cs 0x3000.
        let mut memory = vec![0; 1024];
        memory[0] = 0xcb;
        memory[0x100..0x104].copy_from_slice(&[0x00, 0x01, 0x00, 0x30]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.cs().selector, 0x3000);
        assert_eq!(cpu.registers.eip, 0x0100);
        assert_eq!(cpu.read_gpr16(4), 0x0104); // two word pops from 0x0100
    }

    #[test]
    fn far_call_then_retf_round_trips() {
        // call far 0x0000:0x0010 ; the target at 0x10 is retf (0xcb).
        let mut memory = vec![0; 1024];
        memory[0..5].copy_from_slice(&[0x9a, 0x10, 0x00, 0x00, 0x00]);
        memory[0x10] = 0xcb;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap(); // far call -> cs:0x0000, eip 0x0010
        assert_eq!(cpu.registers.eip, 0x0010);
        cpu.cycle(&mut bus).unwrap(); // retf -> back to cs:0x0000, eip 0x0005
        assert_eq!(cpu.registers.cs().selector, 0x0000);
        assert_eq!(cpu.registers.eip, 0x0005);
        assert_eq!(cpu.read_gpr16(4), 0x0100); // sp restored
    }

    #[test]
    fn ret_near_imm16_pops_and_releases() {
        // ret 0x0004  (0xc2 0x04 0x00). Return ip 0x0100 at ss:0x0100.
        let mut memory = vec![0; 1024];
        memory[0..3].copy_from_slice(&[0xc2, 0x04, 0x00]);
        memory[0x100..0x102].copy_from_slice(&0x0100u16.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eip, 0x0100);
        // sp: 0x0100 -> +2 (word pop) -> +4 (release) = 0x0106
        assert_eq!(cpu.read_gpr16(4), 0x0106);
    }

    #[test]
    fn ret_near_imm16_32bit_preserves_high_esp() {
        // 0x66 0xc2 0x04 0x00 : 32-bit ret, release 4. Pop eip (dword), then release.
        let mut memory = vec![0; 1024];
        memory[0..4].copy_from_slice(&[0x66, 0xc2, 0x04, 0x00]);
        memory[0x100..0x104].copy_from_slice(&0x0000_0100u32.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0xdead_0100);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eip, 0x0000_0100);
        // real-mode 16-bit stack: only SP moves, ESP[31:16] preserved.
        // 0x0100 -> +4 (dword pop) -> +4 (release) = 0x0108
        assert_eq!(cpu.registers.esp(), 0xdead_0108);
    }

    #[test]
    fn retf_imm16_pops_far_and_releases() {
        // retf 0x0004  (0xca 0x04 0x00). Stack: ip 0x0100 then cs 0x3000 at ss:0x0100.
        let mut memory = vec![0; 1024];
        memory[0..3].copy_from_slice(&[0xca, 0x04, 0x00]);
        memory[0x100..0x104].copy_from_slice(&[0x00, 0x01, 0x00, 0x30]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.cs().selector, 0x3000);
        assert_eq!(cpu.registers.eip, 0x0100);
        // sp: 0x0100 -> +4 (far pop) -> +4 (release) = 0x0108
        assert_eq!(cpu.read_gpr16(4), 0x0108);
    }

    #[test]
    fn release_stack_wraps_sp_and_preserves_high_esp_in_real_mode() {
        // release_stack alone, with no surrounding pop, must move only SP on a
        // real-mode 16-bit stack and wrap at the 16-bit boundary. ESP[31:16] must
        // not absorb the carry: a full-ESP add of 0xbeef_fffe + 4 would carry into
        // 0xbef0_0002, while the SP-only path gives 0xbeef_0002.
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.set_esp(0xbeef_fffe);

        cpu.release_stack(4);

        assert_eq!(cpu.registers.esp(), 0xbeef_0002);
    }

    #[test]
    fn far_call_pushes_return_and_loads_target() {
        // call far 0x3000:0x0100  (0x9a 0x00 0x01 0x00 0x30), a 5-byte instruction.
        // Pushes CS (0x0000) then the return IP (0x0005), then loads cs:eip.
        let mut memory = vec![0; 1024];
        memory[0..5].copy_from_slice(&[0x9a, 0x00, 0x01, 0x00, 0x30]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.cs().selector, 0x3000);
        assert_eq!(cpu.registers.eip, 0x0100);
        assert_eq!(cpu.read_gpr16(4), 0x00fc); // two word pushes from 0x0100
        // CS at the higher slot, return IP just below it
        assert_eq!(
            u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
            0x0000
        );
        assert_eq!(
            u16::from_le_bytes([bus.memory[0xfc], bus.memory[0xfd]]),
            0x0005
        );
    }

    #[test]
    fn far_call_via_memory_pushes_return_and_transfers() {
        // call far [0x0200]  (0xff 0x1e 0x00 0x02), a 4-byte instruction. The far
        // pointer at ds:0x0200 is offset 0x0100, selector 0x3000.
        let mut memory = vec![0; 1024];
        memory[0..4].copy_from_slice(&[0xff, 0x1e, 0x00, 0x02]);
        memory[0x200..0x204].copy_from_slice(&[0x00, 0x01, 0x00, 0x30]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.cs().selector, 0x3000);
        assert_eq!(cpu.registers.eip, 0x0100);
        assert_eq!(cpu.read_gpr16(4), 0x00fc);
        // return CS 0x0000 at the higher slot, return IP 0x0004 below it
        assert_eq!(
            u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
            0x0000
        );
        assert_eq!(
            u16::from_le_bytes([bus.memory[0xfc], bus.memory[0xfd]]),
            0x0004
        );
    }

    #[test]
    fn far_jmp_via_memory_transfers_without_pushing() {
        // jmp far [0x0200]  (0xff 0x2e 0x00 0x02). Pointer = offset 0x0100, selector 0x3000.
        let mut memory = vec![0; 1024];
        memory[0..4].copy_from_slice(&[0xff, 0x2e, 0x00, 0x02]);
        memory[0x200..0x204].copy_from_slice(&[0x00, 0x01, 0x00, 0x30]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.cs().selector, 0x3000);
        assert_eq!(cpu.registers.eip, 0x0100);
        assert_eq!(cpu.read_gpr16(4), 0x0100); // nothing pushed
    }

    #[test]
    fn far_call_via_register_operand_delivers_ud() {
        // 0xff /3 with mod=3 (0xff 0xd8) is an invalid encoding -> #UD (vector 6).
        // IVT[6] at 0x18 points to IP 0x00ee, CS 0; the CPU vectors there and clears IF.
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0xff, 0xd8]);
        memory[0x18] = 0xee;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        cpu.set_flag(FLAG_IF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eip, 0x00ee);
        assert!(!cpu.flag(FLAG_IF));
    }

    #[test]
    fn far_jmp_via_register_operand_delivers_ud() {
        // 0xff /5 with mod=3 (0xff 0xe8) is an invalid encoding -> #UD (vector 6).
        // The conformance suite pre-skips exception vectors, so this is the only
        // guard that the register form of the far JMP faults rather than transfers.
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0xff, 0xe8]);
        memory[0x18] = 0xee;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        cpu.set_flag(FLAG_IF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eip, 0x00ee);
        assert!(!cpu.flag(FLAG_IF));
    }

    #[test]
    fn far_call_via_memory_wraps_selector_offset_at_64k() {
        // call far [bx+di] (0xff 0x19) with bx+di = 0xfffe. On a 16-bit real-mode
        // segment the IP is read at ds:0xfffe and the selector offset wraps to
        // ds:0x0000 rather than reading past the 0xffff limit; a real 80386
        // completes this without faulting (SingleStepTests FF.3 "call far
        // [ds:bx+di]" with bx=di=0xffff).
        let ds_base = 0x2_0000usize; // ds selector 0x2000
        let mut memory = vec![0; 0x3_0000];
        memory[0..2].copy_from_slice(&[0xff, 0x19]);
        // IP at ds:0xfffe
        memory[ds_base + 0xfffe..ds_base + 0x1_0000].copy_from_slice(&0x0100u16.to_le_bytes());
        // selector at the wrapped ds:0x0000
        memory[ds_base..ds_base + 2].copy_from_slice(&0x3000u16.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0x2000);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        cpu.write_gpr16(3, 0xfffe); // bx
        cpu.write_gpr16(7, 0x0000); // di
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.cs().selector, 0x3000);
        assert_eq!(cpu.registers.eip, 0x0100);
        assert_eq!(cpu.read_gpr16(4), 0x00fc); // pushed CS then return IP
    }

    #[test]
    fn retf_32bit_pops_full_eip_and_preserves_high_esp() {
        // 0x66 0xcb (32-bit RETF). Pops EIP (dword, not masked to 16) then CS
        // (dword, truncated to the selector). On the real-mode 16-bit stack only
        // SP moves, so ESP[31:16] is preserved.
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0x66, 0xcb]);
        memory[0x100..0x104].copy_from_slice(&0x0001_2345u32.to_le_bytes()); // EIP
        memory[0x104..0x108].copy_from_slice(&0x0000_3000u32.to_le_bytes()); // CS
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0xcafe_0100);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.cs().selector, 0x3000);
        assert_eq!(cpu.registers.eip, 0x0001_2345);
        // sp 0x0100 -> +8 (two dword pops) = 0x0108, high half preserved
        assert_eq!(cpu.registers.esp(), 0xcafe_0108);
    }

    #[test]
    fn movzx_byte_zero_extends_into_ax() {
        // movzx ax, bl  (0x0f 0xb6 0xc3, modrm mod=3 reg=ax rm=bl): bl=0x80 -> ax=0x0080.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xb6, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr8(3, 0x80); // bl
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0080);
    }

    #[test]
    fn movzx_byte_zero_extends_into_eax_clearing_high_bits() {
        // 0x66 0x0f 0xb6 0xc3 (movzx eax, bl): bl=0x80, eax preset 0xffff_ffff -> eax=0x0000_0080.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xb6, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_eax(0xffff_ffff);
        cpu.write_gpr8(3, 0x80);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eax(), 0x0000_0080);
    }

    #[test]
    fn movzx_word_zero_extends_into_eax() {
        // 0x66 0x0f 0xb7 0xc3 (movzx eax, bx): bx=0x8000, eax preset 0xffff_ffff -> eax=0x0000_8000.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xb7, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_eax(0xffff_ffff);
        cpu.write_reg16(Reg16::Bx, 0x8000);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eax(), 0x0000_8000);
    }

    #[test]
    fn movsx_byte_sign_extends_into_ax() {
        // movsx ax, bl (0x0f 0xbe 0xc3): bl=0x80 -> ax=0xff80.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xbe, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr8(3, 0x80);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0xff80);
    }

    #[test]
    fn movsx_byte_sign_extends_into_eax() {
        // 0x66 0x0f 0xbe 0xc3 (movsx eax, bl): bl=0x80 -> eax=0xffff_ff80.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xbe, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr8(3, 0x80);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eax(), 0xffff_ff80);
    }

    #[test]
    fn movsx_word_sign_extends_into_eax() {
        // 0x66 0x0f 0xbf 0xc3 (movsx eax, bx): bx=0x8000 -> eax=0xffff_8000.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xbf, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x8000);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eax(), 0xffff_8000);
    }

    #[test]
    fn movsx_byte_positive_source_zero_fills() {
        // movsx ax, bl (0x0f 0xbe 0xc3): bl=0x7f (positive) -> ax=0x007f, no sign fill.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xbe, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr8(3, 0x7f);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x007f);
    }

    #[test]
    fn movzx_word_into_16bit_dest_preserves_high_eax() {
        // movzx ax, bx (0x0f 0xb7 0xc3, no 0x66): a word source into a 16-bit
        // destination is a plain word move; the high half of EAX is preserved by
        // write_gpr16.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xb7, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_eax(0xdead_0000);
        cpu.write_reg16(Reg16::Bx, 0x8000);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eax(), 0xdead_8000);
    }

    #[test]
    fn movzx_reads_byte_from_memory_source() {
        // movzx ax, byte [0x40] (0x0f 0xb6 0x06 0x40 0x00, modrm mod=00 rm=110 disp16):
        // [ds:0x40]=0x80 -> ax=0x0080. Exercises the memory-source decode path that the
        // register-operand tests do not, since the conformance vectors are not in CI.
        let mut memory = vec![0; 128];
        memory[0..5].copy_from_slice(&[0x0f, 0xb6, 0x06, 0x40, 0x00]);
        memory[0x40] = 0x80;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0080);
    }

    #[test]
    fn setz_sets_byte_when_zf_set() {
        // setz bl (0x0f 0x94 0xc3): ZF=1 -> bl=1. bl preset 0xff to prove it is overwritten.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0x94, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr8(3, 0xff);
        cpu.set_flag(FLAG_ZF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_gpr8(3), 1);
        // SETcc writes the byte without disturbing the flag it tested.
        assert!(cpu.flag(FLAG_ZF));
    }

    #[test]
    fn setz_clears_byte_when_zf_clear() {
        // setz bl (0x0f 0x94 0xc3): ZF=0 -> bl=0.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0x94, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr8(3, 0xff);
        cpu.set_flag(FLAG_ZF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_gpr8(3), 0);
    }

    #[test]
    fn setnz_writes_memory_destination() {
        // setnz byte [0x40] (0x0f 0x95 0x06 0x40 0x00, modrm mod=00 rm=110 disp16):
        // ZF=0 -> !ZF true -> [ds:0x40]=1.
        let mut memory = vec![0; 128];
        memory[0..5].copy_from_slice(&[0x0f, 0x95, 0x06, 0x40, 0x00]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_ZF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(bus.memory[0x40], 1);
    }

    #[test]
    fn imul_0f_af_16bit_fits_clears_carry_overflow() {
        // imul bx, cx (0x0f 0xaf 0xd9, modrm mod=3 reg=bx rm=cx): 3 * 4 = 12, CF=OF=0.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xaf, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 3);
        cpu.write_reg16(Reg16::Cx, 4);
        cpu.set_flag(FLAG_CF | FLAG_OF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Bx), 12);
        assert!(!cpu.flag(FLAG_CF));
        assert!(!cpu.flag(FLAG_OF));
    }

    #[test]
    fn imul_0f_af_16bit_overflow_sets_carry_overflow() {
        // imul bx, cx (0x0f 0xaf 0xd9): 0x1000 * 0x10 = 0x10000, truncates to 0, CF=OF=1.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xaf, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x1000);
        cpu.write_reg16(Reg16::Cx, 0x0010);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Bx), 0x0000);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_OF));
    }

    #[test]
    fn imul_0f_af_32bit_fits_clears_carry_overflow() {
        // 0x66 0x0f 0xaf 0xd9 (imul ebx, ecx): 1000 * 1000 = 1_000_000, CF=OF=0.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xaf, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr32(3, 1000); // ebx
        cpu.write_gpr32(1, 1000); // ecx
        cpu.set_flag(FLAG_CF | FLAG_OF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_gpr32(3), 1_000_000);
        assert!(!cpu.flag(FLAG_CF));
        assert!(!cpu.flag(FLAG_OF));
    }

    #[test]
    fn imul_0f_af_32bit_overflow_sets_carry_overflow() {
        // 0x66 0x0f 0xaf 0xd9 (imul ebx, ecx): 0x10000 * 0x10000 = 0x1_0000_0000,
        // truncates to 0, CF=OF=1.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xaf, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr32(3, 0x0001_0000); // ebx
        cpu.write_gpr32(1, 0x0001_0000); // ecx
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_gpr32(3), 0x0000_0000);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_OF));
    }

    #[test]
    fn imul_0f_af_signed_negative_result_fits() {
        // imul bx, cx (0x0f 0xaf 0xd9): -1 * 5 = -5 (0xfffb), fits signed 16-bit, CF=OF=0.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xaf, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0xffff); // -1
        cpu.write_reg16(Reg16::Cx, 0x0005);
        cpu.set_flag(FLAG_CF | FLAG_OF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Bx), 0xfffb);
        assert!(!cpu.flag(FLAG_CF));
        assert!(!cpu.flag(FLAG_OF));
    }

    #[test]
    fn imul_0f_af_signed_overflow_differs_from_unsigned() {
        // imul bx, cx (0x0f 0xaf 0xd9): -1 * -32768 = +32768. The low half 0x8000
        // sign-extends to -32768, not +32768, so the signed result does not fit:
        // bx=0x8000, CF=OF=1. An unsigned multiply of 0xffff * 0x8000 would truncate
        // to the same 0x8000 but read as non-overflowing, so this distinguishes IMUL.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xaf, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0xffff); // -1
        cpu.write_reg16(Reg16::Cx, 0x8000); // -32768
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Bx), 0x8000);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_OF));
    }

    #[test]
    fn bsf_finds_lowest_set_bit() {
        // bsf bx, cx (0x0f 0xbc 0xd9): cx=0x0140 -> lowest set bit at index 6, ZF=0.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xbc, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 0x0140);
        cpu.set_flag(FLAG_ZF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Bx), 6);
        assert!(!cpu.flag(FLAG_ZF));
    }

    #[test]
    fn bsf_zero_source_sets_zf_and_leaves_dest() {
        // bsf bx, cx (0x0f 0xbc 0xd9): cx=0 -> ZF=1, bx unchanged (preset 0xbeef).
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xbc, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 0x0000);
        cpu.write_reg16(Reg16::Bx, 0xbeef);
        cpu.set_flag(FLAG_ZF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_ZF));
        assert_eq!(cpu.read_reg16(Reg16::Bx), 0xbeef);
    }

    #[test]
    fn bsf_32bit_finds_low_bit() {
        // 0x66 0x0f 0xbc 0xd9 (bsf ebx, ecx): ecx=0x8000_0000 -> index 31, ZF=0.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xbc, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr32(1, 0x8000_0000); // ecx
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_gpr32(3), 31); // ebx
        assert!(!cpu.flag(FLAG_ZF));
    }

    #[test]
    fn bsr_finds_highest_set_bit() {
        // bsr bx, cx (0x0f 0xbd 0xd9): cx=0x0140 -> highest set bit at index 8, ZF=0.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xbd, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 0x0140);
        cpu.set_flag(FLAG_ZF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Bx), 8);
        assert!(!cpu.flag(FLAG_ZF));
    }

    #[test]
    fn bsr_32bit_finds_high_bit() {
        // 0x66 0x0f 0xbd 0xd9 (bsr ebx, ecx): ecx=0x8000_0000 -> index 31, ZF=0.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xbd, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr32(1, 0x8000_0000); // ecx
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_gpr32(3), 31); // ebx
        assert!(!cpu.flag(FLAG_ZF));
    }

    #[test]
    fn bsr_zero_source_sets_zf_and_leaves_dest() {
        // bsr bx, cx (0x0f 0xbd 0xd9): cx=0 -> ZF=1, bx unchanged (preset 0x1234).
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xbd, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 0x0000);
        cpu.write_reg16(Reg16::Bx, 0x1234);
        cpu.set_flag(FLAG_ZF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_ZF));
        assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
    }

    #[test]
    fn bt_register_reads_set_bit() {
        // bt cx, bx (0x0f 0xa3 0xd9, modrm mod=3 reg=bx rm=cx): cx=0x0008 bit 3, bx=3 -> CF=1, cx unchanged.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xa3, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 0x0008);
        cpu.write_reg16(Reg16::Bx, 3);
        cpu.set_flag(FLAG_CF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_CF));
        assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0008);
    }

    #[test]
    fn bt_register_reads_clear_bit() {
        // bt cx, bx: cx=0x0008, bx=2 (bit 2 clear) -> CF=0.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xa3, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 0x0008);
        cpu.write_reg16(Reg16::Bx, 2);
        cpu.set_flag(FLAG_CF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(!cpu.flag(FLAG_CF));
    }

    #[test]
    fn bts_register_sets_bit_and_reads_old() {
        // bts cx, bx (0x0f 0xab 0xd9): cx=0x0000, bx=3 -> CF=0 (old bit), cx=0x0008.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xab, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 0x0000);
        cpu.write_reg16(Reg16::Bx, 3);
        cpu.set_flag(FLAG_CF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(!cpu.flag(FLAG_CF));
        assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0008);
    }

    #[test]
    fn btr_register_clears_bit_and_reads_old() {
        // btr cx, bx (0x0f 0xb3 0xd9): cx=0x0008, bx=3 -> CF=1 (old bit), cx=0x0000.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xb3, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 0x0008);
        cpu.write_reg16(Reg16::Bx, 3);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_CF));
        assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0000);
    }

    #[test]
    fn btc_register_toggles_bit() {
        // btc cx, bx (0x0f 0xbb 0xd9): cx=0x0008, bx=3 -> CF=1 (old), cx=0x0000 (toggled off).
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xbb, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 0x0008);
        cpu.write_reg16(Reg16::Bx, 3);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_CF));
        assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0000);
    }

    #[test]
    fn bts_memory_positive_index_walks_to_next_word() {
        // bts [0x40], bx (0x0f 0xab 0x1e 0x40 0x00, modrm mod=00 reg=bx rm=110 disp16):
        // bx=17 -> block 1, bit 1 -> word at 0x42 (0x40+2). [0x42]=0 -> CF=0, [0x42]=0x0002.
        let mut memory = vec![0; 128];
        memory[0..5].copy_from_slice(&[0x0f, 0xab, 0x1e, 0x40, 0x00]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 17);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(!cpu.flag(FLAG_CF));
        assert_eq!(
            u16::from_le_bytes([bus.memory[0x42], bus.memory[0x43]]),
            0x0002
        );
        assert_eq!(
            u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]),
            0x0000
        );
    }

    #[test]
    fn bt_memory_negative_index_walks_to_previous_word() {
        // bt [0x40], bx (0x0f 0xa3 0x1e 0x40 0x00): bx=0xffff (-1) -> block -1, bit 15 ->
        // word at 0x3e (0x40-2). [0x3e]=0x8000 -> CF=1. BT does not write.
        let mut memory = vec![0; 128];
        memory[0..5].copy_from_slice(&[0x0f, 0xa3, 0x1e, 0x40, 0x00]);
        memory[0x3e..0x40].copy_from_slice(&0x8000u16.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0xffff); // -1
        cpu.set_flag(FLAG_CF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_CF));
        assert_eq!(
            u16::from_le_bytes([bus.memory[0x3e], bus.memory[0x3f]]),
            0x8000
        );
    }

    #[test]
    fn btc_memory_negative_index_walks_and_toggles() {
        // btc [0x40], bx (0x0f 0xbb 0x1e 0x40 0x00): bx=0xffff (-1) -> word at 0x3e, bit 15.
        // [0x3e]=0x8000 (bit 15 set) -> CF=1, the bit toggles off -> [0x3e]=0x0000.
        let mut memory = vec![0; 128];
        memory[0..5].copy_from_slice(&[0x0f, 0xbb, 0x1e, 0x40, 0x00]);
        memory[0x3e..0x40].copy_from_slice(&0x8000u16.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0xffff); // -1
        cpu.set_flag(FLAG_CF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_CF));
        assert_eq!(
            u16::from_le_bytes([bus.memory[0x3e], bus.memory[0x3f]]),
            0x0000
        );
    }

    #[test]
    fn bts_32bit_register_sets_high_bit() {
        // 0x66 0x0f 0xab 0xd9 (bts ecx, ebx): ecx=0, ebx=20 -> CF=0, ecx bit 20 set.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xab, 0xd9]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr32(1, 0); // ecx
        cpu.write_gpr32(3, 20); // ebx
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(!cpu.flag(FLAG_CF));
        assert_eq!(cpu.read_gpr32(1), 0x0010_0000);
    }

    #[test]
    fn bt_immediate_reads_selected_bit() {
        // bt cx, 5 (0x0f 0xba 0xe1 0x05, modrm mod=3 reg=/4 rm=cx): cx=0x0020 bit 5 -> CF=1, cx unchanged.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x0f, 0xba, 0xe1, 0x05]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 0x0020);
        cpu.set_flag(FLAG_CF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_CF));
        assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0020);
    }

    #[test]
    fn btr_immediate_clears_selected_bit() {
        // btr cx, 5 (0x0f 0xba 0xf1 0x05, modrm mod=3 reg=/6 rm=cx): cx=0x0020 -> CF=1, cx=0x0000.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x0f, 0xba, 0xf1, 0x05]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 0x0020);
        cpu.set_flag(FLAG_CF, false); // prove CF=1 comes from the old bit, not a residual
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_CF));
        assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0000);
    }

    #[test]
    fn bts_immediate_memory_no_walk() {
        // bts [0x40], 5 (0x0f 0xba 0x2e 0x40 0x00 0x05, modrm mod=00 reg=/5 rm=110 disp16):
        // imm bit 5, accesses [0x40] directly (no walk). [0x40]=0 -> CF=0, [0x40]=0x0020.
        let mut memory = vec![0; 128];
        memory[0..6].copy_from_slice(&[0x0f, 0xba, 0x2e, 0x40, 0x00, 0x05]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(!cpu.flag(FLAG_CF));
        assert_eq!(
            u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]),
            0x0020
        );
    }

    #[test]
    fn bt_immediate_reg_below_4_delivers_ud() {
        // 0x0f 0xba 0xc1 0x05 (modrm mod=3 reg=/0 rm=cx): reg<4 is invalid -> #UD (vector 6).
        // 1024 bytes so the stack push at 0x0100 (6 bytes) and IVT at 0x18 both fit.
        let mut memory = vec![0; 1024];
        memory[0..4].copy_from_slice(&[0x0f, 0xba, 0xc1, 0x05]);
        memory[0x18] = 0xee; // IVT[6] IP low byte -> IP 0x00ee
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        cpu.set_flag(FLAG_IF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eip, 0x00ee);
        assert!(!cpu.flag(FLAG_IF));
    }

    #[test]
    fn shld_imm_shifts_left_and_fills_from_source() {
        // shld ax, bx, 4 (0x0f 0xa4 0xd8 0x04, modrm mod=3 reg=bx rm=ax):
        // ax=0x1234, bx=0x5678 -> ax=0x2345, CF=1 (bit shifted out of ax bit 12).
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x0f, 0xa4, 0xd8, 0x04]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x1234);
        cpu.write_reg16(Reg16::Bx, 0x5678);
        cpu.set_flag(FLAG_CF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x2345);
        assert!(cpu.flag(FLAG_CF));
    }

    #[test]
    fn shrd_imm_shifts_right_and_fills_from_source() {
        // shrd ax, bx, 4 (0x0f 0xac 0xd8 0x04): ax=0x1234, bx=0x5678 -> ax=0x8123, CF=0.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x0f, 0xac, 0xd8, 0x04]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x1234);
        cpu.write_reg16(Reg16::Bx, 0x5678);
        cpu.set_flag(FLAG_CF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8123);
        assert!(!cpu.flag(FLAG_CF));
    }

    #[test]
    fn shld_cl_uses_cl_count() {
        // shld ax, bx, cl (0x0f 0xa5 0xd8): cl=4 -> same as imm 4: ax=0x2345, CF=1.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xa5, 0xd8]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x1234);
        cpu.write_reg16(Reg16::Bx, 0x5678);
        cpu.write_reg16(Reg16::Cx, 0x0004); // cl = 4
        cpu.set_flag(FLAG_CF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x2345);
        assert!(cpu.flag(FLAG_CF));
    }

    #[test]
    fn shrd_cl_uses_cl_count() {
        // shrd ax, bx, cl (0x0f 0xad 0xd8): cl=4 -> same as shrd imm 4: ax=0x8123, CF=0.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x0f, 0xad, 0xd8]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x1234);
        cpu.write_reg16(Reg16::Bx, 0x5678);
        cpu.write_reg16(Reg16::Cx, 0x0004); // cl = 4
        cpu.set_flag(FLAG_CF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8123);
        assert!(!cpu.flag(FLAG_CF));
    }

    #[test]
    fn shld_32bit_imm() {
        // 0x66 0x0f 0xa4 0xd8 0x08 (shld eax, ebx, 8): eax=0x1234_5678, ebx=0x9abc_def0
        // -> eax=0x3456_789a, CF=0.
        let mut memory = vec![0; 64];
        memory[0..5].copy_from_slice(&[0x66, 0x0f, 0xa4, 0xd8, 0x08]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr32(0, 0x1234_5678); // eax
        cpu.write_gpr32(3, 0x9abc_def0); // ebx
        cpu.set_flag(FLAG_CF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_gpr32(0), 0x3456_789a);
        assert!(!cpu.flag(FLAG_CF));
    }

    #[test]
    fn shld_count_one_sets_overflow_on_sign_change() {
        // shld ax, bx, 1 (0x0f 0xa4 0xd8 0x01): ax=0x4000 -> ax=0x8000, sign flips, OF=1.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x0f, 0xa4, 0xd8, 0x01]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x4000);
        cpu.write_reg16(Reg16::Bx, 0x0000);
        cpu.set_flag(FLAG_OF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
        assert!(cpu.flag(FLAG_OF));
    }

    #[test]
    fn shld_count_one_clears_overflow_without_sign_change() {
        // shld ax, bx, 1: ax=0x0001 -> ax=0x0002, sign unchanged, OF=0.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x0f, 0xa4, 0xd8, 0x01]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0001);
        cpu.write_reg16(Reg16::Bx, 0x0000);
        cpu.set_flag(FLAG_OF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0002);
        assert!(!cpu.flag(FLAG_OF));
    }

    #[test]
    fn shld_count_zero_is_noop() {
        // shld ax, bx, 0 (0x0f 0xa4 0xd8 0x00): ax unchanged, flags unchanged.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x0f, 0xa4, 0xd8, 0x00]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x1234);
        cpu.write_reg16(Reg16::Bx, 0x5678);
        cpu.set_flag(FLAG_CF, true);
        cpu.set_flag(FLAG_OF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1234);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_OF));
    }

    #[test]
    fn shld_count_past_width_rotates_source() {
        // shld ax, bx, 18 (0x0f 0xa4 0xd8 0x12): count 18 > 16 is undefined per Intel; the
        // 386 leaves ax as the source rotated left by 18 mod 16 = 2. bx=0x1234 -> ax=0x48d0.
        // The destination's prior value does not matter (preset 0xffff).
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x0f, 0xa4, 0xd8, 0x12]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0xffff);
        cpu.write_reg16(Reg16::Bx, 0x1234);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x48d0);
    }

    #[test]
    fn shrd_count_past_width_rotates_source() {
        // shrd ax, bx, 18 (0x0f 0xac 0xd8 0x12): the 386 leaves ax as the source rotated
        // right by 2. bx=0x1234 -> ax=0x048d.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x0f, 0xac, 0xd8, 0x12]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0xffff);
        cpu.write_reg16(Reg16::Bx, 0x1234);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x048d);
    }

    #[test]
    fn xchg_byte_swaps_registers() {
        // xchg al, bl (0x86 0xc3, modrm mod=3 reg=al rm=bl). al=0x12, bl=0x34 -> al=0x34, bl=0x12.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0x86, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0012);
        cpu.write_reg16(Reg16::Bx, 0x0034);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x34);
        assert_eq!(cpu.read_reg16(Reg16::Bx) & 0xff, 0x12);
    }

    #[test]
    fn xchg_word_swaps_registers() {
        // xchg bx, ax (0x87 0xc3, modrm reg=ax rm=bx). ax=0x1234, bx=0x5678 -> swapped.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0x87, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x1234);
        cpu.write_reg16(Reg16::Bx, 0x5678);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x5678);
        assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
    }

    #[test]
    fn xchg_word_swaps_register_and_memory() {
        // xchg [0x40], ax (0x87 0x06 0x40 0x00, modrm mod=0 reg=ax rm=110 disp16).
        let mut memory = vec![0; 128];
        memory[0..4].copy_from_slice(&[0x87, 0x06, 0x40, 0x00]);
        memory[0x40] = 0xcd;
        memory[0x41] = 0xab; // word at 0x40 = 0xabcd
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x1234);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0xabcd);
        assert_eq!(
            u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]),
            0x1234
        );
    }

    #[test]
    fn xchg_dword_swaps_registers() {
        // 0x66 0x87 0xc3 (xchg ebx, eax). eax=0x1111_2222, ebx=0x3333_4444 -> swapped.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x66, 0x87, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr32(0, 0x1111_2222);
        cpu.write_gpr32(3, 0x3333_4444);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_gpr32(0), 0x3333_4444);
        assert_eq!(cpu.read_gpr32(3), 0x1111_2222);
    }

    #[test]
    fn xchg_accumulator_swaps_ax_with_reg() {
        // xchg ax, cx (0x91). ax=0x1234, cx=0x5678 -> swapped.
        let mut memory = vec![0; 64];
        memory[0] = 0x91;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x1234);
        cpu.write_reg16(Reg16::Cx, 0x5678);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x5678);
        assert_eq!(cpu.read_reg16(Reg16::Cx), 0x1234);
    }

    #[test]
    fn xchg_accumulator_dword_swaps_eax_with_reg() {
        // 0x66 0x93 (xchg eax, ebx). eax=0x0001_0002, ebx=0x0003_0004 -> swapped.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0x66, 0x93]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr32(0, 0x0001_0002);
        cpu.write_gpr32(3, 0x0003_0004);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_gpr32(0), 0x0003_0004);
        assert_eq!(cpu.read_gpr32(3), 0x0001_0002);
    }

    #[test]
    fn xchg_byte_swaps_register_and_memory_with_displacement() {
        // xchg [bx+0x10], al (0x86 0x47 0x10, modrm mod=1 reg=al rm=[bx]+disp8).
        // bx=0x20 -> address 0x30. Guards against re-decoding the ModRm, which would
        // consume a second displacement byte and advance eip past the instruction.
        let mut memory = vec![0; 128];
        memory[0..3].copy_from_slice(&[0x86, 0x47, 0x10]);
        memory[0x30] = 0x99;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x0020);
        cpu.write_reg16(Reg16::Ax, 0x0055); // AL = 0x55
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x99); // AL got the memory byte
        assert_eq!(bus.memory[0x30], 0x55); // memory got AL
        assert_eq!(cpu.registers.eip, 3); // opcode + modrm + disp8, no extra fetch
    }

    #[test]
    fn loopne_decrements_cx_and_branches_while_not_equal() {
        // loopne +5 (0xe0 0x05). cx=3, ZF=0 -> cx=2, taken: eip = 2 + 5 = 7.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0xe0, 0x05]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 3);
        cpu.set_flag(FLAG_ZF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Cx), 2);
        assert_eq!(cpu.registers.eip, 7);
    }

    #[test]
    fn loopne_falls_through_when_zero_flag_set() {
        // loopne +5: cx=3, ZF=1 -> cx=2, not taken (LOOPNE loops while ZF=0): eip = 2.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0xe0, 0x05]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 3);
        cpu.set_flag(FLAG_ZF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Cx), 2);
        assert_eq!(cpu.registers.eip, 2);
    }

    #[test]
    fn loopne_falls_through_when_count_reaches_zero() {
        // loopne +5: cx=1, ZF=0 -> cx=0, not taken (count zero): eip = 2.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0xe0, 0x05]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 1);
        cpu.set_flag(FLAG_ZF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Cx), 0);
        assert_eq!(cpu.registers.eip, 2);
    }

    #[test]
    fn loope_branches_while_equal() {
        // loope +5 (0xe1 0x05): cx=3, ZF=1 -> cx=2, taken: eip = 7.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0xe1, 0x05]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 3);
        cpu.set_flag(FLAG_ZF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Cx), 2);
        assert_eq!(cpu.registers.eip, 7);
    }

    #[test]
    fn loope_falls_through_when_zero_flag_clear() {
        // loope +5 (0xe1 0x05): cx=3, ZF=0 -> cx=2, not taken (LOOPE loops while ZF=1): eip = 2.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0xe1, 0x05]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 3);
        cpu.set_flag(FLAG_ZF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Cx), 2);
        assert_eq!(cpu.registers.eip, 2);
    }

    #[test]
    fn jcxz_branches_only_when_cx_zero() {
        // jcxz +5 (0xe3 0x05): cx=0 -> taken (eip=7), no decrement.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0xe3, 0x05]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 0);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Cx), 0);
        assert_eq!(cpu.registers.eip, 7);
    }

    #[test]
    fn jcxz_falls_through_when_cx_nonzero() {
        // jcxz +5: cx=1 -> not taken: eip = 2.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0xe3, 0x05]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Cx, 1);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Cx), 1);
        assert_eq!(cpu.registers.eip, 2);
    }

    #[test]
    fn jecxz_uses_ecx_with_address_override() {
        // 0x67 jecxz +5 (0x67 0xe3 0x05): ecx=0 -> taken: eip = 3 + 5 = 8.
        let mut memory = vec![0; 64];
        memory[0..3].copy_from_slice(&[0x67, 0xe3, 0x05]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr32(1, 0); // ecx = 0
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eip, 8);
        assert_eq!(cpu.registers.ecx(), 0); // JECXZ does not decrement
    }

    #[test]
    fn xlat_reads_ds_table_indexed_by_al() {
        // xlat (0xd7): DS:0, BX=0x10, AL=0x05 -> AL = [0x15].
        let mut memory = vec![0; 64];
        memory[0] = 0xd7;
        memory[0x15] = 0xab;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x0010);
        cpu.write_reg16(Reg16::Ax, 0x0005); // AL = 5, AH = 0
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0xab);
        assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x00); // AH unchanged
        assert_eq!(cpu.read_reg16(Reg16::Bx), 0x0010); // BX unchanged
    }

    #[test]
    fn xlat_wraps_the_16bit_base_plus_index() {
        // xlat: BX=0xffff, AL=0x02 -> offset = (0xffff + 2) & 0xffff = 0x0001.
        let mut memory = vec![0; 64];
        memory[0] = 0xd7;
        memory[0x01] = 0xcd;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0xffff);
        cpu.write_reg16(Reg16::Ax, 0x0002);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0xcd);
    }

    #[test]
    fn xlat_honours_a_segment_override() {
        // 0x26 xlat (es override). ES base = 0x0100 << 4 = 0x1000. BX=0x10, AL=0x05 -> [0x1015].
        let mut memory = vec![0; 0x2000];
        memory[0..2].copy_from_slice(&[0x26, 0xd7]);
        memory[0x1015] = 0x99;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0x0100);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Bx, 0x0010);
        cpu.write_reg16(Reg16::Ax, 0x0005);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x99);
    }

    #[test]
    fn daa_low_nibble_correction() {
        // daa (0x27): AL=0x7C, CF=0, AF=0 -> AL=0x82 (low nibble +6), CF=0, AF=1.
        let mut memory = vec![0; 64];
        memory[0] = 0x27;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x007c);
        cpu.set_flag(FLAG_CF, false);
        cpu.set_flag(FLAG_AF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x82);
        assert!(!cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_AF));
    }

    #[test]
    fn daa_both_corrections_set_carry() {
        // daa: AL=0xAA -> +6 = 0xB0 (AF=1), then +0x60 = 0x10 (CF=1).
        let mut memory = vec![0; 64];
        memory[0] = 0x27;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x00aa);
        cpu.set_flag(FLAG_CF, false);
        cpu.set_flag(FLAG_AF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x10);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_AF));
    }

    #[test]
    fn daa_incoming_aux_carry_triggers_correction() {
        // daa: AL=0x20 (low nibble <= 9), AF=1 -> the first correction fires on AF alone:
        // AL=0x26, CF=0, AF=1.
        let mut memory = vec![0; 64];
        memory[0] = 0x27;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0020);
        cpu.set_flag(FLAG_CF, false);
        cpu.set_flag(FLAG_AF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x26);
        assert!(!cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_AF));
    }

    #[test]
    fn das_low_nibble_correction() {
        // das (0x2f): AL=0x4A, CF=0, AF=0 -> AL=0x44 (low nibble -6), CF=0, AF=1.
        let mut memory = vec![0; 64];
        memory[0] = 0x2f;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x004a);
        cpu.set_flag(FLAG_CF, false);
        cpu.set_flag(FLAG_AF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x44);
        assert!(!cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_AF));
    }

    #[test]
    fn das_high_correction_on_incoming_carry() {
        // das: AL=0x00, CF=1, AF=0 -> -0x60 = 0xA0, CF=1, AF=0.
        let mut memory = vec![0; 64];
        memory[0] = 0x2f;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0000);
        cpu.set_flag(FLAG_CF, true);
        cpu.set_flag(FLAG_AF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0xa0);
        assert!(cpu.flag(FLAG_CF));
        assert!(!cpu.flag(FLAG_AF));
    }

    #[test]
    fn aaa_adjusts_and_carries_into_ah() {
        // aaa (0x37): AX=0x000B (AL low nibble > 9) -> AX += 0x106, AL &= 0x0f.
        // AX=0x0111 then AL=0x01 -> AX=0x0101; CF=1, AF=1.
        let mut memory = vec![0; 64];
        memory[0] = 0x37;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x000b);
        cpu.set_flag(FLAG_AF, false);
        cpu.set_flag(FLAG_CF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x01);
        assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x01);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_AF));
    }

    #[test]
    fn aaa_no_adjust_clears_carry() {
        // aaa: AX=0x0005, AF=0 -> only AL &= 0x0f; CF=0, AF=0, AH unchanged.
        let mut memory = vec![0; 64];
        memory[0] = 0x37;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0005);
        cpu.set_flag(FLAG_AF, false);
        cpu.set_flag(FLAG_CF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x05);
        assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x00);
        assert!(!cpu.flag(FLAG_CF));
        assert!(!cpu.flag(FLAG_AF));
    }

    #[test]
    fn aas_adjusts_and_borrows_from_ah() {
        // aas (0x3f): AX=0x020B (AL low nibble > 9) -> AX -= 6, AH -= 1, AL &= 0x0f.
        // 0x020B - 6 = 0x0205, AH-1 -> 0x0105, AL=0x05; CF=1, AF=1.
        let mut memory = vec![0; 64];
        memory[0] = 0x3f;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x020b);
        cpu.set_flag(FLAG_AF, false);
        cpu.set_flag(FLAG_CF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x05);
        assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x01);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_AF));
    }

    #[test]
    fn aas_no_adjust_clears_carry() {
        // aas: AX=0x0204, AF=0 -> only AL &= 0x0f; CF=0, AF=0, AH unchanged.
        let mut memory = vec![0; 64];
        memory[0] = 0x3f;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0204);
        cpu.set_flag(FLAG_AF, false);
        cpu.set_flag(FLAG_CF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x04);
        assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x02);
        assert!(!cpu.flag(FLAG_CF));
        assert!(!cpu.flag(FLAG_AF));
    }

    #[test]
    fn aaa_aux_carry_triggers_adjust() {
        // aaa: AL=0x01 (low nibble <= 9), AF=1 -> the adjust fires on AF alone.
        // AX=0x0001 + 0x106 = 0x0107, then AL &= 0x0f -> AX=0x0107; AL=0x07, AH=0x01, CF=1, AF=1.
        let mut memory = vec![0; 64];
        memory[0] = 0x37;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0001);
        cpu.set_flag(FLAG_AF, true);
        cpu.set_flag(FLAG_CF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x07);
        assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x01);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_AF));
    }

    #[test]
    fn aas_aux_carry_triggers_adjust() {
        // aas: AL=0x08 (low nibble <= 9, >= 6 so no extra AH borrow), AF=1 -> the adjust
        // fires on AF alone. AX=0x0208 - 6 = 0x0202, AH-1 -> 0x0102, AL &= 0x0f -> AX=0x0102;
        // AL=0x02, AH=0x01, CF=1, AF=1.
        let mut memory = vec![0; 64];
        memory[0] = 0x3f;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0208);
        cpu.set_flag(FLAG_AF, true);
        cpu.set_flag(FLAG_CF, false);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x02);
        assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x01);
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_AF));
    }

    #[test]
    fn aam_splits_al_into_ah_and_al() {
        // aam (0xd4 0x0a): AL=0x4B (75) -> AH=7, AL=5. SF=0, ZF=0.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0xd4, 0x0a]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x004b);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x05);
        assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x07);
        assert!(!cpu.flag(FLAG_SF));
        assert!(!cpu.flag(FLAG_ZF));
    }

    #[test]
    fn aam_zero_divisor_is_divide_error() {
        // aam (0xd4 0x00): divide by zero -> DivideError.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0xd4, 0x00]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x004b);
        let mut bus = TestBus::with_memory(memory);

        assert!(matches!(cpu.cycle(&mut bus), Err(CpuError::DivideError)));
    }

    #[test]
    fn aad_folds_ah_into_al() {
        // aad (0xd5 0x0a): AX=0x0507 (AH=5, AL=7) -> AL = 7 + 5*10 = 57 = 0x39, AH=0.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0xd5, 0x0a]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0507);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x39);
        assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x00);
        assert!(!cpu.flag(FLAG_ZF));
    }
}
