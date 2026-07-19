// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_bus::{BusAccessKind, BusError, BusWidth, CpuBus};
pub use izarravm_core::{CpuPersona, GswMode};
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use thiserror::Error;

mod control;
#[path = "core.rs"]
mod cpu_core;
mod decode;
mod execute;
mod execute_extended;
mod flags;
mod fpu;
mod fpu_exec;
#[cfg(feature = "jit")]
mod jit;
/// The unit simulator's headline report, returned by `CpuGsw::take_unit_sim_report` (a diagnostic
/// measurement aid; see `jit::unit_sim`).
#[cfg(feature = "jit")]
pub use jit::unit_sim::SimReport;
mod memory;
mod mmx;
mod mmx_exec;
mod paging;
mod run;
mod strings;
pub use fpu::X87;

/// Whether this build contains the native x64 execution backend.
///
/// Windows and Linux x86-64 are the release targets for native execution.
/// Other targets and interpreter-only builds keep using the portable core.
pub const NATIVE_BACKEND_COMPILED: bool = cfg!(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
));

/// Whether the host can execute the native backend in this build.
///
/// The x64 translator is allowed to emit AVX2 unconditionally. Callers which
/// admit native blocks must check this once before enabling the backend.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub fn native_backend_available() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}

#[cfg(not(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
pub fn native_backend_available() -> bool {
    false
}

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
// pages, AM enables #AC, and NE selects native x87 exception delivery. NW and CD are
// inert storage because cache disabling has no modeled effect.
// CR0 bit 4 is ET (extension type). The existing reset image leaves it clear. The
// active CPU persona decides whether an x87 unit is present.
const CR0_WP: u32 = 0x0001_0000; // bit 16
const CR0_AM: u32 = 0x0004_0000; // bit 18
#[allow(dead_code)]
const CR0_NE: u32 = 0x0000_0020; // bit 5
#[allow(dead_code)]
const CR0_NW: u32 = 0x2000_0000; // bit 29
#[allow(dead_code)]
const CR0_CD: u32 = 0x4000_0000; // bit 30

// Pentium MMX P55C CPUID identity. Leaf 0 returns the maximum basic leaf in EAX and the
// 12-byte Intel vendor string split across EBX, EDX, ECX in the architectural order.
const CPUID_MAX_BASIC_LEAF: u32 = 1;
const CPUID_VENDOR_EBX: u32 = u32::from_le_bytes(*b"Genu");
const CPUID_VENDOR_EDX: u32 = u32::from_le_bytes(*b"ineI");
const CPUID_VENDOR_ECX: u32 = u32::from_le_bytes(*b"ntel");

// Leaf 1 EAX packs type (bits 13-12), family (bits 11-8), model (bits 7-4) and stepping
// (bits 3-0). Family 5, model 4 is the Pentium MMX P55C identity. Stepping 3 is one of
// the production P55C revisions.
const CPUID_TYPE: u32 = 0; // original OEM part
const CPUID_FAMILY: u32 = 5;
const CPUID_MODEL: u32 = 4;
const CPUID_STEPPING: u32 = 3;
const CPUID_VERSION_EAX: u32 =
    (CPUID_TYPE << 12) | (CPUID_FAMILY << 8) | (CPUID_MODEL << 4) | CPUID_STEPPING;

// Leaf 1 reports only behavior modeled by this core.
const CPUID_FEATURE_FPU: u32 = 1 << 0;
const CPUID_FEATURE_TSC: u32 = 1 << 4;
const CPUID_FEATURE_MSR: u32 = 1 << 5;
const CPUID_FEATURE_CX8: u32 = 1 << 8; // CMPXCHG8B
const CPUID_FEATURE_MMX: u32 = 1 << 23;
const CPUID_FEATURES_EDX: u32 = CPUID_FEATURE_FPU
    | CPUID_FEATURE_TSC
    | CPUID_FEATURE_MSR
    | CPUID_FEATURE_CX8
    | CPUID_FEATURE_MMX;

// CR4 bits with a modeled effect. TSD (bit 2) makes RDTSC privileged: when set, RDTSC
// outside CPL 0 raises #GP(0). The other CR4 bits are storage only.
const CR4_TSD: u32 = 0x0000_0004;

// P55C defines VME, PVI, TSD, DE, PSE and MCE. Only TSD has modeled behavior; the
// remaining defined bits are inert storage. PGE is a P6 addition and stays reserved.
const CR4_DEFINED_MASK: u32 = 0x0000_005f; // bits 0-4, 6

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

// Pentium model-specific register addresses used by the modeled P55C subset.
const MSR_MCAR: u32 = 0x0000_0000; // machine-check address
const MSR_MCTR: u32 = 0x0000_0001; // machine-check type
const MSR_TSC: u32 = 0x0000_0010; // time-stamp counter

