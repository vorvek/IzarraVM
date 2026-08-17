// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Which control transfer drives a hardware task switch. The three differ in
/// busy-bit handling, back-link/NT writing, and the incoming TSS type they
/// accept (SDM table 8-2); see `task_switch`.
#[derive(Clone, Copy, PartialEq)]
enum TaskSwitchKind {
    Jump,
    Call,
    Return,
}

impl CpuGsw {
    pub(super) fn software_interrupt<B: CpuBus>(
        &mut self,
        bus: &mut B,
        vector: u8,
    ) -> ExecResult<()> {
        bus.interrupt_acknowledge(vector, self.read_gpr16(0))?;
        if self.is_protected_mode() {
            self.deliver_exception(bus, vector, None, true)
        } else {
            self.real_mode_interrupt(bus, vector)
        }
    }

    fn real_mode_interrupt<B: CpuBus>(&mut self, bus: &mut B, vector: u8) -> ExecResult<()> {
        // Settle deferred arithmetic flags so the eflags image pushed for the handler is live.
        self.materialize_flags();
        self.push(bus, self.registers.eflags as u16 as u32, OperandSize::Word)?;
        self.push(
            bus,
            u32::from(self.registers.cs().selector),
            OperandSize::Word,
        )?;
        self.push(bus, self.registers.eip as u16 as u32, OperandSize::Word)?;
        self.set_flag(FLAG_IF | FLAG_TF, false);
        let vector_address = u32::from(vector) * 4;
        let ip = bus.read_memory(vector_address, BusWidth::Word, BusAccessKind::DataRead)? as u16;
        let cs =
            bus.read_memory(vector_address + 2, BusWidth::Word, BusAccessKind::DataRead)? as u16;
        self.load_segment_real(SegmentIndex::Cs, cs);
        self.set_eip(u32::from(ip));
        Ok(())
    }

    // Hardware interrupt entry. Unlike software_interrupt it does NOT call
    // interrupt_acknowledge: that hook is the software-INT device side-effect path
    // (the video mode-set), not the INTA handshake, which the PIC handled already.
    pub(super) fn hardware_interrupt<B: CpuBus>(
        &mut self,
        bus: &mut B,
        vector: u8,
    ) -> ExecResult<()> {
        if self.is_protected_mode() {
            self.deliver_exception(bus, vector, None, true)
        } else {
            self.real_mode_interrupt(bus, vector)
        }
    }

    /// T1.5 diagnostic: log a `#UD` (vector 6) at the moment it is about to be
    /// reflected to the guest's own IDT handler. Two real V86 EMM managers
    /// (386MAX, JEMMEX) raise `#UD` during their own init and spin forever in
    /// their handler; the CPU reflects it cleanly (no fatal `CpuError`), so the
    /// existing `IZARRAVM_FAULT_TRACE` machine-level trace (which only fires on
    /// a fatal `CpuError` or the 0xE6 `CMD_EXIT` port) prints nothing. This is
    /// the single choke point for every #UD raise site (decoder
    /// unimplemented-opcode fallback and every semantic #UD check): by the time
    /// `deliver_exception` runs, `finish_instruction` has already rewound
    /// `eip`/`cs` to the faulting instruction's first byte (see
    /// `finish_instruction`), so `self.registers.cs()/eip` here IS the actual
    /// faulting guest CS:IP, not a monitor's. Gated on `ud_trace_enabled()`
    /// (same `IZARRAVM_FAULT_TRACE` env var as the machine crate's fault
    /// trace); a no-op call on the cold vector-6-only path when off.
    fn trace_ud_if_enabled<B: CpuBus>(&mut self, bus: &mut B) {
        self.trace_fault_if_enabled(bus, 6, None);
    }

    /// The same diagnostic generalized: log a guest-bound exception delivery
    /// with the faulting CS:IP and raw instruction bytes. Wired for #UD (6) and
    /// #GP (13), the two vectors the game bring-up loop keeps needing. #PF is
    /// deliberately NOT traced: CWSDPMI services page faults constantly and the
    /// spam would bury the signal.
    fn trace_fault_if_enabled<B: CpuBus>(
        &mut self,
        bus: &mut B,
        vector: u8,
        error_code: Option<u32>,
    ) {
        if !ud_trace_enabled() {
            return;
        }
        let cs = self.registers.cs();
        let eip = self.registers.eip;
        // Read the raw bytes at the faulting linear address fresh (rather than
        // reusing whatever the decoder buffered) so this covers BOTH #UD
        // origins uniformly: the decoder's unimplemented-opcode fallback and a
        // semantic #UD raised after decode. Best-effort: stop at the first
        // unreadable byte (e.g. a page boundary fault) rather than erroring --
        // this is a diagnostic read, it must never itself fault the guest.
        const MAX_BYTES: u32 = 12;
        let mut bytes = Vec::with_capacity(MAX_BYTES as usize);
        for i in 0..MAX_BYTES {
            let linear = cs.base.wrapping_add(eip).wrapping_add(i);
            let Ok(phys) = self.translate_linear(bus, linear, false) else {
                break;
            };
            let Ok(byte) =
                bus.read_memory(phys, BusWidth::Byte, BusAccessKind::InstructionPrefetch)
            else {
                break;
            };
            bytes.push(byte as u8);
        }
        let byte_str = bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let name = match vector {
            6 => "#UD",
            11 => "#NP",
            12 => "#SS",
            13 => "#GP",
            other => return eprintln!("fault trace: unexpected trace vector {other}"),
        };
        let ec = error_code.map_or(String::new(), |e| format!(" ec={e:#06x}"));
        eprintln!(
            "fault trace: {name} at CS:IP={:#06x}:{:#010x}{ec} bytes=[{byte_str}] \
             cr0={:#010x} eflags={:#010x} vm={} cpl={}",
            cs.selector,
            eip,
            self.control.cr0,
            self.registers.eflags,
            self.is_v86_mode(),
            self.current_privilege_level(),
        );
        // A selector-format error code names a descriptor; dump it (plus the
        // vector-13 IDT gate, the CS descriptor, and the stack top) so a
        // segment-load #GP shows WHY without a rebuild. This is what pinned
        // the DPMI32VM ring-transition bug: the saved-DS slot on the ring-3
        // stack held a ring-0 selector the IRET should have nulled.
        if let Some(ec) = error_code {
            let index = ec & 0xFFF8;
            let (table, base) = if ec & 0x4 != 0 {
                ("LDT", self.ldtr.base)
            } else {
                ("GDT", self.gdtr.base)
            };
            let mut desc = [0u8; 8];
            for (i, b) in desc.iter_mut().enumerate() {
                if let Ok(phys) = self.translate_linear(bus, base + index + i as u32, false)
                    && let Ok(v) = bus.read_memory(phys, BusWidth::Byte, BusAccessKind::DataRead)
                {
                    *b = v as u8;
                }
            }
            eprintln!(
                "fault trace: {table} base={base:#010x} desc[{index:#x}]={desc:02x?} gdtr={:#010x}/{:#06x}",
                self.gdtr.base, self.gdtr.limit
            );
            // Neighborhood dump: stale-mapping garbage shows as a whole run of
            // non-descriptor bytes, a single clobbered entry as one bad row.
            for row in 0..4u32 {
                let addr = base + index + row * 8 - 8;
                let mut d = [0u8; 8];
                let mut phys0 = 0;
                for (i, b) in d.iter_mut().enumerate() {
                    if let Ok(phys) = self.translate_linear(bus, addr + i as u32, false) {
                        if i == 0 {
                            phys0 = phys;
                        }
                        if let Ok(v) =
                            bus.read_memory(phys, BusWidth::Byte, BusAccessKind::DataRead)
                        {
                            *b = v as u8;
                        }
                    }
                }
                eprintln!(
                    "fault trace: {table}[{:#x}] @lin {addr:#010x} phys {phys0:#010x} = {d:02x?}",
                    index + row * 8 - 8
                );
            }
            let cs_sel = u32::from(self.registers.cs().selector);
            let (tb, tag) = if cs_sel & 4 != 0 {
                (self.ldtr.base, "LDT")
            } else {
                (self.gdtr.base, "GDT")
            };
            let targets = [
                ("IDT gate 13", self.idtr.base + 13 * 8),
                ("IDT gate 12", self.idtr.base + 12 * 8),
                ("IDT gate 11", self.idtr.base + 11 * 8),
                ("CS desc", tb + (cs_sel & 0xFFF8)),
            ];
            for (label, addr) in targets {
                let mut d = [0u8; 8];
                for (i, b) in d.iter_mut().enumerate() {
                    if let Ok(phys) = self.translate_linear(bus, addr + i as u32, false)
                        && let Ok(v) =
                            bus.read_memory(phys, BusWidth::Byte, BusAccessKind::DataRead)
                    {
                        *b = v as u8;
                    }
                }
                eprintln!("fault trace: {label} ({tag}) @{addr:#010x} = {d:02x?}");
            }
            let ss_reg = self.registers.segment(SegmentIndex::Ss);
            eprintln!(
                "fault trace: ldtr sel={:#06x} base={:#010x} tr sel={:#06x} ss={:#06x} esp={:#010x} \
                 ss.base={:#010x} ss.limit={:#010x} ss.acc={:02x} ss.b32={}",
                self.ldtr.selector,
                self.ldtr.base,
                self.tr.selector,
                ss_reg.selector,
                self.registers.esp(),
                ss_reg.base,
                ss_reg.limit,
                ss_reg.access,
                ss_reg.default_size_32,
            );
            let ss_base = self.registers.segment(SegmentIndex::Ss).base;
            let start = ss_base + (self.registers.esp() & !0xF).saturating_sub(16);
            let mut stack = [0u8; 96];
            for (i, b) in stack.iter_mut().enumerate() {
                if let Ok(phys) = self.translate_linear(bus, start + i as u32, false)
                    && let Ok(v) = bus.read_memory(phys, BusWidth::Byte, BusAccessKind::DataRead)
                {
                    *b = v as u8;
                }
            }
            for (row, chunk) in stack.chunks(16).enumerate() {
                let words: Vec<String> = chunk
                    .chunks(2)
                    .map(|w| format!("{:02x}{:02x}", w[1], w[0]))
                    .collect();
                eprintln!(
                    "fault trace: stack {:#010x}: {}",
                    start + row as u32 * 16,
                    words.join(" ")
                );
            }
        }
    }

    pub(super) fn deliver_exception<B: CpuBus>(
        &mut self,
        bus: &mut B,
        vector: u8,
        error_code: Option<u32>,
        // External hardware interrupts and software `INT n` never push an error code,
        // even on a vector that a CPU exception would carry one for (e.g. IRQ0 remapped
        // to vector 8, #DF). Only a genuine CPU exception pushes one.
        is_external: bool,
    ) -> ExecResult<()> {
        if vector == 6 && !is_external {
            self.trace_ud_if_enabled(bus);
        }
        // #GP deliveries bound for a protected-mode guest handler (the V86 ones
        // are the monitor's routine trap traffic - hundreds of thousands per
        // second - so only trace when the guest is NOT in V86).
        if (vector == 13 || vector == 11 || vector == 12) && !is_external && !self.is_v86_mode() {
            self.trace_fault_if_enabled(bus, vector, error_code);
        }
        if !self.is_protected_mode() {
            return self.software_interrupt(bus, vector);
        }

        self.deliver_exception_inner(bus, vector, error_code, is_external)
    }

