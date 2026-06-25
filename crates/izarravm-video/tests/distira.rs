use izarravm_video::{
    BIG_DISTIRA_CHIP_NAME, DEPTHOP_ALWAYS, DEPTHOP_LESSTHAN, Distira, DistiraVertex, FBZ_CHROMAKEY,
    FBZ_DEPTH_ENABLE, FBZ_DEPTH_OP_SHIFT, FBZ_DEPTH_WMASK, FBZ_DRAW_BACK, FBZ_RGB_WMASK,
    LFB_FORMAT_ARGB8888, LFB_WRITE_BACK, SMALL_DISTIRA_CHIP_NAME, SST_ALPHA_MODE, SST_CHROMA_KEY,
    SST_CLIP_LEFT_RIGHT, SST_CLIP_LOW_Y_HIGH_Y, SST_COLOR1, SST_DR_DX, SST_DR_DY, SST_FASTFILL_CMD,
    SST_FBI_INIT0, SST_FBI_INIT1, SST_FBI_INIT2, SST_FBI_INIT3, SST_FBI_INIT7, SST_FBI_ZFUNC_FAIL,
    SST_FBZ_COLOR_PATH, SST_FBZ_MODE, SST_FDR_DX, SST_FDR_DY, SST_FDZ_DX, SST_FOG_COLOR,
    SST_FOG_MODE, SST_FSTART_B, SST_FSTART_G, SST_FSTART_R, SST_FSTART_Z, SST_FTRIANGLE_CMD,
    SST_FVERTEX_AX, SST_FVERTEX_AY, SST_FVERTEX_BX, SST_FVERTEX_BY, SST_FVERTEX_CX, SST_FVERTEX_CY,
    SST_LFB_MODE, SST_START_B, SST_START_G, SST_START_R, SST_START_Z, SST_STATUS,
    SST_SWAPBUFFER_CMD, SST_TRIANGLE_CMD, SST_VERTEX_AX, SST_VERTEX_AY, SST_VERTEX_BX,
    SST_VERTEX_BY, SST_VERTEX_CX, SST_VERTEX_CY,
};

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

fn cmdfifo_type5_header(space: u32, count: u32) -> u32 {
    (space << 30) | (count << 3) | 5
}

fn red_channel(pixel: u32) -> u32 {
    (pixel >> 16) & 0xff
}

#[test]
fn voodoo_registers_store_init_and_render_state() {
    let mut distira = Distira::new();

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
fn voodoo_fifo_drains_queued_register_lfb_and_texture_writes_in_order() {
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
    queued.queue_texture_write_u32(0x10, 0xdead_beef);
    queued.queue_register_write(SST_SWAPBUFFER_CMD, 1);

    assert_eq!(queued.fifo_depth(), 9);
    assert!(!queued.fifo_is_empty());
    assert!(!queued.fifo_is_full());
    assert_ne!(read_reg(&queued, SST_STATUS) & 0x380, 0);
    assert_eq!(queued.read_texture_u32(0x10), 0);

    queued.drain_fifo();

    assert!(queued.fifo_is_empty());
    assert_eq!(read_reg(&queued, SST_STATUS) & 0x380, 0);
    assert_eq!(queued.read_texture_u32(0x10), 0xdead_beef);
    assert_eq!(queued.scanout_argb(), direct.scanout_argb());
}

#[test]
fn command_fifo_type5_texture_packet_writes_texture_memory() {
    const FBIINIT7_CMDFIFO_ENABLE: u32 = 1 << 8;

    let mut distira = Distira::new();
    write_reg(&mut distira, SST_FBI_INIT7, FBIINIT7_CMDFIFO_ENABLE);

    assert!(distira.write_command_fifo_u32(0, cmdfifo_type5_header(3, 2)));
    assert!(distira.write_command_fifo_u32(4, 0x20));
    assert!(distira.write_command_fifo_u32(8, 0x1122_3344));
    assert!(distira.write_command_fifo_u32(12, 0xaabb_ccdd));

    assert_eq!(distira.fifo_depth(), 4);
    assert_eq!(distira.read_texture_u32(0x20), 0);
    assert_eq!(distira.read_texture_u32(0x24), 0);

    distira.drain_fifo();

    assert_eq!(distira.fifo_depth(), 0);
    assert_eq!(distira.read_texture_u32(0x20), 0x1122_3344);
    assert_eq!(distira.read_texture_u32(0x24), 0xaabb_ccdd);
}

#[test]
fn triangle_cmd_rasterizes_flat_untextured_triangle_from_integer_registers() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
    assert_eq!(frame[1], 0x00ff_0000);
    assert_eq!(frame[2], 0x00ff_0000);
    assert_eq!(frame[3], 0x0000_0000);
    assert_eq!(frame[4], 0x00ff_0000);
    assert_eq!(frame[5], 0x00ff_0000);
    assert_eq!(frame[6], 0x0000_0000);
    assert_eq!(frame[8], 0x00ff_0000);
}

