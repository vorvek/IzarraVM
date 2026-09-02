// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Slice 0b of the reflected-call HLE design
//! (`dev_docs/2026-09-03-reflected-call-hle-design.md`, `dev_docs/2026-09-03-
//! reflected-call-hle-review.md`, `dev_docs/2026-09-04-reflected-call-slice0b-
//! plan.md`): the CORRECTED trip-shape INSTRUMENT. Compiled in only under
//! `--features reflected-call-diagnostic`; a plain build carries none of this
//! code and is byte-identical to `main`. Armed by
//! `IZARRAVM_REFLECTED_CALL_DIAGNOSTIC=shape` or `=journal`; unset or `""`
//! means off, matching the campaign's `IZARRAVM_DIRECT_POLL_SKIP` spelling
//! convention.
//!
//! NO BEHAVIOUR CHANGE is claimed for the OFF arm and for the plain build.
//! Both armed modes DO change host behaviour (timing, and `journal` mode
//! forces the native backend off for the whole run): this is a diagnostic,
//! never used in a graded run.
//!
//! # 0b's corrections over slice 0 (see the plan doc, section headers below)
//!
//! * **§2.1**: the matching-return rule now compares SP at the ENTRY stack
//!   segment's OWN architectural width (16 bits when that segment's `B`/`D`
//!   bit is 0, 32 bits when it is 1), decided once at the `INT` and never
//!   re-derived. `CS.base` is dropped from the compare (kept as a diagnostic
//!   counter, `cs_base_differed_on_match`). This was slice 0's actual defect:
//!   comparing full 32-bit `ESP` against a pm16 client whose real-mode/V86
//!   excursion left the upper half dirty made `AH=0Bh` match almost never.
//! * **§2.2**: every far `JMP`/`CALL` is now a full candidate boundary (not a
//!   counter-only observation), closing the C3 gap (a real return landing on
//!   a far-transfer form the old code never checked).
//! * **§2.3**: rules 2 (frame-gone) and 3 (re-entry) close a trip but never
//!   count as a match; only rule 1 (`return_match`) does.
//! * **§2.4**: two near-miss histograms, `near_match[]` (CS/EIP already
//!   agree) and `near_match_cs_eip[]` (they do not -- without this bucket a
//!   C3-shaped failure was invisible, since the old code never inspected a
//!   far transfer for a match at all).
//! * **§3**: write disposition is now R (restored) / D (dead stack, derived
//!   from the trip's own low-water mark, no constant cap in the decision) /
//!   N (refused, everything else), computed once per write at trip close,
//!   admissible for every `AddressClass` except the two new device-window
//!   classes.
//! * **§4**: `probe_physical` never relies on `probe_linear_read_physical`
//!   alone (TLB-hit-only); on a miss it walks the page tables by hand through
//!   `peek_direct_ram`, non-charging, never filling the TLB.
//!
//! # Trip identity (unchanged from slice 0)
//!
//! A trip starts at a software `INT n` taken with `is_protected_mode() &&
//! !is_v86_mode()`. At most one OUTER trip is tracked at a time; a further
//! `INT` while one is open is counted as nested UNLESS it satisfies rule 3
//! (re-entry) or the open trip has gone stale (rule 4).

use super::*;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Software `INT` vectors this instrument journals.
const VECTOR_LO: u8 = 0x10;
const VECTOR_HI: u8 = 0x33;

/// Rule 4 (staleness) bound, unchanged from slice 0.
const MAX_TRIP_INSNS: u64 = 8_192;

/// Read/write-set and instruction/clock sample cap per key.
const MAX_SAMPLES_PER_KEY: usize = 200_000;

/// Top-N write addresses reported per key for manual labelling.
const TOP_ADDRESSES: usize = 16;

/// Top-N distinct `EntryImage`s tracked with counts, for Q2's "top 16 by trip
/// count, cumulative share at 1/4/8/16".
const TOP_IMAGES: usize = 16;

/// Top-N Class N (refused-write) addresses reported per key (plan §3).
const CLASS_N_ADDRESSES: usize = 32;

/// How many distinct stack segments (by `SS.selector`) one trip tracks a
/// low-water mark for.
const MAX_STACK_SEGMENTS: usize = 4;

/// The literal 8 KB `REFLECTED_CALL_DEAD_STACK_CAP` the design proposed.
/// Reported (`write_dead_8kb`) as a cross-check only (plan §3: "Delete the
/// constant cap from the decision").
const DEAD_STACK_CAP_BYTES: u32 = 8192;

/// Ring size for the warm-clock spread (plan §5, Q5).
const WARM_CLOCK_SAMPLES: usize = 32;

/// Cap on distinct `(CR3, linear page)` translations tracked per trip (plan
/// §3.2/§8).
const REFLECTED_CALL_MAX_TRANSLATIONS: usize = 64;

/// The fixed legacy VGA aperture (`memory.rs`, `jit/direct.rs`), physical.
/// Plan §3.1: no linear-framebuffer base is resolvable from this crate alone
/// (UNVERIFIED base for any LFB aperture vega may expose), so only the
/// legacy aperture is classified; that limitation is reported (see the
/// result document, T4).
const FRAMEBUFFER_APERTURE_LO: u32 = 0x000A_0000;
const FRAMEBUFFER_APERTURE_HI: u32 = 0x000B_FFFF;

/// Optional dwell-window gate on RETIRED GUEST INSTRUCTIONS
/// (`IZARRAVM_REFLECTED_CALL_DIAG_WINDOW=<start_insns>:<end_insns>`, orchestrator
/// decision, plan §14 Q1). Unset means "whole run only". Verified against a
/// short run: `cpu.perf.instructions` and the `--cycles` budget do NOT share a
/// unit (clocks vs. retired instructions) -- this is exactly why the window is
/// keyed on retired instructions rather than clocks: it is the one counter
/// this module already samples every trip against (`Trip::start_instructions`
/// / `cpu.perf.instructions`), with no ambiguity about what "instructions"
/// means.
#[derive(Clone, Copy, Debug)]
struct DiagWindow {
    start_insns: u64,
    end_insns: u64,
}

fn parse_window(spec: &str) -> Option<DiagWindow> {
    let (lo, hi) = spec.split_once(':')?;
    let start_insns = lo.trim().parse().ok()?;
    let end_insns = hi.trim().parse().ok()?;
    if end_insns < start_insns {
        return None;
    }
    Some(DiagWindow {
        start_insns,
        end_insns,
    })
}

