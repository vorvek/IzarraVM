// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::ops::Operation;
use crate::run::{budgeted_run_outcome, checked_run_core_total};
use crate::timing_class::TimingClass;
use crate::*;
use std::collections::HashMap;

#[path = "region.rs"]
mod region;

#[cfg(all(
    test,
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "runtime_test.rs"]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    table: usize,
    physical: u32,
    eip: u32,
    cs_base: u32,
    cs_limit: u32,
    cs_selector: u16,
    cpl: u8,
}

struct Trace {
    operations: Box<[Operation]>,
    code: super::native::Code,
    open_tail: Option<u32>,
    source_certificate: Option<(u64, u64)>,
}

impl std::fmt::Debug for Trace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Trace")
            .field("operations", &self.operations.len())
            .finish()
    }
}

#[derive(Debug, Default)]
pub(super) struct Engine {
    traces: HashMap<Key, usize>,
    arena: Vec<Option<Trace>>,
    free: Vec<usize>,
    dispatch: Vec<Option<(Key, usize)>>,
    pub stats: Stats,
    #[cfg(test)]
    fail_next_compile: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub runs: u64,
    pub entries: u64,
    pub compiled: u64,
    pub native: u64,
    pub helpers: u64,
    pub cold: u64,
    pub mismatches: u64,
    pub invalidations: u64,
    pub spans: u64,
    pub memory_spans: u64,
    pub compile_ns: u64,
    pub retired_artifacts: u64,
    pub expansions: u64,
    pub regions: u64,
    pub native_admissions: u64,
    pub region_guard_misses: u64,
    pub dispatch_hits: u64,
    pub dispatch_misses: u64,
    pub carry_native: u64,
    pub carry_misses: u64,
}

type Helper = unsafe extern "C" fn(*mut CpuGsw, *mut (), *mut Frame, *const Operation) -> u32;
type Resolver = unsafe extern "C" fn(*mut CpuGsw, *mut (), *mut Frame) -> usize;

#[derive(Default)]
pub(super) struct Session {
    pub enabled: u32,
    pub bus: izarravm_bus::MkiiBusSessionParts,
    pub raw_limit: u64,
    pub threshold_limit: u64,
    pub load_biases: usize,
    pub mapping_epochs: usize,
}

impl Session {
    fn acquire<B: CpuBus>(cpu: &CpuGsw, bus: &B, owner: *const B) -> Self {
        if cfg!(feature = "int-trace")
            || cfg!(feature = "timing-class-histogram")
            || !cpu.mkii_span_observers_quiet()
        {
            return Self::default();
        }
        let Some(parts) = bus
            .mkii_bus_session()
            .and_then(|grant| grant.into_parts(owner))
        else {
            return Self::default();
        };
        let (load_biases, mapping_epochs) = region::region_maps(cpu).unwrap_or_default();
        Self {
            enabled: 1,
            raw_limit: u64::MAX / parts.bus_numerator,
            threshold_limit: u64::MAX / parts.bus_denominator,
            bus: parts,
            load_biases,
            mapping_epochs,
        }
    }
}

pub(super) struct Frame {
    pub helpers: [Helper; 15],
    pub resolve: Resolver,
    pub dispatch: usize,
    engine: *mut Engine,
    pub memory_ptr: *mut u8,
    pub branch_taken: u32,
    pub region_completed: u32,
    pub region_guard_miss: u32,
    pub region_epoch: u64,
    pub region_user: u32,
    pub region_load_biases: usize,
    pub region_mapping_epochs: usize,
    pub region_inert: u32,
    pub region_full_core: u64,
    pub region_full_rem: u64,
    pub region_monitor: u32,
    pub(super) region_can_take: bool,
    region_running: Option<region::Running>,
    force_canonical: bool,
    pub(super) total: u64,
    pub(super) cap: u64,
    pub(super) bus_at_entry: u64,
    raw_at_entry: u64,
    screen_scale: u64,
    pub(super) cs: SegmentRegister,
    pub(super) table: usize,
    pub(super) source_present: u32,
    pub(super) source_mapping: u64,
    pub(super) source_cost: u64,
    pub(super) session: Session,
    pending: Option<Pending>,
    fetched: Option<Fetched>,
    poll_16_armed: bool,
    pub(super) stop: bool,
    halted: bool,
    error: Option<CpuRunError>,
    pub(super) stats: Stats,
}

struct Pending {
    raw: u64,
    retired: u64,
    start_eip: u32,
    start_cs: u16,
    can_take: bool,
}

struct Fetched {
    insn: DecodedInsn,
    eip: u32,
    cs: SegmentRegister,
    can_take: bool,
}

