// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_bus::{CompiledBusDelta, CompiledBusWindow};

pub(super) struct Running {
    window: CompiledBusWindow,
    can_take: bool,
}

fn validate_fetches<B: CpuBus>(
    cpu: &CpuGsw,
    bus: &B,
    frame: &Frame,
    operations: &[Operation],
) -> Option<u64> {
    let mut raw = 0;
    for op in operations {
        let linear = frame.cs.base.wrapping_add(op.eip);
        let view = cpu.decode_cache.get_packed(linear, false)?;
        if view.len != op.insn.len
            || view.phys_start != op.physical
            || !CpuGsw::fetch_within_limit(op.eip, op.insn.len, frame.cs.limit)
        {
            return None;
        }
        raw += bus.jit_preflight_cached_fetch(linear, op.physical, op.insn.len)?;
    }
    Some(raw)
}

pub(super) unsafe extern "C" fn prepare<B: CpuBus>(
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
        || cpu.class_table() as *const _ as usize != frame.table
        || !cpu.mkii_context()
    {
        return 2;
    }
    if cpu.interrupt_shadow
        || !cpu.mkii_span_observers_quiet()
        || !cpu.fast_map_serve_enabled.enabled
        || cpu.rmw_census_enabled
        || cpu.slot_census_enabled
        || (cpu.alignment_armed && cpu.current_privilege_level() == 3)
        || bus.requires_step_break()
        || !bus.native_fetches_are_uniform()
        || !bus.native_aggregate_accounting_allowed()
    {
        return 0;
    }
    #[cfg(feature = "reflected-call-memo")]
    if cpu.reflected_call_journal {
        return 0;
    }
    cpu.settle_write_record();
    // SAFETY: this region is a contiguous part of the leased operation allocation.
    let operations = unsafe { std::slice::from_raw_parts(operation, first.region_len) };
    let region = first.region.as_ref().unwrap();
    let mut segments = region.segments;
    while segments != 0 {
        let index = segments.trailing_zeros() as usize;
        segments &= segments - 1;
        let segment = [
            SegmentIndex::Es,
            SegmentIndex::Cs,
            SegmentIndex::Ss,
            SegmentIndex::Ds,
            SegmentIndex::Fs,
            SegmentIndex::Gs,
        ][index];
        let write = region.store_segments & (1 << index) != 0;
        if cpu
            .check_segment_access_kind(segment, cpu.registers.segment(segment).access, write)
            .is_err()
        {
            return 0;
        }
    }
    let maps = region_maps(cpu);
    if region.segments != 0 && maps.is_none() {
        return 0;
    }
    let cost = &region.prefixes[first.region_len];
    let persona = cpu.persona();
    let (num, den) = level_timing(persona);
    let Some(scaled_core) = cost
        .raw_core
        .checked_mul(u64::from(num))
        .and_then(|scaled| scaled.checked_add(cpu.timing_rem))
    else {
        return 0;
    };
    let full_core = match persona {
        CpuPersona::I386 => scaled_core / 5,
        CpuPersona::I486 | CpuPersona::I586 => scaled_core / 12,
    };
    let writes = cost.writes != 0;
    let inert = if writes {
        None
    } else {
        bus.certify_inert_read_region()
            .filter(|_| cpu.elapsed_clocks.checked_add(full_core).is_some())
    };
    let window = if inert.is_none() {
        let Some(window) = (if writes {
            bus.begin_ram_write_region()
        } else {
            bus.begin_read_region()
        }) else {
            return 0;
        };
        Some(window)
    } else {
        None
    };
    let (mapping_epoch, fetch_cost) = inert.map_or_else(
        || {
            let window = window.as_ref().unwrap();
            (window.mapping_epoch(), window.fetch_raw_clocks())
        },
        |grant| (grant.epochs().0, 0),
    );
    let fetch_raw = if frame.source_certificate().is_some_and(|certificate| {
        bus.owned_code_replay_epochs() == Some(certificate)
            && certificate == (mapping_epoch, bus.jit_cost_dial_epoch())
    }) && fetch_cost == 0
    {
        Some(0)
    } else {
        validate_fetches(cpu, bus, frame, operations)
    };
    let projected_bus = inert.map_or_else(
        || {
            bus.jit_projected_batch_scaled_bus_clocks(
                window.as_ref().unwrap().delta_raw_clocks(&cost.delta),
            )
        },
        |grant| Some(grant.scaled_bus_clocks()),
    );
    let projected = projected_bus
        .and_then(|bus| bus.checked_sub(frame.bus_at_entry))
        .and_then(|bus| bus.checked_add(frame.total))
        .and_then(|total| total.checked_add(full_core));
    if fetch_raw != Some(fetch_cost * operations.len() as u64)
        || !projected.is_some_and(|total| total < frame.cap)
    {
        if let Some(window) = window {
            bus.finish_compiled_window(window, CompiledBusDelta::default());
        }
        return 0;
    }
    debug_assert!(frame.region_running.is_none());
    debug_assert_eq!(frame.region_inert, 0);
    frame.region_epoch = mapping_epoch;
    frame.region_user = u32::from(cpu.current_privilege_level() == 3);
    (
        frame.region_load_biases,
        frame.region_store_biases,
        frame.region_mapping_epochs,
        frame.region_physical_pages,
    ) = maps.unwrap_or_default();
    frame.region_completed = 0;
    frame.region_guard_miss = 0;
    frame.branch_taken = 0;
    frame.region_write_page = 0;
    frame.region_write_count = 0;
    cpu.core_clocks_so_far = frame.total;
    frame.region_inert = u32::from(inert.is_some() && cost.writes == 0);
    frame.region_full_core = full_core;
    frame.region_full_rem = scaled_core - full_core * u64::from(den);
    frame.region_monitor = u32::from(cpu.is_ring0_protected());
    frame.region_can_take = cpu.can_take_interrupt();
    frame.region_running = window.map(|window| Running {
        window,
        can_take: frame.region_can_take,
    });
    frame.stats.regions += 1;
    1
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn region_maps(cpu: &CpuGsw) -> Option<(usize, usize, usize, usize)> {
    cpu.jit_fast_map.native_bases().map(|maps| {
        (
            maps.load_biases(),
            maps.store_biases(),
            maps.mapping_epochs(),
            maps.physical_pages(),
        )
    })
}

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
pub(super) fn region_maps(_: &CpuGsw) -> Option<(usize, usize, usize, usize)> {
    None
}

pub(super) unsafe extern "C" fn finish<B: CpuBus>(
    cpu: *mut CpuGsw,
    bus: *mut (),
    frame: *mut Frame,
    operation: *const Operation,
) -> u32 {
    let (cpu, bus, frame, first) =
        unsafe { (&mut *cpu, &mut *bus.cast::<B>(), &mut *frame, &*operation) };
    let completed = frame.region_completed as usize;
    assert!(completed <= first.region_len);
    // SAFETY: generated exits report a completed prefix of the leased region.
    let operations = unsafe { std::slice::from_raw_parts(operation, completed) };
    let cost = &first.region.as_ref().unwrap().prefixes[completed];
    let branch_taken = frame.branch_taken != 0;
    let can_take = if let Some(running) = frame.region_running.take() {
        debug_assert_eq!(frame.region_inert, 0);
        bus.finish_compiled_window(running.window, cost.delta);
        running.can_take
    } else {
        assert_ne!(std::mem::take(&mut frame.region_inert), 0);
        frame.region_can_take
    };
    if frame.region_guard_miss != 0 {
        frame.force_canonical = true;
        frame.stats.region_guard_misses += 1;
        frame.stats.carry_misses += u64::from(frame.region_guard_miss == 2);
    }
    if completed != 0 {
        frame.stats.carry_native += cost.carry_ops;
        if frame.branch_taken == 0 {
            let last = operations.last().unwrap();
            cpu.registers.eip = last.eip.wrapping_add(u32::from(last.insn.len));
        }
        cpu.perf.data_direct_reads += cost.reads;
        cpu.perf.direct_data_pointer_reads += cost.reads;
        cpu.fast_map_probe.hits += cost.reads;
        if frame.region_write_count != 0 && completed == first.region_len {
            cpu.record_write_page(frame.region_write_page);
        }
        frame.pending = Some(Pending {
            raw: cost.raw_core,
            retired: completed as u64,
            start_eip: first.eip,
            start_cs: frame.cs.selector,
            can_take,
        });
        frame.retire_pending(cpu, bus);
    }
    u32::from(!frame.stop && !frame.force_canonical && !branch_taken)
}