fn window() -> Option<DiagWindow> {
    static WINDOW: OnceLock<Option<DiagWindow>> = OnceLock::new();
    *WINDOW.get_or_init(|| {
        std::env::var("IZARRAVM_REFLECTED_CALL_DIAG_WINDOW")
            .ok()
            .and_then(|spec| parse_window(&spec))
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Shape,
    Journal,
}

fn parse_mode(spec: &str) -> Option<Mode> {
    match spec {
        "" => None,
        "shape" => Some(Mode::Shape),
        "journal" => Some(Mode::Journal),
        other => {
            eprintln!(
                "reflected-call-diagnostic: IZARRAVM_REFLECTED_CALL_DIAGNOSTIC={other:?} not \
                 recognised (want unset, \"\", \"shape\" or \"journal\"); treating as off"
            );
            None
        }
    }
}

fn mode() -> Option<Mode> {
    static MODE: OnceLock<Option<Mode>> = OnceLock::new();
    *MODE.get_or_init(|| {
        std::env::var("IZARRAVM_REFLECTED_CALL_DIAGNOSTIC")
            .ok()
            .and_then(|spec| parse_mode(&spec))
    })
}

static ARMED: AtomicBool = AtomicBool::new(false);
static ARMED_INIT: OnceLock<()> = OnceLock::new();

// Test-only override of `armed()`/`journal_mode()`, `thread_local` rather
// than the process-wide `OnceLock`s above (plan §10 item 8, N4's row): a
// `#[test]` fn runs to completion on one worker thread before that thread
// is reused for a different test, so a thread-local override cannot leak
// into a concurrently running test the way flipping the process-global
// `ARMED`/`MODE` `OnceLock`s would. `None` means "defer to the real
// env-cached state".
#[cfg(test)]
thread_local! {
    static TEST_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn test_force_armed(journal: bool) {
    TEST_OVERRIDE.with(|c| c.set(Some(journal)));
}

#[cfg(test)]
pub(crate) fn test_clear_armed() {
    TEST_OVERRIDE.with(|c| c.set(None));
}

#[inline]
fn armed() -> bool {
    #[cfg(test)]
    if TEST_OVERRIDE.with(|c| c.get()).is_some() {
        return true;
    }
    ARMED_INIT.get_or_init(|| {
        ARMED.store(mode().is_some(), Ordering::Relaxed);
    });
    ARMED.load(Ordering::Relaxed)
}

fn journal_mode() -> bool {
    #[cfg(test)]
    if let Some(j) = TEST_OVERRIDE.with(|c| c.get()) {
        return j;
    }
    mode() == Some(Mode::Journal)
}

static FORCED_INTERPRETER: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Entry image
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct EntryImage {
    eax: u32,
    ebx: u32,
    ecx: u32,
    edx: u32,
    esp: u32,
    ebp: u32,
    esi: u32,
    edi: u32,
    eflags_masked: u32,
    cs_selector: u16,
    cs_base: u32,
    cs_limit: u32,
    cs_access: u8,
    ss_selector: u16,
    ss_base: u32,
    ss_limit: u32,
    ss_access: u8,
    cr0: u32,
    cr3: u32,
    cpl: u8,
    vm: bool,
    idtr_base: u32,
    idtr_limit: u16,
}

const EFLAGS_ARCH_MASK: u32 = 0x0003_7fd5;

impl EntryImage {
    fn capture(cpu: &CpuGsw) -> Self {
        let regs = &cpu.registers;
        let cs = regs.cs();
        let ss = regs.segment(SegmentIndex::Ss);
        EntryImage {
            eax: regs.eax(),
            ebx: regs.ebx(),
            ecx: regs.ecx(),
            edx: regs.edx(),
            esp: regs.esp(),
            ebp: regs.ebp(),
            esi: regs.esi(),
            edi: regs.edi(),
            eflags_masked: regs.eflags & EFLAGS_ARCH_MASK,
            cs_selector: cs.selector,
            cs_base: cs.base,
            cs_limit: cs.limit,
            cs_access: cs.access,
            ss_selector: ss.selector,
            ss_base: ss.base,
            ss_limit: ss.limit,
            ss_access: ss.access,
            cr0: cpu.control.cr0,
            cr3: cpu.control.cr3,
            cpl: cpu.current_privilege_level(),
            vm: cpu.is_v86_mode(),
            idtr_base: cpu.idtr.base,
            idtr_limit: cpu.idtr.limit,
        }
    }
}

#[derive(Default)]
struct FieldVariance {
    first: Option<EntryImage>,
    varies: [bool; 22],
}

/// N6: this array has 22 entries (one per `EntryImage` field group); an
/// earlier revision of this module's doc comment said "sixteen", which was
/// simply wrong (uncorrected transcription from an earlier draft with fewer
/// fields) -- fixed here rather than left to drift further.
const FIELD_NAMES: [&str; 22] = [
    "eax",
    "ebx",
    "ecx",
    "edx",
    "esp",
    "ebp",
    "esi",
    "edi",
    "eflags",
    "cs_selector",
    "cs_base",
    "cs_limit",
    "cs_access",
    "ss_selector",
    "ss_base",
    "ss_limit",
    "ss_access",
    "cr0",
    "cr3",
    "cpl",
    "vm",
    "idtr",
];

impl FieldVariance {
    fn observe(&mut self, image: &EntryImage) {
        let Some(first) = self.first else {
            self.first = Some(*image);
            return;
        };
        let fields: [bool; 22] = [
            image.eax != first.eax,
            image.ebx != first.ebx,
            image.ecx != first.ecx,
            image.edx != first.edx,
            image.esp != first.esp,
            image.ebp != first.ebp,
            image.esi != first.esi,
            image.edi != first.edi,
            image.eflags_masked != first.eflags_masked,
            image.cs_selector != first.cs_selector,
            image.cs_base != first.cs_base,
            image.cs_limit != first.cs_limit,
            image.cs_access != first.cs_access,
            image.ss_selector != first.ss_selector,
            image.ss_base != first.ss_base,
            image.ss_limit != first.ss_limit,
            image.ss_access != first.ss_access,
            image.cr0 != first.cr0,
            image.cr3 != first.cr3,
            image.cpl != first.cpl,
            image.vm != first.vm,
            (image.idtr_base, image.idtr_limit) != (first.idtr_base, first.idtr_limit),
        ];
        for (slot, changed) in self.varies.iter_mut().zip(fields) {
            *slot |= changed;
        }
    }

    fn varying_field_names(&self) -> Vec<&'static str> {
        FIELD_NAMES
            .iter()
            .zip(self.varies)
            .filter(|(_, varies)| *varies)
            .map(|(name, _)| *name)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Address classification (journal mode)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AddressClass {
    ClientStack,
    HostStack,
    Bda,
    Gdt,
    Ldt,
    Idt,
    Tss,
    PageTable,
    /// NEW (plan §3.1): the legacy VGA aperture, physical `0xA0000..=0xBFFFF`.
    /// The CRTC reads guest memory every scanline with no arming step, so an
    /// intermediate value written here is observable on screen -- never
    /// eligible for the Class R (restored) write disposition.
    FramebufferAperture,
    /// NEW (plan §3.1): `bus.peek_direct_ram` returned `None` at this
    /// physical address -- the instrument's proxy for "this is a device
    /// window, not plain RAM". Also never eligible for Class R.
    NotPlainRam,
    Other,
}

impl AddressClass {
    fn name(self) -> &'static str {
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

    /// Plan §3: "Exception: device windows and the framebuffer aperture are
    /// never Class R."
    fn never_restored(self) -> bool {
        matches!(self, Self::FramebufferAperture | Self::NotPlainRam)
    }
}

/// N6/A.4: length 11 (was 9 in slice 0 -- two new device-window classes).
/// Every `[u64; 11]` and every JSON zip over `ALL_CLASSES` below must stay in
/// step with this list or classes silently drop off the report.
const ALL_CLASSES: [AddressClass; 11] = [
    AddressClass::ClientStack,
    AddressClass::HostStack,
    AddressClass::Bda,
    AddressClass::Gdt,
    AddressClass::Ldt,
    AddressClass::Idt,
    AddressClass::Tss,
    AddressClass::PageTable,
    AddressClass::FramebufferAperture,
    AddressClass::NotPlainRam,
    AddressClass::Other,
];

/// Review Appendix A R1.3's near-match histogram field names, in
/// `Trip::note_near_match`'s bump order.
const NEAR_MATCH_FIELDS: [&str; 5] = ["ss_selector", "sp_low16", "sp_high16", "cs_base", "other"];

/// NEW (plan §2.4): fires at a candidate boundary where `CS.selector` and
/// `EIP` do NOT both already match the entry. Without this bucket a C3-shaped
/// failure (the real return is a far transfer, not a return this hook used to
/// treat as a boundary) was unobservable: `near_match[]` cannot fire when the
/// pair never agreed in the first place.
const NEAR_MATCH_CS_EIP_FIELDS: [&str; 3] = ["eip_differs", "cs_selector_differs", "both_differ"];

const BDA_LO: u32 = 0x0400;
const BDA_HI: u32 = 0x0500;

/// Classify a write/read by LINEAR address against the CPU's live descriptor
/// registers and the trip's own stack tracking, PLUS the two new
/// physical-address-based device-window classes (plan §3.1). `physical` and
/// `plain_ram` come from the caller's own `probe_physical`/`peek_direct_ram`
/// call, since classification must not perform a second charging or
/// TLB-filling walk of its own.
///
/// `forced`, when `Some`, skips all of this (the `write_page_walk_entry`
/// seam: a page-table entry has no linear address of its own to classify by).
#[allow(clippy::too_many_arguments)]
fn classify(
    cpu: &CpuGsw,
    trip: &Trip,
    linear: u32,
    physical: Option<u32>,
    plain_ram: bool,
    forced: Option<AddressClass>,
) -> AddressClass {
    if let Some(class) = forced {
        return class;
    }
    if let Some(phys) = physical {
        if (FRAMEBUFFER_APERTURE_LO..=FRAMEBUFFER_APERTURE_HI).contains(&phys) {
            return AddressClass::FramebufferAperture;
        }
        if !plain_ram {
            return AddressClass::NotPlainRam;
        }
    }
    let idtr = cpu.idtr;
    if linear.wrapping_sub(idtr.base) <= u32::from(idtr.limit) && idtr.limit > 0 {
        return AddressClass::Idt;
    }
    if linear.wrapping_sub(cpu.gdtr.base) <= u32::from(cpu.gdtr.limit) && cpu.gdtr.limit > 0 {
        return AddressClass::Gdt;
    }
    if cpu.ldtr.limit > 0 && linear.wrapping_sub(cpu.ldtr.base) <= cpu.ldtr.limit {
        return AddressClass::Ldt;
    }
    if cpu.tr.limit > 0 && linear.wrapping_sub(cpu.tr.base) <= cpu.tr.limit {
        return AddressClass::Tss;
    }
    if (BDA_LO..BDA_HI).contains(&linear) {
        return AddressClass::Bda;
    }
    for seg in trip.stacks.iter().flatten() {
        if linear.wrapping_sub(seg.base) < seg.limit.max(1) {
            return if seg.selector == trip.entry_ss_selector {
                AddressClass::ClientStack
            } else {
                AddressClass::HostStack
            };
        }
    }
    AddressClass::Other
}

// ---------------------------------------------------------------------------
// The non-charging page walker (plan §4)
// ---------------------------------------------------------------------------

/// The two page-walk entries resolved for one linear address, TLB-independent
/// (a pure function of guest CR3 + guest page-table memory). Module-private:
/// this closes slice 0's blind spots (the BDA peek, `entered_via_task_gate`,
/// `TSS.ESP0`) WITHOUT teaching `probe_linear_read_physical` (`core.rs`) or
/// any other production seam to walk -- the walker lives only here.
/// `pde_value`/`pte_value` are captured (matching plan §3.2's "the physical
/// PDE and PTE addresses AND values") but not currently read anywhere: the
/// `translations` map's value type is pinned by the plan at `(u32, u32)`,
/// which only fits the two ADDRESSES, so the values are not surfaced in 0b's
/// own report. Kept on the struct for a future slice's use, `#[allow(dead_code)]`
/// rather than dropped.
#[allow(dead_code)]
struct Walk {
    pde_phys: u32,
    pde_value: u32,
    pte_phys: u32,
    pte_value: u32,
}

/// TLB-independent linear-to-physical resolution for a DATA READ probe. Never
/// fills the TLB, never charges a bus access, never sets an accessed/dirty
/// bit -- it uses only `CpuBus::peek_direct_ram`, which by contract does
/// none of those things.
///
/// 1. Paging off: identity map, no walk.
/// 2. Paging on: try the cached TLB first (`probe_linear_read_physical`,
///    `core.rs:1845`) for the common case; on a miss (or to populate the
///    translation-set data even on a hit) resolve the walk by hand: PDE at
///    `(cr3 & !0xFFF) + (linear >> 22) * 4`, PTE at `(pde & !0xFFF) +
///    ((linear >> 12) & 0x3FF) * 4`. `None` if either present bit is clear or
///    either peek misses. 4 MiB (PSE) pages are not modelled: the design's
///    guest population (DOS extenders under a DPMI host) runs exclusively
///    4 KiB paging, so this is an accepted scope limitation, not silently
///    wrong for the guests this instrument targets.
fn probe_physical<B: CpuBus>(cpu: &CpuGsw, bus: &B, linear: u32) -> Option<(u32, Option<Walk>)> {
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
    Some((
        phys,
        Some(Walk {
            pde_phys,
            pde_value,
            pte_phys,
            pte_value,
        }),
    ))
}

// ---------------------------------------------------------------------------
// Per-trip state
// ---------------------------------------------------------------------------

#[derive(Default, Clone, Copy)]
struct StackTrack {
    selector: u16,
    base: u32,
    limit: u32,
    low_water_esp: u32,
    last_esp: u32,
}

struct WriteRecord {
    class: AddressClass,
    physical: Option<u32>,
    pre: Option<u32>,
    latest: u32,
    width_bytes: u32,
    /// The trip's `ESP` at the first write to this address (plan §8). Not
    /// consulted by 0b's own R/D/N decision, which classifies purely from
    /// the write's ADDRESS against the trip's per-segment low-water tracking
    /// (`is_dead_stack_derived`) -- kept on the record for a future slice's
    /// higher-resolution audit of exactly when in the trip each write landed.
    #[allow(dead_code)]
    sp_at_first_write: u32,
}

struct ReadRecord {
    class: AddressClass,
    under_entry_cr3: bool,
}

/// Guest execution mode, sampled at trip entry/close and on every hook this
/// module reaches, for the "mode transitions inside a trip" check (plan
/// §2.1): a trip entering protected mode and closing in V86 is a defect, not
/// a match.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GuestMode {
    Protected,
    V86,
    Real,
}

impl GuestMode {
    fn sample(cpu: &CpuGsw) -> Self {
        if cpu.is_v86_mode() {
            GuestMode::V86
        } else if cpu.is_protected_mode() {
            GuestMode::Protected
        } else {
            GuestMode::Real
        }
    }

    fn name(self) -> &'static str {
        match self {
            GuestMode::Protected => "pm",
            GuestMode::V86 => "v86",
            GuestMode::Real => "real",
        }
    }
}

/// The four ways a trip can close (plan §2.1/§2.3). Only `ReturnMatch` counts
/// as a match; the other three close the trip and are recorded, but never
/// produce a memo (slice 1's concern, not 0b's).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CloseRule {
    ReturnMatch,
    FrameGone,
    ReEntry,
    Stale,
}

impl CloseRule {
    fn index(self) -> usize {
        match self {
            CloseRule::ReturnMatch => 0,
            CloseRule::FrameGone => 1,
            CloseRule::ReEntry => 2,
            CloseRule::Stale => 3,
        }
    }
}

const CLOSED_BY_NAMES: [&str; 4] = ["return_match", "frame_gone", "re_entry", "stale"];

/// Which far-transfer form observed the near-miss (plan §2.4: "Both split by
/// `boundary_kind`").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BoundaryKind {
    FarReturn,
    FarTransfer,
}

