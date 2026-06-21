//! Intel 8254 programmable interval timer: three independent counters.
//!
//! Built clean-room from the Intel 8254 datasheet cached at
//! dev_docs/reference/8254/. Channel 0's OUT drives IRQ0. All six counter modes
//! are modeled at input-CLK granularity. BCD counting decrements in decimal
//! (reload 0 means 10000). The nanosecond AC timing and channel 1/2 OUT
//! consumers are out of scope for this slice.

/// One 8254 counter. The counting element `count` decrements on each input CLK;
/// `reload` is the programmed count (0 means 65536). All six modes are modeled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Counter {
    mode: u8, // 0..=5
    rw: RwMode,
    bcd: bool,   // when set, the CE counts in BCD (decimal) rather than binary
    count: u32,  // CE, current value (u32 so 65536 fits)
    reload: u16, // CR, programmed count; 0 reads as 65536
    out: bool,   // OUT pin
    gate: bool,  // GATE level; the PC ties GATE0/GATE1 high (default true)
    state: CounterState,
    null_count: bool,   // set on control-word/count write, cleared when CE loads
    latch: Option<u16>, // counter-latch / read-back count output latch
    status_latch: Option<u8>,
    write_msb_next: bool,
    read_msb_next: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RwMode {
    Lsb,
    Msb,
    LsbThenMsb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CounterState {
    Inactive,  // no live count (after a control word, or a one-shot that finished)
    LoadDelay, // a count was written; CE loads on the next CLK
    Counting,
    WaitGate, // modes 1 and 5: armed, waiting for a GATE rising edge
}

impl Default for Counter {
    fn default() -> Self {
        Self {
            mode: 0,
            rw: RwMode::Lsb,
            bcd: false,
            count: 0,
            reload: 0,
            out: false,
            gate: true,
            state: CounterState::Inactive,
            null_count: false,
            latch: None,
            status_latch: None,
            write_msb_next: false,
            read_msb_next: false,
        }
    }
}

impl Counter {
    fn effective_reload(&self) -> u32 {
        if self.reload == 0 {
            // 0 means the full range. The same load value (0x10000) serves both
            // radices: in binary it counts down 65536 clocks to 0; in BCD the first
            // decrement masks to 0x0000 and wraps to 0x9999, giving a 10000-clock
            // period. (0x10000 == 65536, so one literal covers both.)
            0x10000
        } else {
            u32::from(self.reload)
        }
    }

    /// Decrement a packed-BCD count by `by` (1 or 2), borrowing per nibble. The
    /// count is stored as the guest wrote it: four BCD digits, one per nibble
    /// (0x0100 is decimal 100, not 256). On underflow it wraps to 0x9999, matching
    /// the chip's four-decade decimal counting element.
    fn bcd_dec(value: u32, by: u32) -> u32 {
        let mut result = value & 0xffff;
        for _ in 0..by {
            if result == 0 {
                result = 0x9999;
                continue;
            }
            // Subtract one, propagating a borrow across BCD nibbles.
            let mut digits = [
                result & 0xf,
                (result >> 4) & 0xf,
                (result >> 8) & 0xf,
                (result >> 12) & 0xf,
            ];
            let mut place = 0;
            loop {
                if digits[place] > 0 {
                    digits[place] -= 1;
                    break;
                }
                digits[place] = 9;
                place += 1;
            }
            result = digits[0] | (digits[1] << 4) | (digits[2] << 8) | (digits[3] << 12);
        }
        result
    }

    /// Decrement the counting element by one step in the active radix.
    fn dec(&self, value: u32, by: u32) -> u32 {
        if self.bcd {
            Self::bcd_dec(value, by)
        } else {
            value.wrapping_sub(by)
        }
    }

    fn write_control(&mut self, value: u8) {
        let rw_field = (value >> 4) & 0x3;
        if rw_field == 0 {
            // Counter-latch command: freeze the current count for reading.
            self.latch_count();
            return;
        }
        self.rw = match rw_field {
            1 => RwMode::Lsb,
            2 => RwMode::Msb,
            _ => RwMode::LsbThenMsb,
        };
        // M2 is a don't-care for modes 2 and 3, so 6 and 7 alias to 2 and 3.
        self.mode = match (value >> 1) & 0x7 {
            6 => 2,
            7 => 3,
            m => m,
        };
        self.bcd = value & 1 != 0;
        self.out = self.mode != 0; // mode 0 starts OUT low, the others high
        self.state = CounterState::Inactive;
        self.null_count = true;
        self.write_msb_next = false;
        self.read_msb_next = false;
        self.latch = None;
        self.status_latch = None;
    }

    fn arm(&mut self) {
        self.null_count = true;
        match self.mode {
            // Modes 1 and 5 are retriggerable one-shots. A new count written
            // mid-pulse is staged into `reload` (already done by the caller) and
            // the live pulse keeps running on the old value; the new reload loads
            // on the next GATE rising edge. Only arm to WaitGate when not already
            // counting, so an in-flight pulse is not aborted.
            1 | 5 => {
                if self.state != CounterState::Counting {
                    self.state = CounterState::WaitGate;
                }
            }
            _ => self.state = CounterState::LoadDelay,
        }
    }

    fn write_count(&mut self, value: u8) {
        match self.rw {
            RwMode::Lsb => {
                self.reload = (self.reload & 0xff00) | u16::from(value);
                self.arm();
            }
            RwMode::Msb => {
                self.reload = (self.reload & 0x00ff) | (u16::from(value) << 8);
                self.arm();
            }
            RwMode::LsbThenMsb => {
                if !self.write_msb_next {
                    self.reload = (self.reload & 0xff00) | u16::from(value);
                    self.write_msb_next = true;
                    if self.mode == 0 {
                        // Mode 0: writing the first byte stops counting, OUT low.
                        self.out = false;
                        self.state = CounterState::Inactive;
                    }
                } else {
                    self.reload = (self.reload & 0x00ff) | (u16::from(value) << 8);
                    self.write_msb_next = false;
                    self.arm();
                }
            }
        }
    }

    fn read(&mut self) -> u8 {
        if let Some(status) = self.status_latch.take() {
            return status;
        }
        let value = self.latch.unwrap_or((self.count & 0xffff) as u16);
        match self.rw {
            RwMode::Lsb => {
                self.latch = None;
                (value & 0xff) as u8
            }
            RwMode::Msb => {
                self.latch = None;
                (value >> 8) as u8
            }
            RwMode::LsbThenMsb => {
                if !self.read_msb_next {
                    self.read_msb_next = true;
                    (value & 0xff) as u8
                } else {
                    self.read_msb_next = false;
                    self.latch = None;
                    (value >> 8) as u8
                }
            }
        }
    }

    fn latch_count(&mut self) {
        if self.latch.is_none() {
            self.latch = Some((self.count & 0xffff) as u16);
        }
    }

    fn latch_status(&mut self) {
        if self.status_latch.is_none() {
            let rw_bits = match self.rw {
                RwMode::Lsb => 1,
                RwMode::Msb => 2,
                RwMode::LsbThenMsb => 3,
            };
            self.status_latch = Some(
                (u8::from(self.out) << 7)
                    | (u8::from(self.null_count) << 6)
                    | (rw_bits << 4)
                    | (self.mode << 1)
                    | u8::from(self.bcd),
            );
        }
    }

    fn set_gate(&mut self, level: bool) {
        let rising = !self.gate && level;
        let falling = self.gate && !level;
        self.gate = level;
        if rising {
            match self.mode {
                1 => {
                    self.count = self.effective_reload();
                    self.out = false;
                    self.state = CounterState::Counting;
                }
                5 => {
                    self.count = self.effective_reload();
                    self.out = true;
                    self.state = CounterState::Counting;
                }
                2 | 3 => self.state = CounterState::LoadDelay, // reload on next CLK
                _ => {}
            }
        } else if falling && matches!(self.mode, 2 | 3) {
            // GATE low forces OUT high immediately in modes 2 and 3, with no wait
            // for the next CLK. step_counting keeps a lazy force as a safety net.
            self.out = true;
        }
    }

    /// Advance one input CLK. Returns true on an OUT rising (low to high) edge.
    fn step(&mut self) -> bool {
        match self.state {
            CounterState::Inactive | CounterState::WaitGate => false,
            CounterState::LoadDelay => {
                self.count = self.effective_reload();
                self.null_count = false;
                self.state = CounterState::Counting;
                false
            }
            CounterState::Counting => self.step_counting(),
        }
    }

    fn step_counting(&mut self) -> bool {
        // Level-sensitive GATE: low pauses counting (modes 0, 2, 3, 4).
        if !self.gate {
            // GATE low forces OUT high in modes 2 and 3 and pauses counting.
            if matches!(self.mode, 2 | 3) {
                self.out = true;
            }
            return false;
        }
        match self.mode {
            0 | 1 => {
                self.count = self.dec(self.count, 1);
                if self.count == 0 && !self.out {
                    self.out = true;
                    if self.mode != 0 {
                        self.state = CounterState::Inactive; // one-shot done, await trigger
                    }
                    return true;
                }
                false
            }
            2 => {
                // ponytail: the datasheet forbids a mode-2 count of 1 (count 2 is
                // the minimum). A count of 1 never holds OUT low for a clock; we
                // leave that out-of-spec input to reload here rather than special-
                // case it, matching how real parts treat the illegal value loosely.
                if self.count <= 1 {
                    self.count = self.effective_reload();
                    let rose = !self.out;
                    self.out = true;
                    rose
                } else {
                    self.count = self.dec(self.count, 1);
                    if self.count == 1 {
                        self.out = false;
                    }
                    false
                }
            }
            3 => {
                if self.count <= 2 {
                    self.count = self.effective_reload();
                    self.out = !self.out;
                    self.out // rising edge when OUT returns high
                } else {
                    // Decrement by two. Exact for even counts (the PC timer case);
                    // odd-count duty asymmetry is not modeled.
                    self.count = self.dec(self.count, 2);
                    false
                }
            }
            4 | 5 => {
                // Modes 4 and 5: count down while OUT is high, drive OUT low for one
                // clock at terminal, then back high (the strobe) and stop. The rising
                // edge that fires IRQ0 is that return to high, so the strobe lands N+1
                // clocks after the count is loaded.
                if self.out {
                    self.count = self.dec(self.count, 1);
                    if self.count == 0 {
                        self.out = false; // strobe low for one clock
                    }
                    false
                } else {
                    self.out = true;
                    self.state = CounterState::Inactive; // one-shot strobe done
                    true
                }
            }
            _ => false,
        }
    }
}

/// The three-counter 8254. Channel 0's OUT rising edge is IRQ0.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Pit {
    counters: [Counter; 3],
}