impl Frame {
    pub(super) fn new<B: CpuBus>(cpu: &CpuGsw, bus: &mut B, cap: u64) -> Self {
        Self {
            helpers: [
                step::<B, 0>,
                step::<B, 1>,
                step::<B, 2>,
                step::<B, 3>,
                step::<B, 4>,
                step::<B, 5>,
                step::<B, 6>,
                step::<B, 7>,
                step::<B, 8>,
                step::<B, 9>,
                step::<B, 10>,
                finish::<B>,
                prepare_span::<B>,
                region::prepare::<B>,
                region::finish::<B>,
            ],
            resolve: resolve::<B>,
            dispatch: 0,
            engine: std::ptr::null_mut(),
            memory_ptr: std::ptr::null_mut(),
            branch_taken: 0,
            region_completed: 0,
            region_guard_miss: 0,
            region_epoch: 0,
            region_user: 0,
            region_load_biases: 0,
            region_mapping_epochs: 0,
            region_inert: 0,
            region_full_core: 0,
            region_full_rem: 0,
            region_monitor: 0,
            region_can_take: false,
            region_running: None,
            force_canonical: false,
            total: 0,
            cap,
            bus_at_entry: bus.in_batch_scaled_bus_clocks(),
            raw_at_entry: bus.in_batch_raw_bus_clocks(),
            screen_scale: bus.in_batch_scaled_bus_clocks_screen_scale(),
            cs: cpu.registers.cs(),
            table: cpu.class_table() as *const _ as usize,
            source_present: 0,
            source_mapping: 0,
            source_cost: 0,
            session: Session::default(),
            pending: None,
            fetched: None,
            poll_16_armed: cpu.jit_direct.direct_poll_skip_16_armed_for(),
            stop: false,
            halted: false,
            error: None,
            stats: Stats::default(),
        }
    }

    fn source_certificate(&self) -> Option<(u64, u64)> {
        (self.source_present != 0).then_some((self.source_mapping, self.source_cost))
    }

    fn observe<B: CpuBus>(
        &mut self,
        cpu: &mut CpuGsw,
        bus: &mut B,
        can_take: bool,
        outcome: CpuExecutionResult<CpuCycleOutcome>,
    ) {
        match outcome {
            Err(mut error) => {
                error.consumed_core_clocks =
                    checked_run_core_total(self.total, error.consumed_core_clocks);
                self.error = Some(error);
                self.stop = true;
                return;
            }
            Ok(outcome) => {
                self.total = checked_run_core_total(self.total, outcome.core_clocks);
                self.halted = outcome.halted;
            }
        }
        if cpu.rep_resume_active {
            cpu.perf.brk_rep_resume += 1;
        } else if self.halted {
            cpu.perf.brk_halt += 1;
        } else if bus.requires_step_break() {
            cpu.perf.brk_step += 1;
        } else if !can_take && cpu.can_take_interrupt() {
            cpu.perf.brk_interrupt += 1;
        } else {
            let screen = self.screen_scale != 0
                && bus
                    .in_batch_raw_bus_clocks()
                    .wrapping_sub(self.raw_at_entry)
                    .checked_mul(self.screen_scale)
                    .and_then(|raw| raw.checked_add(self.total))
                    .is_some_and(|used| used < self.cap);
            let hit = self.total >= self.cap
                || (!screen
                    && self
                        .bus_at_entry
                        .checked_add(self.cap - self.total)
                        .is_some_and(|target| bus.in_batch_scaled_bus_clocks_at_least(target)));
            if !hit {
                return;
            }
            cpu.perf.brk_cap += 1;
        }
        self.stop = true;
    }

    fn cold<B: CpuBus>(&mut self, cpu: &mut CpuGsw, bus: &mut B) {
        let can_take = cpu.can_take_interrupt();
        let result = cpu.cycle_no_interrupt_check_at_prefix(
            bus,
            Some(RepBudget {
                bus_at_entry: self.bus_at_entry,
                cap: self.cap,
            }),
            self.total,
        );
        self.stats.cold += 1;
        self.observe(cpu, bus, can_take, result);
    }

    fn retire_pending<B: CpuBus>(&mut self, cpu: &mut CpuGsw, bus: &mut B) {
        if let Some(pending) = self.pending.take() {
            if std::mem::take(&mut self.branch_taken) != 0 {
                cpu.set_eip(cpu.registers.eip);
            }
            let result = if pending.retired == 1 {
                cpu.finish_instruction(
                    bus,
                    InstructionExecution::new(Ok(CycleOutcome {
                        core_clocks: pending.raw as u32,
                        halted: false,
                    })),
                    pending.start_eip,
                    pending.start_cs,
                    0,
                    None,
                    None,
                )
            } else {
                let charged = cpu.scale_clocks_batch(pending.raw);
                cpu.elapsed_clocks += charged;
                cpu.perf.instructions += pending.retired;
                if cpu.is_ring0_protected() {
                    cpu.perf.monitor_resident_core_clocks += charged;
                }
                if cpu.registers.eip == 0x10000 {
                    cpu.wrap_16bit_sequential_run_off();
                }
                Ok(CpuCycleOutcome {
                    core_clocks: charged,
                    halted: false,
                })
            };
            self.stats.native += pending.retired;
            self.observe(cpu, bus, pending.can_take, result);
        }
    }
}