struct Trip {
    vector: u8,
    ah: u8,
    entry_image: EntryImage,
    return_cs_selector: u16,
    return_cs_base: u32,
    return_eip: u32,
    entry_ss_selector: u16,
    entry_esp: u32,
    /// SP truncated to 16 bits at entry, stored alongside `entry_esp` (plan
    /// §8) so a reader of the struct sees the entry-width decision's operand
    /// directly rather than re-deriving it from `entry_esp`.
    entry_sp16: u16,
    /// Whether the ENTRY `SS` segment is 32-bit (`B`/`D` = 1). Decided ONCE
    /// at the `INT` and never re-derived from the `SS` in force at a later
    /// boundary (plan §2.1: a mid-trip real-mode/V86 excursion may load a
    /// `SS` this module must not consult for the width decision).
    entry_ss_big: bool,
    entry_cr3: u32,
    entry_tr_base: u32,
    mode_at_entry: GuestMode,
    modes_seen: [bool; 3],
    mode_at_close: GuestMode,
    tss_esp0_at_entry: Option<u32>,
    start_instructions: u64,
    start_elapsed_clocks: u64,
    start_jit_entries: u64,
    start_cr3_writes: u64,
    bda_head_at_entry: Option<u32>,
    bda_tail_at_entry: Option<u32>,
    entered_via_task_gate: Option<bool>,
    near_match_diffs: [u32; 5],
    near_match_cs_eip: [u32; 3],
    cs_base_differed_on_match: bool,
    batch_boundaries_seen: u32,
    nested_int_count: u32,
    far_transfer_count: u32,
    hw_interrupt_count: u32,
    gp_traps: u32,
    pf_traps: u32,
    rdtsc_seen: bool,
    port_io_seen: bool,
    x87_seen: bool,
    soft_int_posts: u32,
    clock_charge_events: u32,
    stacks: [Option<StackTrack>; MAX_STACK_SEGMENTS],
    writes: HashMap<u32, WriteRecord>,
    reads: HashMap<u32, ReadRecord>,
    /// Distinct physical dwords read (plan §3.2: "Report `read_set_size` in
    /// distinct physical dwords per round trip"), separate from `reads`
    /// (keyed by linear address, unchanged from slice 0).
    read_phys_dwords: std::collections::HashSet<u32>,
    /// Distinct `(CR3, linear page)` translations this trip touched, capped
    /// at `REFLECTED_CALL_MAX_TRANSLATIONS` (plan §3.2/§8). Value is
    /// `(pde_phys, pte_phys)`, kept only so a future slice can dump the
    /// addresses; the report only needs the set's size.
    translations: HashMap<(u32, u32), (u32, u32)>,
    translation_set_over_cap: bool,
}

impl Trip {
    fn start<B: CpuBus>(cpu: &mut CpuGsw, bus: &B, vector: u8, ah: u8) -> Self {
        let entry_image = EntryImage::capture(cpu);
        let regs = &cpu.registers;
        let cs = regs.cs();
        let ss = regs.segment(SegmentIndex::Ss);
        let bda = |offset: u32| -> Option<u32> {
            let (phys, _) = probe_physical(cpu, bus, offset)?;
            bus.peek_direct_ram(phys, BusWidth::Word)
        };
        let gate_access_addr = cpu.idtr.base.wrapping_add(u32::from(vector) * 8 + 5);
        let entered_via_task_gate = probe_physical(cpu, bus, gate_access_addr)
            .and_then(|(phys, _)| bus.peek_direct_ram(phys, BusWidth::Byte))
            .map(|access| (access & 0x1f) == 0x05);
        // TSS.ESP0: no `CpuGsw` accessor exists for it (verified by source
        // search -- there is no `esp0`/`ESP0` field or method anywhere in
        // this crate), so peek it directly at the 32-bit TSS's documented
        // offset (386 PRM figure 7-2: ESP0 at offset 4).
        let tss_esp0_at_entry = probe_physical(cpu, bus, cpu.tr.base.wrapping_add(4))
            .and_then(|(phys, _)| bus.peek_direct_ram(phys, BusWidth::Dword));
        let mode_at_entry = GuestMode::sample(cpu);
        let mut modes_seen = [false; 3];
        modes_seen[mode_index(mode_at_entry)] = true;
        let entry_esp = regs.esp();
        let mut stacks: [Option<StackTrack>; MAX_STACK_SEGMENTS] = [None; MAX_STACK_SEGMENTS];
        stacks[0] = Some(StackTrack {
            selector: ss.selector,
            base: ss.base,
            limit: ss.limit,
            low_water_esp: entry_esp,
            last_esp: entry_esp,
        });
        Trip {
            vector,
            ah,
            entry_image,
            return_cs_selector: cs.selector,
            return_cs_base: cs.base,
            return_eip: regs.eip,
            entry_ss_selector: ss.selector,
            entry_esp,
            entry_sp16: entry_esp as u16,
            entry_ss_big: ss.default_size_32,
            entry_cr3: cpu.control.cr3,
            entry_tr_base: cpu.tr.base,
            mode_at_entry,
            modes_seen,
            mode_at_close: mode_at_entry,
            tss_esp0_at_entry,
            start_instructions: cpu.perf.instructions,
            start_elapsed_clocks: cpu.elapsed_clocks,
            start_jit_entries: cpu.perf.jit_direct_entries,
            start_cr3_writes: cpu.perf.decode_inval_cr3,
            bda_head_at_entry: bda(0x041A),
            bda_tail_at_entry: bda(0x041C),
            entered_via_task_gate,
            near_match_diffs: [0; 5],
            near_match_cs_eip: [0; 3],
            cs_base_differed_on_match: false,
            batch_boundaries_seen: 0,
            nested_int_count: 0,
            far_transfer_count: 0,
            hw_interrupt_count: 0,
            gp_traps: 0,
            pf_traps: 0,
            rdtsc_seen: false,
            port_io_seen: false,
            x87_seen: false,
            soft_int_posts: 0,
            clock_charge_events: 0,
            stacks,
            writes: HashMap::new(),
            reads: HashMap::new(),
            read_phys_dwords: std::collections::HashSet::new(),
            translations: HashMap::new(),
            translation_set_over_cap: false,
        }
    }