impl Pit {
    pub(crate) fn write_port(&mut self, port: u16, value: u8) -> bool {
        match port {
            0x40..=0x42 => self.counters[(port - 0x40) as usize].write_count(value),
            0x43 => self.write_control_word(value),
            _ => return false,
        }
        true
    }

    fn write_control_word(&mut self, value: u8) {
        let sc = (value >> 6) & 0x3;
        if sc == 3 {
            // Read-back command: latch count and/or status for the selected counters.
            // D5 low (0x20) selects latch-count, D4 low (0x10) selects latch-status.
            let latch_count = value & 0x20 == 0;
            let latch_status = value & 0x10 == 0;
            // Both bits high means "latch nothing": a reserved/no-op form. Skip the
            // per-counter loop so it has no effect at all.
            if !latch_count && !latch_status {
                return;
            }
            for (i, counter) in self.counters.iter_mut().enumerate() {
                if value & (1 << (i + 1)) != 0 {
                    if latch_count {
                        counter.latch_count();
                    }
                    if latch_status {
                        counter.latch_status();
                    }
                }
            }
        } else {
            self.counters[sc as usize].write_control(value);
        }
    }

    pub(crate) fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            0x40..=0x42 => Some(self.counters[(port - 0x40) as usize].read()),
            _ => None,
        }
    }

    /// Advance every counter by `clocks` input CLK pulses. Returns the number of
    /// channel-0 OUT rising edges, which the machine turns into IRQ0 requests.
    pub(crate) fn tick(&mut self, clocks: u64) -> u32 {
        let mut edges = 0u32;
        for _ in 0..clocks {
            for (i, counter) in self.counters.iter_mut().enumerate() {
                let rose = counter.step();
                if i == 0 && rose {
                    edges += 1;
                }
            }
        }
        edges
    }

    /// Input CLK pulses until channel 0 produces its next OUT rising edge, or None
    /// if it cannot from its current state. Computed on a clone so it does not
    /// mutate, and shares the exact step logic with `tick`.
    pub(crate) fn clocks_until_channel0_irq(&self) -> Option<u64> {
        let mut probe = self.counters[0].clone();
        // A periodic counter's longest period is 65536 input clocks; cap a little
        // past that so a counter that will never fire returns None.
        (1..=65537u64).find(|&_clocks| probe.step())
    }

    pub(crate) fn set_gate(&mut self, channel: usize, level: bool) {
        if let Some(counter) = self.counters.get_mut(channel) {
            counter.set_gate(level);
        }
    }

    /// The current OUT pin level of a counter. Channel 2 drives the PC speaker.
    /// Out-of-range channels read false.
    pub(crate) fn channel_out(&self, channel: usize) -> bool {
        self.counters.get(channel).map(|c| c.out).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Control words: counter 0, LSB-then-MSB, binary, mode in bits 3..1.
    const CW_MODE0: u8 = 0x30;
    const CW_MODE1: u8 = 0x32;
    const CW_MODE2: u8 = 0x34;
    const CW_MODE3: u8 = 0x36;
    const CW_MODE4: u8 = 0x38;
    const CW_MODE5: u8 = 0x3a;

    fn program_ch0(pit: &mut Pit, control: u8, count: u16) {
        pit.write_port(0x43, control);
        pit.write_port(0x40, (count & 0xff) as u8);
        pit.write_port(0x40, (count >> 8) as u8);
    }

    #[test]
    fn mode3_default_count_is_18_2_hz() {
        // Count 0 means 65536. After the load clock, channel 0 raises IRQ0 every
        // 65536 input clocks: 1_193_182 / 65536 = 18.2065 Hz, the PC timer rate.
        let mut pit = Pit::default();
        program_ch0(&mut pit, CW_MODE3, 0);
        pit.tick(1); // consume the load delay
        assert_eq!(pit.clocks_until_channel0_irq(), Some(65536));
    }

    #[test]
    fn mode3_square_wave_period_and_one_edge() {
        let mut pit = Pit::default();
        program_ch0(&mut pit, CW_MODE3, 4);
        pit.tick(1);
        assert_eq!(pit.clocks_until_channel0_irq(), Some(4));
        assert_eq!(pit.tick(4), 1); // exactly one rising edge per period
        assert_eq!(pit.tick(4), 1); // periodic
    }

    #[test]
    fn mode2_rate_generator_period() {
        let mut pit = Pit::default();
        program_ch0(&mut pit, CW_MODE2, 4);
        pit.tick(1);
        assert_eq!(pit.clocks_until_channel0_irq(), Some(4));
        assert_eq!(pit.tick(4), 1);
        assert_eq!(pit.tick(4), 1);
    }

    #[test]
    fn mode0_one_shot_fires_once() {
        let mut pit = Pit::default();
        program_ch0(&mut pit, CW_MODE0, 4);
        pit.tick(1);
        assert_eq!(pit.clocks_until_channel0_irq(), Some(4)); // OUT rises at terminal
        assert_eq!(pit.tick(4), 1);
        assert_eq!(pit.tick(1000), 0); // no repeat
        assert_eq!(pit.clocks_until_channel0_irq(), None);
    }

    #[test]
    fn mode4_software_strobe_fires_once() {
        let mut pit = Pit::default();
        program_ch0(&mut pit, CW_MODE4, 4);
        pit.tick(1);
        // OUT high, strobes low at terminal then high one clock later (N+1).
        assert_eq!(pit.clocks_until_channel0_irq(), Some(5));
        assert_eq!(pit.tick(5), 1);
        assert_eq!(pit.tick(1000), 0);
    }

    #[test]
    fn modes_1_and_5_need_a_gate_trigger() {
        for cw in [CW_MODE1, CW_MODE5] {
            let mut pit = Pit::default();
            program_ch0(&mut pit, cw, 4);
            pit.tick(1);
            // GATE is high but never had a rising edge: no count, no IRQ.
            assert_eq!(pit.clocks_until_channel0_irq(), None);
            // A falling then rising GATE edge triggers the one-shot.
            pit.set_gate(0, false);
            pit.set_gate(0, true);
            assert!(pit.clocks_until_channel0_irq().is_some());
            assert_eq!(pit.tick(6), 1); // one strobe/edge then done
            assert_eq!(pit.tick(1000), 0);
        }
    }

    #[test]
    fn counter_latch_freezes_the_read() {
        let mut pit = Pit::default();
        program_ch0(&mut pit, CW_MODE3, 100);
        pit.tick(1); // load: count = 100
        pit.tick(4); // count decremented (mode 3 steps by two): 100 -> 92
        pit.write_port(0x43, 0x00); // counter-latch command, counter 0
        pit.tick(10); // keeps counting, but the latch is frozen
        let lo = pit.read_port(0x40).unwrap();
        let hi = pit.read_port(0x40).unwrap();
        assert_eq!(u16::from_le_bytes([lo, hi]), 92);
    }

    #[test]
    fn read_back_status_reports_mode_and_out() {
        let mut pit = Pit::default();
        program_ch0(&mut pit, CW_MODE3, 4);
        pit.tick(1);
        pit.write_port(0x43, 0xe2); // read-back: latch status, counter 0
        let status = pit.read_port(0x40).unwrap();
        assert_eq!(status & 0x80, 0x80); // OUT high in mode 3 after load
        assert_eq!((status >> 1) & 0x07, 3); // mode 3
        assert_eq!((status >> 4) & 0x03, 3); // RW = LSB then MSB
    }

    #[test]
    fn lsb_then_msb_write_and_read() {
        let mut pit = Pit::default();
        pit.write_port(0x43, CW_MODE3);
        pit.write_port(0x40, 0x34); // LSB
        pit.write_port(0x40, 0x12); // MSB -> count 0x1234
        pit.tick(1); // load
        pit.write_port(0x43, 0x00); // latch
        let lo = pit.read_port(0x40).unwrap();
        let hi = pit.read_port(0x40).unwrap();
        assert_eq!(u16::from_le_bytes([lo, hi]), 0x1234);
    }

    // Slice 1: BCD counting. Control words set bit 0 (BCD).
    const CW_MODE0_BCD: u8 = CW_MODE0 | 1;
    const CW_MODE2_BCD: u8 = CW_MODE2 | 1;

    #[test]
    fn bcd_dec_borrows_across_packed_nibbles() {
        // Values are packed BCD: 0x0100 is decimal 100, decrementing to 0x0099.
        assert_eq!(Counter::bcd_dec(0x0100, 1), 0x0099);
        assert_eq!(Counter::bcd_dec(0x0001, 1), 0x0000);
        assert_eq!(Counter::bcd_dec(0x0000, 1), 0x9999); // underflow wraps to top
        assert_eq!(Counter::bcd_dec(0x1000, 1), 0x0999);
        assert_eq!(Counter::bcd_dec(0x0000, 2), 0x9998); // two-step wrap
        assert_eq!(Counter::bcd_dec(0x0100, 2), 0x0098);
    }

    #[test]
    fn bcd_reload_zero_is_full_decimal_range() {
        // Reload 0 in BCD loads 0x10000 so the first decrement wraps to 0x9999 and
        // the period is exactly 10000 input clocks; in binary it is 65536.
        let mut c = Counter {
            bcd: true,
            reload: 0,
            ..Default::default()
        };
        assert_eq!(c.effective_reload(), 0x10000);
        c.bcd = false;
        assert_eq!(c.effective_reload(), 65536);

        // The packed-BCD decrement takes 10000 steps from the full-range load to 0.
        let mut count = 0x10000u32;
        let mut steps = 0u32;
        loop {
            count = Counter::bcd_dec(count, 1);
            steps += 1;
            if count == 0 {
                break;
            }
        }
        assert_eq!(steps, 10000);
    }

    #[test]
    fn bcd_mode2_counts_in_decimal() {
        // Program ch0 mode 2 BCD with count 0x0100 (= 100 decimal). The period in
        // input clocks must be 100, not 256.
        let mut pit = Pit::default();
        program_ch0(&mut pit, CW_MODE2_BCD, 0x0100);
        pit.tick(1); // load
        assert_eq!(pit.clocks_until_channel0_irq(), Some(100));
        assert_eq!(pit.tick(100), 1);
        assert_eq!(pit.tick(100), 1); // periodic
    }

    #[test]
    fn bcd_mode0_one_shot_decimal() {
        // Mode 0 BCD one-shot: count 0x0050 (= 50 decimal) fires once at clock 50.
        let mut pit = Pit::default();
        program_ch0(&mut pit, CW_MODE0_BCD, 0x0050);
        pit.tick(1); // load
        assert_eq!(pit.clocks_until_channel0_irq(), Some(50));
        assert_eq!(pit.tick(50), 1);
        assert_eq!(pit.tick(1000), 0); // no repeat
    }

    #[test]
    fn mode1_new_count_mid_pulse_waits_for_next_gate() {
        // Slice 2: a longer count written during a live mode-1 pulse must not abort
        // the pulse. The original count completes; the new count loads on the next
        // GATE rising edge.
        let mut pit = Pit::default();
        program_ch0(&mut pit, CW_MODE1, 4);
        // Trigger the one-shot with a GATE rising edge, then count partway.
        pit.set_gate(0, false);
        pit.set_gate(0, true);
        assert_eq!(pit.tick(2), 0); // two of four clocks consumed, pulse still live

        // Write a longer count (10) mid-pulse. The live pulse keeps its old count.
        pit.write_port(0x40, 10);
        pit.write_port(0x40, 0);
        assert!(!pit.channel_out(0)); // pulse still low, not aborted
        assert_eq!(pit.tick(2), 1); // original 4-clock pulse completes here
        assert!(pit.channel_out(0));

        // The new count only applies after the next GATE rising edge.
        pit.set_gate(0, false);
        pit.set_gate(0, true);
        assert_eq!(pit.tick(9), 0); // nine of the new ten clocks, still low
        assert_eq!(pit.tick(1), 1); // tenth clock completes the new pulse
    }

    #[test]
    fn mode3_gate_falling_forces_out_high_immediately() {
        // Slice 3: dropping GATE in mode 2/3 forces OUT high at once, no tick.
        let mut pit = Pit::default();
        // Use channel 2 so the wiring is exercised on a non-IRQ counter.
        pit.write_port(0x43, 0xb6); // counter 2, LSB/MSB, mode 3, binary
        pit.write_port(0x42, 10);
        pit.write_port(0x42, 0);
        pit.tick(1); // load
        // Raise then drop GATE on channel 2.
        pit.set_gate(2, true);
        pit.set_gate(2, false);
        assert!(pit.channel_out(2)); // high immediately, with no intervening tick
    }

    #[test]
    fn read_back_latch_nothing_is_a_no_op() {
        // Slice 4: a read-back with D5=D4=1 latches nothing. A following read must
        // still return the live count, not a stale latch.
        let mut pit = Pit::default();
        program_ch0(&mut pit, CW_MODE3, 100);
        pit.tick(1); // load: count = 100
        pit.tick(4); // mode 3 steps by two: 100 -> 92
        // Read-back, counter 0 selected, but neither count nor status latched.
        pit.write_port(0x43, 0xf2); // sc=11, D5=1, D4=1, counter-0 bit set
        pit.tick(4); // keeps counting: 92 -> 84
        // No latch was taken, so a normal read tracks the live count.
        pit.write_port(0x43, 0x00); // now latch for real
        let lo = pit.read_port(0x40).unwrap();
        let hi = pit.read_port(0x40).unwrap();
        assert_eq!(u16::from_le_bytes([lo, hi]), 84);
    }

    #[test]
    fn read_back_latch_nothing_does_not_latch_status() {
        // The no-op form must not produce a status byte either: a plain read after
        // it returns the count, not a latched status.
        let mut pit = Pit::default();
        program_ch0(&mut pit, CW_MODE0, 0x1234);
        pit.tick(1); // load: count = 0x1234
        pit.tick(1); // first real decrement: 0x1234 -> 0x1233
        pit.write_port(0x43, 0xf2); // read-back latch-nothing, counter 0
        pit.write_port(0x43, 0x00); // counter-latch command -> count latched
        let lo = pit.read_port(0x40).unwrap();
        let hi = pit.read_port(0x40).unwrap();
        // The latch-nothing read-back left no status latched, so the read returns
        // the live count: 0x1234 loaded, decremented once to 0x1233.
        assert_eq!(u16::from_le_bytes([lo, hi]), 0x1233);
    }
}