// SAFETY: the generated caller owns the exclusive CPU/bus/frame borrows for this call.
unsafe extern "C" fn step<B: CpuBus, const GROUP: usize>(
    cpu: *mut CpuGsw,
    bus: *mut (),
    frame: *mut Frame,
    operation: *const Operation,
) -> u32 {
    let (cpu, bus, frame, operation) =
        unsafe { (&mut *cpu, &mut *bus.cast::<B>(), &mut *frame, &*operation) };
    frame.retire_pending(cpu, bus);
    if frame.stop
        || cpu.jit_direct.mkii.code_dirty
        || cpu.jit_direct.mkii.mapping_dirty
        || cpu.registers.eip != operation.eip
        || cpu.registers.cs() != frame.cs
        || !cpu.mkii_context()
    {
        return 0;
    }
    let can_take = cpu.can_take_interrupt();
    let interrupt_shadow = cpu.interrupt_shadow;
    cpu.interrupt_shadow = false;
    cpu.core_clocks_so_far = frame.total;
    cpu.begin_instruction();
    let lin = cpu.linear_eip();
    let warm = cpu
        .decode_cache
        .get_packed(lin, false)
        .is_some_and(|screen| {
            screen.phys_start == operation.physical && screen.len == operation.insn.len
        })
        && CpuGsw::fetch_within_limit(operation.eip, operation.insn.len, frame.cs.limit);
    let decoded;
    let fetched = if warm {
        cpu.charge_cached_fetch_at(bus, lin, operation.insn.len, operation.physical)
            .map(|()| &operation.insn)
    } else {
        match cpu.fetch_decoded(bus, lin) {
            Ok(insn) => {
                decoded = insn;
                Ok(&decoded)
            }
            Err(fault) => Err(fault),
        }
    };
    let insn = match fetched {
        Ok(insn) => insn,
        Err(fault) => {
            let result = cpu.finish_instruction(
                bus,
                InstructionExecution::new(Err(fault)),
                operation.eip,
                frame.cs.selector,
                0,
                None,
                None,
            );
            frame.observe(cpu, bus, can_take, result);
            return 0;
        }
    };
    if let Some(census) = &mut cpu.jit_direct.mkii.census {
        let opcode = usize::from(insn.opcode & 255) + usize::from(insn.opcode > 255) * 256;
        let width = usize::from(insn.operand_size == OperandSize::Dword) * 512;
        let memory = usize::from(matches!(insn.operand, Some(DecodedOperand::Mem(_)))) * 1024;
        census[opcode + width + memory] += 1;
    }
    if !warm
        && (*insn != operation.insn
            || cpu.decode_cache.line_phys_start(lin, false) != Some(operation.physical))
    {
        frame.stats.mismatches += 1;
        frame.fetched = Some(Fetched {
            insn: *insn,
            eip: operation.eip,
            cs: frame.cs,
            can_take,
        });
        return 0;
    }
    if GROUP == 0 {
        frame.pending = Some(Pending {
            raw: u64::from(
                cpu.class_table()
                    .raw(operation.pure.expect("native operation").1),
            ),
            retired: 1,
            start_eip: operation.eip,
            start_cs: frame.cs.selector,
            can_take,
        });
        return 1;
    }
    let mut skipped_poll = false;
    if GROUP == 6 && insn.opcode == 0xec && insn.len == 1 && cpu.mkii_span_observers_quiet() {
        let context = crate::jit::poll::PollCalloutContext {
            linear: lin,
            interrupt_shadow,
            sixteen_bit_armed: frame.poll_16_armed,
            core_at_entry: frame.total,
            prefix_raw: 0,
            cap: frame.cap,
            bus_at_entry: frame.bus_at_entry,
        };
        if let Some(outcome) = crate::jit::poll::try_callout_poll_skip(cpu, bus, context) {
            let charged = cpu.scale_clocks_batch(outcome.skipped_raw_core_clocks);
            cpu.elapsed_clocks += charged;
            frame.total = checked_run_core_total(frame.total, charged);
            if cpu.is_ring0_protected() {
                cpu.perf.monitor_resident_core_clocks += charged;
            }
            debug_assert_eq!(frame.total, outcome.now_after);
            cpu.core_clocks_so_far = outcome.now_after;
            skipped_poll = true;
        }
    }
    let branch_taken =
        GROUP == 5 && matches!(insn.opcode, 0x70..=0x7f) && cpu.condition((insn.opcode & 15) as u8);
    let mut work = InstructionWork::default();
    let result = match GROUP {
        1 => cpu.execute_alu_decoded(insn, bus),
        2 => cpu.execute_datamove_decoded(insn, bus),
        3 => cpu.execute_stack_decoded(insn, bus),
        4 => cpu.execute_group_decoded(insn, bus),
        5 => cpu.execute_branch_decoded(insn, bus),
        6 if insn.opcode == 0xec => cpu.execute_port_io_decoded(insn, bus),
        6 => cpu.execute_flags_misc_decoded(insn, bus),
        7 => cpu.execute_system_seg_decoded(insn, bus, &mut work.committed),
        8 => cpu.execute_control_flow_decoded(insn, bus, &mut work.committed),
        9 => cpu.execute_bitmanip_decoded(insn, bus),
        10 => cpu.execute_condmove_decoded(insn, bus),
        _ => unreachable!(),
    };
    let succeeded = result.is_ok();
    #[cfg(feature = "reflected-call-memo")]
    let journal = cpu.reflected_call_journal;
    #[cfg(not(feature = "reflected-call-memo"))]
    let journal = false;
    let outcome = match result {
        Ok(outcome)
            if GROUP != 8
                && work.rep.is_none()
                && work.committed.total() == 0
                && !journal
                && cpu.mkii_span_observers_quiet() =>
        {
            Ok(CpuCycleOutcome {
                core_clocks: cpu.retire_instruction_core(outcome.core_clocks),
                halted: outcome.halted,
            })
        }
        result => cpu.finish_instruction(
            bus,
            InstructionExecution { result, work },
            operation.eip,
            frame.cs.selector,
            0,
            None,
            None,
        ),
    };
    frame.stats.helpers += 1;
    frame.observe(cpu, bus, can_take, outcome);
    if skipped_poll && !frame.stop {
        cpu.perf.brk_step += 1;
        frame.stop = true;
    }
    u32::from(succeeded && !frame.stop && !branch_taken)
}

