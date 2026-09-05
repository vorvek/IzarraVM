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
fn iret_word_inter_privilege_return_pops_ss_sp_and_nulls_ring0_data_segments() {
    // The Tyrian / Borland DPMI16BI shape: the host's 16-bit ring-0 INT 31h
    // handler returns to its ring-3 client with a WORD IRET over the frame
    // [ip, cs, flags, sp, ss]. The word form must pop SS:SP on the ring
    // change and null ring-0 data segments, exactly like the dword form;
    // leaving the ring-0 SS live at CPL 3 is the exodos-smoke-20260816
    // Tyrian crash (PUSH SS / POP ES then faults #GP on the DPL-0 selector).
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    let r3_code = descriptor(0, 0xfffff, 0xfb, 0xc0);
    let r3_data = descriptor(0, 0xfffff, 0xf3, 0xc0);
    bus.memory[(GDT + 0x20) as usize..(GDT + 0x20) as usize + 8].copy_from_slice(&r3_code);
    bus.memory[(GDT + 0x28) as usize..(GDT + 0x28) as usize + 8].copy_from_slice(&r3_data);
    let r3_cs = 0x23u16;
    let r3_ss = 0x2Bu16;
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ds, R0_SS).unwrap(); // ring-0 data
    cpu.registers.set_esp(0x6800);
    // Word frame, pushed high-to-low: SS, SP, FLAGS, CS, IP.
    for v in [u32::from(r3_ss), 0x2000, 0x2, u32::from(r3_cs), 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Word).unwrap();
    }

    cpu.iret(&mut bus, OperandSize::Word).unwrap();

    assert_eq!(cpu.current_privilege_level(), 3, "returned to ring 3");
    assert!(!cpu.is_v86_mode());
    assert_eq!(cpu.registers.eip, 0x1234);
    assert_eq!(cpu.registers.cs().selector, r3_cs);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ss).selector,
        r3_ss,
        "the word IRET's ring change must pop the outer SS"
    );
    assert_eq!(
        cpu.registers.esp(),
        0x2000,
        "outer SP popped from the frame"
    );
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ds).selector,
        0,
        "ring-0 DS must be nulled on the return to ring 3"
    );
}

#[test]
fn iret_word_same_privilege_return_stays_on_the_current_stack() {
    // Guard for the fix above: a word IRET whose popped CS has RPL == CPL is a
    // same-privilege return and must NOT pop SS:SP.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x6800);
    for v in [0x2u32, u32::from(R0_CS), 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Word).unwrap();
    }

    cpu.iret(&mut bus, OperandSize::Word).unwrap();

    assert_eq!(cpu.current_privilege_level(), 0);
    assert_eq!(cpu.registers.eip, 0x1234);
    assert_eq!(cpu.registers.cs().selector, R0_CS);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, R0_SS);
    assert_eq!(cpu.registers.esp(), 0x6800, "three word pops, nothing more");
}

#[test]
fn deliver_exception_through_a_16bit_interrupt_gate_pushes_a_word_frame() {
    // A 16-bit interrupt gate (type 6, here access 0xe6: present, DPL 3) must
    // build a WORD frame -- [ss, sp, flags, cs, ip, err] on a ring cross --
    // and clear IF exactly like its 32-bit sibling. The Borland DPMI16BI host
    // (Tyrian) hangs its whole IDT off type-6 gates; dword pushes through them
    // desynchronize the host's word-sized IRET frames. The gate's high dword
    // is reserved for the 16-bit types: the offset is ONLY the low word.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    let r3_code = descriptor(0, 0xfffff, 0xfb, 0xc0);
    let r3_data = descriptor(0, 0xfffff, 0xf3, 0xc0);
    bus.memory[(GDT + 0x20) as usize..(GDT + 0x20) as usize + 8].copy_from_slice(&r3_code);
    bus.memory[(GDT + 0x28) as usize..(GDT + 0x28) as usize + 8].copy_from_slice(&r3_data);
    let r3_cs = 0x23u16;
    let r3_ss = 0x2Bu16;
    // Vector 13 as a 16-bit interrupt gate to the ring-0 monitor. Poison the
    // reserved high-offset word to prove it is ignored.
    let base = (IDT + 13 * 8) as usize;
    put16(&mut bus.memory, base as u32, MON_CODE as u16);
    put16(&mut bus.memory, base as u32 + 2, R0_CS);
    bus.memory[base + 4] = 0;
    bus.memory[base + 5] = 0xe6; // present, DPL3, 16-bit interrupt gate
    put16(&mut bus.memory, base as u32 + 6, 0xdead);
    // Enter ring 3 through the proven dword IRET path.
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x6800);
    for v in [u32::from(r3_ss), 0x2000, 0x2, u32::from(r3_cs), 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Dword).unwrap();
    }
    cpu.iret(&mut bus, OperandSize::Dword).unwrap();
    assert_eq!(cpu.current_privilege_level(), 3);
    cpu.set_flag(FLAG_IF, true);

    cpu.deliver_exception(&mut bus, 13, Some(0x18), false)
        .unwrap();

    assert_eq!(cpu.current_privilege_level(), 0);
    assert_eq!(cpu.registers.cs().selector, R0_CS);
    assert_eq!(
        cpu.registers.eip, MON_CODE,
        "a 16-bit gate's offset is the low word only; the reserved high \
         word (0xdead) must not reach EIP"
    );
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, R0_SS);
    assert_eq!(
        cpu.registers.esp(),
        ESP0 - 12,
        "six WORD pushes (ss, sp, flags, cs, ip, err), not six dwords"
    );
    assert_eq!(
        cpu.registers.eflags & FLAG_IF,
        0,
        "a type-6 gate is an INTERRUPT gate: IF must be cleared"
    );
    let rd16 = |o: u32| {
        u16::from_le_bytes([
            bus.memory[(ESP0 - 12 + o) as usize],
            bus.memory[(ESP0 - 12 + o) as usize + 1],
        ])
    };
    assert_eq!(rd16(0), 0x18, "error code, word-sized");
    assert_eq!(rd16(2), 0x1234, "interrupted IP");
    assert_eq!(rd16(4), r3_cs, "interrupted CS");
    assert_eq!(
        rd16(6) & 0x200,
        0x200,
        "pushed FLAGS carries the pre-clear IF"
    );
    assert_eq!(rd16(8), 0x2000, "outer SP");
    assert_eq!(rd16(10), r3_ss, "outer SS");
}

#[test]
fn deliver_exception_reads_the_ring0_stack_from_a_286_tss() {
    // Borland's DPMI16BI hangs TR off a 16-bit (286) TSS: SP0 is a WORD at
    // offset +2 and SS0 a WORD at +4 (vs the 386 TSS's ESP0 dword at +4 /
    // SS0 at +8). Reading 386 offsets from a 286 TSS returns SS0=0 and the
    // null SS load kills the delivery with #GP(0) -- the second stage of the
    // exodos-smoke-20260816 Tyrian crash.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    let r3_code = descriptor(0, 0xfffff, 0xfb, 0xc0);
    let r3_data = descriptor(0, 0xfffff, 0xf3, 0xc0);
    bus.memory[(GDT + 0x20) as usize..(GDT + 0x20) as usize + 8].copy_from_slice(&r3_code);
    bus.memory[(GDT + 0x28) as usize..(GDT + 0x28) as usize + 8].copy_from_slice(&r3_data);
    let r3_cs = 0x23u16;
    let r3_ss = 0x2Bu16;
    // Re-shape the TSS as a 286 one: busy 16-bit TSS type (0x3) in TR's cached
    // access, SP0/SS0 at the 286 offsets. Zero the 386 slots to prove they are
    // not read.
    cpu.tr.access = 0x83;
    put32(&mut bus.memory, TSS + 4, 0);
    put16(&mut bus.memory, TSS + 8, 0);
    put16(&mut bus.memory, TSS + 2, 0x6f00); // SP0
    put16(&mut bus.memory, TSS + 4, R0_SS); // SS0
    // 16-bit interrupt gate for vector 0x21, DPL 3, like the DPMI host's IDT.
    let base = IDT + 0x21 * 8;
    put16(&mut bus.memory, base, MON_CODE as u16);
    put16(&mut bus.memory, base + 2, R0_CS);
    bus.memory[base as usize + 4] = 0;
    bus.memory[base as usize + 5] = 0xe6;
    put16(&mut bus.memory, base + 6, 0);
    // Enter ring 3 through the proven dword IRET path.
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x6800);
    for v in [u32::from(r3_ss), 0x2000, 0x2, u32::from(r3_cs), 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Dword).unwrap();
    }
    cpu.iret(&mut bus, OperandSize::Dword).unwrap();
    assert_eq!(cpu.current_privilege_level(), 3);

    cpu.deliver_exception(&mut bus, 0x21, None, true).unwrap();

    assert_eq!(cpu.current_privilege_level(), 0);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ss).selector,
        R0_SS,
        "SS0 comes from the 286 TSS word at +4"
    );
    assert_eq!(
        cpu.registers.esp(),
        0x6f00 - 10,
        "SP0 comes from the 286 TSS word at +2; five word pushes follow"
    );
}

