// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! PS/2 auxiliary (mouse) device, as the 8042 controller multiplexes it.
//! Models a Microsoft IntelliMouse: it powers up as a standard three-byte mouse
//! (id 0x00) and switches to four-byte wheel mode (id 0x03) once the driver
//! plays the 200/100/80 sample-rate "magic knock". A reset or set-defaults drops
//! it back to three bytes. Tracks the reporting enable, the queued data bytes,
//! and the sample-rate/resolution/scaling state the driver sets up during
//! detection.
//!
//! Host motion does not map one-to-one onto packets. The host feeds deltas into
//! accumulators, and the device samples them into a packet at its sample rate,
//! the way a real mouse reads its optics on a clock and reports what it saw.
//! One packet carries at most the 9-bit range; the rest stays in the accumulator
//! for the next sample. 86Box models a PS/2 mouse the same way
//! (`mouse_ps2.c` `ps2_poll` plus `mouse.c` `mouse_subtract_coords`). A queue
//! of one packet per host flush instead grows without bound whenever the guest
//! drains slower than the host flushes (any run below real time), and the
//! cursor then replays seconds of stale motion.

use std::collections::VecDeque;

/// The PS/2 mouse device state. Host motion and button changes accumulate;
/// `sample` turns them into a packet (three bytes, or four with a Z wheel byte
/// in IntelliMouse mode) which, when reporting is on, raises IRQ12 through the
/// controller. The sample-rate history detects the IntelliMouse knock, which
/// flips `intellimouse`/`device_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ps2Mouse {
    pub(crate) queue: VecDeque<u8>, // bytes waiting to be moved into the aux output buffer
    pub(crate) reporting: bool,     // data-reporting enabled (command 0xF4 on / 0xF5 off)
    sample_rate: u8,                // last value set by 0xF3 (sample rate, Hz)
    resolution: u8,                 // last value set by 0xE8 (counts per mm code 0..3)
    scaling_2to1: bool,             // 2:1 scaling (0xE7 on / 0xE6 off)
    buttons: u8,                    // button bitmask last reported (bit0 L, bit1 R, bit2 M)
    expecting_data: Option<u8>,     // a mouse command awaiting its parameter byte
    device_id: u8,                  // 0x00 standard, 0x03 IntelliMouse (set by the knock)
    intellimouse: bool,             // four-byte wheel mode enabled
    rate_history: [u8; 3],          // last three 0xF3 sample rates (for the magic knock)
    pending_dx: i32,                // host motion not yet reported (screen sense, +right)
    pending_dy: i32,                // host motion not yet reported (screen sense, +down)
    pending_dz: i32,                // wheel detents not yet reported
    host_buttons: u8,               // button bitmask the host holds now
}

impl Default for Ps2Mouse {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            reporting: false,
            sample_rate: 100,
            resolution: 2,
            scaling_2to1: false,
            buttons: 0,
            expecting_data: None,
            device_id: 0x00,
            intellimouse: false,
            rate_history: [0; 3],
            pending_dx: 0,
            pending_dy: 0,
            pending_dz: 0,
            host_buttons: 0,
        }
    }
}

impl Ps2Mouse {
    /// Accumulate host motion (`dx`/`dy` host pixels, y down positive; `dz`
    /// wheel detents) and record the host's button mask. Nothing goes on the
    /// wire here; `sample` does that.
    pub(crate) fn host_update(&mut self, dx: i32, dy: i32, dz: i32, buttons: u8) {
        self.pending_dx = self.pending_dx.saturating_add(dx);
        self.pending_dy = self.pending_dy.saturating_add(dy);
        self.pending_dz = self.pending_dz.saturating_add(dz);
        self.host_buttons = buttons & 0x07;
    }

    /// The button mask the host holds, so a wheel-only injection keeps it.
    pub(crate) fn host_buttons(&self) -> u8 {
        self.host_buttons
    }

