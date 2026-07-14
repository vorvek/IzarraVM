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
//! A `UnitSim` runs under a [`SimConfig`] that turns four link mechanisms on or off. The default
//! config (`L0`) reproduces the v1 sim byte for byte; each higher rung on [`SimConfig::ladder`]
//! enables one more mechanism:
//!
//! - `L1` `loop_direct`: a `LoopNear` back-edge behaves exactly as a `DirectNear` branch.
//! - `L2` `call_ret_link`: `CallNear`/`CallIndirect` push a shadow-stack return address and a
//!   `Return` links back to it.
//! - `L3` `smc_restamp`: a store confined to the tail of a unit's members restamps rather than
//!   invalidating the whole unit.
//! - `L4` `itc`: an indirect exit whose target is stable across observations links through a
//!   one-entry inline target cache.
//!
//! [`SimLadder`] fans one observation stream out to all five rungs so a single run measures the
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
    /// L4: a stable indirect target links through a one-entry inline target cache.
    pub itc: bool,
}

impl SimConfig {
    /// The config for ladder rung `rung` (`0..=4`). Each rung is a strict superset of the one below,
    /// so `ladder(0)` is `L0` (all mechanisms off, v1 parity) and `ladder(4)` enables all four.
    #[allow(dead_code)] // Wired into the CPU ladder in Track C task 3; exercised by tests now.
    pub(crate) fn ladder(rung: u8) -> Self {
        SimConfig {
            loop_direct: rung >= 1,
            call_ret_link: rung >= 2,
            smc_restamp: rung >= 3,
            itc: rung >= 4,
        }
    }
}

