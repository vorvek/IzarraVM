// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::timing_class::TimingClass;

/// The loop-invariant half of `rep_chunk_limit`'s admission math, priced once before a REP's
/// loop starts instead of on every fast chunk and every slow iteration. `per_iteration` and the
/// paging setup cost are pure functions of the active mode's bus cost dials and of whether
/// paging is on, and neither can change inside one REP: `bus.rs:1950-1956` documents that every
/// JIT cost dial on `MachineBus` is a function of the active mode alone, a mode change is staged
/// in `pending_mode` and applied only after the current batch, and paging cannot be toggled by
/// the instruction that is currently repeating itself. `CpuBus::rep_data_byte_cost_upper` and
/// `rep_page_walk_cost_upper` both carry that same "must not change within one instruction"
/// promise in their own doc comments, so this holds for every implementor, not only the one it
/// was checked against.
///
/// `self.rep_resume_active` is deliberately NOT folded in here: it is a `CpuGsw` field the
/// resume machinery writes, and there is no equivalent proof that it is invariant across the
/// loop, so `rep_chunk_limit` and `rep_budget_exhausted` keep reading it per call through
/// `rep_core_upper`.
///
/// Two outcomes are NOT loop-invariant and must reach `rep_chunk_limit`'s caller exactly as they
/// did before this hoist, so they are variants of the plan rather than fields inside it:
/// - No REP budget active means UNLIMITED. `compute` checks this FIRST and returns `Unbounded`
///   without touching the paging accessor at all, so a budget-less REP never pays for, or is
///   gated by, a page-walk cost it does not need.
/// - Paging enabled with `bus.rep_page_walk_cost_upper() == None` means YIELD (limit 0), and
///   must reach the caller as `Some(0)`, not sit inside a plan field the budget-less path could
///   observe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RepLimitPlan {
    /// No REP budget is active; `rep_chunk_limit` returns `None` (unlimited) before this is ever
    /// inspected.
    Unbounded,
    /// Paging is on and the bus could not price a page walk; `rep_chunk_limit` returns
    /// `Some(0)`, a yield, without reaching the divide.
    Yield,
    /// The two terms `rep_chunk_limit`'s divide needs, priced once for the whole REP.
    Bounded {
        per_iteration: u64,
        paging_setup: u64,
    },
}

impl RepLimitPlan {
    fn compute<B: CpuBus>(
        cpu: &CpuGsw,
        bus: &B,
        op: StringOp,
        width: BusWidth,
        work: &InstructionWork,
    ) -> Self {
        if cpu.rep_execution.budget.is_none() {
            return Self::Unbounded;
        }
        let bounded = || -> Option<(u64, u64)> {
            let bytes = u64::from(width.bytes());
            let port_io = matches!(op, StringOp::Ins | StringOp::Outs);
            let restricted = port_io
                && matches!(
                    cpu.port_io_priv_mode(),
                    crate::PortIoPrivMode::V86 | crate::PortIoPrivMode::ProtectedUnprivileged
                );
            let byte_cost = bus.rep_data_byte_cost_upper();
            let mut bus_per_iteration = byte_cost
                .checked_mul(bytes)?
                .checked_mul(CpuGsw::rep_memory_accesses(op))?;
            if port_io {
                bus_per_iteration = bus_per_iteration
                    .checked_add(bus.rep_io_cost_upper(cpu.registers.edx() as u16, width))?;
            }
            if restricted {
                bus_per_iteration =
                    bus_per_iteration.checked_add(byte_cost.checked_mul(2 + bytes)?)?;
            }
            let translations = if width == BusWidth::Byte { 1 } else { 2 };
            let paging_setup = if cpu.is_paging_enabled() {
                let walk = bus.rep_page_walk_cost_upper()?;
                if restricted {
                    // Bitmap reads can evict the operand translation on every element.
                    bus_per_iteration = bus_per_iteration
                        .checked_add(walk.checked_mul(2 + bytes + translations)?)?;
                    0
                } else {
                    walk.checked_mul(CpuGsw::rep_memory_accesses(op))?
                        .checked_mul(translations)?
                }
            } else {
                0
            };
            let raw = work
                .rep
                .as_ref()
                .and_then(|invoice| invoice.plan)
                .map_or_else(
                    || {
                        if port_io {
                            u64::from(crate::string_port_element_core_clocks(matches!(
                                op,
                                StringOp::Outs
                            )))
                        } else {
                            u64::from(cpu.class_table().raw(TimingClass::StringElem))
                        }
                    },
                    |plan| plan.element_raw,
                );
            let (num, den) = crate::level_timing(cpu.persona());
            let core = raw.checked_mul(u64::from(num))?.div_ceil(u64::from(den));
            Some((bus_per_iteration.checked_add(core)?, paging_setup))
        };
        match bounded() {
            Some((per_iteration, paging_setup)) => Self::Bounded {
                per_iteration,
                paging_setup,
            },
            None => Self::Yield,
        }
    }
}

