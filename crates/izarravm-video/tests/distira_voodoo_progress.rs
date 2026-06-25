use izarravm_video::*;

fn read_reg(distira: &Distira, reg: usize) -> u32 {
    (0..4)
        .map(|i| u32::from(distira.read_mmio_u8(reg + i)) << (i * 8))
        .fold(0, |a, b| a | b)
}

fn write_reg(distira: &mut Distira, reg: usize, value: u32) {
    for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
        distira.write_mmio_u8(reg + i, byte);
    }
}

fn write_triangle_vertices(distira: &mut Distira) {
    write_reg(distira, SST_VERTEX_AX, 0 << 4);
    write_reg(distira, SST_VERTEX_AY, 0 << 4);
    write_reg(distira, SST_VERTEX_BX, 3 << 4);
    write_reg(distira, SST_VERTEX_BY, 0 << 4);
    write_reg(distira, SST_VERTEX_CX, 0 << 4);
    write_reg(distira, SST_VERTEX_CY, 3 << 4);
}

fn draw_textured_alpha_probe(
    color_path: u32,
    alpha_mode: u32,
    texel: u32,
    start_a: u32,
) -> (Vec<u32>, u32) {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, texel));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, color_path);
    write_reg(&mut distira, SST_ALPHA_MODE, alpha_mode);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_ARGB8332 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_triangle_vertices(&mut distira);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, start_a << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    (
        distira.scanout_argb(),
        read_reg(&distira, SST_FBI_AFUNC_FAIL),
    )
}

fn draw_textured_color_probe(
    color_path: u32,
    texture_mode: u32,
    texel: u32,
    color0: u32,
    color1: u32,
    start_rgb: (u32, u32, u32),
    start_a: u32,
) -> Vec<u32> {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, texel));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, color_path);
    write_reg(&mut distira, SST_COLOR0, color0);
    write_reg(&mut distira, SST_COLOR1, color1);
    write_reg(&mut distira, SST_TEXTURE_MODE, texture_mode << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_triangle_vertices(&mut distira);
    write_reg(&mut distira, SST_START_R, start_rgb.0 << 12);
    write_reg(&mut distira, SST_START_G, start_rgb.1 << 12);
    write_reg(&mut distira, SST_START_B, start_rgb.2 << 12);
    write_reg(&mut distira, SST_START_A, start_a << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);
    distira.scanout_argb()
}

fn draw_solid_depth_triangle(distira: &mut Distira, fbz_mode: u32, color: u32, depth: u32) {
    write_reg(distira, SST_FBZ_MODE, fbz_mode);
    write_triangle_vertices(distira);
    write_reg(distira, SST_START_R, ((color >> 16) & 0xff) << 12);
    write_reg(distira, SST_START_G, ((color >> 8) & 0xff) << 12);
    write_reg(distira, SST_START_B, (color & 0xff) << 12);
    write_reg(distira, SST_START_Z, depth);
    write_reg(distira, SST_TRIANGLE_CMD, 1);
}

#[test]
fn triangle_cmd_alpha_inverts_after_adding_local_alpha() {
    let alpha_mode =
        (0xf0 << ALPHA_REF_SHIFT) | (AFUNC_GREATERTHAN << ALPHA_FUNC_SHIFT) | ALPHA_TEST_ENABLE;
    let color_path = FBZCP_TEXTURE_ENABLED
        | (A_SELECT_TEX << FBZCP_A_SELECT_SHIFT)
        | (2 << FBZCP_CCA_ADD_SHIFT)
        | FBZCP_CCA_INVERT_OUTPUT;

    let (frame, afunc_fail) = draw_textured_alpha_probe(color_path, alpha_mode, 0x101c_101c, 0x40);

    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(afunc_fail, 6);
}

#[test]
fn triangle_cmd_alpha_zero_other_happens_before_add_local() {
    let alpha_mode =
        (0x80 << ALPHA_REF_SHIFT) | (AFUNC_GREATERTHAN << ALPHA_FUNC_SHIFT) | ALPHA_TEST_ENABLE;
    let color_path = FBZCP_TEXTURE_ENABLED
        | (A_SELECT_TEX << FBZCP_A_SELECT_SHIFT)
        | FBZCP_CCA_ZERO_OTHER
        | (2 << FBZCP_CCA_ADD_SHIFT);

    let (frame, afunc_fail) = draw_textured_alpha_probe(color_path, alpha_mode, 0xff1c_ff1c, 0x40);

    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(afunc_fail, 6);
}

