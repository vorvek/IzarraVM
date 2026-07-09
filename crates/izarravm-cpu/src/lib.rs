use izarravm_bus::{BusAccessKind, BusError, BusWidth, CpuBus};
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use thiserror::Error;

mod control;
#[path = "core.rs"]
mod cpu_core;
mod flags;
mod fpu;
mod fpu_exec;
#[cfg(feature = "jit")]
mod jit;
mod memory;
mod mmx;
mod mmx_exec;
mod paging;
mod strings;
pub use fpu::X87;

pub use flags::PendingFlags;
pub(crate) use flags::{
    FLAG_AC, FLAG_AF, FLAG_CF, FLAG_DF, FLAG_ID, FLAG_IF, FLAG_IOPL, FLAG_NT, FLAG_OF, FLAG_PF,
    FLAG_SF, FLAG_TF, FLAG_VM, FLAG_ZF, LazyFlagOp, LazyFlags,
};

#[allow(unused_imports)]
pub(crate) use paging::{
    CodePageCache, DIRECT_PAGE_CACHE_LINES, DirectPageCache, DirectPageCacheEntry, FetchPageCache,
    PREFETCH_WINDOW_BYTES, PrefetchWindow, TLB_ENTRIES, TRACKED_WRITE_PAGES, Tlb, TlbEntry,
};

/// Gate for the opt-in `#UD` diagnostic trace (T1.5: making a reflected #UD
/// observable, see `CpuGsw::trace_ud_if_enabled`). Mirrors
/// `izarravm_machine::fault_trace_enabled` (same env var), cached after the
/// first check so a #UD storm costs one atomic load per fault rather than a
/// syscall. Measurement-only: this crate has no other env dependency, and the
/// gate is read only on the cold vector-6 delivery path, never per-instruction.
fn ud_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("IZARRAVM_FAULT_TRACE").is_some())
}

/// Gate for the differential-oracle per-instruction trace prototype
/// (`IZARRAVM_DIFF_TRACE`). Separate env var from `IZARRAVM_FAULT_TRACE`
/// deliberately: that one fires on a handful of cold fault paths, this one
/// fires on every retired instruction, so it must never share a code path
/// that could accidentally widen the fault trace's cost. Cached after the
/// first check, same pattern as `ud_trace_enabled`, so the steady-state cost
/// when unset is one relaxed load per instruction, not a syscall.
fn diff_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("IZARRAVM_DIFF_TRACE").is_some())
}

/// The one linear address the spike's forced JIT admission compiles
/// (IZARRAVM_JIT_REGION=<hex>, with or without 0x). `None` (the default) keeps the region
/// compiler fully inert: the production admission policy comes after the win exists. Cached on
/// first read, same pattern as `diff_trace_enabled`.
#[cfg(feature = "jit")]
fn jit_forced_region_lin() -> Option<u32> {
    static FORCED: OnceLock<Option<u32>> = OnceLock::new();
    *FORCED.get_or_init(|| {
        let value = std::env::var("IZARRAVM_JIT_REGION").ok()?;
        u32::from_str_radix(value.trim().trim_start_matches("0x"), 16).ok()
    })
}

/// Explicitly flush the diff-trace buffer. Call this once after a headless run loop
/// returns (any `run_until_*` call), NOT per instruction. Measured gap without this:
/// a run that fills the 64 KiB buffer but does not end on an exact flush boundary
/// loses its last partial buffer's worth of lines silently on process exit, which
/// is exactly the boot-sector tail a differential trace most needs (the run's last
/// few hundred instructions, right where a HLT or an interesting divergence sits).
pub fn flush_diff_trace() {
    if let Ok(mut w) = diff_trace_writer().lock() {
        let _ = w.flush();
    }
}

/// Process-wide buffered stderr handle for `emit_diff_trace_line`. A bare `eprintln!`
/// per instruction is one unbuffered syscall per line, which throttles a POST-length
/// trace to a few thousand lines/sec; wrapping stderr in a `BufWriter` amortizes that
/// to one syscall per buffer flush. Callers MUST call `flush_diff_trace()` after their
/// run loop returns -- see that function's doc for what silently goes missing without it.
fn diff_trace_writer() -> &'static Mutex<std::io::BufWriter<std::io::Stderr>> {
    static WRITER: OnceLock<Mutex<std::io::BufWriter<std::io::Stderr>>> = OnceLock::new();
    WRITER.get_or_init(|| {
        Mutex::new(std::io::BufWriter::with_capacity(
            64 * 1024,
            std::io::stderr(),
        ))
    })
}

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

// The full set of CR4 bits this GSW-586 (K6-class) persona defines at all, per the AMD-K6
// BIOS and Software Tools Developers Guide S: 3.7 (Control Register 4 (CR4) Extensions,
// Figure 13 / Table 19): VME(0), PVI(1), TSD(2), DE(3), PSE(4), MCE(6), GPE(7, the K6's
// name for what later became PGE). Bit 5 and bits 31:8 are reserved on real K6 hardware.
// Only TSD is behaviorally wired up (see CR4_TSD above); VME/PVI/DE/PSE/MCE/GPE are not
// emulated (the matching CPUID feature bits stay clear, matching the leaf-1 comment
// above), but a guest is still allowed to set/clear/read them back as inert storage --
// real firmware and memory managers probe CR4 this way. Bits outside this mask are
// rejected on write: the same K6 guide's MOV-to/from-CR4 exception table (S: MOV to and
// from CR4) lists a fault "If 1 is written to any reserved bits" in Real, Virtual-8086,
// and Protected mode alike, and the Pentium Vol. 3 instruction reference repeats it
// verbatim ("#GP(0)"/"Interrupt 13 if an attempt is made to write a 1 to any reserved
// bits of CR4"). So a reserved-bit write faults with #GP(0), the same as EFER/STAR.
const CR4_DEFINED_MASK: u32 = 0x0000_009f; // bits 0-4, 6-7

// DR6 (debug status): bits 0-3 are B0-B3 (which breakpoint condition matched), bit 13
// is BD (an attempt to access a debug register while GD was set), bit 14 is BS (the
// trap flag caused this #DB), bit 15 is BT (a task switch caused this #DB). 386 PRM
// ch12 (Debug Registers) documents bits 4-11 and 16-31 as defined-1 on reset and not
// guaranteed to read as written -- this core stores whatever is written to keep MOV DR6
// round-tripping simple, which is sufficient because breakpoint matching (the only thing
// that would actually set B0-B3/BS/BT/BD) is not implemented yet (ledger row 26,
// deferred). DR6 reset value per the PRM is 0xFFFF_0FF0.
const DR6_RESET: u32 = 0xffff_0ff0;

// DR7 (debug control): bits 0-7 are the L0-L3/G0-G3 local/global breakpoint enables,
// bit 8 LE and bit 9 GE are the (obsolete-by-386) exact-breakpoint-cycle enables, bit 13
// is GD (general detect -- traps any MOV DR), bits 16-31 are the four LEN/R-W condition
// fields for DR0-DR3. Bits 10, 12, 14-15 are hardwired: 386 PRM ch12 defines bit 10 as
// always 1; the others are reserved-as-0 here since this core does not model LE/GE cycle
// exactness. Reset value per the PRM is 0x0000_0400 (bit 10 set, everything else clear).
const DR7_RESET: u32 = 0x0000_0400;
const DR7_FIXED_ONE: u32 = 0x0000_0400; // bit 10, always reads back as 1

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
    #[error(
        "nested fault delivering vector {original_vector}: vector {nested_vector} \
         raised while building the exception frame"
    )]
    NestedFaultDuringDelivery {
        original_vector: u8,
        nested_vector: u8,
    },
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

// `repr(C)` pins the field order. SegmentRegister is nested in Registers (also repr(C)), and the
// JIT's offset guard test computes the eip/eflags offsets through `size_of::<[SegmentRegister; 6]>`,
// so its size must be stable. The derived layout the compiler chose happened to match this order;
// repr(C) freezes it so a future rustc cannot silently move a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
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

// `repr(C)` pins `gpr` at offset 0 within `Registers`, so the JIT's emitted native code can read
// and write `gpr[i]` as `[regs_ptr + 4*i]` without going through a Rust accessor. `registers` is
// the first field of `CpuGsw`; the dispatch passes a `*mut Registers` (derived from the cpu
// pointer) into the region entry for this purpose. The offset guard test in `mod tests` freezes
// the layout assumptions a rustc version bump could otherwise invalidate.
#[derive(Debug, Clone, PartialEq, Eq)]
#[repr(C)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlRegisters {
    pub cr0: u32,
    pub cr2: u32,
    pub cr3: u32,
    // CR4 arrived on the 586. Only TSD (bit 2) has a modeled effect here (it gates
    // RDTSC outside CPL 0); the rest is plain read/write storage so MOV CR4 round-
    // trips. Reset is all zero.
    pub cr4: u32,
    // DR0-DR3: linear breakpoint addresses (386 PRM ch12). Storage only -- no
    // breakpoint matching or #DB generation is implemented (ledger row 26, deferred).
    pub dr0_3: [u32; 4],
    // DR6 (debug status) and DR7 (debug control). See DR6_RESET/DR7_RESET/DR7_FIXED_ONE
    // above for the bit layout and reset values. DR4/DR5 alias these two on a 386 or on
    // any later part with CR4.DE clear (the default here, since DE is not behaviorally
    // wired up) -- see the 0x0f21/0x0f23 handlers.
    pub dr6: u32,
    pub dr7: u32,
}

impl Default for ControlRegisters {
    fn default() -> Self {
        ControlRegisters {
            cr0: 0,
            cr2: 0,
            cr3: 0,
            cr4: 0,
            dr0_3: [0; 4],
            dr6: DR6_RESET,
            dr7: DR7_RESET,
        }
    }
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

