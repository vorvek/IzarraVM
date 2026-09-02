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
//!    hot path**) and from `CpuBus::in_batch_raw_bus_clocks` (already raw and
//!    monotone within a batch). If all 8 triples agree, the key's `learned`
//!    counter increments; otherwise `learn_refused[clocks_unstable]`.
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
//! * `pending_soft_int` and `task_switch` refusal exist as classification
//!   reasons (`LearnRefused`) exercised by unit test injection
//!   (`test_force_refuse`); no production seam posting either condition was
//!   identified in this tree in the time budget for this commit, so neither
//!   is wired to a real hook. `port_io`, `nondeterministic_read` (RDTSC/
//!   RDMSR) and `x87` ARE wired to real production seams.

use super::*;
use crate::reflected_call::*;
use std::collections::HashMap;
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
}

pub(crate) const LEARN_REFUSED_ALL: [LearnRefused; 19] = [
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
    /// The pre-value, if pinned: `Some` only when this trip's own read set
    /// (or translation set, for a page-table entry) already covers this
    /// dword before the first write to it.
    pinned_pre: Option<u32>,
    latest: u32,
    class: AddressClass,
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
    reads: HashMap<u32, u32>,       // aligned phys dword -> first-seen value
    writes: HashMap<u32, WriteObs>, // aligned phys dword -> observation
    translations: HashMap<u32, u32>, // pde/pte aligned phys dword -> value
    read_set_over_cap: bool,
    translation_set_over_cap: bool,
    hazard: Option<LearnRefused>,
    nested_int_count: u32,
    hw_interrupt_seen: bool,
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
            open_bus_raw: bus.in_batch_raw_bus_clocks(),
            stacks,
            stack_segments_over_cap: false,
            journaling: matches!(slot, Slot::JournalA | Slot::JournalB),
            reads: HashMap::new(),
            writes: HashMap::new(),
            translations: HashMap::new(),
            read_set_over_cap: false,
            translation_set_over_cap: false,
            hazard,
            nested_int_count: 0,
            hw_interrupt_seen: false,
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
    reads: HashMap<u32, u32>,
    translations: HashMap<u32, u32>,
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
    natural_samples: Vec<(u64, u64, u64)>,
    pub learn_attempts: u64,
    pub learned: u64,
    pub learn_refused: [u64; 19],
    pub write_class_r_pinned: u64,
    pub write_class_r_unpinned: u64,
    pub write_class_d: u64,
    pub write_class_w_other: u64,
    pub stability: StabilityAcc,
    pub write_set_size: SizeStats,
    pub read_set_size: SizeStats,
}

impl KeyState {
    fn record_failure(&mut self, reason: LearnRefused) {
        self.learn_refused[reason.index()] += 1;
        self.consecutive_failures += 1;
        if self.consecutive_failures >= MEMO_LEARN_BUDGET {
            self.disarmed = true;
        }
        self.slot = SlotState::Warm;
        self.pending_journal = None;
        self.natural_samples.clear();
    }

    fn record_success_and_reset(&mut self) {
        self.consecutive_failures = 0;
        self.slot = SlotState::Warm;
        self.pending_journal = None;
        self.natural_samples.clear();
    }
}