#[test]
fn triangle_cmd_alpha_zero_other_happens_before_invert_output() {
    let alpha_mode =
        (0xf0 << ALPHA_REF_SHIFT) | (AFUNC_GREATERTHAN << ALPHA_FUNC_SHIFT) | ALPHA_TEST_ENABLE;
    let color_path = FBZCP_TEXTURE_ENABLED
        | (A_SELECT_TEX << FBZCP_A_SELECT_SHIFT)
        | FBZCP_CCA_ZERO_OTHER
        | FBZCP_CCA_INVERT_OUTPUT;

    let (frame, afunc_fail) = draw_textured_alpha_probe(color_path, alpha_mode, 0x801c_801c, 0x00);

    assert_eq!(frame[0], 0x0000_ff00);
    assert_eq!(afunc_fail, 0);
}

#[test]
fn triangle_cmd_alpha_clocal_add_mode_inverts_after_add() {
    let alpha_mode =
        (0xa0 << ALPHA_REF_SHIFT) | (AFUNC_GREATERTHAN << ALPHA_FUNC_SHIFT) | ALPHA_TEST_ENABLE;
    let color_path = FBZCP_TEXTURE_ENABLED
        | (A_SELECT_TEX << FBZCP_A_SELECT_SHIFT)
        | (1 << FBZCP_CCA_ADD_SHIFT)
        | FBZCP_CCA_INVERT_OUTPUT;

    let (frame, afunc_fail) = draw_textured_alpha_probe(color_path, alpha_mode, 0x201c_201c, 0x40);

    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(afunc_fail, 6);
}

#[test]
fn triangle_cmd_color_subtracts_before_clocal_modulation() {
    let color_path = FBZCP_TEXTURE_ENABLED
        | FBZCP_CC_LOCALSELECT_COLOR0
        | FBZCP_CC_SUB_CLOCAL
        | (CC_MSELECT_CLOCAL << FBZCP_CC_MSELECT_SHIFT)
        | FBZCP_CC_REVERSE_BLEND;

    let frame = draw_textured_color_probe(
        color_path,
        TEX_R5G6B5,
        0x07e0_07e0,
        0x0000_4000,
        0,
        (0, 0, 0),
        0xff,
    );

    assert_eq!(frame[0], 0x0000_3000);
}

#[test]
fn triangle_cmd_color_subtracts_before_alocal_modulation() {
    let color_path = FBZCP_TEXTURE_ENABLED
        | FBZCP_CC_SUB_CLOCAL
        | (CC_MSELECT_ALOCAL << FBZCP_CC_MSELECT_SHIFT)
        | FBZCP_CC_REVERSE_BLEND;

    let frame = draw_textured_color_probe(
        color_path,
        TEX_R5G6B5,
        0x07e0_07e0,
        0,
        0,
        (0, 0x40, 0),
        0x40,
    );

    assert_eq!(frame[0], 0x0000_3000);
}

#[test]
fn triangle_cmd_color_subtracts_then_modulates_then_adds_clocal() {
    let color_path = FBZCP_TEXTURE_ENABLED
        | FBZCP_CC_LOCALSELECT_COLOR0
        | FBZCP_CC_SUB_CLOCAL
        | (CC_MSELECT_CLOCAL << FBZCP_CC_MSELECT_SHIFT)
        | FBZCP_CC_REVERSE_BLEND
        | FBZCP_CC_ADD_CLOCAL;

    let frame = draw_textured_color_probe(
        color_path,
        TEX_R5G6B5,
        0x07e0_07e0,
        0x0000_4000,
        0,
        (0, 0, 0),
        0xff,
    );

    assert_eq!(frame[0], 0x0000_7100);
}

