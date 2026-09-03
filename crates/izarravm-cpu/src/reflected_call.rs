// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Shared, always-compiled primitives for the reflected-call HLE work
//! (`dev_docs/2026-09-03-reflected-call-hle-design.md` Revision 2,
//! `dev_docs/2026-09-04-reflected-call-slice1-plan.md` Revision 2, section
//! 2's module-layout table, orchestrator decision 13.1). Extracted out of
//! `reflected_call_diag.rs` (the slice 0/0b/0c diagnostic) so the slice 1
//! memo (`reflected_call_memo.rs`) does not duplicate the page walker or the
//! entry-image capture. **No state, no `Mutex`, no env read** -- every item
//! here is a pure function or a plain data type, safe to call from
//! production code with no feature gate.
//!
//! `reflected_call_diag.rs`'s own behaviour is UNCHANGED by this move: it
//! now writes `use crate::reflected_call::*;` in place of its own copies of
//! these items, and nothing else about it moved.

use super::*;

/// The legacy VGA aperture, physical `0xA0000..=0xBFFFF`. The CRTC reads
/// guest memory every scanline with no arming step, so a write here is never
/// eligible for the "restored, so skip it" disposition -- an intermediate
/// value is observable on screen even if the final value matches the entry
/// value.
// Consumed today only by `reflected_call_diag` (`#[cfg(feature =
// "reflected-call-diagnostic")]`); the plain build's memo module does not
// yet classify writes against the aperture (it has no answer path to guard),
// so a plain `cargo check`/`clippy` sees these as dead without the `allow`.
#[allow(dead_code)]
pub(crate) const FRAMEBUFFER_APERTURE_LO: u32 = 0x000A_0000;
#[allow(dead_code)]
pub(crate) const FRAMEBUFFER_APERTURE_HI: u32 = 0x000B_FFFF;

/// The architectural `EFLAGS` bits the entry image and the epilogue compare
/// / restore. Grown from the diagnostic's `0x0003_7fd5` (slice1 plan
/// Revision 2, R2.10 item 10) to add **AC** (bit 18): `AC` together with
/// `CR0.AM` decides whether an unaligned access faults, and a reflected
/// trip's `AH=0Bh` key spends its whole life in a V86 excursion where AC's
/// value at entry is architectural state the image must pin.
pub(crate) const EFLAGS_ARCH_MASK: u32 = 0x0003_7fd5 | (1 << 18);

/// A masked, sign-agnostic compare at the write's own width: `latest` and
/// `pre` are full 32-bit storage, but a byte or word write only ever
/// changed the low N bytes, so classification (Class R: "restored") must
/// compare only those bytes -- comparing the full dword would report a
/// byte write as "not restored" merely because the untouched high bytes of
/// the two 32-bit reads/writes happened to differ (they didn't move; they
/// were simply never part of this access).
pub(crate) fn mask_to_width(v: u32, width_bytes: u32) -> u32 {
    match width_bytes {
        1 => v & 0xFF,
        2 => v & 0xFFFF,
        _ => v,
    }
}

// ---------------------------------------------------------------------------
// Entry image
// ---------------------------------------------------------------------------

/// One cached segment descriptor's selector plus the base/limit/access
/// triple already resident in the CPU's segment-register cache -- reading
/// this costs nothing beyond the field load `SegmentRegister` already is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub(crate) struct CachedSegment {
    pub selector: u16,
    pub base: u32,
    pub limit: u32,
    pub access: u8,
    /// The descriptor's D/B bit as the CPU caches it. Part of the closure rule for
    /// the same reason the base is: `SS.B` decides the stack width every push and pop
    /// inside the trip uses, and `CS.D` decides the default operand and address size
    /// of every instruction it executes -- so two entries agreeing on selector, base,
    /// limit and access but not on this bit are NOT the same architectural state, and
    /// an epilogue that restores the other five fields and not this one leaves the
    /// guest running at the wrong width.
    pub default_size_32: bool,
}

impl CachedSegment {
    fn capture(reg: SegmentRegister) -> Self {
        CachedSegment {
            selector: reg.selector,
            base: reg.base,
            limit: reg.limit,
            access: reg.access,
            default_size_32: reg.default_size_32,
        }
    }

    /// Rebuild the `SegmentRegister` this was captured from, for the answer's
    /// epilogue: every field of `SegmentRegister` is covered, so this is lossless.
    pub(crate) fn to_segment(self) -> SegmentRegister {
        SegmentRegister {
            selector: self.selector,
            base: self.base,
            limit: self.limit,
            access: self.access,
            default_size_32: self.default_size_32,
        }
    }
}

