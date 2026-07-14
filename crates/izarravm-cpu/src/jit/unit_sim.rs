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

use std::collections::{HashMap, HashSet};

/// The control-transfer shape of an observed instruction, as far as the unit model cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransferKind {
    None,
    DirectNear { target: u32 },
    Indirect,
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

/// Headline counters produced by the simulation.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SimReport {
    pub entries: u64,
    pub retired_in_units: u64,
    pub linked_transfers: u64,
    pub unresolved_exits: u64,
    pub side_exits_io: u64,
    pub side_exits_async: u64,
    pub sim_invalidations: u64,
    pub units_built: u64,
    pub units_rebuilt: u64,
}

/// A simulated translation unit, keyed by `(entry_linear, mode_key)` in the owner map.
struct Unit {
    /// Linear PCs discovered as members while the unit executed.
    members: HashSet<u32>,
    /// Physical page of the unit's entry instruction, kept for the cap-sweep metric.
    entry_physical_page: u32,
}

/// Reasons an open entry stops accruing, each mapped to its report counter.
enum ExitReason {
    Unresolved,
    Async,
    Io,
}

/// The unit-and-mode key used to identify a unit.
type UnitKey = (u32, u32);

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
    /// Linked transfers already spent by this entry.
    quota_used: usize,
}

/// Simulates unit growth from a stream of observed instructions.
#[derive(Default)]
pub(crate) struct UnitSim {
    units: HashMap<UnitKey, Unit>,
    /// Physical page -> the unit keys whose members live on that page.
    page_owners: HashMap<u32, HashSet<UnitKey>>,
    /// Keys ever built, so a rebuild after invalidation is not miscounted as a first build.
    ever_built: HashSet<UnitKey>,
    open: Option<OpenEntry>,
    report: SimReport,
}

impl UnitSim {
    /// Feed one retired instruction into the model.
    pub(crate) fn observe(&mut self, insn: ObservedInsn) {
        loop {
            let open = match self.open.as_ref() {
                None => {
                    self.open_entry(insn);
                    return;
                }
                Some(open) => open,
            };

            // A mode switch cannot continue the current unit.
            if insn.mode_key != open.mode_key {
                self.close(ExitReason::Unresolved);
                continue;
            }

            // Continuity: the instruction must be the predicted fall-through or the recorded taken
            // target. Anything else models an interrupt or fault landing mid-unit.
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

    /// Record that a store hit physical `physical` (an address, not a page). Every unit that owns
    /// the written page is invalidated and rebuilt on next execution.
    pub(crate) fn note_code_write(&mut self, physical: u32) {
        let page = physical >> 12;
        let owners = match self.page_owners.remove(&page) {
            Some(owners) => owners,
            None => return,
        };

        let open_key = self.open.as_ref().map(|open| open.key);
        let mut hit_open = false;
        for key in owners {
            if self.units.remove(&key).is_some() {
                self.report.sim_invalidations += 1;
                self.drop_ownership(&key);
            }
            if Some(key) == open_key {
                hit_open = true;
            }
        }

        // A write into the open unit's footprint also ends the current entry.
        if hit_open {
            self.close(ExitReason::Unresolved);
        }
    }

    /// End of an execution batch. Any open entry closes as a budget yield with no exit counter,
    /// mirroring the real backend where each yield is a fresh dispatcher round trip.
    pub(crate) fn note_batch_end(&mut self) {
        self.open = None;
    }

    /// The headline counters.
    pub(crate) fn report(&self) -> SimReport {
        self.report
    }

    /// Per-unit `(member_count, entry_physical_page)` pairs, for recomputing the structural metric
    /// under member caps and physical-window exclusions during the cap sweep.
    pub(crate) fn unit_member_histogram(&self) -> Vec<(usize, u32)> {
        self.units
            .values()
            .map(|unit| (unit.members.len(), unit.entry_physical_page))
            .collect()
    }

    /// Open a new entry on `insn`, building or rebuilding its unit as needed, then accrue it.
    fn open_entry(&mut self, insn: ObservedInsn) {
        let key = (insn.linear, insn.mode_key);
        self.report.entries += 1;

        if !self.units.contains_key(&key) {
            if self.ever_built.contains(&key) {
                self.report.units_rebuilt += 1;
            } else {
                self.report.units_built += 1;
            }
            self.ever_built.insert(key);
            self.units.insert(
                key,
                Unit {
                    members: HashSet::new(),
                    entry_physical_page: insn.physical_page,
                },
            );
        }

        self.open = Some(OpenEntry {
            key,
            window: insn.linear >> 12,
            mode_key: insn.mode_key,
            predicted_fallthrough: 0,
            direct_target: None,
            quota_used: 0,
        });
        self.accrue(insn);
    }

    /// Count `insn` into the open unit and then apply its exit or continuation behaviour.
    fn accrue(&mut self, insn: ObservedInsn) {
        self.report.retired_in_units += 1;

        let key = self.open.as_ref().expect("accrue with an open entry").key;
        if let Some(unit) = self.units.get_mut(&key) {
            unit.members.insert(insn.linear);
        }
        self.page_owners
            .entry(insn.physical_page)
            .or_default()
            .insert(key);

        // Terminators end the unit even when they also carry a transfer or touch I/O.
        if insn.is_terminator {
            self.close(ExitReason::Unresolved);
            return;
        }
        if insn.touches_io {
            self.close(ExitReason::Io);
            return;
        }

        match insn.transfer {
            TransferKind::Indirect => self.close(ExitReason::Unresolved),
            TransferKind::DirectNear { target } => self.handle_direct(insn, target),
            TransferKind::None => {
                let open = self.open.as_mut().expect("open entry present");
                open.predicted_fallthrough = insn.linear.wrapping_add(insn.len as u32);
                open.direct_target = None;
            }
        }
    }

    /// Apply the direct-near branch rules for the just-accrued branch at `insn` targeting `target`.
    fn handle_direct(&mut self, insn: ObservedInsn, target: u32) {
        let (key, window, mode_key, quota_used) = {
            let open = self.open.as_ref().expect("open entry present");
            (open.key, open.window, open.mode_key, open.quota_used)
        };
        let fall = insn.linear.wrapping_add(insn.len as u32);
        let is_member = self
            .units
            .get(&key)
            .is_some_and(|unit| unit.members.contains(&target));
        let in_window = (target >> 12) == window;

        // A back-edge to a member or any in-window target keeps the entry open; the target joins
        // when it is next observed. Both the target and the fall-through remain valid successors.
        if is_member || in_window {
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
            self.report.linked_transfers += 1;
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

    /// Close the open entry, charging the given exit counter.
    fn close(&mut self, reason: ExitReason) {
        match reason {
            ExitReason::Unresolved => self.report.unresolved_exits += 1,
            ExitReason::Async => self.report.side_exits_async += 1,
            ExitReason::Io => self.report.side_exits_io += 1,
        }
        self.open = None;
    }

    /// Remove a unit key from every physical page it owned.
    fn drop_ownership(&mut self, key: &UnitKey) {
        self.page_owners.retain(|_, owners| {
            owners.remove(key);
            !owners.is_empty()
        });
    }
}

#[cfg(test)]
#[path = "unit_sim_test.rs"]
mod tests;
