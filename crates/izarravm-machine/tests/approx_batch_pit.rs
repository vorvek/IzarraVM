//! Pins the Approximate-class batch cap's guest-clock contract: a bus-heavy
//! stretch (a framebuffer blit) must still deliver every PIT channel-0 edge as
//! its own IRQ0. Before the cap counted in-batch scaled bus clocks, a blit's
//! bus time was invisible to the core-only cap, batches overshot the next OUT
//! edge by the bus:core ratio, and the PIC's IRR coalesced the missed edges -
//! a guest timer ISR lost most of its ticks (realtics froze while guest time
//! grew). Real hardware interrupts at every edge at any realistic rate.

use izarravm_core::{GswMode, VideoCard};
use izarravm_machine::{Machine, MachineProfile, StopReason};

/// COM image, org 0x100: hook IVT[8] to a counting handler (counter word in
/// the BIOS inter-application area 0040:00F0), reprogram PIT channel 0 to
/// mode 2 divisor 1193 (~1 kHz), set mode 13h, then REP STOSB 20 * 64 KiB
/// into the 0xA000 aperture and exit. At the 586's calibrated video cost the
/// blit spans ~100 ms of guest time = ~100 PIT edges.
#[rustfmt::skip]
const BLIT_TICKS_COM: &[u8] = &[
    0xfa,                                     // cli
    0x31, 0xc0,                               // xor ax, ax
    0x8e, 0xc0,                               // mov es, ax
    0x26, 0xc7, 0x06, 0x20, 0x00, 0x40, 0x01, // mov word [es:0x20], 0x0140
    0x26, 0x8c, 0x0e, 0x22, 0x00,             // mov [es:0x22], cs
    0xb0, 0x34,                               // mov al, 0x34
    0xe6, 0x43,                               // out 0x43, al
    0xb0, 0xa9,                               // mov al, 0xa9  (1193 lo)
    0xe6, 0x40,                               // out 0x40, al
    0xb0, 0x04,                               // mov al, 0x04  (1193 hi)
    0xe6, 0x40,                               // out 0x40, al
    0xb0, 0xfc,                               // mov al, 0xfc (unmask IRQ0+IRQ1;
    0xe6, 0x21,                               // out 0x21, al  raw-program masks IRQ0)
    0xfb,                                     // sti
    0xb8, 0x13, 0x00,                         // mov ax, 0x0013 (mode set; harmless
    0xcd, 0x10,                               // int 0x10        if HLE declines it)
    0xb8, 0x00, 0xb8,                         // mov ax, 0xb800 (text window: a device
    0x8e, 0xc0,                               // mov es, ax      window in every mode)
    0xba, 0xa0, 0x00,                         // mov dx, 160
    // outer: each rep is 8 KiB ~ 0.6 ms guest, under one 1 kHz PIT period, so
    // edge delivery is pinned by the batch cap, not by REP's atomicity (our
    // REP is uninterruptible per instruction, a separate recorded infidelity;
    // real REP is interruptible between elements).
    0x31, 0xff,                               // xor di, di
    0xb9, 0x00, 0x20,                         // mov cx, 0x2000
    0xb0, 0x5a,                               // mov al, 0x5a
    0xf3, 0xaa,                               // rep stosb
    0x4a,                                     // dec dx
    0x75, 0xf4,                               // jnz outer
    0xb8, 0x00, 0x4c,                         // mov ax, 0x4c00
    0xcd, 0x21,                               // int 0x21
    // handler (0x0140):
    0x50,                                     // push ax
    0x1e,                                     // push ds
    0xb8, 0x40, 0x00,                         // mov ax, 0x0040
    0x8e, 0xd8,                               // mov ds, ax
    0xff, 0x06, 0xf0, 0x00,                   // inc word [0x00f0]
    0xb0, 0x20,                               // mov al, 0x20
    0xe6, 0x20,                               // out 0x20, al
    0x1f,                                     // pop ds
    0x58,                                     // pop ax
    0xcf,                                     // iret
];

#[test]
fn vram_blit_delivers_every_pit_edge_at_586() {
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    profile.cpu = GswMode::Gsw586;
    let mut machine = Machine::new_raw_program(profile, BLIT_TICKS_COM).unwrap();
    let before = machine.elapsed_clocks();
    let reason = machine.run_until_halt_or_cycles(2_000_000_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    let guest_ms = (machine.elapsed_clocks() - before) as f64 / 200_000.0;
    let ticks = u32::from(machine.read_physical_u8(0x4f0))
        | (u32::from(machine.read_physical_u8(0x4f1)) << 8);
    // 1,310,700 VRAM byte writes at the calibrated ~75 ns each is ~100 ms of
    // guest time = ~100 one-kHz edges. The core-only cap regression collapses
    // this to a handful (batches span tens of periods and the IRR coalesces),
    // so a generous lower bound is a sharp discriminator; the upper bound only
    // guards against runaway edge storms.
    assert!(
        (50..=300).contains(&ticks),
        "expected ~100 PIT ticks across the blit, got {ticks} (guest {guest_ms:.1} ms)"
    );
}
