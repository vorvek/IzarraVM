// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn deliver_exception_from_v86_builds_the_v86_frame_on_ring0_stack() {
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);
    cpu.load_segment_real(SegmentIndex::Ds, 0x1111);
    cpu.load_segment_real(SegmentIndex::Es, 0x2222);
    cpu.load_segment_real(SegmentIndex::Fs, 0x3333);
    cpu.load_segment_real(SegmentIndex::Gs, 0x4444);
    let saved_eflags = cpu.registers.eflags;

    cpu.deliver_exception(&mut bus, 13, Some(0), false).unwrap();

    assert!(!cpu.is_v86_mode(), "VM must be cleared on monitor entry");
    assert_eq!(cpu.registers.cs().selector, R0_CS);
    assert_eq!(cpu.registers.eip, MON_CODE);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, R0_SS);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).selector, 0);
    assert_eq!(cpu.registers.segment(SegmentIndex::Gs).selector, 0);
    let esp = cpu.registers.esp();
    let rd = |o: u32| u32::from_le_bytes(cpu_mem(&bus, esp + o));
    // From the handler's ESP upward: [err], EIP, CS, EFLAGS, ESP, SS, ES, DS, FS, GS.
    assert_eq!(rd(0), 0, "error code");
    assert_eq!(rd(4), 0x10, "V86 EIP");
    assert_eq!(rd(8) & 0xffff, 0x0A00, "V86 CS");
    assert_eq!(rd(12) & FLAG_VM, FLAG_VM, "pushed EFLAGS carries VM=1");
    assert_eq!(rd(12), saved_eflags, "pushed EFLAGS is the pre-clear image");
    assert_eq!(rd(16), 0x1000, "V86 ESP");
    assert_eq!(rd(20) & 0xffff, 0x0900, "V86 SS");
    assert_eq!(rd(24) & 0xffff, 0x2222, "V86 ES");
    assert_eq!(rd(28) & 0xffff, 0x1111, "V86 DS");
    assert_eq!(rd(32) & 0xffff, 0x3333, "V86 FS");
    assert_eq!(rd(36) & 0xffff, 0x4444, "V86 GS");
}

#[test]
fn deliver_exception_onto_a_16bit_ring0_stack_wraps_sp_and_preserves_high_esp() {
    // The exact fault scenario this task fixes: a 32-bit interrupt gate delivers
    // onto a ring-0 stack whose SS descriptor has B=0 (a 16-bit stack segment,
    // as DOS4GW/VCPI clients use). 386 PRM 17-43/17-74: "Load new SS:eSP value
    // from TSS" is B-keyed (17-12) -- a B=0 target stack takes the TSS value
    // into SP only, and ESP's high word carries over from the interrupted
    // context untouched. The V86 interrupt frame (10 dwords) is then built at
    // SP-wrapped addresses, only SP advancing.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    // Flip the ring-0 data descriptor's B bit off (byte 6 bit 6 = 0x40). Give
    // TSS ESP0 a nonzero high word (0x0001) to prove it is dropped (SP-only
    // load), and enter V86 with ESP high word 0 so a leftover-high-word bug
    // would be visible in the final ESP.
    bus.memory[(GDT + 0x10 + 6) as usize] &= !0x40;
    put32(&mut bus.memory, TSS + 4, 0x0001_0010);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);

    cpu.deliver_exception(&mut bus, 13, Some(0), false).unwrap();

    assert!(!cpu.stack_is_32bit(), "the loaded SS0 must carry B=0");
    assert!(!cpu.is_v86_mode(), "VM must be cleared on monitor entry");
    assert_eq!(cpu.registers.cs().selector, R0_CS);
    assert_eq!(cpu.registers.eip, MON_CODE);
    assert_eq!(
        cpu.registers.esp(),
        0x0000_ffe8,
        "SP takes only the TSS's low 16 bits, then wraps at the 16-bit \
             boundary; the interrupted context's ESP high word (0) carries over, \
             not the TSS's high word (0x0001)"
    );
    // The frame lives at SS0.base (0) + the wrapped 16-bit SP (0xffe8).
    let rd = |o: u32| u32::from_le_bytes(cpu_mem(&bus, 0xffe8 + o));
    assert_eq!(rd(0), 0, "error code");
    assert_eq!(rd(4), 0x10, "V86 EIP");
    assert_eq!(rd(16), 0x1000, "V86 ESP");
}

