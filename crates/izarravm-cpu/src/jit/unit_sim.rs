// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Trace-driven simulator for hypothetical v1 superblock translation units.
//!
//! This module is a measurement aid only. It consumes facts about retired guest instructions and
//! reconstructs what superblock units would have covered, so the project can judge whether a
//! Cranelift-backed backend would beat the current native path before taking that dependency. It
//! never influences guest execution: nothing here is on the interpreter's hot path and no method
//! returns a value that steers a real translation.
//!
//! `retired_in_units` counts every guest instruction retired while a unit entry is open, including
//! instructions Track C would execute as interpreter call-outs inside the unit; C2's structural
//! metric must use the same contract.
//!
//! # Mechanisms and the ladder
//!
//! A `UnitSim` runs under a [`SimConfig`] that turns link mechanisms on or off. The default config
//! (`L0`) reproduces the v1 sim byte for byte; each higher rung on [`SimConfig::ladder`] enables
//! one more mechanism. Rungs 0..=4 are strict supersets; L5 and L6 are QEMU-shaped alternatives
//! layered on the L3 config rather than on L4 (L5 REPLACES L4's per-site inline target cache with a
//! global hashed lookup, so `itc` is L4-only and not carried at L5+):
//!
//! - `L1` `loop_direct`: a `LoopNear` back-edge behaves exactly as a `DirectNear` branch.
//! - `L2` `call_ret_link`: `CallNear`/`CallIndirect` push a shadow-stack return address and a
//!   `Return` links back to it.
//! - `L3` `smc_restamp`: a store confined to the tail of a unit's members restamps rather than
//!   invalidating the whole unit.
//! - `L4` `itc`: an indirect exit whose target is stable across observations links through a
//!   one-entry inline target cache (L4 only; disabled at L5+).
//! - `L5` `ght`: a QEMU-class global hashed target lookup (a 4096-slot direct-mapped table keyed by
//!   the pinned `tb_jmp_cache_hash_func` of the linear) resolves raw indirect exits and returns,
//!   layered on the L2 shadow return stack. Replaces L4's monomorphic cache as the indirect policy.
//! - `L6` `io_callout`: a `touches_io` instruction no longer closes the entry; it accrues, charges
//!   `io_callouts`, and the entry stays open (models an in-unit port call-out).
//! - `P` `poll_skip` (rung 7, `ladder(6) + poll_skip`): models poll-wait elision. A side-effect-free
//!   device-wait loop (identical eligible traversals, `>= 1` io read, no store) fast-forwards after a
//!   two-traversal warm-up: elided iterations still retire (so `retired_in_units` and `io_callouts`
//!   stay identical to L6) but count in `elided_insns`, and a budget yield no longer closes the
//!   surviving entry (it charges `wait_batch_ends`). `spin_noio_insns` bounds the out-of-scope
//!   memory-poll class. The report prints both `ipe_active` and `ipe_active_slice` quotients.
//!
//! [`SimLadder`] fans one observation stream out to a set of rungs so a single run measures the
//! marginal value of each mechanism against the same trace.

use std::collections::{HashMap, HashSet};

/// The control-transfer shape of an observed instruction, as far as the unit model cares.
///
/// The classifier (`block::observed_transfer`) emits the precise kind for every control transfer;
/// the four rich kinds carry information (call/return structure, loop back-edges) that the Track C
/// configs exploit. `UnitSim::effective_kind` lowers them per the sim's config before any side
/// effect, so the DEFAULT config (which lowers all four rich kinds to `Indirect`) reproduces the v1
/// semantics exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransferKind {
    None,
    DirectNear {
        target: u32,
    },
    Indirect,
    /// A near CALL rel with a statically computable target (recursion, not a loop back-edge).
    CallNear {
        target: u32,
    },
    /// A near indirect CALL (0xFF /2): the target is not statically known.
    CallIndirect,
    /// A near RET (0xC2/0xC3).
    Return,
    /// A LOOP/LOOPcc/JCXZ near branch with a statically computable target.
    LoopNear {
        target: u32,
    },
}

/// One retired guest instruction, described by the facts the simulator needs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ObservedInsn {
    pub linear: u32,
    pub len: u8,
    /// Physical page of the instruction start (`physical_address >> 12`).
    pub physical_page: u32,
    pub mode_key: u32,
    pub transfer: TransferKind,
    pub is_terminator: bool,
    pub touches_io: bool,
    /// P ONLY: this instruction may write memory (a conservative-true classifier; see
    /// `block::writes_memory`). Read exclusively by the rung-P poll-wait detector to disqualify a
    /// loop body that stores; no rung except P consults it (bit-identity checklist item 11). Derived
    /// in `unit_sim_observe` after the sim-disabled early return, so the disabled path is untouched.
    pub writes_memory: bool,
    /// P ONLY: this instruction is a port IN (0xE4/0xE5/0xEC/0xED). Read exclusively by the rung-P
    /// poll-wait detector (a qualifying wait needs >= 1 io read) and the io histogram; no rung except
    /// P consults it. Derived in `unit_sim_observe` after the sim-disabled early return.
    pub io_read: bool,
}

/// Per-sim mechanism gates. `L0` is all-false and reproduces the v1 sim; each higher ladder rung
/// enables one more field (see [`SimConfig::ladder`] and the module docs).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SimConfig {
    /// L1+: a `LoopNear` back-edge is treated as a `DirectNear` branch.
    pub loop_direct: bool,
    /// L2+: `CallNear`/`CallIndirect` push a shadow-stack return address and `Return` links back.
    pub call_ret_link: bool,
    /// L3+: a store confined to a member's tail restamps the unit rather than killing it.
    pub smc_restamp: bool,
    /// L4 ONLY: a stable indirect target links through a one-entry inline target cache. Disabled at
    /// L5+ (the global hashed lookup replaces it), so this is `rung == 4`, not `rung >= 4`.
    pub itc: bool,
    /// L5+: a QEMU-class global hashed target lookup resolves raw indirect exits and returns.
    pub ght: bool,
    /// L6+: a `touches_io` instruction accrues as an in-unit call-out instead of closing the entry.
    pub io_callout: bool,
    /// P ONLY (rung 7): model poll-wait elision on top of the L6 config. A side-effect-free device
    /// wait loop (identical eligible traversals, an io read, no store) fast-forwards: elided
    /// iterations still retire but are counted in `elided_insns`, and a budget yield no longer closes
    /// the surviving entry (`wait_batch_ends`). See the module's poll-wait section.
    pub poll_skip: bool,
}

impl SimConfig {
    /// The config for ladder rung `rung` (`0..=7`). Rungs 0..=4 are strict supersets (`ladder(4)`
    /// enables all four v1-superset mechanisms). L5 and L6 are QEMU-shaped: `ladder(5)` is the L3
    /// config (loop/call-ret/smc) with `ght` on and `itc` OFF (the global lookup replaces the L4
    /// monomorphic cache), and `ladder(6)` adds `io_callout` on top of L5. Rung 7 (labelled `P`) is
    /// `ladder(6)` plus `poll_skip`.
    pub(crate) fn ladder(rung: u8) -> Self {
        SimConfig {
            loop_direct: rung >= 1,
            call_ret_link: rung >= 2,
            smc_restamp: rung >= 3,
            itc: rung == 4,
            ght: rung >= 5,
            io_callout: rung >= 6,
            poll_skip: rung >= 7,
        }
    }
}