#[test]
fn triangle_cmd_applies_integer_gouraud_color_gradients() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);
    write_reg(&mut distira, SST_DR_DX, 85 << 12);
    write_reg(&mut distira, SST_DR_DY, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert!(red_channel(frame[0]) < red_channel(frame[1]));
    assert!(red_channel(frame[1]) < red_channel(frame[2]));
    assert!(red_channel(frame[8]) < red_channel(frame[2]));
    assert_eq!(frame[3], 0x0000_0000);
}

#[test]
fn ftriangle_cmd_rasterizes_flat_untextured_triangle_from_float_registers() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FVERTEX_AX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_AY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BX, 3.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CY, 3.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_R, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_G, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_B, 0.0f32.to_bits());

    write_reg(&mut distira, SST_FTRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
    assert_eq!(frame[1], 0x00ff_0000);
    assert_eq!(frame[2], 0x00ff_0000);
    assert_eq!(frame[3], 0x0000_0000);
    assert_eq!(frame[4], 0x00ff_0000);
    assert_eq!(frame[5], 0x00ff_0000);
    assert_eq!(frame[6], 0x0000_0000);
    assert_eq!(frame[8], 0x00ff_0000);
}

#[test]
fn triangle_cmd_depth_test_rejects_farther_pixels_and_counts_failures() {
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
fn ftriangle_cmd_applies_float_gouraud_color_gradients() {
    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FVERTEX_AX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_AY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BX, 3.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CY, 3.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_R, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_G, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_B, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FDR_DX, 85.0f32.to_bits());
    write_reg(&mut distira, SST_FDR_DY, 0.0f32.to_bits());

    write_reg(&mut distira, SST_FTRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert!(red_channel(frame[0]) < red_channel(frame[1]));
    assert!(red_channel(frame[1]) < red_channel(frame[2]));
    assert!(red_channel(frame[8]) < red_channel(frame[2]));
    assert_eq!(frame[3], 0x0000_0000);
}

#[test]
fn ftriangle_cmd_depth_test_accepts_closer_float_z() {
    const DEPTH_LESS_THAN: u32 = DEPTHOP_LESSTHAN << FBZ_DEPTH_OP_SHIFT;
    const DEPTH_ALWAYS: u32 = DEPTHOP_ALWAYS << FBZ_DEPTH_OP_SHIFT;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_BACK | FBZ_DEPTH_ENABLE | FBZ_DEPTH_WMASK | DEPTH_ALWAYS,
    );
    write_reg(&mut distira, SST_FVERTEX_AX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_AY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BX, 3.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CY, 3.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_R, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_G, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_B, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_Z, 256.0f32.to_bits());
    write_reg(&mut distira, SST_FTRIANGLE_CMD, 1);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_BACK | FBZ_DEPTH_ENABLE | FBZ_DEPTH_WMASK | DEPTH_LESS_THAN,
    );
    write_reg(&mut distira, SST_FSTART_R, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_B, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_Z, 512.0f32.to_bits());
    write_reg(&mut distira, SST_FDZ_DX, (-170.0f32).to_bits());
    write_reg(&mut distira, SST_FTRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
    assert_eq!(frame[2], 0x0000_00ff);
}

