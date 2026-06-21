use izarravm_bus::{BusAccessKind, BusError, BusWidth, CpuBus};
use thiserror::Error;

mod fpu;
mod mmx;
pub use fpu::X87;

const FLAG_CF: u32 = 0x0000_0001;
const FLAG_PF: u32 = 0x0000_0004;
const FLAG_AF: u32 = 0x0000_0010;
const FLAG_ZF: u32 = 0x0000_0040;
const FLAG_SF: u32 = 0x0000_0080;
const FLAG_TF: u32 = 0x0000_0100;
const FLAG_IF: u32 = 0x0000_0200;
const FLAG_DF: u32 = 0x0000_0400;
const FLAG_OF: u32 = 0x0000_0800;
const FLAG_NT: u32 = 0x0000_4000; // bit 14, nested task

/// Segment-selector slots in a 386 TSS, in memory order from offset 72 (ES, CS, SS,
/// DS, FS, GS). Used by the task-switch save and restore.
const TASK_SEGMENTS: [SegmentIndex; 6] = [
    SegmentIndex::Es,
    SegmentIndex::Cs,
    SegmentIndex::Ss,
    SegmentIndex::Ds,
    SegmentIndex::Fs,
    SegmentIndex::Gs,
];
// 486 EFLAGS additions. AC (bit 18) is the alignment-check enable consulted by the
// #AC path together with CR0.AM; ID (bit 21) is the toggleable bit software flips to
// probe for CPUID. Both are plain read/write storage otherwise, and both survive a
// PUSHFD/POPFD round-trip (the dword flag image carries them).
const FLAG_AC: u32 = 0x0004_0000; // bit 18
const FLAG_ID: u32 = 0x0020_0000; // bit 21

const CR0_PE: u32 = 0x0000_0001;
const CR0_MP: u32 = 0x0000_0002;
const CR0_EM: u32 = 0x0000_0004;
const CR0_TS: u32 = 0x0000_0008;
const CR0_PG: u32 = 0x8000_0000;
// 486 control bits added to the 386's PE/PG. WP gates supervisor writes to read-only
// pages in translate_linear, and AM enables the #AC alignment-check path. The rest are
// read/write storage with no modeled effect, kept so MOV CR0 round-trips them:
//   NE (bit 5)  numeric-error reporting; no FPU is emulated, so it is cosmetic.
//   NW (bit 29) / CD (bit 30) cache control; no cache is modeled, so both are
//               cosmetic.
// The cosmetic constants document the bit layout in one place; they are not yet
// read by the core, hence the allow on the trio that has no consumer.
// AM is CR0 bit 18. CR0 bit 4 is ET (extension type), which we leave as 0 because no
// x87 FPU is emulated, consistent with CPUID reporting the FPU feature off.
const CR0_WP: u32 = 0x0001_0000; // bit 16
const CR0_AM: u32 = 0x0004_0000; // bit 18
#[allow(dead_code)]
const CR0_NE: u32 = 0x0000_0020; // bit 5
#[allow(dead_code)]
const CR0_NW: u32 = 0x2000_0000; // bit 29
#[allow(dead_code)]
const CR0_CD: u32 = 0x4000_0000; // bit 30

// GSW-586 CPUID identity. The GSW-586 is the fantasy chip's physical part (a K6-class
// 586). CPUID always reports this identity regardless of the GswMode throttle, which is
// a clock control rather than an ISA switch. Keep every tunable value here so the
// identity is changed in one place.
//
// Leaf 0 returns the maximum basic leaf in EAX and the 12-byte vendor string
// "Genuine GSW " split across EBX, EDX, ECX in that standard order (each register holds
// four string bytes little-endian, so EBX's low byte is 'G', and so on). The full
// processor name "Genuine GSW-80586" does not fit the 12-byte vendor field, so it lives
// in the brand string returned by the extended leaves below.
const CPUID_MAX_BASIC_LEAF: u32 = 1;
const CPUID_VENDOR_EBX: u32 = u32::from_le_bytes(*b"Genu");
const CPUID_VENDOR_EDX: u32 = u32::from_le_bytes(*b"ine ");
const CPUID_VENDOR_ECX: u32 = u32::from_le_bytes(*b"GSW ");

// Leaf 1 EAX packs type (bits 13-12), family (bits 11-8), model (bits 7-4) and stepping
// (bits 3-0). Family 5 marks the 586/K6 class; the model and stepping are chosen values.
const CPUID_TYPE: u32 = 0; // original OEM part
const CPUID_FAMILY: u32 = 5; // 586 / K6 class
const CPUID_MODEL: u32 = 6; // chosen GSW-586 model
const CPUID_STEPPING: u32 = 1; // chosen stepping
const CPUID_VERSION_EAX: u32 =
    (CPUID_TYPE << 12) | (CPUID_FAMILY << 8) | (CPUID_MODEL << 4) | CPUID_STEPPING;

// Leaf 1 feature flags. Only bits for features the core actually emulates are set. FPU
// (bit 0) is off (no FPU is modeled); MMX (bit 23) is on to match the GSW-586 lore. No
// other feature is claimed yet (TSC, paging-extension, and the rest stay off until the
// matching behavior exists).
const CPUID_FEATURE_MMX: u32 = 1 << 23;
const CPUID_FEATURES_EDX: u32 = CPUID_FEATURE_MMX;

// Leaf 1 EBX: brand index 0 (no brand string), CLFLUSH line size and other fields stay 0.
const CPUID_LEAF1_EBX: u32 = 0;
// Leaf 1 ECX: no extended feature is claimed.
const CPUID_LEAF1_ECX: u32 = 0;

// Extended leaves expose the brand string "Genuine GSW-80586", the full human-readable
// processor name. Leaf 0x80000000 reports the maximum extended leaf; leaves 0x80000002
// through 0x80000004 return the 48-byte null-padded brand string, 16 bytes per leaf in
// EAX, EBX, ECX, EDX order. The original K6 lacked the brand-string leaves, so exposing
// them is a fantasy extension that lets the GSW-586 name itself in full.
// The maximum extended leaf reaches 0x80000006 so the AMD-style cache leaves
// 0x80000005 (L1) and 0x80000006 (L2) sit inside the reported range. The K6 did
// expose these cache leaves, so they fit the GSW-586 identity.
const CPUID_MAX_EXT_LEAF: u32 = 0x8000_0006;
const CPUID_BRAND_EAX_0: u32 = u32::from_le_bytes(*b"Genu");
const CPUID_BRAND_EBX_0: u32 = u32::from_le_bytes(*b"ine ");
const CPUID_BRAND_ECX_0: u32 = u32::from_le_bytes(*b"GSW-");
const CPUID_BRAND_EDX_0: u32 = u32::from_le_bytes(*b"8058");
const CPUID_BRAND_EAX_1: u32 = u32::from_le_bytes([b'6', 0, 0, 0]);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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

// Reset state is all zero: PE/PG clear (real mode, no paging) and AM clear. AM is
// correctly CR0 bit 18, so it powers up masked at zero. Bit 4 is ET, which an old
// 386 reset forced on; here it stays 0 since no x87 FPU is emulated, so the default
// is a plain zero (derived).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ControlRegisters {
    pub cr0: u32,
    pub cr2: u32,
    pub cr3: u32,
}

/// The instruction-set level the core presents to the running guest. It is a
/// throttle the Lotura GSW register selects live (286 mode -> I286, and so on), not
/// the physical part: the silicon is always a 586, so the core can execute the full
/// ISA. At a level below the part the core faithfully raises #UD for the features
/// that processor generation lacked, so a guest that opts into Super Slow (286) sees
/// a true 286 instruction boundary.
///
/// This gating is guest-facing only. The default is `I586`, the full ISA, so the
/// BIOS POST and every firmware path run with no restriction; the level drops below
/// I586 only when the guest writes a lower GSW mode to Lotura port 0xE1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum CpuLevel {
    I286,
    I386,
    I486,
    #[default]
    I586,
}

impl CpuLevel {
    /// True when the level predates the 386, which introduced the 32-bit operand
    /// and address forms (the 66h/67h prefixes) and the bulk of the 0F-extended
    /// opcode group the core also supports.
    const fn is_pre_386(self) -> bool {
        matches!(self, Self::I286)
    }

    /// True when CPUID is present. CPUID arrived on the late 486 and is standard on
    /// the 586; the 286 and 386 have no CPUID and report #UD for it.
    const fn has_cpuid(self) -> bool {
        matches!(self, Self::I486 | Self::I586)
    }