#[test]
fn triangle_cmd_color_subtracts_before_adding_clocal() {
    let color_path = FBZCP_TEXTURE_ENABLED
        | FBZCP_CC_LOCALSELECT_COLOR0
        | FBZCP_CC_SUB_CLOCAL
        | FBZCP_CC_ADD_CLOCAL;

    let frame = draw_textured_color_probe(
        color_path,
        TEX_R5G6B5,
        0x0400_0400,
        0x0000_4000,
        0,
        (0, 0, 0),
        0xff,
    );

    assert_eq!(frame[0], 0x0000_8200);
}

#[test]
fn triangle_cmd_color_adds_alocal_with_saturation() {
    let color_path = FBZCP_TEXTURE_ENABLED | FBZCP_CC_ADD_ALOCAL;

    let frame =
        draw_textured_color_probe(color_path, TEX_R5G6B5, 0x0700_0700, 0, 0, (0, 0, 0), 0x40);

    assert_eq!(frame[0], 0x0042_ff42);
}

#[test]
fn triangle_cmd_color_zero_other_happens_before_add_clocal() {
    let color_path = FBZCP_TEXTURE_ENABLED
        | FBZCP_CC_LOCALSELECT_COLOR0
        | FBZCP_CC_ZERO_OTHER
        | FBZCP_CC_ADD_CLOCAL;

    let frame = draw_textured_color_probe(
        color_path,
        TEX_R5G6B5,
        0x07e0_07e0,
        0x00ff_0000,
        0,
        (0, 0, 0),
        0xff,
    );

    assert_eq!(frame[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_color_zero_other_happens_before_output_invert() {
    let color_path = FBZCP_TEXTURE_ENABLED | FBZCP_CC_ZERO_OTHER | FBZCP_CC_INVERT_OUTPUT;

    let frame =
        draw_textured_color_probe(color_path, TEX_R5G6B5, 0x07e0_07e0, 0, 0, (0, 0, 0), 0xff);

    assert_eq!(frame[0], 0x00ff_ffff);
}

#[test]
fn triangle_cmd_color_uses_color0_as_clocal_for_add() {
    let color_path = FBZCP_TEXTURE_ENABLED | FBZCP_CC_LOCALSELECT_COLOR0 | FBZCP_CC_ADD_CLOCAL;

    let frame = draw_textured_color_probe(
        color_path,
        TEX_R5G6B5,
        0x001f_001f,
        0x00ff_0000,
        0,
        (0, 0xff, 0),
        0xff,
    );

    assert_eq!(frame[0], 0x00ff_00ff);
}

#[test]
fn triangle_cmd_color_local_override_uses_color0_when_texture_alpha_high() {
    const FBZCP_CC_LOCALSELECT_OVERRIDE: u32 = 1 << 7;
    let color_path = FBZCP_TEXTURE_ENABLED | FBZCP_CC_LOCALSELECT_OVERRIDE | FBZCP_CC_ADD_CLOCAL;

    let frame = draw_textured_color_probe(
        color_path,
        TEX_ARGB8332,
        0x801c_801c,
        0x00ff_0000,
        0,
        (0, 0, 0xff),
        0xff,
    );

    assert_eq!(frame[0], 0x00ff_ff00);
}

#[test]
fn triangle_cmd_color_local_override_uses_iterated_rgb_when_texture_alpha_low() {
    const FBZCP_CC_LOCALSELECT_OVERRIDE: u32 = 1 << 7;
    let color_path = FBZCP_TEXTURE_ENABLED
        | FBZCP_CC_LOCALSELECT_COLOR0
        | FBZCP_CC_LOCALSELECT_OVERRIDE
        | FBZCP_CC_ADD_CLOCAL;

    let frame = draw_textured_color_probe(
        color_path,
        TEX_ARGB8332,
        0x001c_001c,
        0x00ff_0000,
        0,
        (0, 0, 0xff),
        0xff,
    );

    assert_eq!(frame[0], 0x0000_ffff);
}

#[test]
fn triangle_cmd_alpha_mask_rejects_even_selected_alpha() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0x801c_801c));
    distira.drain_fifo();

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_BACK | FBZ_ALPHA_MASK,
    );
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | (A_SELECT_TEX << FBZCP_A_SELECT_SHIFT),
    );
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_ARGB8332 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_triangle_vertices(&mut distira);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0xff << 12);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(distira.scanout_argb()[0], 0x0000_00ff);
}