impl CpuGsw {
    const MAX_BUDGETED_REP_ITERATIONS: u32 = 4_096;

    pub(super) fn price_rep_invocation(
        &self,
        work: &mut InstructionWork,
        op: StringOp,
        address_size: AddressSize,
    ) {
        let Some(invoice) = work.rep.as_mut() else {
            return;
        };
        let (plan, startup) = RepChargePlan::new(
            self.persona(),
            op,
            invoice.history,
            self.string_count(address_size),
        );
        assert!(self.timing_rem < 12, "invalid REP timing carry");
        plan.max_raw
            .checked_add(self.timing_rem)
            .expect("REP numerator exceeded u64");
        let new_core = self.preview_scale_clocks(plan.max_raw);
        // A memory fault can earn one task switch before its error-code push fails.
        let table = self.class_table();
        let delivery = u64::from(table.raw(TimingClass::TaskSwitch))
            + u64::from(
                table
                    .raw(TimingClass::ExceptionDelivery)
                    .max(table.raw(TimingClass::ExceptionDeliveryV86)),
            );
        let reserve = delivery.div_ceil(12);
        // Fetch may precede this assertion. This bounds the selected memory work,
        // not arbitrary CPU or Machine lifetime exhaustion.
        Self::check_rep_headroom(
            self.elapsed_clocks,
            self.core_clocks_so_far,
            work.committed.total(),
            new_core,
            reserve,
        );
        invoice.raw_due = startup;
        invoice.plan = Some(plan);
    }

    pub(super) fn check_rep_headroom(
        elapsed: u64,
        prefix: u64,
        owner: u64,
        new_core: u64,
        delivery: u64,
    ) {
        let maximum = new_core
            .checked_add(delivery)
            .expect("REP reserve exceeded u64");
        elapsed
            .checked_add(maximum)
            .expect("REP elapsed headroom exhausted");
        let owned = owner
            .checked_add(maximum)
            .expect("REP owner headroom exhausted");
        prefix
            .checked_add(owned)
            .expect("REP run headroom exhausted");
    }

    fn rep_projected_core(&self, work: &InstructionWork) -> u64 {
        let owned = work.committed.projected_after(self.core_clocks_so_far);
        let pending = work
            .rep
            .as_ref()
            .filter(|invoice| invoice.plan.is_some())
            .map_or(0, |invoice| self.preview_scale_clocks(invoice.raw_due));
        owned
            .checked_add(pending)
            .expect("REP projected core exceeded u64")
    }

    pub(super) fn publish_rep_core<B: CpuBus>(&self, bus: &mut B, work: &InstructionWork) {
        if work.sourced_rep() {
            bus.publish_core_clocks(self.rep_projected_core(work));
        }
    }

    fn index_offset(&self, index: u8, address_size: AddressSize) -> u32 {
        match address_size {
            AddressSize::Word => u32::from(self.read_gpr16(index)),
            AddressSize::Dword => self.read_gpr32(index),
        }
    }

    pub(super) fn string_count(&self, address_size: AddressSize) -> u32 {
        self.index_offset(1, address_size) // CX / ECX
    }

    fn decrement_string_count(&mut self, address_size: AddressSize) {
        self.decrement_string_count_by(address_size, 1);
    }

    fn decrement_string_count_by(&mut self, address_size: AddressSize, amount: u32) {
        match address_size {
            AddressSize::Word => {
                let cx = self.read_gpr16(1).wrapping_sub(amount as u16);
                self.write_gpr16(1, cx);
            }
            AddressSize::Dword => {
                let ecx = self.read_gpr32(1).wrapping_sub(amount);
                self.write_gpr32(1, ecx);
            }
        }
    }

    fn rep_core_clocks(op: StringOp) -> u32 {
        match op {
            StringOp::Ins => 15,
            StringOp::Outs => 14,
            _ => 4,
        }
    }

    fn rep_memory_accesses(op: StringOp) -> u64 {
        match op {
            StringOp::Movs | StringOp::Cmps => 2,
            StringOp::Scas | StringOp::Stos | StringOp::Lods | StringOp::Ins | StringOp::Outs => 1,
        }
    }

    /// `rep_core_clocks(op)` scaled by `level_timing(persona)`'s `(num, den)` and rounded up,
    /// same expression `scale_clocks` (`core.rs:1122`) already measured and fixed: the generic
    /// form with a runtime `den` emitted a hardware `div` on every call, where a `match` over the
    /// persona gives the compiler a compile-time divisor and strength-reduces it to a
    /// magic-multiplier multiply-shift. The arms carry `level_timing`'s literals verbatim,
    /// `(2, 5)` and `(1, 12)`, and
    /// `rep_core_upper_matches_the_pre_slice_divide_for_every_reachable_input`
    /// (`strings_test.rs`) pins that substitution exact for every op, persona and
    /// `rep_resume_active` state `rep_chunk_limit` and `rep_budget_exhausted` can reach.
    /// `rep_resume_active` collapses this to 0 regardless of persona, checked BEFORE the match so
    /// neither arm has to encode it.
    fn rep_core_upper(&self, op: StringOp) -> u64 {
        if self.rep_resume_active {
            return 0;
        }
        let raw = match op {
            StringOp::Ins | StringOp::Outs => {
                u64::from(self.string_port_setup_core_clocks(matches!(op, StringOp::Outs), true))
            }
            _ => u64::from(Self::rep_core_clocks(op)),
        };
        match self.persona() {
            CpuPersona::I386 => raw.saturating_mul(2).saturating_add(4) / 5,
            CpuPersona::I486 | CpuPersona::I586 => raw.saturating_add(11) / 12,
        }
    }

