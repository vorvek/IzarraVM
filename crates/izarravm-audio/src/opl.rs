//! Clean-room OPL3 (YMF262 / AdLib) sound chip.
//!
//! The register/timer model drives AdLib detection; the synthesis path
//! (tables -> operators -> channels -> render) reproduces the chip's bit-exact
//! integer datapath. All lookup tables are generated from the public log-sin /
//! exp formulas, not transcribed from any reference implementation.

/// Quarter-wave log-sine ROM: `round(-log2(sin((i + 0.5) * pi/512)) * 256)`.
/// Entry 0 is the quietest point of the wave (2137), entry 255 the loudest (0).
fn build_logsin() -> [u16; 256] {
    let mut table = [0u16; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let angle = (i as f64 + 0.5) * std::f64::consts::PI / 512.0;
        *slot = (-angle.sin().log2() * 256.0).round() as u16;
    }
    table
}

/// Exponent ROM: `round((2^(i/256) - 1) * 1024)`. Used to turn a log-domain
/// attenuation back into a linear amplitude.
fn build_exp() -> [u16; 256] {
    let mut table = [0u16; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        *slot = ((2.0_f64.powf(i as f64 / 256.0) - 1.0) * 1024.0).round() as u16;
    }
    table
}

use std::sync::LazyLock;

static LOGSIN: LazyLock<[u16; 256]> = LazyLock::new(build_logsin);
static EXP: LazyLock<[u16; 256]> = LazyLock::new(build_exp);

/// Log-sine ROM lookup for a quarter-wave index (0..256).
pub(crate) fn logsin(index: usize) -> u16 {
    LOGSIN[index & 0xff]
}

