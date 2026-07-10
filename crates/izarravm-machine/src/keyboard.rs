//! AT keyboard controller (8042-class) as software sees it: status/data ports,
//! a host-fed scancode FIFO, and the command subset the BIOS uses at boot. The
//! controller also multiplexes a PS/2 auxiliary (mouse) device, reachable
//! through the 0xD4 "write to aux" command and reporting on IRQ12.
//! Command set is boot-minimal; extend when the BIOS needs more.

use crate::mouse::Ps2Mouse;
use std::collections::VecDeque;

const STATUS_OBF: u8 = 0x01; // output buffer full (data waiting on 0x60)
const STATUS_SYS: u8 = 0x04; // system flag, set after a passed self-test
const STATUS_AUX: u8 = 0x20; // the byte in the output buffer came from the mouse

// How long, in emulated microseconds, reading ANY real device byte (a
// keyboard scancode or an aux/mouse byte) off 0x60 holds back the next aux
// byte from latching. Real PS/2 hardware serializes each device byte onto
// its own wire at roughly 1ms/byte (~10kHz device clock), so a byte that
// finished arriving microseconds ago genuinely could not be followed by
// another one that fast. Two distinct races this guards:
//   - A guest that reads 0x60 twice in a row (Prince of Persia's INT 09h
//     handler reads 0x60 itself, then chains to the BIOS's INT 09h handler,
//     which reads 0x60 again expecting the same stale scancode -- see
//     `reread_returns_stale_byte_until_next_arrives`) must not have a
//     freshly queued mouse byte race into that second read, corrupting BIOS
//     shift-state handling.
//   - A host mouse "flick" can queue many PS/2 packets at once (no real
//     mouse could ever transmit that fast); without pacing, the mouse
//     driver's IRQ12 handler gets slammed with a burst of back-to-back
//     interrupts far outside anything real hardware produces.
// Excludes controller-command echoes (self-test, CCB read, etc.): those are
// an immediate digital handshake, not a serialized device transmission.
const AUX_BYTE_SETTLE_TICKS: u64 = izarravm_core::MASTER_CLOCK_HZ / 1000;

#[derive(Debug, Clone, PartialEq)]
pub struct Keyboard8042 {
    queue: VecDeque<u8>,         // host-injected scancodes waiting to be latched
    output: Option<u8>,          // the byte currently readable on 0x60
    output_is_aux: bool,         // the latched byte came from the mouse (status bit 5)
    output_is_device_byte: bool, // the latched byte is a real scancode or aux byte
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
    aux_settle_ticks: u64,
}

impl Default for Keyboard8042 {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            output: None,
            output_is_aux: false,
            output_is_device_byte: false,
            status: STATUS_SYS,
            command_byte: 0x01, // IRQ1 enabled, translation on (as a PC BIOS leaves it)
            expecting_command_data: None,
            irq_armed: false,
            irq12_armed: false,
            output_port: 0x03, // A20 enabled (bit1), reset line high (bit0)
            kbd_expecting_data: None,
            scan_set: 2, // PS/2 keyboards power up in set 2
            last_byte: 0,
            mouse: Ps2Mouse::default(),
            aux_settle_ticks: 0,
        }
    }
}

impl Keyboard8042 {
    /// Queue host scancodes (Set 1, make on press / break = 0x80|make on release).
    pub fn push_scancodes(&mut self, codes: &[u8]) {
        self.queue.extend(codes.iter().copied());
        self.latch_next();
    }

    /// Feed host mouse movement to the aux device: a relative delta plus the
    /// button mask. Queues a PS/2 packet and latches the first byte when data
    /// reporting is enabled. Returns true if that should pulse IRQ12.
    pub fn inject_mouse(&mut self, dx: i32, dy: i32, buttons: u8) -> bool {
        let reporting = self.mouse.queue_movement(dx, dy, buttons, 0);
        self.latch_next();
        reporting && self.irq12_armed
    }