#[test]
fn a_delivery_that_faults_midway_restores_the_interrupted_cpl() {
    // If the frame build faults (here: a null SS0 in a 386 TSS), the CPU
    // re-enters fault delivery for the nested exception. `deliver_exception`
    // sets `self.cpl` to the target level before the frame pushes; leaving it
    // there on the error path makes the retried delivery believe no ring
    // cross is needed, so the handler runs on the interrupted ring-3 stack.
    // That desync is what turned Tyrian's recoverable #GP into stack
    // corruption. The error path must restore the interrupted CPL.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    let r3_code = descriptor(0, 0xfffff, 0xfb, 0xc0);
    let r3_data = descriptor(0, 0xfffff, 0xf3, 0xc0);
    bus.memory[(GDT + 0x20) as usize..(GDT + 0x20) as usize + 8].copy_from_slice(&r3_code);
    bus.memory[(GDT + 0x28) as usize..(GDT + 0x28) as usize + 8].copy_from_slice(&r3_data);
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x6800);
    for v in [0x2Bu32, 0x2000, 0x2, 0x23, 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Dword).unwrap();
    }
    cpu.iret(&mut bus, OperandSize::Dword).unwrap();
    assert_eq!(cpu.current_privilege_level(), 3);
    // Poison SS0: the inner-stack switch must fault on the null selector.
    put16(&mut bus.memory, TSS + 8, 0);

    let result = cpu.deliver_exception(&mut bus, 0x21, None, true);

    assert!(
        result.is_err(),
        "null SS0 must fault the delivery: {result:?}"
    );
    assert_eq!(
        cpu.current_privilege_level(),
        3,
        "a faulted delivery must restore the interrupted CPL so the nested \
         exception delivers with a correct ring-cross decision"
    );
}

#[test]
fn retf_word_inter_privilege_return_pops_ss_sp_and_nulls_ring0_data_segments() {
    // 386 PRM RET (far, RPL > CPL): an inter-privilege far return pops SS:SP
    // from the stack after CS:IP and nulls data segments inaccessible at the
    // new CPL. DPMI hosts transfer to ring-3 client exception handlers with
    // exactly this shape (frame [ip, cs, sp, ss], words).
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    let r3_code = descriptor(0, 0xfffff, 0xfb, 0xc0);
    let r3_data = descriptor(0, 0xfffff, 0xf3, 0xc0);
    bus.memory[(GDT + 0x20) as usize..(GDT + 0x20) as usize + 8].copy_from_slice(&r3_code);
    bus.memory[(GDT + 0x28) as usize..(GDT + 0x28) as usize + 8].copy_from_slice(&r3_data);
    let r3_cs = 0x23u16;
    let r3_ss = 0x2Bu16;
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ds, R0_SS).unwrap(); // ring-0 data
    cpu.registers.set_esp(0x6800);
    // Word frame, pushed high-to-low: SS, SP, CS, IP.
    for v in [u32::from(r3_ss), 0x2000, u32::from(r3_cs), 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Word).unwrap();
    }

    cpu.return_far(&mut bus, OperandSize::Word, 0).unwrap();

    assert_eq!(cpu.current_privilege_level(), 3, "returned to ring 3");
    assert_eq!(cpu.registers.eip, 0x1234);
    assert_eq!(cpu.registers.cs().selector, r3_cs);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ss).selector,
        r3_ss,
        "the far return's ring change must pop the outer SS"
    );
    assert_eq!(
        cpu.registers.esp(),
        0x2000,
        "outer SP popped from the frame"
    );
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ds).selector,
        0,
        "ring-0 DS must be nulled on the return to ring 3"
    );
}

#[test]
fn retf_to_a_not_present_code_segment_restores_sp_for_the_restart() {
    // Borland RTM's overlay core: RETF into a swapped-out (P=0) code segment
    // raises #NP, RTM's handler loads the segment, and the RETF re-executes.
    // The pops must be undone on the fault or the restarted RETF pops 4 bytes
    // above the real frame -- on RTM's tight thunk stack that is past the SS
    // limit, and the retry dies #SS (the Tyrian swap-resume abort).
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    // Not-present ring-0 code descriptor at GDT 0x30 (access 0x18: P=0, code).
    let np_code = descriptor(0, 0xfffff, 0x18, 0xc0);
    bus.memory[(GDT + 0x30) as usize..(GDT + 0x30) as usize + 8].copy_from_slice(&np_code);
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x6800);
    for v in [0x30u32, 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Word).unwrap();
    }
    let esp_before = cpu.registers.esp();

    let result = cpu.return_far(&mut bus, OperandSize::Word, 0);

    assert!(result.is_err(), "P=0 target CS must fault: {result:?}");
    assert_eq!(
        cpu.registers.esp(),
        esp_before,
        "a faulted RETF must leave (E)SP exactly pre-instruction so the \
         restart after the segment swap-in re-pops the same frame"
    );
    assert_eq!(cpu.registers.cs().selector, R0_CS, "CS untouched");
}

#[test]
fn iret_word_to_a_not_present_code_segment_restores_sp_for_the_restart() {
    // Same restartability rule for IRET: RTM interrupt returns into
    // swapped-out segments too.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    let np_code = descriptor(0, 0xfffff, 0x18, 0xc0);
    bus.memory[(GDT + 0x30) as usize..(GDT + 0x30) as usize + 8].copy_from_slice(&np_code);
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x6800);
    for v in [0x2u32, 0x30, 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Word).unwrap();
    }
    let esp_before = cpu.registers.esp();

    let result = cpu.iret(&mut bus, OperandSize::Word);

    assert!(result.is_err(), "P=0 target CS must fault: {result:?}");
    assert_eq!(
        cpu.registers.esp(),
        esp_before,
        "a faulted IRET must leave (E)SP exactly pre-instruction"
    );
}

#[test]
fn far_call_to_a_not_present_code_segment_faults_before_pushing() {
    // 386 PRM CALL: the target descriptor is validated (present bit included,
    // #NP) BEFORE the return address is pushed. RTM far-calls into
    // swapped-out overlay segments; a committed push pair before the #NP
    // would leak 4 bytes of stack per swap-in once the call restarts.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    let np_code = descriptor(0, 0xfffff, 0x18, 0xc0);
    bus.memory[(GDT + 0x30) as usize..(GDT + 0x30) as usize + 8].copy_from_slice(&np_code);
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x6800);

    let result = cpu.far_call(&mut bus, 0x30, 0x10, OperandSize::Word);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 11,
                error_code: Some(0x30),
            })
        ),
        "P=0 target CS must raise #NP(selector) before any push: {result:?}"
    );
    assert_eq!(
        cpu.registers.esp(),
        0x6800,
        "no return-address bytes may be committed by the faulted call"
    );
}

/// Install the shared ring-3 code/data descriptor pair at GDT 0x20/0x28 and
/// enter ring 3 through the proven dword IRET path (CS=0x23, SS=0x2B,
/// EIP=0x1234, ESP=0x2000).
fn enter_ring3(cpu: &mut CpuGsw, bus: &mut TestBus) {
    let r3_code = descriptor(0, 0xfffff, 0xfb, 0xc0);
    let r3_data = descriptor(0, 0xfffff, 0xf3, 0xc0);
    bus.memory[(GDT + 0x20) as usize..(GDT + 0x20) as usize + 8].copy_from_slice(&r3_code);
    bus.memory[(GDT + 0x28) as usize..(GDT + 0x28) as usize + 8].copy_from_slice(&r3_data);
    cpu.registers.eflags = 0x2;
    cpu.load_segment(bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x6800);
    for v in [0x2Bu32, 0x2000, 0x2, 0x23, 0x1234] {
        cpu.push(bus, v, OperandSize::Dword).unwrap();
    }
    cpu.iret(bus, OperandSize::Dword).unwrap();
    assert_eq!(cpu.current_privilege_level(), 3);
}