// Leaf 1 EBX: brand index 0 (no brand string), CLFLUSH line size and other fields stay 0.
const CPUID_LEAF1_EBX: u32 = 0;
// Leaf 1 ECX: no extended feature is claimed.
const CPUID_LEAF1_ECX: u32 = 0;

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

    /// A 4-GByte flat 32-bit segment (base 0, full limit).
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

    /// GPR by ModRM index (0=eax..7=edi), for the memory-poll shape's comparand
    /// register (the CMP instruction's ModRM reg field names any GPR).
    #[cfg(feature = "jit")]
    #[cold]
    #[inline(never)]
    pub(crate) fn gpr32(&self, index: u8) -> u32 {
        self.gpr[usize::from(index & 7)]
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
// 386 reset forced on; it stays 0 because x87 presence comes from the active CPU
// persona rather than this stored reset image.
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

/// The modeled Pentium MSR subset. MCAR and MCTR are plain storage because machine-check
/// delivery is not modeled. `tsc_offset` lets a WRMSR rebase the timeline-driven TSC.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Msrs {
    pub mcar: u64,
    pub mctr: u64,
    pub tsc_offset: u64,
}

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
    /// Code-cache invalidation events, including narrow self-modifying-code kills. This is the
    /// aggregate rate; the `decode_inval_*` and `smc_narrow_kills` counters retain the cause and
    /// affected-line detail.
    pub code_invalidations: u64,
    /// Lines killed by the NARROW SMC path (a self-patch whose covering lines were
    /// invalidated individually, no whole-cache flush). decode_inval_smc keeps counting the
    /// global-flush fallbacks only, so the two together split the SMC write traffic.
    pub smc_narrow_kills: u64,
    /// G1 SMC heat demotions: compiled-block admissions refused because the entry chunk (pre-
    /// compile gate) or some chunk under the block span (pre-install gate) crossed the churn
    /// threshold this heat epoch. The block is parked Dormant and the region runs on the
    /// interpreter until an epoch with no fresh invalidation lets it re-admit.
    pub smc_heat_demotions: u64,
    /// Distinct 16-byte code chunks that crossed the heat threshold (one per chunk per epoch). A
    /// diagnostic of how much genuine self-modifying churn the guest generates; near zero on the
    /// periodic-repatch anchors.
    pub smc_heat_chunks_hot: u64,
    /// Device and HLE writes reported with an exact physical range. These writes use the same
    /// narrow code invalidation path as CPU stores instead of clearing the whole code cache.
    pub device_write_ranges: u64,
    pub device_write_bytes: u64,
    /// Exact device-write ranges that overlapped compiled code, decoded code, or prefetched
    /// instruction bytes and therefore invalidated at least one piece of CPU execution state.
    pub device_write_code_hits: u64,
    /// Device writes for which the machine could not provide a physical range. These retain the
    /// conservative whole-cache reset and should stay near zero in normal game workloads.
    pub device_write_coarse_resets: u64,
    /// Compiled-region executions (one per `run_region` call that passed its entry
    /// preconditions) and the instructions those executions retired. `jit_region_insns /
    /// jit_region_entries` is the mean instructions per region entry; a Doom A/B run asserts
    /// `jit_region_entries > 0` to prove the region actually executed. Always present (zero
    /// without the `jit` feature) so perf-row consumers need no feature gymnastics.
    pub jit_region_entries: u64,
    pub jit_region_insns: u64,
    /// Direct x64 block executions, retired guest instructions, and prefix side exits. These do
    /// not include the legacy region engine, so acceptance reports can measure direct coverage
    /// and exits per 100 instructions without mixing the two execution models.
    pub jit_direct_entries: u64,
    pub jit_direct_insns: u64,
    pub jit_direct_side_exits: u64,
    pub jit_direct_exit_cross_page_or_alignment: u64,
    pub jit_direct_exit_unavailable_or_kind: u64,
    pub jit_direct_exit_permission: u64,
    pub jit_direct_exit_code_watch: u64,
    pub jit_direct_exit_other: u64,
    pub jit_direct_compile_attempts: u64,
    pub jit_direct_blocks_installed: u64,
    pub jit_direct_compile_ns: u64,
    pub jit_direct_hot_hits: u64,
    pub jit_direct_hash_hits: u64,
    pub jit_direct_lookup_misses: u64,
    pub jit_direct_linked_transfers: u64,
    /// Cold chain returns. The four reason counters below partition this total exactly.
    pub jit_direct_unresolved_exits: u64,
    pub jit_direct_unresolved_static_unbound: u64,
    pub jit_direct_unresolved_static_hidden: u64,
    pub jit_direct_unresolved_dynamic_miss_or_unbound: u64,
    pub jit_direct_unresolved_dynamic_hidden: u64,
    pub jit_direct_deferred_short: u64,
    pub jit_direct_reject_observer: u64,
    pub jit_direct_reject_interrupt_shadow: u64,
    pub jit_direct_reject_aggregate_accounting: u64,
    /// Machine-level VGA poll spans and complete guest loop iterations elided while
    /// `IZARRAVM_POLL_SKIP` is enabled. They are diagnostics, not retired instructions.
    pub poll_skip_spans: u64,
    pub poll_skip_iterations: u64,
    /// Poll negative cache: classify boundaries answered without a scan.
    pub poll_neg_cache_hits: u64,
    /// Structural negatives recorded into the cache (each was a full scan
    /// with the cache enabled).
    pub poll_neg_cache_stores: u64,
    /// Volatile negatives (scanned, uncacheable: register/segment reasons).
    /// Counts every such scan regardless of `poll_neg_cache_enabled`: volatile
    /// outcomes are never cached, so they re-fire with the switch off. A nonzero
    /// value with the cache disabled is expected, not a leak.
    pub poll_neg_cache_volatile: u64,
    /// Classify boundaries rejected by the loop-head prefilter (cold line,
    /// 16-bit code, or an opcode no certified shape slot can carry) before
    /// any scan or cache probe. Counts regardless of the cache switch.
    pub poll_head_prefilter_rejects: u64,
    pub jit_direct_reject_mode_key: u64,
    pub jit_direct_reject_x87_top: u64,
    pub jit_direct_reject_cs_layout: u64,
    pub jit_direct_reject_cpl: u64,
    pub jit_direct_reject_data_segment: u64,
    pub jit_direct_reject_alignment: u64,
    pub jit_direct_reject_fetch_limit: u64,
    pub jit_direct_reject_zero_budget: u64,
    pub jit_direct_cache_resets: u64,
    pub jit_direct_arena_compactions: u64,
    pub jit_direct_arena_compaction_live_blocks: u64,
    pub jit_direct_arena_compaction_bytes: u64,
    pub jit_direct_arena_compaction_failures: u64,
    pub jit_direct_links_created: u64,
    pub jit_direct_links_cleared: u64,
    /// Reverse-index dependency IDs examined after a direct-mapped decode slot was displaced.
    pub jit_direct_decode_dependencies_scanned: u64,
    /// Compiled portals hidden because one of their live decode lines was displaced.
    pub jit_direct_portals_hidden: u64,
    /// Guest instructions completed by emitted native operations, excluding instructions run by
    /// `region_step`. Compare with `jit_region_insns` to measure native opcode coverage.
    pub jit_native_insns: u64,
    /// Transitions from emitted code into `region_step`, including a transition that reports a
    /// fault. Inline bookkeeping and flag helpers are not counted.
    pub jit_helper_exits: u64,
    /// Calls from emitted byte-memory probes into the exact Rust memory helper.
    pub jit_native_memory_helpers: u64,
    /// Wall time for sampled compiled-region calls and the number of samples. The first entry and
    /// every 1,024th entry are sampled to keep timing overhead out of the hot path.
    pub jit_native_block_ns: u64,
    pub jit_native_block_samples: u64,
    /// Times the compiled-region table hit its capacity and was dropped wholesale (a coarse GC;
    /// see `JIT_REGION_TABLE_CAP`). Nonzero means the working set of hot loops exceeded the cap and
    /// the JIT is re-warming - a signal to raise the cap or add per-entry eviction. Zero on the
    /// single-phase anchors. Always present (zero without the `jit` feature).
    pub jit_table_clears: u64,
    /// Byte loads completed through the native address probe and exact direct-page helper.
    pub jit_native_load_hits: u64,
    /// Byte stores completed through the native address probe and exact direct-page helper.
    pub jit_native_store_hits: u64,
    /// Successful emitted TLB translations before the physical direct-page cache probe.
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
    /// Entries from V86 into the ring-0 monitor through vector 13. Sensitive-
    /// instruction #GP faults and real IRQ5 share this vector. Dividing
    /// `brk_step` by this count gives the mean batch-ending port accesses per
    /// monitor trip.
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

/// One of the closed, page-local, warm 3DA polling-loop shapes certified by the
/// JIT block builder.
///
/// Fields stay private so machine code can only consume the classifier's certified
/// shape through the accessors below.
#[cfg(feature = "jit")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollLoop {
    fetches: [(u32, u32, u8); 6],
    fetch_count: u8,
    // Meaningful only for the Io family (PollFamily::Io / `memory.is_none()`); a
    // memory-family loop fills these with an arbitrary placeholder. The executor
    // must dispatch on `family()` before consulting any Io-only accessor
    // (`resolved_port`, `status_mask`, `fresh_iteration_spins`).
    port_source: PollPortSource,
    branch_shape: PollBranchShape,
    status_mask: u8,
    branch_when_zero: bool,
    raw_core_clocks: u64,
    at_head: bool,
    /// Present only for the memory-poll family (M1). `None` means Io.
    memory: Option<PollMemoryFields>,
}

#[cfg(feature = "jit")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollPortSource {
    CurrentDx,
    Ebx,
    Ecx,
}

#[cfg(feature = "jit")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollBranchShape {
    Direct,
    PairedJmp,
}

/// Which certified shape family a `PollLoop` belongs to. The machine-side
/// executor dispatches on this before touching any family-specific field.
#[cfg(feature = "jit")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollFamily {
    Io,
    Memory,
}

/// Fields specific to the memory-poll family (shape M1: `CMP r32,DS:[disp32]`
/// with no base/index register, terminal `Jcc rel8` back to entry). The bare
/// disp32 restriction means the effective linear address depends on no GPR,
/// only DS's base, which is read fresh at every classification (Found
/// results are never cached, so a DS reload cannot leave a stale linear).
#[cfg(feature = "jit")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PollMemoryFields {
    /// The polled cell's LINEAR address (ds.base + disp32), resolved at
    /// classification time. Not yet translated to physical: paging requires a
    /// fresh TLB probe every certification (see `CpuGsw::probe_linear_read_physical`).
    linear: u32,
    /// Access width in bytes (4, the shape is dword-only).
    width: u8,
    /// GPR index (ModRM reg field of the CMP) holding the comparand.
    comparand_gpr: u8,
    /// True for opcode 0x74 (JE): the loop spins while the cell equals the
    /// comparand. False for 0x75 (JNE): spins while they differ.
    spins_while_equal: bool,
}

#[cfg(feature = "jit")]
impl PollLoop {
    pub fn fetch_count(self) -> usize {
        usize::from(self.fetch_count)
    }

