// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Slice 0b/0c of the reflected-call HLE design
//! (`dev_docs/2026-09-03-reflected-call-hle-design.md`, `dev_docs/2026-09-03-
//! reflected-call-hle-review.md`, `dev_docs/2026-09-04-reflected-call-slice0b-
//! plan.md`, `dev_docs/2026-09-04-reflected-call-slice0b-review.md`): the
//! CORRECTED trip-shape INSTRUMENT. Compiled in only under
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
//! # 0c's corrections over 0b (the slice0b review's D1-D7)
//!
//! * **D1**: a far RETURN (never a far transfer) that lands on the entry's
//!   own CS:EIP:SS with SP at exactly the entry width minus 2 -- the shape of
//!   a handler that returns by `RETF` and leaves the `INT`-pushed FLAGS word
//!   on the caller's stack -- closes as a NEW `return_match_retf_flags`
//!   bucket, reported separately from `return_match`. Rule 2/3 stay
//!   classification-only.
//! * **D2**: `peek_direct_ram` declines (returns `None`) on a misaligned
//!   word/dword access even when every byte in the range is ordinary RAM
//!   (`direct_page_ram_bytes`'s `should_split` term). The write/read path now
//!   falls back to a byte-wise peek before concluding "not plain RAM".
//! * **D3**: `TSS.ESP0` is read at the RIGHT offset for the TR descriptor's
//!   actual type (16-bit TSS: `SP0` at `tr.base+2`; 32-bit TSS: `ESP0` at
//!   `tr.base+4`), and the ESP0-anchored dead-stack window is applied ONLY to
//!   the tracked segment whose selector equals the TSS's own `SS0` -- every
//!   other non-client segment keeps using its own observed high-water mark,
//!   as it always should have.
//! * **D4**: the stack-segment cap is raised (`MAX_STACK_SEGMENTS`, plan §8)
//!   to cover a reflected trip's observed seven concurrent segments, and
//!   `classify` no longer resolves an address-range match against every
//!   tracked segment (ambiguous when two segments share a base or one is
//!   nested inside another's 64 KiB span) -- it classifies by the `SS`
//!   selector actually in force at the access.
//! * **D5**: `on_batch_boundary` now takes a `real_boundary: bool` (plan:
//!   "the machine loop needs a one-line cfg-gated tag on the break reason"):
//!   `izarravm-machine`'s batch loop tags whether the PREVIOUS batch ended on
//!   the cap/a device deadline/a fault/HLT/an HLE post (real) or purely on
//!   the "IF just became enabled" edge (`run.rs`, the `can_take_before`
//!   check) that a reflected trip's own nested `IRET`s cause on every trip by
//!   construction. Only real boundaries count toward `batch_straddle_trips`.
//! * **D6**: the answer-cost report now prices TWO different things
//!   separately: `probe_ns_per_read` (the diagnostic's own page-walking probe
//!   cost, `probe_physical` + `peek_direct_ram`) and NEW `compare_ns_per_read`
//!   (a pre-resolved physical dword compare -- one `peek_direct_ram` per
//!   dword, no page walk -- which is what an HLE answer path would actually
//!   pay, since CR3 never varies on the dominant keys).
//! * **D7**: dead parameters/fields removed (`note_near_miss`'s unused
//!   `boundary` argument, `WriteRecord::sp_at_first_write`, `Walk`'s
//!   unreported `pde_value`/`pte_value`). `NotPlainRam` is no longer
//!   described as a device-window proxy in its own doc comment (see below):
//!   after D2, most of what used to be misclassified `NotPlainRam` was
//!   ordinary misaligned RAM, so the class now means exactly what its peek
//!   says -- "a byte-wise `peek_direct_ram` failed here" -- and nothing about
//!   *why*.
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
use crate::reflected_call::{
    AddressClass, EntryImage, FRAMEBUFFER_APERTURE_HI, FRAMEBUFFER_APERTURE_LO, GuestMode,
    StackTrack, Walk, mask_to_width, probe_physical,
};
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
/// low-water mark for. D4 (slice0b review): a traced `AH=0Bh` trip runs on
/// SEVEN concurrent segments (client, ring-0 monitor, three V86/real-mode
/// excursions, TOKAEMM's flat monitor stack, DOS's own kernel stack); 4 was
/// slice 0b's cap and silently classified the overflow `Other`, where it
/// could never be Class D. Raised with headroom; `stack_segments_over_cap`
/// reports any trip that still overflows this.
const MAX_STACK_SEGMENTS: usize = 12;

