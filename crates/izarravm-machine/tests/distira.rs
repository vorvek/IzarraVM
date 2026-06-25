use izarravm_core::VideoCard;
use izarravm_firmware::I386DX25_TEST_ROM;
use izarravm_machine::{
    ActiveDisplay, BIOS_ROM_SIZE, DISTIRA_LFB_BASE, DISTIRA_MMIO_BASE, Machine, MachineProfile,
    StopReason,
};
use izarravm_video::{
    ALPHA_BLEND_ENABLE, ALPHA_DST_FUNC_SHIFT, ALPHA_SRC_FUNC_SHIFT, BLEND_AONE, BLEND_AZERO,
    DISTIRA_REG_FB_HEIGHT, DISTIRA_REG_FB_WIDTH, FBZ_DRAW_BACK, FBZ_RGB_WMASK,
    FBZCP_TEXTURE_ENABLED, LFB_ENABLE_PIXEL_PIPELINE, LFB_FORMAT_ARGB8888, LFB_FORMAT_RGB565,
    LFB_READ_BACK, LFB_WRITE_BACK, SST_ALPHA_MODE, SST_CLIP_LEFT_RIGHT, SST_CLIP_LOW_Y_HIGH_Y,
    SST_COLOR1, SST_FASTFILL_CMD, SST_FBI_INIT7, SST_FBZ_COLOR_PATH, SST_FBZ_MODE, SST_LFB_MODE,
    SST_START_A, SST_START_B, SST_START_G, SST_START_R, SST_STATUS, SST_SWAPBUFFER_CMD,
    SST_TEX_BASE_ADDR, SST_TEXTURE_MODE, SST_TRIANGLE_CMD, SST_VERTEX_AX, SST_VERTEX_AY,
    SST_VERTEX_BX, SST_VERTEX_BY, SST_VERTEX_CX, SST_VERTEX_CY, TEX_R5G6B5,
};

fn write_reg_at(machine: &mut Machine, base: u32, reg: usize, value: u32) {
    for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
        machine.write_physical_u8(base + reg as u32 + i as u32, byte);
    }
}

fn write_reg(machine: &mut Machine, reg: usize, value: u32) {
    write_reg_at(machine, DISTIRA_MMIO_BASE, reg, value);
}

fn read_reg(machine: &mut Machine, reg: usize) -> u32 {
    (0..4)
        .map(|i| u32::from(machine.read_physical_u8(DISTIRA_MMIO_BASE + reg as u32 + i)) << (i * 8))
        .fold(0, |a, b| a | b)
}

fn read_guest_u32(machine: &mut Machine, address: u32) -> u32 {
    (0..4)
        .map(|i| u32::from(machine.read_physical_u8(address + i)) << (i * 8))
        .fold(0, |a, b| a | b)
}

fn cmdfifo_type1_header(reg: usize, count: u32) -> u32 {
    1 | (((reg as u32) << 1) & 0x7ff8) | (count << 16)
}

fn cmdfifo_type5_framebuffer_header(count: u32) -> u32 {
    (2 << 30) | (count << 3) | 5
}

fn cmdfifo_type5_texture_header(count: u32) -> u32 {
    (3 << 30) | (count << 3) | 5
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_mov_eax_imm32(out: &mut Vec<u8>, value: u32) {
    out.push(0xb8);
    push_u32(out, value);
}

fn push_mov_dx_imm16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&[0x66, 0xba]);
    push_u16(out, value);
}

fn push_out_dx_eax(out: &mut Vec<u8>, port: u16, value: u32) {
    push_mov_dx_imm16(out, port);
    push_mov_eax_imm32(out, value);
    out.push(0xef);
}

fn push_mov_moffs_u32_imm32(out: &mut Vec<u8>, address: u32, value: u32) {
    out.extend_from_slice(&[0xc7, 0x05]);
    push_u32(out, address);
    push_u32(out, value);
}

fn push_mov_moffs_u16_imm16(out: &mut Vec<u8>, address: u32, value: u16) {
    out.extend_from_slice(&[0x66, 0xc7, 0x05]);
    push_u32(out, address);
    push_u16(out, value);
}

fn push_load_ax_moffs(out: &mut Vec<u8>, address: u32) {
    out.extend_from_slice(&[0x66, 0xa1]);
    push_u32(out, address);
}