    /// Guard around the delivery body: a fault raised while the frame is being
    /// built unwinds with `self.cpl` already set to the target level (see the
    /// PRM-transition-point note in the body) and, for a V86 source, VM already
    /// dropped. The nested exception then re-enters delivery from the restored
    /// interrupted state; without this restore the retried delivery sees the
    /// target CPL, skips the ring cross, and builds the handler frame on the
    /// interrupted ring's stack (the Tyrian / DPMI16BI corruption).
    fn deliver_exception_inner<B: CpuBus>(
        &mut self,
        bus: &mut B,
        vector: u8,
        error_code: Option<u32>,
        is_external: bool,
    ) -> ExecResult<()> {
        // Snapshot everything the body mutates before its final CS:EIP commit:
        // CPL, EFLAGS (VM drop, NT/TF/IF clears), the inner SS:ESP installed by
        // `switch_to_inner_stack`, and the V86 data segments nulled on monitor
        // entry. A partial restore (CPL alone) leaves the retried delivery
        // capturing the inner ring-0 stack as the "interrupted" SS:ESP.
        // Settle deferred arithmetic flags FIRST so the EFLAGS snapshot below is
        // the live image; the body's own materialize call is then a no-op. A
        // pre-materialization snapshot would restore a stale image after the
        // lazy-flag state has been consumed.
        self.materialize_flags();
        let entry_cpl = self.cpl;
        let entry_eflags = self.registers.eflags;
        let entry_esp = self.registers.esp();
        let entry_segments = [
            SegmentIndex::Ss,
            SegmentIndex::Ds,
            SegmentIndex::Es,
            SegmentIndex::Fs,
            SegmentIndex::Gs,
        ]
        .map(|segment| (segment, self.registers.segment(segment)));
        let result = self.deliver_exception_body(bus, vector, error_code, is_external);
        if result.is_err() {
            self.cpl = entry_cpl;
            self.registers.eflags = entry_eflags;
            self.registers.set_esp(entry_esp);
            for (segment, register) in entry_segments {
                self.registers.set_segment(segment, register);
            }
        }
        result
    }

    fn deliver_exception_body<B: CpuBus>(
        &mut self,
        bus: &mut B,
        vector: u8,
        error_code: Option<u32>,
        is_external: bool,
    ) -> ExecResult<()> {
        let gate_address = self.idtr.base + u32::from(vector) * 8;
        if u32::from(self.idtr.limit) < u32::from(vector) * 8 + 7 {
            return Err(CpuError::IdtLimit { vector }.into());
        }

        let gate_low = self.read_system_linear_u32(bus, gate_address)?;
        let gate_high = self.read_system_linear_u32(bus, gate_address + 4)?;
        let selector = ((gate_low >> 16) & 0xffff) as u16;
        // Gate types: 0x6/0xe are interrupt gates (IF-clearing), 0x7/0xf trap
        // gates; bit 3 is the gate width. A 16-bit gate (Borland DPMI16BI hangs
        // its whole IDT off type 6) builds a WORD frame, and its high-offset
        // word is reserved: only the low word reaches EIP.
        let gate_type = (gate_high >> 8) & 0x0f;
        let is_interrupt_gate = gate_type & 0x07 == 0x06;
        let gate_is_32 = gate_type & 0x08 != 0;
        // The V86-source frame is ALWAYS dword-sized: the PRM's
        // INTERRUPT-FROM-V86-MODE arm has no 16-bit variant (every push is
        // "padded to two words"), unlike the inner/same-privilege arms, which
        // fork on the gate width. A word frame here would truncate the guest's
        // ESP with no way back on IRET.
        let frame_size = if gate_is_32 || self.is_v86_mode() {
            OperandSize::Dword
        } else {
            OperandSize::Word
        };
        let offset = if gate_is_32 {
            (gate_low & 0x0000_ffff) | (gate_high & 0xffff_0000)
        } else {
            gate_low & 0x0000_ffff
        };

        // Task gate (type 0x5): the gate's selector names a TSS, not a code
        // segment, and delivery is a CALL-style hardware task switch (386 PRM
        // 9.5): back-link written, NT set, the interrupted TSS stays busy. No
        // frame is pushed -- the outgoing state, VM flag included, is saved
        // into the outgoing TSS by the switch -- so this branch runs BEFORE the
        // V86 drop below. Only the error code lands on the NEW task's stack.
        // DOS/4GW 1.97 points #PF at one of these.
        if (gate_high >> 8) & 0x1f == 0x05 {
            self.task_switch(bus, selector, TaskSwitchKind::Call)?;
            if !is_external && vector_pushes_error_code(vector) {
                self.push(bus, error_code.unwrap_or(0), OperandSize::Dword)?;
            }
            return Ok(());
        }

        // Settle deferred arithmetic flags so the eflags image pushed for the handler is live.
        self.materialize_flags();
        let saved_eflags = self.registers.eflags;
        let saved_cs = self.registers.cs().selector;
        let source_v86 = self.is_v86_mode();
        // A V86 IP is 16-bit at every architectural point, so real silicon can
        // never push a frame EIP with a nonzero high word. Emulator-side EIP
        // arithmetic can still leak one (an o32 transfer target past the limit
        // is only caught at the NEXT fetch, with the oversize target live), and
        // a V86 monitor's word-sized frame writes then preserve the high half:
        // TOKAEMM reflected such a frame until its own return IRETD #GP(0)'d at
        // ring 0 (the stage-1 G1 storm, fed by exactly this push). Mask to the
        // only image silicon could produce.
        let saved_eip = if source_v86 {
            self.registers.eip & 0xffff
        } else {
            self.registers.eip
        };
        let cpl = self.current_privilege_level();

        // Drop V86 up front so every segment loaded from here on (the inner SS from the
        // TSS, then CS) is decoded as a protected-mode descriptor rather than an 8086
        // base = selector << 4. The pushed EFLAGS image already captured VM=1 above.
        if source_v86 {
            self.registers.eflags &= !FLAG_VM;
        }

        // The target CS descriptor's DPL decides whether the entry crosses to an inner
        // ring; a V86 source always crosses (a V86 task runs at CPL 3 and the monitor
        // handler at ring 0).
        let (_tl, th) = self.read_transfer_descriptor(bus, selector)?;
        let target_access = (th >> 8) & 0xff;
        let target_dpl = ((target_access >> 5) & 3) as u8;
        let crosses_ring = source_v86 || target_dpl < cpl;

        // PRM transition point, and the actual fix this field exists for: the entered
        // level is set here, BEFORE the frame-push sequence begins, not after CS is
        // loaded at the end of this function. Every push below (the outer SS:ESP, the
        // V86 data segments, EFLAGS/CS/EIP, the error code) must execute as the level
        // the handler is entering, not as whatever the source CS's selector bits say --
        // for a V86 source those bits are the guest's own real-mode-style CS (arbitrary
        // low bits, e.g. the DOS HMA stub at 0xFFFF) and are never the CPL the monitor's
        // own stack accesses run under. Setting the cache here, ahead of `push`, is what
        // makes `translate_linear_checked`'s `PagingAccessor::Current` classify these
        // pushes as supervisor instead of spuriously faulting them as user.
        self.cpl = if crosses_ring { target_dpl } else { cpl };

        if crosses_ring {
            // Inter-privilege entry: load the inner stack from the TSS, then push the
            // outer SS:ESP so IRET can restore it. For a V86 source the four data
            // segments are pushed above SS:ESP (the V86 interrupt frame) and the CPU
            // returns to real-mode-style segments on IRET.
            let (ds, es, fs, gs) = (
                self.registers.segment(SegmentIndex::Ds).selector,
                self.registers.segment(SegmentIndex::Es).selector,
                self.registers.segment(SegmentIndex::Fs).selector,
                self.registers.segment(SegmentIndex::Gs).selector,
            );
            let (old_ss, old_esp) = self.switch_to_inner_stack(bus, target_dpl)?;
            if source_v86 {
                self.push(bus, u32::from(gs), frame_size)?;
                self.push(bus, u32::from(fs), frame_size)?;
                self.push(bus, u32::from(ds), frame_size)?;
                self.push(bus, u32::from(es), frame_size)?;
            }
            self.push(bus, u32::from(old_ss), frame_size)?;
            self.push(bus, old_esp, frame_size)?;
        }
        self.push(bus, saved_eflags, frame_size)?;
        self.push(bus, u32::from(saved_cs), frame_size)?;
        self.push(bus, saved_eip, frame_size)?;
        // The error code is pushed only for a CPU exception on a vector that carries one
        // (8 #DF, 10 #TS, 11 #NP, 12 #SS, 13 #GP, 14 #PF, 17 #AC) — never for an external
        // hardware interrupt or software `INT n`, even when it lands on such a vector.
        if !is_external && vector_pushes_error_code(vector) {
            // TOKAEMM's vec13 discriminator (emulator contract) only ever inspects a
            // V86-ORIGIN vector-13 delivery: a V86 sensitive-instruction #GP (always
            // error code 0 -- there is no selector to blame, the fault is on the
            // instruction itself) or a real IRQ5 reflected onto the same vector. It is
            // never reached by a PROTECTED-MODE selector #GP, which is delivered through
            // the CURRENT IDT -- the guest's own (e.g. DOS4GW's), not TOKAEMM's monitor
            // -- and can legitimately carry a nonzero selector-index error code (Batch
            // A's descriptor/gate/segment-load sweep). So the tripwire is scoped to
            // `source_v86`: a V86-origin #GP must still push exactly 0, but a pmode
            // selector #GP is free to carry a real code. Update tokaemm.asm's
            // vec13_entry BEFORE relaxing the V86-origin half of this.
            debug_assert!(
                !source_v86 || vector != 13 || error_code.unwrap_or(0) == 0,
                "V86-origin vector-13 #GP with a nonzero error code ({error_code:?}) \
                 breaks the TOKAEMM vec13 frame-shape discriminator"
            );
            self.push(bus, error_code.unwrap_or(0), frame_size)?;
        }

        // Entering the handler clears VM/NT/TF; an interrupt gate (not a trap gate) also
        // clears IF.
        self.set_flag(FLAG_VM | FLAG_NT | FLAG_TF, false);
        if is_interrupt_gate {
            self.set_flag(FLAG_IF, false);
        }
        // Leaving V86 drops the guest's real-mode data segments; the handler starts with
        // null selectors and reloads its own.
        if source_v86 {
            self.load_segment_real(SegmentIndex::Ds, 0);
            self.load_segment_real(SegmentIndex::Es, 0);
            self.load_segment_real(SegmentIndex::Fs, 0);
            self.load_segment_real(SegmentIndex::Gs, 0);
        }
        if let Err(fault) = self.load_segment(bus, SegmentIndex::Cs, selector) {
            if ud_trace_enabled() {
                eprintln!(
                    "fault trace: deliver v{vector}: handler CS load failed: {fault:?} \
                     gate={gate_high:#010x}:{gate_low:#010x} sel={selector:#06x} off={offset:#010x} \
                     gdtr={:#010x}/{:#06x} ldtr sel={:#06x} base={:#010x} limit={:#06x}",
                    self.gdtr.base,
                    self.gdtr.limit,
                    self.ldtr.selector,
                    self.ldtr.base,
                    self.ldtr.limit,
                );
            }
            return Err(fault);
        }
        self.set_eip(offset);
        // Count each vector-13 delivery from V86 as one TOKAEMM monitor trip.
        // Sensitive-instruction #GP faults and real IRQ5 both use this vector.
        if source_v86 && vector == 13 {
            self.perf.monitor_trips_vec13 += 1;
        }
        Ok(())
    }