unsafe extern "C" fn finish<B: CpuBus>(
    cpu: *mut CpuGsw,
    bus: *mut (),
    frame: *mut Frame,
    _: *const Operation,
) -> u32 {
    let (cpu, bus, frame) = unsafe { (&mut *cpu, &mut *bus.cast::<B>(), &mut *frame) };
    frame.retire_pending(cpu, bus);
    0
}

unsafe extern "C" fn prepare_span<B: CpuBus>(
    cpu: *mut CpuGsw,
    bus: *mut (),
    frame: *mut Frame,
    operation: *const Operation,
) -> u32 {
    let (cpu, bus, frame, first) =
        unsafe { (&mut *cpu, &mut *bus.cast::<B>(), &mut *frame, &*operation) };
    frame.retire_pending(cpu, bus);
    if frame.stop
        || cpu.jit_direct.mkii.code_dirty
        || cpu.jit_direct.mkii.mapping_dirty
        || cpu.registers.eip != first.eip
        || cpu.registers.cs() != frame.cs
        || !cpu.mkii_context()
    {
        return 2;
    }
    if cpu.interrupt_shadow
        || !cpu.mkii_span_observers_quiet()
        || bus.requires_step_break()
        || !bus.native_fetches_are_uniform()
        || !bus.native_aggregate_accounting_allowed()
    {
        return 0;
    }
    cpu.settle_write_record();
    let memory = if first.memory_cmp_branch {
        let Some(read) = cpu.mkii_memory_read(bus, &first.insn) else {
            return 0;
        };
        Some(read)
    } else {
        None
    };
    // SAFETY: span_len describes a contiguous region of the leased operation allocation.
    let operations = unsafe { std::slice::from_raw_parts(operation, first.span_len) };
    let mut raw_core = 0u64;
    let mut raw_bus = memory.map_or(0, |(_, _, _, clocks)| clocks);
    for (index, op) in operations.iter().enumerate() {
        let linear = frame.cs.base.wrapping_add(op.eip);
        let Some(view) = cpu.decode_cache.get_packed(linear, false) else {
            return 0;
        };
        if view.len != op.insn.len
            || view.phys_start != op.physical
            || !CpuGsw::fetch_within_limit(op.eip, op.insn.len, frame.cs.limit)
        {
            return 0;
        }
        let Some(fetch) = bus.jit_preflight_cached_fetch(linear, op.physical, op.insn.len) else {
            return 0;
        };
        let Some(next_bus) = raw_bus.checked_add(fetch) else {
            return 0;
        };
        raw_bus = next_bus;
        let class = if memory.is_some() {
            if index == 0 {
                TimingClass::AluRegMem
            } else {
                TimingClass::Jcc
            }
        } else {
            op.pure.unwrap().1
        };
        raw_core += u64::from(cpu.class_table().raw(class));
    }
    let Some(projected_bus) = bus.jit_projected_batch_scaled_bus_clocks(raw_bus) else {
        return 0;
    };
    let projected = projected_bus
        .checked_sub(frame.bus_at_entry)
        .and_then(|bus| bus.checked_add(frame.total))
        .and_then(|total| total.checked_add(cpu.preview_scale_clocks(raw_core)));
    if !projected.is_some_and(|total| total < frame.cap) {
        return 0;
    }
    let can_take = cpu.can_take_interrupt();
    cpu.core_clocks_so_far = frame.total;
    for (index, op) in operations.iter().enumerate() {
        cpu.charge_cached_fetch_at(
            bus,
            frame.cs.base.wrapping_add(op.eip),
            op.insn.len,
            op.physical,
        )
        .expect("certified inert RAM fetch failed");
        if index == 0
            && let Some((physical, ptr, width, _)) = memory
        {
            bus.charge_direct_ram_memory(physical, width, BusAccessKind::DataRead)
                .expect("certified inert RAM read charge failed");
            cpu.record_data_read(BusAccessKind::DataRead, true);
            cpu.perf.direct_data_pointer_reads += 1;
            cpu.fast_map_probe.hits += 1;
            frame.memory_ptr = ptr;
        }
    }
    frame.pending = Some(Pending {
        raw: raw_core,
        retired: operations.len() as u64,
        start_eip: first.eip,
        start_cs: frame.cs.selector,
        can_take,
    });
    frame.stats.spans += 1;
    frame.stats.memory_spans += u64::from(memory.is_some());
    1
}