/// Write a 16-bit gate (type in `access`, e.g. 0xe6 interrupt / 0xe7 trap)
/// for `vector`, targeting R0_CS:offset. Poisons the reserved high word.
fn gate16(m: &mut [u8], vector: u8, offset: u16, access: u8) {
    let base = IDT + u32::from(vector) * 8;
    put16(m, base, offset);
    put16(m, base + 2, R0_CS);
    m[base as usize + 4] = 0;
    m[base as usize + 5] = access;
    put16(m, base + 6, 0xdead);
}

#[test]
fn iret_word_inter_privilege_to_a_16bit_stack_takes_sp_only_and_keeps_high_esp() {
    // B-keying pin for the WORD arm: the outer SS has B=0, the inner stack's
    // ESP carries a nonzero high word (0x0001). The popped SP must land in SP
    // only, with the inner high word carried over -- a swapped branch
    // (zero-extending set_esp) yields 0x0000_0010 instead.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    let r3_code = descriptor(0, 0xfffff, 0xfb, 0xc0);
    let r3_data_b0 = descriptor(0, 0xffff, 0xf3, 0x00); // B=0 16-bit stack
    bus.memory[(GDT + 0x20) as usize..(GDT + 0x20) as usize + 8].copy_from_slice(&r3_code);
    bus.memory[(GDT + 0x28) as usize..(GDT + 0x28) as usize + 8].copy_from_slice(&r3_data_b0);
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x0001_6800);
    for v in [0x2Bu32, 0x0010, 0x2, 0x23, 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Word).unwrap();
    }

    cpu.iret(&mut bus, OperandSize::Word).unwrap();

    assert_eq!(cpu.current_privilege_level(), 3);
    assert!(!cpu.stack_is_32bit(), "the loaded outer SS must carry B=0");
    assert_eq!(
        cpu.registers.esp(),
        0x0001_0010,
        "SP takes the popped word; ESP's high word carries over from the \
         inner stack (0x0001), not zero"
    );
}

#[test]
fn iret_word_inter_privilege_with_mismatched_ss_rpl_faults_gp() {
    // 386 PRM IRET (outer level): the popped SS selector's RPL must equal the
    // return CS's RPL, else #GP(SS selector).
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    let r3_code = descriptor(0, 0xfffff, 0xfb, 0xc0);
    let r3_data = descriptor(0, 0xfffff, 0xf3, 0xc0);
    bus.memory[(GDT + 0x20) as usize..(GDT + 0x20) as usize + 8].copy_from_slice(&r3_code);
    bus.memory[(GDT + 0x28) as usize..(GDT + 0x28) as usize + 8].copy_from_slice(&r3_data);
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x6800);
    // SS selector 0x28 = the ring-3 data descriptor with RPL 0 (CS RPL is 3).
    for v in [0x28u32, 0x2000, 0x2, 0x23, 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Word).unwrap();
    }

    let result = cpu.iret(&mut bus, OperandSize::Word);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(0x28),
            })
        ),
        "SS RPL != CS RPL must fault #GP(SS selector): {result:?}"
    );
}

#[test]
fn retf_word_with_release_skips_the_inner_parameter_block_before_ss_sp() {
    // 386 PRM RET n (outer level): the immediate releases the parameter block
    // on the INNER stack before SS:eSP are popped, and again on the outer
    // stack after (the caller's release_stack). Reading SS:SP from inside the
    // parameter area loads garbage into SS.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    let r3_code = descriptor(0, 0xfffff, 0xfb, 0xc0);
    let r3_data = descriptor(0, 0xfffff, 0xf3, 0xc0);
    bus.memory[(GDT + 0x20) as usize..(GDT + 0x20) as usize + 8].copy_from_slice(&r3_code);
    bus.memory[(GDT + 0x28) as usize..(GDT + 0x28) as usize + 8].copy_from_slice(&r3_data);
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x6800);
    // Frame, pushed high-to-low: SS, SP, param2, param1, CS, IP -- the two
    // parameter words sit between CS:IP and the saved SS:SP, exactly as a
    // call gate with param_count=2 leaves them.
    for v in [0x2Bu32, 0x2000, 0xbead, 0xbeef, 0x23, 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Word).unwrap();
    }

    // Mirror the 0xCA opcode handler: return_far with the release, then the
    // caller's outer release.
    cpu.return_far(&mut bus, OperandSize::Word, 4).unwrap();
    cpu.release_stack(4);

    assert_eq!(cpu.current_privilege_level(), 3);
    assert_eq!(cpu.registers.cs().selector, 0x23);
    assert_eq!(cpu.registers.eip, 0x1234);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ss).selector,
        0x2B,
        "SS must come from beyond the parameter block, not from inside it"
    );
    assert_eq!(
        cpu.registers.esp(),
        0x2000 + 4,
        "the immediate is applied to the OUTER stack as well"
    );
}

#[test]
fn a_16bit_trap_gate_delivers_a_word_frame_without_clearing_if() {
    // Type 7 is a 16-bit TRAP gate: word frame like type 6, but IF survives.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    gate16(&mut bus.memory, 0x21, MON_CODE as u16, 0xe7);
    enter_ring3(&mut cpu, &mut bus);
    cpu.set_flag(FLAG_IF, true);

    cpu.deliver_exception(&mut bus, 0x21, None, true).unwrap();

    assert_eq!(cpu.current_privilege_level(), 0);
    assert_eq!(cpu.registers.eip, MON_CODE);
    assert_eq!(
        cpu.registers.esp(),
        ESP0 - 10,
        "five WORD pushes (ss, sp, flags, cs, ip), no error code"
    );
    assert_ne!(
        cpu.registers.eflags & FLAG_IF,
        0,
        "a trap gate must NOT clear IF"
    );
}

#[test]
fn a_v86_source_through_a_16bit_gate_still_builds_the_dword_frame() {
    // 386 PRM INTERRUPT-FROM-V86-MODE has no 16-bit variant: every push is
    // "padded to two words" regardless of the gate width. A word frame would
    // truncate the guest's ESP.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    gate16(&mut bus.memory, 13, MON_CODE as u16, 0xe6);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);

    cpu.deliver_exception(&mut bus, 13, Some(0), false).unwrap();

    assert!(!cpu.is_v86_mode());
    assert_eq!(cpu.registers.eip, MON_CODE);
    assert_eq!(
        cpu.registers.esp(),
        ESP0 - 40,
        "ten DWORD pushes (gs fs ds es ss esp efl cs eip ec), not words"
    );
    let rd = |o: u32| u32::from_le_bytes(cpu_mem(&bus, cpu.registers.esp() + o));
    assert_eq!(rd(0), 0, "error code");
    assert_eq!(rd(4), 0x10, "V86 EIP");
    assert_eq!(rd(16), 0x1000, "V86 ESP, full dword");
}

#[test]
fn an_uninitialized_tr_access_byte_keeps_the_386_tss_read() {
    // The 286/386 TSS discriminator keys on the full type (1/3), so an
    // uninitialized TR cache (access 0) stays on the 386 layout it always had.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    cpu.tr.access = 0;
    enter_v86_direct(&mut cpu, 0x10, 0x1000);

    cpu.deliver_exception(&mut bus, 13, Some(0), false).unwrap();

    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, R0_SS);
    assert_eq!(
        cpu.registers.esp(),
        ESP0 - 40,
        "386 ESP0/SS0 fields were read"
    );
}

