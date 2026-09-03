// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Render the video mode census as JSON for `--mode-census`.
//!
//! Both sections are always present, even when empty. A missing section reads
//! as "not measured"; an empty list reads as "measured, and the guest never
//! went there". The compatibility board grades on that difference, so the
//! shape must not collapse.

use izarravm_video::{DistiraCensus, DistiraScanoutState, ModeCensus, SwapToNextDrainStats};
use serde_json::{Value, json};

// One parameter per independent census source, mirroring the JSON's own flat
// section list -- a struct wrapper would just move the eight fields one level
// out without changing what a caller has to gather. `distira_swap_to_next_drain`
// is the eighth (slice 0 of the async-overlap review).
#[allow(clippy::too_many_arguments)]
pub fn mode_census_json(
    vga: &ModeCensus,
    distira: &DistiraCensus,
    distira_state: Option<DistiraScanoutState>,
    register_writes: &[(usize, u64)],
    register_reads: &[(usize, u64)],
    aperture: izarravm_video::DistiraApertureTraffic,
    offset_bits_or: usize,
    swap_to_next_drain: SwapToNextDrainStats,
) -> Value {
    let vga: Vec<Value> = vga
        .entries()
        .map(|(key, count)| {
            json!({
                "mode": format!("{:?}", key.mode),
                "hdisp_end": key.hdisp_end,
                // vdisp_end and double_scan travel together on purpose: 200
                // lines double scanned and 199 lines single scanned present the
                // same raster height, and a reader needs both to tell them
                // apart.
                "vdisp_end": key.vdisp_end,
                // The SOURCE row count, which is what a game's own resolution
                // means. vdisp_end counts RASTER lines: standard mode 13h
                // reports 400 with double_scan set, not 200. A reader asking
                // "is this a 199-line mode?" wants this field, and getting it
                // wrong is easy enough that it is computed here once.
                "source_lines": key.vdisp_end / if key.double_scan { 2 } else { 1 },
                "vtotal": key.vtotal,
                "double_scan": key.double_scan,
                "line_compare_active": key.line_compare_active,
                "bpp": key.bpp,
                // How many times the guest programmed this geometry. A guest
                // that replays its CRTC table per frame reads in the thousands.
                "entries": count,
            })
        })
        .collect();
    let distira: Vec<Value> = distira
        .entries()
        .map(|(key, count)| {
            json!({
                "width": key.width,
                "height": key.height,
                "entries": count,
            })
        })
        .collect();
    // The scanout snapshot answers the one question a black Distira frame
    // raises: did the guest render and we fail to show it, or did it never
    // render at all? `painted_bytes` is the discriminator, and it counts the
    // WHOLE frame store rather than the scanned-out window on purpose: pixels
    // written at a base or pitch the scanout does not read are exactly the case
    // it exists to catch.
    let state = distira_state.map(|state| {
        json!({
            "width": state.width,
            "height": state.height,
            "pitch": state.pitch,
            "front_base": state.front_base,
            "back_base": state.back_base,
            "scanout_base": state.scanout_base,
            "buffer_stride": state.buffer_stride,
            "display_enabled": state.display_enabled,
            "pending_swaps": state.pending_swaps,
            "swaps_issued": state.swaps_issued,
            "triangles_drawn": state.triangles_drawn,
            // LFB path only. See DistiraScanoutState.
            "color_pixels_stored": state.color_pixels_stored,
            "fastfill_pixels": state.fastfill_pixels,
            "triangle_census": {
                "submitted": state.triangles.submitted,
                "reject_zero_area": state.triangles.reject_zero_area,
                "zero_area_degenerate": state.triangles.zero_area_degenerate,
                "zero_area_collinear": state.triangles.zero_area_collinear,
                "fixed_vertex_writes": state.triangles.fixed_vertex_writes,
                "float_vertex_writes": state.triangles.float_vertex_writes,
                "zero_area_float_samples": state
                    .triangles
                    .zero_area_float_samples
                    .map(|tri| tri.map(|(x, y)| [x, y])),
                "zero_area_samples": state
                    .triangles
                    .zero_area_samples
                    .map(|tri| tri.map(|(x, y)| [x, y])),
                "drawn_samples": state
                    .triangles
                    .drawn_samples
                    .map(|tri| tri.map(|(x, y)| [x, y])),
                "reject_empty_box": state.triangles.reject_empty_box,
                "pixels_in": state.triangles.pixels_in,
                "reject_stipple": state.triangles.reject_stipple,
                "reject_depth": state.triangles.reject_depth,
                "reject_alpha_mask": state.triangles.reject_alpha_mask,
                "reject_chroma": state.triangles.reject_chroma,
                "reject_alpha_test": state.triangles.reject_alpha_test,
                "pixels_out": state.triangles.pixels_out,
                "color_written": state.triangles.color_written,
                "color_written_nonblack": state.triangles.color_written_nonblack,
                "color_offset_min": state.triangles.color_offset_min,
                "color_offset_max": state.triangles.color_offset_max,
                "reject_rgb_wmask": state.triangles.reject_rgb_wmask,
                "reject_offscreen": state.triangles.reject_offscreen,
                "depth_written": state.triangles.depth_written,
                // The async-queue win metric: how many times
                // drain_raster_queue actually rasterised a batch, and how
                // many of those batches were large enough to reach the
                // parallel lanes. See DistiraTriangleCensus::queue_drains.
                "queue_drains": state.triangles.queue_drains,
                "queue_drains_parallel": state.triangles.queue_drains_parallel,
                // Slice 0 of the texture-queue lever: which caller forced
                // each drain. See DistiraDrainCauses.
                "queue_drain_causes": {
                    "texture_write": state.triangles.queue_drain_causes.texture_write,
                    "lfb_read": state.triangles.queue_drain_causes.lfb_read,
                    "lfb_write": state.triangles.queue_drain_causes.lfb_write,
                    "register_read_stats":
                        state.triangles.queue_drain_causes.register_read_stats,
                    "register_write_uncovered":
                        state.triangles.queue_drain_causes.register_write_uncovered,
                    "nop_cmd_reset_stats":
                        state.triangles.queue_drain_causes.nop_cmd_reset_stats,
                    "swap_or_scanout": state.triangles.queue_drain_causes.swap_or_scanout,
                    "queue_full": state.triangles.queue_drain_causes.queue_full,
                    "immediate_triangle":
                        state.triangles.queue_drain_causes.immediate_triangle,
                    "config": state.triangles.queue_drain_causes.config,
                },
            },
            "retrace_count": state.retrace_count,
            "painted_bytes": state.painted_bytes,
            "painted_by_buffer": state.painted_by_buffer,
            "fbz_mode": state.fbz_mode,
            "lfb_mode": state.lfb_mode,
            "aux_base": state.aux_base,
            "frame_store_bytes": state.frame_store_bytes,
        })
    });
    // The lane-cap ladder's own finding: nothing in the census announced
    // the realized pool size, so "did the knob actually grow the pool?"
    // could only be inferred from wall-clock deltas
    // (`dev_docs/2026-09-05-lane-cap-ladder.md`, "Pool sizing" section).
    // `lane_count` is this Distira instance's configured cap
    // (`IZARRAVM_DISTIRA_LANES`, or the >=8-core default of 8); `pool_size`
    // is the dedicated raster pool's actual OS thread count, which the
    // pool never disagrees with the default lane count on (both read the
    // same cached `IZARRAVM_DISTIRA_LANES`/host-core value), but a lower
    // `set_raster_lanes` override CAN sit below the pool's size. Host-side
    // only: identical across arms for otherwise-identical guest state, so
    // it never affects the frame or instruction identity the rest of this
    // census guards.
    let distira_raster_lanes = distira_state.map(|state| {
        json!({
            "lane_count": state.raster_lane_count,
            "pool_size": state.raster_pool_size,
        })
    });
    json!({
        "schema": "izarravm-mode-census-v1",
        "vga": vga,
        "distira": distira,
        "distira_scanout": state,
        // Which SST-1 registers the guest actually drove, busiest first, and
        // the OR of every MMIO offset it used. A guest that renders nothing
        // is answered here first: it names the protocol in use.
        "distira_register_writes": register_writes
            .iter()
            .map(|&(register, count)| json!({ "register": register, "writes": count }))
            .collect::<Vec<_>>(),
        "distira_register_reads": register_reads
            .iter()
            .map(|&(register, count)| json!({ "register": register, "reads": count }))
            .collect::<Vec<_>>(),
        "distira_aperture_traffic": {
            "texture_writes": aperture.texture_writes,
            "texture_writes_refused": aperture.texture_writes_refused,
            "texture_refused_tmu_select": aperture.texture_refused_tmu_select,
            "texture_refused_lod": aperture.texture_refused_lod,
            "texture_refused_bits_or": aperture.texture_refused_bits_or,
            "lfb_writes": aperture.lfb_writes,
            "lfb_reads": aperture.lfb_reads,
            "command_fifo_writes": aperture.command_fifo_writes,
            "texture_reads": aperture.texture_reads,
            "texture_narrow_writes": aperture.texture_narrow_writes,
        },
        "distira_offset_bits_or": offset_bits_or,
        "distira_raster_lanes": distira_raster_lanes,
        // Slice 0 of the async-overlap review
        // (`dev_docs/2026-09-05-distira-async-overlap-review.md` section 2):
        // the swap -> next-drain-call wall-clock window, whole run. Answers
        // whether the fastfill that follows a swap lands microseconds later
        // (a real overlap window an async lever could fill) or effectively
        // at once (the window is already spent before any lever can act).
        "distira_swap_to_next_drain_ns": {
            "count": swap_to_next_drain.count,
            "min": swap_to_next_drain.min_ns,
            "median": swap_to_next_drain.median_ns,
            "max": swap_to_next_drain.max_ns,
        },
    })
}

#[cfg(test)]
#[path = "mode_census_json_test.rs"]
mod tests;