#[test]
fn deliver_exception_onto_a_16bit_ring0_stack_preserves_interrupted_esp_high_word() {
    // Companion to the case above: this time the interrupted V86 context's ESP
    // has a nonzero high word, proving it survives the SP-only TSS load (rather
    // than being replaced by the TSS's, or zeroed).
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    bus.memory[(GDT + 0x10 + 6) as usize] &= !0x40;
    put32(&mut bus.memory, TSS + 4, 0x0000_0010);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);
    cpu.registers.set_esp(0xbeef_1000);

    cpu.deliver_exception(&mut bus, 13, Some(0), false).unwrap();

    assert!(!cpu.stack_is_32bit(), "the loaded SS0 must carry B=0");
    assert_eq!(
        cpu.registers.esp(),
        0xbeef_ffe8,
        "the interrupted context's ESP high word (0xbeef) must carry over \
             onto the new B=0 stack, with SP taken from the TSS and then wrapped"
    );
}

#[test]
fn v86_external_interrupt_on_vector_8_pushes_no_error_code() {
    // A real DOS boot under a V86 monitor keeps the PIC at base 0x08, so IRQ0
    // lands on vector 8 (#DF). An EXTERNAL interrupt must NOT push an error code
    // even there — only a genuine CPU exception does. (is_external = true.)
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    int_gate(&mut bus.memory, 8, MON_CODE);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);

    cpu.deliver_exception(&mut bus, 8, None, true).unwrap();

    // In the monitor: the top of the ring-0 stack is the V86 EIP, not an error
    // code (the frame is EIP, CS, EFLAGS, ... with no error code beneath EIP).
    let esp = cpu.registers.esp();
    assert_eq!(cpu.registers.eip, MON_CODE);
    assert_eq!(
        u32::from_le_bytes(cpu_mem(&bus, esp)),
        0x10,
        "external interrupt on vector 8 must not push an error code"
    );
}

#[test]
fn iret_into_v86_restores_the_task() {
    // Monitor at CPL0 with a V86 return frame on its stack; IRET must re-enter V86.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x6800);
    // Build the 32-bit V86 IRET frame (push high-to-low): GS,FS,DS,ES,SS,ESP,EFLAGS,CS,EIP.
    let vm_eflags = FLAG_VM | 0x2;
    for v in [
        0x4444u32, 0x3333, 0x1111, 0x2222, 0x0900, 0x1000, vm_eflags, 0x0A00, 0x0010,
    ] {
        cpu.push(&mut bus, v, OperandSize::Dword).unwrap();
    }

    cpu.iret(&mut bus, OperandSize::Dword).unwrap();

    assert!(cpu.is_v86_mode(), "IRET with popped VM=1 must re-enter V86");
    assert_eq!(cpu.registers.eip, 0x0010);
    assert_eq!(cpu.registers.cs().selector, 0x0A00);
    assert_eq!(
        cpu.registers.cs().base,
        0x0A00 << 4,
        "real-mode base=sel<<4"
    );
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, 0x0900);
    assert_eq!(cpu.registers.esp(), 0x1000);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).selector, 0x1111);
    assert_eq!(cpu.registers.segment(SegmentIndex::Es).selector, 0x2222);
    assert_eq!(cpu.registers.segment(SegmentIndex::Fs).selector, 0x3333);
    assert_eq!(cpu.registers.segment(SegmentIndex::Gs).selector, 0x4444);
    assert_eq!(cpu.current_privilege_level(), 3, "V86 is always CPL 3");
}

