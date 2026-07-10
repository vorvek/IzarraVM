// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn voodoo_registers_store_init_and_render_state() {
    let mut distira = Distira::new();
    distira.set_init_enable(INIT_ENABLE_WRITE);

    write_reg(&mut distira, SST_FBI_INIT0, 0x0000_0003);
    write_reg(&mut distira, SST_FBI_INIT1, 0x0000_0100);
    write_reg(&mut distira, SST_FBI_INIT2, 0x0000_0200);
    write_reg(&mut distira, SST_FBI_INIT3, 0x0000_0001);
    write_reg(&mut distira, SST_LFB_MODE, 0x0000_0005);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK);
    write_reg(&mut distira, SST_ALPHA_MODE, 0x0001_0001);
    write_reg(&mut distira, SST_CLIP_LEFT_RIGHT, (2 << 16) | 7);
    write_reg(&mut distira, SST_CLIP_LOW_Y_HIGH_Y, (3 << 16) | 9);

    assert_eq!(read_reg(&distira, SST_STATUS) & 0x380, 0);
    assert_eq!(read_reg(&distira, SST_FBI_INIT0), 0x0000_0003);
    assert_eq!(read_reg(&distira, SST_FBI_INIT1), 0x0000_0100);
    assert_eq!(read_reg(&distira, SST_FBI_INIT2), 0x0000_0200);
    assert_eq!(read_reg(&distira, SST_FBI_INIT3), 0x0000_0601);
    assert_eq!(read_reg(&distira, SST_LFB_MODE), 0x0000_0005);
    assert_eq!(read_reg(&distira, SST_FBZ_MODE), FBZ_RGB_WMASK);
    assert_eq!(read_reg(&distira, SST_ALPHA_MODE), 0x0001_0001);
    assert_eq!(read_reg(&distira, SST_CLIP_LEFT_RIGHT), (2 << 16) | 7);
    assert_eq!(read_reg(&distira, SST_CLIP_LOW_Y_HIGH_Y), (3 << 16) | 9);
}

#[test]
fn fbi_init_register_writes_require_pci_init_enable() {
    let mut distira = Distira::new();
    let initial_init2 = read_reg(&distira, SST_FBI_INIT2);

    write_reg(&mut distira, SST_FBI_INIT0, FBIINIT0_GRAPHICS_RESET);
    write_reg(
        &mut distira,
        SST_FBI_INIT2,
        247 << FBIINIT2_BUFFER_OFFSET_SHIFT,
    );
    write_reg(&mut distira, SST_FBI_INIT4, 0x1234_5678);

    assert_eq!(read_reg(&distira, SST_FBI_INIT0), 0);
    assert_eq!(read_reg(&distira, SST_FBI_INIT2), initial_init2);
    assert_eq!(read_reg(&distira, SST_FBI_INIT4), 0);

    distira.set_init_enable(INIT_ENABLE_WRITE);
    write_reg(&mut distira, SST_FBI_INIT4, 0x1234_5678);
    assert_eq!(read_reg(&distira, SST_FBI_INIT4), 0x1234_5678);
}

#[test]
fn fbi_init_layout_and_reset_select_physical_buffers() {
    let mut distira = Distira::new();
    distira.set_init_enable(INIT_ENABLE_WRITE);
    write_reg(
        &mut distira,
        SST_FBI_INIT1,
        FBIINIT1_VIDEO_RESET | (10 << FBIINIT1_TILES_IN_X_SHIFT),
    );
    write_reg(
        &mut distira,
        SST_FBI_INIT2,
        150 << FBIINIT2_BUFFER_OFFSET_SHIFT,
    );
    write_reg(&mut distira, SST_VIDEO_DIMENSIONS, (480 << 16) | 639);

    assert!(!distira.display_enabled());
    assert_eq!(distira.display().width, 640);
    assert_eq!(distira.display().height, 480);
    assert_eq!(distira.display().pitch, 1280);
    assert_eq!(distira.display().front_base, 0);
    assert_eq!(distira.display().back_base, 150 * 4096);

    write_reg(&mut distira, SST_FBI_INIT1, 10 << FBIINIT1_TILES_IN_X_SHIFT);
    assert!(distira.display_enabled());
    distira.swap_buffers();
    assert_eq!(distira.display().front_base, 150 * 4096);

    write_reg(&mut distira, SST_FBI_INIT0, FBIINIT0_GRAPHICS_RESET);
    assert_eq!(distira.display().front_base, 0);
    assert_eq!(distira.display().back_base, 150 * 4096);
}