#[test]
fn a_delivery_that_faults_after_the_stack_switch_restores_ss_esp() {
    // The frame build faults AFTER `switch_to_inner_stack` committed the inner
    // SS:ESP (here: a ring-0 stack whose limit cannot hold the frame). The
    // retried delivery must capture the ORIGINAL ring-3 SS:ESP as the
    // interrupted context, so the error path must restore them.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    // Ring-0 data descriptor with an 8-byte limit at GDT 0x38: the six word
    // pushes (12 bytes) cannot fit.
    let tiny = descriptor(0, 0x8, 0x93, 0x00);
    bus.memory[(GDT + 0x38) as usize..(GDT + 0x38) as usize + 8].copy_from_slice(&tiny);
    put16(&mut bus.memory, TSS + 8, 0x38);
    put32(&mut bus.memory, TSS + 4, 0x8);
    gate16(&mut bus.memory, 13, MON_CODE as u16, 0xe6);
    enter_ring3(&mut cpu, &mut bus);
    let esp_before = cpu.registers.esp();

    let result = cpu.deliver_exception(&mut bus, 13, Some(0), false);

    assert!(result.is_err(), "the frame push must fault: {result:?}");
    assert_eq!(cpu.current_privilege_level(), 3);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ss).selector,
        0x2B,
        "the interrupted ring-3 SS must be restored after the faulted delivery"
    );
    assert_eq!(cpu.registers.esp(), esp_before, "and its ESP with it");
}

#[test]
fn a_v86_delivery_that_faults_on_the_cs_load_restores_the_v86_state() {
    // The V86 tail: pushes succeed, the data segments are nulled, then the CS
    // load faults (#NP on a not-present monitor CS). The restore must bring
    // back VM, the real-mode data segments, and SS:ESP.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    bus.memory[(GDT + 0x08 + 5) as usize] = 0x1b; // R0_CS: present bit off
    enter_v86_direct(&mut cpu, 0x10, 0x1000);

    let result = cpu.deliver_exception(&mut bus, 13, Some(0), false);

    assert!(result.is_err(), "the CS load must fault: {result:?}");
    assert!(cpu.is_v86_mode(), "VM must be restored");
    assert_eq!(cpu.current_privilege_level(), 3);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, 0x0900);
    assert_eq!(cpu.registers.esp(), 0x1000);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ds).selector,
        0x0A00,
        "the nulled V86 data segments must be restored"
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
    // Companion to the reproduction above: delivery genuinely nests a fault here,
    // because ESP0's page is marked NOT PRESENT and the frame push itself raises
    // #PF. `deliver_exception` must report that nested vector truthfully, not a
    // fabricated `IdtLimit` on the ORIGINAL vector (the pre-fix behavior, which
    // discarded the nested vector entirely). What the run loop then DOES with
    // that nested vector is the escalation test below.
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
}

#[test]
fn an_escalation_chain_that_exhausts_every_handler_shuts_down_and_records_the_site() {
    // The same world as the test above, driven through the run loop's own tail
    // (`finish_instruction`, the call site the original bug lived in). Every gate
    // this chain needs is missing, so it walks the whole table and ends in
    // shutdown: #GP (contributory) nests #PF (page fault) from the not-present
    // ESP0 push, which is handled serially; vector 14's gate is zeroed in this
    // world, so its clear present bit nests #NP (contributory) on a page fault,
    // which escalates to #DF; vector 8's gate is zeroed too, so calling the
    // double-fault handler nests #NP once more and the processor shuts down. The
    // reported nested vector is therefore that third fault, and `original_vector`
    // is the one the chain started from.
    //
    // The #NP here is NOT the architectural answer -- it pins the emulator's
    // documented divergence. On metal a zeroed IDT entry is a type-0 descriptor
    // and the PRM's AR-byte gate-type test, which precedes both the DPL and the
    // presence test, raises #GP(vector*8+2). This emulator omits that type check
    // (see the ledger note in `deliver_exception_body`), so with it absent the P
    // check fires first and reports #NP. These vectors read 13 before the P check
    // existed, which was the right vector for the wrong reason: it came from the
    // null-selector load further down, carrying error code 0 rather than
    // vector*8+2. #NP and #GP are both contributory, so the chain and its
    // shutdown are unchanged either way -- only the vector each step reports.
    // Adding the type check later moves these back to 13, correctly this time.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    put32(&mut bus.memory, 0x2000 + 6 * 4, 0x6000 | 0x6);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);
    let start_eip = cpu.registers.eip;
    let start_cs = cpu.registers.cs().selector;
    let result: Result<CpuCycleOutcome, CpuError> = cpu.finish_instruction(
        &mut bus,
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
        Err(CpuError::TripleFault {
            original_vector: 13,
            nested_vector: 11,
        }),
        "{result:?}"
    );

    // The raise site recorded for the machine's stop report must be the
    // instruction that faulted, and must be recorded exactly once. The
    // exception arm rewinds EIP before delivery, and `deliver_exception` loads
    // CS and sets EIP last, so the rewound value is still live on every error
    // path out of it. Rewinding a second time here would be wrong, and reading
    // the live registers instead would report whatever delivery had reached.
    // This one has been watched failing: deleting the record call on the nested
    // path drops it to None.
    let site = cpu
        .fault_site()
        .expect("a nested delivery fault must record its raise site");
    // These two have NOT been watched failing, and the honest reason is worth
    // more than the appearance of a guard. In this scenario the nested fault is
    // the frame push's own #PF, which happens before delivery reaches either the
    // CS load or the EIP write, so the pre-delivery snapshot and the live
    // registers are equal and no mutation tried could separate them (recording
    // live registers here, and moving `set_eip` ahead of the CS load in
    // `deliver_exception`, both left this green). They pin the invariant for a
    // future change that advances EIP earlier in delivery. Treat them as
    // documentation with teeth, not as a proven guard.
    assert_eq!(
        site.eip, start_eip,
        "the site must be the faulting instruction, not a point inside delivery"
    );
    assert_eq!(site.cs.selector, start_cs);
}

/// Two distinct handler entry points, so a test can name which vector the core
/// actually delivered. Only the addresses matter: no code runs at either.
const MON_DF: u32 = MON_CODE + 0x100;
const MON_PF: u32 = MON_CODE + 0x200;
const MON_NP: u32 = MON_CODE + 0x300;

/// An IDT base chosen so vector 13's gate occupies the last 8 bytes of page 12
/// (0xCFF8) and vector 14's the first 8 bytes of page 13 (0xD000), with vector
/// 8's gate at 0xCFD0 on page 12. Clearing one of the two pages' present bits
/// then makes exactly one gate read fault, which is how these tests raise a
/// nested fault at a chosen step of the delivery chain.
const SPLIT_IDT: u32 = 0xD000 - 14 * 8;

/// Write a present 32-bit ring-0 interrupt gate at an absolute linear address.
/// The `int_gate` helper is fixed to the default IDT base, so the relocated-IDT
/// tests need this form.
fn write_gate(m: &mut [u8], at: u32, offset: u32) {
    put16(m, at, offset as u16);
    put16(m, at + 2, R0_CS);
    m[at as usize + 4] = 0;
    m[at as usize + 5] = 0x8e;
    put16(m, at + 6, (offset >> 16) as u16);
}

/// Clear page `page`'s present bit in the identity page table `v86_world` built.
fn unmap_page(m: &mut [u8], page: u32) {
    put32(m, 0x2000 + page * 4, (page << 12) | 0x6);
}

/// Deliver `vector` through the real call site (`finish_instruction`, the tail
/// every faulting instruction runs) and return both its result and the CS:EIP
/// the fault was raised at.
fn deliver_through_finish_instruction(
    cpu: &mut CpuGsw,
    bus: &mut TestBus,
    vector: u8,
    error_code: Option<u32>,
) -> (Result<CpuCycleOutcome, CpuError>, u32, u16) {
    let start_eip = cpu.registers.eip;
    let start_cs = cpu.registers.cs().selector;
    let result = cpu.finish_instruction(
        bus,
        Err(InternalFault::Exception { vector, error_code }),
        start_eip,
        start_cs,
        0,
        None,
        None,
    );
    (result, start_eip, start_cs)
}