    /// Dword form of `read_system_linear`.
    fn read_system_linear_u32<B: CpuBus>(&mut self, bus: &mut B, linear: u32) -> ExecResult<u32> {
        self.read_system_linear(bus, linear, BusWidth::Dword)
    }

    /// Read a byte, word or dword from a linear address through paging with
    /// implicit-supervisor semantics (see `translate_linear_system`). Used for IDT/GDT/LDT
    /// descriptor reads, TSS field access and the TSS I/O permission bitmap, none of which
    /// are checked against the current CPL.
    ///
    /// `read_memory_direct`, not `read_memory`: system structures (GDT/LDT/IDT, the TSS
    /// and its I/O bitmap) live in plain RAM in every guest that boots, and the direct arm
    /// skips the per-call device-window probing `read_memory` does before it reaches the
    /// same slice. The arm is entered only when `direct_page_ram_bytes` succeeds, and it
    /// charges the same `data_access_wait_states` + `trace.record` pair at the same
    /// address as `read_memory`'s direct-RAM arm; the misaligned arm charges
    /// `charge_direct_ram_split`. Anything else falls through to `read_memory` unchanged,
    /// so the charge is bit-identical either way.
    ///
    /// The `direct` flag is dropped rather than fed to `record_data_read`: system reads have
    /// never contributed to `data_direct_reads`/`data_slow_reads`, and those counters are
    /// pinned by the census and bench JSON, so accounting for them here would move them.
    pub(super) fn read_system_linear<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        width: BusWidth,
    ) -> ExecResult<u32> {
        let physical = self.translate_linear_system(bus, linear, false)?;
        Ok(bus
            .read_memory_direct(physical, width, BusAccessKind::DataRead)?
            .value)
    }

    /// Write a value to a linear address through paging with implicit-supervisor
    /// semantics. See `read_system_linear_u32`. Used for TSS busy-bit updates and
    /// page-table-adjacent bookkeeping (accessed/dirty bits) done on the guest's
    /// behalf while servicing a system-structure access.
    fn write_system_linear<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        width: BusWidth,
        value: u32,
    ) -> ExecResult<()> {
        let physical = self.translate_linear_system(bus, linear, true)?;
        bus.write_memory(physical, width, value, BusAccessKind::DataWrite)?;
        Ok(())
    }

    /// Shared flag-load for POPF/POPFD and every IRET/IRETD return form, including the
    /// dedicated return-into-V86 branch. Per the 386 PRM (POPF/POPFD, "386 DX Microprocessor
    /// Instruction Set", opcode 9Dh, p.17-136): "The I/O privilege level is altered only when
    /// executing at privilege level 0. The interrupt flag is altered only when executing at a
    /// level at least as privileged as the I/O privilege level... bits 16 and 17 [VM and RF]
    /// are not affected." IRET carries the identical IOPL/IF rule (section 9.7.1.2, p.9-37):
    /// "The IOPL field of the EFLAGS register is restored only if the CPL is 0. The IF flag is
    /// changed only if CPL <= IOPL."
    ///
    /// `self.cpl` at every call site is still the *pre-transition* privilege level (the
    /// same-privilege IRET forms load flags before touching `self.cpl`), so a plain read of
    /// `current_privilege_level()` here is exactly the "executing at" CPL the PRM means.
    ///
    /// In V86, `check_v86_iopl` traps POPF/POPFD/IRET upstream whenever IOPL < 3, so the only
    /// V86 case that ever reaches this function via `allow_vm_load == false` has IOPL == 3,
    /// CPL == 3 <= IOPL, and the IF gate is a no-op (IF always loads).
    ///
    /// `allow_vm_load` is true only for the ring-0 IRETD-into-V86 branch, which is the one
    /// path allowed to set VM (CPL 0 there, matching the PRM's IOPL-restore gate); every other
    /// caller (POPF/POPFD and the same-privilege/inter-privilege IRET forms) passes false so
    /// VM keeps its live value, per the PRM text above.
    pub(super) fn load_flags(
        &mut self,
        value: u32,
        operand_size: OperandSize,
        allow_vm_load: bool,
    ) {
        let cpl = self.current_privilege_level();
        let old = self.registers.eflags;
        let mut merged = match operand_size {
            OperandSize::Word => (old & 0xffff_0000) | (value & 0xffff) | 0x2,
            OperandSize::Dword => value | 0x2,
        };
        // IOPL: only a CPL-0 load may change it; otherwise keep the live value.
        if cpl != 0 {
            merged = (merged & !FLAG_IOPL) | (old & FLAG_IOPL);
        }
        // IF: only alterable when CPL <= IOPL (checked against the *live* IOPL, which is the
        // value just settled above); otherwise keep the live value.
        let effective_iopl = ((merged >> 12) & 3) as u8;
        if cpl > effective_iopl {
            merged = (merged & !FLAG_IF) | (old & FLAG_IF);
        }
        // VM: masked back to its live value everywhere except the dedicated IRETD-into-V86
        // caller, which passes the popped VM=1 through on purpose.
        if !allow_vm_load {
            merged = (merged & !FLAG_VM) | (old & FLAG_VM);
        }
        // AC is a 486 addition. ID is writable only on the CPUID-capable P55C persona.
        // Unsupported flag bits retain their live value, which is zero in the reset image.
        let fixed = match self.persona() {
            CpuPersona::I386 => FLAG_AC | FLAG_ID,
            CpuPersona::I486 => FLAG_ID,
            CpuPersona::I586 => 0,
        };
        merged = (merged & !fixed) | (old & fixed);
        self.registers.eflags = merged;
        // The loaded image is the new truth for every flag bit; any deferred descriptor would
        // otherwise override the arithmetic bits we just wrote.
        self.pending_flags = PendingFlags::default();
        // The dword form can change EFLAGS.AC (the word form keeps the high half).
        self.recompute_alignment_armed();
    }

    /// Restartability wrapper: a fault raised after the frame pops committed
    /// (typically #NP on the target CS -- Borland RTM returns into swapped-out
    /// overlay segments and re-executes after its handler loads them) must
    /// leave (E)SP exactly pre-instruction, or the restarted pop reads above
    /// the real frame. `finish_instruction` rewinds only EIP/CS.
    pub(super) fn iret<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand_size: OperandSize,
    ) -> ExecResult<()> {
        let esp_before = self.registers.esp();
        let result = self.iret_body(bus, operand_size);
        if result.is_err() {
            self.registers.set_esp(esp_before);
        }
        result
    }

    fn iret_body<B: CpuBus>(&mut self, bus: &mut B, operand_size: OperandSize) -> ExecResult<()> {
        // NT set in protected mode: this IRET ends a nested TASK, not an
        // interrupt frame (386 PRM 9.5). Return through the current TSS's
        // back-link; nothing is popped. The outgoing image is saved with NT=0
        // (SDM table 8-2), so NT clears before the switch saves state. V86 is
        // excluded: a V86 IRET at IOPL 3 is a plain frame pop (the monitor
        // never hands a V86 task NT=1), and below IOPL 3 it trapped upstream.
        if self.is_protected_mode() && !self.is_v86_mode() && self.registers.eflags & FLAG_NT != 0 {
            let back_link = self.read_system_linear(bus, self.tr.base, BusWidth::Word)? as u16;
            // NT is cleared inside `task_switch`, after the back-link validates:
            // a #TS on a bad back-link must leave NT set for restartability.
            return self.task_switch(bus, back_link, TaskSwitchKind::Return);
        }
        match operand_size {
            OperandSize::Word => {
                let ip = self.pop(bus, OperandSize::Word)?;
                let cs = self.pop(bus, OperandSize::Word)? as u16;
                let flags = self.pop(bus, OperandSize::Word)?;
                if self.is_protected_mode()
                    && !self.is_v86_mode()
                    && (cs & 3) as u8 > self.current_privilege_level()
                {
                    // Inter-privilege word return: the 16-bit mirror of the dword
                    // arm below. Borland's DPMI16BI returns its ring-0 INT 31h
                    // handler to the ring-3 client this way; without the SS:SP pop
                    // the client keeps the host's ring-0 SS and its first
                    // PUSH SS / POP ES faults #GP (the exodos Tyrian crash). The
                    // V86 guard mirrors the dword arm's: a V86 CS has arbitrary
                    // low bits, and real mode never reaches this ring check.
                    let sp = self.pop(bus, OperandSize::Word)?;
                    let ss = self.pop(bus, OperandSize::Word)? as u16;
                    // The outer SS's RPL must equal the return CS's RPL (386 PRM
                    // IRET, outer level: "Selector RPL must be equal to the RPL
                    // of the return CS selector ELSE #GP(SS selector)").
                    if (ss & 3) != (cs & 3) {
                        return Err(InternalFault::Exception {
                            vector: 13,
                            error_code: Some(selector_error_code(ss, ss & 0x4 != 0, false)),
                        });
                    }
                    self.load_segment(bus, SegmentIndex::Cs, cs)?;
                    // Mechanical SS load under the pre-IRET CPL; see the dword arm
                    // and `load_segment_system`.
                    self.load_segment_system(bus, SegmentIndex::Ss, ss)?;
                    self.set_eip(ip & 0xffff);
                    // A word frame carries no ESP high half: a B=0 outer stack
                    // takes the popped value into SP only; a B=1 outer stack
                    // zero-extends it (the inner stack's high word never leaks
                    // through a word-sized frame).
                    if self.stack_is_32bit() {
                        self.registers.set_esp(sp & 0xffff);
                    } else {
                        self.write_gpr16(4, sp as u16);
                    }
                    self.load_flags(flags, OperandSize::Word, false);
                    // PRM transition point: the target RPL (non-V86, checked
                    // above) is the new CPL.
                    self.cpl = (cs & 3) as u8;
                    self.invalidate_data_segments_below_cpl();
                    return Ok(());
                }
                self.load_segment(bus, SegmentIndex::Cs, cs)?;
                self.set_eip(ip & 0xffff);
                self.load_flags(flags, OperandSize::Word, false);
                // Same-privilege / real-mode / V86 word return; the target level is
                // exactly the just-loaded CS's RPL, or 3 if that load landed in V86
                // -- but real mode is unconditionally CPL 0 regardless of the
                // selector's low bits (a real-mode CS is not a descriptor selector,
                // so those bits carry no RPL).
                self.cpl = if !self.is_protected_mode() {
                    0
                } else if self.is_v86_mode() {
                    3
                } else {
                    (cs & 3) as u8
                };
            }
            OperandSize::Dword => {
                let esp_before = self.registers.esp();
                let eip = self.pop(bus, OperandSize::Dword)?;
                let cs = self.pop(bus, OperandSize::Dword)? as u16;
                let flags = self.pop(bus, OperandSize::Dword)?;

                if self.current_privilege_level() == 0 && flags & FLAG_VM != 0 {
                    // 386 PRM STACK-RETURN-TO-V86: "instruction pointer not within code
                    // segment limit THEN #GP(0)" is checked against the popped EIP before
                    // EFLAGS/CS/EIP/ESP or any of the V86 data segments are committed -- the
                    // pseudocode gates on it ahead of every `Pop()` in the V86-tail sequence.
                    // A V86 CS is always a real-mode-style segment (fixed 0xffff limit via
                    // `load_segment_real`/`SegmentRegister::real`), so an EIP with a nonzero
                    // high word is always out of range. Faulting here -- with the monitor's
                    // pre-IRET CS:EIP still live and the V86-tail dwords (ESP/SS/ES/DS/FS/GS)
                    // still on the stack -- keeps the #GP(0) frame correct; committing the
                    // V86 state first and letting the next fetch discover the limit violation
                    // would push a fabricated V86 return address (this exact IRET's popped
                    // CS:EIP) into the fault frame instead of resuming the monitor right after
                    // its own IRET, with its own stack intact.
                    if eip > 0xffff {
                        // A fault leaves the instruction restartable: undo the three pops
                        // so the monitor's ESP is exactly pre-IRET (finish_instruction only
                        // rewinds EIP/CS, never ESP).
                        self.registers.set_esp(esp_before);
                        return Err(InternalFault::Exception {
                            vector: 13,
                            error_code: Some(0),
                        });
                    }

                    // Return INTO a V86 task: pop the V86 tail and reload real-mode segments.
                    let esp = self.pop(bus, OperandSize::Dword)?;
                    let ss = self.pop(bus, OperandSize::Dword)? as u16;
                    let es = self.pop(bus, OperandSize::Dword)? as u16;
                    let ds = self.pop(bus, OperandSize::Dword)? as u16;
                    let fs = self.pop(bus, OperandSize::Dword)? as u16;
                    let gs = self.pop(bus, OperandSize::Dword)? as u16;
                    self.load_flags(flags, OperandSize::Dword, true); // flags carry VM=1 (guarded above)
                    self.load_segment_real(SegmentIndex::Cs, cs);
                    self.load_segment_real(SegmentIndex::Ss, ss);
                    self.load_segment_real(SegmentIndex::Ds, ds);
                    self.load_segment_real(SegmentIndex::Es, es);
                    self.load_segment_real(SegmentIndex::Fs, fs);
                    self.load_segment_real(SegmentIndex::Gs, gs);
                    self.set_eip(eip);
                    self.registers.set_esp(esp);
                    // PRM transition point: IRET-into-V86 always lands at CPL 3.
                    self.cpl = 3;
                    return Ok(());
                }

                if self.is_protected_mode()
                    && !self.is_v86_mode()
                    && (cs & 3) as u8 > self.current_privilege_level()
                {
                    // V86 is handled above; a returned V86 CS has arbitrary low bits, and a
                    // real-mode CS is not a selector at all, so this ring check must see
                    // neither. Inter-privilege return to a less-privileged (non-V86) ring:
                    // pop SS:ESP.
                    let esp = self.pop(bus, OperandSize::Dword)?;
                    let ss = self.pop(bus, OperandSize::Dword)? as u16;
                    self.load_segment(bus, SegmentIndex::Cs, cs)?;
                    // The outer SS is a mechanical side effect of this IRET's ring
                    // change, not a direct MOV/POP SS: `self.cpl` above is still the
                    // pre-IRET (inner) level, so the plain-path CPL check would compare
                    // against the wrong privilege level. See `load_segment_system`.
                    self.load_segment_system(bus, SegmentIndex::Ss, ss)?;
                    self.set_eip(eip);
                    // 386 PRM 17-80: "Load SS:eSP from stack" -- eSP is B-keyed (17-12).
                    // Onto a B=0 (16-bit) outer stack, only SP takes the popped value;
                    // ESP's high word carries over from the inner stack untouched (the
                    // documented real-silicon ESP-high-word leak on a 16-bit ring
                    // transition). A B=1 outer stack takes the full popped dword.
                    if self.stack_is_32bit() {
                        self.registers.set_esp(esp);
                    } else {
                        self.write_gpr16(4, esp as u16);
                    }
                    self.load_flags(flags, OperandSize::Dword, false);
                    // PRM transition point: the target RPL (checked above, non-V86) is the
                    // new CPL.
                    self.cpl = (cs & 3) as u8;
                    self.invalidate_data_segments_below_cpl();
                    return Ok(());
                }

                // Same privilege (existing behavior).
                self.load_segment(bus, SegmentIndex::Cs, cs)?;
                self.set_eip(eip);
                self.load_flags(flags, OperandSize::Dword, false);
            }
        }
        Ok(())
    }

    /// 386 PRM (IRET / RET to an outer privilege level): after the CPL drops,
    /// each of DS/ES/FS/GS holding a data segment or non-conforming code
    /// segment whose DPL < the new CPL is loaded with the null selector, so
    /// the outer ring cannot keep using an inner ring's segments. Conforming
    /// code segments are exempt (accessible from any CPL). DPMI hosts rely on
    /// this: their ring-0 handlers IRET to ring 3 with kernel selectors still
    /// in DS, and the ring-3 code PUSH/POPs the (nulled) register afterwards.
    fn invalidate_data_segments_below_cpl(&mut self) {
        for segment in [
            SegmentIndex::Ds,
            SegmentIndex::Es,
            SegmentIndex::Fs,
            SegmentIndex::Gs,
        ] {
            let reg = self.registers.segment(segment);
            if reg.selector & 0xfffc == 0 {
                continue; // already null
            }
            let access = reg.access;
            let conforming_code = access & 0x0c == 0x0c;
            let dpl = (access >> 5) & 3;
            if !conforming_code && dpl < self.cpl {
                self.registers.set_segment(
                    segment,
                    SegmentRegister {
                        selector: 0,
                        ..Default::default()
                    },
                );
            }
        }
    }

    pub(super) fn far_call<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
        offset: u32,
        operand_size: OperandSize,
    ) -> ExecResult<()> {
        // A protected-mode far call to a system descriptor goes through a call gate,
        // which supplies its own CS:offset (the instruction's offset is ignored).
        // A V86 task (PE=1 but VM=1) uses 8086 far-call semantics — its selector is a
        // real-mode segment, never a descriptor — so it falls through to the direct path.
        if self.is_protected_mode() && !self.is_v86_mode() {
            let (low, high) = self.read_transfer_descriptor(bus, selector)?;
            if (high >> 8) & 0x10 == 0 {
                return self.far_system_transfer(bus, selector, low, high, true);
            }
            // 386 PRM CALL: the target descriptor's present bit is checked
            // (#NP(selector)) BEFORE the return address is pushed. Borland RTM
            // far-calls into swapped-out overlay segments and restarts the CALL
            // after its #NP handler loads them; pushes committed ahead of the
            // fault would leak 4 bytes of stack per swap-in on the restart.
            if (high >> 8) & 0x80 == 0 {
                return Err(InternalFault::Exception {
                    vector: 11,
                    error_code: Some(selector_error_code(selector, selector & 0x4 != 0, false)),
                });
            }
        }
        // Direct far call (real mode, or a protected-mode code segment). Push CS first
        // (higher stack address), then the return offset. RETF pops offset then CS.
        // self.registers.eip already points past the instruction.
        // A fault on the SECOND push restores (E)SP past the committed first
        // one, so a PUSH-fault restarts from the pre-instruction stack pointer
        // (same atomicity as `push` itself). The present bit is validated above,
        // ahead of the pushes; a fault in the CS load below for any OTHER reason
        // still leaves both pushes committed (pre-existing ordering divergence).
        let esp_before = self.registers.esp();
        self.push(bus, u32::from(self.registers.cs().selector), operand_size)?;
        if let Err(fault) = self.push(bus, self.registers.eip, operand_size) {
            if self.stack_is_32bit() {
                self.registers.set_esp(esp_before);
            } else {
                self.write_gpr16(4, esp_before as u16);
            }
            return Err(fault);
        }
        self.load_segment(bus, SegmentIndex::Cs, selector)?;
        self.set_eip(offset & operand_size.mask());
        // No DPL/CPL check is enforced on this direct (non-gate) path today (a
        // pre-existing limitation, out of scope here); the cache just tracks whatever
        // level the load landed at, matching the historical live formula exactly. Real
        // mode is unconditionally CPL 0 -- a real-mode CS is not a descriptor selector,
        // so its low bits carry no RPL and must not leak into the cache.
        self.cpl = if !self.is_protected_mode() {
            0
        } else if self.is_v86_mode() {
            3
        } else {
            (selector & 3) as u8
        };
        Ok(())
    }

    /// `release` is the RETF imm16 count. The caller still applies it to the
    /// OUTER stack after this returns (the PRM's second increment); the
    /// inter-privilege arm below additionally applies it to the INNER stack
    /// before popping SS:eSP (386 PRM RET, outer level: "Increment eSP by 8
    /// plus the immediate offset" -- the parameter block copied by the call
    /// gate sits between CS:IP and the saved SS:eSP).
    pub(super) fn return_far<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand_size: OperandSize,
        release: u16,
    ) -> ExecResult<()> {
        // Same restartability wrapper as `iret`: RTM's overlay thunks RETF into
        // not-present segments and restart after the #NP swap-in; the committed
        // pops must be undone on the fault (this was the Tyrian swap-resume
        // abort: the restarted thunk RETF popped 4 bytes past its frame, off
        // the top of the thunk stack, and died #SS).
        let esp_before = self.registers.esp();
        let result = self.return_far_body(bus, operand_size, release);
        if result.is_err() {
            self.registers.set_esp(esp_before);
        }
        result
    }

    fn return_far_body<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand_size: OperandSize,
        release: u16,
    ) -> ExecResult<()> {
        // Pop the offset then CS, mirroring iret's pop order. On the 32-bit form CS
        // occupies four stack bytes; the high two are discarded on load.
        let offset = self.pop(bus, operand_size)?;
        let selector = self.pop(bus, operand_size)? as u16;
        if self.is_protected_mode()
            && !self.is_v86_mode()
            && (selector & 3) as u8 > self.current_privilege_level()
        {
            // 386 PRM RET (far, RPL > CPL): an inter-privilege return pops the
            // outer SS:eSP after CS:IP and nulls inner-ring data segments. DPMI
            // hosts enter ring-3 client exception handlers with exactly this
            // frame; the V86 guard mirrors `iret`'s (a V86 CS has arbitrary low
            // bits, and real mode never reaches this ring check).
            self.release_stack(release);
            let sp = self.pop(bus, operand_size)?;
            let ss = self.pop(bus, operand_size)? as u16;
            // The outer SS's RPL must equal the return CS's RPL (386 PRM RET,
            // outer level: "Selector RPL must equal the RPL of the return CS
            // selector ELSE #GP(selector)").
            if (ss & 3) != (selector & 3) {
                return Err(InternalFault::Exception {
                    vector: 13,
                    error_code: Some(selector_error_code(ss, ss & 0x4 != 0, false)),
                });
            }
            self.load_segment(bus, SegmentIndex::Cs, selector)?;
            // Mechanical SS load under the pre-RET CPL; see `load_segment_system`.
            self.load_segment_system(bus, SegmentIndex::Ss, ss)?;
            self.set_eip(offset & operand_size.mask());
            // B-keyed outer eSP load, same rule as `iret`'s inter-privilege arm:
            // a B=0 outer stack takes SP only (ESP high word carries over); a
            // B=1 stack takes the popped value at the operand width.
            if self.stack_is_32bit() {
                self.registers.set_esp(sp & operand_size.mask());
            } else {
                self.write_gpr16(4, sp as u16);
            }
            // PRM transition point: the target RPL (non-V86, checked above) is
            // the new CPL.
            self.cpl = (selector & 3) as u8;
            self.invalidate_data_segments_below_cpl();
            return Ok(());
        }
        self.load_segment(bus, SegmentIndex::Cs, selector)?;
        self.set_eip(offset & operand_size.mask());
        // Same-privilege return; the cache tracks the loaded CS's RPL, or 3 if this
        // landed back in V86 -- real mode forces CPL 0 (see `far_call`'s comment on
        // why the selector's low bits don't apply there).
        self.cpl = if !self.is_protected_mode() {
            0
        } else if self.is_v86_mode() {
            3
        } else {
            (selector & 3) as u8
        };
        Ok(())
    }

    pub(super) fn release_stack(&mut self, count: u16) {
        // The immediate return forms release `count` bytes of arguments after the
        // pop. The stack pointer width follows SS.B, not the operand size: a
        // 16-bit stack (SS.B=0, which includes real mode and V86) moves only SP
        // and preserves ESP[31:16].
        if self.stack_is_32bit() {
            let esp = self.registers.esp().wrapping_add(u32::from(count));
            self.registers.set_esp(esp);
        } else {
            let sp = self.read_gpr16(4).wrapping_add(count);
            self.write_gpr16(4, sp);
        }
    }

    pub(super) fn far_jump<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
        offset: u32,
        operand_size: OperandSize,
    ) -> ExecResult<()> {
        // As in `far_call`: a V86 task's far jump is 8086-style, not a descriptor load.
        if self.is_protected_mode() && !self.is_v86_mode() {
            let (low, high) = self.read_transfer_descriptor(bus, selector)?;
            if (high >> 8) & 0x10 == 0 {
                return self.far_system_transfer(bus, selector, low, high, false);
            }
        }
        self.load_segment(bus, SegmentIndex::Cs, selector)?;
        self.set_eip(offset & operand_size.mask());
        // Direct (non-gate) JMP: no DPL check enforced today (pre-existing limitation,
        // out of scope); the cache tracks the loaded CS's RPL / V86. Real mode forces
        // CPL 0 (see `far_call`'s comment on why the selector's low bits don't apply).
        self.cpl = if !self.is_protected_mode() {
            0
        } else if self.is_v86_mode() {
            3
        } else {
            (selector & 3) as u8
        };
        Ok(())
    }

    /// Dispatch a far CALL/JMP to a system descriptor: a call gate, a task gate, or a
    /// TSS (a direct task switch). `is_call` distinguishes CALL from JMP.
    fn far_system_transfer<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
        low: u32,
        high: u32,
        is_call: bool,
    ) -> ExecResult<()> {
        let kind = if is_call {
            TaskSwitchKind::Call
        } else {
            TaskSwitchKind::Jump
        };
        match (high >> 8) & 0x0f {
            0x04 | 0x0c if is_call => self.far_call_gate(bus, selector, low, high),
            0x04 | 0x0c => self.far_jump_gate(bus, selector, low, high),
            // Available 386 TSS: a direct task switch.
            0x09 => self.task_switch(bus, selector, kind),
            // Task gate: switch to the TSS the gate names.
            0x05 => {
                let tss_selector = ((low >> 16) & 0xffff) as u16;
                self.task_switch(bus, tss_selector, kind)
            }
            _ => Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(selector_error_code(selector, selector & 0x4 != 0, false)),
            }),
        }
    }

    /// Read a descriptor for a far transfer from the GDT or LDT, faulting (#GP) on a
    /// null or out-of-range selector.
    fn read_transfer_descriptor<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
    ) -> ExecResult<(u32, u32)> {
        let in_ldt = selector & 0x4 != 0;
        let index = u32::from(selector & !0x7);
        let (base, limit) = if in_ldt {
            (self.ldtr.base, self.ldtr.limit)
        } else {
            (self.gdtr.base, u32::from(self.gdtr.limit))
        };
        if index == 0 || index + 7 > limit {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(selector_error_code(selector, in_ldt, false)),
            });
        }
        let addr = base + index;
        let low = self.read_system_linear_u32(bus, addr)?;
        let high = self.read_system_linear_u32(bus, addr + 4)?;
        Ok((low, high))
    }

    /// Decode a call-gate descriptor into (target selector, entry offset, operand size,
    /// parameter count). 386 gates (type 0x0C) carry a 32-bit offset and a dword count;
    /// 286 gates (type 0x04) a 16-bit offset and a word count.
    fn decode_call_gate(low: u32, high: u32) -> (u16, u32, OperandSize, usize) {
        let is_32 = (high >> 8) & 0x0f == 0x0c;
        let target = ((low >> 16) & 0xffff) as u16;
        let offset = if is_32 {
            (low & 0xffff) | (high & 0xffff_0000)
        } else {
            low & 0xffff
        };
        let op = if is_32 {
            OperandSize::Dword
        } else {
            OperandSize::Word
        };
        (target, offset, op, (high & 0x1f) as usize)
    }

    fn far_call_gate<B: CpuBus>(
        &mut self,
        bus: &mut B,
        gate_selector: u16,
        low: u32,
        high: u32,
    ) -> ExecResult<()> {
        let access = (high >> 8) & 0xff;
        let gate_type = access & 0x0f;
        if gate_type != 0x0c && gate_type != 0x04 {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(selector_error_code(
                    gate_selector,
                    gate_selector & 0x4 != 0,
                    false,
                )),
            });
        }
        let gate_dpl = ((access >> 5) & 3) as u8;
        let cpl = self.current_privilege_level();
        let rpl = (gate_selector & 3) as u8;
        if access & 0x80 == 0 || gate_dpl < cpl.max(rpl) {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(selector_error_code(
                    gate_selector,
                    gate_selector & 0x4 != 0,
                    false,
                )),
            });
        }
        let (target_selector, gate_offset, op, param_count) = Self::decode_call_gate(low, high);
        let (tl, th) = self.read_transfer_descriptor(bus, target_selector)?;
        let target_access = (th >> 8) & 0xff;
        // Target must be a present code segment (S = 1 and the executable bit set).
        if target_access & 0x80 == 0 || target_access & 0x18 != 0x18 {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(selector_error_code(
                    target_selector,
                    target_selector & 0x4 != 0,
                    false,
                )),
            });
        }
        let target_dpl = ((target_access >> 5) & 3) as u8;
        let conforming = target_access & 0x04 != 0;
        let mut target = self.descriptor_to_segment(target_selector, tl, th);
        let return_cs = self.registers.cs().selector;
        let return_eip = self.registers.eip;

        if !conforming && target_dpl < cpl {
            // Inter-privilege call: copy parameters off the outer stack, switch to the
            // inner stack from the TSS, then rebuild the frame there.
            //
            // The outer stack's top is SS:SP, per the old SS's own B bit (386 PRM
            // 17-42): a B=0 outer stack wraps the per-slot offset within 16 bits and
            // leaves ESP's high word alone, rather than adding into the full ESP. This
            // matters for the DOS4GW/VCPI case: a 32-bit call gate onto a B=0 outer
            // stack with nonzero ESP[31:16] must still read params at the wrapped SP.
            let mut params = [0u32; 32];
            let psize = op.bytes();
            let outer_esp = self.registers.esp();
            let outer_stack_32 = self.stack_is_32bit();
            for (k, slot) in params.iter_mut().enumerate().take(param_count) {
                let offset = k as u32 * psize;
                let addr = if outer_stack_32 {
                    outer_esp.wrapping_add(offset)
                } else {
                    u32::from((outer_esp as u16).wrapping_add(offset as u16))
                };
                *slot = self.read_memory_sized(
                    bus,
                    SegmentIndex::Ss,
                    addr,
                    op,
                    BusAccessKind::DataRead,
                )?;
            }
            let (old_ss, old_esp) = self.switch_to_inner_stack(bus, target_dpl)?;
            // PRM transition point: the call gate crosses to the inner ring here, before
            // the return frame is built on the new stack -- the pushes below execute at
            // the target level, not the caller's.
            self.cpl = target_dpl;
            self.push(bus, u32::from(old_ss), op)?;
            self.push(bus, old_esp, op)?;
            for k in (0..param_count).rev() {
                self.push(bus, params[k], op)?;
            }
            self.push(bus, u32::from(return_cs), op)?;
            self.push(bus, return_eip, op)?;
            target.selector = (target_selector & !3) | u16::from(target_dpl);
        } else {
            // Same privilege (or a conforming target): push the return frame on the
            // current stack.
            self.push(bus, u32::from(return_cs), op)?;
            self.push(bus, return_eip, op)?;
            target.selector = (target_selector & !3) | u16::from(cpl);
        }
        self.registers.set_segment(SegmentIndex::Cs, target);
        self.invalidate_code_caches_for_cs_load();
        self.set_eip(gate_offset & op.mask());
        Ok(())
    }

    fn far_jump_gate<B: CpuBus>(
        &mut self,
        bus: &mut B,
        gate_selector: u16,
        low: u32,
        high: u32,
    ) -> ExecResult<()> {
        let access = (high >> 8) & 0xff;
        let gate_type = access & 0x0f;
        if gate_type != 0x0c && gate_type != 0x04 {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(selector_error_code(
                    gate_selector,
                    gate_selector & 0x4 != 0,
                    false,
                )),
            });
        }
        let gate_dpl = ((access >> 5) & 3) as u8;
        let cpl = self.current_privilege_level();
        let rpl = (gate_selector & 3) as u8;
        if access & 0x80 == 0 || gate_dpl < cpl.max(rpl) {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(selector_error_code(
                    gate_selector,
                    gate_selector & 0x4 != 0,
                    false,
                )),
            });
        }
        let (target_selector, gate_offset, op, _) = Self::decode_call_gate(low, high);
        let (tl, th) = self.read_transfer_descriptor(bus, target_selector)?;
        let target_access = (th >> 8) & 0xff;
        if target_access & 0x80 == 0 || target_access & 0x18 != 0x18 {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(selector_error_code(
                    target_selector,
                    target_selector & 0x4 != 0,
                    false,
                )),
            });
        }
        let target_dpl = ((target_access >> 5) & 3) as u8;
        let conforming = target_access & 0x04 != 0;
        // A JMP through a gate cannot change privilege: a non-conforming target must be
        // at the current level; a conforming one no more privileged.
        if (!conforming && target_dpl != cpl) || (conforming && target_dpl > cpl) {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(selector_error_code(
                    target_selector,
                    target_selector & 0x4 != 0,
                    false,
                )),
            });
        }
        let mut target = self.descriptor_to_segment(target_selector, tl, th);
        target.selector = (target_selector & !3) | u16::from(cpl);
        self.registers.set_segment(SegmentIndex::Cs, target);
        self.invalidate_code_caches_for_cs_load();
        self.set_eip(gate_offset & op.mask());
        Ok(())
    }

    /// Switch to the inner-ring stack for `target_dpl`, read from the current TSS
    /// (386 layout: ESPn at 4 + 8n, SSn at 8 + 8n). Returns the outgoing SS:ESP.
    fn switch_to_inner_stack<B: CpuBus>(
        &mut self,
        bus: &mut B,
        target_dpl: u8,
    ) -> ExecResult<(u16, u32)> {
        let old_ss = self.registers.segment(SegmentIndex::Ss).selector;
        let old_esp = self.registers.esp();
        // TR names either a 386 TSS (types 0x9/0xB: ESP0 dword at +4, SS0 word
        // at +8, 8 bytes per ring) or a 286 TSS (types 0x1/0x3: SP0 word at +2,
        // SS0 word at +4, 4 bytes per ring). Borland's DPMI16BI runs off a 286
        // TSS; reading the 386 offsets from it yields SS0=0 and the delivery
        // dies on the null SS load.
        // Keyed on the full 286 type pair, not just bit 3, so an uninitialized
        // TR (access 0) keeps the 386 read it always had.
        let (new_esp, new_ss) = if matches!(self.tr.access & 0x1f, 0x01 | 0x03) {
            let sp_addr = self.tr.base + 2 + 4 * u32::from(target_dpl);
            let sp = self.read_system_linear(bus, sp_addr, BusWidth::Word)?;
            let ss = self.read_system_linear(bus, sp_addr + 2, BusWidth::Word)? as u16;
            (sp, ss)
        } else {
            let esp_addr = self.tr.base + 4 + 8 * u32::from(target_dpl);
            let esp = self.read_system_linear_u32(bus, esp_addr)?;
            let ss = self.read_system_linear(bus, esp_addr + 4, BusWidth::Word)? as u16;
            (esp, ss)
        };
        // The TSS-supplied SS for `target_dpl` is validated by the gate's own privilege
        // rules (the gate/target DPL check already run in `far_call_gate`), not the
        // plain-path CPL check: `self.cpl` here is still the outer (higher) level, since
        // the caller sets it to `target_dpl` only after this call returns. See
        // `load_segment_system`.
        self.load_segment_system(bus, SegmentIndex::Ss, new_ss)?;
        // 386 PRM 17-43/17-74: "Load new SS:eSP value from TSS" -- eSP is B-keyed
        // (17-12), not always the full ESP. The just-loaded SS's B bit governs: a
        // 16-bit (B=0) ring-0 stack takes the TSS value into SP only, and ESP's
        // high word carries over from the interrupted context untouched (the same
        // wrap-preserving-high-word rule `push`/`pop` use). A B=1 stack takes the
        // TSS value as the full 32-bit ESP.
        if self.stack_is_32bit() {
            self.registers.set_esp(new_esp);
        } else {
            self.write_gpr16(4, new_esp as u16);
        }
        Ok((old_ss, old_esp))
    }

    /// 386 hardware task switch. Saves the outgoing task's state into the current TSS,
    /// loads the incoming one, juggles the busy bits, and (for a CALL) links back to
    /// the caller and sets NT. TSS memory is read/written through paging with
    /// implicit-supervisor semantics (see `translate_linear_system`). Limit: the 286
    /// (short) TSS form is not modeled.
    fn task_switch<B: CpuBus>(
        &mut self,
        bus: &mut B,
        new_selector: u16,
        kind: TaskSwitchKind,
    ) -> ExecResult<()> {
        // An IRET task return faults with #TS, not #GP, on every back-link
        // malformation (386 PRM 9.5): a null or out-of-limit selector comes back
        // from `read_transfer_descriptor` as #GP and is re-vectored here.
        let fault_vector = match kind {
            TaskSwitchKind::Return => 10,
            _ => 13,
        };
        // A TSS selector must name the GDT: TI=1 faults before any descriptor
        // read. This also keeps `set_tss_busy`, which computes a GDT address
        // unconditionally, from writing through an LDT selector's index into
        // whatever sits at that GDT offset.
        if new_selector & 0x4 != 0 {
            return Err(InternalFault::Exception {
                vector: fault_vector,
                error_code: Some(selector_error_code(new_selector, true, false)),
            });
        }
        let (low, high) = match self.read_transfer_descriptor(bus, new_selector) {
            Err(InternalFault::Exception {
                vector: 13,
                error_code,
            }) if kind == TaskSwitchKind::Return => {
                return Err(InternalFault::Exception {
                    vector: 10,
                    error_code,
                });
            }
            other => other?,
        };
        let access = (high >> 8) & 0xff;
        // JMP/CALL (and a gate-borne exception) need a present, AVAILABLE 386 TSS
        // (type 0x09); busy or wrong type is #GP. An IRET task return goes the other
        // way: the back-link must name a present, BUSY 386 TSS (type 0x0b), anything
        // else is #TS (386 PRM 9.5's back-link validation).
        let wanted_type = match kind {
            TaskSwitchKind::Return => 0x0b,
            _ => 0x09,
        };
        if access & 0x80 == 0 || access & 0x1f != wanted_type {
            return Err(InternalFault::Exception {
                vector: fault_vector,
                error_code: Some(selector_error_code(
                    new_selector,
                    new_selector & 0x4 != 0,
                    false,
                )),
            });
        }
        let new_tss = self.descriptor_to_segment(new_selector, low, high);
        let old_selector = self.tr.selector;

        // The outgoing image of an IRET task return carries NT=0 (SDM table
        // 8-2). Cleared only NOW, after every fault the back-link validation can
        // raise: a #TS/#GP above must leave NT set so the faulting IRET stays
        // restartable and the #TS handler's own nested IRET still task-returns.
        if kind == TaskSwitchKind::Return {
            self.registers.eflags &= !FLAG_NT;
        }
        self.save_task_state(bus)?;
        // SDM table 8-2 busy-bit rules: JMP and IRET free the outgoing TSS; CALL
        // leaves it busy so the nested task's IRET can come back through it.
        if kind != TaskSwitchKind::Call {
            self.set_tss_busy(bus, old_selector, false)?;
        }
        self.load_task_state(bus, new_tss.base)?;
        if kind == TaskSwitchKind::Call {
            // Write the back-link and set NT so the inner IRET returns to the caller.
            self.write_system_linear(bus, new_tss.base, BusWidth::Word, u32::from(old_selector))?;
            self.registers.eflags |= FLAG_NT;
        }
        self.set_tss_busy(bus, new_selector, true)?;
        self.tr = new_tss;
        self.tr.access |= 0x02;
        self.control.cr0 |= CR0_TS;
        Ok(())
    }

    fn save_task_state<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<()> {
        // Settle deferred arithmetic flags so the eflags image saved into the outgoing TSS is live.
        self.materialize_flags();
        let base = self.tr.base;
        self.write_system_linear(bus, base + 32, BusWidth::Dword, self.registers.eip)?;
        self.write_system_linear(bus, base + 36, BusWidth::Dword, self.registers.eflags)?;
        for i in 0..8u32 {
            let value = self.read_gpr32(i as u8);
            self.write_system_linear(bus, base + 40 + i * 4, BusWidth::Dword, value)?;
        }
        for (k, segment) in TASK_SEGMENTS.iter().enumerate() {
            let selector = u32::from(self.registers.segment(*segment).selector);
            self.write_system_linear(bus, base + 72 + k as u32 * 4, BusWidth::Word, selector)?;
        }
        self.write_system_linear(
            bus,
            base + 96,
            BusWidth::Word,
            u32::from(self.ldtr.selector),
        )?;
        Ok(())
    }

    fn load_task_state<B: CpuBus>(&mut self, bus: &mut B, base: u32) -> ExecResult<()> {
        if self.control.cr0 & CR0_PG != 0 {
            // Read through the outgoing task's still-active page tables: CR3 hasn't
            // been reloaded yet, so this TSS field is translated under the old
            // mapping, same as every other field read here.
            self.control.cr3 = self.read_system_linear_u32(bus, base + 28)?;
            // The incoming task reloads CR3, so its page mappings replace the old
            // task's: drop the previous task's cached translations.
            self.flush_tlb_and_code_caches();
        }
        // The LDTR is loaded first so segment loads that reference the LDT resolve.
        let ldtr = self.read_system_linear(bus, base + 96, BusWidth::Word)? as u16;
        self.load_ldtr(bus, ldtr)?;
        let eip = self.read_system_linear_u32(bus, base + 32)?;
        let eflags = self.read_system_linear_u32(bus, base + 36)?;
        for i in 0..8u32 {
            let value = self.read_system_linear_u32(bus, base + 40 + i * 4)?;
            self.write_gpr32(i as u8, value);
        }
        self.registers.eflags = eflags | 0x2;
        // The incoming task's eflags is the new truth; drop any stale arithmetic descriptor.
        self.pending_flags = PendingFlags::default();
        self.recompute_alignment_armed();
        self.set_eip(eip);
        for (k, segment) in TASK_SEGMENTS.iter().enumerate() {
            let selector =
                self.read_system_linear(bus, base + 72 + k as u32 * 4, BusWidth::Word)? as u16;
            // A null data segment (ES/DS/FS/GS) is legal and just unusable; CS and SS
            // must be loadable.
            //
            // "Unusable" is a PROTECTED-MODE statement. The incoming EFLAGS -- VM included --
            // was committed above, before this loop, so `is_v86_mode` here already answers for
            // the task being switched TO: if it is a V86 task, selector 0 is not the null
            // descriptor at all but an ordinary 8086 segment at base 0, and it must be built
            // like every other V86 segment (limit 0xFFFF, real-mode access). Leaving it at
            // `Default::default()` gave such a task a limit-0 DS that faults on its first
            // access -- and, since `load_segment_real_mode` no longer re-stamps the limit,
            // the JIT's `LoadSegReal` lowering would no longer paper over it either.
            if selector & !0x7 == 0
                && !matches!(segment, SegmentIndex::Cs | SegmentIndex::Ss)
                && !self.is_v86_mode()
            {
                self.registers.set_segment(
                    *segment,
                    SegmentRegister {
                        selector,
                        ..Default::default()
                    },
                );
            } else {
                // Task-switch register restore is a system-managed reload, not a plain
                // MOV/POP Sreg: ES (first in TASK_SEGMENTS) loads before CS updates
                // `self.cpl` to the incoming task's level below, so the plain-path
                // CPL-vs-DPL check would run against the outgoing task's stale CPL for
                // at least that register. See `load_segment_system`.
                self.load_segment_system(bus, *segment, selector)?;
            }
            if *segment == SegmentIndex::Cs {
                // PRM transition point: a hardware task switch can land the incoming task
                // in V86 (eflags.VM loaded above) or in an arbitrary protected-mode ring
                // named by the incoming CS's RPL. Set the cache from the just-loaded state,
                // the same rule `current_privilege_level` used to compute live.
                self.cpl = if self.is_v86_mode() {
                    3
                } else {
                    (selector & 3) as u8
                };
            }
        }
        Ok(())
    }

    fn set_tss_busy<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
        busy: bool,
    ) -> ExecResult<()> {
        if selector & !0x7 == 0 {
            return Ok(());
        }
        let addr = self.gdtr.base + u32::from(selector & !0x7) + 5;
        let mut access = self.read_system_linear(bus, addr, BusWidth::Byte)? as u8;
        if busy {
            access |= 0x02;
        } else {
            access &= !0x02;
        }
        self.write_system_linear(bus, addr, BusWidth::Byte, u32::from(access))?;
        Ok(())
    }

    pub(super) fn relative_jump(&mut self, relative: i32, operand_size: OperandSize) {
        self.set_eip(self.registers.eip.wrapping_add(relative as u32) & operand_size.mask());
    }

    /// `load_segment` for the three instructions that load SS directly from the instruction
    /// stream (MOV SS, POP SS, LSS): 386 PRM 11-16 says loading SS this way inhibits interrupts,
    /// NMI, and single-step traps until the next instruction boundary, so a pointer's offset and
    /// stack segment can be loaded as one atomic unit even if an interrupt lands between the two
    /// halves. This is the SAME one-instruction shadow STI arms (`interrupt_shadow`); reused here
    /// rather than duplicated. Only armed on success -- a faulting load (null selector, bad
    /// descriptor) must not suppress the interrupt check. Every OTHER `load_segment(.., Ss, ..)`
    /// call site (IRET's outer-stack pop, the inner-stack switch on a privilege-level change, the
    /// TSS register-restore loop) reloads SS as a side effect of a control transfer, not as the
    /// direct target of one of these three opcodes, and must not arm the shadow -- callers use the
    /// plain `load_segment` for those.
    pub(super) fn load_segment_arming_ss_shadow<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        selector: u16,
    ) -> ExecResult<()> {
        self.load_segment(bus, segment, selector)?;
        if segment == SegmentIndex::Ss {
            self.interrupt_shadow = true;
        }
        Ok(())
    }

    /// The plain MOV Sreg/POP Sreg/LDS-family data-segment-load path: applies the full
    /// PRM 6.7/9.1 privilege check (max(CPL, RPL) <= DPL for a non-conforming target).
    pub(super) fn load_segment<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        selector: u16,
    ) -> ExecResult<()> {
        self.load_segment_checked(bus, segment, selector, true)
    }

    /// A segment reload that is itself the mechanical side effect of a privilege
    /// transition the caller has already validated by its own rules: IRET's outer-stack
    /// SS pop, the call-gate inner-stack switch, and the TSS register-restore loop.
    /// `self.cpl` at the point of this call is mid-transition (still the old level, or
    /// -- for the TSS loop's ES, loaded before CS -- not yet meaningful at all), so the
    /// ordinary CPL-vs-DPL check does not apply; the descriptor type/writability check
    /// still does (a system-managed load must still resolve to a legal data/stack
    /// segment, just not one gated on the stale CPL).
    fn load_segment_system<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        selector: u16,
    ) -> ExecResult<()> {
        self.load_segment_checked(bus, segment, selector, false)
    }

    fn load_segment_checked<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        selector: u16,
        check_privilege: bool,
    ) -> ExecResult<()> {
        if self.is_v86_mode() {
            // A V86 task addresses memory like the 8086: base = selector << 4, 64 KB.
            self.load_segment_real(segment, selector);
        } else if self.is_protected_mode() {
            let register = self.load_protected_segment(bus, segment, selector, check_privilege)?;
            self.registers.set_segment(segment, register);
            if segment == SegmentIndex::Cs {
                self.invalidate_code_caches_for_cs_load();
            }
        } else {
            self.load_segment_real_mode(segment, selector);
        }
        Ok(())
    }

    /// The CANONICALIZING real-mode segment install: every field written from
    /// `SegmentRegister::real`, including `limit = 0xFFFF`.
    ///
    /// This is the right form only where the architecture really does rebuild the whole
    /// descriptor cache: V86 entry and V86 segment loads (386 PRM 26.3.1 -- a V86 task
    /// addresses memory like the 8086, 64 KB, no exceptions), the V86-exit data-segment
    /// clear, and the boot/reset paths. An ordinary REAL-MODE `MOV Sreg, r16` must NOT
    /// come here -- see `load_segment_real_mode`.
    pub(super) fn load_segment_real(&mut self, segment: SegmentIndex, selector: u16) {
        self.registers
            .set_segment(segment, SegmentRegister::real(selector));
        if segment == SegmentIndex::Cs {
            self.invalidate_code_caches_for_cs_load();
        }
    }

    /// A segment load taken with CR0.PE clear and VM clear: plain real mode.
    ///
    /// Real mode does not re-derive a descriptor -- there is no table to read. The 386
    /// recomputes only what the selector determines (base = selector << 4) and the access
    /// rights; the cached LIMIT is left exactly as the last protected-mode load left it.
    /// That single omission is what "unreal"/flat-real mode IS: software enters protected
    /// mode, loads a 4 GB-limit data descriptor into DS/ES/FS/GS/SS, drops CR0.PE, and goes
    /// on addressing 4 GB from real mode. Stamping the limit back to 0xFFFF here destroys
    /// it, and every `mov eax,[esi+ecx*8]` past 64 KB becomes a #GP.
    ///
    /// CS is the documented exception: a real-mode CS load re-canonicalizes to 0xFFFF, so a
    /// far jump out of protected mode really does give back a 64 KB code segment. (We do not
    /// offer a "big real-mode CS" escape hatch.)
    ///
    /// WHICH FIELDS ARE PRESERVED, and why exactly one. The real-mode limit check
    /// (`segment_linear_range` / `segment_linear_byte` in `memory.rs`) consults `base`,
    /// `limit` and `access`; `default_size_32` is reached only through the expand-down
    /// ceiling, which is gated on protected-and-not-V86 and so is dead in real mode. `limit`
    /// is therefore the ONLY field a 4 GB real-mode data segment needs carried over, and it
    /// is the only one carried. `default_size_32` is deliberately re-stamped false rather
    /// than preserved: its other reader is `stack_is_32bit`, and preserving it would hand
    /// real mode a 32-bit implicit stack as a silent side effect of this fix. (DOSBox-X's
    /// `CPU_SetSegGeneral` makes the same limit-only choice; 86Box happens to keep SS's B
    /// bit too. If a guest ever needs a big real-mode stack, that is its own slice with its
    /// own evidence.)
    fn load_segment_real_mode(&mut self, segment: SegmentIndex, selector: u16) {
        if segment == SegmentIndex::Cs {
            self.load_segment_real(segment, selector);
            return;
        }
        let limit = self.registers.segment(segment).limit;
        self.registers.set_segment(
            segment,
            SegmentRegister {
                limit,
                ..SegmentRegister::real(selector)
            },
        );
    }

    fn load_protected_segment<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        selector: u16,
        check_privilege: bool,
    ) -> ExecResult<SegmentRegister> {
        // A null selector (index 0, TI clear) loaded into a data segment (ES/DS/FS/GS) is
        // legal: it installs a null/unusable segment with no fault at load time. A later
        // memory access through it faults with #GP(0) (via the base=0/limit=0 default
        // segment failing the limit check in `segment_linear_byte`). CS and SS must still
        // #GP on a null selector -- CS reaches here via RETF/IRET/interrupt-gate delivery,
        // not just far-jump/call -- mirroring the TSS segment-load precedent above
        // (`selector & !0x7 == 0 && !matches!(segment, Cs | Ss)`). The mask must also require
        // TI=0: `selector & 0xfffc` folds in the index bits only, so a TI=1 index-0 selector
        // (0x0004, LDT[0]) is correctly excluded from this null short-circuit and falls
        // through to resolve against the LDT below.
        if selector & 0xfffc == 0 && !matches!(segment, SegmentIndex::Cs | SegmentIndex::Ss) {
            return Ok(SegmentRegister {
                selector,
                ..Default::default()
            });
        }
        let in_ldt = selector & 0x4 != 0;
        let index = u32::from(selector & !0x7);
        let (table_base, table_limit) = if in_ldt {
            (self.ldtr.base, self.ldtr.limit)
        } else {
            (self.gdtr.base, u32::from(self.gdtr.limit))
        };
        // Index 0 is reserved only in the GDT (the processor never uses GDT[0], per the PRM);
        // an LDT selector with index 0 (TI=1, e.g. 0x0004) is an ordinary, resolvable entry --
        // the null-selector short-circuit above already handled the true null case (index 0,
        // TI 0), so an in_ldt selector reaching here is never null. A bad/out-of-limit
        // selector is #GP regardless of which segment is being loaded (386 PRM 9.3): there is
        // no descriptor to be "not present", so this branch never takes the #NP/#SS fork below.
        if (index == 0 && !in_ldt) || index + 7 > table_limit {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(selector_error_code(selector, in_ldt, false)),
            });
        }
        let descriptor_address = table_base + index;
        let low = self.read_system_linear_u32(bus, descriptor_address)?;
        let high = self.read_system_linear_u32(bus, descriptor_address + 4)?;
        let access = ((high >> 8) & 0xff) as u8;
        let is_segment = access & 0x10 != 0; // S bit: 1 = code/data, 0 = system
        let descriptor_type = access & 0x0f;
        let is_code = is_segment && descriptor_type & 0x8 != 0;
        // 386 PRM 5.2/6.2 (table 5-1's code/data type matrix): a segment register may
        // only be loaded with a code or data descriptor of the kind it can hold. A
        // system-segment or gate descriptor (S clear -- LDT, TSS, call/task/trap/
        // interrupt gate) is never legal in a segment register load; #GP regardless of
        // which register. Otherwise CS accepts only a code descriptor; SS accepts only
        // a writable data descriptor (a stack must be read/write, PRM 5-12); DS/ES/FS/GS
        // accept a readable code descriptor or any data descriptor. This is the plain
        // MOV Sreg/POP Sreg/LDS-family data-segment-load path -- CS loads through a
        // call gate or a conforming/non-conforming far transfer are already checked by
        // `far_call_gate`/`far_jump_gate`/`return_far` and are not duplicated here.
        let type_legal = match segment {
            SegmentIndex::Cs => is_code,
            SegmentIndex::Ss => is_segment && !is_code && descriptor_type & 0x2 != 0,
            _ => is_segment && (!is_code || descriptor_type & 0x2 != 0),
        };
        if !type_legal {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(selector_error_code(selector, in_ldt, false)),
            });
        }
        // Privilege (386 PRM 6.7/9.1): a non-conforming load needs max(CPL, RPL) <= DPL.
        // Conforming code (reachable at any caller privilege, PRM 5-13) and CS itself
        // (whose CPL/RPL/DPL interplay is handled by the gate and far-transfer paths
        // that call `load_segment` for CS, not this plain data-path check) are exempt.
        // `check_privilege` is false for the system-managed reloads that route through
        // `load_segment_system` (see its doc comment): those have already had their own
        // privilege rules applied by the caller against a CPL that has not yet settled
        // to its post-transition value here.
        let conforming = is_code && descriptor_type & 0x4 != 0;
        if check_privilege && segment != SegmentIndex::Cs && !conforming {
            let dpl = (access >> 5) & 3;
            let cpl = self.current_privilege_level();
            let rpl = (selector & 3) as u8;
            if dpl < cpl.max(rpl) {
                return Err(InternalFault::Exception {
                    vector: 13,
                    error_code: Some(selector_error_code(selector, in_ldt, false)),
                });
            }
        }
        if access & 0x80 == 0 {
            // A present-bit-clear descriptor: #NP (vector 11) for every segment except SS,
            // which is #SS (vector 12) instead (386 PRM 9.3's "the SS register is being
            // loaded" carve-out -- the same vector `segment_limit_fault` uses for a stack
            // limit violation).
            let vector = if segment == SegmentIndex::Ss { 12 } else { 11 };
            return Err(InternalFault::Exception {
                vector,
                error_code: Some(selector_error_code(selector, in_ldt, false)),
            });
        }
        // Mark the descriptor Accessed (bit 0 of the type field, PRM 5-12/5-13) on a
        // successful load, same read-modify-write shape as `set_tss_busy`'s busy-bit
        // toggle. Skipped when already set -- the common case once a segment has been
        // touched once -- to avoid a write-back on every reload of a hot selector.
        if access & 0x01 == 0 {
            self.write_system_linear(
                bus,
                descriptor_address + 5,
                BusWidth::Byte,
                u32::from(access | 0x01),
            )?;
        }
        Ok(self.descriptor_to_segment(selector, low, high))
    }
    // ===================== Protected-mode system instructions =====================
    // The 0F 00 / 0F 01 groups plus LAR/LSL/CLTS. LDTR/TR live in the CPU state; the
    // segment-verify and access-rights instructions read descriptors from the GDT or
    // the LDT and never fault on a bad selector (they clear ZF instead).

    pub(super) fn require_cpl0(&self) -> ExecResult<()> {
        if self.current_privilege_level() != 0 {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(0),
            });
        }
        Ok(())
    }

    /// Decode an 8-byte descriptor into a cached segment register. Shared by the
    /// segment loader and the system-register loads.
    fn descriptor_to_segment(&self, selector: u16, low: u32, high: u32) -> SegmentRegister {
        let access = ((high >> 8) & 0xff) as u8;
        let base = ((low >> 16) & 0xffff) | ((high & 0x0000_00ff) << 16) | (high & 0xff00_0000);
        let mut limit = (low & 0xffff) | (high & 0x000f_0000);
        if high & 0x0080_0000 != 0 {
            limit = (limit << 12) | 0x0fff;
        }
        SegmentRegister {
            selector,
            base,
            limit,
            access,
            default_size_32: high & 0x0040_0000 != 0,
        }
    }

    /// Read a descriptor for VERR/VERW/LAR/LSL: from the GDT or the LDT, returning
    /// None (rather than faulting) for a null or out-of-range selector.
    pub(super) fn try_read_descriptor<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
    ) -> ExecResult<Option<(u32, u32)>> {
        let in_ldt = selector & 0x4 != 0;
        let index = u32::from(selector & !0x7);
        let (base, limit) = if in_ldt {
            (self.ldtr.base, self.ldtr.limit)
        } else {
            if index == 0 {
                return Ok(None);
            }
            (self.gdtr.base, u32::from(self.gdtr.limit))
        };
        if index + 7 > limit {
            return Ok(None);
        }
        let addr = base + index;
        let low = self.read_system_linear_u32(bus, addr)?;
        let high = self.read_system_linear_u32(bus, addr + 4)?;
        Ok(Some((low, high)))
    }

    /// Read a GDT descriptor for LLDT/LTR, which #GP on a null or out-of-range selector.
    fn read_gdt_descriptor<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
    ) -> ExecResult<(u32, u32)> {
        let index = u32::from(selector & !0x7);
        if index == 0 || index + 7 > u32::from(self.gdtr.limit) {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(selector_error_code(selector, false, false)),
            });
        }
        let addr = self.gdtr.base + index;
        let low = self.read_system_linear_u32(bus, addr)?;
        let high = self.read_system_linear_u32(bus, addr + 4)?;
        Ok((low, high))
    }

    pub(super) fn store_descriptor_table<B: CpuBus>(
        &mut self,
        bus: &mut B,
        memory: MemoryOperand,
        table: DescriptorTable,
    ) -> ExecResult<()> {
        // Limit: the 16-bit-operand quirk (base masked to 24 bits) is not modeled;
        // the full 32-bit base is always stored.
        self.write_memory_sized(
            bus,
            memory.segment,
            memory.offset,
            OperandSize::Word,
            u32::from(table.limit),
            BusAccessKind::DataWrite,
        )?;
        self.write_memory_sized(
            bus,
            memory.segment,
            memory.offset + 2,
            OperandSize::Dword,
            table.base,
            BusAccessKind::DataWrite,
        )
    }

    pub(super) fn load_ldtr<B: CpuBus>(&mut self, bus: &mut B, selector: u16) -> ExecResult<()> {
        if selector & 0x4 != 0 {
            // The LDT descriptor must live in the GDT (TI = 0).
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(selector_error_code(selector, true, false)),
            });
        }
        if selector & !0x7 == 0 {
            // A null selector marks the LDTR invalid.
            self.ldtr = SegmentRegister {
                selector,
                ..Default::default()
            };
            return Ok(());
        }
        let (low, high) = self.read_gdt_descriptor(bus, selector)?;
        let access = (high >> 8) & 0xff;
        // Present LDT system descriptor (S = 0, type = 2).
        if access & 0x80 == 0 || access & 0x1f != 0x02 {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(selector_error_code(selector, false, false)),
            });
        }
        self.ldtr = self.descriptor_to_segment(selector, low, high);
        Ok(())
    }

    pub(super) fn load_tr<B: CpuBus>(&mut self, bus: &mut B, selector: u16) -> ExecResult<()> {
        if selector & 0x4 != 0 || selector & !0x7 == 0 {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(selector_error_code(selector, selector & 0x4 != 0, false)),
            });
        }
        let (low, high) = self.read_gdt_descriptor(bus, selector)?;
        let access = (high >> 8) & 0xff;
        let descriptor_type = access & 0x1f;
        // Present available TSS: type 1 (286) or 9 (386).
        if access & 0x80 == 0 || (descriptor_type != 0x01 && descriptor_type != 0x09) {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(selector_error_code(selector, false, false)),
            });
        }
        let mut segment = self.descriptor_to_segment(selector, low, high);
        // Mark the TSS busy, both in the cache and back in the GDT descriptor.
        segment.access |= 0x02;
        let index = u32::from(selector & !0x7);
        let access_byte = (access | 0x02) as u8;
        self.write_system_linear(
            bus,
            self.gdtr.base + index + 5,
            BusWidth::Byte,
            u32::from(access_byte),
        )?;
        self.tr = segment;
        Ok(())
    }

    pub(super) fn verify_segment<B: CpuBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
        write: bool,
    ) -> ExecResult<bool> {
        let Some((_, high)) = self.try_read_descriptor(bus, selector)? else {
            return Ok(false);
        };
        let access = (high >> 8) & 0xff;
        let present = access & 0x80 != 0;
        let is_segment = access & 0x10 != 0; // S bit
        if !present || !is_segment {
            return Ok(false);
        }
        let descriptor_type = access & 0x0f;
        let is_code = descriptor_type & 0x8 != 0;
        let dpl = ((access >> 5) & 3) as u8;
        let rpl = (selector & 3) as u8;
        let privilege_ok = dpl >= self.current_privilege_level().max(rpl);
        let ok = if write {
            // VERW: writable data segment.
            !is_code && descriptor_type & 0x2 != 0 && privilege_ok
        } else {
            // VERR: readable. Data is always readable; code needs the readable bit.
            // Conforming code skips the privilege check.
            let readable = if is_code {
                descriptor_type & 0x2 != 0
            } else {
                true
            };
            let conforming = is_code && descriptor_type & 0x4 != 0;
            readable && (conforming || privilege_ok)
        };
        Ok(ok)
    }

    /// Shared LAR/LSL descriptor gate. No present-bit check: both instructions
    /// validate TYPE and privilege only, and return the rights/limit with P as
    /// stored (386 PRM LAR/LSL pages). Borland RTM probes its not-present swap
    /// descriptors with LAR from its #NP handler. The type check is what keeps
    /// an empty (all-zero) descriptor slot rejected now that P no longer
    /// incidentally rejects it: type 0 is invalid for both instructions.
    ///
    /// `wants_limit` selects the LSL table (only the memory-resident system
    /// types 1/2/3/9/B have a limit) over the LAR one (every system type
    /// except 0, 8, 0xA, 0xD).
    pub(super) fn descriptor_accessible(
        &self,
        selector: u16,
        high: u32,
        wants_limit: bool,
    ) -> bool {
        let access = (high >> 8) & 0xff;
        let dpl = ((access >> 5) & 3) as u8;
        let rpl = (selector & 3) as u8;
        let is_segment = access & 0x10 != 0;
        let descriptor_type = access & 0x0f;
        if !is_segment {
            let type_ok = if wants_limit {
                matches!(descriptor_type, 0x01 | 0x02 | 0x03 | 0x09 | 0x0b)
            } else {
                !matches!(descriptor_type, 0x00 | 0x08 | 0x0a | 0x0d)
            };
            if !type_ok {
                return false;
            }
        }
        let conforming_code =
            is_segment && descriptor_type & 0x8 != 0 && descriptor_type & 0x4 != 0;
        // Conforming code is reachable from any privilege; everything else needs
        // DPL >= max(CPL, RPL).
        conforming_code || dpl >= self.current_privilege_level().max(rpl)
    }
}