fn push_load_al_moffs(out: &mut Vec<u8>, address: u32) {
    out.push(0xa0);
    push_u32(out, address);
}

fn push_load_eax_moffs(out: &mut Vec<u8>, address: u32) {
    out.push(0xa1);
    push_u32(out, address);
}

fn push_store_ax_moffs(out: &mut Vec<u8>, address: u32) {
    out.extend_from_slice(&[0x66, 0xa3]);
    push_u32(out, address);
}

fn push_store_al_moffs(out: &mut Vec<u8>, address: u32) {
    out.push(0xa2);
    push_u32(out, address);
}

fn push_store_eax_moffs(out: &mut Vec<u8>, address: u32) {
    out.push(0xa3);
    push_u32(out, address);
}

fn protected_flat_rom(body: &[u8]) -> Vec<u8> {
    const ROM_BASE: u32 = 0x000f_0000;
    let mut protected = vec![
        0x66, 0xb8, 0x10, 0x00, // mov ax,10h
        0x8e, 0xd8, // mov ds,ax
        0x8e, 0xc0, // mov es,ax
        0x8e, 0xd0, // mov ss,ax
        0xbc, 0x00, 0x80, 0x00, 0x00, // mov esp,8000h
    ];
    protected.extend_from_slice(body);
    protected.push(0xf4); // hlt

    let real_prefix_len = 27u16;
    let protected_offset = u32::from(real_prefix_len);
    let gdtr_offset = real_prefix_len as usize + protected.len();
    let gdt_offset = gdtr_offset + 6;

    let mut code = Vec::new();
    code.extend_from_slice(&[0x0e, 0x1f]); // push cs; pop ds
    code.push(0xfa); // cli
    code.extend_from_slice(&[0x66, 0x0f, 0x01, 0x16]); // lgdt [gdtr]
    push_u16(&mut code, gdtr_offset as u16);
    code.extend_from_slice(&[0x0f, 0x20, 0xc0]); // mov eax,cr0
    code.extend_from_slice(&[0x66, 0x83, 0xc8, 0x01]); // or eax,1
    code.extend_from_slice(&[0x0f, 0x22, 0xc0]); // mov cr0,eax
    code.extend_from_slice(&[0x66, 0xea]); // jmp 08h:protected_entry
    push_u32(&mut code, ROM_BASE + protected_offset);
    push_u16(&mut code, 0x0008);
    assert_eq!(code.len(), usize::from(real_prefix_len));
    code.extend_from_slice(&protected);

    push_u16(&mut code, 24 - 1);
    push_u32(&mut code, ROM_BASE + gdt_offset as u32);
    code.extend_from_slice(&[0; 8]);
    code.extend_from_slice(&[0xff, 0xff, 0, 0, 0, 0x9a, 0xcf, 0]);
    code.extend_from_slice(&[0xff, 0xff, 0, 0, 0, 0x92, 0xcf, 0]);

    let mut rom = vec![0; BIOS_ROM_SIZE];
    rom[..code.len()].copy_from_slice(&code);
    rom[0xfff0..0xfff5].copy_from_slice(&[0xea, 0x00, 0x00, 0x00, 0xf0]);
    rom
}

#[test]
fn distira_mmio_and_lfb_are_wired_into_machine_scanout() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Distira),
        I386DX25_TEST_ROM,
    )
    .unwrap();

    assert_eq!(read_reg(&mut machine, SST_STATUS) & 0x380, 0);
    assert_eq!(machine.read_physical_u8(DISTIRA_LFB_BASE), 0xff);
    machine.write_physical_u8(DISTIRA_LFB_BASE, 0x34);
    assert_eq!(machine.read_physical_u8(DISTIRA_LFB_BASE), 0xff);

    write_reg(&mut machine, DISTIRA_REG_FB_WIDTH, 2);
    write_reg(&mut machine, DISTIRA_REG_FB_HEIGHT, 2);
    write_reg(&mut machine, SST_CLIP_LEFT_RIGHT, 2);
    write_reg(&mut machine, SST_CLIP_LOW_Y_HIGH_Y, 2);
    write_reg(&mut machine, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut machine, SST_COLOR1, 0x0034_5678);
    write_reg(&mut machine, SST_FASTFILL_CMD, 1);
    write_reg(&mut machine, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(machine.active_display(), ActiveDisplay::Distira);
    let (frame, width, height) = machine.frame_argb();
    assert_eq!((width, height), (2, 2));
    assert_eq!(frame, vec![0x0031_557b; 4]);
}

