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
const AUX_BYTE_SETTLE_US: f64 = 1000.0;

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
    aux_settle_us: f64, // microseconds left before the next aux byte may latch
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
            aux_settle_us: 0.0,
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
        } else if !aux_disabled && self.aux_settle_us <= 0.0 {
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

    /// Decay the aux settle window (see `AUX_BYTE_SETTLE_US`) by `micros` of
    /// emulated time, releasing a held-back aux byte once it elapses. Called
    /// once per device-clocking tick from `Machine::advance_devices`, which
    /// has the real elapsed time.
    pub(crate) fn advance_mouse_pacing(&mut self, micros: f64) {
        if self.aux_settle_us > 0.0 {
            self.aux_settle_us = (self.aux_settle_us - micros).max(0.0);
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
                    self.aux_settle_us = AUX_BYTE_SETTLE_US;
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
mod tests {
    use super::*;

    #[test]
    fn injected_scancode_reads_back_with_obf_and_irq() {
        let mut kbd = Keyboard8042::default();
        kbd.push_scancodes(&[0x1e]); // 'A' make
        assert_eq!(kbd.read_port(0x64).unwrap() & STATUS_OBF, STATUS_OBF);
        assert!(kbd.take_irq(), "a latched key arms IRQ1");
        assert_eq!(kbd.read_port(0x60), Some(0x1e));
        assert_eq!(
            kbd.read_port(0x64).unwrap() & STATUS_OBF,
            0,
            "OBF clears after read"
        );
    }

    #[test]
    fn reread_returns_stale_byte_until_next_arrives() {
        // A real 8042 keeps the last byte in the output register after a read; a
        // re-read (the BIOS handler reading 0x60 after a game's INT 09h already
        // did) returns the same value rather than 0. Prince of Persia's shift
        // state depends on this.
        let mut kbd = Keyboard8042::default();
        kbd.push_scancodes(&[0x2a]); // shift make
        assert_eq!(kbd.read_port(0x60), Some(0x2a));
        assert_eq!(
            kbd.read_port(0x64).unwrap() & STATUS_OBF,
            0,
            "OBF clears after the read"
        );
        assert_eq!(
            kbd.read_port(0x60),
            Some(0x2a),
            "re-read returns the stale byte, not 0"
        );
        kbd.push_scancodes(&[0xaa]); // shift break replaces it
        assert_eq!(kbd.read_port(0x60), Some(0xaa));
        assert_eq!(kbd.read_port(0x60), Some(0xaa), "now stale on 0xaa");
    }

    #[test]
    fn second_scancode_re_arms_irq_after_read() {
        let mut kbd = Keyboard8042::default();
        kbd.push_scancodes(&[0x1e, 0x9e]); // make + break
        assert!(kbd.take_irq());
        assert_eq!(kbd.read_port(0x60), Some(0x1e));
        assert!(
            kbd.take_irq(),
            "reading latches the next byte and re-arms IRQ1"
        );
        assert_eq!(kbd.read_port(0x60), Some(0x9e));
    }

    #[test]
    fn controller_self_test_returns_0x55() {
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x64, 0xaa);
        assert_eq!(kbd.read_port(0x60), Some(0x55));
    }

    #[test]
    fn irq_disabled_in_command_byte_does_not_arm() {
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x64, 0x60); // write command byte
        kbd.write_port(0x60, 0x00); // IRQ1 disabled
        kbd.push_scancodes(&[0x1e]);
        assert!(!kbd.take_irq());
    }

    /// Enable the mouse the way a driver does: command byte bit 1 set so IRQ12
    /// fires, then a 0xD4-routed 0xF4 (enable data reporting) acked.
    fn enable_mouse(kbd: &mut Keyboard8042) {
        kbd.write_port(0x64, 0x60); // write command byte
        kbd.write_port(0x60, 0x03); // IRQ1 + IRQ12 (mouse) enabled
        kbd.write_port(0x64, 0xD4); // next 0x60 byte goes to the mouse
        kbd.write_port(0x60, 0xF4); // enable data reporting
        assert_eq!(kbd.read_port(0x60), Some(0xFA), "mouse acks enable");
        // The ACK read armed the aux settle window (see AUX_BYTE_SETTLE_US);
        // clear it so callers can inject and immediately read a movement
        // packet, matching a real driver that doesn't move the mouse in the
        // same instant the enable handshake completes.
        kbd.advance_mouse_pacing(AUX_BYTE_SETTLE_US);
    }

    #[test]
    fn aux_write_path_routes_to_mouse_and_acks() {
        let mut kbd = Keyboard8042::default();
        enable_mouse(&mut kbd);
        assert!(kbd.mouse.reporting, "0xF4 enabled data reporting");
    }

    #[test]
    fn keyboard_reread_is_not_hijacked_by_a_pending_mouse_byte() {
        // Regression for the Prince of Persia screen-corruption/freeze bug.
        // PoP's own INT 09h handler reads 0x60, then chains to the BIOS's
        // INT 09h handler, which reads 0x60 again expecting the same stale
        // scancode back (see reread_returns_stale_byte_until_next_arrives).
        // If a mouse packet happens to be queued behind it at that exact
        // moment (e.g. mid-flick), the second read must not see a freshly
        // latched mouse byte instead -- that corrupts the BIOS's
        // shift-state handling and desyncs the mouse driver's own packet
        // framing.
        let mut kbd = Keyboard8042::default();
        enable_mouse(&mut kbd);
        kbd.take_irq12(); // drain the IRQ12 edge the ACK byte itself armed
        kbd.push_scancodes(&[0x1e]); // 'A' make: latches immediately
        // A mouse packet queues up behind the held keyboard byte. It cannot
        // latch yet -- the output register is occupied -- so no IRQ12 arms.
        assert!(
            !kbd.inject_mouse(5, -3, 0x01),
            "the mouse byte is queued but not yet latched"
        );

        // PoP's own ISR consumes the keyboard byte...
        assert_eq!(kbd.read_port(0x60), Some(0x1e));
        assert_eq!(
            kbd.read_port(0x64).unwrap() & STATUS_AUX,
            0,
            "the byte just consumed was the keyboard's"
        );
        // ...then the chained BIOS handler re-reads 0x60, expecting the
        // same stale scancode -- not the mouse byte waiting right behind it.
        assert_eq!(
            kbd.read_port(0x60),
            Some(0x1e),
            "a pending mouse byte must not hijack the chained re-read"
        );
        assert_eq!(
            kbd.read_port(0x64).unwrap() & STATUS_AUX,
            0,
            "the re-read is still flagged as a keyboard byte, not AUX"
        );
        assert!(
            !kbd.take_irq12(),
            "no mouse interrupt has fired yet -- its byte is still held back"
        );

        // Once the settle window elapses, the untouched mouse byte latches
        // normally and the mouse driver's packet framing is never disturbed.
        kbd.advance_mouse_pacing(2000.0); // comfortably past the settle window
        let status = kbd.read_port(0x64).unwrap();
        assert_eq!(
            status & STATUS_OBF,
            STATUS_OBF,
            "the mouse byte now latches"
        );
        assert_eq!(
            status & STATUS_AUX,
            STATUS_AUX,
            "and is correctly flagged AUX"
        );
        assert!(
            kbd.take_irq12(),
            "IRQ12 arms once the byte actually latches"
        );
    }

    #[test]
    fn movement_queues_three_byte_packet_and_arms_irq12() {
        let mut kbd = Keyboard8042::default();
        enable_mouse(&mut kbd);
        // Move right 5, up 3 (screen dy = -3), left button held.
        let irq = kbd.inject_mouse(5, -3, 0x01);
        assert!(irq, "movement with reporting on raises IRQ12");
        // First byte: AUX bit set in status.
        let status = kbd.read_port(0x64).unwrap();
        assert_eq!(status & STATUS_OBF, STATUS_OBF);
        assert_eq!(status & STATUS_AUX, STATUS_AUX, "byte is from the mouse");
        // Flags byte: bit3 set, left button (bit0); +x and (screen up -> +y)
        // both positive, so neither sign bit is set.
        let b0 = kbd.read_port(0x60).unwrap();
        assert_eq!(b0 & 0x08, 0x08, "always-one bit");
        assert_eq!(b0 & 0x01, 0x01, "left button");
        assert_eq!(b0 & 0x10, 0x00, "X positive, no sign");
        assert_eq!(b0 & 0x20, 0x00, "Y positive (up), no sign");
        // Each byte is paced ~1ms apart (AUX_BYTE_SETTLE_US), matching real
        // PS/2 serial transmission; advance past it between reads.
        kbd.advance_mouse_pacing(AUX_BYTE_SETTLE_US);
        let bx = kbd.read_port(0x60).unwrap();
        assert_eq!(bx, 5, "dx byte");
        kbd.advance_mouse_pacing(AUX_BYTE_SETTLE_US);
        let by = kbd.read_port(0x60).unwrap();
        assert_eq!(by, 3, "dy byte (negated screen delta)");
    }

    #[test]
    fn movement_without_reporting_is_dropped() {
        let mut kbd = Keyboard8042::default();
        // No enable: reporting is off by default.
        let irq = kbd.inject_mouse(10, 10, 0);
        assert!(!irq, "no IRQ12 while reporting is disabled");
        assert_eq!(
            kbd.read_port(0x64).unwrap() & STATUS_OBF,
            0,
            "nothing latched"
        );
    }

    #[test]
    fn set_mouse_reporting_enables_packets_without_a_command() {
        let mut kbd = Keyboard8042::default();
        // Command byte bit1 must be set for IRQ12 to arm in latch_next.
        kbd.write_port(0x64, 0x60); // write command byte
        kbd.write_port(0x60, 0x03); // IRQ1 + IRQ12 enabled
        // Enable reporting via the new seam, not the 0xD4/0xF4 command path.
        kbd.set_mouse_reporting(true);
        assert!(kbd.mouse.reporting, "seam flips the reporting flag");
        // A queued packet now latches and arms IRQ12, with no spurious 0xFA ACK.
        let pulse = kbd.inject_mouse(5, -3, 0x01);
        assert!(
            pulse,
            "reporting on plus an armed mouse byte requests IRQ12"
        );
        assert!(kbd.take_irq12(), "IRQ12 edge is pending");
        let b0 = kbd.read_port(0x60).unwrap();
        assert_eq!(b0 & 0x08, 0x08, "sync bit set on packet byte 0");
        assert_eq!(b0 & 0x01, 0x01, "left button reported");
    }

    #[test]
    fn disable_clears_a_pending_irq12_edge() {
        // Enable IRQ12 and reporting, then queue a packet so a byte latches and
        // arms the IRQ12 edge. Disable the mouse interrupt before the run loop
        // consumes that edge (take_irq12). The disable must drop the pending edge,
        // so a disabled mouse raises no interrupt.
        let mut kbd = Keyboard8042::default();
        kbd.set_mouse_irq(true); // command byte bit1 = IRQ12 enabled
        kbd.set_mouse_reporting(true);
        let pulse = kbd.inject_mouse(5, -3, 0x01);
        assert!(pulse, "an armed mouse byte requests IRQ12 while enabled");
        kbd.set_mouse_irq(false); // disable before take_irq12 consumes the edge
        assert!(
            !kbd.take_irq12(),
            "disabling the mouse interrupt drops the pending IRQ12 edge"
        );
    }

    #[test]
    fn negative_delta_sets_sign_bits() {
        let mut kbd = Keyboard8042::default();
        enable_mouse(&mut kbd);
        // Move left 4 (dx -4), down 7 (screen dy +7 -> packet y -7).
        kbd.inject_mouse(-4, 7, 0);
        let b0 = kbd.read_port(0x60).unwrap();
        assert_eq!(b0 & 0x10, 0x10, "X sign set for leftward move");
        assert_eq!(b0 & 0x20, 0x20, "Y sign set for downward move");
        kbd.advance_mouse_pacing(AUX_BYTE_SETTLE_US);
        let bx = kbd.read_port(0x60).unwrap();
        assert_eq!(bx as i8 as i32, -4, "dx is -4 two's complement");
        kbd.advance_mouse_pacing(AUX_BYTE_SETTLE_US);
        let by = kbd.read_port(0x60).unwrap();
        assert_eq!(by as i8 as i32, -7, "dy is -7 (down)");
    }

    // Slice A: output port and A20 gate state.

    #[test]
    fn a20_enabled_by_default() {
        let kbd = Keyboard8042::default();
        assert!(kbd.a20_enabled(), "default output port 0x03 has A20 on");
    }

    #[test]
    fn write_output_port_toggles_a20() {
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x64, 0xD1); // arm: next 0x60 byte drives the output port
        kbd.write_port(0x60, 0x01); // A20 bit clear, reset line high
        assert!(!kbd.a20_enabled(), "A20 off after clearing bit 1");
        kbd.write_port(0x64, 0xD1);
        kbd.write_port(0x60, 0x03); // A20 bit set again
        assert!(kbd.a20_enabled(), "A20 back on after setting bit 1");
    }

    // Slice B: read-port commands on 0x64.

    #[test]
    fn read_output_port_returns_live_state() {
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x64, 0xD1);
        kbd.write_port(0x60, 0x02); // A20 on, reset low
        kbd.write_port(0x64, 0xD0); // read output port
        assert_eq!(
            kbd.read_port(0x60),
            Some(0x02),
            "0xD0 reads what 0xD1 wrote"
        );
    }

    #[test]
    fn read_input_port_reports_unlocked_normal() {
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x64, 0xC0);
        let byte = kbd.read_port(0x60).unwrap();
        assert_eq!(byte & 0x80, 0x80, "bit7 set: keyboard not locked");
        assert_eq!(byte & 0x20, 0x20, "bit5 set: normal");
    }

    #[test]
    fn read_test_inputs_idle_high() {
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x64, 0xE0);
        assert_eq!(kbd.read_port(0x60), Some(0x03), "kbd clock+data idle high");
    }

    // Slice C: interface-test labels.

    #[test]
    fn keyboard_interface_test_returns_zero() {
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x64, 0xAB); // keyboard interface test
        assert_eq!(kbd.read_port(0x60), Some(0x00), "0xAB reports no error");
    }

    #[test]
    fn aux_interface_test_returns_zero() {
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x64, 0xA9); // aux/mouse interface test
        assert_eq!(kbd.read_port(0x60), Some(0x00), "0xA9 reports no error");
    }

    #[test]
    fn read_command_byte_is_not_blocked_by_keyboard_disable() {
        // The BIOS idiom disables the keyboard (0xAD, command-byte bit4) before reading
        // the command byte. The 0x20 controller response must still reach the output
        // buffer, since the disable bit only holds back queued scancodes.
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x64, 0x60); // write command byte
        kbd.write_port(0x60, 0x10); // bit4 set: keyboard clock disabled
        kbd.write_port(0x64, 0x20); // read command byte
        assert_eq!(kbd.read_port(0x60), Some(0x10));
    }

    // Slice D: keyboard-device command set on the 0x60 non-data path.

    #[test]
    fn echo_answers_ee_not_ack() {
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x60, 0xEE); // echo
        assert_eq!(
            kbd.read_port(0x60),
            Some(0xEE),
            "echo replies 0xEE, not 0xFA"
        );
    }

    #[test]
    fn read_id_returns_ack_then_mf2_id() {
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x60, 0xF2); // read ID
        assert_eq!(kbd.read_port(0x60), Some(0xFA), "ACK first");
        assert_eq!(kbd.read_port(0x60), Some(0xAB), "ID low byte");
        assert_eq!(kbd.read_port(0x60), Some(0x41), "ID high byte");
    }

    #[test]
    fn scan_set_store_then_get_roundtrips() {
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x60, 0xF0); // select scancode set
        assert_eq!(kbd.read_port(0x60), Some(0xFA), "ACK the command");
        kbd.write_port(0x60, 0x01); // store set 1
        assert_eq!(kbd.read_port(0x60), Some(0xFA), "ACK the parameter");
        kbd.write_port(0x60, 0xF0); // ask again
        assert_eq!(kbd.read_port(0x60), Some(0xFA), "ACK the query");
        kbd.write_port(0x60, 0x00); // get current set
        assert_eq!(kbd.read_port(0x60), Some(0xFA), "ACK the get");
        assert_eq!(kbd.read_port(0x60), Some(0x01), "reports the stored set");
    }

    #[test]
    fn set_typematic_consumes_rate_without_spurious_ack() {
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x60, 0xF3); // set typematic rate/delay
        assert_eq!(kbd.read_port(0x60), Some(0xFA), "ACK the command");
        kbd.write_port(0x60, 0x2A); // the rate byte (consumed as a parameter)
        assert_eq!(kbd.read_port(0x60), Some(0xFA), "ACK the rate byte");
        // The rate byte must not be mistaken for a fresh command: no extra ACK.
        assert_eq!(
            kbd.read_port(0x64).unwrap() & STATUS_OBF,
            0,
            "no spurious second response"
        );
    }

    #[test]
    fn resend_repushes_last_scancode() {
        let mut kbd = Keyboard8042::default();
        kbd.push_scancodes(&[0x1E]); // 'A' make
        assert_eq!(kbd.read_port(0x60), Some(0x1E));
        kbd.write_port(0x60, 0xFE); // resend
        assert_eq!(kbd.read_port(0x60), Some(0x1E), "resend repeats last byte");
    }

    #[test]
    fn reset_acks_then_self_tests() {
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x60, 0xFF); // keyboard reset
        assert_eq!(kbd.read_port(0x60), Some(0xFA), "ACK");
        assert_eq!(kbd.read_port(0x60), Some(0xAA), "BAT self-test pass");
    }

    #[test]
    fn enable_disable_keyboard_holds_then_releases_scancodes() {
        let mut kbd = Keyboard8042::default();
        kbd.write_port(0x64, 0xAD); // disable keyboard (cmd-byte bit4)
        kbd.push_scancodes(&[0x1E]);
        assert_eq!(
            kbd.read_port(0x64).unwrap() & STATUS_OBF,
            0,
            "masked scancode stays queued, not latched"
        );
        kbd.write_port(0x64, 0xAE); // re-enable keyboard
        assert_eq!(
            kbd.read_port(0x60),
            Some(0x1E),
            "held scancode latches on re-enable"
        );
    }

    #[test]
    fn disable_aux_holds_then_releases_mouse_bytes() {
        let mut kbd = Keyboard8042::default();
        enable_mouse(&mut kbd);
        kbd.write_port(0x64, 0xA7); // disable aux (cmd-byte bit5)
        kbd.inject_mouse(3, 0, 0);
        assert_eq!(
            kbd.read_port(0x64).unwrap() & STATUS_OBF,
            0,
            "masked mouse byte stays queued"
        );
        kbd.write_port(0x64, 0xA8); // re-enable aux
        let status = kbd.read_port(0x64).unwrap();
        assert_eq!(status & STATUS_OBF, STATUS_OBF, "byte latches on re-enable");
        assert_eq!(status & STATUS_AUX, STATUS_AUX, "it is an aux byte");
    }
}