    /// Whether a sample would have something to report: unreported motion, a
    /// wheel detent, or a button edge.
    pub(crate) fn state_pending(&self) -> bool {
        self.reporting
            && (self.pending_dx != 0
                || self.pending_dy != 0
                || self.pending_dz != 0
                || self.host_buttons != self.buttons)
    }

    /// Test seam: host motion accumulated and not yet on the wire (dx, dy).
    #[cfg(test)]
    pub(crate) fn pending_motion(&self) -> (i32, i32) {
        (self.pending_dx, self.pending_dy)
    }

    /// Master-clock ticks between two samples at the current sample rate.
    pub(crate) fn sample_period_ticks(&self) -> u64 {
        izarravm_core::MASTER_CLOCK_HZ / u64::from(self.sample_rate.max(1))
    }

    /// Flip data reporting without a command byte (the BIOS seam). Turning it
    /// off drops accumulated motion: a real mouse in a disabled stream reports
    /// nothing and the driver re-centres on enable.
    pub(crate) fn set_reporting(&mut self, on: bool) {
        self.reporting = on;
        if !on {
            self.drop_pending();
        }
    }

    fn drop_pending(&mut self) {
        self.pending_dx = 0;
        self.pending_dy = 0;
        self.pending_dz = 0;
        self.buttons = self.host_buttons;
    }

    /// One sample tick: if reporting is on, the wire is free (no packet still
    /// queued) and there is state to report, queue one packet carrying as much
    /// of the accumulated motion as the 9-bit fields hold and leave the rest
    /// for the next tick. Returns true if a packet was queued. A packet still
    /// waiting in the queue is backpressure (86Box refuses to report into a
    /// near-full FIFO); the motion stays accumulated rather than stacking up.
    pub(crate) fn sample(&mut self) -> bool {
        if !self.reporting {
            self.drop_pending();
            return false;
        }
        if !self.state_pending() || !self.queue.is_empty() {
            return false;
        }
        let dx = self.pending_dx.clamp(-256, 255);
        let dy = self.pending_dy.clamp(-255, 256);
        self.pending_dx -= dx;
        self.pending_dy -= dy;
        let dz = if self.intellimouse {
            let dz = self.pending_dz.clamp(-8, 7);
            self.pending_dz -= dz;
            dz
        } else {
            self.pending_dz = 0;
            0
        };
        let buttons = self.host_buttons;
        self.queue_movement(dx, dy, buttons, dz)
    }
    /// Put the device into IntelliMouse (4-byte / wheel) mode. The platform enables
    /// this at mouse-enable; the magic knock also reaches it via write_byte.
    pub(crate) fn enable_wheel(&mut self) {
        self.device_id = 0x03;
        self.intellimouse = true;
    }

    pub(crate) fn set_sample_rate_code(&mut self, code: u8) -> bool {
        let rate = match code {
            0 => 10,
            1 => 20,
            2 => 40,
            3 => 60,
            4 => 80,
            5 => 100,
            6 => 200,
            _ => return false,
        };
        self.set_sample_rate(rate);
        true
    }

    fn set_sample_rate(&mut self, rate: u8) {
        self.sample_rate = rate;
        self.rate_history = [self.rate_history[1], self.rate_history[2], rate];
        if self.rate_history == [200, 100, 80] {
            self.enable_wheel();
        }
    }

    pub(crate) fn sample_rate(&self) -> u8 {
        self.sample_rate
    }

    /// Test seam: whether the device is in IntelliMouse 4-byte (wheel) mode.
    #[cfg(test)]
    pub(crate) fn is_intellimouse(&self) -> bool {
        self.intellimouse
    }