    /// Reported (L1 KB, L2 KB) cache for the level. Mirrors the machine's
    /// `CacheModel` geometry (`cache_geometry`) and now drives per-mode data-access
    /// timing through the cosmetic multi-tier cache, so it is no longer a no-timing
    /// readout. Still mirrors `GswMode::cache_kb` in the core so the CPU can answer
    /// the cache readout without a core dependency; the L2 is a motherboard cache
    /// module.
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
const CPU_PROFILE_GROUPS: usize = 16;

/// Host-side performance counters: pure diagnostics for `--headless-bench`, NOT
/// architectural state. Like the decode cache and TLB, this carries an always-equal
/// `PartialEq` so it is excluded from `CpuGsw` equality (conformance and golden-state
/// comparisons must ignore it). The only hot-path cost is one `instructions += 1` per
/// retired instruction; everything else increments at cold per-run sites.
#[derive(Debug, Clone, Default)]
pub struct PerfCounters {
    /// Instructions retired: every retired instruction routes through `finish_instruction`
    /// exactly once. Hardware-interrupt dispatch is charged separately and is NOT counted.
    pub instructions: u64,
    /// Instructions that required a fresh decode (a decode-cache miss in `fetch_decoded`).
    /// Decode-cache hit rate = 1 - decode_misses / instructions.
    pub decode_misses: u64,
    /// Calls to `run_straight_line` (one per machine batch entry). The denominator for
    /// the average straight-line run length = instructions / straight_line_runs.
    pub straight_line_runs: u64,
    /// Why each straight-line run ended (one increment per run). These say what limits
    /// batch length. They sum to `straight_line_runs` except for the rare run that ends in
    /// a propagated hard `CpuError` (a fatal error records no break reason).
    pub brk_decode_or_branch: u64, // next insn not cached / not straight-line / page cross
    /// TEMPORARY instrumentation: split brk_decode_or_branch into its three causes.
    pub brk_cont_decode_miss: u64, // continuation: next insn not in the decode cache
    pub brk_cont_not_continuable: u64, // continuation: next insn is not continuable (OUT/far/INT/HLT)
    pub brk_cont_page_cross: u64,      // continuation: next insn crosses a 4KB page boundary
    pub brk_step: u64,                 // port I/O or a pending HLE soft-int (requires_step_break)
    pub brk_interrupt: u64,            // an instruction made a maskable interrupt serviceable
    pub brk_cap: u64,                  // the run reached the scaled-clock cap
    pub brk_halt: u64,                 // the run executed HLT
    /// Decode-cache invalidation diagnostics. `decode_inval_cs_load` counts CS LOADS (which no
    /// longer flush the decode cache: the D bit is in the hit condition and the fetch limit is
    /// re-checked per hit); `decode_inval_smc` counts SMC whole-cache flushes
    /// (`note_code_write`); `decode_inval_other` counts everything else (paging/TLB flushes,
    /// A20, device DMA writes, ISA-level changes, direct-map changes). Diagnoses an
    /// invalidation storm: the Doom 586 census measured decode_hit pinned at 21% regardless of
    /// cache size by 326M per-CS-load whole-cache flushes.
    pub decode_inval_cs_load: u64,
    pub decode_inval_smc: u64,
    pub decode_inval_other: u64,
    /// Lines killed by the NARROW SMC path (a self-patch whose covering lines were
    /// invalidated individually, no whole-cache flush). decode_inval_smc keeps counting the
    /// global-flush fallbacks only, so the two together split the SMC write traffic.
    pub smc_narrow_kills: u64,
    /// Compiled-region executions (one per `run_region` call that passed its entry
    /// preconditions) and the instructions those executions retired. `jit_region_insns /
    /// jit_region_entries` is the mean instructions per region entry; a Doom A/B run asserts
    /// `jit_region_entries > 0` to prove the region actually executed. Always present (zero
    /// without the `jit` feature) so perf-row consumers need no feature gymnastics.
    pub jit_region_entries: u64,
    pub jit_region_insns: u64,
    /// Times the compiled-region table hit its capacity and was dropped wholesale (a coarse GC;
    /// see `JIT_REGION_TABLE_CAP`). Nonzero means the working set of hot loops exceeded the cap and
    /// the JIT is re-warming - a signal to raise the cap or add per-entry eviction. Zero on the
    /// single-phase anchors. Always present (zero without the `jit` feature).
    pub jit_table_clears: u64,
    /// Byte loads served by the native cost-fold LOAD probe (a page-cache HIT run entirely in emitted
    /// code, skipping the `region_step` call). Bumped natively by the emitted probe. Zero unless
    /// `IZARRAVM_JIT_FOLD` is on AND the block is fold-eligible (Approximate class, unpaged, flat DS);
    /// on the paged Doom/Quake anchors this stays ~0 (the unpaged probe is gated off), which is the
    /// A/B signal that this first cut is Doom-inert. Always present (zero without the `jit` feature).
    pub jit_native_load_hits: u64,
    /// Byte stores served by the native cost-fold STORE probe (a `data_write_pages` HIT written in
    /// emitted code + the `record_write_page`/`note_code_write` finish call, skipping the `region_step`
    /// call). Same gating + Doom-inertness as `jit_native_load_hits`.
    pub jit_native_store_hits: u64,
    /// For paged fold investigation (low native hit rate on Doom): bumped in the emitted
    /// paged probe right after successful TLB translate (before the physical page-cache probe).
    /// Compare to jit_native_*_hits to see TLB hit rate within admitted paged fold slots.
    /// Also helps separate TLB thrash from direct-page cache thrash.
    pub jit_paged_tlb_successes: u64,
    pub data_direct_reads: u64,
    pub data_slow_reads: u64,
    pub data_direct_writes: u64,
    pub data_slow_writes: u64,
    pub direct_page_hits: u64,
    pub direct_page_misses: u64,
    pub direct_data_pointer_reads: u64,
    pub direct_data_pointer_writes: u64,
    pub fetch_page_hits: u64,
    pub fetch_page_misses: u64,
    pub slow_prefetch_refills: u64,
    pub direct_map_invalidations: u64,
    pub rep_string_iterations: u64,
    pub rep_string_fast_iterations: u64,
    pub flag_materializations: u64,
    pub cache_tier_lookups: u64,
    /// V86 trap tax measurement (dev_docs/2026-07-02-v86-trap-tax): every entry into
    /// the ring-0 monitor via vector 13 (a V86 sensitive-instruction #GP or a real
    /// IRQ5), counted at `deliver_exception`. One "trip" per TOKAEMM round-trip.
    /// Combine with `brk_step` (already tracked above) for the batch-breaking
    /// share the trap tax measures: `brk_step / monitor_trips_vec13` is the mean
    /// number of port accesses that ended a batch per trip (was ~2, the vec13
    /// PIC OCW3 select write and its readback; near 0 after the Part 1 fix).
    pub monitor_trips_vec13: u64,
    /// Guest CORE clocks charged to instructions that retired while
    /// `is_ring0_protected()` was true: the V86-#GP-entry-to-IRETD-back residency
    /// in core-clock terms (the just-delivered trap's own exception charge is
    /// included, since the attribution check runs after the inline
    /// `deliver_exception`; the IRETD back into V86 lands in the guest bucket, a
    /// one-instruction undercount per trip). NOTE this is core clocks only: the
    /// monitor's ISA-priced port-access WAIT states travel through the bus-clock
    /// trace, not core clocks, and are not in this bucket.
    pub monitor_resident_core_clocks: u64,
}

impl PartialEq for PerfCounters {
    // Diagnostic-only: never affects CpuGsw equality (conformance / goldens ignore it).
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}
impl Eq for PerfCounters {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuProfileBucket {
    pub name: &'static str,
    pub instructions: u64,
    pub guest_core_clocks: u64,
    pub sample_wall_ns: u64,
    pub samples: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuOpcodeProfileBucket {
    pub opcode: u16,
    pub group: &'static str,
    pub instructions: u64,
    pub guest_core_clocks: u64,
    pub sample_wall_ns: u64,
    pub samples: u64,
    pub register_instructions: u64,
    pub memory_instructions: u64,
    pub register_samples: u64,
    pub memory_samples: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CpuProfileSnapshot {
    pub sample_stride: u64,
    pub groups: Vec<CpuProfileBucket>,
    pub opcodes: Vec<CpuOpcodeProfileBucket>,
    /// Hottest sampled instruction linear addresses, `(linear, samples)`, descending; top 64.
    pub hot_addrs: Vec<(u32, u64)>,
    /// SMC whole-cache flush sources, `(physical 64-byte block, flushes)`, descending; top 16.
    pub smc_flush_blocks: Vec<(u32, u64)>,
}

#[derive(Debug, Clone, Copy, Default)]
struct CpuProfileBucketState {
    instructions: u64,
    guest_core_clocks: u64,
    sample_wall_ns: u64,
    samples: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CpuProfileOperandForm {
    None,
    Register,
    Memory,
}

impl CpuProfileOperandForm {
    #[inline]
    fn from_insn(insn: &DecodedInsn) -> Self {
        match insn.operand {
            Some(DecodedOperand::Reg(_)) => Self::Register,
            Some(DecodedOperand::Mem(_)) => Self::Memory,
            None => Self::None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CpuOpcodeProfileBucketState {
    group: DecodeGroup,
    bucket: CpuProfileBucketState,
    register_instructions: u64,
    memory_instructions: u64,
    register_samples: u64,
    memory_samples: u64,
}

#[derive(Clone)]
struct CpuProfileState {
    enabled: bool,
    sample_stride: u64,
    until_sample: u64,
    groups: [CpuProfileBucketState; CPU_PROFILE_GROUPS],
    opcodes: std::collections::HashMap<u16, CpuOpcodeProfileBucketState>,
    /// Sampled instruction linear addresses (one entry per SAMPLED instruction, so one hash op
    /// per stride, not per instruction): the hot-loop finder for the JIT's region selection.
    addrs: std::collections::HashMap<u32, u64>,
    /// SMC whole-cache flush sources: 64-byte physical block -> flush count (every flush, not
    /// sampled - flushes are rare enough). Locates the code/data byte sharing behind a residual
    /// SMC flush storm (Doom: 3.9M flushes/timedemo survive the stale-mark clear).
    smc_flush_blocks: std::collections::HashMap<u32, u64>,
}

impl Default for CpuProfileState {
    fn default() -> Self {
        Self {
            enabled: false,
            sample_stride: 1,
            until_sample: 1,
            groups: [CpuProfileBucketState::default(); CPU_PROFILE_GROUPS],
            opcodes: std::collections::HashMap::new(),
            addrs: std::collections::HashMap::new(),
            smc_flush_blocks: std::collections::HashMap::new(),
        }
    }
}

impl PartialEq for CpuProfileState {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for CpuProfileState {}

impl std::fmt::Debug for CpuProfileState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CpuProfileState")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CpuGsw {
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
    // Core clocks charged by prior instructions in the CURRENT run_straight_line
    // run, not including the in-flight instruction. Mirrors run_straight_line's
    // local `total` at the point just before that instruction executes, so a port
    // read reached from inside `execute_decoded` can read it directly without a
    // new parameter threaded through every intermediate call (fetch_decoded,
    // execute_decoded, execute_port_io_decoded, ...). Set once per instruction at
    // the top of run_straight_line's loop body / cycle_no_interrupt_check; read by
    // CpuBus::read_io call sites. Zero-initialized by the struct's derive(Default);
    // a hand-written Default impl must keep it 0 (cycle_no_interrupt_check's
    // reset-to-0 assumes a fresh CPU starts there). See dev_docs/
    // 2026-07-02-p4a-lazy-port-device-time-plan.md Task 0.2.
    core_clocks_so_far: u64,
    // Fractional remainder carried by the per-level cycle scaling so the cheap
    // ops do not round to zero. Reset on a level change. See scale_clocks.
    timing_rem: u64,
    // Fractional remainder carried by the per-mode FP-clock scaling (scale_fp_clocks).
    // Reset on a level change alongside timing_rem. See fp_timing.
    fp_rem: u64,
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
    data_read_pages: DirectPageCache,
    data_write_pages: DirectPageCache,
    fetch_page: FetchPageCache,
    written_pages: [Option<u32>; TRACKED_WRITE_PAGES],
    /// How many leading slots of `written_pages` are occupied (0 when no memory writes
    /// happened this instruction, the common case for register-only ops). Lets
    /// `begin_instruction` clear only the used slots instead of an unconditional 64-byte
    /// memset every instruction.
    written_count: u8,
    written_pages_overflow: bool,
    // Direct-mapped cache of decoded instructions keyed by linear EIP. Skips re-decoding hot-loop
    // bytes; a generation counter (inside) invalidates it on any change that could alter a decode.
    // Transparent accelerator, excluded from equality and reset on clone. See DecodeCache.
    decode_cache: DecodeCache,
    /// Compiled loop-regions (feature `jit`). Installed by stamping a 1-based index into the
    /// entry address's `DecodeLine::jit_region`; the table is a transparent accelerator with
    /// the same equality/clone exclusions as the decode cache. See `jit::region`.
    #[cfg(feature = "jit")]
    jit_regions: jit::RegionTable,
    /// Host-side performance counters (diagnostics for `--headless-bench`). Excluded from
    /// equality via `PerfCounters`'s always-equal `PartialEq`, like the decode cache.
    perf: PerfCounters,
    /// Optional host-side profiling. Off for normal execution and excluded from equality.
    profile: CpuProfileState,
    /// Deferred arithmetic flags (lazy-flags optimization). While not none, the six arithmetic-flag
    /// bits in `registers.eflags` are stale. CpuGsw equality is flag-representation-sensitive while a
    /// deferral is outstanding; real flag comparisons go through `flag()` / `eflags()`, which
    /// materialize. (CpuGsw `==` is currently unused.)
    ///
    /// Stored as the #[repr(C)] PendingFlags form so native emitted code can write it directly
    /// (v2 inlining). Legacy LazyFlags is only for conversion during the final migration of
    /// a few sites. Interpreter wall must not regress (A/B first).
    pending_flags: PendingFlags,
    /// Cached `CR0.AM && EFLAGS.AC`, so the per-data-access #AC gate in `check_alignment`
    /// is a single bool test instead of two register loads. Pure derived state, recomputed
    /// by `recompute_alignment_armed` at every writer that can change either bit (see that
    /// method for the chokepoint inventory). `Default`/`reset` leave it `false`, matching
    /// the reset images (CR0 without AM, EFLAGS 0x2); `Clone` copies it consistently.
    alignment_armed: bool,
    /// Current privilege level. Per the 386 PRM, CPL is a *cached* quantity carried in
    /// (the hidden part of) CS, updated only at defined transition points -- it is not a
    /// live formula over the current CS selector. Updated at: real mode / PE clear (0);
    /// far JMP/CALL/RETF/IRET same- and inter-privilege transfers; call/task gates and
    /// `task_switch` (to the target DPL); IRET-into-V86 (3); `deliver_exception` (to the
    /// gate's target level, before the frame-push sequence begins -- see that function);
    /// SYSCALL/SYSRET; reset (0). `current_privilege_level` returns this field directly;
    /// see that method for why a live `CS.selector & 3` read is wrong during exception
    /// delivery out of a V86 source (the source CS can carry arbitrary low bits before
    /// the frame's own CS is loaded, which must not be mistaken for the CPL the pushes
    /// execute under).
    cpl: u8,
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
struct FastStringResult {
    iterations: u32,
    stop: bool,
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
    /// AL/AX,imm forms that share the 0xa8/0xa9 neighbourhood are NOT string ops and route to Misc.
    StringOps,
    /// The port I/O block (task A9): IN AL/AX/EAX from a byte-immediate port (0xe4/0xe5), OUT to a
    /// byte-immediate port (0xe6/0xe7), IN AL/AX/EAX from the DX port (0xec/0xed), and OUT to the DX
    /// port (0xee/0xef). The imm8 forms (0xe4-0xe7) carry a single port-number byte after the opcode;
    /// `decode` reads and stores it in `insn.imm`. The DX forms (0xec-0xef) carry no extra bytes —
    /// the port number comes from the DX register at execute time. None carry a ModRM. The executor
    /// calls `bus.read_io` / `bus.write_io` on the existing port-dispatch path (byte width for the
    /// AL forms, operand-size width for the AX/EAX forms), so `io_touched` is set exactly as before.
    /// The string I/O ops INS/OUTS (0x6c-0x6f) are NOT here — they route to Misc.
    PortIo,
    /// The two-byte bit-manipulation block (task A10): BT/BTS/BTR/BTC with a reg bit index
    /// (0F A3/AB/B3/BB), BT/BTS/BTR/BTC with an imm8 bit index (0F BA group 8, /4../7), BSF/BSR
    /// (0F BC/BD), the double-precision shifts SHLD/SHRD (0F A4/A5/AC/AD, count imm8 or CL),
    /// CMPXCHG (0F B0/B1), and XADD (0F C0/C1). Every one is a ModRM r/m form, so `decode` parses
    /// the ModRM + addressing descriptor for all of them; only the imm8-count forms (0F BA, and the
    /// SHLD/SHRD imm8 variants 0F A4/AC) carry an immediate after the ModRM, which `decode` fetches
    /// into `insn.imm`. The executor reuses `bit_string_op` (so the classic BT-memory bit-offset
    /// addressing — a reg bit index that walks past the operand width into an adjacent memory
    /// element — is computed at execute from the LIVE reg index, never cached at decode),
    /// `double_shift`, `alu_sub`, and `alu_add` verbatim, so the CF/ZF/flag semantics live in one
    /// place. Folded into `insn.opcode` as 0x0F00 | second by the two-byte convention and dispatched
    /// off the full u16 (the `as u8` low byte of 0x0Fa4/a5/b0/b1/c0/c1 would alias single-byte
    /// opcodes).
    BitManip,
    /// The two-byte conditional-move / set-on-condition / two-operand IMUL block (task A11):
    /// CMOVcc reg, r/m (0F 40-0F 4F — 586-class), SETcc r/m8 (0F 90-0F 9F — 386-class), and
    /// IMUL reg, r/m (0F AF — 386-class). Every one is a ModRM r/m form with no immediate after
    /// the ModRM, so `decode` parses the ModRM + addressing descriptor and stores it (no `imm`
    /// fetch). The executor dispatches off the FULL u16 (`insn.opcode`, never narrowed to u8 first)
    /// and reuses `self.condition(insn.opcode as u8 & 0x0f)` for the condition codes and
    /// `self.imul_truncated` for the two-operand IMUL. CMOVcc reads the source r/m even when the
    /// condition is false (memory faults still fire), but writes the destination register only
    /// when the condition holds. SETcc is always byte-wide and uses `write_operand_u8`. The ISA
    /// gates (386+ for SETcc/IMUL, 586+ for CMOVcc) are applied once in `decode`'s
    /// `check_two_byte_isa_gate`; the executor does NOT re-gate.
    CondMove,
    /// The system / descriptor-table / segment-load block (task A12), a MIX of two-byte (0F) and
    /// single-byte opcodes that read/write control, descriptor-table, and segment state. The members
    /// are: the descriptor groups 0F 00 group 6 (SLDT/STR/LLDT/LTR/VERR/VERW) and 0F 01 group 7
    /// (SGDT/SIDT/LGDT/LIDT/SMSW/LMSW/INVLPG), each a ModRM whose `reg` field is the /ext sub-op
    /// selector; LAR (0F 02) and LSL (0F 03), which read descriptor access-rights / limit into a
    /// register; CLTS (0F 06), which clears CR0.TS with no encoded operand; MOV reg,CR / MOV CR,reg
    /// (0F 20/22), whole-32-bit control-register moves whose ModRM is a register form (`mod` treated
    /// as 3, `reg` selects the CR number); LSS/LFS/LGS (0F B2/B4/B5), which load a far pointer
    /// m16:16/32 into SS/FS/GS + reg (mod=3 is #UD; LSS additionally arms the one-instruction
    /// interrupt shadow, the same as MOV SS/POP SS); and the single-byte BOUND r,m (0x62; #BR on
    /// an out-of-range index, mod=3 is #UD) and LES/LDS (0xC4/0xC5; load a far pointer m16:16/32
    /// into ES/DS + reg, mod=3 is #UD).
    ///
    /// Every ModRM form has its ModRM + addressing descriptor parsed once in `decode` (instruction
    /// bytes only, so it stays cacheable); none carry an immediate after the ModRM. The CR/segment/
    /// descriptor state changes run through the EXISTING leaf helpers verbatim (`load_segment`,
    /// `load_ldtr`, `load_tr`, `verify_segment`, `store_descriptor_table`, `flush_tlb_and_code_caches`,
    /// `try_read_descriptor`/`descriptor_accessible`), so the TLB/code-cache invalidation hooks that
    /// Stage B depends on still fire exactly as before. The far pointer for LES/LDS is read FROM
    /// MEMORY at execute time (against live registers), never pre-read at decode; LSS/LFS/LGS
    /// (0F B2/B4/B5) share that same far-pointer shape, loading SS/FS/GS instead of ES/DS, and
    /// LSS additionally arms the one-instruction interrupt shadow (the same shadow MOV SS/POP SS
    /// arm) so a following instruction cannot be interrupted between the offset and selector
    /// halves of the pointer settling. Folded into `insn.opcode` as 0x0F00 | second for the 0F
    /// forms and dispatched off the full u16. The genuinely-unimplemented neighbours (0F 21/23 MOV
    /// reg,DR / MOV DR,reg, 0x63 ARPL) are NOT routed here -- they stay on Fallback / TwoByteFallback
    /// (the fused path #UDs them as `UnsupportedOpcode` / `UnsupportedTwoByteOpcode`).
    SystemSeg,
    /// The x87 FPU block (task A13): the eight escape opcodes 0xD8-0xDF plus WAIT/FWAIT (0x9B).
    /// Each escape opcode carries a ModRM whose `mod` field selects the form — `mod != 3` is a
    /// memory operand (parsed in `decode` as a normal addressing descriptor, instruction bytes
    /// only) and `mod == 3` operates on the FPU stack registers (the ModRM byte alone, no
    /// addressing descriptor, exactly as the fused handler treated it). The `reg` field (and, for
    /// the register forms, the full ModRM byte) selects the x87 operation. None carry an immediate.
    /// 0x9B WAIT has no ModRM at all. The whole group is a THIN wrapper: the executor reproduces the
    /// fused handler's pending-#MF gate, then calls the EXISTING `execute_fpu_register` /
    /// `execute_fpu_memory` (for the escapes) or runs the WAIT #MF check (for 0x9B) verbatim — the
    /// entire x87 stack/control/status logic stays in those leaf helpers. The only change is WHERE
    /// the ModRM is fetched: once, in `decode`.
    Fpu,
    /// The heterogeneous catch-all of every remaining IMPLEMENTED one-off opcode (task A14), a MIX
    /// of single-byte and two-byte (0F) forms that did not fit a themed group. The members are:
    ///   - BCD adjust: DAA (0x27), DAS (0x2F), AAA (0x37), AAS (0x3F) — no encoded operand.
    ///   - AAM (0xD4) / AAD (0xD5) — each carries an imm8 base, fetched by `decode` into `insn.imm`.
    ///   - SALC (0xD6, undocumented) and XLAT (0xD7) — no encoded operand; XLAT reads [seg:BX+AL]
    ///     from memory at execute time against the live registers.
    ///   - TEST AL,imm8 (0xA8, imm8 into `insn.imm`) and TEST AX/EAX,imm (0xA9, operand-size imm).
    ///   - three-operand IMUL: IMUL r,r/m,imm16/32 (0x69) and IMUL r,r/m,imm8 (0x6B) — a ModRM r/m
    ///     plus an immediate (`decode` parses the ModRM + addressing descriptor, then fetches the imm).
    ///   - string port I/O: INSB/INSW (0x6C/0x6D), OUTSB/OUTSW (0x6E/0x6F) — implicit operands; the
    ///     executor is a thin call to the existing `run_string` (REP/DF/segment-override stay there).
    ///   - HLT (0xF4) — sets the halted state.
    ///   - the two-byte system/serializing/CPU-id ops with no encoded operand: SYSCALL (0F 05),
    ///     SYSRET (0F 07), INVD/WBINVD (0F 08/09), WRMSR (0F 30), RDTSC (0F 31), RDMSR (0F 32),
    ///     CPUID (0F A2), BSWAP r32 (0F C8-CF).
    ///   - CMPXCHG8B m64 (0F C7 /1) — a ModRM r/m form (`decode` parses the ModRM + descriptor).
    ///   - the MMX integer-SIMD block (the `is_mmx_two_byte` opcodes): EMMS (0F 77, no ModRM); the
    ///     shift-by-imm forms (0F 71/72/73, a ModRM whose `rm` is the register plus a trailing imm8);
    ///     MOVD/MOVQ and every Pxxx mm,mm/m64 — all ModRM r/m forms (`decode` parses the ModRM +
    ///     descriptor; the imm8 is fetched only for 0F 71/72/73).
    ///
    /// `decode` parses each form's ModRM + addressing descriptor (instruction bytes only, so it stays
    /// cacheable) and its immediate exactly as the fused handler did, so the byte budget — and thus the
    /// fetch clocks — is byte-identical. The executor reuses the existing BCD/`imul_truncated`/
    /// `run_string`/`execute_mmx_decoded`/CPUID/RDTSC/`syscall`/halt leaf logic verbatim; the only
    /// change is WHERE the ModRM/immediate is fetched (once, in `decode`). The 0F forms are folded
    /// into `insn.opcode` as 0x0F00 | second and dispatched off the full u16. The genuinely
    /// unimplemented neighbours (single-byte 0x63 ARPL / 0xF1; 0F 21/23 MOV DR; 0F AA RSM; the
    /// other unmapped 0F bytes) are NOT routed here — they stay on Fallback / TwoByteFallback and
    /// still #UD as `UnsupportedOpcode` / `UnsupportedTwoByteOpcode`.
    Misc,
    /// A single-byte opcode with no split implementation. After Stage A this is a pure dead-end: the
    /// only members are the genuinely-unimplemented 0x63 (ARPL) and 0xF1 (ICEBP), plus — as a
    /// decode-bug guard — any prefix byte `read_prefixes` did not consume. `execute_decoded` raises
    /// `UnsupportedOpcode` for them (via `unsupported_single_byte_opcode`); `decode` parses nothing
    /// extra. No IMPLEMENTED opcode routes here — `every_implemented_opcode_routes_off_the_legacy_fallback`
    /// locks that invariant.
    Fallback,
    /// A two-byte (0F) opcode handled by `execute_two_byte` rather than a dedicated split group
    /// (`opcode & 0xff00 == 0x0f00`). `decode` already folded the second byte into `insn.opcode` as
    /// 0x0F00 | second, read + charged it, and applied the ISA gate; `execute_decoded` hands the
    /// second byte (`insn.opcode as u8`) straight to `execute_two_byte` without re-reading or
    /// re-gating. Most members #UD as `UnsupportedTwoByteOpcode` (the unimplemented 0F bytes), but a
    /// few are explicitly handled there (e.g. 0F AA RSM, which #UDs because no SMM is modeled).
    /// Distinct from `Fallback` so the routing predicate (`& 0xff00 == 0x0f00`) lives only in
    /// `route_group` and the `as u8` narrowing can never alias a 0F opcode onto a single-byte one
    /// (no arm-ordering dependence).
    TwoByteFallback,
}

impl DecodeGroup {
    const ALL: [DecodeGroup; CPU_PROFILE_GROUPS] = [
        DecodeGroup::Alu,
        DecodeGroup::DataMove,
        DecodeGroup::Stack,
        DecodeGroup::Group,
        DecodeGroup::Branch,
        DecodeGroup::ControlFlow,
        DecodeGroup::FlagsMisc,
        DecodeGroup::StringOps,
        DecodeGroup::PortIo,
        DecodeGroup::BitManip,
        DecodeGroup::CondMove,
        DecodeGroup::SystemSeg,
        DecodeGroup::Fpu,
        DecodeGroup::Misc,
        DecodeGroup::Fallback,
        DecodeGroup::TwoByteFallback,
    ];

    const fn profile_index(self) -> usize {
        match self {
            DecodeGroup::Alu => 0,
            DecodeGroup::DataMove => 1,
            DecodeGroup::Stack => 2,
            DecodeGroup::Group => 3,
            DecodeGroup::Branch => 4,
            DecodeGroup::ControlFlow => 5,
            DecodeGroup::FlagsMisc => 6,
            DecodeGroup::StringOps => 7,
            DecodeGroup::PortIo => 8,
            DecodeGroup::BitManip => 9,
            DecodeGroup::CondMove => 10,
            DecodeGroup::SystemSeg => 11,
            DecodeGroup::Fpu => 12,
            DecodeGroup::Misc => 13,
            DecodeGroup::Fallback => 14,
            DecodeGroup::TwoByteFallback => 15,
        }
    }

    const fn profile_name(self) -> &'static str {
        match self {
            DecodeGroup::Alu => "alu",
            DecodeGroup::DataMove => "data_move",
            DecodeGroup::Stack => "stack",
            DecodeGroup::Group => "group",
            DecodeGroup::Branch => "branch",
            DecodeGroup::ControlFlow => "control_flow",
            DecodeGroup::FlagsMisc => "flags_misc",
            DecodeGroup::StringOps => "string_ops",
            DecodeGroup::PortIo => "port_io",
            DecodeGroup::BitManip => "bit_manip",
            DecodeGroup::CondMove => "cond_move",
            DecodeGroup::SystemSeg => "system_seg",
            DecodeGroup::Fpu => "fpu",
            DecodeGroup::Misc => "misc",
            DecodeGroup::Fallback => "fallback",
            DecodeGroup::TwoByteFallback => "two_byte_fallback",
        }
    }
}

/// Whether a decoded group is safe to run as a cached continuation: it either falls through or is a
/// relative branch whose target is just the next live EIP. It must not touch a port, change CS or
/// system state, or halt. String ops (REP included) are admitted one level up in `block_continuable`
/// with their own justification. The executor still checks step breaks, interrupts, faults, and the
/// batch clock cap after every instruction.
fn block_straight_line(g: DecodeGroup) -> bool {
    matches!(
        g,
        DecodeGroup::Alu
            | DecodeGroup::DataMove
            | DecodeGroup::Stack
            | DecodeGroup::Group
            | DecodeGroup::Branch
            | DecodeGroup::FlagsMisc
            | DecodeGroup::BitManip
            | DecodeGroup::CondMove
            | DecodeGroup::Fpu
    )
}

/// Whether a decoded instruction may run as a cached continuation. Group-keyed for the
/// straight-line groups (`block_straight_line`); additionally admits, BY OPCODE within the
/// `ControlFlow` group, the forms that cannot halt, touch a port, or change CS:
/// near RET (0xC3), near RET imm16 (0xC2), and the 0xFF group-5 forms that stay near —
/// the plain fall-through INC r/m (/0), DEC r/m (/1), and PUSH r/m (/6) plus the near
/// indirect CALL (/2) and JMP (/4). (The bench probe showed /6 PUSH r/m alone was ~360k
/// of whetstone's ~360k run breaks: procedure-argument pushes, not transfers at all.)
/// Still ending the run: far RET (0xCA/0xCB), the far directs (0x9A/0xEA), the far
/// indirects (0xFF /3 and /5), the undefined /7 (#UD path), and INT3/INT n/INTO
/// (0xCC-0xCE) / IRET (0xCF) — they load CS or dispatch through the IDT. The continuation
/// follows the new EIP exactly as taken relative branches already do; every
/// per-continuation break check (step break, interrupt transition, clock cap,
/// decode-cache re-peek at the new linear EIP, page-local decode) is unchanged, and a
/// faulting stack read or segment-limit hit still routes through `finish_instruction`'s
/// rewind-and-deliver exactly as on the one-instruction path.
///
/// P4a Task 1.3 additionally admits the IN forms (0xe4 IN AL,imm8; 0xe5 IN AX/EAX,imm8;
/// 0xec IN AL,DX; 0xed IN AX/EAX,DX) within `DecodeGroup::PortIo`, but ONLY when `level`
/// is in the Approximate timing class (I486/I586): a lazy port read (`MachineBus::read_io`)
/// no longer sets `io_touched` for the VGA status ports, so an IN reaching those ports no
/// longer needs to end the run to keep device state exact, letting a poll loop chain as
/// continuations instead of paying a full run restart every iteration. The OUT forms
/// (0xe6/0xe7/0xee/0xef) stay terminators: a write always sets `io_touched` (no lazy write
/// path exists), so admitting them would end the run right after anyway while widening the
/// blast radius for no benefit. INS/OUTS stay terminators too.
///
/// The same Approximate-class gate also admits the TEST accumulator-immediate forms
/// (0xa8 TEST AL,imm8; 0xa9 TEST AX/EAX,imm) within `DecodeGroup::Misc`. Their Misc
/// routing is a decode-classification artifact of the odd opcode neighborhood they share
/// with the BCD/string/HLT one-offs (see `route_group`'s A14 block), not a semantic
/// property: they are pure flag-writing ALU ops (AND-for-flags, no write-back), no memory,
/// no ModRM, no port, no control transfer, and their immediate is fully pre-parsed at
/// decode -- strictly simpler than the ALU forms `block_straight_line` already admits.
/// They matter because the canonical vretrace poll idiom is `IN; TEST AL,imm8; Jcc; JMP`:
/// with IN admitted but TEST still a terminator, every poll iteration ends its run at the
/// TEST and pays a full run restart, which measured at about the cost of the batch
/// epilogue the lazy port read had just eliminated (P4a A/B, poll-3da flat at 0.204/0.051).
/// NO other Misc opcode is admitted: the BCD adjusts, AAM/AAD (#DE path), SALC/XLAT
/// (memory read), INS/OUTS (port + string), and HLT all stay terminators.
///
/// Gated on `level` (not a runtime bus flag) so the Accurate class (I286/I386) keeps
/// BYTE-IDENTICAL batch structure to before this task: `block_continuable` is called once
/// per decode, and `CpuGsw::set_level` unconditionally invalidates the decode cache
/// (`self.decode_cache.invalidate()`), so every decode-cache line is re-decoded -- and this
/// admission re-resolved -- after any level change. There is no stale-entry window where an
/// I286-level IN or TEST could carry an I586-level admission decision forward.
fn block_continuable(
    group: DecodeGroup,
    opcode: u16,
    modrm: Option<ModRm>,
    level: CpuLevel,
) -> bool {
    if block_straight_line(group) {
        return true;
    }
    // String ops (MOVS/CMPS/STOS/LODS/SCAS, REP or not) fall through, never touch a port
    // (INS/OUTS are Misc and stay terminators), and never change CS. The REP forms are
    // safe too because `run_string` executes the WHOLE repeat atomically inside one
    // instruction dispatch (no mid-instruction yield or eip-resume seam exists), so the
    // interrupt window is the instruction boundary in both run positions — identical to
    // the per-instruction loop. A faulting iteration routes through finish_instruction's
    // rewind exactly as on the one-instruction path.
    if group == DecodeGroup::StringOps {
        return true;
    }
    if group == DecodeGroup::PortIo {
        // Only the IN forms, only in the Approximate class; see the doc comment above.
        return level >= CpuLevel::I486 && matches!(opcode, 0xe4 | 0xe5 | 0xec | 0xed);
    }
    if group == DecodeGroup::Misc {
        // Only TEST AL/AX/EAX,imm, only in the Approximate class; see the doc
        // comment above. Everything else in the Misc bucket stays a terminator.
        return level >= CpuLevel::I486 && matches!(opcode, 0xa8 | 0xa9);
    }
    if group != DecodeGroup::ControlFlow {
        return false;
    }
    matches!(opcode, 0xc2 | 0xc3)
        || (opcode == 0xff && matches!(modrm, Some(m) if matches!(m.reg, 0 | 1 | 2 | 4 | 6)))
}

impl CpuProfileState {
    fn enable(&mut self, sample_stride: u64) {
        *self = Self {
            enabled: true,
            sample_stride: sample_stride.max(1),
            until_sample: 1,
            groups: [CpuProfileBucketState::default(); CPU_PROFILE_GROUPS],
            opcodes: std::collections::HashMap::new(),
            addrs: std::collections::HashMap::new(),
            smc_flush_blocks: std::collections::HashMap::new(),
        };
    }

    fn disable(&mut self) {
        *self = Self::default();
    }

    #[inline]
    fn sample_start(&self) -> Option<std::time::Instant> {
        (self.enabled && self.until_sample == 1).then(std::time::Instant::now)
    }

    #[inline]
    fn record(
        &mut self,
        group: DecodeGroup,
        opcode: u16,
        form: CpuProfileOperandForm,
        guest_core_clocks: u64,
        start: Option<std::time::Instant>,
        lin: u32,
    ) {
        if !self.enabled {
            return;
        }
        let sample_wall_ns = start.map(|start| duration_ns_u64(start.elapsed()));
        if sample_wall_ns.is_some() {
            *self.addrs.entry(lin).or_insert(0) += 1;
        }
        let bucket = &mut self.groups[group.profile_index()];
        bucket.instructions += 1;
        bucket.guest_core_clocks += guest_core_clocks;
        if let Some(sample_wall_ns) = sample_wall_ns {
            bucket.samples += 1;
            bucket.sample_wall_ns = bucket.sample_wall_ns.saturating_add(sample_wall_ns);
        }

        let opcode_bucket = self
            .opcodes
            .entry(opcode)
            .or_insert(CpuOpcodeProfileBucketState {
                group,
                bucket: CpuProfileBucketState::default(),
                register_instructions: 0,
                memory_instructions: 0,
                register_samples: 0,
                memory_samples: 0,
            });
        opcode_bucket.bucket.instructions += 1;
        opcode_bucket.bucket.guest_core_clocks += guest_core_clocks;
        match form {
            CpuProfileOperandForm::Register => opcode_bucket.register_instructions += 1,
            CpuProfileOperandForm::Memory => opcode_bucket.memory_instructions += 1,
            CpuProfileOperandForm::None => {}
        }
        if let Some(sample_wall_ns) = sample_wall_ns {
            opcode_bucket.bucket.samples += 1;
            opcode_bucket.bucket.sample_wall_ns = opcode_bucket
                .bucket
                .sample_wall_ns
                .saturating_add(sample_wall_ns);
            match form {
                CpuProfileOperandForm::Register => opcode_bucket.register_samples += 1,
                CpuProfileOperandForm::Memory => opcode_bucket.memory_samples += 1,
                CpuProfileOperandForm::None => {}
            }
        }
        self.until_sample = if self.until_sample <= 1 {
            self.sample_stride
        } else {
            self.until_sample - 1
        };
    }

    fn snapshot(&self) -> CpuProfileSnapshot {
        let mut opcodes = self
            .opcodes
            .iter()
            .map(|(&opcode, state)| CpuOpcodeProfileBucket {
                opcode,
                group: state.group.profile_name(),
                instructions: state.bucket.instructions,
                guest_core_clocks: state.bucket.guest_core_clocks,
                sample_wall_ns: state.bucket.sample_wall_ns,
                samples: state.bucket.samples,
                register_instructions: state.register_instructions,
                memory_instructions: state.memory_instructions,
                register_samples: state.register_samples,
                memory_samples: state.memory_samples,
            })
            .collect::<Vec<_>>();
        opcodes.sort_by_key(|bucket| bucket.opcode);
        let mut hot_addrs = self
            .addrs
            .iter()
            .map(|(&lin, &samples)| (lin, samples))
            .collect::<Vec<_>>();
        hot_addrs.sort_by_key(|&(lin, samples)| (std::cmp::Reverse(samples), lin));
        hot_addrs.truncate(64);
        let mut smc_flush_blocks = self
            .smc_flush_blocks
            .iter()
            .map(|(&block, &flushes)| (block, flushes))
            .collect::<Vec<_>>();
        smc_flush_blocks.sort_by_key(|&(block, flushes)| (std::cmp::Reverse(flushes), block));
        smc_flush_blocks.truncate(16);
        CpuProfileSnapshot {
            sample_stride: self.sample_stride,
            groups: DecodeGroup::ALL
                .iter()
                .map(|&group| {
                    let bucket = self.groups[group.profile_index()];
                    CpuProfileBucket {
                        name: group.profile_name(),
                        instructions: bucket.instructions,
                        guest_core_clocks: bucket.guest_core_clocks,
                        sample_wall_ns: bucket.sample_wall_ns,
                        samples: bucket.samples,
                    }
                })
                .collect(),
            opcodes,
            hot_addrs,
            smc_flush_blocks,
        }
    }
}

fn duration_ns_u64(duration: std::time::Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

/// A decoded instruction: the prefix/opcode/operand-size results plus the pre-parsed ModRM, operand
/// descriptor, and immediate for the forms that carry them. After Stage A every implemented opcode
/// is converted to the decode/execute split; the no-operand forms (and the dead-end fallbacks) leave
/// `modrm`/`operand` as `None` (and `imm` 0). This is the value the decode cache stores per line, so
/// it is kept small and dense (see DecodeCache).
#[derive(Debug, Clone, Copy)]
struct DecodedInsn {
    len: u8,
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
    /// The routed decode group, resolved ONCE in `decode` (the single `route_group` authority) and
    /// stored so `execute_decoded` matches the variant directly instead of re-running the opcode
    /// classifier. `route_group` is pure over `(opcode, prefixes)`, both captured here, so the
    /// stored value is exactly what a re-call would return.
    group: DecodeGroup,
    /// Whether this instruction may run as a straight-line-run continuation
    /// (`block_continuable`), resolved ONCE at decode so the per-continuation gate in
    /// `run_straight_line` is a single flag test. Pure over `(group, opcode, modrm)`,
    /// all captured here. Measured: classifying per continuation instead cost 5-17%
    /// wall from code-layout effects even with identical logic (inline(always) did not
    /// recover it); resolve admission at decode, never in the run loop — this
    /// measurement is the reason.
    continuable: bool,
    /// Never enter this decode in the decode cache. Set when the two-byte ISA gate passed only
    /// via the firmware-ROM / ring-0 exemption: that exemption is context, not bytes, so a
    /// cached replay after a privilege change would skip the #UD. (LOCK-prefixed instructions
    /// are the other no-cache class, detected from `prefixes.lock` directly.)
    no_cache: bool,
}

/// Direct-mapped decode-cache lines (power of two so the index is a mask). A break-attribution
/// measurement on Doom demo3 8G/586 (the real pmode target, not the tiny real-mode benches the
/// earlier 2048 knee was derived from) showed 78% of run breaks were decode-cache misses on
/// continuations at 2048 lines: the pmode code footprint thrashes a 2048-entry direct-mapped
/// cache. Doubling to 4096 cut those misses by 53% (227M -> 106M breaks), boosted insns/run from
/// 14.5 to 23.2 (+60%), and lifted decode_hit from 94.85% to 97.66%. 8192 gave diminishing
/// returns (24.9 insns/run, +7% over 4096). At ~48 bytes per line (DecodeLine = tag + generation +
/// DecodedInsn) 4096 lines is ~192 KB, still inside L2 on a normal (8-32 MB L3) machine.
/// Purely microarchitectural: the decode cache is transparent to CpuGsw equality, so this needs
/// no conformance/regolden work.
const DECODE_CACHE_LINES: usize = 4096;

/// Sweep knob: `IZARRAVM_DECODE_CACHE_LINES=<power of two>` overrides the decode-cache size at
/// construction. Host-side only (a bigger cache changes wall time, never guest state - hit or
/// miss produces the identical DecodedInsn and identical clock charges). Read once, cached.
/// Motivation: the Doom 586 census measured decode_hit=21% / insns/run=1.3 at 2048 lines - the
/// direct-mapped cache thrashes on a 32-bit pmode code footprint; the bench-derived knee (2048,
/// tiny real-mode payloads) does not transfer.
fn decode_cache_lines() -> usize {
    static LINES: OnceLock<usize> = OnceLock::new();
    *LINES.get_or_init(|| {
        std::env::var("IZARRAVM_DECODE_CACHE_LINES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| n.is_power_of_two())
            .unwrap_or(DECODE_CACHE_LINES)
    })
}

/// Continuation misses on one line before hotness-driven admission compiles it (feature `jit`).
/// A hot self-loop head is hit once per iteration, so it admits after this many iterations; a cold
/// one-shot line never reaches it. 32 is early enough to catch a real loop within one batch and high
/// enough that transient warm-up code is not compiled. Only consulted when `jit_auto_admit` is on.
#[cfg(feature = "jit")]
const JIT_HOTNESS_THRESHOLD: u16 = 32;

/// The SMC watch bitmap tracks cached code at BYTE granularity. Coarser granularities fail on the
/// flat tiny-model layout the benchmarks (and many real-mode DOS programs) use: with cs=ds=ss=0,
/// the stack sits just below the code and globals sit just above or among it, so a 4 KB-page OR even
/// a 64-byte-block watch flushes the whole cache on data/stack writes that merely sit near code
/// (measured: dhrystone +45%). A byte-granular mark only flags an address as code when an
/// instruction was actually decoded there, so a write to an adjacent data byte never false-triggers.
/// Byte-granular coverage is the low 2 MiB of physical space, which holds all real-mode code
/// (conventional + UMB + HMA). ABOVE 2 MiB - where DOS-extender workloads load flat pmode code -
/// a coarse 4 KiB-page mark set covers the FULL 4 GB physical space (2^20 pages / 64 = 2^14 `u64`
/// = 128 KB): page granularity is safe there because flat pmode layouts do not interleave a hot
/// stack with code the way the tiny model does, and the alternative is unbounded stale-code
/// replay. That gap used to be masked by the per-CS-load whole-cache flush (one per ~38
/// instructions on Doom); with CS loads no longer flushing (the decode-cache invalidation-storm
/// fix), extended-memory SMC - e.g. Quake's self-patching software renderer - MUST be watched or
/// a stale line replays indefinitely (stage-2 adversarial review finding 12).
/// 2 MiB / 8 bits = 2^15 `u64` = 256 KB, allocated once per cache; only the words for live code and
/// written bytes are ever touched, so its working set is a handful of lines.
const SMC_BYTE_COVERAGE: u32 = 2 << 20;
const SMC_BITMAP_WORDS: usize = (SMC_BYTE_COVERAGE / 64) as usize;
/// One bit per 4 KiB physical page over the whole 4 GB space (see above).
const SMC_PAGE_WORDS: usize = 1 << 14;

/// One direct-mapped decode-cache line. `insn` is `None` until first filled. A filled line is a hit
/// only when its `tag` matches the lookup linear address AND its `gen` matches the live generation,
/// so advancing the generation invalidates every line in O(1) without clearing the array.
#[derive(Debug, Clone, Copy, Default)]
struct DecodeLine {
    tag: u32,
    generation: u32,
    insn: Option<DecodedInsn>,
    /// The CS D bit the instruction was decoded under. The one decode input a CS load can
    /// change that the linear tag does not capture (the default operand/address size), so it is
    /// part of the hit condition instead of CS loads flushing the whole cache. The fetch limit
    /// is the other such input; it is re-checked live at the hit sites, not stored.
    d: bool,
    /// Physical address of the instruction's first byte at decode time, for the narrow SMC
    /// path's covers-the-written-byte check. `phys_start..phys_start + len` is the physical
    /// span; contiguity holds because pages holding a page-straddling instruction take the
    /// global flush instead (see `PageCodeInfo::straddled`).
    phys_start: u32,
    /// 1-based index into the JIT's compiled-region table, `None` when no region starts at this
    /// line's address. Lives IN the decode line (not a separate map) so region lookup rides the
    /// `decode_cache.get` the run loop already does every continuation: admission costs zero
    /// extra lookups (settled invariant 1 from the seed post-mortem). `put` clears it, so a
    /// re-decode (generation bump, SMC) drops the stale region for free.
    #[cfg(feature = "jit")]
    jit_region: Option<std::num::NonZeroU32>,
    /// Miss counter for hotness-driven admission (feature `jit`): each continuation that reaches
    /// this line without a stamped region bumps it, and admission fires once it hits
    /// `JIT_HOTNESS_THRESHOLD`. Reset to 0 on every `put` (a re-decoded line restarts its count),
    /// so a cold one-shot line never reaches the threshold.
    #[cfg(feature = "jit")]
    jit_hotness: u16,
}

/// Per-physical-page code bookkeeping for the narrow SMC path: the ONE linear page code on this
/// physical page was decoded through, plus the two conditions that force the sound global-flush
/// fallback. Rebuilt from scratch after every global flush (cleared with the marks), so it never
/// outlives the lines it describes.
#[derive(Debug, Clone, Copy)]
struct PageCodeInfo {
    lin_page: u32,
    /// A later decode saw a DIFFERENT linear page mapping this physical page: the
    /// physical-to-linear reconstruction is ambiguous, so writes here must flush globally.
    aliased: bool,
    /// An instruction starting on this page crosses into the next page (or one crossed into this
    /// page): physical contiguity of a line's span is not guaranteed, so writes here (and on the
    /// neighbor, which sets its own flag) must flush globally.
    straddled: bool,
}

/// A direct-mapped, generation-stamped cache of decoded instructions keyed by linear EIP
/// (`cs.base + eip`). It lets a hot loop skip re-decoding the same bytes every iteration. The `gen`
/// counter is advanced whenever a decode could change meaning: CS base / paging / mode changes (via
/// `invalidate_code_caches`) and an ISA-level change (via `set_level`). A bump makes every stamped
/// line miss, so the next execution at each address re-decodes and re-stamps. It is NOT advanced on
/// a near branch (`set_eip` only moves eip, which already changes the linear key) nor on a plain
/// data write to a non-code page, both of which would flush the cache every loop iteration.
/// Self-modifying code IS handled: `code_blocks` marks every 64-byte physical block an instruction
/// was cached from, and a write into a marked block advances the generation (cross-page SMC). The
/// benchmark path has no SMC, and identical bench checksums verify nothing stale is served.
///
/// Transparent accelerator, not architectural state: excluded from `CpuGsw` equality (like
/// `PrefetchWindow`) and reset rather than copied on clone.
struct DecodeCache {
    lines: Box<[DecodeLine]>,
    mask: u32,
    generation: u32,
    /// Bitmap (1 bit per physical byte, low `SMC_BYTE_COVERAGE` bytes) of bytes an instruction has
    /// been cached from. A write touching a marked byte advances the generation. Marks are cleared
    /// whenever an SMC flush bumps the generation (no line survives, so live code re-marks on
    /// re-decode); otherwise stale marks accumulate and once-code bytes reused as data flush the
    /// cache forever (measured on Doom: 5.6M spurious flushes/timedemo).
    code_bytes: Box<[u64]>,
    /// Coarse companion above `SMC_BYTE_COVERAGE`: 1 bit per 4 KiB physical page, whole 4 GB
    /// space. See the SMC_BYTE_COVERAGE doc for why page granularity is correct there.
    code_pages: Box<[u64]>,
    /// Indices of `code_bytes` / `code_pages` words holding marks from the CURRENT generation,
    /// pushed on a word's 0 -> nonzero transition. Makes the mark clear proportional to the
    /// marked working set (a few KB of word writes) instead of a 384 KB memset: Doom's 3.9M
    /// SMC flushes/timedemo made the full memset a measured wall REGRESSION (~2.7% at 12G).
    dirty_byte_words: Vec<u32>,
    dirty_page_words: Vec<u32>,
    /// Physical page -> the linear page its cached code was decoded through (the narrow SMC
    /// path's physical-to-linear bridge). Populated by `put`, cleared with the marks, so it
    /// exactly describes the currently markable lines.
    code_page_lin: std::collections::HashMap<u32, PageCodeInfo, U32BuildHasher>,
    /// Bumped whenever a narrow SMC kill lands inside an installed JIT region's physical span:
    /// the region's slot table may now be stale (the entry line's stamp can survive a kill of a
    /// LATER slot's line). `run_region` refuses a region whose `valid_epoch` lags and unstamps
    /// it, forcing matcher re-admission over the fresh decodes. Lives here (not on `CpuGsw`)
    /// because the whole cache is excluded from CPU equality; this is host bookkeeping.
    #[cfg(feature = "jit")]
    jit_smc_epoch: u32,
}

/// A tiny multiplicative hasher for the decode cache's `u32`-keyed `code_page_lin` map, replacing
/// std's SipHash (N2, 2026-07-07 perf plan). `put` runs it on every decode-cache miss-fill; SipHash's
/// per-lookup cost is wasted on a small integer key. No new dependency: one Fibonacci-multiply on the
/// `write_u32` path (the only path a `u32` key ever takes), with a byte fallback for completeness.
#[derive(Default)]
struct U32Hasher(u64);
impl std::hash::Hasher for U32Hasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.0 = (u64::from(i)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ u64::from(b)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }
}
type U32BuildHasher = std::hash::BuildHasherDefault<U32Hasher>;

impl DecodeCache {
    fn new(lines: usize) -> Self {
        assert!(
            lines.is_power_of_two(),
            "decode cache size must be a power of two"
        );
        Self {
            lines: vec![DecodeLine::default(); lines].into_boxed_slice(),
            mask: (lines - 1) as u32,
            // Fresh lines default to generation 0; start live at 1 so they miss until first filled.
            generation: 1,
            code_bytes: vec![0u64; SMC_BITMAP_WORDS].into_boxed_slice(),
            code_pages: vec![0u64; SMC_PAGE_WORDS].into_boxed_slice(),
            dirty_byte_words: Vec::new(),
            dirty_page_words: Vec::new(),
            code_page_lin: std::collections::HashMap::default(),
            #[cfg(feature = "jit")]
            jit_smc_epoch: 0,
        }
    }

    /// Mark the bytes `[physical, physical + len)` as holding cached code, so a later write touching
    /// any of them invalidates the cache. An instruction is at most 15 bytes. Bytes above the
    /// covered range are skipped (treated as non-code by `is_code_byte`).
    #[inline]
    fn mark_code_range(&mut self, physical: u32, len: u8) {
        for i in 0..u32::from(len) {
            let addr = physical.wrapping_add(i);
            if addr < SMC_BYTE_COVERAGE {
                let word = (addr >> 6) as usize;
                if self.code_bytes[word] == 0 {
                    self.dirty_byte_words.push(word as u32);
                }
                self.code_bytes[word] |= 1u64 << (addr & 63);
            } else {
                let page = addr >> 12;
                let word = (page >> 6) as usize;
                if self.code_pages[word] == 0 {
                    self.dirty_page_words.push(word as u32);
                }
                self.code_pages[word] |= 1u64 << (page & 63);
            }
        }
    }

    /// Whether this physical byte was decoded as part of a cached instruction: byte-granular
    /// below `SMC_BYTE_COVERAGE`, 4 KiB-page-granular above it.
    #[inline]
    fn is_code_byte(&self, physical: u32) -> bool {
        if physical < SMC_BYTE_COVERAGE {
            self.code_bytes[(physical >> 6) as usize] & (1u64 << (physical & 63)) != 0
        } else {
            let page = physical >> 12;
            self.code_pages[(page >> 6) as usize] & (1u64 << (page & 63)) != 0
        }
    }

    /// Whether any byte in `[physical, physical + width)` is a cached code byte.
    #[inline]
    fn range_hits_code(&self, physical: u32, width: u32) -> bool {
        (0..width).any(|i| self.is_code_byte(physical.wrapping_add(i)))
    }

    #[inline]
    fn get(&self, lin: u32, d: bool) -> Option<DecodedInsn> {
        let line = &self.lines[(lin & self.mask) as usize];
        if line.generation == self.generation && line.tag == lin && line.d == d {
            line.insn
        } else {
            None
        }
    }

    #[inline]
    fn put(&mut self, lin: u32, insn: DecodedInsn, d: bool, phys: u32) {
        // Narrow-SMC bookkeeping: record (or verify) the linear page this physical page's code
        // decodes through, and flag the two conditions that force the global-flush fallback. A
        // page-straddling instruction flags BOTH pages (its span's physical contiguity is not
        // guaranteed, and a write to either page could hit it).
        let len = u32::from(insn.len);
        let straddles = (lin & 0xfff) + len > 0x1000;
        let pages: [Option<u32>; 2] = [
            Some(phys >> 12),
            straddles.then(|| phys.wrapping_add(len - 1) >> 12),
        ];
        for page in pages.into_iter().flatten() {
            let lin_page = if page == phys >> 12 {
                lin >> 12
            } else {
                (lin.wrapping_add(len - 1)) >> 12
            };
            match self.code_page_lin.entry(page) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(PageCodeInfo {
                        lin_page,
                        aliased: false,
                        straddled: straddles,
                    });
                }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    let info = e.get_mut();
                    info.aliased |= info.lin_page != lin_page;
                    info.straddled |= straddles;
                }
            }
        }
        self.lines[(lin & self.mask) as usize] = DecodeLine {
            tag: lin,
            generation: self.generation,
            insn: Some(insn),
            d,
            phys_start: phys,
            #[cfg(feature = "jit")]
            jit_region: None,
            #[cfg(feature = "jit")]
            jit_hotness: 0,
        };
    }

    /// Try to invalidate ONLY the lines covering the written physical byte `physical` (already
    /// known to be a marked code byte). Returns true when handled narrowly; false means the
    /// caller must fall back to the whole-cache flush (unknown page, an aliased or straddled
    /// page, or any other reason the physical-to-linear reconstruction is unsound). The mark
    /// stays set either way: a stale mark only costs a future narrow attempt, never correctness.
    #[inline]
    fn narrow_invalidate(&mut self, physical: u32) -> Option<u32> {
        let info = *self.code_page_lin.get(&(physical >> 12))?;
        if info.aliased || info.straddled {
            return None;
        }
        // Within one page the offset is mapping-invariant, so the code-side linear of the
        // written byte is reconstructible without the writer's own linear (device/DMA writes
        // narrow too). Any line covering the byte starts at most 14 bytes earlier (15-byte max
        // instruction), under this same mapping (a different mapping would have set `aliased`).
        let written_lin = (info.lin_page << 12) | (physical & 0xfff);
        let mut killed = 0u32;
        for candidate in written_lin.saturating_sub(14)..=written_lin {
            let line = &mut self.lines[(candidate & self.mask) as usize];
            if line.generation != self.generation || line.tag != candidate {
                continue;
            }
            let len = line.insn.map_or(0, |i| u32::from(i.len));
            if line.phys_start <= physical && physical < line.phys_start.wrapping_add(len) {
                // Generation 0 can never match the live generation (invalidate skips 0 on
                // wrap), so the line is dead until re-decoded; a JIT region stamp dies with it.
                line.generation = 0;
                killed += 1;
            }
        }
        Some(killed)
    }

    /// The compiled-region index stamped on the live line for `lin`, if any. A second load of
    /// the same direct-mapped line `get` just hit (still in L1), not a separate map: this is the
    /// entire JIT admission check the continuation loop pays.
    #[cfg(feature = "jit")]
    #[inline]
    fn region_at(&self, lin: u32, d: bool) -> Option<std::num::NonZeroU32> {
        let line = &self.lines[(lin & self.mask) as usize];
        if line.generation == self.generation && line.tag == lin && line.d == d {
            line.jit_region
        } else {
            None
        }
    }

    /// The stored physical start of the live line for `lin`, for region admission (the matcher
    /// walked these lines; the region's physical span derives from the entry line's).
    #[cfg(feature = "jit")]
    fn line_phys_start(&self, lin: u32, d: bool) -> Option<u32> {
        let line = &self.lines[(lin & self.mask) as usize];
        (line.generation == self.generation && line.tag == lin && line.d == d)
            .then_some(line.phys_start)
    }

    /// Whether the line for `lin` is live for exactly this key: the same condition `get` uses,
    /// without copying the insn. The region step probes this per slot, which is the
    /// interpreter's own next-continuation decode probe in miss-detection terms. Consumed by
    /// the jit region step and by tests; harmless dead code in base builds.
    #[cfg_attr(not(feature = "jit"), allow(dead_code))]
    #[inline]
    fn line_live(&self, lin: u32, d: bool) -> bool {
        let line = &self.lines[(lin & self.mask) as usize];
        line.generation == self.generation && line.tag == lin && line.d == d
    }

    /// Drop the region stamp from the live line for `lin` (the region went stale via a narrow SMC
    /// kill inside its span or a mode-key mismatch; the next probe misses and re-admission refreshes
    /// the slots). The line was hot enough to carry a region and is being unstamped because the
    /// region went STALE, not because it cooled, so prime the hotness counter to re-fire admission on
    /// the very next continuation (one interpreted iteration, then re-admit reusing the region's
    /// table slot). Without this, the fire-once counter stays pinned at the threshold and, under pure
    /// auto-admit (no forced address to re-trigger `try_admit`), a self-patching or mode-switching
    /// loop would de-JIT permanently.
    #[cfg(feature = "jit")]
    fn unstamp_region(&mut self, lin: u32, d: bool) {
        let line = &mut self.lines[(lin & self.mask) as usize];
        if line.generation == self.generation && line.tag == lin && line.d == d {
            line.jit_region = None;
            line.jit_hotness = JIT_HOTNESS_THRESHOLD.saturating_sub(1);
        }
    }

    /// Stamp a compiled region's table index onto the live line for `lin`, so the continuation
    /// loop's `region_at` probe finds it. A no-op when the line is not live for exactly this
    /// key: stamping through a stale or mismatched line could attach the region to an address
    /// it was not compiled for.
    #[cfg(feature = "jit")]
    fn stamp_region(&mut self, lin: u32, d: bool, idx: std::num::NonZeroU32) {
        let line = &mut self.lines[(lin & self.mask) as usize];
        if line.generation == self.generation && line.tag == lin && line.d == d {
            line.jit_region = Some(idx);
        }
    }

    /// Bump the hotness miss counter for the live line at `lin` and report whether it just crossed
    /// `JIT_HOTNESS_THRESHOLD`. Fires EXACTLY ONCE (at the crossing) and then pins the counter at the
    /// threshold, so a line that fails to compile is not re-attempted every continuation. A no-op
    /// returning false when the line is not live for this key (the caller has just decoded it via
    /// `get`, so normally it is). Cheap: one array index + a compare + a conditional increment.
    #[cfg(feature = "jit")]
    fn note_hot_miss(&mut self, lin: u32, d: bool) -> bool {
        let line = &mut self.lines[(lin & self.mask) as usize];
        if line.generation == self.generation
            && line.tag == lin
            && line.d == d
            && line.jit_hotness < JIT_HOTNESS_THRESHOLD
        {
            line.jit_hotness += 1;
            line.jit_hotness == JIT_HOTNESS_THRESHOLD
        } else {
            false
        }
    }

    /// Invalidate every cached line by advancing the generation. O(1): stamped lines fail the
    /// generation check and re-decode on next use. Skips 0 on wrap so a fresh line never aliases.
    #[inline]
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.generation = 1;
        }
    }

    /// `invalidate`, plus reset the SMC watch bitmap. For the SMC flush path (`note_code_write`):
    /// after the generation bump no line is live, so every mark can be dropped and rebuilt from
    /// the lines actually re-decoded. Without this the monotonic marks accumulate forever and
    /// ordinary data writes over ONCE-code bytes (a loader region reused as heap, DOS buffers)
    /// re-trigger whole-cache flushes for the rest of the run - the Doom 586 census measured
    /// 5.6M such flushes in 2.6G instructions, one every ~460 instructions. The 256 KB clear is
    /// self-rate-limiting: once cleared, the stale bytes are unmarked and stop flushing.
    fn invalidate_and_clear_code_marks(&mut self) {
        self.invalidate();
        // Zero only the words marked since the last clear (the dirty lists), not the whole
        // 384 KB: with millions of SMC flushes per timedemo the full memset was a measured
        // wall regression. Every set bit lives in a dirty-listed word by construction
        // (mark_code_range pushes on the word's 0 -> nonzero transition).
        for &word in &self.dirty_byte_words {
            self.code_bytes[word as usize] = 0;
        }
        for &word in &self.dirty_page_words {
            self.code_pages[word as usize] = 0;
        }
        self.dirty_byte_words.clear();
        self.dirty_page_words.clear();
        // The narrow-SMC page map describes exactly the markable lines; no line survives a
        // global flush, so it is rebuilt from scratch by the re-decodes.
        self.code_page_lin.clear();
    }
}

impl Default for DecodeCache {
    fn default() -> Self {
        Self::new(decode_cache_lines())
    }
}

impl Clone for DecodeCache {
    fn clone(&self) -> Self {
        // The cache is a transparent accelerator: a clone starts empty (and re-decodes) rather than
        // copying every line, so cloning a CPU stays cheap and never depends on cache contents.
        Self::new(self.lines.len())
    }
}

impl PartialEq for DecodeCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for DecodeCache {}

impl std::fmt::Debug for DecodeCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "DecodeCache {{ {} lines, gen {} }}",
            self.lines.len(),
            self.generation
        )
    }
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

/// #DE (divide error): DIV/IDIV divide-by-zero or quotient overflow, and AAM with a
/// zero base. Vector 0, no error code (386 PRM 9.7 table 9-2: only selector-related
/// faults and #PF carry one).
const fn divide_error() -> InternalFault {
    InternalFault::Exception {
        vector: 0,
        error_code: None,
    }
}

/// #UD (invalid opcode): an unimplemented single-byte opcode, an unimplemented 0F-prefixed
/// opcode, or an unimplemented group-opcode extension. Vector 6, no error code. The
/// diagnostic opcode/CS/EIP/extension detail the old `CpuError` variants carried is not
/// lost: `deliver_exception` calls `trace_ud_if_enabled` for every vector-6 delivery,
/// which dumps CS:IP, the surrounding bytes, CR0, EFLAGS, V86, and CPL.
const fn undefined_opcode() -> InternalFault {
    InternalFault::Exception {
        vector: 6,
        error_code: None,
    }
}

/// Build the selector-fault error code pushed for a #GP/#NP/#SS/#TS on a segment
/// selector (386 PRM 9.7, table 9-2): bits 15:3 the selector's index, bit 2 (TI) set
/// when the selector names the LDT, bit 1 (IDT) set when the fault references an IDT
/// gate rather than a GDT/LDT descriptor, bit 0 (EXT) set when the fault was provoked
/// by an event external to the currently-executing program (never the case for the
/// synchronous descriptor/gate/segment-load faults this core raises). `selector`'s own
/// low 3 bits (RPL and part of TI) are irrelevant here; only the index is folded in,
/// per the table's "the selector index" wording -- TI is passed separately so callers
/// resolving against the LDT don't have to reconstruct it from the selector bits.
const fn selector_error_code(selector: u16, in_ldt: bool, in_idt: bool) -> u32 {
    ((selector as u32) & 0xfff8) | ((in_ldt as u32) << 2) | ((in_idt as u32) << 1)
}

/// A data access (not a segment *load*) that runs off the end of its segment: #SS
/// (vector 12) for SS, #GP (vector 13, error code 0) for every other segment. 386 PRM
/// 9.10/9.11: an ordinary limit violation during a memory reference is not attributed
/// to a selector, so the error code is 0 either way (table 9-2's selector-index error
/// code is only for a segment *load* that names a bad descriptor, handled separately
/// by `selector_error_code`).
const fn segment_limit_fault(segment: SegmentIndex) -> InternalFault {
    InternalFault::Exception {
        vector: if matches!(segment, SegmentIndex::Ss) {
            12
        } else {
            13
        },
        error_code: Some(0),
    }
}

type ExecResult<T> = Result<T, InternalFault>;

/// Which privilege level a linear-to-physical translation is checked against.
/// `Supervisor` forces `user = false` for the implicit system-structure reads
/// described on `translate_linear_system`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PagingAccessor {
    Current,
    Supervisor,
}

impl CpuGsw {
    pub fn cycle<B: CpuBus>(&mut self, bus: &mut B) -> Result<CycleOutcome, CpuError> {
        // `cycle` is the per-instruction prologue (halt-wake + hardware interrupt
        // service) followed by one instruction. The two halves are split so the
        // machine batch loop can run the prologue once per batch and then run a
        // sequence of instructions through `cycle_no_interrupt_check`:
        // `interrupt_pending()` is driven by the PIC and cannot change mid-batch
        // (devices advance only after the batch, and any guest PIC access ends the
        // batch), so a per-batch interrupt check is equivalent to the old
        // per-instruction one. Every existing caller keeps the old behavior because
        // this thin wrapper composes the two halves exactly as before.
        match self.service_pending_interrupt(bus)? {
            Some(outcome) => Ok(outcome),
            None => self.cycle_no_interrupt_check(bus),
        }
    }