#[test]
fn distira_render_threads_are_applied_to_the_machine() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Distira),
        I386DX25_TEST_ROM,
    )
    .unwrap();

    assert_eq!(machine.distira_render_threads(), 2);
    machine.set_distira_render_threads(4);
    assert_eq!(machine.distira_render_threads(), 4);
    machine.set_distira_render_threads(3);
    assert_eq!(machine.distira_render_threads(), 2);
}

#[test]
fn distira_lfb_dword_writes_follow_voodoo_lfb_format() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Distira),
        I386DX25_TEST_ROM,
    )
    .unwrap();

    write_reg(&mut machine, DISTIRA_REG_FB_WIDTH, 2);
    write_reg(&mut machine, DISTIRA_REG_FB_HEIGHT, 1);
    write_reg(
        &mut machine,
        SST_LFB_MODE,
        LFB_FORMAT_ARGB8888 | LFB_WRITE_BACK,
    );
    machine.write_physical_u32(DISTIRA_LFB_BASE, 0x0034_5678);
    write_reg(&mut machine, SST_SWAPBUFFER_CMD, 1);

    let (frame, width, height) = machine.frame_argb();
    assert_eq!((width, height), (2, 1));
    assert_eq!(frame, vec![0x0031_557b, 0x0000_0000]);
}

#[test]
fn distira_lfb_word_writes_use_voodoo_pixel_pipeline() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Distira),
        I386DX25_TEST_ROM,
    )
    .unwrap();

    write_reg(&mut machine, DISTIRA_REG_FB_WIDTH, 1);
    write_reg(&mut machine, DISTIRA_REG_FB_HEIGHT, 1);
    write_reg(&mut machine, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut machine, SST_COLOR1, 0x0000_00ff);
    write_reg(&mut machine, SST_FASTFILL_CMD, 1);
    write_reg(
        &mut machine,
        SST_LFB_MODE,
        LFB_FORMAT_RGB565 | LFB_WRITE_BACK | LFB_ENABLE_PIXEL_PIPELINE,
    );
    write_reg(
        &mut machine,
        SST_ALPHA_MODE,
        ALPHA_BLEND_ENABLE
            | (BLEND_AZERO << ALPHA_SRC_FUNC_SHIFT)
            | (BLEND_AONE << ALPHA_DST_FUNC_SHIFT),
    );

    machine.write_physical_u16(DISTIRA_LFB_BASE, 0xf800);
    write_reg(&mut machine, SST_SWAPBUFFER_CMD, 1);

    let (frame, width, height) = machine.frame_argb();
    assert_eq!((width, height), (1, 1));
    assert_eq!(frame, vec![0x0000_00ff]);
}

#[test]
fn distira_odd_aligned_lfb_word_dword_accesses_use_voodoo_callbacks() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Distira),
        I386DX25_TEST_ROM,
    )
    .unwrap();

    write_reg(&mut machine, DISTIRA_REG_FB_WIDTH, 4);
    write_reg(&mut machine, DISTIRA_REG_FB_HEIGHT, 1);
    write_reg(
        &mut machine,
        SST_LFB_MODE,
        LFB_FORMAT_RGB565 | LFB_WRITE_BACK | LFB_READ_BACK,
    );

    machine.write_physical_u16(DISTIRA_LFB_BASE + 1, 0xf800);
    assert_eq!(machine.read_physical_u16(DISTIRA_LFB_BASE + 1), 0xf800);
    assert_eq!(machine.read_physical_u8(DISTIRA_LFB_BASE + 1), 0xff);

    machine.write_physical_u32(DISTIRA_LFB_BASE + 2, 0x07e0_001f);
    assert_eq!(machine.read_physical_u32(DISTIRA_LFB_BASE + 3), 0x07e0_001f);
    write_reg(&mut machine, SST_SWAPBUFFER_CMD, 1);

    let (frame, width, height) = machine.frame_argb();
    assert_eq!((width, height), (4, 1));
    assert_eq!(frame, vec![0x00ff_0000, 0x0000_00ff, 0x0000_ff00, 0]);
}