impl Engine {
    fn lookup(&mut self, key: Key) -> Option<usize> {
        if self.dispatch.is_empty() {
            self.dispatch.resize(4096, None);
        }
        let slot = (key.physical.wrapping_mul(0x9e37_79b9) >> 20) as usize;
        if let Some((cached, index)) = self.dispatch[slot]
            && cached == key
        {
            self.stats.dispatch_hits += 1;
            return Some(index);
        }
        self.stats.dispatch_misses += 1;
        let index = *self.traces.get(&key)?;
        self.dispatch[slot] = Some((key, index));
        Some(index)
    }

    fn clear(&mut self, cpu: &mut CpuGsw) {
        self.dispatch.fill(None);
        for trace in self.arena.iter().flatten() {
            for operation in &trace.operations {
                cpu.jit_direct
                    .release_mkii_source(operation.physical, u32::from(operation.insn.len));
            }
        }
        self.stats.retired_artifacts += self.traces.len() as u64;
        self.traces.clear();
        self.arena.clear();
        self.free.clear();
        cpu.jit_direct.mkii.sources.clear();
        cpu.jit_direct.mkii.code_dirty = false;
        cpu.jit_direct.mkii.full_flush = false;
        cpu.jit_direct.mkii.dirty_writes.clear();
        self.stats.invalidations += 1;
    }

    fn drain_writes(&mut self, cpu: &mut CpuGsw) {
        if cpu.jit_direct.mkii.full_flush {
            self.clear(cpu);
            return;
        }
        let before = self.traces.len();
        self.dispatch.fill(None);
        self.traces.retain(|_, index| {
            let trace = self.arena[*index].as_ref().unwrap();
            let hit = trace.operations.iter().any(|op| {
                cpu.jit_direct
                    .mkii
                    .dirty_writes
                    .iter()
                    .any(|&(start, len)| {
                        super::physical_ranges(start, len)
                            .into_iter()
                            .flatten()
                            .any(|(start, end)| {
                                u64::from(op.physical) < end
                                    && u64::from(start)
                                        < u64::from(op.physical) + u64::from(op.insn.len)
                            })
                    })
            });
            if hit {
                for op in &trace.operations {
                    cpu.jit_direct
                        .release_mkii_source(op.physical, u32::from(op.insn.len));
                }
                self.arena[*index] = None;
                self.free.push(*index);
            }
            !hit
        });
        cpu.jit_direct.mkii.sources.clear();
        for trace in self.arena.iter().flatten() {
            for op in &trace.operations {
                cpu.jit_direct
                    .mkii
                    .add_source(op.physical, u32::from(op.insn.len));
            }
        }
        cpu.jit_direct.mkii.dirty_writes.clear();
        cpu.jit_direct.mkii.code_dirty = false;
        self.stats.invalidations += 1;
        self.stats.retired_artifacts += (before - self.traces.len()) as u64;
    }