    /// The interrupt prologue of `cycle`: wake from HLT if an enabled interrupt is
    /// pending, then service one pending hardware interrupt. Returns `Some(outcome)`
    /// when this call produced a complete cycle (it stayed halted or it took an
    /// interrupt) and `None` when the caller should run an instruction next. The
    /// STI one-instruction shadow is *tested* here but NOT consumed: the consume
    /// belongs to a running instruction (`cycle_no_interrupt_check`), so that when
    /// the machine runs this once per batch a `STI; HLT` idle loop still eventually
    /// takes its interrupt instead of spinning forever.
    pub fn service_pending_interrupt<B: CpuBus>(
        &mut self,
        bus: &mut B,
    ) -> Result<Option<CycleOutcome>, CpuError> {
        // Wake from HLT only when a maskable interrupt can actually be taken. The 386
        // exits HLT on an enabled interrupt; a masked or IF-disabled request leaves it
        // halted. NMI and the other non-maskable wake sources are out of scope.
        if self.halted {
            if self.flag(FLAG_IF) && bus.interrupt_pending() {
                self.halted = false;
            } else {
                return Ok(Some(CycleOutcome {
                    core_clocks: 1,
                    halted: true,
                }));
            }
        }

        // Test the one-instruction shadow set by STI without consuming it. While the
        // shadow is active the interrupt check is skipped so the instruction after STI
        // always executes before an interrupt can be taken; the consume happens in
        // `cycle_no_interrupt_check` when that next instruction runs.
        if !self.interrupt_shadow && self.flag(FLAG_IF) && bus.interrupt_pending() {
            if let Some(vector) = bus.acknowledge_interrupt() {
                self.hardware_interrupt(bus, vector)
                    .map_err(|fault| match fault {
                        InternalFault::Cpu(error) => error,
                        // A fault raised while `hardware_interrupt` (which calls
                        // `deliver_exception`) was building the IRQ's own frame is a
                        // genuinely nested fault, not an IDT-limit violation on `vector`
                        // itself -- report it truthfully instead of relabeling it.
                        InternalFault::Exception {
                            vector: nested_vector,
                            ..
                        } => CpuError::NestedFaultDuringDelivery {
                            original_vector: vector,
                            nested_vector,
                        },
                    })?;
                let charged = self.scale_clocks(61);
                self.elapsed_clocks += charged;
                return Ok(Some(CycleOutcome {
                    core_clocks: charged.min(u64::from(u32::MAX)) as u32,
                    halted: false,
                }));
            }
        }

        Ok(None)
    }