#[test]
fn distira_guest_lfb_bar_odd_reads_and_writes_use_voodoo_callbacks() {
    const ASSIGNED_BAR: u32 = 0xe200_0000;
    const ASSIGNED_LFB: u32 = ASSIGNED_BAR + 0x0040_0000;
    const SCRATCH: u32 = 0x2200;

    let mut code = Vec::new();
    push_out_dx_eax(&mut code, 0x0cf8, 0x8000_8010);
    push_out_dx_eax(&mut code, 0x0cfc, ASSIGNED_BAR);
    push_out_dx_eax(&mut code, 0x0cf8, 0x8000_8004);
    push_out_dx_eax(&mut code, 0x0cfc, 0x0000_0002);
    push_mov_moffs_u32_imm32(&mut code, ASSIGNED_BAR + DISTIRA_REG_FB_WIDTH as u32, 4);
    push_mov_moffs_u32_imm32(&mut code, ASSIGNED_BAR + DISTIRA_REG_FB_HEIGHT as u32, 1);
    push_mov_moffs_u32_imm32(
        &mut code,
        ASSIGNED_BAR + SST_LFB_MODE as u32,
        LFB_FORMAT_RGB565 | LFB_WRITE_BACK | LFB_READ_BACK,
    );
    push_mov_moffs_u16_imm16(&mut code, ASSIGNED_LFB + 1, 0xf800);
    push_load_ax_moffs(&mut code, ASSIGNED_LFB + 1);
    push_store_ax_moffs(&mut code, SCRATCH);
    push_load_al_moffs(&mut code, ASSIGNED_LFB + 1);
    push_store_al_moffs(&mut code, SCRATCH + 2);
    push_mov_moffs_u32_imm32(&mut code, ASSIGNED_LFB + 2, 0x07e0_001f);
    push_load_eax_moffs(&mut code, ASSIGNED_LFB + 3);
    push_store_eax_moffs(&mut code, SCRATCH + 4);
    push_mov_moffs_u32_imm32(&mut code, ASSIGNED_BAR + SST_SWAPBUFFER_CMD as u32, 1);

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Distira),
        protected_flat_rom(&code),
    )
    .unwrap();

    let reason = machine.run_until_halt_or_cycles(500_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.read_physical_u16(SCRATCH), 0xf800);
    assert_eq!(machine.read_physical_u8(SCRATCH + 2), 0xff);
    assert_eq!(machine.read_physical_u32(SCRATCH + 4), 0x07e0_001f);
    let (frame, width, height) = machine.frame_argb();
    assert_eq!((width, height), (4, 1));
    assert_eq!(frame, vec![0x00ff_0000, 0x0000_00ff, 0x0000_ff00, 0]);
}

