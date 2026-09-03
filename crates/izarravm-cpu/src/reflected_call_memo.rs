// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Slice 1 of the reflected-call HLE design
//! (`dev_docs/2026-09-03-reflected-call-hle-design.md` Revision 2,
//! `dev_docs/2026-09-04-reflected-call-slice1-plan.md` Revision 2, "Revision 2
//! amendments" item 6): **the RECORD-AND-MEASURE commit**. This module learns
//! whether a reflected `INT` round trip is deterministic enough to be worth
//! memoising, and measures the raw-clock stability and write-classification
//! shape that decide it -- but it has **no answer path**: nothing here ever
//! skips guest execution, applies an epilogue, gates on a bus predicate or
//! clamps against a batch cap. Those land in a later slice, once this
//! commit's measurement (`dev_docs/2026-09-04-reflected-call-slice1-measure-
//! first.md`) has graded the three pre-registered NO-GOs.
//!
//! Unlike `reflected_call_diag` (the `#[cfg(feature =
//! "reflected-call-diagnostic")]` instrument, process-global `Mutex` state),
//! this module is **always compiled**, production code behind the runtime
//! knob `IZARRAVM_REFLECTED_CALL_MEMO`, and its state is owned by `CpuGsw`
//! itself (`reflected_call: Option<Box<ReflectedCallMemoState>>`) -- no
//! `Mutex`, no process-global, one pointer of growth, `None` for the whole
//! run when the knob is off.
//!
//! # The learn cycle
//!
//! Per `MemoKey`, trips cycle through four slots (plan section 4.2):
//!
//! 1. **Warm** -- discarded (warms A/D bits, TLB, decode).
//! 2. **Journal A** -- journaled in full (`reflected_call_journal` gates the
//!    memory seams), kept as the comparison baseline.
//! 3. **Journal B** -- journaled in full, compared EXACTLY against Journal A:
//!    same read set with the same values, same translation set, same write
//!    set (both addresses AND, address by address, values), same exit image,
//!    same instruction count. Any structural disagreement is
//!    `learn_refused[journal_mismatch]`; a write whose two values genuinely
//!    differ is `learn_refused[write_class_n]` (R2.3: a memo may never carry
//!    a net write whose value is not pinned by something the trip itself
//!    read). If Journal A and B agree, every write is classified R (pinned
//!    pre-value, restored) / D (dead stack) / W (everything else that is not
//!    R or D, including an UNPINNED restoration -- R2.3's fix) and the
//!    per-key `write_class_r_pinned` tally is updated.
//! 4. **Natural x8** -- run with the journal OFF (this is what makes the
//!    measured raw-clock totals trustworthy: a journaled trip pays extra
//!    peeks the native path never does). Each natural trip's raw core/bus
//!    clocks and instruction count are recovered EXACTLY from
//!    `elapsed_clocks`/`timing_rem` sampled at open and close (R2.2/R2.15's
//!    telescoping-carry identity -- **no per-instruction accumulator on any
//!    hot path**) and from `CpuBus::cumulative_raw_bus_clocks` (whole-run cumulative, NEVER
//!    reset at a machine-batch boundary -- unlike `CpuBus::in_batch_raw_bus_clocks`, which
//!    resets at every batch re-entry INCLUDING the plain IF-edge break a trip's own nested
//!    `IRET`s cause 6-8 times per trip; a Fable review on 2026-09-03 caught this module using
//!    the wrong one, producing a `raw_bus` delta that went negative on `INT 33h`). If all 8
//!    triples agree, the key's `learned` counter increments; otherwise
//!    `learn_refused[clocks_unstable]`.
//!
//! Any refusal, at any slot, restarts the key at Warm and counts one
//! CONSECUTIVE failure (`MEMO_LEARN_BUDGET`); any success at the Natural
//! stage resets the streak to zero (R2.9(i)). A key whose consecutive
//! failures reach the budget is DISARMED for the rest of the run -- plan
//! section 4.5: "in slice 1 that cache is permanent for the run".
//!
//! Because nothing here answers, a key that finishes a successful learn
//! cycle immediately restarts at Warm rather than staying "armed": this is
//! what lets the measure-first pass in
//! `dev_docs/2026-09-04-reflected-call-slice1-measure-first.md` collect
//! thousands of independent samples per key from one ordinary run.
//!
//! # Deliberate scope cuts against the plan's fuller design (documented, not
//! silent)
//!
//! * The design's `control_effects`/nested-ack/live-stack-tail REPLAY lists
//!   (R2.5/R2.6/4.4) are answer-path machinery; this commit tracks only what
//!   the record-and-measure pass needs and does not populate them.
//! * The batch loop's ownership of the interpreter-forcing toggle (plan
//!   section 4.2's last paragraph, an `izarravm-machine` change) is not
//!   wired here: the journal seams this module gates
//!   (`memory.rs`/`control.rs`) already fire uniformly from both the
//!   interpreter and the Direct backend (verified by inspection: they are
//!   the single shared call-out both paths route through), so forcing the
//!   interpreter is a defensive precision measure, not a correctness
//!   requirement for what this commit journals. Left for the slice that
//!   needs it.
//! * A20 retirement (R2.14) is answer-path soundness (it protects a memo's
//!   pre-resolved read set from going stale); this commit builds no memo body
//!   to protect, so it is not wired here -- left for the answer-path commit.
//! * Every refusal lane is now wired to a real production seam (answer-path
//!   amendment 7): `pending_soft_int` to `CpuBus::reflected_call_soft_int_posted`
//!   (asked of the bus at trip close AND as an answer-time screen, because the
//!   poster is the machine's own field and no CPU-side seam sees every one),
//!   `task_switch` to `CpuGsw::task_switch`, `port_io` to the interpreter's and
//!   the JIT's `OUT` call-outs, `nondeterministic_read` to RDTSC/RDMSR, `x87` to
//!   `fpu_exec`, `page_fault` to exception delivery, `hardware_interrupt` to the
//!   IRQ hook, `level_changed` to the raw-clock recovery, `debug_state`/`vme_or_pvi`
//!   to the entry-image hazard test, and the device-edge / cap / DMA-visible lanes
//!   to the bus gate. The one lane with no seam is `mmx`, and that is structural
//!   rather than missing: `two_byte_isa_generation` marks the WHOLE MMX
//!   integer-SIMD block `IsaGeneration::Never` on every persona, so an MMX form
//!   #UDs at the shared decode gate before execute is reached and can never
//!   mutate the shared FPU register file inside a trip. The lane is kept for a
//!   future persona that adds the extension.

use super::*;
use crate::reflected_call::*;
use izarravm_bus::{ReflectedCallDecline, ReflectedCallGateRequest};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Tunables (plan section 4, Revision 2 R2.10 item 16, R2.3/R2.17)
// ---------------------------------------------------------------------------

/// 8x the observed median trip length (plan Revision 2, R2.10 item 16),
/// stated there as 12,608 for this title's two dominant keys.
pub(crate) const MEMO_MAX_TRIP_INSNS: u64 = 12_608;
/// Consecutive learn-attempt failures before a key is disarmed (plan section
/// 4.5, corrected to count CONSECUTIVE failures by R2.9(i)).
pub(crate) const MEMO_LEARN_BUDGET: u32 = 4;
/// Trips a disarmed key must be SEEN (not learned -- `KeyState::trips_seen`) before it is
/// re-armed (Fable re-review, 2026-09-03, campaign verdict 2c): a budget spent entirely inside
/// a menu phase must not blind the dwell for the rest of the run. `2^16`, the review's own
/// figure.
pub(crate) const MEMO_REARM_TRIPS_SEEN: u64 = 65_536;
/// Natural warm trips whose raw-clock triple must agree before a learn is
/// counted (plan section 4.2 point 4).
pub(crate) const MEMO_CLOCK_SAMPLES: usize = 8;
/// Physical dword read-set cap (plan section 4.5).
pub(crate) const MEMO_MAX_READ_SET: usize = 512;
/// Translation-set cap (plan section 4.5).
pub(crate) const MEMO_MAX_TRANSLATION_SET: usize = 64;
/// Replay-write cap, raised from 32 to 192 by R2.3/R2.17 (the pinned-pre-
/// value fix moves most same-address writes from R to W).
pub(crate) const MEMO_MAX_REPLAY_WRITES: usize = 192;
/// Runtime-audit `write_value` strikes a key may take before the bounded disarm
/// (`dev_docs/2026-09-05-reflected-call-learn-protocol-design.md`, option D as
/// recommended). Two, not one: with option E confirming the write model across eight
/// natural samples, a single strike is most likely a cadence longer than that window, and
/// the next learn cycle's demotion may capture it.
pub(crate) const MEMO_WRITE_VALUE_STRIKES: u32 = 2;
/// Default audit period: one answer in 64 is refused and run NATURALLY so its real end
/// state can be compared with what the memo would have produced (measure-first review,
/// campaign verdict (2c)). Chosen, not tuned: it bounds the charged-clock error a regime
/// shift can accumulate to `64 x delta` before the audit catches it, and costs 1.6% of the
/// answer rate.
pub(crate) const MEMO_AUDIT_PERIOD_DEFAULT: u64 = 64;
/// The band, in RAW bus clocks, inside which an audited trip's bus total must land.
/// Exact bus conservation is unattainable in principle (measure-first re-review (1)): an
/// answered trip touches no cache line, so the cache model's answer to the next access
/// stream differs from the real one whatever is charged. The observed jitter on the
/// dominant key is +38/-4 raw at 0.007% of samples; 64 is 5x that, and ~0.0007% of an
/// irq0 edge.
pub(crate) const MEMO_AUDIT_BUS_BAND: u64 = 64;
/// Physical code pages a memo may carry (plan section 4.5's `code_pages_too_many`).
/// The dominant key's trip spans the client, the DPMI host and the DOS kernel; 16 is
/// the plan's own figure.
/// Nested `interrupt_acknowledge` calls a memo may carry (plan R2.6). Over the cap ->
/// `nested_acks_too_many`. The dominant key nests two `INT 16h`, an `INT 28h` and the
/// reflected `INT 21h`; 16 is 4x that.
pub(crate) const MEMO_MAX_NESTED_ACKS: usize = 16;
/// TLB/decode control effects a memo may carry (plan R2.5). The dominant key's trips
/// write CR3 exactly twice (the VCPI pair); 8 is 4x that.
pub(crate) const MEMO_MAX_CONTROL_EFFECTS: usize = 8;
/// Per-key image cache depth, LRU (plan section 3): 0b measured top-8 images cover 99.99% of
/// trips on both dominant keys.
pub(crate) const MEMO_IMAGES_PER_KEY: usize = 8;

// ---------------------------------------------------------------------------
// The knob
// ---------------------------------------------------------------------------

/// Parse `IZARRAVM_REFLECTED_CALL_MEMO`, lifted out of its `OnceLock` closure
/// so it is unit-testable without a process-global env write -- a structural
/// copy of `parse_direct_poll_skip_arm` (`jit/direct.rs`) with one
/// difference, the default: unset or `""` means **off** (plan section 7.3;
/// `=0` is never the off SPELLING, unset is).
pub(crate) fn parse_reflected_call_memo_arm(spec: Result<String, std::env::VarError>) -> bool {
    match spec {
        Err(_) => false,
        Ok(s) if s.is_empty() => false,
        Ok(s) => match s.as_str() {
            "0" | "off" => false,
            "1" | "on" | "memo" => true,
            other => panic!(
                "IZARRAVM_REFLECTED_CALL_MEMO={other:?} not recognised; want unset, \"\", \
                 \"0\"/\"off\" or \"1\"/\"on\"/\"memo\""
            ),
        },
    }
}

/// Parse `IZARRAVM_REFLECTED_CALL_MEMO_AUDIT`, the numeric-knob convention (`sweep_knob`):
/// unset and `""` both mean the DEFAULT, never "off" -- `=0` is the off spelling and it is
/// the only one (`parameter-knobs-have-no-off-spelling`). An unparseable value panics
/// naming the accepted forms, so a mistyped ladder leg cannot silently run the default.
pub(crate) fn parse_reflected_call_memo_audit(spec: Result<String, std::env::VarError>) -> u64 {
    match spec {
        Err(_) => MEMO_AUDIT_PERIOD_DEFAULT,
        Ok(s) if s.trim().is_empty() => MEMO_AUDIT_PERIOD_DEFAULT,
        Ok(s) => match s.trim().parse::<u64>() {
            Ok(n) => n,
            Err(_) => panic!(
                "IZARRAVM_REFLECTED_CALL_MEMO_AUDIT={s:?} not recognised; want unset, \"\"                  (both = {MEMO_AUDIT_PERIOD_DEFAULT}), \"0\" (off), or a positive integer"
            ),
        },
    }
}

fn knob_audit_period() -> u64 {
    static PERIOD: OnceLock<u64> = OnceLock::new();
    *PERIOD.get_or_init(|| {
        parse_reflected_call_memo_audit(std::env::var("IZARRAVM_REFLECTED_CALL_MEMO_AUDIT"))
    })
}

fn knob_armed() -> bool {
    static ARMED: OnceLock<bool> = OnceLock::new();
    *ARMED.get_or_init(|| {
        parse_reflected_call_memo_arm(std::env::var("IZARRAVM_REFLECTED_CALL_MEMO"))
    })
}

/// Read ONCE, at `CpuGsw` construction (plan Revision 2, R2.10 item 15): the
/// hot-path `INT` hook afterwards only ever tests `self.reflected_call.is_none()`,
/// never the `OnceLock`.
pub(crate) fn armed_at_construction() -> bool {
    knob_armed()
}

// ---------------------------------------------------------------------------
// The key
// ---------------------------------------------------------------------------

/// Bucket key: cheap, generic, no vector-specific or title-specific term
/// (plan section 3). `ax` is a bucket REFINEMENT only (R2.10 item 16) -- an
/// eventual answer path's entry-image compare is authoritative; nobody
/// should read `ax` in the key as "the fix" for a function that also depends
/// on, say, `BX`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct MemoKey {
    /// The guest-clock model epoch the trip was LEARNED under.
    ///
    /// A memo records the trip's clocks (`raw_core`, `raw_bus`) and replays
    /// them instead of re-running it, so a memo learned under one charge model
    /// would go on answering with that model's numbers after the model changed
    /// -- and slice 8 changes exactly the numbers a reflected trip is made of
    /// (the V86 monitor trip, the faulting instruction's own class, `IRET`'s
    /// mode rows). Under epoch 1 that would replay a 16.7x-light trip forever.
    ///
    /// In-process the epoch is fixed at construction, so this tag cannot fire
    /// today: it guards a PERSISTED memo and a future per-persona epoch
    /// selection, at one byte in a key that is already eighteen.
    pub epoch: u8,
    pub vector: u8,
    pub ax: u16,
    pub cs_selector: u16,
    pub int_eip: u32,
    pub ss_selector: u16,
    pub ss_big: bool,
    pub cpl: u8,
    pub vm: bool,
}

// ---------------------------------------------------------------------------
// Refusal reasons
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LearnRefused {
    PortIo,
    NondeterministicRead,
    PendingSoftInt,
    X87,
    PageFault,
    TaskSwitch,
    ControlRegisterDelta,
    ClosedWithoutReturn,
    TripTooLong,
    JournalMismatch,
    WriteClassN,
    ReadSetTooLarge,
    TranslationSetTooLarge,
    ReplaySetTooLarge,
    ClocksUnstable,
    HardwareInterrupt,
    VmeOrPvi,
    DebugState,
    LevelChanged,
    NestedAcksTooMany,
    ControlEffectsTooMany,
    ControlEffectUnreplayable,
    Mmx,
    ReadSetUnstable,
    WriteValueUnstable,
}

pub(crate) const LEARN_REFUSED_ALL: [LearnRefused; 25] = [
    LearnRefused::PortIo,
    LearnRefused::NondeterministicRead,
    LearnRefused::PendingSoftInt,
    LearnRefused::X87,
    LearnRefused::PageFault,
    LearnRefused::TaskSwitch,
    LearnRefused::ControlRegisterDelta,
    LearnRefused::ClosedWithoutReturn,
    LearnRefused::TripTooLong,
    LearnRefused::JournalMismatch,
    LearnRefused::WriteClassN,
    LearnRefused::ReadSetTooLarge,
    LearnRefused::TranslationSetTooLarge,
    LearnRefused::ReplaySetTooLarge,
    LearnRefused::ClocksUnstable,
    LearnRefused::HardwareInterrupt,
    LearnRefused::VmeOrPvi,
    LearnRefused::DebugState,
    LearnRefused::LevelChanged,
    LearnRefused::NestedAcksTooMany,
    LearnRefused::ControlEffectsTooMany,
    LearnRefused::ControlEffectUnreplayable,
    LearnRefused::Mmx,
    LearnRefused::ReadSetUnstable,
    LearnRefused::WriteValueUnstable,
];

impl LearnRefused {
    fn index(self) -> usize {
        LEARN_REFUSED_ALL
            .iter()
            .position(|r| *r == self)
            .expect("in LEARN_REFUSED_ALL")
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::PortIo => "port_io",
            Self::NondeterministicRead => "nondeterministic_read",
            Self::PendingSoftInt => "pending_soft_int",
            Self::X87 => "x87",
            Self::PageFault => "page_fault",
            Self::TaskSwitch => "task_switch",
            Self::ControlRegisterDelta => "control_register_delta",
            Self::ClosedWithoutReturn => "closed_without_return",
            Self::TripTooLong => "trip_too_long",
            Self::JournalMismatch => "journal_mismatch",
            Self::WriteClassN => "write_class_n",
            Self::ReadSetTooLarge => "read_set_too_large",
            Self::TranslationSetTooLarge => "translation_set_too_large",
            Self::ReplaySetTooLarge => "replay_set_too_large",
            Self::ClocksUnstable => "clocks_unstable",
            Self::HardwareInterrupt => "hardware_interrupt",
            Self::VmeOrPvi => "vme_or_pvi",
            Self::DebugState => "debug_state",
            Self::LevelChanged => "level_changed",
            Self::NestedAcksTooMany => "nested_acks_too_many",
            Self::ControlEffectsTooMany => "control_effects_too_many",
            Self::ControlEffectUnreplayable => "control_effect_unreplayable",
            Self::Mmx => "mmx",
            Self::ReadSetUnstable => "read_set_unstable",
            Self::WriteValueUnstable => "write_value_unstable",
        }
    }
}