#[test]
fn the_fault_class_table_matches_the_prm() {
    // 386 PRM Table 9-3, transcribed vector by vector. Written from the manual,
    // not from observed behavior: the delivery tests below reach four cells of
    // the matrix, and this pins the rest of the table with them.
    use FaultClass::{Benign, Contributory, PageFault};
    for vector in [1, 2, 3, 4, 5, 6, 7, 16] {
        assert_eq!(fault_class(vector, false), Benign, "vector {vector}");
    }
    for vector in [0, 9, 10, 11, 12, 13] {
        assert_eq!(fault_class(vector, false), Contributory, "vector {vector}");
    }
    assert_eq!(fault_class(14, false), PageFault);
    // Table 9-3 has no row for #DF itself; the core classes it contributory as a
    // fallback for a lost double-fault-in-progress flag, so that such a pair
    // escalates instead of looping. `escalate_delivery` seeds that flag directly,
    // which is what the shutdown tests below actually exercise.
    assert_eq!(fault_class(8, false), Contributory);
    // The PRM's first class is "Benign Exceptions AND INTERRUPTS": an external
    // interrupt or a software INT n is benign on every vector, including the ones
    // an exception would make contributory.
    for vector in [0, 8, 13, 14, 0x21] {
        assert_eq!(fault_class(vector, true), Benign, "vector {vector}");
    }

    let cell = |first, second| escalate_fault(first, second);
    assert_eq!(cell(Benign, Benign), FaultEscalation::Serial);
    assert_eq!(cell(Benign, Contributory), FaultEscalation::Serial);
    assert_eq!(cell(Benign, PageFault), FaultEscalation::Serial);
    assert_eq!(cell(Contributory, Benign), FaultEscalation::Serial);
    assert_eq!(
        cell(Contributory, Contributory),
        FaultEscalation::DoubleFault
    );
    assert_eq!(cell(Contributory, PageFault), FaultEscalation::Serial);
    assert_eq!(cell(PageFault, Benign), FaultEscalation::Serial);
    assert_eq!(cell(PageFault, Contributory), FaultEscalation::DoubleFault);
    assert_eq!(cell(PageFault, PageFault), FaultEscalation::DoubleFault);
}

#[test]
fn two_contributory_faults_escalate_to_a_double_fault() {
    // 386 PRM 9.9.8: "When two contributory events occur, they cannot be
    // handled, and a double-fault exception is generated." The first event is a
    // #GP (13, contributory). Its gate names a selector whose index is past the
    // GDTR limit, so building the frame raises a second #GP (contributory). The
    // core must deliver #DF (vector 8) to the guest instead of stopping the
    // machine.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    put16(&mut bus.memory, IDT + 13 * 8 + 2, 0x0108);
    int_gate(&mut bus.memory, 8, MON_DF);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);

    let (result, start_eip, start_cs) =
        deliver_through_finish_instruction(&mut cpu, &mut bus, 13, Some(0));

    assert!(
        result.is_ok(),
        "the double fault must be delivered to the guest, not reported as a \
         host-fatal error: {result:?}"
    );
    assert_eq!(cpu.registers.eip, MON_DF, "the #DF handler runs");
    assert_eq!(cpu.registers.cs().selector, R0_CS);
    assert!(!cpu.is_v86_mode(), "VM must be cleared on monitor entry");
    let esp = cpu.registers.esp();
    let rd = |o: u32| u32::from_le_bytes(cpu_mem(&bus, esp + o));
    assert_eq!(
        rd(0),
        0,
        "the #DF error code is always 0, whatever the two faults carried"
    );
    assert_eq!(rd(4), start_eip, "the frame names the faulting instruction");
    assert_eq!(rd(8) & 0xffff, u32::from(start_cs));
}

#[test]
fn a_page_fault_during_a_contributory_delivery_is_handled_serially() {
    // 386 PRM 9.9.8: "If a benign or contributory exception is followed by a
    // page fault, the two events can be handled in succession." The first event
    // is a #GP (13, contributory) whose gate sits on a page with the present bit
    // cleared, so reading the gate raises #PF (14). The core must deliver the
    // #PF handler; the #GP is left to be raised again when that handler's IRET
    // restarts the faulting instruction.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    cpu.idtr.base = SPLIT_IDT;
    write_gate(&mut bus.memory, SPLIT_IDT + 13 * 8, MON_CODE);
    write_gate(&mut bus.memory, SPLIT_IDT + 14 * 8, MON_PF);
    unmap_page(&mut bus.memory, 12);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);

    let (result, start_eip, start_cs) =
        deliver_through_finish_instruction(&mut cpu, &mut bus, 13, Some(0));

    assert!(
        result.is_ok(),
        "a page fault during a contributory delivery is handled serially: {result:?}"
    );
    assert_eq!(
        cpu.control.cr2,
        SPLIT_IDT + 13 * 8,
        "the nested fault is the #GP gate read, not a stack access"
    );
    assert_eq!(cpu.registers.eip, MON_PF, "the #PF handler runs");
    assert_eq!(cpu.registers.cs().selector, R0_CS);
    let esp = cpu.registers.esp();
    let rd = |o: u32| u32::from_le_bytes(cpu_mem(&bus, esp + o));
    // Decorative, and worth saying so: the original #GP was raised with code 0
    // and the nested #PF's own code is also 0 (not present, read, supervisor), so
    // this cannot tell which of the two was pushed. The cr2 and MON_PF assertions
    // are what carry the test. A nonzero original code would discriminate, but a
    // V86-origin vector-13 delivery with one trips the TOKAEMM frame-shape
    // debug_assert in `deliver_exception_body`.
    assert_eq!(rd(0), 0, "an error code was pushed for the #PF");
    assert_eq!(
        rd(4),
        start_eip,
        "the #PF frame returns to the faulting instruction, which raises the \
         #GP again -- that is what handling the two serially means"
    );
    assert_eq!(rd(8) & 0xffff, u32::from(start_cs));
}

#[test]
fn a_page_fault_during_a_page_fault_delivery_escalates_to_a_double_fault() {
    // 386 PRM 9.9.8: "if a page fault is followed by a contributory exception or
    // another page fault, a double-fault abort is generated." Vector 14's gate
    // is on the missing page, so delivering a guest #PF raises a second #PF.
    // Vector 8's gate stays on the present page and must be the one entered.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    cpu.idtr.base = SPLIT_IDT;
    write_gate(&mut bus.memory, SPLIT_IDT + 8 * 8, MON_DF);
    write_gate(&mut bus.memory, SPLIT_IDT + 14 * 8, MON_PF);
    unmap_page(&mut bus.memory, 13);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);

    let (result, ..) = deliver_through_finish_instruction(&mut cpu, &mut bus, 14, Some(0x7));

    assert!(
        result.is_ok(),
        "the double fault must be delivered to the guest: {result:?}"
    );
    assert_eq!(
        cpu.registers.eip, MON_DF,
        "the #DF handler runs, not the #PF one"
    );
    let esp = cpu.registers.esp();
    let rd = |o: u32| u32::from_le_bytes(cpu_mem(&bus, esp + o));
    assert_eq!(
        rd(0),
        0,
        "the #DF error code is 0, not the original #PF's 0x7"
    );
}

#[test]
fn a_fault_while_calling_the_double_fault_handler_stops_the_machine() {
    // 386 PRM 9.9.8: "If any other exception occurs while attempting to call the
    // double-fault handler, the processor enters shutdown mode." Same layout as
    // the double-fault case above, except vector 8's gate is left zeroed: its
    // P bit is clear, so it raises #NP while the #DF frame is being built. This
    // is the one case that still stops the emulator.
    //
    // The nested vector pins the emulator's documented divergence, not the
    // architectural answer: metal raises #GP(vector*8+2) for a zeroed entry via
    // the AR-byte gate-type check the PRM puts ahead of both the DPL and the
    // presence test, and which this emulator omits (see the ledger note in
    // `deliver_exception_body`). With that check absent the P check fires first
    // and reports #NP. This read 13 until `deliver_exception_body` gained the P
    // check -- the right vector for the wrong reason, since it came from the
    // null-selector load with error code 0 instead of vector*8+2. #NP and #GP
    // are both contributory (`fault_class`), so the escalation verdict and the
    // shutdown are identical either way; only the reported vector moves. Adding
    // the type check later moves this back to 13, correctly this time.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    cpu.idtr.base = SPLIT_IDT;
    write_gate(&mut bus.memory, SPLIT_IDT + 14 * 8, MON_PF);
    unmap_page(&mut bus.memory, 13);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);

    let (result, ..) = deliver_through_finish_instruction(&mut cpu, &mut bus, 14, Some(0x7));

    assert_eq!(
        result,
        Err(CpuError::TripleFault {
            original_vector: 14,
            nested_vector: 11,
        }),
        "{result:?}"
    );
}

