// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! AT keyboard controller (8042-class) as software sees it: status/data ports,
//! a host-fed scancode FIFO, and the command subset the BIOS uses at boot. The
//! controller also multiplexes a PS/2 auxiliary (mouse) device, reachable
//! through the 0xD4 "write to aux" command and reporting on IRQ12.
//! Command set is boot-minimal; extend when the BIOS needs more.

use crate::mouse::Ps2Mouse;
use std::collections::VecDeque;

const STATUS_OBF: u8 = 0x01; // output buffer full (data waiting on 0x60)
const STATUS_IBF: u8 = 0x02; // input buffer full (controller is processing a write)
const STATUS_SYS: u8 = 0x04; // system flag, set after a passed self-test
const STATUS_CMD: u8 = 0x08; // last accepted input was written to command port 0x64
const STATUS_AUX: u8 = 0x20; // the byte in the output buffer came from the mouse

const CONTROLLER_INPUT_TICKS: u64 = izarravm_core::MASTER_CLOCK_HZ / 50_000; // 20 us
const DEVICE_BYTE_TICKS: u64 = izarravm_core::MASTER_CLOCK_HZ / 1000; // 1 ms

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingInput {
    Command(u8),
    Data(u8),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Keyboard8042 {
    queue: VecDeque<u8>, // host-injected scancodes waiting to be latched
    output: Option<u8>,  // the byte currently readable on 0x60
    output_is_aux: bool, // the latched byte came from the mouse (status bit 5)
    status: u8,
    command_byte: u8,                   // 8042 command byte (bit 0 = IRQ1 enable)
    expecting_command_data: Option<u8>, // a 0x64 command awaiting its 0x60 data
    irq_armed: bool,                    // a freshly latched keyboard byte to pulse IRQ1
    irq12_armed: bool,                  // a freshly latched mouse byte to pulse IRQ12
    output_port: u8,                    // 8042 output port (bit1 = A20 gate, bit0 = reset)
    kbd_expecting_data: Option<u8>,     // a keyboard-device command awaiting its parameter
    scan_set: u8,                       // active scancode set (0xF0 select; default 2)
    last_byte: u8,                      // last scancode latched, for 0xFE resend
    mouse: Ps2Mouse,
    pending_input: Option<PendingInput>,
    input_ticks: u64,
    device_byte_ticks: Option<u64>,
    device_byte_ready: bool,
}

impl Default for Keyboard8042 {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            output: None,
            output_is_aux: false,
            status: STATUS_SYS,
            command_byte: 0x01, // IRQ1 enabled; host input is already translated Set 1
            expecting_command_data: None,
            irq_armed: false,
            irq12_armed: false,
            output_port: 0x03, // A20 enabled (bit1), reset line high (bit0)
            kbd_expecting_data: None,
            scan_set: 2, // PS/2 keyboards power up in set 2
            last_byte: 0,
            mouse: Ps2Mouse::default(),
            pending_input: None,
            input_ticks: 0,
            device_byte_ticks: None,
            device_byte_ready: false,
        }
    }
}

impl Keyboard8042 {
    /// Queue host scancodes (Set 1, make on press / break = 0x80|make on release).
    pub fn push_scancodes(&mut self, codes: &[u8]) {
        self.queue.extend(codes.iter().copied());
        self.schedule_device_byte();
    }

    /// Feed host mouse movement to the aux device: a relative delta plus the
    /// button mask. Queues a PS/2 packet for the serial-byte deadline when data
    /// reporting is enabled. Returns true only if IRQ12 was already armed.
    pub fn inject_mouse(&mut self, dx: i32, dy: i32, buttons: u8) -> bool {
        let reporting = self.mouse.queue_movement(dx, dy, buttons, 0);
        self.schedule_device_byte();
        reporting && self.irq12_armed
    }

    /// Inject a wheel detent: a Z-only PS/2 packet (no motion, buttons unchanged).
    /// Returns true only if IRQ12 was already armed.
    pub fn inject_mouse_wheel(&mut self, dz: i32) -> bool {
        let buttons = self.mouse.current_buttons();
        let reporting = self.mouse.queue_movement(0, 0, buttons, dz);
        self.schedule_device_byte();
        reporting && self.irq12_armed
    }