#[test]
fn voodoo_texture_detail_register_round_trips() {
    const SST_TDETAIL: usize = 0x308;

    let mut distira = Distira::new();

    write_reg(&mut distira, SST_TDETAIL, 0x0001_c23f);

    assert_eq!(read_reg(&distira, SST_TDETAIL), 0x0001_c23f);
}

#[test]
fn clear_back_buffer_and_swap_presents_rgb565_words() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 2);
    distira.clear_back_rgb(0x34, 0x56, 0x78);

    assert!(!distira.display_enabled());
    distira.swap_buffers();

    assert!(distira.display_enabled());
    let frame = distira.scanout_argb();
    assert_eq!(frame.len(), 8);
    assert!(frame.iter().all(|&pixel| pixel == 0x0031_557b));
}

#[test]
fn voodoo_fastfill_and_swap_present_the_back_buffer() {
    let mut distira = Distira::new();
    distira.set_frame_size(2, 2);

    write_reg(&mut distira, SST_CLIP_LEFT_RIGHT, 2);
    write_reg(&mut distira, SST_CLIP_LOW_Y_HIGH_Y, 2);
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_COLOR1, 0x0034_5678);
    write_reg(&mut distira, SST_FASTFILL_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame, vec![0x0031_557b; 4]);
}

#[test]
fn voodoo_lfb_writes_convert_argb8888_to_the_selected_back_buffer() {
    let mut distira = Distira::new();
    distira.set_frame_size(2, 1);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_ARGB8888 | LFB_WRITE_BACK,
    );
    distira.write_lfb_u32(0, 0x0034_5678);
    distira.swap_buffers();

    let frame = distira.scanout_argb();
    assert_eq!(frame, vec![0x0031_557b, 0x0000_0000]);
}

#[test]
fn voodoo_fifo_drains_queued_register_and_lfb_writes_in_order() {
    let mut direct = Distira::new();
    direct.set_frame_size(2, 1);
    write_reg(&mut direct, SST_CLIP_LEFT_RIGHT, 2);
    write_reg(&mut direct, SST_CLIP_LOW_Y_HIGH_Y, 1);
    write_reg(&mut direct, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut direct, SST_COLOR1, 0x0011_2233);
    write_reg(&mut direct, SST_FASTFILL_CMD, 1);
    write_reg(
        &mut direct,
        SST_LFB_MODE,
        LFB_FORMAT_ARGB8888 | LFB_WRITE_BACK,
    );
    direct.write_lfb_u32(0, 0x0034_5678);
    write_reg(&mut direct, SST_SWAPBUFFER_CMD, 1);

    let mut queued = Distira::new();
    queued.set_frame_size(2, 1);
    queued.queue_register_write(SST_CLIP_LEFT_RIGHT, 2);
    queued.queue_register_write(SST_CLIP_LOW_Y_HIGH_Y, 1);
    queued.queue_register_write(SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    queued.queue_register_write(SST_COLOR1, 0x0011_2233);
    queued.queue_register_write(SST_FASTFILL_CMD, 1);
    queued.queue_register_write(SST_LFB_MODE, LFB_FORMAT_ARGB8888 | LFB_WRITE_BACK);
    queued.queue_lfb_write_u32(0, 0x0034_5678);
    queued.queue_register_write(SST_SWAPBUFFER_CMD, 1);

    assert_eq!(queued.fifo_depth(), 8);
    assert!(!queued.fifo_is_empty());
    assert!(!queued.fifo_is_full());
    assert_ne!(read_reg(&queued, SST_STATUS) & 0x380, 0);

    queued.drain_fifo();

    assert!(queued.fifo_is_empty());
    assert_eq!(read_reg(&queued, SST_STATUS) & 0x380, 0);
    assert_eq!(queued.scanout_argb(), direct.scanout_argb());
}

#[test]
fn motherboard_chip_names_are_big_distira_and_small_distira() {
    let distira = Distira::new();

    assert_eq!(
        distira.chip_names(),
        [BIG_DISTIRA_CHIP_NAME, SMALL_DISTIRA_CHIP_NAME]
    );
}

#[test]
fn triangle_rasterizes_to_the_back_buffer_with_rgb565_scanout() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    let written = distira.draw_triangle([
        DistiraVertex::rgb(0.0, 0.0, 255, 0, 0),
        DistiraVertex::rgb(3.0, 0.0, 255, 0, 0),
        DistiraVertex::rgb(0.0, 3.0, 255, 0, 0),
    ]);
    assert_eq!(written, 6);

    distira.swap_buffers();
    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
    assert_eq!(frame[1], 0x00ff_0000);
    assert_eq!(frame[2], 0x00ff_0000);
    assert_eq!(frame[3], 0x0000_0000);
    assert_eq!(frame[4], 0x00ff_0000);
    assert_eq!(frame[5], 0x00ff_0000);
    assert_eq!(frame[6], 0x0000_0000);
    assert_eq!(frame[8], 0x00ff_0000);
    assert_eq!(frame[9], 0x0000_0000);
}