/// Every CPU-state input that can reach the DATA of a journaled write, per
/// slice1 plan Revision 2 R2.1's closure rule: *the read set covers memory
/// only, so every CPU-state input that can reach the data of a journaled
/// write must be in the entry image* -- otherwise two calls that differ only
/// in, say, `DS` produce the same `MemoKey`, the same image, the same read
/// set and the same answer, while the trip's own code stores `DS` into
/// memory the read set never covers.
///
/// Grown from the diagnostic's 22-field image (`reflected_call_diag.rs`,
/// pre-slice1) to this closed set: all six segment selectors WITH their
/// cached descriptors (CS/SS were already here; DS/ES/FS/GS are new), GDTR,
/// LDTR and TR (selector plus the non-blocking R2.10/R2.14 amendment's
/// cached limit and access), CR4 and DR7. `FIELD_NAMES` in the diagnostic
/// module deliberately did NOT grow with this struct (plan section 2: "Do
/// not change `reflected_call_diag.rs`'s behaviour" -- its own 22-name
/// variance table stays scoped to the 22 fields it always tracked).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct EntryImage {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
    pub esp: u32,
    pub ebp: u32,
    pub esi: u32,
    pub edi: u32,
    pub eflags_masked: u32,
    pub cs: CachedSegment,
    pub ss: CachedSegment,
    pub ds: CachedSegment,
    pub es: CachedSegment,
    pub fs: CachedSegment,
    pub gs: CachedSegment,
    pub cr0: u32,
    pub cr3: u32,
    pub cr4: u32,
    pub cpl: u8,
    pub vm: bool,
    pub idtr_base: u32,
    pub idtr_limit: u16,
    pub gdtr_base: u32,
    pub gdtr_limit: u16,
    pub ldtr_selector: u16,
    pub ldtr_base: u32,
    pub ldtr_limit: u32,
    pub ldtr_access: u8,
    pub tr_selector: u16,
    pub tr_base: u32,
    pub tr_limit: u32,
    pub tr_access: u8,
    pub dr7: u32,
}

impl EntryImage {
    pub(crate) fn capture(cpu: &CpuGsw) -> Self {
        let regs = &cpu.registers;
        EntryImage {
            eax: regs.eax(),
            ebx: regs.ebx(),
            ecx: regs.ecx(),
            edx: regs.edx(),
            esp: regs.esp(),
            ebp: regs.ebp(),
            esi: regs.esi(),
            edi: regs.edi(),
            // `cpu.eflags()`, NOT `regs.eflags`: this CPU carries its arithmetic
            // flags lazily, so `registers.eflags` alone is a REPRESENTATION of the
            // flags and not the architectural value (`CpuGsw::settled`'s doc says so
            // in as many words). Capturing the base would let two trips whose
            // architectural flags genuinely differ -- one with the difference still
            // living in `pending_flags` -- compare EQUAL here, which is precisely the
            // closure-rule violation R2.1 exists to close, one level below the
            // register file.
            eflags_masked: cpu.eflags() & EFLAGS_ARCH_MASK,
            cs: CachedSegment::capture(regs.segment(SegmentIndex::Cs)),
            ss: CachedSegment::capture(regs.segment(SegmentIndex::Ss)),
            ds: CachedSegment::capture(regs.segment(SegmentIndex::Ds)),
            es: CachedSegment::capture(regs.segment(SegmentIndex::Es)),
            fs: CachedSegment::capture(regs.segment(SegmentIndex::Fs)),
            gs: CachedSegment::capture(regs.segment(SegmentIndex::Gs)),
            cr0: cpu.control.cr0,
            cr3: cpu.control.cr3,
            cr4: cpu.control.cr4,
            cpl: cpu.current_privilege_level(),
            vm: cpu.is_v86_mode(),
            idtr_base: cpu.idtr.base,
            idtr_limit: cpu.idtr.limit,
            gdtr_base: cpu.gdtr.base,
            gdtr_limit: cpu.gdtr.limit,
            ldtr_selector: cpu.ldtr.selector,
            ldtr_base: cpu.ldtr.base,
            ldtr_limit: cpu.ldtr.limit,
            ldtr_access: cpu.ldtr.access,
            tr_selector: cpu.tr.selector,
            tr_base: cpu.tr.base,
            tr_limit: cpu.tr.limit,
            tr_access: cpu.tr.access,
            dr7: cpu.control.dr7,
        }
    }
}

// ---------------------------------------------------------------------------
// Address classification
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // some variants are constructed only by the diagnostic (feature-gated)
pub(crate) enum AddressClass {
    ClientStack,
    HostStack,
    Bda,
    Gdt,
    Ldt,
    Idt,
    Tss,
    PageTable,
    /// The legacy VGA aperture, physical `0xA0000..=0xBFFFF`. Never eligible
    /// for the "restored" write disposition -- see `FRAMEBUFFER_APERTURE_*`.
    FramebufferAperture,
    /// A byte-wise `peek_direct_ram` failed at this physical address: either
    /// a genuine device window, or an unmapped page. Never eligible for the
    /// "restored" disposition either.
    NotPlainRam,
    Other,
}