    /// Fetch and execute exactly one instruction with NO halt handling and NO
    /// interrupt check. The caller (`cycle`, or the machine batch loop) is
    /// responsible for having run `service_pending_interrupt` first. This consumes
    /// the STI one-instruction shadow: a running instruction uses up the one-cycle
    /// delay, so the instruction after STI runs here and the shadow is clear by the
    /// next interrupt check.
    pub fn cycle_no_interrupt_check<B: CpuBus>(
        &mut self,
        bus: &mut B,
    ) -> Result<CycleOutcome, CpuError> {
        self.interrupt_shadow = false;
        // This is always either a standalone single-step (no prior instructions in
        // "this run") or run_straight_line's FIRST instruction (total == 0 at that
        // point, by construction): both cases mean core_clocks_so_far is 0 here.
        // Continuations inside run_straight_line go through run_one_cached instead,
        // which does not reset this field; run_straight_line sets it explicitly
        // before each continuation call.
        self.core_clocks_so_far = 0;

        self.begin_instruction();
        let start_eip = self.registers.eip;
        let start_cs = self.registers.cs().selector;
        let lin = self.linear_eip();
        let profiling = self.profile.enabled;
        let profile_start = if profiling {
            self.profile.sample_start()
        } else {
            None
        };
        let mut profile_key = None;
        let result = match self.fetch_decoded(bus, lin) {
            Ok(insn) => {
                if profiling {
                    profile_key = Some((
                        insn.group,
                        insn.opcode,
                        CpuProfileOperandForm::from_insn(&insn),
                    ));
                }
                self.execute_decoded(&insn, bus)
            }
            Err(fault) => Err(fault),
        };
        self.finish_instruction(bus, result, start_eip, start_cs, profile_key, profile_start)
    }

    /// Emit one differential-oracle trace line for the instruction that just retired at
    /// `start_cs:start_eip`, in the common trace format: `CS:IP EAX EBX ECX EDX ESI EDI EBP
    /// ESP EFLAGS CS DS ES SS FS GS` (space-separated hex, no leading 0x). Gated on
    /// `diff_trace_enabled()`; callers check the gate themselves so the common case (env
    /// var unset) costs exactly one cached bool load and this function is never called.
    /// Reads `self.registers` fresh, i.e. AFTER the instruction retired, matching a
    /// reference emulator's post-step register dump.
    ///
    /// Prototype-only note: writes go through a process-wide buffered, mutex-guarded
    /// stderr handle (`diff_trace_writer`) rather than a bare `eprintln!`. An unbuffered
    /// `eprintln!` here cost one syscall per retired instruction, which measured at only
    /// ~3300 lines/sec against a redirected file -- far too slow to trace even a single
    /// BIOS POST, let alone anything DOS4GW-shaped. This is a diagnostic tool, not a hot
    /// path, so a mutex per line is an acceptable cost; the buffering is what actually
    /// matters for throughput.
    #[cold]
    fn emit_diff_trace_line(&self, start_cs: u16, start_eip: u32) {
        let r = &self.registers;
        let mut w = diff_trace_writer()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(
            w,
            "{start_cs:04x}:{start_eip:08x} {:08x} {:08x} {:08x} {:08x} {:08x} {:08x} {:08x} \
             {:08x} {:08x} {:04x} {:04x} {:04x} {:04x} {:04x} {:04x}",
            r.eax(),
            r.ebx(),
            r.ecx(),
            r.edx(),
            r.esi(),
            r.edi(),
            r.ebp(),
            r.esp(),
            r.eflags,
            r.cs().selector,
            r.segment(SegmentIndex::Ds).selector,
            r.segment(SegmentIndex::Es).selector,
            r.segment(SegmentIndex::Ss).selector,
            r.segment(SegmentIndex::Fs).selector,
            r.segment(SegmentIndex::Gs).selector,
        );
    }

