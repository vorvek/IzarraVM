// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl Machine {
    /// Service INT 11h (GET EQUIPMENT LIST). Returns the BDA equipment word in AX,
    /// the way a real BIOS reads it from 0040:0010. The high word of EAX is left
    /// alone: callers that test the 386 EAX bits clear it themselves before the
    /// call, per RBIL. No flags change (the IRET restores the caller's FLAGS).
    pub(super) fn handle_int11(&mut self) {
        let word = self.memory.read_u16(0x410).unwrap_or(BIOS_EQUIPMENT_WORD);
        let eax = (self.cpu.registers.eax() & !0xFFFF) | u32::from(word);
        self.cpu.registers.set_eax(eax);
    }

    /// Service INT 12h (GET MEMORY SIZE). Returns the conventional memory size in
    /// KiB in AX, read from the BDA word at 0040:0013 the way a real BIOS does. No
    /// flags change (the IRET restores the caller's FLAGS).
    pub(super) fn handle_int12(&mut self) {
        let kib = self.memory.read_u16(0x413).unwrap_or(BIOS_BASE_MEMORY_KIB);
        let eax = (self.cpu.registers.eax() & !0xFFFF) | u32::from(kib);
        self.cpu.registers.set_eax(eax);
    }

    /// Service INT 14h over COM1/COM2 selected by DX=0/1. The BIOS functions cover
    /// AH=00h-05h, and the FOSSIL calls
    /// use the same timed UART plus the BIOS text cursor and keyboard ring.
    pub(super) fn handle_int14(&mut self) {
        let ax = self.cpu.registers.eax() as u16;
        let ah = (ax >> 8) as u8;
        let al = ax as u8;
        let bx = self.cpu.registers.ebx() as u16;

        match ah {
            0x07 => {
                self.set_ax(0x1208); // INT 08h, about 18 ticks per second.
                self.set_dx(55);
                return;
            }
            0x0D | 0x0E => {
                self.int14_fossil_keyboard_read();
                return;
            }
            0x11 => {
                self.handle_int10_text(0x02);
                return;
            }
            0x12 => {
                self.handle_int10_text(0x03);
                return;
            }
            0x13 | 0x15 => {
                self.teletype_char(al);
                return;
            }
            0x16 => {
                self.set_ax(0x0001);
                return;
            }
            0x17 => return,
            0x7E | 0x7F => {
                self.set_ax(0x1954);
                self.set_bx((bx & 0xff00) | u16::from(al));
                self.set_dx(self.cpu.registers.edx() as u16 & 0x00ff);
                return;
            }
            _ => {}
        }

        let port = self.cpu.registers.edx() as u16;
        if port >= 2 {
            self.set_eax_ah(0x80); // bit7 timeout: no such serial port
            return;
        }
        let second = port == 1;
        match ah {
            0x00 => {
                self.uart_init(second, al);
                let lsr = self.uart_read(second, 5);
                let msr = self.uart_read(second, 6);
                self.set_eax_ah(lsr);
                self.set_eax_al(msr);
            }
            0x01 => {
                self.uart_write(second, 0, al);
                self.finish_uart_transmit(second);
                let lsr = self.uart_read(second, 5);
                self.set_eax_ah(lsr & 0x7f); // bit7 clear = sent
            }
            0x02 => {
                let lsr = self.uart_read(second, 5);
                if lsr & 0x01 != 0 {
                    let byte = self.uart_read(second, 0);
                    self.set_eax_al(byte);
                    self.set_eax_ah(lsr & 0x1e); // line status, data-ready/timeout clear
                } else {
                    // No byte available, and no serial input source is wired, so the
                    // honest result is a receive timeout.
                    self.set_eax_ah(0x80);
                }
            }
            0x03 => {
                let lsr = self.uart_read(second, 5);
                let msr = self.uart_read(second, 6);
                self.set_eax_ah(lsr);
                self.set_eax_al(msr);
            }
            0x04 if bx == 0x4F50 => {
                let mcr = self.uart_read(second, 4) | 0x01;
                self.uart_write(second, 4, mcr);
                self.set_ax(0x1954);
                self.set_bx(0x001B);
            }
            0x04 if self.int14_extended_params_valid() => {
                self.uart_extended_init(second);
                let lsr = self.uart_read(second, 5);
                let msr = self.uart_read(second, 6);
                self.set_eax_ah(lsr);
                self.set_eax_al(msr);
            }
            0x05 => match al {
                0x00 => {
                    let mcr = self.uart_read(second, 4);
                    self.set_bx((self.cpu.registers.ebx() as u16 & 0xff00) | u16::from(mcr));
                    self.set_eax_ah(0x00);
                }
                0x01 => {
                    self.uart_write(second, 4, self.cpu.registers.ebx() as u8);
                    let lsr = self.uart_read(second, 5);
                    let msr = self.uart_read(second, 6);
                    self.set_eax_ah(lsr);
                    self.set_eax_al(msr);
                }
                _ => self.set_eax_ah(0x80),
            },
            0x06 => {
                let mcr = self.uart_read(second, 4);
                let mcr = if al == 0 { mcr & !0x01 } else { mcr | 0x01 };
                self.uart_write(second, 4, mcr);
            }
            0x08 | 0x09 | 0x0F | 0x10 | 0x14 | 0x1A => {}
            0x0A => {
                while self.uart_read(second, 5) & 0x01 != 0 {
                    let _ = self.uart_read(second, 0);
                }
            }
            0x0B => {
                self.uart_write(second, 0, al);
                self.finish_uart_transmit(second);
                self.set_ax(0x0001);
            }
            0x18 => self.int14_fossil_read_block(second),
            0x19 => self.int14_fossil_write_block(second),
            0x1B => self.int14_fossil_driver_info(),
            _ => self.set_eax_ah(0x80),
        }
    }

    fn int14_fossil_keyboard_read(&mut self) {
        const KBD_BDA_BASE: usize = 0x400;
        const KBD_HEAD: usize = 0x1a;
        const KBD_TAIL: usize = 0x1c;
        const KBD_RING_START: u16 = 0x1e;
        const KBD_RING_END: u16 = 0x3e;

        let head = self.memory.read_u16(KBD_BDA_BASE + KBD_HEAD).unwrap_or(0);
        let tail = self.memory.read_u16(KBD_BDA_BASE + KBD_TAIL).unwrap_or(0);
        if head == tail {
            self.set_ax(0xFFFF);
            return;
        }
        let word = self
            .memory
            .read_u16(KBD_BDA_BASE + usize::from(head))
            .unwrap_or(0);
        let mut next = head + 2;
        if next >= KBD_RING_END {
            next = KBD_RING_START;
        }
        let _ = self.write_guest_ram_u16(KBD_BDA_BASE + KBD_HEAD, next);
        self.set_ax(word);
    }

    fn int14_fossil_read_block(&mut self, second: bool) {
        let max = self.cpu.registers.ecx() as u16;
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let di = self.cpu.registers.edi() as u16;
        let mut dst = es + u32::from(di);
        let mut count = 0u16;
        while count < max && self.uart_read(second, 5) & 0x01 != 0 {
            let byte = self.uart_read(second, 0);
            self.write_physical_u8(dst, byte);
            dst = dst.wrapping_add(1);
            count += 1;
        }
        self.set_ax(count);
    }

    fn int14_fossil_write_block(&mut self, second: bool) {
        let count = self.cpu.registers.ecx() as u16;
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let di = self.cpu.registers.edi() as u16;
        for index in 0..count {
            let byte = self.read_physical_u8(es + u32::from(di.wrapping_add(index)));
            self.uart_write(second, 0, byte);
            self.finish_uart_transmit(second);
        }
        self.set_ax(count);
    }

    fn int14_fossil_driver_info(&mut self) {
        let max = self.cpu.registers.ecx() as usize;
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let di = self.cpu.registers.edi() as u16;
        let mut info = [0u8; 21];
        let info_len = info.len();
        info[0..2].copy_from_slice(&(info_len as u16).to_le_bytes());
        info[2] = 5; // FOSSIL spec level.
        info[16] = 80;
        info[17] = 25;
        let count = max.min(info_len);
        self.write_guest_block(es + u32::from(di), &info[..count]);
        self.set_ax(count as u16);
    }

    fn uart_read(&mut self, second: bool, register: u16) -> u8 {
        if second {
            self.serial2.read_port(0x02f8 + register).unwrap_or(0)
        } else {
            self.serial.read_port(0x03f8 + register).unwrap_or(0)
        }
    }

    fn uart_write(&mut self, second: bool, register: u16, value: u8) {
        if second {
            self.serial2.write_port(0x02f8 + register, value);
        } else {
            self.serial.write_port(0x03f8 + register, value);
        }
    }

    /// Program the selected UART from an INT 14h AH=00h parameter byte: bits 7-5 baud
    /// rate, 4-3 parity, 2 stop bits, 1-0 word length. The divisor is stored for
    /// fidelity and drives transmit timing.
    fn uart_init(&mut self, second: bool, params: u8) {
        let divisor: u16 = match params >> 5 {
            0 => 1047, // 110 baud at 1.8432 MHz
            1 => 768,  // 150
            2 => 384,  // 300
            3 => 192,  // 600
            4 => 96,   // 1200
            5 => 48,   // 2400
            6 => 24,   // 4800
            _ => 12,   // 9600
        };
        // Word length (bits 1-0) and stop bits (bit 2) sit in the same positions in
        // the LCR; add the parity bits from AL bits 4-3 (01 odd, 11 even).
        let mut lcr = params & 0x07;
        match (params >> 3) & 0x03 {
            0b01 => lcr |= 0x08,        // parity enable, odd
            0b11 => lcr |= 0x08 | 0x10, // parity enable, even
            _ => {}                     // no parity
        }
        self.uart_write(second, 3, 0x80); // LCR DLAB=1
        self.uart_write(second, 0, (divisor & 0xff) as u8); // DLL
        self.uart_write(second, 1, (divisor >> 8) as u8); // DLM
        self.uart_write(second, 3, lcr); // LCR, clears DLAB
    }

    fn finish_uart_transmit(&mut self, second: bool) {
        let ticks = if second {
            self.serial2.ticks_until_idle()
        } else {
            self.serial.ticks_until_idle()
        };
        self.stall_for_master_ticks(ticks);
    }

    fn int14_extended_params_valid(&self) -> bool {
        let ax = self.cpu.registers.eax() as u16;
        let bx = self.cpu.registers.ebx() as u16;
        let cx = self.cpu.registers.ecx() as u16;
        let al = ax as u8;
        let bh = (bx >> 8) as u8;
        let bl = bx as u8;
        let ch = (cx >> 8) as u8;
        let cl = cx as u8;
        al <= 1 && bh <= 4 && bl <= 1 && ch <= 3 && cl <= 0x0b
    }

    /// Program the selected UART from the PS/2 INT 14h AH=04h extended-configuration
    /// fields: BH parity, BL stop bits, CH word length, CL baud-rate index.
    fn uart_extended_init(&mut self, second: bool) {
        let bx = self.cpu.registers.ebx() as u16;
        let cx = self.cpu.registers.ecx() as u16;
        let parity = (bx >> 8) as u8;
        let stop = bx as u8;
        let word = (cx >> 8) as u8;
        let baud = cx as u8;
        let divisor: u16 = match baud {
            0 => 1047, // 110
            1 => 768,  // 150
            2 => 384,  // 300
            3 => 192,  // 600
            4 => 96,   // 1200
            5 => 48,   // 2400
            6 => 24,   // 4800
            7 => 12,   // 9600
            8 => 6,    // 19200
            9 => 3,    // 38400
            10 => 2,   // 57600-ish, nearest whole divisor
            _ => 1,    // 115200
        };
        let mut lcr = word & 0x03;
        if stop != 0 {
            lcr |= 0x04;
        }
        match parity {
            1 => lcr |= 0x08,               // odd
            2 => lcr |= 0x08 | 0x10,        // even
            3 => lcr |= 0x08 | 0x20,        // stick odd
            4 => lcr |= 0x08 | 0x10 | 0x20, // stick even
            _ => {}
        }
        self.uart_write(second, 3, 0x80);
        self.uart_write(second, 0, (divisor & 0xff) as u8);
        self.uart_write(second, 1, (divisor >> 8) as u8);
        self.uart_write(second, 3, lcr);
    }

    /// Service INT 17h (PRINTER) over LPT1/LPT2 selected by DX=0/1. AH=00h
    /// prints AL, AH=01h initializes, AH=02h reads status. AH
    /// returns the BIOS printer-status byte.
    pub(super) fn handle_int17(&mut self) {
        let ax = self.cpu.registers.eax() as u16;
        let ah = (ax >> 8) as u8;
        let al = ax as u8;
        let port = self.cpu.registers.edx() as u16;
        if port >= 2 {
            self.set_eax_ah(0x01); // bit0 timeout: no such printer
            return;
        }
        let second = port == 1;
        if ah == 0x00 {
            // Latch the byte and pulse -Strobe so the LPT captures it.
            self.lpt_write(second, 0, al);
            let base = self.lpt_read(second, 2) & 0x1e; // keep bits 1-4
            self.lpt_write(second, 2, base | 0x01); // assert -Strobe (edge captures)
            self.lpt_write(second, 2, base); // de-assert
            let ticks = if second {
                self.lpt2.ticks_until_idle()
            } else {
                self.lpt.ticks_until_idle()
            };
            self.stall_for_master_ticks(ticks);
        } else if ah == 0x01 {
            self.lpt_write(second, 2, 0x00); // pulse active-low -Init
            self.lpt_write(second, 2, 0x04);
        }
        // AH=01h initialize and AH=02h are status-only. A completed print has
        // already passed through BUSY and -ACK, so the returned status is idle.
        let status = self.int17_printer_status(second);
        self.set_eax_ah(status);
    }

    fn lpt_read(&self, second: bool, register: u16) -> u8 {
        if second {
            self.lpt2.read_port(0x0278 + register).unwrap_or(0)
        } else {
            self.lpt.read_port(0x0378 + register).unwrap_or(0)
        }
    }

    fn lpt_write(&mut self, second: bool, register: u16, value: u8) {
        if second {
            self.lpt2.write_port(0x0278 + register, value);
        } else {
            self.lpt.write_port(0x0378 + register, value);
        }
    }

    /// Translate the selected LPT status port into the INT 17h status byte: keep bits 7-3
    /// and flip -ACK (bit6) and -Error (bit3) so "acknowledge" and "I/O error" read
    /// in the BIOS sense. An always-ready printer yields 0x90 (not busy, selected).
    fn int17_printer_status(&self, second: bool) -> u8 {
        let port = self.lpt_read(second, 1);
        (port & 0xf8) ^ 0x48
    }

    /// Service the host side of INT 15h. AH=88h returns the extended memory size
    /// (KiB above 1 MiB) in AX with CF clear, the standard way a BIOS learns RAM
    /// size on a machine with no probing path. Capped at 0xFFFF KiB (64 MiB) to
    /// fit the 16-bit AX return; other subfunctions report CF set (unsupported).
    pub(super) fn handle_int15(&mut self) {
        let ax = self.cpu.registers.eax() as u16;
        let ah = (ax >> 8) as u8;
        let al = ax as u8;
        if matches!(
            ax,
            0x1000..=0x1025 | 0x102B..=0x102D | 0xDE00..=0xDE12
        ) {
            self.int15_report_absent_window_manager();
            return;
        }
        match ah {
            // AH=00h-03h cassette services (PC/PCjr). This profile has no cassette.
            0x00..=0x03 => {
                self.set_eax_ah(0x86);
                self.set_int_frame_carry(true);
            }
            // AH=4Fh keyboard intercept. With no resident hook, keep the scan code.
            0x4F => self.set_int_frame_carry(true),
            // AH=80h-82h OS device hooks. The default BIOS handler succeeds.
            0x80..=0x82 => {
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // AH=88h extended memory size in KiB (existing behavior).
            0x88 => {
                let extended_kib = u32::from(self.profile.memory_mib.saturating_sub(1)) * 1024;
                let value = extended_kib.min(0xFFFF) as u16;
                let eax = (self.cpu.registers.eax() & !0xFFFF) | u32::from(value);
                self.cpu.registers.set_eax(eax);
                self.set_int_frame_carry(false);
            }
            // AH=86h WAIT: CX:DX microseconds.
            0x86 => {
                let micros = (u64::from(self.cpu.registers.ecx() as u16) << 16)
                    | u64::from(self.cpu.registers.edx() as u16);
                self.stall_for_micros(micros);
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // AH=87h block move: ES:SI -> GDT; copy CX words src->dst across 1 MB.
            0x87 => self.int15_block_move(),
            // AH=8Ah extended memory size in KiB as a 32-bit DX:AX (the >64 MB-capable
            // sibling of AH=88h, which saturates at 0xFFFF).
            0x8A => {
                let ext_kib = u32::from(self.profile.memory_mib).saturating_sub(1) * 1024;
                self.set_ax(ext_kib as u16);
                self.set_dx((ext_kib >> 16) as u16);
                self.set_int_frame_carry(false);
            }
            // AH=0Fh format-unit periodic interrupt: continue the ESDI format.
            0x0F => {
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // TopView/DESQview, PRINT.COM, and Convertible profile/power calls.
            0x10..=0x12 | 0x20 | 0x40..=0x44 => {
                self.int15_report_absent_window_manager();
            }
            // AH=21h POST error log.
            0x21 => self.int15_post_error_log(al),
            // AX=E801h/E820h/E881h memory-size and memory-map queries (AH=E8h group).
            0xE8 => match self.cpu.registers.eax() as u8 {
                0x01 => self.int15_e801(false),
                0x81 => self.int15_e801(true),
                0x20 => self.int15_e820(),
                _ => self.set_int_frame_carry(true),
            },
            // AH=24h A20 gate (later PS/2s). The 8042 output-port bit 1 is the
            // single A20 state, shared with the fast-A20 port 0x92. The address
            // space is already flat, so this tracks and reports state without
            // masking. AL selects: 00 disable, 01 enable, 02 status, 03 support.
            0x24 => match al {
                0x00 => {
                    self.set_a20_gate(false);
                    self.set_eax_ah(0x00);
                    self.set_int_frame_carry(false);
                }
                0x01 => {
                    self.set_a20_gate(true);
                    self.set_eax_ah(0x00);
                    self.set_int_frame_carry(false);
                }
                0x02 => {
                    self.set_eax_ah(0x00);
                    self.set_eax_al(u8::from(self.keyboard.a20_enabled()));
                    self.set_int_frame_carry(false);
                }
                0x03 => {
                    self.set_eax_ah(0x00);
                    // Bit 0 keyboard controller, bit 1 port 0x92: both supported.
                    self.set_bx(0x0003);
                    self.set_int_frame_carry(false);
                }
                // Undefined subfunction: report function-not-supported.
                _ => {
                    self.set_eax_ah(0x86);
                    self.set_int_frame_carry(true);
                }
            },
            // AH=90h device-wait / AH=91h device-post are OS hooks. With no OS hook
            // installed the BIOS returns "no wait performed" with CF clear, rather than
            // the unsupported-function carry the catch-all would set.
            0x90 | 0x91 => self.set_int_frame_carry(false),
            // AH=83h event wait, AH=84h joystick, AH=85h SysReq hook, AH=89h
            // protected-mode switch.
            0x83 => self.int15_event_wait(al),
            0x84 => self.int15_joystick(),
            0x85 => {
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            0x89 => {
                self.set_eax_ah(0x86);
                self.set_int_frame_carry(true);
            }
            // AH=C0h get system-configuration table: ES:BX -> the table seeded at POST.
            0xC0 => {
                let seg = (BIOS_CONFIG_TABLE_ADDR >> 4) as u16;
                let off = (BIOS_CONFIG_TABLE_ADDR & 0xf) as u16;
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Es, SegmentRegister::real(seg));
                self.set_bx(off);
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // AH=C1h get extended BIOS data area segment: ES = the EBDA segment.
            0xC1 => {
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Es, SegmentRegister::real(EBDA_SEGMENT));
                self.set_int_frame_carry(false);
            }
            // AH=C2h PS/2 pointing-device (mouse) BIOS interface. AL selects the
            // subfunction.
            0xC2 => self.int15_c2_pointing_device(al),
            // AH=C3h/C4h PS/2 watchdog and POS are absent on the base profile.
            0xC3 | 0xC4 => {
                self.set_eax_ah(0x86);
                self.set_int_frame_carry(true);
            }
            _ => self.set_int_frame_carry(true),
        }
    }

    fn int15_report_absent_window_manager(&mut self) {
        self.set_bx(0x0000);
        self.set_eax_ah(0x86);
        self.set_int_frame_carry(true);
    }

    /// INT 15h AH=21h POST error log. AL=00 reads the resident log, AL=01 appends
    /// one device/error pair (BH=device, BL=error).
    fn int15_post_error_log(&mut self, al: u8) {
        match al {
            0x00 => {
                let count = self.read_physical_u8(BIOS_POST_ERROR_LOG_COUNT_ADDR);
                self.set_bx(u16::from(count.min(BIOS_POST_ERROR_LOG_MAX)));
                let seg = (BIOS_POST_ERROR_LOG_ADDR >> 4) as u16;
                let off = (BIOS_POST_ERROR_LOG_ADDR & 0xf) as u16;
                self.cpu
                    .registers
                    .set_segment(SegmentIndex::Es, SegmentRegister::real(seg));
                let edi = (self.cpu.registers.edi() & !0xFFFF) | u32::from(off);
                self.cpu.registers.set_edi(edi);
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            0x01 => {
                let count = self.read_physical_u8(BIOS_POST_ERROR_LOG_COUNT_ADDR);
                if count >= BIOS_POST_ERROR_LOG_MAX {
                    self.set_eax_ah(0x01);
                    self.set_int_frame_carry(true);
                    return;
                }
                let bx = self.cpu.registers.ebx() as u16;
                let device = (bx >> 8) as u8;
                let error = bx as u8;
                let addr = BIOS_POST_ERROR_LOG_ADDR + u32::from(count) * 2;
                let _ = self.write_guest_ram_u8(addr as usize, error);
                let _ = self.write_guest_ram_u8(addr as usize + 1, device);
                let _ = self.write_guest_ram_u8(BIOS_POST_ERROR_LOG_COUNT_ADDR as usize, count + 1);
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            _ => {
                self.set_eax_ah(0x02);
                self.set_int_frame_carry(true);
            }
        }
    }

    /// INT 15h AH=83h event wait. The machine has no async RTC wait queue yet, so
    /// it advances the guest clock, sets the completion byte, and returns.
    fn int15_event_wait(&mut self, al: u8) {
        match al {
            0x00 => {
                let micros = (u64::from(self.cpu.registers.ecx() as u16) << 16)
                    | u64::from(self.cpu.registers.edx() as u16);
                let es = self.cpu.registers.segment(SegmentIndex::Es).base;
                let bx = self.cpu.registers.ebx() as u16;
                let addr = es.wrapping_add(u32::from(bx));
                self.stall_for_micros(micros);
                let byte = self.read_physical_u8(addr);
                let _ = self.write_guest_ram_u8(addr as usize, byte | 0x80);
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            0x01 => {
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            _ => {
                self.set_eax_ah(0x86);
                self.set_int_frame_carry(true);
            }
        }
    }

    /// INT 15h AH=84h joystick BIOS support. No game port is installed, which the
    /// BIOS reports as open switches and zeroed position counters.
    fn int15_joystick(&mut self) {
        match self.cpu.registers.edx() as u16 {
            0x0000 => {
                self.set_ax(0x0000);
                self.set_int_frame_carry(false);
            }
            0x0001 => {
                self.set_ax(0x0000);
                self.set_bx(0x0000);
                self.set_cx(0x0000);
                self.set_dx(0x0000);
                self.set_int_frame_carry(false);
            }
            _ => {
                self.set_eax_ah(0x80);
                self.set_int_frame_carry(true);
            }
        }
    }

    /// INT 15h AH=C2h PS/2 pointing-device interface (RBIL INTERRUP.C). Handles the
    /// query subset a guest probes the BIOS mouse with: enable/disable (C200), reset
    /// (C201), set sample rate (C202), set resolution (C203), get device type
    /// (C204), initialize (C205), and the extended-command group (C206). The aux
    /// device is the same standard PS/2 mouse INT 33h models, so the reset reports
    /// the self-test-passed/device-id bytes a real mouse returns. C207 (set the
    /// device handler) stores the ES:BX far pointer in the EBDA and returns success;
    /// the BIOS INT 74h ISR (izbios ROM) far-calls that pointer on each completed
    /// 3-byte PS/2 packet. C208/C209 (read/write the raw device port) report
    /// function-not-supported (AH=86h, CF set).
    fn int15_c2_pointing_device(&mut self, al: u8) {
        let bh = (self.cpu.registers.ebx() as u16 >> 8) as u8;
        match al {
            // C200 enable/disable (BH=0 disable, 1 enable). Enable or disable
            // hardware aux data reporting so IRQ12 packets stream to the guest
            // INT 74h ISR. Enabling the pointing device also arms IRQ12 in the
            // 8042 command byte (a real PS/2 BIOS does both); without that, a
            // latched aux byte never raises the interrupt and the ISR never runs.
            0x00 => {
                if bh != 0 {
                    self.enable_pointing_device();
                } else {
                    // C200 disable: stop reporting and mask IRQ12. Leave the wheel
                    // mode and EBDA packet size untouched. Known ceiling: the platform
                    // drives the device to 4-byte at enable and assumes it stays. A
                    // guest that resets the aux device (0xFF) and stays 3-byte would
                    // desync the BIOS int74 ISR (still expecting 4 bytes); no consumer
                    // does this today.
                    self.keyboard.set_mouse_reporting(false);
                    self.keyboard.set_mouse_irq(false);
                    self.pic.set_irq_level(12, false);
                }
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C201 reset: BH=0x00 (device id, a standard mouse), BL=0xAA (the
            // reset-complete/BAT-passed signature the device returns; drivers probe
            // for AAh here). Acknowledge with the signature.
            0x01 => {
                let ebx = (self.cpu.registers.ebx() & !0xFFFF) | 0x00AA;
                self.cpu.registers.set_ebx(ebx);
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C202 set sample rate (BH=rate code 0-6).
            0x02 if self.keyboard.set_mouse_sample_rate_code(bh) => {
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            0x02 => {
                self.set_eax_ah(0x86);
                self.set_int_frame_carry(true);
            }
            // C203 set resolution (BH=0-3): no hardware resolution is modeled, so
            // accept and ignore.
            0x03 => {
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C204 get device type: BH=0x00 (a standard PS/2 mouse).
            0x04 => {
                let ebx = self.cpu.registers.ebx() & !0xFF00; // BH=0
                self.cpu.registers.set_ebx(ebx);
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C205 initialize (BH=packet size, 3 for a standard mouse): enable
            // hardware aux reporting, arm IRQ12 in the 8042 command byte, and
            // acknowledge. The driver does a C200 enable afterwards too; both
            // leave reporting on and IRQ12 armed without re-centring.
            0x05 => {
                self.enable_pointing_device();
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C206 extended commands: BH=00 return device status (3 bytes in BL/CL/DL),
            // BH=01/02 set scaling 1:1 / 2:1, BH=03 set resolution. The status bytes
            // describe a stream-mode, scaling-1:1, enabled mouse at the default
            // resolution and sample rate.
            0x06 => {
                if bh == 0x00 {
                    // Status byte 1 (BL): bit5 mouse enabled. Status byte 2 (CL):
                    // resolution code 2. Status byte 3 (DL): current sample rate.
                    let ebx = (self.cpu.registers.ebx() & !0xFF) | 0x20;
                    self.cpu.registers.set_ebx(ebx);
                    let ecx = (self.cpu.registers.ecx() & !0xFF) | 0x02;
                    self.cpu.registers.set_ecx(ecx);
                    let edx = (self.cpu.registers.edx() & !0xFF)
                        | u32::from(self.keyboard.mouse_sample_rate());
                    self.cpu.registers.set_edx(edx);
                }
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C207 set device handler: store the ES:BX far pointer the guest is
            // installing into the EBDA (offset word then segment word) and report
            // success. ES=0:BX=0 deregisters. The producer is the BIOS INT 74h ISR
            // in the izbios ROM: it assembles each 3-byte PS/2 packet and far-calls
            // this pointer with the standard 4-word frame. C208/C209 (the raw
            // device-port read/write) stay unsupported.
            0x07 => {
                // The far pointer's segment is the literal ES the guest passed (the
                // selector), not the derived physical base.
                let es = self.cpu.registers.segment(SegmentIndex::Es).selector;
                let bx = self.cpu.registers.ebx() as u16;
                let base = (u32::from(EBDA_SEGMENT) << 4) + EBDA_MOUSE_HANDLER_OFF;
                self.write_guest_block(base, &bx.to_le_bytes());
                self.write_guest_block(base + 2, &es.to_le_bytes());
                self.set_eax_ah(0x00);
                self.set_int_frame_carry(false);
            }
            // C208/C209 raw device-port read/write: no raw aux-port path is wired.
            // Report function-not-supported.
            _ => {
                self.set_eax_ah(0x86);
                self.set_int_frame_carry(true);
            }
        }
    }

    /// Enable the pointing device the way the INT 15h C200-enable and C205-init
    /// services do: turn on aux data reporting, arm IRQ12 in the 8042 command byte,
    /// and (since our emulated mouse always has a wheel) put it in IntelliMouse
    /// 4-byte mode. The matching EBDA packet-size byte is set to 4 so the BIOS INT
    /// 74h ISR accumulates the wheel byte and delivers it as the frame's Z word.
    fn enable_pointing_device(&mut self) {
        self.keyboard.set_mouse_reporting(true);
        self.keyboard.set_mouse_irq(true);
        self.pic.set_irq_level(12, self.keyboard.irq12_level());
        self.keyboard.enable_mouse_wheel();
        // Tell the BIOS ISR to assemble 4-byte packets. Same EBDA-base computation
        // the C207 handler uses for the handler pointer, at the packet-size offset.
        let pkt_size = (u32::from(EBDA_SEGMENT) << 4) + EBDA_MOUSE_PKT_SIZE_OFF;
        self.write_guest_block(pkt_size, &[4]);
    }

    /// INT 15h AX=E801h (and the AX=E881h 32-bit variant). Reports extended memory in two
    /// pieces the way DOS extenders and HIMEM expect: the 1-16 MB range in KB (AX/CX,
    /// capped at 0x3C00 = 15 MB) and the memory above 16 MB in 64 KB blocks (BX/DX). E881h
    /// returns the same magnitudes in the full 32-bit registers.
    fn int15_e801(&mut self, wide: bool) {
        let ext_kib = u32::from(self.profile.memory_mib) * 1024;
        let ext_kib = ext_kib.saturating_sub(1024); // memory above the first 1 MB
        let below_16m = ext_kib.min(15 * 1024); // 1-16 MB range, max 0x3C00 KB
        let above_16m_blocks = ext_kib.saturating_sub(15 * 1024) / 64; // 64 KB blocks
        if wide {
            self.cpu.registers.set_eax(below_16m);
            self.cpu.registers.set_ebx(above_16m_blocks);
            self.cpu.registers.set_ecx(below_16m);
            self.cpu.registers.set_edx(above_16m_blocks);
        } else {
            self.set_ax(below_16m as u16);
            self.set_bx(above_16m_blocks as u16);
            self.set_cx(below_16m as u16);
            self.set_dx(above_16m_blocks as u16);
        }
        self.set_int_frame_carry(false);
    }

    /// The system memory map E820h enumerates conventional RAM below the EBDA, the reserved
    /// video/ROM hole below 1 MB, and a single available region for everything above 1 MB.
    fn e820_regions(&self) -> Vec<(u64, u64, u32)> {
        let total = u64::from(self.profile.memory_mib) * 0x10_0000;
        let mut regions = vec![
            (0x0u64, CONVENTIONAL_MEMORY_TOP, 1u32),
            (u64::from(EBDA_LINEAR), 0x400, 2),
            (0xA_0000, 0x6_0000, 2),     // video + ROM BIOS hole, reserved
        ];
        if total > 0x10_0000 {
            regions.push((0x10_0000, total - 0x10_0000, 1)); // extended RAM, available
        }
        regions
    }

    /// INT 15h AX=E820h. Walks the memory map one 20-byte descriptor per call: EDX must
    /// carry 'SMAP', EBX is the continuation index (0 to start), ES:DI is the buffer. Each
    /// call returns EAX='SMAP', ECX=20, the descriptor written, and EBX advanced to the
    /// next index or 0 once the last region has been returned.
    fn int15_e820(&mut self) {
        const SMAP: u32 = 0x534D_4150;
        if self.cpu.registers.edx() != SMAP || (self.cpu.registers.ecx() as u16) < 20 {
            self.set_int_frame_carry(true);
            return;
        }
        let regions = self.e820_regions();
        let index = self.cpu.registers.ebx() as usize;
        let Some(&(base, len, kind)) = regions.get(index) else {
            self.set_int_frame_carry(true);
            return;
        };
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let di = self.cpu.registers.edi() as u16;
        let addr = es.wrapping_add(u32::from(di));
        let mut desc = [0u8; 20];
        desc[0..8].copy_from_slice(&base.to_le_bytes());
        desc[8..16].copy_from_slice(&len.to_le_bytes());
        desc[16..20].copy_from_slice(&kind.to_le_bytes());
        self.write_guest_block(addr, &desc);
        self.cpu.registers.set_eax(SMAP);
        self.cpu.registers.set_ecx(20);
        let next = index + 1;
        let continuation = if next < regions.len() { next as u32 } else { 0 };
        self.cpu.registers.set_ebx(continuation);
        self.set_int_frame_carry(false);
    }

    /// INT 15h AH=87h. ES:SI points at a 48-byte GDT the caller built; the source
    /// descriptor is at +0x10 and the destination at +0x18. Each descriptor holds
    /// a 24-bit base across bytes 2,3,4 and the high 8 bits at byte 7. Copies CX
    /// words. This is the standard path HIMEM and DOS extenders use to reach
    /// extended memory from real mode.
    fn int15_block_move(&mut self) {
        let es = self.cpu.registers.segment(SegmentIndex::Es).base;
        let si = self.cpu.registers.esi() as u16;
        let gdt = es.wrapping_add(u32::from(si));
        let base_at = |s: &mut Self, desc: u32| -> u32 {
            u32::from(s.read_physical_u8(desc + 2))
                | (u32::from(s.read_physical_u8(desc + 3)) << 8)
                | (u32::from(s.read_physical_u8(desc + 4)) << 16)
                | (u32::from(s.read_physical_u8(desc + 7)) << 24)
        };
        let src = base_at(self, gdt + 0x10);
        let dst = base_at(self, gdt + 0x18);
        // CX is a word count capped at 0x8000 (64 KB); larger requests are clamped.
        let words = (self.cpu.registers.ecx() as u16).min(0x8000);
        let bytes = usize::from(words) * 2;
        let data = self.read_guest_block(src, bytes);
        self.write_guest_block(dst, &data);
        self.set_eax_ah(0x00);
        self.set_int_frame_carry(false);
    }

    /// Service INT 1Ah. AH=00h/01h read and set the BDA timer tick the ROM int08
    /// maintains; AH=02h/04h read the RTC time and date as BCD (the documented
    /// contract, converted from the binary CMOS). AH=03h/05h/06h/07h are accepted
    /// as no-ops with CF clear, since the host drives the clock.
    pub(super) fn handle_int1a(&mut self) {
        let ax = self.cpu.registers.eax() as u16;
        if ax & 0xff00 == 0xb100 {
            self.handle_pci_bios(false);
            return;
        }
        let ah = (self.cpu.registers.eax() as u16 >> 8) as u8;
        match ah {
            // AH=00h/01h read and set the BIOS tick count; neither reports status
            // in CF, so leaving the carry flag untouched here is intentional.
            0x00 => {
                let ticks = self.read_guest_dword(0x46c);
                let rollover = self.read_physical_u8(0x470);
                let _ = self.write_guest_ram_u8(0x470, 0);
                self.set_eax_al(rollover);
                self.set_cx((ticks >> 16) as u16);
                self.set_dx(ticks as u16);
            }
            0x01 => {
                let cx = self.cpu.registers.ecx() as u16;
                let dx = self.cpu.registers.edx() as u16;
                let _ = self.write_guest_ram_u16(0x46c, dx);
                let _ = self.write_guest_ram_u16(0x46e, cx);
                let _ = self.write_guest_ram_u8(0x470, 0);
            }
            0x02 => {
                let (_, _, _, _, hour, minute, second) = self.rtc.clock();
                let cx = (u16::from(bin_to_bcd(hour)) << 8) | u16::from(bin_to_bcd(minute));
                let dx = u16::from(bin_to_bcd(second)) << 8; // DL = 0 (no DST)
                self.set_cx(cx);
                self.set_dx(dx);
                self.set_int_frame_carry(false);
            }
            0x04 => {
                let (year, month, day, ..) = self.rtc.clock();
                let century = bin_to_bcd(self.rtc.century());
                let yy = bin_to_bcd((year % 100) as u8);
                let cx = (u16::from(century) << 8) | u16::from(yy);
                let dx = (u16::from(bin_to_bcd(month)) << 8) | u16::from(bin_to_bcd(day));
                self.set_cx(cx);
                self.set_dx(dx);
                self.set_int_frame_carry(false);
            }
            // AH=09h read RTC alarm time and status. No alarm source is armed, so
            // return zero time with DL=00h (alarm not enabled).
            0x09 => {
                self.set_cx(0x0000);
                self.set_dx(0x0000);
                self.set_int_frame_carry(false);
            }
            // AH=03h set RTC time: CH/CL/DH are BCD hours/minutes/seconds (DL = DST flag,
            // not modeled). Re-seed the clock keeping the current date.
            0x03 => {
                let cx = self.cpu.registers.ecx() as u16;
                let dx = self.cpu.registers.edx() as u16;
                let hour = bcd_to_bin((cx >> 8) as u8);
                let minute = bcd_to_bin(cx as u8);
                let second = bcd_to_bin((dx >> 8) as u8);
                let (year, month, day, weekday, ..) = self.rtc.clock();
                self.rtc
                    .seed(year, month, day, weekday, hour, minute, second);
                self.set_int_frame_carry(false);
            }
            // AH=05h set RTC date: CH/CL are BCD century/year, DH/DL BCD month/day.
            // Re-seed keeping the current time.
            0x05 => {
                let cx = self.cpu.registers.ecx() as u16;
                let dx = self.cpu.registers.edx() as u16;
                let century = bcd_to_bin((cx >> 8) as u8);
                let yy = bcd_to_bin(cx as u8);
                let month = bcd_to_bin((dx >> 8) as u8);
                let day = bcd_to_bin(dx as u8);
                let year = u16::from(century) * 100 + u16::from(yy);
                let (_, _, _, weekday, hour, minute, second) = self.rtc.clock();
                self.rtc
                    .seed(year, month, day, weekday, hour, minute, second);
                // Persist the century to CMOS 0x32 so it survives an NVRAM reload.
                self.rtc.set_century(century);
                self.set_int_frame_carry(false);
            }
            // AH=0Ah read the system-timer day counter: CX = days since 1980-01-01,
            // derived from the host-authoritative RTC calendar. AL = 0 (no rollover).
            0x0A => {
                let (year, month, day, ..) = self.rtc.clock();
                self.set_cx(days_since_1980(year, month, day));
                self.set_eax_al(0);
                self.set_int_frame_carry(false);
            }
            // AH=0Bh set the system-timer day counter: store CX in the BDA scratch
            // word so a later read returns it. The RTC calendar stays authoritative
            // for AH=0Ah, so this is a write-through latch the BIOS keeps for the OS.
            0x0B => {
                let cx = self.cpu.registers.ecx() as u16;
                let _ = self.write_guest_ram_u16(BDA_DAY_COUNT, cx);
                self.set_int_frame_carry(false);
            }
            // AH=06h/07h set/cancel alarm: no alarm hardware modeled, accept and ignore.
            // AH=08h/0Ch set power-on alarm/date, AH=0Dh reset, AH=0Fh initialize RTC: all
            // documented as succeeding, and the host-driven clock makes them no-ops.
            // Limit: power-management and alarm hardware are not modeled; these return
            // success without persisting state. AH=0Eh keeps the default carry since no
            // power-on alarm date is stored.
            0x06 | 0x07 | 0x08 | 0x0C | 0x0D | 0x0F => self.set_int_frame_carry(false),
            // AH=80h PCjr/Tandy sound multiplexor. A Tandy 1000SL/TL BIOS exposes
            // this as a bare IRET; the base profile keeps the caller state intact.
            0x80 => {}
            _ => self.set_int_frame_carry(true),
        }
    }

    /// PCI BIOS 2.10 services shared by real-mode INT 1Ah and the BIOS32 entry.
    /// BIOS32 calls report carry in live EFLAGS; INT 1Ah patches the saved FLAGS.
    pub(super) fn handle_pci_bios(&mut self, live_flags: bool) {
        const SUCCESS: u8 = 0x00;
        const FUNC_NOT_SUPPORTED: u8 = 0x81;
        const BAD_VENDOR_ID: u8 = 0x83;
        const DEVICE_NOT_FOUND: u8 = 0x86;
        const BAD_REGISTER: u8 = 0x87;

        let function = self.cpu.registers.eax() as u16;
        let mut status = SUCCESS;
        match function {
            0xB101 => {
                self.cpu.registers.set_edx(0x2049_4350); // "PCI "
                self.set_bx(0x0210); // PCI BIOS 2.10
                self.set_cl(0); // last bus
                self.set_eax_al(1); // configuration mechanism #1
            }
            0xB102 => {
                let vendor = self.cpu.registers.edx() as u16;
                let device = self.cpu.registers.ecx() as u16;
                let occurrence = self.cpu.registers.esi() as u16;
                if vendor == 0xffff {
                    status = BAD_VENDOR_ID;
                } else if let Some((bus, devfn)) = self.pci_find(occurrence, |id, _| {
                    id as u16 == vendor && (id >> 16) as u16 == device
                }) {
                    self.set_bx((u16::from(bus) << 8) | u16::from(devfn));
                } else {
                    status = DEVICE_NOT_FOUND;
                }
            }
            0xB103 => {
                let class = self.cpu.registers.ecx() & 0x00ff_ffff;
                let occurrence = self.cpu.registers.esi() as u16;
                if let Some((bus, devfn)) =
                    self.pci_find(occurrence, |_, class_reg| class_reg >> 8 == class)
                {
                    self.set_bx((u16::from(bus) << 8) | u16::from(devfn));
                } else {
                    status = DEVICE_NOT_FOUND;
                }
            }
            0xB108..=0xB10D => {
                let bx = self.cpu.registers.ebx() as u16;
                let bus = (bx >> 8) as u8;
                let devfn = bx as u8;
                let offset = self.cpu.registers.edi() as u16;
                let width = match function {
                    0xB108 | 0xB10B => BusWidth::Byte,
                    0xB109 | 0xB10C => BusWidth::Word,
                    _ => BusWidth::Dword,
                };
                let size = width.bytes() as u16;
                if u32::from(offset) + u32::from(size) > 0x100 || offset & (size - 1) != 0 {
                    status = BAD_REGISTER;
                } else if self.pci.read_bdf(bus, devfn, 0, BusWidth::Word, &self.vega) == 0xffff {
                    status = DEVICE_NOT_FOUND;
                } else if function <= 0xB10A {
                    let value = self
                        .pci
                        .read_bdf(bus, devfn, offset as u8, width, &self.vega);
                    match width {
                        BusWidth::Byte => self.set_cl(value as u8),
                        BusWidth::Word => self.set_cx(value as u16),
                        BusWidth::Dword => self.cpu.registers.set_ecx(value),
                    }
                } else {
                    // Keep in step with the post-PCI block in MachineBus::write_io:
                    // a config write can retarget Distira memory decode (rebuild the
                    // RAM lookup, invalidate Direct maps) or the PIIX IDE
                    // command/BAR (resynchronize BMIDE). The HLE path stays
                    // traceless and charges no I/O wait states, like the reads.
                    let value = self.cpu.registers.ecx();
                    let pci_decode = self.vega.memory_decode_key();
                    self.pci
                        .write_bdf(bus, devfn, offset as u8, width, value, &mut self.vega);
                    if self.vega.memory_decode_key() != pci_decode {
                        self.ram_lookup.rebuild(self.memory.len(), &self.vega);
                        self.mark_direct_map_changed();
                    }
                    if let Some(disk) = self.ata.as_mut() {
                        self.bmide.synchronize(
                            self.pci.ide_bus_master_enabled(),
                            &self.memory,
                            disk,
                        );
                    }
                }
            }
            _ => status = FUNC_NOT_SUPPORTED,
        }
        self.set_eax_ah(status);
        if live_flags {
            if status == SUCCESS {
                self.cpu.registers.eflags &= !1;
            } else {
                self.cpu.registers.eflags |= 1;
            }
        } else {
            self.set_int_frame_carry(status != SUCCESS);
        }
    }

    pub(super) fn handle_bios32_directory(&mut self) {
        if self.cpu.registers.eax() == u32::from_le_bytes(*b"$PCI") {
            self.set_eax_al(0);
            self.cpu.registers.set_ebx(0x000F_0000);
            self.cpu.registers.set_ecx(0x0001_0000);
            self.cpu.registers.set_edx(BIOS32_PCI_ROM_OFFSET as u32);
        } else {
            self.set_eax_al(0x80);
        }
    }

    fn pci_find(
        &self,
        occurrence: u16,
        mut matches: impl FnMut(u32, u32) -> bool,
    ) -> Option<(u8, u8)> {
        let mut found = 0u16;
        for devfn in 0u8..=u8::MAX {
            let id = self.pci.read_bdf(0, devfn, 0, BusWidth::Dword, &self.vega);
            if id as u16 == 0xffff {
                continue;
            }
            let class = self.pci.read_bdf(0, devfn, 8, BusWidth::Dword, &self.vega);
            if matches(id, class) {
                if found == occurrence {
                    return Some((0, devfn));
                }
                found = found.wrapping_add(1);
            }
        }
        None
    }

    /// Point CS:IP at a real-mode far address. Used by the boot vectors to
    /// redirect execution instead of returning through the INT's IRET stub: the
    /// run loop steps the CPU from these registers on its next iteration, so the
    /// guest resumes at `seg:off` as if the BIOS had far-jumped there.
    fn set_cs_ip(&mut self, seg: u16, off: u16) {
        self.cpu
            .registers
            .set_segment(SegmentIndex::Cs, SegmentRegister::real(seg));
        self.cpu.registers.eip = u32::from(off);
    }

    /// Service INT 19h (BOOTSTRAP LOADER). Re-run the boot: load the boot sector of
    /// the default drive to 0000:7C00 and jump there. The default drive is A: when
    /// a floppy is mounted, otherwise the Katea ATA fixed disk (80h) when it carries
    /// a 0x55AA MBR signature. When neither is bootable, fall through to the INT 18h
    /// path. DL carries the drive the loaded code booted from (00h floppy, 80h fixed
    /// disk), the way a real BIOS leaves it.
    ///
    /// This mirrors the izarra-bios ROM's own INT 19h: a mounted floppy is treated
    /// as bootable and sector 0 is loaded with no 0xAA55 signature check, so a guest
    /// re-invoking INT 19h gets the same outcome the ROM gives at power-on.
    ///
    /// Limit: the floppy boot copies sector 0 and jumps; it does not retry on a
    /// read error. The fixed-disk boot loads the real MBR at LBA 0 (signature-gated)
    /// and lets it chain to the active partition. The retired Rust Toka-DOS HLE
    /// boot record no longer backs a non-bootable C: drive.
    pub(super) fn handle_int19(&mut self) {
        if self.read_physical_u8(BIOS_BOOT_CHOICE_ADDR) == 2 {
            if self.boot_el_torito() {
                return;
            }
            self.handle_int18();
            return;
        }
        // A: floppy first. Copy its boot sector (CHS 0,0,1) to 0000:7C00 and jump
        // there. A mounted floppy is bootable (matching the ROM path); only an
        // unreadable sector 0 falls through.
        if let Some(sector) = self
            .floppy
            .as_ref()
            .and_then(|f| f.read_sector(0, 0, 1))
            .filter(|s| s.len() >= 512)
            .map(<[u8]>::to_vec)
        {
            self.write_guest_block(BOOT_SECTOR_ADDRESS as u32, &sector[..512]);
            self.cpu.registers.set_edx(0x00); // DL = 00h: booted from floppy A:
            // The floppy's own sector-0 code is the OS now, so the HLE Toka-DOS
            // and IZEMM stand down and the disk owns the DOS interrupts through the
            // IVT. Real hardware just runs whatever sector 0 holds; this confines
            // the HLE injection to the C: boot below.
            self.booter_inert = true;
            self.set_cs_ip(0x0000, BOOT_SECTOR_ADDRESS as u16);
            return;
        }
        // Fixed disk (Katea ATA primary master): boot from LBA 0 if it carries a
        // boot signature. Unlike the floppy path, INT 13h stays intercepted so
        // Katea keeps serving disk I/O to the booted OS. DL=80h = first fixed disk.
        if let Some(sector0) = self
            .ata
            .as_ref()
            .and_then(|d| d.read_lba(0))
            .filter(|s| s[510] == 0x55 && s[511] == 0xAA)
        {
            self.write_guest_block(BOOT_SECTOR_ADDRESS as u32, &sector0[..512]);
            self.cpu.registers.set_edx(0x80);
            self.booter_inert = true;
            self.set_cs_ip(0x0000, BOOT_SECTOR_ADDRESS as u16);
            return;
        }
        // Nothing bootable (no signed floppy or ATA MBR): the retired Rust
        // Toka-DOS HLE boot fallback is absent, so hand off to the diskless/no-boot
        // path exactly like the firmware's .disk_absent branch.
        self.handle_int18();
    }

    /// Service INT 18h (DISKLESS BOOT HOOK). On a real PC this entered ROM BASIC;
    /// the Izarra 3000 has none, so it reports no bootable device and halts. The
    /// halt stub clears IF first, so the machine genuinely stops rather than
    /// spinning on the timer tick.
    pub(super) fn handle_int18(&mut self) {
        // A real BIOS prints a "no bootable device" message here. The text screen
        // is the BIOS's, so write the line through the same teletype path the rest
        // of the BIOS uses, then jump to the CLI;HLT stub.
        for &byte in b"No bootable device\r\n" {
            self.teletype_char(byte);
        }
        self.set_cs_ip(0x0000, BIOS_HALT_STUB_ADDRESS as u16);
    }
}