    /// Reported (L1 KB, L2 KB) cache for the level. Cosmetic, no timing effect; the
    /// L2 is a motherboard cache module. Mirrors `GswMode::cache_kb` in the core so
    /// the CPU can answer the cache readout without a core dependency.
    pub const fn cache_kb(self) -> (u16, u16) {
        match self {
            Self::I286 => (0, 0),
            Self::I386 => (0, 64),
            Self::I486 => (16, 128),
            Self::I586 => (32, 512),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cpu386 {
    pub registers: Registers,
    pub fpu: X87,
    pub control: ControlRegisters,
    pub gdtr: DescriptorTable,
    pub idtr: DescriptorTable,
    /// Local descriptor table register and task register. The selector plus the
    /// hidden base/limit cached from the system descriptor at load time.
    pub ldtr: SegmentRegister,
    pub tr: SegmentRegister,
    pub elapsed_clocks: u64,
    pub halted: bool,
    // STI sets this to block maskable interrupt delivery for one instruction:
    // the 386 holds off interrupts until the instruction after STI, which makes
    // the STI; HLT idle idiom safe (the HLT runs before any interrupt is taken).
    interrupt_shadow: bool,
    // The guest-facing instruction-set level. Defaults to the full ISA (I586) so
    // firmware POST is never restricted; the Machine lowers it from the live Lotura
    // GSW mode write. See CpuLevel.
    level: CpuLevel,
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
    lock: bool,
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
    Ins,
    Outs,
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

    /// The live instruction-set level the core presents to the guest.
    pub fn level(&self) -> CpuLevel {
        self.level
    }

    /// Set the guest-facing instruction-set level. The Machine calls this from the
    /// live Lotura GSW mode write so a guest that selects Super Slow (286) gets a
    /// true 286 instruction boundary. Firmware POST never calls this, so it always
    /// runs at the default full ISA.
    pub fn set_level(&mut self, level: CpuLevel) {
        self.level = level;
    }

    /// Reported (L1 KB, L2 KB) cache for the live level. Cosmetic, no timing effect.
    pub fn cache_kb(&self) -> (u16, u16) {
        self.level.cache_kb()
    }

    pub fn is_paging_enabled(&self) -> bool {
        self.control.cr0 & CR0_PG != 0
    }

    /// Current privilege level. The CPL lives in the low two bits of the CS selector. Real
    /// mode has no protection rings, so it is always CPL 0 there; only protected mode can
    /// run at a lower privilege.
    fn current_privilege_level(&self) -> u8 {
        if self.is_protected_mode() {
            (self.registers.cs().selector & 3) as u8
        } else {
            0
        }
    }

    /// True when CS points into the BIOS ROM: the real-mode F-segment aperture
    /// (0xF0000..0xFFFFF, which also covers the FF00:0000 IRET stub) or its high
    /// reset alias. The instruction-set gate skips ROM code so firmware always runs
    /// the full ISA. After the guest picks a lower GSW mode through the boot menu,
    /// the BIOS still finishes Accept, services interrupts, and boots unrestricted;
    /// only guest code, which never executes from the ROM, is held to the level.
    fn cs_in_firmware_rom(&self) -> bool {
        let base = self.registers.cs().base;
        (0x000F_0000..=0x000F_FFFF).contains(&base) || base >= 0xFFFF_0000
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
        // Wake from HLT only when a maskable interrupt can actually be taken. The 386
        // exits HLT on an enabled interrupt; a masked or IF-disabled request leaves it
        // halted. NMI and the other non-maskable wake sources are out of scope.
        if self.halted {
            if self.flag(FLAG_IF) && bus.interrupt_pending() {
                self.halted = false;
            } else {
                return Ok(CycleOutcome {
                    core_clocks: 1,
                    halted: true,
                });
            }
        }

        // Consume the one-instruction shadow set by STI. While it is active the
        // interrupt check below is skipped so the instruction after STI always
        // executes before an interrupt can be taken.
        let shadowed = self.interrupt_shadow;
        self.interrupt_shadow = false;

        // Service a pending hardware interrupt before fetching the next instruction.
        if !shadowed && self.flag(FLAG_IF) && bus.interrupt_pending() {
            if let Some(vector) = bus.acknowledge_interrupt() {
                self.hardware_interrupt(bus, vector)
                    .map_err(|fault| match fault {
                        InternalFault::Cpu(error) => error,
                        InternalFault::Exception { vector, .. } => CpuError::IdtLimit { vector },
                    })?;
                self.elapsed_clocks += 61;
                return Ok(CycleOutcome {
                    core_clocks: 61,
                    halted: false,
                });
            }
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
        if prefixes.lock {
            self.check_lock_target(bus, opcode)?;
        }
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
            0x69 => {
                // IMUL r, r/m, imm16/32: signed multiply of r/m by a full-width immediate.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let src = self.read_operand_sized(bus, operand, operand_size)?;
                let imm = self.fetch_immediate(bus, operand_size)?;
                let result = self.imul_truncated(src, imm, operand_size);
                self.write_gpr_sized(modrm.reg, operand_size, result);
                Ok(clocks(14))
            }
            0x6a => {
                // PUSH imm8: sign-extend the byte to the operand size, then push it.
                let value = sign_extend_u8(self.fetch_u8(bus)?);
                self.push(bus, value, operand_size)?;
                Ok(clocks(2))
            }
            0x6b => {
                // IMUL r, r/m, imm8: signed multiply of r/m by a sign-extended byte immediate.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let src = self.read_operand_sized(bus, operand, operand_size)?;
                let imm = sign_extend_u8(self.fetch_u8(bus)?);
                let result = self.imul_truncated(src, imm, operand_size);
                self.write_gpr_sized(modrm.reg, operand_size, result);
                Ok(clocks(14))
            }
            0x62 => {
                // BOUND r, m: the memory operand holds the signed lower and upper array
                // bounds; if the register is outside [lower, upper] raise #BR (vector 5).
                let modrm = self.fetch_modrm(bus)?;
                let memory = match self.decode_rm_operand(bus, prefixes, address_size, modrm)? {
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
                Ok(clocks(10))
            }
            0x6c => {
                self.run_string(bus, StringOp::Ins, BusWidth::Byte, prefixes, address_size)?;
                Ok(clocks(15))
            }
            0x6d => {
                self.run_string(
                    bus,
                    StringOp::Ins,
                    operand_size.bus_width(),
                    prefixes,
                    address_size,
                )?;
                Ok(clocks(15))
            }
            0x6e => {
                self.run_string(bus, StringOp::Outs, BusWidth::Byte, prefixes, address_size)?;
                Ok(clocks(14))
            }
            0x6f => {
                self.run_string(
                    bus,
                    StringOp::Outs,
                    operand_size.bus_width(),
                    prefixes,
                    address_size,
                )?;
                Ok(clocks(14))
            }
            0x70..=0x7f => {
                let rel = self.fetch_i8(bus)? as i32;
                if self.condition(opcode & 0x0f) {
                    self.relative_jump(rel, operand_size);
                }
                Ok(clocks(3))
            }
            // 0x82 is an undocumented alias of 0x80 (group-1 r/m8, imm8): identical encoding.
            0x80 | 0x82 => {
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
            0x8f => {
                // POP r/m16/32 (group 1A). Only reg=000 is defined; other reg
                // values are an illegal encoding. The popped value goes to the
                // r/m operand, which may be a register or memory. No flags change.
                let modrm = self.fetch_modrm(bus)?;
                if modrm.reg != 0 {
                    return Err(CpuError::UnsupportedGroupOpcode {
                        opcode,
                        extension: modrm.reg,
                    }
                    .into());
                }
                // The displacement (if any) is fetched while decoding the operand,
                // so decode first, then pop. A POP into memory addressed through
                // ESP would, per Intel, compute the effective address after the
                // stack increment; that edge case is not exercised here and is
                // deferred (the common register and disp-addressed forms match).
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let value = self.pop(bus, operand_size)?;
                self.write_operand_sized(bus, operand, operand_size, value)?;
                Ok(clocks(5))
            }
            0xc4 | 0xc5 => {
                // LES (0xc4) / LDS (0xc5): load a far pointer from memory. The ModRM r/m
                // names the memory operand; the low half (operand size) goes into the reg
                // operand and the next word is loaded into ES (0xc4) or DS (0xc5). No flags
                // change. mod=3 (a register r/m) is an invalid encoding and faults with #UD.
                let modrm = self.fetch_modrm(bus)?;
                let mem = match self.decode_rm_operand(bus, prefixes, address_size, modrm)? {
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
                let selector_offset = mem.offset.wrapping_add(operand_size.bytes());
                let selector = self.read_memory_sized(
                    bus,
                    mem.segment,
                    selector_offset,
                    OperandSize::Word,
                    BusAccessKind::DataRead,
                )? as u16;
                let segment = if opcode == 0xc4 {
                    SegmentIndex::Es
                } else {
                    SegmentIndex::Ds
                };
                self.load_segment(bus, segment, selector)?;
                self.write_gpr_sized(modrm.reg, operand_size, offset);
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
                // PUSHF / PUSHFD. The low 16 flag bits push the same in both forms. The
                // dword form additionally carries the 486 AC and ID bits (RF and VM are
                // masked to 0 in the pushed image, so they never appear). operand_size
                // drives whether push writes 2 or 4 bytes.
                let value = match operand_size {
                    OperandSize::Word => self.registers.eflags & 0xffff,
                    OperandSize::Dword => self.registers.eflags & (0xffff | FLAG_AC | FLAG_ID),
                };
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
            0xa0 => {
                // MOV AL, moffs8: byte form, ignores the operand-size prefix and
                // leaves flags untouched.
                let offset = self.fetch_moffs(bus, address_size)?;
                let value = self.read_memory_u8(
                    bus,
                    prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                    offset,
                    BusAccessKind::DataRead,
                )?;
                self.write_gpr8(0, value);
                Ok(clocks(4))
            }
            0xa2 => {
                // MOV moffs8, AL: byte counterpart to 0xa3, flags untouched.
                let offset = self.fetch_moffs(bus, address_size)?;
                let value = self.read_gpr8(0);
                self.write_memory_u8(
                    bus,
                    prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                    offset,
                    value,
                    BusAccessKind::DataWrite,
                )?;
                Ok(clocks(4))
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
                // Displacement bytes (if any) come before the immediate in the encoding,
                // so decode the operand first, then fetch the immediate.
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let value = self.fetch_u8(bus)?;
                self.write_operand_u8(bus, operand, value)?;
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
                // Displacement bytes (if any) come before the immediate in the encoding,
                // so decode the operand first, then fetch the immediate.
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let value = self.fetch_immediate(bus, operand_size)?;
                self.write_operand_sized(bus, operand, operand_size, value)?;
                Ok(clocks(2))
            }
            0xc8 => {
                // ENTER imm16, imm8: build a stack frame. NestingLevel is taken mod 32.
                // Counterpart to LEAVE. Stack-pointer width follows push's real-mode-vs-
                // protected model (the SS B-bit refinement is deferred there too).
                let alloc = self.fetch_u16(bus)?;
                let level = u32::from(self.fetch_u8(bus)? & 0x1f);
                let size = operand_size.bytes();
                let frame_bp = self.read_gpr_sized(5, operand_size);
                self.push(bus, frame_bp, operand_size)?;
                let frame_temp = self.read_gpr_sized(4, operand_size);
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
                match operand_size {
                    OperandSize::Word => {
                        let sp = self.read_gpr16(4).wrapping_sub(alloc);
                        self.write_gpr16(4, sp);
                    }
                    OperandSize::Dword => {
                        if self.is_protected_mode() {
                            let esp = self.registers.esp().wrapping_sub(u32::from(alloc));
                            self.registers.set_esp(esp);
                        } else {
                            let sp = self.read_gpr16(4).wrapping_sub(alloc);
                            self.write_gpr16(4, sp);
                        }
                    }
                }
                Ok(clocks(10))
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
            0xcc => {
                // INT 3: one-byte breakpoint trap to vector 3.
                self.software_interrupt(bus, 3)?;
                Ok(clocks(33))
            }
            0xcd => {
                let vector = self.fetch_u8(bus)?;
                self.software_interrupt(bus, vector)?;
                Ok(clocks(37))
            }
            0xce => {
                // INTO: trap to vector 4 only when OF is set; otherwise a no-op.
                if self.flag(FLAG_OF) {
                    self.software_interrupt(bus, 4)?;
                    Ok(clocks(35))
                } else {
                    Ok(clocks(3))
                }
            }
            0xcf => {
                self.iret(bus, operand_size)?;
                Ok(clocks(22))
            }
            0xd6 => {
                // SALC/SETALC (undocumented): AL = CF ? 0xFF : 0x00. Flags unaffected.
                let value = if self.flag(FLAG_CF) { 0xff } else { 0x00 };
                self.write_gpr8(0, value);
                Ok(clocks(2))
            }
            0xd8..=0xdf => self.execute_fpu(bus, opcode, prefixes, address_size),
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
            0xe5 => {
                // IN AX/EAX, imm8: word/dword port input into the accumulator.
                let port = u16::from(self.fetch_u8(bus)?);
                let value = bus.read_io(port, operand_size.bus_width())?;
                self.write_gpr_sized(0, operand_size, value);
                Ok(clocks(12))
            }
            0xe7 => {
                // OUT imm8, AX/EAX: word/dword port output from the accumulator.
                let port = u16::from(self.fetch_u8(bus)?);
                bus.write_io(
                    port,
                    operand_size.bus_width(),
                    self.read_gpr_sized(0, operand_size),
                )?;
                Ok(clocks(10))
            }
            0xed => {
                // IN AX/EAX, DX: word/dword port input addressed by DX.
                let port = self.read_gpr16(2);
                let value = bus.read_io(port, operand_size.bus_width())?;
                self.write_gpr_sized(0, operand_size, value);
                Ok(clocks(12))
            }
            0xef => {
                // OUT DX, AX/EAX: word/dword port output addressed by DX.
                let port = self.read_gpr16(2);
                bus.write_io(
                    port,
                    operand_size.bus_width(),
                    self.read_gpr_sized(0, operand_size),
                )?;
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
            0x9b => {
                // WAIT/FWAIT: trap with #MF if the x87 has a pending unmasked exception
                // (gated on CR0.NE; otherwise the FERR#/IRQ13 path the PC uses applies and
                // is not modeled). With nothing pending it retires as a no-op.
                if self.control.cr0 & CR0_NE != 0 && self.fpu.pending_unmasked_exception() {
                    return Err(InternalFault::Exception {
                        vector: 16,
                        error_code: None,
                    });
                }
                Ok(clocks(6))
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
                // STI sets IF and arms the one-instruction shadow so the instruction
                // immediately after STI always executes before any interrupt is taken.
                self.set_flag(FLAG_IF, true);
                self.interrupt_shadow = true;
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
        // Guest-level ISA gate for the 0F-extended group. At the 286 level the core
        // raises #UD for every 0F opcode the 386 (and later) introduced that it
        // otherwise executes: MOVZX/MOVSX, BT/BTS/BTR/BTC, BSF/BSR, SHLD/SHRD,
        // SETcc, the 0F-form IMUL and Jcc, MOV to/from CR, and the 486 additions
        // (INVD/WBINVD, CMPXCHG, XADD, BSWAP). The 286-era 0F opcodes the core
        // supports (0F 01 LGDT/LIDT) stay allowed. CPUID is gated separately below
        // because it is absent on both the 286 and the 386. Code fetched from the
        // BIOS ROM is exempt (see cs_in_firmware_rom), so the gate only ever holds
        // guest code that selected a lower GSW mode.
        if self.level.is_pre_386() && !self.cs_in_firmware_rom() && is_386plus_two_byte(opcode) {
            return Err(InternalFault::Exception {
                vector: 6,
                error_code: None,
            });
        }
        match opcode {
            // ponytail: MMX is not gated to 586+; a throttled 386/486 GSW mode would
            // wrongly accept it. Gate it with the others if that fidelity gap matters.
            op if is_mmx_two_byte(op) => self.execute_mmx(bus, op, prefixes, address_size),
            0x00 => self.execute_descriptor_segment_group(bus, prefixes, address_size, opcode),
            0x01 => self.execute_descriptor_table_group(bus, prefixes, address_size, opcode),
            0x02 => self.execute_lar(bus, prefixes, address_size, operand_size),
            0x03 => self.execute_lsl(bus, prefixes, address_size, operand_size),
            0x06 => {
                // CLTS: clear the task-switched flag. Privileged.
                self.require_cpl0()?;
                self.control.cr0 &= !CR0_TS;
                Ok(clocks(2))
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
                    // CR0 is fully read/write here, so bit 18 (AM) round-trips. The 386
                    // core used to force bit 4 on (the ET extension-type bit); here ET
                    // stays whatever software writes, with no FPU modeled, so nothing is
                    // forced.
                    0 => self.control.cr0 = value,
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
            0x08 | 0x09 => {
                // INVD (08) / WBINVD (09): flush the internal caches. Both are privileged and
                // raise #UD outside CPL 0. We model no cache, so they are no-ops after the
                // privilege check. WBINVD differs only by writing dirty lines back first, which
                // has no observable effect here.
                if self.current_privilege_level() != 0 {
                    return Err(InternalFault::Exception {
                        vector: 6,
                        error_code: None,
                    });
                }
                Ok(clocks(4))
            }
            0xb0 | 0xb1 => {
                // CMPXCHG r/m, r. B0 is the byte form, B1 the word/dword form. Compare the
                // accumulator (AL/AX/EAX) with the destination exactly like CMP (acc - dest),
                // setting every ALU flag from that subtraction. If they are equal (ZF set after
                // the compare) the source register is stored into the destination; otherwise the
                // destination value is loaded into the accumulator. Either way the destination is
                // written once, which is what makes the LOCK form meaningful.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let size = if opcode == 0xb0 {
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
                Ok(clocks(6))
            }
            0xc0 | 0xc1 => {
                // XADD r/m, r. C0 is the byte form, C1 the word/dword form. The exchange-and-add
                // first saves the destination, then writes dest + src back to the destination and
                // copies the saved destination into the source register. The flags come out
                // exactly like ADD of the two operands (reuse alu_add).
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                if opcode == 0xc0 {
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
                Ok(clocks(4))
            }
            0xa2 => {
                // CPUID (0F A2). Not privileged: it runs at any CPL. The leaf selector is in
                // EAX. The result registers are EAX, EBX, ECX, EDX (full 32-bit writes). We
                // model basic leaves 0 and 1 plus the extended leaves 0x80000000 and
                // 0x80000002..0x80000004 (the brand string); any other leaf returns all zeros,
                // the architectural reply for an unimplemented leaf at or below the maximum.
                //
                // CPUID arrived on the late 486 and is standard on the 586. At the 286 and
                // 386 guest levels it does not exist, so raise #UD. (The 286-level gate above
                // already blocks it; this also covers the 386 level, which keeps the rest of
                // the 0F group but still has no CPUID.) Firmware in the BIOS ROM is exempt.
                if !self.level.has_cpuid() && !self.cs_in_firmware_rom() {
                    return Err(InternalFault::Exception {
                        vector: 6,
                        error_code: None,
                    });
                }
                let leaf = self.registers.eax();
                let (l1_kb, l2_kb) = self.level.cache_kb();
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
                    0x8000_0000 => (CPUID_MAX_EXT_LEAF, 0, 0, 0),
                    0x8000_0002 => (
                        CPUID_BRAND_EAX_0,
                        CPUID_BRAND_EBX_0,
                        CPUID_BRAND_ECX_0,
                        CPUID_BRAND_EDX_0,
                    ),
                    0x8000_0003 => (CPUID_BRAND_EAX_1, 0, 0, 0),
                    0x8000_0004 => (0, 0, 0, 0),
                    // L1 cache (AMD-style): ECX is the L1 data cache, with the size in KB in
                    // bits 31-24. The whole L1 size is reported as the data cache for this
                    // cosmetic readout; the associativity and line fields stay zero.
                    0x8000_0005 => (0, 0, (u32::from(l1_kb) & 0xff) << 24, 0),
                    // L2 cache (AMD-style): ECX carries the L2 size in KB in bits 31-16, with
                    // associativity (bits 15-12) and line size (bits 7-0) left at zero.
                    0x8000_0006 => (0, 0, (u32::from(l2_kb) & 0xffff) << 16, 0),
                    _ => (0, 0, 0, 0),
                };
                self.write_gpr32(0, eax); // EAX
                self.write_gpr32(3, ebx); // EBX
                self.write_gpr32(1, ecx); // ECX
                self.write_gpr32(2, edx); // EDX
                Ok(clocks(14))
            }
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
                Ok(clocks(1))
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
                // A prefix is idempotent: repeating 66h/67h keeps the override on,
                // it does not toggle it back off (so 66 66 op stays operand-size).
                //
                // The 66h operand-size and 67h address-size prefixes are 386
                // additions: the 286 has no 32-bit operand or address form and
                // decodes neither byte as a prefix. At the 286 guest level the core
                // raises #UD for them, which faithfully blocks every 32-bit
                // operation reached through a prefix. Code fetched from the BIOS ROM
                // is exempt (see cs_in_firmware_rom), so firmware is never blocked.
                0x66 | 0x67 if self.level.is_pre_386() && !self.cs_in_firmware_rom() => {
                    return Err(InternalFault::Exception {
                        vector: 6,
                        error_code: None,
                    });
                }
                0x66 => prefixes.operand_size_override = true,
                0x67 => prefixes.address_size_override = true,
                0xf0 => prefixes.lock = true,
                0xf3 => prefixes.rep = Some(RepKind::Repe),
                0xf2 => prefixes.rep = Some(RepKind::Repne),
                _ => {
                    self.registers.eip = eip;
                    return Ok(prefixes);
                }
            }
        }
    }

    fn peek_u8<B: CpuBus>(&mut self, bus: &mut B, offset: u32) -> ExecResult<u8> {
        self.read_memory_u8(
            bus,
            SegmentIndex::Cs,
            offset,
            BusAccessKind::InstructionPrefetch,
        )
    }

    fn check_lock_target<B: CpuBus>(&mut self, bus: &mut B, opcode: u8) -> ExecResult<()> {
        // The byte after the opcode sits at eip (the ModRM, or for 0F the second opcode byte).
        // Peeking re-reads an instruction byte; in real mode it changes no register or memory
        // state. Under paging it may set the page-table accessed bit, as the following fetch
        // would anyway.
        let eip = self.registers.eip;
        let lockable = match opcode {
            // ALU r/m, reg (destination is r/m): ADD/OR/ADC/SBB/AND/SUB/XOR, and XCHG.
            0x00 | 0x01 | 0x08 | 0x09 | 0x10 | 0x11 | 0x18 | 0x19 | 0x20 | 0x21 | 0x28 | 0x29
            | 0x30 | 0x31 | 0x86 | 0x87 => self.peek_u8(bus, eip)? >> 6 != 3,
            // Group ALU 80/81/83: /0..6 write r/m; /7 is CMP (read only, not lockable).
            0x80 | 0x81 | 0x83 => {
                let modrm = self.peek_u8(bus, eip)?;
                modrm >> 6 != 3 && (modrm >> 3) & 7 != 7
            }
            // F6/F7: /2 NOT, /3 NEG write r/m; the other sub-ops do not.
            0xf6 | 0xf7 => {
                let modrm = self.peek_u8(bus, eip)?;
                modrm >> 6 != 3 && matches!((modrm >> 3) & 7, 2 | 3)
            }
            // FE/FF: /0 INC, /1 DEC write r/m; FF /2..7 are CALL/JMP/PUSH (not lockable).
            0xfe | 0xff => {
                let modrm = self.peek_u8(bus, eip)?;
                modrm >> 6 != 3 && matches!((modrm >> 3) & 7, 0 | 1)
            }
            0x0f => {
                let second = self.peek_u8(bus, eip)?;
                match second {
                    // BTS/BTR/BTC r/m, reg write r/m; BT (A3) only reads.
                    0xab | 0xb3 | 0xbb => self.peek_u8(bus, eip.wrapping_add(1))? >> 6 != 3,
                    // BA: /5 BTS, /6 BTR, /7 BTC write; /4 BT only reads.
                    0xba => {
                        let modrm = self.peek_u8(bus, eip.wrapping_add(1))?;
                        modrm >> 6 != 3 && matches!((modrm >> 3) & 7, 5..=7)
                    }
                    // CMPXCHG (B0/B1) and XADD (C0/C1) read-modify-write the r/m destination, so
                    // LOCK is allowed only with a memory operand. The register-dest form is #UD.
                    0xb0 | 0xb1 | 0xc0 | 0xc1 => self.peek_u8(bus, eip.wrapping_add(1))? >> 6 != 3,
                    // BSWAP (C8+r) has a register destination and no memory form; INVD (08) and
                    // WBINVD (09) take no operand. LOCK on any of them is #UD (the false arm).
                    _ => false,
                }
            }
            _ => false,
        };
        if lockable {
            Ok(())
        } else {
            Err(InternalFault::Exception {
                vector: 6,
                error_code: None,
            })
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
        let physical = self.translate_segmented(bus, segment, offset, 1, false)?;
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
        let physical = self.translate_segmented(bus, segment, offset, 1, true)?;
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
        self.check_alignment(offset, size.bytes())?;
        let physical = self.translate_segmented(bus, segment, offset, size.bytes(), false)?;
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
        self.check_alignment(offset, size.bytes())?;
        let physical = self.translate_segmented(bus, segment, offset, size.bytes(), true)?;
        bus.write_memory(physical, size.bus_width(), value, kind)?;
        Ok(())
    }

    // #AC alignment check (486). A data access faults vector 17 (no error code) when
    // CR0.AM and EFLAGS.AC are both set and the access runs at CPL 3, and the effective
    // address is not naturally aligned for its width (word on a 2-byte boundary, dword on
    // a 4-byte boundary). Supervisor accesses (CPL < 3) and instruction fetches are exempt;
    // fetches never route through this helper. Byte accesses (width 1) are always aligned.
    fn check_alignment(&self, offset: u32, width: u32) -> ExecResult<()> {
        if width <= 1 {
            return Ok(());
        }
        let am = self.control.cr0 & CR0_AM != 0;
        let ac = self.registers.eflags & FLAG_AC != 0;
        if am && ac && self.current_privilege_level() == 3 && offset % width != 0 {
            // Real 486 #AC pushes a zero error code; this core models it without one,
            // matching the rest of the spec's fault contract. Flagged as a divergence.
            return Err(InternalFault::Exception {
                vector: 17,
                error_code: None,
            });
        }
        Ok(())
    }

    fn translate_segmented<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        width: u32,
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
        self.translate_linear(bus, linear, write)
    }

    fn translate_linear<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        write: bool,
    ) -> ExecResult<u32> {
        if !self.is_paging_enabled() {
            return Ok(linear);
        }

        // Paging privilege: CPL 3 is a user access, CPL 0-2 are supervisor.
        let user = self.is_protected_mode() && (self.registers.cs().selector & 3) == 3;
        // CR0.WP (a 486 addition) makes supervisor writes obey the page R/W bit too.
        // With WP clear, supervisor writes to read-only pages succeed (386 behavior).
        let wp = self.control.cr0 & CR0_WP != 0;

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
                error_code: Some(page_fault_code(false, write, user)),
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
                error_code: Some(page_fault_code(false, write, user)),
            });
        }

        // Protection check. The combined R/W and U/S come from ANDing the PDE and
        // PTE bits (bit 1 and bit 2). A page is user-accessible only if both U/S
        // bits are set, and writable only if both R/W bits are set.
        //   - A user access faults if it touches a supervisor page, or writes a
        //     read-only page.
        //   - A supervisor write faults only when CR0.WP is set and the page is
        //     read-only (combined R/W = 0). With WP clear, supervisor writes pass.
        // Either way the fault is present=1 and the error-code U/S bit reflects the
        // access (user), not the page. Checked before the dirty bit is set so a
        // faulting write leaves it clear.
        let writable = pde & pte & 0x2 != 0;
        let user_accessible = pde & pte & 0x4 != 0;
        let protection_fault = if user {
            !user_accessible || (write && !writable)
        } else {
            write && wp && !writable
        };
        if protection_fault {
            self.control.cr2 = linear;
            return Err(InternalFault::Exception {
                vector: 14,
                error_code: Some(page_fault_code(true, write, user)),
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
        let physical = self.translate_segmented(bus, segment, offset, width.bytes(), false)?;
        Ok(bus.read_memory(physical, width, BusAccessKind::DataRead)?)
    }

    fn read_string_dst<B: CpuBus>(
        &mut self,
        bus: &mut B,
        address_size: AddressSize,
        width: BusWidth,
    ) -> ExecResult<u32> {
        let offset = self.index_offset(7, address_size); // DI / EDI
        let physical =
            self.translate_segmented(bus, SegmentIndex::Es, offset, width.bytes(), false)?;
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
        let physical =
            self.translate_segmented(bus, SegmentIndex::Es, offset, width.bytes(), true)?;
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
            StringOp::Ins => {
                // INS: [ES:DI] <- port[DX]. ES cannot be overridden.
                let value = bus.read_io(self.read_gpr16(2), width)?;
                self.write_string_dst(bus, address_size, width, value)?;
                self.adjust_index_register(7, address_size, bytes);
            }
            StringOp::Outs => {
                // OUTS: port[DX] <- [DS:SI] (segment overridable).
                let value = self.read_string_src(bus, prefixes, address_size, width)?;
                bus.write_io(self.read_gpr16(2), width, value)?;
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
            self.real_mode_interrupt(bus, vector)
        }
    }

    fn real_mode_interrupt<B: CpuBus>(&mut self, bus: &mut B, vector: u8) -> ExecResult<()> {
        self.push(bus, self.registers.eflags as u16 as u32, OperandSize::Word)?;
        self.push(
            bus,
            u32::from(self.registers.cs().selector),
            OperandSize::Word,
        )?;
        self.push(bus, self.registers.eip as u16 as u32, OperandSize::Word)?;
        self.set_flag(FLAG_IF | FLAG_TF, false);
        let vector_address = u32::from(vector) * 4;
        let ip = bus.read_memory(vector_address, BusWidth::Word, BusAccessKind::DataRead)? as u16;
        let cs =
            bus.read_memory(vector_address + 2, BusWidth::Word, BusAccessKind::DataRead)? as u16;
        self.load_segment_real(SegmentIndex::Cs, cs);
        self.registers.eip = u32::from(ip);
        Ok(())
    }

    // Hardware interrupt entry. Unlike software_interrupt it does NOT call
    // interrupt_acknowledge: that hook is the software-INT device side-effect path
    // (the video mode-set), not the INTA handshake, which the PIC handled already.
    fn hardware_interrupt<B: CpuBus>(&mut self, bus: &mut B, vector: u8) -> ExecResult<()> {
        if self.is_protected_mode() {
            self.deliver_exception(bus, vector, None)
        } else {
            self.real_mode_interrupt(bus, vector)
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
        // The error code is pushed only for the vectors that carry one, regardless of
        // what the caller supplied: 8 #DF, 10 #TS, 11 #NP, 12 #SS, 13 #GP, 14 #PF, 17 #AC.
        if vector_pushes_error_code(vector) {
            self.push(bus, error_code.unwrap_or(0), OperandSize::Dword)?;
        }
        self.set_flag(FLAG_IF | FLAG_TF, false);
        self.load_segment(bus, SegmentIndex::Cs, selector)?;
        self.registers.eip = offset;
        Ok(())
    }

    fn read_system_linear_u32<B: CpuBus>(&mut self, bus: &mut B, linear: u32) -> ExecResult<u32> {
        let physical = self.translate_linear(bus, linear, false)?;
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
        // A protected-mode far call to a system descriptor goes through a call gate,
        // which supplies its own CS:offset (the instruction's offset is ignored).
        if self.is_protected_mode() {
            let (low, high) = self.read_transfer_descriptor(bus, selector)?;
            if (high >> 8) & 0x10 == 0 {
                return self.far_system_transfer(bus, selector, low, high, true);
            }
        }
        // Direct far call (real mode, or a protected-mode code segment). Push CS first
        // (higher stack address), then the return offset. RETF pops offset then CS.
        // self.registers.eip already points past the instruction.
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
        if self.is_protected_mode() {
            let (low, high) = self.read_transfer_descriptor(bus, selector)?;
            if (high >> 8) & 0x10 == 0 {
                return self.far_system_transfer(bus, selector, low, high, false);
            }
        }
        self.load_segment(bus, SegmentIndex::Cs, selector)?;
        self.registers.eip = offset & operand_size.mask();
        Ok(())
    }

    /// Dispatch a far CALL/JMP to a system descriptor: a call gate, a task gate, or a
    /// TSS (a direct task switch). `is_call` distinguishes CALL from JMP.
    fn far_system_transfer<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
        low: u32,
        high: u32,
        is_call: bool,
    ) -> ExecResult<()> {
        match (high >> 8) & 0x0f {
            0x04 | 0x0c if is_call => self.far_call_gate(bus, selector, low, high),
            0x04 | 0x0c => self.far_jump_gate(bus, selector, low, high),
            // Available 386 TSS: a direct task switch.
            0x09 => self.task_switch(bus, selector, is_call),
            // Task gate: switch to the TSS the gate names.
            0x05 => {
                let tss_selector = ((low >> 16) & 0xffff) as u16;
                self.task_switch(bus, tss_selector, is_call)
            }
            _ => Err(CpuError::GeneralProtection { selector }.into()),
        }
    }

    /// Read a descriptor for a far transfer from the GDT or LDT, faulting (#GP) on a
    /// null or out-of-range selector.
    fn read_transfer_descriptor<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
    ) -> ExecResult<(u32, u32)> {
        let in_ldt = selector & 0x4 != 0;
        let index = u32::from(selector & !0x7);
        let (base, limit) = if in_ldt {
            (self.ldtr.base, self.ldtr.limit)
        } else {
            (self.gdtr.base, u32::from(self.gdtr.limit))
        };
        if index == 0 || index + 7 > limit {
            return Err(CpuError::GeneralProtection { selector }.into());
        }
        let addr = base + index;
        let low = bus.read_memory(addr, BusWidth::Dword, BusAccessKind::DataRead)?;
        let high = bus.read_memory(addr + 4, BusWidth::Dword, BusAccessKind::DataRead)?;
        Ok((low, high))
    }

    /// Decode a call-gate descriptor into (target selector, entry offset, operand size,
    /// parameter count). 386 gates (type 0x0C) carry a 32-bit offset and a dword count;
    /// 286 gates (type 0x04) a 16-bit offset and a word count.
    fn decode_call_gate(low: u32, high: u32) -> (u16, u32, OperandSize, usize) {
        let is_32 = (high >> 8) & 0x0f == 0x0c;
        let target = ((low >> 16) & 0xffff) as u16;
        let offset = if is_32 {
            (low & 0xffff) | (high & 0xffff_0000)
        } else {
            low & 0xffff
        };
        let op = if is_32 {
            OperandSize::Dword
        } else {
            OperandSize::Word
        };
        (target, offset, op, (high & 0x1f) as usize)
    }

    fn far_call_gate<B: CpuBus>(
        &mut self,
        bus: &mut B,
        gate_selector: u16,
        low: u32,
        high: u32,
    ) -> ExecResult<()> {
        let access = (high >> 8) & 0xff;
        let gate_type = access & 0x0f;
        if gate_type != 0x0c && gate_type != 0x04 {
            return Err(CpuError::GeneralProtection {
                selector: gate_selector,
            }
            .into());
        }
        let gate_dpl = ((access >> 5) & 3) as u8;
        let cpl = self.current_privilege_level();
        let rpl = (gate_selector & 3) as u8;
        if access & 0x80 == 0 || gate_dpl < cpl.max(rpl) {
            return Err(CpuError::GeneralProtection {
                selector: gate_selector,
            }
            .into());
        }
        let (target_selector, gate_offset, op, param_count) = Self::decode_call_gate(low, high);
        let (tl, th) = self.read_transfer_descriptor(bus, target_selector)?;
        let target_access = (th >> 8) & 0xff;
        // Target must be a present code segment (S = 1 and the executable bit set).
        if target_access & 0x80 == 0 || target_access & 0x18 != 0x18 {
            return Err(CpuError::GeneralProtection {
                selector: target_selector,
            }
            .into());
        }
        let target_dpl = ((target_access >> 5) & 3) as u8;
        let conforming = target_access & 0x04 != 0;
        let mut target = self.descriptor_to_segment(target_selector, tl, th);
        let return_cs = self.registers.cs().selector;
        let return_eip = self.registers.eip;

        if !conforming && target_dpl < cpl {
            // Inter-privilege call: copy parameters off the outer stack, switch to the
            // inner stack from the TSS, then rebuild the frame there.
            let mut params = [0u32; 32];
            let psize = op.bytes();
            let outer_esp = self.registers.esp();
            for (k, slot) in params.iter_mut().enumerate().take(param_count) {
                *slot = self.read_memory_sized(
                    bus,
                    SegmentIndex::Ss,
                    outer_esp + k as u32 * psize,
                    op,
                    BusAccessKind::DataRead,
                )?;
            }
            let (old_ss, old_esp) = self.switch_to_inner_stack(bus, target_dpl)?;
            self.push(bus, u32::from(old_ss), op)?;
            self.push(bus, old_esp, op)?;
            for k in (0..param_count).rev() {
                self.push(bus, params[k], op)?;
            }
            self.push(bus, u32::from(return_cs), op)?;
            self.push(bus, return_eip, op)?;
            target.selector = (target_selector & !3) | u16::from(target_dpl);
        } else {
            // Same privilege (or a conforming target): push the return frame on the
            // current stack.
            self.push(bus, u32::from(return_cs), op)?;
            self.push(bus, return_eip, op)?;
            target.selector = (target_selector & !3) | u16::from(cpl);
        }
        self.registers.set_segment(SegmentIndex::Cs, target);
        self.registers.eip = gate_offset & op.mask();
        Ok(())
    }

    fn far_jump_gate<B: CpuBus>(
        &mut self,
        bus: &mut B,
        gate_selector: u16,
        low: u32,
        high: u32,
    ) -> ExecResult<()> {
        let access = (high >> 8) & 0xff;
        let gate_type = access & 0x0f;
        if gate_type != 0x0c && gate_type != 0x04 {
            return Err(CpuError::GeneralProtection {
                selector: gate_selector,
            }
            .into());
        }
        let gate_dpl = ((access >> 5) & 3) as u8;
        let cpl = self.current_privilege_level();
        let rpl = (gate_selector & 3) as u8;
        if access & 0x80 == 0 || gate_dpl < cpl.max(rpl) {
            return Err(CpuError::GeneralProtection {
                selector: gate_selector,
            }
            .into());
        }
        let (target_selector, gate_offset, op, _) = Self::decode_call_gate(low, high);
        let (tl, th) = self.read_transfer_descriptor(bus, target_selector)?;
        let target_access = (th >> 8) & 0xff;
        if target_access & 0x80 == 0 || target_access & 0x18 != 0x18 {
            return Err(CpuError::GeneralProtection {
                selector: target_selector,
            }
            .into());
        }
        let target_dpl = ((target_access >> 5) & 3) as u8;
        let conforming = target_access & 0x04 != 0;
        // A JMP through a gate cannot change privilege: a non-conforming target must be
        // at the current level; a conforming one no more privileged.
        if (!conforming && target_dpl != cpl) || (conforming && target_dpl > cpl) {
            return Err(CpuError::GeneralProtection {
                selector: target_selector,
            }
            .into());
        }
        let mut target = self.descriptor_to_segment(target_selector, tl, th);
        target.selector = (target_selector & !3) | u16::from(cpl);
        self.registers.set_segment(SegmentIndex::Cs, target);
        self.registers.eip = gate_offset & op.mask();
        Ok(())
    }

    /// Switch to the inner-ring stack for `target_dpl`, read from the current TSS
    /// (386 layout: ESPn at 4 + 8n, SSn at 8 + 8n). Returns the outgoing SS:ESP.
    fn switch_to_inner_stack<B: CpuBus>(
        &mut self,
        bus: &mut B,
        target_dpl: u8,
    ) -> ExecResult<(u16, u32)> {
        let old_ss = self.registers.segment(SegmentIndex::Ss).selector;
        let old_esp = self.registers.esp();
        let esp_addr = self.tr.base + 4 + 8 * u32::from(target_dpl);
        let new_esp = bus.read_memory(esp_addr, BusWidth::Dword, BusAccessKind::DataRead)?;
        let new_ss = bus.read_memory(esp_addr + 4, BusWidth::Word, BusAccessKind::DataRead)? as u16;
        self.load_segment(bus, SegmentIndex::Ss, new_ss)?;
        self.registers.set_esp(new_esp);
        Ok((old_ss, old_esp))
    }

    /// 386 hardware task switch. Saves the outgoing task's state into the current TSS,
    /// loads the incoming one, juggles the busy bits, and (for a CALL) links back to
    /// the caller and sets NT. ponytail: TSS memory is accessed unpaged, and the 286
    /// (short) TSS form is not modeled.
    fn task_switch<B: CpuBus>(
        &mut self,
        bus: &mut B,
        new_selector: u16,
        is_call: bool,
    ) -> ExecResult<()> {
        let (low, high) = self.read_transfer_descriptor(bus, new_selector)?;
        let access = (high >> 8) & 0xff;
        // Present, available 386 TSS (type 0x09). Busy or wrong type is #GP.
        if access & 0x80 == 0 || access & 0x1f != 0x09 {
            return Err(CpuError::GeneralProtection {
                selector: new_selector,
            }
            .into());
        }
        let new_tss = self.descriptor_to_segment(new_selector, low, high);
        let old_selector = self.tr.selector;

        self.save_task_state(bus)?;
        if !is_call {
            self.set_tss_busy(bus, old_selector, false)?;
        }
        self.load_task_state(bus, new_tss.base)?;
        if is_call {
            // Write the back-link and set NT so the inner IRET returns to the caller.
            bus.write_memory(
                new_tss.base,
                BusWidth::Word,
                u32::from(old_selector),
                BusAccessKind::DataWrite,
            )?;
            self.registers.eflags |= FLAG_NT;
        }
        self.set_tss_busy(bus, new_selector, true)?;
        self.tr = new_tss;
        self.tr.access |= 0x02;
        self.control.cr0 |= CR0_TS;
        Ok(())
    }

    fn save_task_state<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<()> {
        let base = self.tr.base;
        bus.write_memory(
            base + 32,
            BusWidth::Dword,
            self.registers.eip,
            BusAccessKind::DataWrite,
        )?;
        bus.write_memory(
            base + 36,
            BusWidth::Dword,
            self.registers.eflags,
            BusAccessKind::DataWrite,
        )?;
        for i in 0..8u32 {
            bus.write_memory(
                base + 40 + i * 4,
                BusWidth::Dword,
                self.read_gpr32(i as u8),
                BusAccessKind::DataWrite,
            )?;
        }
        for (k, segment) in TASK_SEGMENTS.iter().enumerate() {
            bus.write_memory(
                base + 72 + k as u32 * 4,
                BusWidth::Word,
                u32::from(self.registers.segment(*segment).selector),
                BusAccessKind::DataWrite,
            )?;
        }
        bus.write_memory(
            base + 96,
            BusWidth::Word,
            u32::from(self.ldtr.selector),
            BusAccessKind::DataWrite,
        )?;
        Ok(())
    }

    fn load_task_state<B: CpuBus>(&mut self, bus: &mut B, base: u32) -> ExecResult<()> {
        if self.control.cr0 & CR0_PG != 0 {
            self.control.cr3 =
                bus.read_memory(base + 28, BusWidth::Dword, BusAccessKind::DataRead)?;
        }
        // The LDTR is loaded first so segment loads that reference the LDT resolve.
        let ldtr = bus.read_memory(base + 96, BusWidth::Word, BusAccessKind::DataRead)? as u16;
        self.load_ldtr(bus, ldtr)?;
        let eip = bus.read_memory(base + 32, BusWidth::Dword, BusAccessKind::DataRead)?;
        let eflags = bus.read_memory(base + 36, BusWidth::Dword, BusAccessKind::DataRead)?;
        for i in 0..8u32 {
            let value =
                bus.read_memory(base + 40 + i * 4, BusWidth::Dword, BusAccessKind::DataRead)?;
            self.write_gpr32(i as u8, value);
        }
        self.registers.eflags = eflags | 0x2;
        self.registers.eip = eip;
        for (k, segment) in TASK_SEGMENTS.iter().enumerate() {
            let selector = bus.read_memory(
                base + 72 + k as u32 * 4,
                BusWidth::Word,
                BusAccessKind::DataRead,
            )? as u16;
            // A null data segment (ES/DS/FS/GS) is legal and just unusable; CS and SS
            // must be loadable.
            if selector & !0x7 == 0 && !matches!(segment, SegmentIndex::Cs | SegmentIndex::Ss) {
                self.registers.set_segment(
                    *segment,
                    SegmentRegister {
                        selector,
                        ..Default::default()
                    },
                );
            } else {
                self.load_segment(bus, *segment, selector)?;
            }
        }
        Ok(())
    }

    fn set_tss_busy<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
        busy: bool,
    ) -> ExecResult<()> {
        if selector & !0x7 == 0 {
            return Ok(());
        }
        let addr = self.gdtr.base + u32::from(selector & !0x7) + 5;
        let mut access = bus.read_memory(addr, BusWidth::Byte, BusAccessKind::DataRead)? as u8;
        if busy {
            access |= 0x02;
        } else {
            access &= !0x02;
        }
        bus.write_memory(
            addr,
            BusWidth::Byte,
            u32::from(access),
            BusAccessKind::DataWrite,
        )?;
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
        Ok(self.descriptor_to_segment(selector, low, high))
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

    /// True when the interrupt flag is set, so a maskable interrupt can be taken.
    pub fn interrupts_enabled(&self) -> bool {
        self.flag(FLAG_IF)
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

/// True when a second-byte 0F opcode is one the 386 or a later part introduced and
/// the core executes. Used to raise #UD at the 286 guest level. The 286-era 0F
/// opcodes the core supports (0F 01 LGDT/LIDT) return false and stay allowed.
const fn is_386plus_two_byte(opcode: u8) -> bool {
    matches!(
        opcode,
        // 486 cache/atomic group: INVD, WBINVD, CMPXCHG, XADD, BSWAP.
        0x08 | 0x09 | 0xb0 | 0xb1 | 0xc0 | 0xc1 | 0xc8..=0xcf
        // MOV to/from CR (386).
        | 0x20 | 0x22
        // Jcc rel16/32 and SETcc (386).
        | 0x80..=0x8f | 0x90..=0x9f
        // CPUID (gated again by level below; 286 has no CPUID either way).
        | 0xa2
        // SHLD/SHRD, BT group r/m+reg, IMUL r,r/m (386).
        | 0xa3 | 0xa4 | 0xa5 | 0xab | 0xac | 0xad | 0xaf
        // BT/BTS/BTR/BTC r/m+imm8 and r/m+reg, MOVZX/MOVSX, BSF/BSR (386).
        | 0xb3 | 0xba | 0xbb | 0xb6 | 0xb7 | 0xbc | 0xbd | 0xbe | 0xbf
    )
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

/// The processor pushes a 32-bit error code after the return frame for these
/// vectors: 8 #DF, 10 #TS, 11 #NP, 12 #SS, 13 #GP, 14 #PF, 17 #AC. Every other
/// vector (including the software INT n forms) pushes no error code.
const fn vector_pushes_error_code(vector: u8) -> bool {
    matches!(vector, 8 | 10 | 11 | 12 | 13 | 14 | 17)
}

fn page_fault_code(present: bool, write: bool, user: bool) -> u32 {
    // 80386 page-fault error code: bit 0 = P (present), bit 1 = W/R (was a write),
    // bit 2 = U/S (was a user access). The reserved-bit (bit 3, 486+) and
    // instruction-fetch (bit 4, P6+) bits are later additions a 386 never sets.
    u32::from(present) | (u32::from(write) << 1) | (u32::from(user) << 2)
}

/// Two-operand x87 arithmetic. `op` is the group-1 /digit: 0 add, 1 mul, 4 sub
/// (a-b), 5 reverse-sub (b-a), 6 div (a/b), 7 reverse-div (b/a). 2 and 3 are the
/// compare encodings and are handled by the caller, never here.
fn fpu_arith(op: u8, a: f64, b: f64) -> f64 {
    match op {
        0 => a + b,
        1 => a * b,
        4 => a - b,
        5 => b - a,
        6 => a / b,
        7 => b / a,
        _ => a,
    }
}

/// FIST/FISTP rounding. ponytail: round-to-nearest-even only; the control-word RC
/// field is not yet honored (it almost always is round-to-nearest anyway).
fn fpu_round_to_i64(value: f64) -> i64 {
    value.round_ties_even() as i64
}

impl Cpu386 {
    // ============================ x87 FPU (387-class) ============================
    // Escape opcodes 0xD8-0xDF. Registers are f64 (see fpu.rs for the precision
    // ceiling). Transcendental functions, 80-bit-extended and BCD memory operands,
    // integer-operand arithmetic (the FIADD family on 0xDA/0xDE memory), FCMOVcc,
    // and the environment save/restore set are not implemented yet and return
    // UnsupportedOpcode so they stay visible rather than silently wrong. See
    // dev_docs/coverage-roadmap.md phase 2.

    fn execute_fpu<B: CpuBus>(
        &mut self,
        bus: &mut B,
        opcode: u8,
        prefixes: Prefixes,
        address_size: AddressSize,
    ) -> ExecResult<CycleOutcome> {
        let modrm = self.fetch_modrm(bus)?;
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
        if modrm.mode == 3 {
            self.execute_fpu_register(opcode, modrm)
        } else {
            let mem = match self.decode_rm_operand(bus, prefixes, address_size, modrm)? {
                RmOperand::Memory(memory) => memory,
                RmOperand::Register(_) => unreachable!("mode != 3 decodes to a memory operand"),
            };
            let operand_size = self.operand_size(prefixes);
            self.execute_fpu_memory(bus, opcode, modrm.reg, mem, operand_size)
        }
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
            0xda => {
                if byte == 0xe9 {
                    // FUCOMPP: unordered compare ST(0) with ST(1), then pop both.
                    let a = self.fpu.get(0);
                    let b = self.fpu.get(1);
                    self.fpu_compare(a, b);
                    self.fpu.pop();
                    self.fpu.pop();
                    Ok(clocks(5))
                } else {
                    self.fpu_unsupported(opcode) // FCMOVcc (P6) deferred to phase 5
                }
            }
            0xd9 => self.fpu_d9_register(byte, i),
            0xdb => self.fpu_db_register(byte),
            0xdd => self.fpu_dd_register(reg, i),
            0xdf => {
                if byte == 0xe0 {
                    // FNSTSW AX.
                    let sw = self.fpu.status;
                    self.write_gpr16(0, sw);
                    Ok(clocks(3))
                } else {
                    self.fpu_unsupported(opcode)
                }
            }
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
    /// op. ponytail: only these two classes are detected from the f64 result; overflow,
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
                let v = self.fpu.get(0).round_ties_even();
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
        // ponytail: computes the full remainder in one step and reports reduction
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
            _ => self.fpu_unsupported(0xdb), // FCMOVcc (P6) deferred
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
                // FUCOM / FUCOMP. ponytail: treated like FCOM/FCOMP; the unordered-vs-
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

    fn fpu_unsupported(&self, opcode: u8) -> ExecResult<CycleOutcome> {
        Err(CpuError::UnsupportedOpcode {
            opcode,
            cs: self.registers.cs().selector,
            eip: self.registers.eip,
        }
        .into())
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
    /// FILD m64, the FLD m80 mantissa, FNSAVE/FRSTOR, and the MMX 64-bit transfers.
    fn read_qword<B: CpuBus>(&mut self, bus: &mut B, mem: MemoryOperand) -> ExecResult<u64> {
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

    fn write_qword<B: CpuBus>(
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
        let v = fpu_round_to_i64(value) as i16 as u16;
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
        let v = fpu_round_to_i64(value) as i32 as u32;
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
        self.write_qword(bus, mem, fpu_round_to_i64(value) as u64)
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
                // Subnormal f64. ponytail: rare path normalized by scaling, not exact bits.
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
        // ponytail: the instruction and data pointers (the last four env slots) are
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
        // ponytail: registers are saved in stack order ST(0)..ST(7); hardware uses
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
        let negative = value.is_sign_negative();
        let mut magnitude = fpu_round_to_i64(value.abs()).unsigned_abs();
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

/// The base MMX second-byte opcodes (after 0F). Excludes the SSE/SSE2 additions
/// that share the integer-SIMD ranges (PADDQ 0F D4, PAVGB 0F E0, etc.).
const fn is_mmx_two_byte(opcode: u8) -> bool {
    matches!(
        opcode,
        0x60..=0x6b
            | 0x6e
            | 0x6f
            | 0x71..=0x77
            | 0x7e
            | 0x7f
            | 0xd1..=0xd3
            | 0xd5
            | 0xd8
            | 0xd9
            | 0xdb
            | 0xdc
            | 0xdd
            | 0xdf
            | 0xe1
            | 0xe2
            | 0xe5
            | 0xe8
            | 0xe9
            | 0xeb
            | 0xec
            | 0xed
            | 0xef
            | 0xf1..=0xf3
            | 0xf5
            | 0xf8..=0xfa
            | 0xfc..=0xfe
    )
}

impl Cpu386 {
    // ================================ MMX ================================
    // 0F-extended integer SIMD on the eight 64-bit MMX registers (mmx.rs holds the
    // lane math). EMMS takes no operand; the shift-by-immediate forms (0F 71/72/73)
    // and MOVD/MOVQ have their own operand shapes; everything else is the regular
    // Pxxx mm, mm/m64 form.

    fn execute_mmx<B: CpuBus>(
        &mut self,
        bus: &mut B,
        opcode: u8,
        prefixes: Prefixes,
        address_size: AddressSize,
    ) -> ExecResult<CycleOutcome> {
        if opcode == 0x77 {
            self.fpu.emms();
            return Ok(clocks(6));
        }
        let modrm = self.fetch_modrm(bus)?;

        if matches!(opcode, 0x71..=0x73) {
            // Shift mm by an immediate; modrm.reg selects the shift, modrm.rm the register.
            let count = u64::from(self.fetch_u8(bus)?);
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

        let dest = modrm.reg;
        match opcode {
            0x6e => {
                // MOVD mm, r/m32: zero-extend a dword into the register.
                let v =
                    self.read_rm_sized(bus, prefixes, address_size, OperandSize::Dword, modrm)?;
                self.fpu.set_mm(dest, u64::from(v));
                return Ok(clocks(4));
            }
            0x7e => {
                // MOVD r/m32, mm: store the low dword.
                let v = self.fpu.mm(dest) as u32;
                self.write_rm_sized(bus, prefixes, address_size, OperandSize::Dword, modrm, v)?;
                return Ok(clocks(4));
            }
            0x6f => {
                // MOVQ mm, mm/m64.
                let v = self.read_mmx_operand(bus, prefixes, address_size, modrm)?;
                self.fpu.set_mm(dest, v);
                return Ok(clocks(4));
            }
            0x7f => {
                // MOVQ mm/m64, mm.
                let v = self.fpu.mm(dest);
                self.write_mmx_operand(bus, prefixes, address_size, modrm, v)?;
                return Ok(clocks(4));
            }
            _ => {}
        }

        let src = self.read_mmx_operand(bus, prefixes, address_size, modrm)?;
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

    fn read_mmx_operand<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        modrm: ModRm,
    ) -> ExecResult<u64> {
        match self.decode_rm_operand(bus, prefixes, address_size, modrm)? {
            RmOperand::Register(index) => Ok(self.fpu.mm(index)),
            RmOperand::Memory(mem) => self.read_qword(bus, mem),
        }
    }

    fn write_mmx_operand<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        modrm: ModRm,
        value: u64,
    ) -> ExecResult<()> {
        match self.decode_rm_operand(bus, prefixes, address_size, modrm)? {
            RmOperand::Register(index) => {
                self.fpu.set_mm(index, value);
                Ok(())
            }
            RmOperand::Memory(mem) => self.write_qword(bus, mem, value),
        }
    }
}

impl Cpu386 {
    // ===================== Protected-mode system instructions =====================
    // The 0F 00 / 0F 01 groups plus LAR/LSL/CLTS. LDTR/TR live in the CPU state; the
    // segment-verify and access-rights instructions read descriptors from the GDT or
    // the LDT and never fault on a bad selector (they clear ZF instead).

    fn require_cpl0(&self) -> ExecResult<()> {
        if self.current_privilege_level() != 0 {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(0),
            });
        }
        Ok(())
    }

    /// Decode an 8-byte descriptor into a cached segment register. Shared by the
    /// segment loader and the system-register loads.
    fn descriptor_to_segment(&self, selector: u16, low: u32, high: u32) -> SegmentRegister {
        let access = ((high >> 8) & 0xff) as u8;
        let base = ((low >> 16) & 0xffff) | ((high & 0x0000_00ff) << 16) | (high & 0xff00_0000);
        let mut limit = (low & 0xffff) | (high & 0x000f_0000);
        if high & 0x0080_0000 != 0 {
            limit = (limit << 12) | 0x0fff;
        }
        SegmentRegister {
            selector,
            base,
            limit,
            access,
            default_size_32: high & 0x0040_0000 != 0,
        }
    }

    /// Read a descriptor for VERR/VERW/LAR/LSL: from the GDT or the LDT, returning
    /// None (rather than faulting) for a null or out-of-range selector.
    fn try_read_descriptor<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
    ) -> ExecResult<Option<(u32, u32)>> {
        let in_ldt = selector & 0x4 != 0;
        let index = u32::from(selector & !0x7);
        let (base, limit) = if in_ldt {
            (self.ldtr.base, self.ldtr.limit)
        } else {
            if index == 0 {
                return Ok(None);
            }
            (self.gdtr.base, u32::from(self.gdtr.limit))
        };
        if index + 7 > limit {
            return Ok(None);
        }
        let addr = base + index;
        let low = bus.read_memory(addr, BusWidth::Dword, BusAccessKind::DataRead)?;
        let high = bus.read_memory(addr + 4, BusWidth::Dword, BusAccessKind::DataRead)?;
        Ok(Some((low, high)))
    }

    /// Read a GDT descriptor for LLDT/LTR, which #GP on a null or out-of-range selector.
    fn read_gdt_descriptor<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
    ) -> ExecResult<(u32, u32)> {
        let index = u32::from(selector & !0x7);
        if index == 0 || index + 7 > u32::from(self.gdtr.limit) {
            return Err(CpuError::GeneralProtection { selector }.into());
        }
        let addr = self.gdtr.base + index;
        let low = bus.read_memory(addr, BusWidth::Dword, BusAccessKind::DataRead)?;
        let high = bus.read_memory(addr + 4, BusWidth::Dword, BusAccessKind::DataRead)?;
        Ok((low, high))
    }

    fn execute_descriptor_table_group<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        opcode: u8,
    ) -> ExecResult<CycleOutcome> {
        let modrm = self.fetch_modrm(bus)?;
        match modrm.reg {
            4 => {
                // SMSW r/m16: store the machine status word (low 16 bits of CR0).
                let msw = self.control.cr0 as u16;
                self.write_rm_sized(
                    bus,
                    prefixes,
                    address_size,
                    OperandSize::Word,
                    modrm,
                    u32::from(msw),
                )?;
                Ok(clocks(2))
            }
            6 => {
                // LMSW r/m16: load MP/EM/TS; PE can be set but not cleared. Privileged.
                self.require_cpl0()?;
                let msw =
                    self.read_rm_sized(bus, prefixes, address_size, OperandSize::Word, modrm)?;
                let switchable = CR0_MP | CR0_EM | CR0_TS;
                let mut cr0 = (self.control.cr0 & !switchable) | (msw & switchable);
                if msw & CR0_PE != 0 {
                    cr0 |= CR0_PE;
                }
                self.control.cr0 = cr0;
                Ok(clocks(3))
            }
            reg => {
                let memory = match self.decode_rm_operand(bus, prefixes, address_size, modrm)? {
                    RmOperand::Memory(memory) => memory,
                    RmOperand::Register(_) => {
                        // SGDT/SIDT/LGDT/LIDT/INVLPG all require a memory operand.
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
                        Ok(clocks(11))
                    }
                    1 => {
                        // SIDT m: store the IDTR pseudo-descriptor.
                        self.store_descriptor_table(bus, memory, self.idtr)?;
                        Ok(clocks(11))
                    }
                    2 | 3 => {
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
                        Ok(clocks(11))
                    }
                    7 => {
                        // INVLPG m: privileged on the 486. We model no TLB, so it is a no-op
                        // after the privilege check.
                        if self.current_privilege_level() != 0 {
                            return Err(InternalFault::Exception {
                                vector: 6,
                                error_code: None,
                            });
                        }
                        Ok(clocks(12))
                    }
                    _ => Err(CpuError::UnsupportedGroupOpcode {
                        opcode,
                        extension: reg,
                    }
                    .into()),
                }
            }
        }
    }

    fn store_descriptor_table<B: CpuBus>(
        &mut self,
        bus: &mut B,
        memory: MemoryOperand,
        table: DescriptorTable,
    ) -> ExecResult<()> {
        // ponytail: the 16-bit-operand quirk (base masked to 24 bits) is not modeled;
        // the full 32-bit base is always stored.
        self.write_memory_sized(
            bus,
            memory.segment,
            memory.offset,
            OperandSize::Word,
            u32::from(table.limit),
            BusAccessKind::DataWrite,
        )?;
        self.write_memory_sized(
            bus,
            memory.segment,
            memory.offset + 2,
            OperandSize::Dword,
            table.base,
            BusAccessKind::DataWrite,
        )
    }

    fn execute_descriptor_segment_group<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        opcode: u8,
    ) -> ExecResult<CycleOutcome> {
        // The whole 0F 00 group is invalid outside protected mode.
        if !self.is_protected_mode() {
            return Err(InternalFault::Exception {
                vector: 6,
                error_code: None,
            });
        }
        let modrm = self.fetch_modrm(bus)?;
        match modrm.reg {
            0 => {
                // SLDT r/m16: store the LDTR selector.
                let selector = u32::from(self.ldtr.selector);
                self.write_rm_sized(
                    bus,
                    prefixes,
                    address_size,
                    OperandSize::Word,
                    modrm,
                    selector,
                )?;
                Ok(clocks(2))
            }
            1 => {
                // STR r/m16: store the task-register selector.
                let selector = u32::from(self.tr.selector);
                self.write_rm_sized(
                    bus,
                    prefixes,
                    address_size,
                    OperandSize::Word,
                    modrm,
                    selector,
                )?;
                Ok(clocks(2))
            }
            2 => {
                // LLDT r/m16: load the local descriptor table register. Privileged.
                self.require_cpl0()?;
                let selector =
                    self.read_rm_sized(bus, prefixes, address_size, OperandSize::Word, modrm)?
                        as u16;
                self.load_ldtr(bus, selector)?;
                Ok(clocks(11))
            }
            3 => {
                // LTR r/m16: load the task register. Privileged.
                self.require_cpl0()?;
                let selector =
                    self.read_rm_sized(bus, prefixes, address_size, OperandSize::Word, modrm)?
                        as u16;
                self.load_tr(bus, selector)?;
                Ok(clocks(11))
            }
            4 | 5 => {
                // VERR (/4) / VERW (/5): set ZF if the segment is readable / writable.
                let selector =
                    self.read_rm_sized(bus, prefixes, address_size, OperandSize::Word, modrm)?
                        as u16;
                let ok = self.verify_segment(bus, selector, modrm.reg == 5)?;
                self.set_flag(FLAG_ZF, ok);
                Ok(clocks(10))
            }
            reg => Err(CpuError::UnsupportedGroupOpcode {
                opcode,
                extension: reg,
            }
            .into()),
        }
    }

    fn load_ldtr<B: CpuBus>(&mut self, bus: &mut B, selector: u16) -> ExecResult<()> {
        if selector & 0x4 != 0 {
            // The LDT descriptor must live in the GDT (TI = 0).
            return Err(CpuError::GeneralProtection { selector }.into());
        }
        if selector & !0x7 == 0 {
            // A null selector marks the LDTR invalid.
            self.ldtr = SegmentRegister {
                selector,
                ..Default::default()
            };
            return Ok(());
        }
        let (low, high) = self.read_gdt_descriptor(bus, selector)?;
        let access = (high >> 8) & 0xff;
        // Present LDT system descriptor (S = 0, type = 2).
        if access & 0x80 == 0 || access & 0x1f != 0x02 {
            return Err(CpuError::GeneralProtection { selector }.into());
        }
        self.ldtr = self.descriptor_to_segment(selector, low, high);
        Ok(())
    }

    fn load_tr<B: CpuBus>(&mut self, bus: &mut B, selector: u16) -> ExecResult<()> {
        if selector & 0x4 != 0 || selector & !0x7 == 0 {
            return Err(CpuError::GeneralProtection { selector }.into());
        }
        let (low, high) = self.read_gdt_descriptor(bus, selector)?;
        let access = (high >> 8) & 0xff;
        let descriptor_type = access & 0x1f;
        // Present available TSS: type 1 (286) or 9 (386).
        if access & 0x80 == 0 || (descriptor_type != 0x01 && descriptor_type != 0x09) {
            return Err(CpuError::GeneralProtection { selector }.into());
        }
        let mut segment = self.descriptor_to_segment(selector, low, high);
        // Mark the TSS busy, both in the cache and back in the GDT descriptor.
        segment.access |= 0x02;
        let index = u32::from(selector & !0x7);
        let access_byte = (access | 0x02) as u8;
        bus.write_memory(
            self.gdtr.base + index + 5,
            BusWidth::Byte,
            u32::from(access_byte),
            BusAccessKind::DataWrite,
        )?;
        self.tr = segment;
        Ok(())
    }

    fn verify_segment<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
        write: bool,
    ) -> ExecResult<bool> {
        let Some((_, high)) = self.try_read_descriptor(bus, selector)? else {
            return Ok(false);
        };
        let access = (high >> 8) & 0xff;
        let present = access & 0x80 != 0;
        let is_segment = access & 0x10 != 0; // S bit
        if !present || !is_segment {
            return Ok(false);
        }
        let descriptor_type = access & 0x0f;
        let is_code = descriptor_type & 0x8 != 0;
        let dpl = ((access >> 5) & 3) as u8;
        let rpl = (selector & 3) as u8;
        let privilege_ok = dpl >= self.current_privilege_level().max(rpl);
        let ok = if write {
            // VERW: writable data segment.
            !is_code && descriptor_type & 0x2 != 0 && privilege_ok
        } else {
            // VERR: readable. Data is always readable; code needs the readable bit.
            // Conforming code skips the privilege check.
            let readable = if is_code {
                descriptor_type & 0x2 != 0
            } else {
                true
            };
            let conforming = is_code && descriptor_type & 0x4 != 0;
            readable && (conforming || privilege_ok)
        };
        Ok(ok)
    }

    fn descriptor_accessible(&self, selector: u16, high: u32) -> bool {
        let access = (high >> 8) & 0xff;
        if access & 0x80 == 0 {
            return false; // not present
        }
        let dpl = ((access >> 5) & 3) as u8;
        let rpl = (selector & 3) as u8;
        let is_segment = access & 0x10 != 0;
        let descriptor_type = access & 0x0f;
        let conforming_code =
            is_segment && descriptor_type & 0x8 != 0 && descriptor_type & 0x4 != 0;
        // Conforming code is reachable from any privilege; everything else needs
        // DPL >= max(CPL, RPL).
        conforming_code || dpl >= self.current_privilege_level().max(rpl)
    }

    fn execute_lar<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        operand_size: OperandSize,
    ) -> ExecResult<CycleOutcome> {
        if !self.is_protected_mode() {
            return Err(InternalFault::Exception {
                vector: 6,
                error_code: None,
            });
        }
        let modrm = self.fetch_modrm(bus)?;
        let selector =
            self.read_rm_sized(bus, prefixes, address_size, OperandSize::Word, modrm)? as u16;
        match self.try_read_descriptor(bus, selector)? {
            Some((_, high)) if self.descriptor_accessible(selector, high) => {
                // The access-rights bytes: the access byte plus, for a dword operand, the
                // G/D/B/AVL attribute nibble.
                let mask = match operand_size {
                    OperandSize::Word => 0x0000_ff00,
                    OperandSize::Dword => 0x00f0_ff00,
                };
                self.write_gpr_sized(modrm.reg, operand_size, high & mask);
                self.set_flag(FLAG_ZF, true);
            }
            _ => self.set_flag(FLAG_ZF, false),
        }
        Ok(clocks(11))
    }

    fn execute_lsl<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        operand_size: OperandSize,
    ) -> ExecResult<CycleOutcome> {
        if !self.is_protected_mode() {
            return Err(InternalFault::Exception {
                vector: 6,
                error_code: None,
            });
        }
        let modrm = self.fetch_modrm(bus)?;
        let selector =
            self.read_rm_sized(bus, prefixes, address_size, OperandSize::Word, modrm)? as u16;
        match self.try_read_descriptor(bus, selector)? {
            Some((low, high)) if self.descriptor_accessible(selector, high) => {
                let mut limit = (low & 0xffff) | (high & 0x000f_0000);
                if high & 0x0080_0000 != 0 {
                    limit = (limit << 12) | 0x0fff;
                }
                self.write_gpr_sized(modrm.reg, operand_size, limit);
                self.set_flag(FLAG_ZF, true);
            }
            _ => self.set_flag(FLAG_ZF, false),
        }
        Ok(clocks(11))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use izarravm_bus::{BusCycle, BusTrace, BusWidth};

    #[derive(Default)]
    struct TestBus {
        memory: Vec<u8>,
        trace: BusTrace,
        pending_irq: Option<u8>,
    }

    impl TestBus {
        fn with_memory(memory: Vec<u8>) -> Self {
            Self {
                memory,
                trace: BusTrace::default(),
                pending_irq: None,
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

        fn interrupt_pending(&self) -> bool {
            self.pending_irq.is_some()
        }

        fn acknowledge_interrupt(&mut self) -> Option<u8> {
            self.pending_irq.take()
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
    fn moffs_loads_al_from_direct_offset() {
        // mov al, [0x0200] (0xa0 0x00 0x02). Byte form ignores the operand-size
        // prefix and touches only AL. It must not disturb flags.
        let mut memory = vec![0; 1024];
        memory[0..3].copy_from_slice(&[0xa0, 0x00, 0x02]);
        memory[0x200] = 0x7e;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x11ff);
        let flags_before = cpu.registers.eflags;
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        // AL replaced, AH preserved, instruction is three bytes long.
        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x117e);
        assert_eq!(cpu.registers.eip, 3);
        assert_eq!(cpu.registers.eflags, flags_before);
    }

    #[test]
    fn moffs_stores_al_to_direct_offset() {
        // mov [0x0200], al (0xa2 0x00 0x02). Byte form writes only one byte and
        // leaves flags alone.
        let mut memory = vec![0; 1024];
        memory[0..3].copy_from_slice(&[0xa2, 0x00, 0x02]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x22a5);
        let flags_before = cpu.registers.eflags;
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(bus.memory[0x200], 0xa5);
        // The neighbouring byte is untouched by a byte store.
        assert_eq!(bus.memory[0x201], 0x00);
        assert_eq!(cpu.registers.eip, 3);
        assert_eq!(cpu.registers.eflags, flags_before);
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
    fn user_mode_paging_respects_the_supervisor_bit() {
        // PD at 0x1000, PT at 0x2000. Linear 0x3000 maps to a present, writable,
        // supervisor (U/S=0) page at frame 0x5000.
        let mut memory = vec![0; 0x6000];
        memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes()); // PDE: PT, present+rw+user
        memory[0x200c..0x2010].copy_from_slice(&0x0000_5003u32.to_le_bytes()); // PTE[3]: frame, present+rw, U/S=0
        let mut cpu = Cpu386::default();
        cpu.control.cr0 |= CR0_PE | CR0_PG;
        cpu.control.cr3 = 0x1000;
        let flat_cs = |rpl| SegmentRegister {
            selector: rpl,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: false,
        };
        let mut bus = TestBus::with_memory(memory);

        // CPL 3: a user read of the supervisor page faults with #PF, error code
        // present|user (0b101 = 0x5), and cr2 set to the faulting linear address.
        cpu.registers.set_segment(SegmentIndex::Cs, flat_cs(0x0003));
        let faulted = cpu.translate_linear(&mut bus, 0x3000, false);
        assert!(
            matches!(
                faulted,
                Err(InternalFault::Exception {
                    vector: 14,
                    error_code: Some(0x5)
                })
            ),
            "{faulted:?}"
        );
        assert_eq!(cpu.control.cr2, 0x3000);

        // CPL 0: a 386 has no CR0.WP, so supervisor reaches the same page fine.
        cpu.registers.set_segment(SegmentIndex::Cs, flat_cs(0x0000));
        assert_eq!(
            cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
            0x5000
        );
    }

    #[test]
    fn cr0_wp_gates_supervisor_writes_to_read_only_pages() {
        // PD at 0x1000, PT at 0x2000. Linear 0x3000 maps to a present, read-only
        // (R/W=0), supervisor (U/S=0) page at frame 0x5000.
        let mut memory = vec![0; 0x6000];
        memory[0x1000..0x1004].copy_from_slice(&0x0000_2001u32.to_le_bytes()); // PDE: PT, present, R/W=0, U/S=0
        memory[0x200c..0x2010].copy_from_slice(&0x0000_5001u32.to_le_bytes()); // PTE[3]: frame, present, R/W=0, U/S=0
        let mut cpu = Cpu386::default();
        cpu.control.cr0 |= CR0_PE | CR0_PG;
        cpu.control.cr3 = 0x1000;
        // Supervisor: CPL 0.
        cpu.registers.set_segment(
            SegmentIndex::Cs,
            SegmentRegister {
                selector: 0x0000,
                base: 0,
                limit: 0xffff_ffff,
                access: 0x9b,
                default_size_32: false,
            },
        );
        let mut bus = TestBus::with_memory(memory);

        // WP clear (the 386 default): a supervisor write to the read-only page
        // succeeds and resolves to the mapped frame.
        assert_eq!(cpu.control.cr0 & CR0_WP, 0);
        assert_eq!(
            cpu.translate_linear(&mut bus, 0x3000, true).unwrap(),
            0x5000
        );

        // A supervisor read always passes regardless of WP.
        assert_eq!(
            cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
            0x5000
        );

        // WP set (the 486 feature): the same supervisor write now faults #PF with
        // error code present|write (bits 0 and 1 -> 0b011 = 0x3); the U/S bit is 0
        // because the access is supervisor, and cr2 holds the faulting address.
        cpu.control.cr0 |= CR0_WP;
        let faulted = cpu.translate_linear(&mut bus, 0x3000, true);
        assert!(
            matches!(
                faulted,
                Err(InternalFault::Exception {
                    vector: 14,
                    error_code: Some(0x3)
                })
            ),
            "{faulted:?}"
        );
        assert_eq!(cpu.control.cr2, 0x3000);

        // A supervisor read is unaffected by WP and still resolves.
        assert_eq!(
            cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
            0x5000
        );
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
    fn repeated_operand_size_prefix_stays_active() {
        // 66 66 d1 e0 = shl eax,1 with a redundant operand-size prefix. The
        // second 66 must not cancel the first, so this stays a 32-bit shift.
        let mut memory = vec![0; 16];
        memory[0..4].copy_from_slice(&[0x66, 0x66, 0xd1, 0xe0]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_eax(0x4000_0000);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eax(), 0x8000_0000);
        assert_eq!(cpu.registers.eip, 4); // two prefixes + opcode + modrm
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
    fn lds_loads_offset_and_ds() {
        // lds bx, [0x0200]  (0xc5 0x1e 0x00 0x02). Loads the far pointer at DS:0x0200:
        // BX <- word[0x0200], DS <- word[0x0202]. No flags change.
        let mut memory = vec![0; 0x1000];
        memory[0..4].copy_from_slice(&[0xc5, 0x1e, 0x00, 0x02]);
        memory[0x0200] = 0x34; // offset low
        memory[0x0201] = 0x12; // offset high -> 0x1234
        memory[0x0202] = 0x00; // selector low
        memory[0x0203] = 0x90; // selector high -> 0x9000
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        let flags_before = cpu.registers.eflags;
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
        assert_eq!(cpu.registers.segment(SegmentIndex::Ds).selector, 0x9000);
        assert_eq!(cpu.registers.segment(SegmentIndex::Ds).base, 0x9000 << 4);
        assert_eq!(cpu.registers.eflags, flags_before);
    }

    #[test]
    fn les_loads_offset_and_es() {
        // les di, [bx]  (0xc4 0x3f). With BX=0x0300 it loads DS:0x0300:
        // DI <- word[0x0300], ES <- word[0x0302]. No flags change.
        let mut memory = vec![0; 0x1000];
        memory[0..2].copy_from_slice(&[0xc4, 0x3f]);
        memory[0x0300] = 0x78; // offset low
        memory[0x0301] = 0x56; // offset high -> 0x5678
        memory[0x0302] = 0x00; // selector low
        memory[0x0303] = 0xb8; // selector high -> 0xb800
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_ebx(0x0300);
        let flags_before = cpu.registers.eflags;
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Di), 0x5678);
        assert_eq!(cpu.registers.segment(SegmentIndex::Es).selector, 0xb800);
        assert_eq!(cpu.registers.segment(SegmentIndex::Es).base, 0xb800 << 4);
        assert_eq!(cpu.registers.eflags, flags_before);
    }

    #[test]
    fn lds_with_register_operand_delivers_ud() {
        // lds ax, bx  (0xc5 0xc3, mod=3) is an invalid encoding -> #UD (vector 6).
        // IVT[6] at 0x18 points to IP 0x00ee, CS 0; the CPU vectors there.
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0xc5, 0xc3]);
        memory[0x18] = 0xee; // vector 6 IP low byte (IP = 0x00ee)
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eip, 0x00ee);
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
        // 0x66 0x9c PUSHFD. EFLAGS carries garbage in the high bits; the 486 pushes
        // the defined low 16 plus AC (bit 18) and ID (bit 21). With every high bit
        // set in the source, the dword on the stack is 0x0024_0493.
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
        assert_eq!(pushed, 0x0024_0493);
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
    fn pop_rm16_into_memory_disp16() {
        // 8F /0 with mod=00 rm=110 disp16: POP word [0x0200]. The encoding the
        // Wizardry III booter uses (with a CS override). Pops the stack top into
        // the memory word and advances SP by 2. Arithmetic flags are untouched.
        let mut memory = vec![0; 1024];
        memory[0..4].copy_from_slice(&[0x8f, 0x06, 0x00, 0x02]);
        // Stack top at ss:0x0100 = 0xbeef.
        memory[0x100..0x102].copy_from_slice(&0xbeefu16.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        cpu.registers.eflags = 0x0000_0ed7; // all arithmetic flags set
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(
            u16::from_le_bytes([bus.memory[0x200], bus.memory[0x201]]),
            0xbeef
        );
        assert_eq!(cpu.read_gpr16(4), 0x0102); // SP advanced by 2
        assert_eq!(cpu.registers.eflags, 0x0000_0ed7); // flags unchanged
    }

    #[test]
    fn pop_rm16_into_register() {
        // 8F /0 with mod=11 rm=011: POP BX. Register destination form.
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0x8f, 0xc3]);
        memory[0x100..0x102].copy_from_slice(&0x1234u16.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
        assert_eq!(cpu.read_gpr16(4), 0x0102);
    }

    #[test]
    fn pop_rm32_into_register_preserves_high_esp() {
        // 0x66 8F /0 mod=11 rm=001: POP ECX, 32-bit operand on a 16-bit stack.
        // The full dword loads into ECX; SP advances by 4 and ESP[31:16] is kept.
        let mut memory = vec![0; 1024];
        memory[0..3].copy_from_slice(&[0x66, 0x8f, 0xc1]);
        memory[0x100..0x104].copy_from_slice(&0xcafe_f00du32.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0xdead_0100);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.ecx(), 0xcafe_f00d);
        assert_eq!(cpu.registers.esp(), 0xdead_0104);
    }

    #[test]
    fn pop_rm_reg_nonzero_is_illegal() {
        // 8F with reg != 0 is an illegal group encoding (group 1A reserves only /0).
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0x8f, 0xcb]); // mod=11 reg=001 rm=011
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        let mut bus = TestBus::with_memory(memory);

        let err = cpu.cycle(&mut bus).unwrap_err();
        assert!(matches!(
            err,
            CpuError::UnsupportedGroupOpcode {
                opcode: 0x8f,
                extension: 1
            }
        ));
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

    #[test]
    fn lock_add_to_memory_executes() {
        // lock add [0x40], ax (0xf0 0x01 0x06 0x40 0x00). mem[0x40]=0x0010, ax=0x0005 -> 0x0015.
        let mut memory = vec![0; 128];
        memory[0..5].copy_from_slice(&[0xf0, 0x01, 0x06, 0x40, 0x00]);
        memory[0x40] = 0x10;
        memory[0x41] = 0x00;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0005);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(
            u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]),
            0x0015
        );
    }

    #[test]
    fn lock_bts_to_memory_executes() {
        // lock bts [0x40], ax (0xf0 0x0f 0xab 0x06 0x40 0x00). ax=3 -> set bit 3 of [0x40].
        let mut memory = vec![0; 128];
        memory[0..6].copy_from_slice(&[0xf0, 0x0f, 0xab, 0x06, 0x40, 0x00]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0003);
        cpu.set_flag(FLAG_CF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(bus.memory[0x40], 0x08); // bit 3 set
        assert!(!cpu.flag(FLAG_CF)); // old bit was 0
    }

    #[test]
    fn lock_on_register_destination_delivers_ud() {
        // lock add ax, bx (0xf0 0x01 0xd8, mod=3 register dest). LOCK needs memory -> #UD.
        let mut memory = vec![0; 1024];
        memory[0..3].copy_from_slice(&[0xf0, 0x01, 0xd8]);
        memory[0x18] = 0xee; // IVT[6] -> IP 0x00ee, CS 0
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
    fn lock_xchg_register_delivers_ud() {
        // lock xchg ax, bx (0xf0 0x87 0xd8, mod=3). XCHG needs memory -> #UD.
        let mut memory = vec![0; 1024];
        memory[0..3].copy_from_slice(&[0xf0, 0x87, 0xd8]);
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
    fn lock_inc_register_delivers_ud() {
        // lock inc al (0xf0 0xfe 0xc0, FE /0 mod=3). INC of a register under LOCK -> #UD.
        let mut memory = vec![0; 1024];
        memory[0..3].copy_from_slice(&[0xf0, 0xfe, 0xc0]);
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
    fn lock_cmp_memory_delivers_ud() {
        // lock cmp [0x40], ax (0xf0 0x39 0x06 0x40 0x00). CMP is not lockable even to memory -> #UD.
        let mut memory = vec![0; 1024];
        memory[0..5].copy_from_slice(&[0xf0, 0x39, 0x06, 0x40, 0x00]);
        memory[0x18] = 0xee;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        cpu.set_flag(FLAG_IF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eip, 0x00ee);
        assert!(!cpu.flag(FLAG_IF));
    }

    #[test]
    fn lock_non_lockable_opcode_delivers_ud() {
        // lock mov ax, bx (0xf0 0x89 0xd8). MOV is not lockable -> #UD.
        let mut memory = vec![0; 1024];
        memory[0..3].copy_from_slice(&[0xf0, 0x89, 0xd8]);
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
    fn lock_bts_imm_to_memory_executes() {
        // lock bts [0x40], 3 (0xf0 0x0f 0xba 0x2e 0x40 0x00 0x03, /5 = BTS). set bit 3 of [0x40].
        let mut memory = vec![0; 128];
        memory[0..7].copy_from_slice(&[0xf0, 0x0f, 0xba, 0x2e, 0x40, 0x00, 0x03]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_CF, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(bus.memory[0x40], 0x08); // bit 3 set
        assert!(!cpu.flag(FLAG_CF)); // old bit was 0
    }

    #[test]
    fn lock_btc_imm_register_delivers_ud() {
        // lock btc bx, 5 (0xf0 0x0f 0xba 0xfb 0x05, /7 = BTC, mod=3 register dest) -> #UD.
        let mut memory = vec![0; 1024];
        memory[0..5].copy_from_slice(&[0xf0, 0x0f, 0xba, 0xfb, 0x05]);
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
    fn bswap_reverses_dword_byte_order() {
        // bswap eax (0x0f 0xc8). eax = 0x12345678 -> 0x78563412 in 32-bit operand mode.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0x0f, 0xc8]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        // A 32-bit code segment so the default operand size is dword.
        let mut cs = cpu.registers.cs();
        cs.default_size_32 = true;
        cpu.registers.set_segment(SegmentIndex::Cs, cs);
        cpu.write_gpr32(0, 0x1234_5678);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_gpr32(0), 0x7856_3412);
        // A second BSWAP restores the original (round-trip).
        cpu.registers.eip = 0;
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.read_gpr32(0), 0x1234_5678);
    }

    #[test]
    fn invd_and_wbinvd_noop_at_cpl0() {
        // invd (0x0f 0x08) then wbinvd (0x0f 0x09) in real mode (CPL 0). Both are no-ops:
        // they advance past their two bytes and touch no register or flag.
        let mut memory = vec![0; 64];
        memory[0..4].copy_from_slice(&[0x0f, 0x08, 0x0f, 0x09]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        let flags_before = cpu.registers.eflags;
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.eip, 2);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.eip, 4);
        assert_eq!(cpu.registers.eflags, flags_before);
    }

    #[test]
    fn invd_at_cpl3_delivers_ud() {
        // invd (0x0f 0x08) at CPL 3 in protected mode raises #UD (vector 6).
        let mut cpu = Cpu386::default();
        cpu.control.cr0 |= CR0_PE;
        cpu.registers.set_segment(
            SegmentIndex::Cs,
            SegmentRegister {
                selector: 0x0003,
                base: 0,
                limit: 0xffff_ffff,
                access: 0x9b,
                default_size_32: false,
            },
        );
        cpu.registers.eip = 0;
        let mut bus = TestBus::with_memory(vec![0x0f, 0x08, 0, 0]);

        let result = cpu.execute_instruction(&mut bus);

        assert!(
            matches!(
                result,
                Err(InternalFault::Exception {
                    vector: 6,
                    error_code: None
                })
            ),
            "{result:?}"
        );
    }

    #[test]
    fn wbinvd_at_cpl3_delivers_ud() {
        // wbinvd (0x0f 0x09) at CPL 3 in protected mode raises #UD (vector 6).
        let mut cpu = Cpu386::default();
        cpu.control.cr0 |= CR0_PE;
        cpu.registers.set_segment(
            SegmentIndex::Cs,
            SegmentRegister {
                selector: 0x0003,
                base: 0,
                limit: 0xffff_ffff,
                access: 0x9b,
                default_size_32: false,
            },
        );
        cpu.registers.eip = 0;
        let mut bus = TestBus::with_memory(vec![0x0f, 0x09, 0, 0]);

        let result = cpu.execute_instruction(&mut bus);

        assert!(
            matches!(
                result,
                Err(InternalFault::Exception {
                    vector: 6,
                    error_code: None
                })
            ),
            "{result:?}"
        );
    }

    #[test]
    fn invlpg_memory_noop_at_cpl0() {
        // invlpg [0x40] (0x0f 0x01 0x3e 0x40 0x00, /7 with a memory operand) in real mode.
        // No TLB is modeled, so it is a no-op that advances past its bytes and leaves the
        // pointed-at memory untouched.
        let mut memory = vec![0; 128];
        memory[0..5].copy_from_slice(&[0x0f, 0x01, 0x3e, 0x40, 0x00]);
        memory[0x40] = 0xaa;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eip, 5);
        assert_eq!(bus.memory[0x40], 0xaa);
    }

    #[test]
    fn invlpg_at_cpl3_delivers_ud() {
        // invlpg [0x40] at CPL 3 in protected mode raises #UD (vector 6).
        let mut cpu = Cpu386::default();
        cpu.control.cr0 |= CR0_PE;
        cpu.registers.set_segment(
            SegmentIndex::Cs,
            SegmentRegister {
                selector: 0x0003,
                base: 0,
                limit: 0xffff_ffff,
                access: 0x9b,
                default_size_32: false,
            },
        );
        cpu.registers.set_segment(
            SegmentIndex::Ds,
            SegmentRegister {
                selector: 0x0003,
                base: 0,
                limit: 0xffff_ffff,
                access: 0x93,
                default_size_32: false,
            },
        );
        cpu.registers.eip = 0;
        let mut bus = TestBus::with_memory(vec![0x0f, 0x01, 0x3e, 0x40, 0x00, 0, 0, 0]);

        let result = cpu.execute_instruction(&mut bus);

        assert!(
            matches!(
                result,
                Err(InternalFault::Exception {
                    vector: 6,
                    error_code: None
                })
            ),
            "{result:?}"
        );
    }

    #[test]
    fn invlpg_register_form_delivers_ud() {
        // 0F 01 /7 with a register operand (mod=3) is #UD. ModRM 0xff = mod 3, reg 7, rm 7.
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        let mut bus = TestBus::with_memory(vec![0x0f, 0x01, 0xff, 0, 0]);

        let result = cpu.execute_instruction(&mut bus);

        assert!(
            matches!(
                result,
                Err(InternalFault::Exception {
                    vector: 6,
                    error_code: None
                })
            ),
            "{result:?}"
        );
    }

    #[test]
    fn hardware_irq_injects_when_if_enabled() {
        // IVT[8] (physical 0x20) -> IP 0x00cc, CS 0. With IF=1 and a pending IRQ,
        // cycle() vectors to the handler before the NOP at eip 0 can execute.
        let mut memory = vec![0; 1024];
        memory[0] = 0x90; // nop that must NOT run
        memory[0x20] = 0xcc; // handler IP low byte
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        cpu.set_flag(FLAG_IF, true);
        let mut bus = TestBus::with_memory(memory);
        bus.pending_irq = Some(8);

        let outcome = cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eip, 0x00cc);
        assert!(!cpu.flag(FLAG_IF)); // delivery clears IF
        assert!(!outcome.halted);
        assert_eq!(bus.acknowledge_interrupt(), None); // the request was consumed
    }

    #[test]
    fn hardware_irq_held_off_when_if_clear() {
        // IF=0: the pending IRQ waits and the NOP at eip 0 runs instead.
        let mut memory = vec![0; 1024];
        memory[0] = 0x90; // nop
        memory[0x20] = 0xcc;
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_IF, false);
        let mut bus = TestBus::with_memory(memory);
        bus.pending_irq = Some(8);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eip, 1); // NOP executed, no vector taken
        assert_eq!(bus.acknowledge_interrupt(), Some(8)); // still pending
    }

    #[test]
    fn hlt_wakes_on_pending_irq() {
        let mut memory = vec![0; 1024];
        memory[0] = 0xf4; // hlt
        memory[0x20] = 0xcc; // IVT[8] IP
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        cpu.set_flag(FLAG_IF, true);
        let mut bus = TestBus::with_memory(memory);

        // First cycle executes HLT and halts.
        assert!(cpu.cycle(&mut bus).unwrap().halted);

        // A pending IRQ wakes the CPU and is delivered on the next cycle.
        bus.pending_irq = Some(8);
        let woken = cpu.cycle(&mut bus).unwrap();
        assert!(!woken.halted);
        assert_eq!(cpu.registers.eip, 0x00cc);
    }

    #[test]
    fn hlt_stays_halted_without_deliverable_irq() {
        let mut memory = vec![0; 1024];
        memory[0] = 0xf4; // hlt
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.set_flag(FLAG_IF, true);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap(); // execute HLT

        // No pending IRQ: stays halted.
        assert!(cpu.cycle(&mut bus).unwrap().halted);

        // Pending IRQ but IF=0: masked at the CPU, stays halted.
        cpu.set_flag(FLAG_IF, false);
        bus.pending_irq = Some(8);
        assert!(cpu.cycle(&mut bus).unwrap().halted);
    }

    // --- 486 read-modify-write opcodes: XADD and CMPXCHG ---

    #[test]
    fn xadd_byte_swaps_and_adds_with_add_flags() {
        // 0F C0 /r XADD r/m8, r8. ModRM C3: mode 3, reg = AL(0), rm = BL(3).
        // dest = BL, src = AL. After: BL = BL + AL, AL = old BL, flags like ADD(BL, AL).
        let mut memory = vec![0; 16];
        memory[0..3].copy_from_slice(&[0x0f, 0xc0, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr8(0, 0x01); // AL (src)
        cpu.write_gpr8(3, 0xff); // BL (dest)
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_gpr8(3), 0x00); // dest = 0xff + 0x01
        assert_eq!(cpu.read_gpr8(0), 0xff); // src = old dest
        // 0xff + 0x01 wraps to 0 with carry, half-carry, and a zero result.
        assert!(cpu.flag(FLAG_CF));
        assert!(cpu.flag(FLAG_ZF));
        assert!(cpu.flag(FLAG_AF));
        assert!(!cpu.flag(FLAG_OF));
        assert!(!cpu.flag(FLAG_SF));
    }

    #[test]
    fn xadd_word_matches_add_flags() {
        // 0F C1 /r XADD r/m16, r16. ModRM C3: reg = AX(0), rm = BX(3).
        let mut memory = vec![0; 16];
        memory[0..3].copy_from_slice(&[0x0f, 0xc1, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr16(0, 0x7fff); // AX (src)
        cpu.write_gpr16(3, 0x0001); // BX (dest)
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.read_gpr16(3), 0x8000); // 0x0001 + 0x7fff
        assert_eq!(cpu.read_gpr16(0), 0x0001); // old dest
        // Signed overflow: positive + positive crossed into the sign bit.
        assert!(cpu.flag(FLAG_OF));
        assert!(cpu.flag(FLAG_SF));
        assert!(!cpu.flag(FLAG_CF));
        assert!(!cpu.flag(FLAG_ZF));
    }

    #[test]
    fn xadd_dword_matches_add_flags() {
        // 66h is not needed: with a 32-bit operand prefix on a real-mode CS, 66 0F C1 /r.
        // ModRM C3: reg = EAX(0), rm = EBX(3).
        let mut memory = vec![0; 16];
        memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xc1, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_eax(0x1111_1111); // src
        cpu.registers.set_ebx(0x2222_2222); // dest
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.ebx(), 0x3333_3333);
        assert_eq!(cpu.registers.eax(), 0x2222_2222);
        assert!(!cpu.flag(FLAG_CF));
        assert!(!cpu.flag(FLAG_OF));
        assert!(!cpu.flag(FLAG_ZF));
        assert!(!cpu.flag(FLAG_SF));
    }

    #[test]
    fn cmpxchg_byte_equal_stores_source() {
        // 0F B0 /r CMPXCHG r/m8, r8. ModRM C3: reg = CL(1, src), rm = BL(3, dest).
        // AL == BL so ZF is set and the source (CL) is stored into BL; AL is unchanged.
        let mut memory = vec![0; 16];
        memory[0..3].copy_from_slice(&[0x0f, 0xb0, 0xcb]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr8(0, 0x42); // AL (accumulator)
        cpu.write_gpr8(3, 0x42); // BL (dest), equal to AL
        cpu.write_gpr8(1, 0x99); // CL (src)
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_ZF)); // equal compare
        assert_eq!(cpu.read_gpr8(3), 0x99); // dest = src
        assert_eq!(cpu.read_gpr8(0), 0x42); // accumulator unchanged
    }

    #[test]
    fn cmpxchg_byte_unequal_loads_destination() {
        // AL != BL: ZF clear, AL = BL, BL unchanged.
        let mut memory = vec![0; 16];
        memory[0..3].copy_from_slice(&[0x0f, 0xb0, 0xcb]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr8(0, 0x42); // AL
        cpu.write_gpr8(3, 0x10); // BL (dest), not equal
        cpu.write_gpr8(1, 0x99); // CL (src)
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(!cpu.flag(FLAG_ZF)); // unequal compare
        assert_eq!(cpu.read_gpr8(0), 0x10); // accumulator = dest
        assert_eq!(cpu.read_gpr8(3), 0x10); // dest unchanged
        // Flags must match CMP(0x42, 0x10) = 0x32: no borrow, positive, nonzero.
        assert!(!cpu.flag(FLAG_CF));
        assert!(!cpu.flag(FLAG_SF));
    }

    #[test]
    fn cmpxchg_word_equal_stores_source() {
        // 0F B1 /r CMPXCHG r/m16, r16. ModRM C3: reg = CX(1, src), rm = BX(3, dest).
        let mut memory = vec![0; 16];
        memory[0..3].copy_from_slice(&[0x0f, 0xb1, 0xcb]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr16(0, 0x1234); // AX
        cpu.write_gpr16(3, 0x1234); // BX, equal
        cpu.write_gpr16(1, 0xbeef); // CX (src)
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(cpu.flag(FLAG_ZF));
        assert_eq!(cpu.read_gpr16(3), 0xbeef);
        assert_eq!(cpu.read_gpr16(0), 0x1234);
    }

    #[test]
    fn cmpxchg_dword_unequal_loads_destination() {
        // 66 0F B1 /r CMPXCHG r/m32, r32. ModRM C3: reg = ECX(1, src), rm = EBX(3, dest).
        let mut memory = vec![0; 16];
        memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xb1, 0xcb]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_eax(0xaaaa_aaaa); // EAX
        cpu.registers.set_ebx(0x5555_5555); // EBX (dest), not equal
        cpu.registers.set_ecx(0xdead_beef); // ECX (src)
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert!(!cpu.flag(FLAG_ZF));
        assert_eq!(cpu.registers.eax(), 0x5555_5555); // accumulator = dest
        assert_eq!(cpu.registers.ebx(), 0x5555_5555); // dest unchanged
    }

    #[test]
    fn lock_xadd_to_memory_is_accepted() {
        // F0 0F C1 06 00 02: LOCK XADD [0x0200], AX. ModRM 06 is mode 0 rm 6 (direct disp16),
        // a memory destination, so the LOCK is legal and the instruction runs.
        let mut memory = vec![0; 1024];
        memory[0..6].copy_from_slice(&[0xf0, 0x0f, 0xc1, 0x06, 0x00, 0x02]);
        memory[0x200..0x202].copy_from_slice(&0x0010u16.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        cpu.write_gpr16(0, 0x0001); // AX (src)
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        // [0x0200] = 0x0010 + 0x0001, AX = old [0x0200].
        assert_eq!(
            u16::from_le_bytes([bus.memory[0x200], bus.memory[0x201]]),
            0x0011
        );
        assert_eq!(cpu.read_gpr16(0), 0x0010);
    }

    #[test]
    fn lock_xadd_to_register_is_undefined_opcode() {
        // F0 0F C1 C3: LOCK XADD BX, AX. The register destination makes the LOCK prefix illegal,
        // so the decoder raises #UD (vector 6) before executing.
        let mut memory = vec![0; 16];
        memory[0..4].copy_from_slice(&[0xf0, 0x0f, 0xc1, 0xc3]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        let mut bus = TestBus::with_memory(memory);

        let fault = cpu.execute_instruction(&mut bus).unwrap_err();
        assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
    }

    #[test]
    fn lock_bswap_is_undefined_opcode() {
        // F0 0F C8: LOCK BSWAP EAX. BSWAP has no memory form, so LOCK is always #UD.
        let mut memory = vec![0; 16];
        memory[0..3].copy_from_slice(&[0xf0, 0x0f, 0xc8]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        let mut bus = TestBus::with_memory(memory);

        let fault = cpu.execute_instruction(&mut bus).unwrap_err();
        assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
    }

    // Build a CPL-3 protected-mode CPU whose CS and DS are flat user segments, running
    // MOV AX, moffs16 (0xa1) that reads a word from DS:moffs. The caller picks the
    // moffs so the access lands on an even or odd boundary.
    fn cpl3_word_read_at(moffs: u16) -> (Cpu386, TestBus) {
        let mut memory = vec![0; 256];
        memory[0] = 0xa1;
        memory[1..3].copy_from_slice(&moffs.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.control.cr0 |= CR0_PE;
        cpu.registers.set_segment(
            SegmentIndex::Cs,
            SegmentRegister {
                selector: 0x0003,
                base: 0,
                limit: 0xffff_ffff,
                access: 0x9b,
                default_size_32: false,
            },
        );
        cpu.registers.set_segment(
            SegmentIndex::Ds,
            SegmentRegister {
                selector: 0x0003,
                base: 0,
                limit: 0xffff_ffff,
                access: 0x93,
                default_size_32: false,
            },
        );
        cpu.registers.eip = 0;
        (cpu, TestBus::with_memory(memory))
    }

    #[test]
    fn misaligned_word_read_faults_ac_when_am_and_ac_set_at_cpl3() {
        // CR0.AM and EFLAGS.AC both set, CPL 3, odd word address: #AC (vector 17, no
        // error code).
        let (mut cpu, mut bus) = cpl3_word_read_at(0x0041);
        cpu.control.cr0 |= CR0_AM;
        cpu.set_flag(FLAG_AC, true);

        let result = cpu.execute_instruction(&mut bus);

        assert!(
            matches!(
                result,
                Err(InternalFault::Exception {
                    vector: 17,
                    error_code: None
                })
            ),
            "{result:?}"
        );
    }

    #[test]
    fn misaligned_word_read_no_fault_without_cr0_am() {
        // EFLAGS.AC set but CR0.AM clear: the alignment check stays masked, no fault.
        // Set CR0 bit 4 (ET) too: it is not AM, so it must not arm the check.
        let (mut cpu, mut bus) = cpl3_word_read_at(0x0041);
        cpu.control.cr0 |= 0x0000_0010; // bit 4 (ET), not AM
        cpu.set_flag(FLAG_AC, true);

        assert!(cpu.execute_instruction(&mut bus).is_ok());
    }

    #[test]
    fn misaligned_word_read_no_fault_without_eflags_ac() {
        // CR0.AM set but EFLAGS.AC clear: software has not opted in, no fault.
        let (mut cpu, mut bus) = cpl3_word_read_at(0x0041);
        cpu.control.cr0 |= CR0_AM;

        assert!(cpu.execute_instruction(&mut bus).is_ok());
    }

    #[test]
    fn misaligned_word_read_no_fault_at_supervisor() {
        // AM and AC both set, but CPL 0 (supervisor): exempt, no fault. Reuse the
        // CPL-3 setup and drop CS/DS RPL to 0.
        let (mut cpu, mut bus) = cpl3_word_read_at(0x0041);
        cpu.control.cr0 |= CR0_AM;
        cpu.set_flag(FLAG_AC, true);
        let mut cs = cpu.registers.cs();
        cs.selector = 0x0000;
        cpu.registers.set_segment(SegmentIndex::Cs, cs);
        let mut ds = cpu.registers.segment(SegmentIndex::Ds);
        ds.selector = 0x0000;
        cpu.registers.set_segment(SegmentIndex::Ds, ds);

        assert!(cpu.execute_instruction(&mut bus).is_ok());
    }

    #[test]
    fn aligned_word_read_never_faults_with_am_and_ac() {
        // Even word address: aligned, so no #AC even with AM and AC set at CPL 3.
        let (mut cpu, mut bus) = cpl3_word_read_at(0x0040);
        cpu.control.cr0 |= CR0_AM;
        cpu.set_flag(FLAG_AC, true);

        assert!(cpu.execute_instruction(&mut bus).is_ok());
    }

    #[test]
    fn eflags_ac_and_id_survive_pushf_popf_round_trip() {
        // 66 9c PUSHFD ; 66 9d POPFD. Set AC and ID, perturb both after they reach the
        // stack, and confirm POPFD restores them from the dword flag image.
        let mut memory = vec![0; 1024];
        memory[0..2].copy_from_slice(&[0x66, 0x9c]);
        memory[2..4].copy_from_slice(&[0x66, 0x9d]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        cpu.set_flag(FLAG_AC, true);
        cpu.set_flag(FLAG_ID, true);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap(); // pushfd
        cpu.set_flag(FLAG_AC, false); // perturb after the image is on the stack
        cpu.set_flag(FLAG_ID, false);
        cpu.cycle(&mut bus).unwrap(); // popfd

        assert!(cpu.flag(FLAG_AC));
        assert!(cpu.flag(FLAG_ID));
    }

    fn run_cpuid(leaf: u32) -> Cpu386 {
        // CPUID (0F A2) with the leaf selector in EAX. Returns the CPU after one step so the
        // caller can read EAX/EBX/ECX/EDX.
        let mut memory = vec![0; 64];
        memory[0..2].copy_from_slice(&[0x0f, 0xa2]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_eax(leaf);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        cpu
    }

    #[test]
    fn cpuid_leaf0_reports_vendor_string_and_max_leaf() {
        let cpu = run_cpuid(0);
        // Max basic leaf is 1.
        assert_eq!(cpu.registers.eax(), 1);
        // Vendor string "Genuine GSW " in the standard EBX, EDX, ECX order, four bytes
        // little-endian per register.
        assert_eq!(cpu.registers.ebx().to_le_bytes(), *b"Genu");
        assert_eq!(cpu.registers.edx().to_le_bytes(), *b"ine ");
        assert_eq!(cpu.registers.ecx().to_le_bytes(), *b"GSW ");
        // Concatenating EBX:EDX:ECX yields the full 12-byte vendor string.
        let mut vendor = [0u8; 12];
        vendor[0..4].copy_from_slice(&cpu.registers.ebx().to_le_bytes());
        vendor[4..8].copy_from_slice(&cpu.registers.edx().to_le_bytes());
        vendor[8..12].copy_from_slice(&cpu.registers.ecx().to_le_bytes());
        assert_eq!(&vendor, b"Genuine GSW ");
    }

    #[test]
    fn cpuid_leaf1_reports_family5_and_mmx_without_fpu() {
        let cpu = run_cpuid(1);
        let eax = cpu.registers.eax();
        // Family is bits 11-8 and must be 5 (586 / K6 class).
        assert_eq!((eax >> 8) & 0xf, 5);
        // Type is bits 13-12 (OEM = 0).
        assert_eq!((eax >> 12) & 0x3, 0);
        // MMX is bit 23 of EDX; FPU is bit 0 and must be off.
        let edx = cpu.registers.edx();
        assert_ne!(edx & (1 << 23), 0, "MMX bit should be set");
        assert_eq!(edx & 1, 0, "FPU bit should be clear");
        // Brand index 0 and no extended feature claimed.
        assert_eq!(cpu.registers.ebx(), 0);
        assert_eq!(cpu.registers.ecx(), 0);
    }

    #[test]
    fn cpuid_unknown_leaf_returns_zeros() {
        let cpu = run_cpuid(0x4000_0000);
        assert_eq!(cpu.registers.eax(), 0);
        assert_eq!(cpu.registers.ebx(), 0);
        assert_eq!(cpu.registers.ecx(), 0);
        assert_eq!(cpu.registers.edx(), 0);
    }

    #[test]
    fn cpuid_brand_string_reports_genuine_gsw_80586() {
        // Leaf 0x80000000 reports the maximum extended leaf, and 0x80000002..0x80000004
        // return the 48-byte null-padded brand string, 16 bytes per leaf in EAX, EBX, ECX,
        // EDX order. Concatenated, they spell the full processor name "Genuine GSW-80586".
        assert_eq!(run_cpuid(0x8000_0000).registers.eax(), 0x8000_0006);
        let mut brand = [0u8; 48];
        for (i, leaf) in [0x8000_0002u32, 0x8000_0003, 0x8000_0004]
            .iter()
            .enumerate()
        {
            let cpu = run_cpuid(*leaf);
            let base = i * 16;
            brand[base..base + 4].copy_from_slice(&cpu.registers.eax().to_le_bytes());
            brand[base + 4..base + 8].copy_from_slice(&cpu.registers.ebx().to_le_bytes());
            brand[base + 8..base + 12].copy_from_slice(&cpu.registers.ecx().to_le_bytes());
            brand[base + 12..base + 16].copy_from_slice(&cpu.registers.edx().to_le_bytes());
        }
        let mut expected = [0u8; 48];
        expected[..17].copy_from_slice(b"Genuine GSW-80586");
        assert_eq!(brand, expected);
    }

    #[test]
    fn cpuid_is_not_privileged_at_cpl3() {
        // CPUID runs at any privilege level. In protected mode at CPL 3 it must execute,
        // not fault, and still report the GSW-586 identity.
        let mut cpu = Cpu386::default();
        cpu.control.cr0 |= CR0_PE;
        cpu.registers.set_segment(
            SegmentIndex::Cs,
            SegmentRegister {
                selector: 0x0003,
                base: 0,
                limit: 0xffff_ffff,
                access: 0x9b,
                default_size_32: false,
            },
        );
        cpu.registers.eip = 0;
        cpu.registers.set_eax(0);
        let mut bus = TestBus::with_memory(vec![0x0f, 0xa2, 0, 0]);

        assert!(cpu.execute_instruction(&mut bus).is_ok());
        assert_eq!(cpu.registers.eax(), 1);
        assert_eq!(cpu.registers.ebx().to_le_bytes(), *b"Genu");
    }

    #[test]
    fn default_level_is_full_isa() {
        // The core resets to the full ISA so firmware POST is never restricted.
        assert_eq!(Cpu386::default().level(), CpuLevel::I586);
    }

    #[test]
    fn cpu_level_cache_table() {
        assert_eq!(CpuLevel::I286.cache_kb(), (0, 0));
        assert_eq!(CpuLevel::I386.cache_kb(), (0, 64));
        assert_eq!(CpuLevel::I486.cache_kb(), (16, 128));
        assert_eq!(CpuLevel::I586.cache_kb(), (32, 512));
    }

    /// Run a single instruction from `code` at the given level and return the result.
    fn run_at_level(code: &[u8], level: CpuLevel) -> Result<CycleOutcome, InternalFault> {
        let mut memory = vec![0; 1024];
        memory[..code.len()].copy_from_slice(code);
        let mut cpu = Cpu386::default();
        cpu.set_level(level);
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        let mut bus = TestBus::with_memory(memory);
        cpu.execute_instruction(&mut bus)
    }

    #[test]
    fn movzx_is_undefined_opcode_at_286_but_runs_at_386() {
        // 0F B6 C3: MOVZX AX, BL. A 386 addition, so #UD at the 286 level and fine above it.
        let code = [0x0f, 0xb6, 0xc3];
        let fault = run_at_level(&code, CpuLevel::I286).unwrap_err();
        assert!(
            matches!(fault, InternalFault::Exception { vector: 6, .. }),
            "MOVZX must raise #UD at I286"
        );
        assert!(
            run_at_level(&code, CpuLevel::I386).is_ok(),
            "MOVZX must execute at I386"
        );
        assert!(run_at_level(&code, CpuLevel::I586).is_ok());
    }

    #[test]
    fn firmware_rom_cs_is_exempt_from_the_286_gate() {
        // MOVZX AX, BL is a 386 op: guest code at the 286 level #UDs on it, but the
        // BIOS ROM must keep running the full ISA so a lowered GSW mode never faults
        // firmware (Accept, interrupt service, boot). CS in the F-segment ROM
        // aperture (base 0xF0000) is the exemption.
        let code = [0x0f, 0xb6, 0xc3];
        assert!(
            matches!(
                run_at_level(&code, CpuLevel::I286).unwrap_err(),
                InternalFault::Exception { vector: 6, .. }
            ),
            "guest MOVZX must still #UD at I286"
        );
        let mut memory = vec![0u8; 0x10_0000];
        let base = 0x000F_0000usize;
        memory[base..base + code.len()].copy_from_slice(&code);
        let mut cpu = Cpu386::default();
        cpu.set_level(CpuLevel::I286);
        cpu.load_segment_real(SegmentIndex::Cs, 0xF000);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        let mut bus = TestBus::with_memory(memory);
        assert!(
            cpu.execute_instruction(&mut bus).is_ok(),
            "MOVZX fetched from BIOS ROM must run even at I286"
        );
    }

    #[test]
    fn operand_size_prefix_is_undefined_opcode_at_286() {
        // 66 B8 ... a 32-bit MOV EAX, imm32 reached through the operand-size prefix. The 286
        // has no 66h prefix, so the decoder #UDs on the prefix byte; 386 and up run it.
        let code = [0x66, 0xb8, 0x78, 0x56, 0x34, 0x12];
        let fault = run_at_level(&code, CpuLevel::I286).unwrap_err();
        assert!(
            matches!(fault, InternalFault::Exception { vector: 6, .. }),
            "the 66h operand-size prefix must raise #UD at I286"
        );
        assert!(run_at_level(&code, CpuLevel::I386).is_ok());
    }

    #[test]
    fn address_size_prefix_is_undefined_opcode_at_286() {
        // 67 prefix: a 32-bit address form. Absent on the 286, present from the 386.
        // 67 8B 00 would be MOV with a 32-bit address; the prefix alone must #UD at I286.
        let code = [0x67, 0x90];
        let fault = run_at_level(&code, CpuLevel::I286).unwrap_err();
        assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
        // At I386 the prefix decodes and the NOP after it runs.
        assert!(run_at_level(&code, CpuLevel::I386).is_ok());
    }

    #[test]
    fn shld_and_setcc_are_undefined_opcodes_at_286() {
        // 0F A4 SHLD and 0F 90 SETO are both 386 additions.
        let shld = [0x0f, 0xa4, 0xc3, 0x04]; // SHLD BX, AX, 4
        let setcc = [0x0f, 0x90, 0xc0]; // SETO AL
        for code in [&shld[..], &setcc[..]] {
            let fault = run_at_level(code, CpuLevel::I286).unwrap_err();
            assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
            assert!(run_at_level(code, CpuLevel::I386).is_ok());
        }
    }

    #[test]
    fn lgdt_still_runs_at_286() {
        // 0F 01 /2 LGDT is a 286 instruction, so it must NOT be gated at the 286 level.
        // ModRM 16 = mode 0, reg 2 (LGDT), rm 6 (direct disp16) pointing at a 6-byte pseudo-
        // descriptor in memory.
        let mut memory = vec![0; 1024];
        memory[0..4].copy_from_slice(&[0x0f, 0x01, 0x16, 0x20]); // disp16 = 0x0020
        // 6-byte GDTR image at 0x0020: limit then 32-bit base.
        memory[0x20..0x26].copy_from_slice(&[0xff, 0x00, 0x00, 0x10, 0x00, 0x00]);
        let mut cpu = Cpu386::default();
        cpu.set_level(CpuLevel::I286);
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        let mut bus = TestBus::with_memory(memory);
        assert!(cpu.execute_instruction(&mut bus).is_ok());
        assert_eq!(cpu.gdtr.limit, 0x00ff);
        assert_eq!(cpu.gdtr.base, 0x0000_1000);
    }

    #[test]
    fn cpuid_is_undefined_opcode_below_486() {
        // CPUID is absent on the 286 and 386; it appears on the 486 and 586.
        let code = [0x0f, 0xa2];
        assert!(matches!(
            run_at_level(&code, CpuLevel::I286).unwrap_err(),
            InternalFault::Exception { vector: 6, .. }
        ));
        assert!(matches!(
            run_at_level(&code, CpuLevel::I386).unwrap_err(),
            InternalFault::Exception { vector: 6, .. }
        ));
        assert!(run_at_level(&code, CpuLevel::I486).is_ok());
        assert!(run_at_level(&code, CpuLevel::I586).is_ok());
    }

    #[test]
    fn cpuid_cache_leaves_report_level_sizes() {
        // The AMD-style L1 (0x80000005) and L2 (0x80000006) leaves carry the live level's
        // cache sizes in ECX: L1 KB in bits 31-24, L2 KB in bits 31-16.
        let mut cpu = run_cpuid(0x8000_0005);
        assert_eq!(cpu.registers.ecx() >> 24, 32); // I586 L1 = 32 KB
        cpu = run_cpuid(0x8000_0006);
        assert_eq!(cpu.registers.ecx() >> 16, 512); // I586 L2 = 512 KB
    }

    #[test]
    fn id_flag_toggle_detection_sequence_finds_cpuid() {
        // The standard CPUID-presence probe: read EFLAGS, flip ID (bit 21), write it back,
        // read EFLAGS again, and conclude CPUID exists if ID changed. Model that here using
        // PUSHFD/POPFD plus a software toggle of FLAG_ID, then run CPUID leaf 0 to confirm
        // the detection concludes correctly.
        let mut memory = vec![0; 1024];
        // 66 9c PUSHFD ; 66 9d POPFD to round-trip the dword image carrying ID.
        memory[0..2].copy_from_slice(&[0x66, 0x9c]);
        memory[2..4].copy_from_slice(&[0x66, 0x9d]);
        // 0f a2 CPUID with EAX = 0 already loaded.
        memory[4..6].copy_from_slice(&[0x0f, 0xa2]);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x0100);
        cpu.registers.set_eax(0);
        let mut bus = TestBus::with_memory(memory);

        // Establish ID = 0, flip it on, and confirm the flag image carries the change so a
        // detection routine would observe ID as toggleable (CPUID present).
        let before = cpu.flag(FLAG_ID);
        cpu.set_flag(FLAG_ID, !before);
        cpu.cycle(&mut bus).unwrap(); // pushfd captures ID = 1
        cpu.set_flag(FLAG_ID, before); // perturb
        cpu.cycle(&mut bus).unwrap(); // popfd restores ID = 1
        let toggled = cpu.flag(FLAG_ID);
        assert_eq!(toggled, !before, "ID flag must be toggleable");

        // Detection concluded CPUID is present; execute it and confirm the GSW-586 vendor.
        cpu.cycle(&mut bus).unwrap(); // cpuid
        assert_eq!(cpu.registers.eax(), 1);
        assert_eq!(cpu.registers.ebx().to_le_bytes(), *b"Genu");
    }

    // ---- Slice 1: real-mode integer opcode completion (see dev_docs/COVERAGE.md) ----

    fn real_mode_cpu(code: &[u8], mem_len: usize) -> (Cpu386, Vec<u8>) {
        let mut memory = vec![0u8; mem_len];
        memory[..code.len()].copy_from_slice(code);
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        (cpu, memory)
    }

    #[test]
    fn int3_traps_to_vector_3() {
        // 0xCC. IVT[3] (linear 12) -> CS:IP = 0000:0100.
        let (mut cpu, mut memory) = real_mode_cpu(&[0xcc], 0x200);
        memory[12..14].copy_from_slice(&0x0100u16.to_le_bytes());
        memory[14..16].copy_from_slice(&0x0000u16.to_le_bytes());
        cpu.write_reg16(Reg16::Sp, 0x0200);
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.eip, 0x0100);
        assert_eq!(cpu.registers.cs().selector, 0);
        // flags, CS, return-IP(=1) were pushed: SP fell by 6, return IP word is 1.
        assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01fa);
        assert_eq!(
            u16::from_le_bytes([bus.memory[0x1fa], bus.memory[0x1fb]]),
            1
        );
    }

    #[test]
    fn into_traps_only_when_overflow_set() {
        // 0xCE with OF=1 traps to vector 4 (IVT[4] linear 16 -> 0000:0200).
        let (mut cpu, mut memory) = real_mode_cpu(&[0xce], 0x300);
        memory[16..18].copy_from_slice(&0x0200u16.to_le_bytes());
        cpu.write_reg16(Reg16::Sp, 0x0280);
        cpu.set_flag(FLAG_OF, true);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.eip, 0x0200, "OF set: INTO must trap");

        // OF=0: INTO is a no-op, just advances past the one byte.
        let (mut cpu, memory) = real_mode_cpu(&[0xce], 0x40);
        cpu.set_flag(FLAG_OF, false);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.eip, 1, "OF clear: INTO must fall through");
    }

    #[test]
    fn word_in_out_use_word_width() {
        // IN AX, DX (0xED): word port read lands in AX (TestBus returns 0).
        let (mut cpu, memory) = real_mode_cpu(&[0xed], 0x10);
        cpu.write_reg16(Reg16::Ax, 0xffff);
        cpu.write_reg16(Reg16::Dx, 0x03f8);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0000);
        assert!(
            bus.trace
                .cycles()
                .iter()
                .any(|c| c.kind == BusAccessKind::IoRead
                    && c.width == BusWidth::Word
                    && c.address == 0x03f8)
        );

        // OUT DX, AX (0xEF): word port write at DX.
        let (mut cpu, memory) = real_mode_cpu(&[0xef], 0x10);
        cpu.write_reg16(Reg16::Ax, 0x1234);
        cpu.write_reg16(Reg16::Dx, 0x03f8);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert!(
            bus.trace
                .cycles()
                .iter()
                .any(|c| c.kind == BusAccessKind::IoWrite
                    && c.width == BusWidth::Word
                    && c.address == 0x03f8)
        );
    }

    #[test]
    fn push_imm8_sign_extends_to_word() {
        // 0x6A 0x80 -> push 0xFF80 onto a 16-bit stack.
        let (mut cpu, memory) = real_mode_cpu(&[0x6a, 0x80], 0x120);
        cpu.write_reg16(Reg16::Sp, 0x0100);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.read_reg16(Reg16::Sp), 0x00fe);
        assert_eq!(
            u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
            0xff80
        );
    }

    #[test]
    fn imul_imm8_sign_extended() {
        // IMUL AX, AX, -1  (0x6B 0xC0 0xFF): 2 * -1 = -2, fits, CF/OF clear.
        let (mut cpu, memory) = real_mode_cpu(&[0x6b, 0xc0, 0xff], 0x10);
        cpu.write_reg16(Reg16::Ax, 0x0002);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.read_reg16(Reg16::Ax), 0xfffe);
        assert!(!cpu.flag(FLAG_CF) && !cpu.flag(FLAG_OF));
    }

    #[test]
    fn imul_imm16_overflow_sets_carry_and_overflow() {
        // IMUL AX, AX, 0x0004 (0x69 0xC0 0x04 0x00) with AX=0x4000 -> 0x10000, truncates.
        let (mut cpu, memory) = real_mode_cpu(&[0x69, 0xc0, 0x04, 0x00], 0x10);
        cpu.write_reg16(Reg16::Ax, 0x4000);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0000);
        assert!(cpu.flag(FLAG_CF) && cpu.flag(FLAG_OF));
    }

    #[test]
    fn enter_level_zero_builds_frame() {
        // ENTER 4, 0 (0xC8 0x04 0x00 0x00): push BP, BP=SP, SP-=4.
        let (mut cpu, memory) = real_mode_cpu(&[0xc8, 0x04, 0x00, 0x00], 0x120);
        cpu.write_reg16(Reg16::Bp, 0xbbbb);
        cpu.write_reg16(Reg16::Sp, 0x0100);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.read_reg16(Reg16::Bp),
            0x00fe,
            "BP = frame after PUSH BP"
        );
        assert_eq!(cpu.read_reg16(Reg16::Sp), 0x00fa, "SP -= alloc");
        assert_eq!(
            u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
            0xbbbb
        );
    }

    #[test]
    fn salc_sets_al_from_carry() {
        // 0xD6 with CF=1 -> AL=0xFF (AH preserved).
        let (mut cpu, memory) = real_mode_cpu(&[0xd6], 0x10);
        cpu.write_reg16(Reg16::Ax, 0x1234);
        cpu.set_flag(FLAG_CF, true);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x12ff);

        // CF=0 -> AL=0x00.
        let (mut cpu, memory) = real_mode_cpu(&[0xd6], 0x10);
        cpu.write_reg16(Reg16::Ax, 0x1234);
        cpu.set_flag(FLAG_CF, false);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1200);
    }

    #[test]
    fn opcode_82_aliases_80_add() {
        // ADD AL, 5 encoded with the undocumented 0x82 group-1 opcode.
        let (mut cpu, memory) = real_mode_cpu(&[0x82, 0xc0, 0x05], 0x10);
        cpu.write_reg16(Reg16::Ax, 0x0010);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x15);
    }

    #[test]
    fn wait_is_a_nop_without_fpu() {
        let (mut cpu, memory) = real_mode_cpu(&[0x9b], 0x10);
        let flags_before = cpu.registers.eflags;
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.eip, 1);
        assert_eq!(cpu.registers.eflags, flags_before);
    }

    // ---- Phase 2 slice A: x87 FPU foundation (see dev_docs/coverage-roadmap.md) ----

    #[test]
    fn fninit_then_fld1_pushes_one() {
        // FNINIT (DB E3) then FLD1 (D9 E8).
        let (mut cpu, memory) = real_mode_cpu(&[0xdb, 0xe3, 0xd9, 0xe8], 0x20);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.fpu.get(0), 1.0);
        assert_eq!(cpu.fpu.top(), 7);
    }

    #[test]
    fn fld_fadd_fstp_round_trips_m64() {
        // FLD m64 [0x100]; FADD m64 [0x108]; FSTP m64 [0x110]. 2.5 + 1.25 = 3.75.
        let code = [
            0xdd, 0x06, 0x00, 0x01, 0xdc, 0x06, 0x08, 0x01, 0xdd, 0x1e, 0x10, 0x01,
        ];
        let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
        memory[0x100..0x108].copy_from_slice(&2.5f64.to_le_bytes());
        memory[0x108..0x110].copy_from_slice(&1.25f64.to_le_bytes());
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        cpu.cycle(&mut bus).unwrap();
        cpu.cycle(&mut bus).unwrap();
        let stored = f64::from_le_bytes(bus.memory[0x110..0x118].try_into().unwrap());
        assert_eq!(stored, 3.75);
    }

    #[test]
    fn fxch_swaps_st0_and_st1() {
        // FLD1 (D9 E8); FLDZ (D9 EE); FXCH ST(1) (D9 C9). ST0 ends as 1.0.
        let (mut cpu, memory) = real_mode_cpu(&[0xd9, 0xe8, 0xd9, 0xee, 0xd9, 0xc9], 0x20);
        let mut bus = TestBus::with_memory(memory);
        for _ in 0..3 {
            cpu.cycle(&mut bus).unwrap();
        }
        assert_eq!(cpu.fpu.get(0), 1.0);
        assert_eq!(cpu.fpu.get(1), 0.0);
    }

    #[test]
    fn fnstsw_ax_reports_top_in_status() {
        // FLD1 (D9 E8) then FNSTSW AX (DF E0): TOP=7 lands in AX bits 11-13.
        let (mut cpu, memory) = real_mode_cpu(&[0xd9, 0xe8, 0xdf, 0xe0], 0x20);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        cpu.cycle(&mut bus).unwrap();
        assert_eq!((cpu.read_reg16(Reg16::Ax) >> 11) & 0x7, 7);
    }

    #[test]
    fn fild_fmulp_fistp_integer_path() {
        // FILD m32 [0x100]=5; FILD m32 [0x104]=3; FMULP ST1,ST0 (DE C9); FISTP m32 [0x108].
        let code = [
            0xdb, 0x06, 0x00, 0x01, 0xdb, 0x06, 0x04, 0x01, 0xde, 0xc9, 0xdb, 0x1e, 0x08, 0x01,
        ];
        let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
        memory[0x100..0x104].copy_from_slice(&5i32.to_le_bytes());
        memory[0x104..0x108].copy_from_slice(&3i32.to_le_bytes());
        let mut bus = TestBus::with_memory(memory);
        for _ in 0..4 {
            cpu.cycle(&mut bus).unwrap();
        }
        let stored = i32::from_le_bytes(bus.memory[0x108..0x10c].try_into().unwrap());
        assert_eq!(stored, 15);
    }

    #[test]
    fn fsub_reverse_forms_differ() {
        // D8 /5 FSUBR ST0,ST(i): ST0 = ST(i) - ST0. Start ST0=2, ST1=10 -> 8.
        // FLD m64 [0x100]=10; FLD m64 [0x108]=2; FSUBR ST0,ST1 (D8 E9).
        let code = [0xdd, 0x06, 0x00, 0x01, 0xdd, 0x06, 0x08, 0x01, 0xd8, 0xe9];
        let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
        memory[0x100..0x108].copy_from_slice(&10.0f64.to_le_bytes());
        memory[0x108..0x110].copy_from_slice(&2.0f64.to_le_bytes());
        let mut bus = TestBus::with_memory(memory);
        for _ in 0..3 {
            cpu.cycle(&mut bus).unwrap();
        }
        assert_eq!(cpu.fpu.get(0), 8.0);
    }

    // ---- Phase 2 slice B: x87 transcendentals ----

    #[test]
    fn f2xm1_of_one_is_one() {
        // FLD1 (D9 E8); F2XM1 (D9 F0): 2^1 - 1 = 1.
        let (mut cpu, memory) = real_mode_cpu(&[0xd9, 0xe8, 0xd9, 0xf0], 0x20);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.fpu.get(0), 1.0);
    }

    #[test]
    fn fyl2x_computes_y_times_log2_x() {
        // FLD1 (ST1=1); FLD m64 [0x100]=2 (ST0=2); FYL2X (D9 F1): 1 * log2(2) = 1.
        let code = [0xd9, 0xe8, 0xdd, 0x06, 0x00, 0x01, 0xd9, 0xf1];
        let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
        memory[0x100..0x108].copy_from_slice(&2.0f64.to_le_bytes());
        let mut bus = TestBus::with_memory(memory);
        for _ in 0..3 {
            cpu.cycle(&mut bus).unwrap();
        }
        assert_eq!(cpu.fpu.get(0), 1.0);
    }

    #[test]
    fn fscale_scales_by_power_of_two() {
        // FLD m64 [0x100]=2 (ST1); FLD m64 [0x108]=3 (ST0); FSCALE (D9 FD): 3 * 2^2 = 12.
        let code = [0xdd, 0x06, 0x00, 0x01, 0xdd, 0x06, 0x08, 0x01, 0xd9, 0xfd];
        let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
        memory[0x100..0x108].copy_from_slice(&2.0f64.to_le_bytes());
        memory[0x108..0x110].copy_from_slice(&3.0f64.to_le_bytes());
        let mut bus = TestBus::with_memory(memory);
        for _ in 0..3 {
            cpu.cycle(&mut bus).unwrap();
        }
        assert_eq!(cpu.fpu.get(0), 12.0);
    }

    #[test]
    fn fptan_replaces_st0_and_pushes_one() {
        // FLDZ (D9 EE); FPTAN (D9 F2): tan(0)=0 in ST1, 1.0 pushed into ST0.
        let (mut cpu, memory) = real_mode_cpu(&[0xd9, 0xee, 0xd9, 0xf2], 0x20);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.fpu.get(0), 1.0);
        assert_eq!(cpu.fpu.get(1), 0.0);
    }

    // ---- Phase 2 slice C: integer-operand arithmetic + 80-bit extended ----

    #[test]
    fn fidiv_divides_by_an_integer_operand() {
        // FILD m32 [0x100]=20; FIDIV m32 [0x104]=4 (DA /6). 20 / 4 = 5.
        let code = [0xdb, 0x06, 0x00, 0x01, 0xda, 0x36, 0x04, 0x01];
        let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
        memory[0x100..0x104].copy_from_slice(&20i32.to_le_bytes());
        memory[0x104..0x108].copy_from_slice(&4i32.to_le_bytes());
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.fpu.get(0), 5.0);
    }

    #[test]
    fn extended80_round_trips_through_memory() {
        // FLD m64 [0x100]=3.5; FSTP m80 [0x108] (DB /7); FLD m80 [0x108] (DB /5).
        let code = [
            0xdd, 0x06, 0x00, 0x01, 0xdb, 0x3e, 0x08, 0x01, 0xdb, 0x2e, 0x08, 0x01,
        ];
        let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
        memory[0x100..0x108].copy_from_slice(&3.5f64.to_le_bytes());
        let mut bus = TestBus::with_memory(memory);
        for _ in 0..3 {
            cpu.cycle(&mut bus).unwrap();
        }
        assert_eq!(cpu.fpu.get(0), 3.5);
    }

    // ---- Phase 2 slice D: BCD, environment, state save/restore, FUCOMPP ----

    #[test]
    fn fbld_fbstp_round_trips_packed_bcd() {
        // FILD m32 [0x100]=12345; FBSTP m80 [0x108] (DF /6); FBLD m80 [0x108] (DF /4).
        let code = [
            0xdb, 0x06, 0x00, 0x01, 0xdf, 0x36, 0x08, 0x01, 0xdf, 0x26, 0x08, 0x01,
        ];
        let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
        memory[0x100..0x104].copy_from_slice(&12345i32.to_le_bytes());
        let mut bus = TestBus::with_memory(memory);
        for _ in 0..3 {
            cpu.cycle(&mut bus).unwrap();
        }
        assert_eq!(cpu.fpu.get(0), 12345.0);
    }

    #[test]
    fn fnsave_frstor_round_trips_registers() {
        // FLD1; FLD m64 [0x180]=2.5; FNSAVE [0x100] (DD /6); FRSTOR [0x100] (DD /4).
        let code = [
            0xd9, 0xe8, 0xdd, 0x06, 0x80, 0x01, 0xdd, 0x36, 0x00, 0x01, 0xdd, 0x26, 0x00, 0x01,
        ];
        let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
        memory[0x180..0x188].copy_from_slice(&2.5f64.to_le_bytes());
        let mut bus = TestBus::with_memory(memory);
        for _ in 0..4 {
            cpu.cycle(&mut bus).unwrap();
        }
        assert_eq!(cpu.fpu.get(0), 2.5);
        assert_eq!(cpu.fpu.get(1), 1.0);
    }

    #[test]
    fn fnstenv_fldenv_round_trips_top() {
        // FLD1 (TOP=7); FNSTENV [0x100] (D9 /6); FNINIT (TOP=0); FLDENV [0x100] (D9 /4).
        let code = [
            0xd9, 0xe8, 0xd9, 0x36, 0x00, 0x01, 0xdb, 0xe3, 0xd9, 0x26, 0x00, 0x01,
        ];
        let (mut cpu, memory) = real_mode_cpu(&code, 0x200);
        let mut bus = TestBus::with_memory(memory);
        for _ in 0..4 {
            cpu.cycle(&mut bus).unwrap();
        }
        assert_eq!(cpu.fpu.top(), 7);
    }

    #[test]
    fn fucompp_sets_equal_condition() {
        // FLD1; FLD1; FUCOMPP (DA E9): equal -> C3 set, both popped.
        let (mut cpu, memory) = real_mode_cpu(&[0xd9, 0xe8, 0xd9, 0xe8, 0xda, 0xe9], 0x20);
        let mut bus = TestBus::with_memory(memory);
        for _ in 0..3 {
            cpu.cycle(&mut bus).unwrap();
        }
        assert_eq!((cpu.fpu.status >> 14) & 1, 1, "C3 set on equal");
        assert_eq!(cpu.fpu.top(), 0, "both operands popped");
    }

    // ---- Phase 3: MMX execute path (lane math is unit-tested in mmx.rs) ----

    #[test]
    fn movd_then_movq_copies_registers() {
        // MOVD mm0, eax (0F 6E C0); MOVQ mm1, mm0 (0F 6F C8).
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x6e, 0xc0, 0x0f, 0x6f, 0xc8], 0x20);
        cpu.registers.set_eax(0x0a0b_0c0d);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.fpu.mm(0), 0x0a0b_0c0d);
        assert_eq!(cpu.fpu.mm(1), 0x0a0b_0c0d);
    }

    #[test]
    fn paddb_adds_packed_bytes_from_memory() {
        // MOVQ mm0, [0x100]; PADDB mm0, [0x108].
        let code = [0x0f, 0x6f, 0x06, 0x00, 0x01, 0x0f, 0xfc, 0x06, 0x08, 0x01];
        let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
        memory[0x100..0x108].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        memory[0x108..0x110].copy_from_slice(&[10, 10, 10, 10, 10, 10, 10, 10]);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.fpu.mm(0).to_le_bytes(),
            [11, 12, 13, 14, 15, 16, 17, 18]
        );
    }

    #[test]
    fn emms_marks_the_x87_stack_empty() {
        // MOVD mm0, eax marks the tags valid; EMMS (0F 77) empties them.
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x6e, 0xc0, 0x0f, 0x77], 0x20);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.fpu.tag, 0x0000, "MMX write marks tags valid");
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.fpu.tag, 0xffff, "EMMS empties the tag word");
    }

    #[test]
    fn psllw_immediate_shifts_each_word() {
        // MOVD mm0, eax (0x0001_0002); PSLLW mm0, 4 (0F 71 /6 imm8).
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x6e, 0xc0, 0x0f, 0x71, 0xf0, 0x04], 0x20);
        cpu.registers.set_eax(0x0001_0002);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        cpu.cycle(&mut bus).unwrap();
        // low dword loaded; word lanes 0x0002 and 0x0001 each shift left by 4.
        assert_eq!(cpu.fpu.mm(0) & 0xffff_ffff, 0x0010_0020);
    }

    // ---- Phase 4 slice A: protected-mode system instructions ----

    /// Protected-mode CPU with a GDT (base 0x100, limit 0x1f) holding one descriptor
    /// at selector 0x08. CS selector 0 => CPL 0.
    fn protected_cpu(code: &[u8], descriptor_low: u32, descriptor_high: u32) -> (Cpu386, Vec<u8>) {
        let mut memory = vec![0u8; 0x200];
        memory[..code.len()].copy_from_slice(code);
        memory[0x108..0x10c].copy_from_slice(&descriptor_low.to_le_bytes());
        memory[0x10c..0x110].copy_from_slice(&descriptor_high.to_le_bytes());
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.gdtr = DescriptorTable {
            base: 0x100,
            limit: 0x1f,
        };
        cpu.control.cr0 |= CR0_PE;
        (cpu, memory)
    }

    #[test]
    fn smsw_stores_machine_status_word() {
        // SMSW eax (0F 01 E0).
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x01, 0xe0], 0x20);
        cpu.control.cr0 = CR0_TS | CR0_MP; // 0x0A
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.eax() & 0xffff, 0x000a);
    }

    #[test]
    fn lmsw_sets_protection_enable() {
        // LMSW ax (0F 01 F0) with AX bit 0 set turns on CR0.PE.
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x01, 0xf0], 0x20);
        cpu.write_reg16(Reg16::Ax, 0x0001);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_ne!(cpu.control.cr0 & CR0_PE, 0);
    }

    #[test]
    fn clts_clears_task_switched() {
        // CLTS (0F 06).
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x06], 0x20);
        cpu.control.cr0 |= CR0_TS;
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.control.cr0 & CR0_TS, 0);
    }

    #[test]
    fn sgdt_stores_the_gdtr() {
        // SGDT [0x100] (0F 01 06 00 01): limit word then base dword.
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x01, 0x06, 0x00, 0x01], 0x200);
        cpu.gdtr = DescriptorTable {
            base: 0x1234_5678,
            limit: 0x0abc,
        };
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            u16::from_le_bytes([bus.memory[0x100], bus.memory[0x101]]),
            0x0abc
        );
        assert_eq!(
            u32::from_le_bytes(bus.memory[0x102..0x106].try_into().unwrap()),
            0x1234_5678
        );
    }

    #[test]
    fn sldt_stores_the_ldtr_selector() {
        // SLDT ax (0F 00 C0), protected mode only.
        let (mut cpu, memory) = protected_cpu(&[0x0f, 0x00, 0xc0], 0, 0);
        cpu.ldtr.selector = 0x0028;
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0028);
    }

    #[test]
    fn sldt_is_invalid_in_real_mode() {
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x00, 0xc0], 0x20);
        let mut bus = TestBus::with_memory(memory);
        let fault = cpu.execute_instruction(&mut bus).unwrap_err();
        assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
    }

    #[test]
    fn lldt_loads_the_descriptor() {
        // LDT descriptor at selector 0x08: base 0x0004_0000, limit 0x0fff, access 0x82.
        let low = 0x0000_0fff; // limit low, base low 16 = 0
        let high = 0x0000_8204; // base[23:16]=0x04, access=0x82
        let (mut cpu, memory) = protected_cpu(&[0x0f, 0x00, 0xd0], low, high);
        cpu.write_reg16(Reg16::Ax, 0x0008);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.ldtr.selector, 0x0008);
        assert_eq!(cpu.ldtr.base, 0x0004_0000);
        assert_eq!(cpu.ldtr.limit, 0x0fff);
    }

    #[test]
    fn verr_sets_zf_for_a_readable_segment() {
        // Readable data segment: access 0x92 (P, S, data, writable -> readable).
        let (mut cpu, memory) = protected_cpu(&[0x0f, 0x00, 0xe0], 0x0000_ffff, 0x0000_9200);
        cpu.write_reg16(Reg16::Ax, 0x0008);
        cpu.set_flag(FLAG_ZF, false);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert!(
            cpu.flag(FLAG_ZF),
            "VERR should set ZF for a readable segment"
        );
    }

    #[test]
    fn lar_and_lsl_read_descriptor_fields() {
        // Data segment access 0x92, byte-granular limit 0xffff.
        // LAR ax, cx (0F 02 C1); CX holds the selector.
        let (mut cpu, memory) = protected_cpu(&[0x0f, 0x02, 0xc1], 0x0000_ffff, 0x0000_9200);
        cpu.write_reg16(Reg16::Cx, 0x0008);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert!(cpu.flag(FLAG_ZF));
        assert_eq!(cpu.read_reg16(Reg16::Ax), 0x9200);

        // LSL ax, cx (0F 03 C1) -> the byte-granular limit.
        let (mut cpu, memory) = protected_cpu(&[0x0f, 0x03, 0xc1], 0x0000_ffff, 0x0000_9200);
        cpu.write_reg16(Reg16::Cx, 0x0008);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert!(cpu.flag(FLAG_ZF));
        assert_eq!(cpu.read_reg16(Reg16::Ax), 0xffff);
    }

    // ---- Phase 4 slice B: exception error codes and FPU #MF ----

    #[test]
    fn error_code_vectors_are_classified() {
        for v in [8u8, 10, 11, 12, 13, 14, 17] {
            assert!(
                vector_pushes_error_code(v),
                "vector {v} should carry a code"
            );
        }
        for v in [0u8, 1, 3, 4, 5, 6, 7, 9, 16, 18, 19] {
            assert!(!vector_pushes_error_code(v), "vector {v} carries no code");
        }
    }

    /// FLDZ; FLD1; FDIV ST0,ST1 (divide 1 by 0); FWAIT.
    const DIV_BY_ZERO_THEN_WAIT: [u8; 7] = [0xd9, 0xee, 0xd9, 0xe8, 0xd8, 0xf1, 0x9b];

    #[test]
    fn unmasked_divide_by_zero_traps_mf_on_fwait() {
        let (mut cpu, memory) = real_mode_cpu(&DIV_BY_ZERO_THEN_WAIT, 0x40);
        cpu.fpu.control = 0x037b; // default mask with ZM (bit 2) cleared
        cpu.control.cr0 |= CR0_NE;
        let mut bus = TestBus::with_memory(memory);
        for _ in 0..3 {
            cpu.cycle(&mut bus).unwrap(); // FLDZ, FLD1, FDIV
        }
        assert_ne!(cpu.fpu.status & 0x04, 0, "ZE flag set");
        let fault = cpu.execute_instruction(&mut bus).unwrap_err();
        assert!(matches!(fault, InternalFault::Exception { vector: 16, .. }));
    }

    #[test]
    fn masked_divide_by_zero_does_not_trap() {
        let (mut cpu, memory) = real_mode_cpu(&DIV_BY_ZERO_THEN_WAIT, 0x40);
        // Default control 0x037F masks every exception.
        cpu.control.cr0 |= CR0_NE;
        let mut bus = TestBus::with_memory(memory);
        for _ in 0..4 {
            cpu.cycle(&mut bus).unwrap(); // FWAIT retires normally
        }
        assert_ne!(cpu.fpu.status & 0x04, 0, "ZE flag still latched");
    }

    #[test]
    fn mf_is_suppressed_when_ne_is_clear() {
        // Unmasked exception but CR0.NE clear: the PC's FERR/IRQ13 path applies, so no
        // internal #MF. FWAIT retires.
        let (mut cpu, memory) = real_mode_cpu(&DIV_BY_ZERO_THEN_WAIT, 0x40);
        cpu.fpu.control = 0x037b;
        let mut bus = TestBus::with_memory(memory);
        for _ in 0..4 {
            cpu.cycle(&mut bus).unwrap();
        }
    }

    // ---- Phase 4 slice C: call gates and privilege-level stack switching ----

    /// Protected-mode CPU with a GDT at 0x100 holding the given (selector, low, high)
    /// descriptors. CS/SS default to ring 0 (real-mode shells, base 0); SP at 0x80.
    fn protected_cpu_with_gdt(code: &[u8], descriptors: &[(u16, u32, u32)]) -> (Cpu386, Vec<u8>) {
        let mut memory = vec![0u8; 0x400];
        memory[..code.len()].copy_from_slice(code);
        for &(sel, low, high) in descriptors {
            let off = 0x100 + (sel & !0x7) as usize;
            memory[off..off + 4].copy_from_slice(&low.to_le_bytes());
            memory[off + 4..off + 8].copy_from_slice(&high.to_le_bytes());
        }
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.registers.set_esp(0x80);
        cpu.gdtr = DescriptorTable {
            base: 0x100,
            limit: 0xff,
        };
        cpu.control.cr0 |= CR0_PE;
        (cpu, memory)
    }

    // Flat ring-0 code at 0x08, and a 386 call gate at 0x10 -> 0x08:0x40.
    const RING0_CODE: (u16, u32, u32) = (0x08, 0x0000_ffff, 0x00cf_9b00);
    const CALL_GATE_DPL0: (u16, u32, u32) = (0x10, 0x0008_0040, 0x0000_8c00);

    #[test]
    fn call_gate_same_privilege_transfers() {
        // CALL FAR 0x10:0 -> through the gate to 0x08:0x40, return pushed.
        let (mut cpu, memory) = protected_cpu_with_gdt(
            &[0x9a, 0x00, 0x00, 0x10, 0x00],
            &[RING0_CODE, CALL_GATE_DPL0],
        );
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.cs().selector, 0x08);
        assert_eq!(cpu.registers.eip, 0x40);
        // The gate is a 386 (32-bit) gate, so the return CS:EIP is two dwords.
        assert_eq!(
            cpu.registers.esp(),
            0x80 - 8,
            "return offset+selector pushed"
        );
    }

    #[test]
    fn jmp_gate_transfers_without_pushing_return() {
        // JMP FAR 0x10:0 -> same target, no return frame.
        let (mut cpu, memory) = protected_cpu_with_gdt(
            &[0xea, 0x00, 0x00, 0x10, 0x00],
            &[RING0_CODE, CALL_GATE_DPL0],
        );
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.cs().selector, 0x08);
        assert_eq!(cpu.registers.eip, 0x40);
        assert_eq!(cpu.registers.esp(), 0x80, "JMP pushes nothing");
    }

    #[test]
    fn call_gate_inter_privilege_switches_stack() {
        // Ring-3 caller through a DPL-3 gate into ring-0 code, copying two dword params.
        let ring0_data = (0x10u16, 0x0000_ffff, 0x00cf_9300);
        let gate_dpl3 = (0x30u16, 0x0008_0040, 0x0000_ec02); // DPL3 386 gate, 2 params
        let (mut cpu, mut memory) = protected_cpu_with_gdt(
            &[0x9a, 0x00, 0x00, 0x30, 0x00],
            &[RING0_CODE, ring0_data, gate_dpl3],
        );
        // Run at CPL 3 with a ring-3 CS and SS (set the cached registers directly).
        cpu.registers.set_segment(
            SegmentIndex::Cs,
            SegmentRegister {
                selector: 0x1b,
                base: 0,
                limit: 0xf_ffff,
                access: 0xfb,
                default_size_32: false,
            },
        );
        cpu.registers.set_segment(
            SegmentIndex::Ss,
            SegmentRegister {
                selector: 0x23,
                base: 0,
                limit: 0xf_ffff,
                access: 0xf3,
                default_size_32: false,
            },
        );
        cpu.registers.set_esp(0xc0);
        // Two parameters on the outer stack.
        memory[0xc0..0xc4].copy_from_slice(&0x1111u32.to_le_bytes());
        memory[0xc4..0xc8].copy_from_slice(&0x2222u32.to_le_bytes());
        // TSS at 0x300 with the ring-0 stack: ESP0 at +4, SS0 at +8.
        cpu.tr.base = 0x300;
        memory[0x304..0x308].copy_from_slice(&0x00f0u32.to_le_bytes());
        memory[0x308..0x30a].copy_from_slice(&0x0010u16.to_le_bytes());
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.cs().selector, 0x08, "entered ring-0 code");
        assert_eq!(cpu.registers.eip, 0x40);
        assert_eq!(
            cpu.registers.segment(SegmentIndex::Ss).selector,
            0x10,
            "switched to SS0"
        );
        // Frame on the new stack: 6 dwords pushed below ESP0 = 0xF0.
        assert_eq!(cpu.registers.esp(), 0xf0 - 24);
        // Return EIP (5, past the CALL) at the top; param0 above the return frame.
        assert_eq!(
            u32::from_le_bytes(bus.memory[0xd8..0xdc].try_into().unwrap()),
            5
        );
        assert_eq!(
            u32::from_le_bytes(bus.memory[0xe0..0xe4].try_into().unwrap()),
            0x1111
        );
    }

    // ---- Phase 4 slice D: hardware task switch ----

    #[test]
    fn jmp_to_tss_performs_a_task_switch() {
        // New 386 TSS at 0x380 (selector 0x18), old busy TSS at 0x300 (selector 0x20).
        let new_tss = (0x18u16, 0x0380_0067, 0x0000_8900);
        let old_tss = (0x20u16, 0x0300_0067, 0x0000_8b00);
        let ring0_data = (0x10u16, 0x0000_ffff, 0x00cf_9300);
        let (mut cpu, mut memory) = protected_cpu_with_gdt(
            &[0xea, 0x00, 0x00, 0x18, 0x00],
            &[RING0_CODE, ring0_data, new_tss, old_tss],
        );
        cpu.tr = SegmentRegister {
            selector: 0x20,
            base: 0x300,
            limit: 0x67,
            access: 0x8b,
            default_size_32: false,
        };
        let put32 =
            |m: &mut [u8], off: usize, v: u32| m[off..off + 4].copy_from_slice(&v.to_le_bytes());
        let put16 =
            |m: &mut [u8], off: usize, v: u16| m[off..off + 2].copy_from_slice(&v.to_le_bytes());
        put32(&mut memory, 0x380 + 32, 0x200); // EIP
        put32(&mut memory, 0x380 + 36, 0x0000_0002); // EFLAGS
        put32(&mut memory, 0x380 + 40, 0xaaaa); // EAX
        put32(&mut memory, 0x380 + 56, 0x00f0); // ESP
        put16(&mut memory, 0x380 + 72, 0x10); // ES
        put16(&mut memory, 0x380 + 76, 0x08); // CS
        put16(&mut memory, 0x380 + 80, 0x10); // SS
        put16(&mut memory, 0x380 + 84, 0x10); // DS
        let mut bus = TestBus::with_memory(memory);

        cpu.cycle(&mut bus).unwrap();

        assert_eq!(cpu.registers.cs().selector, 0x08, "loaded new task CS");
        assert_eq!(cpu.registers.eip, 0x200);
        assert_eq!(cpu.registers.eax(), 0xaaaa);
        assert_eq!(cpu.registers.esp(), 0x00f0);
        assert_eq!(cpu.tr.selector, 0x18, "task register points at the new TSS");
        assert_ne!(cpu.control.cr0 & CR0_TS, 0, "TS set on a task switch");
        // The outgoing task's EIP (past the 5-byte JMP) was saved into the old TSS.
        assert_eq!(
            u32::from_le_bytes(bus.memory[0x320..0x324].try_into().unwrap()),
            5
        );
        // JMP clears the old TSS busy bit in its GDT descriptor (0x8b -> 0x89).
        assert_eq!(bus.memory[0x100 + 0x20 + 5], 0x89);
    }

    // ---- Phase 1 slice 2 cleanup: BOUND and INS/OUTS ----

    #[test]
    fn bound_passes_when_in_range() {
        // BOUND AX, [0x100] (62 06 00 01); bounds [10, 20]; AX = 15.
        let (mut cpu, mut memory) = real_mode_cpu(&[0x62, 0x06, 0x00, 0x01], 0x200);
        memory[0x100..0x102].copy_from_slice(&10u16.to_le_bytes());
        memory[0x102..0x104].copy_from_slice(&20u16.to_le_bytes());
        cpu.write_reg16(Reg16::Ax, 15);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.eip, 4);
    }

    #[test]
    fn bound_raises_br_out_of_range() {
        let (mut cpu, mut memory) = real_mode_cpu(&[0x62, 0x06, 0x00, 0x01], 0x200);
        memory[0x100..0x102].copy_from_slice(&10u16.to_le_bytes());
        memory[0x102..0x104].copy_from_slice(&20u16.to_le_bytes());
        cpu.write_reg16(Reg16::Ax, 25);
        let mut bus = TestBus::with_memory(memory);
        let fault = cpu.execute_instruction(&mut bus).unwrap_err();
        assert!(matches!(fault, InternalFault::Exception { vector: 5, .. }));
    }

    #[test]
    fn insb_stores_port_byte_to_es_di() {
        // INSB (0x6C): [ES:DI] <- port[DX]. TestBus returns 0, so the 0xFF clears.
        let (mut cpu, mut memory) = real_mode_cpu(&[0x6c], 0x200);
        memory[0x100] = 0xff;
        cpu.write_reg16(Reg16::Dx, 0x03f8);
        cpu.write_reg16(Reg16::Di, 0x0100);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(bus.memory[0x100], 0x00);
        assert_eq!(cpu.read_reg16(Reg16::Di), 0x0101);
        assert!(
            bus.trace
                .cycles()
                .iter()
                .any(|c| c.kind == BusAccessKind::IoRead && c.address == 0x03f8)
        );
    }

    #[test]
    fn rep_outsw_writes_words_from_ds_si() {
        // REP OUTSW (F3 6F): write CX words from [DS:SI] to port[DX].
        let (mut cpu, memory) = real_mode_cpu(&[0xf3, 0x6f], 0x200);
        cpu.write_reg16(Reg16::Cx, 2);
        cpu.write_reg16(Reg16::Si, 0x0100);
        cpu.write_reg16(Reg16::Dx, 0x03f8);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        let writes = bus
            .trace
            .cycles()
            .iter()
            .filter(|c| {
                c.kind == BusAccessKind::IoWrite && c.width == BusWidth::Word && c.address == 0x03f8
            })
            .count();
        assert_eq!(writes, 2);
        assert_eq!(cpu.read_reg16(Reg16::Cx), 0);
        assert_eq!(cpu.read_reg16(Reg16::Si), 0x0104);
    }
}