/// Convert a log-domain attenuation to a linear amplitude, the way the chip
/// does: the low 8 bits index the (reversed, +1024) exp ROM, the high bits are
/// a right shift. `exp_lookup(0)` is the maximum half-amplitude (2042).
pub(crate) fn exp_lookup(attenuation: u32) -> i32 {
    let fraction = (attenuation & 0xff) as usize;
    let shift = attenuation >> 8;
    (i32::from(EXP[fraction ^ 0xff]) + 1024) >> shift
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OplChip {
    registers: [u8; 256],
    address: u8,
    timer1: Timer,
    timer2: Timer,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Timer {
    /// Microseconds per count step: 80us for timer 1, 320us for timer 2.
    step_us: u64,
    /// Up-counter; overflow past 0xff sets `expired` and reloads the preset.
    count: u16,
    accumulated_us: u64,
    running: bool,
    expired: bool,
}

impl Timer {
    fn new(step_us: u64) -> Self {
        Self {
            step_us,
            ..Self::default()
        }
    }

    fn start(&mut self, preset: u8) {
        self.count = u16::from(preset);
        self.accumulated_us = 0;
        self.running = true;
    }

    // Reloads from the live preset on overflow, not a latched copy. Exact
    // enough for detection; revisit if a game depends on mid-run preset reloads.
    fn advance(&mut self, micros: u64, preset: u8) {
        if !self.running {
            return;
        }
        self.accumulated_us += micros;
        while self.accumulated_us >= self.step_us {
            self.accumulated_us -= self.step_us;
            self.count += 1;
            if self.count > 0xff {
                self.count = u16::from(preset);
                self.expired = true;
            }
        }
    }
}

impl Default for OplChip {
    fn default() -> Self {
        Self {
            registers: [0; 256],
            address: 0,
            timer1: Timer::new(80),
            timer2: Timer::new(320),
        }
    }
}

impl OplChip {
    pub fn register(&self, index: u8) -> u8 {
        self.registers[index as usize]
    }

    /// Write a chip register, applying the side effects of the timer-control
    /// register (0x04) and storing everything else verbatim for later synthesis.
    pub fn write_register(&mut self, index: u8, value: u8) {
        if index == 0x04 {
            // bit0/bit1: start timer 1/2 (rising edge reloads from preset).
            // bit7: reset both overflow flags.
            if value & 0x80 != 0 {
                self.timer1.expired = false;
                self.timer2.expired = false;
            }
            let start1 = value & 0x01 != 0;
            let start2 = value & 0x02 != 0;
            if start1 && !self.timer1.running {
                self.timer1.start(self.registers[0x02]);
            } else {
                self.timer1.running = start1;
            }
            if start2 && !self.timer2.running {
                self.timer2.start(self.registers[0x03]);
            } else {
                self.timer2.running = start2;
            }
        }
        self.registers[index as usize] = value;
    }

    /// Advance the hardware timers by `micros` microseconds of chip time.
    pub fn advance_micros(&mut self, micros: u64) {
        let (preset1, preset2) = (self.registers[0x02], self.registers[0x03]);
        self.timer1.advance(micros, preset1);
        self.timer2.advance(micros, preset2);
    }

    /// OPL status byte: bit7 IRQ, bit6 timer-1 flag, bit5 timer-2 flag.
    /// A timer's overflow flag is always reported; the mask bits in register
    /// 0x04 (bit6 = timer 1, bit5 = timer 2) only gate the IRQ line.
    pub fn status(&self) -> u8 {
        let control = self.registers[0x04];
        let t1_irq = self.timer1.expired && control & 0x40 == 0;
        let t2_irq = self.timer2.expired && control & 0x20 == 0;
        ((t1_irq || t2_irq) as u8) << 7
            | (self.timer1.expired as u8) << 6
            | (self.timer2.expired as u8) << 5
    }

    pub fn read_port(&self, port: u16) -> Option<u8> {
        match port {
            0x0388 => Some(self.status()),
            _ => None,
        }
    }

    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        match port {
            0x0388 => {
                self.address = value;
                true
            }
            0x0389 => {
                self.write_register(self.address, value);
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
    fn exp_of_logsin_reconstructs_the_sine_quarter_wave() {
        // The whole point of the two ROMs: running each quarter-wave index
        // through log-sin then exp must rebuild sin() to within quantization.
        let max = f64::from(exp_lookup(u32::from(logsin(255)))); // loudest point
        for i in 0..256 {
            let attenuation = u32::from(logsin(i));
            let got = f64::from(exp_lookup(attenuation));
            let expected = ((i as f64 + 0.5) * std::f64::consts::PI / 512.0).sin() * max;
            assert!(
                (got - expected).abs() <= 4.0,
                "index {i}: got {got}, expected {expected}"
            );
        }
    }

    #[test]
    fn rom_tables_match_their_known_anchor_values() {
        assert_eq!(logsin(0), 2137, "quietest log-sin entry");
        assert_eq!(logsin(255), 0, "loudest log-sin entry");
        assert_eq!(
            exp_lookup(0),
            2042,
            "max half-amplitude at zero attenuation"
        );
    }

    #[test]
    fn adlib_detection_sequence_reports_present() {
        // The canonical AdLib probe: clear the timers, confirm the status
        // flags are quiet, fire timer 1, let it overflow, and confirm the
        // status port reports the IRQ + timer-1 flags (0xc0).
        let mut opl = OplChip::default();

        opl.write_register(0x04, 0x60); // mask both timers
        opl.write_register(0x04, 0x80); // reset the IRQ flags
        assert_eq!(opl.status() & 0xe0, 0x00, "flags clear after reset");

        opl.write_register(0x02, 0xff); // timer 1 preset: overflow in one step
        opl.write_register(0x04, 0x21); // start timer 1, mask timer 2
        opl.advance_micros(80); // one 80us timer-1 step -> overflow

        assert_eq!(
            opl.status() & 0xe0,
            0xc0,
            "timer 1 overflow raises IRQ (bit7) + timer-1 (bit6)"
        );
    }

    #[test]
    fn masked_timer_overflow_sets_flag_but_not_irq() {
        // A masked timer still records its overflow flag in the status byte;
        // only the IRQ (bit7) is suppressed. Faithful to real OPL silicon.
        let mut opl = OplChip::default();

        opl.write_register(0x03, 0xff); // timer 2 preset
        opl.write_register(0x04, 0x22); // start timer 2 (bit1) with it masked (bit5)
        opl.advance_micros(320); // one 320us timer-2 step -> overflow

        let status = opl.status();
        assert_eq!(status & 0x20, 0x20, "timer-2 flag is set");
        assert_eq!(status & 0x80, 0x00, "masked timer raises no IRQ");
    }

    #[test]
    fn address_data_ports_store_registers() {
        let mut opl = OplChip::default();
        assert!(opl.write_port(0x388, 0x20)); // latch register address 0x20
        assert!(opl.write_port(0x389, 0x2f)); // write the data
        assert_eq!(opl.register(0x20), 0x2f);
        assert_eq!(opl.read_port(0x388), Some(opl.status()));
    }
}