/// What an audited trip disagreed with the memo about (plan R2.7 / R2.18 as amended:
/// the audit is a FORMULA over the observed write set, not a set comparison, and it is
/// content-only -- it never answers, so it exercises no apply-path bug).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AuditMismatch {
    /// The exit architectural state (registers, EFLAGS, six segments, ESP) differed.
    Epilogue,
    /// Some address's real final value differed from the memo's prediction: its replay
    /// value where the memo replays it, else the value it held at trip entry.
    WriteValue,
    /// The raw core-clock total differed. EXACT is the bar here: core and instruction
    /// sums were unanimous over 525,352 measured samples.
    CoreClocks,
    /// The instruction count differed -- a different path through the handler.
    Instructions,
    /// The raw bus total fell outside `MEMO_AUDIT_BUS_BAND`.
    BusClocks,
    /// The trip did not close as a return match at all, or was refused, so there is
    /// nothing to compare. Counted, never acted on.
    Unusable,
}

pub(crate) const AUDIT_MISMATCH_ALL: [AuditMismatch; 6] = [
    AuditMismatch::Epilogue,
    AuditMismatch::WriteValue,
    AuditMismatch::CoreClocks,
    AuditMismatch::Instructions,
    AuditMismatch::BusClocks,
    AuditMismatch::Unusable,
];

impl AuditMismatch {
    fn index(self) -> usize {
        AUDIT_MISMATCH_ALL
            .iter()
            .position(|k| *k == self)
            .expect("in AUDIT_MISMATCH_ALL")
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Epilogue => "epilogue",
            Self::WriteValue => "write_value",
            Self::CoreClocks => "core_clocks",
            Self::Instructions => "instructions",
            Self::BusClocks => "bus_clocks",
            Self::Unusable => "unusable",
        }
    }
}

/// Why a memo was retired, one named lane per cause (plan section 7.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RetireCause {
    A20,
    CodeWatch,
    CodeMarkEpoch,
    Audit,
}

pub(crate) const RETIRE_CAUSE_ALL: [RetireCause; 4] = [
    RetireCause::A20,
    RetireCause::CodeWatch,
    RetireCause::CodeMarkEpoch,
    RetireCause::Audit,
];

impl RetireCause {
    fn index(self) -> usize {
        RETIRE_CAUSE_ALL
            .iter()
            .position(|k| *k == self)
            .expect("in RETIRE_CAUSE_ALL")
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::A20 => "a20",
            Self::CodeWatch => "code_watch",
            Self::CodeMarkEpoch => "code_mark_epoch",
            Self::Audit => "audit",
        }
    }
}

// ---------------------------------------------------------------------------
// Write classification
// ---------------------------------------------------------------------------

/// One journaled write, keyed by its pre-resolved ALIGNED PHYSICAL dword
/// (plan Revision 2 item 18 / R2.20(c): never a linear address, never
/// unaligned -- `peek_direct_ram` declines a misaligned access, and
/// pre-resolving at record time is what makes an eventual answer-time
/// compare cheap).
#[derive(Clone, PartialEq, Eq, Debug)]
struct WriteObs {
    linear: u32,
    /// The `SS` selector IN FORCE at the moment of this write (Fable review 2026-09-03,
    /// finding 5): needed at tally time to pick the RIGHT stack tracker (the trip may carry
    /// several concurrent stack segments -- client, host, V86 excursion) rather than always
    /// comparing against the entry segment's own tracker regardless of which segment the
    /// write actually fell in.
    ss_selector: u16,
    /// The value of the WHOLE aligned dword immediately before this trip's FIRST write to
    /// it, peeked directly. Two consumers: the audit's formula (plan R2.18 needs "the value
    /// the address held at trip entry" for EVERY written address, including ones the trip
    /// never read), and the Class R test, which is a compare of pre against post over the
    /// bytes actually written.
    pre_dword: Option<u32>,
    /// The dword as this trip has left it so far, and the MASK of the bytes it wrote.
    ///
    /// **Accumulated, not overwritten.** A trip that writes two different fields of the
    /// same aligned dword -- a byte here, a word there -- produces two journal entries at
    /// one key, and keeping only the LAST loses the first: the memo then replays half the
    /// dword and leaves the other half holding whatever the previous trip put there. That
    /// is silent guest-memory corruption, and it is what the fixture-scale AUDIT caught on
    /// `tyrian-specs-586` (2026-09-03): `audit_mismatch[write_value]` fired on 13,768 of
    /// 13,768 audited trips while the epilogue, the core clocks and the instruction count
    /// all agreed exactly.
    latest_dword: u32,
    mask: u32,
    class: AddressClass,
}

impl WriteObs {
    /// Splice one write's bytes into the accumulated dword.
    fn apply(&mut self, offset: u32, width_bytes: u32, value: u32) {
        let (byte_mask, shifted) = splice(offset, width_bytes, value);
        self.latest_dword = (self.latest_dword & !byte_mask) | shifted;
        self.mask |= byte_mask;
    }
}

/// The byte mask this access covers inside its aligned dword, and its value shifted into
/// place. An access running off the end of the dword is CLIPPED here; the seam that split
/// it hands the tail in as its own fragment.
fn splice(offset: u32, width_bytes: u32, value: u32) -> (u32, u32) {
    let bytes = width_bytes.min(4 - offset);
    let sub = if bytes >= 4 {
        u32::MAX
    } else {
        (1u32 << (bytes * 8)) - 1
    };
    let byte_mask = sub << (offset * 8);
    (byte_mask, (value & sub) << (offset * 8))
}

/// One TLB / decode-cache invalidation effect the trip caused, captured in TRIP ORDER
/// at the CPU's own seams and REPLAYED at answer time through those same functions
/// (slice1 plan R2.5). A real `AH=0Bh` trip runs `flush_tlb_and_code_caches_for_cr3_write`
/// twice (the VCPI CR3 pair, `cr3_writes_per_trip` median 2), and an answered trip that
/// skipped it would leave the TLB holding entries the real trip retired -- so a guest
/// that edits a page table and relies on the reflected call's mode switch to publish the
/// edit would read a stale translation.
///
/// `Cr3Write` carries the value ALREADY MASKED the way `MOV CR3`'s executor masks it,
/// because that is what `flush_tlb_and_code_caches_for_cr3_write` stores into
/// `control.cr3` -- which makes "replaying the effects reproduces the exit image's CR3"
/// an exact equality the memo build checks rather than an argument.
///
/// A CR0 write is deliberately NOT a variant: see `note_cr0_write`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ControlEffect {
    Cr3Write(u32),
    Invlpg(u32),
}

// ---------------------------------------------------------------------------
// The open trip
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Slot {
    Warm,
    JournalA,
    JournalB,
    Natural(u8),
    /// An AUDIT trip: the screens all passed and the memo WOULD have answered, and the
    /// answer was refused on purpose so the real trip's end state can be compared with
    /// the memo's prediction (plan R2.7). It leaves the key's own learn slot untouched.
    Audit,
}

impl From<SlotState> for Slot {
    fn from(s: SlotState) -> Slot {
        match s {
            SlotState::Warm => Slot::Warm,
            SlotState::JournalA => Slot::JournalA,
            SlotState::JournalB => Slot::JournalB,
            SlotState::Natural(i) => Slot::Natural(i),
        }
    }
}

impl Slot {
    fn journals(self) -> bool {
        matches!(self, Slot::JournalA | Slot::JournalB | Slot::Audit)
    }
}

/// A minimal per-trip stack tracker: reuses the shared `StackTrack`
/// primitive, capped small (a reflected trip's client/host/V86-excursion
/// stacks rarely exceed a handful of concurrent segments; 0c's own
/// instrument capped at 12 after raising it from 4).
const MAX_STACK_SEGMENTS: usize = 12;

#[derive(Clone)]
struct OpenTrip {
    key: MemoKey,
    slot: Slot,
    entry_image: EntryImage,
    return_cs_selector: u16,
    return_eip: u32,
    entry_ss_selector: u16,
    entry_esp: u32,
    entry_ss_big: bool,
    entry_persona: CpuPersona,
    open_elapsed_clocks: u64,
    open_timing_rem: u64,
    open_instructions: u64,
    open_bus_raw: u64,
    stacks: [Option<StackTrack>; MAX_STACK_SEGMENTS],
    stack_segments_over_cap: bool,
    // Journal-only (Slot::JournalA/JournalB); unused for Warm/Natural.
    journaling: bool,
    /// Aligned phys dword -> (first-seen dword value, MASK of the bytes actually read).
    reads: HashMap<u32, (u32, u32)>,
    writes: HashMap<u32, WriteObs>, // aligned phys dword -> observation
    translations: HashMap<u32, (u32, u32)>, // pde/pte aligned phys dword -> (value, mask)
    read_set_over_cap: bool,
    translation_set_over_cap: bool,
    hazard: Option<LearnRefused>,
    nested_int_count: u32,
    /// Every nested `INT n` inside this trip, in TRIP ORDER, as the
    /// `(vector, ax)` pair its `interrupt_acknowledge` was issued with (plan R2.6).
    /// `MachineBus::interrupt_acknowledge` records a bus-trace entry with I/O wait
    /// states, posts `pending_soft_int` for the program-runtime vectors and stashes
    /// `last_int_vector`, so eliding these acks would silently drop a device side
    /// effect the real trip made. Recorded on EVERY trip, not only a journaled one:
    /// the acks are part of the trip's effect regardless of which learn slot observed
    /// it, and the memo is built from the JournalB trip's copy.
    nested_acks: Vec<(u8, u16)>,
    nested_acks_over_cap: bool,
    /// The trip's TLB/decode invalidations, in trip order (plan R2.5).
    control_effects: Vec<ControlEffect>,
    control_effects_over_cap: bool,
    /// The memo this trip is auditing (`Slot::Audit` only).
    audit: Option<Arc<Memo>>,
    hw_interrupt_seen: bool,
    /// Plan 4.4, the live stack tail: the physical address and WORD value at
    /// `[entry_esp - 2, entry_esp)` sampled at trip OPEN, before the `INT`'s own push --
    /// generic scratch content, unrelated to this trip. Compared at close against the same
    /// address's live value to decide whether the `RETF`-with-flags shape's `INT`-pushed
    /// FLAGS word needs to be in `memo.replay` (it always does in practice, but the compare
    /// is the generic rule the plan states, not a hardcoded "always emit").
    entry_tail: Option<(u32, u32)>,
}

// `OpenTrip` participates in `CpuGsw`'s derived `PartialEq`/`Eq`/`Clone` only
// because it is reachable through `CpuGsw::reflected_call`. Its contents are
// host bookkeeping about a trip in flight, not architectural guest state, so
// equality is defined as "both sides have some open trip, or neither does" --
// the rest of `CpuGsw`'s derived fields carry the real architectural compare.
impl PartialEq for OpenTrip {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}
impl Eq for OpenTrip {}
impl std::fmt::Debug for OpenTrip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenTrip")
            .field("key", &self.key)
            .field("slot", &self.slot)
            .finish_non_exhaustive()
    }
}

fn aligned_dword(phys: u32) -> u32 {
    phys & !0x3
}

/// R2.10 item 10/11: refuse a trip entered with `CR4.VME|PVI` set,
/// `EFLAGS.TF` set, or `DR7` enabling a breakpoint.
const CR4_VME: u32 = 1 << 0;
const CR4_PVI: u32 = 1 << 1;
const EFLAGS_TF: u32 = 1 << 8;

fn dr7_enables_a_breakpoint(dr7: u32) -> bool {
    (dr7 & 0b1111) != 0
}

fn entry_hazard(image: &EntryImage) -> Option<LearnRefused> {
    if image.cr4 & (CR4_VME | CR4_PVI) != 0 {
        return Some(LearnRefused::VmeOrPvi);
    }
    if image.eflags_masked & EFLAGS_TF != 0 || dr7_enables_a_breakpoint(image.dr7) {
        return Some(LearnRefused::DebugState);
    }
    None
}

impl OpenTrip {
    fn start<B: CpuBus>(cpu: &CpuGsw, bus: &B, key: MemoKey, slot: Slot) -> Self {
        let regs = &cpu.registers;
        let cs = regs.cs();
        let ss = regs.segment(SegmentIndex::Ss);
        let entry_image = EntryImage::capture(cpu);
        let entry_esp = regs.esp();
        let mut stacks: [Option<StackTrack>; MAX_STACK_SEGMENTS] = [None; MAX_STACK_SEGMENTS];
        stacks[0] = Some(StackTrack {
            selector: ss.selector,
            base: ss.base,
            limit: ss.limit,
            low_water_esp: entry_esp,
            last_esp: entry_esp,
        });
        let hazard = entry_hazard(&entry_image);
        let tail_linear = ss.base.wrapping_add(entry_esp.wrapping_sub(2));
        let entry_tail = probe_physical(cpu, bus, tail_linear)
            .and_then(|(phys, _walk)| bus.peek_direct_ram(phys, BusWidth::Word).map(|v| (phys, v)));
        OpenTrip {
            key,
            slot,
            entry_image,
            return_cs_selector: cs.selector,
            return_eip: regs.eip,
            entry_ss_selector: ss.selector,
            entry_esp,
            entry_ss_big: ss.default_size_32,
            entry_persona: cpu.persona(),
            open_elapsed_clocks: cpu.elapsed_clocks,
            open_timing_rem: cpu.reflected_call_timing_rem(),
            open_instructions: cpu.perf.instructions,
            open_bus_raw: bus.cumulative_raw_bus_clocks(),
            stacks,
            stack_segments_over_cap: false,
            journaling: slot.journals(),
            reads: HashMap::new(),
            writes: HashMap::new(),
            translations: HashMap::new(),
            read_set_over_cap: false,
            translation_set_over_cap: false,
            hazard,
            nested_int_count: 0,
            nested_acks: Vec::new(),
            nested_acks_over_cap: false,
            control_effects: Vec::new(),
            control_effects_over_cap: false,
            audit: None,
            hw_interrupt_seen: false,
            entry_tail,
        }
    }

    fn width_mask(&self) -> u32 {
        if self.entry_ss_big {
            0xFFFF_FFFF
        } else {
            0xFFFF
        }
    }

    fn sp_at_entry_width(&self, esp: u32) -> u32 {
        if self.entry_ss_big {
            esp
        } else {
            u32::from(esp as u16)
        }
    }

    fn entry_sp_at_width(&self) -> u32 {
        self.sp_at_entry_width(self.entry_esp)
    }

    /// Rule 1 -- the return-match close, plan section 4.1/D1: `RETF`/`IRET`
    /// landing on the entry's own CS:EIP:SS with SP == entry SP (`IRET` or a
    /// non-flags-leaving `RETF`; `Some(false)`), OR the `RETF`-with-flags arm
    /// (SP == entry SP - 2 at the entry width; `Some(true)`). `None` when
    /// this boundary is not this trip's own matching return.
    fn is_return_match(&self, cpu: &CpuGsw) -> Option<bool> {
        let regs = &cpu.registers;
        let cs = regs.cs();
        let ss = regs.segment(SegmentIndex::Ss);
        let esp = regs.esp();
        if cs.selector != self.return_cs_selector
            || regs.eip != self.return_eip
            || ss.selector != self.entry_ss_selector
        {
            return None;
        }
        let sp_here = self.sp_at_entry_width(esp);
        let entry_sp = self.entry_sp_at_width();
        if sp_here == entry_sp {
            return Some(false);
        }
        if sp_here == entry_sp.wrapping_sub(2) & self.width_mask() {
            return Some(true);
        }
        None
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
        self.stack_segments_over_cap = true;
    }