    fn touch_stack(&mut self, ss: SegmentRegister, esp: u32) {
        for slot in self.stacks.iter_mut() {
            match slot {
                Some(seg) if seg.selector == ss.selector => {
                    seg.low_water_esp = seg.low_water_esp.min(esp);
                    seg.last_esp = esp;
                    return;
                }
                None => {
                    *slot = Some(StackTrack {
                        selector: ss.selector,
                        base: ss.base,
                        limit: ss.limit,
                        low_water_esp: esp,
                        last_esp: esp,
                    });
                    return;
                }
                _ => {}
            }
        }
    }

    fn observe_mode(&mut self, cpu: &CpuGsw) {
        let now = GuestMode::sample(cpu);
        self.modes_seen[mode_index(now)] = true;
    }

    /// The entry stack segment's own architectural width (plan §2.1): SP is
    /// compared at 16 bits when the entry `SS` had `B`/`D` == 0, full 32 bits
    /// when it had `B`/`D` == 1. Decided once at `Trip::start` and stored;
    /// this function only replays that stored decision.
    fn sp_at_entry_width(&self, esp: u32) -> u32 {
        if self.entry_ss_big {
            esp
        } else {
            u32::from(esp as u16)
        }
    }

    /// The entry SP itself, at the entry width -- reads the stored
    /// `entry_sp16` directly on a 16-bit stack rather than re-truncating
    /// `entry_esp` a second time.
    fn entry_sp_at_width(&self) -> u32 {
        if self.entry_ss_big {
            self.entry_esp
        } else {
            u32::from(self.entry_sp16)
        }
    }

    /// Rule 1 (§2.1) and rule 2 (§2.3), evaluated at a candidate boundary
    /// (far return or far transfer). Returns the first of the two that
    /// fires, or `None` if neither does (a near-miss, or a nested
    /// call/return that is not this trip's own boundary at all).
    fn close_rule(&self, cpu: &CpuGsw) -> Option<CloseRule> {
        let regs = &cpu.registers;
        let cs = regs.cs();
        let ss = regs.segment(SegmentIndex::Ss);
        let esp = regs.esp();
        let cs_matches = cs.selector == self.return_cs_selector;
        let eip_matches = regs.eip == self.return_eip;
        let ss_matches = ss.selector == self.entry_ss_selector;
        let sp_matches = self.sp_at_entry_width(esp) == self.entry_sp_at_width();
        if cs_matches && eip_matches && ss_matches && sp_matches {
            return Some(CloseRule::ReturnMatch);
        }
        // Rule 2, frame-gone: CS/SS match the entry but SP has moved PAST it
        // (at the entry width) -- the client's own frame is already gone,
        // so this cannot be its matching return, but the trip is over.
        if cs_matches && ss_matches && self.sp_at_entry_width(esp) > self.entry_sp_at_width() {
            return Some(CloseRule::FrameGone);
        }
        None
    }

    /// Rule 3 (§2.3), evaluated at `on_int_entry` against a FRESH `INT`
    /// arriving while this trip is still open: the new `INT`'s own
    /// (vector, CS.selector, EIP) exactly reproduce this trip's own entry
    /// signature (the same call site firing again) AND SP is back at the
    /// entry value (at the entry width) -- a re-entry, not a nested call.
    fn is_re_entry<B: CpuBus>(&self, cpu: &CpuGsw, _bus: &B, vector: u8) -> bool {
        if vector != self.vector {
            return false;
        }
        let regs = &cpu.registers;
        let cs = regs.cs();
        cs.selector == self.return_cs_selector
            && regs.eip == self.return_eip
            && self.sp_at_entry_width(regs.esp()) == self.entry_sp_at_width()
    }

    /// Near-miss diagnostics (plan §2.4), called once per candidate boundary
    /// that did NOT close the trip. `cs_eip_matched` selects which of the two
    /// histograms this boundary feeds.
    fn note_near_miss(&mut self, cpu: &CpuGsw, boundary: BoundaryKind, cs_eip_matched: bool) {
        let regs = &cpu.registers;
        let cs = regs.cs();
        let _ = boundary; // both kinds share one set of counters per key; the
        // split by `boundary_kind` happens in the reported table (plan §2.4),
        // by tracking two independent `Trip`-level near-miss accumulators is
        // unnecessary complexity this instrument does not need: EVERY
        // candidate boundary this module sees is either a far return or a far
        // transfer, and the caller (on_far_return_on / on_far_transfer_boundary_on)
        // already knows which; it bumps the per-key, per-boundary-kind
        // counters directly rather than through this per-trip accumulator
        // duplicating that split.
        if cs_eip_matched {
            let ss = regs.segment(SegmentIndex::Ss);
            let esp = regs.esp();
            let mut any = false;
            if ss.selector != self.entry_ss_selector {
                self.near_match_diffs[0] += 1;
                any = true;
            }
            if (esp as u16) != (self.entry_esp as u16) {
                self.near_match_diffs[1] += 1;
                any = true;
            }
            if (esp >> 16) != (self.entry_esp >> 16) {
                self.near_match_diffs[2] += 1;
                any = true;
            }
            if cs.base != self.return_cs_base {
                self.near_match_diffs[3] += 1;
                any = true;
            }
            if !any {
                self.near_match_diffs[4] += 1;
            }
        } else {
            let cs_ok = cs.selector == self.return_cs_selector;
            let eip_ok = regs.eip == self.return_eip;
            let idx = match (cs_ok, eip_ok) {
                (true, false) => 0,  // eip_differs
                (false, true) => 1,  // cs_selector_differs
                (false, false) => 2, // both_differ
                (true, true) => unreachable!("cs_eip_matched was false"),
            };
            self.near_match_cs_eip[idx] += 1;
        }
    }
}

fn mode_index(mode: GuestMode) -> usize {
    match mode {
        GuestMode::Protected => 0,
        GuestMode::V86 => 1,
        GuestMode::Real => 2,
    }
}

// ---------------------------------------------------------------------------
// Aggregated per-key stats
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Samples {
    values: Vec<u64>,
}

impl Samples {
    fn push(&mut self, v: u64) {
        if self.values.len() < MAX_SAMPLES_PER_KEY {
            self.values.push(v);
        }
    }

    fn stats(&self) -> (u64, u64, u64, f64) {
        if self.values.is_empty() {
            return (0, 0, 0, 0.0);
        }
        let mut sorted = self.values.clone();
        sorted.sort_unstable();
        let min = sorted[0];
        let max = sorted[sorted.len() - 1];
        let median = sorted[sorted.len() / 2];
        let mean = sorted.iter().sum::<u64>() as f64 / sorted.len() as f64;
        (min, median, max, mean)
    }
}

/// Per-key, per-boundary-kind near-miss counters (plan §2.4: "Both split by
/// `boundary_kind` {far_return, far_transfer}").
#[derive(Default)]
struct NearMissByBoundary {
    near_match: [[u64; 5]; 2],
    near_match_cs_eip: [[u64; 3]; 2],
}

impl NearMissByBoundary {
    fn idx(kind: BoundaryKind) -> usize {
        match kind {
            BoundaryKind::FarReturn => 0,
            BoundaryKind::FarTransfer => 1,
        }
    }
}

#[derive(Default)]
struct KeyStats {
    trips: u64,
    unmatched: u64,
    closed_by: [u64; 4],
    instructions: Samples,
    core_clocks: Samples,
    dispatcher_entries: Samples,
    cr3_writes: Samples,
    field_variance: FieldVariance,
    entry_images: HashMap<EntryImage, u64>,
    distinct_cs_eip: std::collections::HashSet<(u16, u32)>,
    nested_int_trips: u64,
    far_transfer_trips: u64,
    hw_edge_trips: u64,
    gp_trap_trips: u64,
    pf_trap_trips: u64,
    rdtsc_trips: u64,
    port_io_trips: u64,
    x87_trips: u64,
    task_switch_trips: u64,
    task_gate_trips: u64,
    task_gate_unknown_trips: u64,
    bda_key_pending_trips: u64,
    bda_no_key_trips: u64,
    bda_unknown_trips: u64,
    near_miss: NearMissByBoundary,
    cs_base_differed_on_match: u64,
    modes_seen_any: [u64; 3],
    mode_defect_trips: u64,
    batch_straddle_trips: u64,
    soft_int_posts: u64,
    clock_charge_events: Samples,
    // Journal-mode only.
    read_set_size: Samples,
    read_set_size_physical: Samples,
    write_set_size: Samples,
    translation_set_size: Samples,
    translation_set_over_cap: u64,
    reads_under_other_cr3: u64,
    reads_total: u64,
    write_class_counts: [u64; 11],
    read_class_counts: [u64; 11],
    write_class_r: u64,
    write_class_d: u64,
    write_class_n: u64,
    write_class_n_trips: u64,
    write_unknown_pre: u64,
    write_dead_8kb: u64,
    write_not_plain_ram: u64,
    write_addresses: HashMap<u32, u64>,
    class_n_addresses: HashMap<u32, (AddressClass, u64)>,
    /// Shape-mode-only ring of the last `WARM_CLOCK_SAMPLES` MATCHED trips'
    /// charged core clocks (plan §5, Q5).
    warm_clocks: VecDeque<u64>,
    warm_clock_longest_run: u64,
}