    /// Enable or disable PS/2 aux data reporting directly, the seam the BIOS
    /// INT 15h AX=C200/C205 services use. This flips the same flag the guest's
    /// 0xD4-routed 0xF4/0xF5 commands set, without queuing an ACK into the aux
    /// stream. It does not clear the queue or re-centre; that is the driver's job.
    pub fn set_mouse_reporting(&mut self, on: bool) {
        self.mouse.reporting = on;
    }

    /// Put the aux device into IntelliMouse 4-byte mode (the platform enables wheel
    /// support at mouse-enable).
    pub fn enable_mouse_wheel(&mut self) {
        self.mouse.enable_wheel();
    }

    pub fn set_mouse_sample_rate_code(&mut self, code: u8) -> bool {
        self.mouse.set_sample_rate_code(code)
    }

    pub fn mouse_sample_rate(&self) -> u8 {
        self.mouse.sample_rate()
    }

    /// Test seam: report whether the aux device is in IntelliMouse 4-byte mode.
    #[cfg(test)]
    pub fn mouse_wheel_enabled(&self) -> bool {
        self.mouse.is_intellimouse()
    }

    /// Enable or disable IRQ12 (the mouse interrupt) in the 8042 command byte,
    /// the seam the BIOS INT 15h AX=C200/C205 services use when they enable the
    /// pointing device. A real PS/2 BIOS enabling the mouse sets command-byte
    /// bit1 so latched aux bytes raise IRQ12; without it the aux byte latches but
    /// no interrupt fires. Bit1 is set/cleared in place so the keyboard's IRQ1
    /// enable (bit0) and the device masks (bits 4/5) are preserved. Enabling
    /// re-latches a held byte so a packet already queued can raise IRQ12 at once.
    pub fn set_mouse_irq(&mut self, on: bool) {
        if on {
            self.command_byte |= 0x02;
            self.arm_current_output();
            if self.device_byte_ready {
                self.latch_ready_device_byte();
            }
            self.schedule_device_byte();
        } else {
            self.command_byte &= !0x02;
            // A byte latched while the interrupt was enabled may have left the
            // edge armed; drop it so a disabled mouse raises no IRQ12.
            self.irq12_armed = false;
        }
    }

    /// Handle a byte the guest wrote straight to the keyboard device (the 0x60
    /// non-data path). Mirrors the aux handshake: most commands ACK with 0xFA,
    /// a few queue extra report bytes, and a parameter-taking command records
    /// the next byte. Replies go through the scancode queue so OBF/IRQ1 framing
    /// matches a real keystroke.
    fn write_keyboard_byte(&mut self, value: u8) {
        if let Some(cmd) = self.kbd_expecting_data.take() {
            match cmd {
                0xF0 => {
                    // Set/get scancode set. Param 0 reports the current set,
                    // 1/2/3 store it; either way the keyboard ACKs first.
                    self.queue.push_back(0xFA);
                    if value == 0x00 {
                        self.queue.push_back(self.scan_set);
                    } else if (1..=3).contains(&value) {
                        self.scan_set = value;
                    }
                }
                // 0xF3 set typematic rate/delay: swallow the rate byte, ACK it.
                0xF3 => self.queue.push_back(0xFA),
                _ => self.queue.push_back(0xFA),
            }
            self.schedule_device_byte();
            return;
        }
        match value {
            0xFF => self.push_scancodes(&[0xFA, 0xAA]), // reset: ACK then self-test pass
            0xEE => self.push_scancodes(&[0xEE]),       // echo answers 0xEE, not an ACK
            0xF2 => self.push_scancodes(&[0xFA, 0xAB, 0x41]), // read-ID: ACK then MF2 id
            0xF0 | 0xF3 => {
                // Scancode-set select / set-typematic: ACK, then take one param.
                self.kbd_expecting_data = Some(value);
                self.push_scancodes(&[0xFA]);
            }
            0xFE => {
                // Resend: re-queue the last latched scancode (no ACK).
                let last = self.last_byte;
                self.push_scancodes(&[last]);
            }
            // 0xF4 enable, 0xF5 disable, 0xF6 set-defaults: plain ACK.
            _ => self.push_scancodes(&[0xFA]),
        }
    }

    /// Put a processed controller response ahead of keyboard scancodes. Any
    /// unread byte is pushed back instead of being discarded.
    fn respond_controller(&mut self, response: u8) {
        // Only a fresh (OBF-set) byte gets pushed back; a stale byte left in the
        // output register after a read (OBF clear) was already consumed and must
        // not be re-queued.
        if self.status & STATUS_OBF != 0 {
            if let Some(latched) = self.output.take() {
                if self.output_is_aux {
                    self.mouse.queue.push_front(latched);
                } else {
                    self.queue.push_front(latched);
                }
                self.device_byte_ready = true;
            }
        }
        self.output = Some(response);
        self.output_is_aux = false;
        self.status |= STATUS_OBF;
        self.status &= !STATUS_AUX;
        if self.command_byte & 0x01 != 0 {
            self.irq_armed = true;
        }
    }