#[test]
fn iret_into_v86_with_dirty_high_word_eip_faults_before_committing_v86_state() {
    // Same 32-bit V86 IRET frame as `iret_into_v86_restores_the_task`, but the popped
    // EIP carries a nonzero high word (0x0001_0000). 386 PRM STACK-RETURN-TO-V86 checks
    // "instruction pointer not within code segment limit" against the popped EIP and
    // raises #GP(0) *before* EFLAGS/CS/EIP/ESP or the V86 data segments are committed --
    // ahead of every `Pop()` in the pseudocode's V86-tail sequence. A V86 CS is always a
    // 16-bit real-mode-style segment (fixed 0xffff limit), so this EIP is always out of
    // range: `iret` must return the fault directly, leaving the ring-0 monitor's own
    // CS/EIP/segments untouched (as if the IRET itself never executed), not commit a
    // fabricated V86 frame and only discover the violation on the next fetch.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x6800);
    let vm_eflags = FLAG_VM | 0x2;
    for v in [
        0x4444u32,
        0x3333,
        0x1111,
        0x2222,
        0x0900,
        0x1000,
        vm_eflags,
        0x0A00,
        0x0001_0000,
    ] {
        cpu.push(&mut bus, v, OperandSize::Dword).unwrap();
    }

    let result = cpu.iret(&mut bus, OperandSize::Dword);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(0),
            })
        ),
        "out-of-limit popped EIP must fault #GP(0) directly from iret: {result:?}"
    );
    assert!(
        !cpu.is_v86_mode(),
        "a faulted IRET must not have entered V86"
    );
    assert_eq!(
        cpu.registers.cs().selector,
        R0_CS,
        "the monitor's own CS must be untouched by the faulted IRET"
    );
    // 9 dwords were pushed to build the frame; the faulted IRET must restore ESP to
    // that pre-IRET value (finish_instruction rewinds only EIP/CS, so iret itself
    // must undo its three pops or the monitor's stack drifts 12 bytes per fault).
    assert_eq!(
        cpu.registers.esp(),
        0x6800 - 9 * 4,
        "a faulted IRET must leave ESP exactly pre-IRET"
    );
}

#[test]
fn iret_inter_privilege_return_to_ring3() {
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    // Ring-3 code (access 0xfb) + data (0xf3) at GDT slots 0x20 / 0x28.
    let r3_code = descriptor(0, 0xfffff, 0xfb, 0xc0);
    let r3_data = descriptor(0, 0xfffff, 0xf3, 0xc0);
    bus.memory[(GDT + 0x20) as usize..(GDT + 0x20) as usize + 8].copy_from_slice(&r3_code);
    bus.memory[(GDT + 0x28) as usize..(GDT + 0x28) as usize + 8].copy_from_slice(&r3_data);
    let r3_cs = 0x23u16; // 0x20 | RPL3
    let r3_ss = 0x2Bu16; // 0x28 | RPL3
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x6800);
    // Inter-privilege IRET frame (high-to-low): SS, ESP, EFLAGS, CS, EIP.
    for v in [u32::from(r3_ss), 0x2000, 0x2, u32::from(r3_cs), 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Dword).unwrap();
    }
    cpu.iret(&mut bus, OperandSize::Dword).unwrap();
    assert_eq!(cpu.current_privilege_level(), 3, "returned to ring 3");
    assert!(!cpu.is_v86_mode());
    assert_eq!(cpu.registers.eip, 0x1234);
    assert_eq!(cpu.registers.cs().selector, r3_cs);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, r3_ss);
    assert_eq!(cpu.registers.esp(), 0x2000);
}