    /// Handle a byte the guest wrote to the mouse (via the controller's 0xD4
    /// path). Most commands queue an ACK (0xFA); a parameter-taking command
    /// (set sample rate / resolution) records the next byte as its parameter.
    pub(crate) fn write_byte(&mut self, value: u8) {
        if let Some(cmd) = self.expecting_data.take() {
            match cmd {
                // 0xF3 set sample rate, 0xE8 set resolution: record the parameter.
                0xF3 => {
                    self.set_sample_rate(value);
                }
                0xE8 => self.resolution = value,
                _ => {}
            }
            self.queue.push_back(0xFA);
            return;
        }
        match value {
            0xFF => {
                // Reset: ACK, then self-test pass (0xAA) and the device id (0x00).
                self.set_reporting(false);
                self.sample_rate = 100;
                self.resolution = 2;
                self.scaling_2to1 = false;
                self.device_id = 0x00;
                self.intellimouse = false;
                self.rate_history = [0; 3];
                self.queue.push_back(0xFA);
                self.queue.push_back(0xAA);
                self.queue.push_back(0x00);
            }
            0xF6 => {
                // Set defaults.
                self.set_reporting(false);
                self.sample_rate = 100;
                self.resolution = 2;
                self.scaling_2to1 = false;
                self.device_id = 0x00;
                self.intellimouse = false;
                self.rate_history = [0; 3];
                self.queue.push_back(0xFA);
            }
            0xF4 => {
                self.reporting = true;
                self.queue.push_back(0xFA);
            }
            0xF5 => {
                self.set_reporting(false);
                self.queue.push_back(0xFA);
            }
            0xF3 | 0xE8 => {
                // Set sample rate / resolution: ACK now, value arrives next byte.
                self.expecting_data = Some(value);
                self.queue.push_back(0xFA);
            }
            0xE7 => {
                self.scaling_2to1 = true;
                self.queue.push_back(0xFA);
            }
            0xE6 => {
                self.scaling_2to1 = false;
                self.queue.push_back(0xFA);
            }
            0xE9 => {
                // Status request: ACK then a three-byte status packet.
                self.queue.push_back(0xFA);
                let mut byte0 = 0u8;
                if self.scaling_2to1 {
                    byte0 |= 0x10;
                }
                if self.reporting {
                    byte0 |= 0x20;
                }
                byte0 |= self.buttons & 0x07;
                self.queue.push_back(byte0);
                self.queue.push_back(self.resolution);
                self.queue.push_back(self.sample_rate);
            }
            0xF2 => {
                // Get device id: ACK then 0x00 (standard) or 0x03 (IntelliMouse).
                self.queue.push_back(0xFA);
                self.queue.push_back(self.device_id);
            }
            _ => self.queue.push_back(0xFA), // ack anything else
        }
    }

    /// Queue a movement packet for `dx`/`dy` (host pixels, y down positive), the
    /// button mask, and `dz` (wheel detents). The packet is three bytes for a
    /// standard mouse, or four (with a signed Z byte) in IntelliMouse mode.
    /// Returns true if reporting is enabled so the controller can raise IRQ12.
    /// Movement while reporting is off is dropped, matching a real mouse that
    /// holds its line idle until enabled. `sample` is the production caller;
    /// the device tests build packets with it directly.
    pub(crate) fn queue_movement(&mut self, dx: i32, dy: i32, buttons: u8, dz: i32) -> bool {
        self.buttons = buttons & 0x07;
        if !self.reporting {
            return false;
        }
        // Clamp to the 9-bit two's-complement range the packet carries.
        let cx = dx.clamp(-256, 255);
        // PS/2 reports +y as up; screen-space dy is +down, so negate.
        let cy = (-dy).clamp(-256, 255);
        let mut byte0 = 0x08 | (buttons & 0x07); // bit3 always set
        if cx < 0 {
            byte0 |= 0x10; // X sign
        }
        if cy < 0 {
            byte0 |= 0x20; // Y sign
        }
        self.queue.push_back(byte0);
        self.queue.push_back((cx & 0xff) as u8);
        self.queue.push_back((cy & 0xff) as u8);
        if self.intellimouse {
            let cz = dz.clamp(-8, 7) as i8; // signed wheel detent
            self.queue.push_back(cz as u8);
        }
        true
    }
}

#[cfg(test)]
#[path = "mouse_test.rs"]
mod tests;