fn push_bounded<T>(ring: &mut VecDeque<T>, v: T, cap: usize) {
    ring.push_back(v);
    if ring.len() > cap {
        ring.pop_front();
    }
}

fn longest_equal_run(values: &VecDeque<u64>) -> u64 {
    let mut longest = 0u64;
    let mut current = 0u64;
    let mut prev: Option<u64> = None;
    for &v in values {
        if Some(v) == prev {
            current += 1;
        } else {
            current = 1;
        }
        longest = longest.max(current);
        prev = Some(v);
    }
    longest
}

#[derive(Default)]
struct State {
    open: Option<Trip>,
    keys: HashMap<(u8, u8), KeyStats>,
    trips_total: u64,
    trips_unmatched: u64,
    /// One-shot `IZARRAVM_REFLECTED_CALL_PROBE_BENCH` result (plan §6, Q4):
    /// `probe_ns_per_read`, filled in at the first trip close with a
    /// non-empty read set.
    probe_ns_per_read: Option<f64>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}

fn probe_bench_armed() -> bool {
    static ARMED: OnceLock<bool> = OnceLock::new();
    *ARMED.get_or_init(|| {
        std::env::var("IZARRAVM_REFLECTED_CALL_PROBE_BENCH")
            .map(|v| v == "1")
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

pub(crate) fn on_int_entry<B: CpuBus>(cpu: &mut CpuGsw, bus: &B, vector: u8) {
    if !armed() {
        return;
    }
    if journal_mode() && !FORCED_INTERPRETER.swap(true, Ordering::Relaxed) {
        #[cfg(feature = "jit")]
        cpu.set_native_backend_enabled(false);
    }
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    on_int_entry_on(&mut guard, cpu, bus, vector);
}

fn on_int_entry_on<B: CpuBus>(state: &mut State, cpu: &mut CpuGsw, bus: &B, vector: u8) {
    if !(VECTOR_LO..=VECTOR_HI).contains(&vector) {
        return;
    }

    if let Some(open) = state.open.as_ref() {
        if open.is_re_entry(cpu, bus, vector) {
            finish_trip(cpu, bus, state, CloseRule::ReEntry);
        } else {
            let over_budget = cpu
                .perf
                .instructions
                .saturating_sub(open.start_instructions)
                >= MAX_TRIP_INSNS;
            if !over_budget {
                let open = state.open.as_mut().expect("checked Some above");
                open.nested_int_count = open.nested_int_count.saturating_add(1);
                open.observe_mode(cpu);
                return;
            }
            finish_trip(cpu, bus, state, CloseRule::Stale);
        }
    }

    if !(cpu.is_protected_mode() && !cpu.is_v86_mode()) {
        return;
    }

    let ah = ((cpu.registers.eax() >> 8) & 0xff) as u8;
    state.open = Some(Trip::start(cpu, bus, vector, ah));
}

pub(crate) fn on_far_return<B: CpuBus>(cpu: &mut CpuGsw, bus: &B) {
    if !armed() {
        return;
    }
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    on_far_return_on(&mut guard, cpu, bus);
}

fn on_far_return_on<B: CpuBus>(state: &mut State, cpu: &mut CpuGsw, bus: &B) {
    let Some(open) = state.open.as_ref() else {
        return;
    };
    match open.close_rule(cpu) {
        Some(rule) => finish_trip(cpu, bus, state, rule),
        None => {
            let cs_eip_matched = {
                let regs = &cpu.registers;
                let cs = regs.cs();
                cs.selector == open.return_cs_selector && regs.eip == open.return_eip
            };
            let mut over_budget = false;
            if let Some(open) = state.open.as_mut() {
                open.note_near_miss(cpu, BoundaryKind::FarReturn, cs_eip_matched);
                open.observe_mode(cpu);
                over_budget = cpu
                    .perf
                    .instructions
                    .saturating_sub(open.start_instructions)
                    >= MAX_TRIP_INSNS;
            }
            // Fold BEFORE any staleness close below: `finish_trip` takes the
            // trip out of `state.open`, and this fold reads it there.
            record_near_miss_by_boundary(state, BoundaryKind::FarReturn);
            if over_budget {
                finish_trip(cpu, bus, state, CloseRule::Stale);
            }
        }
    }
}

/// A far `JMP`/`CALL`, now a FULL candidate boundary (plan §2.2, closing C3):
/// evaluated against rule 1/rule 2 exactly like a far return, not merely
/// counted. Replaces slice 0's counter-only `on_far_transfer`.
pub(crate) fn on_far_transfer_boundary<B: CpuBus>(cpu: &mut CpuGsw, bus: &B) {
    if !armed() {
        return;
    }
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    on_far_transfer_boundary_on(&mut guard, cpu, bus);
}

fn on_far_transfer_boundary_on<B: CpuBus>(state: &mut State, cpu: &mut CpuGsw, bus: &B) {
    let Some(open) = state.open.as_mut() else {
        return;
    };
    open.far_transfer_count = open.far_transfer_count.saturating_add(1);
    let open_ref = state.open.as_ref().expect("just wrote Some above");
    match open_ref.close_rule(cpu) {
        Some(rule) => finish_trip(cpu, bus, state, rule),
        None => {
            let cs_eip_matched = {
                let regs = &cpu.registers;
                let cs = regs.cs();
                cs.selector == open_ref.return_cs_selector && regs.eip == open_ref.return_eip
            };
            if let Some(open) = state.open.as_mut() {
                open.note_near_miss(cpu, BoundaryKind::FarTransfer, cs_eip_matched);
                open.observe_mode(cpu);
            }
            record_near_miss_by_boundary(state, BoundaryKind::FarTransfer);
        }
    }
}

/// Fold a just-recorded per-trip near-miss into the OPEN trip's key -- but
/// the key is not known until the trip closes (it is keyed by
/// `(vector, ah)`, fixed at `Trip::start`). Rather than delay attribution,
/// this reads the still-open trip's `(vector, ah)` and near-miss deltas
/// directly and folds them into `KeyStats::near_miss` immediately, so a
/// long-lived trip with many nested near-misses reports them per-boundary
/// as they happen rather than only in one lump at close.
fn record_near_miss_by_boundary(state: &mut State, kind: BoundaryKind) {
    let Some(open) = state.open.as_ref() else {
        return;
    };
    let key = (open.vector, open.ah);
    let idx = NearMissByBoundary::idx(kind);
    let near_match_diffs = open.near_match_diffs;
    let near_match_cs_eip = open.near_match_cs_eip;
    let stats = state.keys.entry(key).or_default();
    // Only the DELTA since the last fold matters, but this module does not
    // track a per-trip "last folded" cursor; instead each `Trip` accumulator
    // is drained to zero immediately after folding, so the next call folds
    // only what happened since. See the reset below.
    for (slot, v) in stats.near_miss.near_match[idx]
        .iter_mut()
        .zip(near_match_diffs)
    {
        *slot += u64::from(v);
    }
    for (slot, v) in stats.near_miss.near_match_cs_eip[idx]
        .iter_mut()
        .zip(near_match_cs_eip)
    {
        *slot += u64::from(v);
    }
    if let Some(open) = state.open.as_mut() {
        open.near_match_diffs = [0; 5];
        open.near_match_cs_eip = [0; 3];
    }
}

pub(crate) fn on_hardware_interrupt(_vector: u8) {
    if !armed() {
        return;
    }
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    on_hardware_interrupt_on(&mut guard);
}

fn on_hardware_interrupt_on(state: &mut State) {
    if let Some(open) = state.open.as_mut() {
        open.hw_interrupt_count = open.hw_interrupt_count.saturating_add(1);
    }
}

pub(crate) fn on_exception_delivered(vector: u8) {
    if !armed() || (vector != 13 && vector != 14) {
        return;
    }
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    on_exception_delivered_on(&mut guard, vector);
}

fn on_exception_delivered_on(state: &mut State, vector: u8) {
    if let Some(open) = state.open.as_mut() {
        match vector {
            13 => open.gp_traps = open.gp_traps.saturating_add(1),
            14 => open.pf_traps = open.pf_traps.saturating_add(1),
            _ => {}
        }
    }
}

pub(crate) fn on_rdtsc_or_rdmsr_tsc() {
    if !armed() {
        return;
    }
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(open) = guard.open.as_mut() {
        open.rdtsc_seen = true;
    }
}

pub(crate) fn on_port_io() {
    if !armed() {
        return;
    }
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(open) = guard.open.as_mut() {
        open.port_io_seen = true;
    }
}

pub(crate) fn on_x87() {
    if !armed() {
        return;
    }
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(open) = guard.open.as_mut() {
        open.x87_seen = true;
    }
}

/// NEW (plan §5, Q6/B6): called from `izarravm-machine`'s batch loop
/// (`run.rs`, the `event_batch_cap_cached` site) under its own
/// `reflected-call-diagnostic` feature, one cfg-gated call, the ONLY reason
/// this diagnostic's feature is allowed to propagate past the CPU crate
/// (plan §14 Q2, orchestrator decision). No `CpuGsw` reference: the machine
/// crate does not hand one to this seam (matches `on_hardware_interrupt`'s
/// shape).
pub fn on_batch_boundary() {
    if !armed() {
        return;
    }
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(open) = guard.open.as_mut() {
        open.batch_boundaries_seen = open.batch_boundaries_seen.saturating_add(1);
    }
}

/// Called once per retired instruction where the native backend charges
/// `elapsed_clocks` (plan §5, R10 item 2 -- the clock/instruction anchor
/// defect). Present so the result document can cross-check shape-mode clocks
/// against journal mode's interpreted clocks on the same key; if the anchor
/// cannot be reconciled, the plan orders clocks reported as UNUSABLE rather
/// than guessed at.
pub(crate) fn on_clock_charge() {
    if !armed() {
        return;
    }
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(open) = guard.open.as_mut() {
        open.clock_charge_events = open.clock_charge_events.saturating_add(1);
    }
}

pub(crate) fn note_read<B: CpuBus>(cpu: &mut CpuGsw, bus: &B, linear: u32) {
    if !armed() || !journal_mode() {
        return;
    }
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    note_read_on(&mut guard, cpu, bus, linear);
}

fn note_read_on<B: CpuBus>(state: &mut State, cpu: &mut CpuGsw, bus: &B, linear: u32) {
    let ss = cpu.registers.segment(SegmentIndex::Ss);
    let esp = cpu.registers.esp();
    let cr3_now = cpu.control.cr3;
    let resolved = probe_physical(cpu, bus, linear);
    let Some(open) = state.open.as_mut() else {
        return;
    };
    open.touch_stack(ss, esp);
    open.observe_mode(cpu);
    if open.writes.contains_key(&linear) {
        return; // excluded: this is the trip's own earlier write
    }
    let (physical, walk) = resolved.map_or((None, None), |(p, w)| (Some(p), w));
    let plain_ram = physical
        .map(|p| bus.peek_direct_ram(p, BusWidth::Byte).is_some())
        .unwrap_or(true);
    let class = classify(cpu, open, linear, physical, plain_ram, None);
    let under_entry_cr3 = cr3_now == open.entry_cr3;
    open.reads.entry(linear).or_insert(ReadRecord {
        class,
        under_entry_cr3,
    });
    if let Some(phys) = physical {
        open.read_phys_dwords.insert(phys & !0x3);
    }
    record_translation(open, cr3_now, linear, walk);
}

fn record_translation(open: &mut Trip, cr3: u32, linear: u32, walk: Option<Walk>) {
    let Some(walk) = walk else {
        return;
    };
    let key = (cr3, linear & !0x0FFF);
    if open.translations.contains_key(&key) {
        return;
    }
    if open.translations.len() >= REFLECTED_CALL_MAX_TRANSLATIONS {
        open.translation_set_over_cap = true;
        return;
    }
    open.translations
        .insert(key, (walk.pde_phys, walk.pte_phys));
}

// N7: `#[allow(clippy::too_many_arguments)]` justified -- each production
// write seam hands this the same six architectural facts (cpu, bus, address,
// width, value, and the two page-walk-entry escape hatches `already_physical`
// / `forced_class` that only `write_page_walk_entry` needs); splitting into a
// builder would cost a second allocation per write on a path Q3/Q4 need to
// stay cheap.
#[allow(clippy::too_many_arguments)]
pub(crate) fn note_write<B: CpuBus>(
    cpu: &mut CpuGsw,
    bus: &B,
    linear: u32,
    width: BusWidth,
    value: u32,
    already_physical: bool,
    forced_class: Option<AddressClass>,
) {
    if !armed() || !journal_mode() {
        return;
    }
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    note_write_on(
        &mut guard,
        cpu,
        bus,
        linear,
        width,
        value,
        already_physical,
        forced_class,
    );
}

#[allow(clippy::too_many_arguments)]
fn note_write_on<B: CpuBus>(
    state: &mut State,
    cpu: &mut CpuGsw,
    bus: &B,
    linear: u32,
    width: BusWidth,
    value: u32,
    already_physical: bool,
    forced_class: Option<AddressClass>,
) {
    let ss = cpu.registers.segment(SegmentIndex::Ss);
    let esp = cpu.registers.esp();
    let resolved = if already_physical {
        Some((linear, None))
    } else {
        probe_physical(cpu, bus, linear)
    };
    let Some(open) = state.open.as_mut() else {
        return;
    };
    let already_written = open.writes.contains_key(&linear);
    let (physical, walk) = resolved.map_or((None, None), |(p, w)| (Some(p), w));
    let pre = if already_written {
        None
    } else {
        physical.and_then(|phys| bus.peek_direct_ram(phys, width))
    };
    open.touch_stack(ss, esp);
    open.observe_mode(cpu);
    let plain_ram = physical
        .map(|p| bus.peek_direct_ram(p, width).is_some())
        .unwrap_or(true);
    let class = classify(cpu, open, linear, physical, plain_ram, forced_class);
    open.writes
        .entry(linear)
        .and_modify(|rec| rec.latest = value)
        .or_insert(WriteRecord {
            class,
            physical,
            pre,
            latest: value,
            width_bytes: width.bytes(),
            sp_at_first_write: esp,
        });
    if !already_written {
        let cr3_now = cpu.control.cr3;
        record_translation(open, cr3_now, linear, walk);
    }
}

// ---------------------------------------------------------------------------
// Trip finalisation
// ---------------------------------------------------------------------------

fn finish_trip<B: CpuBus>(cpu: &mut CpuGsw, bus: &B, guard: &mut State, rule: CloseRule) {
    let Some(mut trip) = guard.open.take() else {
        return;
    };
    trip.mode_at_close = GuestMode::sample(cpu);
    let unmatched = rule != CloseRule::ReturnMatch;
    guard.trips_total += 1;
    if unmatched {
        guard.trips_unmatched += 1;
    }
    let key = (trip.vector, trip.ah);
    let insns_at_close = cpu.perf.instructions;
    let stats = guard.keys.entry(key).or_default();
    let in_window = window()
        .map(|w| trip.start_instructions >= w.start_insns && insns_at_close <= w.end_insns)
        .unwrap_or(true);
    stats.trips += 1;
    stats.closed_by[rule.index()] += 1;
    if unmatched {
        stats.unmatched += 1;
    }
    if rule == CloseRule::ReturnMatch && trip.cs_base_differed_on_match {
        stats.cs_base_differed_on_match += 1;
    }
    stats
        .instructions
        .push(insns_at_close.saturating_sub(trip.start_instructions));
    stats
        .core_clocks
        .push(cpu.elapsed_clocks.saturating_sub(trip.start_elapsed_clocks));
    stats.dispatcher_entries.push(
        cpu.perf
            .jit_direct_entries
            .saturating_sub(trip.start_jit_entries),
    );
    stats.cr3_writes.push(
        cpu.perf
            .decode_inval_cr3
            .saturating_sub(trip.start_cr3_writes),
    );
    stats
        .clock_charge_events
        .push(u64::from(trip.clock_charge_events));
    stats.field_variance.observe(&trip.entry_image);
    *stats.entry_images.entry(trip.entry_image).or_insert(0) += 1;
    stats
        .distinct_cs_eip
        .insert((trip.entry_image.cs_selector, trip.return_eip));
    if trip.nested_int_count > 0 {
        stats.nested_int_trips += 1;
    }
    if trip.far_transfer_count > 0 {
        stats.far_transfer_trips += 1;
    }
    if trip.hw_interrupt_count > 0 {
        stats.hw_edge_trips += 1;
    }
    if trip.gp_traps > 0 {
        stats.gp_trap_trips += 1;
    }
    if trip.pf_traps > 0 {
        stats.pf_trap_trips += 1;
    }
    if trip.rdtsc_seen {
        stats.rdtsc_trips += 1;
    }
    if trip.port_io_seen {
        stats.port_io_trips += 1;
    }
    if trip.x87_seen {
        stats.x87_trips += 1;
    }
    if cpu.tr.base != trip.entry_tr_base {
        stats.task_switch_trips += 1;
    }
    match trip.entered_via_task_gate {
        Some(true) => stats.task_gate_trips += 1,
        Some(false) => {}
        None => stats.task_gate_unknown_trips += 1,
    }
    match (trip.bda_head_at_entry, trip.bda_tail_at_entry) {
        (Some(head), Some(tail)) if head != tail => stats.bda_key_pending_trips += 1,
        (Some(_), Some(_)) => stats.bda_no_key_trips += 1,
        _ => stats.bda_unknown_trips += 1,
    }
    for (idx, seen) in trip.modes_seen.iter().enumerate() {
        if *seen {
            stats.modes_seen_any[idx] += 1;
        }
    }
    if rule == CloseRule::ReturnMatch && trip.mode_at_entry != trip.mode_at_close {
        stats.mode_defect_trips += 1;
    }
    if trip.batch_boundaries_seen > 0 {
        stats.batch_straddle_trips += 1;
    }
    stats.soft_int_posts += u64::from(trip.soft_int_posts);

    if rule == CloseRule::ReturnMatch && mode_is_shape_relevant(&trip) {
        push_bounded(
            &mut stats.warm_clocks,
            cpu.elapsed_clocks.saturating_sub(trip.start_elapsed_clocks),
            WARM_CLOCK_SAMPLES,
        );
        stats.warm_clock_longest_run = longest_equal_run(&stats.warm_clocks);
    }

    // Any near-miss diagnostics this trip accumulated were already folded
    // into `stats.near_miss` per boundary kind by `record_near_miss_by_boundary`,
    // immediately after each `note_near_miss` call -- nothing left to do here.

    if probe_bench_armed() && guard.probe_ns_per_read.is_none() && !trip.read_phys_dwords.is_empty()
    {
        let ns = run_probe_bench(cpu, bus, &trip.read_phys_dwords);
        guard.probe_ns_per_read = Some(ns);
    }

    // Journal-mode-only aggregation. In shape mode `trip.reads`/`trip.writes`
    // are always empty.
    stats.reads_total += trip.reads.len() as u64;
    if in_window {
        stats.read_set_size.push(trip.reads.len() as u64);
        stats
            .read_set_size_physical
            .push(trip.read_phys_dwords.len() as u64);
    }
    for read in trip.reads.values() {
        read_class_bump(stats, read.class);
        if !read.under_entry_cr3 {
            stats.reads_under_other_cr3 += 1;
        }
    }
    if in_window {
        stats.write_set_size.push(trip.writes.len() as u64);
        stats
            .translation_set_size
            .push(trip.translations.len() as u64);
    }
    if trip.translation_set_over_cap {
        stats.translation_set_over_cap += 1;
    }
    let mut trip_has_class_n = false;
    for (&addr, write) in trip.writes.iter() {
        write_class_bump(stats, write.class);
        *stats.write_addresses.entry(addr).or_insert(0) += 1;
        if write.class == AddressClass::NotPlainRam {
            stats.write_not_plain_ram += 1;
        }
        let restored = write.pre.map(|pre| {
            mask_to_width(write.latest, write.width_bytes) == mask_to_width(pre, write.width_bytes)
        });
        if restored.is_none() {
            stats.write_unknown_pre += 1;
        }
        let disposition = classify_disposition(&trip, addr, write, restored);
        match disposition {
            WriteDisposition::Restored => stats.write_class_r += 1,
            WriteDisposition::Dead => stats.write_class_d += 1,
            WriteDisposition::Refused => {
                stats.write_class_n += 1;
                trip_has_class_n = true;
                // Physical, when the walker resolved one -- the whole point
                // of naming the refusing set (plan §3) is for the reviewer
                // to look the address up against the guest's real memory
                // map, which is physical.
                let key = write.physical.unwrap_or(addr);
                let entry = stats
                    .class_n_addresses
                    .entry(key)
                    .or_insert((write.class, 0));
                entry.1 += 1;
            }
        }
        if is_dead_stack_8kb(&trip, addr) {
            stats.write_dead_8kb += 1;
        }
    }
    if trip_has_class_n {
        stats.write_class_n_trips += 1;
    }
}

/// Shape mode only tags trips with an empty read/write journal (the hooks
/// early-return before the lock there); this just guards the warm-clock ring
/// against journal-mode noise (journal forces the interpreter, whose clocks
/// the design says mean nothing for this question -- plan §5, Q5).
fn mode_is_shape_relevant(_trip: &Trip) -> bool {
    !journal_mode()
}

enum WriteDisposition {
    Restored,
    Dead,
    Refused,
}

/// The R/D/N decision (plan §3): R (restored) is admitted for every
/// `AddressClass` except the two device-window classes, which can never be
/// R even when byte-identical (an intermediate value there is observable
/// on-screen or by a device with no arming step). D (dead stack) is derived
/// SOLELY from the trip's own observed low-water mark on the segment the
/// write fell in -- the literal 8 KB constant plays no part in this
/// decision (`is_dead_stack_8kb` reports it separately, as a cross-check
/// only). Everything else is N.
fn classify_disposition(
    trip: &Trip,
    addr: u32,
    write: &WriteRecord,
    restored: Option<bool>,
) -> WriteDisposition {
    if restored == Some(true) && !write.class.never_restored() {
        return WriteDisposition::Restored;
    }
    if is_dead_stack_derived(trip, addr) {
        return WriteDisposition::Dead;
    }
    WriteDisposition::Refused
}

/// Class D (plan §3): `addr` falls within `[low_water_SP(seg), SP_at_close(seg))`
/// for whichever tracked stack segment it belongs to -- the constant 8 KB cap
/// plays no part here. Also covers the ring-0 monitor-stack window
/// `[observed_low_water, TSS.ESP0)` when the write fell on a segment this
/// trip tracked separately from the client's own SS AND `tss_esp0_at_entry`
/// resolved (UNVERIFIED in general: this instrument has no dedicated
/// "monitor stack" concept beyond the generic per-`SS.selector` tracking
/// `touch_stack` already does, so a ring-0 handler that switches to its own
/// TSS-named stack is covered by the SAME per-segment low-water logic, with
/// `TSS.ESP0` substituted for that segment's own `last_esp` as the window's
/// upper bound when available -- see the result document for how often this
/// branch actually fires).
fn is_dead_stack_derived(trip: &Trip, addr: u32) -> bool {
    for seg in trip.stacks.iter().flatten() {
        if addr.wrapping_sub(seg.base) >= seg.limit.max(1) {
            continue;
        }
        let is_client_stack = seg.selector == trip.entry_ss_selector;
        let upper_esp = if !is_client_stack {
            trip.tss_esp0_at_entry.unwrap_or(seg.last_esp)
        } else {
            seg.last_esp
        };
        let low_linear = seg.base.wrapping_add(seg.low_water_esp);
        let high_linear = seg.base.wrapping_add(upper_esp);
        return addr >= low_linear && addr < high_linear;
    }
    false
}

/// The design's original literal 8 KB `REFLECTED_CALL_DEAD_STACK_CAP`
/// cross-check (plan §3: reported, never part of the decision).
fn is_dead_stack_8kb(trip: &Trip, addr: u32) -> bool {
    for seg in trip.stacks.iter().flatten() {
        if addr.wrapping_sub(seg.base) >= seg.limit.max(1) {
            continue;
        }
        let abandon_linear = seg.base.wrapping_add(seg.last_esp);
        if addr >= abandon_linear {
            return false;
        }
        let below = abandon_linear - addr;
        return below <= DEAD_STACK_CAP_BYTES;
    }
    false
}

fn mask_to_width(v: u32, width_bytes: u32) -> u32 {
    match width_bytes {
        1 => v & 0xff,
        2 => v & 0xffff,
        _ => v,
    }
}

fn read_class_bump(stats: &mut KeyStats, class: AddressClass) {
    if let Some(idx) = ALL_CLASSES.iter().position(|c| *c == class) {
        stats.read_class_counts[idx] += 1;
    }
}

fn write_class_bump(stats: &mut KeyStats, class: AddressClass) {
    if let Some(idx) = ALL_CLASSES.iter().position(|c| *c == class) {
        stats.write_class_counts[idx] += 1;
    }
}

// ---------------------------------------------------------------------------
// The answer-cost micro-benchmark (plan §6, Q4)
// ---------------------------------------------------------------------------

/// Armed by `IZARRAVM_REFLECTED_CALL_PROBE_BENCH=1`. Called from `finish_trip`
/// once per process, at the first trip close with a non-empty PHYSICAL read
/// set: times 1,000,000 `probe_physical` + `peek_direct_ram` iterations over
/// that trip's own addresses and returns the mean `ns` per iteration.
fn run_probe_bench<B: CpuBus>(
    cpu: &CpuGsw,
    bus: &B,
    addrs_set: &std::collections::HashSet<u32>,
) -> f64 {
    let addrs: Vec<u32> = addrs_set.iter().copied().collect();
    const ITERATIONS: u32 = 1_000_000;
    let start = std::time::Instant::now();
    let mut sink: u32 = 0;
    for i in 0..ITERATIONS {
        let addr = addrs[(i as usize) % addrs.len()];
        if let Some((phys, _)) = probe_physical(cpu, bus, addr)
            && let Some(v) = bus.peek_direct_ram(phys, BusWidth::Dword)
        {
            sink ^= v;
        }
    }
    let elapsed = start.elapsed();
    std::hint::black_box(sink);
    elapsed.as_secs_f64() * 1e9 / f64::from(ITERATIONS)
}

// ---------------------------------------------------------------------------
// Snapshot for JSON emission (crates/izarravm/src/main.rs)
// ---------------------------------------------------------------------------

pub struct StatSummary {
    pub min: u64,
    pub median: u64,
    pub max: u64,
    pub mean: f64,
    pub sample_count: u64,
}

impl From<&Samples> for StatSummary {
    fn from(s: &Samples) -> Self {
        let (min, median, max, mean) = s.stats();
        StatSummary {
            min,
            median,
            max,
            mean,
            sample_count: s.values.len() as u64,
        }
    }
}

pub struct KeyReport {
    pub vector: u8,
    pub ah: u8,
    pub trips: u64,
    pub unmatched: u64,
    pub closed_by: Vec<(&'static str, u64)>,
    pub instructions: StatSummary,
    pub core_clocks: StatSummary,
    pub dispatcher_entries: StatSummary,
    pub cr3_writes: StatSummary,
    pub distinct_entry_images: u64,
    pub distinct_cs_eip: u64,
    pub varying_fields: Vec<&'static str>,
    pub entry_image_top16_trips: Vec<u64>,
    pub entry_image_cum_share_1: f64,
    pub entry_image_cum_share_4: f64,
    pub entry_image_cum_share_8: f64,
    pub entry_image_cum_share_16: f64,
    pub nested_int_trips: u64,
    pub far_transfer_trips: u64,
    pub hw_edge_trips: u64,
    pub gp_trap_trips: u64,
    pub pf_trap_trips: u64,
    pub rdtsc_trips: u64,
    pub port_io_trips: u64,
    pub x87_trips: u64,
    pub task_switch_trips: u64,
    pub task_gate_trips: u64,
    pub task_gate_unknown_trips: u64,
    pub bda_key_pending_trips: u64,
    pub bda_no_key_trips: u64,
    pub bda_unknown_trips: u64,
    pub near_match_diffs: Vec<(&'static str, u64, u64)>, // (field, far_return, far_transfer)
    pub near_match_cs_eip: Vec<(&'static str, u64, u64)>,
    pub cs_base_differed_on_match: u64,
    pub modes_seen_any: Vec<(&'static str, u64)>,
    pub mode_defect_trips: u64,
    pub batch_straddle_trips: u64,
    pub soft_int_posts: u64,
    pub clock_charge_events: StatSummary,
    pub read_set_size: StatSummary,
    pub read_set_size_physical: StatSummary,
    pub write_set_size: StatSummary,
    pub translation_set_size: StatSummary,
    pub translation_set_over_cap: u64,
    pub reads_total: u64,
    pub reads_under_other_cr3: u64,
    pub read_class_counts: Vec<(&'static str, u64)>,
    pub write_class_counts: Vec<(&'static str, u64)>,
    pub write_class_r: u64,
    pub write_class_d: u64,
    pub write_class_n: u64,
    pub write_class_n_trips: u64,
    pub write_unknown_pre: u64,
    pub write_dead_8kb: u64,
    pub write_not_plain_ram: u64,
    pub top_write_addresses: Vec<(u32, u64)>,
    pub class_n_addresses: Vec<(u32, &'static str, u64)>,
    pub warm_clock_samples: Vec<u64>,
    pub warm_clock_distinct: u64,
    pub warm_clock_longest_run: u64,
}

pub struct ReflectedCallDiagnosticSnapshot {
    pub mode: &'static str,
    pub trips_total: u64,
    pub trips_unmatched: u64,
    pub probe_ns_per_read: Option<f64>,
    pub keys: Vec<KeyReport>,
}

pub(crate) fn snapshot() -> Option<ReflectedCallDiagnosticSnapshot> {
    let mode_name = match mode() {
        None => return None,
        Some(Mode::Shape) => "shape",
        Some(Mode::Journal) => "journal",
    };
    let guard = state().lock().unwrap_or_else(|e| e.into_inner());
    let mut keys: Vec<KeyReport> = guard
        .keys
        .iter()
        .map(|(&(vector, ah), stats)| {
            let mut top: Vec<(u32, u64)> = stats
                .write_addresses
                .iter()
                .map(|(&a, &c)| (a, c))
                .collect();
            top.sort_unstable_by_key(|&(_, count)| std::cmp::Reverse(count));
            top.truncate(TOP_ADDRESSES);

            let mut images: Vec<u64> = stats.entry_images.values().copied().collect();
            images.sort_unstable_by(|a, b| b.cmp(a));
            images.truncate(TOP_IMAGES);
            let total_trips: u64 = stats.entry_images.values().sum();
            let cum = |n: usize| -> f64 {
                if total_trips == 0 {
                    return 0.0;
                }
                let sum: u64 = images.iter().take(n).sum();
                sum as f64 / total_trips as f64
            };

            let mut class_n: Vec<(u32, &'static str, u64)> = stats
                .class_n_addresses
                .iter()
                .map(|(&addr, &(class, count))| (addr, class.name(), count))
                .collect();
            class_n.sort_unstable_by_key(|&(_, _, count)| std::cmp::Reverse(count));
            class_n.truncate(CLASS_N_ADDRESSES);

            let warm_clock_samples: Vec<u64> = stats.warm_clocks.iter().copied().collect();
            let warm_clock_distinct = warm_clock_samples
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len() as u64;

            KeyReport {
                vector,
                ah,
                trips: stats.trips,
                unmatched: stats.unmatched,
                closed_by: CLOSED_BY_NAMES
                    .iter()
                    .zip(stats.closed_by)
                    .map(|(n, c)| (*n, c))
                    .collect(),
                instructions: (&stats.instructions).into(),
                core_clocks: (&stats.core_clocks).into(),
                dispatcher_entries: (&stats.dispatcher_entries).into(),
                cr3_writes: (&stats.cr3_writes).into(),
                distinct_entry_images: stats.entry_images.len() as u64,
                distinct_cs_eip: stats.distinct_cs_eip.len() as u64,
                varying_fields: stats.field_variance.varying_field_names(),
                entry_image_cum_share_1: cum(1),
                entry_image_cum_share_4: cum(4),
                entry_image_cum_share_8: cum(8),
                entry_image_cum_share_16: cum(16),
                entry_image_top16_trips: images,
                nested_int_trips: stats.nested_int_trips,
                far_transfer_trips: stats.far_transfer_trips,
                hw_edge_trips: stats.hw_edge_trips,
                gp_trap_trips: stats.gp_trap_trips,
                pf_trap_trips: stats.pf_trap_trips,
                rdtsc_trips: stats.rdtsc_trips,
                port_io_trips: stats.port_io_trips,
                x87_trips: stats.x87_trips,
                task_switch_trips: stats.task_switch_trips,
                task_gate_trips: stats.task_gate_trips,
                task_gate_unknown_trips: stats.task_gate_unknown_trips,
                bda_key_pending_trips: stats.bda_key_pending_trips,
                bda_no_key_trips: stats.bda_no_key_trips,
                bda_unknown_trips: stats.bda_unknown_trips,
                near_match_diffs: NEAR_MATCH_FIELDS
                    .iter()
                    .enumerate()
                    .map(|(i, name)| {
                        (
                            *name,
                            stats.near_miss.near_match[0][i],
                            stats.near_miss.near_match[1][i],
                        )
                    })
                    .collect(),
                near_match_cs_eip: NEAR_MATCH_CS_EIP_FIELDS
                    .iter()
                    .enumerate()
                    .map(|(i, name)| {
                        (
                            *name,
                            stats.near_miss.near_match_cs_eip[0][i],
                            stats.near_miss.near_match_cs_eip[1][i],
                        )
                    })
                    .collect(),
                cs_base_differed_on_match: stats.cs_base_differed_on_match,
                modes_seen_any: [GuestMode::Protected, GuestMode::V86, GuestMode::Real]
                    .iter()
                    .zip(stats.modes_seen_any)
                    .map(|(m, c)| (m.name(), c))
                    .collect(),
                mode_defect_trips: stats.mode_defect_trips,
                batch_straddle_trips: stats.batch_straddle_trips,
                soft_int_posts: stats.soft_int_posts,
                clock_charge_events: (&stats.clock_charge_events).into(),
                read_set_size: (&stats.read_set_size).into(),
                read_set_size_physical: (&stats.read_set_size_physical).into(),
                write_set_size: (&stats.write_set_size).into(),
                translation_set_size: (&stats.translation_set_size).into(),
                translation_set_over_cap: stats.translation_set_over_cap,
                reads_total: stats.reads_total,
                reads_under_other_cr3: stats.reads_under_other_cr3,
                read_class_counts: ALL_CLASSES
                    .iter()
                    .zip(stats.read_class_counts)
                    .map(|(c, n)| (c.name(), n))
                    .collect(),
                write_class_counts: ALL_CLASSES
                    .iter()
                    .zip(stats.write_class_counts)
                    .map(|(c, n)| (c.name(), n))
                    .collect(),
                write_class_r: stats.write_class_r,
                write_class_d: stats.write_class_d,
                write_class_n: stats.write_class_n,
                write_class_n_trips: stats.write_class_n_trips,
                write_unknown_pre: stats.write_unknown_pre,
                write_dead_8kb: stats.write_dead_8kb,
                write_not_plain_ram: stats.write_not_plain_ram,
                top_write_addresses: top,
                class_n_addresses: class_n,
                warm_clock_samples,
                warm_clock_distinct,
                warm_clock_longest_run: stats.warm_clock_longest_run,
            }
        })
        .collect();
    keys.sort_unstable_by_key(|k| (k.vector, k.ah));
    Some(ReflectedCallDiagnosticSnapshot {
        mode: mode_name,
        trips_total: guard.trips_total,
        trips_unmatched: guard.trips_unmatched,
        probe_ns_per_read: guard.probe_ns_per_read,
        keys,
    })
}

#[cfg(test)]
#[path = "reflected_call_diag_test.rs"]
mod tests;