    fn trace<B: CpuBus>(&mut self, cpu: &mut CpuGsw, bus: &mut B) -> Option<&Trace> {
        let cs = cpu.registers.cs();
        let eip = cpu.registers.eip;
        let lin = cpu.linear_eip();
        let physical = cpu.decode_cache.line_phys_start(lin, false)?;
        bus.jit_preflight_cached_fetch(lin, physical, 1)?;
        let key = Key {
            table: cpu.class_table() as *const _ as usize,
            physical,
            eip,
            cs_base: cs.base,
            cs_limit: cs.limit,
            cs_selector: cs.selector,
            cpl: cpu.cpl,
        };
        let mut existing = self.lookup(key);
        let extension = if let Some(index) = existing {
            let trace = self.arena[index].as_ref().unwrap();
            let Some(next) = trace.open_tail else {
                return self.arena[index].as_ref();
            };
            let Some(operation) = Self::decoded_operation(cpu, bus, cs, next, physical) else {
                return self.arena[index].as_ref();
            };
            Some(operation)
        } else {
            None
        };
        {
            let compile_start = std::time::Instant::now();
            if extension.is_none() && self.traces.len() >= 16384 {
                self.clear(cpu);
                existing = None;
            }
            let mut operations = existing.map_or_else(Vec::new, |index| {
                let trace = self.arena[index].as_ref().unwrap();
                trace
                    .operations
                    .iter()
                    .map(|op| {
                        Operation::lower(op.eip, op.physical, op.insn)
                            .expect("owned supported operation")
                    })
                    .collect()
            });
            if let Some(extension) = extension {
                operations.push(extension);
            }
            let mut next = operations
                .last()
                .map_or(eip, |op| op.eip + u32::from(op.insn.len));
            let mut open_tail = None;
            while operations.len() < 64
                && next < 0x10000
                && next <= cs.limit
                && cs.base.wrapping_add(next) >> 12 == lin >> 12
                && operations.last().is_none_or(|op| {
                    matches!(op.insn.opcode, 0x70..=0x7f)
                        || !matches!(
                            op.insn.group,
                            DecodeGroup::Branch | DecodeGroup::ControlFlow
                        )
                })
            {
                let Some(operation) = Self::decoded_operation(cpu, bus, cs, next, physical) else {
                    open_tail = Some(next);
                    break;
                };
                next += u32::from(operation.insn.len);
                operations.push(operation);
            }
            if operations.is_empty() {
                return None;
            }
            let operations = operations.into_boxed_slice();
            let mut operations = operations;
            let mut start = 0;
            while start < operations.len() {
                if start + 1 < operations.len()
                    && matches!(operations[start].insn.opcode, 0x3a | 0x3b)
                    && matches!(operations[start].insn.operand, Some(DecodedOperand::Mem(_)))
                    && matches!(operations[start + 1].insn.opcode, 0x70..=0x7f)
                {
                    operations[start].span_len = 2;
                    operations[start].memory_cmp_branch = true;
                    start += 2;
                    continue;
                }
                let mut end = start;
                while end < operations.len() && operations[end].pure.is_some() {
                    end += 1;
                }
                if end > start {
                    operations[start].span_len = end - start;
                }
                start = end.max(start + 1);
            }
            let mut start = 0;
            while start < operations.len() {
                let mut end = start;
                while end < operations.len()
                    && (operations[end].region_pure().is_some()
                        || operations[end].read.is_some()
                        || (end > start
                            && operations[end - 1].branch_alu().is_some()
                            && matches!(operations[end].insn.opcode, 0x70..=0x7f)))
                {
                    end += 1;
                }
                if end - start >= 2 {
                    operations[start].region_len = end - start;
                    operations[start].region = Some(super::ops::Region::build(
                        cpu.class_table(),
                        &operations[start..end],
                    ));
                }
                start = end.max(start + 1);
            }
            #[cfg(test)]
            if std::mem::take(&mut self.fail_next_compile) {
                return existing.and_then(|index| self.arena[index].as_ref());
            }
            let Some(code) = super::native::compile(&operations, cpu.persona()) else {
                return existing.and_then(|index| self.arena[index].as_ref());
            };
            let last = operations.last().unwrap();
            let source_len = last.eip + u32::from(last.insn.len) - eip;
            let source_certificate = operations
                .iter()
                .all(|op| op.physical == physical.wrapping_add(op.eip - eip))
                .then(|| bus.certify_owned_code_span(lin, physical, source_len))
                .flatten();
            for operation in &operations {
                cpu.mkii_watch_source(operation.physical, u32::from(operation.insn.len));
            }
            self.dispatch.fill(None);
            let index = existing.unwrap_or_else(|| {
                let index = self.free.pop().unwrap_or_else(|| {
                    self.arena.push(None);
                    self.arena.len() - 1
                });
                self.traces.insert(key, index);
                index
            });
            if let Some(previous) = self.arena[index].replace(Trace {
                operations,
                code,
                open_tail,
                source_certificate,
            }) {
                for operation in &previous.operations {
                    cpu.jit_direct
                        .release_mkii_source(operation.physical, u32::from(operation.insn.len));
                }
                self.stats.expansions += 1;
                self.stats.retired_artifacts += 1;
            }
            self.stats.compiled += 1;
            self.stats.compile_ns +=
                compile_start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
            self.arena[index].as_ref()
        }
    }

    fn decoded_operation<B: CpuBus>(
        cpu: &CpuGsw,
        bus: &B,
        cs: SegmentRegister,
        eip: u32,
        physical: u32,
    ) -> Option<Operation> {
        let linear = cs.base.wrapping_add(eip);
        let view = cpu.decode_cache.get_view(linear, false)?;
        if !CpuGsw::fetch_within_limit(eip, view.insn.len, cs.limit)
            || (0xa0..=0xbf).contains(&(view.phys_start >> 12))
            || (linear & 0xfff) + u32::from(view.insn.len) > 4096
            || view.phys_start >> 12 != physical >> 12
        {
            return None;
        }
        bus.jit_preflight_cached_fetch(linear, view.phys_start, view.insn.len)?;
        Operation::lower(eip, view.phys_start, view.insn)
    }
}