#[test]
fn triangle_cmd_alpha_test_rejects_pixels_below_reference() {
    const SST_START_A: usize = 0x030;
    const SST_DA_DX: usize = 0x050;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (96 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
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
    write_reg(&mut distira, SST_START_A, 0);
    write_reg(&mut distira, SST_DA_DX, 100 << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_0000);
    assert_eq!(frame[1], 0x00ff_0000);
    assert_eq!(frame[2], 0x00ff_0000);
    assert_ne!(read_reg(&distira, SST_FBI_AFUNC_FAIL), 0);
}

#[test]
fn ftriangle_cmd_alpha_test_uses_float_alpha_derivatives() {
    const SST_FSTART_A: usize = 0x0b0;
    const SST_FDA_DX: usize = 0x0d0;
    const SST_FBI_AFUNC_FAIL: usize = 0x158;
    const AFUNC_GREATER_THAN: u32 = 4;
    const ALPHA_TEST_ENABLE: u32 = 1;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        (96 << 24) | (AFUNC_GREATER_THAN << 1) | ALPHA_TEST_ENABLE,
    );
    write_reg(&mut distira, SST_FVERTEX_AX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_AY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BX, 3.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CY, 3.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_R, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_G, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_B, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_A, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FDA_DX, 100.0f32.to_bits());

    write_reg(&mut distira, SST_FTRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_0000);
    assert_eq!(frame[1], 0x00ff_0000);
    assert_eq!(frame[2], 0x00ff_0000);
    assert_ne!(read_reg(&distira, SST_FBI_AFUNC_FAIL), 0);
}

#[test]
fn triangle_cmd_alpha_blends_source_over_destination() {
    const SST_START_A: usize = 0x030;
    const AFUNC_ASRC_ALPHA: u32 = 1;
    const AFUNC_AOMSRC_ALPHA: u32 = 5;
    const ALPHA_BLEND_ENABLE: u32 = 1 << 4;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(
        &mut distira,
        SST_ALPHA_MODE,
        ALPHA_BLEND_ENABLE | (AFUNC_ASRC_ALPHA << 8) | (AFUNC_AOMSRC_ALPHA << 12),
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
    write_reg(&mut distira, SST_START_A, 128 << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0084_007b);
    assert_eq!(frame[3], 0x0000_00ff);
}

#[test]
fn triangle_cmd_chroma_key_rejects_matching_source_color() {
    const SST_FBI_CHROMA_FAIL: usize = 0x150;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 255);

    write_reg(
        &mut distira,
        SST_FBZ_MODE,
        FBZ_RGB_WMASK | FBZ_DRAW_BACK | FBZ_CHROMAKEY,
    );
    write_reg(&mut distira, SST_CHROMA_KEY, 0x00ff_0000);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_00ff);
    assert_eq!(read_reg(&distira, SST_FBI_CHROMA_FAIL), 6);
}

#[test]
fn triangle_cmd_applies_constant_fog_color() {
    const FOG_ENABLE: u32 = 0x01;
    const FOG_CONSTANT: u32 = 0x20;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FOG_MODE, FOG_ENABLE | FOG_CONSTANT);
    write_reg(&mut distira, SST_FOG_COLOR, 0x0000_0033);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0);
    write_reg(&mut distira, SST_START_B, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0031);
}

#[test]
fn triangle_cmd_samples_rgb565_texture_when_texture_path_is_enabled() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
}

#[test]
fn triangle_cmd_uses_s_texture_gradient_for_nearest_rgb565_sampling() {
    const SST_START_S: usize = 0x034;
    const SST_START_T: usize = 0x038;
    const SST_DS_DX: usize = 0x054;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;
    const TEX_COORD_ONE: u32 = 1 << 14;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_f800));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_S, 0);
    write_reg(&mut distira, SST_START_T, 0);
    write_reg(&mut distira, SST_DS_DX, TEX_COORD_ONE);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
    assert_eq!(frame[1], 0x0000_ff00);
}

#[test]
fn ftriangle_cmd_uses_float_s_texture_gradient_for_nearest_rgb565_sampling() {
    const SST_FSTART_S: usize = 0x0b4;
    const SST_FSTART_T: usize = 0x0b8;
    const SST_FDS_DX: usize = 0x0d4;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_f800));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_FVERTEX_AX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_AY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BX, 3.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_BY, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CX, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FVERTEX_CY, 3.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_R, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_G, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_B, 255.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_S, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FSTART_T, 0.0f32.to_bits());
    write_reg(&mut distira, SST_FDS_DX, 1.0f32.to_bits());

    write_reg(&mut distira, SST_FTRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
    assert_eq!(frame[1], 0x0000_ff00);
}

