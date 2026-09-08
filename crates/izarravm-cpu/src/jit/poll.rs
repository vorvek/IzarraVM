// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::block::{PollScanOutcome, build_poll_loop_from};
use super::direct::{direct_poll_skip_max_raw, direct_poll_skip_min_iterations};
use crate::{CpuGsw, PollFamily};
use izarravm_bus::{CalloutPollDecline, CalloutPollSkipOutcome, CalloutPollSkipRequest, CpuBus};
use izarravm_core::CpuPersona;

pub(crate) struct PollCalloutContext {
    pub linear: u32,
    pub interrupt_shadow: bool,
    pub sixteen_bit_armed: bool,
    pub core_at_entry: u64,
    pub prefix_raw: u64,
    pub cap: u64,
    pub bus_at_entry: u64,
}

// Success commits virtual bus work; the caller must settle its CPU clocks and run the real IN.
pub(crate) fn try_callout_poll_skip<B: CpuBus>(
    cpu: &mut CpuGsw,
    bus: &mut B,
    context: PollCalloutContext,
) -> Option<CalloutPollSkipOutcome> {
    if cpu.is_v86_mode() || cpu.current_privilege_level() > cpu.iopl() {
        return None;
    }
    let port = cpu.read_gpr16(2);
    cpu.jit_direct.note_poll_attempt();
    if port != 0x03da {
        cpu.jit_direct.note_poll_declined_port();
    } else if !cpu.jit_direct.direct_poll_skip_armed_for() {
        cpu.jit_direct.note_poll_declined_knob();
    } else if !(matches!(cpu.persona(), CpuPersona::I486 | CpuPersona::I586)
        && !context.interrupt_shadow
        && !cpu.profile.enabled
        && !crate::run::diff_trace_enabled())
    {
        cpu.jit_direct.note_poll_declined_eligibility();
    } else if !cpu.registers.cs().default_size_32
        && !(context.sixteen_bit_armed && cpu.registers.cs().limit <= 0xffff)
    {
        cpu.jit_direct.note_poll_declined_sixteen_bit();
    } else {
        let d = cpu.registers.cs().default_size_32;
        let sixteen_bit_ok = !d;
        let slot_linear = context.linear;
        let memo_key =
            sixteen_bit_ok.then(|| (cpu.read_gpr8(4), cpu.decode_cache.poll_neg_gen(slot_linear)));
        if let Some((ah, page_gen)) = memo_key
            && cpu
                .jit_direct
                .poll_mask_decline_memo_hit(slot_linear, ah, page_gen)
        {
            cpu.jit_direct.note_poll_declined_mask_source();
        } else if cpu.poll_neg_cache_enabled && cpu.decode_cache.poll_negative_live(slot_linear, d)
        {
            cpu.perf.poll_neg_cache_hits += 1;
            cpu.jit_direct.note_poll_declined_shape();
        } else {
            match build_poll_loop_from(cpu, slot_linear, sixteen_bit_ok) {
                PollScanOutcome::NegativeCacheable => {
                    if cpu.poll_neg_cache_enabled {
                        cpu.perf.poll_neg_cache_stores += 1;
                        cpu.decode_cache.record_poll_negative(slot_linear, d);
                    }
                    cpu.jit_direct.note_poll_declined_shape();
                }
                PollScanOutcome::NegativeVolatile => {
                    cpu.perf.poll_neg_cache_volatile += 1;
                    cpu.jit_direct.note_poll_declined_shape();
                }
                PollScanOutcome::Found(poll) => {
                    debug_assert_eq!(
                        poll.family(),
                        PollFamily::Io,
                        "a port call-out's backward scan certified a non-Io shape"
                    );
                    debug_assert!(
                        (0..poll.fetch_count()).any(|index| poll
                            .fetch(index)
                            .is_some_and(|(linear, _, len)| linear == slot_linear && len == 1)),
                        "the certified shape does not contain this call-out's own IN slot"
                    );
                    let poll = poll.with_resolved_mask(cpu);
                    if poll.family() != PollFamily::Io || poll.resolved_port(cpu) != 0x03da {
                        cpu.jit_direct.note_poll_declined_port_source();
                    } else if !matches!(poll.status_mask(), 0x01 | 0x08) {
                        cpu.jit_direct.note_poll_declined_mask_source();
                        if let Some((ah, page_gen)) = memo_key {
                            cpu.jit_direct
                                .record_poll_mask_decline(slot_linear, ah, page_gen);
                        }
                    } else {
                        bus.publish_core_clocks(
                            context
                                .core_at_entry
                                .saturating_add(cpu.preview_scale_clocks(context.prefix_raw)),
                        );
                        let mut fetches = [(0u32, 0u32, 0u8); 6];
                        for (slot, index) in fetches.iter_mut().zip(0..poll.fetch_count()) {
                            if let Some(fetch) = poll.fetch(index) {
                                *slot = fetch;
                            }
                        }
                        let (core_num, core_den) = crate::level_timing(cpu.persona());
                        let request = CalloutPollSkipRequest {
                            fetches,
                            fetch_count: poll.fetch_count() as u8,
                            status_mask: poll.status_mask(),
                            spins_when_bit_set: poll.fresh_iteration_spins(poll.status_mask()),
                            raw_core_clocks: cpu.poll_skip_raw_core_clocks(poll),
                            core_clocks_at_block_entry: context.core_at_entry,
                            prefix_raw: context.prefix_raw,
                            core_num,
                            core_den,
                            timing_rem: cpu.poll_skip_timing_remainder(),
                            cap: context.cap,
                            bus_scaled_at_run_entry: context.bus_at_entry,
                            min_iterations: direct_poll_skip_min_iterations(),
                            max_skipped_raw: direct_poll_skip_max_raw(),
                        };
                        match bus.callout_poll_skip(&request) {
                            Ok(outcome) => {
                                bus.publish_core_clocks(outcome.now_after);
                                let head = poll.fetch(0).map_or(slot_linear, |(l, _, _)| l);
                                cpu.jit_direct.note_poll_skip_span(
                                    poll.diagnostic_class(),
                                    outcome.iterations,
                                    outcome.skipped_raw_core_clocks,
                                    outcome.committed_raw_bus_clocks,
                                    head,
                                );
                                return Some(outcome);
                            }
                            Err(CalloutPollDecline::Cap) => {
                                cpu.jit_direct.note_poll_declined_cap();
                            }
                            Err(_) => {
                                cpu.jit_direct.note_poll_declined_seam();
                            }
                        }
                    }
                }
            }
        }
    }
    None
}