// ---------------------------------------------------------------------------
// The per-CPU state
// ---------------------------------------------------------------------------

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct ReflectedCallMemoState {
    keys: HashMap<MemoKey, KeyState>,
    open: Option<OpenTrip>,
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

fn key_for(cpu: &CpuGsw, vector: u8, ax: u16) -> MemoKey {
    let cs = cpu.registers.cs();
    let ss = cpu.registers.segment(SegmentIndex::Ss);
    MemoKey {
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

/// `CpuGsw::software_interrupt`'s hook. Only tracks a software `INT` taken
/// from protected mode outside V86 (plan section 1's IN scope), vectors
/// `0x10..=0x33` (the same window the diagnostic journals -- BIOS/DOS/DPMI).
pub(crate) fn on_int<B: CpuBus>(cpu: &mut CpuGsw, bus: &B, vector: u8) {
    if cpu.reflected_call.is_none() {
        return;
    }
    if !(0x10..=0x33).contains(&vector) {
        return;
    }
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
            finish_trip(cpu, bus, open, false);
        } else {
            let state = cpu.reflected_call.as_mut().expect("checked above");
            let mut open = open;
            open.nested_int_count = open.nested_int_count.saturating_add(1);
            state.open = Some(open);
            return;
        }
    }
    if !(cpu.is_protected_mode() && !cpu.is_v86_mode()) {
        return;
    }
    let ax = (cpu.registers.eax() & 0xFFFF) as u16;
    let key = key_for(cpu, vector, ax);
    let state = cpu.reflected_call.as_mut().expect("checked above");
    let key_state = state.keys.entry(key).or_default();
    if key_state.disarmed {
        return;
    }
    let slot: Slot = key_state.slot.into();
    let journaling = matches!(slot, Slot::JournalA | Slot::JournalB);
    cpu.reflected_call_journal = journaling;
    let open = OpenTrip::start(cpu, bus, key, slot);
    let state = cpu.reflected_call.as_mut().expect("checked above");
    state.open = Some(open);
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

pub(crate) fn note_read<B: CpuBus>(cpu: &mut CpuGsw, bus: &B, linear: u32) {
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
    if !open.writes.contains_key(&aligned_dword(linear)) {
        let resolved = probe_physical(cpu, bus, linear);
        if let Some((physical, walk)) = resolved {
            let dword = aligned_dword(physical);
            if !open.reads.contains_key(&dword) {
                if open.reads.len() >= MEMO_MAX_READ_SET {
                    open.read_set_over_cap = true;
                } else if let Some(v) = bus.peek_direct_ram(dword, BusWidth::Dword) {
                    open.reads.insert(dword, v);
                }
            }
            if let Some(walk) = walk {
                for wphys in [walk.pde_phys, walk.pte_phys] {
                    let wdword = aligned_dword(wphys);
                    if !open.translations.contains_key(&wdword) {
                        if open.translations.len() >= MEMO_MAX_TRANSLATION_SET {
                            open.translation_set_over_cap = true;
                        } else if let Some(v) = bus.peek_direct_ram(wdword, BusWidth::Dword) {
                            open.translations.insert(wdword, v);
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
        let class = forced_class.unwrap_or_else(|| classify_write(cpu, linear, ss.selector));
        let masked = mask_to_width(value, width.bytes());
        let pinned_pre = open
            .reads
            .get(&dword)
            .copied()
            .or_else(|| open.translations.get(&dword).copied());
        if let Some(rec) = open.writes.get_mut(&dword) {
            rec.latest = masked;
            if rec.pinned_pre.is_none() {
                rec.pinned_pre = pinned_pre;
            }
        } else if open.writes.len() >= MEMO_MAX_REPLAY_WRITES {
            open.hazard.get_or_insert(LearnRefused::ReplaySetTooLarge);
        } else {
            open.writes.insert(
                dword,
                WriteObs {
                    linear,
                    pinned_pre,
                    latest: masked,
                    class,
                },
            );
        }
    }
    cpu.reflected_call.as_mut().expect("checked above").open = Some(open);
}

fn classify_write(cpu: &CpuGsw, linear: u32, ss_selector: u16) -> AddressClass {
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
        return AddressClass::ClientStack; // refined to Host/Client at close by the caller
    }
    AddressClass::Other
}

// ---------------------------------------------------------------------------
// Trip finalisation
// ---------------------------------------------------------------------------

fn finish_trip<B: CpuBus>(cpu: &mut CpuGsw, bus: &B, open: OpenTrip, is_match: bool) {
    cpu.reflected_call_journal = false;
    let close_bus_raw = bus.in_batch_raw_bus_clocks();
    let close_elapsed = cpu.elapsed_clocks;
    let close_rem = cpu.reflected_call_timing_rem();
    let close_persona = cpu.persona();
    let close_instructions = cpu.perf.instructions;
    let exit_image = EntryImage::capture(cpu);

    let key = open.key;
    let state = cpu.reflected_call.as_mut().expect("checked by callers");
    let key_state = state.keys.entry(key).or_default();

    if let Some(hazard) = open.hazard {
        key_state.record_failure(hazard);
        return;
    }
    if !is_match {
        key_state.record_failure(LearnRefused::ClosedWithoutReturn);
        return;
    }
    if open.hw_interrupt_seen {
        key_state.record_failure(LearnRefused::HardwareInterrupt);
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
                &open.writes,
                &open.reads,
                &open.translations,
                insns_b,
                &exit_image,
            ) {
                Ok(()) => {
                    tally_write_classes(key_state, &open, &open.writes);
                    key_state.slot = SlotState::Natural(0);
                }
                Err(reason) => {
                    key_state.record_failure(reason);
                }
            }
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
            let Some(raw_bus) = close_bus_raw.checked_sub(open.open_bus_raw) else {
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
                if all_equal {
                    key_state.learned += 1;
                    key_state.record_success_and_reset();
                } else {
                    key_state.record_failure(LearnRefused::ClocksUnstable);
                }
            } else {
                key_state.slot = SlotState::Natural(i + 1);
            }
        }
    }
}

fn compare_journal(
    baseline: &JournalSnapshot,
    writes: &HashMap<u32, WriteObs>,
    reads: &HashMap<u32, u32>,
    translations: &HashMap<u32, u32>,
    insns_b: u64,
    exit_image: &EntryImage,
) -> Result<(), LearnRefused> {
    if baseline.insns != insns_b || baseline.exit_image != *exit_image {
        return Err(LearnRefused::JournalMismatch);
    }
    if baseline.reads.len() != reads.len() || baseline.translations.len() != translations.len() {
        return Err(LearnRefused::JournalMismatch);
    }
    for (addr, v) in &baseline.reads {
        if reads.get(addr) != Some(v) {
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
        if a.latest != b.latest {
            return Err(LearnRefused::WriteClassN);
        }
    }
    Ok(())
}

fn tally_write_classes(key_state: &mut KeyState, open: &OpenTrip, writes: &HashMap<u32, WriteObs>) {
    for obs in writes.values() {
        if obs.class.never_restored() {
            key_state.write_class_w_other += 1;
            continue;
        }
        let seg_selector = if obs.class == AddressClass::ClientStack {
            open.entry_ss_selector
        } else {
            0
        };
        if matches!(obs.class, AddressClass::ClientStack)
            && open.is_dead_stack(seg_selector, obs.linear)
        {
            key_state.write_class_d += 1;
            continue;
        }
        match obs.pinned_pre {
            Some(pre) if pre == obs.latest => {
                key_state.write_class_r_pinned += 1;
            }
            _ => {
                key_state.write_class_r_unpinned += 1;
            }
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
        for (idx, (_, expect)) in cells.iter().enumerate() {
            let v = buf[idx];
            sink += u64::from(v == *expect);
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
    let mut out = String::from("{\"armed\":true,\"keys\":[");
    let mut first = true;
    for (key, ks) in &state.keys {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&format!(
            "{{\"vector\":{},\"ax\":{},\"int_eip\":{},\"cpl\":{},\"vm\":{},\
             \"disarmed\":{},\"learn_attempts\":{},\"learned\":{},\
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
            key.cpl,
            key.vm,
            ks.disarmed,
            ks.learn_attempts,
            ks.learned,
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
        out.push_str("}}");
    }
    out.push_str("]}");
    out
}

#[cfg(test)]
#[path = "reflected_call_memo_test.rs"]
mod tests;