    /// Class D (plan section 4.3): below this trip's own observed low-water
    /// mark for the stack segment the write fell in.
    fn is_dead_stack(&self, seg_selector: u16, linear: u32) -> bool {
        for seg in self.stacks.iter().flatten() {
            if seg.selector != seg_selector {
                continue;
            }
            let addr_from_base = linear.wrapping_sub(seg.base);
            let low_from_base = seg.low_water_esp.wrapping_sub(seg.base);
            return addr_from_base < low_from_base;
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Per-key state and aggregate statistics
// ---------------------------------------------------------------------------

/// Streaming raw-clock stability statistics, per key (R2.20(a) / plan
/// Revision 2 amendments item 5(a)): fraction of learn attempts whose 8
/// natural samples all agree, the number of DISTINCT `(raw_core, raw_bus,
/// insns)` triples observed (capped, since in practice this is tiny), and
/// the longest run of consecutive identical triples across the whole
/// natural-trip stream for this key.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct StabilityAcc {
    pub total_attempts: u64,
    pub stable_attempts: u64,
    pub total_natural_trips: u64,
    distinct_triples: HashMap<(u64, u64, u64), u64>,
    pub longest_run: u64,
    current_run: u64,
    last_triple: Option<(u64, u64, u64)>,
}

const MAX_DISTINCT_TRIPLES_TRACKED: usize = 64;

impl StabilityAcc {
    fn observe_natural_sample(&mut self, triple: (u64, u64, u64)) {
        self.total_natural_trips += 1;
        if self.distinct_triples.len() < MAX_DISTINCT_TRIPLES_TRACKED
            || self.distinct_triples.contains_key(&triple)
        {
            *self.distinct_triples.entry(triple).or_insert(0) += 1;
        }
        if self.last_triple == Some(triple) {
            self.current_run += 1;
        } else {
            self.current_run = 1;
        }
        self.longest_run = self.longest_run.max(self.current_run);
        self.last_triple = Some(triple);
    }

    fn finish_attempt(&mut self, stable: bool) {
        self.total_attempts += 1;
        if stable {
            self.stable_attempts += 1;
        }
    }

    pub(crate) fn distinct_triple_count(&self) -> usize {
        self.distinct_triples.len()
    }

    pub(crate) fn stable_fraction(&self) -> f64 {
        if self.total_attempts == 0 {
            0.0
        } else {
            self.stable_attempts as f64 / self.total_attempts as f64
        }
    }
}

/// Cheap running median/min/max over a bounded reservoir, used for the
/// per-key write/read-set-size report (R2.20(b)).
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct SizeStats {
    pub count: u64,
    pub min: u32,
    pub max: u32,
    sample: Vec<u32>,
}

const SIZE_STATS_RESERVOIR: usize = 4096;

impl SizeStats {
    fn observe(&mut self, v: u32) {
        if self.count == 0 {
            self.min = v;
            self.max = v;
        } else {
            self.min = self.min.min(v);
            self.max = self.max.max(v);
        }
        self.count += 1;
        if self.sample.len() < SIZE_STATS_RESERVOIR {
            self.sample.push(v);
        }
    }

    pub(crate) fn median(&self) -> Option<u32> {
        if self.sample.is_empty() {
            return None;
        }
        let mut s = self.sample.clone();
        s.sort_unstable();
        Some(s[s.len() / 2])
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum SlotState {
    #[default]
    Warm,
    JournalA,
    JournalB,
    Natural(u8),
}

/// The structural comparison baseline captured from Journal A, per plan
/// section 4.2 point 2: read set (address -> value), translation set, write
/// set (address -> observation), instruction count, exit image.
#[derive(Clone, PartialEq, Eq, Debug)]
struct JournalSnapshot {
    /// Amendment 2: JournalA's own entry image. `compare_journal` checks this against
    /// JournalB's entry image -- two occurrences of the same `MemoKey` whose entry states
    /// genuinely differ (say, DS) must never silently agree just because the read set
    /// happened not to expose the difference; this is the same closure-rule concern
    /// BLOCKING finding 1 fixed for the image fields, applied to the A-vs-B compare itself.
    entry_image: EntryImage,
    reads: HashMap<u32, (u32, u32)>,
    translations: HashMap<u32, (u32, u32)>,
    writes: HashMap<u32, WriteObs>,
    insns: u64,
    exit_image: EntryImage,
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct KeyState {
    slot: SlotState,
    consecutive_failures: u32,
    pub disarmed: bool,
    pending_journal: Option<JournalSnapshot>,
    /// JournalB's classified write set, carried across the 8 Natural samples (which run with
    /// journaling OFF and so cannot re-observe writes) so a `Memo` can be assembled once the
    /// clocks are confirmed stable, without re-deriving classification from data the Natural
    /// trips never collected.
    confirmed: Option<ConfirmedTrip>,
    natural_samples: Vec<(u64, u64, u64)>,
    /// Every `INT` occurrence `on_int` identified as this key, whether it opened a trip or was
    /// dropped because the key is disarmed (Fable review 2026-09-03, finding 6(iii): without
    /// this the JSON cannot show that a key was seen 727,000 times while only 44 of them ever
    /// became a tracked trip).
    pub trips_seen: u64,
    /// Of `trips_seen`, how many were dropped specifically because `disarmed` was already true.
    pub disarmed_returns: u64,
    /// `trips_seen` at the moment this key was last disarmed; `on_int` re-arms once
    /// `trips_seen` has advanced by `MEMO_REARM_TRIPS_SEEN` past this (Fable re-review,
    /// 2026-09-03: "the disarm is never permanent... a budget spent in a menu cannot blind
    /// the dwell").
    disarmed_at_trips_seen: u64,
    /// Times this key has been re-armed after a permanent-looking disarm.
    pub rearms: u64,
    pub learn_attempts: u64,
    pub learned: u64,
    pub learn_refused: [u64; 25],
    /// Answers this key has applied since its last audit, and the signed bus-clock drift
    /// the memo has accumulated against what the audits actually observed (amendment 6).
    answers_since_audit: u64,
    bus_drift_acc: i64,
    pub audits: u64,
    /// Runtime-audit `write_value` strikes (option D). ONE strike retires and re-learns --
    /// with option E in place a single strike is most likely a cadence longer than the
    /// eight natural samples, which the next learn cycle's demotion may well capture. TWO
    /// strikes means the key is not memoisable and it stops paying for attempts.
    write_value_strikes: u32,
    /// Times this key was disarmed by the two-strikes rule (bounded: `maybe_rearm` still
    /// re-arms it after `MEMO_REARM_TRIPS_SEEN`).
    pub write_value_disarms: u64,
    /// The distinct read-set cells that have made this key's memo fall through, with the
    /// value the memo pinned, the value found live, and how often. Capped: a handful of
    /// addresses is what a real "this input is not actually an input" looks like, and an
    /// unbounded map would be a leak on a key whose whole read set is unstable.
    read_set_offenders: Vec<(u32, u32, u32, u64)>,
    /// The same dump for the AUDIT's write formula: the cell whose predicted final value
    /// disagreed with the observed one, what was predicted, what was found.
    audit_offenders: Vec<(u32, u32, u32, u64, u32)>,
    pub write_class_r_pinned: u64,
    pub write_class_r_unpinned: u64,
    pub write_class_d: u64,
    pub write_class_w_other: u64,
    pub stability: StabilityAcc,
    pub write_set_size: SizeStats,
    pub read_set_size: SizeStats,
}

/// How many distinct read-set offenders one key reports (see `read_set_offenders`).
const MEMO_READ_SET_OFFENDERS_TRACKED: usize = 8;

impl KeyState {
    /// `provenance` says WHERE the offending dword came from, which is the one bit that
    /// separates the two live hypotheses for a write-value divergence: bit 0, the memo
    /// REPLAYS this dword (so a stale replay value is being written over live state); bit
    /// 1, the audited trip's own journal saw a write to it (so the trip really does write
    /// it and the memo's classification is what is wrong). Neither bit set means the dword
    /// reached the comparison from the replay set alone with no live write behind it.
    fn note_audit_offender(
        &mut self,
        (phys, predicted, observed): (u32, u32, u32),
        provenance: u32,
    ) {
        if let Some(entry) = self.audit_offenders.iter_mut().find(|e| e.0 == phys) {
            entry.1 = predicted;
            entry.2 = observed;
            entry.3 += 1;
            entry.4 |= provenance;
            return;
        }
        if self.audit_offenders.len() < MEMO_READ_SET_OFFENDERS_TRACKED {
            self.audit_offenders
                .push((phys, predicted, observed, 1, provenance));
        }
    }

    fn note_read_set_offender(&mut self, (phys, expected, live): (u32, u32, u32)) {
        if let Some(entry) = self.read_set_offenders.iter_mut().find(|e| e.0 == phys) {
            entry.1 = expected;
            entry.2 = live;
            entry.3 += 1;
            return;
        }
        if self.read_set_offenders.len() < MEMO_READ_SET_OFFENDERS_TRACKED {
            self.read_set_offenders.push((phys, expected, live, 1));
        }
    }

    /// Plan section 4.2 point 2, restored by the Fable review (2026-09-03): "`journal_mismatch`
    /// is NOT structurally cached (it can become learnable)" -- only `ClocksUnstable` counts
    /// toward `MEMO_LEARN_BUDGET`'s CONSECUTIVE-failure disarm. Every other refusal reason
    /// (a structural journal mismatch, a boundary/cap refusal, a hazard) resets the key to
    /// `Warm` and re-learns on the next matching trip, exactly like a success, without ever
    /// touching the budget. This is the fix for the defect the review's traced counters
    /// found: the dwell's own dominant keys were disarming after 4 attempts on
    /// `journal_mismatch`/`clocks_unstable` mixed reasons and then dropping ~1.46 M returns
    /// for the rest of the run.
    fn record_failure(&mut self, reason: LearnRefused) {
        self.learn_refused[reason.index()] += 1;
        if reason == LearnRefused::ClocksUnstable {
            self.consecutive_failures += 1;
            if self.consecutive_failures >= MEMO_LEARN_BUDGET {
                self.disarmed = true;
                self.disarmed_at_trips_seen = self.trips_seen;
            }
        }
        self.slot = SlotState::Warm;
        self.pending_journal = None;
        self.confirmed = None;
        self.natural_samples.clear();
    }

    /// Fable re-review, 2026-09-03, campaign verdict (2c): "disarm is never permanent -- re-arm
    /// after `trips_seen` advances by 2^16, so a budget spent in a menu cannot blind the
    /// dwell." Called from `on_int` before the disarmed-drop check; a no-op unless the key is
    /// both disarmed and has seen `MEMO_REARM_TRIPS_SEEN` more trips since it was disarmed.
    fn maybe_rearm(&mut self) {
        if self.disarmed
            && self.trips_seen.saturating_sub(self.disarmed_at_trips_seen) >= MEMO_REARM_TRIPS_SEEN
        {
            self.disarmed = false;
            self.consecutive_failures = 0;
            self.slot = SlotState::Warm;
            self.pending_journal = None;
            self.confirmed = None;
            self.natural_samples.clear();
            self.rearms += 1;
        }
    }

    fn record_success_and_reset(&mut self) {
        self.consecutive_failures = 0;
        self.slot = SlotState::Warm;
        self.pending_journal = None;
        self.natural_samples.clear();
        // `confirmed` is deliberately NOT cleared here: the `Slot::Natural` success arm reads
        // it via `take()` before calling this, so by the time this runs it is already `None`.
    }
}

// ---------------------------------------------------------------------------
// The per-CPU state
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ReflectedCallMemoState {
    keys: HashMap<MemoKey, KeyState>,
    open: Option<OpenTrip>,
    /// The answer-path memo cache: per key, up to `MEMO_IMAGES_PER_KEY` learned memos, most
    /// recently used last (plan section 3). Empty on the record-and-measure build; the
    /// answer-path commit populates it once a learn cycle's 8 natural samples all agree.
    /// `Arc` rather than a bare `Memo`: the answer path must hold the memo while it
    /// MUTATES the `CpuGsw` that owns this cache, and an `Arc` clone is one refcount
    /// bump against the several-hundred-cell deep copy a `Memo` clone would be at
    /// 726,000 answers per guest second.
    pub(crate) memos: HashMap<MemoKey, Vec<Arc<Memo>>>,
    /// `reflected_call_a20_retires` (plan Revision 2 amendments, item A): incremented by the
    /// COUNT of memos discarded, every time `retire_all_memos` runs.
    pub(crate) a20_retires: u64,
    /// Plan section 7.4's `fell_through[]` surface: the screens 3 and 6 lanes
    /// (`not_memoised`, `entry_state_mismatch`, `read_set_mismatch`,
    /// `read_set_unreadable`); the later screens' lanes are the `fell_through_*` fields
    /// below. `would_answer` counts a screens-3-and-6 PASS, so `would_answer - answered`
    /// is exactly what the pending-interrupt screen, the clamp and the observer test
    /// refused -- a number no single lane carries.
    pub(crate) not_memoised: u64,
    pub(crate) entry_state_mismatch: u64,
    pub(crate) read_set_mismatch: u64,
    pub(crate) read_set_unreadable: u64,
    pub(crate) would_answer: u64,
    /// Answer-path counters (plan section 7.4). `answered` counts trips the memo
    /// actually applied; the `fell_through_*` lanes are one per screen.
    pub(crate) answered: u64,
    pub(crate) insns_elided: u64,
    pub(crate) core_clocks_charged: u64,
    pub(crate) bus_clocks_charged: u64,
    pub(crate) fell_through_pending_interrupt: u64,
    pub(crate) fell_through_cap: u64,
    pub(crate) fell_through_device_edge: u64,
    pub(crate) fell_through_dma_visible: u64,
    pub(crate) fell_through_not_armed: u64,
    pub(crate) fell_through_clock_projection: u64,
    pub(crate) fell_through_soft_int_posted: u64,
    /// Memos retired because a guest write landed on a physical range one of their trips
    /// fetched code from (plan R2.4 / BLOCKING finding 4).
    pub(crate) code_watch_retires: u64,
    /// Memos retired because the decode cache cleared its SMC marks wholesale, so the
    /// marks the overlap retire above depends on no longer stand.
    pub(crate) code_mark_epoch_retires: u64,
    /// The `DecodeCache::code_mark_epoch` every live memo was built under. A memo cache
    /// is only ever populated under ONE epoch: the first build after a bump retires the
    /// rest, so this single field stands for every memo in the map.
    pub(crate) code_mark_epoch: u64,
    /// `IZARRAVM_REFLECTED_CALL_MEMO_AUDIT`, read ONCE when this state is created.
    pub(crate) audit_period: u64,
    /// Trips refused on purpose so the memo's prediction could be compared against the
    /// real thing, and what they disagreed about.
    pub(crate) audited: u64,
    pub(crate) audit_mismatch: [u64; 6],
    /// Memos retired, per cause.
    pub(crate) retired: [u64; 4],
    /// Memos a retire made room for and a later learn cycle rebuilt.
    pub(crate) relearned: u64,
    /// Audits forced by the drift accumulator rather than by the period (amendment 6).
    pub(crate) drift_forced_audits: u64,
}

impl Default for ReflectedCallMemoState {
    fn default() -> Self {
        ReflectedCallMemoState {
            keys: HashMap::new(),
            open: None,
            memos: HashMap::new(),
            a20_retires: 0,
            not_memoised: 0,
            entry_state_mismatch: 0,
            read_set_mismatch: 0,
            read_set_unreadable: 0,
            would_answer: 0,
            answered: 0,
            insns_elided: 0,
            core_clocks_charged: 0,
            bus_clocks_charged: 0,
            fell_through_pending_interrupt: 0,
            fell_through_cap: 0,
            fell_through_device_edge: 0,
            fell_through_dma_visible: 0,
            fell_through_not_armed: 0,
            fell_through_clock_projection: 0,
            fell_through_soft_int_posted: 0,
            code_watch_retires: 0,
            code_mark_epoch_retires: 0,
            code_mark_epoch: 0,
            // Read ONCE, here, exactly as the arm knob is: the answer path afterwards only
            // ever reads this field.
            audit_period: knob_audit_period(),
            audited: 0,
            audit_mismatch: [0; 6],
            retired: [0; 4],
            relearned: 0,
            drift_forced_audits: 0,
        }
    }
}

/// Why a candidate memo did not answer (plan section 5, screens 2/3/6): the entry-image and
/// physical read/translation-set compare, screened BEFORE any mutation (the fall-through
/// invariant -- "any mismatch = fall through to the guest with zero state changed").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FellThrough {
    NotMemoised,
    EntryStateMismatch,
    ReadSetMismatch,
    ReadSetUnreadable,
    PendingInterrupt,
    Cap,
    DeviceEdge,
    DmaVisible,
    NotArmed,
    ClockProjection,
    SoftIntPosted,
}

/// Screen ONE candidate memo (plan section 5, steps 3 and 6): compare all 43 entry-image
/// fields, then the pre-resolved physical read set, then the translation set, against LIVE
/// memory -- read-only, no mutation anywhere in this function. `Ok(())` means every screen
/// passed; the caller applies nothing here (screens 1/2/4/5/7/8 -- knob, bucket, pending
/// interrupt, clamp, observer test, apply order -- are amendment 3's, once a pass means
/// something).
/// `screen_memo` with the offending CELL reported. The plan asks for exactly this dump
/// ("naming the address is still worth one per-address dump before the answer path
/// lands"): a read-set miss is the mechanism's dominant fall-through, and a bare counter
/// says only that some input moved, never WHICH -- which is the difference between a
/// screen that is doing its job and a memo pinning a cell that was never an input.
pub(crate) fn screen_memo_detailed<B: CpuBus>(
    cpu: &CpuGsw,
    bus: &B,
    memo: &Memo,
    offender: &mut Option<(u32, u32, u32)>,
) -> Result<(), FellThrough> {
    let live_image = EntryImage::capture(cpu);
    if live_image != memo.image {
        return Err(FellThrough::EntryStateMismatch);
    }
    for &(phys, expected, mask) in memo.reads.iter().chain(memo.translations.iter()) {
        match bus.peek_direct_ram(phys, BusWidth::Dword) {
            None => {
                *offender = Some((phys, expected, 0));
                return Err(FellThrough::ReadSetUnreadable);
            }
            Some(live) if (live ^ expected) & mask != 0 => {
                *offender = Some((phys, expected & mask, live & mask));
                return Err(FellThrough::ReadSetMismatch);
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// Screen every cached memo for `key`, most-recently-inserted first (plan section 3's LRU),
/// returning the first that passes. `None` with no memo cached at all is
/// `FellThrough::NotMemoised`; `None` with candidates present but none matching is the LAST
/// candidate's own reason (arbitrary among misses, since none of them will answer either
/// way -- only used for a counter bucket, never for control flow in this commit).
pub(crate) fn screen_key_detailed<'a, B: CpuBus>(
    cpu: &CpuGsw,
    bus: &B,
    memos: &'a [Arc<Memo>],
    offender: &mut Option<(u32, u32, u32)>,
) -> Result<&'a Arc<Memo>, FellThrough> {
    if memos.is_empty() {
        return Err(FellThrough::NotMemoised);
    }
    let mut last_reason = FellThrough::NotMemoised;
    for memo in memos.iter().rev() {
        match screen_memo_detailed(cpu, bus, memo, offender) {
            Ok(()) => return Ok(memo),
            Err(reason) => last_reason = reason,
        }
    }
    Err(last_reason)
}

impl ReflectedCallMemoState {
    /// Plan Revision 2 amendments, item A (BLOCKING): A20 is neither a register nor memory --
    /// it changes the PHYSICAL address every linear access resolves to -- while every memo's
    /// read/translation/replay set was pre-resolved to physical at record time under
    /// whatever A20 state was in force then. A memo learned with the gate open and answered
    /// with it closed (or vice versa) would compare and replay the WRONG physical cells.
    /// Rather than add a 44th image field, every memo -- for every key -- retires the instant
    /// the gate changes. Called from `CpuGsw::note_a20_changed`, the single production seam
    /// (`izarravm-machine/src/run.rs:2023`); a coarse whole-cache flush is fine, exactly like
    /// the code-cache/decode-cache invalidation A20 already triggers there.
    pub(crate) fn retire_all_memos(&mut self) {
        let retired = self.clear_all_memos();
        self.a20_retires += retired;
        self.retired[RetireCause::A20.index()] += retired;
    }

    fn clear_all_memos(&mut self) -> u64 {
        let mut retired = 0u64;
        for memos in self.memos.values_mut() {
            retired += memos.len() as u64;
            memos.clear();
        }
        retired
    }
}

/// One learned answer-path memo (plan section 3): everything the answer path needs to
/// reproduce a trip without running it. `image` is the ENTRY state (screen 3's comparison
/// baseline); `epilogue` is the EXIT state captured the same way (`EntryImage::capture`),
/// reused verbatim rather than a separate hand-rolled struct, since the two share every
/// field the epilogue must restore (all six segments with cached descriptors, EFLAGS, the
/// GPRs including ESP) plus fields the epilogue does not need (CR0/CR3/CR4/CPL/VM, pinned
/// equal to the entry's own by the control-register-delta refusal already enforced at learn
/// time) -- reusing the type costs a few unread fields, not a soundness gap.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Memo {
    pub(crate) image: EntryImage,
    /// Pre-resolved aligned physical dword -> (expected value, MASK of the bytes the trip
    /// actually read), the inputs (plan section 3/5.6). The mask is what keeps the screen
    /// from pinning bytes the trip never looked at -- see `note_read`.
    pub(crate) reads: Box<[(u32, u32, u32)]>,
    /// Pre-resolved aligned physical PDE/PTE dword -> (value, mask) (plan section 5.6).
    pub(crate) translations: Box<[(u32, u32, u32)]>,
    /// The EXIT architectural state, reused for the epilogue (see the struct doc above).
    pub(crate) epilogue: EntryImage,
    /// `EIP` the epilogue sets: `int_eip + insn_len`, constant for a given key (plan 5.8d).
    pub(crate) return_eip: u32,
    /// Class W: deterministic net writes plus the live stack tail (plan 4.3/4.4), applied
    /// at answer time through `CpuGsw::write_physical_replay`. Aligned physical dword, the
    /// MASK of the bytes the trip wrote, and the dword's post-trip value -- so a replay
    /// reproduces exactly the bytes the trip touched, however many separate writes made
    /// them, and never clobbers a neighbour it did not.
    pub(crate) replay: Box<[(u32, u32, u32)]>,
    /// Class R ranges, coalesced, for the answer-time observer test (plan 5.7 / review A.4):
    /// (physical_lo, physical_hi_inclusive).
    pub(crate) class_r_ranges: Box<[(u32, u32)]>,
    pub(crate) raw_core_clocks: u64,
    pub(crate) raw_bus_clocks: u64,
    pub(crate) insns: u64,
    /// Nested `interrupt_acknowledge` pairs, in TRIP ORDER, re-issued as the answer's
    /// FIRST mutation (plan R2.6).
    pub(crate) nested_acks: Box<[(u8, u16)]>,
    /// TLB/decode control effects, in trip order, replayed AFTER the Class W writes and
    /// BEFORE the epilogue (plan R2.16 item 2: control effects LAST, so a replayed write
    /// into a page-table entry cannot leave a TLB that predates it).
    pub(crate) control_effects: Box<[ControlEffect]>,
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

fn key_for(cpu: &CpuGsw, vector: u8, ax: u16) -> MemoKey {
    let cs = cpu.registers.cs();
    let ss = cpu.registers.segment(SegmentIndex::Ss);
    MemoKey {
        // Saturating rather than truncating: an epoch above 255 is not
        // expressible by the knob, and a wrap would silently make two epochs
        // share a key -- the one thing this field exists to prevent.
        epoch: u8::try_from(cpu.timing_epoch()).unwrap_or(u8::MAX),
        vector,
        ax,
        cs_selector: cs.selector,
        int_eip: cpu.registers.eip,
        ss_selector: ss.selector,
        ss_big: ss.default_size_32,
        cpl: cpu.current_privilege_level(),
        vm: cpu.is_v86_mode(),
    }
}

/// What `on_int` did, so `CpuGsw::software_interrupt` knows whether to run the real
/// trip. `Answered` means the memo APPLIED: every screen passed, the nested acks were
/// re-issued, the Class W writes replayed, the control effects reproduced, the epilogue
/// installed and the clocks charged -- so the caller must return WITHOUT calling
/// `deliver_interrupt`. Every other path is `NotAnswered` and the guest runs the real
/// trip, bit-identically to a build with the knob off.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum IntOutcome {
    Answered,
    NotAnswered,
}

/// `CpuGsw::software_interrupt`'s hook, called immediately AFTER
/// `bus.interrupt_acknowledge(vector, AX)` and BEFORE `deliver_interrupt` -- which is
/// why the outer ack is never replayed (it has already run and is part of the trip's
/// effect either way) while the NESTED ones are.
///
/// Only opens a record for a software `INT` taken from protected mode outside V86
/// (plan section 1's IN scope), vectors `0x10..=0x33` (BIOS/DOS/DPMI). A nested `INT`
/// inside an already-open trip is recorded at ANY vector: its `interrupt_acknowledge`
/// has already been issued on the bus above this hook, so a vector outside the opening
/// window is still an effect the answer must reproduce.
pub(crate) fn on_int<B: CpuBus>(
    cpu: &mut CpuGsw,
    bus: &mut B,
    vector: u8,
) -> ExecResult<IntOutcome> {
    if cpu.reflected_call.is_none() {
        return Ok(IntOutcome::NotAnswered);
    }
    let ax = (cpu.registers.eax() & 0xFFFF) as u16;
    let state = cpu.reflected_call.as_mut().expect("checked above");
    if let Some(open) = state.open.take() {
        // A fresh `INT` while one is already open: either a re-entry (this
        // trip's own signature firing again with SP back at entry) or a
        // genuine nested call. Rule 3 (re-entry) closes without a match --
        // never producing a "learned" outcome, same as rule 2.
        let is_reentry = vector == open.key.vector
            && cpu.registers.cs().selector == open.return_cs_selector
            && cpu.registers.eip == open.return_eip
            && open.sp_at_entry_width(cpu.registers.esp()) == open.entry_sp_at_width();
        let over_budget =
            cpu.perf.instructions.saturating_sub(open.open_instructions) >= MEMO_MAX_TRIP_INSNS;
        if is_reentry || over_budget {
            finish_trip(cpu, &*bus, open, false);
        } else {
            let state = cpu.reflected_call.as_mut().expect("checked above");
            let mut open = open;
            open.nested_int_count = open.nested_int_count.saturating_add(1);
            // Plan R2.6: the pair its `interrupt_acknowledge` was issued with, in trip
            // order. `ax` is read from the SAME register the caller passed
            // (`self.read_gpr16(0)`), one statement above this hook.
            if open.nested_acks.len() >= MEMO_MAX_NESTED_ACKS {
                open.nested_acks_over_cap = true;
            } else {
                open.nested_acks.push((vector, ax));
            }
            state.open = Some(open);
            return Ok(IntOutcome::NotAnswered);
        }
    }
    if !(0x10..=0x33).contains(&vector) {
        return Ok(IntOutcome::NotAnswered);
    }
    if !(cpu.is_protected_mode() && !cpu.is_v86_mode()) {
        return Ok(IntOutcome::NotAnswered);
    }
    let key = key_for(cpu, vector, ax);
    let state = cpu.reflected_call.as_mut().expect("checked above");
    let key_state = state.keys.entry(key).or_default();
    key_state.trips_seen += 1;
    key_state.maybe_rearm();

    // The answer path (plan section 5). Every screen precedes every mutation, and a
    // miss at any screen is a FALL-THROUGH: nothing written, no register moved, the
    // guest runs the real trip.
    if try_answer(cpu, bus, key)?.is_some() {
        return Ok(IntOutcome::Answered);
    }
    // An AUDIT fall-through has already opened its own trip in place of the answer
    // (`open_audit_trip`), and the learn path below must not overwrite it -- the audit
    // grades the memo, the learn cycle builds one, and the two use the same single-trip
    // slot. Every other fall-through leaves `open` untouched and falls into the learn
    // path exactly as before.
    if cpu
        .reflected_call
        .as_ref()
        .is_some_and(|state| state.open.is_some())
    {
        return Ok(IntOutcome::NotAnswered);
    }

    let key_state = cpu
        .reflected_call
        .as_mut()
        .expect("checked above")
        .keys
        .get_mut(&key)
        .expect("just touched above");
    if key_state.disarmed {
        key_state.disarmed_returns += 1;
        return Ok(IntOutcome::NotAnswered);
    }
    let slot: Slot = key_state.slot.into();
    let journaling = slot.journals();
    cpu.reflected_call_journal = journaling;
    let open = OpenTrip::start(cpu, &*bus, key, slot);
    let state = cpu.reflected_call.as_mut().expect("checked above");
    state.open = Some(open);
    // The candidate memo's read set is pinned from LIVE memory at the natural trips'
    // opens, which is the state an answer would screen against. See the function.
    if matches!(slot, Slot::Natural(_)) {
        sample_read_set_at_open(cpu, &*bus, key);
        sample_write_model_at_open(cpu, &*bus, key);
    }
    Ok(IntOutcome::NotAnswered)
}

/// Pin the candidate memo's read set to what its cells hold at the START of an
/// occurrence -- which is what the answer's screen compares against, and which is NOT
/// what the journal trip's own reads observed.
///
/// Called at each of the 8 natural trips' opens. The first sample OVERWRITES the recorded
/// values; the remaining seven must agree with it (under each cell's own byte mask) or the
/// candidate is refused as `read_set_unstable` -- a cell whose pre-trip value is not stable
/// across eight consecutive occurrences is not an input a memo can screen on, and pinning
/// it anyway costs a fall-through on every trip forever.
///
/// **Why this is measured and not derived.** Three derivations were tried against
/// `tyrian-specs-586` on 2026-09-03, and the per-key offender dump refuted each in turn:
/// the journal trip's own READ value fails on physical `0x17b0` (DOS data, 726,123 of
/// 726,134 answer attempts); the POST-trip value fixes `0x17b0` (down to 6) and breaks
/// `0x17cc` instead (726,117), because this title's two dominant keys INTERLEAVE and what
/// `0x17cc` holds before an `AH=0Bh` trip is what the `INT 33h` trip in between wrote; and
/// no rule over one trip's own journal can see that at all. Eight samples of the actual
/// pre-trip state can, and they cost ~150 peeks per learn cycle.
fn sample_read_set_at_open<B: CpuBus>(cpu: &mut CpuGsw, bus: &B, key: MemoKey) {
    let live: Vec<Option<u32>> = {
        let Some(state) = cpu.reflected_call.as_ref() else {
            return;
        };
        let Some(confirmed) = state.keys.get(&key).and_then(|ks| ks.confirmed.as_ref()) else {
            return;
        };
        confirmed
            .reads
            .iter()
            .map(|&(addr, _, _)| bus.peek_direct_ram(addr, BusWidth::Dword))
            .collect()
    };
    let Some(confirmed) = cpu
        .reflected_call
        .as_mut()
        .and_then(|state| state.keys.get_mut(&key))
        .and_then(|ks| ks.confirmed.as_mut())
    else {
        return;
    };
    if !confirmed.read_set_pinned {
        confirmed.read_set_pinned = true;
        for (cell, sampled) in confirmed.reads.iter_mut().zip(live) {
            match sampled {
                Some(value) => cell.1 = value,
                // A cell that is not plain RAM at open would fail the answer's own screen
                // anyway (`read_set_unreadable`); marking it unstable refuses the memo now
                // rather than building one that can never answer.
                None => confirmed.read_set_unstable = true,
            }
        }
        return;
    }
    for (cell, sampled) in confirmed.reads.iter().zip(live) {
        let disagrees = match sampled {
            Some(value) => (value ^ cell.1) & cell.2 != 0,
            None => true,
        };
        if disagrees {
            confirmed.read_set_unstable = true;
            return;
        }
    }
}

/// Option E, half one: peek the write CHECK SET at a natural trip's OPEN.
///
/// This is `sample_read_set_at_open`'s sibling and it rests on the same property:
/// `CpuBus::peek_direct_ram` takes `&self`, so it cannot charge a bus clock or move a
/// counter. The eight natural samples therefore keep measuring NATIVE clocks while also
/// gathering write evidence -- which is the whole reason option E costs no learn latency
/// and no clock sample, where journaling a confirmation trip would cost both.
fn sample_write_model_at_open<B: CpuBus>(cpu: &mut CpuGsw, bus: &B, key: MemoKey) {
    let pre: Vec<Option<u32>> = {
        let Some(state) = cpu.reflected_call.as_ref() else {
            return;
        };
        let Some(confirmed) = state.keys.get(&key).and_then(|ks| ks.confirmed.as_ref()) else {
            return;
        };
        confirmed
            .write_check
            .iter()
            .map(|c| bus.peek_direct_ram(c.dword, BusWidth::Dword))
            .collect()
    };
    if let Some(confirmed) = cpu
        .reflected_call
        .as_mut()
        .and_then(|state| state.keys.get_mut(&key))
        .and_then(|ks| ks.confirmed.as_mut())
    {
        confirmed.write_open_pre = pre;
        confirmed.write_model_pinned = true;
    }
}

/// Option E, half two: at the natural trip's CLOSE, evaluate the AUDIT'S OWN FORMULA
/// against the candidate model, and act on the shape of any disagreement.
///
/// `predicted` is `pre` for a Class R entry (the model says "unchanged") and
/// `(pre & !mask) | (value & mask)` for a Class W one -- the same splice
/// `audit_write_values_disagree` applies to a live `Memo`, here applied to the candidate.
/// Everything is compared UNDER the entry's own mask: bytes the trip never wrote are not
/// part of its model.
///
/// The two actions are option C's demotion rule, on evidence this gathers for free:
///
/// * **A Class R entry disagrees** -- the "restoration" was an accident of phase. DEMOTE it
///   to Class W carrying the observed value. That is what keeps a key answering when its
///   trip really does write a constant; it is the difference between this design and one
///   that simply refuses.
/// * **A Class W entry disagrees** -- its recorded value is not the value it takes. Take the
///   observed value ONCE; a second disagreement means the cell genuinely varies and the
///   learn is refused (`LearnRefused::WriteValueUnstable`). A per-trip counter is not
///   memoisable and no amount of re-taking makes it so.
///
/// **What it cannot see**, written down rather than left implicit: a cadence longer than the
/// eight natural samples, and an address the Journal-A/B pair never saw written at all
/// (the check set is built from that pair, so E walks it rather than the natural trip's
/// writes -- a natural trip has no journal). Both are option D's, the period-64 runtime
/// audit, which is why D stays on permanently rather than being retired by E.
fn confirm_write_model_at_close<B: CpuBus>(cpu: &mut CpuGsw, bus: &B, key: MemoKey) {
    let observed: Vec<Option<u32>> = {
        let Some(state) = cpu.reflected_call.as_ref() else {
            return;
        };
        let Some(confirmed) = state.keys.get(&key).and_then(|ks| ks.confirmed.as_ref()) else {
            return;
        };
        if !confirmed.write_model_pinned {
            return;
        }
        confirmed
            .write_check
            .iter()
            .map(|c| bus.peek_direct_ram(c.dword, BusWidth::Dword))
            .collect()
    };
    let Some(confirmed) = cpu
        .reflected_call
        .as_mut()
        .and_then(|state| state.keys.get_mut(&key))
        .and_then(|ks| ks.confirmed.as_mut())
    else {
        return;
    };
    for (index, check) in confirmed.write_check.iter_mut().enumerate() {
        let (Some(pre), Some(Some(now))) = (
            confirmed.write_open_pre.get(index).copied().flatten(),
            observed.get(index).copied(),
        ) else {
            // A cell that is not plain RAM at one end of the trip cannot be modelled, and
            // the answer's own screens would refuse it anyway.
            confirmed.write_model_unstable = true;
            return;
        };
        let predicted = if check.is_class_r {
            pre
        } else {
            (pre & !check.mask) | (check.value & check.mask)
        };
        if (predicted ^ now) & check.mask == 0 {
            continue;
        }
        if check.retaken {
            confirmed.write_model_unstable = true;
            return;
        }
        check.value = now;
        check.is_class_r = false;
        check.retaken = true;
    }
}

/// Screen and, if every screen passes, APPLY one memo (plan section 5, steps 2-8).
/// `Ok(Some(()))` means answered; `Ok(None)` is a fall-through with ZERO state changed
/// anywhere -- registers, memory, clocks and counters other than the fall-through lane
/// itself. `Err` can only come from a replayed nested ack, and is the fault the real
/// trip would have taken at its first nested `INT`.
fn try_answer<B: CpuBus>(cpu: &mut CpuGsw, bus: &mut B, key: MemoKey) -> ExecResult<Option<()>> {
    // Screen 0: the code watch's wholesale half (plan R2.4). One `u64` compare.
    sync_code_mark_epoch(cpu);
    // Screen 2/3/6: bucket lookup, the 43-field entry-image compare, then the
    // pre-resolved physical read and translation sets against LIVE memory. Read-only.
    let mut offender: Option<(u32, u32, u32)> = None;
    let screened = {
        let state = cpu.reflected_call.as_ref().expect("caller checked");
        match state.memos.get(&key) {
            Some(memos) => screen_key_detailed(cpu, &*bus, memos, &mut offender).map(Arc::clone),
            None => Err(FellThrough::NotMemoised),
        }
    };
    let memo = match screened {
        Ok(memo) => memo,
        Err(reason) => {
            note_fell_through(cpu, reason);
            if let Some(cell) = offender
                && let Some(key_state) = cpu
                    .reflected_call
                    .as_mut()
                    .and_then(|state| state.keys.get_mut(&key))
            {
                key_state.note_read_set_offender(cell);
            }
            return Ok(None);
        }
    };
    cpu.reflected_call
        .as_mut()
        .expect("caller checked")
        .would_answer += 1;

    // Screen 4: the pending-interrupt predicate. An interrupt that is deliverable at
    // this instant must be taken at this instant, not after a lump.
    if bus.interrupt_pending() && cpu.can_take_interrupt() {
        note_fell_through(cpu, FellThrough::PendingInterrupt);
        return Ok(None);
    }

    // Screen 4b (amendment 7): an HLE software interrupt already posted. The machine
    // services it at the next batch boundary, and a lump taken now would run guest work
    // past the instant it was posted at.
    if bus.reflected_call_soft_int_posted() {
        note_fell_through(cpu, FellThrough::SoftIntPosted);
        return Ok(None);
    }

    // Screen 5: the clamp (plan section 6 / R4.2). Project the core charge FIRST --
    // it is the number the gate bounds, and projecting after a mutation would mean
    // discovering an overflow with the answer half applied.
    let Some(scaled_core) = cpu.project_reflected_call_core(memo.raw_core_clocks) else {
        note_fell_through(cpu, FellThrough::ClockProjection);
        return Ok(None);
    };
    let gate = bus.reflected_call_gate(&ReflectedCallGateRequest {
        scaled_core_clocks: scaled_core,
        raw_bus_clocks: memo.raw_bus_clocks,
        run_core_clocks_so_far: cpu.reflected_call_run_core_clocks(),
    });
    if let Err(decline) = gate {
        note_fell_through(
            cpu,
            match decline {
                ReflectedCallDecline::Cap => FellThrough::Cap,
                ReflectedCallDecline::DeviceEdge => FellThrough::DeviceEdge,
                ReflectedCallDecline::DmaVisible => FellThrough::DmaVisible,
                ReflectedCallDecline::NotArmed => FellThrough::NotArmed,
            },
        );
        return Ok(None);
    }

    // Screen 7: the answer-time observer test over the coalesced Class R ranges
    // (review A.4). A Class R write is SKIPPED, so its intermediate value must not be
    // visible to a device window, the framebuffer aperture or an armed DMA region.
    for &(lo, hi) in memo.class_r_ranges.iter() {
        if bus.reflected_call_dma_visible(lo, hi) {
            note_fell_through(cpu, FellThrough::DmaVisible);
            return Ok(None);
        }
    }

    // Screen 8, and the last one: is this the answer that gets AUDITED? Every Nth answer
    // of a key is refused ON PURPOSE and run naturally, so its real end state can be
    // compared with what the memo would have produced (plan R2.7 as amended: the audit is
    // record-and-compare, never rollback, because no machine-level snapshot of CPU plus
    // 64 MiB plus device state exists in this tree). It is placed HERE, after every screen
    // has passed, so an audited trip is one the memo really would have answered -- an
    // audit taken before the screens would grade a memo against a trip it was never going
    // to serve.
    if audit_is_due(cpu, key) {
        open_audit_trip(cpu, bus, key, memo);
        return Ok(None);
    }

    apply_answer(cpu, bus, &memo, scaled_core)?;
    note_answer_for_audit(cpu, key);
    Ok(Some(()))
}

/// Is this key's next answer the audited one? Two ways it can be: the ordinary period
/// (`IZARRAVM_REFLECTED_CALL_MEMO_AUDIT`, default 64), and the DRIFT ACCUMULATOR
/// (amendment 6) -- see `KeyState::bus_drift_acc`.
fn audit_is_due(cpu: &mut CpuGsw, key: MemoKey) -> bool {
    let half_edge = half_irq0_edge_clocks(cpu.mode_clock_hz());
    let state = cpu.reflected_call.as_mut().expect("caller checked");
    let period = state.audit_period;
    if period == 0 {
        return false;
    }
    let Some(key_state) = state.keys.get_mut(&key) else {
        return false;
    };
    if key_state.bus_drift_acc.unsigned_abs() >= half_edge {
        state.drift_forced_audits += 1;
        return true;
    }
    key_state.answers_since_audit >= period.saturating_sub(1)
}

fn note_answer_for_audit(cpu: &mut CpuGsw, key: MemoKey) {
    if let Some(key_state) = cpu
        .reflected_call
        .as_mut()
        .and_then(|state| state.keys.get_mut(&key))
    {
        key_state.answers_since_audit = key_state.answers_since_audit.saturating_add(1);
    }
}

/// Half the spacing between two IRQ0 edges, in the guest clocks `elapsed_clocks` counts.
///
/// The PIT's channel 0 runs at the standard 1.19318 MHz / 65536 = **18.2065 Hz**, so one
/// edge is `clock_hz / 18.2065` guest clocks and half an edge is that over two. Stated as
/// integer arithmetic over the persona's own clock rather than as a baked constant, so a
/// 486 row and a 586 row each get their own figure: at 166 MHz that is
/// `166e6 * 10_000 / 182_065 / 2 = 4,558,812`.
///
/// The accumulator it bounds is in RAW BUS clocks while this is in GUEST clocks, and the
/// comparison is deliberately made across that gap: `bus_timing` is `(16, 105)` on the 586
/// and `(1, 3)` on the 486, both well under 1, so a raw bus total always maps to FEWER
/// guest clocks than itself. Comparing raw against a guest-clock bound therefore fires
/// EARLY -- more audits than strictly needed, never fewer -- which is the safe direction
/// and needs no bus-scale accessor the CPU crate does not have.
pub(crate) fn half_irq0_edge_clocks(clock_hz: u64) -> u64 {
    clock_hz.saturating_mul(10_000) / 182_065 / 2
}

/// Refuse the answer and open a journaled AUDIT trip in its place. The key's own learn
/// slot is untouched: an audit is not a learn attempt, and a key mid-cycle must not lose
/// its place to one.
fn open_audit_trip<B: CpuBus>(cpu: &mut CpuGsw, bus: &B, key: MemoKey, memo: Arc<Memo>) {
    cpu.reflected_call_journal = true;
    let mut open = OpenTrip::start(cpu, bus, key, Slot::Audit);
    open.audit = Some(memo);
    let state = cpu.reflected_call.as_mut().expect("caller checked");
    state.open = Some(open);
}

/// Bring the memo cache into line with the decode cache's wholesale-mark-clear counter
/// (plan R2.4). A memo's code watch rests on its code pages STAYING MARKED, and marks are
/// cleared only wholesale, in `DecodeCache::retire_ring`; so the instant that counter
/// moves, every memo built under the old epoch has lost the thing that would have caught a
/// patch to its code, and must go. Called before any screen runs, and again before a memo
/// is inserted, so the cache is only ever populated under ONE epoch.
fn sync_code_mark_epoch(cpu: &mut CpuGsw) {
    let live = cpu.reflected_call_code_mark_epoch();
    let state = cpu.reflected_call.as_mut().expect("caller checked");
    if state.code_mark_epoch == live {
        return;
    }
    let retired = state.clear_all_memos();
    state.code_mark_epoch_retires += retired;
    state.retired[RetireCause::CodeMarkEpoch.index()] += retired;
    state.code_mark_epoch = live;
}

fn note_fell_through(cpu: &mut CpuGsw, reason: FellThrough) {
    let state = cpu.reflected_call.as_mut().expect("caller checked");
    match reason {
        FellThrough::NotMemoised => state.not_memoised += 1,
        FellThrough::EntryStateMismatch => state.entry_state_mismatch += 1,
        FellThrough::ReadSetMismatch => state.read_set_mismatch += 1,
        FellThrough::ReadSetUnreadable => state.read_set_unreadable += 1,
        FellThrough::PendingInterrupt => state.fell_through_pending_interrupt += 1,
        FellThrough::Cap => state.fell_through_cap += 1,
        FellThrough::DeviceEdge => state.fell_through_device_edge += 1,
        FellThrough::DmaVisible => state.fell_through_dma_visible += 1,
        FellThrough::NotArmed => state.fell_through_not_armed += 1,
        FellThrough::ClockProjection => state.fell_through_clock_projection += 1,
        FellThrough::SoftIntPosted => state.fell_through_soft_int_posted += 1,
    }
}

/// The apply order, exactly (plan section 5.8 as amended by R2.16 item 2):
///
/// 1. **Nested acks**, in trip order -- the first mutation, and the only step that can
///    `Err`; it does so before any register, memory cell or clock has moved.
/// 2. **Class W replay writes**, at their own physical address and width, through
///    `CpuGsw::write_physical_replay`, so each reaches `note_code_write_inner` and the
///    device write path exactly as the original write did.
/// 3. **Control effects**, LAST among the memory-visible steps: a replayed write into a
///    page-table entry must not leave a TLB that predates it.
/// 4. **The epilogue** -- eight GPRs, EFLAGS as a full architectural image, all six
///    segment registers WITH their cached descriptors, CPL, `EIP = int_eip + insn_len`
///    and the recorded final `ESP` (which reproduces the `RETF`-with-flags SP delta
///    exactly, because it was RECORDED rather than computed).
/// 5. **Clocks** -- the raw core total through the same remainder-carry scaler the
///    guest's own retirement uses, and the raw bus total NET OF what this answer's own
///    acks and writes just charged (plan R2.6's double-count rule).
/// 6. **`perf.instructions`**, advanced by the trip's own count: the guest really did
///    advance past those instructions, so the ON and OFF arms' instruction totals are
///    equal rather than merely reconcilable.
/// 7. **End the batch**, so no interrupt is ever deferred across a lump.
fn apply_answer<B: CpuBus>(
    cpu: &mut CpuGsw,
    bus: &mut B,
    memo: &Memo,
    scaled_core: u64,
) -> ExecResult<()> {
    // Sampled BEFORE the first mutation: everything the answer itself charges to the
    // bus between here and the commit below is subtracted from the memo's recorded
    // bus total, so the answer charges only the REMAINDER of what it did not itself
    // spend (plan R2.6).
    let bus_raw_before = bus.cumulative_raw_bus_clocks();

    for &(vector, ax) in memo.nested_acks.iter() {
        bus.interrupt_acknowledge(vector, ax)?;
    }

    for &(dword, mask, value) in memo.replay.iter() {
        if mask == u32::MAX {
            cpu.write_physical_replay(bus, dword, BusWidth::Dword, value)?;
            continue;
        }
        // Byte at a time for a partial dword: at most four writes, and it is the only
        // shape that touches the trip's own bytes and no others. A wider write would
        // clobber a neighbour the trip never wrote -- which is the whole reason the mask
        // exists.
        for byte in 0..4u32 {
            if mask & (0xFF << (byte * 8)) == 0 {
                continue;
            }
            cpu.write_physical_replay(
                bus,
                dword + byte,
                BusWidth::Byte,
                (value >> (byte * 8)) & 0xFF,
            )?;
        }
    }

    for effect in memo.control_effects.iter() {
        match *effect {
            ControlEffect::Cr3Write(value) => cpu.flush_tlb_and_code_caches_for_cr3_write(value),
            ControlEffect::Invlpg(linear) => cpu.apply_invlpg(linear),
        }
    }

    apply_epilogue(cpu, memo);

    let self_charged = bus
        .cumulative_raw_bus_clocks()
        .saturating_sub(bus_raw_before);
    debug_assert!(
        self_charged <= memo.raw_bus_clocks,
        "an answer charged more bus clocks itself than the whole recorded trip did"
    );
    let bus_charged =
        bus.reflected_call_commit_bus(memo.raw_bus_clocks.saturating_sub(self_charged));
    let core_charged = cpu
        .commit_reflected_call_core(memo.raw_core_clocks)
        .unwrap_or(0);
    debug_assert_eq!(
        core_charged, scaled_core,
        "the committed core charge must equal the projection the clamp was granted on"
    );
    cpu.perf.instructions = cpu.perf.instructions.saturating_add(memo.insns);

    let state = cpu.reflected_call.as_mut().expect("caller checked");
    state.answered += 1;
    state.insns_elided = state.insns_elided.saturating_add(memo.insns);
    state.core_clocks_charged = state.core_clocks_charged.saturating_add(core_charged);
    state.bus_clocks_charged = state.bus_clocks_charged.saturating_add(bus_charged);

    bus.note_reflected_call_answered();
    Ok(())
}

/// Install the trip's recorded EXIT architectural state. Deliberately does NOT touch
/// CR0/CR3/CR4, the descriptor-table registers or DR7: CR3 is moved by the replayed
/// control effects (and `build_confirmed` proves replaying them lands on the exit
/// image's own CR3), and every other one of those is pinned EQUAL between the entry
/// and exit images by `build_confirmed`'s system-state check, so there is nothing to
/// restore and writing them here would only be a second, unchecked path to the same
/// value.
fn apply_epilogue(cpu: &mut CpuGsw, memo: &Memo) {
    let ep = &memo.epilogue;
    // Read the architectural EFLAGS through the lazy-flag model, then tear the
    // descriptor down: leaving `pending_flags` standing over a freshly assigned base
    // would construct a state no execution path produces (a settled base with a live
    // descriptor over it), which `CpuGsw::settled`'s doc names explicitly.
    let live_eflags = cpu.eflags();
    cpu.clear_pending_flags();
    let regs = &mut cpu.registers;
    regs.set_eax(ep.eax);
    regs.set_ebx(ep.ebx);
    regs.set_ecx(ep.ecx);
    regs.set_edx(ep.edx);
    regs.set_ebp(ep.ebp);
    regs.set_esi(ep.esi);
    regs.set_edi(ep.edi);
    regs.set_esp(ep.esp);
    regs.eflags = (live_eflags & !EFLAGS_ARCH_MASK) | ep.eflags_masked;
    regs.set_segment(SegmentIndex::Cs, ep.cs.to_segment());
    regs.set_segment(SegmentIndex::Ss, ep.ss.to_segment());
    regs.set_segment(SegmentIndex::Ds, ep.ds.to_segment());
    regs.set_segment(SegmentIndex::Es, ep.es.to_segment());
    regs.set_segment(SegmentIndex::Fs, ep.fs.to_segment());
    regs.set_segment(SegmentIndex::Gs, ep.gs.to_segment());
    cpu.set_current_privilege_level(ep.cpl);
    // Last, and through `set_eip`: it clears the REP resume state and invalidates the
    // prefetch queue, both of which a real far return does on its way out.
    cpu.set_eip(memo.return_eip);
}

/// `CpuGsw::iret`/`CpuGsw::return_far`'s hook: a far RETURN boundary.
pub(crate) fn on_far_return<B: CpuBus>(cpu: &mut CpuGsw, bus: &B) {
    if cpu.reflected_call.is_none() {
        return;
    }
    let state = cpu.reflected_call.as_mut().expect("checked above");
    let Some(open) = state.open.take() else {
        return;
    };
    match open.is_return_match(cpu) {
        Some(_) => finish_trip(cpu, bus, open, true),
        None => {
            let over_budget =
                cpu.perf.instructions.saturating_sub(open.open_instructions) >= MEMO_MAX_TRIP_INSNS;
            if over_budget {
                finish_trip(cpu, bus, open, false);
            } else {
                cpu.reflected_call.as_mut().expect("checked above").open = Some(open);
            }
        }
    }
}

/// A far `CALL`/`JMP`: classification only (plan section 4.1 / review A.3),
/// may close an open trip via rule 2 (frame-gone) but NEVER produces a
/// learned outcome from that close.
pub(crate) fn on_far_transfer<B: CpuBus>(cpu: &mut CpuGsw, bus: &B) {
    if cpu.reflected_call.is_none() {
        return;
    }
    let state = cpu.reflected_call.as_mut().expect("checked above");
    let Some(open) = state.open.take() else {
        return;
    };
    let regs = &cpu.registers;
    let cs = regs.cs();
    let ss = regs.segment(SegmentIndex::Ss);
    let frame_gone = cs.selector == open.return_cs_selector
        && ss.selector == open.entry_ss_selector
        && open.sp_at_entry_width(regs.esp()) > open.entry_sp_at_width();
    if frame_gone {
        finish_trip(cpu, bus, open, false);
    } else {
        cpu.reflected_call.as_mut().expect("checked above").open = Some(open);
    }
}

/// Production seam: `CpuGsw::note_code_write_inner` (`core.rs`), on the tail where
/// `invalidated` is known -- i.e. exactly when a guest write hit a compiled block, a
/// cached decode line or a marked translation page. Retires the WHOLE memo cache (plan
/// R2.4 / BLOCKING finding 4: a slice that answers with no code-write retirement answers
/// from stale code forever -- a TSR chaining `INT 21h`, a DPMI host rewriting its own
/// hook, ordinary self-modifying code -- and no screen can see it, because the read set
/// covers DATA and never instruction fetch).
///
/// Coarser than the plan's per-memo range overlap, and deliberately so; the call site
/// carries the measurement that decided it.
pub(crate) fn note_code_hit(cpu: &mut CpuGsw) {
    let Some(state) = cpu.reflected_call.as_mut() else {
        return;
    };
    if state.memos.is_empty() {
        return;
    }
    let retired = state.clear_all_memos();
    state.code_watch_retires += retired;
    state.retired[RetireCause::CodeWatch.index()] += retired;
}

/// Production seam: `CpuGsw::flush_tlb_and_code_caches_for_cr3_write` (`core.rs`), the
/// single entry point every `MOV CR3` reaches. Records the effect in TRIP ORDER so the
/// answer can replay it by calling that same function (plan R2.5). Called only while
/// `reflected_call_journal` is set -- the memo is built from the JournalB trip.
pub(crate) fn note_cr3_write(cpu: &mut CpuGsw, new_cr3: u32) {
    push_control_effect(cpu, ControlEffect::Cr3Write(new_cr3));
}

/// Production seam: the `INVLPG m` arm of `execute_extended.rs`, recorded before
/// `CpuGsw::apply_invlpg` runs so the replay calls the same extracted function.
pub(crate) fn note_invlpg(cpu: &mut CpuGsw, linear: u32) {
    push_control_effect(cpu, ControlEffect::Invlpg(linear));
}

/// Production seam: `CpuGsw::flush_tlb_for_cr0_write` (`core.rs`). A CR0 write inside a
/// trip REFUSES the memo. Unlike a CR3 write or an INVLPG, whose whole effect is a
/// TLB/decode teardown this module replays by calling the same function, a CR0 write
/// can move PE, PG or WP -- the guest's addressing mode -- and replaying only "the
/// flush" would reproduce the cache effect while dropping every other consequence the
/// writer's own call site carries. Refusing is sound by construction and free here:
/// the measured dominant key writes CR3 twice per trip and CR0 never.
pub(crate) fn note_cr0_write(cpu: &mut CpuGsw) {
    refuse_open(cpu, LearnRefused::ControlRegisterDelta);
}

fn push_control_effect(cpu: &mut CpuGsw, effect: ControlEffect) {
    let Some(open) = cpu
        .reflected_call
        .as_mut()
        .and_then(|state| state.open.as_mut())
    else {
        return;
    };
    if !open.journaling {
        return;
    }
    if open.control_effects.len() >= MEMO_MAX_CONTROL_EFFECTS {
        open.control_effects_over_cap = true;
        return;
    }
    open.control_effects.push(effect);
}

/// Production seam: `CpuGsw::task_switch` (`control.rs`), reached by every task gate,
/// `CALL`/`JMP` through a TSS descriptor and `IRET` back-link return. A task switch
/// replaces the whole architectural context -- the memo's epilogue restores registers,
/// segments and CPL, and nothing it carries can reproduce a TSS load -- so a trip
/// containing one can never be answered (plan section 4.5's `task_switch` lane, wired to
/// a real seam here rather than left as a classification reason the tests inject).
pub(crate) fn note_task_switch(cpu: &mut CpuGsw) {
    refuse_open(cpu, LearnRefused::TaskSwitch);
}

/// Production seam: `execute.rs`'s port-I/O call-out, sibling to
/// `reflected_call_diag::on_port_io`.
pub(crate) fn note_port_io(cpu: &mut CpuGsw) {
    refuse_open(cpu, LearnRefused::PortIo);
}

/// Production seam: `fpu_exec.rs`, sibling to `reflected_call_diag::on_x87`.
pub(crate) fn note_x87(cpu: &mut CpuGsw) {
    refuse_open(cpu, LearnRefused::X87);
}

/// Production seam: `execute_extended.rs`'s RDTSC/RDMSR-of-TSC call-out.
pub(crate) fn note_rdtsc_or_rdmsr(cpu: &mut CpuGsw) {
    refuse_open(cpu, LearnRefused::NondeterministicRead);
}

/// Production seam: `control.rs`'s exception-delivery hook, vector 14 only
/// (page fault).
pub(crate) fn note_exception(cpu: &mut CpuGsw, vector: u8) {
    if vector == 14 {
        refuse_open(cpu, LearnRefused::PageFault);
    }
}

pub(crate) fn on_hardware_interrupt(cpu: &mut CpuGsw) {
    if let Some(open) = cpu
        .reflected_call
        .as_mut()
        .and_then(|state| state.open.as_mut())
    {
        open.hw_interrupt_seen = true;
    }
}

/// No identified production seam in this tree posts a "pending soft INT
/// inside a trip" or "task switch inside a trip" signal distinctly from what
/// the other refusals already catch; both classification reasons exist and
/// are exercised by `test_force_refuse` (test 13's shape), documented as a
/// scope cut in this module's top doc comment.
#[cfg(test)]
pub(crate) fn test_force_refuse(cpu: &mut CpuGsw, reason: LearnRefused) {
    refuse_open(cpu, reason);
}

fn refuse_open(cpu: &mut CpuGsw, reason: LearnRefused) {
    if let Some(open) = cpu
        .reflected_call
        .as_mut()
        .and_then(|state| state.open.as_mut())
    {
        open.hazard.get_or_insert(reason);
    }
}

/// One journaled READ. `width` is load-bearing (2026-09-03 measurement): the read set is
/// keyed by aligned physical DWORD, so recording a byte read as a whole dword pins three
/// bytes the trip never looked at. On `tyrian-specs-586` that over-pinning was fatal --
/// physical `0x17cc` mismatched on 726,117 of 726,134 answer attempts on the dominant key
/// because ONE byte of its dword is a DOS counter the trip does not read, and the memo
/// pinned the whole word. A MASK of the bytes actually read is what makes the screen say
/// what it means.
pub(crate) fn note_read<B: CpuBus>(cpu: &mut CpuGsw, bus: &B, linear: u32, width: BusWidth) {
    if cpu.reflected_call.is_none() {
        return;
    }
    let state = cpu.reflected_call.as_mut().expect("checked above");
    let Some(mut open) = state.open.take() else {
        return;
    };
    let ss = cpu.registers.segment(SegmentIndex::Ss);
    let esp = cpu.registers.esp();
    open.touch_stack(ss, esp);
    if !open.journaling {
        cpu.reflected_call.as_mut().expect("checked above").open = Some(open);
        return;
    }
    {
        let resolved = probe_physical(cpu, bus, linear);
        if let Some((physical, walk)) = resolved {
            let dword = aligned_dword(physical);
            // A cell this trip has ALREADY WRITTEN is not an input: what a later read of
            // it sees is the trip's own store, not guest state the answer's screens can
            // compare. Putting it in the read set makes the memo compare its own output
            // against whatever the PREVIOUS trip left there, which mismatches on almost
            // every occurrence.
            //
            // The screen was here before this fix, but it tested `open.writes` -- a map
            // keyed by aligned PHYSICAL dword -- with the LINEAR address. Under paging
            // the two differ, so it essentially never fired: measured on
            // `tyrian-specs-586` (2026-09-03), 1,387,638 of 1,459,463 answer attempts
            // fell through as `read_set_mismatch` and NOTHING was ever answered, while
            // Journal A and B agreed 65,633 times -- because two consecutive trips write
            // and read the same slots identically, and only the compare against LIVE
            // pre-trip memory can see the difference.
            if open.writes.contains_key(&dword) {
                cpu.reflected_call.as_mut().expect("checked above").open = Some(open);
                return;
            }
            let offset = physical - dword;
            // The bytes of THIS dword the access actually touches. An access that runs off
            // the end of the dword (a misaligned word or dword) has its tail recorded by
            // its own fragment or, where the seam does not split it, left unpinned -- the
            // conservative direction for a SCREEN is to pin less, never to pin bytes the
            // trip did not read, which is what produces a fall-through on every occurrence.
            let bytes = width.bytes().min(4 - offset);
            let mask = if bytes >= 4 {
                u32::MAX
            } else {
                ((1u32 << (bytes * 8)) - 1) << (offset * 8)
            };
            match open.reads.get_mut(&dword) {
                Some(entry) => entry.1 |= mask,
                None => {
                    if open.reads.len() >= MEMO_MAX_READ_SET {
                        open.read_set_over_cap = true;
                    } else if let Some(v) = bus.peek_direct_ram(dword, BusWidth::Dword) {
                        open.reads.insert(dword, (v, mask));
                    }
                }
            }
            if let Some(walk) = walk {
                for wphys in [walk.pde_phys, walk.pte_phys] {
                    let wdword = aligned_dword(wphys);
                    if !open.translations.contains_key(&wdword) {
                        if open.translations.len() >= MEMO_MAX_TRANSLATION_SET {
                            open.translation_set_over_cap = true;
                        } else if let Some(v) = bus.peek_direct_ram(wdword, BusWidth::Dword) {
                            // A PDE/PTE is read as a whole dword, always.
                            open.translations.insert(wdword, (v, u32::MAX));
                        }
                    }
                }
            }
        }
    }
    cpu.reflected_call.as_mut().expect("checked above").open = Some(open);
}

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
    if cpu.reflected_call.is_none() {
        return;
    }
    let state = cpu.reflected_call.as_mut().expect("checked above");
    let Some(mut open) = state.open.take() else {
        return;
    };
    let ss = cpu.registers.segment(SegmentIndex::Ss);
    let esp = cpu.registers.esp();
    open.touch_stack(ss, esp);
    if !open.journaling {
        cpu.reflected_call.as_mut().expect("checked above").open = Some(open);
        return;
    }
    let resolved = if already_physical {
        Some((linear, None))
    } else {
        probe_physical(cpu, bus, linear)
    };
    if let Some((physical, _walk)) = resolved {
        let dword = aligned_dword(physical);
        let class = forced_class.unwrap_or_else(|| {
            classify_write(
                cpu,
                bus,
                physical,
                linear,
                ss.selector,
                open.entry_ss_selector,
            )
        });
        let offset = physical - dword;
        if let Some(rec) = open.writes.get_mut(&dword) {
            rec.apply(offset, width.bytes(), value);
        } else if open.writes.len() >= MEMO_MAX_REPLAY_WRITES {
            open.hazard.get_or_insert(LearnRefused::ReplaySetTooLarge);
        } else {
            let pre_dword = bus.peek_direct_ram(dword, BusWidth::Dword);
            let mut rec = WriteObs {
                linear,
                ss_selector: ss.selector,
                pre_dword,
                latest_dword: pre_dword.unwrap_or(0),
                mask: 0,
                class,
            };
            rec.apply(offset, width.bytes(), value);
            open.writes.insert(dword, rec);
        }
    }
    cpu.reflected_call.as_mut().expect("checked above").open = Some(open);
}

#[allow(clippy::too_many_arguments)]
fn classify_write<B: CpuBus>(
    cpu: &CpuGsw,
    bus: &B,
    physical: u32,
    linear: u32,
    ss_selector: u16,
    entry_ss_selector: u16,
) -> AddressClass {
    // Fable review 2026-09-03, finding 5: the classifier already resolves `physical` (its
    // caller just walked it) and already has a bus handle, so the two never-restored device
    // classes cost the four lines below rather than staying permanently unreachable
    // (`write_class_w_other` silently vacuous, a framebuffer write wrongly eligible as
    // replayable W).
    if (FRAMEBUFFER_APERTURE_LO..=FRAMEBUFFER_APERTURE_HI).contains(&physical) {
        return AddressClass::FramebufferAperture;
    }
    if bus
        .peek_direct_ram(aligned_dword(physical), BusWidth::Dword)
        .is_none()
    {
        return AddressClass::NotPlainRam;
    }
    let idtr = cpu.idtr;
    if idtr.limit > 0 && linear.wrapping_sub(idtr.base) <= u32::from(idtr.limit) {
        return AddressClass::Idt;
    }
    if cpu.gdtr.limit > 0 && linear.wrapping_sub(cpu.gdtr.base) <= u32::from(cpu.gdtr.limit) {
        return AddressClass::Gdt;
    }
    if cpu.ldtr.limit > 0 && linear.wrapping_sub(cpu.ldtr.base) <= cpu.ldtr.limit {
        return AddressClass::Ldt;
    }
    if cpu.tr.limit > 0 && linear.wrapping_sub(cpu.tr.base) <= cpu.tr.limit {
        return AddressClass::Tss;
    }
    if (0x0400..0x0500).contains(&linear) {
        return AddressClass::Bda;
    }
    let ss = cpu.registers.segment(SegmentIndex::Ss);
    if ss.selector == ss_selector && linear.wrapping_sub(ss.base) < ss.limit.max(1) {
        return if ss_selector == entry_ss_selector {
            AddressClass::ClientStack
        } else {
            AddressClass::HostStack
        };
    }
    AddressClass::Other
}

// ---------------------------------------------------------------------------
// Trip finalisation
// ---------------------------------------------------------------------------

/// Wrapper around the real close, maintaining `journal_keys` -- the count of keys sitting
/// in a journal slot, which is what tells the machine's batch loop to force the
/// interpreter (see `CpuGsw::reflected_call_wants_interpreter`). Kept as a before/after
/// delta around ONE key rather than a scan of the map: `finish_trip` is per tracked trip,
/// not per answer, but the map holds every key the run has ever seen.
fn finish_trip<B: CpuBus>(cpu: &mut CpuGsw, bus: &B, open: OpenTrip, is_match: bool) {
    cpu.reflected_call_journal = false;
    let close_bus_raw = bus.cumulative_raw_bus_clocks();
    let close_elapsed = cpu.elapsed_clocks;
    let close_rem = cpu.reflected_call_timing_rem();
    let close_persona = cpu.persona();
    let close_instructions = cpu.perf.instructions;
    let exit_image = EntryImage::capture(cpu);
    let soft_int_posted = bus.reflected_call_soft_int_posted();
    // Plan 4.4: which return arm actually matched, so a JournalB success knows whether the
    // live stack tail (the `RETF`-with-flags shape's `INT`-pushed FLAGS word) applies. Cheap,
    // pure, and safe to compute even when `is_match` is false (then unused).
    let is_flags_arm = is_match && open.is_return_match(cpu) == Some(true);

    let key = open.key;
    // Option E, at the close of every natural sample and BEFORE the clock recovery below,
    // so an unstable write model short-circuits the rest of the learn. It runs here, at the
    // top of the close, rather than inside the `Slot::Natural` arm for a borrow reason
    // worth stating: that arm holds a `&mut KeyState` borrowed out of `cpu`, and the
    // confirmation needs `&mut CpuGsw` to reach the same state. Hoisting it is not a
    // compromise -- the arm's own first act is the clock recovery, so "before the clock
    // recovery" and "at the top of the close" are the same instant.
    if is_match && matches!(open.slot, Slot::Natural(_)) {
        confirm_write_model_at_close(cpu, bus, key);
    }
    let state = cpu.reflected_call.as_mut().expect("checked by callers");
    let key_state = state.keys.entry(key).or_default();

    // Fable re-review, 2026-09-03, nit (i): checked BEFORE `open.hazard` -- a hardware
    // interrupt taken inside the trip (its EOI is an `OUT`) used to be reported as `port_io`
    // (447 of them on one recipe-A run) because the port-I/O hazard was set first and this
    // check ran second, so `hardware_interrupt` read 0 on every key even when IRQs were the
    // real reason a trip could not be learned.
    if open.hw_interrupt_seen {
        key_state.record_failure(LearnRefused::HardwareInterrupt);
        return;
    }
    // Amendment 7: an HLE software interrupt posted during this trip. The posted vector is
    // guest-visible work the memo carries no record of, and the machine services it at the
    // next batch boundary -- so a memo built from such a trip would answer without it.
    // Asked of the BUS at close rather than tracked per-seam: `pending_soft_int` is the
    // machine's own field and this is the one predicate that sees every poster.
    if soft_int_posted {
        key_state.record_failure(LearnRefused::PendingSoftInt);
        return;
    }
    if let Some(hazard) = open.hazard {
        key_state.record_failure(hazard);
        return;
    }
    if !is_match {
        key_state.record_failure(LearnRefused::ClosedWithoutReturn);
        return;
    }
    if open.stack_segments_over_cap {
        key_state.record_failure(LearnRefused::ClosedWithoutReturn);
        return;
    }
    // Control-register net delta (plan section 4.5): CR0/CR4 must be
    // unchanged net across the trip; CR3 is licensed to vary (the VCPI
    // pair), which is exactly why this test is CR0/CR4 only, never CR3.
    if exit_image.cr0 != open.entry_image.cr0 || exit_image.cr4 != open.entry_image.cr4 {
        key_state.record_failure(LearnRefused::ControlRegisterDelta);
        return;
    }

    match open.slot {
        Slot::Warm => {
            key_state.slot = SlotState::JournalA;
        }
        Slot::JournalA => {
            if open.read_set_over_cap {
                key_state.record_failure(LearnRefused::ReadSetTooLarge);
                return;
            }
            if open.translation_set_over_cap {
                key_state.record_failure(LearnRefused::TranslationSetTooLarge);
                return;
            }
            key_state.write_set_size.observe(open.writes.len() as u32);
            key_state.read_set_size.observe(open.reads.len() as u32);
            key_state.pending_journal = Some(JournalSnapshot {
                entry_image: open.entry_image,
                reads: open.reads,
                translations: open.translations,
                writes: open.writes,
                insns: close_instructions.saturating_sub(open.open_instructions),
                exit_image,
            });
            key_state.slot = SlotState::JournalB;
        }
        Slot::JournalB => {
            if open.read_set_over_cap {
                key_state.record_failure(LearnRefused::ReadSetTooLarge);
                return;
            }
            if open.translation_set_over_cap {
                key_state.record_failure(LearnRefused::TranslationSetTooLarge);
                return;
            }
            let insns_b = close_instructions.saturating_sub(open.open_instructions);
            let Some(baseline) = key_state.pending_journal.take() else {
                key_state.record_failure(LearnRefused::JournalMismatch);
                return;
            };
            key_state.learn_attempts += 1;
            match compare_journal(
                &baseline,
                &open.entry_image,
                &open.writes,
                &open.reads,
                &open.translations,
                insns_b,
                &exit_image,
            ) {
                Ok(()) => {
                    tally_write_classes(key_state, &open, &open.writes);
                    match build_confirmed(&open, bus, is_flags_arm, &exit_image) {
                        Ok(confirmed) => {
                            key_state.confirmed = Some(confirmed);
                            key_state.slot = SlotState::Natural(0);
                        }
                        Err(reason) => {
                            key_state.record_failure(reason);
                        }
                    }
                }
                Err(reason) => {
                    key_state.record_failure(reason);
                }
            }
        }
        Slot::Audit => {
            let memo = open.audit.clone().expect("an audit trip carries its memo");
            let insns = close_instructions.saturating_sub(open.open_instructions);
            let raw_core = recover_raw_core_clocks(
                open.open_elapsed_clocks,
                open.open_timing_rem,
                close_elapsed,
                close_rem,
                open.entry_persona,
                close_persona,
            );
            let raw_bus = recover_raw_bus_clocks(open.open_bus_raw, close_bus_raw);
            let observed = AuditObservation {
                raw_core,
                raw_bus,
                insns,
                exit_image,
            };
            finish_audit(cpu, bus, key, &memo, &open, &observed);
        }
        Slot::Natural(i) => {
            let raw_core = recover_raw_core_clocks(
                open.open_elapsed_clocks,
                open.open_timing_rem,
                close_elapsed,
                close_rem,
                open.entry_persona,
                close_persona,
            );
            let Some(raw_core) = raw_core else {
                key_state.record_failure(LearnRefused::LevelChanged);
                return;
            };
            let Some(raw_bus) = recover_raw_bus_clocks(open.open_bus_raw, close_bus_raw) else {
                key_state.record_failure(LearnRefused::ClocksUnstable);
                return;
            };
            let insns = close_instructions.saturating_sub(open.open_instructions);
            let triple = (raw_core, raw_bus, insns);
            key_state.stability.observe_natural_sample(triple);
            key_state.natural_samples.push(triple);
            if usize::from(i) + 1 >= MEMO_CLOCK_SAMPLES {
                let all_equal = key_state.natural_samples.windows(2).all(|w| w[0] == w[1]);
                key_state.stability.finish_attempt(all_equal);
                if all_equal
                    && key_state
                        .confirmed
                        .as_ref()
                        .is_some_and(|c| c.write_model_unstable)
                {
                    // Option E's refusal: a check-set cell disagreed twice across the eight
                    // natural samples, so its value genuinely varies and no memo can carry
                    // it. This is the lane that stops a per-trip counter being frozen into
                    // a replay set -- the defect the fixture-scale audit named.
                    key_state.record_failure(LearnRefused::WriteValueUnstable);
                } else if all_equal
                    && key_state
                        .confirmed
                        .as_ref()
                        .is_some_and(|c| c.read_set_unstable)
                {
                    key_state.record_failure(LearnRefused::ReadSetUnstable);
                } else if all_equal {
                    key_state.learned += 1;
                    let confirmed = key_state.confirmed.take();
                    key_state.record_success_and_reset();
                    if let Some(c) = confirmed {
                        // Re-derived from the check set AFTER the natural samples' own
                        // demotions, so what the answer replays and what it skips is the
                        // classification the eight samples confirmed, not the one Journal B
                        // guessed.
                        let (replay, class_r_ranges) = derive_replay_and_ranges(&c.write_check);
                        let memo = Arc::new(Memo {
                            image: c.entry_image,
                            reads: c.reads.into_boxed_slice(),
                            translations: c.translations.into_boxed_slice(),
                            epilogue: c.epilogue,
                            return_eip: c.return_eip,
                            replay: replay.into_boxed_slice(),
                            class_r_ranges: class_r_ranges.into_boxed_slice(),
                            raw_core_clocks: raw_core,
                            raw_bus_clocks: raw_bus,
                            insns,
                            nested_acks: c.nested_acks.into_boxed_slice(),
                            control_effects: c.control_effects.into_boxed_slice(),
                        });
                        // The cache is only ever populated under ONE `code_mark_epoch`
                        // (see `sync_code_mark_epoch`): a bump between the last answer
                        // attempt and this insert wipes the older memos first, rather
                        // than leaving a mixed cache the single epoch field misdescribes.
                        sync_code_mark_epoch(cpu);
                        let state = cpu.reflected_call.as_mut().expect("checked above");
                        let slot_vec = state.memos.entry(key).or_default();
                        // Same entry image relearned (e.g. after an A20 retire): replace
                        // rather than duplicate.
                        slot_vec.retain(|m| m.image != memo.image);
                        if slot_vec.len() >= MEMO_IMAGES_PER_KEY {
                            slot_vec.remove(0); // FIFO eviction of the oldest image.
                        }
                        slot_vec.push(memo);
                    }
                } else {
                    key_state.record_failure(LearnRefused::ClocksUnstable);
                }
            } else {
                key_state.slot = SlotState::Natural(i + 1);
            }
        }
    }
}

/// What an audited trip actually did, gathered at its close.
struct AuditObservation {
    raw_core: Option<u64>,
    raw_bus: Option<u64>,
    insns: u64,
    exit_image: EntryImage,
}

/// Grade one audited trip against the memo that would have answered it, and act.
///
/// **The write comparison is a FORMULA, not a set comparison** (plan R2.18): *for every
/// address in the OBSERVED write set, the memo's predicted final value -- its replay value
/// where the memo replays it, else the value the address held at trip entry -- equals the
/// observed final value.* Set-vs-set would miss the case that matters most: an address the
/// memo classified R that is unpinned at audit time, where the memo predicts "no change"
/// and the real trip writes. The union with the memo's OWN replay set covers the mirror
/// case: an address the memo would write that this trip did not.
///
/// **What each disagreement costs the memo.** A core-clock or instruction-count
/// disagreement retires it and lets the key re-learn: those two were unanimous over
/// 525,352 measured samples, so a disagreement is a genuinely different trip and charging
/// the old constant would break the conservation the whole slice rests on. A bus total
/// outside `MEMO_AUDIT_BUS_BAND` does the same. An epilogue or write-value disagreement is
/// a soundness failure, and retires it too. Retiring is never a DISARM: the key may learn
/// again immediately, which is what stops one regime shift from switching the mechanism
/// off for the rest of the run.
fn finish_audit<B: CpuBus>(
    cpu: &mut CpuGsw,
    bus: &B,
    key: MemoKey,
    memo: &Memo,
    open: &OpenTrip,
    observed: &AuditObservation,
) {
    let mut mismatches: Vec<AuditMismatch> = Vec::new();
    let (Some(raw_core), Some(raw_bus)) = (observed.raw_core, observed.raw_bus) else {
        record_audit(cpu, key, &[AuditMismatch::Unusable], 0);
        return;
    };
    if observed.exit_image != memo.epilogue {
        mismatches.push(AuditMismatch::Epilogue);
    }
    if raw_core != memo.raw_core_clocks {
        mismatches.push(AuditMismatch::CoreClocks);
    }
    if observed.insns != memo.insns {
        mismatches.push(AuditMismatch::Instructions);
    }
    let bus_delta = i64::try_from(memo.raw_bus_clocks).unwrap_or(i64::MAX)
        - i64::try_from(raw_bus).unwrap_or(i64::MAX);
    if bus_delta.unsigned_abs() > MEMO_AUDIT_BUS_BAND {
        mismatches.push(AuditMismatch::BusClocks);
    }
    let mut write_offender: Option<(u32, u32, u32)> = None;
    if audit_write_values_disagree(bus, memo, open, &mut write_offender) {
        mismatches.push(AuditMismatch::WriteValue);
    }
    if let Some(cell) = write_offender
        && let Some(key_state) = cpu
            .reflected_call
            .as_mut()
            .and_then(|state| state.keys.get_mut(&key))
    {
        let provenance = u32::from(memo.replay.iter().any(|&(a, _, _)| a == cell.0))
            | (u32::from(open.writes.contains_key(&cell.0)) << 1);
        key_state.note_audit_offender(cell, provenance);
    }
    record_audit(cpu, key, &mismatches, bus_delta);
}

/// The write half of the audit formula. Walks the UNION of the observed write set and the
/// memo's replay set, at aligned-dword granularity, comparing the memo's prediction with
/// the live post-trip value.
fn audit_write_values_disagree<B: CpuBus>(
    bus: &B,
    memo: &Memo,
    open: &OpenTrip,
    offender: &mut Option<(u32, u32, u32)>,
) -> bool {
    let mut dwords: Vec<u32> = open.writes.keys().copied().collect();
    for &(addr, _, _) in memo.replay.iter() {
        dwords.push(addr);
    }
    dwords.sort_unstable();
    dwords.dedup();
    for dword in dwords {
        // Class D is excluded, and that is the formula's meaning rather than a hole in it.
        // A dead-stack write lies below this trip's OWN observed low-water mark for the
        // segment it fell in: nothing reads it again, which is exactly why the memo skips
        // it. Grading the memo on it asks the answer to reproduce bytes no guest can
        // observe, and on this title's dominant key that is 128 stack dwords per trip --
        // measured 2026-09-03, `audit_mismatch[write_value]` fired on 14,510 of 14,510
        // audited trips while the epilogue, the core clocks and the instruction count all
        // agreed exactly, and every one of them was this.
        if let Some(obs) = open.writes.get(&dword)
            && matches!(
                obs.class,
                AddressClass::ClientStack | AddressClass::HostStack
            )
            && open.is_dead_stack(obs.ss_selector, obs.linear)
        {
            continue;
        }
        let Some(observed_now) = bus.peek_direct_ram(dword, BusWidth::Dword) else {
            // The cell is not plain RAM any more. A memo may not carry such an address at
            // all (`build_confirmed` refuses `never_restored` classes), so this is a
            // disagreement, not an excuse.
            return true;
        };
        // "The value the address held at trip entry": the peek `note_write` took before
        // this trip's FIRST write to the dword. An address the trip never wrote still
        // holds its entry value now, so the live read is that value.
        let entry = open
            .writes
            .get(&dword)
            .and_then(|obs| obs.pre_dword)
            .unwrap_or(observed_now);
        let mut predicted = entry;
        for &(addr, mask, value) in memo.replay.iter() {
            if addr != dword {
                continue;
            }
            predicted = (predicted & !mask) | (value & mask);
        }
        if predicted != observed_now {
            *offender = Some((dword, predicted, observed_now));
            return true;
        }
    }
    false
}

/// Book one audit's outcome and act on it: retire the memo on any disagreement, and fold
/// the observed bus error into the key's drift accumulator (amendment 6).
fn record_audit(cpu: &mut CpuGsw, key: MemoKey, mismatches: &[AuditMismatch], bus_delta: i64) {
    let state = cpu.reflected_call.as_mut().expect("caller checked");
    state.audited += 1;
    for m in mismatches {
        state.audit_mismatch[m.index()] += 1;
    }
    let actionable = mismatches
        .iter()
        .any(|m| !matches!(m, AuditMismatch::Unusable));
    if actionable {
        let retired = state
            .memos
            .get_mut(&key)
            .map(|memos| {
                let n = memos.len() as u64;
                memos.clear();
                n
            })
            .unwrap_or(0);
        state.retired[RetireCause::Audit.index()] += retired;
    }
    let Some(key_state) = state.keys.get_mut(&key) else {
        return;
    };
    key_state.audits += 1;
    if actionable {
        // Retiring is not disarming: the key restarts its learn cycle at Warm and may
        // rebuild a memo immediately (measure-first review: "a disagreement retires the
        // memo and re-learns; never disarms"). `relearned` is what makes a key that is
        // thrashing between learn and retire visible as a counter rather than as a quiet
        // collapse in the answer rate.
        key_state.slot = SlotState::Warm;
        key_state.pending_journal = None;
        key_state.confirmed = None;
        key_state.natural_samples.clear();
        state.relearned += 1;
        key_state.bus_drift_acc = 0;
        // Option D's two-strikes rule. With option E confirming the write model over eight
        // natural samples, ONE `write_value` strike is most likely a cadence longer than
        // that window -- which the next learn cycle's own demotion may well capture -- so
        // it still costs only a retire and a re-learn. A SECOND strike says the key is not
        // memoisable, and it stops paying for attempts. The disarm is the existing BOUNDED
        // one: `maybe_rearm` brings the key back after `MEMO_REARM_TRIPS_SEEN` trips, so a
        // key that becomes learnable later is not written off for the run.
        if mismatches.contains(&AuditMismatch::WriteValue) {
            key_state.write_value_strikes = key_state.write_value_strikes.saturating_add(1);
            if key_state.write_value_strikes >= MEMO_WRITE_VALUE_STRIKES {
                key_state.disarmed = true;
                key_state.disarmed_at_trips_seen = key_state.trips_seen;
                key_state.write_value_disarms += 1;
                key_state.write_value_strikes = 0;
            }
        }
    } else {
        // Amendment 6: every answer since the last audit charged the memo's own bus total,
        // and this audit is the only evidence of what that total should have been, so the
        // whole batch of answers carries the error this one trip revealed.
        let answers = i64::try_from(key_state.answers_since_audit).unwrap_or(i64::MAX);
        key_state.bus_drift_acc = key_state
            .bus_drift_acc
            .saturating_add(bus_delta.saturating_mul(answers));
    }
    key_state.answers_since_audit = 0;
}

fn compare_journal(
    baseline: &JournalSnapshot,
    entry_image: &EntryImage,
    writes: &HashMap<u32, WriteObs>,
    reads: &HashMap<u32, (u32, u32)>,
    translations: &HashMap<u32, (u32, u32)>,
    insns_b: u64,
    exit_image: &EntryImage,
) -> Result<(), LearnRefused> {
    if baseline.entry_image != *entry_image {
        return Err(LearnRefused::JournalMismatch);
    }
    if baseline.insns != insns_b || baseline.exit_image != *exit_image {
        return Err(LearnRefused::JournalMismatch);
    }
    if baseline.reads.len() != reads.len() || baseline.translations.len() != translations.len() {
        return Err(LearnRefused::JournalMismatch);
    }
    // Values compared under the INTERSECTION of the two masks: a byte neither trip read is
    // not evidence about either. The masks themselves must agree, or the two trips read
    // different bytes of the same dword and are not the same shape.
    for (addr, &(a_value, a_mask)) in &baseline.reads {
        let Some(&(b_value, b_mask)) = reads.get(addr) else {
            return Err(LearnRefused::JournalMismatch);
        };
        if a_mask != b_mask || (a_value ^ b_value) & a_mask != 0 {
            return Err(LearnRefused::JournalMismatch);
        }
    }
    for (addr, v) in &baseline.translations {
        if translations.get(addr) != Some(v) {
            return Err(LearnRefused::JournalMismatch);
        }
    }
    if baseline.writes.len() != writes.len() {
        return Err(LearnRefused::JournalMismatch);
    }
    for (addr, a) in &baseline.writes {
        let Some(b) = writes.get(addr) else {
            return Err(LearnRefused::JournalMismatch);
        };
        // The bytes each trip wrote must be the same bytes, and must have been left holding
        // the same values. A post-value that varies between two consecutive trips is a
        // memo that would freeze a per-trip counter (design 4.9's hole).
        if a.mask != b.mask || (a.latest_dword ^ b.latest_dword) & a.mask != 0 {
            return Err(LearnRefused::WriteClassN);
        }
    }
    Ok(())
}

/// R2.3's PINNED requirement, at the granularity the write actually has.
///
/// A write may be Class R -- skipped, because the trip restored what was there -- only if
/// the trip's own read set covers EVERY BYTE it wrote. The read set is what the answer's
/// screen compares, so a byte the trip wrote but never read is a byte the screen cannot
/// vouch for: at answer time it may hold something else, the real trip would overwrite it,
/// and skipping the write diverges with nothing able to catch it (finding 3).
///
/// **Per byte, not per dword.** Both sets are keyed by aligned dword and both carry a byte
/// MASK, and testing mere dword MEMBERSHIP admits exactly the case the fixture-scale audit
/// caught on `tyrian-specs-586` (2026-09-03): a trip that reads the high half of a dword
/// and writes the low half was called pinned, classified R and skipped, while the low half
/// varied freely underneath the screen. It was the whole of the remaining divergence --
/// one address per dominant key, `0x1a3ea4` on `INT 33h` (54,988 of 55,188 audits) and
/// `0x127150` on `AH=0Bh` (60,243 of 60,243) -- with the epilogue, the core clocks, the
/// instruction count and the bus total all agreeing exactly.
fn write_is_pinned(open: &OpenTrip, dword: u32, write_mask: u32) -> bool {
    let read_mask = open
        .reads
        .get(&dword)
        .or_else(|| open.translations.get(&dword))
        .map(|&(_, mask)| mask)
        .unwrap_or(0);
    read_mask & write_mask == write_mask
}

/// The Class W cells the answer WRITES: aligned physical dword, the mask of the bytes the
/// trip wrote, and the dword's post-trip value.
type ReplaySet = Vec<(u32, u32, u32)>;
/// The Class R cells the answer SKIPS, coalesced into inclusive physical ranges for the
/// answer-time observer test.
type ClassRRanges = Vec<(u32, u32)>;

/// Derive the answer path's two write descriptions from the check set: `replay` (the Class
/// W cells, written at answer time) and `class_r_ranges` (the Class R cells, SKIPPED at
/// answer time and screened by the observer test). Called once when the candidate is built
/// and again after the eight natural samples have demoted whatever they caught, so the two
/// descriptions are never derived from different classifications.
fn derive_replay_and_ranges(write_check: &[WriteCheck]) -> (ReplaySet, ClassRRanges) {
    let mut replay: ReplaySet = Vec::new();
    let mut class_r_dwords: Vec<u32> = Vec::new();
    for c in write_check {
        if c.is_class_r {
            class_r_dwords.push(c.dword);
        } else {
            replay.push((c.dword, c.mask, c.value));
        }
    }
    replay.sort_unstable_by_key(|&(addr, _, _)| addr);
    class_r_dwords.sort_unstable();
    class_r_dwords.dedup();
    let mut class_r_ranges: ClassRRanges = Vec::new();
    for dword in class_r_dwords {
        if let Some((_, hi)) = class_r_ranges.last_mut()
            && dword <= hi.wrapping_add(1)
        {
            *hi = dword + 3;
            continue;
        }
        class_r_ranges.push((dword, dword + 3));
    }
    (replay, class_r_ranges)
}

/// One address of the WRITE CHECK SET: the union of Class W and Class R, which is what the
/// eight natural samples confirm the write MODEL against
/// (`dev_docs/2026-09-05-reflected-call-learn-protocol-design.md`, option E).
///
/// Class D never enters it -- a dead-stack write is unobservable by construction -- which
/// is why the set is ~42 dwords rather than the ~250 the raw write set holds.
///
/// The design writes this as `(u32, u32, u32, bool)`; it is a named struct because the
/// confirmation needs a fifth field (`retaken`) that the tuple has no room for, and four
/// same-typed positional `u32`s at a call site are exactly how a mask and a value get
/// swapped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct WriteCheck {
    /// Aligned physical dword.
    dword: u32,
    /// The bytes the trip wrote. Every comparison below is made UNDER this mask: bytes the
    /// trip never wrote are not part of its model and may move freely.
    mask: u32,
    /// For Class W, the value the trip leaves in the masked bytes. Meaningless (and never
    /// read) while `is_class_r`.
    value: u32,
    /// Class R -- the trip restored what was there, so the model predicts UNCHANGED.
    is_class_r: bool,
    /// Has this entry already had its value re-taken once (a Class R demotion, or a Class W
    /// value replaced)? A SECOND disagreement after that is a genuinely varying cell, and
    /// the learn is refused.
    retaken: bool,
}

/// JournalB's classification, carried forward to Natural-sample completion so a `Memo` can be
/// built (plan section 3) once the raw-clock samples confirm the trip is worth memoising.
#[derive(Clone, PartialEq, Eq, Debug)]
struct ConfirmedTrip {
    entry_image: EntryImage,
    reads: Vec<(u32, u32, u32)>,
    translations: Vec<(u32, u32, u32)>,
    epilogue: EntryImage,
    return_eip: u32,
    replay: Vec<(u32, u32, u32)>,
    class_r_ranges: Vec<(u32, u32)>,
    nested_acks: Vec<(u8, u16)>,
    control_effects: Vec<ControlEffect>,
    /// Has `sample_read_set_at_open` taken its first live sample yet, and did any later
    /// sample disagree with it? See that function.
    read_set_pinned: bool,
    read_set_unstable: bool,
    /// The write CHECK SET (option E). `replay` and `class_r_ranges` above are DERIVED from
    /// this at memo-build time (`derive_replay_and_ranges`), so there is exactly one
    /// classification and the natural samples' demotions cannot drift away from it.
    write_check: Vec<WriteCheck>,
    /// Scratch, parallel to `write_check`: the values peeked at the CURRENT natural trip's
    /// open. `None` where the peek declined.
    write_open_pre: Vec<Option<u32>>,
    /// The twins of the read set's two flags, for the write model.
    write_model_pinned: bool,
    write_model_unstable: bool,
}

/// Amendment 2: classify JournalB's write set into Class R (restored, admitted classes only --
/// recorded into `class_r_ranges` for the answer-time observer test, review A.4, and SKIPPED
/// at answer time) and Class W (deterministic net write -- REPLAYED at answer time), and
/// assemble the rest of what the answer path needs. A write to an address
/// `AddressClass::never_restored` (the framebuffer aperture or a device window/unmapped page)
/// refuses the WHOLE trip (`write_class_n`): plan 4.3's Class W rule requires "plain RAM
/// outside every device window and outside the framebuffer aperture", so such a write is
/// eligible for neither R (skipping a hardware-visible write is wrong) nor W -- it can never
/// be memoised. Class D (dead stack) is skipped and recorded nowhere.
fn build_confirmed<B: CpuBus>(
    open: &OpenTrip,
    bus: &B,
    is_flags_arm: bool,
    exit_image: &EntryImage,
) -> Result<ConfirmedTrip, LearnRefused> {
    if open.nested_acks_over_cap {
        return Err(LearnRefused::NestedAcksTooMany);
    }
    if open.control_effects_over_cap {
        return Err(LearnRefused::ControlEffectsTooMany);
    }
    // The epilogue restores registers, EFLAGS, the six segment registers and CPL, and
    // the control-effect replay moves CR3. NOTHING restores the descriptor-table
    // registers, CR0, CR4 or DR7 -- so a trip that moves any of them NET cannot be
    // answered, and says so here rather than leaving a silent hole in the epilogue.
    // (The entry image PINS all of them at entry; this is the exit-side half of the
    // same closure rule.)
    let entry = &open.entry_image;
    let system_state_moved = exit_image.cr0 != entry.cr0
        || exit_image.cr4 != entry.cr4
        || exit_image.dr7 != entry.dr7
        || exit_image.idtr_base != entry.idtr_base
        || exit_image.idtr_limit != entry.idtr_limit
        || exit_image.gdtr_base != entry.gdtr_base
        || exit_image.gdtr_limit != entry.gdtr_limit
        || exit_image.ldtr_selector != entry.ldtr_selector
        || exit_image.ldtr_base != entry.ldtr_base
        || exit_image.ldtr_limit != entry.ldtr_limit
        || exit_image.ldtr_access != entry.ldtr_access
        || exit_image.tr_selector != entry.tr_selector
        || exit_image.tr_base != entry.tr_base
        || exit_image.tr_limit != entry.tr_limit
        || exit_image.tr_access != entry.tr_access
        || exit_image.vm != entry.vm;
    if system_state_moved {
        return Err(LearnRefused::ControlRegisterDelta);
    }
    // CR3 IS licensed to vary (the VCPI pair), but only because the control-effect
    // replay moves it. Prove that here, as an equality rather than an argument:
    // `flush_tlb_and_code_caches_for_cr3_write` stores its argument into `control.cr3`,
    // so replaying the recorded effects from the entry's CR3 must land on the exit
    // image's. If it does not, the trip changed CR3 through a path this journal does
    // not see, and no memo may exist for it.
    let replayed_cr3 = open
        .control_effects
        .iter()
        .rev()
        .find_map(|e| match *e {
            ControlEffect::Cr3Write(v) => Some(v),
            ControlEffect::Invlpg(_) => None,
        })
        .unwrap_or(entry.cr3);
    if replayed_cr3 != exit_image.cr3 {
        return Err(LearnRefused::ControlEffectUnreplayable);
    }
    // The CHECK SET is the single classification (option E): `replay` and `class_r_ranges`
    // are derived from it, here and again after the natural samples have had their say, so
    // a demotion cannot leave the two descriptions disagreeing.
    let mut write_check: Vec<WriteCheck> = Vec::new();
    for (&dword, obs) in &open.writes {
        if obs.class.never_restored() {
            return Err(LearnRefused::WriteClassN);
        }
        if matches!(
            obs.class,
            AddressClass::ClientStack | AddressClass::HostStack
        ) && open.is_dead_stack(obs.ss_selector, obs.linear)
        {
            continue; // Class D: skip, recorded nowhere.
        }
        // Class R (restored): every byte written must be covered by the read set (see
        // `write_is_pinned`) AND must hold what it held before. `note_read` already
        // excludes a cell the trip wrote FIRST, so read-set coverage IS "read before
        // written".
        let pinned = write_is_pinned(open, dword, obs.mask);
        let restored = obs
            .pre_dword
            .is_some_and(|pre| (pre ^ obs.latest_dword) & obs.mask == 0);
        write_check.push(WriteCheck {
            dword,
            mask: obs.mask,
            value: obs.latest_dword,
            // Class R: restored, admitted, skipped at answer time. Class W: replayed.
            is_class_r: pinned && restored,
            retaken: false,
        });
    }

    // Plan 4.4, the live stack tail: the `RETF`-with-flags shape leaves the `INT`-pushed
    // FLAGS word at `[entry_esp - 2, entry_esp)` guest-visible above the final SP. Generic
    // rule: whenever that word differs from what was there at entry (ordinary pre-trip stack
    // scratch, unrelated to this trip), it must be replayed -- omitting it silently corrupts
    // the guest stack (this is what test 7 catches).
    if is_flags_arm
        && let Some((phys, entry_value)) = open.entry_tail
        && let Some(close_value) = bus.peek_direct_ram(phys, BusWidth::Word)
        && close_value != entry_value
    {
        let dword = aligned_dword(phys);
        let (byte_mask, shifted) = splice(phys - dword, 2, close_value);
        // Folded into the check set as Class W, so the natural samples confirm the FLAGS
        // word like any other replayed byte rather than trusting one observation of it.
        match write_check.iter_mut().find(|c| c.dword == dword) {
            Some(existing) => {
                let base = if existing.is_class_r {
                    bus.peek_direct_ram(dword, BusWidth::Dword).unwrap_or(0)
                } else {
                    existing.value
                };
                existing.value = (base & !byte_mask) | shifted;
                existing.mask |= byte_mask;
                existing.is_class_r = false;
            }
            None => {
                let base = bus.peek_direct_ram(dword, BusWidth::Dword).unwrap_or(0);
                write_check.push(WriteCheck {
                    dword,
                    mask: byte_mask,
                    value: (base & !byte_mask) | shifted,
                    is_class_r: false,
                    retaken: false,
                });
            }
        }
    }

    write_check.sort_unstable_by_key(|c| c.dword);
    let (replay, class_r_ranges) = derive_replay_and_ranges(&write_check);
    if replay.len() > MEMO_MAX_REPLAY_WRITES {
        return Err(LearnRefused::ReplaySetTooLarge);
    }

    // The values recorded here are the ones the READ observed, which is NOT what the
    // answer's screen needs: the screen runs BEFORE a trip, so what it must pin is the
    // value each cell holds at the START of an occurrence. The two differ for every cell
    // the trip itself writes, and neither the read value nor the post-trip value is right
    // in general -- on this title the two dominant keys INTERLEAVE, so what a cell holds
    // before an `AH=0Bh` trip is what the `INT 33h` trip in between left there.
    //
    // So the pin is MEASURED rather than derived: `sample_read_set_at_open` overwrites
    // these values from live memory at the first of the 8 natural trips' opens and
    // requires the remaining 7 to agree. See its own comment for the measurement that
    // decided it.
    let mut reads: Vec<(u32, u32, u32)> = open
        .reads
        .iter()
        .map(|(&addr, &(value, mask))| (addr, value, mask))
        .collect();
    reads.sort_unstable_by_key(|&(a, _, _)| a);
    let mut translations: Vec<(u32, u32, u32)> = open
        .translations
        .iter()
        .map(|(&a, &(v, m))| (a, v, m))
        .collect();
    translations.sort_unstable_by_key(|&(a, _, _)| a);

    Ok(ConfirmedTrip {
        entry_image: open.entry_image,
        reads,
        translations,
        epilogue: *exit_image,
        return_eip: open.return_eip,
        replay,
        class_r_ranges,
        nested_acks: open.nested_acks.clone(),
        control_effects: open.control_effects.clone(),
        read_set_pinned: false,
        read_set_unstable: false,
        write_open_pre: vec![None; write_check.len()],
        write_check,
        write_model_pinned: false,
        write_model_unstable: false,
    })
}

fn tally_write_classes(key_state: &mut KeyState, open: &OpenTrip, writes: &HashMap<u32, WriteObs>) {
    for (dword, obs) in writes {
        if obs.class.never_restored() {
            key_state.write_class_w_other += 1;
            continue;
        }
        // Fable review 2026-09-03, finding 5: pick the tracker by the SS selector IN FORCE at
        // the write (recorded on `WriteObs`), not unconditionally the entry segment's own --
        // a trip can carry several concurrent stack segments (client, host, a V86 excursion),
        // and the design's Class D rule is per SEGMENT, no constant cap.
        if matches!(
            obs.class,
            AddressClass::ClientStack | AddressClass::HostStack
        ) && open.is_dead_stack(obs.ss_selector, obs.linear)
        {
            key_state.write_class_d += 1;
            continue;
        }
        let pinned = write_is_pinned(open, *dword, obs.mask);
        let restored = obs
            .pre_dword
            .is_some_and(|pre| (pre ^ obs.latest_dword) & obs.mask == 0);
        if pinned && restored {
            key_state.write_class_r_pinned += 1;
        } else {
            key_state.write_class_r_unpinned += 1;
        }
    }
}

/// R2.2/R2.15's telescoping-carry raw-clock recovery: `raw = (den *
/// elapsed_delta + rem_after - rem_before) / num`, computed in `i64` (the
/// numerator underflows in `u64` when `elapsed_delta == 0`), with a
/// divisibility assert and a refusal on a persona/level change (which resets
/// `timing_rem` and breaks the telescope).
fn recover_raw_core_clocks(
    open_elapsed: u64,
    open_rem: u64,
    close_elapsed: u64,
    close_rem: u64,
    open_persona: CpuPersona,
    close_persona: CpuPersona,
) -> Option<u64> {
    if open_persona != close_persona {
        return None;
    }
    let (num, den) = crate::level_timing(close_persona);
    let elapsed_delta = close_elapsed.checked_sub(open_elapsed)?;
    let numerator: i64 = (den as i64)
        .checked_mul(elapsed_delta as i64)?
        .checked_add(close_rem as i64)?
        .checked_sub(open_rem as i64)?;
    if numerator < 0 {
        return None;
    }
    debug_assert!(
        numerator % i64::from(num) == 0,
        "raw-clock recovery numerator must be exactly divisible by the scaler's `num`"
    );
    Some((numerator / i64::from(num)) as u64)
}

/// Raw bus clock recovery: a plain delta over `CpuBus::cumulative_raw_bus_clocks`, which is
/// monotone for the WHOLE run (never reset at a machine-batch boundary). Fable review
/// 2026-09-03, finding 1: the previous version sampled `CpuBus::in_batch_raw_bus_clocks`,
/// which resets at every batch re-entry (a trip's own nested `IRET`s cause 6-8 of these), so
/// `open_bus_raw` could be larger than `close_bus_raw` even on a perfectly healthy trip --
/// `checked_sub` then returned `None` and the trip was refused as `clocks_unstable` for a
/// reason that had nothing to do with clock jitter. No carry is needed here (unlike the core
/// recovery above): `cumulative_raw_bus_clocks` is already the RAW, unscaled total.
fn recover_raw_bus_clocks(open_bus_raw: u64, close_bus_raw: u64) -> Option<u64> {
    close_bus_raw.checked_sub(open_bus_raw)
}

// ---------------------------------------------------------------------------
// Compare-loop microbenchmark (R2.20(c)), behind its own knob
// ---------------------------------------------------------------------------

/// `IZARRAVM_REFLECTED_CALL_MEMO_BENCH=<R>`: runs a microbenchmark of the
/// ACTUAL compare loop over `R` pre-resolved aligned physical dwords against
/// an in-memory buffer (never `probe_physical`, 0b's defect D6) and prints
/// `ns_per_read` to stderr. Off unless the env var is set; never touches
/// guest state.
pub fn maybe_run_compare_bench() {
    let Ok(spec) = std::env::var("IZARRAVM_REFLECTED_CALL_MEMO_BENCH") else {
        return;
    };
    let Ok(r) = spec.trim().parse::<usize>() else {
        return;
    };
    let (ns_per_read, ns_total) = run_compare_bench(r);
    eprintln!(
        "reflected-call-memo compare-bench: R={r} ns_per_read={ns_per_read:.3} ns_total_per_iter={ns_total:.3}"
    );
}

pub(crate) fn run_compare_bench(r: usize) -> (f64, f64) {
    let cells: Vec<(u32, u32)> = (0..r)
        .map(|i| ((i as u32) * 4, i as u32 ^ 0x5a5a_5a5a))
        .collect();
    let buf = vec![0u32; r.max(1)];
    const ITERS: u32 = 2000;
    let start = std::time::Instant::now();
    let mut sink = 0u64;
    for _ in 0..ITERS {
        // `black_box` on both operands of the comparison (Fable re-review, 2026-09-03, nit
        // (v)): without it the optimizer can prove `buf[idx]` is always 0 and `expect` is
        // always non-zero (both come from a compile-time-transparent `vec![0; r]` and an XOR
        // with a constant) and fold the whole inner loop to nothing, which is exactly the
        // "meaningless" result the implementer's own caveat warned about.
        for (idx, (_, expect)) in cells.iter().enumerate() {
            let v = std::hint::black_box(buf[idx]);
            sink += u64::from(v == std::hint::black_box(*expect));
        }
    }
    std::hint::black_box(sink);
    let elapsed = start.elapsed();
    let total_reads = f64::from(ITERS) * r.max(1) as f64;
    let ns_total = elapsed.as_nanos() as f64 / f64::from(ITERS);
    (elapsed.as_nanos() as f64 / total_reads, ns_total)
}

// ---------------------------------------------------------------------------
// JSON report
// ---------------------------------------------------------------------------

pub fn reflected_call_memo_json(cpu: &CpuGsw) -> String {
    let Some(state) = cpu.reflected_call.as_ref() else {
        return "{\"armed\":false}".to_string();
    };
    let mut out = format!(
        "{{\"armed\":true,\"audit_period\":{},\"answered\":{},\"would_answer\":{},         \"insns_elided\":{},\"core_clocks_charged\":{},\"bus_clocks_charged\":{},         \"audited\":{},\"relearned\":{},\"drift_forced_audits\":{},         \"a20_retires\":{},\"code_watch_retires\":{},\"code_mark_epoch_retires\":{},         \"fell_through\":{{\"not_memoised\":{},\"entry_state_mismatch\":{},         \"read_set_mismatch\":{},\"read_set_unreadable\":{},\"pending_interrupt\":{},         \"cap\":{},\"device_edge\":{},\"dma_visible\":{},\"not_armed\":{},         \"clock_projection\":{},\"soft_int_posted\":{}}},",
        state.audit_period,
        state.answered,
        state.would_answer,
        state.insns_elided,
        state.core_clocks_charged,
        state.bus_clocks_charged,
        state.audited,
        state.relearned,
        state.drift_forced_audits,
        state.a20_retires,
        state.code_watch_retires,
        state.code_mark_epoch_retires,
        state.not_memoised,
        state.entry_state_mismatch,
        state.read_set_mismatch,
        state.read_set_unreadable,
        state.fell_through_pending_interrupt,
        state.fell_through_cap,
        state.fell_through_device_edge,
        state.fell_through_dma_visible,
        state.fell_through_not_armed,
        state.fell_through_clock_projection,
        state.fell_through_soft_int_posted,
    );
    out.push_str("\"audit_mismatch\":{");
    let mut afirst = true;
    for kind in AUDIT_MISMATCH_ALL {
        if !afirst {
            out.push(',');
        }
        afirst = false;
        out.push_str(&format!(
            "\"{}\":{}",
            kind.name(),
            state.audit_mismatch[kind.index()]
        ));
    }
    out.push_str("},\"retired\":{");
    let mut rfirst = true;
    for cause in RETIRE_CAUSE_ALL {
        if !rfirst {
            out.push(',');
        }
        rfirst = false;
        out.push_str(&format!(
            "\"{}\":{}",
            cause.name(),
            state.retired[cause.index()]
        ));
    }
    out.push_str("},\"keys\":[");
    let mut first = true;
    for (key, ks) in &state.keys {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&format!(
            "{{\"vector\":{},\"ax\":{},\"int_eip\":{},\"cs_selector\":{},\"ss_selector\":{},\"cpl\":{},\"vm\":{},\
             \"disarmed\":{},\"trips_seen\":{},\"disarmed_returns\":{},\"rearms\":{},\"learn_attempts\":{},\"learned\":{},\"audits\":{},\"write_value_disarms\":{},\"bus_drift_acc\":{},\"memos\":{},\
             \"write_class_r_pinned\":{},\"write_class_r_unpinned\":{},\
             \"write_class_d\":{},\"write_class_w_other\":{},\
             \"stability_total_attempts\":{},\"stability_stable_attempts\":{},\
             \"stability_distinct_triples\":{},\"stability_longest_run\":{},\
             \"stability_stable_fraction\":{:.6},\
             \"write_set_size_count\":{},\"write_set_size_min\":{},\"write_set_size_max\":{},\"write_set_size_median\":{},\
             \"read_set_size_count\":{},\"read_set_size_min\":{},\"read_set_size_max\":{},\"read_set_size_median\":{},\
             \"learn_refused\":{{",
            key.vector,
            key.ax,
            key.int_eip,
            key.cs_selector,
            key.ss_selector,
            key.cpl,
            key.vm,
            ks.disarmed,
            ks.trips_seen,
            ks.disarmed_returns,
            ks.rearms,
            ks.learn_attempts,
            ks.learned,
            ks.audits,
            ks.write_value_disarms,
            ks.bus_drift_acc,
            state.memos.get(key).map(Vec::len).unwrap_or(0),
            ks.write_class_r_pinned,
            ks.write_class_r_unpinned,
            ks.write_class_d,
            ks.write_class_w_other,
            ks.stability.total_attempts,
            ks.stability.stable_attempts,
            ks.stability.distinct_triple_count(),
            ks.stability.longest_run,
            ks.stability.stable_fraction(),
            ks.write_set_size.count,
            ks.write_set_size.min,
            ks.write_set_size.max,
            ks.write_set_size.median().unwrap_or(0),
            ks.read_set_size.count,
            ks.read_set_size.min,
            ks.read_set_size.max,
            ks.read_set_size.median().unwrap_or(0),
        ));
        let mut rfirst = true;
        for reason in LEARN_REFUSED_ALL {
            if !rfirst {
                out.push(',');
            }
            rfirst = false;
            out.push_str(&format!(
                "\"{}\":{}",
                reason.name(),
                ks.learn_refused[reason.index()]
            ));
        }
        out.push_str("},\"read_set_offenders\":[");
        let mut ofirst = true;
        for (phys, expected, live, hits) in &ks.read_set_offenders {
            if !ofirst {
                out.push(',');
            }
            ofirst = false;
            out.push_str(&format!(
                "{{\"phys\":{phys},\"expected\":{expected},\"live\":{live},\"hits\":{hits}}}"
            ));
        }
        out.push_str("],\"audit_offenders\":[");
        let mut afirst2 = true;
        for (phys, predicted, observed, hits, provenance) in &ks.audit_offenders {
            if !afirst2 {
                out.push(',');
            }
            afirst2 = false;
            out.push_str(&format!(
                "{{\"phys\":{phys},\"predicted\":{predicted},\"observed\":{observed},\"hits\":{hits},\"provenance\":{provenance}}}"
            ));
        }
        out.push_str("]}");
    }
    out.push_str("]}");
    out
}

#[cfg(test)]
#[path = "reflected_call_memo_test.rs"]
mod tests;