    /// The shared rewind / deliver / scale tail of a single instruction's execution. It owns ONLY
    /// what happens after `result` is produced: on a delivered exception it rewinds eip (and CS, if a
    /// far transfer moved it) to the faulting instruction and delivers the fault through
    /// `deliver_exception` exactly as the per-instruction path always did, charging the architectural
    /// 59 core clocks for the dispatch; on a CPU-level error it propagates; otherwise it scales the
    /// retired clocks (the single per-mode timing dial) and accumulates them. Callers charge their own
    /// fetch BEFORE calling this, so it never touches fetch clocks and never double-charges.
    /// `start_eip` / `start_cs` are captured before the fetch so the rewind lands on the instruction's
    /// first byte.
    #[inline]
    fn finish_instruction<B: CpuBus>(
        &mut self,
        bus: &mut B,
        result: ExecResult<CycleOutcome>,
        start_eip: u32,
        start_cs: u16,
        profile_key: Option<(DecodeGroup, u16, CpuProfileOperandForm)>,
        profile_start: Option<std::time::Instant>,
    ) -> Result<CycleOutcome, CpuError> {
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(InternalFault::Exception { vector, error_code }) => {
                self.set_eip(start_eip);
                if self.registers.cs().selector != start_cs {
                    self.load_segment_real(SegmentIndex::Cs, start_cs);
                }
                self.deliver_exception(bus, vector, error_code, false)
                    .map_err(|fault| match fault {
                        InternalFault::Cpu(error) => error,
                        // As above: a fault raised while building `vector`'s own frame
                        // (e.g. the ring-0 stack access that was the actual dossier bug)
                        // is a nested fault, not an IDT-limit violation on `vector`.
                        InternalFault::Exception {
                            vector: nested_vector,
                            ..
                        } => CpuError::NestedFaultDuringDelivery {
                            original_vector: vector,
                            nested_vector,
                        },
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
        self.perf.instructions += 1;
        // V86 trap tax residency: see PerfCounters::monitor_resident_core_clocks.
        if self.is_ring0_protected() {
            self.perf.monitor_resident_core_clocks += charged;
        }
        if let Some((group, opcode, form)) = profile_key {
            // The hot-address histogram wants the linear address of the instruction START.
            // A far transfer already moved the CS base by now, mis-attributing that one
            // sample; histogram noise, accepted (this is a host-side loop finder, not
            // architectural state).
            let lin = self.registers.cs().base.wrapping_add(start_eip);
            self.profile
                .record(group, opcode, form, charged, profile_start, lin);
        }
        if diff_trace_enabled() {
            self.emit_diff_trace_line(start_cs, start_eip);
        }
        Ok(CycleOutcome {
            core_clocks: charged.min(u64::from(u32::MAX)) as u32,
            halted: outcome.halted,
        })
    }

    /// Run a straight-line run of instructions in one cross-crate call instead of bouncing to the
    /// machine batch loop once per instruction. The first instruction always goes through the normal
    /// single-instruction path (`cycle_no_interrupt_check`), which handles a decode miss, a fault, and
    /// halt. Each continuation runs ONLY when the next instruction is already in the decode cache,
    /// passes the `block_continuable` gate (the straight-line groups plus the near RET / near
    /// indirect CALL/JMP transfers), and stays inside the current 4 KB page; any miss, gated
    /// opcode, or page cross ends the run, and that terminator then runs through the normal path on
    /// the next machine-loop entry. `cap` bounds the run in scaled core clocks.
    ///
    /// This is the lean recompiler path: it needs no block cache. Self-modifying code is handled for
    /// free because every continuation re-peeks `decode_cache.get(lin)`. A guest write that modifies a
    /// later instruction bumps the decode-cache generation (via `note_code_write`), so the next `get`
    /// misses and the run ends; the modified instruction re-decodes through the normal path. The
    /// per-instruction `scale_clocks` + `charge_cached_fetch` calls are kept, so cyc/iter and the bus
    /// clock metric stay bit-identical to the per-instruction loop.
    ///
    /// Interrupt semantics match the per-batch model exactly: every instruction (the first AND each
    /// continuation) has its "a maskable interrupt just became serviceable" transition checked
    /// uniformly, so the run ends at precisely the boundary the old per-instruction loop would have
    /// stopped at (the post-STI instruction consuming the shadow, or POPF/IRET enabling IF). The
    /// machine's own per-batch transition check then services the interrupt at the next batch entry.
    pub fn run_straight_line<B: CpuBus>(
        &mut self,
        bus: &mut B,
        cap: u64,
    ) -> Result<CycleOutcome, CpuError> {
        let mut total = 0u64;
        let mut first = true;
        // Guest-clock budget honesty: `cap` is a guest-clock budget (the machine
        // derives it from PIT-edge instants), but `total` counts core clocks
        // only. Track the batch's scaled-bus growth across this run so a
        // bus-heavy run (a framebuffer blit is several bus clocks per core
        // clock) ends at the budget instead of overshooting the next timer
        // edge by the bus:core ratio. Zero-cost where the bus reports 0 (the
        // Accurate class and non-batching buses): the check degrades to the
        // historical core-only comparison bit-for-bit.
        let bus_at_entry = bus.in_batch_scaled_bus_clocks();
        self.perf.straight_line_runs += 1;
        loop {
            let can_take_before = self.can_take_interrupt();
            let outcome = if first {
                first = false;
                self.cycle_no_interrupt_check(bus)?
            } else {
                let lin = self.linear_eip();
                let cs = self.registers.cs();
                let insn = match self.decode_cache.get(lin, cs.default_size_32) {
                    Some(i) => {
                        if !i.continuable {
                            self.perf.brk_decode_or_branch += 1;
                            self.perf.brk_cont_not_continuable += 1;
                            break;
                        }
                        if (lin & 0xfff) + u32::from(i.len) > 0x1000 {
                            self.perf.brk_decode_or_branch += 1;
                            self.perf.brk_cont_page_cross += 1;
                            break;
                        }
                        if !Self::fetch_within_limit(self.registers.eip, i.len, cs.limit) {
                            self.perf.brk_decode_or_branch += 1;
                            self.perf.brk_cont_not_continuable += 1;
                            break;
                        }
                        i
                    }
                    None => {
                        self.perf.brk_decode_or_branch += 1;
                        self.perf.brk_cont_decode_miss += 1;
                        break;
                    }
                };
                // JIT admission: a compiled region stamped on this line runs natively instead of
                // the interpreted continuation, occupying one loop iteration; the loop's own
                // break checks below then fire at exactly the boundary the region stopped at.
                // The probe read+branch is the whole per-continuation dispatch cost.
                #[cfg(feature = "jit")]
                let region_outcome = self.try_region_continuation(
                    bus,
                    lin,
                    cs.default_size_32,
                    total,
                    bus_at_entry,
                    cap,
                )?;
                #[cfg(not(feature = "jit"))]
                let region_outcome: Option<CycleOutcome> = None;
                match region_outcome {
                    Some(outcome) => outcome,
                    None => {
                        // A continuation skips cycle_no_interrupt_check (which resets this
                        // field to 0 for a fresh first instruction), so set it explicitly:
                        // total is exactly the prior instructions' charge in this run, not
                        // including the continuation about to execute.
                        self.core_clocks_so_far = total;
                        self.run_one_cached(bus, &insn, lin)?
                    }
                }
            };
            total += u64::from(outcome.core_clocks);
            // The post-instruction break checks run in the SAME ORDER the old per-instruction machine
            // loop used (halted -> step-break -> interrupt-transition -> cap), so the run ends at
            // exactly the boundary that loop would have stopped at.
            if outcome.halted {
                self.perf.brk_halt += 1;
                return Ok(CycleOutcome {
                    core_clocks: total.min(u64::from(u32::MAX)) as u32,
                    halted: true,
                });
            }
            // A port access touched time-dependent device state, or an HLE software interrupt is
            // pending (e.g. an INT n, or an x87 #MF routed to vector 0x10). End the run so the machine
            // services it now, at the old per-instruction boundary. Checked after the FIRST
            // instruction too: a port OUT/IN as the run's first instruction must end the run after
            // that one instruction, exactly like the old loop.
            if bus.requires_step_break() {
                self.perf.brk_step += 1;
                break;
            }
            // End the run the instant an instruction makes a maskable interrupt serviceable (the
            // post-STI instruction consuming the shadow, or POPF/IRET enabling IF), so the machine
            // batch loop services it at the next batch entry, at exactly the old per-instruction
            // boundary. Checked after the FIRST instruction too: an IF-on-then-off-within-a-run
            // sequence would otherwise lose the interrupt.
            if !can_take_before && self.can_take_interrupt() {
                self.perf.brk_interrupt += 1;
                break;
            }
            if total + (bus.in_batch_scaled_bus_clocks() - bus_at_entry) >= cap {
                self.perf.brk_cap += 1;
                break;
            }
        }
        Ok(CycleOutcome {
            core_clocks: total.min(u64::from(u32::MAX)) as u32,
            halted: false,
        })
    }

    /// Enable or disable hotness-driven JIT admission (feature `jit`). Off by default; the CLI/GUI
    /// turns it on to run the JIT on real workloads. Independent of the forced-address override.
    /// Lives on the region table (a transparent accelerator excluded from CPU equality), so setting
    /// it never makes an otherwise-identical CPU compare unequal.
    #[cfg(feature = "jit")]
    pub fn set_jit_auto_admit(&mut self, on: bool) {
        self.jit_regions.set_auto_admit(on);
    }

    /// Enable/disable the cost-fold native-LOAD path (env `IZARRAVM_JIT_FOLD`), a process-global toggle
    /// read at region emit time. Off by default so production (`IZARRAVM_JIT=1` alone) and every
    /// bit-identical test are undisturbed. Associated (no `self`): it sets a global, like
    /// `NATIVE_BOOKKEEPING`. Turning it on makes JIT-block timing approximate (bus cost is folded), so
    /// it is validated by the anchor bands, not the differential timing asserts.
    #[cfg(feature = "jit")]
    pub fn set_jit_fold_timing(on: bool) {
        jit::block::FOLD_TIMING.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// The JIT dispatch at the continuation seam: run the region stamped on this line, or (on the
    /// forced admission address, or once a line is hot enough) compile/re-stamp one first. `Ok(None)`
    /// means "no region ran"; the caller falls back to the interpreted continuation.
    #[cfg(feature = "jit")]
    fn try_region_continuation<B: CpuBus>(
        &mut self,
        bus: &mut B,
        lin: u32,
        d: bool,
        total: u64,
        bus_at_entry: u64,
        cap: u64,
    ) -> Result<Option<CycleOutcome>, CpuError> {
        let idx = match self.decode_cache.region_at(lin, d) {
            Some(idx) => idx,
            None => {
                // Admit when either the forced-address override names this line (the spike/test
                // path) or hotness admission is enabled and this line's miss counter just crossed
                // the threshold. Both are cheap: a compare and a counter bump. When neither fires,
                // this branch is the whole per-continuation dispatch cost on the miss path.
                let hot = self.jit_regions.auto_admit() && self.decode_cache.note_hot_miss(lin, d);
                let forced = jit_forced_region_lin() == Some(lin);
                if !forced && !hot {
                    return Ok(None);
                }
                // Auto-admission (hotness) compiles ONLY self-loops: a linear block runs once per
                // entry then returns, so its region prologue/epilogue is pure overhead over the same
                // interpreted instructions and can never win (measured: broad linear-block admission
                // was a ~2.9x Doom wall regression). The forced-address override still admits any
                // block, for the spike/tests. Refusing is always state-correct.
                let Some(idx) = jit::block::try_admit_gated(self, lin, d, !forced) else {
                    return Ok(None);
                };
                self.decode_cache.stamp_region(lin, d, idx);
                idx
            }
        };
        self.run_region(bus, idx, lin, d, total, bus_at_entry, cap)
    }

    /// Execute a compiled region as one continuation of `run_straight_line`. On return the
    /// loop's own post-checks (halted, step break, interrupt transition, cap) re-fire at the
    /// exact boundary the region stopped at, so break attribution and batch semantics stay
    /// interpreter-identical. `Ok(None)` = an entry precondition failed, interpret instead.
    ///
    /// Deferred-at-exit accounting (one `scale_clocks` batch, `elapsed_clocks`,
    /// `perf.instructions`, ring-0 residency) is sound because no admitted block reads any of
    /// it mid-region (see the builder's invariants in `jit::block`) and the batch equals
    /// the per-instruction sums by the remainder-carry identity.
    #[cfg(feature = "jit")]
    #[allow(clippy::too_many_arguments)]
    fn run_region<B: CpuBus>(
        &mut self,
        bus: &mut B,
        idx: std::num::NonZeroU32,
        lin: u32,
        d: bool,
        total: u64,
        bus_at_entry: u64,
        cap: u64,
    ) -> Result<Option<CycleOutcome>, CpuError> {
        // Preconditions the region cannot honor per instruction: profiling and diff-trace
        // sample every instruction, and a live STI shadow could make an interrupt newly
        // serviceable after the FIRST slot, a mid-region boundary the run loop cannot see.
        if self.profile.enabled || diff_trace_enabled() || self.interrupt_shadow {
            return Ok(None);
        }
        let eip = self.registers.eip;
        let cs_limit = self.registers.cs().limit;
        let epoch = self.decode_cache.jit_smc_epoch;
        let ring0 = self.is_ring0_protected();
        let mode_key = self.jit_mode_key();
        let (num, den) = level_timing(self.level);
        let rem0 = self.timing_rem;
        // For a region with native cost-fold LOAD/STORE slots: DS flatness (and, for stores, DS
        // writability) is a runtime value NOT in the mode key, so re-check it here per entry (like the
        // per-slot CS-limit check below). Computed before the region borrow so `self` is free to read
        // the segment. Cheap; only consulted for regions that emitted a native probe.
        let ds_flat = self.jit_segment_flat(SegmentIndex::Ds);
        let ds_writable = self.jit_segment_writable(SegmentIndex::Ds);
        let step_fn = jit::step::region_step::<B> as jit::step::RegionStepFn;
        let (entry, ctx_ptr) = {
            let Some(region) = self.jit_regions.get_mut(idx) else {
                return Ok(None);
            };
            if region.entry_lin != lin || region.d != d {
                return Ok(None);
            }
            if region.mode_key != mode_key {
                // The same phys/d line is being entered in a different CPU mode/size than the block
                // was compiled for (real vs pmode vs V86, or a size/level change). The block key
                // includes the mode bitmask (spec §2.2), so this is a miss: drop the stamp and let
                // the forced-admission path re-build the block for the current mode.
                self.decode_cache.unstamp_region(lin, d);
                return Ok(None);
            }
            if region.valid_epoch != epoch {
                // A narrow SMC kill landed inside this region's span since the slots were last
                // built: the stamp may outlive the killed slot lines, so drop it and let the
                // forced-admission path re-run the builder over the fresh decodes.
                self.decode_cache.unstamp_region(lin, d);
                return Ok(None);
            }
            if region.has_native_fold && !ds_flat {
                // This region's native cost-fold LOAD/STORE slots assume DS is flat (EA == linear ==
                // physical). DS is no longer flat, so the emitted probe would compute the wrong
                // address. Bail to the interpreter (always correct); leave the stamp so a later entry
                // re-uses the region if DS becomes flat again (self-healing, no re-admit churn).
                return Ok(None);
            }
            if region.has_native_store && !ds_writable {
                // A native STORE slot assumes DS permits writes (a write-cache HIT only proves the
                // physical page was writable via some segment). DS is now read-only, so the native store
                // would silently write where the interpreter #GPs — bail to the interpreter, which
                // faults correctly. Self-healing like the flatness check.
                return Ok(None);
            }
            let ctx = &mut *region.ctx;
            // Every slot must pass the same live CS-limit check its interpreted continuation
            // would have (limits cannot change inside: no CS writer is admitted).
            for slot in &ctx.slots {
                let slot_eip = eip.wrapping_add(slot.lin.wrapping_sub(lin));
                if !Self::fetch_within_limit(slot_eip, slot.insn.len, cs_limit) {
                    return Ok(None);
                }
            }
            ctx.step_fn = Some(step_fn);
            ctx.inline_step_fn =
                Some(jit::step::region_inline_slot::<B> as jit::step::RegionStepFn);
            // Raw fn pointers to the flag helpers. The cast through `as` is sound: each helper is
            // `fn(&mut self, ...)` and we store it as `unsafe extern "C" fn(*mut CpuGsw, ...)`,
            // calling it with the cpu pointer the emitted code already holds; the `&mut` rebind
            // inside is the same disjoint-reborrow pattern region_step uses.
            ctx.set_pending_add_fn = Some(unsafe {
                std::mem::transmute::<fn(&mut CpuGsw, u32, u32), jit::step::SetPendingAddFn>(
                    Self::jit_set_pending_add as fn(&mut CpuGsw, u32, u32),
                )
            });
            ctx.set_shift_flags_fn = Some(unsafe {
                std::mem::transmute::<fn(&mut CpuGsw, u32, u8), jit::step::SetShiftFlagsFn>(
                    Self::jit_set_shift_flags_shr as fn(&mut CpuGsw, u32, u8),
                )
            });
            ctx.charge_fetch_fn =
                Some(jit::step::jit_charge_fetch::<B> as jit::step::ChargeFetchFn);
            ctx.bus_clocks_fn = Some(jit::step::jit_bus_clocks::<B> as jit::step::BusClocksFn);
            ctx.line_live_fn = Some(jit::step::jit_line_live as jit::step::LineLiveFn);
            ctx.store_finish_fn = Some(unsafe {
                std::mem::transmute::<fn(&mut CpuGsw, u32), jit::step::StoreFinishFn>(
                    Self::jit_store_u8_finish as fn(&mut CpuGsw, u32),
                )
            });
            ctx.entry_eip = eip;
            ctx.raw_clocks = 0;
            ctx.insn_count = 0;
            ctx.run_total_at_entry = total;
            ctx.bus_at_run_start = bus_at_entry;
            ctx.cap = cap;
            ctx.rem0 = rem0;
            ctx.scale_num = num;
            ctx.scale_den = den;
            ctx.d = d;
            ctx.exit = jit::step::RegionExitKind::Boundary;
            ctx.fault = None;
            ctx.halted = false;
            // Cost-fold: start the folded-bus accumulator empty; region_step flushes it. A native
            // MEMORY slot folds ONE instruction-fetch + ONE data-byte cost; a native ALU slot folds the
            // fetch only. Stash both constants from the concrete bus here (THE WRINKLE: the bus-agnostic
            // emitted buffer cannot call a bus method). Zero on buses without bus timing, so a native
            // slot folds nothing there.
            ctx.folded_raw_bus = 0;
            ctx.fetch_cost = bus.jit_fetch_cost_clocks();
            ctx.fold_bus_cost = ctx.fetch_cost + bus.jit_data_byte_cost_clocks();
            (region.entry, std::ptr::from_mut(ctx))
        };
        // SAFETY: the emitted code only forwards these pointers to `region_step::<B>`, whose
        // contract this call establishes: `self` and `bus` stay live `&mut` for the whole call
        // (no other reference to either exists here), `ctx` is the running region's boxed
        // mailbox (a separate allocation from `self`, so the step function's two reborrows are
        // disjoint; nothing reachable from the execute dispatch touches `jit_regions`), and `B`
        // is the concrete bus type behind the erased pointer.
        unsafe {
            (entry)(
                std::ptr::from_mut(self),
                (std::ptr::from_mut(bus)).cast(),
                ctx_ptr,
            );
        }
        let (raw, count, halted, fault, folded_bus) = {
            let region = self
                .jit_regions
                .get_mut(idx)
                .expect("the region that just ran is still installed");
            let ctx = &mut *region.ctx;
            (
                ctx.raw_clocks,
                ctx.insn_count,
                ctx.halted,
                ctx.fault.take(),
                std::mem::take(&mut ctx.folded_raw_bus),
            )
        };
        // Flush any bus cost the tail native fold slots accumulated but did not hand to a region_step
        // slot: a region that exits directly from a native LOAD's line_live probe (or an inline slot's
        // cap check immediately after one) leaves the last fold unflushed. Bus-timing only (the core
        // term rides raw_clocks below), keeping the device-visible clock total current at region exit.
        if folded_bus > 0 {
            bus.charge_bus_clocks_bulk(folded_bus);
        }
        let charged = self.scale_clocks_batch(raw);
        self.elapsed_clocks += charged;
        self.perf.instructions += u64::from(count);
        self.perf.jit_region_entries += 1;
        self.perf.jit_region_insns += u64::from(count);
        if ring0 {
            self.perf.monitor_resident_core_clocks += charged;
        }
        let mut out = charged;
        if let Some((start_eip, fault)) = fault {
            match fault {
                InternalFault::Cpu(error) => return Err(error),
                // finish_instruction's Exception arm, minus the CS restore (no admitted shape
                // can change CS mid-region): rewind to the faulting instruction, deliver, and
                // charge the interpreter's 59-clock delivery cost.
                InternalFault::Exception { vector, error_code } => {
                    self.set_eip(start_eip);
                    self.deliver_exception(bus, vector, error_code, false)
                        .map_err(|fault| match fault {
                            InternalFault::Cpu(error) => error,
                            InternalFault::Exception {
                                vector: nested_vector,
                                ..
                            } => CpuError::NestedFaultDuringDelivery {
                                original_vector: vector,
                                nested_vector,
                            },
                        })?;
                    let charged_fault = self.scale_clocks(59);
                    self.elapsed_clocks += charged_fault;
                    self.perf.instructions += 1;
                    if self.is_ring0_protected() {
                        self.perf.monitor_resident_core_clocks += charged_fault;
                    }
                    out += charged_fault;
                }
            }
        }
        Ok(Some(CycleOutcome {
            core_clocks: out.min(u64::from(u32::MAX)) as u32,
            halted,
        }))
    }

    /// Execute one already-decoded cached instruction as a straight-line continuation. Consumes the
    /// one-instruction STI shadow (a running instruction uses up the one-cycle delay), charges the
    /// cached-hit fetch (without re-decoding, so no double charge), runs the decoded form, and uses a
    /// small profiling-off success tail. Faults and profiling route through `finish_instruction`, so a
    /// mid-run fault still rewinds eip to the faulting instruction and delivers normally.
    #[inline]
    fn run_one_cached<B: CpuBus>(
        &mut self,
        bus: &mut B,
        insn: &DecodedInsn,
        lin: u32,
    ) -> Result<CycleOutcome, CpuError> {
        self.interrupt_shadow = false;
        self.begin_instruction();
        let start_eip = self.registers.eip;
        let start_cs = self.registers.cs().selector;
        let profiling = self.profile.enabled;
        if !profiling {
            return match self
                .charge_cached_fetch(bus, lin, insn.len)
                .and_then(|()| self.execute_hot_cached_or_decoded(insn, bus))
            {
                Ok(outcome) => {
                    let charged = self.scale_clocks(outcome.core_clocks);
                    self.elapsed_clocks += charged;
                    self.perf.instructions += 1;
                    // V86 trap tax residency: the monitor's own straight-line
                    // instructions chain through this cached fast tail, not
                    // finish_instruction, so the residency attribution must
                    // live here too or the monitor body goes uncounted.
                    if self.is_ring0_protected() {
                        self.perf.monitor_resident_core_clocks += charged;
                    }
                    // Same reasoning as the residency counter above: this fast
                    // tail bypasses finish_instruction, so the diff-trace hook
                    // must be duplicated here or every cached-path instruction
                    // (the common case) goes untraced.
                    if diff_trace_enabled() {
                        self.emit_diff_trace_line(start_cs, start_eip);
                    }
                    Ok(CycleOutcome {
                        core_clocks: charged.min(u64::from(u32::MAX)) as u32,
                        halted: outcome.halted,
                    })
                }
                Err(fault) => {
                    self.finish_instruction(bus, Err(fault), start_eip, start_cs, None, None)
                }
            };
        }
        let profile_start = self.profile.sample_start();
        let result = self
            .charge_cached_fetch(bus, lin, insn.len)
            .and_then(|()| self.execute_hot_cached_or_decoded(insn, bus));
        self.finish_instruction(
            bus,
            result,
            start_eip,
            start_cs,
            profiling.then_some((
                insn.group,
                insn.opcode,
                CpuProfileOperandForm::from_insn(insn),
            )),
            profile_start,
        )
    }

    #[inline]
    fn execute_hot_cached_or_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        if let Some(outcome) = self.execute_hot_cached_decoded(insn) {
            return Ok(outcome);
        }
        match insn.group {
            DecodeGroup::Alu => {
                if let Some(outcome) = self.execute_hot_cached_alu_memory(insn, bus)? {
                    return Ok(outcome);
                }
            }
            DecodeGroup::DataMove => {
                if let Some(outcome) = self.execute_hot_cached_datamove(insn, bus)? {
                    return Ok(outcome);
                }
            }
            DecodeGroup::FlagsMisc => {
                if let Some(outcome) = self.execute_hot_cached_flags_misc(insn, bus)? {
                    return Ok(outcome);
                }
            }
            DecodeGroup::Group => {
                if let Some(outcome) = self.execute_hot_cached_group1_memory(insn, bus)? {
                    return Ok(outcome);
                }
            }
            DecodeGroup::Stack => {
                if let Some(outcome) = self.execute_hot_cached_stack(insn, bus)? {
                    return Ok(outcome);
                }
            }
            DecodeGroup::Branch => {
                if let Some(outcome) = self.execute_hot_cached_branch(insn, bus)? {
                    return Ok(outcome);
                }
            }
            _ => {}
        }
        self.execute_decoded(insn, bus)
    }

    /// Hot cached-instruction subset that never touches the bus and cannot fault. EIP has already
    /// advanced past the instruction by `charge_cached_fetch`, matching the normal decoded executor.
    #[inline]
    fn execute_hot_cached_decoded(&mut self, insn: &DecodedInsn) -> Option<CycleOutcome> {
        if insn.opcode <= 0xff {
            let opcode = insn.opcode as u8;
            if opcode < 0x40 && (opcode & 0x07) < 6 {
                return self.execute_hot_cached_alu(insn, opcode);
            }
            if matches!(opcode, 0x80..=0x83) {
                return self.execute_hot_cached_group1(insn, opcode);
            }
            if matches!(opcode, 0xc0 | 0xc1 | 0xd0..=0xd3) {
                return self.execute_hot_cached_group2(insn, opcode);
            }
        }

        match insn.opcode {
            0x0fb6 => {
                let modrm = insn.modrm?;
                let DecodedOperand::Reg(index) = insn.operand? else {
                    return None;
                };
                self.write_gpr_sized(
                    modrm.reg,
                    insn.operand_size,
                    u32::from(self.read_gpr8(index)),
                );
                Some(clocks(3))
            }
            0x0fb7 => {
                let modrm = insn.modrm?;
                let DecodedOperand::Reg(index) = insn.operand? else {
                    return None;
                };
                self.write_gpr_sized(
                    modrm.reg,
                    insn.operand_size,
                    self.read_gpr_sized(index, OperandSize::Word),
                );
                Some(clocks(3))
            }
            0x0fbe => {
                let modrm = insn.modrm?;
                let DecodedOperand::Reg(index) = insn.operand? else {
                    return None;
                };
                let value = self.read_gpr8(index) as i8 as i32 as u32;
                self.write_gpr_sized(modrm.reg, insn.operand_size, value);
                Some(clocks(3))
            }
            0x0fbf => {
                let modrm = insn.modrm?;
                let DecodedOperand::Reg(index) = insn.operand? else {
                    return None;
                };
                let value = self.read_gpr_sized(index, OperandSize::Word) as i16 as i32 as u32;
                self.write_gpr_sized(modrm.reg, insn.operand_size, value);
                Some(clocks(3))
            }
            0x70..=0x7f | 0x0f80..=0x0f8f => {
                let cc = (insn.opcode & 0x0f) as u8;
                let taken = match cc {
                    0x4 => self.flag(FLAG_ZF),
                    0x5 => !self.flag(FLAG_ZF),
                    _ => self.condition(cc),
                };
                if taken {
                    self.relative_jump(insn.imm as i32, insn.operand_size);
                }
                Some(clocks(3))
            }
            opcode if opcode <= 0xff => match opcode as u8 {
                0x40..=0x4f => {
                    let index = insn.opcode as u8 & 0x07;
                    let value = self.read_gpr_sized(index, insn.operand_size);
                    let result = self.inc_dec(
                        value,
                        insn.opcode as u8 >= 0x48,
                        insn.operand_size.bus_width(),
                    );
                    self.write_gpr_sized(index, insn.operand_size, result);
                    Some(clocks(2))
                }
                0x84 => {
                    let modrm = insn.modrm?;
                    if modrm.mode == 3 && modrm.reg == modrm.rm {
                        let value = self.read_gpr8(modrm.rm);
                        self.alu_logic(u32::from(value), BusWidth::Byte);
                        return Some(clocks(2));
                    }
                    let DecodedOperand::Reg(index) = insn.operand? else {
                        return None;
                    };
                    let value = self.read_gpr8(index);
                    let result = value & self.read_gpr8(modrm.reg);
                    self.alu_logic(u32::from(result), BusWidth::Byte);
                    Some(clocks(2))
                }
                0x85 => {
                    let modrm = insn.modrm?;
                    if modrm.mode == 3 && modrm.reg == modrm.rm {
                        let value = self.read_gpr_sized(modrm.rm, insn.operand_size);
                        self.alu_logic(value, insn.operand_size.bus_width());
                        return Some(clocks(2));
                    }
                    let DecodedOperand::Reg(index) = insn.operand? else {
                        return None;
                    };
                    let value = self.read_gpr_sized(index, insn.operand_size);
                    let result = value & self.read_gpr_sized(modrm.reg, insn.operand_size);
                    self.alu_logic(result, insn.operand_size.bus_width());
                    Some(clocks(2))
                }
                0x88 => {
                    let modrm = insn.modrm?;
                    let DecodedOperand::Reg(index) = insn.operand? else {
                        return None;
                    };
                    self.write_gpr8(index, self.read_gpr8(modrm.reg));
                    Some(clocks(2))
                }
                0x89 => {
                    let modrm = insn.modrm?;
                    let DecodedOperand::Reg(index) = insn.operand? else {
                        return None;
                    };
                    if insn.operand_size == OperandSize::Word {
                        self.write_gpr16(index, self.read_gpr16(modrm.reg));
                    } else {
                        self.write_gpr32(index, self.read_gpr32(modrm.reg));
                    }
                    Some(clocks(2))
                }
                0x8a => {
                    let modrm = insn.modrm?;
                    let DecodedOperand::Reg(index) = insn.operand? else {
                        return None;
                    };
                    self.write_gpr8(modrm.reg, self.read_gpr8(index));
                    Some(clocks(2))
                }
                0x8b => {
                    let modrm = insn.modrm?;
                    let DecodedOperand::Reg(index) = insn.operand? else {
                        return None;
                    };
                    if insn.operand_size == OperandSize::Word {
                        self.write_gpr16(modrm.reg, self.read_gpr16(index));
                    } else {
                        self.write_gpr32(modrm.reg, self.read_gpr32(index));
                    }
                    Some(clocks(2))
                }
                0x8d => {
                    let modrm = insn.modrm?;
                    let DecodedOperand::Mem(addr) = insn.operand? else {
                        return None;
                    };
                    let memory = self.resolve_memory_addr_mode(&addr);
                    self.write_gpr_sized(modrm.reg, insn.operand_size, memory.offset);
                    Some(clocks(2))
                }
                0x90 => Some(clocks(3)),
                0x91..=0x97 => {
                    let reg = opcode as u8 & 0x07;
                    let acc = self.read_gpr_sized(0, insn.operand_size);
                    let other = self.read_gpr_sized(reg, insn.operand_size);
                    self.write_gpr_sized(0, insn.operand_size, other);
                    self.write_gpr_sized(reg, insn.operand_size, acc);
                    Some(clocks(3))
                }
                0xb0..=0xb7 => {
                    self.write_gpr8(insn.opcode as u8 - 0xb0, insn.imm as u8);
                    Some(clocks(2))
                }
                0xb8..=0xbf => {
                    self.write_gpr_sized(insn.opcode as u8 - 0xb8, insn.operand_size, insn.imm);
                    Some(clocks(2))
                }
                0xe0 | 0xe1 => {
                    let count_nonzero = match insn.address_size {
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
                    if count_nonzero && (if insn.opcode as u8 == 0xe1 { zf } else { !zf }) {
                        self.relative_jump(insn.imm as i32, insn.operand_size);
                    }
                    Some(clocks(11))
                }
                0xe2 => {
                    let taken = match insn.address_size {
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
                        self.relative_jump(insn.imm as i32, insn.operand_size);
                    }
                    Some(clocks(11))
                }
                0xe3 => {
                    let taken = match insn.address_size {
                        AddressSize::Word => self.read_gpr16(1) == 0,
                        AddressSize::Dword => self.registers.ecx() == 0,
                    };
                    if taken {
                        self.relative_jump(insn.imm as i32, insn.operand_size);
                    }
                    Some(clocks(9))
                }
                0xe9 | 0xeb => {
                    self.relative_jump(insn.imm as i32, insn.operand_size);
                    Some(clocks(7))
                }
                _ => None,
            },
            _ => None,
        }
    }

    #[inline]
    fn execute_hot_cached_alu(&mut self, insn: &DecodedInsn, opcode: u8) -> Option<CycleOutcome> {
        let op = (opcode >> 3) & 0x07;
        let form = opcode & 0x07;
        let write_back = op != 7;
        let operand_size = insn.operand_size;

        match form {
            0 => {
                let modrm = insn.modrm?;
                let DecodedOperand::Reg(index) = insn.operand? else {
                    return None;
                };
                let result = self.alu(
                    op,
                    u32::from(self.read_gpr8(index)),
                    u32::from(self.read_gpr8(modrm.reg)),
                    BusWidth::Byte,
                ) as u8;
                if write_back {
                    self.write_gpr8(index, result);
                }
            }
            1 => {
                let modrm = insn.modrm?;
                let DecodedOperand::Reg(index) = insn.operand? else {
                    return None;
                };
                let result = self.alu(
                    op,
                    self.read_gpr_sized(index, operand_size),
                    self.read_gpr_sized(modrm.reg, operand_size),
                    operand_size.bus_width(),
                );
                if write_back {
                    self.write_gpr_sized(index, operand_size, result);
                }
            }
            2 => {
                let modrm = insn.modrm?;
                let DecodedOperand::Reg(index) = insn.operand? else {
                    return None;
                };
                let result = self.alu(
                    op,
                    u32::from(self.read_gpr8(modrm.reg)),
                    u32::from(self.read_gpr8(index)),
                    BusWidth::Byte,
                ) as u8;
                if write_back {
                    self.write_gpr8(modrm.reg, result);
                }
            }
            3 => {
                let modrm = insn.modrm?;
                let DecodedOperand::Reg(index) = insn.operand? else {
                    return None;
                };
                let result = self.alu(
                    op,
                    self.read_gpr_sized(modrm.reg, operand_size),
                    self.read_gpr_sized(index, operand_size),
                    operand_size.bus_width(),
                );
                if write_back {
                    self.write_gpr_sized(modrm.reg, operand_size, result);
                }
            }
            4 => {
                let result =
                    self.alu(op, u32::from(self.read_gpr8(0)), insn.imm, BusWidth::Byte) as u8;
                if write_back {
                    self.write_gpr8(0, result);
                }
            }
            5 => {
                let result = self.alu(
                    op,
                    self.read_gpr_sized(0, operand_size),
                    insn.imm,
                    operand_size.bus_width(),
                );
                if write_back {
                    self.write_gpr_sized(0, operand_size, result);
                }
            }
            _ => return None,
        }

        Some(clocks(2))
    }

    #[inline]
    fn execute_hot_cached_alu_memory<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<Option<CycleOutcome>> {
        let opcode = insn.opcode as u8;
        if opcode >= 0x40 || (opcode & 0x07) >= 4 {
            return Ok(None);
        }
        let Some(modrm) = insn.modrm else {
            return Ok(None);
        };
        let Some(DecodedOperand::Mem(addr)) = insn.operand else {
            return Ok(None);
        };
        let memory = self.resolve_memory_addr_mode(&addr);

        let op = (opcode >> 3) & 0x07;
        let write_back = op != 7;
        match opcode & 0x07 {
            0 => {
                let value = self.read_memory_u8(
                    bus,
                    memory.segment,
                    memory.offset,
                    BusAccessKind::DataRead,
                )?;
                let reg = self.read_gpr8(modrm.reg);
                let result = self.alu(op, u32::from(value), u32::from(reg), BusWidth::Byte) as u8;
                if write_back {
                    self.write_memory_u8(
                        bus,
                        memory.segment,
                        memory.offset,
                        result,
                        BusAccessKind::DataWrite,
                    )?;
                }
                Ok(Some(clocks(2)))
            }
            1 => {
                let value = self.read_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    insn.operand_size,
                    BusAccessKind::DataRead,
                )?;
                let reg = self.read_gpr_sized(modrm.reg, insn.operand_size);
                let result = self.alu(op, value, reg, insn.operand_size.bus_width());
                if write_back {
                    self.write_memory_sized(
                        bus,
                        memory.segment,
                        memory.offset,
                        insn.operand_size,
                        result,
                        BusAccessKind::DataWrite,
                    )?;
                }
                Ok(Some(clocks(2)))
            }
            2 => {
                let value = self.read_memory_u8(
                    bus,
                    memory.segment,
                    memory.offset,
                    BusAccessKind::DataRead,
                )?;
                let reg = self.read_gpr8(modrm.reg);
                let result = self.alu(op, u32::from(reg), u32::from(value), BusWidth::Byte) as u8;
                if write_back {
                    self.write_gpr8(modrm.reg, result);
                }
                Ok(Some(clocks(2)))
            }
            3 => {
                let value = self.read_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    insn.operand_size,
                    BusAccessKind::DataRead,
                )?;
                let reg = self.read_gpr_sized(modrm.reg, insn.operand_size);
                let result = self.alu(op, reg, value, insn.operand_size.bus_width());
                if write_back {
                    self.write_gpr_sized(modrm.reg, insn.operand_size, result);
                }
                Ok(Some(clocks(2)))
            }
            _ => Ok(None),
        }
    }

    #[inline]
    fn execute_hot_cached_group1(
        &mut self,
        insn: &DecodedInsn,
        opcode: u8,
    ) -> Option<CycleOutcome> {
        let modrm = insn.modrm?;
        let DecodedOperand::Reg(index) = insn.operand? else {
            return None;
        };

        match opcode {
            0x80 | 0x82 => {
                let result = self.alu(
                    modrm.reg,
                    u32::from(self.read_gpr8(index)),
                    insn.imm,
                    BusWidth::Byte,
                ) as u8;
                if modrm.reg != 7 {
                    self.write_gpr8(index, result);
                }
            }
            0x81 | 0x83 => {
                let result = self.alu(
                    modrm.reg,
                    self.read_gpr_sized(index, insn.operand_size),
                    insn.imm,
                    insn.operand_size.bus_width(),
                );
                if modrm.reg != 7 {
                    self.write_gpr_sized(index, insn.operand_size, result);
                }
            }
            _ => return None,
        }

        Some(clocks(2))
    }

    #[inline]
    fn execute_hot_cached_group2(
        &mut self,
        insn: &DecodedInsn,
        opcode: u8,
    ) -> Option<CycleOutcome> {
        let modrm = insn.modrm?;
        let DecodedOperand::Reg(index) = insn.operand? else {
            return None;
        };
        let count = match opcode {
            0xc0 | 0xc1 => insn.imm as u8,
            0xd0 | 0xd1 => 1,
            _ => (self.registers.ecx() & 0xff) as u8,
        };

        if opcode & 1 == 0 {
            let result = self.shift_rotate(
                modrm.reg,
                u32::from(self.read_gpr8(index)),
                count,
                BusWidth::Byte,
            ) as u8;
            self.write_gpr8(index, result);
        } else {
            let result = self.shift_rotate(
                modrm.reg,
                self.read_gpr_sized(index, insn.operand_size),
                count,
                insn.operand_size.bus_width(),
            );
            self.write_gpr_sized(index, insn.operand_size, result);
        }

        Some(clocks(2))
    }

    #[inline]
    fn execute_hot_cached_group1_memory<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<Option<CycleOutcome>> {
        let opcode = insn.opcode as u8;
        if !matches!(opcode, 0x80..=0x83) {
            return Ok(None);
        }
        let Some(modrm) = insn.modrm else {
            return Ok(None);
        };
        let Some(DecodedOperand::Mem(addr)) = insn.operand else {
            return Ok(None);
        };
        let memory = self.resolve_memory_addr_mode(&addr);

        match opcode {
            0x80 | 0x82 => {
                let value = self.read_memory_u8(
                    bus,
                    memory.segment,
                    memory.offset,
                    BusAccessKind::DataRead,
                )?;
                let result = self.alu(modrm.reg, u32::from(value), insn.imm, BusWidth::Byte) as u8;
                if modrm.reg != 7 {
                    self.write_memory_u8(
                        bus,
                        memory.segment,
                        memory.offset,
                        result,
                        BusAccessKind::DataWrite,
                    )?;
                }
                Ok(Some(clocks(2)))
            }
            0x81 | 0x83 => {
                let value = self.read_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    insn.operand_size,
                    BusAccessKind::DataRead,
                )?;
                let result = self.alu(modrm.reg, value, insn.imm, insn.operand_size.bus_width());
                if modrm.reg != 7 {
                    self.write_memory_sized(
                        bus,
                        memory.segment,
                        memory.offset,
                        insn.operand_size,
                        result,
                        BusAccessKind::DataWrite,
                    )?;
                }
                Ok(Some(clocks(2)))
            }
            _ => Ok(None),
        }
    }

    #[inline]
    fn execute_hot_cached_datamove<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<Option<CycleOutcome>> {
        let opcode = insn.opcode as u8;
        if !matches!(opcode, 0x88..=0x8b) {
            return Ok(None);
        }
        let Some(modrm) = insn.modrm else {
            return Ok(None);
        };
        let Some(DecodedOperand::Mem(addr)) = insn.operand else {
            return Ok(None);
        };
        let memory = self.resolve_memory_addr_mode(&addr);

        match opcode {
            0x88 => {
                let value = self.read_gpr8(modrm.reg);
                self.write_memory_u8(
                    bus,
                    memory.segment,
                    memory.offset,
                    value,
                    BusAccessKind::DataWrite,
                )?;
                Ok(Some(clocks(2)))
            }
            0x89 => {
                let value = self.read_gpr_sized(modrm.reg, insn.operand_size);
                self.write_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    insn.operand_size,
                    value,
                    BusAccessKind::DataWrite,
                )?;
                Ok(Some(clocks(2)))
            }
            0x8a => {
                let value = self.read_memory_u8(
                    bus,
                    memory.segment,
                    memory.offset,
                    BusAccessKind::DataRead,
                )?;
                self.write_gpr8(modrm.reg, value);
                Ok(Some(clocks(2)))
            }
            0x8b => {
                let value = self.read_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    insn.operand_size,
                    BusAccessKind::DataRead,
                )?;
                self.write_gpr_sized(modrm.reg, insn.operand_size, value);
                Ok(Some(clocks(2)))
            }
            _ => Ok(None),
        }
    }