#[test]
fn distira_guest_cmdfifo_type1_packets_use_assigned_bar_aperture() {
    const ASSIGNED_BAR: u32 = 0xe300_0000;
    const CMD_FIFO_BASE: u32 = ASSIGNED_BAR + 0x0020_0000;
    const SST_CMD_FIFO_DEPTH: usize = 0x1f4;
    const FBIINIT7_CMDFIFO_ENABLE: u32 = 1 << 8;

    let mut code = Vec::new();
    push_out_dx_eax(&mut code, 0x0cf8, 0x8000_8010);
    push_out_dx_eax(&mut code, 0x0cfc, ASSIGNED_BAR);
    push_out_dx_eax(&mut code, 0x0cf8, 0x8000_8004);
    push_out_dx_eax(&mut code, 0x0cfc, 0x0000_0002);
    push_mov_moffs_u32_imm32(&mut code, ASSIGNED_BAR + DISTIRA_REG_FB_WIDTH as u32, 2);
    push_mov_moffs_u32_imm32(&mut code, ASSIGNED_BAR + DISTIRA_REG_FB_HEIGHT as u32, 1);
    push_mov_moffs_u32_imm32(
        &mut code,
        ASSIGNED_BAR + SST_FBI_INIT7 as u32,
        FBIINIT7_CMDFIFO_ENABLE,
    );
    push_mov_moffs_u32_imm32(
        &mut code,
        CMD_FIFO_BASE,
        cmdfifo_type1_header(SST_CLIP_LEFT_RIGHT, 1),
    );
    push_mov_moffs_u32_imm32(&mut code, CMD_FIFO_BASE + 4, 2);
    push_mov_moffs_u32_imm32(
        &mut code,
        CMD_FIFO_BASE + 8,
        cmdfifo_type1_header(SST_CLIP_LOW_Y_HIGH_Y, 1),
    );
    push_mov_moffs_u32_imm32(&mut code, CMD_FIFO_BASE + 12, 1);
    push_mov_moffs_u32_imm32(
        &mut code,
        CMD_FIFO_BASE + 16,
        cmdfifo_type1_header(SST_FBZ_MODE, 1),
    );
    push_mov_moffs_u32_imm32(&mut code, CMD_FIFO_BASE + 20, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    push_mov_moffs_u32_imm32(
        &mut code,
        CMD_FIFO_BASE + 24,
        cmdfifo_type1_header(SST_COLOR1, 1),
    );
    push_mov_moffs_u32_imm32(&mut code, CMD_FIFO_BASE + 28, 0x0034_5678);
    push_mov_moffs_u32_imm32(
        &mut code,
        CMD_FIFO_BASE + 32,
        cmdfifo_type1_header(SST_FASTFILL_CMD, 1),
    );
    push_mov_moffs_u32_imm32(&mut code, CMD_FIFO_BASE + 36, 1);
    push_mov_moffs_u32_imm32(
        &mut code,
        CMD_FIFO_BASE + 40,
        cmdfifo_type1_header(SST_SWAPBUFFER_CMD, 1),
    );
    push_mov_moffs_u32_imm32(&mut code, CMD_FIFO_BASE + 44, 1);

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Distira),
        protected_flat_rom(&code),
    )
    .unwrap();

    let reason = machine.run_until_halt_or_cycles(500_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    assert_eq!(read_reg(&mut machine, SST_CMD_FIFO_DEPTH), 12);
    assert_ne!(read_reg(&mut machine, SST_STATUS) & 0x380, 0);

    machine.drain_distira_fifo();

    assert_eq!(read_reg(&mut machine, SST_CMD_FIFO_DEPTH), 0);
    assert_eq!(read_reg(&mut machine, SST_STATUS) & 0x380, 0);
    let (frame, width, height) = machine.frame_argb();
    assert_eq!((width, height), (2, 1));
    assert_eq!(frame, vec![0x0031_557b; 2]);
}