/// Headline counters produced by the simulation. Public because it is returned by
/// `CpuGsw::take_unit_sim_report`, the diagnostic accessor Track C tooling reads.
///
/// The link counters partition every linked transfer by the raw kind that produced it, so a
/// downstream summary can add them without double counting: total links = `linked_transfers`
/// (`DirectNear`) + `loop_links` (`LoopNear`) + `call_links` (`CallNear`) + `ret_links` (`Return`
/// via the shadow stack) + `itc_hits` (`Indirect`/`CallIndirect` via the L4 inline target cache) +
/// `ght_hits` (`Indirect`/`CallIndirect` via the L5 global hashed table) + `ght_ret_hits` (`Return`
/// resolved via the L5 global hashed table). `ght_ret_hits` is attributed by ORIGIN (any table hit
/// resolving a return, whether the shadow stack was empty or its compare fell through), so that
/// `ret_links + ght_ret_hits` bounds pure-QEMU return resolution from above.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SimReport {
    pub entries: u64,
    pub retired_in_units: u64,
    pub linked_transfers: u64,
    pub unresolved_exits: u64,
    pub side_exits_io: u64,
    pub side_exits_async: u64,
    pub sim_invalidations: u64,
    pub units_built: u64,
    pub units_rebuilt: u64,
    /// Out-of-window `LoopNear` links to a known unit entry (L1+).
    pub loop_links: u64,
    /// `CallNear` links to a known callee unit entry (L2+).
    pub call_links: u64,
    /// `Return` links back to a shadow-stack return address that is a known unit entry (L2+).
    pub ret_links: u64,
    /// Indirect/`CallIndirect` links through the inline target cache to a stable target (L4).
    pub itc_hits: u64,
    /// Indirect/`CallIndirect` links resolved through the L5 global hashed target table.
    pub ght_hits: u64,
    /// `Return` links resolved through the L5 global hashed target table (empty-stack returns and
    /// non-empty returns whose shadow compare fell through to the table both count here).
    pub ght_ret_hits: u64,
    /// L6 `touches_io` instructions that accrued as an in-unit call-out instead of closing the entry.
    pub io_callouts: u64,
    /// Restamps charged at unit re-entry after a tail-confined store dirtied it (L3+).
    pub sim_restamps: u64,
    /// P: instructions in poll-wait iterations that a deadline-stopping skip would have elided. These
    /// iterations STILL retire (so `retired_in_units` is config-independent); this counts how many of
    /// those retirements the skip removes from the active stream. Upper bound on real skip coverage
    /// (a counted-io delay loop is elided here but not by a real skip); see gate caveat (c).
    pub elided_insns: u64,
    /// P: poll-wait episodes entered (one per loop that reached wait mode).
    pub elided_waits: u64,
    /// P: budget yields (`note_batch_end`) absorbed by a surviving wait-mode entry instead of closing
    /// it. The pessimistic quotient (`ipe_active_slice`) prices one dispatch per absorbed yield.
    pub wait_batch_ends: u64,
    /// P DIAGNOSTIC (never elided): instructions in identical, store-free, io-FREE spin loops that
    /// pass every wait test except the io-read requirement (Doom's maketic memory tic-spin). Bounds
    /// the headroom a memory-poll-capable skip could add; out of scope for the io-read model here.
    pub spin_noio_insns: u64,
}

/// A simulated translation unit, keyed by `(entry_linear, mode_key)` in the owner map.
struct Unit {
    /// Linear PCs discovered as members while the unit executed, mapped to the instruction length
    /// last seen at that PC. The length feeds the L3 restamp classifier (member span checks).
    members: HashMap<u32, u8>,
    /// Physical pages this unit owns entries for in `page_owners`, so invalidation only visits
    /// the pages that actually reference the unit.
    pages: HashSet<u32>,
    /// Physical page of the unit's entry instruction, kept for the cap-sweep metric.
    entry_physical_page: u32,
    /// Set by an L3 tail-confined store; charges one `sim_restamps` at the next entry, then clears.
    dirty: bool,
}

/// Reasons an open entry stops accruing, each mapped to its report counter.
enum ExitReason {
    Unresolved,
    Async,
    Io,
}

/// The unit-and-mode key used to identify a unit.
type UnitKey = (u32, u32);

/// A deferred resolution armed by a closing transfer that keeps the entry OPEN across it: the check
/// is settled at the next observation. At most one is armed per closing instruction (a non-empty
/// `Return` arms `Return`; `Indirect`/`CallIndirect` arm `Itc` at L4 or `Ght` at L5+; an empty-stack
/// `Return` arms `Ght` at L5+; those raw kinds are disjoint, so they never conflict). `None` is the
/// L0/L1 state and leaves the dual-successor prediction untouched.
#[derive(Clone, Copy)]
enum Deferred {
    None,
    /// A `Return` popped `expected` off the shadow stack; a link fires if the next observation lands
    /// there and it is a known same-mode unit entry (stage 1), else the L5 table probe (stage 2).
    Return {
        expected: u32,
    },
    /// An indirect exit consulted the inline target cache at `cache_key`; `cached` is the remembered
    /// target (if any). A hit fires if the next observation matches `cached`; any miss refills
    /// `cache_key` with the observed target. L4 only.
    Itc {
        cache_key: (UnitKey, u32),
        cached: Option<u32>,
    },
    /// An L5 exit resolved entirely against the global hashed table at the next observation. Carries
    /// only its ORIGIN (`from_return` distinguishes an empty-stack `Return`, which counts
    /// `ght_ret_hits`, from a raw `Indirect`/`CallIndirect`, which counts `ght_hits`).
    Ght {
        from_return: bool,
    },
}

/// State of the currently open entry, if any.
struct OpenEntry {
    /// The unit instructions currently accrue to. This can switch on a linked transfer.
    key: UnitKey,
    /// The 4 KiB window (`entry_linear >> 12`) of the current unit.
    window: u32,
    mode_key: u32,
    /// The fall-through PC (`linear + len`) of the last accrued instruction.
    predicted_fallthrough: u32,
    /// The recorded taken target of the last accrued direct branch, if it was one.
    direct_target: Option<u32>,
    /// A pending deferred resolution (L2+), orthogonal to the dual-successor prediction above: when
    /// it is `None` the fall-through/direct-target continuity runs exactly as at L0.
    deferred: Deferred,
    /// Linked transfers already spent by this entry.
    quota_used: usize,
}