#[test]
fn iret_to_outer_ring_nulls_data_segments_inaccessible_at_the_new_cpl() {
    // 386 PRM (IRET, return to outer privilege level): each of DS/ES/FS/GS
    // holding a data or non-conforming code segment with DPL < new CPL is
    // loaded with the null selector. Borland's DPMI32VM relies on this: its
    // ring-0 trap handler IRETDs to ring 3 with DS still holding the ring-0
    // data selector; ring-3 code then PUSH/POPs DS, which only works if the
    // return nulled it (popping a DPL-0 selector at CPL 3 is #GP).
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    let r3_code = descriptor(0, 0xfffff, 0xfb, 0xc0);
    let r3_data = descriptor(0, 0xfffff, 0xf3, 0xc0);
    // Conforming ring-0 code (access 0x9f): readable at any CPL, must survive.
    let r0_conforming = descriptor(0, 0xfffff, 0x9f, 0xc0);
    bus.memory[(GDT + 0x20) as usize..(GDT + 0x20) as usize + 8].copy_from_slice(&r3_code);
    bus.memory[(GDT + 0x28) as usize..(GDT + 0x28) as usize + 8].copy_from_slice(&r3_data);
    bus.memory[(GDT + 0x30) as usize..(GDT + 0x30) as usize + 8].copy_from_slice(&r0_conforming);
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ds, R0_SS).unwrap(); // ring-0 data
    cpu.load_segment(&mut bus, SegmentIndex::Es, 0x2B).unwrap(); // ring-3 data
    cpu.load_segment(&mut bus, SegmentIndex::Fs, R0_SS).unwrap(); // ring-0 data
    cpu.load_segment(&mut bus, SegmentIndex::Gs, 0x33).unwrap(); // conforming r0 code
    cpu.registers.set_esp(0x6800);
    for v in [0x2Bu32, 0x2000, 0x2, 0x23, 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Dword).unwrap();
    }
    cpu.iret(&mut bus, OperandSize::Dword).unwrap();
    assert_eq!(cpu.current_privilege_level(), 3);
    let sel = |cpu: &CpuGsw, s| cpu.registers.segment(s).selector;
    assert_eq!(sel(&cpu, SegmentIndex::Ds), 0, "ring-0 DS nulled");
    assert_eq!(sel(&cpu, SegmentIndex::Fs), 0, "ring-0 FS nulled");
    assert_eq!(sel(&cpu, SegmentIndex::Es), 0x2B, "ring-3 ES survives");
    assert_eq!(
        sel(&cpu, SegmentIndex::Gs),
        0x33,
        "conforming code GS survives"
    );
}

#[test]
fn iret_inter_privilege_return_to_a_16bit_stack_wraps_sp_and_preserves_high_esp() {
    // 386 PRM 17-80: "Load SS:eSP from stack" is B-keyed (17-12). Returning to
    // an outer ring whose SS descriptor has B=0 (the DPMI/DOS-extender 16-bit
    // stack shape) must take the popped value into SP only, wrap at the
    // 16-bit boundary, and leave ESP's high word as the inner stack's --
    // exactly the documented real-silicon ESP-high-word leak on a 16-bit ring
    // transition.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    // Ring-3 code (access 0xfb) + a B=0 (16-bit) ring-3 data descriptor (0xf3,
    // flags byte with the B bit, 0x40, cleared) at GDT slots 0x20 / 0x28.
    let r3_code = descriptor(0, 0xfffff, 0xfb, 0xc0);
    let r3_data = descriptor(0, 0xffff, 0xf3, 0x00);
    bus.memory[(GDT + 0x20) as usize..(GDT + 0x20) as usize + 8].copy_from_slice(&r3_code);
    bus.memory[(GDT + 0x28) as usize..(GDT + 0x28) as usize + 8].copy_from_slice(&r3_data);
    let r3_cs = 0x23u16; // 0x20 | RPL3
    let r3_ss = 0x2Bu16; // 0x28 | RPL3
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    // Low half of ESP (0x6800) is the address `push` actually uses (the
    // inner stack is B=1, so it addresses with full ESP); the high half
    // (0x0001) must not leak onto the B=0 outer stack after IRET, and the
    // physical address stays within the test's identity-mapped 0x20000
    // bytes (0x0001_6800 < 0x20000).
    cpu.registers.set_esp(0x0001_6800);
    // Popped ESP has a different nonzero high word (0x0002); a B=0 target
    // stack must drop it (SP-only load), not adopt it.
    for v in [u32::from(r3_ss), 0x0002_0010, 0x2, u32::from(r3_cs), 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Dword).unwrap();
    }
    cpu.iret(&mut bus, OperandSize::Dword).unwrap();
    assert_eq!(cpu.current_privilege_level(), 3, "returned to ring 3");
    assert!(!cpu.stack_is_32bit(), "the loaded outer SS must carry B=0");
    assert_eq!(cpu.registers.eip, 0x1234);
    assert_eq!(cpu.registers.cs().selector, r3_cs);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, r3_ss);
    assert_eq!(
        cpu.registers.esp(),
        0x0001_0010,
        "SP takes the popped value's low 16 bits; ESP's high word carries \
             over from the inner stack (0x0001), not the popped high word \
             (0x0002)"
    );
}

