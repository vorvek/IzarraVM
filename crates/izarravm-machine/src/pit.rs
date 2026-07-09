//! Intel 8254 programmable interval timer: three independent counters.
//!
//! Built clean-room from the Intel 8254 datasheet cached at
//! dev_docs/reference/8254/. Channel 0's OUT drives IRQ0; channel 1 is the AT
//! DRAM-refresh timer (mode 2) and channel 2 the PC speaker. All six counter modes
//! are modeled at input-CLK granularity, including the mode-3 odd-count asymmetry.
//! BCD counting decrements in decimal (reload 0 means 10000). Channel 1 and 2 OUT
//! are exposed through channel_out; the nanosecond AC timing is out of scope.

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

    /// Decrement a packed-BCD count by `by` (1, 2, or 3), borrowing per nibble. The
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

    /// Input CLK pulses until this counter's next OUT rising edge, or None
    /// when no rise can occur without new guest input (inactive, awaiting a
    /// GATE trigger, paused by a low GATE, an OUT that never rises again, or a
    /// degenerate count). Derived analytically from the same mode equations
    /// step_counting walks, so a caller can afford it once per CPU batch (the
    /// clone-and-step clocks_until_channel0_irq costs up to 65537 steps); the
    /// differential test pins this function to that simulation. BCD counters
    /// return a conservative None: no PC software clocks the PIT in BCD, and a
    /// None only relaxes the caller's batch cap (the edges themselves are
    /// counted exactly by tick_recording_out_transitions either way).
    fn clocks_until_out_rise(&self) -> Option<u64> {
        if self.bcd {
            return None;
        }
        match self.state {
            CounterState::Inactive | CounterState::WaitGate => None,
            // The pending count loads on the next CLK (one step, no edge) and
            // counting starts from the reload value. A low GATE still loads but
            // then pauses, and only guest port I/O can raise it again.
            CounterState::LoadDelay => {
                if !self.gate {
                    return None;
                }
                self.rise_from(self.effective_reload())
                    .map(|steps| steps + 1)
            }
            CounterState::Counting => {
                if !self.gate {
                    return None;
                }
                self.rise_from(self.count)
            }
        }
    }

    /// CLKs until the next OUT rising edge counting from `value` at the current
    /// OUT level, per the step_counting mode equations (binary radix, GATE
    /// high, already counting).
    fn rise_from(&self, value: u32) -> Option<u64> {
        let v = u64::from(value);
        let reload = u64::from(self.effective_reload());
        match self.mode {
            // Modes 0/1: OUT rises once, when the count reaches zero. A high
            // OUT never rises again (mode 0 keeps counting with OUT high; a
            // mode-1 pulse ends by going Inactive).
            // Divergence flag: in mode 0 a SINGLE-BYTE (LSB-only/MSB-only RW)
            // count rewrite after terminal count does not drop OUT in this
            // model (only the LSB-then-MSB first byte forces it low, see
            // write_count), so the reloaded count runs with OUT high and never
            // edges; the 8254 datasheet drops OUT on any new initial count.
            // step_counting encodes the same behavior, so this estimator and
            // the tick path stay behaviorally equivalent.
            0 | 1 => {
                if self.out || v == 0 {
                    None
                } else {
                    Some(v)
                }
            }
            2 => {
                if !self.out {
                    // OUT is low for exactly the count==1 clock; the next CLK
                    // reloads and rises.
                    Some(1)
                } else if v >= 2 {
                    // The count reaches 1 (OUT drops) after v-1 CLKs; one more
                    // reloads and rises.
                    Some(v)
                } else if reload >= 2 {
                    // Out-of-spec count <= 1 with OUT high: the next CLK
                    // reloads without an edge, then a full period runs.
                    Some(1 + reload)
                } else {
                    // Illegal reload 1: every CLK reloads, OUT never drops.
                    None
                }
            }
            3 => {
                if self.out {
                    Some(Self::mode3_half(v, true) + Self::mode3_half(reload, false))
                } else {
                    Some(Self::mode3_half(v, false))
                }
            }
            // Modes 4/5: count down with OUT high, strobe low for one CLK at
            // terminal, rise on the CLK after.
            4 | 5 => {
                if !self.out {
                    Some(1)
                } else if v == 0 {
                    None
                } else {
                    Some(v + 1)
                }
            }
            _ => None,
        }
    }

    /// Mode 3: CLKs until OUT toggles, counting from `value` in the half-cycle
    /// whose OUT level is `out`. The counting element steps by two, with an odd
    /// count trimmed on the first CLK of the half (by one with OUT high, by
    /// three with OUT low), so an odd period splits (N+1)/2 high, (N-1)/2 low.
    fn mode3_half(value: u64, out: bool) -> u64 {
        if value % 2 == 0 || !out {
            (value / 2).max(1)
        } else {
            value.div_ceil(2)
        }
    }

    /// The OUT level `clocks` input CLKs from now, without stepping. O(1): a small
    /// constant number of arithmetic ops and at most one modulo, never a loop over
    /// `clocks`. Reuses `rise_from`'s per-mode case analysis (the phase math is the
    /// same; this walks the SAME state machine, just answering "what level" instead
    /// of "how long until the next rise"). BCD counters return None (see
    /// `clocks_until_out_rise`: no PC software clocks the PIT in BCD, so this
    /// conservatively declines rather than modeling decimal half-cycles); a caller
    /// falls back to the non-lazy path exactly as it already does for a BCD rise
    /// query. GATE low mid-batch cannot happen without an intervening port write
    /// (`set_gate` is only reachable from a write path, which already ends the
    /// batch), so this assumes GATE stays at its current level for the whole
    /// `clocks` span, matching the batch-boundary contract `predicted_beam` and
    /// `clocks_until_out_rise` already rely on.
    ///
    /// Wired to production via `Pit::out_after` (P4a Task 2.3): the lazy port
    /// 0x61 bits 4/5 read peeks channel 1 and channel 2 through it.
    fn out_after(&self, clocks: u64) -> Option<bool> {
        if self.bcd {
            return None;
        }
        match self.state {
            // No live count: OUT cannot move without a guest write (arms a new
            // count) or a GATE edge (neither is a CLK), so it holds its level for
            // any `clocks` span within one batch.
            CounterState::Inactive | CounterState::WaitGate => Some(self.out),
            CounterState::LoadDelay => {
                if !self.gate {
                    return Some(self.out);
                }
                if clocks == 0 {
                    return Some(self.out);
                }
                // One CLK loads (no edge); the rest counts from the reload value.
                // Mode 0's LoadDelay always enters with OUT low (write_count forces
                // it there for the LSB-then-MSB first byte, and write_control does
                // for every mode), matching step's own load-then-count sequencing.
                let reload = u64::from(self.effective_reload());
                Some(Self::counting_out_after(
                    self.mode,
                    reload,
                    reload,
                    self.out,
                    clocks - 1,
                ))
            }
            CounterState::Counting => {
                if !self.gate {
                    return Some(self.out);
                }
                Some(Self::counting_out_after(
                    self.mode,
                    u64::from(self.count),
                    u64::from(self.effective_reload()),
                    self.out,
                    clocks,
                ))
            }
        }
    }

    /// OUT level `clocks` CLKs after a Counting state with counting element
    /// `value` at level `out`, per mode. Binary radix, GATE already high (the
    /// caller handles Inactive/WaitGate/GATE-low/BCD). Mirrors `rise_from`'s case
    /// split mode for mode so the two stay obviously in sync; unlike `rise_from`
    /// this never returns early on "no more edges" (modes 0/1/4/5 with OUT already
    /// high) because the level itself, not a distance to the next edge, is being
    /// asked for -- once OUT settles it just holds at that level.
    ///
    /// Relies on the state-machine invariant (true for every state `step_counting`
    /// actually produces, verified against the oracle): `value <= 1` at a Counting
    /// state boundary implies `out == false` in modes 2 and 3. The chip only sets
    /// `out = false` in the same step that decrements the counting element to 1
    /// (mode 2) or trims it into the `<= 1` range on an OUT-low half-clock (mode
    /// 3), so a stored state never has both `value <= 1` and `out == true`
    /// together; an out-of-spec reload of 0 or 1 still falls out of these
    /// equations without a panic (reload 0 is impossible, `effective_reload`
    /// always returns 0x10000 for a raw 0; reload 1 is the datasheet's own
    /// "illegal" case, handled explicitly below).
    fn counting_out_after(mode: u8, value: u64, reload: u64, out: bool, clocks: u64) -> bool {
        if clocks == 0 {
            return out;
        }
        match mode {
            // Modes 0/1: OUT rises once, at terminal count, and then holds (mode 0
            // keeps counting with OUT high; a mode-1 pulse's Counting state ends
            // there). OUT already high, or a degenerate v == 0 entry (mirrors
            // rise_from's None -- no rise within the modeled range) holds forever.
            0 | 1 => {
                if out || value == 0 {
                    out
                } else {
                    clocks >= value
                }
            }
            2 => {
                if reload <= 1 {
                    // The datasheet's illegal input (reload 0 is impossible via
                    // effective_reload; reload 1 reloads every CLK): OUT never
                    // drops, mirroring rise_from's own "Illegal reload 1" branch.
                    return true;
                }
                // Invariant: value <= 1 at a stored Counting state implies out ==
                // false (see the doc comment above). So out == true here means
                // value > 1: find the CLK where the counting element reaches 1
                // (the one low CLK per period), then fold the remainder into one
                // period of length `reload`.
                let next_low_at = if out { value - 1 } else { reload };
                if clocks < next_low_at {
                    return true;
                }
                let phase = (clocks - next_low_at) % reload;
                phase != 0
            }
            3 => {
                // CLKs until the current half-cycle's toggle, then fold the rest
                // of `clocks` into at most one full period (high half + low half,
                // which the odd-count asymmetry still sums to exactly `reload`
                // per mode3_odd_count_period_is_exact) via one modulo, then at
                // most one more half-length comparison -- O(1), never a loop over
                // `clocks` or over elapsed periods.
                let to_toggle = Self::mode3_half(value, out);
                if clocks < to_toggle {
                    return out;
                }
                let rem = clocks - to_toggle;
                let level = !out;
                if reload <= 1 {
                    // The datasheet's illegal mode-3 input (count 2 is the
                    // minimum legal reload): mode3_half floors both phases to 1
                    // clock each, so the real period is 2 CLKs (one high, one
                    // low), not `reload`'s single clock -- the "halves sum to
                    // reload" identity the general branch leans on does not hold
                    // here, so this folds the remainder by 2 directly instead.
                    return if rem % 2 == 0 { level } else { !level };
                }
                let phase = rem % reload;
                let half = Self::mode3_half(reload, level);
                if phase < half { level } else { !level }
            }
            // Modes 4/5: count down with OUT high, strobe low for one CLK at
            // terminal (the clock where the count reaches 0), then rise on the
            // CLK after and hold (the one-shot's Counting state ends there).
            4 | 5 => {
                if !out {
                    true
                } else if value == 0 {
                    out // degenerate entry, mirrors rise_from's None: never strobes
                } else {
                    match clocks.cmp(&value) {
                        std::cmp::Ordering::Less => true,
                        std::cmp::Ordering::Equal => false,
                        std::cmp::Ordering::Greater => true,
                    }
                }
            }
            _ => out,
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
            // Modes 2 and 3 are periodic. A count written while the counter is
            // already running is latched into `reload` (done by the caller) and
            // adopted at the next terminal count / half-cycle by step_counting,
            // which reloads from `reload`. It must NOT reset the live count, or a
            // guest that rewrites the count faster than one period (Prince of
            // Persia's speaker driver does) would never complete a cycle and the
            // tone would die. Only the initial load takes the immediate LoadDelay.
            2 | 3 => {
                if self.state != CounterState::Counting {
                    self.state = CounterState::LoadDelay;
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
                // Limit: the datasheet forbids a mode-2 count of 1 (count 2 is
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
                // Limit: a mode-3 count of 1 is illegal per the datasheet (count 2
                // is the minimum). effective_reload of 1 reaches here and reloads every
                // clock with no half-period, which is a loose handling of the bad input.
                //
                // The counting element steps by two so a half-period spans count/2
                // clocks. An odd count splits asymmetrically: the chip trims the count
                // even on the first clock of each half-period. With OUT high it
                // decrements by one (high phase is (N+1)/2 clocks); with OUT low it
                // decrements by three (low phase is (N-1)/2 clocks). The count only
                // stays odd on that first clock, so an odd count is the marker for it.
                let first_half_clock = self.count & 1 == 1;
                let by = if first_half_clock {
                    if self.out { 1 } else { 3 }
                } else {
                    2
                };
                if self.count <= by {
                    self.count = self.effective_reload();
                    self.out = !self.out;
                    self.out // rising edge when OUT returns high
                } else {
                    self.count = self.dec(self.count, by);
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

/// The AT DRAM-refresh divisor: channel 1 runs mode 2 with this count so its OUT
/// pulses at the refresh rate. A real AT BIOS POST programs 18 (0x12); the exact
/// period is approximate, the value only needs to make port 0x61 bit 4 toggle.
// Limit: 18 is the canonical AT refresh divisor but the precise refresh
// timing is not modeled to the nanosecond; this only seeds a live heartbeat.
const REFRESH_DIVISOR: u16 = 18;

/// The three-counter 8254. Channel 0's OUT rising edge is IRQ0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pit {
    counters: [Counter; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutTransition {
    pub(crate) tick: u64,
    pub(crate) level: bool,
}

impl Default for Pit {
    /// Power-on state with channel 1 pre-seeded as the AT DRAM-refresh timer
    /// (mode 2, count 18). A guest that never programs channel 1 still sees port
    /// 0x61 bit 4 toggle, the "memory refresh is alive" heartbeat some guests spin
    /// on, exactly as a real AT does after its BIOS programs the refresh timer.
    fn default() -> Self {
        let mut pit = Self {
            counters: [Counter::default(), Counter::default(), Counter::default()],
        };
        // Counter 1, LSB/MSB, mode 2, binary: SC=01, RW=11, mode=010 -> 0x74.
        pit.write_control_word(0x74);
        pit.counters[1].write_count((REFRESH_DIVISOR & 0xff) as u8);
        pit.counters[1].write_count((REFRESH_DIVISOR >> 8) as u8);
        pit
    }
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

    fn tick_with_observer<F>(
        &mut self,
        clocks: u64,
        watch_channel: Option<usize>,
        mut out_changed: F,
    ) -> u32
    where
        F: FnMut(u64, bool),
    {
        let mut edges = 0u32;
        let mut watched = watch_channel.map(|channel| self.channel_out(channel));
        for tick in 1..=clocks {
            for (i, counter) in self.counters.iter_mut().enumerate() {
                let rose = counter.step();
                if i == 0 && rose {
                    edges += 1;
                }
            }
            if let Some(channel) = watch_channel {
                let level = self.channel_out(channel);
                if Some(level) != watched {
                    watched = Some(level);
                    out_changed(tick, level);
                }
            }
        }
        edges
    }

    /// Advance every counter by `clocks` input CLK pulses. Returns the number of
    /// channel-0 OUT rising edges, which the machine turns into IRQ0 requests.
    #[cfg(test)]
    pub(crate) fn tick(&mut self, clocks: u64) -> u32 {
        self.tick_with_observer(clocks, None, |_, _| {})
    }

    /// Advance every counter and append channel OUT transitions with the PIT input
    /// tick on which they occurred. Tick numbers are 1-based within this advance.
    pub(crate) fn tick_recording_out_transitions(
        &mut self,
        clocks: u64,
        channel: usize,
        transitions: &mut Vec<OutTransition>,
    ) -> u32 {
        self.tick_with_observer(clocks, Some(channel), |tick, level| {
            transitions.push(OutTransition { tick, level });
        })
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

    /// Input CLK pulses until `channel`'s next OUT rising edge, or None when it
    /// cannot rise without new guest input. Analytic (O(1)); used by the
    /// Approximate-class batch cap once per CPU batch. Out-of-range channels
    /// report None.
    pub(crate) fn clocks_until_out_rise(&self, channel: usize) -> Option<u64> {
        self.counters
            .get(channel)
            .and_then(|counter| counter.clocks_until_out_rise())
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

    /// The analytic live OUT level of `channel` `clocks` input CLKs from now,
    /// without stepping (P4a Task 2.3: the lazy port 0x61 bits 4/5 read).
    /// `None` when the channel is out of range or the counter is BCD (see
    /// `Counter::out_after`); the caller falls back to a real `tick` in either
    /// case.
    pub(crate) fn out_after(&self, channel: usize, clocks: u64) -> Option<bool> {
        self.counters.get(channel).and_then(|c| c.out_after(clocks))
    }
}

#[cfg(test)]
#[path = "pit_test.rs"]
mod tests;