/// Which report counter a successful deferred link charges, so `link_switch` can share one
/// switch-and-accrue path across all four deferred-resolution kinds.
enum LinkKind {
    /// L2+ shadow-stack return link (`ret_links`).
    Ret,
    /// L4 inline-target-cache hit (`itc_hits`).
    Itc,
    /// L5 global-table hit resolving an indirect exit (`ght_hits`).
    Ght,
    /// L5 global-table hit resolving a return (`ght_ret_hits`).
    GhtRet,
}

/// What an L3 store does to one unit whose page it touched.
enum WriteAction {
    /// v1 whole-unit kill (invalidate + rebuild on next entry).
    Kill,
    /// Tail-confined store: keep the unit, mark it dirty for a restamp at next entry.
    Restamp,
    /// The store hit the owned page but no member span: nothing happens.
    Ignore,
}

/// The number of slots in the L5 global hashed target table (QEMU `TB_JMP_CACHE_BITS` = 12, so
/// `1 << 12 = 4096`, verified against master).
const GHT_SLOTS: usize = 4096;

/// The L5 global hashed target table: a 4096-slot direct-mapped array, each slot holding a
/// `(linear, mode_key)` unit-entry key (collisions evict). Wrapped so [`UnitSim`] can keep deriving
/// `Default` (a bare `[Option<_>; 4096]` has no `Default` impl).
struct GhtTable(Vec<Option<(u32, u32)>>);

impl Default for GhtTable {
    fn default() -> Self {
        GhtTable(vec![None; GHT_SLOTS])
    }
}

/// The direct-mapped slot index for `lin`, pinned to QEMU softmmu `tb_jmp_cache_hash_func`
/// (`accel/tcg/cputlb.c`) with `TARGET_PAGE_BITS = 12` and `TB_JMP_PAGE_BITS = 6`. Applied to the
/// LINEAR ONLY: QEMU hashes the pc alone and validates the rest of the key in the slot compare, so
/// the `mode_key` is stored but not hashed. `0b111111_000000 == 0xFC0` is `TB_JMP_PAGE_MASK` and
/// `0b111111 == 0x3F` is `TB_JMP_ADDR_MASK`; the result is always in `0..4096`.
fn ght_index(lin: u32) -> usize {
    let tmp = lin ^ (lin >> (12 - 6));
    (((tmp >> (12 - 6)) & 0b111111_000000) | (tmp & 0b111111)) as usize
}

/// The largest loop body (in instructions) rung P will treat as a candidate poll-wait, and the cap
/// on the in-progress traversal buffer. A qualifying wait body is `<= POLL_QUAL_BODY`; `POLL_MAX_BODY`
/// is a generous slack so a short prologue before the loop head still lets the anchor be found before
/// the buffer is force-reset (a longer non-looping run is not a tight poll loop and never qualifies).
const POLL_QUAL_BODY: usize = 8;
const POLL_MAX_BODY: usize = 64;

/// One instruction recorded in the in-progress traversal buffer (rung P). Only the facts the
/// poll-wait shape test needs: the PC (for the identical-sequence compare) and the three
/// disqualifiers/qualifiers.
#[derive(Clone, Copy)]
struct PollInsn {
    pc: u32,
    /// The lowered transfer was `None` (a straight-line body instruction). An eligible traversal is
    /// all-`None` bodies plus the single terminating in-window back-edge.
    none_transfer: bool,
    io_read: bool,
    writes_memory: bool,
}

/// Rung-P poll-wait detector state, held on the sim (NOT the open entry) so it survives budget yields
/// (`note_batch_end`): a real device poll batch-ends every iteration, so the identical traversals
/// that arm wait mode only accumulate across yields. Reset by `close` (a discontinuity or unresolved
/// exit breaks the loop) and by any observation that cannot be part of the tracked loop. Inert unless
/// `config.poll_skip`.
#[derive(Default)]
struct PollState {
    /// The loop anchor: the in-window back-edge target PC (a member of the loop), once discovered.
    anchor: Option<u32>,
    /// The in-progress traversal, from the last anchor arrival up to and including the current
    /// instruction. A completed traversal is the tail from the last anchor occurrence.
    cur: Vec<PollInsn>,
    /// The last completed traversal's PC sequence, for the identical-sequence compare.
    ref_pcs: Vec<u32>,
    has_ref: bool,
    /// Consecutive identical completed traversals (reset to 1 on a mismatch). Wait mode arms at 2.
    matches: u32,
    /// In wait mode: elided iterations are counted and budget yields no longer close the entry.
    wait: bool,
    /// The previous instruction closed a traversal; the next observation is expected to re-enter at
    /// the anchor. If it does not, the loop exited (fall-through) or was diverted: detection resets.
    expect_anchor: bool,
}

/// Simulates unit growth from a stream of observed instructions.
#[derive(Default)]
pub(crate) struct UnitSim {
    config: SimConfig,
    units: HashMap<UnitKey, Unit>,
    /// Physical page -> the unit keys whose members live on that page.
    page_owners: HashMap<u32, HashSet<UnitKey>>,
    /// Keys ever built, so a rebuild after invalidation is not miscounted as a first build.
    ever_built: HashSet<UnitKey>,
    /// Shadow return-address stack (L2+), capped at 64 with drop-oldest on overflow. Reviewed
    /// semantics, frozen: the stack is per-sim, survives batch ends, entry closes, and SMC
    /// invalidations, and carries no mode tagging; stale frames bias approximately neutral (a false
    /// ret_link needs the next instruction to coincidentally match a stale address AND be a known
    /// same-mode unit entry).
    shadow: Vec<u32>,
    /// Inline target cache (L4): `(unit key, exit linear) -> last observed target`. Entries rooted
    /// at a unit are flushed when that unit is killed (see `flush_itc_for`).
    itc_cache: HashMap<(UnitKey, u32), u32>,
    /// Global hashed target table (L5): direct-mapped, indexed by [`ght_index`] of the linear.
    /// Populated at entry-open, at deferred-check resolution, and on shadow-ret-link success; probed
    /// at deferred-check resolution. Stale slots for killed units fail naturally (a hit requires the
    /// unit to still exist), so no scan is needed on invalidation.
    ght_table: GhtTable,
    open: Option<OpenEntry>,
    /// Rung-P poll-wait detector (inert unless `config.poll_skip`).
    poll: PollState,
    report: SimReport,
}

/// Shadow-stack cap: the deepest call nesting the sim tracks. Overflow drops the oldest frame.
const SHADOW_CAP: usize = 64;