#[test]
fn triangle_cmd_alpha_mask_allows_odd_selected_alpha() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);
    assert!(distira.queue_texture_write_u32(0, 0x811c_811c));
    distira.drain_fifo();

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_BACK | FBZ_ALPHA_MASK,
    );
    write_reg(
        &mut distira,
        SST_FBZ_COLOR_PATH,
        FBZCP_TEXTURE_ENABLED | (A_SELECT_TEX << FBZCP_A_SELECT_SHIFT),
    );
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_ARGB8332 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_triangle_vertices(&mut distira);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_A, 0xff << 12);
    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(distira.scanout_argb()[0], 0x0000_ff00);
}

#[test]
fn triangle_cmd_without_depth_write_mask_does_not_update_depth() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    draw_solid_depth_triangle(
        &mut distira,
        FBZ_RGB_WMASK | FBZ_DRAW_BACK | FBZ_DEPTH_ENABLE | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
        0xff0000,
        0x1000,
    );
    draw_solid_depth_triangle(
        &mut distira,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | (DEPTHOP_LESSTHAN << FBZ_DEPTH_OP_SHIFT),
        0x00ff00,
        0x8000,
    );
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(distira.scanout_argb()[0], 0x0000_ff00);
}

#[test]
fn triangle_cmd_with_depth_write_mask_updates_depth() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    draw_solid_depth_triangle(
        &mut distira,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
        0xff0000,
        0x1000,
    );
    draw_solid_depth_triangle(
        &mut distira,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | (DEPTHOP_LESSTHAN << FBZ_DEPTH_OP_SHIFT),
        0x00ff00,
        0x8000,
    );
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(distira.scanout_argb()[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_depth_only_draw_updates_depth_without_rgb_writes() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);

    draw_solid_depth_triangle(
        &mut distira,
        FBZ_DRAW_BACK | FBZ_DEPTH_ENABLE | FBZ_DEPTH_WMASK | (DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT),
        0xff0000,
        0x1000,
    );
    draw_solid_depth_triangle(
        &mut distira,
        FBZ_RGB_WMASK
            | FBZ_DRAW_BACK
            | FBZ_DEPTH_ENABLE
            | FBZ_DEPTH_WMASK
            | (DEPTHOP_LESSTHAN << FBZ_DEPTH_OP_SHIFT),
        0x00ff00,
        0x8000,
    );
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(distira.scanout_argb()[0], 0x0000_00ff);
}

#[test]
fn triangle_cmd_draw_front_writes_scanout_without_swap() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);

    draw_solid_depth_triangle(&mut distira, FBZ_RGB_WMASK | FBZ_DRAW_FRONT, 0xff0000, 0);

    assert_eq!(distira.scanout_argb()[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_draw_back_waits_for_swap_before_scanout() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);

    draw_solid_depth_triangle(&mut distira, FBZ_RGB_WMASK | FBZ_DRAW_BACK, 0xff0000, 0);
    assert_eq!(distira.scanout_argb()[0], 0x0000_0000);

    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);
    assert_eq!(distira.scanout_argb()[0], 0x00ff_0000);
}

#[test]
fn voodoo_lfb_writes_argb8888_to_front_buffer_when_selected() {
    let mut distira = Distira::new();
    distira.set_frame_size(2, 1);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_ARGB8888 | LFB_WRITE_FRONT,
    );
    distira.write_lfb_u32(0, 0x00ff_0000);

    assert_eq!(distira.scanout_argb(), vec![0x00ff_0000, 0x0000_0000]);
}

#[test]
fn voodoo_lfb_writes_rgb555_dwords_to_back_buffer() {
    let mut distira = Distira::new();
    distira.set_frame_size(2, 1);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_RGB555 | LFB_WRITE_BACK,
    );
    distira.write_lfb_u32(0, 0x03e0_7c00);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(distira.scanout_argb(), vec![0x00ff_0000, 0x0000_ff00]);
}