#[test]
fn v86_out_consults_the_io_permission_bitmap() {
    // Guest at 0x0A00:0 does `OUT 0x21, AL` (E6 21). Bitmap traps port 0x21.
    let mut bitmap = vec![0u8; 0x20 + 1]; // ports 0..0x100 + terminator byte
    bitmap[0x21 / 8] |= 1 << (0x21 % 8);
    let guest = [0xe6, 0x21, 0xf4]; // out 0x21, al ; hlt
    let (mut cpu, mut bus) = v86_world(&[0xf4], &guest, &bitmap);
    enter_v86_direct(&mut cpu, 0, 0x1000);

    let outcome = cpu.cycle(&mut bus);
    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(
        !cpu.is_v86_mode(),
        "trapped OUT must land in the ring-0 monitor"
    );
    assert_eq!(cpu.registers.cs().selector, R0_CS);
}

#[test]
fn v86_out_to_a_permitted_port_runs_the_io() {
    let bitmap = vec![0u8; 0x20 + 1]; // all-zero: everything permitted
    let guest = [0xe6, 0x21, 0xf4]; // out 0x21, al ; hlt
    let (mut cpu, mut bus) = v86_world(&[0xf4], &guest, &bitmap);
    enter_v86_direct(&mut cpu, 0, 0x1000);

    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.is_v86_mode(), "permitted OUT stays in V86");
    assert!(
        bus.trace
            .cycles()
            .iter()
            .any(|c| c.kind == BusAccessKind::IoWrite && c.address == 0x21),
        "permitted OUT should reach the I/O bus"
    );
}

#[test]
fn v86_monitor_round_trip_go_no_go() {
    // Guest: STI (fb) ; OUT 0x80,AL (e6 80) ; INT 0x21 (cd 21) ; HLT (f4).
    // HLT is now privileged (require_cpl0): a V86 task is always CPL 3, so the
    // guest's HLT traps into the monitor exactly like STI and INT 0x21 rather
    // than halting the machine directly. The monitor emulates it by advancing
    // past the F4 byte and halting for real at ring 0 (mirroring TOKAEMM's
    // `.hlt` handler in tokaemm.asm): that real HLT is what stops the machine,
    // observed here as `outcome.halted` while CS is still the ring-0 monitor
    // selector (not while `cpu.is_v86_mode()`, since the guest itself never
    // executes HLT to completion anymore).
    let guest = [0xfb, 0xe6, 0x80, 0xcd, 0x21, 0xf4];
    let monitor = [0xf4]; // unused: we emulate the monitor from Rust below.
    let bitmap = vec![0u8; 0x20 + 1]; // all-zero: ports 0..0x100 permitted (+ terminator byte)
    let (mut cpu, mut bus) = v86_world(&monitor, &guest, &bitmap);
    enter_v86_direct(&mut cpu, 0, 0x1000);

    let mut traps = 0;
    let mut monitor_halted = false;
    for _ in 0..64 {
        let outcome = cpu.cycle(&mut bus).unwrap();
        if !cpu.is_v86_mode() && cpu.registers.cs().selector == R0_CS {
            if outcome.halted {
                // The monitor's HLT-emulation path ran its own real HLT at ring 0
                // to idle the machine on the guest's behalf; nothing left to IRET.
                monitor_halted = true;
                break;
            }
            // In the monitor because the guest faulted. Read the V86 #GP(13) frame,
            // advance the guest EIP past the faulting instruction, IRET back to V86.
            // STI, INT 0x21, and now HLT all arrive here as #GP(13): each is either
            // IOPL-sensitive (check_v86_iopl) or CPL-sensitive (require_cpl0) and a
            // V86 task always runs at IOPL 0 / CPL 3. INT 0x21 does NOT dispatch
            // through its own IDT gate (it is intercepted before delivery), so every
            // trap in this test is vector 13.
            traps += 1;
            // Discard the error code (vector 13 pushes one) so IRET pops from EIP.
            // Frame layout from the handler's ESP upward is [err], EIP, CS, ... (see the
            // sibling deliver_exception test); after skipping the 4-byte error code the
            // V86 EIP is at the top of stack, so cpu_mem(&bus, esp) reads it directly.
            let esp = cpu.registers.esp() + 4;
            cpu.registers.set_esp(esp);
            let guest_eip = u32::from_le_bytes(cpu_mem(&bus, esp));
            // The guest is loaded at phys 0xA000 == V86 CS(0x0A00) << 4, so guest_eip
            // (a segment offset) indexes the guest code bytes directly. This literal
            // tracks v86_world's guest load base and enter_v86_direct's V86 CS.
            let opcode = bus.memory[(0xA000 + guest_eip) as usize];
            let len = match opcode {
                0xfb => 1, // STI
                0xcd => 2, // INT imm8
                0xf4 => {
                    // HLT: the guest's virtual IF is set (STI already ran), so a
                    // faithful monitor would really halt here on the guest's behalf
                    // (tokaemm.asm's `.hlt` runs `sti; hlt` at ring 0) rather than
                    // resuming V86. This Rust stand-in for the monitor halts the CPU
                    // directly instead of round-tripping through an IRET into V86
                    // followed immediately by a real HLT trap: same observable
                    // result (the machine halts with CS still the monitor selector),
                    // fewer moving parts in the harness.
                    cpu.halted = true;
                    continue;
                }
                other => {
                    panic!("unexpected trap on opcode {other:#x} at guest eip {guest_eip:#x}")
                }
            };
            bus.memory[esp as usize..esp as usize + 4]
                .copy_from_slice(&(guest_eip + len).to_le_bytes());
            cpu.iret(&mut bus, OperandSize::Dword).unwrap();
            continue;
        }
    }

    assert!(
        monitor_halted,
        "the monitor never halted on the guest's HLT"
    );
    assert_eq!(traps, 3, "STI, INT 0x21, and HLT must each trap once");
    assert!(
        bus.trace
            .cycles()
            .iter()
            .any(|c| c.kind == BusAccessKind::IoWrite && c.address == 0x80),
        "permitted OUT 0x80 should have run in V86"
    );
}