#[test]
fn an_external_interrupt_on_vector_8_is_not_a_double_fault_in_progress() {
    // IRQ0 lands on vector 8 whenever the PIC is left at base 0x08, which is
    // where a real DOS boot leaves it. The PRM's benign class is titled "Benign
    // Exceptions and Interrupts", so the vector number alone must not make the
    // core treat the delivery as the double-fault handler's call.
    //
    // The nested fault here has to be CONTRIBUTORY for the test to bite: benign
    // and contributory both pair with a page fault as Serial, so a nested #PF
    // would deliver the same handler either way and could not tell a working
    // classifier from one that ignored `is_external`. So the IRQ's gate names a
    // code descriptor with the present bit clear: the frame builds, then the
    // handler's CS load raises #NP (11), which is contributory. Correct
    // behavior delivers #NP serially; classing vector 8 as an exception instead
    // pairs contributory with contributory, escalates to #DF, re-enters the same
    // bad gate and shuts down.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    cpu.idtr.base = SPLIT_IDT;
    let absent_code = descriptor(0, 0xfffff, 0x1b, 0xc0);
    bus.memory[(GDT + 0x20) as usize..(GDT + 0x20) as usize + 8].copy_from_slice(&absent_code);
    write_gate(&mut bus.memory, SPLIT_IDT + 8 * 8, MON_DF);
    put16(&mut bus.memory, SPLIT_IDT + 8 * 8 + 2, 0x20);
    write_gate(&mut bus.memory, SPLIT_IDT + 11 * 8, MON_NP);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);
    cpu.registers.eflags |= FLAG_IF;
    bus.pending_irq = Some(8);

    let result = cpu.service_pending_interrupt(&mut bus);

    assert!(
        result.is_ok(),
        "an external vector-8 interrupt whose delivery faults must escalate like \
         any benign event, not shut down: {result:?}"
    );
    assert_eq!(
        cpu.registers.eip, MON_NP,
        "the #NP handler runs serially; vector 8 here was an interrupt, not a #DF"
    );
    assert_eq!(cpu.registers.cs().selector, R0_CS);
}

#[test]
fn a_non_external_vector_8_delivery_that_faults_shuts_down_immediately() {
    // Structural guard, not a live path: no site in the core raises vector 8 as
    // a processor exception today. If one is ever added, that delivery IS the
    // double-fault handler's call, so a fault during it must shut the processor
    // down instead of synthesizing a second #DF. Without the seed, the pair
    // (contributory, page fault) would read as Serial and the guest would get
    // the #PF handler.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    cpu.idtr.base = SPLIT_IDT;
    write_gate(&mut bus.memory, SPLIT_IDT + 14 * 8, MON_PF);
    unmap_page(&mut bus.memory, 12);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);

    let result = cpu.deliver_exception_escalating(&mut bus, 8, Some(0), false);

    assert_eq!(
        result,
        Err(CpuError::TripleFault {
            original_vector: 8,
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

/// The third site that raises a fatal `CpuError` is interrupt delivery, and it
/// is the one where the faulting-instruction rule does NOT apply: an IRQ is
/// asynchronous and is taken at an instruction boundary, so the boundary itself
/// is the right thing to report. Rewinding it, or reporting the instruction
/// before it, would name code that had already retired successfully.
///
/// Watched failing: dropping the record call on this path leaves `fault_site`
/// as None and the expect below fires. The two ADDRESS assertions were not, and
/// cannot be here: an `IdtLimit` refusal happens before delivery touches CS or
/// EIP, so recording live registers instead of the boundary snapshot leaves them
/// green. Same honest caveat as the nested-delivery fixture below.
#[test]
fn a_fatal_fault_delivering_an_irq_reports_the_boundary_it_was_taken_at() {
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0x90, 0xf4], &[0x00]);
    enter_v86_direct(&mut cpu, 0, 0x1000);
    // Shrink the IDT so vector 0x30's gate is out of range: delivery then fails
    // with IdtLimit, which is fatal rather than a deliverable exception.
    cpu.idtr.limit = 0x20;
    cpu.set_flag(FLAG_IF, true);
    bus.pending_irq = Some(0x30);

    let boundary_eip = cpu.registers.eip;
    let boundary_cs = cpu.registers.cs().selector;
    let result = cpu.service_pending_interrupt(&mut bus);

    // TripleFault, not IdtLimit, since 2026-08-30: see
    // `a_software_interrupt_past_the_idt_limit_raises_gp_rather_than_stopping`.
    // A limit of 0x20 covers neither vector 0x30 nor vector 13, so the #GP the
    // PRM raises is itself undeliverable and the chain ends in shutdown. The
    // fault-site assertions below are what this test exists for and they are
    // unchanged.
    assert!(
        matches!(result, Err(CpuError::TripleFault { .. })),
        "{result:?}"
    );
    let site = cpu
        .fault_site()
        .expect("a fatal fault in IRQ delivery must record its raise site");
    assert_eq!(
        site.eip, boundary_eip,
        "an asynchronous interrupt has no faulting instruction, so the site is \
         the boundary the IRQ was taken at"
    );
    assert_eq!(site.cs.selector, boundary_cs);
}

/// Stage-1 defect E7 (SpacPlum/baroll/MontyNrm): straight-line V86 code
/// running up to the top of its 64K segment must #GP(0) at the instruction
/// whose bytes straddle the CS limit, before that instruction executes. The
/// 386 PRM applies the code-segment limit to instruction fetch; a V86 (or
/// real-mode) CS always carries limit 0xFFFF, so EIP can never silently
/// pass 0x10000. Before the fix the fetch path had no code-limit check at
/// all: EIP kept climbing (SpacPlum's monitor frame carried 0x0001006b),
/// and the TOKAEMM monitor's IRETD back to V86 then faulted at ring 0.
#[test]
fn v86_code_fetch_straddling_the_cs_limit_raises_gp0_before_executing() {
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[], &[0x00]);
    // mov ax, 0x1234 (3 bytes) at CS:0xFFFE: its last byte lies past the
    // 0xFFFF limit. Guest CS is 0x0A00, so the bytes sit at linear 0x19FFE.
    bus.memory[0x19FFE] = 0xB8;
    bus.memory[0x19FFF] = 0x34;
    bus.memory[0x1A000] = 0x12;
    enter_v86_direct(&mut cpu, 0xFFFE, 0x1000);
    cpu.write_reg16(Reg16::Ax, 0xDEAD);

    cpu.run_straight_line(&mut bus, 10_000).unwrap();

    assert!(
        !cpu.is_v86_mode(),
        "the straddling fetch must deliver #GP(0) into the monitor; EIP ran \
         past the limit instead (eip={:#x})",
        cpu.registers.eip
    );
    assert_eq!(cpu.registers.cs().selector, R0_CS);
    assert_eq!(
        cpu.read_reg16(Reg16::Ax),
        0xDEAD,
        "the straddling instruction must not execute"
    );
    let esp = cpu.registers.esp();
    let rd = |o: u32| u32::from_le_bytes(cpu_mem(&bus, esp + o));
    assert_eq!(rd(0), 0, "error code");
    assert_eq!(
        rd(4),
        0xFFFE,
        "fault semantics: the frame EIP points AT the straddling instruction"
    );
    assert_eq!(rd(8) & 0xffff, 0x0A00, "V86 CS");
    assert_eq!(rd(12) & FLAG_VM, FLAG_VM, "pushed EFLAGS carries VM=1");
}

/// Companion boundary case: EIP already past the limit (a wild o32 transfer
/// can leave it there; 0x1006b is SpacPlum's own leaked value). The next
/// instruction boundary must #GP(0) instead of fetching from beyond the
/// segment.
#[test]
fn v86_code_fetch_with_eip_beyond_the_cs_limit_raises_gp0() {
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[], &[0x00]);
    // A NOP at linear CS.base + 0x1006b: reachable only through the defect.
    bus.memory[0x1A06B] = 0x90;
    enter_v86_direct(&mut cpu, 0x1006b, 0x1000);

    cpu.run_straight_line(&mut bus, 10_000).unwrap();

    assert!(
        !cpu.is_v86_mode(),
        "a fetch at EIP {:#x} (past the 0xFFFF limit) must deliver #GP(0)",
        0x1006b
    );
    assert_eq!(cpu.registers.cs().selector, R0_CS);
    let esp = cpu.registers.esp();
    let rd = |o: u32| u32::from_le_bytes(cpu_mem(&bus, esp + o));
    assert_eq!(rd(0), 0, "error code");
    // The stage-1 G1 storm was fed by exactly this frame: a V86 EIP with a
    // nonzero high word. Real silicon cannot push one (V86 IP is 16-bit at
    // every architectural point), and a monitor's word-sized frame writes
    // (TOKAEMM reflect_vector) leave the high half in place, so its return
    // IRETD then faults at ring 0. The pushed image must carry only the low
    // 16 bits, whatever emulator-side arithmetic leaked into live EIP.
    assert_eq!(
        rd(4),
        0x006b,
        "the V86 frame EIP must be masked to 16 bits (got {:#x})",
        rd(4)
    );
}