/// The literal 8 KB `REFLECTED_CALL_DEAD_STACK_CAP` the design proposed.
/// Reported (`write_dead_8kb`) as a cross-check only (plan §3: "Delete the
/// constant cap from the decision").
const DEAD_STACK_CAP_BYTES: u32 = 8192;

/// Ring size for the warm-clock spread (plan §5, Q5).
const WARM_CLOCK_SAMPLES: usize = 32;

/// Cap on distinct `(CR3, linear page)` translations tracked per trip (plan
/// §3.2/§8).
const REFLECTED_CALL_MAX_TRANSLATIONS: usize = 64;

/// Merge-review nit 6: per-trip `reads`/`writes` had no cap of their own --
/// `MAX_TRIP_INSNS` bounds INSTRUCTIONS, not memory accesses, so a single
/// budgeted `REP` string op inside a trip could add a very large read or
/// write set. Give both the same `_over_cap` treatment `translations` already
/// has, at roughly `MAX_TRIP_INSNS`'s own order of magnitude (a REP moving
/// one byte per instruction cannot touch more distinct addresses than the
/// trip has instructions to spend).
const REFLECTED_CALL_MAX_TRIP_READS: usize = 8_192;
const REFLECTED_CALL_MAX_TRIP_WRITES: usize = 8_192;

/// Merge-review nit 6: per-key `entry_images`/`write_addresses`/
/// `class_n_addresses` grew for the life of the process with no cap of their
/// own -- `MAX_SAMPLES_PER_KEY`/`TOP_IMAGES`/`CLASS_N_ADDRESSES` bound only
/// what is SAMPLED or REPORTED, not the underlying accumulator. Capped with
/// generous headroom over `TOP_IMAGES`/`TOP_ADDRESSES`/`CLASS_N_ADDRESSES`
/// (16-32) so the cap is never reached by a workload with a genuinely small
/// distinct population, only by a pathological one; an `_over_cap` counter
/// reports it rather than silently corrupting `distinct_entry_images`, which
/// IS a reported metric (Q2).
const REFLECTED_CALL_MAX_ENTRY_IMAGES: usize = 4_096;
const REFLECTED_CALL_MAX_WRITE_ADDRESSES: usize = 4_096;
const REFLECTED_CALL_MAX_CLASS_N_ADDRESSES_TRACKED: usize = 4_096;