    fn queued_device_byte(&self) -> bool {
        let keyboard = self.command_byte & 0x10 == 0 && !self.queue.is_empty();
        let auxiliary = self.command_byte & 0x20 == 0 && !self.mouse.queue.is_empty();
        keyboard || auxiliary
    }

    fn schedule_device_byte(&mut self) {
        if self.status & STATUS_OBF == 0
            && !self.device_byte_ready
            && self.device_byte_ticks.is_none()
            && self.queued_device_byte()
        {
            self.device_byte_ticks = Some(DEVICE_BYTE_TICKS);
        }
    }

    /// Latch one byte whose serial transfer has completed. Keyboard data keeps
    /// priority over auxiliary data, matching the existing controller behavior.
    fn latch_ready_device_byte(&mut self) {
        // A fresh byte (OBF set) is still waiting to be read; do not overwrite it.
        // A stale byte left after a read (OBF clear) may be overwritten by the
        // next queued byte.
        if self.status & STATUS_OBF != 0 {
            return;
        }
        // Command-byte bit4 masks the keyboard, bit5 the aux device. A masked
        // stream stays queued (not dropped) so its bytes latch on re-enable.
        let kbd_disabled = self.command_byte & 0x10 != 0;
        let aux_disabled = self.command_byte & 0x20 != 0;
        if !kbd_disabled && !self.queue.is_empty() {
            let code = self.queue.pop_front().unwrap();
            self.output = Some(code);
            self.output_is_aux = false;
            self.last_byte = code; // remember for a 0xFE resend
            self.status |= STATUS_OBF;
            self.status &= !STATUS_AUX;
            if self.command_byte & 0x01 != 0 {
                self.irq_armed = true;
            }
            self.device_byte_ready = false;
        } else if !aux_disabled {
            if let Some(code) = self.mouse.queue.pop_front() {
                self.output = Some(code);
                self.output_is_aux = true;
                self.status |= STATUS_OBF | STATUS_AUX;
                // Command byte bit 1 enables the mouse interrupt (IRQ12).
                if self.command_byte & 0x02 != 0 {
                    self.irq12_armed = true;
                }
                self.device_byte_ready = false;
            }
        }
    }

    fn arm_current_output(&mut self) {
        if self.status & STATUS_OBF == 0 {
            return;
        }
        if self.output_is_aux {
            if self.command_byte & 0x02 != 0 {
                self.irq12_armed = true;
            }
        } else if self.command_byte & 0x01 != 0 {
            self.irq_armed = true;
        }
    }

    fn process_input(&mut self, input: PendingInput) {
        self.status &= !STATUS_IBF;
        match input {
            PendingInput::Command(command) => self.process_command(command),
            PendingInput::Data(data) => self.process_data(data),
        }
        if self.device_byte_ready {
            self.latch_ready_device_byte();
        }
        self.schedule_device_byte();
    }

    fn process_data(&mut self, value: u8) {
        if let Some(command) = self.expecting_command_data.take() {
            match command {
                0x60 => {
                    self.command_byte = value;
                    self.arm_current_output();
                }
                0xD4 => self.mouse.write_byte(value),
                0xD1 => self.output_port = value,
                _ => {}
            }
        } else {
            self.write_keyboard_byte(value);
        }
    }

    fn process_command(&mut self, value: u8) {
        match value {
            0xAA => self.respond_controller(0x55),
            0xAB | 0xA9 => self.respond_controller(0x00),
            0x20 => self.respond_controller(self.command_byte),
            0x60 | 0xD4 | 0xD1 => self.expecting_command_data = Some(value),
            0xA7 => self.command_byte |= 0x20,
            0xA8 => self.command_byte &= !0x20,
            0xAD => self.command_byte |= 0x10,
            0xAE => self.command_byte &= !0x10,
            0xD0 => self.respond_controller(self.output_port),
            0xC0 => self.respond_controller(0xA0),
            0xE0 => self.respond_controller(0x03),
            _ => {}
        }
    }

