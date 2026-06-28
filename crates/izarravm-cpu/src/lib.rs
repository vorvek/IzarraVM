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
const FLAG_VM: u32 = 0x0002_0000; // bit 17, virtual-8086 mode

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
// (bit 0) is off (no FPU is modeled). TSC (bit 4) and MSR (bit 5) are on: RDTSC and
// RDMSR/WRMSR with the K6 model-specific register set are implemented. MMX (bit 23) is
// on to match the GSW-586 lore. The rest stay off until the matching behavior exists.
const CPUID_FEATURE_TSC: u32 = 1 << 4;
const CPUID_FEATURE_MSR: u32 = 1 << 5;
const CPUID_FEATURE_CX8: u32 = 1 << 8; // CMPXCHG8B
const CPUID_FEATURE_MMX: u32 = 1 << 23;
const CPUID_FEATURES_EDX: u32 =
    CPUID_FEATURE_TSC | CPUID_FEATURE_MSR | CPUID_FEATURE_CX8 | CPUID_FEATURE_MMX;

// Extended-leaf (0x80000001) feature flags. The AMD Processor Recognition app note (Table
// 6) places three K6 additions at their own bit positions: SYSCALL/SYSRET (bit 10), integer
// CMOVcc (bit 15) and FP FCMOVcc (bit 16). TSC/MSR/CX8/MMX share the standard positions. As
// with leaf 1, only emulated features are set, so FPU/VME/DE/PSE/MCE/PGE stay clear (the
// GSW-586 emulates none of them, and the real K6 generates no machine-check exception).
const CPUID_EXT_FEATURE_SYSCALL: u32 = 1 << 10;
const CPUID_EXT_FEATURE_CMOV: u32 = 1 << 15;
const CPUID_EXT_FEATURE_FCMOV: u32 = 1 << 16;
const CPUID_EXT_FEATURES_EDX: u32 = CPUID_FEATURE_TSC
    | CPUID_FEATURE_MSR
    | CPUID_FEATURE_CX8
    | CPUID_FEATURE_MMX
    | CPUID_EXT_FEATURE_SYSCALL
    | CPUID_EXT_FEATURE_CMOV
    | CPUID_EXT_FEATURE_FCMOV;

// CR4 bits with a modeled effect. TSD (bit 2) makes RDTSC privileged: when set, RDTSC
// outside CPL 0 raises #GP(0). The other CR4 bits are storage only.
const CR4_TSD: u32 = 0x0000_0004;

// K6 model-specific register addresses (the value the RDMSR/WRMSR ECX selector carries).
// This is the full software-visible set from the AMD-K6 BIOS and Software Tools guide:
// the two machine-check registers, the time-stamp counter, the AMD extended-feature and
// SYSCALL-target registers, and the write-handling control register.
const MSR_MCAR: u32 = 0x0000_0000; // machine-check address
const MSR_MCTR: u32 = 0x0000_0001; // machine-check type
const MSR_TSC: u32 = 0x0000_0010; // time-stamp counter
const MSR_EFER: u32 = 0xc000_0080; // extended feature enable (bit 0 = SCE)
const MSR_STAR: u32 = 0xc000_0081; // SYSCALL/SYSRET target address
const MSR_WHCR: u32 = 0xc000_0082; // write handling control

// EFER bit 0: System Call Extension. SYSCALL and SYSRET raise #UD when it is clear.
const EFER_SCE: u64 = 0x1;

// Writable masks for the two MSRs with reserved bits. Per the AMD-K6 guide (EFER Table 40,
// STAR Table 41) writing a 1 to any reserved bit raises #GP(0). EFER defines only SCE (bits
// 63-1 reserved); STAR holds the target EIP (31-0) and the CS/SS selector base (47-32), with
// bits 63-48 reserved.
const EFER_WRITABLE: u64 = EFER_SCE;
const STAR_WRITABLE: u64 = 0x0000_ffff_ffff_ffff;

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

    /// A 4-GByte flat 32-bit segment (base 0, full limit). Used by SYSCALL/SYSRET, which
    /// load fixed flat descriptors from the selector in STAR without touching the GDT.
    pub const fn flat(selector: u16, access: u8) -> Self {
        Self {
            selector,
            base: 0,
            limit: 0xffff_ffff,
            access,
            default_size_32: true,
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
    // CR4 arrived on the 586. Only TSD (bit 2) has a modeled effect here (it gates
    // RDTSC outside CPL 0); the rest is plain read/write storage so MOV CR4 round-
    // trips. Reset is all zero.
    pub cr4: u32,
}

/// The K6 model-specific register file behind RDMSR/WRMSR. MCAR/MCTR/WHCR are plain
/// 64-bit storage with no modeled effect (no machine-check or write-allocate logic is
/// emulated). EFER bit 0 (SCE) and STAR feed SYSCALL/SYSRET. `tsc_offset` is added to
/// the running core-clock count so a WRMSR to the TSC can rebase it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Msrs {
    pub mcar: u64,
    pub mctr: u64,
    pub whcr: u64,
    pub efer: u64,
    pub star: u64,
    pub tsc_offset: u64,
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

    /// True when the 586-class instruction additions are present (RDTSC, RDMSR/WRMSR,
    /// CMOVcc, CMPXCHG8B, SYSCALL/SYSRET, RSM). The physical part is always a 586, so
    /// only a guest that throttled to a lower GSW mode sees these as #UD.
    const fn has_pentium_isa(self) -> bool {
        matches!(self, Self::I586)
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

/// Direct-mapped TLB size (entries). Covers TLB_ENTRIES * 4 KiB before two pages
/// collide on a slot; a 386/486 had 32, this keeps a few more so a fetch/execute
/// loop's interleaved code and data pages do not evict each other every step.
const TLB_ENTRIES: usize = 64;
const PREFETCH_WINDOW_BYTES: usize = 32;
const TRACKED_WRITE_PAGES: usize = 8;

#[derive(Clone, Copy)]
struct TlbEntry {
    /// Linear page number (linear >> 12). Meaningful only when `generation` is current.
    tag: u32,
    /// Physical page base (pte & 0xffff_f000).
    phys: u32,
    /// Live only while equal to the owning Tlb's `generation`.
    generation: u32,
    /// Cached combined PDE&PTE R/W bit (page is writable).
    writable: bool,
    /// Cached combined PDE&PTE U/S bit (page is user-accessible).
    user: bool,
    /// PTE dirty bit already set, so a write hit needs no page-table update.
    dirty: bool,
}

impl TlbEntry {
    const EMPTY: Self = Self {
        tag: 0,
        phys: 0,
        generation: 0,
        writable: false,
        user: false,
        dirty: false,
    };
}

/// Direct-mapped linear->physical translation cache. `generation` bumps to flush
/// in O(1); an entry is live only while its `generation` matches. Contents are
/// microarchitectural, so the TLB is transparent to Cpu386 equality and prints
/// terse. Non-snooping, which matches real x86: a guest must INVLPG or reload CR3
/// after editing a page-table entry, and IzarraVM flushes on exactly those events.
#[derive(Clone)]
struct Tlb {
    entries: [TlbEntry; TLB_ENTRIES],
    generation: u32,
}

impl Default for Tlb {
    fn default() -> Self {
        Self {
            entries: [TlbEntry::EMPTY; TLB_ENTRIES],
            generation: 1,
        }
    }
}

impl Tlb {
    fn slot(page: u32) -> usize {
        (page as usize) & (TLB_ENTRIES - 1)
    }

    fn lookup(&self, page: u32) -> Option<TlbEntry> {
        let e = self.entries[Self::slot(page)];
        (e.generation == self.generation && e.tag == page).then_some(e)
    }

    fn insert(&mut self, page: u32, phys: u32, writable: bool, user: bool, dirty: bool) {
        self.entries[Self::slot(page)] = TlbEntry {
            tag: page,
            phys,
            generation: self.generation,
            writable,
            user,
            dirty,
        };
    }

    /// Drop every cached translation (CR0/CR3 write, task switch, INVLPG). The rare
    /// generation wrap clears the table so stale gen-0 entries cannot alias.
    fn flush(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.entries = [TlbEntry::EMPTY; TLB_ENTRIES];
            self.generation = 1;
        }
    }
}

impl PartialEq for Tlb {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for Tlb {}

impl std::fmt::Debug for Tlb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tlb {{ {TLB_ENTRIES} entries }}")
    }
}

#[derive(Clone, Default)]
struct CodePageCache {
    valid: bool,
    cs: SegmentRegister,
    linear_page: u32,
    physical_page: u32,
}

impl PartialEq for CodePageCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for CodePageCache {}

impl std::fmt::Debug for CodePageCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CodePageCache")
    }
}

#[derive(Clone)]
struct PrefetchWindow {
    bytes: [u8; PREFETCH_WINDOW_BYTES],
    cs: SegmentRegister,
    linear_base: u32,
    physical_base: u32,
    len: u8,
}

impl Default for PrefetchWindow {
    fn default() -> Self {
        Self {
            bytes: [0; PREFETCH_WINDOW_BYTES],
            cs: SegmentRegister::default(),
            linear_base: 0,
            physical_base: 0,
            len: 0,
        }
    }
}

impl PrefetchWindow {
    fn invalidate(&mut self) {
        self.len = 0;
    }

    fn get(&self, cs: SegmentRegister, linear: u32) -> Option<(u8, u32)> {
        if self.cs != cs {
            return None;
        }
        let offset = linear.checked_sub(self.linear_base)? as usize;
        if offset < usize::from(self.len) {
            Some((self.bytes[offset], self.physical_base + offset as u32))
        } else {
            None
        }
    }

    fn physical_page(&self) -> Option<u32> {
        (self.len != 0).then_some(self.physical_base >> 12)
    }
}

impl PartialEq for PrefetchWindow {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for PrefetchWindow {}

impl std::fmt::Debug for PrefetchWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PrefetchWindow {{ {} bytes }}", self.len)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cpu386 {
    pub registers: Registers,
    pub fpu: X87,
    pub control: ControlRegisters,
    pub msr: Msrs,
    pub gdtr: DescriptorTable,
    pub idtr: DescriptorTable,
    /// Local descriptor table register and task register. The selector plus the
    /// hidden base/limit cached from the system descriptor at load time.
    pub ldtr: SegmentRegister,
    pub tr: SegmentRegister,
    pub elapsed_clocks: u64,
    // Fractional remainder carried by the per-level cycle scaling so the cheap
    // ops do not round to zero. Reset on a level change. See scale_clocks.
    timing_rem: u64,
    pub halted: bool,
    // STI sets this to block maskable interrupt delivery for one instruction:
    // the 386 holds off interrupts until the instruction after STI, which makes
    // the STI; HLT idle idiom safe (the HLT runs before any interrupt is taken).
    interrupt_shadow: bool,
    // The guest-facing instruction-set level. Defaults to the full ISA (I586) so
    // firmware POST is never restricted; the Machine lowers it from the live Lotura
    // GSW mode write. See CpuLevel.
    level: CpuLevel,
    // Caches linear->physical page translations so paged protected mode (DOS
    // extenders, Win9x) does not re-walk the two-level page table on every access.
    // Flushed on CR0/CR3 writes, task switch, and INVLPG.
    tlb: Tlb,
    code_page: CodePageCache,
    prefetch: PrefetchWindow,
    written_pages: [Option<u32>; TRACKED_WRITE_PAGES],
    written_pages_overflow: bool,
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

/// A decoded memory addressing-mode *descriptor* (NOT a resolved address). It holds the
/// segment plus the base/index register numbers, scale, and displacement read from the
/// instruction bytes, so the effective offset can be recomputed from live registers each
/// time the decoded instruction is replayed. `resolve_addr_mode` turns this into a live
/// `RmOperand::Memory`. `address_size` is carried here (rather than passed to
/// `resolve_addr_mode`) so resolve is a pure function of the descriptor and so it can pick
/// 16- vs 32-bit register width and the matching offset wraparound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AddrMode {
    segment: SegmentIndex,
    base: Option<u8>,
    index: Option<u8>,
    scale: u8,
    disp: i32,
    address_size: AddressSize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodedOperand {
    Reg(u8),
    Mem(AddrMode),
}

/// Which decode/execute path an opcode takes. This is the SINGLE source of truth for routing:
/// `route_group` classifies an opcode once, and both `decode` (to decide what to pre-parse) and
/// `execute_decoded` (to decide how to execute) match on the same `DecodeGroup` value. Never
/// re-derive a routing predicate inline in either function — a one-sided edit would have `decode`
/// parse a ModRM the executor never consumes (or make a `.expect()` panic).
///
/// Extension pattern for the remaining group-conversion tasks: add ONE variant here, ONE arm in
/// `route_group`, ONE parse arm in `decode`, and ONE execute arm in `execute_decoded`. `Fallback`
/// is everything still on the legacy fused dispatch; `TwoByteFallback` is the un-converted 0F map.
///
/// For a two-byte (0F) group there are two extra rules, both because `decode` already folded the
/// second byte into `insn.opcode` as 0x0F00 | second (see the "two-byte (0F) decode convention"
/// block in `decode`): (a) the execute arm MUST dispatch off the full `insn.opcode` (u16) BEFORE
/// any `as u8` narrowing — `0x0Fb6` narrows to `0xb6` and would alias a single-byte opcode (see the
/// aliasing note in `execute_datamove_decoded`); and (b) the execute arm must NOT re-read the
/// second byte and must NOT re-apply the ISA gate — both already happened once in `decode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodeGroup {
    /// The ALU block: ADD/OR/ADC/SBB/AND/SUB/XOR/CMP across forms 0-5
    /// (`opcode < 0x40 && (opcode & 7) < 6`).
    Alu,
    /// The data-movement block: MOV r/m<->reg (0x88-0x8b), MOV r/m<->Sreg (0x8c/0x8e),
    /// LEA (0x8d), MOV (E)AX<->moffs (0xa0-0xa3), MOV r,imm (0xb0-0xbf), MOV r/m,imm group 11
    /// (0xc6/0xc7), XCHG r/m,reg (0x86/0x87), XCHG reg,(E)AX (0x90-0x97; 0x90 is NOP), and the
    /// two-byte MOVZX/MOVSX (0F B6/B7/BE/BF). The 0F forms join here now that the two-byte decode
    /// convention exists: `decode` folds the second opcode byte into `insn.opcode` as 0x0F00 |
    /// second, so `route_group` can classify them and the executor can pre-parse their ModRM.
    DataMove,
    /// The stack block: PUSH/POP reg (0x50-0x5f), PUSH/POP seg (0x06/0x07/0x0e/0x16/0x17/
    /// 0x1e/0x1f), PUSH imm (0x68/0x6a), POP r/m (0x8f), PUSHA/POPA (0x60/0x61),
    /// PUSHF/POPF (0x9c/0x9d), ENTER/LEAVE (0xc8/0xc9). `decode` reads the ModRM (for 0x8f)
    /// or the immediate bytes (for 0x68/0x6a/0xc8) and stores them; the executor re-uses
    /// pre-parsed values so no instruction bytes are re-fetched.
    Stack,
    /// The arithmetic /ext groups 1-4, every one a ModRM whose `reg` field selects the sub-op:
    /// group 1 ALU r/m,imm (0x80/0x81/0x82/0x83), group 2 shift/rotate (0xc0/0xc1/0xd0-0xd3),
    /// group 3 TEST/NOT/NEG/MUL/IMUL/DIV/IDIV (0xf6/0xf7), and group 4 INC/DEC byte (0xfe).
    /// `decode` parses the ModRM + addressing descriptor and then the immediate IF this opcode
    /// carries one: group 1 always (imm8 for 0x80/0x82, imm16/32 for 0x81, sign-extended imm8
    /// for 0x83), group 2's count-by-imm8 forms (0xc0/0xc1) always, but group 3's immediate is
    /// present ONLY for the TEST sub-op (`reg <= 1`) — NOT/NEG/MUL/IMUL/DIV/IDIV have none, so
    /// the byte budget changes with `reg`. The executor reuses `self.alu`/`shift_rotate`/`mul`/
    /// `div`/`inc_dec` verbatim so the flag and #DE/#UD fault logic lives in exactly one place.
    /// Group 5 (0xff: indirect CALL/JMP, control flow) is deliberately NOT here.
    Group,
    /// The relative-displacement + loop control-flow block (task A6a): Jcc short (0x70-0x7f, rel8),
    /// the two-byte Jcc near (0F 80-0F 8F, rel16/32 — folded into `insn.opcode` as 0x0F8x by the
    /// two-byte convention), JMP short (0xeb, rel8), JMP near (0xe9, rel16/32), CALL near (0xe8,
    /// rel16/32), and the loop/JCXZ branches (0xe0-0xe3, rel8). Every one ends in a single
    /// `relative_jump(disp, operand_size)` when taken, so `decode` reads the displacement (sign-
    /// extended to i32 per the rel8/rel16/rel32 width, charging its fetch once) and stores it in
    /// `insn.imm`; the executor re-uses it without re-fetching. eip is already at the instruction
    /// end when the executor runs (decode advanced it), so the eip-relative target math is bit-for-
    /// bit identical to the fused path. The far/indirect/RET/INT control flow and 0xFF group 5 stay
    /// on Fallback/TwoByteFallback (task A6b) — do NOT route them here.
    Branch,
    /// The far/indirect/RET/INT control-flow block (task A6b): CALL far direct (0x9a) and JMP far
    /// direct (0xea), RET near (0xc3) and RET near imm16 (0xc2), RETF (0xcb) and RETF imm16 (0xca),
    /// INT3 (0xcc), INT n (0xcd), INTO (0xce), IRET (0xcf), and the heterogeneous 0xff group 5
    /// (/0 INC, /1 DEC, /2 near-indirect CALL, /3 far-indirect CALL, /4 near-indirect JMP, /5
    /// far-indirect JMP, /6 PUSH r/m, /7 #UD). `decode` reads each form's immediate (the far-pointer
    /// offset+selector for 0x9a/0xea into `imm`/`imm2`, the imm16 stack-release for 0xc2/0xca into
    /// `imm`, the imm8 vector for 0xcd into `imm`) or parses the ModRM + addressing descriptor (for
    /// 0xff); the executor consumes those and re-fetches nothing. The indirect CALL/JMP read their
    /// target FROM MEMORY at execute time, so decode captures only the addressing descriptor, NOT the
    /// target. Every executor reuses the existing far-call/far-jump, ret/retf, interrupt-delivery,
    /// IRET, inc_dec, and push helpers verbatim, so the protected-mode descriptor loads, gates,
    /// faults, interrupt-shadow/IF semantics, and clocks are byte-identical to the fused path.
    ControlFlow,
    /// The flags and miscellaneous register block (task A7): TEST r/m,reg (0x84/0x85), INC/DEC reg
    /// (0x40-0x4f), CBW/CWDE (0x98), CWD/CDQ (0x99), SAHF (0x9e), LAHF (0x9f), and the single
    /// flag-bit ops CMC/CLC/STC/CLI/STI/CLD/STD (0xf5/0xf8-0xfd). TEST (0x84/0x85) carries a
    /// ModRM r/m form and is the only A7 opcode `decode` pre-parses; all other A7 opcodes carry no
    /// encoded operand. The executor reuses `alu` (AND-for-flags, no write-back), `inc_dec`
    /// (CF preserved), and the existing flag setters verbatim; STI's interrupt shadow is set in the
    /// executor exactly as the fused handler did.
    FlagsMisc,
    /// The string-operation block (task A8): MOVS (0xa4/0xa5), CMPS (0xa6/0xa7), STOS (0xaa/0xab),
    /// LODS (0xac/0xad), and SCAS (0xae/0xaf). None carry a ModRM or an immediate — the operands are
    /// implicit (DS:SI source, ES:DI destination, accumulator), so `decode` pre-parses nothing beyond
    /// the prefixes + opcode it already read (the REP/REPNE prefix and any segment override are
    /// captured in `insn.prefixes` by `read_prefixes`). The executor is a thin call to the existing
    /// `run_string` helper with `insn.prefixes` passed through, exactly as the fused arms did, so the
    /// REP loop, the REPE/REPNE ZF-termination, the DF-driven SI/DI increment/decrement, the operand-
    /// size element width, the DS:SI segment override on the source (ES:DI destination fixed), and the
    /// per-iteration data-access clocks all stay in `run_string`/`string_step` unchanged. The TEST
    /// AL/AX,imm forms that share the 0xa8/0xa9 neighbourhood are NOT string ops and stay on Fallback.
    StringOps,
    /// Not yet converted to the split: decode parses nothing extra and `execute_decoded` hands the
    /// opcode to the shared fused `dispatch_opcode`.
    Fallback,
    /// An un-converted two-byte (0F) opcode (`opcode & 0xff00 == 0x0f00`). `decode` already folded
    /// the second byte into `insn.opcode` as 0x0F00 | second, read + charged it, and applied the
    /// ISA gate; `execute_decoded` hands the second byte (`insn.opcode as u8`) straight to
    /// `execute_two_byte` without re-reading or re-gating. Distinct from `Fallback` so the routing
    /// predicate (`& 0xff00 == 0x0f00`) lives only in `route_group` and the `as u8` narrowing can
    /// never alias a 0F opcode onto a single-byte one (no arm-ordering dependence).
    TwoByteFallback,
}

/// How decode charges the instruction-fetch clocks when an instruction is replayed from a
/// cache. Unused in Stage A: decode here charges fetch clocks through its real `fetch_u8`
/// reads (exactly as the fused path did), so `execute_decoded` charges nothing extra. This
/// type is populated with a placeholder (the real instruction length, zero wait states) and
/// is only fully exercised in the later cache stage.
// Stage-A scaffold: the variants/fields are written but not yet read; the cache stage
// (Stage B) consumes them to replay fetch clocks without re-reading the bus.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
enum FetchCost {
    Uniform { len: u8, wait_states: u8 },
    PerByte([u8; 15], u8),
}

/// A decoded instruction: the prefix/opcode/operand-size results plus, for opcodes already
/// converted to the decode/execute split, the pre-parsed ModRM, operand descriptor, and
/// immediate. Opcodes still on the legacy path leave `modrm`/`operand` as `None` (and `imm` 0)
/// and are re-read by `execute_instruction_legacy`.
// Stage-A scaffold: `imm` is read by the converted ALU/group immediate forms and `imm2` by the
// converted ENTER (and any other second-immediate form); the `len`/`fetch` placeholders are
// populated by `decode` but stay unread until the cache stage lands.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
struct DecodedInsn {
    /// eip of the first byte of the instruction (before any prefixes), used to rewind so the
    /// legacy fallback re-reads from the instruction start.
    start_eip: u32,
    len: u8,
    fetch: FetchCost,
    prefixes: Prefixes,
    opcode: u16,
    operand_size: OperandSize,
    address_size: AddressSize,
    modrm: Option<ModRm>,
    operand: Option<DecodedOperand>,
    /// The instruction's primary immediate. Also carries the moffs displacement for the direct-
    /// address MOV forms 0xA0-0xA3 (decode fetches it address-size-wide; the executor uses it as
    /// the memory offset, not as a data immediate).
    imm: u32,
    /// A second immediate when the encoding carries two: ENTER's nesting level (0xc8, masked to
    /// 5 bits in `decode`). Available for any future group/other form that needs a second
    /// immediate; left 0 for the single-immediate and no-immediate opcodes.
    imm2: u32,
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

    fn invalidate_code_caches(&mut self) {
        self.code_page.valid = false;
        self.prefetch.invalidate();
    }

    fn flush_tlb_and_code_caches(&mut self) {
        self.tlb.flush();
        self.invalidate_code_caches();
    }

    fn set_eip(&mut self, eip: u32) {
        self.registers.eip = eip;
        self.prefetch.invalidate();
    }