#[test]
fn ordered_dither_changes_low_colors_by_pixel_position() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    distira.set_dither_enabled(true);

    distira.draw_triangle([
        DistiraVertex::rgb(0.0, 0.0, 7, 3, 7),
        DistiraVertex::rgb(4.0, 0.0, 7, 3, 7),
        DistiraVertex::rgb(0.0, 4.0, 7, 3, 7),
    ]);
    distira.swap_buffers();

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_0000);
    assert_eq!(frame[1], 0x0008_0408);
}

#[test]
fn dac_data_ics_probe_answers_gclk1_vclk1_vclk7_through_fbi_init2() {
    // Mirrors sst1InitDacDetectICS (dac.c): the guest addresses DAC register
    // 7 with the ICS PLL sub-register index to probe (VCLK1=0x01, VCLK7=0x07,
    // GCLK1=0x0b), then issues a read cycle against DAC register 5 (the PLL
    // port) and expects fbiInit2's readback (gated by initEnable's remap
    // bit) to answer with that sub-register's ICS5342 power-on default.
    let mut distira = Distira::new();
    distira.set_init_enable(INIT_ENABLE_REMAP);

    let probe = |distira: &mut Distira, pll_index: u32| -> u32 {
        // Address DAC register 7 (write cycle, no SST_DACDATA_RD) and load
        // the PLL sub-register index into it.
        write_reg(distira, SST_DAC_DATA, (7 << DACDATA_ADDR_SHIFT) | pll_index);
        // Now issue a read cycle against DAC register 5 (the PLL port).
        write_reg(
            distira,
            SST_DAC_DATA,
            (5 << DACDATA_ADDR_SHIFT) | DACDATA_RD,
        );
        read_reg(distira, SST_FBI_INIT2) & 0xff
    };

    assert_eq!(
        probe(&mut distira, 0x01),
        0x55,
        "VCLK1 should read back 0x55"
    );
    assert_eq!(
        probe(&mut distira, 0x07),
        0x71,
        "VCLK7 should read back 0x71"
    );
    assert_eq!(
        probe(&mut distira, 0x0b),
        0x79,
        "GCLK1 should read back 0x79"
    );
}