impl UnitSim {
    /// Build a sim running under `config`. `UnitSim::default()` is `with_config(SimConfig::default())`
    /// which is ladder rung 0 (v1 parity).
    pub(crate) fn with_config(config: SimConfig) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    /// Feed one retired instruction into the model.
    pub(crate) fn observe(&mut self, insn: ObservedInsn) {
        loop {
            let (mode_key, deferred_active) = match self.open.as_ref() {
                None => {
                    self.open_entry(insn);
                    return;
                }
                Some(open) => (open.mode_key, !matches!(open.deferred, Deferred::None)),
            };

            // A pending deferred resolution (L2+) settles before ordinary continuity. On success it
            // links and accrues the current instruction; on failure it closes the entry unresolved
            // and the instruction is reprocessed as a fresh entry.
            if deferred_active {
                if self.resolve_deferred(insn) {
                    return;
                }
                continue;
            }

            // A mode switch cannot continue the current unit.
            if insn.mode_key != mode_key {
                self.close(ExitReason::Unresolved);
                continue;
            }

            // Continuity: the instruction must be the predicted fall-through or the recorded taken
            // target. Anything else models an interrupt or fault landing mid-unit.
            let open = self.open.as_ref().expect("open entry present");
            let continues = insn.linear == open.predicted_fallthrough
                || open.direct_target == Some(insn.linear);
            if !continues {
                self.close(ExitReason::Async);
                continue;
            }

            // Growth window: the instruction must sit in the unit's page and end inside it.
            let in_window = (insn.linear >> 12) == open.window;
            let end_in_window = (insn.linear & 0xfff) + insn.len as u32 <= 0x1000;
            if !in_window || !end_in_window {
                self.close(ExitReason::Unresolved);
                continue;
            }

            self.accrue(insn);
            return;
        }
    }

    /// Record a guest store of `width` bytes starting at physical `physical`. Every unit that owns a
    /// touched page is invalidated (v1), or, under L3, restamped when the store is confined to a
    /// member's tail. The store visits the first byte's page and, when it spans a boundary, the last
    /// byte's page too (a store touches at most two pages here), matching the caller's old two-call
    /// contract. The production feed lives inside `note_code_write_hit`, so the sim only sees stores
    /// that survived G2 same-value elision: it mirrors the post-elision production invalidation
    /// choke, never the raw store stream. Sized stores are additionally watch-gated at the caller
    /// (write_linear_fragment probes the code watch before calling the hit path), so the sim never
    /// observes sized stores that miss all watched code.
    pub(crate) fn note_code_write(&mut self, physical: u32, width: u32) {
        let first_page = physical >> 12;
        let last = physical.wrapping_add(width.saturating_sub(1));
        let last_page = last >> 12;
        self.note_code_write_page(physical, width, first_page);
        if last_page != first_page {
            self.note_code_write_page(physical, width, last_page);
        }
    }

    /// Process the portion of a store that lands on physical `page`.
    fn note_code_write_page(&mut self, physical: u32, width: u32, page: u32) {
        // L0-L2: the v1 whole-unit kill, structurally identical to the original single-page path.
        if !self.config.smc_restamp {
            let owners = match self.page_owners.remove(&page) {
                Some(owners) => owners,
                None => return,
            };
            let open_key = self.open.as_ref().map(|open| open.key);
            let mut hit_open = false;
            for key in owners {
                if let Some(unit) = self.units.remove(&key) {
                    self.report.sim_invalidations += 1;
                    self.drop_ownership(&key, &unit.pages);
                    self.flush_itc_for(&key);
                }
                if Some(key) == open_key {
                    hit_open = true;
                }
            }
            if hit_open {
                self.close(ExitReason::Unresolved);
            }
            return;
        }

        // L3: classify the store against each owning unit's members. A surviving (restamp) write
        // keeps the unit, so we cannot wholesale-remove the page entry; iterate a snapshot instead.
        let owners: Vec<UnitKey> = match self.page_owners.get(&page) {
            Some(owners) => owners.iter().copied().collect(),
            None => return,
        };
        let (wlo, whi) = page_write_range(physical, width, page);
        let open_key = self.open.as_ref().map(|open| open.key);
        let mut hit_open = false;
        for key in owners {
            match self.classify_write(&key, wlo, whi) {
                WriteAction::Kill => {
                    if let Some(unit) = self.units.remove(&key) {
                        self.report.sim_invalidations += 1;
                        self.drop_ownership(&key, &unit.pages);
                        self.flush_itc_for(&key);
                    }
                    if Some(key) == open_key {
                        hit_open = true;
                    }
                }
                WriteAction::Restamp => {
                    if let Some(unit) = self.units.get_mut(&key) {
                        unit.dirty = true;
                    }
                    // A store into the executing region forces an exit in any real design, so the
                    // open entry closes even though the unit survives.
                    if Some(key) == open_key {
                        hit_open = true;
                    }
                }
                WriteAction::Ignore => {}
            }
        }
        if hit_open {
            self.close(ExitReason::Unresolved);
        }
    }

    /// Classify an L3 store's page-local byte range `[wlo, whi)` against unit `key`'s members. All
    /// of a unit's members share one linear window, so their offset-within-page is directly
    /// comparable with the store's (the physical-remap caveat is documented on `accrue`).
    fn classify_write(&self, key: &UnitKey, wlo: u32, whi: u32) -> WriteAction {
        // An empty range writes nothing; without this guard a zero-width store strictly inside a
        // member span would satisfy the tail-confinement test and restamp spuriously.
        if wlo == whi {
            return WriteAction::Ignore;
        }
        let unit = match self.units.get(key) {
            Some(unit) => unit,
            None => return WriteAction::Ignore,
        };
        let mut intersected = false;
        let mut touches_first = false;
        let mut all_within_tail = true;
        for (&m_start, &m_len) in &unit.members {
            let m_off = m_start & 0xfff;
            let m_end = m_off + m_len as u32;
            if wlo < m_end && m_off < whi {
                intersected = true;
                if wlo <= m_off && m_off < whi {
                    touches_first = true;
                }
                if !(wlo > m_off && whi <= m_end) {
                    all_within_tail = false;
                }
            }
        }
        if !intersected {
            return WriteAction::Ignore;
        }
        if touches_first || !all_within_tail {
            return WriteAction::Kill;
        }
        WriteAction::Restamp
    }

    /// End of an execution batch. Any open entry closes as a budget yield with no exit counter,
    /// mirroring the real backend where each yield is a fresh dispatcher round trip. A pending
    /// deferred check, however, is charged unresolved before the entry clears: the resolution it was
    /// waiting for never arrives.
    pub(crate) fn note_batch_end(&mut self) {
        if let Some(open) = self.open.as_ref() {
            // P: a budget yield does NOT close a surviving wait-mode entry (BLOCKER-1). It charges a
            // wait_batch_end - the substrate-yield the pessimistic quotient prices - and keeps the
            // entry open so the poll continues to accrue and elide across the yield. An eligible
            // wait-mode loop never has a pending deferred (its bodies are `None` + one back-edge), so
            // this branch never strands one.
            if self.config.poll_skip && self.poll.wait {
                self.report.wait_batch_ends += 1;
                return;
            }
            if !matches!(open.deferred, Deferred::None) {
                self.close(ExitReason::Unresolved);
                return;
            }
        }
        // A budget yield is not a discontinuity, so the poll detector is deliberately NOT reset here:
        // a real poll batch-ends every iteration, and detection must survive to accumulate the
        // identical traversals that arm wait mode. The next observation's anchor check catches a
        // genuine loop exit across the yield.
        self.open = None;
    }

    /// The headline counters.
    pub(crate) fn report(&self) -> SimReport {
        self.report
    }