    fn record_write_page(&mut self, physical: u32) {
        let page = physical >> 12;
        if self.written_pages.contains(&Some(page)) {
            return;
        }
        if let Some(slot) = self.written_pages.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(page);
        } else {
            self.written_pages_overflow = true;
        }
    }

    fn begin_instruction(&mut self) {
        // A 486 prefetch queue is a snapshot: writes to already fetched bytes are
        // not observed until control flow or the next refill invalidates the queue.
        if self.written_pages_overflow {
            self.prefetch.invalidate();
        } else if let Some(code_page) = self.prefetch.physical_page() {
            if self.written_pages.contains(&Some(code_page)) {
                self.prefetch.invalidate();
            }
        }
        self.written_pages = [None; TRACKED_WRITE_PAGES];
        self.written_pages_overflow = false;
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
        self.timing_rem = 0;
    }

    /// Scale a retired instruction's clocks by the active level's timing factor,
    /// carrying the fractional remainder so a run of cheap ops is not rounded to
    /// zero. This is the single per-mode timing dial; it feeds both the CPU's
    /// own clock counter and, through the returned CycleOutcome, the machine's
    /// device timing.
    ///
    /// May return 0 for a cheap op in a faster mode; that is safe because the
    /// remainder carry guarantees a clock tick within a few instructions and the
    /// machine's batch loop advances on instruction progress, not on clocks alone.
    fn scale_clocks(&mut self, clocks: u32) -> u64 {
        let (num, den) = level_timing(self.level);
        let scaled = u64::from(clocks) * u64::from(num) + self.timing_rem;
        self.timing_rem = scaled % u64::from(den);
        scaled / u64::from(den)
    }

    /// Reported (L1 KB, L2 KB) cache for the live level. Cosmetic, no timing effect.
    pub fn cache_kb(&self) -> (u16, u16) {
        self.level.cache_kb()
    }

    pub fn is_paging_enabled(&self) -> bool {
        self.control.cr0 & CR0_PG != 0
    }

    /// Virtual-8086 mode: the VM flag set while in protected mode. A V86 task runs
    /// 8086 code with real-mode segment addressing but always at CPL 3.
    fn is_v86_mode(&self) -> bool {
        self.is_protected_mode() && self.registers.eflags & FLAG_VM != 0
    }

    /// The I/O privilege level (EFLAGS bits 12-13).
    fn iopl(&self) -> u8 {
        ((self.registers.eflags >> 12) & 3) as u8
    }

    /// IOPL-sensitive instructions (CLI, STI, PUSHF/POPF, INT n) fault to the monitor
    /// inside a V86 task when IOPL is below 3.
    fn check_v86_iopl(&self) -> ExecResult<()> {
        if self.is_v86_mode() && self.iopl() < 3 {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(0),
            });
        }
        Ok(())
    }

    /// Current privilege level. The CPL lives in the low two bits of the CS selector. Real
    /// mode has no protection rings, so it is always CPL 0 there; a V86 task is always
    /// CPL 3; otherwise protected mode can run at a lower privilege.
    fn current_privilege_level(&self) -> u8 {
        if self.is_v86_mode() {
            3
        } else if self.is_protected_mode() {
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
                let charged = self.scale_clocks(61);
                self.elapsed_clocks += charged;
                return Ok(CycleOutcome {
                    core_clocks: charged.min(u64::from(u32::MAX)) as u32,
                    halted: false,
                });
            }
        }

        self.begin_instruction();
        let start_eip = self.registers.eip;
        let start_cs = self.registers.cs().selector;
        let result = self
            .decode(bus)
            .and_then(|insn| self.execute_decoded(&insn, bus));
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(InternalFault::Exception { vector, error_code }) => {
                self.set_eip(start_eip);
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

        let charged = self.scale_clocks(outcome.core_clocks);
        self.elapsed_clocks += charged;
        Ok(CycleOutcome {
            core_clocks: charged.min(u64::from(u32::MAX)) as u32,
            halted: outcome.halted,
        })
    }

    /// The single routing authority for the decode/execute split: classify an opcode into the
    /// group whose dedicated split path handles it, or `Fallback` for the shared fused dispatch.
    /// `decode` and `execute_decoded` both call this and match on the result, so the predicate
    /// lives in exactly one place. `prefixes` is taken (unused for the ALU group) because future
    /// groups route on it (e.g. the 0x0F two-byte map, or operand-size-sensitive forms).
    fn route_group(opcode: u16, _prefixes: Prefixes) -> DecodeGroup {
        // Two-byte (0F) map — the ONE place the `& 0xff00 == 0x0f00` predicate lives. `decode`
        // folds the second byte into `opcode` as 0x0F00 | second, so a 0F opcode is classified by
        // its low byte. MOVZX/MOVSX (0F B6/B7/BE/BF) are data movement and run through the split;
        // every other 0F opcode is `TwoByteFallback` (the un-converted fused `execute_two_byte`).
        // Handled first so the single-byte predicates below never see a 0F-high-byte value.
        if opcode & 0xff00 == 0x0f00 {
            return match opcode & 0xff {
                0xb6 | 0xb7 | 0xbe | 0xbf => DecodeGroup::DataMove,
                // Two-byte Jcc near (0F 80-0F 8F, rel16/32). The branch group (task A6a) handles
                // these; every other 0F opcode stays on the un-converted fused path.
                0x80..=0x8f => DecodeGroup::Branch,
                _ => DecodeGroup::TwoByteFallback,
            };
        }
        // ALU block: ADD/OR/ADC/SBB/AND/SUB/XOR/CMP, forms 0-5 (`op = (opcode>>3)&7`,
        // `form = opcode & 7`; forms 6/7 are the segment PUSH/POP and are NOT ALU).
        if opcode < 0x40 && (opcode & 0x07) < 6 {
            return DecodeGroup::Alu;
        }
        // Single-byte data-movement block. Listed explicitly (not a range) because the surrounding
        // opcodes are unrelated: 0x8f is POP r/m, 0xa4-0xaf are the string ops, 0xc4/0xc5 are
        // LES/LDS. 0x90-0x97 is XCHG reg,(E)AX with 0x90 = NOP. The MOVZX/MOVSX two-byte forms are
        // intentionally absent (see `DecodeGroup::DataMove`).
        if matches!(
            opcode,
            0x86 | 0x87
                | 0x88
                | 0x89
                | 0x8a
                | 0x8b
                | 0x8c
                | 0x8d
                | 0x8e
                | 0x90..=0x97
                | 0xa0..=0xa3
                | 0xb0..=0xbf
                | 0xc6
                | 0xc7
        ) {
            return DecodeGroup::DataMove;
        }
        // Stack block: PUSH/POP reg, PUSH/POP seg, PUSH imm, POP r/m, PUSHA/POPA,
        // PUSHF/POPF, ENTER/LEAVE. 0xFF (group 5, which includes PUSH r/m /6) is a
        // separate multi-sub-op group handled as a unit by task A5 — do NOT list it here.
        if matches!(
            opcode,
            0x06 | 0x07 | 0x0e | 0x16 | 0x17 | 0x1e | 0x1f | 0x50
                ..=0x5f | 0x60 | 0x61 | 0x68 | 0x6a | 0x8f | 0x9c | 0x9d | 0xc8 | 0xc9
        ) {
            return DecodeGroup::Stack;
        }
        // Arithmetic /ext groups 1-4 (every one a ModRM whose `reg` selects the sub-op): group 1
        // ALU r/m,imm (0x80-0x83), group 2 shift/rotate (0xc0/0xc1/0xd0-0xd3), group 3 TEST/NOT/
        // NEG/MUL/IMUL/DIV/IDIV (0xf6/0xf7), group 4 INC/DEC byte (0xfe). 0xff (group 5) is the
        // indirect-CALL/JMP control-flow group and stays on Fallback — do NOT list it here.
        if matches!(
            opcode,
            0x80..=0x83 | 0xc0 | 0xc1 | 0xd0..=0xd3 | 0xf6 | 0xf7 | 0xfe
        ) {
            return DecodeGroup::Group;
        }
        // Relative-displacement + loop control flow (task A6a): Jcc short (0x70-0x7f), the loop/JCXZ
        // branches (0xe0-0xe3), CALL near (0xe8), JMP near (0xe9), JMP short (0xeb). The two-byte
        // Jcc near forms are routed in the 0F block above.
        if matches!(opcode, 0x70..=0x7f | 0xe0..=0xe3 | 0xe8 | 0xe9 | 0xeb) {
            return DecodeGroup::Branch;
        }
        // Far/indirect/RET/INT control flow + 0xff group 5 (task A6b): CALL/JMP far direct
        // (0x9a/0xea), RET/RETF with and without an imm16 release (0xc2/0xc3/0xca/0xcb), INT3/INT n/
        // INTO/IRET (0xcc-0xcf), and 0xff (group 5: INC/DEC r/m, near/far indirect CALL/JMP, PUSH
        // r/m, /7 #UD). These change CS/segment state and are delivered through the existing
        // far-call/far-jump/ret/retf/interrupt/IRET helpers, which the executor reuses verbatim.
        if matches!(
            opcode,
            0x9a | 0xc2 | 0xc3 | 0xca | 0xcb | 0xcc | 0xcd | 0xce | 0xcf | 0xea | 0xff
        ) {
            return DecodeGroup::ControlFlow;
        }
        // Flags + misc register block (task A7): TEST r/m,reg (0x84/0x85), INC/DEC reg (0x40-0x4f),
        // CBW/CWDE (0x98), CWD/CDQ (0x99), SAHF/LAHF (0x9e/0x9f), and the single flag-bit ops
        // CMC/CLC/STC/CLI/STI/CLD/STD (0xf5/0xf8-0xfd). None carry an immediate; only 0x84/0x85
        // carry a ModRM (parsed in `decode`).
        if matches!(
            opcode,
            0x40..=0x4f
                | 0x84
                | 0x85
                | 0x98
                | 0x99
                | 0x9e
                | 0x9f
                | 0xf5
                | 0xf8
                | 0xf9
                | 0xfa
                | 0xfb
                | 0xfc
                | 0xfd
        ) {
            return DecodeGroup::FlagsMisc;
        }
        // String operations (task A8): MOVS (0xa4/0xa5), CMPS (0xa6/0xa7), STOS (0xaa/0xab), LODS
        // (0xac/0xad), SCAS (0xae/0xaf). None carry a ModRM or an immediate. 0xa8/0xa9 (TEST AL/AX,imm)
        // sit between them and are deliberately excluded — they are not string ops and stay Fallback.
        if matches!(opcode, 0xa4..=0xa7 | 0xaa..=0xaf) {
            return DecodeGroup::StringOps;
        }
        DecodeGroup::Fallback
    }

    /// Stage A of the decode/execute split. Reads the prefixes and opcode (mirroring the top
    /// of the legacy fused path) and, for the opcodes already converted to the split, parses
    /// the ModRM + addressing-mode descriptor up front. Opcodes still on the legacy path leave
    /// `modrm`/`operand` as `None`; `execute_decoded` hands them to the shared fused dispatch,
    /// which re-reads their ModRM/immediates from the post-opcode eip.
    ///
    /// Clock note (rule 2): decode's real `fetch_u8` reads charge the instruction-fetch clocks
    /// for the prefixes + opcode exactly once. `execute_decoded` charges nothing extra: the
    /// split opcode runs from the pre-decoded operand, and the legacy fallback continues the
    /// fused dispatch from where decode left off (it does NOT re-read the prefixes/opcode).
    fn decode<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<DecodedInsn> {
        let start_eip = self.registers.eip;
        let prefixes = self.read_prefixes(bus)?;
        let opcode = self.fetch_u8(bus)?;
        if prefixes.lock {
            // The LOCK check runs on the first opcode byte and peeks (does not consume) the byte
            // after it — for 0F that peek is the second opcode byte, so it must happen before the
            // second-byte fetch below, exactly as the fused path ordered it.
            self.check_lock_target(bus, opcode)?;
        }
        let operand_size = self.operand_size(prefixes);
        let address_size = self.address_size(prefixes);

        // The two-byte (0F) decode convention. When the first byte is 0F, read the second byte
        // here — charging its instruction-fetch exactly once — and fold it into `insn.opcode` as
        // `0x0F00 | second`. Every later 0F group routes on this combined value, and the fused
        // fallback (`execute_two_byte`) consumes the second byte from `insn.opcode as u8` rather
        // than re-reading it. The 286/586 ISA #UD gates apply once, right after the read (matching
        // the point the fused path faulted), with the firmware-ROM exemption preserved.
        let opcode = if opcode == 0x0f {
            let second = self.fetch_u8(bus)?;
            self.check_two_byte_isa_gate(second)?;
            0x0f00u16 | u16::from(second)
        } else {
            u16::from(opcode)
        };

        let mut insn = DecodedInsn {
            start_eip,
            // `len`/`fetch` are placeholders here; the single finalize after the group pre-parse
            // below overwrites them with the real consumed length (prefixes + opcode + operands).
            // In Stage A `fetch` is unused at replay time — decode charges fetch clocks via its
            // real reads; the cache stage (Stage B) is where `fetch` is consumed.
            len: 0,
            fetch: FetchCost::Uniform {
                len: 0,
                wait_states: 0,
            },
            prefixes,
            opcode,
            operand_size,
            address_size,
            modrm: None,
            operand: None,
            imm: 0,
            imm2: 0,
        };

        // Pre-parse the operands of converted groups. The group classifier is the single routing
        // authority; `execute_decoded` matches the same `DecodeGroup` to decide how to execute.
        match Self::route_group(insn.opcode, prefixes) {
            DecodeGroup::Alu => {
                // ALU block. Forms 0-3 carry a ModRM: parse it + its addressing-mode descriptor now
                // (the descriptor reads instruction bytes only, so it stays cacheable). Forms 4/5
                // carry an accumulator immediate: fetch it here (charging its fetch clocks once) so
                // the executor consumes `imm` without re-reading. `op = (opcode>>3)&7`, `form = &7`.
                let form = opcode & 0x07;
                if form < 4 {
                    let modrm = self.fetch_modrm(bus)?;
                    let operand = self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                    insn.modrm = Some(modrm);
                    insn.operand = Some(operand);
                } else if form == 4 {
                    insn.imm = u32::from(self.fetch_u8(bus)?);
                } else {
                    insn.imm = self.fetch_immediate(bus, operand_size)?;
                }
            }
            DecodeGroup::DataMove => {
                // Data-movement block. The arms split by how the operand is encoded; the byte
                // budget each consumes here is what the executor must NOT re-fetch. The 0F
                // MOVZX/MOVSX forms (0x0Fb6/b7/be/bf) carry a plain ModRM, like the single-byte
                // ModRM forms below.
                match opcode {
                    // ModRM r/m forms: MOV r/m<->reg/Sreg, LEA, XCHG r/m (single byte) and
                    // MOVZX/MOVSX (two byte). Parse the ModRM + its addressing-mode descriptor
                    // (instruction bytes only, so it stays cacheable).
                    0x86..=0x8e | 0x0fb6 | 0x0fb7 | 0x0fbe | 0x0fbf => {
                        let modrm = self.fetch_modrm(bus)?;
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.modrm = Some(modrm);
                        insn.operand = Some(operand);
                    }
                    // MOV r/m,imm (group 11). The displacement (if any) precedes the immediate in
                    // the encoding, so parse the operand first, then fetch the immediate. Only
                    // reg=000 is a defined encoding; for any other reg field the fused handler
                    // faults *before* decoding the operand or immediate, so do the same here and
                    // leave `operand`/`imm` unparsed (the executor re-detects the bad reg and
                    // raises the identical group-opcode error with the same bytes consumed).
                    0xc6 => {
                        let modrm = self.fetch_modrm(bus)?;
                        insn.modrm = Some(modrm);
                        if modrm.reg == 0 {
                            let operand =
                                self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                            insn.operand = Some(operand);
                            insn.imm = u32::from(self.fetch_u8(bus)?);
                        }
                    }
                    0xc7 => {
                        let modrm = self.fetch_modrm(bus)?;
                        insn.modrm = Some(modrm);
                        if modrm.reg == 0 {
                            let operand =
                                self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                            insn.operand = Some(operand);
                            insn.imm = self.fetch_immediate(bus, operand_size)?;
                        }
                    }
                    // MOV (E)AX<->moffs: a direct displacement (address-size wide), no ModRM.
                    0xa0..=0xa3 => {
                        insn.imm = self.fetch_moffs(bus, address_size)?;
                    }
                    // MOV r8,imm8.
                    0xb0..=0xb7 => {
                        insn.imm = u32::from(self.fetch_u8(bus)?);
                    }
                    // MOV r16/32,imm16/32.
                    0xb8..=0xbf => {
                        insn.imm = self.fetch_immediate(bus, operand_size)?;
                    }
                    // XCHG reg,(E)AX (0x90-0x97): no operand bytes; 0x90 is NOP (XCHG AX,AX).
                    _ => {}
                }
            }
            DecodeGroup::Stack => {
                // Stack block. Most opcodes carry no extra encoded bytes; only four sub-cases
                // fetch operand bytes here (all others are either register-encoded or implied).
                match insn.opcode as u8 {
                    // 0x68 PUSH imm16/32: fetch the full-width immediate; executor pushes it.
                    0x68 => {
                        insn.imm = self.fetch_immediate(bus, operand_size)?;
                    }
                    // 0x6a PUSH imm8: fetch one byte; executor sign-extends to operand width.
                    0x6a => {
                        insn.imm = u32::from(self.fetch_u8(bus)?);
                    }
                    // 0x8f POP r/m (group 1A): fetch ModRM + addressing descriptor. For
                    // reg!=0 (undefined encoding) leave `operand` as None so the executor can
                    // re-detect the bad reg field and raise the identical error with the same
                    // bytes consumed (mirrors the group-11 approach in DataMove).
                    0x8f => {
                        let modrm = self.fetch_modrm(bus)?;
                        insn.modrm = Some(modrm);
                        if modrm.reg == 0 {
                            let operand =
                                self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                            insn.operand = Some(operand);
                        }
                    }
                    // 0xc8 ENTER imm16, imm8: frame size into `imm`, nesting level into `imm2`
                    // (masked to 5 bits here so the executor doesn't have to repeat it).
                    0xc8 => {
                        insn.imm = u32::from(self.fetch_u16(bus)?);
                        insn.imm2 = u32::from(self.fetch_u8(bus)? & 0x1f);
                    }
                    // All other stack opcodes (PUSH/POP reg, PUSH/POP seg, PUSHA/POPA,
                    // PUSHF/POPF, LEAVE) carry no extra encoded bytes.
                    _ => {}
                }
            }
            DecodeGroup::Group => {
                // Arithmetic /ext groups 1-4. Every opcode here is a ModRM whose `reg` field is
                // the sub-op selector; parse the ModRM + addressing descriptor (instruction bytes
                // only, so it stays cacheable) for all of them. Then fetch the immediate ONLY for
                // the opcodes that carry one, mirroring each fused handler's fetch order exactly so
                // the bytes consumed (and thus the fetch clocks charged) are byte-identical:
                //   - group 1 (0x80-0x83): always an immediate. 0x80/0x82 imm8, 0x81 imm16/32,
                //     0x83 a sign-extended imm8 (sign-extend here so the executor takes `imm` as-is,
                //     matching the fused handler which sign-extended at fetch time).
                //   - group 2 count-by-imm8 (0xc0/0xc1): always one imm8 count byte. The 1/CL forms
                //     (0xd0-0xd3) and group 4 (0xfe) carry NO immediate.
                //   - group 3 (0xf6/0xf7): an immediate ONLY for the TEST sub-op. The fused
                //     reference implements TEST as `reg == 0` alone (the `reg == 1` alias is NOT a
                //     TEST there — it falls through to UnsupportedGroupOpcode and consumes no
                //     immediate), so we match it exactly: fetch the immediate only for `reg == 0`.
                //     NOT/NEG/MUL/IMUL/DIV/IDIV (and the undefined reg==1) have none, so the byte
                //     budget here depends on `reg`. Getting this conditional wrong mis-charges the
                //     fetch and diverges from the fused path.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                insn.modrm = Some(modrm);
                insn.operand = Some(operand);
                match opcode {
                    0x80 | 0x82 => insn.imm = u32::from(self.fetch_u8(bus)?),
                    0x81 => insn.imm = self.fetch_immediate(bus, operand_size)?,
                    0x83 => insn.imm = sign_extend_u8(self.fetch_u8(bus)?),
                    0xc0 | 0xc1 => insn.imm = u32::from(self.fetch_u8(bus)?),
                    0xf6 if modrm.reg == 0 => insn.imm = u32::from(self.fetch_u8(bus)?),
                    0xf7 if modrm.reg == 0 => insn.imm = self.fetch_immediate(bus, operand_size)?,
                    // 0xd0-0xd3 (count 1/CL), 0xfe (INC/DEC), and 0xf6/0xf7 with reg!=0 carry no
                    // immediate after the ModRM.
                    _ => {}
                }
            }
            DecodeGroup::Branch => {
                // Relative-displacement + loop control flow. Every opcode here carries a relative
                // displacement and nothing else; fetch it now (charging its fetch clocks once) and
                // store it sign-extended to i32 in `insn.imm`. The executor replays the SAME
                // `relative_jump(disp, operand_size)` math the fused path used, so the byte width of
                // the sign-extension is what matters and is matched per-opcode here:
                //   - rel8 (Jcc short 0x70-0x7f, the loop/JCXZ branches 0xe0-0xe3, JMP short 0xeb):
                //     one displacement byte, sign-extended.
                //   - rel16/32 (CALL near 0xe8, JMP near 0xe9, two-byte Jcc near 0x0F80-0x0F8F):
                //     operand-size-wide displacement, sign-extended (matching `fetch_relative`).
                // Storing the displacement (not the target) keeps the eip-relative computation in
                // the executor, where eip is already at the instruction end.
                match insn.opcode {
                    0x70..=0x7f | 0xe0..=0xe3 | 0xeb => {
                        insn.imm = self.fetch_i8(bus)? as i32 as u32;
                    }
                    // 0xe8/0xe9 (single byte) and 0x0F80-0x0F8F (two byte) take an operand-size-wide
                    // relative displacement.
                    _ => {
                        insn.imm = self.fetch_relative(bus, operand_size)? as u32;
                    }
                }
            }
            DecodeGroup::ControlFlow => {
                // Far/indirect/RET/INT control flow + 0xff group 5. Each form reads exactly the bytes
                // its fused handler read, in the same order, so the fetch clocks are byte-identical:
                match insn.opcode as u8 {
                    // 0x9a CALL far direct / 0xea JMP far direct: a far pointer immediate — the
                    // offset (operand-size wide) THEN the 16-bit selector, exactly as the fused
                    // handler fetched them. Store the offset in `imm` and the selector in `imm2`; the
                    // executor reconstructs the same far target.
                    0x9a | 0xea => {
                        insn.imm = match operand_size {
                            OperandSize::Word => u32::from(self.fetch_u16(bus)?),
                            OperandSize::Dword => self.fetch_u32(bus)?,
                        };
                        insn.imm2 = u32::from(self.fetch_u16(bus)?);
                    }
                    // 0xc2 RET near imm16 / 0xca RETF imm16: the 16-bit stack-release count is part
                    // of the instruction stream and is fetched BEFORE the executor pops, so read it
                    // here. (The operand size only selects the pop width, not the release width.)
                    0xc2 | 0xca => {
                        insn.imm = u32::from(self.fetch_u16(bus)?);
                    }
                    // 0xcd INT n: the imm8 vector. Read it here; the executor reuses it. (The V86
                    // IOPL check is part of execution, not decode, so it stays in the executor.)
                    0xcd => {
                        insn.imm = u32::from(self.fetch_u8(bus)?);
                    }
                    // 0xff group 5: parse the ModRM + addressing descriptor (instruction bytes only,
                    // so it stays cacheable). The /ext is `modrm.reg`. The indirect CALL/JMP read
                    // their target FROM MEMORY at execute time (resolved against live registers), so
                    // decode captures ONLY the descriptor here — never the target.
                    0xff => {
                        let modrm = self.fetch_modrm(bus)?;
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.modrm = Some(modrm);
                        insn.operand = Some(operand);
                    }
                    // 0xc3 RET near, 0xcb RETF, 0xcc INT3, 0xce INTO, 0xcf IRET: no encoded operand.
                    _ => {}
                }
            }
            DecodeGroup::FlagsMisc => {
                // Flags + misc register block. Only TEST r/m,reg (0x84/0x85) carries a ModRM; parse
                // it + the addressing-mode descriptor here (instruction bytes only, stays cacheable).
                // Every other A7 opcode carries no encoded operand after the opcode byte — the
                // register/flag operands are implicit (reg field encoded in the opcode or implied).
                match opcode {
                    0x84 | 0x85 => {
                        let modrm = self.fetch_modrm(bus)?;
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.modrm = Some(modrm);
                        insn.operand = Some(operand);
                    }
                    // INC/DEC reg (0x40-0x4f), CBW/CWDE (0x98), CWD/CDQ (0x99), SAHF/LAHF
                    // (0x9e/0x9f), CMC/CLC/STC/CLI/STI/CLD/STD (0xf5/0xf8-0xfd): no operand bytes.
                    _ => {}
                }
            }
            DecodeGroup::StringOps => {
                // String operations (MOVS/CMPS/STOS/LODS/SCAS). No ModRM, no immediate: the operands
                // are all implicit (DS:SI source, ES:DI destination, the accumulator), so there is
                // nothing to pre-parse here. The REP/REPNE prefix and any segment override were
                // already read into `insn.prefixes` by `read_prefixes` at the top of `decode`; the
                // executor passes them straight through to `run_string`. The element width is derived
                // from the opcode's low bit (byte vs operand-size) in the executor, not the stream.
            }
            // Both fallback groups pre-parse nothing in `decode` (the second 0F byte was already
            // folded into `insn.opcode` above): their executors re-read any ModRM/immediate from the
            // post-opcode eip in the shared fused dispatch.
            DecodeGroup::Fallback | DecodeGroup::TwoByteFallback => {}
        }

        // Finalize `len`/`fetch` once, after every group's pre-parse, so a converted group never
        // has to re-write them: a group's match arm only fetches its operand bytes; this single
        // assignment captures the total bytes `decode` consumed (prefixes + opcode + operands).
        let consumed = self.registers.eip.wrapping_sub(start_eip) as u8;
        insn.len = consumed;
        insn.fetch = FetchCost::Uniform {
            len: consumed,
            wait_states: 0,
        };

        Ok(insn)
    }

    /// Stage A executor. For the opcodes converted to the split (the whole ALU block), execute from
    /// the pre-decoded `operand`/`modrm`/`imm` (resolving the addressing-mode descriptor against the
    /// live registers). Every other opcode continues into the shared fused dispatch (which re-reads
    /// its ModRM/immediates from the post-opcode eip) so behavior is byte-for-byte unchanged.
    fn execute_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        // Route on the same group classifier `decode` used, so the parse side and the execute
        // side can never drift out of sync.
        match Self::route_group(insn.opcode, insn.prefixes) {
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
            DecodeGroup::ControlFlow => self.execute_control_flow_decoded(insn, bus),
            // The flags + misc register block (TEST r/m,reg, INC/DEC reg, CBW/CWD, SAHF/LAHF, and
            // the single flag-bit ops) runs through its split executor, consuming the pre-parsed
            // ModRM/operand for TEST and running the same flag/register logic as the fused path.
            DecodeGroup::FlagsMisc => self.execute_flags_misc_decoded(insn, bus),
            // The string-operation block (MOVS/CMPS/STOS/LODS/SCAS, byte and word/dword) runs through
            // its split executor, a thin call to the existing `run_string` helper with the pre-decoded
            // `insn.prefixes` (REP/REPNE + segment override) passed through — the REP loop, ZF
            // termination, DF direction, and per-iteration clocks all stay in `run_string` unchanged.
            DecodeGroup::StringOps => self.execute_string_decoded(insn, bus),
            DecodeGroup::TwoByteFallback => {
                // Un-converted two-byte (0F) opcode. `decode` already read + charged the second
                // byte and applied the ISA gate, folding it into `insn.opcode` as 0x0F00 | second.
                // Hand the second byte to `execute_two_byte`, which re-reads only this opcode's
                // ModRM/immediate from the post-second-byte eip (the second byte is never re-read).
                self.execute_two_byte(
                    bus,
                    insn.opcode as u8,
                    insn.prefixes,
                    insn.address_size,
                    insn.operand_size,
                )
            }
            DecodeGroup::Fallback => {
                // Fallback: `decode` already read the prefixes + opcode and ran the LOCK check,
                // leaving eip just past the opcode. Continue into the shared dispatch from there,
                // so each arm re-reads its ModRM/immediates exactly as before and the prefix/opcode
                // fetch clocks are charged only once (rule 2).
                self.dispatch_opcode(
                    bus,
                    insn.start_eip,
                    insn.prefixes,
                    insn.opcode as u8,
                    insn.operand_size,
                    insn.address_size,
                )
            }
        }
    }

    /// Resolve a pre-decoded ModRM r/m form into its `(ModRm, RmOperand)`: the ModRM (for its `reg`
    /// field) plus the r/m operand resolved against the live registers — a register operand as-is, a
    /// memory descriptor with its effective address recomputed now (`resolve_addr_mode` reads only
    /// base/index registers, no instruction bytes). Centralizes the `decode`-populated `.expect`s so
    /// each group executor doesn't repeat them.
    ///
    /// Shared by every group whose decode arm pre-parses a ModRM (ALU, data-move, and the stack /
    /// group1-5 / bit / system / FPU groups to come): the panic location already names the calling
    /// executor, so the messages stay group-agnostic. Calling this when decode did NOT populate
    /// `modrm`/`operand` (i.e. a non-ModRM form) is a routing bug and panics by design.
    fn resolve_decoded_modrm_operand(&self, insn: &DecodedInsn) -> (ModRm, RmOperand) {
        let modrm = insn.modrm.expect("ModRM r/m form decoded with a ModRM");
        let operand = match insn
            .operand
            .expect("ModRM r/m form decoded with an operand")
        {
            DecodedOperand::Reg(index) => RmOperand::Register(index),
            DecodedOperand::Mem(addr) => self.resolve_addr_mode(&addr),
        };
        (modrm, operand)
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

        Ok(clocks(2))
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
                return Ok(clocks(3));
            }
            0x0fb7 => {
                // MOVZX r, r/m16: zero-extend the word into the destination.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = self.read_operand_sized(bus, operand, OperandSize::Word)?;
                self.write_gpr_sized(modrm.reg, operand_size, value);
                return Ok(clocks(3));
            }
            0x0fbe => {
                // MOVSX r, r/m8: sign-extend the byte into the destination.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = sign_extend_u8(self.read_operand_u8(bus, operand)?);
                self.write_gpr_sized(modrm.reg, operand_size, value);
                return Ok(clocks(3));
            }
            0x0fbf => {
                // MOVSX r, r/m16: sign-extend the word into the destination.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value =
                    self.read_operand_sized(bus, operand, OperandSize::Word)? as i16 as i32 as u32;
                self.write_gpr_sized(modrm.reg, operand_size, value);
                return Ok(clocks(3));
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
                Ok(clocks(3))
            }
            0x87 => {
                // XCHG r/m16/32, r16/32. Cross-write.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let rm = self.read_operand_sized(bus, operand, operand_size)?;
                let reg = self.read_gpr_sized(modrm.reg, operand_size);
                self.write_operand_sized(bus, operand, operand_size, reg)?;
                self.write_gpr_sized(modrm.reg, operand_size, rm);
                Ok(clocks(3))
            }
            0x88 => {
                // MOV r/m8, r8.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = self.read_gpr8(modrm.reg);
                self.write_operand_u8(bus, operand, value)?;
                Ok(clocks(2))
            }
            0x89 => {
                // MOV r/m16/32, r16/32.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = self.read_gpr_sized(modrm.reg, operand_size);
                self.write_operand_sized(bus, operand, operand_size, value)?;
                Ok(clocks(2))
            }
            0x8a => {
                // MOV r8, r/m8.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = self.read_operand_u8(bus, operand)?;
                self.write_gpr8(modrm.reg, value);
                Ok(clocks(2))
            }
            0x8b => {
                // MOV r16/32, r/m16/32.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = self.read_operand_sized(bus, operand, operand_size)?;
                self.write_gpr_sized(modrm.reg, operand_size, value);
                Ok(clocks(2))
            }
            0x8c => {
                // MOV r/m16, Sreg. Always a word store regardless of operand size.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = u32::from(self.segment_from_reg_field(modrm.reg).selector);
                self.write_operand_sized(bus, operand, OperandSize::Word, value)?;
                Ok(clocks(2))
            }
            0x8d => {
                // LEA reg, m: load the effective address, not the memory it points at. mod=3 (a
                // register r/m) is an invalid encoding and faults #UD.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                match operand {
                    RmOperand::Memory(mem) => {
                        self.write_gpr_sized(modrm.reg, operand_size, mem.offset);
                        Ok(clocks(2))
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
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = self.read_operand_sized(bus, operand, OperandSize::Word)?;
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
            0x90 => {
                // NOP (XCHG (E)AX, (E)AX): a no-op with the same clocks as the other XCHG-acc forms.
                Ok(clocks(3))
            }
            0x91..=0x97 => {
                // XCHG (E)AX, reg. The register index is the low 3 opcode bits.
                let reg = opcode & 7;
                let acc = self.read_gpr_sized(0, operand_size);
                let other = self.read_gpr_sized(reg, operand_size);
                self.write_gpr_sized(0, operand_size, other);
                self.write_gpr_sized(reg, operand_size, acc);
                Ok(clocks(3))
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
                Ok(clocks(4))
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
                Ok(clocks(4))
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
                Ok(clocks(4))
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
                Ok(clocks(4))
            }
            0xb0..=0xb7 => {
                // MOV r8, imm8. The immediate was captured into `imm` by decode.
                self.write_gpr8(opcode - 0xb0, insn.imm as u8);
                Ok(clocks(2))
            }
            0xb8..=0xbf => {
                // MOV r16/32, imm16/32.
                self.write_gpr_sized(opcode - 0xb8, operand_size, insn.imm);
                Ok(clocks(2))
            }
            0xc6 => {
                // MOV r/m8, imm8 (group 11). Only reg=000 is defined; decode left `operand`/`imm`
                // unparsed for any other reg field, so re-raise the identical group-opcode error.
                let modrm = insn.modrm.expect("group-11 form decoded with a ModRM");
                if modrm.reg != 0 {
                    return Err(CpuError::UnsupportedGroupOpcode {
                        opcode,
                        extension: modrm.reg,
                    }
                    .into());
                }
                let (_, operand) = self.resolve_decoded_modrm_operand(insn);
                self.write_operand_u8(bus, operand, insn.imm as u8)?;
                Ok(clocks(2))
            }
            0xc7 => {
                // MOV r/m16/32, imm16/32 (group 11). Same reg=000 gate as 0xc6.
                let modrm = insn.modrm.expect("group-11 form decoded with a ModRM");
                if modrm.reg != 0 {
                    return Err(CpuError::UnsupportedGroupOpcode {
                        opcode,
                        extension: modrm.reg,
                    }
                    .into());
                }
                let (_, operand) = self.resolve_decoded_modrm_operand(insn);
                self.write_operand_sized(bus, operand, operand_size, insn.imm)?;
                Ok(clocks(2))
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
                // PUSH imm16/32: `decode` fetched the full-width immediate into `insn.imm`.
                self.push(bus, insn.imm, operand_size)?;
                Ok(clocks(2))
            }
            0x6a => {
                // PUSH imm8: sign-extend the byte (stored in `insn.imm`) to the operand size.
                let value = sign_extend_u8(insn.imm as u8);
                self.push(bus, value, operand_size)?;
                Ok(clocks(2))
            }
            0x8f => {
                // POP r/m16/32 (group 1A). Only reg=000 is defined; other reg values are an
                // illegal encoding. `decode` left `operand` as None for any reg != 0, so
                // re-raise the identical error with the same bytes consumed.
                let modrm = insn.modrm.expect("POP r/m decoded with a ModRM");
                if modrm.reg != 0 {
                    return Err(CpuError::UnsupportedGroupOpcode {
                        opcode,
                        extension: modrm.reg,
                    }
                    .into());
                }
                let (_, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = self.pop(bus, operand_size)?;
                self.write_operand_sized(bus, operand, operand_size, value)?;
                Ok(clocks(5))
            }
            0x9c => {
                // PUSHF / PUSHFD. The low 16 flag bits push the same in both forms. The
                // dword form additionally carries the 486 AC and ID bits (RF and VM are
                // masked to 0 in the pushed image). operand_size drives whether push writes
                // 2 or 4 bytes.
                self.check_v86_iopl()?;
                let value = match operand_size {
                    OperandSize::Word => self.registers.eflags & 0xffff,
                    OperandSize::Dword => self.registers.eflags & (0xffff | FLAG_AC | FLAG_ID),
                };
                self.push(bus, value, operand_size)?;
                Ok(clocks(3))
            }
            0x9d => {
                // POPF / POPFD: load the popped image through the shared flag-load.
                self.check_v86_iopl()?;
                let value = self.pop(bus, operand_size)?;
                self.load_flags(value, operand_size);
                Ok(clocks(4))
            }
            0xc8 => {
                // ENTER imm16, imm8: build a stack frame. NestingLevel (already masked to 5
                // bits by `decode`) is taken from `insn.imm2`; frame size from `insn.imm`.
                let alloc = insn.imm as u16;
                let level = insn.imm2; // already & 0x1f from decode
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
                // LEAVE: (E)SP <- (E)BP, then (E)BP <- pop. The stack-pointer move follows
                // the stack width: a 16-bit real-mode stack moves only SP and keeps
                // ESP[31:16]; a 32-bit stack moves the full ESP. The operand size still
                // selects BP vs EBP for the popped frame pointer.
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
                Ok(clocks(2))
            }
            0x81 => {
                // Group 1 ALU r/m16/32, imm16/32. Full-width immediate from `decode`.
                let imm = insn.imm;
                let a = self.read_operand_sized(bus, operand, operand_size)?;
                let result = self.alu(modrm.reg, a, imm, operand_size.bus_width());
                if modrm.reg != 7 {
                    self.write_operand_sized(bus, operand, operand_size, result)?;
                }
                Ok(clocks(2))
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
                Ok(clocks(2))
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
                Ok(clocks(2))
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
                        return Err(CpuError::UnsupportedGroupOpcode {
                            opcode: 0xf7,
                            extension: modrm.reg,
                        }
                        .into());
                    }
                }
                Ok(clocks(2))
            }
            0xfe => {
                // Group 4 INC/DEC byte. /0 INC, /1 DEC; any other reg is #UD (the fused reference's
                // UnsupportedGroupOpcode). INC/DEC preserve CF (handled inside `inc_dec`).
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
            return Ok(clocks(3));
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
                Ok(clocks(11))
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
                Ok(clocks(11))
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
                Ok(clocks(9))
            }
            0xe8 => {
                // CALL near, relative. Push the return address (eip, already at the instruction
                // end) before branching — the same order the fused handler used.
                self.push(bus, self.registers.eip, operand_size)?;
                self.relative_jump(rel, operand_size);
                Ok(clocks(7))
            }
            0xe9 => {
                // JMP near, relative.
                self.relative_jump(rel, operand_size);
                Ok(clocks(7))
            }
            0xeb => {
                // JMP short, relative.
                self.relative_jump(rel, operand_size);
                Ok(clocks(7))
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
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = self.read_operand_u8(bus, operand)?;
                let reg = self.read_gpr8(modrm.reg);
                self.alu(4, u32::from(value), u32::from(reg), BusWidth::Byte);
                Ok(clocks(2))
            }
            0x85 => {
                // TEST r/m16/32, reg16/32. AND-for-flags only; no write-back.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = self.read_operand_sized(bus, operand, operand_size)?;
                let reg = self.read_gpr_sized(modrm.reg, operand_size);
                self.alu(4, value, reg, operand_size.bus_width());
                Ok(clocks(2))
            }
            opcode @ 0x40..=0x4f => {
                // INC (0x40-0x47) / DEC (0x48-0x4f) register. CF is preserved by `inc_dec`.
                let index = opcode & 0x07;
                let is_dec = opcode >= 0x48;
                let value = self.read_gpr_sized(index, operand_size);
                let result = self.inc_dec(value, is_dec, operand_size.bus_width());
                self.write_gpr_sized(index, operand_size, result);
                Ok(clocks(2))
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
            0x9e => {
                // SAHF: load CF/PF/AF/ZF/SF from AH; OF and the reserved bits are untouched.
                // The trailing | 0x02 keeps the always-one reserved bit set.
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
            0xf5 => {
                // CMC: complement the carry flag.
                self.set_flag(FLAG_CF, !self.flag(FLAG_CF));
                Ok(clocks(2))
            }
            0xf8 => {
                // CLC: clear the carry flag.
                self.set_flag(FLAG_CF, false);
                Ok(clocks(2))
            }
            0xf9 => {
                // STC: set the carry flag.
                self.set_flag(FLAG_CF, true);
                Ok(clocks(2))
            }
            0xfa => {
                // CLI. IOPL-sensitive: faults to the monitor in a V86 task below IOPL 3.
                self.check_v86_iopl()?;
                self.set_flag(FLAG_IF, false);
                Ok(clocks(3))
            }
            0xfb => {
                // STI sets IF and arms the one-instruction shadow so the instruction immediately
                // after STI always executes before any interrupt is taken. The shadow is set here
                // in the executor exactly as the fused handler did.
                self.check_v86_iopl()?;
                self.set_flag(FLAG_IF, true);
                self.interrupt_shadow = true;
                Ok(clocks(3))
            }
            0xfc => {
                // CLD: clear the direction flag.
                self.set_flag(FLAG_DF, false);
                Ok(clocks(2))
            }
            0xfd => {
                // STD: set the direction flag.
                self.set_flag(FLAG_DF, true);
                Ok(clocks(2))
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
        self.run_string(bus, op, width, prefixes, address_size)?;
        Ok(clocks(4))
    }

    /// The far/indirect/RET/INT control-flow block + 0xff group 5 through the decode/execute split.
    /// Each arm mirrors the former fused handler verbatim — same far-pointer reconstruction, same
    /// ret/retf and interrupt/IRET delivery, same FF sub-op dispatch off `modrm.reg`, same clocks —
    /// but consumes what `decode` pre-parsed (the far-pointer offset/selector in `imm`/`imm2`, the
    /// imm16 release in `imm`, the imm8 vector in `imm`, or the ModRM/descriptor) so the executor
    /// re-fetches no instruction byte. The protected-mode descriptor loads, gates, faults, the
    /// V86 IOPL check, the interrupt-shadow/IF semantics, and the FF indirect target read all stay in
    /// the unchanged helpers, so behavior is byte-for-byte identical to the fused path.
    fn execute_control_flow_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        let operand_size = insn.operand_size;
        let address_size = insn.address_size;

        match insn.opcode as u8 {
            0x9a => {
                // CALL far direct. `decode` fetched the far pointer (offset into `imm`, selector into
                // `imm2`); reconstruct it and deliver through the unchanged far-call helper.
                let offset = insn.imm;
                let selector = insn.imm2 as u16;
                self.far_call(bus, selector, offset, operand_size)?;
                Ok(clocks(17))
            }
            0xea => {
                // JMP far direct. Same far-pointer reconstruction, via the far-jump helper.
                let offset = insn.imm;
                let selector = insn.imm2 as u16;
                self.far_jump(bus, selector, offset, operand_size)?;
                Ok(clocks(17))
            }
            0xc2 => {
                // RET near, release imm16 bytes of arguments. `decode` fetched the release count into
                // `imm`; pop the return offset (operand-size wide) THEN release, the same order the
                // fused handler used.
                let release = insn.imm as u16;
                let target = self.pop(bus, operand_size)?;
                self.set_eip(target & operand_size.mask());
                self.release_stack(release);
                Ok(clocks(10))
            }
            0xc3 => {
                let target = self.pop(bus, operand_size)?;
                self.set_eip(target & operand_size.mask());
                Ok(clocks(10))
            }
            0xca => {
                // RETF, release imm16 bytes. `decode` fetched the count into `imm`; pop CS:IP via the
                // far-return helper THEN release.
                let release = insn.imm as u16;
                self.return_far(bus, operand_size)?;
                self.release_stack(release);
                Ok(clocks(17))
            }
            0xcb => {
                self.return_far(bus, operand_size)?;
                Ok(clocks(17))
            }
            0xcc => {
                // INT 3: one-byte breakpoint trap to vector 3, via the shared delivery path.
                self.software_interrupt(bus, 3)?;
                Ok(clocks(33))
            }
            0xcd => {
                // INT n. IOPL-sensitive in V86 (checked here, exactly as the fused handler did,
                // before the delivery). `decode` fetched the vector into `imm`.
                self.check_v86_iopl()?;
                let vector = insn.imm as u8;
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
                        Ok(clocks(2))
                    }
                    2 => {
                        let target = self.read_operand_sized(bus, operand, operand_size)?;
                        self.push(bus, self.registers.eip, operand_size)?;
                        self.set_eip(target & operand_size.mask());
                        Ok(clocks(7))
                    }
                    4 => {
                        let target = self.read_operand_sized(bus, operand, operand_size)?;
                        self.set_eip(target & operand_size.mask());
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
                        // The selector follows the offset in memory. Its address is computed in the
                        // address-size space, so on a 16-bit real-mode segment it wraps at 0xffff
                        // (offset 0xfffe puts the selector at 0x0000, not past the limit), matching
                        // the 80386.
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
            opcode => unreachable!("control-flow opcode {opcode:#x}"),
        }
    }

    // Transitional fused entry: reads the prefixes + opcode + LOCK check itself, then dispatches.
    // The production `cycle` path goes through `decode`/`execute_decoded` (which call
    // `dispatch_opcode` directly to keep the fetch clocks charged once), so this whole-instruction
    // entry now has callers only in the test suite; it is retained as the documented fused
    // reference and is removed when the seam covers every opcode.
    #[allow(dead_code)]
    fn execute_instruction_legacy<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<CycleOutcome> {
        let instruction_eip = self.registers.eip;
        let prefixes = self.read_prefixes(bus)?;
        let opcode = self.fetch_u8(bus)?;
        if prefixes.lock {
            self.check_lock_target(bus, opcode)?;
        }
        let operand_size = self.operand_size(prefixes);
        let address_size = self.address_size(prefixes);
        self.dispatch_opcode(
            bus,
            instruction_eip,
            prefixes,
            opcode,
            operand_size,
            address_size,
        )
    }

    /// The live opcode dispatch the production seam calls on every fallback opcode (and that the
    /// test-only `execute_instruction_legacy` reference also delegates to). The caller is
    /// responsible for having already read the prefixes + opcode and run any LOCK check;
    /// `instruction_eip` is only used for the unsupported-opcode error, and the current eip must
    /// point at the byte immediately after the opcode so each arm re-reads its ModRM/immediate
    /// from there. The seam calls this with the values `decode` already read, so the prefix/opcode
    /// instruction-fetch clocks are charged exactly once.
    #[allow(clippy::too_many_arguments)]
    fn dispatch_opcode<B: CpuBus>(
        &mut self,
        bus: &mut B,
        instruction_eip: u32,
        prefixes: Prefixes,
        opcode: u8,
        operand_size: OperandSize,
        address_size: AddressSize,
    ) -> ExecResult<CycleOutcome> {
        match opcode {
            // 0x06/0x07/0x0e/0x16/0x17/0x1e/0x1f (PUSH/POP seg) are converted to the
            // decode/execute split (`DecodeGroup::Stack` -> `execute_stack_decoded`);
            // not handled here.
            0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 | 0x67 => {
                Err(CpuError::UnsupportedOpcode {
                    opcode,
                    cs: self.registers.cs().selector,
                    eip: instruction_eip,
                }
                .into())
            }
            // 0x50-0x57 (PUSH reg), 0x58-0x5f (POP reg), 0x60 (PUSHA/PUSHAD), 0x61 (POPA/POPAD),
            // 0x68 (PUSH imm16/32) are converted to the decode/execute split
            // (`DecodeGroup::Stack` -> `execute_stack_decoded`); not handled here.
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
            // 0x6a (PUSH imm8) is converted to the decode/execute split
            // (`DecodeGroup::Stack` -> `execute_stack_decoded`); not handled here.
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
            // 0x70-0x7f (Jcc short, rel8) are converted to the decode/execute split:
            // `route_group` classifies them as `DecodeGroup::Branch` and `execute_branch_decoded`
            // runs them. Not handled here.
            // 0x80/0x81/0x82/0x83 (group 1 ALU r/m,imm; 0x82 is the undocumented 0x80 alias) are
            // converted to the decode/execute split: `route_group` classifies them as
            // `DecodeGroup::Group` and `execute_group_decoded` runs them. Not handled here.
            // 0x84/0x85 (TEST r/m,reg byte/word) are converted to the decode/execute split:
            // `route_group` classifies them as `DecodeGroup::FlagsMisc` and
            // `execute_flags_misc_decoded` runs them. Not handled here.
            // 0x86-0x8e (XCHG r/m,reg; MOV r/m<->reg/Sreg; LEA) are converted to the
            // decode/execute split: `route_group` classifies them as `DecodeGroup::DataMove` and
            // `execute_datamove_decoded` runs them. They must NOT be re-handled here (single path).
            // 0x8f (POP r/m, group 1A) is converted to `DecodeGroup::Stack` ->
            // `execute_stack_decoded`; not handled here.
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
            // 0x90 (NOP / XCHG AX,AX) and 0x91-0x97 (XCHG (E)AX, reg) are converted to the split
            // (`DecodeGroup::DataMove` -> `execute_datamove_decoded`); not handled here.
            // 0x98 (CBW/CWDE), 0x99 (CWD/CDQ), 0x9e (SAHF), 0x9f (LAHF) are converted to the
            // decode/execute split: `route_group` classifies them as `DecodeGroup::FlagsMisc` and
            // `execute_flags_misc_decoded` runs them. Not handled here.
            // 0x9c (PUSHF/PUSHFD) and 0x9d (POPF/POPFD) are converted to the decode/execute
            // split (`DecodeGroup::Stack` -> `execute_stack_decoded`); not handled here.
            // 0xa0-0xa3 (MOV (E)AX<->moffs, byte and word) are converted to the split
            // (`DecodeGroup::DataMove` -> `execute_datamove_decoded`); not handled here.
            // 0xa4/0xa5 (MOVS), 0xa6/0xa7 (CMPS), 0xae/0xaf (SCAS) are converted to the
            // decode/execute split: `route_group` classifies them as `DecodeGroup::StringOps` and
            // `execute_string_decoded` runs them (a thin call to the unchanged `run_string` helper
            // with the prefixes passed through, so the REP loop/DF/segment-override/per-iteration
            // clocks stay in one place). Not handled here. 0xa8/0xa9 below are TEST AL/AX,imm — not
            // string ops — and remain on the fused path.
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
            // 0xaa/0xab (STOS) and 0xac/0xad (LODS) are converted to the decode/execute split:
            // `route_group` classifies them as `DecodeGroup::StringOps` and `execute_string_decoded`
            // runs them via the unchanged `run_string` helper. Not handled here.
            // 0xb0-0xb7 (MOV r8,imm8) and 0xb8-0xbf (MOV r16/32,imm) are converted to the split
            // (`DecodeGroup::DataMove` -> `execute_datamove_decoded`); not handled here.
            // 0xc2/0xc3 (RET near, with and without an imm16 release) and 0xca/0xcb (RETF, ditto) are
            // converted to the decode/execute split: `route_group` classifies them as
            // `DecodeGroup::ControlFlow` and `execute_control_flow_decoded` runs them (the imm16
            // release count is parsed in `decode`). Not handled here.
            // 0xc6/0xc7 (MOV r/m,imm group 11) are converted to the split
            // (`DecodeGroup::DataMove` -> `execute_datamove_decoded`); not handled here.
            // 0xc8 (ENTER) and 0xc9 (LEAVE) are converted to `DecodeGroup::Stack` ->
            // `execute_stack_decoded`; not handled here.
            // 0xcc (INT3), 0xcd (INT n, imm8 vector), 0xce (INTO), 0xcf (IRET) are converted to the
            // split: `route_group` classifies them as `DecodeGroup::ControlFlow` and
            // `execute_control_flow_decoded` runs them (the imm8 vector for 0xcd is parsed in
            // `decode`; the V86 IOPL check and the interrupt/IRET delivery stay in the executor's
            // shared helpers). Not handled here.
            0xd6 => {
                // SALC/SETALC (undocumented): AL = CF ? 0xFF : 0x00. Flags unaffected.
                let value = if self.flag(FLAG_CF) { 0xff } else { 0x00 };
                self.write_gpr8(0, value);
                Ok(clocks(2))
            }
            0xd8..=0xdf => self.execute_fpu(bus, opcode, prefixes, address_size),
            // 0xe0/0xe1 (LOOPNE/LOOPE), 0xe2 (LOOP), 0xe3 (JCXZ/JECXZ) are converted to the
            // decode/execute split: `route_group` classifies them as `DecodeGroup::Branch` and
            // `execute_branch_decoded` runs them. Not handled here.
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
            // 0xe8 (CALL near, rel16/32), 0xe9 (JMP near, rel16/32), 0xeb (JMP short, rel8) are
            // converted to the decode/execute split: `route_group` classifies them as
            // `DecodeGroup::Branch` and `execute_branch_decoded` runs them. Not handled here.
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
            // 0x9a (CALL far direct) and 0xea (JMP far direct) are converted to the decode/execute
            // split: `route_group` classifies them as `DecodeGroup::ControlFlow` and
            // `execute_control_flow_decoded` runs them (the far pointer — offset then selector — is
            // parsed in `decode` into `imm`/`imm2`). Not handled here.
            0xf4 => {
                self.halted = true;
                Ok(CycleOutcome {
                    core_clocks: 5,
                    halted: true,
                })
            }
            // 0xf6/0xf7 (group 3: TEST/NOT/NEG/MUL/IMUL/DIV/IDIV) are converted to the
            // decode/execute split: `route_group` classifies them as `DecodeGroup::Group` and
            // `execute_group_decoded` runs them (the conditional TEST immediate is parsed in
            // `decode`). Not handled here.
            // 0xf5 (CMC), 0xf8 (CLC), 0xf9 (STC), 0xfa (CLI), 0xfb (STI), 0xfc (CLD), 0xfd (STD)
            // are converted to the decode/execute split: `route_group` classifies them as
            // `DecodeGroup::FlagsMisc` and `execute_flags_misc_decoded` runs them. Not handled here.
            // 0xc0/0xc1/0xd0-0xd3 (group 2 shift/rotate) and 0xfe (group 4 INC/DEC byte) are
            // converted to the decode/execute split: `route_group` classifies them as
            // `DecodeGroup::Group` and `execute_group_decoded` runs them. Not handled here.
            // 0xff (group 5: INC/DEC r/m, near/far indirect CALL/JMP, PUSH r/m, /7 #UD) is converted
            // to the split: `route_group` classifies it as `DecodeGroup::ControlFlow` and
            // `execute_control_flow_decoded` runs it (the ModRM + addressing descriptor is parsed in
            // `decode`; the indirect target is read from memory in the executor). Not handled here.
            // 0x40-0x4f (INC/DEC reg) are converted to the decode/execute split:
            // `route_group` classifies them as `DecodeGroup::FlagsMisc` and
            // `execute_flags_misc_decoded` runs them. Not handled here.
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
            0x0f => {
                // LEGACY-REFERENCE ONLY. This arm is reached solely via the test-only
                // `execute_instruction_legacy`, which reads a single opcode byte and so arrives here
                // with the bare 0x0F. The production `cycle` path never does: `decode` folds the
                // second byte into `insn.opcode` as 0x0F00 | second and routes it through
                // `DecodeGroup::TwoByteFallback`, which calls `execute_two_byte` directly — so a
                // combined 0F value never re-enters `dispatch_opcode`.
                //
                // DO NOT copy this `fetch_u8` + `check_two_byte_isa_gate` shape into a converted 0F
                // executor. It re-reads and re-charges the second byte (and re-applies the gate) ON
                // PURPOSE, so the fused reference stays self-contained; the production path reads and
                // gates that byte exactly once, in `decode`. A converted 0F group must take the
                // second byte from `insn.opcode` instead (see the extension rules on `DecodeGroup`).
                let second = self.fetch_u8(bus)?;
                self.check_two_byte_isa_gate(second)?;
                self.execute_two_byte(bus, second, prefixes, address_size, operand_size)
            }
            _ => Err(CpuError::UnsupportedOpcode {
                opcode,
                cs: self.registers.cs().selector,
                eip: instruction_eip,
            }
            .into()),
        }
    }

    /// The guest-level ISA #UD gate for the whole 0F-extended group. At the 286 level the core
    /// raises #UD for every 0F opcode the 386 (and later) introduced that it otherwise executes:
    /// MOVZX/MOVSX, BT/BTS/BTR/BTC, BSF/BSR, SHLD/SHRD, SETcc, the 0F-form IMUL and Jcc, MOV
    /// to/from CR, and the 486 additions (INVD/WBINVD, CMPXCHG, XADD, BSWAP). The 286-era 0F
    /// opcodes the core supports (0F 01 LGDT/LIDT) stay allowed. CPUID is gated separately because
    /// it is absent on both the 286 and the 386. The 586-class additions (RDTSC, RDMSR/WRMSR,
    /// CMOVcc, CMPXCHG8B, SYSCALL/SYSRET, RSM) #UD when the guest has throttled below the 586
    /// level. Code fetched from the BIOS ROM is exempt (see `cs_in_firmware_rom`), so the gate only
    /// ever holds guest code that selected a lower GSW mode.
    ///
    /// `decode` applies this once, right after reading the second 0F byte — the same logical point
    /// (and eip) the fused path faulted at — so both the converted split path and the un-converted
    /// fused fallback share a single gate.
    fn check_two_byte_isa_gate(&self, second: u8) -> ExecResult<()> {
        if self.cs_in_firmware_rom() {
            return Ok(());
        }
        if (self.level.is_pre_386() && is_386plus_two_byte(second))
            || (!self.level.has_pentium_isa() && is_586plus_two_byte(second))
        {
            return Err(InternalFault::Exception {
                vector: 6,
                error_code: None,
            });
        }
        Ok(())
    }

    /// Execute an un-converted 0F (two-byte) opcode. `second` is the second opcode byte that
    /// `decode` (or, for the fused reference, the `0x0F` dispatch arm) already read + charged and
    /// gated; this never re-reads it. Converted 0F groups (MOVZX/MOVSX) bypass this entirely via
    /// `route_group`/`execute_decoded`.
    fn execute_two_byte<B: CpuBus>(
        &mut self,
        bus: &mut B,
        opcode: u8,
        prefixes: Prefixes,
        address_size: AddressSize,
        operand_size: OperandSize,
    ) -> ExecResult<CycleOutcome> {
        match opcode {
            // Limit: MMX is not gated to 586+; a throttled 386/486 GSW mode would
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
            0x30 => {
                // WRMSR: write EDX:EAX into the model-specific register selected by ECX.
                // Privileged (#GP(0) outside CPL 0). An undefined MSR selector also #GP(0)s.
                self.require_cpl0()?;
                let value = self.read_edx_eax();
                match self.read_gpr32(1) {
                    MSR_MCAR => self.msr.mcar = value,
                    MSR_MCTR => self.msr.mctr = value,
                    // Rebase the time-stamp counter: store the offset that makes the running
                    // core-clock count read back as the written value.
                    MSR_TSC => self.msr.tsc_offset = value.wrapping_sub(self.elapsed_clocks),
                    MSR_EFER => {
                        if value & !EFER_WRITABLE != 0 {
                            return Err(InternalFault::Exception {
                                vector: 13,
                                error_code: Some(0),
                            });
                        }
                        self.msr.efer = value;
                    }
                    MSR_STAR => {
                        if value & !STAR_WRITABLE != 0 {
                            return Err(InternalFault::Exception {
                                vector: 13,
                                error_code: Some(0),
                            });
                        }
                        self.msr.star = value;
                    }
                    MSR_WHCR => self.msr.whcr = value,
                    _ => {
                        return Err(InternalFault::Exception {
                            vector: 13,
                            error_code: Some(0),
                        });
                    }
                }
                Ok(clocks(30))
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
                Ok(clocks(11))
            }
            0x32 => {
                // RDMSR: read the model-specific register selected by ECX into EDX:EAX.
                // Privileged; an undefined selector #GP(0)s.
                self.require_cpl0()?;
                let value = match self.read_gpr32(1) {
                    MSR_MCAR => self.msr.mcar,
                    MSR_MCTR => self.msr.mctr,
                    MSR_TSC => self.time_stamp_counter(),
                    MSR_EFER => self.msr.efer,
                    MSR_STAR => self.msr.star,
                    MSR_WHCR => self.msr.whcr,
                    _ => {
                        return Err(InternalFault::Exception {
                            vector: 13,
                            error_code: Some(0),
                        });
                    }
                };
                self.set_edx_eax(value);
                Ok(clocks(11))
            }
            0x05 => self.syscall(),
            0x07 => self.sysret(),
            0xaa => {
                // RSM: return from System Management Mode. No SMI source is modeled, so the
                // processor is never in SMM and RSM outside SMM is #UD, as on real hardware.
                // Limit: SMM entry is not implemented; RSM is therefore always invalid.
                Err(InternalFault::Exception {
                    vector: 6,
                    error_code: None,
                })
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
                    4 => self.control.cr4,
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
                    // CR0 (paging enable / WP) and CR3 (page-table base) both
                    // change translations, so flush the TLB. CR2/CR4 do not.
                    0 => {
                        self.control.cr0 = value;
                        self.flush_tlb_and_code_caches();
                    }
                    2 => self.control.cr2 = value,
                    3 => {
                        self.control.cr3 = value & 0xffff_f000;
                        self.flush_tlb_and_code_caches();
                    }
                    4 => self.control.cr4 = value,
                    _ => {}
                }
                Ok(clocks(6))
            }
            0x40..=0x4f => {
                // CMOVcc r, r/m: the source is always read (so a memory source still faults
                // when it should), but the destination register is written only when the
                // condition in the low nibble holds. A false condition leaves it untouched.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
                let value = self.read_operand_sized(bus, operand, operand_size)?;
                if self.condition(opcode & 0x0f) {
                    self.write_gpr_sized(modrm.reg, operand_size, value);
                }
                Ok(clocks(1))
            }
            // 0x80-0x8f (Jcc near, rel16/32) are converted to the decode/execute split: `decode`
            // folds them into `insn.opcode` as 0x0F80-0x0F8F, `route_group` classifies them as
            // `DecodeGroup::Branch`, and `execute_branch_decoded` runs them. Not handled here.
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
            // MOVZX/MOVSX (0F B6/B7/BE/BF) are converted to the decode/execute split; they route
            // through `DecodeGroup::DataMove` and `execute_datamove_decoded`, never reaching here.
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
                    // Extended leaf 1: the AMD processor signature in EAX (same family/model/
                    // stepping packing as leaf 1) and the extended feature flags in EDX. EBX
                    // and ECX are reserved.
                    0x8000_0001 => (CPUID_VERSION_EAX, 0, 0, CPUID_EXT_FEATURES_EDX),
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
            0xc7 => {
                // CMPXCHG8B m64 (0F C7 /1): compare EDX:EAX with the 64-bit memory operand.
                // Equal -> ZF set and ECX:EBX stored; unequal -> ZF clear and the memory
                // value loaded into EDX:EAX. Only ZF changes. The register form and any other
                // group-7 extension are #UD.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.decode_rm_operand(bus, prefixes, address_size, modrm)?;
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
                    // Re-write the destination with its own value so the bus still sees a
                    // write on the unequal branch, matching the locked read-modify-write.
                    self.write_qword(bus, mem, current)?;
                    self.set_flag(FLAG_ZF, false);
                }
                Ok(clocks(10))
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
                    // CMPXCHG8B (C7 /1) likewise read-modify-writes its m64. LOCK needs a memory
                    // operand; the register form is #UD with or without LOCK.
                    0xc7 => self.peek_u8(bus, eip.wrapping_add(1))? >> 6 != 3,
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

    fn code_linear_for_offset(&self, offset: u32, width: u32) -> ExecResult<u32> {
        let descriptor = self.registers.cs();
        if descriptor.base == 0 && descriptor.limit == u32::MAX {
            return Ok(offset);
        }
        if offset > descriptor.limit
            || offset.saturating_add(width.saturating_sub(1)) > descriptor.limit
        {
            return Err(CpuError::SegmentLimit {
                segment: SegmentIndex::Cs,
                offset,
                width,
            }
            .into());
        }
        Ok(descriptor.base.wrapping_add(offset))
    }

    fn translate_code_linear<B: CpuBus>(&mut self, bus: &mut B, linear: u32) -> ExecResult<u32> {
        let cs = self.registers.cs();
        let page = linear >> 12;
        if self.code_page.valid && self.code_page.cs == cs && self.code_page.linear_page == page {
            return Ok(self.code_page.physical_page | (linear & 0x0fff));
        }
        let physical = self.translate_linear(bus, linear, false)?;
        self.code_page = CodePageCache {
            valid: true,
            cs,
            linear_page: page,
            physical_page: physical & 0xffff_f000,
        };
        Ok(physical)
    }

    fn refill_prefetch<B: CpuBus>(
        &mut self,
        bus: &mut B,
        offset: u32,
        linear: u32,
    ) -> ExecResult<()> {
        let cs = self.registers.cs();
        let physical = self.translate_code_linear(bus, linear)?;
        let page_remaining = 0x1000 - (linear as usize & 0x0fff);
        let linear_remaining = (u32::MAX - linear) as usize + 1;
        let segment_remaining = if cs.base == 0 && cs.limit == u32::MAX {
            PREFETCH_WINDOW_BYTES
        } else {
            (cs.limit - offset + 1) as usize
        };
        let mut len = PREFETCH_WINDOW_BYTES
            .min(page_remaining)
            .min(linear_remaining)
            .min(segment_remaining);
        let mut bytes = [0u8; PREFETCH_WINDOW_BYTES];
        len = bus.prefetch_memory(physical, &mut bytes[..len])?;
        if len == 0 {
            return Err(BusError::UnmappedMemory { address: physical }.into());
        }
        self.prefetch.bytes[..len].copy_from_slice(&bytes[..len]);
        self.prefetch.cs = cs;
        self.prefetch.linear_base = linear;
        self.prefetch.physical_base = physical;
        self.prefetch.len = len as u8;
        Ok(())
    }

    fn fetch_u8<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<u8> {
        let offset = self.registers.eip;
        let cs = self.registers.cs();
        let linear = self.code_linear_for_offset(offset, 1)?;
        if self.prefetch.get(cs, linear).is_none() {
            self.refill_prefetch(bus, offset, linear)?;
        }
        let (value, physical) = self.prefetch.get(cs, linear).expect("prefetch refilled");
        bus.charge_instruction_fetch(physical)?;
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
        // Parse the addressing-mode descriptor (reads instruction bytes, no GP registers),
        // then resolve it against live registers. The two-step form keeps a single source of
        // truth for both the legacy callers and the decode/execute split.
        match self.parse_addressing_mode(bus, prefixes, address_size, modrm)? {
            DecodedOperand::Reg(index) => Ok(RmOperand::Register(index)),
            DecodedOperand::Mem(addr) => Ok(self.resolve_addr_mode(&addr)),
        }
    }

    /// Parse a ModRM addressing mode into a descriptor. Reads only instruction bytes
    /// (displacement, SIB) and never a general register, so the result can be replayed after
    /// the registers change. The effective offset is computed later by `resolve_addr_mode`.
    fn parse_addressing_mode<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        modrm: ModRm,
    ) -> ExecResult<DecodedOperand> {
        if modrm.mode == 3 {
            return Ok(DecodedOperand::Reg(modrm.rm));
        }

        let mut addr = match address_size {
            AddressSize::Word => self.parse_16bit_address(bus, modrm)?,
            AddressSize::Dword => self.parse_32bit_address(bus, modrm)?,
        };
        if let Some(segment) = prefixes.segment_override {
            addr.segment = segment;
        }
        Ok(DecodedOperand::Mem(addr))
    }

    /// Resolve an addressing-mode descriptor into a live memory operand by reading the base
    /// and index registers now. Reads only general registers (no instruction bytes), so it is
    /// safe to call repeatedly on a cached descriptor.
    fn resolve_addr_mode(&self, addr: &AddrMode) -> RmOperand {
        let offset = match addr.address_size {
            AddressSize::Word => {
                let base = addr
                    .base
                    .map_or(0u32, |reg| u32::from(self.read_gpr16(reg)));
                let index = addr
                    .index
                    .map_or(0u32, |reg| u32::from(self.read_gpr16(reg)));
                let sum = base
                    .wrapping_add(index.wrapping_mul(u32::from(addr.scale)))
                    .wrapping_add(addr.disp as u32);
                (sum as u16) as u32
            }
            AddressSize::Dword => {
                let base = addr.base.map_or(0u32, |reg| self.read_gpr32(reg));
                let index = addr.index.map_or(0u32, |reg| self.read_gpr32(reg));
                base.wrapping_add(index.wrapping_mul(u32::from(addr.scale)))
                    .wrapping_add(addr.disp as u32)
            }
        };
        RmOperand::Memory(MemoryOperand {
            segment: addr.segment,
            offset,
        })
    }

    fn parse_16bit_address<B: CpuBus>(
        &mut self,
        bus: &mut B,
        modrm: ModRm,
    ) -> ExecResult<AddrMode> {
        // 16-bit addressing combines a fixed pair of registers; encode each pair as the
        // descriptor's (base, index) with scale 1. bx=3, bp=5, si=6, di=7.
        let mut uses_bp = false;
        let (base, index) = match modrm.rm {
            0 => (Some(3), Some(6)), // bx+si
            1 => (Some(3), Some(7)), // bx+di
            2 => {
                uses_bp = true;
                (Some(5), Some(6)) // bp+si
            }
            3 => {
                uses_bp = true;
                (Some(5), Some(7)) // bp+di
            }
            4 => (None, Some(6)),                 // si
            5 => (None, Some(7)),                 // di
            6 if modrm.mode == 0 => (None, None), // disp16 only
            6 => {
                uses_bp = true;
                (Some(5), None) // bp
            }
            _ => (Some(3), None), // bx
        };

        let disp = match modrm.mode {
            0 if modrm.rm == 6 => i32::from(self.fetch_u16(bus)? as i16),
            0 => 0,
            1 => self.fetch_i8(bus)? as i32,
            2 => i32::from(self.fetch_u16(bus)? as i16),
            _ => 0,
        };

        let segment = if uses_bp {
            SegmentIndex::Ss
        } else {
            SegmentIndex::Ds
        };
        Ok(AddrMode {
            segment,
            base,
            index,
            scale: 1,
            disp,
            address_size: AddressSize::Word,
        })
    }

    fn parse_32bit_address<B: CpuBus>(
        &mut self,
        bus: &mut B,
        modrm: ModRm,
    ) -> ExecResult<AddrMode> {
        let mut base_reg = None;
        let mut index_reg = None;
        let mut scale = 1u8;

        if modrm.rm == 4 {
            let sib = self.fetch_u8(bus)?;
            scale = 1 << (sib >> 6);
            let idx = (sib >> 3) & 0x07;
            if idx != 4 {
                index_reg = Some(idx);
            }
            let base = sib & 0x07;
            if !(modrm.mode == 0 && base == 5) {
                base_reg = Some(base);
            }
        } else if !(modrm.mode == 0 && modrm.rm == 5) {
            base_reg = Some(modrm.rm);
        }

        let disp = match modrm.mode {
            0 if base_reg.is_none() => self.fetch_u32(bus)? as i32,
            0 => 0,
            1 => self.fetch_i8(bus)? as i32,
            2 => self.fetch_u32(bus)? as i32,
            _ => 0,
        };
        let segment = if matches!(base_reg, Some(4 | 5)) {
            SegmentIndex::Ss
        } else {
            SegmentIndex::Ds
        };
        Ok(AddrMode {
            segment,
            base: base_reg,
            index: index_reg,
            scale,
            disp,
            address_size: AddressSize::Dword,
        })
    }

    // (`read_rm_u8` was removed with the legacy 0x84 TEST r/m8,reg8 handler — its only remaining
    // caller. The converted flags-misc executor reads the byte r/m via `read_operand_u8` on the
    // pre-decoded operand instead. `write_rm_u8` was removed earlier with the legacy 0x88 MOV
    // r/m8,r8 handler. The sized/read siblings remain in use by the fallback handlers.)

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
        let linear = if descriptor.base == 0 && descriptor.limit == u32::MAX {
            offset
        } else {
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
            descriptor.base.wrapping_add(offset)
        };
        self.translate_linear(bus, linear, write)
    }

    fn translate_linear<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        write: bool,
    ) -> ExecResult<u32> {
        if !self.is_paging_enabled() {
            if write {
                self.record_write_page(linear);
            }
            return Ok(linear);
        }

        // Paging privilege: CPL 3 is a user access, CPL 0-2 are supervisor.
        let user = self.is_protected_mode() && (self.registers.cs().selector & 3) == 3;
        // CR0.WP (a 486 addition) makes supervisor writes obey the page R/W bit too.
        // With WP clear, supervisor writes to read-only pages succeed (386 behavior).
        let wp = self.control.cr0 & CR0_WP != 0;

        // TLB fast path: a cached entry skips the two page-table reads (and the
        // accessed-bit write the fill already did). The protection check is redone
        // from the cached page bits against the *current* accessor (CPL can change
        // without a flush); WP changes flush, so `wp` is consistent within a
        // generation. A write to a page whose dirty bit is not yet set falls through
        // to the walk so the PTE's D bit is updated.
        let page = linear >> 12;
        if let Some(e) = self.tlb.lookup(page) {
            let protection_fault = if user {
                !e.user || (write && !e.writable)
            } else {
                write && wp && !e.writable
            };
            if protection_fault {
                self.control.cr2 = linear;
                return Err(InternalFault::Exception {
                    vector: 14,
                    error_code: Some(page_fault_code(true, write, user)),
                });
            }
            // Serve the hit for a read, or a write to an already-dirty page.
            if !write || e.dirty {
                let physical = e.phys | (linear & 0x0000_0fff);
                if write {
                    self.record_write_page(physical);
                }
                return Ok(physical);
            }
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

        // Cache the completed translation. Only reached on the success path, so a
        // page that faulted (not present / protection) is never cached. `dirty`
        // records whether the PTE's D bit is now set, so a later read hits but a
        // first write to a still-clean page re-walks to set it.
        self.tlb.insert(
            page,
            pte & 0xffff_f000,
            writable,
            user_accessible,
            pte & 0x40 != 0,
        );

        let physical = (pte & 0xffff_f000) | (linear & 0x0000_0fff);
        if write {
            self.record_write_page(physical);
        }
        Ok(physical)
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
        self.set_eip(u32::from(ip));
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
        self.set_eip(offset);
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
                self.set_eip(ip & 0xffff);
                self.load_flags(flags, OperandSize::Word);
            }
            OperandSize::Dword => {
                let eip = self.pop(bus, OperandSize::Dword)?;
                let cs = self.pop(bus, OperandSize::Dword)? as u16;
                let flags = self.pop(bus, OperandSize::Dword)?;
                self.load_segment(bus, SegmentIndex::Cs, cs)?;
                self.set_eip(eip);
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
        self.set_eip(offset & operand_size.mask());
        Ok(())
    }

    fn return_far<B: CpuBus>(&mut self, bus: &mut B, operand_size: OperandSize) -> ExecResult<()> {
        // Pop the offset then CS, mirroring iret's pop order. On the 32-bit form CS
        // occupies four stack bytes; the high two are discarded on load.
        let offset = self.pop(bus, operand_size)?;
        let selector = self.pop(bus, operand_size)? as u16;
        self.load_segment(bus, SegmentIndex::Cs, selector)?;
        self.set_eip(offset & operand_size.mask());
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
        self.set_eip(offset & operand_size.mask());
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
        self.invalidate_code_caches();
        self.set_eip(gate_offset & op.mask());
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
        self.invalidate_code_caches();
        self.set_eip(gate_offset & op.mask());
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
    /// the caller and sets NT. Limit: TSS memory is accessed unpaged, and the 286
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
            // The incoming task reloads CR3, so its page mappings replace the old
            // task's: drop the previous task's cached translations.
            self.flush_tlb_and_code_caches();
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
        self.set_eip(eip);
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
        self.set_eip(self.registers.eip.wrapping_add(relative as u32) & operand_size.mask());
    }

    fn load_segment<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        selector: u16,
    ) -> ExecResult<()> {
        if self.is_v86_mode() {
            // A V86 task addresses memory like the 8086: base = selector << 4, 64 KB.
            self.load_segment_real(segment, selector);
        } else if self.is_protected_mode() {
            let register = self.load_protected_segment(bus, selector)?;
            self.registers.set_segment(segment, register);
            if segment == SegmentIndex::Cs {
                self.invalidate_code_caches();
            }
        } else {
            self.load_segment_real(segment, selector);
        }
        Ok(())
    }

    fn load_segment_real(&mut self, segment: SegmentIndex, selector: u16) {
        self.registers
            .set_segment(segment, SegmentRegister::real(selector));
        if segment == SegmentIndex::Cs {
            self.invalidate_code_caches();
        }
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

    /// The EDX:EAX register pair as one 64-bit value (EDX is the high dword). Used by the
    /// 64-bit MSR and time-stamp instructions.
    fn read_edx_eax(&self) -> u64 {
        (u64::from(self.read_gpr32(2)) << 32) | u64::from(self.read_gpr32(0))
    }

    /// Split a 64-bit value into EDX:EAX (EDX high, EAX low).
    fn set_edx_eax(&mut self, value: u64) {
        self.write_gpr32(0, value as u32);
        self.write_gpr32(2, (value >> 32) as u32);
    }

    /// The time-stamp counter: the running core-clock count plus whatever offset a WRMSR
    /// to the TSC last rebased it by.
    fn time_stamp_counter(&self) -> u64 {
        self.elapsed_clocks.wrapping_add(self.msr.tsc_offset)
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

/// Provisional per-level cycle scaling as (numerator, denominator). ponytail: a
/// coarse scalar that makes the four modes differ in instructions per clock;
/// sub-project B calibrates these against the cached CPU manuals and refines to
/// a per-instruction-class model if a single scalar cannot hold the benchmarks
/// in their era band. I386 is 1:1 because the base clocks(N) table already holds
/// roughly 386-era values.
const fn level_timing(level: CpuLevel) -> (u32, u32) {
    match level {
        CpuLevel::I286 => (4, 3),
        CpuLevel::I386 => (1, 1),
        CpuLevel::I486 => (1, 2),
        CpuLevel::I586 => (2, 5),
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

/// True when a second-byte 0F opcode is a 586-class addition the core executes only at
/// the full GSW level. Used to raise #UD when the guest throttled to 286/386/486.
const fn is_586plus_two_byte(opcode: u8) -> bool {
    matches!(
        opcode,
        // SYSCALL/SYSRET; WRMSR, RDTSC, RDMSR; CMOVcc; RSM; CMPXCHG8B.
        0x05 | 0x07 | 0x30..=0x32 | 0x40..=0x4f | 0xaa | 0xc7
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

/// FIST/FISTP rounding. Limit: round-to-nearest-even only; the control-word RC
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
                0xc0..=0xdf => {
                    // FCMOVcc ST(0), ST(i): move ST(i) into ST(0) when the integer-flag
                    // condition holds. The row picks B(CF), E(ZF), BE(CF|ZF) or U(PF).
                    let cc = match (byte >> 3) & 3 {
                        0 => 0x2, // FCMOVB
                        1 => 0x4, // FCMOVE
                        2 => 0x6, // FCMOVBE
                        _ => 0xa, // FCMOVU
                    };
                    let take = self.condition(cc);
                    self.fpu_cmov(take, byte & 7)
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
                0xe8..=0xef => {
                    // FUCOMIP ST(0), ST(i): compare, set the integer flags, then pop ST(0).
                    let a = self.fpu.get(0);
                    let b = self.fpu.get(byte & 7);
                    self.fpu_compare_set_eflags(a, b);
                    self.fpu.pop();
                    Ok(clocks(4))
                }
                0xf0..=0xf7 => {
                    // FCOMIP ST(0), ST(i): same as FUCOMIP in this model, then pop.
                    let a = self.fpu.get(0);
                    let b = self.fpu.get(byte & 7);
                    self.fpu_compare_set_eflags(a, b);
                    self.fpu.pop();
                    Ok(clocks(4))
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

    /// FCMOVcc: copy ST(i) into ST(0) when the integer-flag condition holds, leaving the
    /// stack untouched otherwise. The condition was already evaluated by the caller.
    fn fpu_cmov(&mut self, take: bool, i: u8) -> ExecResult<CycleOutcome> {
        if take {
            let v = self.fpu.get(i);
            self.fpu.set(0, v);
        }
        Ok(clocks(4))
    }

    /// FCOMI/FUCOMI: set the integer EFLAGS ZF/PF/CF from comparing ST(0) with ST(i), the
    /// way SAHF would after FNSTSW. Unordered (a NaN operand) sets all three. OF/SF/AF are
    /// cleared. Limit: FCOMI's SNaN-vs-QNaN #IA distinction is not modeled, so FCOMI and
    /// FUCOMI behave identically here.
    fn fpu_compare_set_eflags(&mut self, a: f64, b: f64) {
        let (zf, pf, cf) = if a.is_nan() || b.is_nan() {
            (true, true, true)
        } else if a > b {
            (false, false, false)
        } else if a < b {
            (false, false, true)
        } else {
            (true, false, false)
        };
        self.set_flag(FLAG_ZF, zf);
        self.set_flag(FLAG_PF, pf);
        self.set_flag(FLAG_CF, cf);
        self.set_flag(FLAG_OF, false);
        self.set_flag(FLAG_SF, false);
        self.set_flag(FLAG_AF, false);
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
            0xc0..=0xdf => {
                // FCMOVNcc ST(0), ST(i): the negated conditions of the DA forms.
                let cc = match (byte >> 3) & 3 {
                    0 => 0x3, // FCMOVNB
                    1 => 0x5, // FCMOVNE
                    2 => 0x7, // FCMOVNBE
                    _ => 0xb, // FCMOVNU
                };
                let take = self.condition(cc);
                self.fpu_cmov(take, byte & 7)
            }
            0xe8..=0xef => {
                // FUCOMI ST(0), ST(i): compare and set the integer flags ZF/PF/CF directly.
                let a = self.fpu.get(0);
                let b = self.fpu.get(byte & 7);
                self.fpu_compare_set_eflags(a, b);
                Ok(clocks(4))
            }
            0xf0..=0xf7 => {
                // FCOMI ST(0), ST(i): identical here to FUCOMI (the SNaN/QNaN #IA split is
                // not modeled, see fpu_compare_set_eflags).
                let a = self.fpu.get(0);
                let b = self.fpu.get(byte & 7);
                self.fpu_compare_set_eflags(a, b);
                Ok(clocks(4))
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

    /// SYSCALL (0F 05): the K6 fast system-call entry. Saves the return EIP in ECX, jumps
    /// to the STAR target, and loads fixed flat CS/SS from STAR[47:32] with CPL forced to
    /// 0. IF and VM are cleared. #UD when EFER.SCE is clear. No privilege or mode checks.
    fn syscall(&mut self) -> ExecResult<CycleOutcome> {
        if self.msr.efer & EFER_SCE == 0 {
            return Err(InternalFault::Exception {
                vector: 6,
                error_code: None,
            });
        }
        let star = self.msr.star;
        // EIP already points past SYSCALL, so it is the return address.
        let return_eip = self.registers.eip;
        self.write_gpr32(1, return_eip); // ECX
        self.set_eip(star as u32); // STAR[31:0]
        self.set_flag(FLAG_IF, false);
        self.set_flag(FLAG_VM, false);
        // STAR[47:32] is the CS/SS selector base; force the CS RPL to 0 so CPL reads 0.
        let cs_sel = ((star >> 32) as u16) & 0xfffc;
        self.registers
            .set_segment(SegmentIndex::Cs, SegmentRegister::flat(cs_sel, 0x9b));
        self.invalidate_code_caches();
        self.registers.set_segment(
            SegmentIndex::Ss,
            SegmentRegister::flat(cs_sel.wrapping_add(8), 0x93),
        );
        Ok(clocks(10))
    }

    /// SYSRET (0F 07): the matching return. Restores EIP from ECX and loads flat CS/SS
    /// from STAR[47:32] with the RPL forced to 3 (SS selector is the base + 16). IF is set.
    /// #UD when EFER.SCE is clear, #GP(0) outside CPL 0.
    fn sysret(&mut self) -> ExecResult<CycleOutcome> {
        if self.msr.efer & EFER_SCE == 0 {
            return Err(InternalFault::Exception {
                vector: 6,
                error_code: None,
            });
        }
        if self.current_privilege_level() != 0 {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(0),
            });
        }
        let star = self.msr.star;
        self.set_eip(self.read_gpr32(1)); // ECX -> EIP
        self.set_flag(FLAG_IF, true);
        let base = (star >> 32) as u16; // STAR[47:32]
        let cs_sel = (base & 0xfffc) | 3; // CPL 3
        let ss_sel = (base.wrapping_add(16) & 0xfffc) | 3; // SS = base + 16, RPL 3
        self.registers
            .set_segment(SegmentIndex::Cs, SegmentRegister::flat(cs_sel, 0xfb));
        self.invalidate_code_caches();
        self.registers
            .set_segment(SegmentIndex::Ss, SegmentRegister::flat(ss_sel, 0xf3));
        Ok(clocks(10))
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
                if self.control.cr0 != cr0 {
                    self.control.cr0 = cr0;
                    self.flush_tlb_and_code_caches();
                }
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
                        // INVLPG m: privileged on the 486. Flush the whole TLB (a
                        // single-page invalidate is a permitted superset and keeps
                        // the decode here simple); rare enough in DOS-era code that
                        // the extra refills do not matter.
                        if self.current_privilege_level() != 0 {
                            return Err(InternalFault::Exception {
                                vector: 6,
                                error_code: None,
                            });
                        }
                        self.flush_tlb_and_code_caches();
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
        // Limit: the 16-bit-operand quirk (base masked to 24 bits) is not modeled;
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
            let start = address as usize;
            let end = start
                .checked_add(width.bytes() as usize)
                .ok_or(BusError::UnmappedMemory { address })?;
            if end > self.memory.len() {
                return Err(BusError::UnmappedMemory { address });
            }
            Ok(match width {
                BusWidth::Byte => u32::from(self.memory[start]),
                BusWidth::Word => u32::from(u16::from_le_bytes([
                    self.memory[start],
                    self.memory[start + 1],
                ])),
                BusWidth::Dword => u32::from_le_bytes([
                    self.memory[start],
                    self.memory[start + 1],
                    self.memory[start + 2],
                    self.memory[start + 3],
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

        fn prefetch_memory(&mut self, address: u32, out: &mut [u8]) -> Result<usize, BusError> {
            let start = address as usize;
            if start >= self.memory.len() {
                return Err(BusError::UnmappedMemory { address });
            }
            let len = out.len().min(self.memory.len() - start);
            out[..len].copy_from_slice(&self.memory[start..start + len]);
            Ok(len)
        }

        fn charge_instruction_fetch(&mut self, address: u32) -> Result<(), BusError> {
            self.trace.push(BusCycle::new(
                BusAccessKind::InstructionPrefetch,
                address,
                BusWidth::Byte,
                0,
            ));
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

    // Paged-mode fetch throughput; the case the TLB targets. Run with:
    // cargo test --release -p izarravm-cpu -- --ignored --nocapture tlb_paged
    #[test]
    #[ignore]
    fn tlb_paged_fetch_throughput() {
        let mut memory = vec![0u8; 0x10000];
        memory[0..3].copy_from_slice(&[0xfa, 0xeb, 0xfe]); // cli; jmp $
        memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes()); // PDE[0] -> PT
        for i in 0..16u32 {
            let off = 0x2000 + (i as usize) * 4;
            memory[off..off + 4].copy_from_slice(&((i << 12) | 0x007).to_le_bytes()); // identity PTEs
        }
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        cpu.control.cr3 = 0x1000;
        cpu.control.cr0 |= CR0_PG;
        let mut bus = TestBus::with_memory(memory);

        let iters = 50_000_000u64;
        let t = std::time::Instant::now();
        for _ in 0..iters {
            cpu.cycle(&mut bus).unwrap();
        }
        let secs = t.elapsed().as_secs_f64();
        println!(
            "tlb_paged_fetch_throughput: {iters} paged instructions in {secs:.3}s = {:.1} M instr/s",
            iters as f64 / secs / 1.0e6
        );
    }

    #[test]
    fn tlb_caches_translations_and_is_non_snooping_until_flushed() {
        // PD at 0x1000, PT at 0x2000. Linear 0x3000 -> present+rw+user frame 0x5000.
        let mut memory = vec![0; 0x7000];
        memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes()); // PDE[0]
        memory[0x200c..0x2010].copy_from_slice(&0x0000_5007u32.to_le_bytes()); // PTE[3]
        let mut cpu = Cpu386::default();
        cpu.control.cr0 |= CR0_PG;
        cpu.control.cr3 = 0x1000;
        let mut bus = TestBus::with_memory(memory);

        // First translation walks the table and fills the TLB.
        assert_eq!(
            cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
            0x5000
        );

        // Repoint the PTE to frame 0x6000 in memory with no INVLPG / CR3 reload.
        bus.memory[0x200c..0x2010].copy_from_slice(&0x0000_6007u32.to_le_bytes());

        // Real x86 TLBs do not snoop page-table writes: the stale cached frame is
        // returned until an explicit flush -- the faithful behavior a guest relies
        // on (it must INVLPG / reload CR3 after editing a PTE).
        assert_eq!(
            cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
            0x5000
        );

        // After a flush the next access re-walks and sees the new mapping.
        cpu.tlb.flush();
        assert_eq!(
            cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
            0x6000
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

        let result = cpu.execute_instruction_legacy(&mut bus);

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

        let result = cpu.execute_instruction_legacy(&mut bus);

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

        let result = cpu.execute_instruction_legacy(&mut bus);

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

        let result = cpu.execute_instruction_legacy(&mut bus);

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

        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
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

        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
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

        // 0xa1 (MOV AX, moffs) is converted to the split, so drive it through the split executor;
        // the legacy fused entry no longer carries that arm. The #AC alignment check fires in the
        // shared memory-read helper either way.
        let result = exec_one_split(&mut cpu, &mut bus);

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

        assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
    }

    #[test]
    fn misaligned_word_read_no_fault_without_eflags_ac() {
        // CR0.AM set but EFLAGS.AC clear: software has not opted in, no fault.
        let (mut cpu, mut bus) = cpl3_word_read_at(0x0041);
        cpu.control.cr0 |= CR0_AM;

        assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
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

        assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
    }

    #[test]
    fn aligned_word_read_never_faults_with_am_and_ac() {
        // Even word address: aligned, so no #AC even with AM and AC set at CPL 3.
        let (mut cpu, mut bus) = cpl3_word_read_at(0x0040);
        cpu.control.cr0 |= CR0_AM;
        cpu.set_flag(FLAG_AC, true);

        assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
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

        assert!(cpu.execute_instruction_legacy(&mut bus).is_ok());
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

    // --- Phase 5 Slice A: RDTSC, RDMSR/WRMSR, the K6 MSR set, CR4 ---

    fn cpl3_code(code: &[u8]) -> (Cpu386, TestBus) {
        // Protected mode with a flat CPL-3 code segment (selector RPL 3), the same shape
        // the #AC/CPUID privilege tests use, but loaded with arbitrary code.
        let mut memory = vec![0u8; 256];
        memory[..code.len()].copy_from_slice(code);
        let mut cpu = Cpu386::default();
        cpu.control.cr0 |= CR0_PE;
        cpu.registers.set_segment(
            SegmentIndex::Cs,
            SegmentRegister {
                selector: 0x0003,
                base: 0,
                limit: 0xffff_ffff,
                access: 0x9b,
                default_size_32: true,
            },
        );
        cpu.registers.eip = 0;
        (cpu, TestBus::with_memory(memory))
    }

    #[test]
    fn higher_levels_charge_fewer_clocks_for_the_same_work() {
        // 01 D8 is ADD AX, BX: a register ALU op that never faults.
        fn elapsed_for(level: CpuLevel) -> u64 {
            let (mut cpu, memory) = real_mode_cpu(&[0x01, 0xd8], 0x20);
            cpu.set_level(level);
            let mut bus = TestBus::with_memory(memory);
            for _ in 0..1000 {
                cpu.registers.eip = 0;
                cpu.cycle(&mut bus).unwrap();
            }
            cpu.elapsed_clocks
        }
        let i386 = elapsed_for(CpuLevel::I386);
        let i486 = elapsed_for(CpuLevel::I486);
        let i586 = elapsed_for(CpuLevel::I586);
        assert!(
            i486 < i386,
            "486 ({i486}) should charge fewer than 386 ({i386})"
        );
        assert!(
            i586 < i486,
            "586 ({i586}) should charge fewer than 486 ({i486})"
        );
    }

    #[test]
    fn rdtsc_reads_elapsed_core_clocks_into_edx_eax() {
        // 0F 31. EDX:EAX take the 64-bit time-stamp counter (the running core-clock count).
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x31], 0x20);
        cpu.elapsed_clocks = 0x1_0000_0002;
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.eax(), 0x0000_0002);
        assert_eq!(cpu.registers.edx(), 0x0000_0001);
    }

    #[test]
    fn wrmsr_rdmsr_round_trip_whcr() {
        // 0F 30 WRMSR then 0F 32 RDMSR with ECX selecting WHCR. EDX:EAX is the 64-bit value.
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x30, 0x0f, 0x32], 0x20);
        cpu.registers.set_ecx(MSR_WHCR);
        cpu.registers.set_edx(0xdead_beef);
        cpu.registers.set_eax(0x0bad_f00d);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap(); // wrmsr
        assert_eq!(cpu.msr.whcr, 0xdead_beef_0bad_f00d);
        cpu.registers.set_eax(0);
        cpu.registers.set_edx(0);
        cpu.cycle(&mut bus).unwrap(); // rdmsr
        assert_eq!(cpu.registers.edx(), 0xdead_beef);
        assert_eq!(cpu.registers.eax(), 0x0bad_f00d);
    }

    #[test]
    fn wrmsr_efer_rejects_reserved_bits() {
        // Only EFER.SCE (bit 0) is writable; any reserved bit set raises #GP(0).
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x30], 0x20);
        cpu.registers.set_ecx(MSR_EFER);
        cpu.registers.set_edx(0);
        cpu.registers.set_eax(0x2); // bit 1 is reserved
        let mut bus = TestBus::with_memory(memory);
        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
        assert!(matches!(
            fault,
            InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            }
        ));
    }

    #[test]
    fn wrmsr_star_rejects_reserved_bits() {
        // STAR bits 63-48 are reserved; setting one raises #GP(0).
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x30], 0x20);
        cpu.registers.set_ecx(MSR_STAR);
        cpu.registers.set_edx(0x0001_0000); // bit 48 set
        cpu.registers.set_eax(0);
        let mut bus = TestBus::with_memory(memory);
        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
        assert!(matches!(
            fault,
            InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            }
        ));
    }

    #[test]
    fn wrmsr_efer_and_star_accept_their_defined_bits() {
        // SCE in EFER and the selector base / target EIP in STAR write without faulting.
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x30, 0x0f, 0x30], 0x20);
        cpu.registers.set_ecx(MSR_EFER);
        cpu.registers.set_edx(0);
        cpu.registers.set_eax(EFER_SCE as u32);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.msr.efer, EFER_SCE);
        cpu.registers.set_ecx(MSR_STAR);
        cpu.registers.set_edx(0x0000_ffff); // selector base in 47-32
        cpu.registers.set_eax(0x0001_0000); // target EIP in 31-0
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.msr.star, 0x0000_ffff_0001_0000);
    }

    #[test]
    fn wrmsr_tsc_rebases_so_the_counter_reads_the_written_value() {
        // Writing the TSC stores an offset such that the running core-clock count reads
        // back as the written value. execute_instruction does not advance elapsed_clocks,
        // so the read is exact.
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x30], 0x20);
        cpu.elapsed_clocks = 500;
        cpu.registers.set_ecx(MSR_TSC);
        cpu.registers.set_edx(0);
        cpu.registers.set_eax(1_000_000);
        let mut bus = TestBus::with_memory(memory);
        cpu.execute_instruction_legacy(&mut bus).unwrap();
        assert_eq!(cpu.time_stamp_counter(), 1_000_000);
    }

    #[test]
    fn wrmsr_is_general_protection_at_cpl3() {
        let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x30]);
        cpu.registers.set_ecx(MSR_WHCR);
        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
        assert!(matches!(
            fault,
            InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            }
        ));
    }

    #[test]
    fn rdmsr_unknown_selector_is_general_protection() {
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x32], 0x20);
        cpu.registers.set_ecx(0x1234_5678);
        let mut bus = TestBus::with_memory(memory);
        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
        assert!(matches!(
            fault,
            InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            }
        ));
    }

    #[test]
    fn rdtsc_is_general_protection_when_tsd_set_at_cpl3() {
        let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x31]);
        cpu.control.cr4 |= CR4_TSD;
        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
        assert!(matches!(
            fault,
            InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            }
        ));
    }

    #[test]
    fn rdtsc_runs_at_cpl3_when_tsd_clear() {
        let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x31]);
        cpu.elapsed_clocks = 42;
        assert!(cpu.execute_instruction_legacy(&mut bus).is_ok());
        assert_eq!(cpu.registers.eax(), 42);
    }

    #[test]
    fn mov_cr4_round_trips() {
        // 0F 22 E0 = MOV CR4, EAX (reg=4, rm=EAX); 0F 20 E3 = MOV EBX, CR4 (reg=4, rm=EBX).
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x22, 0xe0, 0x0f, 0x20, 0xe3], 0x20);
        cpu.registers.set_eax(CR4_TSD);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.control.cr4, CR4_TSD);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.ebx(), CR4_TSD);
    }

    #[test]
    fn cpuid_leaf1_reports_tsc_and_msr() {
        let edx = run_cpuid(1).registers.edx();
        assert_ne!(edx & (1 << 4), 0, "TSC feature bit should be set");
        assert_ne!(edx & (1 << 5), 0, "MSR feature bit should be set");
    }

    #[test]
    fn rdtsc_is_undefined_opcode_below_586() {
        // RDTSC is a 586 addition: #UD at the throttled 486 level, fine at 586.
        let code = [0x0f, 0x31];
        assert!(matches!(
            run_at_level(&code, CpuLevel::I486).unwrap_err(),
            InternalFault::Exception { vector: 6, .. }
        ));
        assert!(run_at_level(&code, CpuLevel::I586).is_ok());
    }

    // --- Phase 5 Slice B: CMOVcc, FCMOVcc, FCOMI/FUCOMI ---

    #[test]
    fn cmove_word_moves_low_half_when_zf_set() {
        // 0F 44 C3: CMOVE AX, BX (16-bit operand in real mode).
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x44, 0xc3], 0x20);
        cpu.registers.set_eax(0x1111_1111);
        cpu.registers.set_ebx(0xaaaa_bbbb);
        cpu.set_flag(FLAG_ZF, true);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        // A 16-bit move writes only the low word; the upper half of EAX is preserved.
        assert_eq!(cpu.registers.eax(), 0x1111_bbbb);
    }

    #[test]
    fn cmove_does_not_move_when_zf_clear() {
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x44, 0xc3], 0x20);
        cpu.registers.set_eax(0x1111_1111);
        cpu.registers.set_ebx(0xaaaa_bbbb);
        cpu.set_flag(FLAG_ZF, false);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.eax(), 0x1111_1111);
    }

    #[test]
    fn cmovne_dword_moves_when_zf_clear() {
        // 66 0F 45 C3: CMOVNE EAX, EBX (32-bit operand).
        let (mut cpu, memory) = real_mode_cpu(&[0x66, 0x0f, 0x45, 0xc3], 0x20);
        cpu.registers.set_eax(0x1111_1111);
        cpu.registers.set_ebx(0xaaaa_bbbb);
        cpu.set_flag(FLAG_ZF, false);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.eax(), 0xaaaa_bbbb);
    }

    #[test]
    fn cmovcc_is_undefined_opcode_below_586() {
        let code = [0x0f, 0x44, 0xc3];
        assert!(matches!(
            run_at_level(&code, CpuLevel::I486).unwrap_err(),
            InternalFault::Exception { vector: 6, .. }
        ));
        assert!(run_at_level(&code, CpuLevel::I586).is_ok());
    }

    #[test]
    fn fcmove_moves_st1_into_st0_when_zf_set() {
        // DA C9: FCMOVE ST(0), ST(1).
        let (mut cpu, memory) = real_mode_cpu(&[0xda, 0xc9], 0x20);
        cpu.fpu.push(2.0); // ST(1)
        cpu.fpu.push(1.0); // ST(0)
        cpu.set_flag(FLAG_ZF, true);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.fpu.get(0), 2.0);
    }

    #[test]
    fn fcmove_leaves_st0_when_zf_clear() {
        let (mut cpu, memory) = real_mode_cpu(&[0xda, 0xc9], 0x20);
        cpu.fpu.push(2.0);
        cpu.fpu.push(1.0);
        cpu.set_flag(FLAG_ZF, false);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.fpu.get(0), 1.0);
    }

    #[test]
    fn fcmovnb_moves_st1_into_st0_when_cf_clear() {
        // DB C1: FCMOVNB ST(0), ST(1).
        let (mut cpu, memory) = real_mode_cpu(&[0xdb, 0xc1], 0x20);
        cpu.fpu.push(7.0); // ST(1)
        cpu.fpu.push(3.0); // ST(0)
        cpu.set_flag(FLAG_CF, false);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.fpu.get(0), 7.0);
    }

    #[test]
    fn fcomi_sets_integer_flags_from_the_comparison() {
        // DB F1: FCOMI ST(0), ST(1). The result lands in ZF/PF/CF.
        fn run(st0: f64, st1: f64) -> (bool, bool, bool) {
            let (mut cpu, memory) = real_mode_cpu(&[0xdb, 0xf1], 0x40);
            cpu.fpu.push(st1);
            cpu.fpu.push(st0);
            let mut bus = TestBus::with_memory(memory);
            cpu.cycle(&mut bus).unwrap();
            (cpu.flag(FLAG_ZF), cpu.flag(FLAG_PF), cpu.flag(FLAG_CF))
        }
        assert_eq!(run(2.0, 1.0), (false, false, false)); // ST0 > ST1
        assert_eq!(run(1.0, 2.0), (false, false, true)); // ST0 < ST1
        assert_eq!(run(1.0, 1.0), (true, false, false)); // equal
        assert_eq!(run(f64::NAN, 1.0), (true, true, true)); // unordered
    }

    #[test]
    fn fcomip_compares_then_pops() {
        // DF F1: FCOMIP ST(0), ST(1). Equal operands set ZF, then ST(0) is popped.
        let (mut cpu, memory) = real_mode_cpu(&[0xdf, 0xf1], 0x40);
        cpu.fpu.push(2.0);
        cpu.fpu.push(2.0);
        let top_before = cpu.fpu.top();
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert!(cpu.flag(FLAG_ZF));
        assert_eq!(cpu.fpu.top(), (top_before + 1) & 7);
    }

    // --- Phase 5 Slice C: CMPXCHG8B ---

    fn read_dword(memory: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([memory[at], memory[at + 1], memory[at + 2], memory[at + 3]])
    }

    #[test]
    fn cmpxchg8b_equal_stores_ecx_ebx_and_sets_zf() {
        // 0F C7 0E 40 00: CMPXCHG8B [0x0040] (reg=/1, mod=0 rm=6 direct disp16).
        let (mut cpu, mut memory) = real_mode_cpu(&[0x0f, 0xc7, 0x0e, 0x40, 0x00], 0x80);
        memory[0x40..0x44].copy_from_slice(&0x5566_7788u32.to_le_bytes());
        memory[0x44..0x48].copy_from_slice(&0x1122_3344u32.to_le_bytes());
        cpu.registers.set_eax(0x5566_7788); // EDX:EAX equals the memory value
        cpu.registers.set_edx(0x1122_3344);
        cpu.registers.set_ebx(0xcafe_babe); // ECX:EBX is the value to store
        cpu.registers.set_ecx(0xdead_beef);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert!(cpu.flag(FLAG_ZF));
        assert_eq!(read_dword(&bus.memory, 0x40), 0xcafe_babe);
        assert_eq!(read_dword(&bus.memory, 0x44), 0xdead_beef);
    }

    #[test]
    fn cmpxchg8b_unequal_loads_edx_eax_and_clears_zf() {
        let (mut cpu, mut memory) = real_mode_cpu(&[0x0f, 0xc7, 0x0e, 0x40, 0x00], 0x80);
        memory[0x40..0x44].copy_from_slice(&0xaaaa_bbbbu32.to_le_bytes());
        memory[0x44..0x48].copy_from_slice(&0xcccc_ddddu32.to_le_bytes());
        cpu.registers.set_eax(0x0000_0001); // EDX:EAX differs from memory
        cpu.registers.set_edx(0x0000_0002);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert!(!cpu.flag(FLAG_ZF));
        assert_eq!(cpu.registers.eax(), 0xaaaa_bbbb);
        assert_eq!(cpu.registers.edx(), 0xcccc_dddd);
        assert_eq!(read_dword(&bus.memory, 0x40), 0xaaaa_bbbb); // memory unchanged
    }

    #[test]
    fn cmpxchg8b_register_form_is_undefined_opcode() {
        // 0F C7 C9: mod=3 register form is #UD.
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0xc7, 0xc9], 0x20);
        let mut bus = TestBus::with_memory(memory);
        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
        assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
    }

    #[test]
    fn cmpxchg8b_wrong_group_extension_is_undefined_opcode() {
        // 0F C7 06 40 00: reg=/0, not CMPXCHG8B -> #UD.
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0xc7, 0x06, 0x40, 0x00], 0x80);
        let mut bus = TestBus::with_memory(memory);
        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
        assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
    }

    #[test]
    fn lock_cmpxchg8b_to_memory_is_accepted() {
        // F0 0F C7 0E 40 00: LOCK CMPXCHG8B [0x0040].
        let (mut cpu, mut memory) = real_mode_cpu(&[0xf0, 0x0f, 0xc7, 0x0e, 0x40, 0x00], 0x80);
        memory[0x40..0x48].copy_from_slice(&0u64.to_le_bytes());
        cpu.registers.set_ebx(0x11);
        cpu.registers.set_ecx(0x22);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert!(cpu.flag(FLAG_ZF)); // EDX:EAX = 0 equals zeroed memory
        assert_eq!(read_dword(&bus.memory, 0x40), 0x11);
        assert_eq!(read_dword(&bus.memory, 0x44), 0x22);
    }

    #[test]
    fn lock_cmpxchg8b_register_form_is_undefined_opcode() {
        // F0 0F C7 C9: LOCK on the register form -> #UD.
        let (mut cpu, memory) = real_mode_cpu(&[0xf0, 0x0f, 0xc7, 0xc9], 0x20);
        let mut bus = TestBus::with_memory(memory);
        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
        assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
    }

    #[test]
    fn cpuid_leaf1_reports_cx8() {
        let edx = run_cpuid(1).registers.edx();
        assert_ne!(edx & (1 << 8), 0, "CX8 feature bit should be set");
    }

    #[test]
    fn cmpxchg8b_is_undefined_opcode_below_586() {
        let code = [0x0f, 0xc7, 0x0e, 0x40, 0x00];
        assert!(matches!(
            run_at_level(&code, CpuLevel::I486).unwrap_err(),
            InternalFault::Exception { vector: 6, .. }
        ));
        assert!(run_at_level(&code, CpuLevel::I586).is_ok());
    }

    // --- Phase 5 Slice D: SYSCALL/SYSRET and RSM ---

    #[test]
    fn syscall_jumps_to_star_target_and_loads_flat_segments() {
        // 0F 05 SYSCALL. STAR: target EIP = 0x0001_0000, CS/SS selector base = 0x08.
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x05], 0x40);
        cpu.msr.efer = EFER_SCE;
        cpu.msr.star = (0x0008u64 << 32) | 0x0001_0000;
        cpu.set_flag(FLAG_IF, true);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.ecx(), 2); // return address (EIP past the 2-byte SYSCALL)
        assert_eq!(cpu.registers.eip, 0x0001_0000);
        let cs = cpu.registers.cs();
        assert_eq!(cs.selector, 0x08);
        assert_eq!(cs.base, 0);
        assert_eq!(cs.limit, 0xffff_ffff);
        assert!(cs.default_size_32);
        assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, 0x10); // base + 8
        assert!(!cpu.flag(FLAG_IF)); // SYSCALL clears IF
    }

    #[test]
    fn syscall_is_undefined_opcode_without_sce() {
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x05], 0x20);
        cpu.msr.efer = 0;
        let mut bus = TestBus::with_memory(memory);
        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
        assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
    }

    #[test]
    fn sysret_returns_to_ecx_with_cpl3_and_sets_if() {
        // 0F 07 SYSRET. STAR CS/SS base = 0x08; SYSRET forces RPL 3 and SS = base + 16.
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x07], 0x20);
        cpu.msr.efer = EFER_SCE;
        cpu.msr.star = 0x0008u64 << 32;
        cpu.registers.set_ecx(0x0002_0000);
        cpu.set_flag(FLAG_IF, false);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.eip, 0x0002_0000);
        assert_eq!(cpu.registers.cs().selector, 0x0b); // 0x08 | 3
        assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, 0x1b); // (0x08 + 16) | 3
        assert!(cpu.flag(FLAG_IF)); // SYSRET sets IF
    }

    #[test]
    fn sysret_is_general_protection_at_cpl3() {
        let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x07]);
        cpu.msr.efer = EFER_SCE;
        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
        assert!(matches!(
            fault,
            InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            }
        ));
    }

    #[test]
    fn sysret_is_undefined_opcode_without_sce() {
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x07], 0x20);
        cpu.msr.efer = 0;
        let mut bus = TestBus::with_memory(memory);
        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
        assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
    }

    #[test]
    fn rsm_is_undefined_opcode_outside_smm() {
        // No SMM is modeled, so RSM always faults #UD.
        let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0xaa], 0x20);
        let mut bus = TestBus::with_memory(memory);
        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
        assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
    }

    #[test]
    fn syscall_is_undefined_opcode_below_586() {
        // Even with SCE enabled, a throttled 486-level guest sees #UD from the 586 gate.
        let mut memory = vec![0u8; 64];
        memory[..2].copy_from_slice(&[0x0f, 0x05]);
        let mut cpu = Cpu386::default();
        cpu.set_level(CpuLevel::I486);
        cpu.msr.efer = EFER_SCE;
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.registers.eip = 0;
        let mut bus = TestBus::with_memory(memory);
        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
        assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
    }

    /// Run a single instruction from `code` at the given level and return the result.
    /// Run one instruction through the production decode/execute split and return the raw
    /// `InternalFault` (without exception delivery), so a test can assert `is_ok()`/`unwrap_err()`
    /// exactly as it did against `execute_instruction_legacy`. Use this for opcodes already
    /// converted to the split — `execute_instruction_legacy` routes only through the legacy fused
    /// `dispatch_opcode`, which no longer carries the converted arms, so it would wrongly #UD them.
    fn exec_one_split<B: CpuBus>(cpu: &mut Cpu386, bus: &mut B) -> ExecResult<CycleOutcome> {
        cpu.begin_instruction();
        let insn = cpu.decode(bus)?;
        cpu.execute_decoded(&insn, bus)
    }

    fn run_at_level(code: &[u8], level: CpuLevel) -> Result<CycleOutcome, InternalFault> {
        let mut memory = vec![0; 1024];
        memory[..code.len()].copy_from_slice(code);
        let mut cpu = Cpu386::default();
        cpu.set_level(level);
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.registers.eip = 0;
        let mut bus = TestBus::with_memory(memory);
        // Route through the split so converted groups (ALU, data-move) execute; the legacy fused
        // entry would #UD them now that their arms are gone from `dispatch_opcode`. Prefix-gating
        // (66/67 at I286, the 0F two-byte ISA gate) lives in `decode`/`dispatch_opcode` either way,
        // so the #UD-at-286 assertions still hold.
        exec_one_split(&mut cpu, &mut bus)
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
        cpu.write_reg16(Reg16::Bx, 0x0042);
        let mut bus = TestBus::with_memory(memory);
        // Drive through the split (MOVZX is a converted group now); the firmware-ROM exemption to
        // the 286 ISA gate lives in `decode`, which `exec_one_split` exercises. Confirm the op truly
        // ran (AX = zero-extended BL) rather than merely not faulting.
        assert!(
            exec_one_split(&mut cpu, &mut bus).is_ok(),
            "MOVZX fetched from BIOS ROM must run even at I286"
        );
        assert_eq!(
            cpu.read_reg16(Reg16::Ax),
            0x0042,
            "MOVZX must have zero-extended BL into AX"
        );
    }

    #[test]
    fn unconverted_two_byte_opcode_matches_the_fused_reference_after_the_convention() {
        // RDTSC (0F 31) is NOT converted: it stays on the Fallback path. After the two-byte decode
        // convention landed it must still execute identically to the fused reference — decode folds
        // the second byte into insn.opcode and the Fallback arm hands it to execute_two_byte without
        // re-reading it. Diff the split path against execute_instruction_legacy (both un-converted,
        // so both still run RDTSC) for register/eip equality AND identical InstructionPrefetch
        // counts: a second-byte double-charge in the convention would drift the fetch count here.
        let code = [0x0f, 0x31];
        let mut mem = vec![0u8; 64];
        mem[..code.len()].copy_from_slice(&code);

        let mut fused = Cpu386::default();
        fused.load_segment_real(SegmentIndex::Cs, 0);
        fused.registers.eip = 0;
        let mut fbus = TestBus::with_memory(mem.clone());
        fused.begin_instruction();
        fused
            .execute_instruction_legacy(&mut fbus)
            .expect("RDTSC must run on the fused reference");

        let mut split = Cpu386::default();
        split.load_segment_real(SegmentIndex::Cs, 0);
        split.registers.eip = 0;
        let mut sbus = TestBus::with_memory(mem);
        exec_one_split(&mut split, &mut sbus).expect("RDTSC must run through the split convention");

        assert_eq!(split.registers.eip, fused.registers.eip, "eip must match");
        assert_eq!(split.registers.gpr, fused.registers.gpr, "gpr must match");
        assert_eq!(
            seam_fetch_count(&sbus),
            seam_fetch_count(&fbus),
            "the convention must charge the same fetches as the fused path (no second-byte re-read)"
        );
    }

    #[test]
    fn throttled_286_raises_ud_for_an_unconverted_two_byte_opcode_via_the_new_gate() {
        // BSWAP EAX (0F C8) is a 486 addition and stays on Fallback. The ISA gate that #UDs it at
        // the 286 level now lives in `decode` (the shared convention point), not in execute_two_byte.
        // Proving an *un-converted* 0F op still #UDs confirms the gate did not get tied to the one
        // converted group.
        let code = [0x0f, 0xc8];
        assert!(
            matches!(
                run_at_level(&code, CpuLevel::I286).unwrap_err(),
                InternalFault::Exception { vector: 6, .. }
            ),
            "BSWAP must #UD at I286 through the new gate location"
        );
        assert!(
            run_at_level(&code, CpuLevel::I486).is_ok(),
            "BSWAP must run at I486"
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
        assert!(cpu.execute_instruction_legacy(&mut bus).is_ok());
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
    fn cpuid_extended_leaf1_reports_amd_feature_flags() {
        let cpu = run_cpuid(0x8000_0001);
        // EAX carries the processor signature: family 5.
        assert_eq!((cpu.registers.eax() >> 8) & 0xf, 5);
        let edx = cpu.registers.edx();
        // The implemented instructions sit at their AMD extended-leaf bit positions.
        assert_ne!(edx & (1 << 10), 0, "SYSCALL/SYSRET (bit 10)");
        assert_ne!(edx & (1 << 15), 0, "integer CMOVcc (bit 15)");
        assert_ne!(edx & (1 << 16), 0, "FP FCMOVcc (bit 16)");
        assert_ne!(edx & (1 << 4), 0, "TSC");
        assert_ne!(edx & (1 << 5), 0, "MSR");
        assert_ne!(edx & (1 << 8), 0, "CX8");
        assert_ne!(edx & (1 << 23), 0, "MMX");
        // Features the GSW-586 does not emulate stay clear.
        assert_eq!(edx & 1, 0, "FPU off");
        assert_eq!(edx & (1 << 7), 0, "no machine-check exception");
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

    /// Shared seed for the seam differential / golden batteries below: a fixed real-mode register
    /// set plus a known word at [0x20], so each instruction has stable inputs.
    fn seam_seed(cpu: &mut Cpu386) {
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Ax, 0x0102);
        cpu.write_reg16(Reg16::Bx, 0x0010);
        cpu.write_reg16(Reg16::Cx, 0x0304);
        cpu.write_reg16(Reg16::Dx, 0x0506);
        cpu.write_reg16(Reg16::Si, 0x0008);
        cpu.write_reg16(Reg16::Di, 0x0018);
        cpu.write_reg16(Reg16::Bp, 0x0010);
    }

    fn seam_fetch_count(bus: &TestBus) -> usize {
        bus.trace
            .cycles()
            .iter()
            .filter(|c| c.kind == BusAccessKind::InstructionPrefetch)
            .count()
    }

    #[test]
    fn seam_matches_fused_path_across_addressing_forms() {
        // Run a battery of *non-converted* (legacy-fallback) instructions through cycle()
        // (decode/execute split) and through execute_instruction_legacy (fused) from identical
        // start state and assert identical end state. Guards that, for opcodes still on the legacy
        // path, the seam stays behaviorally bit-identical to the fused path across addressing forms.
        // The ALU block and the single-byte data-movement block (MOV/LEA/XCHG and friends) are no
        // longer comparable this way: both are fully converted to the split, so their former fused
        // executors were deleted (there must be no second plumbing path). They are covered against
        // golden end-states in `alu_split_matches_golden_across_ops` and
        // `datamove_split_matches_golden_across_ops`. `inc word [bx]` (0xff /0) used to be the case
        // here, but 0xff (group 5) is now converted (`DecodeGroup::ControlFlow`). `test [bx], cx`
        // (0x85) was used after that, but 0x84/0x85 are now converted (`DecodeGroup::FlagsMisc`).
        // Use `xlat` (0xd7), still on Fallback, to keep exercising a memory-read through the seam:
        // AL = [DS:BX+AL]. With seam_seed: BX=0x10, AL=0x02, so reads from 0x12.
        let cases: &[(&str, &[u8])] = &[("xlat", &[0xd7])];
        for (name, code) in cases {
            let mut mem = vec![0u8; 0x200];
            mem[..code.len()].copy_from_slice(code);
            mem[0x12] = 0xab; // the XLAT lookup result planted at [BX+AL]=0x12

            let mut fused = Cpu386::default();
            seam_seed(&mut fused);
            let mut fbus = TestBus::with_memory(mem.clone());
            fused.begin_instruction();
            let _ = fused.execute_instruction_legacy(&mut fbus);

            let mut split = Cpu386::default();
            seam_seed(&mut split);
            let mut sbus = TestBus::with_memory(mem.clone());
            let _ = split.cycle(&mut sbus);

            assert_eq!(
                split.registers.gpr, fused.registers.gpr,
                "gpr mismatch for {name}"
            );
            assert_eq!(
                split.registers.eflags, fused.registers.eflags,
                "eflags mismatch for {name}"
            );
            assert_eq!(
                split.registers.eip, fused.registers.eip,
                "eip mismatch for {name}"
            );
            assert_eq!(sbus.memory, fbus.memory, "memory mismatch for {name}");

            // Clock-neutrality guard: the seam must charge each instruction-fetch byte exactly
            // once. The machine derives device/IRQ timing from the bus-trace fetch records, so a
            // double-charge (e.g. decode reading the opcode and the fallback re-reading it) drifts
            // timing. Count the InstructionPrefetch bus cycles on each path and require equality.
            // This is scale-independent, unlike `core_clocks` (cycle() scales via scale_clocks
            // while the fused reference returns raw clocks).
            assert_eq!(
                seam_fetch_count(&sbus),
                seam_fetch_count(&fbus),
                "instruction-fetch cycle count mismatch for {name} (seam must charge fetches once)"
            );
        }
    }

    /// One golden end-state for an ALU case run from `seam_seed`: the opcode bytes plus the
    /// expected end gpr (AX,CX,DX,BX,SP,BP,SI,DI), eflags, eip, (offset,value) memory writes, and
    /// InstructionPrefetch fetch count. Shared between the assertion test and the regen helper.
    struct AluGolden {
        name: &'static str,
        code: &'static [u8],
        gpr: [u32; 8],
        eflags: u32,
        eip: u32,
        deltas: &'static [(usize, u8)],
        fetch: usize,
    }

    /// The ALU differential battery: every op/form/addressing-mode case plus its golden end-state.
    ///
    /// HOW TO CAPTURE / REGENERATE GOLDENS (read before editing any `gpr`/`eflags`/`deltas`/`fetch`
    /// below, and follow this same recipe for every future group-conversion task):
    ///   1. The goldens are captured from the PRIOR fused reference (`execute_instruction_legacy`),
    ///      NOT from the new split path. Capturing from the split would be tautological — it would
    ///      assert the code matches itself and catch nothing.
    ///   2. Run `cargo test -p izarravm-cpu --lib regen_alu_goldens -- --ignored --nocapture` while
    ///      the group's fused arm still exists, then paste the printed literals here. For a new
    ///      group, capture BEFORE you delete its fused arm from `dispatch_opcode`.
    ///   3. For THIS (ALU) group the fused arm is already gone on `perf-decode-cache`, so the regen
    ///      helper must be run from the pre-split base commit (332be72): `git stash`, check out the
    ///      base, run the command, paste, then return. (These goldens were captured exactly so.)
    ///   4. Never hand-edit a golden to make a failing test pass — re-capture from the reference.
    fn alu_golden_cases() -> &'static [AluGolden] {
        &[
            // Forms 0-3 (r/m,reg and reg,r/m, byte and word), several addressing modes.
            AluGolden {
                name: "add ax,bx",
                code: &[0x01, 0xd8],
                gpr: [274, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x06,
                eip: 0x02,
                deltas: &[],
                fetch: 3,
            },
            AluGolden {
                name: "add [bx+si],ax",
                code: &[0x01, 0x00],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x02,
                eip: 0x02,
                deltas: &[(24, 2), (25, 1)],
                fetch: 3,
            },
            AluGolden {
                name: "add [bp+di+4],cx",
                code: &[0x01, 0x4b, 0x04],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x02,
                eip: 0x03,
                deltas: &[(44, 4), (45, 3)],
                fetch: 4,
            },
            AluGolden {
                name: "add [0x20],dx",
                code: &[0x01, 0x16, 0x20, 0x00],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x06,
                eip: 0x04,
                deltas: &[(32, 23), (33, 22)],
                fetch: 5,
            },
            AluGolden {
                name: "add [si],al(byte)",
                code: &[0x00, 0x04],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x02,
                eip: 0x02,
                deltas: &[(8, 2)],
                fetch: 3,
            },
            // Every ALU op through word r/m,reg (form 1) with a memory operand: op-by-op coverage.
            AluGolden {
                name: "add [bx],ax(form1)",
                code: &[0x01, 0x07],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x02,
                eip: 0x02,
                deltas: &[(16, 2), (17, 1)],
                fetch: 3,
            },
            AluGolden {
                name: "or [bx],ax(form1)",
                code: &[0x09, 0x07],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x02,
                eip: 0x02,
                deltas: &[(16, 2), (17, 1)],
                fetch: 3,
            },
            AluGolden {
                name: "adc [bx],ax(form1)",
                code: &[0x11, 0x07],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x02,
                eip: 0x02,
                deltas: &[(16, 2), (17, 1)],
                fetch: 3,
            },
            AluGolden {
                name: "sbb [bx],ax(form1)",
                code: &[0x19, 0x07],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x93,
                eip: 0x02,
                deltas: &[(16, 254), (17, 254)],
                fetch: 3,
            },
            AluGolden {
                name: "and [bx],ax(form1)",
                code: &[0x21, 0x07],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x46,
                eip: 0x02,
                deltas: &[],
                fetch: 3,
            },
            AluGolden {
                name: "sub [bx],ax(form1)",
                code: &[0x29, 0x07],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x93,
                eip: 0x02,
                deltas: &[(16, 254), (17, 254)],
                fetch: 3,
            },
            AluGolden {
                name: "xor [bx],ax(form1)",
                code: &[0x31, 0x07],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x02,
                eip: 0x02,
                deltas: &[(16, 2), (17, 1)],
                fetch: 3,
            },
            AluGolden {
                name: "cmp [bx],ax(form1)",
                code: &[0x39, 0x07],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x93,
                eip: 0x02,
                deltas: &[],
                fetch: 3,
            },
            // reg,r/m direction (form 3, word; writes a register) and byte directions (forms 0/2).
            AluGolden {
                name: "or cx,[bx+si]",
                code: &[0x0b, 0x08],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x02,
                eip: 0x02,
                deltas: &[],
                fetch: 3,
            },
            AluGolden {
                name: "and dx,[di]",
                code: &[0x23, 0x15],
                gpr: [258, 772, 0, 16, 0, 16, 8, 24],
                eflags: 0x46,
                eip: 0x02,
                deltas: &[],
                fetch: 3,
            },
            AluGolden {
                name: "adc al,[bx](byte form2)",
                code: &[0x12, 0x07],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x02,
                eip: 0x02,
                deltas: &[],
                fetch: 3,
            },
            AluGolden {
                name: "xor [si],bl(byte form0)",
                code: &[0x30, 0x1c],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x02,
                eip: 0x02,
                deltas: &[(8, 16)],
                fetch: 3,
            },
            // Immediate accumulator forms: byte AL,imm8 (form 4) and word AX,imm16 (form 5).
            AluGolden {
                name: "add al,imm8(form4)",
                code: &[0x04, 0x7f],
                gpr: [385, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x896,
                eip: 0x02,
                deltas: &[],
                fetch: 3,
            },
            AluGolden {
                name: "or al,imm8(form4)",
                code: &[0x0c, 0xaa],
                gpr: [426, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x86,
                eip: 0x02,
                deltas: &[],
                fetch: 3,
            },
            AluGolden {
                name: "cmp al,imm8(form4)",
                code: &[0x3c, 0x05],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x93,
                eip: 0x02,
                deltas: &[],
                fetch: 3,
            },
            AluGolden {
                name: "add ax,imm16(form5)",
                code: &[0x05, 0x34, 0x12],
                gpr: [4918, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x06,
                eip: 0x03,
                deltas: &[],
                fetch: 4,
            },
            AluGolden {
                name: "sub ax,imm16(form5)",
                code: &[0x2d, 0x34, 0x12],
                gpr: [61134, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x93,
                eip: 0x03,
                deltas: &[],
                fetch: 4,
            },
            AluGolden {
                name: "cmp ax,imm16(form5)",
                code: &[0x3d, 0x02, 0x01],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x46,
                eip: 0x03,
                deltas: &[],
                fetch: 4,
            },
            // Remaining addressing forms carried over from the original battery.
            AluGolden {
                name: "sub [bp+2],ax",
                code: &[0x29, 0x46, 0x02],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x93,
                eip: 0x03,
                deltas: &[(18, 254), (19, 254)],
                fetch: 4,
            },
            AluGolden {
                name: "xor [di],bx",
                code: &[0x31, 0x1d],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x02,
                eip: 0x02,
                deltas: &[(24, 16)],
                fetch: 3,
            },
            AluGolden {
                name: "cmp [bx+4],dx",
                code: &[0x39, 0x57, 0x04],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x97,
                eip: 0x03,
                deltas: &[],
                fetch: 4,
            },
        ]
    }

    #[test]
    fn alu_split_matches_golden_across_ops() {
        // The whole ALU block (ADD/OR/ADC/SBB/AND/SUB/XOR/CMP) is converted to the decode/execute
        // split, so it can no longer be diffed against a fused executor (that path was deleted to
        // keep a single ALU implementation). Instead, run each op/form through cycle() and assert
        // the architectural end-state against goldens captured from the pre-split fused path
        // (commit 332be72; see `alu_golden_cases` for the capture recipe). This exercises decode's
        // ModRM/immediate parsing, the executor's operand wiring + write-back gating, the EA
        // recompute, and the once-only instruction-fetch charge.
        for g in alu_golden_cases() {
            let mut mem = vec![0u8; 0x200];
            mem[..g.code.len()].copy_from_slice(g.code);
            mem[0x20..0x22].copy_from_slice(&0x1111u16.to_le_bytes());
            let initial = mem.clone();

            let mut split = Cpu386::default();
            seam_seed(&mut split);
            let mut sbus = TestBus::with_memory(mem);
            let _ = split.cycle(&mut sbus);

            assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
            assert_eq!(
                split.registers.eflags, g.eflags,
                "eflags mismatch for {}",
                g.name
            );
            assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
            let deltas: Vec<(usize, u8)> = sbus
                .memory
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != initial[*i])
                .map(|(i, b)| (i, *b))
                .collect();
            assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
            assert_eq!(
                seam_fetch_count(&sbus),
                g.fetch,
                "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
                g.name
            );
        }
    }

    /// Regenerate the `alu_golden_cases` literals from the PRIOR fused reference. Ignored by
    /// default (it only prints; it asserts nothing). This is the copy-paste template for every
    /// future group-conversion task: drive each case through `execute_instruction_legacy` (the
    /// fused path) and print a ready-to-paste golden literal, so the goldens come from the
    /// reference implementation rather than from the split path they guard (which would be
    /// tautological).
    ///
    /// Run it WHILE the group's fused arm still exists:
    ///   cargo test -p izarravm-cpu --lib regen_alu_goldens -- --ignored --nocapture
    /// For the ALU group specifically the fused arm is already deleted on this branch, so this must
    /// be run from the pre-split base commit (332be72) — see the recipe on `alu_golden_cases`. A
    /// case whose opcode the current fused path can no longer execute prints a TODO marker instead
    /// of a wrong literal, so a stale run can never silently bake bad goldens.
    ///
    /// The printed `code` bytes are decimal (e.g. `&[1, 216]`); that compiles identically to the
    /// hex source form, so paste the numeric result fields and keep your hex encoding if preferred.
    #[test]
    #[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
    fn regen_alu_goldens() {
        for g in alu_golden_cases() {
            let mut mem = vec![0u8; 0x200];
            mem[..g.code.len()].copy_from_slice(g.code);
            mem[0x20..0x22].copy_from_slice(&0x1111u16.to_le_bytes());
            let initial = mem.clone();

            let mut fused = Cpu386::default();
            seam_seed(&mut fused);
            let mut fbus = TestBus::with_memory(mem);
            fused.begin_instruction();
            // The fused reference is the source of truth. If it errors, the group's fused arm is
            // gone from THIS checkout: re-run from the base commit instead of pasting bad output.
            if fused.execute_instruction_legacy(&mut fbus).is_err() {
                println!(
                    "            // TODO regen {}: fused path unavailable here; run from base commit 332be72",
                    g.name
                );
                continue;
            }
            let deltas: Vec<(usize, u8)> = fbus
                .memory
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != initial[*i])
                .map(|(i, b)| (i, *b))
                .collect();
            let fetch = seam_fetch_count(&fbus);
            println!(
                "            AluGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
                g.name,
                g.code,
                fused.registers.gpr,
                fused.registers.eflags,
                fused.registers.eip,
                deltas,
                fetch,
            );
        }
    }

    /// One golden end-state for a data-movement case, captured the same way as `AluGolden`: opcode
    /// bytes plus expected end gpr (AX,CX,DX,BX,SP,BP,SI,DI), eflags, eip, (offset,value) memory
    /// writes, and InstructionPrefetch fetch count. Data-movement ops do not touch flags, so the
    /// eflags field just confirms that (it should equal the seed's `0x02`).
    struct DataMoveGolden {
        name: &'static str,
        code: &'static [u8],
        gpr: [u32; 8],
        eflags: u32,
        eip: u32,
        deltas: &'static [(usize, u8)],
        fetch: usize,
    }

    /// The data-movement differential battery: MOV/LEA/XCHG across forms and addressing modes, plus
    /// the moffs / immediate / Sreg variants, each with its golden end-state. Captured from the
    /// PRIOR fused reference (`execute_instruction_legacy` -> `dispatch_opcode`) via
    /// `regen_datamove_goldens`; see `alu_golden_cases` for the full capture recipe (the goldens
    /// must come from the reference path, never from the split path they guard). The two-byte
    /// MOVZX/MOVSX forms — also in `DecodeGroup::DataMove` — have their own battery
    /// (`movzx_movsx_golden_cases`), so they are absent here.
    fn datamove_golden_cases() -> &'static [DataMoveGolden] {
        &[
            // MOV r/m<->reg, byte and word, register and memory r/m.
            DataMoveGolden {
                name: "mov [bx],cx",
                code: &[137, 15],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x2,
                deltas: &[(16, 4), (17, 3)],
                fetch: 3,
            },
            DataMoveGolden {
                name: "mov [bp+si+4],al(byte)",
                code: &[136, 66, 4],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[(28, 2)],
                fetch: 4,
            },
            DataMoveGolden {
                name: "mov dx,bx(reg)",
                code: &[137, 218],
                gpr: [258, 772, 16, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
            DataMoveGolden {
                name: "mov cx,[0x20]",
                code: &[139, 14, 32, 0],
                gpr: [258, 4369, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x4,
                deltas: &[],
                fetch: 5,
            },
            DataMoveGolden {
                name: "mov al,[bx](byte)",
                code: &[138, 7],
                gpr: [256, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
            // MOV r/m,Sreg and MOV Sreg,r/m (load ES, leaves the addressing segments untouched).
            DataMoveGolden {
                name: "mov [bx],es",
                code: &[140, 7],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
            DataMoveGolden {
                name: "mov es,[0x20]",
                code: &[142, 6, 32, 0],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x4,
                deltas: &[],
                fetch: 5,
            },
            // LEA: effective address into the register, disp+index and direct-disp forms.
            DataMoveGolden {
                name: "lea ax,[bx+si+3]",
                code: &[141, 64, 3],
                gpr: [27, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[],
                fetch: 4,
            },
            DataMoveGolden {
                name: "lea dx,[0x20]",
                code: &[141, 22, 32, 0],
                gpr: [258, 772, 32, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x4,
                deltas: &[],
                fetch: 5,
            },
            // MOV (E)AX<->moffs, byte and word, read and write.
            DataMoveGolden {
                name: "mov al,[moffs8 0x20]",
                code: &[160, 32, 0],
                gpr: [273, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[],
                fetch: 4,
            },
            DataMoveGolden {
                name: "mov ax,[moffs 0x20]",
                code: &[161, 32, 0],
                gpr: [4369, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[],
                fetch: 4,
            },
            DataMoveGolden {
                name: "mov [moffs8 0x30],al",
                code: &[162, 48, 0],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[(48, 2)],
                fetch: 4,
            },
            DataMoveGolden {
                name: "mov [moffs 0x30],ax",
                code: &[163, 48, 0],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[(48, 2), (49, 1)],
                fetch: 4,
            },
            // MOV r,imm (byte and word).
            DataMoveGolden {
                name: "mov bl,0x7f",
                code: &[179, 127],
                gpr: [258, 772, 1286, 127, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
            DataMoveGolden {
                name: "mov si,0x1234",
                code: &[190, 52, 18],
                gpr: [258, 772, 1286, 16, 0, 16, 4660, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[],
                fetch: 4,
            },
            // MOV r/m,imm (group 11), register and memory.
            DataMoveGolden {
                name: "mov byte [bx],0x55",
                code: &[198, 7, 85],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[(16, 85)],
                fetch: 4,
            },
            DataMoveGolden {
                name: "mov word [bx],0xbeef",
                code: &[199, 7, 239, 190],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x4,
                deltas: &[(16, 239), (17, 190)],
                fetch: 5,
            },
            DataMoveGolden {
                name: "mov dx,0xabcd(grp11 reg)",
                code: &[199, 194, 205, 171],
                gpr: [258, 772, 43981, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x4,
                deltas: &[],
                fetch: 5,
            },
            // XCHG r/m,reg (byte and word, register and memory) and XCHG (E)AX,reg + NOP.
            DataMoveGolden {
                name: "xchg [bx],cx",
                code: &[135, 15],
                gpr: [258, 0, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x2,
                deltas: &[(16, 4), (17, 3)],
                fetch: 3,
            },
            DataMoveGolden {
                name: "xchg dl,bl(byte reg)",
                code: &[134, 211],
                gpr: [258, 772, 1296, 6, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
            DataMoveGolden {
                name: "xchg ax,cx",
                code: &[145],
                gpr: [772, 258, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
            DataMoveGolden {
                name: "nop",
                code: &[144],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
        ]
    }

    #[test]
    fn datamove_split_matches_golden_across_ops() {
        // The single-byte data-movement block (MOV/LEA/XCHG and their immediate/moffs/Sreg forms)
        // is converted to the decode/execute split, so it can no longer be diffed against a fused
        // executor (that path was deleted to keep a single implementation). Instead, run each form
        // through cycle() and assert the architectural end-state against goldens captured from the
        // pre-split fused path (see `datamove_golden_cases` for the capture recipe). This exercises
        // decode's ModRM/immediate/moffs parsing, the executor's operand wiring, the EA recompute,
        // and the once-only instruction-fetch charge.
        for g in datamove_golden_cases() {
            let mut mem = vec![0u8; 0x200];
            mem[..g.code.len()].copy_from_slice(g.code);
            mem[0x20..0x22].copy_from_slice(&0x1111u16.to_le_bytes());
            let initial = mem.clone();

            let mut split = Cpu386::default();
            seam_seed(&mut split);
            let mut sbus = TestBus::with_memory(mem);
            let _ = split.cycle(&mut sbus);

            assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
            assert_eq!(
                split.registers.eflags, g.eflags,
                "eflags mismatch for {}",
                g.name
            );
            assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
            let deltas: Vec<(usize, u8)> = sbus
                .memory
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != initial[*i])
                .map(|(i, b)| (i, *b))
                .collect();
            assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
            assert_eq!(
                seam_fetch_count(&sbus),
                g.fetch,
                "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
                g.name
            );
        }
    }

    /// Regenerate the `datamove_golden_cases` literals from the PRIOR fused reference. Ignored by
    /// default (it only prints). Mirror of `regen_alu_goldens`: drive each case through
    /// `execute_instruction_legacy` (the fused path) and print a ready-to-paste literal, so the
    /// goldens come from the reference rather than the split path they guard.
    ///
    /// Run it WHILE the group's fused arms still exist in `dispatch_opcode`:
    ///   cargo test -p izarravm-cpu --lib regen_datamove_goldens -- --ignored --nocapture
    /// then paste the output over `datamove_golden_cases` and only then delete the fused arms.
    #[test]
    #[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
    fn regen_datamove_goldens() {
        for g in datamove_golden_cases() {
            let mut mem = vec![0u8; 0x200];
            mem[..g.code.len()].copy_from_slice(g.code);
            mem[0x20..0x22].copy_from_slice(&0x1111u16.to_le_bytes());
            let initial = mem.clone();

            let mut fused = Cpu386::default();
            seam_seed(&mut fused);
            let mut fbus = TestBus::with_memory(mem);
            fused.begin_instruction();
            if fused.execute_instruction_legacy(&mut fbus).is_err() {
                println!(
                    "            // TODO regen {}: fused path unavailable here; run before deleting the fused arms",
                    g.name
                );
                continue;
            }
            let deltas: Vec<(usize, u8)> = fbus
                .memory
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != initial[*i])
                .map(|(i, b)| (i, *b))
                .collect();
            let fetch = seam_fetch_count(&fbus);
            println!(
                "            DataMoveGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
                g.name,
                g.code,
                fused.registers.gpr,
                fused.registers.eflags,
                fused.registers.eip,
                deltas,
                fetch,
            );
        }
    }

    /// One golden end-state for a MOVZX/MOVSX case run from `movzx_seed` (a real-mode register set
    /// with sentinel bytes/words in memory). The opcode bytes plus expected end gpr, eflags
    /// (MOVZX/MOVSX never touch flags, so this must stay the seed's `0x02`), eip, memory writes
    /// (always empty — these are pure loads), and InstructionPrefetch fetch count. Captured from the
    /// PRIOR fused reference via `regen_movzx_movsx_goldens`; see `alu_golden_cases` for the recipe.
    struct MovzxMovsxGolden {
        name: &'static str,
        code: &'static [u8],
        gpr: [u32; 8],
        eflags: u32,
        eip: u32,
        deltas: &'static [(usize, u8)],
        fetch: usize,
    }

    /// Seed for the MOVZX/MOVSX battery. Same register set as `seam_seed`, but it also plants
    /// sentinels so the byte/word, sign/zero, and EA-recompute cases have stable, sign-bit-set
    /// sources: byte 0x80 at [0x10] (= [BX]), word 0x8081 at [0x18] (= [BX+SI], BX=0x10 + SI=0x08),
    /// and word 0xBEEF at [0x20] (the direct-disp source). The 0x80/0x8081/0xBEEF high bits make
    /// zero- vs sign-extension visibly different.
    fn movzx_seed(cpu: &mut Cpu386, mem: &mut [u8]) {
        seam_seed(cpu);
        mem[0x10] = 0x80;
        mem[0x18..0x1a].copy_from_slice(&0x8081u16.to_le_bytes());
        mem[0x20..0x22].copy_from_slice(&0xBEEFu16.to_le_bytes());
    }

    /// The MOVZX/MOVSX differential battery: 0F B6/B7 (zero-extend byte/word) and 0F BE/BF
    /// (sign-extend byte/word), each in a register form and a memory form, plus an EA-recompute
    /// case ([BX+SI], resolved against the live registers in the executor). Goldens captured from
    /// the fused reference (`execute_instruction_legacy`); never edit by hand — re-run the regen.
    fn movzx_movsx_golden_cases() -> &'static [MovzxMovsxGolden] {
        &[
            // MOVZX r16, r/m8 (0F B6): zero-extend a byte. BL = low byte of BX(0x10) = 0x10.
            MovzxMovsxGolden {
                name: "movzx ax, bl(reg)",
                code: &[15, 182, 195],
                gpr: [16, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[],
                fetch: 4,
            },
            // byte [BX] = [0x10] = 0x80, zero-extended to 0x0080 (= 128).
            MovzxMovsxGolden {
                name: "movzx ax, [bx](byte, sign bit set)",
                code: &[15, 182, 7],
                gpr: [128, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[],
                fetch: 4,
            },
            // MOVZX r16, r/m16 (0F B7): word [0x20] = 0xBEEF, zero-extended (= 48879).
            MovzxMovsxGolden {
                name: "movzx cx, [0x20](word)",
                code: &[15, 183, 14, 32, 0],
                gpr: [258, 48879, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x5,
                deltas: &[],
                fetch: 6,
            },
            // MOVSX r16, r/m8 (0F BE): byte [BX] = 0x80, sign-extended to 0xFF80 (= 65408).
            MovzxMovsxGolden {
                name: "movsx dx, [bx](byte, sign bit set)",
                code: &[15, 190, 23],
                gpr: [258, 772, 65408, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[],
                fetch: 4,
            },
            // DL = low byte of DX(0x0506) = 0x06, positive, sign-extends to 0x0006 (= 6).
            MovzxMovsxGolden {
                name: "movsx ax, dl(reg, positive byte)",
                code: &[15, 190, 194],
                gpr: [6, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[],
                fetch: 4,
            },
            // MOVSX r16, r/m16 (0F BF), EA recomputed from live BX+SI = 0x18; word [0x18] = 0x8081,
            // sign-extended stays 0x8081 at 16 bits (= 32897).
            MovzxMovsxGolden {
                name: "movsx ax, [bx+si](word, sign bit set)",
                code: &[15, 191, 0],
                gpr: [32897, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[],
                fetch: 4,
            },
        ]
    }

    #[test]
    fn movzx_movsx_split_matches_golden() {
        // MOVZX/MOVSX (0F B6/B7/BE/BF) are converted to the split, so they can no longer be diffed
        // against a fused executor (that arm was deleted). Run each through cycle() and assert the
        // architectural end-state against goldens captured from the pre-split fused path. Covers
        // byte and word sources, zero vs sign extend, reg and mem operands, and an EA-recompute
        // case. MOVZX/MOVSX do not modify flags, so eflags must stay the seed value.
        for g in movzx_movsx_golden_cases() {
            let mut mem = vec![0u8; 0x200];
            mem[..g.code.len()].copy_from_slice(g.code);
            let mut split = Cpu386::default();
            movzx_seed(&mut split, &mut mem);
            let initial = mem.clone();
            let mut sbus = TestBus::with_memory(mem);
            split.cycle(&mut sbus).expect("movzx/movsx must execute");

            assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
            assert_eq!(
                split.registers.eflags, g.eflags,
                "eflags mismatch for {} (MOVZX/MOVSX must not touch flags)",
                g.name
            );
            assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
            let deltas: Vec<(usize, u8)> = sbus
                .memory
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != initial[*i])
                .map(|(i, b)| (i, *b))
                .collect();
            assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
            assert_eq!(
                seam_fetch_count(&sbus),
                g.fetch,
                "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
                g.name
            );
        }
    }

    /// Regenerate the `movzx_movsx_golden_cases` literals from the PRIOR fused reference. Ignored by
    /// default. Mirror of `regen_datamove_goldens`; run WHILE the MOVZX/MOVSX arms still exist in
    /// `execute_two_byte`:
    ///   cargo test -p izarravm-cpu --lib regen_movzx_movsx_goldens -- --ignored --nocapture
    /// then paste the output over `movzx_movsx_golden_cases` and only then delete the fused arms.
    #[test]
    #[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
    fn regen_movzx_movsx_goldens() {
        for g in movzx_movsx_golden_cases() {
            let mut mem = vec![0u8; 0x200];
            mem[..g.code.len()].copy_from_slice(g.code);
            let mut fused = Cpu386::default();
            movzx_seed(&mut fused, &mut mem);
            let initial = mem.clone();
            let mut fbus = TestBus::with_memory(mem);
            fused.begin_instruction();
            if fused.execute_instruction_legacy(&mut fbus).is_err() {
                println!(
                    "            // TODO regen {}: fused path unavailable here; run before deleting the fused arms",
                    g.name
                );
                continue;
            }
            let deltas: Vec<(usize, u8)> = fbus
                .memory
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != initial[*i])
                .map(|(i, b)| (i, *b))
                .collect();
            let fetch = seam_fetch_count(&fbus);
            println!(
                "            MovzxMovsxGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
                g.name,
                g.code,
                fused.registers.gpr,
                fused.registers.eflags,
                fused.registers.eip,
                deltas,
                fetch,
            );
        }
    }

    #[test]
    fn group11_mov_rm_imm_with_nonzero_reg_faults_without_consuming_the_immediate() {
        // C6 /1 is an undefined group-11 encoding (only reg=000 is MOV r/m,imm). This is the one
        // data-move path the goldens can't cover: decode DEFERS parsing the operand/immediate when
        // reg != 0 and the executor re-raises the error. Drive it through the split (which returns
        // the raw fault without eip rewind) and assert two things:
        //   1. the fault is UnsupportedGroupOpcode { opcode: 0xc6, extension: 1 }, and
        //   2. eip advanced to exactly 2 (opcode + ModRM) — proving decode did NOT over-consume the
        //      trailing imm8 (0x55) on the fault path, so the bytes charged match the fused handler.
        let (mut cpu, memory) = real_mode_cpu(&[0xc6, 0xc9, 0x55], 0x20);
        let mut bus = TestBus::with_memory(memory);

        let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
        assert!(
            matches!(
                fault,
                InternalFault::Cpu(CpuError::UnsupportedGroupOpcode {
                    opcode: 0xc6,
                    extension: 1
                })
            ),
            "{fault:?}"
        );
        assert_eq!(
            cpu.registers.eip, 2,
            "decode must stop after the ModRM on the fault path (imm8 not consumed)"
        );
    }

    #[test]
    fn decode_then_execute_matches_golden_for_add_rm_reg() {
        // 01 D8 = ADD AX, BX (ALU form 1, op=0, modrm mode=3 rm=0 reg=3). The decode +
        // execute_decoded path must produce the architectural ADD result. (Once the ALU block was
        // fully converted to the split, the former fused executor was deleted, so this asserts the
        // known-correct end-state directly rather than diffing against a removed reference path.)
        let code = [0x01, 0xd8];

        let (mut split, mem) = real_mode_cpu(&code, 0x10);
        split.write_reg16(Reg16::Ax, 0x1234);
        split.write_reg16(Reg16::Bx, 0x1111);
        let mut split_bus = TestBus::with_memory(mem);
        split.begin_instruction();
        let insn = split.decode(&mut split_bus).unwrap();
        assert_eq!(insn.opcode, 0x01);
        assert_eq!(insn.operand, Some(DecodedOperand::Reg(0))); // r/m = AX
        let split_outcome = split.execute_decoded(&insn, &mut split_bus).unwrap();

        // 0x1234 + 0x1111 = 0x2345: no carry/zero/sign/overflow/aux, low byte 0x45 has odd parity
        // (PF clear), so only the always-set reserved bit 1 remains.
        assert_eq!(split.read_reg16(Reg16::Ax), 0x2345);
        assert_eq!(split.read_reg16(Reg16::Bx), 0x1111); // source untouched
        assert_eq!(split.registers.eflags, 0x02);
        assert_eq!(split.registers.eip, 0x02);
        assert_eq!(split_outcome.core_clocks, 2);
    }

    #[test]
    fn decoded_add_rm_reg_recomputes_ea_from_live_registers() {
        // 01 07 = ADD [BX], AX (modrm mode=0 rm=7 -> [BX]). Decode once, then change BX before
        // executing: the addressing-mode descriptor must resolve against the *new* BX, proving
        // the decoded form stores a descriptor and not a baked-in offset.
        let code = [0x01, 0x07];
        let (mut cpu, mut mem) = real_mode_cpu(&code, 0x40);
        // Seed both candidate target words.
        mem[0x20..0x22].copy_from_slice(&0x0001u16.to_le_bytes());
        mem[0x30..0x32].copy_from_slice(&0x0001u16.to_le_bytes());
        let mut bus = TestBus::with_memory(mem);
        cpu.write_reg16(Reg16::Ax, 0x0010);
        cpu.write_reg16(Reg16::Bx, 0x0020);

        cpu.begin_instruction();
        let insn = cpu.decode(&mut bus).unwrap();
        // The descriptor must name BX (register 3) as its base, not a resolved offset.
        match insn.operand {
            Some(DecodedOperand::Mem(addr)) => {
                assert_eq!(addr.base, Some(3));
                assert_eq!(addr.index, None);
                assert_eq!(addr.disp, 0);
            }
            other => panic!("expected a memory operand, got {other:?}"),
        }

        // Move the pointer before executing.
        cpu.write_reg16(Reg16::Bx, 0x0030);
        cpu.execute_decoded(&insn, &mut bus).unwrap();

        assert_eq!(bus.memory[0x20], 0x01, "old target must be untouched");
        assert_eq!(bus.memory[0x30], 0x11, "new target (BX=0x30) gets AX added");
    }

    #[test]
    fn alu_split_recomputes_effective_address() {
        // 00 07 = ADD [BX], AL (ALU form 0, op=0). Decode once, then execute against two different
        // BX values: each execution must resolve [BX] against the *current* BX and update the byte
        // there, proving the generalized ALU split recomputes the effective address every run.
        let code = [0x00, 0x07];
        let (mut cpu, mut mem) = real_mode_cpu(&code, 0x60);
        mem[0x40] = 0x01;
        mem[0x50] = 0x02;
        let mut bus = TestBus::with_memory(mem);
        cpu.write_reg16(Reg16::Ax, 0x0010); // AL = 0x10, AH = 0

        cpu.begin_instruction();
        let insn = cpu.decode(&mut bus).unwrap();

        // First run with BX = 0x40: the byte at [0x40] gains AL.
        cpu.write_reg16(Reg16::Bx, 0x0040);
        cpu.execute_decoded(&insn, &mut bus).unwrap();
        assert_eq!(bus.memory[0x40], 0x11, "[BX=0x40] must get AL added");
        assert_eq!(bus.memory[0x50], 0x02, "[0x50] untouched on the first run");

        // Re-execute the SAME decoded instruction with BX = 0x50: the EA must follow BX.
        cpu.write_reg16(Reg16::Bx, 0x0050);
        cpu.execute_decoded(&insn, &mut bus).unwrap();
        assert_eq!(bus.memory[0x40], 0x11, "[0x40] untouched on the second run");
        assert_eq!(bus.memory[0x50], 0x12, "[BX=0x50] must get AL added");
    }

    #[test]
    fn self_modified_opcode_beyond_prefetch_window_is_seen() {
        let mut code = vec![0x90; 0x40]; // nop sled
        code[0..5].copy_from_slice(&[0xc6, 0x06, 0x21, 0x00, 0xf4]); // mov byte [0021h],hlt
        code[0x21] = 0x90; // replaced before execution reaches it
        code[0x22..0x24].copy_from_slice(&[0xeb, 0xfe]); // stale path would loop here
        let (mut cpu, memory) = real_mode_cpu(&code, 0x40);
        let mut bus = TestBus::with_memory(memory);

        let mut halted = false;
        for _ in 0..40 {
            halted = cpu.cycle(&mut bus).unwrap().halted;
            if halted {
                break;
            }
        }

        assert!(halted, "modified HLT at 0021h must execute");
        assert_eq!(cpu.registers.eip, 0x22);
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
        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
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
        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
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
        let fault = cpu.execute_instruction_legacy(&mut bus).unwrap_err();
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

    // ---- Phase 4 slice E: virtual-8086 mode ----

    #[test]
    fn v86_segment_load_uses_real_mode_base() {
        // MOV DS, AX (8E D8) in a V86 task: DS base = selector << 4.
        let (mut cpu, memory) = real_mode_cpu(&[0x8e, 0xd8], 0x40);
        cpu.control.cr0 |= CR0_PE;
        cpu.registers.eflags |= FLAG_VM;
        cpu.write_reg16(Reg16::Ax, 0x1234);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.registers.segment(SegmentIndex::Ds).base, 0x1_2340);
        assert_eq!(cpu.registers.segment(SegmentIndex::Ds).selector, 0x1234);
    }

    #[test]
    fn cli_faults_in_v86_below_iopl3() {
        // CLI (0xFA) in a V86 task with IOPL 0 traps to the monitor with #GP(0).
        // CLI is converted to DecodeGroup::FlagsMisc, so drive it through the split (exec_one_split)
        // rather than execute_instruction_legacy, which no longer carries the 0xFA arm.
        let (mut cpu, memory) = real_mode_cpu(&[0xfa], 0x40);
        cpu.control.cr0 |= CR0_PE;
        cpu.registers.eflags = 0x2 | FLAG_VM; // IOPL 0
        let mut bus = TestBus::with_memory(memory);
        let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
        assert!(matches!(fault, InternalFault::Exception { vector: 13, .. }));
    }

    #[test]
    fn cli_runs_in_v86_at_iopl3() {
        // With IOPL 3 the V86 task may touch IF directly.
        let (mut cpu, memory) = real_mode_cpu(&[0xfa], 0x40);
        cpu.control.cr0 |= CR0_PE;
        cpu.registers.eflags = 0x2 | FLAG_VM | 0x3000 | FLAG_IF; // IOPL 3, IF set
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert!(!cpu.flag(FLAG_IF), "CLI cleared IF");
    }

    // ---- Stack-group golden battery (A4) ----

    /// One golden end-state for a stack-group case, captured from the fused reference
    /// (`execute_instruction_legacy`) via `regen_stack_goldens`. Stack ops mutate SS:SP and stack
    /// memory, so this captures the full register file (incl. ESP/EBP), eflags (PUSHF/POPF/POPA
    /// touch flags), eip, memory-write deltas, and the InstructionPrefetch fetch count.
    struct StackGolden {
        name: &'static str,
        code: &'static [u8],
        gpr: [u32; 8],
        eflags: u32,
        eip: u32,
        deltas: &'static [(usize, u8)],
        fetch: usize,
    }

    /// Seed for the stack golden battery. Uses a 512-byte memory image with a stack at
    /// 0x1f0 (grows down into the low half) and known register values for non-stack GPRs.
    /// The instruction is placed at offset 0; the stack region starts at 0x1f0.
    fn stack_seed(cpu: &mut Cpu386) {
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        // AX=0x0102, CX=0x0304, DX=0x0506, BX=0x0708 (non-zero non-trivial values)
        cpu.write_reg16(Reg16::Ax, 0x0102);
        cpu.write_reg16(Reg16::Cx, 0x0304);
        cpu.write_reg16(Reg16::Dx, 0x0506);
        cpu.write_reg16(Reg16::Bx, 0x0708);
        // SP=0x01f0, BP=0x01f0 (frame-pointer tests start at the same level)
        cpu.write_reg16(Reg16::Sp, 0x01f0);
        cpu.write_reg16(Reg16::Bp, 0x01f0);
        cpu.write_reg16(Reg16::Si, 0x0008);
        cpu.write_reg16(Reg16::Di, 0x0018);
        // eflags: only the always-set reserved bit 1 (PUSHF/POPF tests perturb CF below)
        cpu.registers.eflags = 0x02;
    }

    /// The stack-group differential battery. Captured from the PRIOR fused reference
    /// (`execute_instruction_legacy`) via `regen_stack_goldens`; see `alu_golden_cases` for the
    /// full capture recipe. Never edit by hand — re-run the regen from the pre-split commit.
    fn stack_golden_cases() -> &'static [StackGolden] {
        &[
            // PUSH reg (0x50-0x57): SP decrements by 2, then value written at ss:SP.
            // Initial SP=0x1f0 so push target is 0x1ee (= 494). The initial 0xBEEF at 0x1f0
            // is unaffected by pushes (they go to 0x1ee = 494).
            StackGolden {
                name: "push ax",
                code: &[80],
                gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[(494, 2), (495, 1)],
                fetch: 2,
            },
            StackGolden {
                name: "push bx",
                code: &[83],
                gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[(494, 8), (495, 7)],
                fetch: 2,
            },
            StackGolden {
                name: "push cx",
                code: &[81],
                gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[(494, 4), (495, 3)],
                fetch: 2,
            },
            StackGolden {
                name: "push si",
                code: &[86],
                gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[(494, 8)],
                fetch: 2,
            },
            // POP reg (0x58-0x5f): reads from SS:SP=0x1f0 (BEEF planted there), SP += 2.
            StackGolden {
                name: "pop ax",
                code: &[88],
                gpr: [48879, 772, 1286, 1800, 498, 496, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
            StackGolden {
                name: "pop bx",
                code: &[91],
                gpr: [258, 772, 1286, 48879, 498, 496, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
            // PUSH seg (0x06/0x0e/0x16/0x1e): push ES/CS/SS/DS selectors. All are 0 from
            // stack_seed, so no bytes change from initial (they write 0x0000 over 0x0000).
            StackGolden {
                name: "push es",
                code: &[6],
                gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
            StackGolden {
                name: "push cs",
                code: &[14],
                gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
            StackGolden {
                name: "push ss",
                code: &[22],
                gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
            StackGolden {
                name: "push ds",
                code: &[30],
                gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
            // POP seg (0x07/0x17/0x1f): pops 0xBEEF from stack into ES/SS/DS. No gpr delta
            // (segment selectors are not in `gpr`); SP advances.
            StackGolden {
                name: "pop es",
                code: &[7],
                gpr: [258, 772, 1286, 1800, 498, 496, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
            StackGolden {
                name: "pop ss",
                code: &[23],
                gpr: [258, 772, 1286, 1800, 498, 496, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
            StackGolden {
                name: "pop ds",
                code: &[31],
                gpr: [258, 772, 1286, 1800, 498, 496, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
            // PUSH imm16 (0x68): push 0x1234 to ss:0x1ee.
            StackGolden {
                name: "push imm16 0x1234",
                code: &[104, 52, 18],
                gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[(494, 52), (495, 18)],
                fetch: 4,
            },
            // PUSH imm8 +5 (0x6a 0x05): sign-extended to 0x0005; high byte 0x00 over 0x00 = no delta.
            StackGolden {
                name: "push imm8 +5",
                code: &[106, 5],
                gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
                eflags: 0x2,
                eip: 0x2,
                deltas: &[(494, 5)],
                fetch: 3,
            },
            // PUSH imm8 -1 (0x6a 0xff): sign-extended to 0xffff; both bytes 0xff change.
            StackGolden {
                name: "push imm8 -1",
                code: &[106, 255],
                gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
                eflags: 0x2,
                eip: 0x2,
                deltas: &[(494, 255), (495, 255)],
                fetch: 3,
            },
            // POP r/m (0x8f /0) memory form: 8F 06 10 01 = POP word [0x0110]. Pops 0xBEEF from
            // ss:0x1f0, writes to ds:0x0110 (= offset 272 dec). SP advances to 0x1f2 (= 498).
            StackGolden {
                name: "pop r/m mem [0x0110]",
                code: &[143, 6, 16, 1],
                gpr: [258, 772, 1286, 1800, 498, 496, 8, 24],
                eflags: 0x2,
                eip: 0x4,
                deltas: &[(272, 239), (273, 190)],
                fetch: 5,
            },
            // POP r/m register form: 8F /0 mod=11 rm=000 -> POP AX. AX gets 0xBEEF.
            StackGolden {
                name: "pop r/m reg ax",
                code: &[143, 192],
                gpr: [48879, 772, 1286, 1800, 498, 496, 8, 24],
                eflags: 0x2,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
            // PUSHA (0x60): snapshot SP=0x1f0 before pushing 8 words. Pushes AX,CX,DX,BX,
            // snapshot-SP,BP,SI,DI. SP ends at 0x1e0 (= 480). The BEEF word at 0x1f0 is
            // overwritten by the SP-snapshot push (0x1ee-0x1ef <- 0x1f0 LE).
            StackGolden {
                name: "pusha",
                code: &[96],
                gpr: [258, 772, 1286, 1800, 480, 496, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[
                    (480, 24),
                    (482, 8),
                    (484, 240),
                    (485, 1),
                    (486, 240),
                    (487, 1),
                    (488, 8),
                    (489, 7),
                    (490, 6),
                    (491, 5),
                    (492, 4),
                    (493, 3),
                    (494, 2),
                    (495, 1),
                ],
                fetch: 2,
            },
            // POPA (0x61): pops DI,SI,BP,discard,BX,DX,CX,AX from SP=0x1f0. DI gets 0xBEEF
            // (it's the first pop at 0x1f0). All others pop 0x00. SP ends at 0x200 (= 512).
            StackGolden {
                name: "popa",
                code: &[97],
                gpr: [0, 0, 0, 0, 512, 0, 0, 48879],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
            // PUSHF (0x9c): push eflags (0x0002) to ss:0x1ee. High byte 0x00 over 0x00 = no delta.
            StackGolden {
                name: "pushf",
                code: &[156],
                gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[(494, 2)],
                fetch: 2,
            },
            // POPF (0x9d): pops 0x0097 from ss:0x1f0 (overridden from BEEF in the test loop).
            // CF+PF+AF+ZF+SF all set. SP advances to 0x1f2 (= 498).
            StackGolden {
                name: "popf",
                code: &[157],
                gpr: [258, 772, 1286, 1800, 498, 496, 8, 24],
                eflags: 0x97,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
            // ENTER imm16=4, imm8=1 (nesting level 1): push BP (0x01f0), copy frame ptr, set
            // BP = pre-push SP - 2, then SP -= alloc (4). Stack frame consumes 4 bytes (2 for
            // saved BP, 2 for the display copy). SP ends at 0x1e8 (= 488); BP=0x1ee (= 494).
            StackGolden {
                name: "enter 4,1",
                code: &[200, 4, 0, 1],
                gpr: [258, 772, 1286, 1800, 488, 494, 8, 24],
                eflags: 0x2,
                eip: 0x4,
                deltas: &[(492, 238), (493, 1), (494, 240), (495, 1)],
                fetch: 5,
            },
            // LEAVE (0xc9): SP <- BP = 0x1f0, then pop BP from ss:0x1f0 (BEEF). BP = 0xBEEF,
            // SP = 0x1f2 (= 498).
            StackGolden {
                name: "leave",
                code: &[201],
                gpr: [258, 772, 1286, 1800, 498, 48879, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
        ]
    }

    /// Seed memory for the stack golden battery. Plants 0xBEEF at SS:SP=0x1f0 (the first
    /// word a POP reads) so POP tests have a stable, visible source. Each case gets a fresh
    /// 0x200-byte vector so earlier writes don't bleed into later cases. The POPF case
    /// overwrites this with 0x0097 in the regen/assert loops to give CF+PF+AF+ZF+SF.
    fn stack_seed_mem(mem: &mut [u8], code: &[u8]) {
        mem[..code.len()].copy_from_slice(code);
        // POP tests: plant 0xBEEF at ss:0x1f0 (the initial SP — the first word a POP reads).
        mem[0x1f0..0x1f2].copy_from_slice(&0xbeefu16.to_le_bytes());
    }

    #[test]
    fn stack_split_matches_golden_across_ops() {
        // The stack-group opcodes (PUSH/POP reg/seg/imm, PUSHA/POPA, PUSHF/POPF, ENTER/LEAVE,
        // POP r/m) are converted to the decode/execute split, so they can no longer be diffed
        // against a fused executor (that path was deleted). Run each through cycle() and assert
        // the architectural end-state against goldens captured from the pre-split fused path via
        // `regen_stack_goldens`. Covers register and memory operands, SP semantics, flag
        // masking (PUSHF/POPF), the PUSHA SP-snapshot, and the ENTER nesting frame-copy.
        for g in stack_golden_cases() {
            let mut mem = vec![0u8; 0x200];
            stack_seed_mem(&mut mem, g.code);
            // POPF needs a known flags word at SS:SP (0x1f0) instead of BEEF.
            if g.name == "popf" {
                mem[0x1f0..0x1f2].copy_from_slice(&0x0097u16.to_le_bytes());
            }
            let initial = mem.clone();

            let mut split = Cpu386::default();
            stack_seed(&mut split);
            let mut sbus = TestBus::with_memory(mem);
            let _ = split.cycle(&mut sbus);

            assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
            assert_eq!(
                split.registers.eflags, g.eflags,
                "eflags mismatch for {}",
                g.name
            );
            assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
            let deltas: Vec<(usize, u8)> = sbus
                .memory
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != initial[*i])
                .map(|(i, b)| (i, *b))
                .collect();
            assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
            assert_eq!(
                seam_fetch_count(&sbus),
                g.fetch,
                "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
                g.name
            );
        }
    }

    /// Regenerate `stack_golden_cases` from the fused reference. Ignored by default.
    /// Run WHILE the stack group's fused arms still exist in `dispatch_opcode`:
    ///   cargo test -p izarravm-cpu --lib regen_stack_goldens -- --ignored --nocapture
    /// then paste the output over `stack_golden_cases` and only then do the conversion.
    #[test]
    #[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
    fn regen_stack_goldens() {
        for g in stack_golden_cases() {
            let mut mem = vec![0u8; 0x200];
            stack_seed_mem(&mut mem, g.code);
            if g.name == "popf" {
                mem[0x1f0..0x1f2].copy_from_slice(&0x0097u16.to_le_bytes());
            }
            let initial = mem.clone();

            let mut fused = Cpu386::default();
            stack_seed(&mut fused);
            let mut fbus = TestBus::with_memory(mem);
            fused.begin_instruction();
            if fused.execute_instruction_legacy(&mut fbus).is_err() {
                println!(
                    "            // TODO regen {}: fused path unavailable here; run before deleting fused arms",
                    g.name
                );
                continue;
            }
            let deltas: Vec<(usize, u8)> = fbus
                .memory
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != initial[*i])
                .map(|(i, b)| (i, *b))
                .collect();
            let fetch = seam_fetch_count(&fbus);
            println!(
                "            StackGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
                g.name,
                g.code,
                fused.registers.gpr,
                fused.registers.eflags,
                fused.registers.eip,
                deltas,
                fetch,
            );
        }
    }

    /// One golden end-state for an arithmetic /ext group case (groups 1-4), captured the same way
    /// as the other group goldens: opcode bytes plus expected end gpr (AX,CX,DX,BX,SP,BP,SI,DI),
    /// eflags, eip, (offset,value) memory writes, and InstructionPrefetch fetch count. Groups 1-3
    /// touch flags (ALU/shift/TEST/NEG/MUL/DIV), so eflags is load-bearing; group 4 (INC/DEC) must
    /// leave CF untouched, which the CF-preserving seed makes visible in eflags.
    struct GroupGolden {
        name: &'static str,
        code: &'static [u8],
        gpr: [u32; 8],
        eflags: u32,
        eip: u32,
        deltas: &'static [(usize, u8)],
        fetch: usize,
    }

    /// Seed for the group golden battery: the same register file as `seam_seed` plus CL=4 (a small
    /// shift count) and CF pre-set in eflags so the INC/DEC CF-preservation is observable. CX is
    /// 0x0304 so CL = 0x04.
    fn group_seed(cpu: &mut Cpu386) {
        seam_seed(cpu);
        // Pre-set CF (bit 0) on top of the always-set reserved bit 1. This makes the group 4
        // INC/DEC CF-preservation visible (CF must still be set after) and feeds ADC/SBB/RCR.
        cpu.registers.eflags = 0x03;
    }

    /// Seed memory for the group battery: plant 0x3412 at [bx] = ds:0x10 (the r/m memory target),
    /// so byte [0x10] = 0x12 and word [0x10] = 0x3412. Fresh image per case so writes don't bleed.
    fn group_seed_mem(mem: &mut [u8], code: &[u8]) {
        mem[..code.len()].copy_from_slice(code);
        mem[0x10..0x12].copy_from_slice(&0x3412u16.to_le_bytes());
    }

    /// The arithmetic /ext group (1-4) differential battery. Captured from the PRIOR fused reference
    /// (`execute_instruction_legacy`) via `regen_group_goldens`; see `alu_golden_cases` for the full
    /// capture recipe. Never edit by hand — re-run the regen WHILE the fused arms still exist in
    /// `dispatch_opcode`, then paste, then delete the fused arms. Covers: group 1 ALU r/m,imm
    /// (byte/word, CMP no-writeback, 0x83 sign-extend), group 2 shift/rotate (SHL/SHR/SAR/ROL/RCR
    /// with count 1/CL/imm8), group 3 TEST-with-imm/NOT/NEG/MUL/IMUL and a non-faulting DIV, and
    /// group 4 INC/DEC (CF preserved). The DIV-by-zero #DE fault is a separate test (goldens only
    /// capture success).
    fn group_golden_cases() -> &'static [GroupGolden] {
        &[
            // Group 1: ALU r/m, imm (0x80/0x81/0x82/0x83). Includes CMP no-writeback and 0x83
            // sign-extend (both byte/word and a register form).
            GroupGolden {
                name: "add byte [bx],0x05 (80 /0)",
                code: &[128, 7, 5],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x6,
                eip: 0x3,
                deltas: &[(16, 23)],
                fetch: 4,
            },
            GroupGolden {
                name: "or byte [bx],0xf0 (80 /1)",
                code: &[128, 15, 240],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x82,
                eip: 0x3,
                deltas: &[(16, 242)],
                fetch: 4,
            },
            GroupGolden {
                name: "cmp byte [bx],0x12 (80 /7 no writeback)",
                code: &[128, 63, 18],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x46,
                eip: 0x3,
                deltas: &[],
                fetch: 4,
            },
            GroupGolden {
                name: "add word [bx],0x1234 (81 /0)",
                code: &[129, 7, 52, 18],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x4,
                deltas: &[(16, 70), (17, 70)],
                fetch: 5,
            },
            GroupGolden {
                name: "cmp word [bx],0x3412 (81 /7 no writeback)",
                code: &[129, 63, 18, 52],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x46,
                eip: 0x4,
                deltas: &[],
                fetch: 5,
            },
            GroupGolden {
                name: "add word [bx],-2 (83 /0 sign-extend)",
                code: &[131, 7, 254],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x13,
                eip: 0x3,
                deltas: &[(16, 16)],
                fetch: 4,
            },
            GroupGolden {
                name: "sub ax,-1 (83 /5 sign-extend reg)",
                code: &[131, 232, 255],
                gpr: [259, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x17,
                eip: 0x3,
                deltas: &[],
                fetch: 4,
            },
            // Group 2: shift/rotate (0xc0/0xc1/0xd0-0xd3). Flags load-bearing; count 1/CL/imm8.
            GroupGolden {
                name: "shl byte [bx],1 (d0 /4)",
                code: &[208, 39],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x6,
                eip: 0x2,
                deltas: &[(16, 36)],
                fetch: 3,
            },
            GroupGolden {
                name: "shr word [bx],1 (d1 /5)",
                code: &[209, 47],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x6,
                eip: 0x2,
                deltas: &[(16, 9), (17, 26)],
                fetch: 3,
            },
            GroupGolden {
                name: "shl ax,1 (d1 /4 reg)",
                code: &[209, 224],
                gpr: [516, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
            GroupGolden {
                name: "rol byte [bx],cl (d2 /0)",
                code: &[210, 7],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x3,
                eip: 0x2,
                deltas: &[(16, 33)],
                fetch: 3,
            },
            GroupGolden {
                name: "sar word [bx],cl (d3 /7)",
                code: &[211, 63],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x6,
                eip: 0x2,
                deltas: &[(16, 65), (17, 3)],
                fetch: 3,
            },
            GroupGolden {
                name: "rcr word [bx],3 (c1 /3 imm8)",
                code: &[193, 31, 3],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[(16, 130), (17, 166)],
                fetch: 4,
            },
            GroupGolden {
                name: "shl ax,4 (c1 /4 imm8 reg)",
                code: &[193, 224, 4],
                gpr: [4128, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[],
                fetch: 4,
            },
            // Group 3: F6/F7 (TEST-with-imm/NOT/NEG/MUL/IMUL/DIV). DIV here is non-faulting; the
            // DIV-by-zero #DE is covered by `group_div_by_zero_raises_de_through_the_split`.
            GroupGolden {
                name: "test byte [bx],0x0f (f6 /0 imm)",
                code: &[246, 7, 15],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x3,
                deltas: &[],
                fetch: 4,
            },
            GroupGolden {
                name: "test ax,0x00ff (f7 /0 imm reg)",
                code: &[247, 192, 255, 0],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x4,
                deltas: &[],
                fetch: 5,
            },
            GroupGolden {
                name: "not word [bx] (f7 /2)",
                code: &[247, 23],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x3,
                eip: 0x2,
                deltas: &[(16, 237), (17, 203)],
                fetch: 3,
            },
            GroupGolden {
                name: "neg ax (f7 /3 reg)",
                code: &[247, 216],
                gpr: [65278, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x93,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
            GroupGolden {
                name: "mul bl (f6 /4 reg)",
                code: &[246, 227],
                gpr: [32, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
            GroupGolden {
                name: "imul cx (f7 /5 reg)",
                code: &[247, 233],
                gpr: [2568, 772, 3, 16, 0, 16, 8, 24],
                eflags: 0x803,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
            GroupGolden {
                name: "div bl (f6 /6 reg, non-faulting)",
                code: &[246, 243],
                gpr: [528, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x3,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
            // Group 4: INC/DEC byte (0xfe). CF must be preserved (the seed pre-sets CF; both end
            // states keep bit 0 set).
            GroupGolden {
                name: "inc byte [bx] (fe /0, CF preserved)",
                code: &[254, 7],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x3,
                eip: 0x2,
                deltas: &[(16, 19)],
                fetch: 3,
            },
            GroupGolden {
                name: "dec byte [bx] (fe /1, CF preserved)",
                code: &[254, 15],
                gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x7,
                eip: 0x2,
                deltas: &[(16, 17)],
                fetch: 3,
            },
        ]
    }

    #[test]
    fn group_split_matches_golden_across_ops() {
        // The arithmetic /ext groups 1-4 (ALU r/m,imm; shift/rotate; TEST/NOT/NEG/MUL/IMUL/DIV/IDIV;
        // INC/DEC) are converted to the decode/execute split, so they can no longer be diffed
        // against a fused executor (those arms were deleted). Run each through cycle() and assert
        // the architectural end-state against goldens captured from the pre-split fused path via
        // `regen_group_goldens`. Exercises decode's ModRM/addressing parse, the conditional F6/F7
        // immediate, the executor's sub-op dispatch + write-back gating (CMP/TEST flags-only), the
        // reused shift/mul/div flag logic, CF preservation on INC/DEC, and the once-only fetch
        // charge. The DIV-by-zero #DE fault is covered separately (goldens capture success only).
        for g in group_golden_cases() {
            let mut mem = vec![0u8; 0x200];
            group_seed_mem(&mut mem, g.code);
            let initial = mem.clone();

            let mut split = Cpu386::default();
            group_seed(&mut split);
            let mut sbus = TestBus::with_memory(mem);
            let _ = split.cycle(&mut sbus);

            assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
            assert_eq!(
                split.registers.eflags, g.eflags,
                "eflags mismatch for {}",
                g.name
            );
            assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
            let deltas: Vec<(usize, u8)> = sbus
                .memory
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != initial[*i])
                .map(|(i, b)| (i, *b))
                .collect();
            assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
            assert_eq!(
                seam_fetch_count(&sbus),
                g.fetch,
                "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
                g.name
            );
        }
    }

    /// Regenerate `group_golden_cases` from the fused reference. Ignored by default.
    /// Run WHILE the group's fused arms (0x80-0x83, 0xc0/0xc1/0xd0-0xd3, 0xf6/0xf7, 0xfe) still
    /// exist in `dispatch_opcode`:
    ///   cargo test -p izarravm-cpu --lib regen_group_goldens -- --ignored --nocapture
    /// then paste the output over `group_golden_cases` and only then delete the fused arms.
    #[test]
    #[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
    fn regen_group_goldens() {
        for g in group_golden_cases() {
            let mut mem = vec![0u8; 0x200];
            group_seed_mem(&mut mem, g.code);
            let initial = mem.clone();

            let mut fused = Cpu386::default();
            group_seed(&mut fused);
            let mut fbus = TestBus::with_memory(mem);
            fused.begin_instruction();
            if fused.execute_instruction_legacy(&mut fbus).is_err() {
                println!(
                    "            // TODO regen {}: fused path unavailable here; run before deleting fused arms",
                    g.name
                );
                continue;
            }
            let deltas: Vec<(usize, u8)> = fbus
                .memory
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != initial[*i])
                .map(|(i, b)| (i, *b))
                .collect();
            let fetch = seam_fetch_count(&fbus);
            println!(
                "            GroupGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
                g.name,
                g.code,
                fused.registers.gpr,
                fused.registers.eflags,
                fused.registers.eip,
                deltas,
                fetch,
            );
        }
    }

    #[test]
    fn group_div_by_zero_raises_de_through_the_split() {
        // The DIV-by-zero #DE fault path (goldens capture success only, so it needs an explicit
        // test). `div bl` (F6 /6, mod=11 rm=011) with BL = 0 must raise the divide error. The
        // group 3 fused arm is deleted on this branch, so this drives the decode/execute split
        // (exec_one_split) directly and asserts the fault is CpuError::DivideError. The `div`
        // helper checks divide-by-zero BEFORE any register write, and `decode` consumes exactly
        // the F6 + ModRM bytes (no immediate for /6), so we also assert eip advanced by 2. The
        // InstructionPrefetch count (3, one read-ahead past the 2-byte op — see the non-faulting
        // `div bl` golden, which also reports 3) confirms decode charged the fetch and the
        // executor faulted with no extra fetch.
        let code = [0xf6, 0xf3]; // div bl
        let mut mem = vec![0u8; 0x40];
        mem[..code.len()].copy_from_slice(&code);

        let mut split = Cpu386::default();
        split.load_segment_real(SegmentIndex::Cs, 0);
        split.load_segment_real(SegmentIndex::Ds, 0);
        split.registers.eip = 0;
        split.write_reg16(Reg16::Ax, 0x0102);
        split.write_reg16(Reg16::Bx, 0x0700); // BL = 0 -> divide by zero
        let mut sbus = TestBus::with_memory(mem);
        let split_err = exec_one_split(&mut split, &mut sbus).unwrap_err();

        assert!(
            matches!(split_err, InternalFault::Cpu(CpuError::DivideError)),
            "split DIV-by-zero must raise CpuError::DivideError, got {split_err:?}"
        );
        // AX must be untouched: the #DE is raised before any quotient/remainder write-back.
        assert_eq!(
            split.read_reg16(Reg16::Ax),
            0x0102,
            "AX must be unchanged when DIV faults before write-back"
        );
        assert_eq!(
            split.registers.eip, 2,
            "decode must consume the F6 + ModRM bytes (no immediate for /6) before the #DE"
        );
        assert_eq!(
            seam_fetch_count(&sbus),
            3,
            "the split must charge the same fetches as the non-faulting div bl golden (3)"
        );
    }

    /// One golden end-state for a relative/loop branch case (task A6a). Adds `cx` to the shared
    /// golden shape so a single battery can drive both the taken and not-taken LOOP/JCXZ/LOOPcc
    /// outcomes (which differ only in the post-decrement count) from one seed — `branch_seed`
    /// overwrites CX with this per-case value before the instruction runs. The captured fields are
    /// the standard set: end gpr (AX,CX,DX,BX,SP,BP,SI,DI), eflags, eip (the branch target — the key
    /// assertion for this group), (offset,value) memory writes (CALL's pushed return address), and
    /// the InstructionPrefetch fetch count.
    struct BranchGolden {
        name: &'static str,
        code: &'static [u8],
        cx: u32,
        gpr: [u32; 8],
        eflags: u32,
        eip: u32,
        deltas: &'static [(usize, u8)],
        fetch: usize,
    }

    /// Seed for the branch golden battery. CS/DS/SS = 0, eip = 0, SP = 0x100 (a safe in-image stack
    /// so CALL's push lands in the 0x200-byte image), ZF pre-set (so the Jcc/LOOPcc condition cases
    /// are deterministic), and CX set per case (the caller overwrites it from `BranchGolden::cx`).
    fn branch_seed(cpu: &mut Cpu386, cx: u32) {
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Sp, 0x0100);
        cpu.set_flag(FLAG_ZF, true);
        cpu.registers.set_ecx(cx);
    }

    /// The relative/loop branch differential battery. Captured from the PRIOR fused reference via
    /// `regen_branch_goldens`; see `alu_golden_cases` for the full capture recipe. The branch group's
    /// fused arms (0x70-0x7f, 0xe0-0xe3, 0xe8/0xe9/0xeb, 0F 80-0F 8F) are already deleted on
    /// `perf-decode-cache`, so these were captured from the pre-split base commit (a94ed279): check
    /// it out, run the regen, paste, return. Never hand-edit a golden — re-capture from the reference.
    /// Covers: Jcc short taken/not-taken (JZ/JNZ with ZF set), Jcc near (two-byte) taken/not-taken,
    /// JMP short, JMP near, CALL near (the pushed return address + SP delta), LOOP taken (CX
    /// decremented, nonzero) and not-taken (CX hits 0), LOOPE/LOOPNE (ZF interaction), and JCXZ
    /// taken (CX==0) / not-taken (CX!=0).
    fn branch_golden_cases() -> &'static [BranchGolden] {
        &[
            // Jcc short (rel8). ZF is pre-set, so JZ is taken and JNZ falls through.
            BranchGolden {
                name: "jz +5 taken (74, ZF set)",
                code: &[0x74, 0x05],
                cx: 3,
                gpr: [0, 3, 0, 0, 256, 0, 0, 0],
                eflags: 0x42,
                eip: 0x7,
                deltas: &[],
                fetch: 3,
            },
            BranchGolden {
                name: "jnz +5 not taken (75, ZF set)",
                code: &[0x75, 0x05],
                cx: 3,
                gpr: [0, 3, 0, 0, 256, 0, 0, 0],
                eflags: 0x42,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
            // Jcc short with a backward (negative) rel8 — exercises the sign-extension.
            BranchGolden {
                name: "jz -2 taken backward (74, ZF set)",
                code: &[0x74, 0xfe],
                cx: 3,
                gpr: [0, 3, 0, 0, 256, 0, 0, 0],
                eflags: 0x42,
                eip: 0x0,
                deltas: &[],
                fetch: 3,
            },
            // Jcc near, two-byte (rel16). ZF pre-set: 0F 84 taken, 0F 85 falls through.
            BranchGolden {
                name: "jz near +0x100 taken (0F 84, ZF set)",
                code: &[0x0f, 0x84, 0x00, 0x01],
                cx: 3,
                gpr: [0, 3, 0, 0, 256, 0, 0, 0],
                eflags: 0x42,
                eip: 0x104,
                deltas: &[],
                fetch: 5,
            },
            BranchGolden {
                name: "jnz near +0x100 not taken (0F 85, ZF set)",
                code: &[0x0f, 0x85, 0x00, 0x01],
                cx: 3,
                gpr: [0, 3, 0, 0, 256, 0, 0, 0],
                eflags: 0x42,
                eip: 0x4,
                deltas: &[],
                fetch: 5,
            },
            // JMP short (rel8) and JMP near (rel16): unconditional.
            BranchGolden {
                name: "jmp short +5 (eb)",
                code: &[0xeb, 0x05],
                cx: 3,
                gpr: [0, 3, 0, 0, 256, 0, 0, 0],
                eflags: 0x42,
                eip: 0x7,
                deltas: &[],
                fetch: 3,
            },
            BranchGolden {
                name: "jmp near +0x100 (e9)",
                code: &[0xe9, 0x00, 0x01],
                cx: 3,
                gpr: [0, 3, 0, 0, 256, 0, 0, 0],
                eflags: 0x42,
                eip: 0x103,
                deltas: &[],
                fetch: 4,
            },
            // CALL near (rel16): push the return address (post-instruction eip = 3) then branch.
            // SP drops by 2 (0x100 -> 0xfe) and [SS:0xfe] holds the little-endian return address.
            BranchGolden {
                name: "call near +0x100 (e8, push return)",
                code: &[0xe8, 0x00, 0x01],
                cx: 3,
                gpr: [0, 3, 0, 0, 254, 0, 0, 0],
                eflags: 0x42,
                eip: 0x103,
                deltas: &[(0xfe, 0x03)],
                fetch: 4,
            },
            // LOOP (0xe2): decrement CX, branch while nonzero.
            BranchGolden {
                name: "loop +5 taken (e2, cx 3->2)",
                code: &[0xe2, 0x05],
                cx: 3,
                gpr: [0, 2, 0, 0, 256, 0, 0, 0],
                eflags: 0x42,
                eip: 0x7,
                deltas: &[],
                fetch: 3,
            },
            BranchGolden {
                name: "loop +5 not taken (e2, cx 1->0)",
                code: &[0xe2, 0x05],
                cx: 1,
                gpr: [0, 0, 0, 0, 256, 0, 0, 0],
                eflags: 0x42,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
            // LOOPE (0xe1, loops while ZF=1) and LOOPNE (0xe0, loops while ZF=0). ZF pre-set.
            BranchGolden {
                name: "loope +5 taken (e1, ZF set, cx 3->2)",
                code: &[0xe1, 0x05],
                cx: 3,
                gpr: [0, 2, 0, 0, 256, 0, 0, 0],
                eflags: 0x42,
                eip: 0x7,
                deltas: &[],
                fetch: 3,
            },
            BranchGolden {
                name: "loopne +5 not taken (e0, ZF set, cx 3->2)",
                code: &[0xe0, 0x05],
                cx: 3,
                gpr: [0, 2, 0, 0, 256, 0, 0, 0],
                eflags: 0x42,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
            // JCXZ (0xe3): branch when CX == 0, no decrement.
            BranchGolden {
                name: "jcxz +5 taken (e3, cx==0)",
                code: &[0xe3, 0x05],
                cx: 0,
                gpr: [0, 0, 0, 0, 256, 0, 0, 0],
                eflags: 0x42,
                eip: 0x7,
                deltas: &[],
                fetch: 3,
            },
            BranchGolden {
                name: "jcxz +5 not taken (e3, cx!=0)",
                code: &[0xe3, 0x05],
                cx: 1,
                gpr: [0, 1, 0, 0, 256, 0, 0, 0],
                eflags: 0x42,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
        ]
    }

    #[test]
    fn branch_split_matches_golden_across_ops() {
        // The relative/loop branch block (Jcc short/near, JMP short/near, CALL near, LOOP/LOOPE/
        // LOOPNE/JCXZ) is converted to the decode/execute split, so its fused arms are deleted and it
        // can no longer be diffed against a fused executor. Run each case through cycle() (the split)
        // and assert the architectural end-state against goldens captured from the pre-split fused
        // path via `regen_branch_goldens`. The eip field is the load-bearing assertion: it is the
        // branch target, proving decode stored the right sign-extended displacement and the executor
        // reproduced the fused eip-relative math (rel8 vs rel16, taken vs fall-through). CALL also
        // asserts the pushed return address (memory delta) and the SP decrement (gpr[4]).
        for g in branch_golden_cases() {
            let mut mem = vec![0u8; 0x200];
            mem[..g.code.len()].copy_from_slice(g.code);
            let initial = mem.clone();

            let mut split = Cpu386::default();
            branch_seed(&mut split, g.cx);
            let mut sbus = TestBus::with_memory(mem);
            let _ = split.cycle(&mut sbus);

            assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
            assert_eq!(
                split.registers.eflags, g.eflags,
                "eflags mismatch for {}",
                g.name
            );
            assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
            let deltas: Vec<(usize, u8)> = sbus
                .memory
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != initial[*i])
                .map(|(i, b)| (i, *b))
                .collect();
            assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
            assert_eq!(
                seam_fetch_count(&sbus),
                g.fetch,
                "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
                g.name
            );
        }
    }

    /// Regenerate `branch_golden_cases` from the fused reference. Ignored by default.
    /// The branch fused arms are already deleted on `perf-decode-cache`, so run this from the
    /// pre-split base commit (a94ed279) where they still exist:
    ///   git stash && git checkout a94ed279
    ///   cargo test -p izarravm-cpu --lib regen_branch_goldens -- --ignored --nocapture
    /// then paste the output over `branch_golden_cases`, return to the branch, and only then trust it.
    #[test]
    #[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
    fn regen_branch_goldens() {
        for g in branch_golden_cases() {
            let mut mem = vec![0u8; 0x200];
            mem[..g.code.len()].copy_from_slice(g.code);
            let initial = mem.clone();

            let mut fused = Cpu386::default();
            branch_seed(&mut fused, g.cx);
            let mut fbus = TestBus::with_memory(mem);
            fused.begin_instruction();
            if fused.execute_instruction_legacy(&mut fbus).is_err() {
                println!(
                    "            // TODO regen {}: fused path unavailable here; run against the base commit",
                    g.name
                );
                continue;
            }
            let deltas: Vec<(usize, u8)> = fbus
                .memory
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != initial[*i])
                .map(|(i, b)| (i, *b))
                .collect();
            let fetch = seam_fetch_count(&fbus);
            println!(
                "            BranchGolden {{ name: {:?}, code: &{:?}, cx: {}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
                g.name,
                g.code,
                g.cx,
                fused.registers.gpr,
                fused.registers.eflags,
                fused.registers.eip,
                deltas,
                fetch,
            );
        }
    }

    /// One golden end-state for a control-flow case (task A6b). Mirrors the `BranchGolden` shape but
    /// adds `cs` (the CS selector) and a per-case `setup` closure, because this group changes
    /// segment state (RETF, far-direct CALL/JMP, and the INT/IRET deliveries reload CS) and each
    /// form needs its own in-memory image (a far pointer / IVT entry / saved stack frame). The
    /// captured fields are the standard set plus `cs`: end gpr (AX,CX,DX,BX,SP,BP,SI,DI), the CS
    /// selector, eflags, eip, (offset,value) memory writes (CALL/PUSH/INT push; INC/DEC write), and
    /// the InstructionPrefetch fetch count.
    struct ControlFlowGolden {
        name: &'static str,
        code: &'static [u8],
        /// Per-case memory image written before the run (IVT entries, far pointers, saved frames),
        /// applied identically on the split and the fused-reference paths.
        setup: fn(&mut [u8]),
        gpr: [u32; 8],
        cs: u16,
        eflags: u32,
        eip: u32,
        deltas: &'static [(usize, u8)],
        fetch: usize,
    }

    /// Shared register seed for the control-flow golden battery: CS/DS/SS = 0, eip = 0, SP = 0x100
    /// (a safe in-image stack), BX = 0x40 (so `[bx]` addresses the in-image FF r/m operand), and the
    /// OF/IF flags set so INTO traps and the interrupt deliveries record IF being cleared. The
    /// per-case `setup` closure lays down the memory image each form needs.
    fn controlflow_seed(cpu: &mut Cpu386) {
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Sp, 0x0100);
        cpu.write_reg16(Reg16::Bx, 0x0040);
        cpu.set_flag(FLAG_OF, true);
        cpu.set_flag(FLAG_IF, true);
    }

    /// The far/indirect/RET/INT control-flow + 0xff group-5 differential battery. Captured from the
    /// PRIOR fused reference via `regen_controlflow_goldens`; see `branch_golden_cases` for the
    /// capture recipe. These opcodes' fused arms are deleted on `perf-decode-cache`, so the goldens
    /// were captured from the pre-split base commit (HEAD before A6b, dc1cf4e2): the regen runs the
    /// fused `execute_instruction_legacy` there, prints the literals, and they are pasted back.
    ///
    /// Covers the non-faulting success paths: RET near (with and without an imm16 SP-release), RETF
    /// (the CS reload + SP delta), FF /0 INC and FF /1 DEC r/m (the memory write + the flag update),
    /// FF /6 PUSH r/m (the pushed value + SP drop), FF /2 near-indirect CALL (the pushed return +
    /// the new eip), FF /4 near-indirect JMP (the new eip, nothing pushed), CALL/JMP far direct
    /// (0x9a/0xea — the CS:eip transfer, plus CALL's pushed CS:IP), and the INT3/INT n/INTO/IRET
    /// deliveries (CS:eip from the IVT, the pushed FLAGS:CS:IP frame / the restored frame, IF
    /// cleared). The shared `controlflow_seed` plus each case's `setup` makes every input stable.
    fn controlflow_golden_cases() -> &'static [ControlFlowGolden] {
        &[
            ControlFlowGolden {
                name: "ret near (c3, pop 0x0100)",
                code: &[0xc3],
                setup: |m| m[0x100..0x102].copy_from_slice(&0x0100u16.to_le_bytes()),
                gpr: [0, 0, 0, 64, 258, 0, 0, 0],
                cs: 0x0,
                eflags: 0xa02,
                eip: 0x100,
                deltas: &[],
                fetch: 2,
            },
            ControlFlowGolden {
                name: "ret near imm16 (c2 04 00, pop then release 4)",
                code: &[0xc2, 0x04, 0x00],
                setup: |m| m[0x100..0x102].copy_from_slice(&0x0100u16.to_le_bytes()),
                gpr: [0, 0, 0, 64, 262, 0, 0, 0],
                cs: 0x0,
                eflags: 0xa02,
                eip: 0x100,
                deltas: &[],
                fetch: 4,
            },
            ControlFlowGolden {
                name: "retf (cb, pop 0x0100:0x3000)",
                code: &[0xcb],
                setup: |m| {
                    m[0x100..0x102].copy_from_slice(&0x0100u16.to_le_bytes());
                    m[0x102..0x104].copy_from_slice(&0x3000u16.to_le_bytes());
                },
                gpr: [0, 0, 0, 64, 260, 0, 0, 0],
                cs: 0x3000,
                eflags: 0xa02,
                eip: 0x100,
                deltas: &[],
                fetch: 2,
            },
            ControlFlowGolden {
                name: "ff /0 inc word [bx] (0x0080 -> 0x0081)",
                code: &[0xff, 0x07],
                setup: |m| m[0x40..0x42].copy_from_slice(&0x0080u16.to_le_bytes()),
                gpr: [0, 0, 0, 64, 256, 0, 0, 0],
                cs: 0x0,
                eflags: 0x206,
                eip: 0x2,
                deltas: &[(64, 129)],
                fetch: 3,
            },
            ControlFlowGolden {
                name: "ff /1 dec word [bx] (0x0080 -> 0x007f)",
                code: &[0xff, 0x0f],
                setup: |m| m[0x40..0x42].copy_from_slice(&0x0080u16.to_le_bytes()),
                gpr: [0, 0, 0, 64, 256, 0, 0, 0],
                cs: 0x0,
                eflags: 0x212,
                eip: 0x2,
                deltas: &[(64, 127)],
                fetch: 3,
            },
            ControlFlowGolden {
                name: "ff /6 push word [bx] (push 0x0080)",
                code: &[0xff, 0x37],
                setup: |m| m[0x40..0x42].copy_from_slice(&0x0080u16.to_le_bytes()),
                gpr: [0, 0, 0, 64, 254, 0, 0, 0],
                cs: 0x0,
                eflags: 0xa02,
                eip: 0x2,
                deltas: &[(254, 128)],
                fetch: 3,
            },
            ControlFlowGolden {
                name: "ff /2 call near [bx] (push return 2, jump 0x0080)",
                code: &[0xff, 0x17],
                setup: |m| m[0x40..0x42].copy_from_slice(&0x0080u16.to_le_bytes()),
                gpr: [0, 0, 0, 64, 254, 0, 0, 0],
                cs: 0x0,
                eflags: 0xa02,
                eip: 0x80,
                deltas: &[(254, 2)],
                fetch: 3,
            },
            ControlFlowGolden {
                name: "ff /4 jmp near [bx] (jump 0x0080, nothing pushed)",
                code: &[0xff, 0x27],
                setup: |m| m[0x40..0x42].copy_from_slice(&0x0080u16.to_le_bytes()),
                gpr: [0, 0, 0, 64, 256, 0, 0, 0],
                cs: 0x0,
                eflags: 0xa02,
                eip: 0x80,
                deltas: &[],
                fetch: 3,
            },
            ControlFlowGolden {
                name: "call far 0x3000:0x0100 (9a, push cs:ip)",
                code: &[0x9a, 0x00, 0x01, 0x00, 0x30],
                setup: |_m| {},
                gpr: [0, 0, 0, 64, 252, 0, 0, 0],
                cs: 0x3000,
                eflags: 0xa02,
                eip: 0x100,
                deltas: &[(252, 5)],
                fetch: 6,
            },
            ControlFlowGolden {
                name: "jmp far 0x3000:0x0100 (ea, nothing pushed)",
                code: &[0xea, 0x00, 0x01, 0x00, 0x30],
                setup: |_m| {},
                gpr: [0, 0, 0, 64, 256, 0, 0, 0],
                cs: 0x3000,
                eflags: 0xa02,
                eip: 0x100,
                deltas: &[],
                fetch: 6,
            },
            ControlFlowGolden {
                name: "int3 (cc, ivt[3] -> 0000:0040)",
                code: &[0xcc],
                setup: |m| m[12..14].copy_from_slice(&0x0040u16.to_le_bytes()),
                gpr: [0, 0, 0, 64, 250, 0, 0, 0],
                cs: 0x0,
                eflags: 0x802,
                eip: 0x40,
                deltas: &[(250, 1), (254, 2), (255, 10)],
                fetch: 2,
            },
            ControlFlowGolden {
                name: "int 0x21 (cd 21, ivt[0x21] -> 0000:0050)",
                code: &[0xcd, 0x21],
                setup: |m| m[0x84..0x86].copy_from_slice(&0x0050u16.to_le_bytes()),
                gpr: [0, 0, 0, 64, 250, 0, 0, 0],
                cs: 0x0,
                eflags: 0x802,
                eip: 0x50,
                deltas: &[(250, 2), (254, 2), (255, 10)],
                fetch: 3,
            },
            ControlFlowGolden {
                name: "into with OF set (ce, ivt[4] -> 0000:0060)",
                code: &[0xce],
                setup: |m| m[16..18].copy_from_slice(&0x0060u16.to_le_bytes()),
                gpr: [0, 0, 0, 64, 250, 0, 0, 0],
                cs: 0x0,
                eflags: 0x802,
                eip: 0x60,
                deltas: &[(250, 1), (254, 2), (255, 10)],
                fetch: 2,
            },
            ControlFlowGolden {
                name: "iret (cf, restore 0000:0100 flags 0x0202)",
                code: &[0xcf],
                setup: |m| {
                    m[0x100..0x102].copy_from_slice(&0x0100u16.to_le_bytes());
                    m[0x102..0x104].copy_from_slice(&0x0000u16.to_le_bytes());
                    m[0x104..0x106].copy_from_slice(&0x0202u16.to_le_bytes());
                },
                gpr: [0, 0, 0, 64, 262, 0, 0, 0],
                cs: 0x0,
                eflags: 0x202,
                eip: 0x100,
                deltas: &[],
                fetch: 2,
            },
        ]
    }

    #[test]
    fn controlflow_split_matches_golden_across_ops() {
        // The far/indirect/RET/INT control-flow block + 0xff group 5 is converted to the decode/
        // execute split, so its fused arms are deleted and it can no longer be diffed against a fused
        // executor in-tree. Run each case through cycle() (the split) and assert the architectural
        // end-state against goldens captured from the pre-split fused path via
        // `regen_controlflow_goldens`. eip is the branch/return/vector target; cs proves RETF / the
        // far-direct / INT deliveries reloaded the segment; the memory deltas prove CALL/PUSH/INT
        // pushed (and INC/DEC wrote) the right bytes; the fetch count proves decode charged each
        // instruction byte exactly once.
        for g in controlflow_golden_cases() {
            let mut mem = vec![0u8; 0x200];
            mem[..g.code.len()].copy_from_slice(g.code);
            (g.setup)(&mut mem);
            let initial = mem.clone();

            let mut split = Cpu386::default();
            controlflow_seed(&mut split);
            let mut sbus = TestBus::with_memory(mem);
            split.cycle(&mut sbus).unwrap();

            assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
            assert_eq!(
                split.registers.cs().selector,
                g.cs,
                "cs mismatch for {}",
                g.name
            );
            assert_eq!(
                split.registers.eflags, g.eflags,
                "eflags mismatch for {}",
                g.name
            );
            assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
            let deltas: Vec<(usize, u8)> = sbus
                .memory
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != initial[*i])
                .map(|(i, b)| (i, *b))
                .collect();
            assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
            assert_eq!(
                seam_fetch_count(&sbus),
                g.fetch,
                "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
                g.name
            );
        }
    }

    /// Regenerate `controlflow_golden_cases` from the fused reference. Ignored by default. The
    /// control-flow fused arms are already deleted on `perf-decode-cache`, so run this from the
    /// pre-split base commit (HEAD before A6b, dc1cf4e2) where they still exist:
    ///   git worktree add ../regen dc1cf4e2 && cd ../regen
    ///   cargo test -p izarravm-cpu --lib regen_controlflow_goldens -- --ignored --nocapture
    /// then paste the output over `controlflow_golden_cases`, return to the branch, and only then
    /// trust it. (Copy this test body + the struct/seed into the throwaway worktree if the fused
    /// base predates them.)
    #[test]
    #[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
    fn regen_controlflow_goldens() {
        for g in controlflow_golden_cases() {
            let mut mem = vec![0u8; 0x200];
            mem[..g.code.len()].copy_from_slice(g.code);
            (g.setup)(&mut mem);
            let initial = mem.clone();

            let mut fused = Cpu386::default();
            controlflow_seed(&mut fused);
            let mut fbus = TestBus::with_memory(mem);
            fused.begin_instruction();
            if fused.execute_instruction_legacy(&mut fbus).is_err() {
                println!(
                    "            // TODO regen {}: fused path unavailable here; run against the base commit",
                    g.name
                );
                continue;
            }
            let deltas: Vec<(usize, u8)> = fbus
                .memory
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != initial[*i])
                .map(|(i, b)| (i, *b))
                .collect();
            let fetch = seam_fetch_count(&fbus);
            println!(
                "            // {}\n            gpr: {:?}, cs: {:#x}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {},",
                g.name,
                fused.registers.gpr,
                fused.registers.cs().selector,
                fused.registers.eflags,
                fused.registers.eip,
                deltas,
                fetch,
            );
        }
    }

    /// FF /7 is an undefined group-5 encoding and must raise the group-opcode error (which the
    /// emulator maps to #UD), not silently execute. Drive it through the split and assert the error.
    #[test]
    fn controlflow_ff_ext7_is_undefined() {
        // 0xff 0x3f: mod=00 reg=111 rm=111 -> group 5 /7 with a memory r/m. The /7 extension is
        // undefined regardless of the addressing form.
        let (mut cpu, memory) = real_mode_cpu(&[0xff, 0x3f], 0x100);
        let mut bus = TestBus::with_memory(memory);
        let err = exec_one_split(&mut cpu, &mut bus).unwrap_err();
        assert!(
            matches!(
                err,
                InternalFault::Cpu(CpuError::UnsupportedGroupOpcode {
                    opcode: 0xff,
                    extension: 7
                })
            ),
            "FF /7 must raise the undefined group-opcode error, got {err:?}"
        );
    }

    // ---- Flags + misc register golden battery (A7) ----

    /// One golden end-state for a flags/misc register case (task A7). The standard shape:
    /// opcode bytes, expected end gpr (AX,CX,DX,BX,SP,BP,SI,DI), eflags, eip, and the
    /// InstructionPrefetch fetch count. No memory writes (none of the A7 opcodes write to memory),
    /// so no `deltas` field. The `eflags` field is load-bearing for most cases (TEST/SAHF/CLC/STC/
    /// CMC/CLD/STD/CLI/STI change flags; INC/DEC change S/Z/O/A/P while preserving CF; CBW/CWD
    /// change registers only). `eip` advances past the instruction (1 byte for all except TEST
    /// 0x84/0x85 which have a ModRM, so 2 bytes).
    struct FlagsMiscGolden {
        name: &'static str,
        code: &'static [u8],
        gpr: [u32; 8],
        eflags: u32,
        eip: u32,
        fetch: usize,
    }

    /// Seed for the flags/misc golden battery: the same register file as `seam_seed` plus CF
    /// pre-set (so INC/DEC CF-preservation is visible and CMC/CLC/STC have a known starting CF),
    /// and AH=0xd7 (= 0b11010111: CF/PF/AF/ZF/SF all 1, bits 3/5 forced — so LAHF/SAHF transfer
    /// a non-trivial value). AH lives in the high byte of AX; write_gpr8(4, 0xd7) sets it.
    fn flags_misc_seed(cpu: &mut Cpu386) {
        seam_seed(cpu);
        // CF set (bit 0 on top of always-1 bit 1). Makes INC/DEC CF-preservation observable and
        // gives CMC/CLC/STC a known starting state.
        cpu.registers.eflags = 0x03;
        // AH = 0xd7 (bit pattern: CF=1, PF=1, AF=1, ZF=1, SF=1, reserved bits 1/3/5).
        // This is the value SAHF loads into the low flag byte, and LAHF reads it back out.
        cpu.write_gpr8(4, 0xd7);
    }

    /// Seed memory for the flags/misc battery: plant a word at [bx]=ds:0x10 (the TEST r/m target).
    fn flags_misc_seed_mem(mem: &mut [u8], code: &[u8]) {
        mem[..code.len()].copy_from_slice(code);
        // TEST byte [bx]: [0x10] = 0x12; TEST word [bx]: [0x10..0x12] = 0x3412.
        mem[0x10..0x12].copy_from_slice(&0x3412u16.to_le_bytes());
    }

    /// The flags + misc register differential battery (task A7). Captured from the PRIOR fused
    /// reference (`execute_instruction_legacy`) via `regen_flags_misc_goldens`; see
    /// `alu_golden_cases` for the full capture recipe. Never edit by hand — re-run the regen WHILE
    /// the fused arms (0x40-0x4f, 0x84/0x85, 0x98/0x99, 0x9e/0x9f, 0xf5/0xf8-0xfd) still exist
    /// in `dispatch_opcode`, then paste, then delete the fused arms. Covers: TEST byte/word reg and
    /// mem (flags set, no write-back); INC/DEC reg (CF preserved, overflow and sign visible); CBW/
    /// CWDE/CWD/CDQ (operand-size-dependent sign extension); SAHF/LAHF (flag-byte round-trip); and
    /// all seven flag-bit ops CMC/CLC/STC/CLI/STI/CLD/STD (correct bit set/clear/complement;
    /// STI interrupt shadow is covered by a dedicated test).
    fn flags_misc_golden_cases() -> &'static [FlagsMiscGolden] {
        // Captured from the fused reference (`execute_instruction_legacy`) via
        // `regen_flags_misc_goldens` run against parent commit 3912fbc5.
        // Seed: AX=0xD702 (AH=0xd7, AL=0x02; seam_seed sets AX=0x0102 then AH=0xd7),
        // CX=0x0304, DX=0x0506, BX=0x0010, SP=0, BP=0x0010, SI=0x0008, DI=0x0018.
        // eflags=0x03 (CF=1, always-1 bit1=1). Memory: [0x10..0x12] = 0x3412.
        &[
            // TEST r/m8,reg8 (0x84): flags only, no write-back. TEST AL,AL: 0x02 AND 0x02 = 0x02,
            // ZF=0 PF=0 SF=0 CF=0 OF=0 → eflags=0x02 (reserved bit only).
            FlagsMiscGolden {
                name: "test al,al (84 c0)",
                code: &[0x84, 0xc0],
                gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x2,
                fetch: 3,
            },
            // TEST r/m8,reg8 (0x84): TEST [bx],cl: [0x10]=0x12 AND CL=0x04 → 0x00, ZF=1 → 0x46.
            FlagsMiscGolden {
                name: "test [bx],cl (84 0f)",
                code: &[0x84, 0x0f],
                gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x46,
                eip: 0x2,
                fetch: 3,
            },
            // TEST r/m16,reg16 (0x85): TEST BX,CX: 0x0010 AND 0x0304 = 0x0000, ZF=1 PF=1 → 0x46.
            FlagsMiscGolden {
                name: "test bx,cx (85 cb)",
                code: &[0x85, 0xcb],
                gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x46,
                eip: 0x2,
                fetch: 3,
            },
            // TEST r/m16,reg16 (0x85): TEST [bx],cx: [0x10]=0x3412 AND 0x0304 = 0x0000, ZF=1 → 0x46.
            FlagsMiscGolden {
                name: "test [bx],cx (85 0f)",
                code: &[0x85, 0x0f],
                gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x46,
                eip: 0x2,
                fetch: 3,
            },
            // INC AX (0x40): AX=0xd702 → 0xd703. CF preserved (stays 1). AF set (low nibble 2→3).
            FlagsMiscGolden {
                name: "inc ax (40)",
                code: &[0x40],
                gpr: [55043, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x87,
                eip: 0x1,
                fetch: 2,
            },
            // INC DI (0x47): DI=0x0018 → 0x0019. CF preserved (stays 1). No half-carry.
            FlagsMiscGolden {
                name: "inc di (47)",
                code: &[0x47],
                gpr: [55042, 772, 1286, 16, 0, 16, 8, 25],
                eflags: 0x3,
                eip: 0x1,
                fetch: 2,
            },
            // DEC AX (0x48): AX=0xd702 → 0xd701. CF preserved (stays 1). SF set (high bit of AH).
            FlagsMiscGolden {
                name: "dec ax (48)",
                code: &[0x48],
                gpr: [55041, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x83,
                eip: 0x1,
                fetch: 2,
            },
            // DEC DI (0x4f): DI=0x0018 → 0x0017. CF preserved (stays 1). AF set.
            FlagsMiscGolden {
                name: "dec di (4f)",
                code: &[0x4f],
                gpr: [55042, 772, 1286, 16, 0, 16, 8, 23],
                eflags: 0x7,
                eip: 0x1,
                fetch: 2,
            },
            // CBW (0x98): sign-extend AL=0x02 (positive) → AX=0x0002. AH cleared.
            FlagsMiscGolden {
                name: "cbw (98, al=0x02)",
                code: &[0x98],
                gpr: [2, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x3,
                eip: 0x1,
                fetch: 2,
            },
            // CWD (0x99): AX=0xd702 (sign bit set; 0xd702 as i16 = -10494 < 0) → DX=0xFFFF.
            FlagsMiscGolden {
                name: "cwd (99, ax positive)",
                code: &[0x99],
                gpr: [55042, 772, 65535, 16, 0, 16, 8, 24],
                eflags: 0x3,
                eip: 0x1,
                fetch: 2,
            },
            // SAHF (0x9e): AH=0xd7 (= 1101_0111b) → flags low byte = d7 (CF=1 PF=1 AF=1 ZF=1 SF=1).
            FlagsMiscGolden {
                name: "sahf (9e, ah=0xd7)",
                code: &[0x9e],
                gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0xd7,
                eip: 0x1,
                fetch: 2,
            },
            // LAHF (0x9f): eflags=0x03 → AH = (0x03 & 0xD5) | 0x02 = 0x03. AX = 0x0302=770.
            FlagsMiscGolden {
                name: "lahf (9f, eflags=0x03)",
                code: &[0x9f],
                gpr: [770, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x3,
                eip: 0x1,
                fetch: 2,
            },
            // CMC (0xf5): CF was 1 → CF=0. eflags: 0x03 → 0x02.
            FlagsMiscGolden {
                name: "cmc (f5, cf=1->0)",
                code: &[0xf5],
                gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                fetch: 2,
            },
            // CLC (0xf8): CF=0. eflags: 0x03 → 0x02.
            FlagsMiscGolden {
                name: "clc (f8)",
                code: &[0xf8],
                gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x2,
                eip: 0x1,
                fetch: 2,
            },
            // STC (0xf9): CF=1. eflags stays 0x03 (already set).
            FlagsMiscGolden {
                name: "stc (f9)",
                code: &[0xf9],
                gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x3,
                eip: 0x1,
                fetch: 2,
            },
            // CLD (0xfc): DF=0. DF was already 0 in seed; eflags stays 0x03.
            FlagsMiscGolden {
                name: "cld (fc)",
                code: &[0xfc],
                gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x3,
                eip: 0x1,
                fetch: 2,
            },
            // STD (0xfd): DF=1. eflags: 0x03 → 0x403.
            FlagsMiscGolden {
                name: "std (fd)",
                code: &[0xfd],
                gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
                eflags: 0x403,
                eip: 0x1,
                fetch: 2,
            },
        ]
    }

    #[test]
    fn flags_misc_split_matches_golden_across_ops() {
        // The flags + misc register block (TEST r/m,reg, INC/DEC reg, CBW/CWD, SAHF/LAHF, and the
        // single flag-bit ops) is converted to the decode/execute split, so its fused arms are
        // deleted and it can no longer be diffed against a fused executor in-tree. Run each case
        // through cycle() (the split) and assert the architectural end-state against goldens
        // captured from the pre-split fused path via `regen_flags_misc_goldens`. eflags is
        // load-bearing for most cases (flags change); eip proves decode consumed the right bytes
        // (1 for implicit-operand ops, 2 for TEST with ModRM); fetch proves each instruction byte
        // was charged exactly once. INC/DEC CF-preservation is observable because the seed pre-sets
        // CF and the goldens carry it.
        for g in flags_misc_golden_cases() {
            let mut mem = vec![0u8; 0x200];
            flags_misc_seed_mem(&mut mem, g.code);

            let mut split = Cpu386::default();
            flags_misc_seed(&mut split);
            let mut sbus = TestBus::with_memory(mem);
            let _ = split.cycle(&mut sbus);

            assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
            assert_eq!(
                split.registers.eflags, g.eflags,
                "eflags mismatch for {}",
                g.name
            );
            assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
            assert_eq!(
                seam_fetch_count(&sbus),
                g.fetch,
                "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
                g.name
            );
        }
    }

    /// Regenerate `flags_misc_golden_cases` from the fused reference. Ignored by default.
    /// Run WHILE the fused arms (0x40-0x4f, 0x84/0x85, 0x98/0x99, 0x9e/0x9f, 0xf5/0xf8-0xfd)
    /// still exist in `dispatch_opcode` (i.e. the parent commit 3912fbc5):
    ///   git worktree add ../regen-a7 3912fbc5
    ///   cd ../regen-a7
    ///   cargo test -p izarravm-cpu --lib regen_flags_misc_goldens -- --ignored --nocapture
    /// then paste the output over `flags_misc_golden_cases` and only then delete the fused arms.
    #[test]
    #[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
    fn regen_flags_misc_goldens() {
        for g in flags_misc_golden_cases() {
            let mut mem = vec![0u8; 0x200];
            flags_misc_seed_mem(&mut mem, g.code);
            let initial = mem.clone();

            let mut fused = Cpu386::default();
            flags_misc_seed(&mut fused);
            let mut fbus = TestBus::with_memory(mem);
            fused.begin_instruction();
            if fused.execute_instruction_legacy(&mut fbus).is_err() {
                println!(
                    "            // TODO regen {}: fused path unavailable here; run against the base commit",
                    g.name
                );
                continue;
            }
            let deltas: Vec<(usize, u8)> = fbus
                .memory
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != initial[*i])
                .map(|(i, b)| (i, *b))
                .collect();
            let fetch = seam_fetch_count(&fbus);
            println!(
                "            FlagsMiscGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, fetch: {} }},",
                g.name,
                g.code,
                fused.registers.gpr,
                fused.registers.eflags,
                fused.registers.eip,
                fetch,
            );
            if !deltas.is_empty() {
                println!("            // memory deltas: {:?}", deltas);
            }
        }
    }

    /// STI's interrupt shadow: after STI, the immediately-following instruction executes before
    /// any interrupt is taken, even when a hardware interrupt is already pending. Drive three
    /// back-to-back cycles through the split: STI then NOP (0x90) then another NOP. A fake
    /// interrupt is pending from the start via `TestBus.pending_irq`. Prove: (1) after STI the
    /// interrupt is NOT taken (shadow active), (2) after NOP the interrupt is still pending (shadow
    /// let NOP through), and (3) after the next cycle the interrupt is consumed (shadow expired).
    #[test]
    fn sti_interrupt_shadow_defers_interrupt_by_one_instruction() {
        let mut memory = vec![0u8; 0x400];
        // STI (0xfb) followed by two NOPs (0x90).
        memory[0] = 0xfb; // STI
        memory[1] = 0x90; // NOP — executes before interrupt is taken (shadow)
        memory[2] = 0x90; // NOP — not reached; interrupt taken instead
        // IVT entry for vector 0x08 (IRQ0) at byte offset 0x20 (0x0008 * 4):
        // offset=0x0200, segment=0x0000.
        memory[0x20..0x22].copy_from_slice(&0x0200u16.to_le_bytes());
        memory[0x22..0x24].copy_from_slice(&0x0000u16.to_le_bytes());
        // IRET at the handler target (not reached in this test but avoids unmapped-memory errors
        // if the CPU tries to read into it).
        memory[0x200] = 0xcf;

        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
        cpu.write_reg16(Reg16::Sp, 0x100);
        // Start with IF clear so STI is what enables interrupts.
        cpu.set_flag(FLAG_IF, false);

        let mut bus = TestBus::with_memory(memory);
        // Arm a pending IRQ 8. `interrupt_pending()` returns true while `pending_irq.is_some()`.
        bus.pending_irq = Some(8);

        // Cycle 1: STI (0xfb). IF becomes set; interrupt_shadow is armed. The pending IRQ is NOT
        // serviced yet (shadow active): eip advances to 1.
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.registers.eip, 1,
            "eip must be 1 after STI — NOP not yet executed"
        );
        assert!(cpu.flag(FLAG_IF), "STI must set IF");
        assert!(
            bus.pending_irq.is_some(),
            "interrupt must not be taken during the STI cycle itself"
        );

        // Cycle 2: NOP (0x90). Shadow consumed at cycle start → interrupt check skipped → NOP
        // executes → eip advances to 2. IRQ still pending.
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.registers.eip, 2,
            "eip must be 2 after NOP — shadow let NOP through"
        );
        assert!(
            bus.pending_irq.is_some(),
            "interrupt must still be pending after NOP (shadow consumed, interrupt check skipped)"
        );

        // Cycle 3: no shadow, IF set, IRQ pending → interrupt is acknowledged before fetch.
        // `acknowledge_interrupt` takes the pending_irq, so it becomes None.
        cpu.cycle(&mut bus).unwrap();
        assert!(
            bus.pending_irq.is_none(),
            "interrupt must be taken after the shadow expires"
        );
    }

    /// One golden end-state for a string-operation case (task A8). The string ops touch both
    /// registers and memory, and the inputs differ widely per form (SI/DI/CX/AX/DF, the REP prefix,
    /// the source/dest memory image), so each case carries its own register seed (`regs`) and memory
    /// image (`setup`) on top of the shared `string_seed`. The captured fields are the standard
    /// differential set plus the destination memory writes: end gpr (AX,CX,DX,BX,SP,BP,SI,DI),
    /// eflags (CMPS/SCAS set them; MOVS/STOS/LODS leave them), eip, the (offset,value) memory deltas
    /// (MOVS/STOS write the destination; CMPS/SCAS/LODS write nothing), and the InstructionPrefetch
    /// fetch count (prefix + opcode, charged once in `decode` — small and CX-independent even for the
    /// REP forms, since the per-element data accesses are bus reads/writes, not instruction fetches).
    struct StringGolden {
        name: &'static str,
        code: &'static [u8],
        /// Per-case register seed applied after `string_seed` (SI/DI/CX/AX, the DF flag, segment
        /// bases for the override case), applied identically on the split and fused-reference paths.
        regs: fn(&mut Cpu386),
        /// Per-case memory image (the source and destination bytes), applied identically on both
        /// paths before the run.
        setup: fn(&mut [u8]),
        gpr: [u32; 8],
        eflags: u32,
        eip: u32,
        deltas: &'static [(usize, u8)],
        fetch: usize,
    }

    /// Shared register seed for the string-operation golden battery: CS/DS/ES/SS = 0 and eip = 0.
    /// Everything that varies per form (the index registers, the count, the accumulator, DF, and the
    /// ES base for the segment-override case) is set by each case's `regs` closure, so the seed itself
    /// stays minimal and every input is explicit at the case site.
    fn string_seed(cpu: &mut Cpu386) {
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.registers.eip = 0;
    }

    /// The string-operation differential battery (task A8). Captured from the PRIOR fused reference
    /// (`execute_instruction_legacy`) via `regen_string_goldens`; see `flags_misc_golden_cases` for
    /// the capture recipe. Never edit by hand — re-run the regen WHILE the fused arms (0xa4-0xa7,
    /// 0xaa-0xaf) still exist in `dispatch_opcode`, then paste, then delete the fused arms.
    ///
    /// Covers the plain single-step forms (MOVSB forward DF=0 and backward DF=1; MOVSW; CMPSB
    /// flags+advance; STOSB; LODSB; SCASB; the DS:SI segment override) AND the REP forms, which are
    /// the load-bearing cases: REP MOVSB (CX iterations → CX=0, every element copied, SI/DI advanced
    /// by CX*width), REPE CMPSB (early termination on the first mismatch → CX and ZF prove where it
    /// stopped), and REPNE SCASB (early termination on the first match → CX and ZF).
    fn string_golden_cases() -> &'static [StringGolden] {
        &[
            // MOVSB forward (0xa4), DF=0: [ds:si]=0x42 at 0x100 → [es:di] at 0x200; SI/DI increment.
            StringGolden {
                name: "movsb df=0 (a4)",
                code: &[0xa4],
                regs: |c| {
                    c.set_flag(FLAG_DF, false);
                    c.registers.set_esi(0x100);
                    c.registers.set_edi(0x200);
                },
                setup: |m| m[0x100] = 0x42,
                gpr: [0, 0, 0, 0, 0, 0, 0x101, 0x201],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[(0x200, 0x42)],
                fetch: 2,
            },
            // MOVSB backward (0xa4), DF=1: same copy, but SI/DI decrement.
            StringGolden {
                name: "movsb df=1 (a4)",
                code: &[0xa4],
                regs: |c| {
                    c.set_flag(FLAG_DF, true);
                    c.registers.set_esi(0x100);
                    c.registers.set_edi(0x200);
                },
                setup: |m| m[0x100] = 0x42,
                gpr: [0, 0, 0, 0, 0, 0, 0x0ff, 0x1ff],
                eflags: 0x402,
                eip: 0x1,
                deltas: &[(0x200, 0x42)],
                fetch: 2,
            },
            // MOVSW (0xa5), DF=0: word [0x100..0x102]=0x1234 → [0x200..0x202]; SI/DI += 2.
            StringGolden {
                name: "movsw df=0 (a5)",
                code: &[0xa5],
                regs: |c| {
                    c.set_flag(FLAG_DF, false);
                    c.registers.set_esi(0x100);
                    c.registers.set_edi(0x200);
                },
                setup: |m| m[0x100..0x102].copy_from_slice(&0x1234u16.to_le_bytes()),
                gpr: [0, 0, 0, 0, 0, 0, 0x102, 0x202],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[(0x200, 0x34), (0x201, 0x12)],
                fetch: 2,
            },
            // CMPSB unequal (0xa6): [ds:si]=0x10, [es:di]=0x20 → 0x10-0x20 borrows (ZF=0, CF=1);
            // SI/DI advance even on mismatch. No memory write.
            StringGolden {
                name: "cmpsb unequal (a6)",
                code: &[0xa6],
                regs: |c| {
                    c.set_flag(FLAG_DF, false);
                    c.registers.set_esi(0x100);
                    c.registers.set_edi(0x200);
                },
                setup: |m| {
                    m[0x100] = 0x10;
                    m[0x200] = 0x20;
                },
                gpr: [0, 0, 0, 0, 0, 0, 0x101, 0x201],
                eflags: 0x87,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
            // STOSB (0xaa): AL=0x5a → [es:di]=0x200; DI increments. AL preserved.
            StringGolden {
                name: "stosb (aa)",
                code: &[0xaa],
                regs: |c| {
                    c.set_flag(FLAG_DF, false);
                    c.write_gpr8(0, 0x5a);
                    c.registers.set_edi(0x200);
                },
                setup: |_m| {},
                gpr: [0x5a, 0, 0, 0, 0, 0, 0, 0x201],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[(0x200, 0x5a)],
                fetch: 2,
            },
            // LODSB (0xac): [ds:si]=0x7e at 0x100 → AL; SI increments. No memory write.
            StringGolden {
                name: "lodsb (ac)",
                code: &[0xac],
                regs: |c| {
                    c.set_flag(FLAG_DF, false);
                    c.registers.set_esi(0x100);
                },
                setup: |m| m[0x100] = 0x7e,
                gpr: [0x7e, 0, 0, 0, 0, 0, 0x101, 0],
                eflags: 0x2,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
            // SCASB equal (0xae): AL=0x41, [es:di]=0x41 → ZF set; DI increments, SI untouched.
            StringGolden {
                name: "scasb equal (ae)",
                code: &[0xae],
                regs: |c| {
                    c.set_flag(FLAG_DF, false);
                    c.write_gpr8(0, 0x41);
                    c.registers.set_edi(0x200);
                },
                setup: |m| m[0x200] = 0x41,
                gpr: [0x41, 0, 0, 0, 0, 0, 0, 0x201],
                eflags: 0x46,
                eip: 0x1,
                deltas: &[],
                fetch: 2,
            },
            // MOVSB with an ES: source segment override (0x26 0xa4): ds=0, es base 0x200, so the source
            // reads from es:si (0x210), not ds:si (0x10); the destination stays es:di (0x230).
            StringGolden {
                name: "es: movsb override (26 a4)",
                code: &[0x26, 0xa4],
                regs: |c| {
                    c.load_segment_real(SegmentIndex::Es, 0x20); // base 0x200
                    c.set_flag(FLAG_DF, false);
                    c.registers.set_esi(0x10);
                    c.registers.set_edi(0x30);
                },
                setup: |m| m[0x210] = 0x99,
                gpr: [0, 0, 0, 0, 0, 0, 0x11, 0x31],
                eflags: 0x2,
                eip: 0x2,
                deltas: &[(0x230, 0x99)],
                fetch: 3,
            },
            // REP MOVSB (0xf3 0xa4), CX=3, DF=0: copies 3 bytes [0x100..0x103]→[0x200..0x203];
            // CX→0, SI/DI advance by 3. The fetch count is small (prefix+opcode), CX-independent.
            StringGolden {
                name: "rep movsb cx=3 (f3 a4)",
                code: &[0xf3, 0xa4],
                regs: |c| {
                    c.set_flag(FLAG_DF, false);
                    c.registers.set_esi(0x100);
                    c.registers.set_edi(0x200);
                    c.registers.set_ecx(3);
                },
                setup: |m| m[0x100..0x103].copy_from_slice(&[1, 2, 3]),
                gpr: [0, 0, 0, 0, 0, 0, 0x103, 0x203],
                eflags: 0x2,
                eip: 0x2,
                deltas: &[(0x200, 1), (0x201, 2), (0x202, 3)],
                fetch: 3,
            },
            // REPE CMPSB (0xf3 0xa6), CX=4, DF=0: "AABB" vs "AACC" mismatches at index 2, so the
            // repeat stops there with ZF clear after 3 iterations; CX 4→3→2→1, SI/DI advance by 3.
            StringGolden {
                name: "repe cmpsb cx=4 (f3 a6)",
                code: &[0xf3, 0xa6],
                regs: |c| {
                    c.set_flag(FLAG_DF, false);
                    c.registers.set_esi(0x100);
                    c.registers.set_edi(0x200);
                    c.registers.set_ecx(4);
                },
                setup: |m| {
                    m[0x100..0x104].copy_from_slice(b"AABB");
                    m[0x200..0x204].copy_from_slice(b"AACC");
                },
                gpr: [0, 1, 0, 0, 0, 0, 0x103, 0x203],
                eflags: 0x97,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
            // REPNE SCASB (0xf2 0xae), CX=4, AL='C', DF=0: dest "AACA" scans until the match at
            // index 2, stopping with ZF set after 3 iterations; CX 4→3→2→1, DI advances by 3.
            StringGolden {
                name: "repne scasb cx=4 (f2 ae)",
                code: &[0xf2, 0xae],
                regs: |c| {
                    c.set_flag(FLAG_DF, false);
                    c.write_gpr8(0, b'C');
                    c.registers.set_edi(0x200);
                    c.registers.set_ecx(4);
                },
                setup: |m| m[0x200..0x204].copy_from_slice(b"AACA"),
                gpr: [0x43, 1, 0, 0, 0, 0, 0, 0x203],
                eflags: 0x46,
                eip: 0x2,
                deltas: &[],
                fetch: 3,
            },
        ]
    }

    #[test]
    fn string_split_matches_golden_across_ops() {
        // The string-operation block (MOVS/CMPS/STOS/LODS/SCAS and the REP/REPE/REPNE forms) is
        // converted to the decode/execute split, so its fused arms are deleted and it can no longer be
        // diffed against a fused executor in-tree. Run each case through cycle() (the split) and
        // assert the architectural end-state against goldens captured from the pre-split fused path via
        // `regen_string_goldens`. The register file proves SI/DI/CX/AX moved correctly (direction,
        // element width, REP count decremented to 0 or stopped early); eflags is load-bearing for
        // CMPS/SCAS; the memory deltas prove the destination image (MOVS/STOS) is byte-exact; and the
        // fetch count proves each instruction-fetch byte (prefix + opcode) was charged exactly once
        // regardless of how many elements the REP loop processed.
        for g in string_golden_cases() {
            let mut mem = vec![0u8; 0x400];
            mem[..g.code.len()].copy_from_slice(g.code);
            (g.setup)(&mut mem);

            let mut split = Cpu386::default();
            string_seed(&mut split);
            (g.regs)(&mut split);
            let mut sbus = TestBus::with_memory(mem);
            let _ = split.cycle(&mut sbus);

            assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
            assert_eq!(
                split.registers.eflags, g.eflags,
                "eflags mismatch for {}",
                g.name
            );
            assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
            for &(offset, value) in g.deltas {
                assert_eq!(
                    sbus.memory[offset], value,
                    "memory[{offset:#x}] mismatch for {}",
                    g.name
                );
            }
            assert_eq!(
                seam_fetch_count(&sbus),
                g.fetch,
                "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
                g.name
            );
        }
    }

    /// Regenerate `string_golden_cases` from the fused reference. Ignored by default. Run WHILE the
    /// fused arms (0xa4-0xa7, 0xaa-0xaf) still exist in `dispatch_opcode` (i.e. the parent commit
    /// a9e0fec0):
    ///   git worktree add ../regen-a8 a9e0fec0
    ///   cd ../regen-a8
    ///   cargo test -p izarravm-cpu --lib regen_string_goldens -- --ignored --nocapture
    /// then paste the output over `string_golden_cases` and only then delete the fused arms.
    #[test]
    #[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
    fn regen_string_goldens() {
        for g in string_golden_cases() {
            let mut mem = vec![0u8; 0x400];
            mem[..g.code.len()].copy_from_slice(g.code);
            (g.setup)(&mut mem);
            let initial = mem.clone();

            let mut fused = Cpu386::default();
            string_seed(&mut fused);
            (g.regs)(&mut fused);
            let mut fbus = TestBus::with_memory(mem);
            fused.begin_instruction();
            if fused.execute_instruction_legacy(&mut fbus).is_err() {
                println!(
                    "            // TODO regen {}: fused path unavailable here; run against the base commit",
                    g.name
                );
                continue;
            }
            let deltas: Vec<(usize, u8)> = fbus
                .memory
                .iter()
                .enumerate()
                .filter(|(i, b)| **b != initial[*i])
                .map(|(i, b)| (i, *b))
                .collect();
            let fetch = seam_fetch_count(&fbus);
            println!(
                "            // {}: gpr {:?}, eflags {:#x}, eip {:#x}, deltas {:?}, fetch {}",
                g.name,
                fused.registers.gpr,
                fused.registers.eflags,
                fused.registers.eip,
                deltas,
                fetch,
            );
        }
    }
}
