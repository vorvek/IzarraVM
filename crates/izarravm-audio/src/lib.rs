use izarravm_core::{AudioConfig, MidiBackend};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDeviceKind {
    PcSpeaker,
    SoundBlaster,
    Opl3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixerConfig {
    pub sample_rate_hz: u32,
    pub channels: u16,
}

impl Default for MixerConfig {
    fn default() -> Self {
        Self {
            sample_rate_hz: 48_000,
            channels: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSubsystem {
    pub mixer: MixerConfig,
    pub devices: Vec<AudioDeviceKind>,
    pub midi_backend: MidiBackend,
}

impl AudioSubsystem {
    pub fn from_config(config: &AudioConfig) -> Self {
        let mut devices = Vec::new();
        if config.pc_speaker {
            devices.push(AudioDeviceKind::PcSpeaker);
        }
        if config.sound_blaster {
            devices.push(AudioDeviceKind::SoundBlaster);
        }
        if config.opl3 {
            devices.push(AudioDeviceKind::Opl3);
        }

        Self {
            mixer: MixerConfig::default(),
            devices,
            midi_backend: config.midi.backend,
        }
    }
}

/// Clean-room OPL2 (AdLib / YMF262-family) register model.
///
/// Models the register file plus the two hardware timers that drive AdLib
/// detection on ports 0x388 (address/status) and 0x389 (data). Sample
/// synthesis (phase + envelope generators) is not here yet; this is the
/// register/timer substrate every DOS game pokes before it makes a sound.
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

pub fn cpal_backend_marker() -> &'static str {
    std::any::type_name::<cpal::StreamConfig>()
}

pub fn midir_backend_marker() -> &'static str {
    std::any::type_name::<midir::MidiOutput>()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn enabled_audio_devices_follow_config() {
        let mut config = AudioConfig {
            opl3: false,
            ..AudioConfig::default()
        };
        let subsystem = AudioSubsystem::from_config(&config);
        assert_eq!(
            subsystem.devices,
            vec![AudioDeviceKind::PcSpeaker, AudioDeviceKind::SoundBlaster]
        );

        config.sound_blaster = false;
        let subsystem = AudioSubsystem::from_config(&config);
        assert_eq!(subsystem.devices, vec![AudioDeviceKind::PcSpeaker]);
    }
}