    pub(crate) fn ticks_until_event(&self) -> Option<u64> {
        self.pending_input
            .map(|_| self.input_ticks)
            .into_iter()
            .chain(self.device_byte_ticks)
            .min()
    }

    pub(crate) fn ticks_until_irq(&self) -> Option<u64> {
        self.ticks_until_event()
    }

    pub(crate) fn irq1_enabled(&self) -> bool {
        self.command_byte & 0x01 != 0
    }

    pub(crate) fn irq12_enabled(&self) -> bool {
        self.command_byte & 0x02 != 0
    }

    pub(crate) fn irq1_level(&self) -> bool {
        self.status & STATUS_OBF != 0 && !self.output_is_aux && self.irq1_enabled()
    }

    pub(crate) fn irq12_level(&self) -> bool {
        self.status & STATUS_OBF != 0 && self.output_is_aux && self.irq12_enabled()
    }

    pub(crate) fn advance_master_ticks(&mut self, ticks: u64) {
        let mut remaining = ticks;
        while remaining > 0 {
            let Some(next) = self.ticks_until_event() else {
                break;
            };
            if remaining < next {
                if self.pending_input.is_some() {
                    self.input_ticks -= remaining;
                }
                if let Some(serial) = self.device_byte_ticks.as_mut() {
                    *serial -= remaining;
                }
                break;
            }

            if self.pending_input.is_some() {
                self.input_ticks -= next;
            }
            if let Some(serial) = self.device_byte_ticks.as_mut() {
                *serial -= next;
            }
            remaining -= next;

            if self.input_ticks == 0
                && let Some(input) = self.pending_input.take()
            {
                self.process_input(input);
            }
            if self.device_byte_ticks == Some(0) {
                self.device_byte_ticks = None;
                self.device_byte_ready = true;
                self.latch_ready_device_byte();
            }
            self.schedule_device_byte();
        }
    }

    /// Take the pending "announce a keyboard byte" edge; the caller pulses IRQ1.
    #[cfg(test)]
    pub fn take_irq(&mut self) -> bool {
        let armed = self.irq_armed;
        self.irq_armed = false;
        armed
    }

    /// Take the pending "announce a mouse byte" edge; the caller pulses IRQ12.
    #[cfg(test)]
    pub fn take_irq12(&mut self) -> bool {
        let armed = self.irq12_armed;
        self.irq12_armed = false;
        armed
    }

    /// State of the A20 gate driven by the controller output port (bit 1). Port
    /// 0x92 (fast A20) and INT 15h AH=24h read this so every A20 method agrees.
    pub fn a20_enabled(&self) -> bool {
        self.output_port & 0x02 != 0
    }

    /// Drive the A20 gate from outside the keyboard path (the fast-A20 port 0x92
    /// and the INT 15h AH=24h BIOS service), keeping output-port bit 1 the single
    /// source of truth. The other output-port bits (reset line, etc.) are left
    /// alone. The flat address space is not actually masked; this tracks state so
    /// the reported A20 status stays coherent across all three methods.
    pub fn set_a20(&mut self, enabled: bool) {
        if enabled {
            self.output_port |= 0x02;
        } else {
            self.output_port &= !0x02;
        }
    }

    pub fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            0x60 => {
                // Real 8042: a read clears OBF but leaves the byte in the output
                // register, so a re-read before a new byte arrives returns the same
                // (stale) value. A guest INT 09h that reads 0x60 and then chains to
                // the BIOS handler (which reads 0x60 again) depends on this; Prince
                // of Persia does exactly that and reads its shift state from the
                // BDA flag the BIOS sets from that second read.
                let value = self.output.unwrap_or(0x00);
                self.status &= !(STATUS_OBF | STATUS_AUX);
                self.output_is_aux = false;
                if self.device_byte_ready {
                    self.latch_ready_device_byte();
                }
                self.schedule_device_byte();
                Some(value)
            }
            0x64 => Some(self.status),
            _ => None,
        }
    }

    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        let input = match port {
            0x60 => PendingInput::Data(value),
            0x64 => PendingInput::Command(value),
            _ => return false,
        };
        if self.pending_input.is_none() {
            self.pending_input = Some(input);
            self.input_ticks = CONTROLLER_INPUT_TICKS;
            self.status |= STATUS_IBF;
            if port == 0x64 {
                self.status |= STATUS_CMD;
            } else {
                self.status &= !STATUS_CMD;
            }
        }
        true
    }
}

#[cfg(test)]
#[path = "keyboard_test.rs"]
mod tests;