    pub fn fetch(self, index: usize) -> Option<(u32, u32, u8)> {
        (index < self.fetch_count()).then_some(self.fetches[index])
    }

    pub fn status_mask(self) -> u8 {
        self.status_mask
    }

    pub fn at_head(self) -> bool {
        self.at_head
    }

    pub fn resolved_port(self, cpu: &CpuGsw) -> u16 {
        let value = match self.port_source {
            PollPortSource::CurrentDx => cpu.registers.edx(),
            PollPortSource::Ebx => cpu.registers.ebx(),
            PollPortSource::Ecx => cpu.registers.ecx(),
        };
        value as u16
    }

    pub fn raw_core_clocks(self) -> u64 {
        self.raw_core_clocks
    }

    pub fn diagnostic_class(self) -> u8 {
        match (self.port_source, self.branch_shape) {
            (PollPortSource::CurrentDx, PollBranchShape::Direct) => 0,
            (PollPortSource::Ebx | PollPortSource::Ecx, PollBranchShape::Direct) => 1,
            (PollPortSource::Ebx | PollPortSource::Ecx, PollBranchShape::PairedJmp) => 2,
            (PollPortSource::CurrentDx, PollBranchShape::PairedJmp) => unreachable!(),
        }
    }

    pub fn fresh_iteration_spins(self, status: u8) -> bool {
        let zero = status & self.status_mask == 0;
        let branch_taken = zero == self.branch_when_zero;
        match self.branch_shape {
            PollBranchShape::Direct => branch_taken,
            PollBranchShape::PairedJmp => !branch_taken,
        }
    }

    pub fn fresh_backedge_taken(self, status: u8) -> bool {
        self.fresh_iteration_spins(status)
    }

    /// Which certified shape family this loop belongs to. The executor must
    /// check this before calling any Io-only accessor.
    #[cold]
    #[inline(never)]
    pub fn family(self) -> PollFamily {
        if self.memory.is_some() {
            PollFamily::Memory
        } else {
            PollFamily::Io
        }
    }

    /// The polled cell's linear address, for the memory family only.
    #[cold]
    #[inline(never)]
    pub fn memory_cell_linear(self) -> Option<u32> {
        self.memory.map(|m| m.linear)
    }

    /// The polled cell's access width in bytes, for the memory family only.
    #[cold]
    #[inline(never)]
    pub fn memory_cell_width(self) -> Option<u8> {
        self.memory.map(|m| m.width)
    }

    /// The comparand register's LIVE value, for the memory family only.
    #[cold]
    #[inline(never)]
    pub fn memory_comparand(self, cpu: &CpuGsw) -> Option<u32> {
        let mem = self.memory?;
        Some(cpu.registers.gpr32(mem.comparand_gpr))
    }

    /// Whether the loop is currently spinning, given the polled cell's current
    /// value and the comparand's live value (R1: the executor must check this
    /// before committing any memory-family skip). `None` outside the memory
    /// family.
    #[cold]
    #[inline(never)]
    pub fn memory_spin_predicate(self, cell_value: u32, comparand: u32) -> Option<bool> {
        let mem = self.memory?;
        let equal = cell_value == comparand;
        Some(if mem.spins_while_equal { equal } else { !equal })
    }
}

impl PartialEq for PerfCounters {
    // Diagnostic-only: never affects CpuGsw equality (conformance / goldens ignore it).
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}
impl Eq for PerfCounters {}

/// Subset of `poll_skip_spans`/`poll_skip_iterations` attributable to the
/// memory-poll shape family (M1: CMP r32,DS:[disp32]; Jcc back to entry),
/// bumped alongside the general totals in `commit_poll_skip_core`. Kept OUT
/// of `PerfCounters` and at the very tail of `CpuGsw` so no pre-existing
/// field offset moves (growing `PerfCounters` shifted the hot `pending_flags`
/// field and cost the interpreter measurable wall time; the offset pin in
/// cpu_test.rs guards 4440). Serialized into the same perf JSON keys by
/// `perf_counters_json`. Unconditional (not cfg jit) like the other poll
/// counters in `PerfCounters`, so non-jit consumers can name the type.
#[derive(Debug, Clone, Copy, Default)]
pub struct PollSkipMemoryCounters {
    pub spans: u64,
    pub iterations: u64,
}

impl PartialEq for PollSkipMemoryCounters {
    // Diagnostic-only, like PerfCounters: never affects CpuGsw equality.
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}
impl Eq for PollSkipMemoryCounters {}