    /// `level_timing`'s literals, pinned beside the match that carries them verbatim (review
    /// N2: these used to live in `strings_test.rs` as `#[cfg(test)]`-only tripwires, which meant
    /// only a test build would break if `level_timing` ever moved without `rep_core_upper`
    /// following; a plain `const _` assert has no runtime cost, so there is no reason not to make
    /// a release build catch it too). If a future change to `level_timing` moves either pair
    /// without the match following, this fails at COMPILE time rather than after the match has
    /// silently stopped agreeing with it.
    const _REP_CORE_UPPER_I386_LITERAL: () =
        assert!(matches!(level_timing(CpuPersona::I386), (2, 5)));
    const _REP_CORE_UPPER_I486_LITERAL: () =
        assert!(matches!(level_timing(CpuPersona::I486), (1, 12)));
    const _REP_CORE_UPPER_I586_LITERAL: () =
        assert!(matches!(level_timing(CpuPersona::I586), (1, 12)));

    fn rep_chunk_limit<B: CpuBus>(
        &self,
        plan: RepLimitPlan,
        bus: &B,
        op: StringOp,
        width: BusWidth,
        work: &InstructionWork,
    ) -> Option<u32> {
        let budget = self.rep_execution.budget?;
        // Recomputes the whole plan from scratch on every call (once per fast chunk, once
        // per slow REP iteration) in debug builds only -- the same idiom run.rs:3040 uses
        // for global_block_upper's memo, so this is house style, not an oversight. Kept
        // rather than demoted to a separate test-only check because the hazard it catches
        // (a bus cost dial or the paging mode moving mid-REP) can only be exercised through
        // a real run_string loop, not a unit test in isolation. Cost: it will make a
        // debug-build REP-heavy test measurably slower than the release path; that is
        // expected, not a regression to bisect.
        debug_assert_eq!(
            plan,
            RepLimitPlan::compute(self, bus, op, width, work),
            "REP limit plan went stale mid-loop: a bus cost dial or the paging mode moved \
             inside one REP, which rep_data_byte_cost_upper's and rep_page_walk_cost_upper's \
             doc comments both say must not happen"
        );
        let (per_iteration, paging_setup) = match plan {
            RepLimitPlan::Unbounded => {
                // budget is Some here (the `?` above would have returned), so a plan computed
                // as Unbounded (which only happens when budget is None) cannot reach this arm
                // unless budget flipped mid-loop, which run_string's single pre-loop compute
                // and CpuGsw's own write sites (run.rs:408, :410 -- set before, cleared after,
                // never inside) both rule out. Fail closed rather than divide by a bound that
                // was never priced.
                return Some(0);
            }
            RepLimitPlan::Yield => return Some(0),
            RepLimitPlan::Bounded {
                per_iteration,
                paging_setup,
            } => (per_iteration, paging_setup),
        };
        let bus_growth = bus
            .in_batch_scaled_bus_clocks()
            .saturating_sub(budget.bus_at_entry);
        let used = self.rep_projected_core(work).saturating_add(bus_growth);
        let core_upper = if work.sourced_rep() {
            0
        } else {
            self.rep_core_upper(op)
        };
        let available = budget
            .cap
            .saturating_sub(used)
            .saturating_sub(core_upper)
            .saturating_sub(paging_setup);
        let limit = available
            .checked_div(per_iteration)
            .unwrap_or(u64::from(Self::MAX_BUDGETED_REP_ITERATIONS))
            .min(u64::from(Self::MAX_BUDGETED_REP_ITERATIONS)) as u32;
        Some(limit)
    }

    fn rep_budget_exhausted<B: CpuBus>(
        &self,
        bus: &B,
        op: StringOp,
        work: &InstructionWork,
    ) -> bool {
        let Some(budget) = self.rep_execution.budget else {
            return false;
        };
        let bus_growth = bus
            .in_batch_scaled_bus_clocks()
            .saturating_sub(budget.bus_at_entry);
        let core_upper = if work.sourced_rep() {
            0
        } else {
            self.rep_core_upper(op)
        };
        self.rep_projected_core(work)
            .saturating_add(bus_growth)
            .saturating_add(core_upper)
            >= budget.cap
    }