    /// Per-unit `(member_count, entry_physical_page)` pairs, for recomputing the structural metric
    /// under member caps and physical-window exclusions during the cap sweep.
    pub(crate) fn unit_member_histogram(&self) -> Vec<(usize, u32)> {
        let mut v: Vec<(usize, u32)> = self
            .units
            .values()
            .map(|unit| (unit.members.len(), unit.entry_physical_page))
            .collect();
        // Sorted so evidence output is deterministic across runs despite hash-map iteration order.
        v.sort_unstable();
        v
    }

    /// Open a new entry on `insn`, building or rebuilding its unit as needed, then accrue it.
    fn open_entry(&mut self, insn: ObservedInsn) {
        let key = (insn.linear, insn.mode_key);
        self.report.entries += 1;

        if let Some(unit) = self.units.get_mut(&key) {
            // A resident unit dirtied by an L3 tail store is restamped on this entry.
            if unit.dirty {
                unit.dirty = false;
                self.report.sim_restamps += 1;
            }
        } else {
            if self.ever_built.insert(key) {
                self.report.units_built += 1;
            } else {
                self.report.units_rebuilt += 1;
            }
            self.units.insert(
                key,
                Unit {
                    members: HashMap::new(),
                    pages: HashSet::new(),
                    entry_physical_page: insn.physical_page,
                    dirty: false,
                },
            );
        }

        self.open = Some(OpenEntry {
            key,
            window: insn.linear >> 12,
            mode_key: insn.mode_key,
            predicted_fallthrough: 0,
            direct_target: None,
            deferred: Deferred::None,
            quota_used: 0,
        });
        // L5 install hook: a dispatcher-entered unit installs its entry key in the global table.
        if self.config.ght {
            self.ght_install(insn.linear, insn.mode_key);
        }
        self.accrue(insn);
    }

    /// Count `insn` into the open unit and then apply its exit or continuation behaviour.
    fn accrue(&mut self, insn: ObservedInsn) {
        self.report.retired_in_units += 1;

        let key = self.open.as_ref().expect("accrue with an open entry").key;
        let unit = self.units.get_mut(&key).expect("open entry's unit exists");
        // `insert` overwrites the member length every accrual (feeds the L3 span classifier) and
        // returns `None` only on the first insert, which is exactly when page ownership is recorded.
        // Caveat, accepted: if a linear PC's physical page is remapped while the unit lives, the new
        // page is not recorded (acceptable for this linear-keyed diagnostic).
        let first_insert = unit.members.insert(insn.linear, insn.len).is_none();
        if first_insert {
            unit.pages.insert(insn.physical_page);
            self.page_owners
                .entry(insn.physical_page)
                .or_default()
                .insert(key);
        }

        // Terminators end the unit even when they also carry a transfer or touch I/O. This check
        // stays BEFORE the I/O rule: a terminator that touches I/O closes unresolved (as at L0), and
        // never counts an io call-out.
        if insn.is_terminator {
            self.close(ExitReason::Unresolved);
            return;
        }
        if insn.touches_io {
            if self.config.io_callout {
                // L6: the port access is an in-unit call-out. The instruction accrues (already done
                // above), charges an io call-out, and processing CONTINUES to normal transfer
                // handling so the entry stays open.
                self.report.io_callouts += 1;
            } else {
                self.close(ExitReason::Io);
                return;
            }
        }

        let effective = self.effective_kind(insn.transfer);

        // P: feed the poll-wait detector every non-terminating accrued instruction (a terminator
        // closed above; `close` resets detection). This never changes retired_in_units, io_callouts,
        // or any exit counter - it only drives the new P-only counters and the wait-mode entry
        // survival in `note_batch_end`.
        if self.config.poll_skip {
            let window = self.open.as_ref().expect("open entry present").window;
            self.poll_observe(&insn, effective, window);
        }

        match effective {
            TransferKind::Indirect => self.close_or_arm_itc(insn),
            TransferKind::DirectNear { target } => self.handle_direct(insn, target, false),
            TransferKind::LoopNear { target } => self.handle_direct(insn, target, true),
            TransferKind::CallNear { target } => self.handle_call_near(insn, target),
            TransferKind::CallIndirect => self.handle_call_indirect(insn),
            TransferKind::Return => self.handle_return(),
            TransferKind::None => {
                let open = self.open.as_mut().expect("open entry present");
                open.predicted_fallthrough = insn.linear.wrapping_add(insn.len as u32);
                open.direct_target = None;
            }
        }
    }

    /// Lower a classifier-emitted `TransferKind` to the shape the accrue logic acts on, per the
    /// sim's config. This is the seam where each Track C mechanism turns on: a rich kind whose
    /// mechanism is OFF is lowered to `Indirect` (so it is indistinguishable from a raw indirect at
    /// the observation point, with no pushes, no cache probe, no deferred check); a rich kind whose
    /// mechanism is ON passes through unchanged for its dedicated handler.
    ///
    /// ORDERING CONTRACT: this lowering runs BEFORE any side effect of the transfer, so a
    /// lowered-to-`Indirect` transfer performs exactly what a raw `Indirect` would and no more. At
    /// L0 every rich kind is lowered, so every observed behaviour is byte-identical to the v1 sim.
    ///
    /// We keep the return type `TransferKind` rather than a narrowed enum. A narrowed type only buys
    /// compile-time safety when the set of accrue-handled kinds is fixed, but that set GROWS with the
    /// config (at L4 accrue legitimately handles all seven variants). A config-parametrized narrow
    /// enum would just re-encode this per-config lowering with more machinery and no added guarantee,
    /// so the plain lowering, plus accrue's now-exhaustive match with a real arm per kind (no
    /// `unreachable!`), is the clearer contract.
    fn effective_kind(&self, t: TransferKind) -> TransferKind {
        match t {
            TransferKind::LoopNear { .. } if !self.config.loop_direct => TransferKind::Indirect,
            TransferKind::CallNear { .. } if !self.config.call_ret_link => TransferKind::Indirect,
            TransferKind::CallIndirect if !self.config.call_ret_link => TransferKind::Indirect,
            TransferKind::Return if !self.config.call_ret_link => TransferKind::Indirect,
            other => other,
        }
    }