#[test]
fn triangle_cmd_bilinear_filters_rgb565_texels() {
    const SST_START_S: usize = 0x034;
    const SST_START_T: usize = 0x038;
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;
    const TEXTUREMODE_BILINEAR_FILTER: u32 = 0x2;
    const TEX_COORD_HALF: u32 = 1 << 13;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x07e0_f800));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        SST_TEXTURE_MODE,
        (TEX_R5G6B5 << 8) | TEXTUREMODE_BILINEAR_FILTER,
    );
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);
    write_reg(&mut distira, SST_START_S, TEX_COORD_HALF);
    write_reg(&mut distira, SST_START_T, 0);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x007b_7d00);
}

#[test]
fn triangle_cmd_selects_rgb565_mip_level_from_tlod_min() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TLOD: usize = 0x304;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;
    const LOD1_MIN: u32 = 1 << 2;
    const RGB565_LOD1_OFFSET: usize = 256 * 256 * 2;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0xf800_f800));
    assert!(distira.queue_texture_write_u32(RGB565_LOD1_OFFSET, 0x07e0_07e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, SST_TLOD, LOD1_MIN);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0000_ff00);
}

#[test]
fn triangle_cmd_combines_two_rgb565_tmus() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const TREX0: usize = 0x2 << 10;
    const TREX1: usize = 0x4 << 10;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_R5G6B5: u32 = 0x0a;
    const TC_ADD_CLOCAL: u32 = 1 << 18;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0xf800_f800));
    assert!(distira.queue_texture_write_u32(4, 0x001f_001f));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(
        &mut distira,
        TREX0 | SST_TEXTURE_MODE,
        (TEX_R5G6B5 << 8) | TC_ADD_CLOCAL,
    );
    write_reg(&mut distira, TREX0 | SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, TREX1 | SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut distira, TREX1 | SST_TEX_BASE_ADDR, 4);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_00ff);
}

#[test]
fn triangle_cmd_samples_rgb332_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_RGB332: u32 = 0x00;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_00e0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_RGB332 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
}

#[test]
fn triangle_cmd_samples_i8_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_I8: u32 = 0x03;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_0080));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_I8 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0084_8284);
}

#[test]
fn triangle_cmd_samples_a8_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_A8: u32 = 0x02;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_0080));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_A8 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0084_8284);
}

#[test]
fn triangle_cmd_samples_ai44_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_AI8: u32 = 0x04;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_0008));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_AI8 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x008c_8a8c);
}

#[test]
fn triangle_cmd_samples_ai88_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_A8I8: u32 = 0x0d;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_ff80));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_A8I8 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x0084_8284);
}

#[test]
fn triangle_cmd_samples_argb8332_texture_when_selected() {
    const SST_TEXTURE_MODE: usize = 0x300;
    const SST_TEX_BASE_ADDR: usize = 0x30c;
    const FBZCP_TEXTURE_ENABLED: u32 = 1 << 27;
    const TEX_ARGB8332: u32 = 0x08;

    let mut distira = Distira::new();
    distira.set_frame_size(4, 4);
    distira.clear_back_rgb(0, 0, 0);
    assert!(distira.queue_texture_write_u32(0, 0x0000_ffe0));
    distira.drain_fifo();

    write_reg(&mut distira, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut distira, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut distira, SST_TEXTURE_MODE, TEX_ARGB8332 << 8);
    write_reg(&mut distira, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut distira, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut distira, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut distira, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut distira, SST_START_R, 0xff << 12);
    write_reg(&mut distira, SST_START_G, 0xff << 12);
    write_reg(&mut distira, SST_START_B, 0xff << 12);

    write_reg(&mut distira, SST_TRIANGLE_CMD, 1);
    write_reg(&mut distira, SST_SWAPBUFFER_CMD, 1);

    let frame = distira.scanout_argb();
    assert_eq!(frame[0], 0x00ff_0000);
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