/// Track C C1a: clif side-exit shell admission and entry diagnostics, the clif analogues of
/// the `jit_direct_*`/`jit_direct_reject_*` counters in `PerfCounters`. Kept OUT of
/// `PerfCounters` and at the very tail of `CpuGsw`, following the `PollSkipMemoryCounters`
/// pattern exactly: growing `PerfCounters` shifts the hot `pending_flags` field off its
/// pinned 4440 and costs the interpreter measurable wall time (the offset pin in cpu_test.rs
/// guards it). A C1a shell never retires a guest instruction natively (F-A1 option B: it
/// side-exits immediately), so there is no `insns` counterpart yet; `entries` counts adapter
/// round trips instead. Unconditional (not cfg-gated) like the other diagnostic counters, so
/// non-clif consumers can name the type.
#[derive(Debug, Clone, Copy, Default)]
pub struct JitClifCounters {
    pub compile_attempts: u64,
    pub units_installed: u64,
    pub entries: u64,
    pub side_exits: u64,
    pub reject_observer: u64,
    pub reject_interrupt_shadow: u64,
    pub reject_aggregate_accounting: u64,
    pub reject_mode_key: u64,
    pub reject_cs_layout: u64,
    pub reject_cpl: u64,
    pub reject_data_segment: u64,
    pub reject_alignment: u64,
    pub reject_fetch_limit: u64,
    pub reject_zero_budget: u64,
    /// C1c memory side-exit reasons (diagnostic only; the guest cannot observe which check
    /// fired, per the design's un-advanced-EIP discipline).
    pub mem_exit_alignment: u64,
    pub mem_exit_unavailable_or_kind: u64,
    pub mem_exit_permission: u64,
    pub mem_exit_code_watch: u64,
    pub mem_exit_segment_limit: u64,
    /// C1d: completed linked transfers (Direct's `linked_transfers` analogue) and hops
    /// that landed in the resolver trampoline (the unresolved split).
    pub linked_transfers: u64,
    pub unresolved_transfers: u64,
    /// C1e: clif-only recompile-churn diagnostics (D3). `smc_unit_restamps` and
    /// `smc_unit_kills` partition every SMC write that hit a COMPILED clif unit's span;
    /// `smc_unit_kills_multi_slot` is the subset of kills escalated by the coarse
    /// multi-slot rule (every touched slot individually tail-confined, but more than one
    /// touched). `smc_unit_kills_no_layout` is C1f's permanent-zero regression tripwire
    /// (dev_docs/plans/2026-07-19-clif-compile-churn-fix-design.md, Option 2): it used to
    /// count `Seen`/`Dormant` entries dropped by a page-granular conservative-eviction rule
    /// that caused a compile-churn treadmill (a heat-parked verdict erased by any unrelated
    /// write sharing its 4KB page, forcing a full re-walk and recompile for no reason); that
    /// rule is gone (`Seen`/`Dormant` hold no resource a write could invalidate, so nothing
    /// is lost by never dropping them), so this field is now EXPECTED to stay zero forever.
    /// A nonzero value signals the page-eviction bug has been reintroduced.
    pub smc_unit_kills: u64,
    pub smc_unit_restamps: u64,
    pub smc_unit_kills_no_layout: u64,
    pub smc_unit_kills_multi_slot: u64,
    /// Track C1f (dev_docs/plans/2026-07-19-clif-compile-second-cause-design.md): the
    /// second compile-treadmill diagnosis, committing the counters the C0 review's
    /// MINOR-2 asked for so a future re-diagnosis never again depends on reverted local
    /// instrumentation (see the track-c1f-postfix summary's "BLIND SPOT" note). Time spent
    /// inside the ONE `jit::clif::lower::compile_unit` Cranelift call per `Seen`-branch
    /// visit that reaches it (`run.rs`, immediately before `units_installed` or
    /// `park_compile_failed`). Previously this nanosecond total was folded into
    /// `PerfCounters::jit_direct_compile_ns`, mislabeling clif's own compile cost as
    /// Direct's (confirmed live: a clif-only run showed `jit_direct_compile_attempts == 0`
    /// with `jit_direct_compile_ns` in the tens of seconds); this field is the clif-only
    /// source of truth.
    pub compile_ns: u64,
    /// `Seen`-branch visits that bail WITHOUT installing and WITHOUT parking Dormant, so
    /// the key stays `Seen` and the very next visit re-runs the ENTIRE admission pipeline
    /// (heat checks, `walk_unit`, `plan_unit`, the code-page cover check,
    /// `SegmentLayout::capture`) from scratch. Two distinct un-parked bail points exist in
    /// `try_clif_continuation` today, each counted individually so the treadmill's source
    /// is attributable instead of inferred:
    /// - `retry_no_fast_map` (run.rs:1253-1261, the C0 review's MINOR-2 suspect): a
    ///   memory-bearing unit whose `FastMap` storage has not been allocated yet
    ///   (`jit::fast_map::FastMap::native_bases` returns `None` only before the very first
    ///   population anywhere, so this should be a narrow startup-window cost, not a
    ///   sustained one, UNLESS FastMap population is not actually active under the
    ///   running policy — see `memory.rs::fast_map_population_enabled`).
    /// - `retry_incomplete_walk` (run.rs:1190-1194): `walk_unit` returned no admittable
    ///   layout (`instructions == 0`), which given `clif_hot` already confirmed the
    ///   decode-cache line at this exact `(lin, d)` moments earlier, can only mean the
    ///   entry instruction itself is structurally unclassifiable
    ///   (`direct::unit_growth_classify` declines it, or its prefixes are unsupported) —
    ///   a PERMANENT, deterministic condition for that address, so once `clif_hot` latches
    ///   (frozen at threshold, never reset while the line survives), this bail can repeat
    ///   on literally every loop iteration of that address.
    pub retry_incomplete_walk: u64,
    pub retry_no_fast_map: u64,
    /// `Seen`-branch visits parked Dormant for a heat or structural admission reason,
    /// broken out by the exact guard that fired (mirroring the `jit_direct_reject_*`
    /// per-reason pattern already used for `run_clif_unit`'s entry guards), so a churn
    /// regression names its own cause instead of hiding in one lump. `park_heat_chunk` and
    /// `park_heat_span` duplicate a subset of the shared (Direct+clif) `smc_heat_demotions`
    /// counter but isolate clif's own two heat-gate sites specifically.
    ///
    /// Invariant (checkable in a regression test, since `compile_attempts` increments
    /// unconditionally once the leading-run gate passes, `run.rs:1204`, strictly before
    /// every field below fires): `compile_attempts` equals the sum of `units_installed`,
    /// `park_no_code_cover`, `park_heat_span`, `park_segment_capture_failed`,
    /// `retry_no_fast_map`, `park_backend_unavailable`, `park_compile_failed`, and
    /// `park_install_failed`. `park_no_lowerable`, `park_heat_chunk`, and
    /// `retry_incomplete_walk` all fire BEFORE the `compile_attempts` increment and are
    /// deliberately excluded from that sum.
    pub park_no_lowerable: u64,
    pub park_heat_chunk: u64,
    pub park_heat_span: u64,
    pub park_no_code_cover: u64,
    pub park_segment_capture_failed: u64,
    /// The clif compile/install infrastructure itself was unavailable (backend allocation
    /// failed after `ClifBackend::new()`, the sentinel descriptor was unavailable, or the
    /// backend vanished on the second borrow): three merged `run.rs` sites, expected near
    /// zero on any supported host.
    pub park_backend_unavailable: u64,
    pub park_compile_failed: u64,
    pub park_install_failed: u64,
    /// A snapshot (not an accumulator) of `ClifUnitCache`'s admission map size
    /// (`entries: HashMap<ClifUnitKey, ClifUnitState>`) at read time, the requested
    /// "entries-map size" gauge; zero on a non-clif-backend build.
    pub entries_len: u64,
}

impl PartialEq for JitClifCounters {
    // Diagnostic-only, like PerfCounters: never affects CpuGsw equality.
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}
impl Eq for JitClifCounters {}

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