    /// Apply the direct-near branch rules for the just-accrued branch at `insn` targeting `target`.
    /// `is_loop` routes an out-of-window link to `loop_links` instead of `linked_transfers`; it is
    /// only ever set for a `LoopNear` reaching here under L1+ (a raw `DirectNear` passes `false`).
    fn handle_direct(&mut self, insn: ObservedInsn, target: u32, is_loop: bool) {
        let (window, mode_key, quota_used) = {
            let open = self.open.as_ref().expect("open entry present");
            (open.window, open.mode_key, open.quota_used)
        };
        let fall = insn.linear.wrapping_add(insn.len as u32);
        // Members are only ever inserted under the open window check and the window always equals
        // the current unit's entry page (including after a linked-transfer switch), so membership
        // implies in_window; testing the window alone is exact.
        let in_window = (target >> 12) == window;

        // A back-edge or any in-window target keeps the entry open; the target joins when it is
        // next observed. Both the target and the fall-through remain valid successors.
        if in_window {
            let open = self.open.as_mut().expect("open entry present");
            open.predicted_fallthrough = fall;
            open.direct_target = Some(target);
            return;
        }

        // Out of window. A jump to a known unit's entry chains; otherwise the exit is unresolved.
        let target_key = (target, mode_key);
        if self.units.contains_key(&target_key) {
            if quota_used >= crate::jit::direct::MAX_CHAIN_BLOCKS {
                self.close(ExitReason::Unresolved);
                return;
            }
            if is_loop {
                self.report.loop_links += 1;
            } else {
                self.report.linked_transfers += 1;
            }
            let open = self.open.as_mut().expect("open entry present");
            open.quota_used += 1;
            open.key = target_key;
            open.window = target >> 12;
            open.predicted_fallthrough = fall;
            open.direct_target = Some(target);
        } else {
            self.close(ExitReason::Unresolved);
        }
    }

    /// L2+ near-CALL: push the return address, then link to a known callee entry or close unresolved.
    fn handle_call_near(&mut self, insn: ObservedInsn, target: u32) {
        let ret_addr = insn.linear.wrapping_add(insn.len as u32);
        self.push_shadow(ret_addr);
        let (mode_key, quota_used) = {
            let open = self.open.as_ref().expect("open entry present");
            (open.mode_key, open.quota_used)
        };
        let target_key = (target, mode_key);
        if self.units.contains_key(&target_key) {
            if quota_used >= crate::jit::direct::MAX_CHAIN_BLOCKS {
                self.close(ExitReason::Unresolved);
                return;
            }
            self.report.call_links += 1;
            let open = self.open.as_mut().expect("open entry present");
            open.quota_used += 1;
            open.key = target_key;
            open.window = target >> 12;
            open.predicted_fallthrough = ret_addr;
            open.direct_target = Some(target);
        } else {
            self.close(ExitReason::Unresolved);
        }
    }

    /// L2+ near indirect CALL: push the return address, then behave as an indirect exit (unresolved,
    /// or ITC-armed at L4).
    fn handle_call_indirect(&mut self, insn: ObservedInsn) {
        let ret_addr = insn.linear.wrapping_add(insn.len as u32);
        self.push_shadow(ret_addr);
        self.close_or_arm_itc(insn);
    }

    /// L2+ near RET. A non-empty shadow stack pops and arms a deferred return-link check (stage-1
    /// shadow compare, then the L5 table). An empty shadow stack closes unresolved at L2-L4 (v1
    /// parity, nothing to defer); at L5 (review finding M2) it instead arms a plain `Ght` check so
    /// the return can still resolve via the global table, matching pure QEMU's per-ret lookup.
    fn handle_return(&mut self) {
        match self.shadow.pop() {
            None => {
                if self.config.ght {
                    let open = self.open.as_mut().expect("open entry present");
                    open.deferred = Deferred::Ght { from_return: true };
                } else {
                    self.close(ExitReason::Unresolved);
                }
            }
            Some(expected) => {
                let open = self.open.as_mut().expect("open entry present");
                open.deferred = Deferred::Return { expected };
            }
        }
    }

    /// An indirect exit at `insn`. Branch order per the L0-L4 bit-identity checklist: L4 arms a
    /// deferred `Itc` check against the cached target; else L5 arms a `Ght` check resolved against
    /// the global table; else (L0-L3) it closes unresolved exactly like a v1 indirect.
    fn close_or_arm_itc(&mut self, insn: ObservedInsn) {
        if self.config.itc {
            let key = self.open.as_ref().expect("open entry present").key;
            let cache_key = (key, insn.linear);
            let cached = self.itc_cache.get(&cache_key).copied();
            let open = self.open.as_mut().expect("open entry present");
            open.deferred = Deferred::Itc { cache_key, cached };
            return;
        }
        if self.config.ght {
            let open = self.open.as_mut().expect("open entry present");
            open.deferred = Deferred::Ght { from_return: false };
            return;
        }
        self.close(ExitReason::Unresolved);
    }

    /// Settle the open entry's pending deferred check against the just-observed `insn`. Returns
    /// `true` when the check linked (and `insn` was accrued into the switched unit), `false` when it
    /// failed (the entry was closed unresolved and `insn` must be reprocessed as a fresh entry).
    fn resolve_deferred(&mut self, insn: ObservedInsn) -> bool {
        let (deferred, mode_key, quota_used) = {
            let open = self.open.as_ref().expect("deferred needs an open entry");
            (open.deferred, open.mode_key, open.quota_used)
        };

        // The L4 inline target cache keeps its original two-outcome behaviour, byte-identical: a hit
        // links (`itc_hits`), any non-hit refills the cache and closes unresolved. L4 has `ght` off,
        // so none of the L5 install/table machinery below is reachable here.
        if let Deferred::Itc { cache_key, cached } = deferred {
            let success = cached == Some(insn.linear)
                && insn.mode_key == mode_key
                && self.units.contains_key(&(insn.linear, mode_key));
            if !success {
                self.itc_cache.insert(cache_key, insn.linear);
                self.close(ExitReason::Unresolved);
                return false;
            }
            return self.link_switch(insn, mode_key, quota_used, LinkKind::Itc);
        }

        // Whether the observed instruction is a known same-mode unit entry: the precondition for
        // both the stage-1/stage-2 link tests and the L5 install-on-resolution rule.
        let observed_known_unit =
            insn.mode_key == mode_key && self.units.contains_key(&(insn.linear, mode_key));

        let expected = match deferred {
            Deferred::Return { expected } => Some(expected),
            Deferred::Ght { .. } => None,
            Deferred::Itc { .. } => unreachable!("Itc handled above"),
            Deferred::None => unreachable!("resolve_deferred with no pending check"),
        };
        let from_return = match deferred {
            Deferred::Return { .. } => true,
            Deferred::Ght { from_return } => from_return,
            _ => unreachable!(),
        };

        // Stage 1 (Return only): the shadow-stack compare.
        let stage1 = expected == Some(insn.linear) && observed_known_unit;
        // Stage 2 (L5): the global hashed table probe. Runs for a `Ght` check from the start and for
        // a `Return` whose shadow compare failed. Reads the slot BEFORE any install below.
        let stage2 = !stage1
            && self.config.ght
            && observed_known_unit
            && self.ght_slot_matches(insn.linear, insn.mode_key);

        // L5 install-on-resolution: install the observed target whenever it is a known same-mode
        // unit entry, on hit AND on miss (a stage-1 shadow-ret-link success installs here too). This
        // runs AFTER the stage-2 probe so it cannot spuriously satisfy its own lookup.
        if self.config.ght && observed_known_unit {
            self.ght_install(insn.linear, insn.mode_key);
        }

        if stage1 {
            return self.link_switch(insn, mode_key, quota_used, LinkKind::Ret);
        }
        if stage2 {
            let kind = if from_return {
                LinkKind::GhtRet
            } else {
                LinkKind::Ght
            };
            return self.link_switch(insn, mode_key, quota_used, kind);
        }
        self.close(ExitReason::Unresolved);
        false
    }