    /// Inject a wheel detent: a Z-only PS/2 packet (no motion, buttons unchanged).
    /// Returns true if reporting is on so the caller can raise IRQ12.
    pub fn inject_mouse_wheel(&mut self, dz: i32) -> bool {
        let buttons = self.mouse.current_buttons();
        let reporting = self.mouse.queue_movement(0, 0, buttons, dz);
        self.latch_next();
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
            self.latch_next();
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
            self.latch_next();
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

    /// Put a controller command response (self-test 0x55, interface test 0x00)
    /// into the output buffer ahead of keyboard scancodes. A real 8042 holds the
    /// keyboard while it processes a command and returns the answer immediately, so
    /// any scancode already latched is pushed back to the front of the queue rather
    /// than dropped. This keeps a self-test from eating host keystrokes.
    fn respond_immediately(&mut self, response: u8) {
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
            }
        }
        self.output = Some(response);
        self.output_is_aux = false;
        self.output_is_device_byte = false; // a controller echo, not a real device byte
        self.status |= STATUS_OBF;
        self.status &= !STATUS_AUX;
        if self.command_byte & 0x01 != 0 {
            self.irq_armed = true;
        }
    }

    /// Move the next queued byte into the output buffer if it is free. A waiting
    /// keyboard byte is preferred; a mouse byte is drained only when no scancode
    /// is pending. A mouse byte sets the AUX status bit and arms IRQ12 instead of
    /// IRQ1.
    fn latch_next(&mut self) {
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
            self.output_is_device_byte = true;
            self.last_byte = code; // remember for a 0xFE resend
            self.status |= STATUS_OBF;
            self.status &= !STATUS_AUX;
            if self.command_byte & 0x01 != 0 {
                self.irq_armed = true;
            }
        } else if !aux_disabled && self.aux_settle_ticks == 0 {
            if let Some(code) = self.mouse.queue.pop_front() {
                self.output = Some(code);
                self.output_is_aux = true;
                self.output_is_device_byte = true;
                self.status |= STATUS_OBF | STATUS_AUX;
                // Command byte bit 1 enables the mouse interrupt (IRQ12).
                if self.command_byte & 0x02 != 0 {
                    self.irq12_armed = true;
                }
            }
        }
    }

    /// Decay the aux settle window by fixed master ticks, releasing a held byte
    /// at the same guest-time deadline in every CPU mode.
    pub(crate) fn advance_mouse_pacing(&mut self, master_ticks: u64) {
        if self.aux_settle_ticks > 0 {
            self.aux_settle_ticks = self.aux_settle_ticks.saturating_sub(master_ticks);
            self.latch_next(); // a byte held back by the settle window may now latch
        }
    }

    /// Take the pending "announce a keyboard byte" edge; the caller pulses IRQ1.
    pub fn take_irq(&mut self) -> bool {
        let armed = self.irq_armed;
        self.irq_armed = false;
        armed
    }

    /// Take the pending "announce a mouse byte" edge; the caller pulses IRQ12.
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
                if self.status & STATUS_OBF != 0 && self.output_is_device_byte {
                    // A real device byte (keyboard or aux) was just consumed:
                    // hold off latching the next aux byte for a short settle
                    // window. This guards two races: a chained re-read (see
                    // the comment above) seeing this same stale value rather
                    // than a freshly arrived aux byte, and a flooded aux
                    // queue (a host mouse "flick" can queue many packets at
                    // once) delivering its bytes to the guest faster than any
                    // real PS/2 mouse could transmit them.
                    self.aux_settle_ticks = AUX_BYTE_SETTLE_TICKS;
                }
                self.status &= !(STATUS_OBF | STATUS_AUX);
                self.output_is_aux = false;
                self.output_is_device_byte = false;
                self.latch_next(); // latch the next queued byte now that OBF is clear
                Some(value)
            }
            0x64 => Some(self.status),
            _ => None,
        }
    }

    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        match port {
            0x60 => {
                if let Some(cmd) = self.expecting_command_data.take() {
                    match cmd {
                        0x60 => self.command_byte = value,
                        0xD4 => {
                            // Byte destined for the mouse: hand it to the aux
                            // device, then latch whatever it queued in reply.
                            self.mouse.write_byte(value);
                            self.latch_next();
                        }
                        0xD1 => self.output_port = value, // drive the output port (A20)
                        _ => {} // other command-data writes ignored until needed
                    }
                } else {
                    self.write_keyboard_byte(value);
                }
                true
            }
            0x64 => {
                match value {
                    0xAA => self.respond_immediately(0x55), // controller self-test OK
                    0xAB => self.respond_immediately(0x00), // keyboard interface test OK
                    0xA9 => self.respond_immediately(0x00), // aux (mouse) interface test OK
                    0x20 => {
                        // Read command byte. This is a controller-generated response, so it
                        // goes straight to the output buffer and is not held back by the
                        // keyboard-disable bit the way a queued scancode would be.
                        let cb = self.command_byte;
                        self.respond_immediately(cb);
                    }
                    0x60 => self.expecting_command_data = Some(0x60), // write command byte
                    0xA7 => self.command_byte |= 0x20, // disable aux (mouse): set bit5
                    0xA8 => {
                        // enable aux (mouse): clear bit5, then drain any byte
                        // that queued up while it was masked.
                        self.command_byte &= !0x20;
                        self.latch_next();
                    }
                    0xD4 => self.expecting_command_data = Some(0xD4), // write next byte to aux
                    0xAD => self.command_byte |= 0x10,                // disable keyboard: set bit4
                    0xAE => {
                        // enable keyboard: clear bit4, then drain a held scancode.
                        self.command_byte &= !0x10;
                        self.latch_next();
                    }
                    0xD0 => self.respond_immediately(self.output_port), // read output port (A20 state)
                    0xC0 => self.respond_immediately(0xA0), // read input port: kbd unlocked (bit7), normal (bit5)
                    0xE0 => self.respond_immediately(0x03), // read test inputs: kbd clock+data idle high
                    0xD1 => self.expecting_command_data = Some(0xD1), // write output port (A20)
                    _ => {}                                 // Rest accepted and ignored
                }
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
#[path = "keyboard_test.rs"]
mod tests;
