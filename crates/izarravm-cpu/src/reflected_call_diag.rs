// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Slice 0 of the reflected-call HLE design
//! (`dev_docs/2026-09-03-reflected-call-hle-design.md`, `dev_docs/2026-09-03-
//! reflected-call-hle-review.md`): the trip-shape INSTRUMENT. Compiled in only
//! under `--features reflected-call-diagnostic`; a plain build carries none of
//! this code and is byte-identical to `main`. Armed by
//! `IZARRAVM_REFLECTED_CALL_DIAGNOSTIC=shape` or `=journal`; unset or `""`
//! means off, matching the campaign's `IZARRAVM_DIRECT_POLL_SKIP` spelling
//! convention.
//!
//! NO BEHAVIOUR CHANGE is claimed for the OFF arm and for the plain build.
//! Both armed modes DO change host behaviour (timing, and `journal` mode
//! forces the native backend off for the whole run): this is a diagnostic,
//! never used in a graded run, and the design's own learning protocol forces
//! the same knob for the same reason (see the design doc section 3.3).
//!
//! # Two modes, because one hook cannot see both halves of what slice 0 asks
//!
//! **`shape`**: natural execution, no backend forcing. Answers the trip-count,
//! instruction/clock/dispatcher-entry-per-trip and entry-image-distinctness
//! questions (design section 8, items 1-2, 6 first half, 7's "warm" leg, and
//! the edge/batch-cap questions from the review's item 7). It CANNOT see the
//! read/write journal: native-compiled code accesses guest memory through a
//! fast-map raw pointer that bypasses every seam this module could hook (the
//! design's own reason `set_native_backend_enabled(false)` is required for
//! learning -- design section 3.3).
//!
//! **`journal`**: forces the native backend off for the whole run (once, the
//! first time this module sees the knob armed) so every guest memory access
//! passes the seams in section 3.3's table. Answers the read/write-set,
//! address-classification, restored-vs-net, CR3-at-read and refusal-class
//! questions (review section 3, items 2-4, 8). Dispatcher-entry and "warm"
//! clock numbers are not meaningful here (native is off throughout) and are
//! not collected in this mode; run `shape` for those and compare per key --
//! that comparison IS section 5.1's warm-versus-interpreted question.
//!
//! # Trip identity
//!
//! A trip starts at a software `INT n` taken with `is_protected_mode() &&
//! !is_v86_mode()` (the design's entry predicate, section 2). At most one
//! OUTER trip is tracked at a time; a further `INT` while one is open is
//! counted as a nested INT on the open trip and does not itself start
//! tracking (matches finding A4: the trip's own DOS-kernel body issues nested
//! `INT`s by construction).
//!
//! A trip's MATCHING RETURN is the first `IRET` or `RETF`/`RETF imm16` after
//! which `CS.selector`, `CS.base`, `EIP`, `SS.selector` and `ESP` all equal
//! their values at the moment the trip's `INT` retired (`EIP` there is the
//! address one past the `INT`, i.e. what the client resumes at -- the design's
//! own rule, section 3.4). A far return that does not match this exactly
//! leaves the trip open (it is a nested return).
//!
//! A trip is UNMATCHED if it is still open when `MAX_TRIP_INSNS` retired
//! instructions have passed since its `INT` (checked both at the next far
//! return and at the next `INT`, so a trip with no far return at all still
//! eventually closes). This is checked ONLY by that staleness bound, not by
//! "a fresh INT arrived": an earlier version of this module also treated a
//! fresh outer-predicate `INT` arriving while a trip was open as the old
//! trip being abandoned (finding A3's `^C`-into-`INT 23h`-and-never-returns
//! shape), and measurement refuted that as the general rule -- DOS4GW's own
//! `INT 21h AH=0Bh` handler reflects to real mode by calling INTO the DPMI
//! host from PROTECTED MODE, satisfying the outer predicate itself before
//! ever reaching V86, so that rule marked essentially every `AH=0Bh` trip
//! unmatched (722,870 of 722,870 in the first dwell run this instrument
//! made). See `on_int_entry_on`'s comment for the full account. An unmatched
//! trip is closed out and counted in `trips_unmatched`; its partial journal
//! is folded into the key's stats same as a matched trip's, since a partial
//! write-set is still evidence about what the trip touched.