impl AddressClass {
    #[allow(dead_code)] // used only by the diagnostic (feature-gated) in a plain build
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::ClientStack => "client_stack",
            Self::HostStack => "host_stack",
            Self::Bda => "bda",
            Self::Gdt => "gdt",
            Self::Ldt => "ldt",
            Self::Idt => "idt",
            Self::Tss => "tss",
            Self::PageTable => "page_table",
            Self::FramebufferAperture => "framebuffer_aperture",
            Self::NotPlainRam => "not_plain_ram",
            Self::Other => "other",
        }
    }

    /// Design R2/slice1 plan section 4.3: device windows and the
    /// framebuffer aperture are never Class R ("restored, so skip it") --
    /// the CRTC (and, for a device window, the device itself) observes
    /// every intermediate value, not merely the final one an entry-vs-exit
    /// compare would see.
    pub(crate) fn never_restored(self) -> bool {
        matches!(self, Self::FramebufferAperture | Self::NotPlainRam)
    }
}

// ---------------------------------------------------------------------------
// Guest mode
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // consumed only by the diagnostic (feature-gated) in a plain build
pub(crate) enum GuestMode {
    Protected,
    V86,
    Real,
}

impl GuestMode {
    #[allow(dead_code)] // used only by the diagnostic (feature-gated) in a plain build
    pub(crate) fn sample(cpu: &CpuGsw) -> Self {
        if cpu.is_v86_mode() {
            GuestMode::V86
        } else if cpu.is_protected_mode() {
            GuestMode::Protected
        } else {
            GuestMode::Real
        }
    }

    #[allow(dead_code)] // used only by the diagnostic (feature-gated) in a plain build
    pub(crate) fn name(self) -> &'static str {
        match self {
            GuestMode::Protected => "pm",
            GuestMode::V86 => "v86",
            GuestMode::Real => "real",
        }
    }
}

// ---------------------------------------------------------------------------
// Stack tracking
// ---------------------------------------------------------------------------

#[derive(Default, Clone, Copy, Debug)]
#[allow(dead_code)] // `limit` is read only by the diagnostic's classifier (feature-gated)
pub(crate) struct StackTrack {
    pub selector: u16,
    pub base: u32,
    pub limit: u32,
    pub low_water_esp: u32,
    pub last_esp: u32,
}

// ---------------------------------------------------------------------------
// The non-charging page walker
// ---------------------------------------------------------------------------

/// The two page-walk entries resolved for one linear address, TLB-
/// independent (a pure function of guest CR3 + guest page-table memory).
pub(crate) struct Walk {
    pub pde_phys: u32,
    pub pte_phys: u32,
}

/// TLB-independent linear-to-physical resolution for a DATA READ probe.
/// Never fills the TLB, never charges a bus access, never sets an
/// accessed/dirty bit -- it uses only `CpuBus::peek_direct_ram`, which by
/// contract does none of those things.
///
/// 1. Paging off: identity map, no walk.
/// 2. Paging on: resolve by hand: PDE at `(cr3 & !0xFFF) + (linear >> 22) *
///    4`, PTE at `(pde & !0xFFF) + ((linear >> 12) & 0x3FF) * 4`. `None` if
///    either present bit is clear or either peek misses. 4 MiB (PSE) pages
///    are not modelled: the reflected-call guest population (DOS extenders
///    under a DPMI host) runs exclusively 4 KiB paging.
pub(crate) fn probe_physical<B: CpuBus>(
    cpu: &CpuGsw,
    bus: &B,
    linear: u32,
) -> Option<(u32, Option<Walk>)> {
    if !cpu.is_paging_enabled() {
        return Some((linear, None));
    }
    let cr3 = cpu.control.cr3;
    let pde_phys = (cr3 & !0xFFF).wrapping_add((linear >> 22) * 4);
    let pde_value = bus.peek_direct_ram(pde_phys, BusWidth::Dword)?;
    if pde_value & 1 == 0 {
        return None;
    }
    let pte_phys = (pde_value & !0xFFF).wrapping_add(((linear >> 12) & 0x3FF) * 4);
    let pte_value = bus.peek_direct_ram(pte_phys, BusWidth::Dword)?;
    if pte_value & 1 == 0 {
        return None;
    }
    let phys = (pte_value & !0xFFF) | (linear & 0x0FFF);
    Some((phys, Some(Walk { pde_phys, pte_phys })))
}