#[test]
fn distira_guest_cmdfifo_type5_framebuffer_packets_use_assigned_bar_aperture() {
    const ASSIGNED_BAR: u32 = 0xe400_0000;
    const CMD_FIFO_BASE: u32 = ASSIGNED_BAR + 0x0020_0000;
    const SST_CMD_FIFO_DEPTH: usize = 0x1f4;
    const FBIINIT7_CMDFIFO_ENABLE: u32 = 1 << 8;

    let mut code = Vec::new();
    push_out_dx_eax(&mut code, 0x0cf8, 0x8000_8010);
    push_out_dx_eax(&mut code, 0x0cfc, ASSIGNED_BAR);
    push_out_dx_eax(&mut code, 0x0cf8, 0x8000_8004);
    push_out_dx_eax(&mut code, 0x0cfc, 0x0000_0002);
    push_mov_moffs_u32_imm32(&mut code, ASSIGNED_BAR + DISTIRA_REG_FB_WIDTH as u32, 2);
    push_mov_moffs_u32_imm32(&mut code, ASSIGNED_BAR + DISTIRA_REG_FB_HEIGHT as u32, 1);
    push_mov_moffs_u32_imm32(
        &mut code,
        ASSIGNED_BAR + SST_LFB_MODE as u32,
        LFB_FORMAT_ARGB8888 | LFB_WRITE_BACK,
    );
    push_mov_moffs_u32_imm32(
        &mut code,
        ASSIGNED_BAR + SST_FBI_INIT7 as u32,
        FBIINIT7_CMDFIFO_ENABLE,
    );
    push_mov_moffs_u32_imm32(
        &mut code,
        CMD_FIFO_BASE,
        cmdfifo_type5_framebuffer_header(1),
    );
    push_mov_moffs_u32_imm32(&mut code, CMD_FIFO_BASE + 4, 0);
    push_mov_moffs_u32_imm32(&mut code, CMD_FIFO_BASE + 8, 0x0034_5678);

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Distira),
        protected_flat_rom(&code),
    )
    .unwrap();

    let reason = machine.run_until_halt_or_cycles(500_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    assert_eq!(read_reg(&mut machine, SST_CMD_FIFO_DEPTH), 3);

    machine.drain_distira_fifo();
    write_reg(&mut machine, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(read_reg(&mut machine, SST_CMD_FIFO_DEPTH), 0);
    let (frame, width, height) = machine.frame_argb();
    assert_eq!((width, height), (2, 1));
    assert_eq!(frame, vec![0x0031_557b, 0x0000_0000]);
}

#[test]
fn distira_guest_cmdfifo_type5_texture_packets_use_assigned_bar_aperture() {
    const ASSIGNED_BAR: u32 = 0xe500_0000;
    const CMD_FIFO_BASE: u32 = ASSIGNED_BAR + 0x0020_0000;
    const SST_CMD_FIFO_DEPTH: usize = 0x1f4;
    const FBIINIT7_CMDFIFO_ENABLE: u32 = 1 << 8;

    let mut code = Vec::new();
    push_out_dx_eax(&mut code, 0x0cf8, 0x8000_8010);
    push_out_dx_eax(&mut code, 0x0cfc, ASSIGNED_BAR);
    push_out_dx_eax(&mut code, 0x0cf8, 0x8000_8004);
    push_out_dx_eax(&mut code, 0x0cfc, 0x0000_0002);
    push_mov_moffs_u32_imm32(
        &mut code,
        ASSIGNED_BAR + SST_FBI_INIT7 as u32,
        FBIINIT7_CMDFIFO_ENABLE,
    );
    push_mov_moffs_u32_imm32(&mut code, CMD_FIFO_BASE, cmdfifo_type5_texture_header(1));
    push_mov_moffs_u32_imm32(&mut code, CMD_FIFO_BASE + 4, 0);
    push_mov_moffs_u32_imm32(&mut code, CMD_FIFO_BASE + 8, 0x07e0_07e0);

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Distira),
        protected_flat_rom(&code),
    )
    .unwrap();

    let reason = machine.run_until_halt_or_cycles(500_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    assert_eq!(read_reg(&mut machine, SST_CMD_FIFO_DEPTH), 3);

    machine.drain_distira_fifo();

    assert_eq!(read_reg(&mut machine, SST_CMD_FIFO_DEPTH), 0);
    write_reg(&mut machine, DISTIRA_REG_FB_WIDTH, 4);
    write_reg(&mut machine, DISTIRA_REG_FB_HEIGHT, 4);
    write_reg(&mut machine, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut machine, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut machine, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut machine, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut machine, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut machine, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut machine, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut machine, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut machine, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut machine, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut machine, SST_START_R, 0xff << 12);
    write_reg(&mut machine, SST_START_G, 0xff << 12);
    write_reg(&mut machine, SST_START_B, 0xff << 12);
    write_reg(&mut machine, SST_START_A, 0xff << 12);
    write_reg(&mut machine, SST_TRIANGLE_CMD, 1);
    write_reg(&mut machine, SST_SWAPBUFFER_CMD, 1);

    let (frame, width, height) = machine.frame_argb();
    assert_eq!((width, height), (4, 4));
    assert_eq!(frame[0], 0x0000_ff00);
}