use super::*;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Software `INT` vectors this instrument journals. The design's population is
/// 0x16 (keyboard) and 0x21 (DOS); the review's A6 adds 0x33 (mouse); the task
/// asks for the full firmware/DOS range 0x10-0x33 so a title that reflects
/// through a different vector is not silently invisible.
const VECTOR_LO: u8 = 0x10;
const VECTOR_HI: u8 = 0x33;

/// Belt-and-suspenders bound on how long an open trip may run before it is
/// declared unmatched. The DOSBox-X trace measured 1,092 instructions for one
/// round trip; this is roughly 8x that, matching the design's own
/// `REFLECTED_CALL_MAX_TRIP_INSNS` proposal (8,192).
const MAX_TRIP_INSNS: u64 = 8_192;

/// Read/write-set and instruction/clock sample cap per key, so a multi-second
/// dwell with hundreds of thousands of trips on one key does not grow this
/// module's tables without bound. Distribution stats (min/median/max) are
/// computed from the retained sample; `trips` on the snapshot is the TRUE
/// total, so a reader can see when the sample truncated.
const MAX_SAMPLES_PER_KEY: usize = 200_000;

/// Top-N write addresses reported per key for manual labelling (design section
/// 8, item 3: "the top 16 addresses by frequency with a label").
const TOP_ADDRESSES: usize = 16;

/// How many distinct stack segments (by `SS.selector`) one trip tracks a
/// low-water mark for. Two is the expected shape (the client's own stack and
/// the DPMI/VCPI host's); a trip that touches a third stack falls back to
/// classifying its writes "Other" rather than growing unboundedly.
const MAX_STACK_SEGMENTS: usize = 4;

/// The literal 8 KB the design proposes as `REFLECTED_CALL_DEAD_STACK_CAP`
/// (design section 2). Kept alongside the derived (low-water-mark) rule so the
/// result document can report the refusal histogram under both, which is
/// exactly the review's B2 finding: the two rules disagree sharply on this
/// workload.
const DEAD_STACK_CAP_BYTES: u32 = 8192;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Shape,
    Journal,
}

/// Parse `IZARRAVM_REFLECTED_CALL_DIAGNOSTIC`. Unset or `""` is off, on the
/// `IZARRAVM_DIRECT_POLL_SKIP` convention the design's section 5.3 requires
/// for its own knob; this is a diagnostic-only knob and is not bound by that
/// requirement, but there is no reason to invent a third spelling shape.
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

/// Cheap gate for every call site: one relaxed atomic load, mirroring
/// `int_trace::armed()`'s reasoning (no other memory is published through this
/// flag, so there is no happens-before relationship for an acquire/release
/// pair to establish).
static ARMED: AtomicBool = AtomicBool::new(false);
static ARMED_INIT: OnceLock<()> = OnceLock::new();

#[inline]
fn armed() -> bool {
    ARMED_INIT.get_or_init(|| {
        ARMED.store(mode().is_some(), Ordering::Relaxed);
    });
    ARMED.load(Ordering::Relaxed)
}

fn journal_mode() -> bool {
    mode() == Some(Mode::Journal)
}

/// Whether this run has already forced the native backend off for `journal`
/// mode. Forced exactly once, the first trip start seen; `journal` mode
/// never turns it back on, matching the design's own learning precondition
/// (section 3.3) held for the whole run rather than per trip, which is the
/// only way this module can guarantee coverage of every seam in section
/// 3.3's table without needing per-trip save/restore plumbing this
/// diagnostic does not otherwise need.
static FORCED_INTERPRETER: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Entry image
// ---------------------------------------------------------------------------

/// The strict memo-key fields (design section 3.2), plus the extra state the
/// task asks for distinctness over. `Hash`/`Eq` derive the strict-key equality
/// the design's own memo would use; distinctness counting is "how many
/// distinct values of this struct were seen for this (vector, AH)".
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

/// Architectural EFLAGS bits (386 PRM figure 2-8), masking out the reserved
/// and read-as-1 bit 1 so two images that differ only there are not counted
/// as distinct.
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