#[test]
fn voodoo_lfb_writes_argb1555_dwords_to_back_buffer() {
    let mut distira = Distira::new();
    distira.set_frame_size(2, 1);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_ARGB1555 | LFB_WRITE_BACK,
    );
    distira.write_lfb_u32(0, 0x83e0_fc00);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(distira.scanout_argb(), vec![0x00ff_0000, 0x0000_ff00]);
}

#[test]
fn voodoo_lfb_writes_rgb565_dword_as_two_pixels() {
    let mut distira = Distira::new();
    distira.set_frame_size(2, 1);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_RGB565 | LFB_WRITE_BACK,
    );
    distira.write_lfb_u32(0, 0x07e0_f800);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(distira.scanout_argb(), vec![0x00ff_0000, 0x0000_ff00]);
}

#[test]
fn voodoo_lfb_depth_dword_writes_two_aux_pixels() {
    let mut distira = Distira::new();
    distira.set_frame_size(2, 1);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_DEPTH | LFB_WRITE_BACK | LFB_READ_AUX,
    );
    distira.write_lfb_u32(0, 0x2222_1111);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(distira.scanout_argb(), vec![0x0000_0000, 0x0000_0000]);
    assert_eq!(
        (0..4)
            .map(|offset| distira.read_lfb_u8(offset))
            .collect::<Vec<_>>(),
        vec![0x11, 0x11, 0x22, 0x22]
    );
}

#[test]
fn voodoo_lfb_depth_rgb565_dword_writes_one_color_and_depth_pixel() {
    let mut distira = Distira::new();
    distira.set_frame_size(2, 1);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_DEPTH_RGB565 | LFB_WRITE_BACK | LFB_READ_AUX,
    );
    distira.write_lfb_u32(4, 0x3333_f800);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(distira.scanout_argb(), vec![0x0000_0000, 0x00ff_0000]);
    assert_eq!(distira.read_lfb_u8(2), 0x33);
    assert_eq!(distira.read_lfb_u8(3), 0x33);
}

#[test]
fn voodoo_lfb_depth_rgb555_dword_converts_color_and_depth() {
    let mut distira = Distira::new();
    distira.set_frame_size(1, 1);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_DEPTH_RGB555 | LFB_WRITE_BACK | LFB_READ_AUX,
    );
    distira.write_lfb_u32(0, 0x4444_03e0);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(distira.scanout_argb(), vec![0x0000_ff00]);
    assert_eq!(distira.read_lfb_u8(0), 0x44);
    assert_eq!(distira.read_lfb_u8(1), 0x44);
}

#[test]
fn voodoo_lfb_depth_argb1555_dword_converts_color_and_depth() {
    let mut distira = Distira::new();
    distira.set_frame_size(1, 1);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_DEPTH_ARGB1555 | LFB_WRITE_BACK | LFB_READ_AUX,
    );
    distira.write_lfb_u32(0, 0x5555_fc00);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(distira.scanout_argb(), vec![0x00ff_0000]);
    assert_eq!(distira.read_lfb_u8(0), 0x55);
    assert_eq!(distira.read_lfb_u8(1), 0x55);
}

#[test]
fn voodoo_lfb_pipeline_respects_rgb_write_mask() {
    let mut distira = Distira::new();
    distira.set_frame_size(2, 1);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_RGB565 | LFB_WRITE_BACK | LFB_ENABLE_PIXEL_PIPELINE,
    );
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_DRAW_BACK);
    distira.write_lfb_u32(0, 0x07e0_f800);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(distira.scanout_argb(), vec![0x0000_0000, 0x0000_0000]);
}

#[test]
fn voodoo_lfb_pipeline_respects_depth_write_mask() {
    let mut distira = Distira::new();
    distira.set_frame_size(2, 1);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_DEPTH | LFB_WRITE_BACK | LFB_READ_AUX | LFB_ENABLE_PIXEL_PIPELINE,
    );
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_DRAW_BACK);
    distira.write_lfb_u32(0, 0x2222_1111);
    assert_eq!(
        (0..4)
            .map(|offset| distira.read_lfb_u8(offset))
            .collect::<Vec<_>>(),
        vec![0xff, 0xff, 0xff, 0xff]
    );

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_DRAW_BACK | FBZ_DEPTH_WMASK);
    distira.write_lfb_u32(0, 0x4444_3333);
    assert_eq!(
        (0..4)
            .map(|offset| distira.read_lfb_u8(offset))
            .collect::<Vec<_>>(),
        vec![0x33, 0x33, 0x44, 0x44]
    );
}