#[test]
fn dac_data_write_side_effects_are_accepted_without_special_casing() {
    // Writing an arbitrary DAC register (not the PLL port, not a read cycle)
    // stores the byte and does not panic or corrupt other DAC state; a read
    // cycle against the PLL port with an unprobed index falls through to
    // the default 0xff, matching 86Box's dac_readdata reset-then-maybe-
    // overwritten shape. This is the "accepted/ignored gracefully" contract
    // the plan calls for beyond the three known ICS registers.
    let mut distira = Distira::new();
    distira.set_init_enable(INIT_ENABLE_REMAP);

    write_reg(&mut distira, SST_DAC_DATA, (2 << DACDATA_ADDR_SHIFT) | 0x42);
    write_reg(&mut distira, SST_DAC_DATA, (7 << DACDATA_ADDR_SHIFT) | 0x99);
    write_reg(
        &mut distira,
        SST_DAC_DATA,
        (5 << DACDATA_ADDR_SHIFT) | DACDATA_RD,
    );
    assert_eq!(read_reg(&distira, SST_FBI_INIT2) & 0xff, 0xff);
}

#[test]
fn dac_read_cycle_returns_the_addressed_register() {
    // A read cycle against a non-PLL DAC register must answer with THAT
    // register's byte, not whatever dac_data[7] (the PLL index latch) holds.
    let mut distira = Distira::new();
    distira.set_init_enable(INIT_ENABLE_REMAP);

    write_reg(&mut distira, SST_DAC_DATA, (2 << DACDATA_ADDR_SHIFT) | 0x42);
    write_reg(&mut distira, SST_DAC_DATA, (7 << DACDATA_ADDR_SHIFT) | 0x99);
    write_reg(
        &mut distira,
        SST_DAC_DATA,
        (2 << DACDATA_ADDR_SHIFT) | DACDATA_RD,
    );
    assert_eq!(
        read_reg(&distira, SST_FBI_INIT2) & 0xff,
        0x42,
        "register 2 reads back its own byte"
    );
}

#[test]
fn fbi_init2_reads_raw_storage_when_remap_bit_is_clear() {
    // Without initEnable's remap bit, fbiInit2 behaves like every other
    // fbiInit register: plain byte-mergeable storage, and a DAC read cycle
    // does not leak into it.
    let mut distira = Distira::new();
    distira.set_init_enable(INIT_ENABLE_WRITE);
    write_reg(&mut distira, SST_FBI_INIT2, 0x0000_0200);

    write_reg(&mut distira, SST_DAC_DATA, (7 << DACDATA_ADDR_SHIFT) | 0x0b);
    write_reg(
        &mut distira,
        SST_DAC_DATA,
        (5 << DACDATA_ADDR_SHIFT) | DACDATA_RD,
    );

    assert_eq!(read_reg(&distira, SST_FBI_INIT2), 0x0000_0200);
}

#[test]
fn w_buffer_mode_orders_depth_by_nearer_reciprocal_w() {
    // FBZ_W_BUFFER (SST_WBUFFER, bit 3 of fbzMode): when selected, the depth
    // test/write path uses the iterated 1/w value instead of the
    // fixed-point Z path. Drives the same shape as the existing
    // triangle_cmd_depth_test_rejects_farther_pixels test through the W
    // registers (SST_START_W/SST_DW_DX/DY) instead of SST_START_Z, and
    // checks the nearer (larger 1/w) triangle wins under the LESSTHAN op.
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | FBZ_W_BUFFER
            | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);
    // A small 1/w (0.01, register units are 14.18-scale like the S/T
    // texture coordinates the W wire format shares): far away.
    write_reg(&mut distira, SST_START_W, 164);
    write_reg(&mut distira, SST_DW_DX, 0);
    write_reg(&mut distira, SST_DW_DY, 0);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | FBZ_W_BUFFER
            | (DEPTHOP_LESSTHAN << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    // A larger 1/w (0.5): nearer. Must win and overwrite the far red triangle.
    write_reg(&mut distira, SST_START_W, 8192);
    write_reg(&mut distira, SST_DW_DX, 0);
    write_reg(&mut distira, SST_DW_DY, 0);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(
        frame[0], 0x0000_00ff,
        "the nearer (larger 1/w) triangle must win"
    );
    assert_eq!(frame[1], 0x0000_00ff);
}