fn select_next<B: CpuBus>(
    engine: &mut Engine,
    cpu: &mut CpuGsw,
    bus: &mut B,
    frame: &mut Frame,
) -> usize {
    loop {
        if cpu.jit_direct.mkii.code_dirty {
            engine.drain_writes(cpu);
        }
        cpu.jit_direct.mkii.mapping_dirty = false;
        if !std::mem::take(&mut frame.force_canonical)
            && let Some(trace) = engine.trace(cpu, bus)
        {
            frame.cs = cpu.registers.cs();
            frame.table = cpu.class_table() as *const _ as usize;
            (
                frame.source_present,
                frame.source_mapping,
                frame.source_cost,
            ) = trace
                .source_certificate
                .map_or((0, 0, 0), |(mapping, cost)| (1, mapping, cost));
            frame.stats.entries += 1;
            return trace.code.body_ptr() as usize;
        }
        frame.cold(cpu, bus);
        if frame.stop || !cpu.mkii_context() {
            return 0;
        }
    }
}

unsafe extern "C" fn resolve<B: CpuBus>(
    cpu: *mut CpuGsw,
    bus: *mut (),
    frame: *mut Frame,
) -> usize {
    // SAFETY: only the stable dispatcher calls this, after the trace has tail-jumped out.
    // All pointers belong to this run; no engine borrow crosses a native transfer.
    let (cpu, bus, frame) = unsafe { (&mut *cpu, &mut *bus.cast::<B>(), &mut *frame) };
    debug_assert!(frame.pending.is_none());
    debug_assert!(frame.region_running.is_none());
    debug_assert_eq!(frame.region_inert, 0);
    if let Some(fetched) = frame.fetched.take() {
        let execution = cpu.execute_decoded_with_rep_budget(
            &fetched.insn,
            bus,
            Some(RepBudget {
                bus_at_entry: frame.bus_at_entry,
                cap: frame.cap,
            }),
            None,
        );
        let outcome = if cpu.rep_execution.yielded {
            Ok(cpu.pause_rep_instruction(bus, fetched.insn, fetched.eip, fetched.cs, execution))
        } else {
            cpu.finish_instruction(
                bus,
                execution,
                fetched.eip,
                fetched.cs.selector,
                0,
                None,
                None,
            )
        };
        frame.observe(cpu, bus, fetched.can_take, outcome);
        cpu.jit_direct.mkii.invalidate_code();
    }
    if frame.stop || !cpu.mkii_context() {
        return 0;
    }
    // SAFETY: the local engine is stationary until the dispatcher returns to run_mkii.
    select_next(unsafe { &mut *frame.engine }, cpu, bus, frame)
}

impl CpuGsw {
    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    fn mkii_memory_read<B: CpuBus>(
        &self,
        bus: &B,
        insn: &DecodedInsn,
    ) -> Option<(u32, *mut u8, BusWidth, u64)> {
        #[cfg(feature = "reflected-call-memo")]
        if self.reflected_call_journal {
            return None;
        }
        if !self.fast_map_serve_enabled.enabled
            || self.rmw_census_enabled
            || self.slot_census_enabled
        {
            return None;
        }
        let RmOperand::Memory(memory) = self.resolve_decoded_modrm_operand(insn).1 else {
            return None;
        };
        let width = if insn.opcode == 0x3a {
            BusWidth::Byte
        } else {
            insn.operand_size.bus_width()
        };
        self.check_alignment(memory.offset, width.bytes()).ok()?;
        let linear = self
            .segment_linear_range(memory.segment, memory.offset, width.bytes(), false)
            .ok()?;
        if width.misaligned_at(linear) {
            return None;
        }
        let epoch = self.data_read_pages.mapping_epoch();
        let access = self.jit_fast_map.lookup_access(
            linear,
            epoch,
            width,
            false,
            self.current_privilege_level() == 3,
            self.control.cr0 & CR0_WP != 0,
        )?;
        if access.is_mode13() {
            return None;
        }
        let raw = bus.jit_preflight_ram_read(access.physical(), width, epoch)?;
        Some((access.physical(), access.ptr(), width, raw))
    }

    #[cfg(not(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )))]
    fn mkii_memory_read<B: CpuBus>(
        &self,
        _: &B,
        _: &DecodedInsn,
    ) -> Option<(u32, *mut u8, BusWidth, u64)> {
        None
    }

    fn mkii_span_observers_quiet(&self) -> bool {
        #[cfg(feature = "int-trace")]
        if crate::int_trace::armed() {
            return false;
        }
        #[cfg(feature = "reflected-call-diagnostic")]
        if self.retire_gates.reflected_call_diag_armed {
            return false;
        }
        self.jit_direct.mkii.census.is_none()
            && !self.profile.enabled
            && !self.retire_gates.diff_trace
            && !self.retire_gates.barrier_census
            && self.unit_sim.0.is_none()
            && !cfg!(feature = "timing-class-histogram")
    }
    pub(super) fn mkii_context(&self) -> bool {
        self.is_protected_mode()
            && !self.is_v86_mode()
            && !self.registers.cs().default_size_32
            && !self.rep_resume_active
            && self.registers.eflags & FLAG_TF == 0
    }