    /// Switch the open entry to the just-resolved target `insn` and accrue it, charging one link of
    /// `kind`. A successful link consumes quota exactly like the direct-link path; exhaustion closes
    /// unresolved and reprocesses the target as a fresh entry (returning `false`). Returns `true`
    /// once the target has accrued into the switched unit.
    fn link_switch(
        &mut self,
        insn: ObservedInsn,
        mode_key: u32,
        quota_used: usize,
        kind: LinkKind,
    ) -> bool {
        if quota_used >= crate::jit::direct::MAX_CHAIN_BLOCKS {
            self.close(ExitReason::Unresolved);
            return false;
        }
        match kind {
            LinkKind::Ret => self.report.ret_links += 1,
            LinkKind::Itc => self.report.itc_hits += 1,
            LinkKind::Ght => self.report.ght_hits += 1,
            LinkKind::GhtRet => self.report.ght_ret_hits += 1,
        }
        let target_key = (insn.linear, mode_key);
        {
            let open = self.open.as_mut().expect("open entry present");
            open.quota_used += 1;
            open.key = target_key;
            open.window = insn.linear >> 12;
            open.deferred = Deferred::None;
            open.predicted_fallthrough = 0;
            open.direct_target = None;
        }
        self.accrue(insn);
        true
    }

    /// Install `(lin, mode)` in the L5 global hashed table (direct-mapped: overwrites any collision).
    fn ght_install(&mut self, lin: u32, mode: u32) {
        self.ght_table.0[ght_index(lin)] = Some((lin, mode));
    }

    /// Whether the L5 table slot for `lin` holds exactly `(lin, mode)`.
    fn ght_slot_matches(&self, lin: u32, mode: u32) -> bool {
        self.ght_table.0[ght_index(lin)] == Some((lin, mode))
    }

    /// Push a return address onto the shadow stack, dropping the oldest frame on overflow.
    fn push_shadow(&mut self, addr: u32) {
        if self.shadow.len() >= SHADOW_CAP {
            self.shadow.remove(0);
        }
        self.shadow.push(addr);
    }

    /// Close the open entry, charging the given exit counter.
    fn close(&mut self, reason: ExitReason) {
        match reason {
            ExitReason::Unresolved => self.report.unresolved_exits += 1,
            ExitReason::Async => self.report.side_exits_async += 1,
            ExitReason::Io => self.report.side_exits_io += 1,
        }
        self.open = None;
        // P: a real close is a discontinuity (an ISR landing, an unresolved exit): the tracked loop
        // is broken, so wait mode ends and detection restarts (re-entry pays the 2 warm-up
        // traversals). The pre-declared drift lives here - an ISR closing a surviving wait entry
        // charges side_exits_async at P where the L6 batch-end cleared it counter-free.
        if self.config.poll_skip {
            self.poll_reset();
        }
    }

    /// Drop every inline-target-cache entry rooted at a killed unit. A real inline cache is
    /// embedded in the discarded code, so a rebuilt unit pays the first-encounter miss again;
    /// without this flush `itc_hits` would be biased upward across SMC kills.
    fn flush_itc_for(&mut self, key: &UnitKey) {
        self.itc_cache.retain(|(unit, _), _| unit != key);
    }

    /// Remove a unit key from the physical pages it owned, visiting only those pages.
    fn drop_ownership(&mut self, key: &UnitKey, pages: &HashSet<u32>) {
        for page in pages {
            if let Some(owners) = self.page_owners.get_mut(page) {
                owners.remove(key);
                if owners.is_empty() {
                    self.page_owners.remove(page);
                }
            }
        }
    }

    /// P: fold one accrued instruction into the poll-wait detector. `effective` is the config-lowered
    /// transfer and `window` the open unit's 4 KiB window (for the in-window back-edge test). Purely
    /// observational except for its effect on `note_batch_end` survival and the P-only counters.
    fn poll_observe(&mut self, insn: &ObservedInsn, effective: TransferKind, window: u32) {
        let p = insn.linear;
        let none_transfer = matches!(effective, TransferKind::None);
        // An in-window near direct/loop branch is the only shape that can terminate an eligible
        // traversal; carry its target so the back-edge test below can check it against the anchor.
        let dir_target = match effective {
            TransferKind::DirectNear { target } | TransferKind::LoopNear { target }
                if (target >> 12) == window =>
            {
                Some(target)
            }
            _ => None,
        };

        // A completed traversal expected the loop to re-enter at its anchor. If this observation is
        // not the anchor, the loop exited (a fall-through past the back-edge) or was diverted: leave
        // wait mode and restart detection, then process this instruction as a fresh sequence start.
        if self.poll.expect_anchor {
            self.poll.expect_anchor = false;
            if Some(p) != self.poll.anchor {
                self.poll_reset_detection();
            }
        }

        // Anything that is neither a straight-line body nor an in-window near branch (a raw indirect,
        // a return, a call, an out-of-window link) cannot belong to an eligible poll loop: reset.
        if !none_transfer && dir_target.is_none() {
            self.poll_reset_detection();
            return;
        }

        // Bound the in-progress buffer: a long non-looping run is not a tight poll loop.
        if self.poll.cur.len() >= POLL_MAX_BODY {
            self.poll_reset_detection();
        }
        self.poll.cur.push(PollInsn {
            pc: p,
            none_transfer,
            io_read: insn.io_read,
            writes_memory: insn.writes_memory,
        });

        // A near branch back to a member already seen this traversal closes it (and pins the anchor
        // the first time). Any other in-window direct is a forward body branch: it stays in the
        // buffer as a non-`None` instruction, which will disqualify the traversal at completion.
        if let Some(target) = dir_target {
            let body = &self.poll.cur[..self.poll.cur.len() - 1];
            let is_backedge = match self.poll.anchor {
                Some(a) => target == a,
                None => body.iter().any(|x| x.pc == target),
            };
            if is_backedge {
                self.poll_complete(target);
            }
        }
    }