#[test]
fn z_buffer_mode_is_unaffected_by_the_w_buffer_wiring() {
    // Regression guard: adding W-buffer support must not change Z-buffer
    // behavior when FBZ_W_BUFFER is clear. Same shape as the existing
    // triangle_cmd_depth_test_rejects_farther_pixels_and_counts_failures
    // test, kept here as a direct before/after comparison point.
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);
    write_reg(&mut distira, SST_START_Z, 0x0100 << 12);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | (DEPTHOP_LESSTHAN << FBZ_DEPTH_OP_SHIFT),
    );
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_Z, 0x0200 << 12);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
    assert_eq!(frame[1], 0x00ff_0000);
    assert_ne!(read_reg(&distira, SST_FBI_ZFUNC_FAIL), 0);
}

#[test]
fn v_retrace_and_status_bit_toggle_so_a_poll_loop_terminates() {
    // SST_V_RETRACE/SST_HV_RETRACE/SST_STATUS's vsync bit (bit 6, per
    // 86Box's vid_voodoo.c SST_status handler: "temp |= 0x40" when NOT in
    // retrace) were previously hardcoded, which would hang a real
    // grSstVRetrace()-style poll loop forever on whichever edge it waits
    // for. Advancing the device's frame-phase clock must move the beam
    // through both a "not retracing" and a "retracing" phase, so a guest
    // polling loop waiting on either edge observes it and terminates.
    let mut distira = Distira::new();
    distira.set_frame_size(64, 48);

    let mut saw_not_retracing = (read_reg(&distira, SST_STATUS) & 0x40) != 0;
    let mut saw_retracing = (read_reg(&distira, SST_STATUS) & 0x40) == 0;
    let initial_v_retrace = read_reg(&distira, SST_V_RETRACE);
    let mut v_retrace_changed = false;
    let mut hv_retrace_nonzero = false;

    for _ in 0..2000 {
        distira.advance_frame_phase(10_000);
        let status = read_reg(&distira, SST_STATUS);
        if status & 0x40 != 0 {
            saw_not_retracing = true;
        } else {
            saw_retracing = true;
        }
        if read_reg(&distira, SST_V_RETRACE) != initial_v_retrace {
            v_retrace_changed = true;
        }
        if read_reg(&distira, SST_HV_RETRACE) != 0 {
            hv_retrace_nonzero = true;
        }
    }

    assert!(
        saw_not_retracing,
        "the beam must spend time outside retrace"
    );
    assert!(saw_retracing, "the beam must spend time inside retrace");
    assert!(
        v_retrace_changed,
        "SST_V_RETRACE must advance, not stay fixed"
    );
    assert!(
        hv_retrace_nonzero,
        "SST_HV_RETRACE must report a nonzero line/time value at some point"
    );
}

#[test]
fn lfb_aperture_wraps_its_unused_high_address_bit() {
    let mut distira = Distira::new();
    distira.set_frame_size(1, 1);
    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_WRITE_FRONT | LFB_FORMAT_RGB565,
    );
    distira.write_lfb_u16(1 << 21, 0xf800);
    assert_eq!(distira.scanout_argb(), vec![0x00ff_0000]);
}

#[test]
fn lfb_physical_addresses_past_two_megabytes_are_open_bus() {
    let mut distira = Distira::new();
    distira.set_init_enable(INIT_ENABLE_WRITE);
    write_reg(&mut distira, SST_FBI_INIT1, 13 << FBIINIT1_TILES_IN_X_SHIFT);
    write_reg(
        &mut distira,
        SST_FBI_INIT2,
        247 << FBIINIT2_BUFFER_OFFSET_SHIFT,
    );
    write_reg(&mut distira, SST_VIDEO_DIMENSIONS, (600 << 16) | 799);
    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_DEPTH | LFB_WRITE_FRONT | LFB_READ_AUX,
    );

    let aperture_offset = (100 << 11) | (128 << 1);
    distira.write_lfb_u16(aperture_offset, 0xdead);

    assert_eq!(
        distira.read_lfb_u16(aperture_offset),
        0xffff,
        "the 800x600 auxiliary buffer starts near the end of installed memory"
    );
}