/// The wrap half of E7 (what the fault trace actually showed: delivery at
/// EIP exactly 0x10000): an instruction whose LAST byte sits at offset
/// 0xFFFF is legal, and the 16-bit IP then wraps to 0 -- real-mode .COM
/// wrap tricks depend on it, and no fault is raised. Before the fix the
/// interpreter left the unwrapped 0x10000 in EIP and the fetch guard
/// #GP(0)'d a legal program.
#[test]
fn v86_sequential_run_off_at_the_64k_boundary_wraps_ip_to_zero() {
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[], &[0x00]);
    // inc ax; inc ax at 0xFFFE: the second ends exactly at the limit.
    bus.memory[0x19FFE] = 0x40;
    bus.memory[0x19FFF] = 0x40;
    // The wrap target CS:0000 (linear 0xA000): mov bx,0xBEEF then a HLT,
    // which traps to the monitor (CPL 3) and halts there.
    bus.memory[0xA000..0xA004].copy_from_slice(&[0xbb, 0xef, 0xbe, 0xf4]);
    enter_v86_direct(&mut cpu, 0xFFFE, 0x1000);
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.write_reg16(Reg16::Bx, 0x1111);

    for _ in 0..64 {
        if cpu.run_straight_line(&mut bus, 10_000).unwrap().halted {
            break;
        }
    }

    assert_eq!(
        cpu.read_reg16(Reg16::Ax),
        2,
        "both boundary instructions must execute"
    );
    assert_eq!(
        cpu.read_reg16(Reg16::Bx),
        0xBEEF,
        "execution must continue at CS:0000 after the wrap (eip={:#x}, \
         v86={})",
        cpu.registers.eip,
        cpu.is_v86_mode()
    );
}

/// The metal invariant the VIF-gated-INTA design rests on: a V86 task running at
/// real IOPL 3 owns the real IF, and while the guest holds it clear NOTHING
/// acknowledges the PIC.
///
/// Today's TOKAEMM monitor runs its V86 guest at IOPL 0, so every guest CLI/STI
/// traps to the monitor and the real IF stays pinned open -- the core then takes
/// the IRQ (INTA) immediately and the monitor queues the vector in `vip`. The
/// design deletes that by giving the guest IOPL 3. This fixture certifies the
/// three CPU-level facts that makes possible, none of which involve tokaemm.asm:
///
/// * a guest CLI at IOPL 3 executes rather than faulting, and clears the real IF;
/// * with IF clear, the boundary check never calls `acknowledge_interrupt`, so
///   the bus's pending-IRQ slot is still occupied afterwards. An untaken
///   `Option` IS "no INTA" at this layer: `TestBus::acknowledge_interrupt`
///   `take()`s it, exactly as the real bus pulses INTA at the 8259A;
/// * a guest STI re-arms it, honouring the one-instruction shadow, and the NEXT
///   boundary takes the vector.
///
/// The design leans on all three. If this fails, the design is not implementable
/// as written.
#[test]
fn a_v86_guest_at_iopl3_holds_off_inta_across_its_own_cli_window() {
    // cli ; nop ; sti ; nop ; nop
    let guest = [0xfa, 0x90, 0xfb, 0x90, 0x90];
    let (mut cpu, mut bus) = v86_world(&[0xf4], &guest, &[0x00]);
    enter_v86_direct(&mut cpu, 0, 0x1000);
    // `enter_v86_direct` leaves IOPL 0 (the monitor's shape today). The design's
    // shape is IOPL 3, which is what makes CLI/STI the guest's own.
    cpu.registers.eflags |= FLAG_IOPL;
    cpu.set_flag(FLAG_IF, true);

    // 1. The guest CLI runs and clears the REAL IF; it does not #GP to the monitor.
    cpu.cycle(&mut bus).unwrap();
    assert!(
        cpu.is_v86_mode(),
        "CLI at IOPL 3 must execute in the V86 task, not fault into the monitor"
    );
    assert!(
        !cpu.flag(FLAG_IF),
        "the guest's CLI must clear the real IF at IOPL 3"
    );

    // 2. Raise a PIC line inside the guest's CLI window and step past it. Vector
    //    0x21 is the one NON-#GP vector v86_world gives a present gate (13 has
    //    the other), so a delivery that DID happen lands somewhere observable
    //    instead of faulting, and cannot be confused with a #GP.
    bus.pending_irq = Some(0x21);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eip, 2,
        "the NOP after the CLI must have executed"
    );
    assert_eq!(
        bus.pending_irq,
        Some(0x21),
        "no INTA may reach the chip while the guest holds IF clear: the request \
         must still be pending, unacknowledged"
    );
    assert!(cpu.is_v86_mode(), "and the guest must still be running");

    // 3. The guest's own STI re-opens the window -- but the one-instruction
    //    shadow means the instruction AFTER it still runs first.
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.flag(FLAG_IF), "the guest's STI must set the real IF");
    assert_eq!(
        bus.pending_irq,
        Some(0x21),
        "STI itself must not be the boundary the interrupt is taken at"
    );
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eip, 4,
        "the shadow instruction after STI must execute"
    );
    assert_eq!(
        bus.pending_irq,
        Some(0x21),
        "the STI shadow must still hold the request off"
    );

    // 4. The next boundary takes it -- and THAT is where the INTA happens.
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        bus.pending_irq, None,
        "the boundary after the shadow must acknowledge the request"
    );
    assert!(
        !cpu.is_v86_mode(),
        "delivery out of V86 lands in the ring-0 monitor"
    );
    assert_eq!(cpu.registers.eip, MON_CODE);
    assert_eq!(cpu.registers.cs().selector, R0_CS);
}

// ---------------------------------------------------------------------------
// IDT gate DPL / P-bit conformance (386 PRM, PROTECTED-MODE arm of the
// INT n / INT 3 / INTO operation, dev_docs/reference/i386/i386.txt):
//
//     IF software interrupt (* i.e. caused by INT n, INT 3, or INTO *)
//     THEN
//          IF gate descriptor DPL < CPL
//          THEN #GP(vector number * 8+2+EXT);
//          FI;
//     FI;
//     Gate must be present, else #NP(vector number * 8+2+EXT);
//
// A V86 task is always CPL 3, so it is the sharpest source for the rule: every
// gate `v86_world` builds is DPL 0 (`int_gate` writes access 0x8e), which is
// what real silicon refuses an `INT n` through. These call the delivery entry
// points directly rather than executing `CD 21`, so the gate rule is isolated
// from the separate V86 IOPL gate on `INT n`.
// ---------------------------------------------------------------------------

/// Overwrite the access byte of the IDT gate for `vector`. 0x8e/0xee are a
/// present 32-bit interrupt gate at DPL 0 / DPL 3; clearing bit 7 makes it
/// not-present.
fn set_gate_access(m: &mut [u8], vector: u8, access: u8) {
    m[(IDT + u32::from(vector) * 8 + 5) as usize] = access;
}

/// The IDT-style error code the PRM's `vector number * 8 + 2 + EXT` names.
fn idt_error_code(vector: u8, ext: bool) -> u32 {
    u32::from(vector) * 8 + 2 + u32::from(ext)
}

#[test]
fn v86_software_int_through_a_dpl0_gate_general_protection_faults() {
    // The case found at the 2026-08-18 IOPL-3 final review: a CPL-3 V86 guest's
    // INT n through a DPL-0 gate must #GP on metal; the emulator dispatched it.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xcd, 0x21], &[0x00]);
    enter_v86_direct(&mut cpu, 0x1000, 0x1000);
    assert_eq!(cpu.current_privilege_level(), 3, "a V86 task is CPL 3");

    let result = cpu.software_interrupt(&mut bus, 0x21);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(code),
            }) if code == idt_error_code(0x21, false)
        ),
        "INT 0x21 from CPL 3 through a DPL-0 gate must #GP(0x21*8+2), got {result:?}"
    );
    assert!(
        cpu.is_v86_mode(),
        "the refused delivery leaves the guest in V86"
    );
    assert_eq!(
        cpu.registers.cs().selector,
        0x0A00,
        "and leaves its CS untouched -- no monitor entry happened"
    );
}