#[test]
fn distira_guest_direct_texture_bar_writes_feed_texture_sampling() {
    const ASSIGNED_BAR: u32 = 0xe600_0000;
    const ASSIGNED_TEX: u32 = ASSIGNED_BAR + 0x0080_0000;

    let mut code = Vec::new();
    push_out_dx_eax(&mut code, 0x0cf8, 0x8000_8010);
    push_out_dx_eax(&mut code, 0x0cfc, ASSIGNED_BAR);
    push_out_dx_eax(&mut code, 0x0cf8, 0x8000_8004);
    push_out_dx_eax(&mut code, 0x0cfc, 0x0000_0002);
    push_mov_moffs_u32_imm32(&mut code, ASSIGNED_TEX, 0x07e0_07e0);

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Distira),
        protected_flat_rom(&code),
    )
    .unwrap();

    let reason = machine.run_until_halt_or_cycles(500_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    write_reg(&mut machine, DISTIRA_REG_FB_WIDTH, 4);
    write_reg(&mut machine, DISTIRA_REG_FB_HEIGHT, 4);
    write_reg(&mut machine, SST_FBZ_MODE, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    write_reg(&mut machine, SST_FBZ_COLOR_PATH, FBZCP_TEXTURE_ENABLED);
    write_reg(&mut machine, SST_TEXTURE_MODE, TEX_R5G6B5 << 8);
    write_reg(&mut machine, SST_TEX_BASE_ADDR, 0);
    write_reg(&mut machine, SST_VERTEX_AX, 0 << 4);
    write_reg(&mut machine, SST_VERTEX_AY, 0 << 4);
    write_reg(&mut machine, SST_VERTEX_BX, 3 << 4);
    write_reg(&mut machine, SST_VERTEX_BY, 0 << 4);
    write_reg(&mut machine, SST_VERTEX_CX, 0 << 4);
    write_reg(&mut machine, SST_VERTEX_CY, 3 << 4);
    write_reg(&mut machine, SST_START_R, 0xff << 12);
    write_reg(&mut machine, SST_START_G, 0xff << 12);
    write_reg(&mut machine, SST_START_B, 0xff << 12);
    write_reg(&mut machine, SST_START_A, 0xff << 12);
    write_reg(&mut machine, SST_TRIANGLE_CMD, 1);
    write_reg(&mut machine, SST_SWAPBUFFER_CMD, 1);

    let (frame, width, height) = machine.frame_argb();
    assert_eq!((width, height), (4, 4));
    assert_eq!(frame[0], 0x0000_ff00);
}

#[test]
fn distira_pci_config_ports_report_voodoo_graphics_identity() {
    // mov dx,0x0cf8; mov eax,0x80008000; out dx,eax
    // mov dx,0x0cfc; in eax,dx; mov [0x0200],eax; int 20h
    const PROG: [u8; 22] = [
        0xBA, 0xF8, 0x0C, 0x66, 0xB8, 0x00, 0x80, 0x00, 0x80, 0x66, 0xEF, 0xBA, 0xFC, 0x0C, 0x66,
        0xED, 0x66, 0xA3, 0x00, 0x02, 0xCD, 0x20,
    ];
    let mut machine =
        Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Distira), &PROG).unwrap();

    let reason = machine.run_until_halt_or_cycles(100_000).unwrap();

    assert_eq!(reason, StopReason::DosExit { code: 0 });
    // The direct DOS loader enters .COM programs at segment 0x0200; the guest
    // stores at DS:0200, so the physical result lives at 0x2200.
    assert_eq!(read_guest_u32(&mut machine, 0x2200), 0x0001_121a);
}

#[test]
fn distira_pci_bar_maps_voodoo_mmio_and_lfb_windows() {
    const ASSIGNED_BAR: u32 = 0xE200_0000;
    const ASSIGNED_LFB: u32 = ASSIGNED_BAR + 0x0040_0000;
    const PROG: [u8; 46] = [
        0xBA, 0xF8, 0x0C, 0x66, 0xB8, 0x10, 0x80, 0x00, 0x80, 0x66, 0xEF, 0xBA, 0xFC, 0x0C, 0x66,
        0xB8, 0x00, 0x00, 0x00, 0xE2, 0x66, 0xEF, 0xBA, 0xF8, 0x0C, 0x66, 0xB8, 0x04, 0x80, 0x00,
        0x80, 0x66, 0xEF, 0xBA, 0xFC, 0x0C, 0x66, 0xB8, 0x02, 0x00, 0x00, 0x00, 0x66, 0xEF, 0xCD,
        0x20,
    ];
    let mut machine =
        Machine::new_dos_program(MachineProfile::gsw_386(16, VideoCard::Distira), &PROG).unwrap();

    let reason = machine.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });

    write_reg_at(&mut machine, ASSIGNED_BAR, DISTIRA_REG_FB_WIDTH, 2);
    write_reg_at(&mut machine, ASSIGNED_BAR, DISTIRA_REG_FB_HEIGHT, 1);
    write_reg_at(
        &mut machine,
        ASSIGNED_BAR,
        SST_LFB_MODE,
        LFB_FORMAT_ARGB8888 | LFB_WRITE_BACK,
    );
    machine.write_physical_u32(ASSIGNED_LFB, 0x0034_5678);
    write_reg_at(&mut machine, ASSIGNED_BAR, SST_SWAPBUFFER_CMD, 1);

    let (frame, width, height) = machine.frame_argb();
    assert_eq!((width, height), (2, 1));
    assert_eq!(frame, vec![0x0031_557b, 0x0000_0000]);
}