/// Keep ordinary opcode buckets unchanged, but split x87 escapes by ModRM byte. The eight escape
/// opcodes reuse that byte as their operation selector, so aggregating only D8 through DF hides the
/// exact forms a direct backend still needs.
fn cpu_profile_opcode(insn: &DecodedInsn) -> u16 {
    if insn.group == DecodeGroup::Fpu
        && matches!(insn.opcode, 0xd8..=0xdf)
        && let Some(modrm) = insn.modrm
    {
        return 0x8000
            | ((insn.opcode - 0xd8) << 8)
            | (u16::from(modrm.mode) << 6)
            | (u16::from(modrm.reg) << 3)
            | u16::from(modrm.rm);
    }
    insn.opcode
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

#[cfg(feature = "jit")]
#[derive(Debug, Default)]
struct DirectRuntimeState {
    // Inline host policy for interpreter hot paths. Dynamic entry guards remain in the backend.
    admission_active: bool,
}

#[cfg(feature = "jit")]
impl Clone for DirectRuntimeState {
    fn clone(&self) -> Self {
        Self::default()
    }
}

#[cfg(feature = "jit")]
impl PartialEq for DirectRuntimeState {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

#[cfg(feature = "jit")]
impl Eq for DirectRuntimeState {}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    // a hand-written Default impl must keep it 0 because
    // cycle_no_interrupt_check assumes a fresh CPU starts there.
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
    // Inline hot gate for the boxed REP continuation. Ordinary run entries read this bool without
    // chasing the cold resume-state pointer.
    rep_resume_active: bool,
    // Host-side continuation state for a budget-split REP instruction. A paused REP keeps its
    // architectural EIP at the prefix byte so an interrupt frame restarts it correctly, while
    // this state retains the decoded instruction and post-decode EIP for a no-interrupt resume.
    rep_execution: Box<RepExecution>,
    // The active GSW mode. Its core-table row owns the guest-facing persona,
    // cache geometry, ordering, and clock identity.
    mode: GswMode,
    // Caches linear->physical page translations so paged protected mode (DOS
    // extenders, Win9x) does not re-walk the two-level page table on every access.
    // Flushed on CR0/CR3 writes, task switch, and INVLPG.
    tlb: Tlb,
    code_page: CodePageCache,
    prefetch: PrefetchWindow,
    /// C1e decode scratch (valid only during one `decode` call): the EIP watermark after
    /// the last STRUCTURAL byte (opcode/ModRM/SIB) and the displacement byte count
    /// consumed so far, from which the finalize step derives the recorded
    /// `{disp_len, imm_len}` pair. Both are ZEROED by the finalize step so no residue
    /// outlives the decode: two lockstep arms legally decode different numbers of times
    /// (a native clif unit retires slots without re-decoding), so persistent residue
    /// would trip the derived `CpuGsw` equality on nothing architectural (found by the
    /// C1e storm battery). Loose fields, not a struct: the lone `u8` packs into an
    /// existing padding hole, keeping `pending_flags` on its pinned offset 4440 (the
    /// cpu_test.rs offset pin; a `{u32, u8}` struct here shifted it by 8).
    decode_tail_start: u32,
    decode_disp_len: u8,
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
    /// Helper-free register block cache. Like the decode and region caches, this is host-only
    /// accelerator state and clones empty.
    /// The Direct block cache plus the jit state shared across backends (the hoisted G1 SMC
    /// heat map; see `jit::JitState`). Kept under the existing field name because the wrapper
    /// derefs to the block cache, preserving every `jit_direct.<method>` call site.
    #[cfg(feature = "jit")]
    jit_direct: Box<jit::JitState>,
    #[cfg(feature = "jit")]
    direct_runtime: DirectRuntimeState,
    /// IZARRAVM_POLL_SKIP_NEG_CACHE (default on, "0"/"" disables): consult
    /// and populate the poll negative cache. Kill switch for A/B proofs;
    /// the scan itself is always correct without it.
    #[cfg(feature = "jit")]
    poll_neg_cache_enabled: bool,
    /// Linear-page pointer map for the direct x64 backend. Large arrays allocate on first fill;
    /// clones start empty like the other host-only accelerator caches.
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    jit_fast_map: Box<jit::fast_map::FastMap>,
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
    /// Optional trace-driven unit simulator (feature `jit`, diagnostic). Off by default and enabled
    /// per-CPU via `set_unit_sim_enabled`; when on it observes every retired interpreter instruction
    /// to measure hypothetical superblock units. Non-architectural: excluded from CPU equality (a
    /// `UnitSimSlot`'s always-true `PartialEq`, like the decode cache) and cloned off, so enabling it
    /// never makes an otherwise-identical CPU compare unequal. See `jit::unit_sim`.
    #[cfg(feature = "jit")]
    unit_sim: UnitSimSlot,
    /// Current privilege level. Per the 386 PRM, CPL is a *cached* quantity carried in
    /// (the hidden part of) CS, updated only at defined transition points -- it is not a
    /// live formula over the current CS selector. Updated at: real mode / PE clear (0);
    /// far JMP/CALL/RETF/IRET same- and inter-privilege transfers; call/task gates and
    /// `task_switch` (to the target DPL); IRET-into-V86 (3); `deliver_exception` (to the
    /// gate's target level, before the frame-push sequence begins -- see that function);
    /// reset (0). `current_privilege_level` returns this field directly;
    /// see that method for why a live `CS.selector & 3` read is wrong during exception
    /// delivery out of a V86 source (the source CS can carry arbitrary low bits before
    /// the frame's own CS is loaded, which must not be mistaken for the CPL the pushes
    /// execute under).
    cpl: u8,
    /// Memory-poll skip counters, deliberately the LAST field: adding them to
    /// `PerfCounters` (declared far above) shifted every later CpuGsw field,
    /// moving the hot `pending_flags` off its pinned 4440 and costing the
    /// interpreter measurable wall time. At the tail they change only the
    /// struct's total size. See `PollSkipMemoryCounters`.
    poll_skip_memory: PollSkipMemoryCounters,
    /// Clif shell diagnostics, also at the tail for the same layout reason (Track C C1a);
    /// see `JitClifCounters`.
    jit_clif: JitClifCounters,
}

impl Default for CpuGsw {
    fn default() -> Self {
        let decode_cache = DecodeCache::default();
        #[cfg(feature = "jit")]
        let jit_direct = Box::new(jit::JitState::new(jit::direct::BlockCache::new(
            decode_cache.line_count(),
        )));
        Self {
            registers: Registers::default(),
            fpu: X87::default(),
            control: ControlRegisters::default(),
            msr: Msrs::default(),
            gdtr: DescriptorTable::default(),
            idtr: DescriptorTable::default(),
            ldtr: SegmentRegister::default(),
            tr: SegmentRegister::default(),
            elapsed_clocks: 0,
            core_clocks_so_far: 0,
            timing_rem: 0,
            fp_rem: 0,
            halted: false,
            interrupt_shadow: false,
            rep_resume_active: false,
            rep_execution: Box::default(),
            mode: GswMode::Gsw586,
            tlb: Tlb::default(),
            code_page: CodePageCache::default(),
            prefetch: PrefetchWindow::default(),
            decode_tail_start: 0,
            decode_disp_len: 0,
            data_read_pages: DirectPageCache::default(),
            data_write_pages: DirectPageCache::default(),
            fetch_page: FetchPageCache::default(),
            written_pages: [None; TRACKED_WRITE_PAGES],
            written_count: 0,
            written_pages_overflow: false,
            decode_cache,
            #[cfg(feature = "jit")]
            jit_regions: jit::RegionTable::default(),
            #[cfg(feature = "jit")]
            jit_direct,
            #[cfg(feature = "jit")]
            direct_runtime: DirectRuntimeState::default(),
            #[cfg(feature = "jit")]
            poll_neg_cache_enabled: poll_neg_cache_default(),
            #[cfg(all(
                feature = "jit",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            jit_fast_map: Box::new(jit::fast_map::FastMap::default()),
            perf: PerfCounters::default(),
            profile: CpuProfileState::default(),
            pending_flags: PendingFlags::default(),
            alignment_armed: false,
            #[cfg(feature = "jit")]
            unit_sim: UnitSimSlot::default(),
            cpl: 0,
            poll_skip_memory: PollSkipMemoryCounters::default(),
            jit_clif: JitClifCounters::default(),
        }
    }
}

#[cfg(feature = "jit")]
impl CpuGsw {
    /// Test seam: force the poll negative cache on or off regardless of the
    /// IZARRAVM_POLL_SKIP_NEG_CACHE environment. Host bookkeeping only.
    pub fn set_poll_neg_cache_enabled_for_test(&mut self, enabled: bool) {
        self.poll_neg_cache_enabled = enabled;
    }
}

/// Non-architectural slot holding the optional unit simulator (feature `jit`). Wrapping the boxed
/// `UnitSim` lets `CpuGsw` keep its derived `PartialEq`/`Eq`/`Clone`/`Debug`: this type reports
/// always-equal (two CPUs differing only in diagnostic sim state are the same machine), clones with
/// the sim disabled (a transparent diagnostic, like the decode and region caches that clone empty),
/// and prints opaquely. See `jit::unit_sim`.
#[cfg(feature = "jit")]
#[derive(Default)]
struct UnitSimSlot(Option<Box<jit::unit_sim::SimLadder>>);

#[cfg(feature = "jit")]
impl PartialEq for UnitSimSlot {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

#[cfg(feature = "jit")]
impl Eq for UnitSimSlot {}

#[cfg(feature = "jit")]
impl Clone for UnitSimSlot {
    fn clone(&self) -> Self {
        // Diagnostic accelerator: a clone starts with the sim disabled, matching the decode/region
        // caches that clone empty. Re-enable it on the clone if measurement is wanted there.
        Self(None)
    }
}

#[cfg(feature = "jit")]
impl std::fmt::Debug for UnitSimSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnitSimSlot")
            .field("enabled", &self.0.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleOutcome {
    pub core_clocks: u32,
    pub halted: bool,
}

/// Result of an event-capped CPU run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetedRunOutcome {
    pub consumed_core_clocks: u32,
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
    /// The two-byte set-on-condition / two-operand IMUL block:
    /// SETcc r/m8 (0F 90-0F 9F) and IMUL reg, r/m (0F AF). Both are ModRM r/m forms with no immediate after
    /// the ModRM, so `decode` parses the ModRM + addressing descriptor and stores it (no `imm`
    /// fetch). The executor dispatches off the FULL u16 (`insn.opcode`, never narrowed to u8 first)
    /// and reuses `self.condition(insn.opcode as u8 & 0x0f)` for SETcc and
    /// `self.imul_truncated` for IMUL. SETcc is always byte-wide and uses `write_operand_u8`.
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
    ///   - the two-byte system/serializing/CPU-id ops with no encoded operand: INVD/WBINVD
    ///     (0F 08/09), WRMSR (0F 30), RDTSC (0F 31), RDMSR (0F 32),
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
    /// `run_string`/`execute_mmx_decoded`/CPUID/RDTSC/halt leaf logic verbatim; the only
    /// change is WHERE the ModRM/immediate is fetched (once, in `decode`). The 0F forms are folded
    /// into `insn.opcode` as 0x0F00 | second and dispatched off the full u16. The genuinely
    /// unimplemented neighbours (single-byte 0xF1; 0F AA RSM; the other unmapped 0F bytes) are NOT
    /// routed here. They stay on Fallback / TwoByteFallback and
    /// still #UD as `UnsupportedOpcode` / `UnsupportedTwoByteOpcode`.
    Misc,
    /// A single-byte opcode with no split implementation. After Stage A this is a pure dead-end: the
    /// only member is the genuinely-unimplemented 0xF1 (ICEBP), plus, as a decode-bug guard, any
    /// prefix byte `read_prefixes` did not consume. `execute_decoded` raises
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// C1e (D3, review finding M3): the ENCODED byte counts of this instruction's
    /// displacement and immediate fields, recorded by the decoder as it consumes them
    /// (the single decode authority; never back-computed from the stored values, whose
    /// widths are ambiguous after sign extension). The moffs address bytes of 0xA0-0xA3
    /// count as DISPLACEMENT (they are one architecturally, and clif's operand-lane
    /// routing depends on it).
    /// MEASURED layout effect (the L2-note truthfulness check the design mandates): the
    /// pair did NOT fit `DecodedInsn`'s padding; `size_of::<DecodedInsn>()` grew 36 -> 40
    /// (pinned by a test), so a `DecodeLine` grows by the same 4 bytes and the 4096-line
    /// cache by ~16 KB, still comfortably inside a normal L2 (the sizing comment at
    /// `DECODE_CACHE_LINES` is updated to match).
    disp_len: u8,
    imm_len: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RepBudget {
    bus_at_entry: u64,
    cap: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RepResume {
    insn: DecodedInsn,
    start_eip: u32,
    post_eip: u32,
    cs: SegmentRegister,
    precharged_core: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RepExecution {
    budget: Option<RepBudget>,
    resume: Option<RepResume>,
    yielded: bool,
}

/// Direct-mapped decode-cache lines (power of two so the index is a mask). A break-attribution
/// measurement on Doom demo3 8G/586 (the real pmode target, not the tiny real-mode benches the
/// earlier 2048 knee was derived from) showed 78% of run breaks were decode-cache misses on
/// continuations at 2048 lines: the pmode code footprint thrashes a 2048-entry direct-mapped
/// cache. Doubling to 4096 cut those misses by 53% (227M -> 106M breaks), boosted insns/run from
/// 14.5 to 23.2 (+60%), and lifted decode_hit from 94.85% to 97.66%. 8192 gave diminishing
/// returns (24.9 insns/run, +7% over 4096). At ~52 bytes per line (DecodeLine = tag + generation +
/// DecodedInsn; DecodedInsn grew 36 -> 40 bytes when C1e added the recorded
/// {disp_len, imm_len} pair) 4096 lines is ~208 KB, still inside L2 on a normal (8-32 MB L3)
/// machine.
/// Purely microarchitectural: the decode cache is transparent to CpuGsw equality, so this needs
/// no conformance/regolden work.
const DECODE_CACHE_LINES: usize = 4096;

/// Sweep knob: `IZARRAVM_DECODE_CACHE_LINES=<power of two>` overrides the decode-cache size at
/// construction. Decode replacement changes cold-fetch timing, so performance comparisons must
/// keep this value fixed. Read once, cached.
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
    /// path's covers-the-written-byte check. `phys_start..phys_start + len` is contiguous because
    /// page-straddling instructions are never inserted into this cache.
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
    /// Saturating direct-code admission count, independent of legacy region stamps.
    #[cfg(feature = "jit")]
    jit_direct_hotness: u8,
    /// Independent clif admission counter (Track C C1a, plan section 2.4 row P4): the two
    /// backends compile under a runtime policy switch (decision D-C1.4) with their own
    /// hotness history, so this never shares `jit_direct_hotness`.
    #[cfg(all(
        feature = "clif-backend",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    jit_clif_hotness: u8,
}

/// Per-physical-page code bookkeeping for the narrow SMC path: the ONE linear page code on this
/// physical page was decoded through, plus the alias condition that forces the sound global-flush
/// fallback. Page-straddling instructions are not cached. Rebuilt from scratch after every global
/// flush (cleared with the marks), so it never outlives the lines it describes.
#[derive(Debug, Clone, Copy)]
struct PageCodeInfo {
    lin_page: u32,
    /// A later decode saw a DIFFERENT linear page mapping this physical page: the
    /// physical-to-linear reconstruction is ambiguous, so writes here must flush globally.
    aliased: bool,
}

/// Slot count for the poll negative cache's per-page insert generations
/// (4 KiB linear pages, hashed by low page bits). Collisions merely
/// over-invalidate. Power of two.
#[cfg(feature = "jit")]
const POLL_NEG_GEN_SLOTS: usize = 1024;
/// Slot count for the poll negative cache itself. Power of two.
#[cfg(feature = "jit")]
const POLL_NEG_SLOTS: usize = 8192;
/// Generation payload width inside a packed negative entry. The 30-bit window
/// can wrap after 2^30 inserts on one page; a bucket aliasing back to an old
/// value only makes a stale negative look live, which suppresses a skip and
/// never corrupts guest state.
#[cfg(feature = "jit")]
const POLL_NEG_GEN_MASK: u32 = 0x3FFF_FFFF;

/// IZARRAVM_POLL_SKIP_NEG_CACHE policy: enabled unless explicitly "0" or "".
/// Lives here with the flag it feeds (`CpuGsw::poll_neg_cache_enabled`) rather
/// than in run.rs, which is at its line-policy ceiling.
#[cfg(feature = "jit")]
pub(crate) fn poll_neg_cache_policy(value: Option<&str>) -> bool {
    !matches!(value, Some("" | "0"))
}

/// Ambient default for the poll negative cache, read fresh at CPU construction.
#[cfg(feature = "jit")]
pub(crate) fn poll_neg_cache_default() -> bool {
    let value = std::env::var("IZARRAVM_POLL_SKIP_NEG_CACHE").ok();
    poll_neg_cache_policy(value.as_deref())
}

/// A direct-mapped, generation-stamped cache of decoded instructions keyed by linear EIP
/// (`cs.base + eip`). It lets a hot loop skip re-decoding the same bytes every iteration. The `gen`
/// counter is advanced whenever a decode could change meaning: CS base / paging / mode changes (via
/// `invalidate_code_caches`) and a mode change (via `set_mode`). A bump makes every stamped
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
    /// Coarse interpreter companion for the whole 4 GB physical space: 1 bit per 4 KiB page.
    /// Low memory still uses `code_bytes` for exact invalidation decisions.
    code_pages: Box<[u64]>,
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    native_code_watch: Box<jit::code_watch::StickyDecodeCodeWatch>,
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
    /// Per-linear-page insert generations for the poll-classification
    /// negative cache. Bumped ONLY by `put`: removals (narrow kills, whole
    /// cache generation flushes, displacement evictions) can never turn a
    /// structural "no poll shape" into a match, because every certified
    /// shape ends on a warm branch terminator and a shortened block ends on
    /// a non-terminator, so adding a warm line is the one mutation that can
    /// flip a negative, and every warm line is added through `put`. Host
    /// bookkeeping, excluded from CPU equality like the rest of the cache.
    #[cfg(feature = "jit")]
    poll_neg_gens: Box<[u32]>,
    /// Direct-mapped negative cache: packed (lin:32 | d:1 | valid:1 |
    /// gen:30) entries. A live entry means "the last full backward scan at
    /// this (lin, d) found no poll shape for code-byte-only reasons and no
    /// insert has touched the page since."
    #[cfg(feature = "jit")]
    poll_neg: Box<[u64]>,
    /// Bumped whenever a narrow SMC kill lands inside an installed JIT region's physical span:
    /// the region's slot table may now be stale (the entry line's stamp can survive a kill of a
    /// LATER slot's line). `run_region` refuses a region whose `valid_epoch` lags and unstamps
    /// it, forcing matcher re-admission over the fresh decodes. Lives here (not on `CpuGsw`)
    /// because the whole cache is excluded from CPU equality; this is host bookkeeping.
    #[cfg(feature = "jit")]
    jit_smc_epoch: u32,
}

/// Result of publishing one decoded instruction. A different live key reports the displaced slot
/// so compiled blocks can preserve the interpreter's cold-fetch timing.
#[must_use]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DecodeInsertOutcome {
    inserted: bool,
    evicted_slot: Option<u32>,
}

impl DecodeInsertOutcome {
    const fn rejected() -> Self {
        Self {
            inserted: false,
            evicted_slot: None,
        }
    }
}

/// A tiny multiplicative hasher for the decode cache's `u32`-keyed `code_page_lin` map, replacing
/// std's SipHash. `put` runs it on every decode-cache miss-fill; SipHash's
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
            #[cfg(all(
                feature = "jit",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            native_code_watch: Box::default(),
            dirty_byte_words: Vec::new(),
            dirty_page_words: Vec::new(),
            code_page_lin: std::collections::HashMap::default(),
            #[cfg(feature = "jit")]
            poll_neg_gens: vec![0u32; POLL_NEG_GEN_SLOTS].into_boxed_slice(),
            #[cfg(feature = "jit")]
            poll_neg: vec![0u64; POLL_NEG_SLOTS].into_boxed_slice(),
            #[cfg(feature = "jit")]
            jit_smc_epoch: 0,
        }
    }

    #[cfg(feature = "jit")]
    #[inline]
    fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Mark the bytes `[physical, physical + len)` as holding cached code, so a later write touching
    /// any of them invalidates the cache. The page map covers every decoded instruction, including
    /// code that has not become hot enough to compile, while the low-memory byte map keeps the
    /// interpreter invalidation exact.
    #[inline]
    fn mark_code_range(&mut self, physical: u32, len: u8) {
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        self.native_code_watch.mark_range(physical, u32::from(len));
        let first_page = physical >> 12;
        let last_page = physical.wrapping_add(u32::from(len).saturating_sub(1)) >> 12;
        for page in first_page..=last_page {
            let word = (page >> 6) as usize;
            if self.code_pages[word] == 0 {
                self.dirty_page_words.push(word as u32);
            }
            self.code_pages[word] |= 1u64 << (page & 63);
        }
        for i in 0..u32::from(len) {
            let addr = physical.wrapping_add(i);
            if addr < SMC_BYTE_COVERAGE {
                let word = (addr >> 6) as usize;
                if self.code_bytes[word] == 0 {
                    self.dirty_byte_words.push(word as u32);
                }
                self.code_bytes[word] |= 1u64 << (addr & 63);
            }
        }
    }

    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    fn native_code_watch_table(&mut self) -> usize {
        self.native_code_watch.table_base()
    }

    #[cfg(all(
        test,
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    fn assert_native_watch_consistent(&self) {
        let mut expected = std::collections::HashSet::new();
        for line in &self.lines {
            if line.generation != self.generation {
                continue;
            }
            let Some(insn) = line.insn else {
                continue;
            };
            let mut chunk = line.phys_start & !0xf;
            let last = line.phys_start.wrapping_add(u32::from(insn.len) - 1) & !0xf;
            loop {
                expected.insert(chunk);
                if chunk == last {
                    break;
                }
                chunk = chunk.wrapping_add(16);
            }
        }
        for chunk in expected {
            assert!(self.native_code_watch.is_watched(chunk));
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
    fn put(&mut self, lin: u32, insn: DecodedInsn, d: bool, phys: u32) -> DecodeInsertOutcome {
        let len = u32::from(insn.len);
        if len == 0 {
            return DecodeInsertOutcome::rejected();
        }
        let Some(linear_last) = lin.checked_add(len.saturating_sub(1)) else {
            return DecodeInsertOutcome::rejected();
        };
        let Some(physical_last) = phys.checked_add(len.saturating_sub(1)) else {
            return DecodeInsertOutcome::rejected();
        };
        if lin >> 12 != linear_last >> 12 || phys >> 12 != physical_last >> 12 {
            // The tail can map to a noncontiguous physical page. Re-decode page-straddling
            // instructions instead of publishing a fictitious contiguous watch.
            return DecodeInsertOutcome::rejected();
        }

        // A new warm line can turn a cached poll-scan negative on this page
        // into a match; retire the page's negatives. Decode-miss path only.
        #[cfg(feature = "jit")]
        {
            let gen_slot = ((lin >> 12) as usize) & (POLL_NEG_GEN_SLOTS - 1);
            self.poll_neg_gens[gen_slot] = self.poll_neg_gens[gen_slot].wrapping_add(1);
        }

        let slot = lin & self.mask;
        let index = slot as usize;
        let previous = self.lines[index];
        // Decode-native marks are generation-sticky. Mark the replacement before publishing it;
        // a displaced line deliberately leaves a conservative mark until global invalidation.
        self.mark_code_range(phys, insn.len);

        let page = phys >> 12;
        match self.code_page_lin.entry(page) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(PageCodeInfo {
                    lin_page: lin >> 12,
                    aliased: false,
                });
            }
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let info = e.get_mut();
                info.aliased |= info.lin_page != lin >> 12;
            }
        }
        let displaced = previous.generation == self.generation
            && previous.insn.is_some()
            && (previous.tag != lin || previous.d != d);
        self.lines[index] = DecodeLine {
            tag: lin,
            generation: self.generation,
            insn: Some(insn),
            d,
            phys_start: phys,
            #[cfg(feature = "jit")]
            jit_region: None,
            #[cfg(feature = "jit")]
            jit_hotness: 0,
            #[cfg(feature = "jit")]
            jit_direct_hotness: 0,
            #[cfg(all(
                feature = "clif-backend",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            jit_clif_hotness: 0,
        };

        DecodeInsertOutcome {
            inserted: true,
            evicted_slot: displaced.then_some(slot),
        }
    }

    /// Try to invalidate ONLY the lines covering the written physical byte `physical` (already
    /// known to be a marked code byte). Returns true when handled narrowly; false means the
    /// caller must fall back to the whole-cache flush (unknown page, an aliased page, or any other
    /// reason the physical-to-linear reconstruction is unsound). The mark
    /// stays set either way: a stale mark only costs a future narrow attempt, never correctness.
    #[inline]
    fn narrow_invalidate(&mut self, physical: u32) -> Option<u32> {
        let info = *self.code_page_lin.get(&(physical >> 12))?;
        if info.aliased {
            return None;
        }
        // Within one page the offset is mapping-invariant, so the code-side linear of the
        // written byte is reconstructible without the writer's own linear (device/DMA writes
        // narrow too). Any line covering the byte starts at most 14 bytes earlier (15-byte max
        // instruction), under this same mapping (a different mapping would have set `aliased`).
        let written_lin = (info.lin_page << 12) | (physical & 0xfff);
        let mut killed = 0u32;
        for candidate in written_lin.saturating_sub(14)..=written_lin {
            let removed = {
                let line = &mut self.lines[(candidate & self.mask) as usize];
                if line.generation != self.generation || line.tag != candidate {
                    false
                } else {
                    let len = line.insn.map_or(0, |i| u32::from(i.len));
                    if line.phys_start <= physical && physical < line.phys_start.wrapping_add(len) {
                        // The sticky native mark stays conservative until global invalidation.
                        line.generation = 0;
                        true
                    } else {
                        false
                    }
                }
            };
            if removed {
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

    /// The stored physical start of the live line for `lin`. Warm fetch timing uses this to retain
    /// the translation from the cold decode; JIT admission also derives block provenance from it.
    fn line_phys_start(&self, lin: u32, d: bool) -> Option<u32> {
        let line = &self.lines[(lin & self.mask) as usize];
        (line.generation == self.generation && line.tag == lin && line.d == d)
            .then_some(line.phys_start)
    }

    /// Whether the line for `lin` is live for exactly this key: the same condition `get` uses,
    /// without copying the insn. The region step probes this per slot, which is the
    /// interpreter's own next-continuation decode probe in miss-detection terms. Compiled root
    /// dispatch uses it to validate a suspended portal before republishing it.
    #[cfg_attr(not(feature = "jit"), allow(dead_code))]
    #[inline]
    fn line_live(&self, lin: u32, d: bool) -> bool {
        let line = &self.lines[(lin & self.mask) as usize];
        line.generation == self.generation && line.tag == lin && line.d == d
    }

    /// Current insert generation for the page holding `lin`.
    #[cfg(feature = "jit")]
    #[inline]
    fn poll_neg_gen(&self, lin: u32) -> u32 {
        self.poll_neg_gens[((lin >> 12) as usize) & (POLL_NEG_GEN_SLOTS - 1)]
    }

    #[cfg(feature = "jit")]
    #[inline]
    fn poll_neg_slot(lin: u32, d: bool) -> usize {
        // Fold the top log2(POLL_NEG_SLOTS) bits of the 32-bit mix. Deriving the
        // shift from the slot count keeps the two in lockstep (8192 slots => 19).
        const SHIFT: u32 = 32 - POLL_NEG_SLOTS.trailing_zeros();
        let mixed = (lin ^ (lin >> 13) ^ (u32::from(d) << 5)).wrapping_mul(0x9E37_79B9);
        (mixed >> SHIFT) as usize & (POLL_NEG_SLOTS - 1)
    }

    /// Whether a live negative covers (lin, d): same key, same page insert
    /// generation. A hit means the scan would still return None.
    #[cfg(feature = "jit")]
    #[inline]
    pub(crate) fn poll_negative_live(&self, lin: u32, d: bool) -> bool {
        let entry = self.poll_neg[Self::poll_neg_slot(lin, d)];
        entry & 0xFFFF_FFFF == u64::from(lin)
            && (entry >> 32) & 1 == u64::from(d)
            && (entry >> 33) & 1 == 1
            && (entry >> 34) as u32 == self.poll_neg_gen(lin) & POLL_NEG_GEN_MASK
    }

    /// Record a structural (code-byte-only) negative for (lin, d).
    #[cfg(feature = "jit")]
    #[inline]
    pub(crate) fn record_poll_negative(&mut self, lin: u32, d: bool) {
        // `gen` is a reserved keyword under the 2024 edition (generator blocks).
        let page_gen = u64::from(self.poll_neg_gen(lin) & POLL_NEG_GEN_MASK);
        self.poll_neg[Self::poll_neg_slot(lin, d)] =
            u64::from(lin) | (u64::from(d) << 32) | (1u64 << 33) | (page_gen << 34);
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

    /// Observe a continuation for direct-code admission. Once the second encounter makes the line
    /// hot, later encounters stay eligible for the direct cache's separate first-seen/compile probe.
    #[cfg(feature = "jit")]
    #[inline]
    fn direct_hot(&mut self, lin: u32, d: bool, threshold: u8) -> bool {
        let line = &mut self.lines[(lin & self.mask) as usize];
        if line.generation != self.generation || line.tag != lin || line.d != d {
            return false;
        }
        if line.jit_direct_hotness < threshold {
            line.jit_direct_hotness += 1;
        }
        line.jit_direct_hotness == threshold
    }

    /// Clif analogue of `direct_hot` (Track C C1a, plan section 2.4 row P4): an independent
    /// per-decode-line counter, since the two backends compile under a runtime policy switch
    /// (decision D-C1.4) and never share admission history.
    #[cfg(all(
        feature = "clif-backend",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    fn clif_hot(&mut self, lin: u32, d: bool, threshold: u8) -> bool {
        let line = &mut self.lines[(lin & self.mask) as usize];
        if line.generation != self.generation || line.tag != lin || line.d != d {
            return false;
        }
        if line.jit_clif_hotness < threshold {
            line.jit_clif_hotness += 1;
        }
        line.jit_clif_hotness == threshold
    }

    /// Invalidate every cached line and drop every matching code watch. The generation advance and
    /// watch clear stay one operation so no dead decode line can leave an unowned native watch.
    fn invalidate_and_clear_code_marks(&mut self) {
        if self.generation == u32::MAX {
            // Clear old generation-1 lines before 1 can become live again.
            self.lines.fill(DecodeLine::default());
            self.generation = 1;
        } else {
            self.generation += 1;
        }
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
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        self.native_code_watch.clear();
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
/// era targets (386 ~9200, 486 ~61000, 586 ~475000) to within ~0.3%.
///
/// fp-mandel TRADE-OFF: fp-mandel is x87-compute-bound (~7280 instruction clocks
/// vs ~6247 bus per pixel), so it rides this dial. Dhrystone pinned to its owner
/// target forces the compute dial small on the fast modes, which makes fp-mandel
/// run well above its ratio-anchored band and at a 586/486 ratio of ~8x (the model
/// floor with Dhrystone pinned is ~7.8x; see bench_reference.rs). Matching both the
/// fp-mandel ratio AND the Dhrystone target needs a separate x87 latency dial (a
/// deferred Whetstone-payload follow-up); Dhrystone is PRIMARY, so fp-mandel's band
/// is recentered on the achieved value and the ratio gap recorded.
const fn level_timing(persona: CpuPersona) -> (u32, u32) {
    match persona {
        CpuPersona::I386 => (2, 5),
        CpuPersona::I486 => (1, 12),
        CpuPersona::I586 => (1, 12),
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
/// 386/486: their FP rides `level_timing` alone, keeping the frozen-class
/// bench bytes and the 486 Whetstone anchor (6.5 MFLOPS) untouched.
///
/// The I586 values replace the old flat (31/34) scalar with separate classes.
/// Quake demo1's FP
/// clocks are conversion/traffic-shaped (FILD/FIST + f32/f64 memory ops) while
/// Whetstone's are register-arithmetic/transcendental-shaped, and the era
/// anchors pull those classes in OPPOSITE directions (real P55C-200: Quake
/// ~42 fps, Whetstone 34.5 MFLOPS). Register-class ops get CHEAPER than raw 387
/// clocks (U/V pairing, 1-clock FADD/FMUL issue); the int<->fp boundary pays an
/// effective stall surcharge (see FpOpClass::IntConvert). CALIBRATION
/// CONSTRAINTS: Whetstone 586 = 34.5 MFLOPS and 486 = 6.5 stay era-exact;
/// Dhrystone/Sieve run no x87 and stay bit-identical; 386 frozen.
const fn fp_timing_class(persona: CpuPersona, class: FpOpClass) -> u32 {
    match persona {
        CpuPersona::I386 | CpuPersona::I486 => FP_TIMING_DEN,
        CpuPersona::I586 => match class {
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
pub const fn bus_timing(persona: CpuPersona) -> (u32, u32) {
    match persona {
        CpuPersona::I386 => (23, 31),
        CpuPersona::I486 => (1, 3),
        CpuPersona::I586 => (7, 30),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IsaGeneration {
    I386,
    I486,
    P55c,
    Never,
}

const fn persona_supports(persona: CpuPersona, required: IsaGeneration) -> bool {
    match required {
        IsaGeneration::I386 => true,
        IsaGeneration::I486 => matches!(persona, CpuPersona::I486 | CpuPersona::I586),
        IsaGeneration::P55c => matches!(persona, CpuPersona::I586),
        IsaGeneration::Never => false,
    }
}

/// Generation requirement for each implemented two-byte opcode family. Operand-sensitive
/// additions such as INVLPG and CR4 are checked by their executors after the ModRM is decoded.
const fn two_byte_isa_generation(opcode: u8) -> IsaGeneration {
    match opcode {
        // AMD fast system calls and the P6 conditional-move family are outside P55C.
        // RSM stays invalid because this core never enters SMM.
        0x05 | 0x07 | 0x40..=0x4f | 0xaa => IsaGeneration::Never,
        // 486 additions.
        0x08 | 0x09 | 0xb0 | 0xb1 | 0xc0 | 0xc1 | 0xc8..=0xcf => IsaGeneration::I486,
        // Pentium and Pentium MMX additions.
        0x30..=0x32 | 0xa2 | 0xc7 => IsaGeneration::P55c,
        op if is_mmx_two_byte(op) => IsaGeneration::P55c,
        _ => IsaGeneration::I386,
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
    value.count_ones().is_multiple_of(2)
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