// ---- Non-identity-mapped system structures (translate_linear_system) ----------
//
// `v86_world`'s page tables are identity-mapped, so TSS/GDT/IDT linear == physical
// there and every system-structure read would look correct even with the raw,
// unpaged `bus.read_memory` these tests were written to catch (the JEMMEX bug:
// its monitor sits at a high linear alias -- e.g. 0xf8017000 -- of low physical
// RAM). These tests add a *second* PDE mapping a high linear window onto the same
// physical TSS/GDT page, then address the TSS/GDT only through that alias, so a
// regression back to raw `bus.read_memory(self.tr.base + ..)` reads unmapped
// physical memory (or, in TestBus, the wrong bytes) instead of the real fields.

/// Linear window aliasing the TSS's physical page one PDE slot up (JEMMEX-style
/// high monitor mapping): PDE[1] -> the same page table as PDE[0], so linear
/// 0x00400000 + phys(0..0x1000) reads/writes the identical physical bytes as the
/// identity mapping at phys directly.
const ALIAS_BASE: u32 = 0x0040_0000;

/// Extend `v86_world`'s page directory with a second PDE (index 1) pointing at
/// the same page table as PDE[0], then move the TSS to be addressed only through
/// the alias: `cpu.tr.base` and the TSS GDT descriptor's base are both set to
/// `ALIAS_BASE + TSS`, while the bytes still live at physical `TSS`. A test that
/// reads/writes the TSS via a raw, unpaged `bus.read_memory(self.tr.base + ..)`
/// would touch physical `ALIAS_BASE + TSS` (zeroed, wrong data) instead of the
/// real TSS at physical `TSS`.
fn alias_tss_through_second_pde(bus: &mut TestBus, cpu: &mut CpuGsw) {
    // PDE[1] (linear 0x0040_0000..0x0080_0000) -> the same PT as PDE[0].
    put32(&mut bus.memory, 0x1000 + 4, 0x2000 | 0x7);
    let tss_limit = cpu.tr.limit;
    cpu.tr.base = ALIAS_BASE + TSS;
    // Repoint the TSS GDT descriptor's base at the alias too, so LTR-style
    // re-reads and `set_tss_busy`'s GDT access-byte patch land on the alias.
    let d = descriptor(ALIAS_BASE + TSS, tss_limit, 0x89, 0x00);
    bus.memory[(GDT + 0x18) as usize..(GDT + 0x18) as usize + 8].copy_from_slice(&d);
}