// The fixed legacy VGA aperture (`memory.rs`, `jit/direct.rs`), physical --
// `FRAMEBUFFER_APERTURE_LO`/`_HI`, moved to `crate::reflected_call` (slice1).
// Plan §3.1: no linear-framebuffer base is resolvable from this crate alone
// (UNVERIFIED base for any LFB aperture vega may expose), so only the
// legacy aperture is classified; that limitation is reported (see the
// result document, T4).

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
pub(crate) fn armed() -> bool {
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
            image.cs.selector != first.cs.selector,
            image.cs.base != first.cs.base,
            image.cs.limit != first.cs.limit,
            image.cs.access != first.cs.access,
            image.ss.selector != first.ss.selector,
            image.ss.base != first.ss.base,
            image.ss.limit != first.ss.limit,
            image.ss.access != first.ss.access,
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
    ss_selector: u16,
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
    // D4 (slice0b review §3): classify by the `SS` selector actually IN
    // FORCE at this access, not by an address-range scan over every tracked
    // segment. A range scan is ambiguous the moment two tracked segments
    // share a base (a ring-0 flat SS aliasing the TSS's own base) or one's
    // 64 KiB span nests inside another's (a V86 real-mode stack sitting
    // inside a flat protected-mode segment) -- the review traced exactly
    // that overlap misclassifying V86-stack writes under the wrong window.
    for seg in trip.stacks.iter().flatten() {
        if seg.selector != ss_selector {
            continue;
        }
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

// `Walk`/`probe_physical` moved to `crate::reflected_call` (slice1); merge-
// review nit 8's doc-comment correction (the walk always resolves by hand,
// never consulting the cached TLB) was ported to that copy.

/// D2 (slice0b review §3): `CpuBus::peek_direct_ram(phys, width)` declines
/// (returns `None`) on a misaligned word/dword access -- `direct_page_ram_bytes`'s
/// `should_split` term, `bus.rs:4159`/`:4586` -- even when every byte in the
/// range is ordinary RAM. 0b's classifier called every one of those `None`s
/// `NotPlainRam`; the slice0b review traced every top `not_plain_ram` address
/// in every leg to an ODD address of the DPMI host's own real-mode register
/// block, which is plain RAM the width-native peek merely declined to read.
/// Try the width-native peek first (the common, aligned case costs nothing
/// extra); on a decline, fall back to a byte-wise peek and reassemble --
/// `None` only when even THAT fails (a genuine device window or unmapped
/// page).
fn peek_ram_width_safe<B: CpuBus>(bus: &B, phys: u32, width: BusWidth) -> Option<u32> {
    if let Some(v) = bus.peek_direct_ram(phys, width) {
        return Some(v);
    }
    let n = width.bytes();
    let mut bytes = [0u8; 4];
    for i in 0..n {
        bytes[i as usize] = bus.peek_direct_ram(phys.wrapping_add(i), BusWidth::Byte)? as u8;
    }
    Some(u32::from_le_bytes(bytes))
}

// ---------------------------------------------------------------------------
// Per-trip state
// ---------------------------------------------------------------------------

struct WriteRecord {
    class: AddressClass,
    physical: Option<u32>,
    pre: Option<u32>,
    latest: u32,
    width_bytes: u32,
}

struct ReadRecord {
    class: AddressClass,
    under_entry_cr3: bool,
}

// Guest execution mode, sampled at trip entry/close and on every hook this
// module reaches, for the "mode transitions inside a trip" check (plan
// §2.1): a trip entering protected mode and closing in V86 is a defect, not
// a match. Type moved to `crate::reflected_call::GuestMode`.

/// The four ways a trip can close (plan §2.1/§2.3). Only `ReturnMatch` counts
/// as a match; the other three close the trip and are recorded, but never
/// produce a memo (slice 1's concern, not 0b's).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CloseRule {
    ReturnMatch,
    /// NEW (0c, D1): a far RETURN (never a far transfer) lands exactly on the
    /// entry's CS:EIP:SS with SP at the entry width minus 2 -- the shape of a
    /// handler that returns by `RETF`, popping only CS:IP and leaving the
    /// `INT`-pushed FLAGS word on the caller's own stack (slice0b review §1:
    /// RTM's protected-mode `INT 21h` hook does exactly this; the client's
    /// own wrapper pops the FLAGS word 14 instructions later, which is what
    /// slice 0b's rule 2 `frame_gone` close was actually seeing). Counts as a
    /// match, reported in its own `closed_by` bucket, never folded into
    /// `return_match`.
    ReturnMatchRetfFlags,
    FrameGone,
    ReEntry,
    Stale,
}

impl CloseRule {
    fn index(self) -> usize {
        match self {
            CloseRule::ReturnMatch => 0,
            CloseRule::ReturnMatchRetfFlags => 1,
            CloseRule::FrameGone => 2,
            CloseRule::ReEntry => 3,
            CloseRule::Stale => 4,
        }
    }

    /// Whether this rule counts as a genuine matching return (plan §2.3: only
    /// rule 1 counts as a match -- 0c extends "rule 1" to include the
    /// RETF-with-flags arm, which is architecturally the SAME return, just
    /// caught one `RETF` earlier).
    fn is_match(self) -> bool {
        matches!(
            self,
            CloseRule::ReturnMatch | CloseRule::ReturnMatchRetfFlags
        )
    }
}

const CLOSED_BY_NAMES: [&str; 5] = [
    "return_match",
    "return_match_retf_flags",
    "frame_gone",
    "re_entry",
    "stale",
];

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
    /// The TR descriptor's own SS0 field (D3, slice0b review): the ESP0
    /// window in `is_dead_stack_derived` may only be applied to the tracked
    /// stack SEGMENT this selector names -- not, as 0b did, to every
    /// non-client segment indiscriminately.
    tss_ss0_selector_at_entry: Option<u16>,
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
    /// D4: this trip observed more than `MAX_STACK_SEGMENTS` distinct `SS`
    /// selectors.
    stack_segments_over_cap: bool,
    /// Merge-review nit 6: this trip's `reads`/`read_phys_dwords` hit
    /// `REFLECTED_CALL_MAX_TRIP_READS` (a budgeted `REP` can blow up a read
    /// set); further distinct reads this trip stop being tracked, and this is
    /// reported rather than silently under-counting `read_set_size`.
    reads_over_cap: bool,
    /// As `reads_over_cap`, for `writes` against `REFLECTED_CALL_MAX_TRIP_WRITES`.
    writes_over_cap: bool,
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
        // this crate), so peek it directly -- at the offset for the TR
        // descriptor's ACTUAL type (D3, slice0b review): a 32-bit TSS has
        // ESP0 at offset 4 and SS0 at offset 8 (386 PRM figure 7-2); a
        // 16-bit (286-style) TSS has SP0 at offset 2 and SS0 at offset 4.
        // Bit 3 of the TR descriptor's type nibble distinguishes them (32-bit
        // avail/busy TSS = type 0x9/0xB, 16-bit avail/busy TSS = 0x1/0x3).
        // 0b always read offset 4 as a dword, which on a 16-bit TSS reads
        // SS0 (a word) as if it were ESP0 -- exactly the traced `Some(0x18)`
        // on every trip, `0x18` being the ring-0 SS selector, not a stack
        // pointer.
        let tss_is_32bit = cpu.tr.access & 0x08 != 0;
        let (esp0_off, ss0_off, esp0_width) = if tss_is_32bit {
            (4u32, 8u32, BusWidth::Dword)
        } else {
            (2u32, 4u32, BusWidth::Word)
        };
        let tss_esp0_at_entry = probe_physical(cpu, bus, cpu.tr.base.wrapping_add(esp0_off))
            .and_then(|(phys, _)| bus.peek_direct_ram(phys, esp0_width));
        let tss_ss0_selector_at_entry = probe_physical(cpu, bus, cpu.tr.base.wrapping_add(ss0_off))
            .and_then(|(phys, _)| bus.peek_direct_ram(phys, BusWidth::Word))
            .map(|v| v as u16);
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
            tss_ss0_selector_at_entry,
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
            stack_segments_over_cap: false,
            reads_over_cap: false,
            writes_over_cap: false,
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
        // Every tracked slot is occupied by a DIFFERENT selector: this trip
        // has more concurrent stack segments than `MAX_STACK_SEGMENTS`
        // tracks. Reported (`stack_segments_over_cap`), never silently
        // dropped (D4).
        self.stack_segments_over_cap = true;
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

    /// Rule 1 (§2.1, plus 0c's D1 `RETF`-with-flags arm) and rule 2 (§2.3),
    /// evaluated at a candidate boundary. Returns the first of these that
    /// fires, or `None` if none does (a near-miss, or a nested call/return
    /// that is not this trip's own boundary at all).
    fn close_rule(&self, cpu: &CpuGsw, boundary: BoundaryKind) -> Option<CloseRule> {
        let regs = &cpu.registers;
        let cs = regs.cs();
        let ss = regs.segment(SegmentIndex::Ss);
        let esp = regs.esp();
        let cs_matches = cs.selector == self.return_cs_selector;
        let eip_matches = regs.eip == self.return_eip;
        let ss_matches = ss.selector == self.entry_ss_selector;
        let sp_here = self.sp_at_entry_width(esp);
        let entry_sp = self.entry_sp_at_width();
        if cs_matches && eip_matches && ss_matches && sp_here == entry_sp {
            return Some(CloseRule::ReturnMatch);
        }
        // D1 (slice0b review §1): a far RETURN -- never a far transfer, which
        // has no FLAGS word of its own to leave behind -- that lands on the
        // entry's own CS:EIP:SS with SP sitting exactly at entry-width minus
        // 2 has already popped the return CS:IP the `INT` pushed and left
        // only the FLAGS word un-popped. Architecturally this IS the trip's
        // matching return; a caller-side epilogue may pop the FLAGS word many
        // instructions later (that pop is not this trip's concern -- the
        // trip already closed here).
        if boundary == BoundaryKind::FarReturn
            && cs_matches
            && eip_matches
            && ss_matches
            && sp_here == entry_sp.wrapping_sub(2) & self.width_mask()
        {
            return Some(CloseRule::ReturnMatchRetfFlags);
        }
        // Rule 2, frame-gone: CS/SS match the entry but SP has moved PAST it
        // (at the entry width) -- the client's own frame is already gone,
        // so this cannot be its matching return, but the trip is over.
        if cs_matches && ss_matches && sp_here > entry_sp {
            return Some(CloseRule::FrameGone);
        }
        None
    }

    /// The entry stack segment's own architectural width, as a mask (plan
    /// §2.1 / D1): `0xFFFF` on a 16-bit stack, `0xFFFF_FFFF` on a 32-bit one.
    fn width_mask(&self) -> u32 {
        if self.entry_ss_big {
            0xFFFF_FFFF
        } else {
            0xFFFF
        }
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
    /// histograms this boundary feeds. D7 (slice0b review): both kinds share
    /// one set of per-trip counters; the split by `boundary_kind` happens in
    /// the reported table via `record_near_miss_by_boundary`, whose CALLER
    /// already knows which kind of boundary this is -- so this function no
    /// longer takes a `boundary` parameter of its own to ignore.
    fn note_near_miss(&mut self, cpu: &CpuGsw, cs_eip_matched: bool) {
        let regs = &cpu.registers;
        let cs = regs.cs();
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
    closed_by: [u64; 5],
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
    // Journal-mode only. Each of these four is reported TWICE (plan §14 Q1,
    // orchestrator decision): the unsuffixed field is WHOLE-RUN (every
    // trip), the `_windowed` field only counts trips whose instruction range
    // falls entirely inside `IZARRAVM_REFLECTED_CALL_DIAG_WINDOW`. When no
    // window is configured the `_windowed` fields stay empty (`sample_count:
    // 0`), which is the honest "not armed" signal rather than a silent
    // duplicate of the whole-run numbers.
    read_set_size: Samples,
    read_set_size_windowed: Samples,
    read_set_size_physical: Samples,
    read_set_size_physical_windowed: Samples,
    write_set_size: Samples,
    write_set_size_windowed: Samples,
    translation_set_size: Samples,
    translation_set_size_windowed: Samples,
    trips_in_window: u64,
    translation_set_over_cap: u64,
    stack_segments_over_cap_trips: u64,
    /// Merge-review nit 6: counts a distinct entry image / write address /
    /// Class N address that arrived AFTER its accumulator was already at its
    /// cap (`REFLECTED_CALL_MAX_ENTRY_IMAGES`/`_WRITE_ADDRESSES`/
    /// `_CLASS_N_ADDRESSES_TRACKED`) and was therefore dropped rather than
    /// tracked.
    entry_images_over_cap: u64,
    write_addresses_over_cap: u64,
    class_n_addresses_over_cap: u64,
    /// Trips whose per-trip `reads`/`writes` hit
    /// `REFLECTED_CALL_MAX_TRIP_READS`/`_WRITES`.
    trips_reads_over_cap: u64,
    trips_writes_over_cap: u64,
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
    /// non-empty read set. This is the diagnostic's OWN page-walking PROBE
    /// cost (`probe_physical` + `peek_direct_ram`), not an answer-path cost
    /// (D6, slice0b review §4).
    probe_ns_per_read: Option<f64>,
    /// NEW (0c, D6): the pre-resolved physical COMPARE cost -- one
    /// `peek_direct_ram(phys, Dword)` per distinct physical dword, no page
    /// walk at all -- filled in alongside `probe_ns_per_read` from the same
    /// trip's read set. This is what an HLE answer path would actually pay:
    /// the design's dominant keys never vary CR3, so physical addresses can
    /// be pre-resolved once at learn time and the answer path never walks
    /// page tables again.
    compare_ns_per_read: Option<f64>,
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
    match open.close_rule(cpu, BoundaryKind::FarReturn) {
        Some(rule) => finish_trip(cpu, bus, state, rule),
        None => {
            let cs_eip_matched = {
                let regs = &cpu.registers;
                let cs = regs.cs();
                cs.selector == open.return_cs_selector && regs.eip == open.return_eip
            };
            let mut over_budget = false;
            if let Some(open) = state.open.as_mut() {
                open.note_near_miss(cpu, cs_eip_matched);
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
    match open_ref.close_rule(cpu, BoundaryKind::FarTransfer) {
        Some(rule) => finish_trip(cpu, bus, state, rule),
        None => {
            let cs_eip_matched = {
                let regs = &cpu.registers;
                let cs = regs.cs();
                cs.selector == open_ref.return_cs_selector && regs.eip == open_ref.return_eip
            };
            if let Some(open) = state.open.as_mut() {
                open.note_near_miss(cpu, cs_eip_matched);
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
///
/// `real_boundary` is D5 (slice0b review §2): 0b counted EVERY batch entry
/// while a trip was open, including the ones caused by the trip's OWN nested
/// `IRET`s re-enabling IF (`run.rs`'s `can_take_before` check at the top of
/// its inner run loop) -- a reflected trip is BY CONSTRUCTION a chain of gate
/// entries and `IRET`s, so that counter was 100% tautological (a trip cannot
/// help but contain the very edges it is being blamed for straddling). The
/// caller now tags whether the PREVIOUS batch ended for a reason independent
/// of this trip's own instructions (the cap, a cached device deadline, a
/// fault, HLT, an HLE INT post) -- `true` -- or purely on that IF-enable edge
/// -- `false`. Only `true` calls advance `batch_boundaries_seen`, so
/// `batch_straddle_trips` now means "this trip's execution spanned more than
/// one REAL batch", not "this trip contains an `IRET`".
pub fn on_batch_boundary(real_boundary: bool) {
    if !armed() {
        return;
    }
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    on_batch_boundary_on(&mut guard, real_boundary);
}

fn on_batch_boundary_on(state: &mut State, real_boundary: bool) {
    if !real_boundary {
        return;
    }
    if let Some(open) = state.open.as_mut() {
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
    // Byte width is never misaligned, so this read-side check was already
    // width-safe pre-D2; kept as Byte rather than switched to the access's
    // own (unknown here) width.
    let plain_ram = physical
        .map(|p| bus.peek_direct_ram(p, BusWidth::Byte).is_some())
        .unwrap_or(true);
    let class = classify(cpu, open, linear, physical, plain_ram, ss.selector, None);
    let under_entry_cr3 = cr3_now == open.entry_cr3;
    // Merge-review nit 6: cap the per-trip read side. An already-recorded
    // address is always updated for free (a HashMap lookup either way); a
    // NEW address past the cap is dropped and flagged rather than growing
    // `reads`/`read_phys_dwords` without bound (a budgeted `REP` can revisit
    // many distinct addresses in one trip).
    if open.reads.contains_key(&linear) {
        // Already tracked; nothing to add.
    } else if open.reads.len() >= REFLECTED_CALL_MAX_TRIP_READS {
        open.reads_over_cap = true;
    } else {
        open.reads.insert(
            linear,
            ReadRecord {
                class,
                under_entry_cr3,
            },
        );
        if let Some(phys) = physical {
            open.read_phys_dwords.insert(phys & !0x3);
        }
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
    // D2: `peek_ram_width_safe` (not the width-native `peek_direct_ram`
    // alone) so a misaligned word/dword write to ordinary RAM still resolves
    // a `pre` value and is still recognised as plain RAM.
    let pre = if already_written {
        None
    } else {
        physical.and_then(|phys| peek_ram_width_safe(bus, phys, width))
    };
    open.touch_stack(ss, esp);
    open.observe_mode(cpu);
    let plain_ram = physical
        .map(|p| peek_ram_width_safe(bus, p, width).is_some())
        .unwrap_or(true);
    let class = classify(
        cpu,
        open,
        linear,
        physical,
        plain_ram,
        ss.selector,
        forced_class,
    );
    // Merge-review nit 6: cap the per-trip write side, same shape as the read
    // side above. An already-recorded address is always updated for free; a
    // NEW address past the cap is dropped and flagged.
    if already_written {
        if let Some(rec) = open.writes.get_mut(&linear) {
            rec.latest = value;
        }
    } else if open.writes.len() >= REFLECTED_CALL_MAX_TRIP_WRITES {
        open.writes_over_cap = true;
    } else {
        open.writes.insert(
            linear,
            WriteRecord {
                class,
                physical,
                pre,
                latest: value,
                width_bytes: width.bytes(),
            },
        );
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
    let unmatched = !rule.is_match();
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
    if rule.is_match() && trip.cs_base_differed_on_match {
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
    if stats.entry_images.contains_key(&trip.entry_image)
        || stats.entry_images.len() < REFLECTED_CALL_MAX_ENTRY_IMAGES
    {
        *stats.entry_images.entry(trip.entry_image).or_insert(0) += 1;
    } else {
        stats.entry_images_over_cap += 1;
    }
    stats
        .distinct_cs_eip
        .insert((trip.entry_image.cs.selector, trip.return_eip));
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
    if rule.is_match() && trip.mode_at_entry != trip.mode_at_close {
        stats.mode_defect_trips += 1;
    }
    if trip.batch_boundaries_seen > 0 {
        stats.batch_straddle_trips += 1;
    }
    stats.soft_int_posts += u64::from(trip.soft_int_posts);
    if trip.reads_over_cap {
        stats.trips_reads_over_cap += 1;
    }
    if trip.writes_over_cap {
        stats.trips_writes_over_cap += 1;
    }

    if rule.is_match() && mode_is_shape_relevant(&trip) {
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
        // D6: the SAME read set, priced as a pre-resolved physical compare
        // instead of a page-walking probe -- the answer-cost estimate, kept
        // separate from the probe cost above.
        let compare_ns = run_compare_bench(bus, &trip.read_phys_dwords);
        guard.compare_ns_per_read = Some(compare_ns);
    }

    // Journal-mode-only aggregation. In shape mode `trip.reads`/`trip.writes`
    // are always empty. WHOLE-RUN totals are unconditional; `_windowed`
    // counterparts additionally require `in_window` (plan §14 Q1).
    if in_window {
        stats.trips_in_window += 1;
    }
    stats.reads_total += trip.reads.len() as u64;
    stats.read_set_size.push(trip.reads.len() as u64);
    stats
        .read_set_size_physical
        .push(trip.read_phys_dwords.len() as u64);
    if in_window {
        stats.read_set_size_windowed.push(trip.reads.len() as u64);
        stats
            .read_set_size_physical_windowed
            .push(trip.read_phys_dwords.len() as u64);
    }
    for read in trip.reads.values() {
        read_class_bump(stats, read.class);
        if !read.under_entry_cr3 {
            stats.reads_under_other_cr3 += 1;
        }
    }
    stats.write_set_size.push(trip.writes.len() as u64);
    stats
        .translation_set_size
        .push(trip.translations.len() as u64);
    if in_window {
        stats.write_set_size_windowed.push(trip.writes.len() as u64);
        stats
            .translation_set_size_windowed
            .push(trip.translations.len() as u64);
    }
    if trip.translation_set_over_cap {
        stats.translation_set_over_cap += 1;
    }
    if trip.stack_segments_over_cap {
        stats.stack_segments_over_cap_trips += 1;
    }
    let mut trip_has_class_n = false;
    for (&addr, write) in trip.writes.iter() {
        write_class_bump(stats, write.class);
        if stats.write_addresses.contains_key(&addr)
            || stats.write_addresses.len() < REFLECTED_CALL_MAX_WRITE_ADDRESSES
        {
            *stats.write_addresses.entry(addr).or_insert(0) += 1;
        } else {
            stats.write_addresses_over_cap += 1;
        }
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
                if stats.class_n_addresses.contains_key(&key)
                    || stats.class_n_addresses.len() < REFLECTED_CALL_MAX_CLASS_N_ADDRESSES_TRACKED
                {
                    let entry = stats
                        .class_n_addresses
                        .entry(key)
                        .or_insert((write.class, 0));
                    entry.1 += 1;
                } else {
                    stats.class_n_addresses_over_cap += 1;
                }
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
        // D3 (slice0b review §3): the ESP0-anchored window only applies to
        // the ONE tracked segment whose selector is the TSS's own SS0 -- not,
        // as 0b did, to every non-client segment. Every other non-client
        // stack (a V86 real-mode excursion, a DOS-kernel stack, TOKAEMM's
        // flat monitor stack) keeps using its OWN observed high-water mark,
        // exactly like the client stack does.
        let is_ss0_segment = trip.tss_ss0_selector_at_entry == Some(seg.selector);
        let upper_esp = if is_ss0_segment {
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

/// D6 (slice0b review §4): the ANSWER-cost estimate, as opposed to
/// `run_probe_bench`'s diagnostic-probe cost. `addrs_set` already holds
/// PHYSICAL dwords (`Trip::read_phys_dwords`), pre-resolved once when the
/// read happened -- exactly what an HLE answer path would have cached at
/// learn time, since the design's dominant keys never vary CR3. Prices ONE
/// `peek_direct_ram(phys, Dword)` per iteration, no page walk at all.
fn run_compare_bench<B: CpuBus>(bus: &B, addrs_set: &std::collections::HashSet<u32>) -> f64 {
    let addrs: Vec<u32> = addrs_set.iter().copied().collect();
    const ITERATIONS: u32 = 1_000_000;
    let start = std::time::Instant::now();
    let mut sink: u32 = 0;
    for i in 0..ITERATIONS {
        let addr = addrs[(i as usize) % addrs.len()];
        if let Some(v) = bus.peek_direct_ram(addr, BusWidth::Dword) {
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
    pub trips_in_window: u64,
    pub read_set_size: StatSummary,
    pub read_set_size_windowed: StatSummary,
    pub read_set_size_physical: StatSummary,
    pub read_set_size_physical_windowed: StatSummary,
    pub write_set_size: StatSummary,
    pub write_set_size_windowed: StatSummary,
    pub translation_set_size: StatSummary,
    pub translation_set_size_windowed: StatSummary,
    pub translation_set_over_cap: u64,
    pub stack_segments_over_cap_trips: u64,
    pub entry_images_over_cap: u64,
    pub write_addresses_over_cap: u64,
    pub class_n_addresses_over_cap: u64,
    pub trips_reads_over_cap: u64,
    pub trips_writes_over_cap: u64,
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
    /// D6: the pre-resolved physical compare cost, separate from the probe cost above.
    pub compare_ns_per_read: Option<f64>,
    /// `IZARRAVM_REFLECTED_CALL_DIAG_WINDOW`'s bounds (retired guest
    /// instructions), when configured -- `None` means every key's
    /// `_windowed` field is empty (not armed), not a silent duplicate of the
    /// whole-run numbers (plan §14 Q1).
    pub window_start_insns: Option<u64>,
    pub window_end_insns: Option<u64>,
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
                trips_in_window: stats.trips_in_window,
                read_set_size: (&stats.read_set_size).into(),
                read_set_size_windowed: (&stats.read_set_size_windowed).into(),
                read_set_size_physical: (&stats.read_set_size_physical).into(),
                read_set_size_physical_windowed: (&stats.read_set_size_physical_windowed).into(),
                write_set_size: (&stats.write_set_size).into(),
                write_set_size_windowed: (&stats.write_set_size_windowed).into(),
                translation_set_size: (&stats.translation_set_size).into(),
                translation_set_size_windowed: (&stats.translation_set_size_windowed).into(),
                translation_set_over_cap: stats.translation_set_over_cap,
                stack_segments_over_cap_trips: stats.stack_segments_over_cap_trips,
                entry_images_over_cap: stats.entry_images_over_cap,
                write_addresses_over_cap: stats.write_addresses_over_cap,
                class_n_addresses_over_cap: stats.class_n_addresses_over_cap,
                trips_reads_over_cap: stats.trips_reads_over_cap,
                trips_writes_over_cap: stats.trips_writes_over_cap,
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
        compare_ns_per_read: guard.compare_ns_per_read,
        window_start_insns: window().map(|w| w.start_insns),
        window_end_insns: window().map(|w| w.end_insns),
        keys,
    })
}

#[cfg(test)]
#[path = "reflected_call_diag_test.rs"]
mod tests;