#[test]
fn voodoo_lfb_pipeline_depth_test_rejects_farther_depth_color_writes() {
    let mut distira = Distira::new();
    distira.set_frame_size(1, 1);

    write_reg(&mut distira, SST_LFB_MODE, LFB_FORMAT_DEPTH | LFB_READ_AUX);
    distira.write_lfb_u32(0, 0x0000_4000);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_DEPTH_RGB565 | LFB_WRITE_FRONT | LFB_READ_AUX | LFB_ENABLE_PIXEL_PIPELINE,
    );
    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK
            | FBZ_DEPTH_WMASK
            | FBZ_DEPTH_ENABLE
            | (DEPTHOP_LESSTHAN << FBZ_DEPTH_OP_SHIFT),
    );
    distira.write_lfb_u32(0, 0x8000_f800);

    assert_eq!(distira.scanout_argb(), vec![0x0000_0000]);
    assert_eq!(distira.read_lfb_u8(0), 0x00);
    assert_eq!(distira.read_lfb_u8(1), 0x40);

    distira.write_lfb_u32(0, 0x1000_07e0);

    assert_eq!(distira.scanout_argb(), vec![0x0000_ff00]);
    assert_eq!(distira.read_lfb_u8(0), 0x00);
    assert_eq!(distira.read_lfb_u8(1), 0x10);
}

#[test]
fn voodoo_lfb_pipeline_chroma_key_rejects_matching_color() {
    let mut distira = Distira::new();
    distira.set_frame_size(2, 1);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_RGB565 | LFB_WRITE_FRONT | LFB_ENABLE_PIXEL_PIPELINE,
    );
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_CHROMAKEY);
    write_reg(&mut distira, SST_CHROMA_KEY, 0x00ff_0000);
    distira.write_lfb_u32(0, 0x07e0_f800);

    assert_eq!(distira.scanout_argb(), vec![0x0000_0000, 0x0000_ff00]);
    assert_eq!(read_reg(&distira, SST_FBI_CHROMA_FAIL), 1);
}

#[test]
fn voodoo_lfb_pipeline_alpha_test_rejects_low_argb8888_alpha() {
    let mut distira = Distira::new();
    distira.set_frame_size(1, 1);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_ARGB8888 | LFB_WRITE_FRONT | LFB_ENABLE_PIXEL_PIPELINE,
    );
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK);
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        ALPHA_TEST_ENABLE | (AFUNC_GREATERTHAN << ALPHA_FUNC_SHIFT) | (0x80 << ALPHA_REF_SHIFT),
    );

    distira.write_lfb_u32(0, 0x40ff_0000);
    assert_eq!(distira.scanout_argb(), vec![0x0000_0000]);
    assert_eq!(read_reg(&distira, SST_FBI_AFUNC_FAIL), 1);

    distira.write_lfb_u32(0, 0xff00_ff00);
    assert_eq!(distira.scanout_argb(), vec![0x0000_ff00]);
    assert_eq!(read_reg(&distira, SST_FBI_AFUNC_FAIL), 1);
}

#[test]
fn voodoo_lfb_pipeline_applies_constant_fog_before_write() {
    let mut distira = Distira::new();
    distira.set_frame_size(1, 1);

    write_reg(
        &mut distira,
        SST_LFB_MODE,
        LFB_FORMAT_ARGB8888 | LFB_WRITE_FRONT | LFB_ENABLE_PIXEL_PIPELINE,
    );
    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK);
    write_reg(&mut distira, SST_FOG_MODE, FOG_ENABLE | FOG_CONSTANT);
    write_reg(&mut distira, SST_FOG_COLOR, 0x0000_4000);
    distira.write_lfb_u32(0, 0xff00_0000);

    assert_eq!(distira.scanout_argb(), vec![0x0000_4100]);
}