    /// Specialized `mov r8, [mem]` (0x8A) execute for a JIT `MemLoadU8` slot: the exact body of
    /// `execute_hot_cached_datamove`'s 0x8A arm, reached WITHOUT the group/opcode dispatch chain
    /// (`execute_hot_cached_decoded`'s register-only probe + the group match + the datamove opcode
    /// match). Bit-identical to the interpreter by construction — it calls the same
    /// `resolve_memory_addr_mode` / `read_memory_u8` / `write_gpr8` and returns the same `clocks(2)` —
    /// so it inherits every segment/paging/fault/SMC/BusTrace behavior for free, in every CPU mode.
    /// Any shape the classifier did not intend (defensive) falls back to the full dispatch, so the
    /// result is identical regardless.
    #[cfg(feature = "jit")]
    pub(crate) fn jit_execute_load_u8<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        if insn.opcode == 0x8a {
            if let (Some(modrm), Some(DecodedOperand::Mem(addr))) = (insn.modrm, insn.operand) {
                let memory = self.resolve_memory_addr_mode(&addr);
                let value = self.read_memory_u8(
                    bus,
                    memory.segment,
                    memory.offset,
                    BusAccessKind::DataRead,
                )?;
                self.write_gpr8(modrm.reg, value);
                return Ok(clocks(2));
            }
        }
        self.execute_hot_cached_or_decoded(insn, bus)
    }

    /// Specialized `mov [mem], r8` (0x88) execute for a JIT `MemStoreU8` slot: the exact body of
    /// `execute_hot_cached_datamove`'s 0x88 arm, reached WITHOUT the group/opcode dispatch chain.
    /// Bit-identical to the interpreter by construction — same `resolve_memory_addr_mode` /
    /// `read_gpr8` / `write_memory_u8` / `clocks(2)`. In particular `write_memory_u8` runs
    /// `note_code_write` on every store, so the SMC code-write watch (Round 3 trap #2) is inherited
    /// for free, along with the segment/paging/fault/BusTrace behavior, in every CPU mode. Any shape
    /// the classifier did not intend falls back to the full dispatch, so the result is identical.
    #[cfg(feature = "jit")]
    pub(crate) fn jit_execute_store_u8<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        if insn.opcode == 0x88 {
            if let (Some(modrm), Some(DecodedOperand::Mem(addr))) = (insn.modrm, insn.operand) {
                let memory = self.resolve_memory_addr_mode(&addr);
                let value = self.read_gpr8(modrm.reg);
                self.write_memory_u8(
                    bus,
                    memory.segment,
                    memory.offset,
                    value,
                    BusAccessKind::DataWrite,
                )?;
                return Ok(clocks(2));
            }
        }
        self.execute_hot_cached_or_decoded(insn, bus)
    }

    /// Specialized `mov r16/r32, [mem]` (0x8B) execute for a JIT `MemLoadSized` slot: the exact body
    /// of `execute_hot_cached_datamove`'s 0x8B arm, reached WITHOUT the group/opcode dispatch chain.
    /// Bit-identical to the interpreter — same `resolve_memory_addr_mode` / `read_memory_sized`
    /// (which does the alignment/#AC, page-cross and segment/paging checks for the width) /
    /// `write_gpr_sized` / `clocks(2)`. `insn.operand_size` is the captured decode size (unprefixed in
    /// a region, so it is the segment default), so word and dword loads are both correct. Any
    /// unexpected shape falls back to the full dispatch.
    #[cfg(feature = "jit")]
    pub(crate) fn jit_execute_load_sized<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        if insn.opcode == 0x8b {
            if let (Some(modrm), Some(DecodedOperand::Mem(addr))) = (insn.modrm, insn.operand) {
                let memory = self.resolve_memory_addr_mode(&addr);
                let value = self.read_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    insn.operand_size,
                    BusAccessKind::DataRead,
                )?;
                self.write_gpr_sized(modrm.reg, insn.operand_size, value);
                return Ok(clocks(2));
            }
        }
        self.execute_hot_cached_or_decoded(insn, bus)
    }

    /// Specialized `mov [mem], r16/r32` (0x89) execute for a JIT `MemStoreSized` slot: the exact body
    /// of `execute_hot_cached_datamove`'s 0x89 arm, reached WITHOUT the group/opcode dispatch chain.
    /// Bit-identical — same `resolve_memory_addr_mode` / `read_gpr_sized` / `write_memory_sized`
    /// (which runs `note_code_write`, so the SMC watch and the alignment/page-cross/segment/paging
    /// checks are inherited) / `clocks(2)`, in every mode. Any unexpected shape falls back.
    #[cfg(feature = "jit")]
    pub(crate) fn jit_execute_store_sized<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        if insn.opcode == 0x89 {
            if let (Some(modrm), Some(DecodedOperand::Mem(addr))) = (insn.modrm, insn.operand) {
                let memory = self.resolve_memory_addr_mode(&addr);
                let value = self.read_gpr_sized(modrm.reg, insn.operand_size);
                self.write_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    insn.operand_size,
                    value,
                    BusAccessKind::DataWrite,
                )?;
                return Ok(clocks(2));
            }
        }
        self.execute_hot_cached_or_decoded(insn, bus)
    }

    #[inline]
    fn execute_hot_cached_flags_misc<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<Option<CycleOutcome>> {
        match insn.opcode as u8 {
            0x84 => {
                let Some(modrm) = insn.modrm else {
                    return Ok(None);
                };
                let Some(DecodedOperand::Mem(addr)) = insn.operand else {
                    return Ok(None);
                };
                let memory = self.resolve_memory_addr_mode(&addr);
                let value = self.read_memory_u8(
                    bus,
                    memory.segment,
                    memory.offset,
                    BusAccessKind::DataRead,
                )?;
                let reg = self.read_gpr8(modrm.reg);
                self.alu(4, u32::from(value), u32::from(reg), BusWidth::Byte);
                Ok(Some(clocks(2)))
            }
            0x85 => {
                let Some(modrm) = insn.modrm else {
                    return Ok(None);
                };
                let Some(DecodedOperand::Mem(addr)) = insn.operand else {
                    return Ok(None);
                };
                let memory = self.resolve_memory_addr_mode(&addr);
                let value = self.read_memory_sized(
                    bus,
                    memory.segment,
                    memory.offset,
                    insn.operand_size,
                    BusAccessKind::DataRead,
                )?;
                let reg = self.read_gpr_sized(modrm.reg, insn.operand_size);
                self.alu(4, value, reg, insn.operand_size.bus_width());
                Ok(Some(clocks(2)))
            }
            0x98 => {
                match insn.operand_size {
                    OperandSize::Word => {
                        let ax = i16::from(self.read_gpr8(0) as i8) as u16;
                        self.write_gpr16(0, ax);
                    }
                    OperandSize::Dword => {
                        let eax = i32::from(self.read_gpr16(0) as i16) as u32;
                        self.write_gpr32(0, eax);
                    }
                }
                Ok(Some(clocks(3)))
            }
            0x99 => {
                match insn.operand_size {
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
                Ok(Some(clocks(2)))
            }
            0x9e => {
                self.materialize_flags();
                let ah = u32::from(self.read_gpr8(4));
                self.registers.eflags = (self.registers.eflags & !0xd5) | (ah & 0xd5) | 0x02;
                Ok(Some(clocks(3)))
            }
            0x9f => {
                self.materialize_flags();
                let ah = ((self.registers.eflags as u8) & 0xd5) | 0x02;
                self.write_gpr8(4, ah);
                Ok(Some(clocks(2)))
            }
            0xf5 => {
                self.set_flag(FLAG_CF, !self.flag(FLAG_CF));
                Ok(Some(clocks(2)))
            }
            0xf8 => {
                self.set_flag(FLAG_CF, false);
                Ok(Some(clocks(2)))
            }
            0xf9 => {
                self.set_flag(FLAG_CF, true);
                Ok(Some(clocks(2)))
            }
            0xfc => {
                self.set_flag(FLAG_DF, false);
                Ok(Some(clocks(2)))
            }
            0xfd => {
                self.set_flag(FLAG_DF, true);
                Ok(Some(clocks(2)))
            }
            _ => Ok(None),
        }
    }

    #[inline]
    fn execute_hot_cached_stack<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<Option<CycleOutcome>> {
        let opcode = insn.opcode as u8;
        let operand_size = insn.operand_size;
        match opcode {
            0x50..=0x57 => {
                let value = self.read_gpr_sized(opcode - 0x50, operand_size);
                self.push(bus, value, operand_size)?;
                Ok(Some(clocks(2)))
            }
            0x58..=0x5f => {
                let value = self.pop(bus, operand_size)?;
                self.write_gpr_sized(opcode - 0x58, operand_size, value);
                Ok(Some(clocks(4)))
            }
            0x68 => {
                self.push(bus, insn.imm, operand_size)?;
                Ok(Some(clocks(2)))
            }
            0x6a => {
                self.push(bus, sign_extend_u8(insn.imm as u8), operand_size)?;
                Ok(Some(clocks(2)))
            }
            _ => Ok(None),
        }
    }

    #[inline]
    fn execute_hot_cached_branch<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<Option<CycleOutcome>> {
        if insn.opcode as u8 != 0xe8 {
            return Ok(None);
        }

        self.push(bus, self.registers.eip, insn.operand_size)?;
        self.relative_jump(insn.imm as i32, insn.operand_size);
        Ok(Some(clocks(7)))
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
                // Two-byte bit-manipulation block (task A10): BT/BTS/BTR/BTC reg (A3/AB/B3/BB),
                // BT/BTS/BTR/BTC imm8 (BA group 8), BSF/BSR (BC/BD), SHLD/SHRD (A4/A5/AC/AD),
                // CMPXCHG (B0/B1), XADD (C0/C1). Every one is a ModRM r/m form.
                0xa3 | 0xab | 0xb3 | 0xbb | 0xba | 0xbc | 0xbd | 0xa4 | 0xa5 | 0xac | 0xad
                | 0xb0 | 0xb1 | 0xc0 | 0xc1 => DecodeGroup::BitManip,
                // Two-byte conditional-move / SETcc / two-operand IMUL block (task A11):
                // CMOVcc reg,r/m (40-4F, 586-class), SETcc r/m8 (90-9F, 386-class), and
                // IMUL reg,r/m (AF, 386-class). All are ModRM r/m forms with no immediate.
                0x40..=0x4f | 0x90..=0x9f | 0xaf => DecodeGroup::CondMove,
                // System / descriptor-table / segment block (task A12), 0F forms: the descriptor
                // groups 0F 00 (group 6) and 0F 01 (group 7), LAR/LSL (0F 02/03), CLTS (0F 06),
                // MOV reg,CR / MOV CR,reg (0F 20/22), MOV reg,DR / MOV DR,reg (0F 21/23, ledger
                // row 25), and LSS/LFS/LGS (0F B2/B4/B5, a far-pointer load like LES/LDS but into
                // SS/FS/GS).
                0x00 | 0x01 | 0x02 | 0x03 | 0x06 | 0x20 | 0x21 | 0x22 | 0x23 | 0xb2 | 0xb4
                | 0xb5 => DecodeGroup::SystemSeg,
                // Heterogeneous one-off 0F block (task A14): the no-operand system/serializing/CPU-id
                // ops SYSCALL/SYSRET (05/07), INVD/WBINVD (08/09), WRMSR/RDTSC/RDMSR (30/31/32),
                // CPUID (A2), BSWAP (C8-CF); CMPXCHG8B (C7, a ModRM form); PUSH/POP FS/GS
                // (A0/A1/A8/A9, 386+, mirroring the one-byte ES/SS/DS segment push/pop arms in
                // `execute_stack_decoded`); and the whole MMX block (`is_mmx_two_byte`). 0F AA
                // (RSM) is unimplemented and stays TwoByteFallback.
                0x05
                | 0x07
                | 0x08
                | 0x09
                | 0x30
                | 0x31
                | 0x32
                | 0xa0
                | 0xa1
                | 0xa2
                | 0xa8
                | 0xa9
                | 0xc7
                | 0xc8..=0xcf => DecodeGroup::Misc,
                second if is_mmx_two_byte(second as u8) => DecodeGroup::Misc,
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
        // sit between them and are deliberately excluded — they are not string ops and route to Misc.
        if matches!(opcode, 0xa4..=0xa7 | 0xaa..=0xaf) {
            return DecodeGroup::StringOps;
        }
        // Port I/O block (task A9): IN AL imm8 (0xe4), IN AX/EAX imm8 (0xe5), OUT imm8 AL (0xe6),
        // OUT imm8 AX/EAX (0xe7), IN AL DX (0xec), IN AX/EAX DX (0xed), OUT DX AL (0xee),
        // OUT DX AX/EAX (0xef). 0xe0-0xe3 are the loop/JCXZ branches (DecodeGroup::Branch) and are
        // already routed above; 0xe8/0xe9/0xeb are CALL/JMP (also Branch). The INS/OUTS forms
        // (0x6c-0x6f) are NOT listed here — they route to Misc.
        if matches!(opcode, 0xe4..=0xe7 | 0xec..=0xef) {
            return DecodeGroup::PortIo;
        }
        // System / descriptor-table / segment block (task A12), single-byte forms: BOUND r,m
        // (0x62) and LES/LDS (0xc4/0xc5). Each is a ModRM r/m form whose memory operand decode
        // pre-parses; the far pointer for LES/LDS is read FROM MEMORY at execute. 0x63 (ARPL) is
        // unimplemented in the fused path (`UnsupportedOpcode`) and stays on Fallback.
        if matches!(opcode, 0x62 | 0xc4 | 0xc5) {
            return DecodeGroup::SystemSeg;
        }
        // x87 FPU block (task A13): the eight escape opcodes 0xD8-0xDF (each a ModRM r/m or
        // register form) and WAIT/FWAIT (0x9B, no ModRM). `decode` fetches the ModRM once (and the
        // addressing descriptor for the mod != 3 memory forms); the executor reproduces the
        // fused #MF gate and calls the existing `execute_fpu_register`/`execute_fpu_memory`.
        if matches!(opcode, 0x9b | 0xd8..=0xdf) {
            return DecodeGroup::Fpu;
        }
        // Heterogeneous one-off single-byte block (task A14): BCD adjust DAA/DAS/AAA/AAS
        // (0x27/0x2f/0x37/0x3f), three-operand IMUL (0x69/0x6b), string port I/O INS/OUTS
        // (0x6c-0x6f), TEST AL/AX,imm (0xa8/0xa9), AAM/AAD (0xd4/0xd5), SALC/XLAT (0xd6/0xd7),
        // and HLT (0xf4). 0x63 (ARPL) and 0xf1 are unimplemented in the fused path and stay
        // on Fallback (they #UD as `UnsupportedOpcode`).
        if matches!(
            opcode,
            0x27 | 0x2f | 0x37 | 0x3f | 0x69 | 0x6b | 0x6c
                ..=0x6f | 0xa8 | 0xa9 | 0xd4 | 0xd5 | 0xd6 | 0xd7 | 0xf4
        ) {
            return DecodeGroup::Misc;
        }
        DecodeGroup::Fallback
    }

    /// Stage B fetch front-end. Returns the decoded instruction for the current linear EIP, served
    /// from the decode cache on a hit (re-decode skipped) or decoded once and cached on a miss. On a
    /// hit, `decode` does not run, so this replays the instruction-fetch clocks `decode` would have
    /// charged and advances eip past the instruction, leaving the CPU in exactly the state the miss
    /// path produces before `execute_decoded` runs (eip at the instruction end; the same fetch bus
    /// cycles charged). The prefetch window is not touched on a hit because `execute_decoded` reads
    /// operands over the data bus, never the instruction stream.
    fn fetch_decoded<B: CpuBus>(&mut self, bus: &mut B, lin: u32) -> ExecResult<DecodedInsn> {
        let cs = self.registers.cs();
        if let Some(insn) = self.decode_cache.get(lin, cs.default_size_32) {
            // Live fetch-limit recheck: the line may have been cached under a larger CS limit
            // (CS loads no longer flush the cache). A violation falls through to `decode`,
            // which enforces the fault at exactly the byte the fetch would have crossed.
            if Self::fetch_within_limit(self.registers.eip, insn.len, cs.limit) {
                self.charge_cached_fetch(bus, lin, insn.len)?;
                return Ok(insn);
            }
        }
        let insn = self.decode(bus)?;
        self.perf.decode_misses += 1;
        // A LOCK-prefixed instruction is never cached: `decode` runs `check_lock_target`, which both
        // peeks the lock target over the bus (charging fetch clocks that are NOT part of `len`, so a
        // cached replay would under-charge them) and raises #UD for a non-lockable target. Replaying
        // it from the cache would skip both. LOCK is rare, so re-decoding it every time is free.
        if !insn.prefixes.lock && !insn.no_cache {
            // Mark the physical block(s) this instruction occupies so a later write into them
            // invalidates the cache (cross-page SMC). decode just warmed the code-page translation,
            // so resolving the physical start is a cache hit (and the identity map without paging). A
            // page-straddling instruction under paging marks the tail block from the contiguous
            // physical of its first page, which is the one remaining exotic gap.
            let physical = self.translate_code_linear(bus, lin)?;
            self.decode_cache.mark_code_range(physical, insn.len);
            self.decode_cache
                .put(lin, insn, cs.default_size_32, physical);
        }
        Ok(insn)
    }

    /// Charge the instruction-fetch bus clocks for a decode-cache HIT and advance eip past the
    /// instruction. This is an I-CACHE HIT: the (already-decoded) instruction is served from the
    /// instruction cache, so `charge_instruction_fetch_run` charges it as a SINGLE I-cache access for
    /// cacheable RAM (the bus collapses the run to one cycle there; ROM/device code stays per byte).
    ///
    /// Calibration note (B-T8/B-T9): the COLD decode path (`decode` -> `fetch_u8`) still charges one
    /// fetch cycle per byte PLUS the opcode double-charge (`read_prefixes` peeks the opcode, then
    /// `decode` re-fetches it), i.e. `len + 1` cycles. This warm replay no longer mirrors that: the
    /// `len + 1` per-byte charge and the opcode double-charge are slow-bus/decode-time artifacts, not
    /// I-cache costs. Charging them on every execution floored the fast modes' Dhrystone/Sieve far
    /// below their era bands. A warm hit costs one I-cache access; the cold decode legitimately costs
    /// more. Over a benchmark loop the warm replay dominates, so the per-mode metric reflects the
    /// I-cache cost. The first (cold) execution costing more is physically correct and guest-invisible
    /// (it changes only the bus-clock metric, never a result).
    fn charge_cached_fetch<B: CpuBus>(&mut self, bus: &mut B, lin: u32, len: u8) -> ExecResult<()> {
        bus.charge_instruction_fetch_run(lin, u32::from(len))?;
        self.registers.eip = self.registers.eip.wrapping_add(u32::from(len));
        Ok(())
    }

    /// Whether an instruction of `len` bytes starting at `eip` fetches entirely within the CS
    /// `limit`. The cached-hit counterpart of the per-byte limit check `decode`'s `fetch_u8`
    /// performs: a `false` here must MISS to `decode` so the #GP is raised at the same byte.
    #[inline]
    fn fetch_within_limit(eip: u32, len: u8, limit: u32) -> bool {
        // `limit - (len - 1)` is the last start offset whose full fetch stays inside; a limit
        // smaller than `len - 1` admits no start at all (checked_sub catches it).
        match limit.checked_sub(u32::from(len) - 1) {
            Some(last_ok_start) => eip <= last_ok_start,
            None => false,
        }
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
        let (opcode, isa_gate_exempt) = if opcode == 0x0f {
            let second = self.fetch_u8(bus)?;
            let exempt_used = self.check_two_byte_isa_gate(second)?;
            (0x0f00u16 | u16::from(second), exempt_used)
        } else {
            (u16::from(opcode), false)
        };

        // The single `route_group` authority runs ONCE here; the result is stored in the insn so
        // `execute_decoded` matches the variant directly rather than re-classifying the opcode.
        let group = Self::route_group(opcode, prefixes);

        let mut insn = DecodedInsn {
            // `len` is a placeholder here; the single finalize after the group pre-parse below
            // overwrites it with the real consumed length (prefixes + opcode + operands).
            len: 0,
            prefixes,
            opcode,
            operand_size,
            address_size,
            modrm: None,
            operand: None,
            imm: 0,
            imm2: 0,
            group,
            // Placeholder; the finalize below resolves it once the ModRM (the 0xFF /ext
            // discriminator) has been pre-parsed.
            continuable: false,
            // Both ISA-gate exemptions are context, not bytes, so neither may be cached: the
            // two-byte gate reports its exemption directly; a 66/67 prefix at a pre-386 level
            // can only have survived read_prefixes via the same firmware-ROM/ring-0 exemption
            // (any other context #UDs there), so the prefix flags themselves are the signal.
            no_cache: isa_gate_exempt
                || (self.level.is_pre_386()
                    && (prefixes.operand_size_override || prefixes.address_size_override)),
        };

        // Pre-parse the operands of converted groups, dispatching on the group resolved above.
        match group {
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
            DecodeGroup::PortIo => {
                // Port I/O block. The imm8 forms (0xe4-0xe7) carry one port-number byte after the
                // opcode; read it here (charging its instruction-fetch exactly once) and store it in
                // `insn.imm`. The DX forms (0xec-0xef) carry no extra bytes — the port comes from DX
                // at execute time. No ModRM in any form.
                // The imm8 forms carry one port-number byte; the DX forms (0xec..=0xef) do not.
                if let 0xe4..=0xe7 = opcode {
                    insn.imm = u32::from(self.fetch_u8(bus)?);
                }
            }
            DecodeGroup::BitManip => {
                // Two-byte bit-manipulation block. Every opcode is a ModRM r/m form; parse the
                // ModRM + addressing descriptor (instruction bytes only, so it stays cacheable)
                // for all of them. The reg field is the source register for BT/BTS/BTR/BTC reg,
                // SHLD/SHRD, CMPXCHG, and XADD; the destination register for BSF/BSR; and the
                // sub-op selector (the /ext) for the 0F BA group. The bit-offset-adjusted memory
                // address for the BT-memory reg form is computed at EXECUTE from the live reg bit
                // index (in `bit_string_op`), so decode captures only the base descriptor here.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                insn.modrm = Some(modrm);
                insn.operand = Some(operand);
                // Three forms carry an imm8 AFTER the ModRM+displacement: 0F BA (the bit index)
                // and the SHLD/SHRD imm8 variants 0F A4/AC (the shift count). The CL-count forms
                // 0F A5/AD and the reg-index/reg-source forms carry no immediate.
                if let 0xba | 0xa4 | 0xac = insn.opcode & 0xff {
                    insn.imm = u32::from(self.fetch_u8(bus)?);
                }
            }
            DecodeGroup::CondMove => {
                // Conditional-move / SETcc / two-operand IMUL block (task A11). Every opcode in
                // this group is a ModRM r/m form with no immediate after the ModRM+displacement.
                // Parse the ModRM + addressing descriptor (instruction bytes only, so it stays
                // cacheable); the executor reads `modrm.reg` (CMOVcc/IMUL destination) and the
                // r/m operand at execute time. No imm8 is ever present, so no `insn.imm` fetch.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                insn.modrm = Some(modrm);
                insn.operand = Some(operand);
            }
            DecodeGroup::SystemSeg => {
                // System / descriptor-table / segment block (task A12). Every opcode here except
                // CLTS (0F 06) carries a ModRM; the /ext (`modrm.reg`) selects the sub-op for the
                // 0F 00 / 0F 01 groups. Parse the ModRM + addressing descriptor (instruction bytes
                // only, so it stays cacheable). None carry an immediate after the ModRM.
                match insn.opcode {
                    // CLTS: no encoded operand.
                    0x0f06 => {}
                    // MOV reg,CR / MOV CR,reg (0F 20/22) and MOV reg,DR / MOV DR,reg (0F 21/23):
                    // the ModRM is always a register form (the `reg` field is the CR/DR number,
                    // `rm` the GPR). The fused path fetches ONLY the ModRM byte and #UDs when
                    // `mode != 3` BEFORE touching any addressing byte, so do the same here: fetch
                    // the ModRM, store it, and DO NOT parse an addressing mode (a non-register
                    // `mode` is rejected in the executor with no extra fetch).
                    0x0f20..=0x0f23 => {
                        let modrm = self.fetch_modrm(bus)?;
                        insn.modrm = Some(modrm);
                    }
                    // The 0F 00/01/02/03 groups, BOUND (0x62), and LES/LDS (0xc4/0xc5): a normal
                    // ModRM r/m form. Parse the ModRM + its addressing descriptor.
                    _ => {
                        let modrm = self.fetch_modrm(bus)?;
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.modrm = Some(modrm);
                        insn.operand = Some(operand);
                    }
                }
            }
            DecodeGroup::Fpu => {
                // x87 FPU block (task A13). WAIT/FWAIT (0x9B) has no ModRM — nothing to pre-parse.
                // Each escape opcode (0xD8-0xDF) carries a ModRM: fetch it once here. The fused
                // handler treated `mod == 3` as the register form (it dispatched on the raw ModRM
                // byte WITHOUT decoding an addressing mode) and `mod != 3` as a memory operand (it
                // decoded the addressing mode). Mirror that split exactly so the same instruction
                // bytes are consumed and charged once: store the ModRM always, and parse the
                // addressing descriptor ONLY for the memory forms. No FPU opcode carries an
                // immediate after the ModRM.
                if opcode != 0x9b {
                    let modrm = self.fetch_modrm(bus)?;
                    if modrm.mode != 3 {
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.operand = Some(operand);
                    }
                    insn.modrm = Some(modrm);
                }
            }
            DecodeGroup::Misc => {
                // The heterogeneous one-off block (task A14). Each opcode reads exactly the bytes
                // its fused handler read, in the same order, so the fetch clocks stay byte-identical.
                match insn.opcode {
                    // Three-operand IMUL: a ModRM r/m form THEN an immediate (operand-size-wide for
                    // 0x69, sign-extended imm8 for 0x6b). Parse the ModRM + addressing descriptor
                    // (instruction bytes only, so it stays cacheable), then fetch the immediate.
                    0x69 => {
                        let modrm = self.fetch_modrm(bus)?;
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.modrm = Some(modrm);
                        insn.operand = Some(operand);
                        insn.imm = self.fetch_immediate(bus, operand_size)?;
                    }
                    0x6b => {
                        let modrm = self.fetch_modrm(bus)?;
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.modrm = Some(modrm);
                        insn.operand = Some(operand);
                        insn.imm = sign_extend_u8(self.fetch_u8(bus)?);
                    }
                    // AAM/AAD (0xd4/0xd5): the imm8 base (TEST AL,imm8 0xa8 likewise): fetch one byte.
                    0xa8 | 0xd4 | 0xd5 => {
                        insn.imm = u32::from(self.fetch_u8(bus)?);
                    }
                    // TEST AX/EAX,imm (0xa9): an operand-size-wide accumulator immediate.
                    0xa9 => {
                        insn.imm = self.fetch_immediate(bus, operand_size)?;
                    }
                    // CMPXCHG8B (0F C7 /1): a ModRM r/m (m64) form, no immediate. Parse the ModRM +
                    // addressing descriptor; the executor re-detects the register form / bad /ext.
                    0x0fc7 => {
                        let modrm = self.fetch_modrm(bus)?;
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.modrm = Some(modrm);
                        insn.operand = Some(operand);
                    }
                    // The MMX shift-by-immediate forms (0F 71/72/73). The fused path read ONLY the
                    // ModRM byte and then the imm8 count — it never decoded an addressing mode (these
                    // are register-form, `modrm.rm` is the target). Mirror that exactly so the byte
                    // budget matches even the malformed mode != 3 encoding: ModRM, then imm8, with no
                    // addressing-descriptor parse.
                    0x0f71..=0x0f73 => {
                        let modrm = self.fetch_modrm(bus)?;
                        insn.modrm = Some(modrm);
                        insn.imm = u32::from(self.fetch_u8(bus)?);
                    }
                    // The rest of the MMX block, except EMMS (0F 77), which has no ModRM and falls to
                    // the no-operand arm below. Every other MMX opcode is a ModRM r/m form: parse the
                    // ModRM + addressing descriptor. (MOVD/MOVQ and the Pxxx forms carry no immediate.)
                    op if op != 0x0f77 && op & 0xff00 == 0x0f00 && is_mmx_two_byte(op as u8) => {
                        let modrm = self.fetch_modrm(bus)?;
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.modrm = Some(modrm);
                        insn.operand = Some(operand);
                    }
                    // Every other one-off carries no encoded operand after the opcode byte(s):
                    // the BCD adjusts (0x27/0x2f/0x37/0x3f), SALC/XLAT (0xd6/0xd7), INS/OUTS
                    // (0x6c-0x6f), HLT (0xf4), EMMS (0F 77), and the no-operand 0F system/serializing/
                    // CPU-id ops (05/07/08/09/30/31/32/a2/c8-cf). XLAT reads memory at execute from
                    // live registers; the rest take implicit/register/no operands.
                    _ => {}
                }
            }
            // Both fallback groups pre-parse nothing in `decode` (the second 0F byte was already
            // folded into `insn.opcode` above): their executors re-read any ModRM/immediate from the
            // post-opcode eip in the shared fused dispatch.
            DecodeGroup::Fallback | DecodeGroup::TwoByteFallback => {}
        }

        // Finalize `len` once, after every group's pre-parse, so a converted group never has to
        // re-write it: a group's match arm only fetches its operand bytes; this single assignment
        // captures the total bytes `decode` consumed (prefixes + opcode + operands). Any future
        // early `Ok` return before this line would skip BOTH `len` and `continuable`.
        insn.len = self.registers.eip.wrapping_sub(start_eip) as u8;
        // Resolve the continuation gate once per decode (the ModRM is in by now), so the
        // per-continuation check in `run_straight_line` reads a single cached flag.
        insn.continuable = block_continuable(insn.group, insn.opcode, insn.modrm, self.level);

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
            // The port I/O block (IN AL/AX/EAX, OUT AL/AX/EAX, both imm8-port and DX-port forms)
            // runs through its split executor, which calls `bus.read_io`/`bus.write_io` on the same
            // port-dispatch path as the fused arms — so `io_touched` is set exactly as before.
            DecodeGroup::PortIo => self.execute_port_io_decoded(insn, bus),
            // The two-byte bit-manipulation block (BT/BTS/BTR/BTC, BSF/BSR, SHLD/SHRD, CMPXCHG,
            // XADD) runs through its split executor, consuming the pre-decoded ModRM/operand (and
            // the pre-fetched imm8 for 0F BA/A4/AC) and reusing `bit_string_op`/`double_shift`/
            // `alu_sub`/`alu_add` verbatim so the bit-addressing and flag logic stays in one place.
            DecodeGroup::BitManip => self.execute_bitmanip_decoded(insn, bus),
            // The two-byte conditional-move / SETcc / two-operand IMUL block (CMOVcc, SETcc,
            // IMUL reg,r/m) runs through its split executor, consuming the pre-decoded
            // ModRM/operand and reusing `self.condition` (the same helper Jcc and the fused
            // CMOVcc/SETcc arms used) and `self.imul_truncated` verbatim.
            DecodeGroup::CondMove => self.execute_condmove_decoded(insn, bus),
            // The system / descriptor-table / segment block (0F 00/01/02/03/06/20/22, BOUND,
            // LES/LDS) runs through its split executor, consuming the pre-decoded ModRM/operand and
            // reusing the existing CR/segment/descriptor leaf helpers verbatim so the TLB and
            // code-cache invalidation hooks fire exactly as before.
            DecodeGroup::SystemSeg => self.execute_system_seg_decoded(insn, bus),
            // The x87 FPU block (0xD8-0xDF) + WAIT/FWAIT (0x9B) runs through its split executor: a
            // thin wrapper that reproduces the fused pending-#MF gate, then resolves the pre-decoded
            // ModRM/operand (for the memory forms) and calls the existing `execute_fpu_register` /
            // `execute_fpu_memory` verbatim — the entire x87 stack/control/status logic stays in
            // those leaf helpers unchanged.
            DecodeGroup::Fpu => self.execute_fpu_decoded(insn, bus),
            // The heterogeneous one-off block (BCD adjust, AAM/AAD, SALC/XLAT, TEST imm, three-
            // operand IMUL, INS/OUTS, HLT, and the no-operand 0F system/serializing/CPU-id ops,
            // CMPXCHG8B, and MMX) runs through its split executor, consuming the pre-decoded
            // ModRM/operand/immediate and reusing the existing BCD/`imul_truncated`/`run_string`/
            // CPUID/RDTSC/`syscall`/halt/MMX leaf logic verbatim.
            DecodeGroup::Misc => self.execute_misc_decoded(insn, bus),
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
                // genuinely-unimplemented ones (0x63 ARPL, 0xF1 ICEBP) and — as a decode-bug guard —
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
                Ok(clocks(2))
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
                Ok(clocks(2))
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
                Ok(clocks(2))
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
                self.load_segment_arming_ss_shadow(bus, segment, value as u16)?;
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
                    return Err(undefined_opcode());
                }
                let (_, operand) = self.resolve_decoded_modrm_operand(insn);
                self.write_operand_u8(bus, operand, insn.imm as u8)?;
                Ok(clocks(2))
            }
            0xc7 => {
                // MOV r/m16/32, imm16/32 (group 11). Same reg=000 gate as 0xc6.
                let modrm = insn.modrm.expect("group-11 form decoded with a ModRM");
                if modrm.reg != 0 {
                    return Err(undefined_opcode());
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
                Ok(clocks(2))
            }
            0x07 => {
                // POP ES. 386 PRM: a 32-bit operand size pops a full dword and loads the
                // low 16 bits, discarding the upper half; a 16-bit operand size pops 2 bytes.
                let value = self.pop(bus, operand_size)? as u16;
                self.load_segment(bus, SegmentIndex::Es, value)?;
                Ok(clocks(7))
            }
            0x0e => {
                // PUSH CS. Same 386 PRM operand-size rule as PUSH ES above.
                self.push(
                    bus,
                    u32::from(self.registers.segment(SegmentIndex::Cs).selector),
                    operand_size,
                )?;
                Ok(clocks(2))
            }
            0x16 => {
                // PUSH SS. Same 386 PRM operand-size rule as PUSH ES above.
                self.push(
                    bus,
                    u32::from(self.registers.segment(SegmentIndex::Ss).selector),
                    operand_size,
                )?;
                Ok(clocks(2))
            }
            0x17 => {
                // POP SS. Arms the one-instruction interrupt shadow like MOV SS (386 PRM 11-16),
                // so a following POP (E)SP is guaranteed to run before any interrupt is taken.
                // Same 386 PRM operand-size rule as POP ES above.
                let value = self.pop(bus, operand_size)? as u16;
                self.load_segment_arming_ss_shadow(bus, SegmentIndex::Ss, value)?;
                Ok(clocks(7))
            }
            0x1e => {
                // PUSH DS. Same 386 PRM operand-size rule as PUSH ES above.
                self.push(
                    bus,
                    u32::from(self.registers.segment(SegmentIndex::Ds).selector),
                    operand_size,
                )?;
                Ok(clocks(2))
            }
            0x1f => {
                // POP DS. Same 386 PRM operand-size rule as POP ES above.
                let value = self.pop(bus, operand_size)? as u16;
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
                // A fault on ANY of the eight pushes restores (E)SP to the
                // pre-instruction value (386 PRM: PUSHA restores ESP so the
                // instruction restarts whole; individual committed sub-pushes
                // are just re-written on the restart).
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
                // On a 16-bit stack (SS.B=0), POPAD leaves SP advanced but lets the
                // discarded saved-ESP slot's high half land in ESP[31:16]. Verified
                // against the 80386 vectors; the register loads above are unaffected.
                if !self.stack_is_32bit() && matches!(operand_size, OperandSize::Dword) {
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
                Ok(clocks(5))
            }
            0x9c => {
                // PUSHF / PUSHFD. The low 16 flag bits push the same in both forms. The
                // dword form additionally carries the 486 AC and ID bits (RF and VM are
                // masked to 0 in the pushed image). operand_size drives whether push writes
                // 2 or 4 bytes.
                self.check_v86_iopl()?;
                // Settle any deferred arithmetic flags so the pushed image has live CF/PF/AF/ZF/SF/OF.
                self.materialize_flags();
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
                self.load_flags(value, operand_size, false);
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
                Ok(clocks(10))
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
                        return Err(undefined_opcode());
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
                        return Err(undefined_opcode());
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
                Ok(clocks(2))
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
                // Settle deferred flags first: the read-modify-write reads registers.eflags to
                // preserve OF and control bits, so a stale descriptor would corrupt OF in the result.
                self.materialize_flags();
                let ah = u32::from(self.read_gpr8(4));
                self.registers.eflags = (self.registers.eflags & !0xd5) | (ah & 0xd5) | 0x02;
                Ok(clocks(3))
            }
            0x9f => {
                // LAHF: AH = low flag byte with bit1 forced 1, bits 3 and 5 forced 0.
                // Settle deferred flags so the captured low byte (CF/PF/AF/ZF/SF) is live.
                self.materialize_flags();
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

    /// The port I/O block through the decode/execute split (task A9). Calls `bus.read_io` /
    /// `bus.write_io` on the same path as the former fused arms, so `io_touched` is set exactly
    /// as before. For the imm8 forms (0xe4-0xe7) `decode` pre-read the port number into `insn.imm`;
    /// for the DX forms (0xec-0xef) the port comes from the DX register (GPR index 2) at execute
    /// time. The low bit of the opcode selects the I/O direction within each pair (0 = IN, 1 = OUT
    /// only for 0xe4/0xe5 vs 0xe6/0xe7, respectively; 0 = IN, 1 = unused for the 0xec range where
    /// bit 1 distinguishes direction: see comments per arm). Clocks match the fused arms verbatim
    /// (12 for IN, 10 for OUT).
    /// In V86 (or protected mode with CPL > IOPL), `IN`/`OUT` consult the TSS
    /// I/O-permission bitmap: the access is allowed only if every bit for ports
    /// `port..port+width` is 0. A bit at or beyond the TSS limit is treated as set
    /// (not permitted). A denied access faults `#GP(0)` to the monitor.
    fn check_io_permission<B: CpuBus>(
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
        let operand_size = insn.operand_size;
        match insn.opcode as u8 {
            0xe4 => {
                // IN AL, imm8: byte port input. `decode` stored the port number in `insn.imm`.
                let port = insn.imm as u16;
                self.check_io_permission(bus, port, BusWidth::Byte)?;
                let value = bus.read_io(
                    port,
                    BusWidth::Byte,
                    self.core_clocks_so_far,
                    self.is_ring0_protected(),
                )? as u8;
                self.write_gpr8(0, value);
                Ok(clocks(12))
            }
            0xe5 => {
                // IN AX/EAX, imm8: word/dword port input into the accumulator.
                let port = insn.imm as u16;
                self.check_io_permission(bus, port, operand_size.bus_width())?;
                let value = bus.read_io(
                    port,
                    operand_size.bus_width(),
                    self.core_clocks_so_far,
                    self.is_ring0_protected(),
                )?;
                self.write_gpr_sized(0, operand_size, value);
                Ok(clocks(12))
            }
            0xe6 => {
                // OUT imm8, AL: byte port output from AL.
                let port = insn.imm as u16;
                self.check_io_permission(bus, port, BusWidth::Byte)?;
                bus.write_io(
                    port,
                    BusWidth::Byte,
                    u32::from(self.read_gpr8(0)),
                    self.is_ring0_protected(),
                )?;
                Ok(clocks(10))
            }
            0xe7 => {
                // OUT imm8, AX/EAX: word/dword port output from the accumulator.
                let port = insn.imm as u16;
                self.check_io_permission(bus, port, operand_size.bus_width())?;
                bus.write_io(
                    port,
                    operand_size.bus_width(),
                    self.read_gpr_sized(0, operand_size),
                    self.is_ring0_protected(),
                )?;
                Ok(clocks(10))
            }
            0xec => {
                // IN AL, DX: byte port input. Port number in DX (GPR 2).
                let port = self.read_gpr16(2);
                self.check_io_permission(bus, port, BusWidth::Byte)?;
                let value = bus.read_io(
                    port,
                    BusWidth::Byte,
                    self.core_clocks_so_far,
                    self.is_ring0_protected(),
                )? as u8;
                self.write_gpr8(0, value);
                Ok(clocks(12))
            }
            0xed => {
                // IN AX/EAX, DX: word/dword port input addressed by DX.
                let port = self.read_gpr16(2);
                self.check_io_permission(bus, port, operand_size.bus_width())?;
                let value = bus.read_io(
                    port,
                    operand_size.bus_width(),
                    self.core_clocks_so_far,
                    self.is_ring0_protected(),
                )?;
                self.write_gpr_sized(0, operand_size, value);
                Ok(clocks(12))
            }
            0xee => {
                // OUT DX, AL: byte port output addressed by DX.
                let port = self.read_gpr16(2);
                self.check_io_permission(bus, port, BusWidth::Byte)?;
                bus.write_io(
                    port,
                    BusWidth::Byte,
                    u32::from(self.read_gpr8(0)),
                    self.is_ring0_protected(),
                )?;
                Ok(clocks(10))
            }
            0xef => {
                // OUT DX, AX/EAX: word/dword port output addressed by DX.
                let port = self.read_gpr16(2);
                self.check_io_permission(bus, port, operand_size.bus_width())?;
                bus.write_io(
                    port,
                    operand_size.bus_width(),
                    self.read_gpr_sized(0, operand_size),
                    self.is_ring0_protected(),
                )?;
                Ok(clocks(10))
            }
            opcode => unreachable!("port-I/O opcode {opcode:#x}"),
        }
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
    fn execute_bitmanip_decoded<B: CpuBus>(
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
                Ok(clocks(10))
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
                Ok(clocks(10))
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
                Ok(clocks(6))
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
                Ok(clocks(6))
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
                Ok(clocks(3))
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
                Ok(clocks(3))
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
                Ok(clocks(6))
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
                Ok(clocks(4))
            }
            opcode => unreachable!("bit-manipulation opcode {opcode:#x}"),
        }
    }

    /// The conditional-move / SETcc / two-operand IMUL block (A11) through the decode/execute split
    /// (task A11). Mirrors the former fused arms in `execute_two_byte` verbatim — same condition
    /// helper, same CMOVcc read-before-conditional-write, same SETcc byte-write, same
    /// `imul_truncated` — but consumes the ModRM/operand pre-decoded by `decode` (no re-fetch).
    /// Dispatches off the FULL u16 `insn.opcode` (0x0F40-0x0F4F, 0x0F90-0x0F9F, 0x0FAF) so the
    /// `as u8` narrowing can never alias these onto single-byte opcodes.
    fn execute_condmove_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
    ) -> ExecResult<CycleOutcome> {
        let operand_size = insn.operand_size;

        match insn.opcode {
            0x0f40..=0x0f4f => {
                // CMOVcc reg, r/m: the source r/m is ALWAYS read (so a faulting memory operand
                // still faults even when the condition is false), but the destination register is
                // written only when the condition holds. A false condition leaves it untouched.
                // The condition code is the low nibble of the second byte (insn.opcode & 0x0f).
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let value = self.read_operand_sized(bus, operand, operand_size)?;
                if self.condition((insn.opcode & 0x0f) as u8) {
                    self.write_gpr_sized(modrm.reg, operand_size, value);
                }
                Ok(clocks(1))
            }
            0x0f90..=0x0f9f => {
                // SETcc r/m8: set the byte operand to 1 when the condition holds, else 0. Always
                // byte-wide regardless of the operand-size prefix. The condition code is the low
                // nibble of the second byte (insn.opcode & 0x0f). Touches no flags.
                let (_, operand) = self.resolve_decoded_modrm_operand(insn);
                let set = self.condition((insn.opcode & 0x0f) as u8);
                self.write_operand_u8(bus, operand, u8::from(set))?;
                Ok(clocks(4))
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
                Ok(clocks(9))
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
    /// 0x62/0xc4/0xc5) so the `as u8` narrowing can never alias a 0F opcode onto a single-byte one.
    fn execute_system_seg_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
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
                        Ok(clocks(2))
                    }
                    1 => {
                        // STR r/m16: store the task-register selector.
                        let selector = u32::from(self.tr.selector);
                        self.write_operand_sized(bus, operand, OperandSize::Word, selector)?;
                        Ok(clocks(2))
                    }
                    2 => {
                        // LLDT r/m16: load the local descriptor table register. Privileged.
                        self.require_cpl0()?;
                        let selector =
                            self.read_operand_sized(bus, operand, OperandSize::Word)? as u16;
                        self.load_ldtr(bus, selector)?;
                        Ok(clocks(11))
                    }
                    3 => {
                        // LTR r/m16: load the task register. Privileged.
                        self.require_cpl0()?;
                        let selector =
                            self.read_operand_sized(bus, operand, OperandSize::Word)? as u16;
                        self.load_tr(bus, selector)?;
                        Ok(clocks(11))
                    }
                    4 | 5 => {
                        // VERR (/4) / VERW (/5): set ZF if the segment is readable / writable.
                        let selector =
                            self.read_operand_sized(bus, operand, OperandSize::Word)? as u16;
                        let ok = self.verify_segment(bus, selector, modrm.reg == 5)?;
                        self.set_flag(FLAG_ZF, ok);
                        Ok(clocks(10))
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
                        Ok(clocks(2))
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
                            self.control.cr0 = cr0;
                            self.recompute_alignment_armed();
                            self.flush_tlb_and_code_caches();
                            // LMSW can only set PE (never clear it, masked out of
                            // `switchable` above), and require_cpl0 above already forced
                            // cpl == 0 -- entering protected mode this way starts at ring 0
                            // per the PRM, and cpl was already 0, so no assignment needed.
                        }
                        Ok(clocks(3))
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
                                Ok(clocks(11))
                            }
                            1 => {
                                // SIDT m: store the IDTR pseudo-descriptor.
                                self.store_descriptor_table(bus, memory, self.idtr)?;
                                Ok(clocks(11))
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
                                Ok(clocks(11))
                            }
                            7 => {
                                // INVLPG m: privileged on the 486. Flush the whole TLB (a
                                // single-page invalidate is a permitted superset).
                                if self.current_privilege_level() != 0 {
                                    return Err(InternalFault::Exception {
                                        vector: 6,
                                        error_code: None,
                                    });
                                }
                                self.flush_tlb_and_code_caches();
                                Ok(clocks(12))
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
                    Some((_, high)) if self.descriptor_accessible(selector, high) => {
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
            0x0f06 => {
                // CLTS: clear the task-switched flag. Privileged.
                self.require_cpl0()?;
                self.control.cr0 &= !CR0_TS;
                Ok(clocks(2))
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
                self.require_cpl0()?;
                let modrm = insn.modrm.expect("MOV reg,CR decoded with a ModRM");
                if modrm.mode != 3 {
                    return Err(undefined_opcode());
                }
                // 386 PRM 12.2.4 / table 12-1: only CR0, CR2, CR3 (and, on this 586-class
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
                Ok(clocks(6))
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
                self.require_cpl0()?;
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
                        self.control.cr0 = value;
                        self.recompute_alignment_armed();
                        self.flush_tlb_and_code_caches();
                    }
                    2 => self.control.cr2 = value,
                    3 => {
                        // CR3 (the Page Directory Base Register) holds the page-directory
                        // physical base in bits 31:12 per 386 PRM 5.2.2. Bits 4:3 are PWT/PCD,
                        // a 486-and-later addition absent from the 386 PRM -- defined here per
                        // Pentium Vol. 3 S9 (register description) and S18.3 (PCD/PWT Bits),
                        // which this 586-class persona implements as cache-control hints, and
                        // bits 2:0 are reserved. The page-table base used by the walker keeps
                        // masking to `cr3 & 0xFFFFF000` at the use site -- only the stored
                        // value gains PWT/PCD so a guest that sets them can read them back.
                        self.control.cr3 = value & 0xffff_f018;
                        self.flush_tlb_and_code_caches();
                    }
                    4 => {
                        // 386 PRM has no CR4 (a 486/586 addition); on this GSW-586 (K6-class)
                        // persona only TSD (bit 2) has a modeled effect and only VME/PVI/TSD/
                        // DE/PSE/MCE/GPE (bits 0-4, 6-7) are architecturally defined at all
                        // per the AMD-K6 BIOS and Software Tools Developers Guide S: 3.7
                        // (Control Register 4 (CR4) Extensions, Figure 13/Table 19). The same
                        // guide's MOV-to/from-CR4 exception table, and the Pentium Vol. 3
                        // instruction reference, both document a hard fault ("#GP(0)" in
                        // protected mode, "Interrupt 13" in real mode) if a 1 is written to
                        // any reserved bit -- so a write outside CR4_DEFINED_MASK faults
                        // instead of silently dropping the bits, matching EFER/STAR above.
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
                Ok(clocks(6))
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
                Ok(clocks(6))
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
                Ok(clocks(6))
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
                Ok(clocks(10))
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
                let selector_offset = mem.offset.wrapping_add(operand_size.bytes());
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
                Ok(clocks(7))
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
                let selector_offset = mem.offset.wrapping_add(operand_size.bytes());
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
                Ok(clocks(7))
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
                let vector = insn.imm as u8;
                // In V86 a below-IOPL `INT n` faults to the monitor, but the emulator's HLE
                // BIOS/DOS services (INT 10h video, INT 13h disk, …) are driven from
                // `interrupt_acknowledge`, which the fault path would otherwise skip — so the
                // guest's console output would never render under a V86 monitor. Notify the bus
                // first, exactly as real-mode `software_interrupt` does, then raise the #GP.
                if self.is_v86_mode() && self.iopl() < 3 {
                    bus.interrupt_acknowledge(vector, self.read_gpr16(0))?;
                    self.check_v86_iopl()?;
                }
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
                // IRET is IOPL-sensitive in V86 (386 PRM): #GP(0) below IOPL 3, so the
                // V86 monitor's .iret_op performs the virtualized pop (VIF from the
                // image, real IF stays 1). Mirrors CLI/STI/PUSHF/POPF.
                self.check_v86_iopl()?;
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
                    _extension => Err(undefined_opcode()),
                }
            }
            opcode => unreachable!("control-flow opcode {opcode:#x}"),
        }
    }

    /// Raise the #UD for a single-byte opcode that the decode/execute split does not implement.
    /// After Stage A every IMPLEMENTED opcode is routed by `route_group` to a dedicated split group,
    /// so the only opcodes that reach here (via the `DecodeGroup::Fallback` arm of `execute_decoded`)
    /// are the genuinely-unimplemented ones — 0x63 (ARPL) and 0xF1 (ICEBP), plus any prefix byte that
    /// `read_prefixes` did not consume (which would be a decode bug). All produce the same
    /// `UnsupportedOpcode` the fused path produced: `opcode` is the byte, `cs` the current selector,
    /// and `eip` the instruction's start (the byte before any ModRM/immediate would sit), matching
    /// the legacy error fields exactly.
    fn unsupported_single_byte_opcode(&self) -> InternalFault {
        undefined_opcode()
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
    /// Ring-0 protected-mode code (`is_ring0_protected()`) is exempt too, for the same reason as
    /// the `read_prefixes` 66h/67h gate below: TOKAEMM's monitor is chipset-side, not guest
    /// software, and it is 32-bit-default code that uses MOVZX/MOVSX/BSF/etc freely. V86 tasks are
    /// always CPL 3 architecturally, so this can never leak into guest-facing V86 code, and it
    /// reads false in real mode (not protected). V86 and real-mode fidelity are unchanged.
    ///
    /// ASSUMPTION (same as `is_ring0_protected()`'s own doc): today ring-0 PM is ONLY the
    /// chipset-side TOKAEMM monitor. A guest OS running its own ring-0 protected mode on a
    /// throttled persona (OS/2 1.x or Windows standard mode on the 286 persona), or a future
    /// VCPI client (which runs ring-0 PM by design), would get the full core ISA here -- including
    /// the 586-only additions this same short-circuit skips on the 386/486 personas -- where real
    /// hardware would #UD. Correct-by-design for the monitor (same precedent as the blanket
    /// firmware-ROM exemption); revisit when VCPI/DPMI lands, likely by scoping the exemption to
    /// monitor identity (e.g. a CS-range check like `cs_in_firmware_rom`) instead of privilege.
    ///
    /// `decode` applies this once, right after reading the second 0F byte — the same logical point
    /// (and eip) the fused path faulted at — so both the converted split path and the un-converted
    /// fused fallback share a single gate.
    /// Returns whether the firmware-ROM / ring-0 exemption was DECISIVE (the opcode would have
    /// #UD'd at this level without it). Such a decode must not enter the decode cache: the
    /// exemption is a property of the executing context (CS region, privilege), not of the
    /// bytes, and a cached replay after a privilege change would skip the #UD.
    fn check_two_byte_isa_gate(&self, second: u8) -> ExecResult<bool> {
        let gated = (self.level.is_pre_386() && is_386plus_two_byte(second))
            || (!self.level.has_pentium_isa() && is_586plus_two_byte(second));
        if !gated {
            return Ok(false);
        }
        if self.cs_in_firmware_rom() || self.is_ring0_protected() {
            return Ok(true);
        }
        Err(InternalFault::Exception {
            vector: 6,
            error_code: None,
        })
    }

    /// Execute a two-byte (0F) opcode that has no dedicated split group. `opcode` is the second
    /// opcode byte that `decode` already read + charged and gated; this never re-reads it. Reached
    /// two ways: the `TwoByteFallback` arm of `execute_decoded` (which #UDs the unimplemented bytes),
    /// and as a leaf call from `execute_misc_decoded` for the no-operand 0F members. The converted 0F
    /// groups (MOVZX/MOVSX and the rest) bypass this entirely via `route_group`/`execute_decoded`.
    ///
    /// Most opcodes handled here re-read no further instruction bytes, so the heterogeneous
    /// `Misc` group (task A14) also leaf-calls this for its 0F members
    /// (SYSCALL/SYSRET/INVD/WBINVD/WRMSR/RDTSC/RDMSR/CPUID/BSWAP) rather than duplicating them.
    /// PUSH/POP FS/GS (0F A0/A1/A8/A9) are Misc members too: like their one-byte ES/SS/DS
    /// counterparts in `execute_stack_decoded`, they touch the stack, so `bus` is threaded
    /// through. The genuinely unimplemented 0F bytes still fall through to the
    /// `UnsupportedTwoByteOpcode` arm and #UD.
    fn execute_two_byte<B: CpuBus>(
        &mut self,
        bus: &mut B,
        opcode: u8,
        operand_size: OperandSize,
    ) -> ExecResult<CycleOutcome> {
        match opcode {
            // The MMX block (`is_mmx_two_byte`) is converted to the decode/execute split (task A14):
            // `route_group` classifies it as `DecodeGroup::Misc` and `execute_mmx_decoded` runs it
            // (the ModRM + the 0F 71/72/73 imm8 are parsed in `decode`). Not handled here.
            // Limit: MMX is not gated to 586+; a throttled 386/486 GSW mode would wrongly accept it.
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
            // 0F 20 (MOV reg,CR) and 0F 22 (MOV CR,reg) are converted to the decode/execute split
            // (task A12): `route_group` classifies them as `DecodeGroup::SystemSeg` and
            // `execute_system_seg_decoded` runs them (the register-form ModRM is parsed in `decode`;
            // the whole-32-bit CR read/write, the `mode != 3` #UD, and the CR0/CR3 TLB flush via the
            // unchanged `flush_tlb_and_code_caches` stay in the executor). Not handled here. 0F 21/23
            // (MOV reg,DR / MOV DR,reg) remain unimplemented and #UD as `UnsupportedTwoByteOpcode`.
            // CMOVcc (0x40-0x4F), SETcc (0x90-0x9F), and IMUL reg,r/m (0xAF) are converted to
            // the decode/execute split (task A11): `route_group` classifies them as
            // `DecodeGroup::CondMove` and `execute_condmove_decoded` runs them. Not handled here.
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
            // CMPXCHG (0xb0/0xb1) and XADD (0xc0/0xc1) are converted to the decode/execute split
            // (task A10): `route_group` classifies them as `DecodeGroup::BitManip` and
            // `execute_bitmanip_decoded` runs them. Not handled here.
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
                // the 0F group but still has no CPUID.) Firmware in the BIOS ROM is exempt,
                // and so is ring-0 protected mode -- the same chipset-side-monitor exemption
                // (and the same ASSUMPTION/revisit trigger) as the two ISA gates in
                // read_prefixes and check_two_byte_isa_gate; without it, future ring-0
                // monitor code executing CPUID on a sub-486 persona would die by the same
                // unpopulated-low-vector cascade the gate exemptions fixed.
                if !self.level.has_cpuid()
                    && !self.cs_in_firmware_rom()
                    && !self.is_ring0_protected()
                {
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
                Ok(clocks(1))
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
                Ok(clocks(2))
            }
            0xa8 => {
                self.push(
                    bus,
                    u32::from(self.registers.segment(SegmentIndex::Gs).selector),
                    operand_size,
                )?;
                Ok(clocks(2))
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
                Ok(clocks(7))
            }
            0xa9 => {
                let value = self.pop(bus, operand_size)? as u16;
                self.load_segment(bus, SegmentIndex::Gs, value)?;
                Ok(clocks(7))
            }
            _ => Err(undefined_opcode()),
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
                //
                // Ring-0 protected-mode code (`is_ring0_protected()`: PE set, CPL 0,
                // not V86) is exempt too, parallel to the firmware exemption. That
                // state is TOKAEMM's own monitor -- chipset-side code that runs
                // underneath the guest, not guest software -- so it is never subject
                // to the guest-facing ISA level the player selected. The level gate
                // exists to make the emulated machine LOOK like a 286 to the guest;
                // it must bind guest-facing execution only. V86 tasks are
                // architecturally always CPL 3, so `is_ring0_protected()` (which
                // requires !is_v86_mode()) can never accidentally exempt them: V86
                // guest code stays gated exactly as before. Real mode is likewise
                // unaffected (`is_protected_mode()` is false there, so
                // `is_ring0_protected()` is false too). Without this, a 286-mode
                // session with TOKAEMM resident dies the instant the monitor's
                // 32-bit-default entry code (e.g. `vec13_entry`'s `66 B8 .. / mov
                // ds, ax`) runs, and the resulting #UD cascades into a worse fault
                // because TOKAEMM's IDT does not populate the low exception vectors.
                //
                // ASSUMPTION (same as `is_ring0_protected()`'s own doc): today
                // ring-0 PM is ONLY the chipset-side TOKAEMM monitor. A guest OS
                // running its own ring-0 protected mode on the 286 persona (OS/2
                // 1.x, Windows standard mode), or a future VCPI client (ring-0 PM
                // by design), would get 386+ ISA here where a real 286 #UDs.
                // Correct-by-design for the monitor; revisit when VCPI/DPMI lands,
                // likely by scoping this to monitor identity instead of privilege.
                // See check_two_byte_isa_gate's doc for the full statement.
                0x66 | 0x67
                    if self.level.is_pre_386()
                        && !self.cs_in_firmware_rom()
                        && !self.is_ring0_protected() =>
                {
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
            // 386 PRM 9.9.13: exceeding the CS limit on an instruction fetch is an
            // ordinary #GP(0), not a host-fatal error. This must reach `finish_instruction`
            // as `InternalFault::Exception` (rewind + `deliver_exception`, which already
            // reflects faults into a V86 monitor) rather than `InternalFault::Cpu`, whose
            // `SegmentLimit` variant propagates straight out of `cycle` and halts the machine.
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(0),
            });
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
        self.perf.slow_prefetch_refills += 1;
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
        // Observation seam: the machine recognizes its BIOS INT stub landings
        // by the LINEAR fetch address (a paging guest may shadow the stub
        // page, so the physical address cannot identify it).
        bus.note_code_fetch_linear(linear);
        if let Some((value, physical)) = self.fetch_page.get(cs, linear) {
            self.perf.fetch_page_hits += 1;
            bus.charge_instruction_fetch(physical)?;
            self.registers.eip = self.registers.eip.wrapping_add(1);
            return Ok(value);
        }
        self.perf.fetch_page_misses += 1;
        if let Some((value, physical)) = self.prefetch.get(cs, linear) {
            bus.charge_instruction_fetch(physical)?;
            self.registers.eip = self.registers.eip.wrapping_add(1);
            return Ok(value);
        }
        let physical = self.translate_code_linear(bus, linear)?;
        if let Some(page) = bus.direct_page(physical, BusAccessKind::InstructionPrefetch)? {
            self.perf.direct_page_hits += 1;
            self.fetch_page.put(cs, linear, page);
            let (value, physical) = self
                .fetch_page
                .get(cs, linear)
                .expect("fetch page refilled");
            bus.charge_instruction_fetch(physical)?;
            self.registers.eip = self.registers.eip.wrapping_add(1);
            return Ok(value);
        }
        self.perf.direct_page_misses += 1;
        self.refill_prefetch(bus, offset, linear)?;
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
    #[inline]
    fn resolve_memory_addr_mode(&self, addr: &AddrMode) -> MemoryOperand {
        let disp = addr.disp as u32;
        let offset = match addr.address_size {
            AddressSize::Word => {
                let base = match addr.base {
                    Some(reg) => u32::from(self.read_gpr16(reg)),
                    None => 0,
                };
                let index = match addr.index {
                    Some(reg) => u32::from(self.read_gpr16(reg)),
                    None => 0,
                };
                let sum = base.wrapping_add(index).wrapping_add(disp);
                (sum as u16) as u32
            }
            AddressSize::Dword => {
                let base = match addr.base {
                    Some(reg) => self.read_gpr32(reg),
                    None => 0,
                };
                let index = match addr.index {
                    Some(reg) => {
                        let value = self.read_gpr32(reg);
                        if addr.scale == 1 {
                            value
                        } else {
                            value.wrapping_mul(u32::from(addr.scale))
                        }
                    }
                    None => 0,
                };
                base.wrapping_add(index).wrapping_add(disp)
            }
        };
        MemoryOperand {
            segment: addr.segment,
            offset,
        }
    }

    #[inline]
    fn resolve_addr_mode(&self, addr: &AddrMode) -> RmOperand {
        RmOperand::Memory(self.resolve_memory_addr_mode(addr))
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

/// Per-level instruction-clock scaling as (numerator, denominator), CALIBRATED
/// (B-T10) against the Neurketa compute benchmarks. A retired op's base clocks are
/// scaled by num/den (with a fractional remainder carry in `scale_clocks`), so
/// num/den < 1 runs the mode faster.
///
/// This is the COMPUTE dial. Since B-T10 a second per-mode dial (`bus_timing`)
/// scales the whole bus portion (fetch + data access), so every guest clock is
/// `scale_clocks(instruction) + scale_bus(bus)`. The bus dial carries the absolute
/// per-mode magnitude (it lets a fast part pull away from the old flat per-access
/// floor), so this dial only trims the compute share. Dhrystone (the PRIMARY
/// oracle) is a fetch+data mix split roughly compute/bus; these values plus
/// `bus_timing` seat all four modes' Dhrystones/sec on the owner's authoritative
/// era targets (286 ~3500, 386 ~9200, 486 ~61000, 586 ~475000) to within ~0.3%.
///
/// fp-mandel TRADE-OFF: fp-mandel is x87-compute-bound (~7280 instruction clocks
/// vs ~6247 bus per pixel), so it rides this dial. Dhrystone pinned to its owner
/// target forces the compute dial small on the fast modes, which makes fp-mandel
/// run well above its ratio-anchored band and at a 586/486 ratio of ~8x (the model
/// floor with Dhrystone pinned is ~7.8x; see bench_reference.rs). Matching both the
/// fp-mandel ratio AND the Dhrystone target needs a separate x87 latency dial (a
/// deferred Whetstone-payload follow-up); Dhrystone is PRIMARY, so fp-mandel's band
/// is recentered on the achieved value and the ratio gap recorded.
const fn level_timing(level: CpuLevel) -> (u32, u32) {
    match level {
        CpuLevel::I286 => (3, 5),
        CpuLevel::I386 => (2, 5),
        CpuLevel::I486 => (1, 12),
        CpuLevel::I586 => (1, 12),
    }
}

/// x87 op classes for the per-class FP-timing dial. The class is derived at the
/// FPU dispatch tail from (escape opcode, ModRM), so the classifier sees exactly
/// what the census profiler's opcode rows see:
/// - `IntConvert`: the int<->fp boundary — every DB/DF/DA MEMORY form (FILD,
///   FIST(P), FBLD/FBSTP, integer-operand arithmetic). On a real P55C, FIST is
///   unpairable and drains the FP pipe, exposing the full latency of the chain
///   that produced the value; a sequential interpreter charges issue cost only,
///   so this class carries an effective stall surcharge (the Quake span
///   rasterizer's fixed-point boundary is exactly this traffic).
/// - `F32Mem` / `F64Mem`: D8/D9 and DC/DD memory forms (f32/f64 load, store,
///   memory-operand arithmetic).
/// - `Register`: every mode==3 form — register arithmetic, FXCH, compares,
///   constants, transcendentals, control ops. P5 pairing and the 1-clock
///   FADD/FMUL issue rate make this class CHEAPER than the raw 387 clocks.
/// - `Wait`: the 9B WAIT opcode (Whetstone-era code is full of FWAITs; real P5
///   treats them as ~free once no exception is pending).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FpOpClass {
    /// DB memory forms: 32-bit FILD/FIST(P) (+f80 loads/stores) - the class the
    /// Quake span rasterizer's fixed-point boundary lives in.
    IntConvert32,
    /// DF/DA/DE memory forms: the era-compiler conversion families - DF int16
    /// loads/stores (+m64 FILD/FISTP under /5 //7, +BCD), DE int16 arith, DA
    /// int32 arith. Grouped as one calibration knob; the width in the name
    /// records the dominant DF-int16 shape, not every member's encoding.
    IntConvert16,
    F32Mem,
    F64Mem,
    Register,
    Wait,
}

/// Common denominator for every `fp_timing_class` ratio: sharing one denominator
/// keeps the single carried remainder (`fp_rem`) exact across ops of different
/// classes (a per-op numerator over a fixed base, tunable in 1/8 steps).
const FP_TIMING_DEN: u32 = 8;

/// Per-mode, per-class FP-op-clock numerator over `FP_TIMING_DEN` — a SEPARATE
/// dial from `level_timing` so the P55C's x87 pipeline can be modeled at I586
/// without touching the integer-instruction compute ratio. Identity (8/8) for
/// 286/386/486: their FP rides `level_timing` alone, keeping the frozen-class
/// bench bytes and the 486 Whetstone anchor (6.5 MFLOPS) untouched.
///
/// The I586 values replace the old flat (31/34) scalar with the class shape the
/// workload census demanded (dev_docs quake-fps-cal notes): Quake demo1's FP
/// clocks are conversion/traffic-shaped (FILD/FIST + f32/f64 memory ops) while
/// Whetstone's are register-arithmetic/transcendental-shaped, and the era
/// anchors pull those classes in OPPOSITE directions (real P55C-200: Quake
/// ~42 fps, Whetstone 34.5 MFLOPS). Register-class ops get CHEAPER than raw 387
/// clocks (U/V pairing, 1-clock FADD/FMUL issue); the int<->fp boundary pays an
/// effective stall surcharge (see FpOpClass::IntConvert). CALIBRATION
/// CONSTRAINTS: Whetstone 586 = 34.5 MFLOPS and 486 = 6.5 stay era-exact;
/// Dhrystone/Sieve run no x87 and stay bit-identical; 286/386 frozen.
const fn fp_timing_class(level: CpuLevel, class: FpOpClass) -> u32 {
    match level {
        CpuLevel::I286 | CpuLevel::I386 | CpuLevel::I486 => FP_TIMING_DEN,
        CpuLevel::I586 => match class {
            // Census-guided, empirically walked against the two era anchors
            // (Whetstone 586 = 34.5 MFLOPS, Quake demo1 ~42 fps).
            FpOpClass::IntConvert32 => 272, // x34: conversion drains the FP pipe
            FpOpClass::IntConvert16 => 256, // x32: FIST16 drains the pipe too, a touch less
            FpOpClass::F32Mem => 8,         // x1
            FpOpClass::F64Mem => 8,         // x1
            FpOpClass::Register => 2,       // x0.25: pairing/issue-rate honesty
            FpOpClass::Wait => 1,           // x0.125: FWAIT is ~free on a P5
        },
    }
}

/// Per-level BUS-clock scaling as (numerator, denominator), CALIBRATED (B-T10).
/// This is the THIRD timing lever (after `level_timing` and the cache `tier_cost`
/// wait-states): it scales the ENTIRE bus portion of a step (instruction fetch +
/// every tiered data access) by num/den, with a fractional-remainder carry in the
/// machine's `scale_bus` so a cheap access is not rounded to zero.
///
/// Why it exists: the bus portion is `2 + wait_states` clocks per access and the
/// `2` base is mode-INDEPENDENT, so before this dial a fast part could not pull
/// away from a flat per-access floor and 486/586 Dhrystone/Sieve floored far below
/// their era absolutes. Scaling the whole bus portion per mode supplies the
/// absolute magnitude the fast modes need (num/den < 1 makes the bus effectively
/// faster, lifting iters/sec), while the relative L1<L2<RAM structure stays in the
/// `tier_cost` wait-states. The slow modes keep num/den ~ 1 (their flat-floor bus
/// was already near band); the fast modes use a smaller ratio to reach their
/// targets (486 ~0.33, 586 ~0.18). These values, with `level_timing`, seat all four
/// Dhrystone modes on the owner's authoritative targets (the PRIMARY oracle).
///
/// BANDWIDTH coupling (see bench_reference.rs): the bandwidth tool now reports the
/// SCALED bus delta, so a tier's MB/s is `4 * clock_hz / ((2 + ws) * (num/den)) /
/// 1e6`. A smaller num/den (needed for fast-mode Dhrystone) multiplies bandwidth UP
/// by den/num, so the fast-mode L1 bandwidth lands ABOVE the SpeedSys era figures
/// and cannot be pulled back without re-slowing Dhrystone (Dhrystone is ~30% L1
/// data, so the L1 wait-state is shared). The L2/RAM tiers are decoupled (the
/// benchmarks fit L1/L2 and never miss), so their large `tier_cost` miss penalties
/// pull those tiers down to SpeedSys on the 486; on the 586 the u8 wait-state cap
/// (255) over a 16-dword line floors L2/RAM above SpeedSys. Era anchors and the gap
/// are recorded in each bandwidth `cite`.
pub const fn bus_timing(level: CpuLevel) -> (u32, u32) {
    match level {
        CpuLevel::I286 => (6, 11),
        CpuLevel::I386 => (23, 31),
        CpuLevel::I486 => (1, 3),
        CpuLevel::I586 => (7, 30),
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
        // PUSH/POP FS/GS (386): FS and GS do not exist before the 386.
        | 0xa0 | 0xa1 | 0xa8 | 0xa9
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

/// Round per the control word's RC field (bits 10-11): 00 nearest-even, 01 toward
/// negative infinity, 10 toward positive infinity, 11 chop. DJGPP-compiled code
/// (Quake among it) flips RC to chop around every C `(int)` cast, so FIST/FISTP/
/// FRNDINT/FBSTP must honor it rather than always rounding to nearest.
fn fpu_round_rc(control: u16, value: f64) -> f64 {
    match (control >> 10) & 3 {
        0 => value.round_ties_even(),
        1 => value.floor(),
        2 => value.ceil(),
        _ => value.trunc(),
    }
}

impl CpuGsw {
    /// The heterogeneous one-off block (task A14) through the decode/execute split. Each arm mirrors
    /// the former fused handler verbatim — same flag effects, same memory access, same clocks — but
    /// consumes the ModRM/operand/immediate `decode` pre-parsed (so the executor never re-fetches an
    /// instruction byte). The MMX block and CMPXCHG8B resolve their pre-decoded ModRM here; the
    /// no-operand 0F system/serializing/CPU-id ops (SYSCALL/SYSRET/INVD/WBINVD/WRMSR/RDTSC/RDMSR/
    /// CPUID/BSWAP) read no instruction bytes, so they reuse the existing `execute_two_byte` leaf
    /// logic verbatim (it re-reads nothing for them). Dispatch is off the FULL u16 `insn.opcode`
    /// so a 0F low byte can never alias a single-byte opcode.
    fn execute_misc_decoded<B: CpuBus>(
        &mut self,
        insn: &DecodedInsn,
        bus: &mut B,
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
            0x69 => {
                // IMUL r, r/m, imm16/32: signed multiply of r/m by a full-width immediate.
                // `decode` parsed the ModRM/operand and fetched the immediate into `insn.imm`.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let src = self.read_operand_sized(bus, operand, operand_size)?;
                let result = self.imul_truncated(src, insn.imm, operand_size);
                self.write_gpr_sized(modrm.reg, operand_size, result);
                Ok(clocks(14))
            }
            0x6b => {
                // IMUL r, r/m, imm8: signed multiply of r/m by a sign-extended byte immediate.
                // `decode` parsed the ModRM/operand and sign-extended the imm8 into `insn.imm`.
                let (modrm, operand) = self.resolve_decoded_modrm_operand(insn);
                let src = self.read_operand_sized(bus, operand, operand_size)?;
                let result = self.imul_truncated(src, insn.imm, operand_size);
                self.write_gpr_sized(modrm.reg, operand_size, result);
                Ok(clocks(14))
            }
            0x6c => {
                self.run_string(
                    bus,
                    StringOp::Ins,
                    BusWidth::Byte,
                    insn.prefixes,
                    address_size,
                )?;
                Ok(clocks(15))
            }
            0x6d => {
                self.run_string(
                    bus,
                    StringOp::Ins,
                    operand_size.bus_width(),
                    insn.prefixes,
                    address_size,
                )?;
                Ok(clocks(15))
            }
            0x6e => {
                self.run_string(
                    bus,
                    StringOp::Outs,
                    BusWidth::Byte,
                    insn.prefixes,
                    address_size,
                )?;
                Ok(clocks(14))
            }
            0x6f => {
                self.run_string(
                    bus,
                    StringOp::Outs,
                    operand_size.bus_width(),
                    insn.prefixes,
                    address_size,
                )?;
                Ok(clocks(14))
            }
            0xa8 => {
                // TEST AL, imm8: AND-for-flags, no write-back. `decode` fetched the imm8.
                let al = self.read_gpr8(0);
                self.alu(4, u32::from(al), insn.imm, BusWidth::Byte);
                Ok(clocks(2))
            }
            0xa9 => {
                // TEST AX/EAX, imm: AND-for-flags, no write-back. `decode` fetched the immediate.
                let acc = self.read_gpr_sized(0, operand_size);
                self.alu(4, acc, insn.imm, operand_size.bus_width());
                Ok(clocks(2))
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
                Ok(clocks(17))
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
                Ok(clocks(19))
            }
            0xd6 => {
                // SALC/SETALC (undocumented): AL = CF ? 0xFF : 0x00. Flags unaffected.
                let value = if self.flag(FLAG_CF) { 0xff } else { 0x00 };
                self.write_gpr8(0, value);
                Ok(clocks(2))
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
                Ok(clocks(5))
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
                Ok(clocks(10))
            }
            // The MMX block (EMMS, the shift-by-imm forms, MOVD/MOVQ, and the Pxxx forms) runs
            // through its split executor, consuming the pre-decoded ModRM/operand and the pre-fetched
            // imm8 (for the 0F 71/72/73 shifts).
            op if op & 0xff00 == 0x0f00 && is_mmx_two_byte(op as u8) => {
                self.execute_mmx_decoded(insn, bus)
            }
            // The remaining 0F system/serializing/CPU-id/stack ops re-read no further instruction
            // bytes in `execute_two_byte`, so reuse that leaf logic verbatim: SYSCALL (05), SYSRET
            // (07), INVD/WBINVD (08/09), WRMSR/RDTSC/RDMSR (30/31/32), PUSH FS/GS (A0/A8), CPUID
            // (A2), POP FS/GS (A1/A9), BSWAP (C8-CF). `decode` already read + gated the second
            // byte; this never re-reads it.
            0x0f05
            | 0x0f07
            | 0x0f08
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

#[cfg(test)]
#[path = "cpu_test.rs"]
mod tests;