#[test]
fn distira_cmdfifo_aperture_drains_type1_register_packets() {
    const CMD_FIFO_BASE: u32 = DISTIRA_MMIO_BASE + 0x0020_0000;
    const SST_CMD_FIFO_DEPTH: usize = 0x1f4;
    const FBIINIT7_CMDFIFO_ENABLE: u32 = 1 << 8;

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Distira),
        I386DX25_TEST_ROM,
    )
    .unwrap();

    write_reg(&mut machine, DISTIRA_REG_FB_WIDTH, 2);
    write_reg(&mut machine, DISTIRA_REG_FB_HEIGHT, 1);
    write_reg(&mut machine, SST_FBI_INIT7, FBIINIT7_CMDFIFO_ENABLE);

    machine.write_physical_u32(CMD_FIFO_BASE, cmdfifo_type1_header(SST_CLIP_LEFT_RIGHT, 1));
    machine.write_physical_u32(CMD_FIFO_BASE + 4, 2);
    machine.write_physical_u32(
        CMD_FIFO_BASE + 8,
        cmdfifo_type1_header(SST_CLIP_LOW_Y_HIGH_Y, 1),
    );
    machine.write_physical_u32(CMD_FIFO_BASE + 12, 1);
    machine.write_physical_u32(CMD_FIFO_BASE + 16, cmdfifo_type1_header(SST_FBZ_MODE, 1));
    machine.write_physical_u32(CMD_FIFO_BASE + 20, FBZ_RGB_WMASK | FBZ_DRAW_BACK);
    machine.write_physical_u32(CMD_FIFO_BASE + 24, cmdfifo_type1_header(SST_COLOR1, 1));
    machine.write_physical_u32(CMD_FIFO_BASE + 28, 0x0034_5678);
    machine.write_physical_u32(
        CMD_FIFO_BASE + 32,
        cmdfifo_type1_header(SST_FASTFILL_CMD, 1),
    );
    machine.write_physical_u32(CMD_FIFO_BASE + 36, 1);
    machine.write_physical_u32(
        CMD_FIFO_BASE + 40,
        cmdfifo_type1_header(SST_SWAPBUFFER_CMD, 1),
    );
    machine.write_physical_u32(CMD_FIFO_BASE + 44, 1);

    assert_eq!(read_reg(&mut machine, SST_CMD_FIFO_DEPTH), 12);
    assert_ne!(read_reg(&mut machine, SST_STATUS) & 0x380, 0);

    machine.drain_distira_fifo();

    assert_eq!(read_reg(&mut machine, SST_CMD_FIFO_DEPTH), 0);
    assert_eq!(read_reg(&mut machine, SST_STATUS) & 0x380, 0);
    let (frame, width, height) = machine.frame_argb();
    assert_eq!((width, height), (2, 1));
    assert_eq!(frame, vec![0x0031_557b; 2]);
}

#[test]
fn distira_cmdfifo_type5_framebuffer_packet_writes_lfb() {
    const CMD_FIFO_BASE: u32 = DISTIRA_MMIO_BASE + 0x0020_0000;
    const SST_CMD_FIFO_DEPTH: usize = 0x1f4;
    const FBIINIT7_CMDFIFO_ENABLE: u32 = 1 << 8;

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Distira),
        I386DX25_TEST_ROM,
    )
    .unwrap();

    write_reg(&mut machine, DISTIRA_REG_FB_WIDTH, 2);
    write_reg(&mut machine, DISTIRA_REG_FB_HEIGHT, 1);
    write_reg(
        &mut machine,
        SST_LFB_MODE,
        LFB_FORMAT_ARGB8888 | LFB_WRITE_BACK,
    );
    write_reg(&mut machine, SST_FBI_INIT7, FBIINIT7_CMDFIFO_ENABLE);

    machine.write_physical_u32(CMD_FIFO_BASE, cmdfifo_type5_framebuffer_header(1));
    machine.write_physical_u32(CMD_FIFO_BASE + 4, 0);
    machine.write_physical_u32(CMD_FIFO_BASE + 8, 0x0034_5678);

    assert_eq!(read_reg(&mut machine, SST_CMD_FIFO_DEPTH), 3);

    machine.drain_distira_fifo();
    write_reg(&mut machine, SST_SWAPBUFFER_CMD, 1);

    assert_eq!(read_reg(&mut machine, SST_CMD_FIFO_DEPTH), 0);
    let (frame, width, height) = machine.frame_argb();
    assert_eq!((width, height), (2, 1));
    assert_eq!(frame, vec![0x0031_557b, 0x0000_0000]);
}