/// Which `EntryImage` fields varied across the trips seen for one key, and the
/// first value seen for each (the "don't-care mask" question, design section
/// 3.2 / review A1). Sixteen name/first/varies triples; not a HashSet per
/// field because that would cost one allocation per field per key for a
/// question whose interesting answer is a single bit.
#[derive(Default)]
struct FieldVariance {
    first: Option<EntryImage>,
    varies: [bool; 22],
}

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
            Self::Other => "other",
        }
    }
}

const ALL_CLASSES: [AddressClass; 9] = [
    AddressClass::ClientStack,
    AddressClass::HostStack,
    AddressClass::Bda,
    AddressClass::Gdt,
    AddressClass::Ldt,
    AddressClass::Idt,
    AddressClass::Tss,
    AddressClass::PageTable,
    AddressClass::Other,
];

/// BDA: the fixed low-memory range `0000:0400`-`0000:04FF` (386 PRM /
/// IBM PC AT technical reference). Address-range classification only, exactly
/// as the design's own vocabulary calls for.
const BDA_LO: u32 = 0x0400;
const BDA_HI: u32 = 0x0500;

/// Classify by address against the CPU's live descriptor-table registers and
/// the trip's own stack tracking. GDT/LDT/IDT/TSS take priority over BDA
/// (their ranges never overlap it in a sane guest) and stack takes priority
/// over "other" only when the trip has seen that segment as SS.
///
/// `forced`, when `Some`, skips all of this (the `write_page_walk_entry`
/// seam: a page-table entry has no linear address of its own to classify by).
///
/// `write_system_linear` passes its genuine LINEAR address, so range
/// classification against `idtr`/`gdtr`/`ldtr`/`tr` (which hold linear bases)
/// applies directly. `write_page_walk_entry` has no linear address of its
/// own (a page-table entry is addressed physically); it forces the class
/// instead of calling in here with one.
fn classify(cpu: &CpuGsw, trip: &Trip, linear: u32, forced: Option<AddressClass>) -> AddressClass {
    if let Some(class) = forced {
        return class;
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
    /// Value at the trip's FIRST write to this address, from an independent
    /// non-charging peek taken before that write committed
    /// (`CpuBus::peek_direct_ram`). `None` when the peek missed (a TLB miss on
    /// `probe_linear_read_physical`, or the bus declined) -- the write is
    /// still counted and classified, only "restored vs net" is left unknown
    /// for it.
    pre: Option<u32>,
    /// Value at the trip's LAST write to this address so far.
    latest: u32,
    width_bytes: u32,
}

struct ReadRecord {
    class: AddressClass,
    under_entry_cr3: bool,
}

struct Trip {
    vector: u8,
    ah: u8,
    entry_image: EntryImage,
    /// The return site: EIP one past the `INT`, which is what a matching
    /// `IRET`/`RETF` must land on (design section 3.4).
    return_cs_selector: u16,
    return_cs_base: u32,
    return_eip: u32,
    entry_ss_selector: u16,
    entry_esp: u32,
    entry_cr3: u32,
    entry_tr_base: u32,
    start_instructions: u64,
    start_elapsed_clocks: u64,
    start_jit_entries: u64,
    start_cr3_writes: u64,
    bda_head_at_entry: Option<u32>,
    bda_tail_at_entry: Option<u32>,
    /// Whether the IDT gate this vector reads at entry is a TASK gate (386
    /// PRM type 0x5): a task-gate `INT` delivery does not push a frame at
    /// all (the state saves into the outgoing TSS) and its `IRET` returns
    /// through the TSS back-link rather than a frame pop, so it can never
    /// satisfy `matches_return`'s CS/EIP/SS/ESP comparison even when
    /// architecturally correct. `None` when the peek missed (a TLB miss).
    /// Added after the first TOKAEMM dwell run showed `AH=0Bh` closing
    /// EVERY trip via the staleness bound, `task_switch_trips` true on 98.8%
    /// of them -- this confirms or refutes that reading directly rather than
    /// inferring it from `TR` churn alone.
    entered_via_task_gate: Option<bool>,
    nested_int_count: u32,
    far_transfer_count: u32,
    hw_interrupt_count: u32,
    gp_traps: u32,
    pf_traps: u32,
    rdtsc_seen: bool,
    port_io_seen: bool,
    x87_seen: bool,
    stacks: [Option<StackTrack>; MAX_STACK_SEGMENTS],
    writes: HashMap<u32, WriteRecord>,
    reads: HashMap<u32, ReadRecord>,
}

impl Trip {
    fn start<B: CpuBus>(cpu: &mut CpuGsw, bus: &B, vector: u8, ah: u8) -> Self {
        let entry_image = EntryImage::capture(cpu);
        let regs = &cpu.registers;
        let cs = regs.cs();
        let ss = regs.segment(SegmentIndex::Ss);
        let bda = |offset: u32| -> Option<u32> {
            let phys = cpu.probe_linear_read_physical(offset)?;
            bus.peek_direct_ram(phys, BusWidth::Word)
        };
        let gate_access_addr = cpu.idtr.base.wrapping_add(u32::from(vector) * 8 + 5);
        let entered_via_task_gate = cpu
            .probe_linear_read_physical(gate_access_addr)
            .and_then(|phys| bus.peek_direct_ram(phys, BusWidth::Byte))
            .map(|access| (access & 0x1f) == 0x05);
        let mut stacks: [Option<StackTrack>; MAX_STACK_SEGMENTS] = [None; MAX_STACK_SEGMENTS];
        stacks[0] = Some(StackTrack {
            selector: ss.selector,
            base: ss.base,
            limit: ss.limit,
            low_water_esp: regs.esp(),
            last_esp: regs.esp(),
        });
        Trip {
            vector,
            ah,
            entry_image,
            return_cs_selector: cs.selector,
            return_cs_base: cs.base,
            return_eip: regs.eip,
            entry_ss_selector: ss.selector,
            entry_esp: regs.esp(),
            entry_cr3: cpu.control.cr3,
            entry_tr_base: cpu.tr.base,
            start_instructions: cpu.perf.instructions,
            start_elapsed_clocks: cpu.elapsed_clocks,
            start_jit_entries: cpu.perf.jit_direct_entries,
            start_cr3_writes: cpu.perf.decode_inval_cr3,
            bda_head_at_entry: bda(0x041A),
            bda_tail_at_entry: bda(0x041C),
            entered_via_task_gate,
            nested_int_count: 0,
            far_transfer_count: 0,
            hw_interrupt_count: 0,
            gp_traps: 0,
            pf_traps: 0,
            rdtsc_seen: false,
            port_io_seen: false,
            x87_seen: false,
            stacks,
            writes: HashMap::new(),
            reads: HashMap::new(),
        }
    }

    /// Track the currently active stack segment's low-water and last ESP.
    /// Called on every journalled memory access, regardless of that access's
    /// own address, because it is cheap live-register bookkeeping rather than
    /// a memory read.
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
        // All MAX_STACK_SEGMENTS slots taken by other selectors: this trip
        // touches more stacks than tracked. Silently drop -- the write still
        // gets classified "Other" by `classify`, which is honest (the
        // instrument does not know this segment) rather than wrong.
    }

    fn matches_return(&self, cpu: &CpuGsw) -> bool {
        let regs = &cpu.registers;
        let cs = regs.cs();
        let ss = regs.segment(SegmentIndex::Ss);
        cs.selector == self.return_cs_selector
            && cs.base == self.return_cs_base
            && regs.eip == self.return_eip
            && ss.selector == self.entry_ss_selector
            && regs.esp() == self.entry_esp
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

#[derive(Default)]
struct KeyStats {
    trips: u64,
    unmatched: u64,
    instructions: Samples,
    core_clocks: Samples,
    dispatcher_entries: Samples,
    cr3_writes: Samples,
    field_variance: FieldVariance,
    distinct_images: std::collections::HashSet<EntryImage>,
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
    // Journal-mode only.
    read_set_size: Samples,
    write_set_size: Samples,
    reads_under_other_cr3: u64,
    reads_total: u64,
    write_class_counts: [u64; 9],
    read_class_counts: [u64; 9],
    write_restored: u64,
    write_net_change: u64,
    write_unknown_pre: u64,
    write_dead_8kb: u64,
    write_dead_derived: u64,
    write_live: u64,
    write_addresses: HashMap<u32, u64>,
}

/// Everything this module owns: the mode (resolved once), the single open
/// trip (if any) and every key's aggregated stats. One process-global mutex,
/// on `int_trace`'s stated assumption: this instrument is armed by hand for a
/// single-machine run.
#[derive(Default)]
struct State {
    open: Option<Trip>,
    keys: HashMap<(u8, u8), KeyStats>,
    trips_total: u64,
    trips_unmatched: u64,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

// Every hook below splits into a thin PUBLIC wrapper (checks `armed()`,
// acquires the global lock, forces the backend if `journal` mode needs it)
// and a private `*_on` function that takes `&mut State` explicitly and does
// the real work. The split exists for testing: `reflected_call_diag_test.rs`
// drives the `*_on` functions directly against a locally-constructed `State`,
// so a test exercises the SAME logic the hooks run without touching the
// process-global `Mutex` or the env-var-cached `armed()`/`journal_mode()`
// (which, being process-wide `OnceLock`s, cannot be re-armed per test and
// would otherwise leak one test's arming into every other test in the same
// binary run concurrently by `cargo test`'s default parallelism).

/// `CpuGsw::software_interrupt`'s protected-mode arm, immediately after
/// `bus.interrupt_acknowledge` and before `deliver_interrupt` -- the design's
/// own hook point (section 3.1).
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

    // A trip already open: this INT is NESTED (the open trip's own body
    // issued it -- finding A4) and is counted, never a boundary, REGARDLESS
    // of whether it individually satisfies the outer predicate.
    //
    // An earlier version of this function treated an outer-predicate INT
    // arriving while a trip was open as the old trip being ABANDONED
    // (finding A3's ^C-into-INT-23h shape). Measurement on the owner's
    // TOKAEMM tree refuted that as the general rule: DOS4GW's own `INT 21h`
    // handler for `AH=0Bh` reflects to real mode by calling INTO THE DPMI
    // HOST from PROTECTED MODE (satisfying the outer predicate itself,
    // before ever dropping to V86), so treating that nested call as an
    // abandonment marked essentially EVERY `AH=0Bh` trip unmatched
    // (722,870 of 722,870 in the first dwell run this instrument made).
    //
    // A trip's real outcome being a non-return is caught SOLELY by the
    // staleness bound (`MAX_TRIP_INSNS`) now, checked at every opportunity
    // this function or `on_far_return` gets -- here, so a trip that runs
    // long without ANY far return (nested INTs only) still eventually
    // closes, and a fresh trip may start on the SAME INT that discovered the
    // staleness.
    if let Some(open) = state.open.as_ref() {
        let over_budget = cpu
            .perf
            .instructions
            .saturating_sub(open.start_instructions)
            >= MAX_TRIP_INSNS;
        if !over_budget {
            let open = state.open.as_mut().expect("checked Some above");
            open.nested_int_count = open.nested_int_count.saturating_add(1);
            return;
        }
        finish_trip(cpu, state, true);
    }

    if !(cpu.is_protected_mode() && !cpu.is_v86_mode()) {
        return;
    }

    let ah = ((cpu.registers.eax() >> 8) & 0xff) as u8;
    state.open = Some(Trip::start(cpu, bus, vector, ah));
}

/// Called after `IRET`/`RETF`/`RETF imm16` complete successfully. Checks the
/// open trip's return match; if it matches, closes the trip out. Also bounds
/// an open trip that has run unreasonably long (see the module doc's
/// "unmatched" definition).
pub(crate) fn on_far_return(cpu: &mut CpuGsw) {
    if !armed() {
        return;
    }
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    on_far_return_on(&mut guard, cpu);
}

fn on_far_return_on(state: &mut State, cpu: &mut CpuGsw) {
    let Some(open) = state.open.as_ref() else {
        return;
    };
    if open.matches_return(cpu) {
        finish_trip(cpu, state, false);
        return;
    }
    // Not the outer trip's own return: it is a nested return (the trip's own
    // body returning from a call it made), so the trip stays open. Only the
    // staleness bound closes it here.
    if let Some(open) = state.open.as_mut() {
        let over_budget = cpu
            .perf
            .instructions
            .saturating_sub(open.start_instructions)
            >= MAX_TRIP_INSNS;
        if over_budget {
            finish_trip(cpu, state, true);
        }
    }
}

/// `CALL FAR` / `JMP FAR` inside an open trip: informational only (A4's
/// nested-transfer population), never a trip boundary.
pub(crate) fn on_far_transfer(_cpu: &CpuGsw) {
    if !armed() {
        return;
    }
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    on_far_transfer_on(&mut guard);
}

fn on_far_transfer_on(state: &mut State) {
    if let Some(open) = state.open.as_mut() {
        open.far_transfer_count = open.far_transfer_count.saturating_add(1);
    }
}

/// `CpuGsw::hardware_interrupt`: a device edge landed while a trip is open
/// (design section 8, item 5 / review B6's un-modelled straddle question).
/// Counts the edge on the open trip directly rather than through a
/// perf-counter delta, so it is exact regardless of what else bumps
/// `brk_interrupt`.
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

/// `deliver_interrupt`, vector 13 (#GP) or 14 (#PF), source `Exception`.
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

/// One of the six census-style memory seams (design section 3.3), a data
/// read, or `real_mode_interrupt`'s IVT reads. `linear` is EXCLUDED from the
/// read set if the trip already wrote it (design vocabulary, section 2: a
/// read of the trip's own earlier write is not an input). Records the
/// address's CLASS and whether it was read under the trip's entry CR3
/// (review B4) -- not the value itself, which this instrument's outputs never
/// need: distinguishing "answerable" from "not" only needs to know THAT an
/// address was read, not what it held.
pub(crate) fn note_read(cpu: &mut CpuGsw, linear: u32) {
    if !armed() || !journal_mode() {
        return;
    }
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    note_read_on(&mut guard, cpu, linear);
}

fn note_read_on(state: &mut State, cpu: &mut CpuGsw, linear: u32) {
    let ss = cpu.registers.segment(SegmentIndex::Ss);
    let esp = cpu.registers.esp();
    let cr3_now = cpu.control.cr3;
    let Some(open) = state.open.as_mut() else {
        return;
    };
    open.touch_stack(ss, esp);
    if open.writes.contains_key(&linear) {
        return; // excluded: this is the trip's own earlier write
    }
    let class = classify(cpu, open, linear, None);
    let under_entry_cr3 = cr3_now == open.entry_cr3;
    open.reads.entry(linear).or_insert(ReadRecord {
        class,
        under_entry_cr3,
    });
}

/// One of the six census-style memory seams, a data write. Also updates the
/// trip's stack low-water tracking.
///
/// `already_physical`: `linear` is in fact already a physical address (only
/// `write_page_walk_entry`, which has no linear address of its own to give).
/// `forced_class`: skip range-based classification entirely (again, only
/// `write_page_walk_entry`).
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
    let Some(open) = state.open.as_mut() else {
        return;
    };
    let pre = if open.writes.contains_key(&linear) {
        None
    } else if already_physical {
        bus.peek_direct_ram(linear, width)
    } else {
        cpu.probe_linear_read_physical(linear)
            .and_then(|phys| bus.peek_direct_ram(phys, width))
    };
    open.touch_stack(ss, esp);
    let class = classify(cpu, open, linear, forced_class);
    open.writes
        .entry(linear)
        .and_modify(|rec| rec.latest = value)
        .or_insert(WriteRecord {
            class,
            pre,
            latest: value,
            width_bytes: width.bytes(),
        });
}

// ---------------------------------------------------------------------------
// Trip finalisation
// ---------------------------------------------------------------------------

fn finish_trip(cpu: &mut CpuGsw, guard: &mut State, unmatched: bool) {
    let Some(trip) = guard.open.take() else {
        return;
    };
    guard.trips_total += 1;
    if unmatched {
        guard.trips_unmatched += 1;
    }
    let key = (trip.vector, trip.ah);
    let stats = guard.keys.entry(key).or_default();
    stats.trips += 1;
    if unmatched {
        stats.unmatched += 1;
    }
    stats.instructions.push(
        cpu.perf
            .instructions
            .saturating_sub(trip.start_instructions),
    );
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
    stats.field_variance.observe(&trip.entry_image);
    stats.distinct_images.insert(trip.entry_image);
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

    // Journal-mode-only aggregation. In shape mode `trip.reads`/`trip.writes`
    // are always empty (the hooks that would populate them early-return
    // before the lock in shape mode), so this is a no-op there.
    stats.reads_total += trip.reads.len() as u64;
    stats.read_set_size.push(trip.reads.len() as u64);
    for read in trip.reads.values() {
        read_class_bump(stats, read.class);
        if !read.under_entry_cr3 {
            stats.reads_under_other_cr3 += 1;
        }
    }
    stats.write_set_size.push(trip.writes.len() as u64);
    for (&addr, write) in trip.writes.iter() {
        write_class_bump(stats, write.class);
        *stats.write_addresses.entry(addr).or_insert(0) += 1;
        match write.pre {
            None => stats.write_unknown_pre += 1,
            Some(pre) => {
                let post_masked = mask_to_width(write.latest, write.width_bytes);
                let pre_masked = mask_to_width(pre, write.width_bytes);
                if pre_masked == post_masked {
                    stats.write_restored += 1;
                } else {
                    stats.write_net_change += 1;
                }
            }
        }
        if matches!(
            write.class,
            AddressClass::ClientStack | AddressClass::HostStack
        ) {
            classify_dead_stack(&trip, addr, stats);
        }
    }
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

/// Report the dead-stack verdict for one write under BOTH rules (review B2):
/// the design's literal 8 KB constant cap, and the derived rule bounded by
/// the trip's own observed SP low-water mark on that segment.
fn classify_dead_stack(trip: &Trip, addr: u32, stats: &mut KeyStats) {
    for seg in trip.stacks.iter().flatten() {
        if addr.wrapping_sub(seg.base) >= seg.limit.max(1) {
            continue;
        }
        let abandon_linear = seg.base.wrapping_add(seg.last_esp);
        if addr >= abandon_linear {
            stats.write_live += 1;
            return;
        }
        let below = abandon_linear - addr;
        if below <= DEAD_STACK_CAP_BYTES {
            stats.write_dead_8kb += 1;
        }
        let low_water_linear = seg.base.wrapping_add(seg.low_water_esp);
        if addr >= low_water_linear {
            stats.write_dead_derived += 1;
        }
        return;
    }
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
    pub instructions: StatSummary,
    pub core_clocks: StatSummary,
    pub dispatcher_entries: StatSummary,
    pub cr3_writes: StatSummary,
    pub distinct_entry_images: u64,
    pub distinct_cs_eip: u64,
    pub varying_fields: Vec<&'static str>,
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
    pub read_set_size: StatSummary,
    pub write_set_size: StatSummary,
    pub reads_total: u64,
    pub reads_under_other_cr3: u64,
    pub read_class_counts: Vec<(&'static str, u64)>,
    pub write_class_counts: Vec<(&'static str, u64)>,
    pub write_restored: u64,
    pub write_net_change: u64,
    pub write_unknown_pre: u64,
    pub write_dead_8kb: u64,
    pub write_dead_derived: u64,
    pub write_live: u64,
    pub top_write_addresses: Vec<(u32, u64)>,
}

pub struct ReflectedCallDiagnosticSnapshot {
    pub mode: &'static str,
    pub trips_total: u64,
    pub trips_unmatched: u64,
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
            KeyReport {
                vector,
                ah,
                trips: stats.trips,
                unmatched: stats.unmatched,
                instructions: (&stats.instructions).into(),
                core_clocks: (&stats.core_clocks).into(),
                dispatcher_entries: (&stats.dispatcher_entries).into(),
                cr3_writes: (&stats.cr3_writes).into(),
                distinct_entry_images: stats.distinct_images.len() as u64,
                distinct_cs_eip: stats.distinct_cs_eip.len() as u64,
                varying_fields: stats.field_variance.varying_field_names(),
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
                read_set_size: (&stats.read_set_size).into(),
                write_set_size: (&stats.write_set_size).into(),
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
                write_restored: stats.write_restored,
                write_net_change: stats.write_net_change,
                write_unknown_pre: stats.write_unknown_pre,
                write_dead_8kb: stats.write_dead_8kb,
                write_dead_derived: stats.write_dead_derived,
                write_live: stats.write_live,
                top_write_addresses: top,
            }
        })
        .collect();
    keys.sort_unstable_by_key(|k| (k.vector, k.ah));
    Some(ReflectedCallDiagnosticSnapshot {
        mode: mode_name,
        trips_total: guard.trips_total,
        trips_unmatched: guard.trips_unmatched,
        keys,
    })
}

#[cfg(test)]
#[path = "reflected_call_diag_test.rs"]
mod tests;