    /// P: a traversal just closed on an in-window back-edge to `anchor`. Evaluate it against the
    /// previous traversal, arm/hold/exit wait mode, and count elided or spin-noio instructions.
    fn poll_complete(&mut self, anchor: u32) {
        self.poll.anchor = Some(anchor);
        // The traversal is the buffer tail from the last anchor occurrence (drop any prologue that
        // preceded the loop head on the first pass).
        let start = self
            .poll
            .cur
            .iter()
            .rposition(|x| x.pc == anchor)
            .unwrap_or(0);
        let tail = &self.poll.cur[start..];
        let len = tail.len();
        // Eligible: every instruction except the terminating back-edge is a straight-line body.
        let eligible = tail
            .iter()
            .take(len.saturating_sub(1))
            .all(|x| x.none_transfer);
        let io_reads = tail.iter().filter(|x| x.io_read).count();
        let has_write = tail.iter().any(|x| x.writes_memory);
        let pcs: Vec<u32> = tail.iter().map(|x| x.pc).collect();

        let identical = self.poll.has_ref && self.poll.ref_pcs == pcs;
        self.poll.matches = if identical { self.poll.matches + 1 } else { 1 };
        self.poll.ref_pcs = pcs;
        self.poll.has_ref = true;

        // The wait shape: identical eligible traversal, body within the cap, no store. An io read
        // makes it a poll-skip candidate; its absence (but everything else passing) makes it a
        // memory-poll spin (diagnostic only, never elided).
        let small = eligible && len <= POLL_QUAL_BODY && !has_write;
        let qualifies = small && io_reads >= 1;
        let noio = small && io_reads == 0;

        if self.poll.wait {
            if qualifies && identical {
                // An elided iteration: it still retired (accrued above), but a deadline-stopping skip
                // would have fast-forwarded through it. Count its instructions.
                self.report.elided_insns += len as u64;
            } else {
                // A diverging traversal exits wait mode; normal rules already applied to its
                // instructions as they accrued.
                self.poll.wait = false;
            }
        } else if qualifies && self.poll.matches >= 2 {
            // Two identical eligible io-read traversals: arm wait mode. This (second) traversal is
            // the warm-up and is NOT elided; the third onward are.
            self.poll.wait = true;
            self.report.elided_waits += 1;
        }
        if noio && identical && self.poll.matches >= 2 {
            self.report.spin_noio_insns += len as u64;
        }

        self.poll.cur.clear();
        self.poll.expect_anchor = true;
    }

    /// P: clear the in-progress detection (anchor, buffer, reference) but leave `wait` to the caller.
    fn poll_reset_detection(&mut self) {
        self.poll.wait = false;
        self.poll.anchor = None;
        self.poll.cur.clear();
        self.poll.ref_pcs.clear();
        self.poll.has_ref = false;
        self.poll.matches = 0;
        self.poll.expect_anchor = false;
    }

    /// P: fully reset the detector, including wait mode (called from `close`, a discontinuity).
    fn poll_reset(&mut self) {
        self.poll.wait = false;
        self.poll_reset_detection();
    }
}

/// The page-local byte range `[lo, hi)` (offsets within `page`) that a store of `width` bytes at
/// physical `physical` covers on that page. An empty range (`lo == hi`) means the store does not
/// touch the page. Computed in `u64` so a store near the top of the address space cannot overflow.
fn page_write_range(physical: u32, width: u32, page: u32) -> (u32, u32) {
    let start = physical as u64;
    let end = start + width as u64;
    let page_lo = (page as u64) << 12;
    let page_hi = page_lo + 0x1000;
    let lo = start.clamp(page_lo, page_hi) - page_lo;
    let hi = end.clamp(page_lo, page_hi) - page_lo;
    (lo as u32, hi as u32)
}

/// Fans one observation stream out to a set of ladder rungs, so a single guest run measures the
/// marginal value of each mechanism against the same trace. Wired into the CPU via
/// `set_unit_sim_enabled`; unit-tested here.
pub(crate) struct SimLadder {
    sims: Vec<(&'static str, UnitSim)>,
    /// Per-port io-read histogram (behind `IZARRAVM_IO_HIST=1`; `None` = disabled, zero cost). Kept
    /// on the ladder, not the per-rung sims, so an io read is counted ONCE per retirement regardless
    /// of how many rungs observe it. Populated from the observe site (the port is a runtime fact for
    /// the DX forms), read out at the end of a headless run.
    io_hist: Option<HashMap<u16, u64>>,
}

/// The static label for ladder rung `rung` (`0..=7`). Rung 7 is the poll-wait rung, labelled `P`.
fn rung_name(rung: u8) -> &'static str {
    match rung {
        0 => "L0",
        1 => "L1",
        2 => "L2",
        3 => "L3",
        4 => "L4",
        5 => "L5",
        6 => "L6",
        7 => "P",
        _ => panic!("ladder rungs are 0..=7"),
    }
}

impl SimLadder {
    /// The full ladder, one sim per rung `L0..L6`. Test-only: the CPU wiring uses
    /// [`SimLadder::with_rungs`] with the cheaper measurement set.
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_rungs(&[0, 1, 2, 3, 4, 5, 6])
    }

    /// A ladder over an explicit list of rungs, each named `L{rung}` (rung 7 is `P`). Lets a run pick
    /// just the measurement set instead of paying for every rung. The io histogram is off; a headless
    /// run turns it on with [`SimLadder::enable_io_hist`].
    pub(crate) fn with_rungs(rungs: &[u8]) -> Self {
        let sims = rungs
            .iter()
            .map(|&rung| {
                (
                    rung_name(rung),
                    UnitSim::with_config(SimConfig::ladder(rung)),
                )
            })
            .collect();
        Self {
            sims,
            io_hist: None,
        }
    }

    /// Turn on the per-port io-read histogram (headless-run diagnostic, `IZARRAVM_IO_HIST=1`).
    pub(crate) fn enable_io_hist(&mut self) {
        self.io_hist = Some(HashMap::new());
    }

    /// Record one io-read retirement against `port` (a no-op when the histogram is disabled).
    pub(crate) fn record_io_read(&mut self, port: u16) {
        if let Some(hist) = self.io_hist.as_mut() {
            *hist.entry(port).or_insert(0) += 1;
        }
    }

    /// The io-read histogram sorted by count descending (ties broken by ascending port), or `None`
    /// when it was never enabled. The caller caps the printed depth.
    pub(crate) fn io_hist_sorted(&self) -> Option<Vec<(u16, u64)>> {
        self.io_hist.as_ref().map(|hist| {
            let mut v: Vec<(u16, u64)> = hist.iter().map(|(&port, &count)| (port, count)).collect();
            v.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            v
        })
    }

    /// Observe one retired instruction on every rung.
    pub(crate) fn observe(&mut self, insn: ObservedInsn) {
        for (_, sim) in &mut self.sims {
            sim.observe(insn);
        }
    }

    /// Mirror a guest store into every rung.
    pub(crate) fn note_code_write(&mut self, physical: u32, width: u32) {
        for (_, sim) in &mut self.sims {
            sim.note_code_write(physical, width);
        }
    }

    /// End the batch on every rung.
    pub(crate) fn note_batch_end(&mut self) {
        for (_, sim) in &mut self.sims {
            sim.note_batch_end();
        }
    }

    /// The per-rung `(name, report, member-histogram)` triples.
    #[allow(clippy::type_complexity)] // Signature fixed by the Track C task 3 reporting contract.
    pub(crate) fn reports(&self) -> Vec<(&'static str, SimReport, Vec<(usize, u32)>)> {
        self.sims
            .iter()
            .map(|(name, sim)| (*name, sim.report(), sim.unit_member_histogram()))
            .collect()
    }
}

#[cfg(test)]
#[path = "unit_sim_test.rs"]
mod tests;