#[test]
fn deliver_exception_from_v86_with_cs_rpl3_does_not_fault_the_monitors_own_pushes() {
    // Dossier reproduction: a V86 source whose CS selector carries RPL bits == 3
    // (the DOS HMA stub lives at 0xFFFF, reached via an XMS chain-through) must not
    // make `deliver_exception`'s own ring-0 stack pushes look like a user access.
    // Before the fix, `current_privilege_level` derived "user" live from
    // `CS.selector & 3` -- read at the moment of the push, i.e. still the V86
    // source's arbitrary CS -- so a supervisor-only ESP0 page spuriously #PF'd on
    // the monitor's own frame-push, with CR2 landing on the stack pointer itself.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    // The frame's pushes land BELOW ESP0 (0x7000), on page 6 (0x6000..0x6FFF), not
    // ESP0's own page: make that page supervisor-only (present+rw, U/S=0, dropping
    // the 0x4 user bit `v86_world` sets by default).
    put32(&mut bus.memory, 0x2000 + 6 * 4, 0x6000 | 0x3);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);
    // Load a V86 CS whose low bits are 3 -- a real-mode-style segment, so this is
    // legal V86 state, just an unusual selector value (0xFFFF, the HMA stub).
    cpu.load_segment_real(SegmentIndex::Cs, 0xffff);

    let result = cpu.deliver_exception(&mut bus, 13, Some(0), false);

    assert!(
        result.is_ok(),
        "the monitor's own supervisor-stack pushes must not spuriously #PF: {result:?}"
    );
    assert!(!cpu.is_v86_mode(), "VM must be cleared on monitor entry");
    assert_eq!(cpu.registers.cs().selector, R0_CS);
    assert_eq!(cpu.registers.eip, MON_CODE);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ss).selector,
        R0_SS,
        "entry crossed to the ring-0 stack despite the V86 source CS's RPL bits"
    );
}

#[test]
fn nested_fault_during_delivery_reports_truthfully_not_as_idt_limit() {
    // Companion to the reproduction above: when delivery genuinely nests a fault
    // (here, ESP0's page is marked NOT PRESENT, so the frame push itself raises
    // #PF), `cycle`'s error mapping must surface `NestedFaultDuringDelivery` with
    // both vectors, not relabel it as a fabricated `IdtLimit` on the ORIGINAL vector
    // (the pre-fix behavior, which discarded the nested vector entirely).
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    // The frame's pushes land on page 6 (0x6000..0x6FFF), just below ESP0: clear
    // that page's present bit entirely.
    put32(&mut bus.memory, 0x2000 + 6 * 4, 0x6000 | 0x6);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);

    let outer = cpu.deliver_exception(&mut bus, 13, Some(0), false);
    let inner_fault = outer.expect_err("the not-present ESP0 push must nest a fault");
    let InternalFault::Exception {
        vector: nested_vector,
        ..
    } = inner_fault
    else {
        panic!("expected a nested processor exception, got {inner_fault:?}");
    };
    assert_eq!(nested_vector, 14, "the nested fault is the write's own #PF");

    // Drive the same scenario through `cycle`'s public error mapping (the call site
    // this bug actually lived in) by raising vector 13 as the guest's own delivered
    // exception via a HLT that is not privileged in V86 IOPL<3 -- reuse
    // deliver_exception directly through the same `finish_instruction` tail instead,
    // since that IS the call site under test (see `finish_instruction`).
    let (mut cpu2, mut bus2) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    put32(&mut bus2.memory, 0x2000 + 6 * 4, 0x6000 | 0x6);
    enter_v86_direct(&mut cpu2, 0x10, 0x1000);
    let start_eip = cpu2.registers.eip;
    let start_cs = cpu2.registers.cs().selector;
    let result: Result<CycleOutcome, CpuError> = cpu2.finish_instruction(
        &mut bus2,
        Err(InternalFault::Exception {
            vector: 13,
            error_code: Some(0),
        }),
        start_eip,
        start_cs,
        0,
        None,
        None,
    );
    assert_eq!(
        result,
        Err(CpuError::NestedFaultDuringDelivery {
            original_vector: 13,
            nested_vector: 14,
        }),
        "{result:?}"
    );
}