#[test]
fn v86_software_int_through_a_dpl3_gate_dispatches() {
    // The non-vacuous partner: the SAME delivery through a DPL-3 gate (the
    // 0xEE posture TOKAEMM's IDT now carries) must still reach the monitor.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xcd, 0x21], &[0x00]);
    set_gate_access(&mut bus.memory, 0x21, 0xee);
    enter_v86_direct(&mut cpu, 0x1000, 0x1000);

    cpu.software_interrupt(&mut bus, 0x21).unwrap();

    assert!(!cpu.is_v86_mode(), "delivery out of V86 enters the monitor");
    assert_eq!(cpu.registers.cs().selector, R0_CS);
    assert_eq!(cpu.registers.eip, MON_CODE);
}

#[test]
fn v86_hardware_interrupt_through_a_dpl0_gate_dispatches() {
    // The EXT exemption, pinned against the identical gate the software test
    // above is refused by: the PRM guards the DPL comparison with "IF software
    // interrupt", so an external delivery through a DPL-0 gate is legal at any
    // CPL. Without this the fix would break every IRQ a V86 guest takes.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    enter_v86_direct(&mut cpu, 0x1000, 0x1000);

    cpu.hardware_interrupt(&mut bus, 0x21).unwrap();

    assert!(!cpu.is_v86_mode(), "the IRQ is delivered into the monitor");
    assert_eq!(cpu.registers.cs().selector, R0_CS);
    assert_eq!(cpu.registers.eip, MON_CODE);
}

#[test]
fn v86_software_int_through_a_not_present_gate_raises_np() {
    // The P-bit check sits OUTSIDE the PRM's "IF software interrupt" guard, so
    // it applies to every source; the gate here is DPL 3 so the DPL rule cannot
    // fire and the #NP is isolated.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xcd, 0x21], &[0x00]);
    set_gate_access(&mut bus.memory, 0x21, 0x6e); // DPL 3, P = 0
    enter_v86_direct(&mut cpu, 0x1000, 0x1000);

    let result = cpu.software_interrupt(&mut bus, 0x21);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 11,
                error_code: Some(code),
            }) if code == idt_error_code(0x21, false)
        ),
        "a not-present gate must #NP(0x21*8+2), got {result:?}"
    );
}

#[test]
fn v86_hardware_interrupt_through_a_not_present_gate_raises_np_with_ext_set() {
    // Same rule, EXT = 1: the PRM's P-bit line carries the same +EXT term as
    // the DPL line, and an external delivery is the one source that sets it.
    // This is the decision the brief asked for: hardware delivery is NOT exempt
    // from the P bit (only from the DPL comparison), so a not-present gate
    // faults for an IRQ too -- with error code vector*8+2+1.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    set_gate_access(&mut bus.memory, 0x21, 0x6e); // DPL 3, P = 0
    enter_v86_direct(&mut cpu, 0x1000, 0x1000);

    let result = cpu.hardware_interrupt(&mut bus, 0x21);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 11,
                error_code: Some(code),
            }) if code == idt_error_code(0x21, true)
        ),
        "an external delivery through a not-present gate must #NP(0x21*8+2+1), got {result:?}"
    );
}

#[test]
fn v86_software_int_through_a_not_present_dpl0_gate_takes_gp_not_np() {
    // Exception priority. Both conditions hold at once; the PRM orders the DPL
    // test BEFORE the presence test, so the guest must see #GP, not #NP. This
    // is what pins the checks to their position in the body -- after the gate
    // is read, before any stack switch or target-segment check.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xcd, 0x21], &[0x00]);
    set_gate_access(&mut bus.memory, 0x21, 0x0e); // DPL 0, P = 0
    enter_v86_direct(&mut cpu, 0x1000, 0x1000);

    let result = cpu.software_interrupt(&mut bus, 0x21);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(code),
            }) if code == idt_error_code(0x21, false)
        ),
        "DPL is tested before P, so this is #GP not #NP, got {result:?}"
    );
}

#[test]
fn v86_int3_through_a_dpl0_gate_general_protection_faults() {
    // "IF software interrupt (* i.e. caused by INT n, INT 3, or INTO *)": the
    // breakpoint vector takes the same rule as INT n. A guest debugger's DPL-0
    // vector-3 gate is exactly the "user code cannot single-step into the
    // kernel handler" case the rule exists for.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xcc], &[0x00]);
    int_gate(&mut bus.memory, 3, MON_CODE); // present, DPL 0
    enter_v86_direct(&mut cpu, 0x1000, 0x1000);

    let result = cpu.software_interrupt(&mut bus, 3);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(code),
            }) if code == idt_error_code(3, false)
        ),
        "INT3 from CPL 3 through a DPL-0 gate must #GP(3*8+2), got {result:?}"
    );
}

#[test]
fn a_software_interrupt_past_the_idt_limit_raises_gp_rather_than_stopping() {
    // 386 PRM, INT n / INT 3 / INTO, protected-mode arm:
    //
    //     IF vector*8+7 > IDT limit THEN #GP(vector*8+2+EXT);
    //
    // An out-of-limit vector is a DELIVERABLE #GP, not a processor abort. A
    // guest whose IDT is short but which covers vector 13 gets its own handler.
    //
    // FOUND BY Zone 66, 2026-08-30. It enters protected mode through VCPI, loads
    // its own 7-entry GDT and 49-vector IDT at ring 0, runs a V86 task of its
    // own, and issues INT 0FDh from it. Its IDT covers vector 13, so on real
    // silicon its own #GP handler runs. This core stopped the machine instead
    // and the game died 3 guest seconds in.
    //
    // `escalate_delivery`'s comment argued the stop was harmless because
    // "escalating would change the outcome only for a guest that keeps a short
    // IDT AND a real #DF handler". That reasoning skips a step: the FIRST
    // escalation from an out-of-limit vector is #GP, not #DF, so a short IDT
    // with a real #GP handler is enough -- and Zone 66 is one.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xcd, 0xfd, 0xf4], &[0x00]);
    enter_v86_direct(&mut cpu, 0, 0x1000);
    // IOPL 3, which `enter_v86_direct` does not set. INT n is IOPL-sensitive in
    // V86: at IOPL < 3 it raises #GP(0) and never reaches the IDT limit check at
    // all, so this test would pass for the wrong reason and read error code 0.
    // TOKAEMM runs its guest at real IOPL 3 for exactly this reason, so IOPL 3
    // is also what Zone 66 actually runs at.
    cpu.registers.eflags |= 0x3000;
    // Covers vector 13 (needs 13*8+7 = 111 = 0x6f) but NOT vector 0xfd.
    cpu.idtr.limit = 0x6f;

    cpu.run_budgeted(&mut bus, 10_000).expect("no hard stop");

    assert!(!cpu.is_v86_mode(), "the #GP handler runs at ring 0");
    assert_eq!(cpu.registers.eip, MON_CODE, "the guest's own #GP gate ran");
    let rd = |o: u32| u32::from_le_bytes(cpu_mem(&bus, cpu.registers.esp() + o));
    assert_eq!(
        rd(0),
        0xfd * 8 + 2,
        "error code is vector*8+2: the IDT bit set, EXT clear for a software INT"
    );
}

#[test]
fn an_out_of_limit_vector_whose_gp_gate_is_also_missing_ends_in_a_triple_fault() {
    // The other arm, and the reason the old stop looked adequate. When the IDT
    // is too short to hold vector 13 either, the #GP escalates to #DF, the #DF
    // cannot be delivered, and the processor shuts down. The machine still
    // stops -- but through the architectural chain, not a bespoke error.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xcd, 0xfd, 0xf4], &[0x00]);
    enter_v86_direct(&mut cpu, 0, 0x1000);
    cpu.registers.eflags |= 0x3000; // IOPL 3; see the test above
    cpu.idtr.limit = 0x20;

    let result = cpu.run_budgeted(&mut bus, 10_000);

    assert!(
        matches!(result, Err(CpuError::TripleFault { .. })),
        "{result:?}"
    );
}
