// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The Direct backend's structural-stop census and the diagnostic reporting surface on
//! `JitState`: the per-barrier rows, the unbound-exit and dynamic-miss class tallies, and the
//! stall snapshot. Split out of `direct.rs` verbatim to keep that file under the source-line
//! ceiling; nothing here changed but the visibility the module boundary forces.

use super::*;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct BarrierKey {
    opcode: u16,
    modrm_reg: u8,
    operand_form: u8,
    operand_size: u8,
    address_size: u8,
    prefix_mask: u16,
}

impl BarrierKey {
    fn from_insn(insn: &DecodedInsn) -> Self {
        let operand_form = match insn.operand {
            None => 0,
            Some(DecodedOperand::Reg(_)) => 1,
            Some(DecodedOperand::Mem(_)) => 2,
        };
        let mut prefix_mask = u16::from(insn.prefixes.operand_size_override)
            | (u16::from(insn.prefixes.address_size_override) << 1)
            | (u16::from(insn.prefixes.lock) << 2);
        prefix_mask |= match insn.prefixes.rep {
            None => 0,
            Some(crate::RepKind::Repe) => 1 << 3,
            Some(crate::RepKind::Repne) => 2 << 3,
        };
        if let Some(segment) = insn.prefixes.segment_override {
            prefix_mask |= (u16::try_from(segment_index(segment)).unwrap_or(0) + 1) << 5;
        }
        Self {
            opcode: insn.opcode,
            modrm_reg: insn.modrm.map_or(u8::MAX, |modrm| modrm.reg),
            operand_form,
            operand_size: u8::from(insn.operand_size == OperandSize::Dword),
            address_size: u8::from(insn.address_size == AddressSize::Dword),
            prefix_mask,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct BarrierStats {
    hits: u64,
    runtime_hits: u64,
    native_prefix_instructions: u64,
    native_suffix_instructions: u64,
    max_native_prefix: u8,
    max_native_suffix: u8,
    /// Exits that actually happened into a block this barrier rejected. RUNTIME-weighted, unlike
    /// `hits` (compile attempts) which mis-ranked the ShiftCl slice by three orders of magnitude.
    unbound_exits: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BarrierObservation {
    pub(super) entry_linear: u32,
    pub(super) native_prefix: usize,
    pub(super) native_suffix: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DirectBarrierCensus {
    rows: HashMap<BarrierKey, BarrierStats>,
    /// Why static successor cells were unbound at the exits that hit them, indexed by
    /// `UnboundTarget`. Lives HERE and not in `PerfCounters` on purpose: `PerfCounters` is
    /// embedded in `CpuGsw` ahead of `pending_flags`, whose offset is pinned by
    /// `arch_payload_keeps_pending_flags_offset_pinned` (canonical_state_test.rs) and
    /// `pending_flags_offset` (cpu_test.rs) because emitted code bakes it. Growing `PerfCounters`
    /// for a diagnostic shifts that pin; the census is an `Option<Box<_>>` on `JitState` and costs
    /// the layout nothing.
    unbound: [u64; UnboundTarget::COUNT],
    /// The same classification for DYNAMIC successor misses (computed RET/JMP/CALL targets),
    /// kept in its own lane because the two have different fixes: a static unbound wants its
    /// target compiled, a dynamic miss whose target reads `CompiledButUnlinked` wants a wider
    /// inline cache than the hardcoded two ways.
    unbound_dynamic: [u64; UnboundTarget::COUNT],
    /// Block entry linear -> the barrier row that refused it, so a rejected-target exit can be
    /// attributed back to the opcode responsible. Keyed on linear alone: two rejected blocks
    /// sharing a linear across mode/physical would merge, which is acceptable for a diagnostic
    /// and keeps the compile-side insert to one word.
    rejected_barrier: HashMap<u32, BarrierKey>,
}

impl DirectBarrierCensus {
    fn note_unbound(&mut self, kind: UnboundTarget) {
        self.unbound[kind as usize] += 1;
    }

    fn note_unbound_dynamic(&mut self, kind: UnboundTarget) {
        self.unbound_dynamic[kind as usize] += 1;
    }

    /// Attribute one rejected-target exit back to the barrier that refused that block.
    fn note_unbound_rejected_at(&mut self, linear: u32) {
        let Some(&key) = self.rejected_barrier.get(&linear) else {
            return;
        };
        let row = self.rows.entry(key).or_default();
        row.unbound_exits = row.unbound_exits.saturating_add(1);
    }

    fn record(&mut self, insn: &DecodedInsn, observation: BarrierObservation) {
        let BarrierObservation {
            entry_linear,
            native_prefix,
            native_suffix,
        } = observation;
        let key = BarrierKey::from_insn(insn);
        self.rejected_barrier.insert(entry_linear, key);
        let row = self.rows.entry(key).or_default();
        row.hits = row.hits.saturating_add(1);
        row.native_prefix_instructions = row
            .native_prefix_instructions
            .saturating_add(native_prefix as u64);
        row.native_suffix_instructions = row
            .native_suffix_instructions
            .saturating_add(native_suffix as u64);
        row.max_native_prefix = row.max_native_prefix.max(native_prefix as u8);
        row.max_native_suffix = row.max_native_suffix.max(native_suffix as u8);
    }

    fn note_interpreted(&mut self, insn: &DecodedInsn) {
        // EVERY row, not only the ex-helper families. `runtime_hits` counts how many times the
        // guest actually EXECUTES this shape interpreted, which makes it the census's only
        // per-execution, position-free column - and therefore the only one that can rank a shape
        // by what it costs rather than by where a block happened to stop.
        //
        // It used to carry `&& row.helper_family.is_some()`, an artifact of the commit that
        // instrumented the three helper-eligible opcodes, and that one conjunct left 34 of 36
        // rows reading zero. It is what let `unbound_exits` be the ranking column by default, and
        // `unbound_exits` ranked `0x8C` (a segment reload run ~1.2M times) SEVEN TIMES ABOVE
        // `0x38 /0` (an inner-loop CMP), when the second was worth three times the whole rest of
        // the night put together. Costs nothing when the census is off: the call site in `run.rs`
        // is gated on `barrier_census_active()` before the arguments are even built.
        let key = BarrierKey::from_insn(insn);
        if let Some(row) = self.rows.get_mut(&key) {
            row.runtime_hits = row.runtime_hits.saturating_add(1);
        }
    }

    pub(crate) fn snapshot(&self) -> DirectBarrierCensusSnapshot {
        let mut keyed_rows: Vec<_> = self
            .rows
            .iter()
            .map(|(&key, &stats)| (key, census_row(key, stats)))
            .collect();
        // Sorted by RUNTIME unbound exits first, tiebroken by compile attempts (`hits`).
        keyed_rows.sort_by(|(left_key, left), (right_key, right)| {
            right
                .unbound_exits
                .cmp(&left.unbound_exits)
                .then_with(|| right.hits.cmp(&left.hits))
                .then_with(|| left_key.cmp(right_key))
        });
        DirectBarrierCensusSnapshot {
            rows: keyed_rows.into_iter().map(|(_, row)| row).collect(),
            unbound_targets: UnboundTarget::ALL
                .iter()
                .map(|kind| (kind.label(), self.unbound[*kind as usize]))
                .collect(),
            dynamic_miss_targets: UnboundTarget::ALL
                .iter()
                .map(|kind| (kind.label(), self.unbound_dynamic[*kind as usize]))
                .collect(),
        }
    }
}

fn census_row(key: BarrierKey, stats: BarrierStats) -> DirectBarrierCensusRow {
    DirectBarrierCensusRow {
        opcode: key.opcode,
        modrm_reg: (key.modrm_reg != u8::MAX).then_some(key.modrm_reg),
        operand_form: match key.operand_form {
            1 => "register",
            2 => "memory",
            _ => "none",
        },
        operand_size: if key.operand_size != 0 {
            "dword"
        } else {
            "word"
        },
        address_size: if key.address_size != 0 {
            "dword"
        } else {
            "word"
        },
        prefix_mask: key.prefix_mask,
        unbound_exits: stats.unbound_exits,
        hits: stats.hits,
        runtime_hits: stats.runtime_hits,
        native_prefix_instructions: stats.native_prefix_instructions,
        native_suffix_instructions: stats.native_suffix_instructions,
        max_native_prefix: stats.max_native_prefix,
        max_native_suffix: stats.max_native_suffix,
    }
}

pub(crate) fn barrier_census_default() -> Option<Box<DirectBarrierCensus>> {
    matches!(
        std::env::var("IZARRAVM_DIRECT_BARRIER_CENSUS").as_deref(),
        Ok("1")
    )
    .then(|| Box::new(DirectBarrierCensus::default()))
}

impl crate::jit::JitState {
    pub(super) fn barrier_census_enabled(&self) -> bool {
        self.direct_barrier_census.is_some()
    }

    pub(super) fn record_barrier(&mut self, insn: &DecodedInsn, observation: BarrierObservation) {
        if let Some(census) = self.direct_barrier_census.as_mut() {
            census.record(insn, observation);
        }
    }

    /// Whether the census exists at all. Callers MUST gate on this before calling
    /// `note_barrier_census_interpreted`, which sits on the per-interpreted-instruction retire
    /// path. Checking `is_some` inside the callee is too late for the gate to save anything.
    #[inline]
    pub(crate) fn barrier_census_active(&self) -> bool {
        self.direct_barrier_census.is_some()
    }

    pub(crate) fn note_barrier_census_interpreted(&mut self, insn: &DecodedInsn) {
        if let Some(census) = self.direct_barrier_census.as_mut() {
            census.note_interpreted(insn);
        }
    }

    /// Record why a static successor was unbound. No-op unless the census is allocated, and the
    /// CALLER still gates on `barrier_census_active` so the key construction is skipped too.
    pub(crate) fn note_unbound_target(&mut self, kind: UnboundTarget, linear: u32) {
        if let Some(census) = self.direct_barrier_census.as_mut() {
            census.note_unbound(kind);
            if kind == UnboundTarget::Rejected {
                census.note_unbound_rejected_at(linear);
            }
        }
    }

    /// Unlike the census snapshot this is ALWAYS available: none of its three groups is census
    /// gated, because each is a single increment on a path that has already left native code.
    pub(crate) fn stall_snapshot(&self) -> crate::DirectStallSnapshot {
        crate::DirectStallSnapshot {
            dormant: DormantReason::ALL
                .iter()
                .map(|r| (r.label(), self.stalls.dormant[*r as usize]))
                .collect(),
            link_refusals: LinkRefusal::ALL
                .iter()
                .map(|r| (r.label(), self.stalls.link_refusals[*r as usize]))
                .collect(),
            links_cleared: LinkClearCause::ALL
                .iter()
                .map(|c| (c.label(), self.stalls.links_cleared[*c as usize]))
                .collect(),
            side_exit_segment_limit: self.stalls.side_exit_segment_limit,
            side_exit_x87_eligibility: self.stalls.side_exit_x87_eligibility,
            side_exit_callout_step_break: self.stalls.side_exit_callout_step_break,
            side_exit_callout_abnormal: self.stalls.side_exit_callout_abnormal,
            callout_executed: self.stalls.callout_executed,
        }
    }

    pub(crate) fn note_dynamic_miss_target(&mut self, kind: UnboundTarget) {
        if let Some(census) = self.direct_barrier_census.as_mut() {
            census.note_unbound_dynamic(kind);
        }
    }

    /// Both are one unconditional increment on a path that has already taken a dispatcher exit,
    /// so unlike the census hooks these are NOT gated: the gate would cost as much as the work.
    pub(crate) fn note_side_exit_segment_limit(&mut self) {
        self.stalls.side_exit_segment_limit += 1;
    }

    pub(crate) fn note_side_exit_x87_eligibility(&mut self) {
        self.stalls.side_exit_x87_eligibility += 1;
    }

    pub(crate) fn note_side_exit_callout_step_break(&mut self) {
        self.stalls.side_exit_callout_step_break += 1;
    }

    pub(crate) fn note_side_exit_callout_abnormal(&mut self) {
        self.stalls.side_exit_callout_abnormal += 1;
    }

    /// One unconditional increment inside the call-out helper, which has already left native code
    /// and is about to touch the bus -- the same "the gate would cost as much as the work"
    /// reasoning as the two side-exit counters above.
    pub(crate) fn note_callout_executed(&mut self) {
        self.stalls.callout_executed += 1;
    }

    pub(crate) fn barrier_census_snapshot(&self) -> Option<DirectBarrierCensusSnapshot> {
        self.direct_barrier_census
            .as_deref()
            .map(DirectBarrierCensus::snapshot)
    }

    pub(crate) fn set_barrier_census_enabled(&mut self, enabled: bool) {
        self.direct_barrier_census = enabled.then(|| Box::new(DirectBarrierCensus::default()));
    }
}