    pub(crate) fn mkii_enabled(&self) -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        let enabled = self.jit_direct.mkii.enabled.unwrap_or_else(|| {
            *ENABLED
                .get_or_init(|| std::env::var_os("IZARRAVM_DYNAREC_MKII").is_some_and(|v| v == "1"))
        });
        enabled
            && cfg!(all(
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))
            && self.mkii_context()
            && self.mode().uses_approximate_timing()
            && self.jit_direct.execution_enabled()
            && !self.profile.enabled
            && !self.retire_gates.diff_trace
            && !self.retire_gates.barrier_census
            && self.unit_sim.0.is_none()
            && !cfg!(feature = "timing-class-histogram")
    }

    pub(crate) fn run_mkii<B: CpuBus>(
        &mut self,
        bus: &mut B,
        cap: u64,
    ) -> CpuExecutionResult<BudgetedRunOutcome> {
        static DISPATCHER: OnceLock<Option<super::native::Code>> = OnceLock::new();
        self.run_mkii_with_dispatcher(
            bus,
            cap,
            DISPATCHER.get_or_init(super::native::dispatcher).as_ref(),
        )
    }

    fn run_mkii_with_dispatcher<B: CpuBus>(
        &mut self,
        bus: &mut B,
        cap: u64,
        dispatcher: Option<&super::native::Code>,
    ) -> CpuExecutionResult<BudgetedRunOutcome> {
        static CENSUS: OnceLock<bool> = OnceLock::new();
        if *CENSUS.get_or_init(|| std::env::var_os("IZARRAVM_MKII_CENSUS").is_some())
            && self.jit_direct.mkii.census.is_none()
        {
            self.jit_direct.mkii.census = Some(Box::new([0; 2048]));
        }
        let mut engine = std::mem::take(&mut self.jit_direct.mkii.engine);
        let bus_root = std::ptr::from_mut(bus);
        // SAFETY: all subsequent execution borrows come from this invocation's owner.
        let mut frame = Frame::new(self, unsafe { &mut *bus_root }, cap);
        frame.session = Session::acquire(self, unsafe { &*bus_root }, bus_root);
        self.perf.straight_line_runs += 1;
        if let Some(dispatcher) = dispatcher {
            let first = select_next(&mut engine, self, unsafe { &mut *bus_root }, &mut frame);
            if first != 0 {
                frame.engine = std::ptr::from_mut(&mut engine);
                frame.dispatch = dispatcher.body_ptr() as usize;
                // SAFETY: the dispatcher owns the common frame. Only its resolver mutates
                // the stationary engine, after every return address has left trace code.
                let entry: unsafe extern "C" fn(*mut CpuGsw, *mut (), *mut Frame, usize) =
                    unsafe { std::mem::transmute(dispatcher.entry_ptr()) };
                unsafe { entry(self, bus_root.cast(), &mut frame, first) };
            }
        } else {
            let bus = unsafe { &mut *bus_root };
            loop {
                if self.jit_direct.mkii.code_dirty {
                    engine.drain_writes(self);
                }
                self.jit_direct.mkii.mapping_dirty = false;
                frame.cold(self, bus);
                if frame.stop || !self.mkii_context() {
                    break;
                }
            }
        }
        engine.stats.runs += 1;
        engine.stats.entries += frame.stats.entries;
        engine.stats.native += frame.stats.native;
        engine.stats.helpers += frame.stats.helpers;
        engine.stats.cold += frame.stats.cold;
        engine.stats.mismatches += frame.stats.mismatches;
        engine.stats.spans += frame.stats.spans;
        engine.stats.memory_spans += frame.stats.memory_spans;
        engine.stats.regions += frame.stats.regions;
        engine.stats.native_admissions += frame.stats.native_admissions;
        engine.stats.region_guard_misses += frame.stats.region_guard_misses;
        engine.stats.carry_native += frame.stats.carry_native;
        engine.stats.carry_misses += frame.stats.carry_misses;
        self.jit_direct.mkii.engine = engine;
        match frame.error {
            Some(error) => Err(error),
            None => Ok(budgeted_run_outcome(frame.total, frame.halted)),
        }
    }

    pub fn dynarec_mkii_stats(&self) -> Stats {
        self.jit_direct.mkii.engine.stats
    }

    pub fn dynarec_mkii_census(&self) -> Option<&[u64]> {
        self.jit_direct
            .mkii
            .census
            .as_deref()
            .map(|counts| counts.as_slice())
    }

    pub fn set_dynarec_mkii_enabled(&mut self, on: bool) {
        self.jit_direct.mkii.enabled = Some(on);
    }

    #[cfg(test)]
    pub(crate) fn fail_mkii_compile_for_test(&mut self) {
        self.jit_direct.mkii.engine.fail_next_compile = true;
    }

    #[cfg(test)]
    pub(crate) fn run_mkii_without_dispatcher_for_test<B: CpuBus>(
        &mut self,
        bus: &mut B,
        cap: u64,
    ) -> CpuExecutionResult<BudgetedRunOutcome> {
        self.run_mkii_with_dispatcher(bus, cap, None)
    }
}