/// Headline counters produced by the simulation. Public because it is returned by
/// `CpuGsw::take_unit_sim_report`, the diagnostic accessor Track C tooling reads.
///
/// The five link counters partition every linked transfer by the raw kind that produced it, so a
/// downstream summary can add them without double counting: total links = `linked_transfers`
/// (`DirectNear`) + `loop_links` (`LoopNear`) + `call_links` (`CallNear`) + `ret_links` (`Return`) +
/// `itc_hits` (`Indirect`/`CallIndirect` via the inline target cache).
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
    /// Restamps charged at unit re-entry after a tail-confined store dirtied it (L3+).
    pub sim_restamps: u64,
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
/// is settled at the next observation. At most one is armed per closing instruction (`Return` arms
/// `Return`; `Indirect`/`CallIndirect` arm `Itc`; those raw kinds are disjoint, so they never
/// conflict). `None` is the L0/L1 state and leaves the dual-successor prediction untouched.
#[derive(Clone, Copy)]
enum Deferred {
    None,
    /// A `Return` popped `expected` off the shadow stack; a link fires if the next observation lands
    /// there and it is a known same-mode unit entry.
    Return {
        expected: u32,
    },
    /// An indirect exit consulted the inline target cache at `cache_key`; `cached` is the remembered
    /// target (if any). A hit fires if the next observation matches `cached`; any miss refills
    /// `cache_key` with the observed target.
    Itc {
        cache_key: (UnitKey, u32),
        cached: Option<u32>,
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

/// What an L3 store does to one unit whose page it touched.
enum WriteAction {
    /// v1 whole-unit kill (invalidate + rebuild on next entry).
    Kill,
    /// Tail-confined store: keep the unit, mark it dirty for a restamp at next entry.
    Restamp,
    /// The store hit the owned page but no member span: nothing happens.
    Ignore,
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
    /// Shadow return-address stack (L2+), capped at 64 with drop-oldest on overflow.
    shadow: Vec<u32>,
    /// Inline target cache (L4): `(unit key, exit linear) -> last observed target`.
    itc_cache: HashMap<(UnitKey, u32), u32>,
    open: Option<OpenEntry>,
    report: SimReport,
}

/// Shadow-stack cap: the deepest call nesting the sim tracks. Overflow drops the oldest frame.
const SHADOW_CAP: usize = 64;

impl UnitSim {
    /// Build a sim running under `config`. `UnitSim::default()` is `with_config(SimConfig::default())`
    /// which is ladder rung 0 (v1 parity).
    #[allow(dead_code)] // Wired into the CPU ladder in Track C task 3; exercised by tests now.
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
    /// contract.
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
            if !matches!(open.deferred, Deferred::None) {
                self.close(ExitReason::Unresolved);
                return;
            }
        }
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

        // Terminators end the unit even when they also carry a transfer or touch I/O.
        if insn.is_terminator {
            self.close(ExitReason::Unresolved);
            return;
        }
        if insn.touches_io {
            self.close(ExitReason::Io);
            return;
        }

        match self.effective_kind(insn.transfer) {
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

    /// L2+ near RET: an empty shadow stack closes unresolved immediately (v1 parity, nothing to
    /// defer); otherwise pop and arm a deferred return-link check that keeps the entry open.
    fn handle_return(&mut self) {
        match self.shadow.pop() {
            None => self.close(ExitReason::Unresolved),
            Some(expected) => {
                let open = self.open.as_mut().expect("open entry present");
                open.deferred = Deferred::Return { expected };
            }
        }
    }

    /// An indirect exit at `insn`. Under L4 it arms a deferred ITC check against the cached target
    /// and keeps the entry open; otherwise it closes unresolved exactly like a v1 indirect.
    fn close_or_arm_itc(&mut self, insn: ObservedInsn) {
        if !self.config.itc {
            self.close(ExitReason::Unresolved);
            return;
        }
        let key = self.open.as_ref().expect("open entry present").key;
        let cache_key = (key, insn.linear);
        let cached = self.itc_cache.get(&cache_key).copied();
        let open = self.open.as_mut().expect("open entry present");
        open.deferred = Deferred::Itc { cache_key, cached };
    }

    /// Settle the open entry's pending deferred check against the just-observed `insn`. Returns
    /// `true` when the check linked (and `insn` was accrued into the switched unit), `false` when it
    /// failed (the entry was closed unresolved and `insn` must be reprocessed as a fresh entry).
    fn resolve_deferred(&mut self, insn: ObservedInsn) -> bool {
        let (deferred, mode_key, quota_used) = {
            let open = self.open.as_ref().expect("deferred needs an open entry");
            (open.deferred, open.mode_key, open.quota_used)
        };
        let target = match deferred {
            Deferred::Return { expected } => Some(expected),
            Deferred::Itc { cached, .. } => cached,
            Deferred::None => unreachable!("resolve_deferred with no pending check"),
        };
        let success = match target {
            Some(t) => {
                insn.mode_key == mode_key
                    && insn.linear == t
                    && self.units.contains_key(&(t, mode_key))
            }
            None => false,
        };

        // Any ITC non-hit (a miss or a first encounter) refills the cache with the observed target.
        if let Deferred::Itc { cache_key, .. } = deferred {
            if !success {
                self.itc_cache.insert(cache_key, insn.linear);
            }
        }

        if !success {
            self.close(ExitReason::Unresolved);
            return false;
        }

        // A successful link consumes quota exactly like the direct-link path; exhaustion closes
        // unresolved and reprocesses the target as a fresh entry.
        if quota_used >= crate::jit::direct::MAX_CHAIN_BLOCKS {
            self.close(ExitReason::Unresolved);
            return false;
        }
        match deferred {
            Deferred::Return { .. } => self.report.ret_links += 1,
            Deferred::Itc { .. } => self.report.itc_hits += 1,
            Deferred::None => unreachable!(),
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

/// Fans one observation stream out to all five ladder rungs (`L0..=L4`), so a single guest run
/// measures the marginal value of each mechanism against the same trace. Wired into the CPU in
/// Track C task 3; unit-tested here.
#[allow(dead_code)]
pub(crate) struct SimLadder {
    sims: Vec<(&'static str, UnitSim)>,
}

#[allow(dead_code)]
impl SimLadder {
    /// One sim per ladder rung, named `L0..L4`.
    pub(crate) fn new() -> Self {
        let sims = (0u8..=4)
            .map(|rung| {
                let name: &'static str = match rung {
                    0 => "L0",
                    1 => "L1",
                    2 => "L2",
                    3 => "L3",
                    4 => "L4",
                    _ => unreachable!("ladder rungs are 0..=4"),
                };
                (name, UnitSim::with_config(SimConfig::ladder(rung)))
            })
            .collect();
        Self { sims }
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