#[test]
fn deliver_exception_from_v86_reads_esp0_ss0_through_a_non_identity_tss_mapping() {
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    alias_tss_through_second_pde(&mut bus, &mut cpu);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);

    cpu.deliver_exception(&mut bus, 13, Some(0), false).unwrap();

    // ESP0/SS0 came from the TSS at its aliased linear address, not from
    // unmapped physical memory at ALIAS_BASE + TSS + 4/+8 (which is zeroed).
    assert!(!cpu.is_v86_mode(), "VM must be cleared on monitor entry");
    assert_eq!(cpu.registers.cs().selector, R0_CS);
    assert_eq!(cpu.registers.eip, MON_CODE);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ss).selector,
        R0_SS,
        "SS0 must come from the TSS through the paged (aliased) address"
    );
    // ESP0 from the TSS, minus the 10-dword V86 interrupt frame (err code, EIP,
    // CS, EFLAGS, ESP, SS, ES, DS, FS, GS) pushed onto the new stack.
    assert_eq!(
        cpu.registers.esp(),
        ESP0 - 40,
        "ESP0 must come from the TSS through the paged (aliased) address"
    );
}

#[test]
fn ltr_loads_a_gdt_tss_descriptor_through_a_non_identity_mapping() {
    // Put the GDT itself behind the alias: GDT descriptors are read via
    // `read_gdt_descriptor` -> `read_system_linear_u32`, so aliasing the GDT's
    // page (not just the TSS's) exercises that path directly.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    // PDE[1] -> the same PT as PDE[0] (GDT/TSS both live in the identity-mapped
    // low pages, so one alias PDE covers both).
    put32(&mut bus.memory, 0x1000 + 4, 0x2000 | 0x7);
    cpu.gdtr.base = ALIAS_BASE + GDT;
    cpu.registers.eflags = 0x2; // ring 0, no VM/IOPL surprises

    cpu.load_tr(&mut bus, TSS_SEL).unwrap();

    assert_eq!(cpu.tr.selector, TSS_SEL);
    assert_eq!(
        cpu.tr.base, TSS,
        "LTR must decode the TSS descriptor's base field from the aliased GDT"
    );
    assert_eq!(
        cpu.tr.access & 0x02,
        0x02,
        "LTR must mark the TSS busy in the cached descriptor"
    );
    // The busy bit patch-back must land on the real (aliased) GDT byte, not on
    // unmapped physical memory.
    let access_byte = bus.memory[(GDT + 0x18 + 5) as usize];
    assert_eq!(
        access_byte & 0x02,
        0x02,
        "GDT busy bit must be set in place"
    );
}

#[test]
fn v86_io_bitmap_check_reads_through_a_non_identity_mapped_tss() {
    // Bitmap traps port 0x21, but the TSS (and its I/O-map base word / bitmap
    // bytes) is only reachable through the ALIAS_BASE linear window. A raw,
    // unpaged read of `self.tr.base + 0x66` would read zeroed physical memory
    // at ALIAS_BASE + TSS + 0x66 and see io_base = 0 with an all-zero bitmap,
    // wrongly permitting the OUT.
    let mut bitmap = vec![0u8; 0x20 + 1];
    bitmap[0x21 / 8] |= 1 << (0x21 % 8);
    let guest = [0xe6, 0x21, 0xf4]; // out 0x21, al ; hlt
    let (mut cpu, mut bus) = v86_world(&[0xf4], &guest, &bitmap);
    alias_tss_through_second_pde(&mut bus, &mut cpu);
    enter_v86_direct(&mut cpu, 0, 0x1000);

    let outcome = cpu.cycle(&mut bus);

    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(
        !cpu.is_v86_mode(),
        "the I/O-bitmap trap must be read through the aliased TSS mapping"
    );
    assert_eq!(cpu.registers.cs().selector, R0_CS);
}
