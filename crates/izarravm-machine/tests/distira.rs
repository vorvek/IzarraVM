use izarravm_core::VideoCard;
use izarravm_firmware::I386DX25_TEST_ROM;
use izarravm_machine::{
    ActiveDisplay, DISTIRA_LFB_BASE, DISTIRA_MMIO_BASE, Machine, MachineProfile, StopReason,
};
use izarravm_video::{
    ALPHA_BLEND_ENABLE, ALPHA_DST_FUNC_SHIFT, ALPHA_SRC_FUNC_SHIFT, BLEND_AONE, BLEND_AZERO,
    DISTIRA_REG_FB_HEIGHT, DISTIRA_REG_FB_WIDTH, FBZ_DRAW_BACK, FBZ_RGB_WMASK,
    LFB_ENABLE_PIXEL_PIPELINE, LFB_FORMAT_ARGB8888, LFB_FORMAT_RGB565, LFB_WRITE_BACK,
    SST_ALPHA_MODE, SST_CLIP_LEFT_RIGHT, SST_CLIP_LOW_Y_HIGH_Y, SST_COLOR1, SST_FASTFILL_CMD,
    SST_FBI_INIT7, SST_FBZ_MODE, SST_LFB_MODE, SST_STATUS, SST_SWAPBUFFER_CMD,
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

#[test]
fn distira_mmio_and_lfb_are_wired_into_machine_scanout() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Distira),
        I386DX25_TEST_ROM,
    )
    .unwrap();

    assert_eq!(read_reg(&mut machine, SST_STATUS) & 0x380, 0);
    machine.write_physical_u8(DISTIRA_LFB_BASE, 0x34);
    assert_eq!(machine.read_physical_u8(DISTIRA_LFB_BASE), 0x34);

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