    fn read_string_src<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        width: BusWidth,
    ) -> ExecResult<u32> {
        let segment = prefixes.segment_override.unwrap_or(SegmentIndex::Ds);
        let offset = self.index_offset(6, address_size); // SI / ESI
        self.read_memory_bus_width(bus, segment, offset, width, BusAccessKind::DataRead)
    }

    fn read_string_dst<B: CpuBus>(
        &mut self,
        bus: &mut B,
        address_size: AddressSize,
        width: BusWidth,
    ) -> ExecResult<u32> {
        let offset = self.index_offset(7, address_size); // DI / EDI
        self.read_memory_bus_width(
            bus,
            SegmentIndex::Es,
            offset,
            width,
            BusAccessKind::DataRead,
        )
    }

    fn acc_read(&self, width: BusWidth) -> u32 {
        match width {
            BusWidth::Byte => u32::from(self.read_gpr8(0)),
            BusWidth::Word => u32::from(self.read_gpr16(0)),
            BusWidth::Dword => self.read_gpr32(0),
        }
    }

    fn acc_write(&mut self, width: BusWidth, value: u32) {
        match width {
            BusWidth::Byte => self.write_gpr8(0, value as u8),
            BusWidth::Word => self.write_gpr16(0, value as u16),
            BusWidth::Dword => self.write_gpr32(0, value),
        }
    }

    fn write_string_dst<B: CpuBus>(
        &mut self,
        bus: &mut B,
        address_size: AddressSize,
        width: BusWidth,
        value: u32,
    ) -> ExecResult<()> {
        let offset = self.index_offset(7, address_size); // DI / EDI
        self.write_memory_bus_width(
            bus,
            SegmentIndex::Es,
            offset,
            width,
            value,
            BusAccessKind::DataWrite,
        )
    }

    fn string_forward_chunk_iterations(
        &self,
        segment: SegmentIndex,
        offset: u32,
        address_size: AddressSize,
        width: BusWidth,
        count: u32,
    ) -> u32 {
        if count == 0 {
            return 0;
        }
        let bytes = width.bytes() as usize;
        let mut max_bytes = count as usize * bytes;
        let address_remaining = match address_size {
            AddressSize::Word => 0x1_0000usize - (offset as usize & 0xffff),
            AddressSize::Dword => (u32::MAX - offset) as usize + 1,
        };
        max_bytes = max_bytes.min(address_remaining);

        let descriptor = self.registers.segment(segment);
        if descriptor.base != 0 || descriptor.limit != u32::MAX {
            if offset > descriptor.limit {
                return 0;
            }
            max_bytes = max_bytes.min((descriptor.limit - offset) as usize + 1);
        }

        let linear = descriptor.base.wrapping_add(offset);
        max_bytes = max_bytes.min(0x1000 - (linear as usize & 0x0fff));
        (max_bytes / bytes).min(count as usize) as u32
    }

    fn read_direct_string_value<B: CpuBus>(
        &mut self,
        bus: &mut B,
        physical: u32,
        width: BusWidth,
    ) -> Result<Option<u32>, BusError> {
        let access = width.bytes() as usize;
        if bus.direct_memory_bytes(physical, access, width, BusAccessKind::DataRead) != access {
            return Ok(None);
        }
        let mut bytes = [0u8; 4];
        let got = bus.read_memory_bytes_direct(
            physical,
            &mut bytes[..access],
            width,
            BusAccessKind::DataRead,
        )?;
        debug_assert!(got == 0 || got == access);
        if got != access {
            return Ok(None);
        }
        self.record_data_read(BusAccessKind::DataRead, true);
        Ok(Some(match width {
            BusWidth::Byte => u32::from(bytes[0]),
            BusWidth::Word => u32::from(u16::from_le_bytes([bytes[0], bytes[1]])),
            BusWidth::Dword => u32::from_le_bytes(bytes),
        }))
    }

    fn ranges_overlap(a: u32, b: u32, bytes: usize) -> bool {
        let a = a as usize;
        let b = b as usize;
        let a_end = a.saturating_add(bytes);
        let b_end = b.saturating_add(bytes);
        a < b_end && b < a_end
    }

    fn string_step<B: CpuBus>(
        &mut self,
        bus: &mut B,
        committed: &mut CommittedCore,
        op: StringOp,
        width: BusWidth,
        prefixes: Prefixes,
        address_size: AddressSize,
    ) -> ExecResult<()> {
        let bytes = width.bytes();
        match op {
            StringOp::Movs => {
                let value = self.read_string_src(bus, prefixes, address_size, width)?;
                self.write_string_dst(bus, address_size, width, value)?;
                self.adjust_index_register(6, address_size, bytes);
                self.adjust_index_register(7, address_size, bytes);
            }
            StringOp::Cmps => {
                let a = self.read_string_src(bus, prefixes, address_size, width)?;
                let b = self.read_string_dst(bus, address_size, width)?;
                self.alu_sub(a, b, 0, width); // flags only: [DS:SI] - [ES:DI]
                self.adjust_index_register(6, address_size, bytes);
                self.adjust_index_register(7, address_size, bytes);
            }
            StringOp::Scas => {
                let a = self.acc_read(width);
                let b = self.read_string_dst(bus, address_size, width)?;
                self.alu_sub(a, b, 0, width); // flags only: accumulator - [ES:DI]
                self.adjust_index_register(7, address_size, bytes);
            }
            StringOp::Stos => {
                let value = self.acc_read(width);
                self.write_string_dst(bus, address_size, width, value)?;
                self.adjust_index_register(7, address_size, bytes);
            }
            StringOp::Lods => {
                let value = self.read_string_src(bus, prefixes, address_size, width)?;
                self.acc_write(width, value);
                self.adjust_index_register(6, address_size, bytes);
            }
            StringOp::Ins => {
                // INS: [ES:DI] <- port[DX]. ES cannot be overridden.
                let port = self.read_gpr16(2);
                // P3, gap 3. String port I/O never consulted the TSS bitmap
                // (`dev_docs/2026-09-05-v86-port-io-timing-research.md` section 5), so it could
                // not `#GP` under ANY monitor -- wrong under one trapping a port a driver reaches
                // with `REP INSW`. Ordered BEFORE the access and before any index adjustment, so
                // a denied element faults with the string state untouched and `CX` still counting
                // it, which is what a restartable string instruction requires.
                self.check_io_permission(bus, port, width)?;
                let value = bus.read_io_string_element(
                    port,
                    width,
                    committed.projected_after(self.core_clocks_so_far),
                    self.is_ring0_protected(),
                )?;
                self.write_string_dst(bus, address_size, width, value)?;
                self.adjust_index_register(7, address_size, bytes);
                if prefixes.rep.is_some() {
                    self.charge_string_port_element_core(false, committed);
                }
            }
            StringOp::Outs => {
                // OUTS: port[DX] <- [DS:SI] (segment overridable).
                let port = self.read_gpr16(2);
                self.check_io_permission(bus, port, width)?;
                let value = self.read_string_src(bus, prefixes, address_size, width)?;
                bus.write_io_string_element(
                    port,
                    width,
                    value,
                    committed.projected_after(self.core_clocks_so_far),
                    self.is_ring0_protected(),
                )?;
                self.adjust_index_register(6, address_size, bytes);
                if prefixes.rep.is_some() {
                    self.charge_string_port_element_core(true, committed);
                }
            }
        }
        Ok(())
    }

    /// Diagnostic store watchpoint for the string paths. The bulk routes below hand whole
    /// spans straight to the bus, bypassing the per-store checks in `memory.rs`, so a
    /// `REP MOVS`/`REP STOS` into the watched range would otherwise be invisible.
    #[cfg(feature = "watch-write")]
    #[inline(always)]
    fn watch_bulk_write(&self, context: &str, dst: u32, bytes: u32) {
        if crate::write_watch_hits(crate::write_watch_packed(), dst, bytes) {
            crate::report_write_watch(
                context,
                self.registers.cs().selector,
                self.registers.eip,
                dst,
                bytes,
                0,
                self.registers.segment(SegmentIndex::Es).selector,
                self.registers.edi(),
                self.registers.segment(SegmentIndex::Ds).selector,
                self.registers.esi(),
            );
        }
    }

    fn finish_buffered_movs_first<B: CpuBus>(
        &mut self,
        bus: &mut B,
        dst: u32,
        width: BusWidth,
        value: u32,
        address_size: AddressSize,
    ) -> ExecResult<FastStringResult> {
        #[cfg(feature = "watch-write")]
        self.watch_bulk_write("movs1", dst, width.bytes());
        let write = bus.write_memory_direct(dst, width, value, BusAccessKind::DataWrite)?;
        self.record_data_write(BusAccessKind::DataWrite, write.direct);
        self.adjust_index_register(6, address_size, width.bytes());
        self.adjust_index_register(7, address_size, width.bytes());
        self.decrement_string_count(address_size);
        Ok(FastStringResult {
            iterations: 1,
            stop: false,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn try_run_string_fast<B: CpuBus>(
        &mut self,
        bus: &mut B,
        op: StringOp,
        width: BusWidth,
        prefixes: Prefixes,
        address_size: AddressSize,
        rep: RepKind,
        max_iterations: u32,
    ) -> ExecResult<Option<FastStringResult>> {
        if self.flag(FLAG_DF) {
            return Ok(None);
        }

        let count = self.string_count(address_size).min(max_iterations);
        if count == 0 {
            return Ok(Some(FastStringResult {
                iterations: 0,
                stop: false,
            }));
        }

        let access = width.bytes() as usize;
        match op {
            StringOp::Movs => {
                let src_segment = prefixes.segment_override.unwrap_or(SegmentIndex::Ds);
                let src_off = self.index_offset(6, address_size);
                let dst_off = self.index_offset(7, address_size);
                let iterations = self
                    .string_forward_chunk_iterations(
                        src_segment,
                        src_off,
                        address_size,
                        width,
                        count,
                    )
                    .min(self.string_forward_chunk_iterations(
                        SegmentIndex::Es,
                        dst_off,
                        address_size,
                        width,
                        count,
                    ));
                if iterations == 0 {
                    return Ok(None);
                }
                let bytes = iterations as usize * access;
                let (_, src) =
                    self.translate_segmented(bus, src_segment, src_off, width.bytes(), false)?;
                let Some(first) = self.read_direct_string_value(bus, src, width)? else {
                    return Ok(None);
                };
                let (_, dst) =
                    self.translate_segmented(bus, SegmentIndex::Es, dst_off, width.bytes(), true)?;
                let bulk_direct = !Self::ranges_overlap(src, dst, bytes)
                    && bus.direct_memory_bytes(src, bytes, width, BusAccessKind::DataRead) == bytes
                    && bus.direct_memory_bytes(dst, bytes, width, BusAccessKind::DataWrite)
                        == bytes;
                if !bulk_direct {
                    return self
                        .finish_buffered_movs_first(bus, dst, width, first, address_size)
                        .map(Some);
                }

                // Heap scratch on rep_execution, not a `[0u8; 4096]` local: see RepBulkScratch's
                // doc. The mutable borrow below ends at its last use (the read call, or the
                // copy if `bytes == access`), so the early-return bail and the later
                // watch_bulk_write/write calls below -- both needing `self` again -- are fine
                // under NLL without moving any statement.
                let buf = &mut self.rep_execution.bulk.0[..bytes];
                let first_bytes = first.to_le_bytes();
                buf[..access].copy_from_slice(&first_bytes[..access]);
                if bytes > access {
                    let got = bus.read_memory_bytes_direct(
                        src.wrapping_add(access as u32),
                        &mut buf[access..],
                        width,
                        BusAccessKind::DataRead,
                    )?;
                    if got != bytes - access {
                        return self
                            .finish_buffered_movs_first(bus, dst, width, first, address_size)
                            .map(Some);
                    }
                }
                // NOT hoisted above the short-read bail just above: that bail must not be
                // attributed a write-watch hit for a write that never happened.
                #[cfg(feature = "watch-write")]
                self.watch_bulk_write("movs", dst, bytes as u32);
                let put = bus.write_memory_bytes_direct(
                    dst,
                    &self.rep_execution.bulk.0[..bytes],
                    width,
                    BusAccessKind::DataWrite,
                )?;
                if put != bytes {
                    return Ok(None);
                }
                if bytes > access {
                    let remaining_dst = dst.wrapping_add(access as u32);
                    // G2 out of scope: bulk MOVS already streamed the new bytes into the
                    // destination through write_memory_bytes_direct above, so the old bytes are
                    // gone. A pre-read to compare would be an O(n) tax on the fast string path;
                    // this invalidation stays unconditional.
                    self.note_code_write(remaining_dst, (bytes - access) as u32);
                    self.record_write_page(remaining_dst);
                }
                self.adjust_index_register(6, address_size, bytes as u32);
                self.adjust_index_register(7, address_size, bytes as u32);
                self.decrement_string_count_by(address_size, iterations);
                self.perf.data_direct_reads += u64::from(iterations - 1);
                self.perf.data_direct_writes += u64::from(iterations);
                Ok(Some(FastStringResult {
                    iterations,
                    stop: false,
                }))
            }
            StringOp::Stos => {
                let dst_off = self.index_offset(7, address_size);
                let iterations = self.string_forward_chunk_iterations(
                    SegmentIndex::Es,
                    dst_off,
                    address_size,
                    width,
                    count,
                );
                if iterations == 0 {
                    return Ok(None);
                }
                let bytes = iterations as usize * access;
                let (_, dst) =
                    self.translate_segmented(bus, SegmentIndex::Es, dst_off, bytes as u32, true)?;
                if bus.direct_memory_bytes(dst, bytes, width, BusAccessKind::DataWrite) != bytes {
                    return Ok(None);
                }

                let value = self.acc_read(width);
                let mut pattern = [0u8; 4];
                match width {
                    BusWidth::Byte => pattern[0] = value as u8,
                    BusWidth::Word => pattern[..2].copy_from_slice(&(value as u16).to_le_bytes()),
                    BusWidth::Dword => pattern.copy_from_slice(&value.to_le_bytes()),
                }
                // Heap scratch on rep_execution: see RepBulkScratch's doc. No bail sits between
                // the fill and the write here, so the same shape as MOVS's is used only for
                // consistency, not because it is load-bearing in this arm.
                {
                    let buf = &mut self.rep_execution.bulk.0[..bytes];
                    for chunk in buf.chunks_mut(access) {
                        chunk.copy_from_slice(&pattern[..access]);
                    }
                }
                #[cfg(feature = "watch-write")]
                self.watch_bulk_write("stos", dst, bytes as u32);
                let put = bus.write_memory_bytes_direct(
                    dst,
                    &self.rep_execution.bulk.0[..bytes],
                    width,
                    BusAccessKind::DataWrite,
                )?;
                if put != bytes {
                    return Ok(None);
                }
                self.adjust_index_register(7, address_size, bytes as u32);
                self.decrement_string_count_by(address_size, iterations);
                self.perf.data_direct_writes += u64::from(iterations);
                Ok(Some(FastStringResult {
                    iterations,
                    stop: false,
                }))
            }
            StringOp::Lods => {
                let src_segment = prefixes.segment_override.unwrap_or(SegmentIndex::Ds);
                let src_off = self.index_offset(6, address_size);
                let iterations = self.string_forward_chunk_iterations(
                    src_segment,
                    src_off,
                    address_size,
                    width,
                    count,
                );
                if iterations == 0 {
                    return Ok(None);
                }
                let bytes = iterations as usize * access;
                let (_, src) =
                    self.translate_segmented(bus, src_segment, src_off, bytes as u32, false)?;
                if bus.direct_memory_bytes(src, bytes, width, BusAccessKind::DataRead) != bytes {
                    return Ok(None);
                }

                // Heap scratch on rep_execution: see RepBulkScratch's doc.
                let buf = &mut self.rep_execution.bulk.0[..bytes];
                let got = bus.read_memory_bytes_direct(src, buf, width, BusAccessKind::DataRead)?;
                if got != bytes {
                    return Ok(None);
                }
                let last = &self.rep_execution.bulk.0[bytes - access..bytes];
                let value = match width {
                    BusWidth::Byte => u32::from(last[0]),
                    BusWidth::Word => u32::from(u16::from_le_bytes([last[0], last[1]])),
                    BusWidth::Dword => u32::from_le_bytes([last[0], last[1], last[2], last[3]]),
                };
                self.acc_write(width, value);
                self.adjust_index_register(6, address_size, bytes as u32);
                self.decrement_string_count_by(address_size, iterations);
                self.perf.data_direct_reads += u64::from(iterations);
                Ok(Some(FastStringResult {
                    iterations,
                    stop: false,
                }))
            }
            StringOp::Cmps => {
                let src_segment = prefixes.segment_override.unwrap_or(SegmentIndex::Ds);
                let src_off = self.index_offset(6, address_size);
                let dst_off = self.index_offset(7, address_size);
                if self.string_forward_chunk_iterations(
                    src_segment,
                    src_off,
                    address_size,
                    width,
                    1,
                ) == 0
                    || self.string_forward_chunk_iterations(
                        SegmentIndex::Es,
                        dst_off,
                        address_size,
                        width,
                        1,
                    ) == 0
                {
                    return Ok(None);
                }
                let (_, src) =
                    self.translate_segmented(bus, src_segment, src_off, width.bytes(), false)?;
                let Some(a) = self.read_direct_string_value(bus, src, width)? else {
                    return Ok(None);
                };
                let (_, dst) =
                    self.translate_segmented(bus, SegmentIndex::Es, dst_off, width.bytes(), false)?;
                let b = if let Some(value) = self.read_direct_string_value(bus, dst, width)? {
                    value
                } else {
                    let read = bus.read_memory_direct(dst, width, BusAccessKind::DataRead)?;
                    self.record_data_read(BusAccessKind::DataRead, read.direct);
                    // DELIBERATELY not fed to the slow-read page histogram
                    // (`IZARRAVM_SLOW_READ_HISTO`): every other contributor to `data_slow_reads`
                    // buckets a LINEAR page, and `dst` here is already translated. Mixing the two
                    // address spaces in one table would be worse than the omission, which the
                    // report makes visible by printing the histogram total against
                    // `data_slow_reads` -- a REP CMPS-heavy workload shows up as a shortfall
                    // rather than as a silently mislabelled bucket.
                    read.value
                };
                self.alu_sub(a, b, 0, width);
                self.adjust_index_register(6, address_size, width.bytes());
                self.adjust_index_register(7, address_size, width.bytes());
                self.decrement_string_count(address_size);
                let zf = self.flag(FLAG_ZF);
                let stop = match rep {
                    RepKind::Repe => !zf,
                    RepKind::Repne => zf,
                };
                Ok(Some(FastStringResult {
                    iterations: 1,
                    stop,
                }))
            }
            StringOp::Scas => {
                let dst_off = self.index_offset(7, address_size);
                if self.string_forward_chunk_iterations(
                    SegmentIndex::Es,
                    dst_off,
                    address_size,
                    width,
                    1,
                ) == 0
                {
                    return Ok(None);
                }
                let (_, dst) =
                    self.translate_segmented(bus, SegmentIndex::Es, dst_off, width.bytes(), false)?;
                let Some(b) = self.read_direct_string_value(bus, dst, width)? else {
                    return Ok(None);
                };
                self.alu_sub(self.acc_read(width), b, 0, width);
                self.adjust_index_register(7, address_size, width.bytes());
                self.decrement_string_count(address_size);
                let zf = self.flag(FLAG_ZF);
                let stop = match rep {
                    RepKind::Repe => !zf,
                    RepKind::Repne => zf,
                };
                Ok(Some(FastStringResult {
                    iterations: 1,
                    stop,
                }))
            }
            StringOp::Ins | StringOp::Outs => Ok(None),
        }
    }

    pub(super) fn run_string<B: CpuBus>(
        &mut self,
        bus: &mut B,
        work: &mut InstructionWork,
        op: StringOp,
        width: BusWidth,
        prefixes: Prefixes,
        address_size: AddressSize,
    ) -> ExecResult<()> {
        match prefixes.rep {
            None => {
                self.string_step(bus, &mut work.committed, op, width, prefixes, address_size)?
            }
            Some(kind) => {
                let mut chunk_iterations = 0u32;
                // Priced once for the whole REP: see RepLimitPlan's doc for why per_iteration
                // and the paging setup cost cannot change across this loop's iterations.
                let plan = RepLimitPlan::compute(self, bus, op, width, work);
                let mut allowance = self.rep_chunk_limit(plan, bus, op, width, work);
                loop {
                    if self.string_count(address_size) == 0 {
                        break;
                    }
                    let remaining = match allowance {
                        None => u32::MAX,
                        Some(available) => {
                            let natural = available.min(
                                Self::MAX_BUDGETED_REP_ITERATIONS.saturating_sub(chunk_iterations),
                            );
                            if natural == 0 && self.rep_resume_active && chunk_iterations == 0 {
                                1
                            } else {
                                natural
                            }
                        }
                    };
                    if remaining == 0 {
                        self.rep_execution.yielded = true;
                        break;
                    }
                    self.publish_rep_core(bus, work);
                    if let Some(fast) = self.try_run_string_fast(
                        bus,
                        op,
                        width,
                        prefixes,
                        address_size,
                        kind,
                        remaining,
                    )? {
                        if let Some(invoice) = work.rep.as_mut() {
                            invoice.complete(fast.iterations);
                        }
                        self.perf.rep_string_iterations += u64::from(fast.iterations);
                        self.perf.rep_string_fast_iterations += u64::from(fast.iterations);
                        chunk_iterations = chunk_iterations.saturating_add(fast.iterations);
                        if let Some(available) = allowance.as_mut() {
                            *available = available.saturating_sub(fast.iterations);
                        }
                        if fast.stop {
                            break;
                        }
                        if let Some(refreshed) = self.rep_chunk_limit(plan, bus, op, width, work) {
                            allowance = Some(
                                allowance.map_or(refreshed, |available| available.min(refreshed)),
                            );
                        }
                        if self.string_count(address_size) != 0
                            && self.rep_execution.budget.is_some()
                            && (allowance == Some(0)
                                || chunk_iterations >= Self::MAX_BUDGETED_REP_ITERATIONS
                                || bus.requires_step_break()
                                || self.rep_budget_exhausted(bus, op, work))
                        {
                            self.rep_execution.yielded = true;
                            break;
                        }
                        continue;
                    }
                    self.publish_rep_core(bus, work);
                    self.string_step(bus, &mut work.committed, op, width, prefixes, address_size)?;
                    if let Some(invoice) = work.rep.as_mut() {
                        invoice.complete(1);
                    }
                    self.perf.rep_string_iterations += 1;
                    chunk_iterations += 1;
                    if let Some(available) = allowance.as_mut() {
                        *available = available.saturating_sub(1);
                    }
                    self.decrement_string_count(address_size);
                    // CMPS/SCAS also end the repeat on the ZF condition. REPE continues while
                    // ZF is set; REPNE continues while ZF is clear. MOVS/STOS/LODS ignore ZF.
                    if matches!(op, StringOp::Cmps | StringOp::Scas) {
                        let zf = self.flag(FLAG_ZF);
                        let again = match kind {
                            RepKind::Repe => zf,
                            RepKind::Repne => !zf,
                        };
                        if !again {
                            break;
                        }
                    }
                    if let Some(refreshed) = self.rep_chunk_limit(plan, bus, op, width, work) {
                        allowance =
                            Some(allowance.map_or(refreshed, |available| available.min(refreshed)));
                    }
                    if self.string_count(address_size) != 0
                        && self.rep_execution.budget.is_some()
                        && (allowance == Some(0)
                            || chunk_iterations >= Self::MAX_BUDGETED_REP_ITERATIONS
                            || bus.requires_step_break()
                            || self.rep_budget_exhausted(bus, op, work))
                    {
                        self.rep_execution.yielded = true;
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    fn adjust_index_register(&mut self, index: u8, address_size: AddressSize, amount: u32) {
        let delta = if self.flag(FLAG_DF) {
            0u32.wrapping_sub(amount)
        } else {
            amount
        };

        match address_size {
            AddressSize::Word => {
                let value = self.read_gpr16(index).wrapping_add(delta as u16);
                self.write_gpr16(index, value);
            }
            AddressSize::Dword => {
                let value = self.read_gpr32(index).wrapping_add(delta);
                self.write_gpr32(index, value);
            }
        }
    }
}

#[cfg(test)]
#[path = "strings_test.rs"]
mod tests;
