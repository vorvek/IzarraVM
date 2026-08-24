// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Intel 8254 programmable interval timer: three independent counters.
//!
//! Built clean-room from the Intel 8254 datasheet. Channel 0's OUT drives IRQ0;
//! channel 1 is the AT
//! DRAM-refresh timer (mode 2) and channel 2 the PC speaker. All six counter modes
//! are modeled at input-CLK granularity, including the mode-3 odd-count asymmetry.
//! BCD counting decrements in decimal (reload 0 means 10000). Channel 1 and 2 OUT
//! are exposed through channel_out; the nanosecond AC timing is out of scope.

use izarravm_core::{CanonicalFieldWriter, CanonicalStateError};
use std::sync::OnceLock;

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

/// Truncate a counting-element value to the 16 bits a counter read exposes.
fn mask16(value: u64) -> u16 {
    (value & 0xffff) as u16
}

/// Decrement inside the 32-bit counting-element domain `step_counting` uses, so a
/// peek wraps exactly where a real sequence of `step` calls would.
fn wrap32(value: u64, by: u64) -> u64 {
    value.wrapping_sub(by) & 0xffff_ffff
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
        if value.is_multiple_of(2) || !out {
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
    /// The lazy port 0x61 bits 4/5 read peeks channel 1 and channel 2 through
    /// `Pit::out_after`.
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
                    // The load itself does not touch OUT, but the CLK after it
                    // enters `step_counting`, which forces OUT high in modes 2
                    // and 3 while the GATE is low. Two CLKs are needed: one to
                    // load, one to reach the force.
                    return Some(self.gate_low_out(clocks >= 2));
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
                    // `step_counting`'s first act with the GATE low is the
                    // mode-2/3 OUT force, so a single CLK is enough.
                    return Some(self.gate_low_out(clocks >= 1));
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

    /// `step_counting`'s lazy GATE-low OUT force, as a peek. With the GATE low the
    /// counting element is frozen, but modes 2 and 3 still drive OUT high (the
    /// datasheet's GATE-low behaviour; `set_gate` applies it eagerly on the falling
    /// edge and `step_counting` keeps it as a safety net). The peeks must apply it
    /// too: `latch_status` WRITES `out_after`'s answer into the status latch the
    /// guest then reads back, so a peek that skipped the force would hand the guest
    /// a level the chip does not hold. `forced` is whether the queried span reaches
    /// a CLK that runs `step_counting` at all.
    fn gate_low_out(&self, forced: bool) -> bool {
        if forced && matches!(self.mode, 2 | 3) {
            true
        } else {
            self.out
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
    /// Relies on NO reachability invariant: every combination of `value`, `reload`
    /// and `out` is answered exactly as `step_counting` would, whether or not the
    /// write paths can produce it. An earlier version leaned on "`value <= 1` at a
    /// Counting boundary implies `out == false` in modes 2 and 3" and, next to a
    /// hoisted `reload <= 1` check, that reasoning went wrong on a state the guest
    /// really does reach -- a periodic counter whose reload register is rewritten
    /// while its counting element is still large (`arm` deliberately does not reset
    /// the CE for modes 2 and 3). The differential test in `pit_test` now pins this
    /// against clone-and-step over CONSTRUCTED states, not only reachable ones, so
    /// no invariant has to be re-argued when a write path changes.
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
                // `step_counting` walks the CE down from `value` and only reloads
                // when it is already <= 1, so the periodic regime does not begin
                // until the FIRST reload CLK. From value >= 2 the CE reaches 1
                // after value-1 CLKs (the one low CLK) and reloads on the next;
                // from value <= 1 the very first CLK reloads.
                let first_reload_at = if value >= 2 { value } else { 1 };
                if clocks < first_reload_at {
                    // Before that reload nothing touches OUT except the single CLK
                    // that lands the CE on 1.
                    return if clocks + 1 == first_reload_at {
                        false
                    } else {
                        out
                    };
                }
                if reload <= 1 {
                    // The datasheet's illegal input (reload 0 is impossible via
                    // effective_reload; reload 1 reloads every CLK once the CE is
                    // in the reload regime, so the CE never lands on 1 again and
                    // OUT never drops). This can only be answered AFTER the walk
                    // above: a live CE larger than the rewritten reload still has
                    // its own low CLK to serve, which a hoisted check would miss.
                    return true;
                }
                let phase = (clocks - first_reload_at) % reload;
                // The CE lands on 1 on the last CLK of every period.
                phase + 1 != reload
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
                    return if rem.is_multiple_of(2) { level } else { !level };
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

    /// The counting element `clocks` input CLKs from now, as a counter READ would
    /// see it (masked to 16 bits), without stepping. The counter-value analogue of
    /// `out_after`, and the reason it exists: devices advance at batch END in every
    /// CPU mode, so a mid-batch latch or read of 0x40/0x42 would otherwise report
    /// the CE as of BATCH START. A guest that measures elapsed time by
    /// latch-compute-latch (the classic PC calibration loop) would then under-read
    /// the first sample by however far into its batch the latch landed -- up to the
    /// whole batch grain. This peek removes that error outright, so the batch cap is
    /// free to be as coarse as the fallback allows without moving a counter read.
    ///
    /// O(1) per mode: the same case analysis `counting_out_after` walks, answering
    /// "what value" instead of "what level", never a loop over `clocks`. BCD
    /// counters return None on the same grounds as `out_after` (no PC software
    /// clocks the PIT in BCD); the caller falls back to the live field, i.e. exactly
    /// today's batch-start behavior. GATE is assumed to hold its level for the whole
    /// span for the same batch-boundary reason `out_after` states.
    fn count_after(&self, clocks: u64) -> Option<u16> {
        if self.bcd {
            return None;
        }
        match self.state {
            // No live count: the CE cannot move without a guest write or a GATE
            // edge, neither of which is a CLK.
            CounterState::Inactive | CounterState::WaitGate => Some(self.masked_count()),
            CounterState::LoadDelay => {
                if clocks == 0 {
                    return Some(self.masked_count());
                }
                // One CLK loads the CE from the reload register (no edge); the rest
                // counts down from there, mirroring `step`'s load-then-count order.
                let reload = self.effective_reload();
                // The load is UNCONDITIONAL in `step`: only `step_counting` is
                // GATE-gated, so a low GATE still loads on the first CLK and then
                // pauses AT the reload value (`clocks_until_out_rise` states the
                // same rule: "a low GATE still loads but then pauses"). The
                // mirrored gate-low arm in `out_after` holds the stored level for
                // that first CLK (the load does not move OUT) and only applies the
                // mode-2/3 force from the second CLK on; the CE, by contrast, moves
                // on the load itself, and returning the pre-load field here would
                // report a stale count for a guest that drops GATE2 (0x61 bit 0)
                // and then reads 0x42.
                if !self.gate {
                    return Some(mask16(u64::from(reload)));
                }
                Some(Self::counting_count_after(
                    self.mode,
                    reload,
                    reload,
                    self.out,
                    clocks - 1,
                ))
            }
            CounterState::Counting => {
                if !self.gate {
                    return Some(self.masked_count());
                }
                Some(Self::counting_count_after(
                    self.mode,
                    self.count,
                    self.effective_reload(),
                    self.out,
                    clocks,
                ))
            }
        }
    }

    fn masked_count(&self) -> u16 {
        mask16(u64::from(self.count))
    }

    /// The CE `clocks` CLKs after a Counting state holding `value` at level `out`,
    /// per mode. Binary radix, GATE already high (the caller handles the other
    /// states). Mirrors `step_counting`'s case split mode for mode, including its
    /// out-of-spec branches, so the two stay obviously in sync; the differential
    /// test in `pit_test` pins it to clone-and-step.
    fn counting_count_after(mode: u8, value: u32, reload: u32, out: bool, clocks: u64) -> u16 {
        let v = u64::from(value);
        if clocks == 0 {
            return mask16(v);
        }
        let r = u64::from(reload); // >= 1: effective_reload maps a raw 0 to 0x10000
        match mode {
            // Mode 0 never stops: OUT rises at terminal count and the CE keeps
            // decrementing (and wrapping) forever, which is what a guest polling a
            // mode-0 counter past terminal actually reads back.
            0 => mask16(wrap32(v, clocks)),
            // Mode 1's one-shot ends AT terminal count (state goes Inactive), so the
            // CE parks at 0 there. The degenerate `v == 0` entry mirrors
            // `step_counting`: the first CLK wraps instead of terminating.
            1 => {
                let to_zero = if v == 0 { 1u64 << 32 } else { v };
                if !out && clocks >= to_zero {
                    0
                } else {
                    mask16(wrap32(v, clocks))
                }
            }
            2 => {
                // From v >= 2 the CE walks down to 1 (v-1 CLKs, OUT drops there) and
                // the next CLK reloads to r; from v <= 1 the very first CLK reloads.
                // After that it is periodic with period r. An illegal reload of 1
                // needs NO special case here -- `r - (clocks - at) % r` is 1 for
                // r == 1 -- but the walk down from a larger live CE must come
                // first, or a reload rewritten to 1 mid-period would report the
                // reload while the CE is still counting down to it.
                let (base, at) = if v >= 2 { (v, v) } else { (v, 1) };
                if clocks < at {
                    return mask16(base - clocks);
                }
                mask16(r - (clocks - at) % r)
            }
            3 => {
                let to_toggle = Self::mode3_half(v, out);
                if clocks < to_toggle {
                    return mask16(Self::mode3_value_in_half(v, out, clocks));
                }
                // Illegal reload <= 1 reloads every CLK, so no half-period exists.
                if r <= 1 {
                    return mask16(r);
                }
                // At `to_toggle` the CE reloads to r and OUT toggles. The two halves
                // sum to exactly r (mode3_odd_count_period_is_exact), so one modulo
                // folds the remainder into a single period and one comparison picks
                // the half.
                let rem = (clocks - to_toggle) % r;
                let first_level = !out;
                let first_half = Self::mode3_half(r, first_level);
                if rem < first_half {
                    mask16(Self::mode3_value_in_half(r, first_level, rem))
                } else {
                    mask16(Self::mode3_value_in_half(r, out, rem - first_half))
                }
            }
            // Modes 4/5: while OUT is high the CE counts down to 0 (the strobe
            // clock); the CLK after that raises OUT and ends the one-shot, leaving
            // the CE parked at its terminal value. A mid-strobe entry (OUT low) is
            // exactly that last CLK, so the CE does not move at all.
            4 | 5 => {
                if !out {
                    return mask16(v);
                }
                let to_zero = if v == 0 { 1u64 << 32 } else { v };
                if clocks >= to_zero {
                    0
                } else {
                    mask16(wrap32(v, clocks))
                }
            }
            _ => mask16(v),
        }
    }

    /// The CE `k` CLKs into a mode-3 half-period that began at `value` with OUT at
    /// `out`, for `k` strictly inside the half (the caller folds the phase first).
    /// The chip steps the CE by two, trimming an odd count on the FIRST CLK of the
    /// half -- by one with OUT high, by three with OUT low -- which is the same
    /// asymmetry `step_counting` and `mode3_half` encode.
    fn mode3_value_in_half(value: u64, out: bool, k: u64) -> u64 {
        if k == 0 {
            return value;
        }
        if value & 1 == 1 {
            if out {
                value + 1 - 2 * k
            } else {
                value - 1 - 2 * k
            }
        } else {
            value - 2 * k
        }
    }

    fn write_control(&mut self, value: u8, clocks: u64) {
        let rw_field = (value >> 4) & 0x3;
        if rw_field == 0 {
            // Counter-latch command: freeze the current count for reading.
            self.latch_count(clocks);
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

    /// `clocks` is the in-batch PIT-clock offset of this access; an unlatched read
    /// reports the CE as of that instant rather than as of batch start (see
    /// `count_after`).
    fn read(&mut self, clocks: u64) -> u8 {
        if let Some(status) = self.status_latch.take() {
            return status;
        }
        let live = self
            .count_after(clocks)
            .unwrap_or_else(|| self.masked_count());
        let value = self.latch.unwrap_or(live);
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

    /// Freeze the CE as of the in-batch instant `clocks`, not as of batch start:
    /// the latch is a snapshot taken when the guest asked, and the batch-end advance
    /// then steps the live CE past it exactly as the chip would.
    fn latch_count(&mut self, clocks: u64) {
        if self.latch.is_none() {
            let live = self
                .count_after(clocks)
                .unwrap_or_else(|| self.masked_count());
            self.latch = Some(live);
        }
    }

    /// The status byte's NULL COUNT bit at the same in-batch instant `out_after`
    /// peeks OUT at. Bit 6 is the guest's "has the count I just wrote reached the
    /// counting element yet" answer, and it is the one bit of the status byte
    /// besides OUT that a CLK can move -- so reading the live field made a
    /// mid-batch status read report the BATCH-START value, which is the same
    /// staleness `count_after` and `out_after` closed for the other two ports.
    ///
    /// Exact, not conservative, and it needs no BCD escape: this is a
    /// state-machine fact, not an arithmetic one.
    /// - `LoadDelay` with at least one CLK to spend: `step` loads the counting
    ///   element from the reload register and clears `null_count` on that first
    ///   CLK, and it does so UNCONDITIONALLY -- only `step_counting` is
    ///   GATE-gated -- so a low GATE does not hold the bit set. This is the same
    ///   load-on-the-first-CLK rule `count_after`'s `LoadDelay` arm mirrors.
    /// - `WaitGate` (modes 1 and 5, armed and waiting for a GATE rising edge):
    ///   `step` returns without loading, so the bit HOLDS however many CLKs
    ///   pass. Only a GATE edge clears it, and GATE is assumed to hold its level
    ///   across the batch for the same boundary reason `out_after` states.
    /// - `Inactive` and `Counting`: no CLK touches `null_count` in either.
    fn null_count_after(&self, clocks: u64) -> bool {
        match self.state {
            CounterState::LoadDelay if clocks >= 1 => false,
            _ => self.null_count,
        }
    }

    /// Same instant for the status byte's OUT bit, through the existing `out_after`
    /// peek, and for its NULL COUNT bit through `null_count_after`. The other
    /// fields (RW mode, mode, BCD) are register state that no CLK can move.
    fn latch_status(&mut self, clocks: u64) {
        if self.status_latch.is_none() {
            let out = self.out_after(clocks).unwrap_or(self.out);
            let null_count = self.null_count_after(clocks);
            let rw_bits = match self.rw {
                RwMode::Lsb => 1,
                RwMode::Msb => 2,
                RwMode::LsbThenMsb => 3,
            };
            self.status_latch = Some(
                (u8::from(out) << 7)
                    | (u8::from(null_count) << 6)
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

    // -- Analytic bulk advance (`IZARRAVM_PIT_BULK_ADVANCE`) ------------------
    //
    // `advance` and `out_transitions_in` below are the closed forms of `step`
    // and of the per-CLK observer loop in `Pit::tick_with_observer`. They mirror
    // `step_counting` arm for arm rather than composing the read-only peeks
    // (`out_after` / `count_after`), because the peeks return a 16-bit READ
    // view, and the counting element can legitimately hold 0x10000 (a raw reload
    // of 0) at the instant it reloads. A masked write-back would be a real state
    // divergence.
    //
    // The GATE-low mode-2/3 OUT force is no longer a second reason: `out_after`
    // applies it through `gate_low_out`, since `latch_status` writes that answer
    // into a latch the guest reads back. `advance_counting` still applies it in
    // its own arm, on the field itself.

    /// Store a counting-element value computed in the 32-bit domain `wrap32` and
    /// `effective_reload` work in. Never masked to 16 bits: `count` really does
    /// hold 0x10000 for one CLK after a raw-zero reload, and `Counter` derives
    /// `PartialEq`, so a mask here would show up as a state divergence.
    fn set_count(&mut self, value: u64) {
        self.count = (value & 0xffff_ffff) as u32;
    }

    /// Why this counter cannot be advanced analytically, or `None` when it can.
    ///
    /// The whole chip is screened through this BEFORE any counter is mutated: a
    /// decline discovered halfway through would leave a partly advanced chip
    /// that the per-CLK fallback would then advance a second time.
    fn bulk_decline(&self) -> Option<BulkDecline> {
        if self.bcd {
            // Same ground as `out_after` / `count_after` / `clocks_until_out_rise`:
            // no PC software clocks the PIT in BCD, so decimal half-cycles are
            // not modeled analytically. Not the knob -- a BCD counter takes the
            // loop on both arms.
            return Some(BulkDecline::Bcd);
        }
        let live = matches!(self.state, CounterState::LoadDelay | CounterState::Counting);
        if live && matches!(self.mode, 2 | 3) && self.effective_reload() <= 1 {
            // The datasheet's illegal input (count 2 is the minimum for modes 2
            // and 3). `step_counting` handles it loosely -- it reloads on every
            // CLK -- which leaves no period for the analytic form to fold, and
            // in mode 3 no half-period either. Declining is exact by
            // construction; guessing would not be.
            return Some(BulkDecline::IllegalReload);
        }
        None
    }

    /// Apply `clocks` input CLKs and return the number of OUT RISING edges in
    /// `(0, clocks]`. The post-state is what `clocks` calls to `step` produce,
    /// field for field -- `Counter` derives `PartialEq`, so the differential
    /// test asserts exactly that and does not spot-check fields.
    ///
    /// The caller has already screened the chip through `bulk_decline`, so this
    /// cannot fail: binary radix, and modes 2/3 have a foldable period.
    fn advance(&mut self, clocks: u64) -> u64 {
        if clocks == 0 {
            return 0;
        }
        match self.state {
            // No live count: `step` returns false and mutates nothing.
            CounterState::Inactive | CounterState::WaitGate => 0,
            CounterState::LoadDelay => {
                // `step`'s LoadDelay arm, verbatim and UNCONDITIONAL -- the load
                // does not consult GATE. It is also the ONLY place `null_count`
                // is cleared; the mode-2/3 reload inside `step_counting` does
                // not clear it.
                self.count = self.effective_reload();
                self.null_count = false;
                self.state = CounterState::Counting;
                self.advance_counting(clocks - 1)
            }
            CounterState::Counting => self.advance_counting(clocks),
        }
    }

    /// `advance`'s Counting core: `clocks` CLKs of `step_counting`, closed form.
    fn advance_counting(&mut self, clocks: u64) -> u64 {
        if clocks == 0 {
            return 0;
        }
        if !self.gate {
            // `step_counting`'s lazy GATE-low force, reproduced deliberately.
            //
            // The read-only peeks do NOT do this: `out_after`'s Counting +
            // !gate arm returns the STORED level. Today that divergence is
            // unreachable -- the only two ways into a GATE-low mode-2/3 state
            // are `set_gate`'s falling edge and `write_control`, and both set
            // `out = true` on the way in -- so the peeks and the tick path
            // agree. This must not LEAN on that: it writes the field, and a
            // latent disagreement written back is a real state divergence.
            if matches!(self.mode, 2 | 3) {
                self.out = true;
            }
            return 0;
        }
        let v = u64::from(self.count);
        let r = u64::from(self.effective_reload());
        match self.mode {
            0 | 1 => {
                // The CE decrements every CLK; OUT rises exactly once, on the
                // CLK that takes it to zero, and only from a low OUT. A `v == 0`
                // entry wraps rather than edging (the decrement runs before the
                // zero test), which is `counting_out_after`'s `out || value == 0`
                // guard; `clocks` is capped below 2^32 by `Pit::bulk_decline`, so
                // the wrap cannot come back round to zero inside one advance.
                let rose = !self.out && v != 0 && clocks >= v;
                if rose {
                    self.out = true;
                    if self.mode != 0 {
                        // Mode 1's one-shot ENDS at terminal count, parking the
                        // CE at 0; mode 0 keeps decrementing with OUT high.
                        self.state = CounterState::Inactive;
                        self.count = 0;
                        return 1;
                    }
                }
                self.set_count(wrap32(v, clocks));
                u64::from(rose)
            }
            2 => {
                // The CE walks down to 1 (OUT drops on that CLK) and the NEXT
                // CLK reloads and raises OUT. So from v >= 2 the first reload
                // lands on CLK v; from v <= 1 it lands on CLK 1. `rose` at that
                // first reload is `!out`, which v >= 2 forces true because the
                // preceding CLK just drove OUT low. `r >= 2` here (bulk_decline).
                let (first_reload, rose_first) = if v >= 2 { (v, true) } else { (1, !self.out) };
                if clocks < first_reload {
                    // Still inside the initial run-down, so v >= 2 and the CE is
                    // v - clocks; OUT is low only on the single CLK where it is 1.
                    let count = v - clocks;
                    self.set_count(count);
                    if count == 1 {
                        self.out = false;
                    }
                    return 0;
                }
                let past = clocks - first_reload;
                let phase = past % r;
                self.set_count(r - phase);
                self.out = phase != r - 1;
                u64::from(rose_first) + past / r
            }
            3 => {
                // The CE steps by two and an odd count is trimmed on the FIRST
                // CLK of a half (by one with OUT high, by three with OUT low),
                // which `mode3_half` / `mode3_value_in_half` already encode. The
                // two halves sum to exactly `reload`
                // (`mode3_odd_count_period_is_exact`), so one modulo folds the
                // remainder into a single period. `r >= 2` here (bulk_decline).
                let to_toggle = Self::mode3_half(v, self.out);
                if clocks < to_toggle {
                    self.set_count(Self::mode3_value_in_half(v, self.out, clocks));
                    return 0;
                }
                let level0 = !self.out; // the level the FIRST toggle lands on
                let half0 = Self::mode3_half(r, level0);
                let rem = clocks - to_toggle;
                let phase = rem % r;
                let (level, count) = if phase < half0 {
                    (level0, Self::mode3_value_in_half(r, level0, phase))
                } else {
                    (
                        !level0,
                        Self::mode3_value_in_half(r, !level0, phase - half0),
                    )
                };
                self.out = level;
                self.set_count(count);
                // Toggles after the first land at rem == k*r (back onto level0)
                // and at rem == k*r + half0 (onto !level0). A rise is a toggle
                // onto true, so only one of those two families ever counts.
                if level0 {
                    1 + rem / r
                } else if rem >= half0 {
                    (rem - half0) / r + 1
                } else {
                    0
                }
            }
            4 | 5 => {
                if !self.out {
                    // Mid-strobe: the very next CLK raises OUT and ends the
                    // one-shot. The CE does not move.
                    self.out = true;
                    self.state = CounterState::Inactive;
                    return 1;
                }
                if v == 0 {
                    // Degenerate entry: the CE wraps instead of strobing (the
                    // decrement runs before the zero test), mirroring
                    // `rise_from`'s None. The sub-2^32 cap on `clocks` keeps the
                    // wrap from coming back round to zero.
                    self.set_count(wrap32(v, clocks));
                    return 0;
                }
                if clocks <= v {
                    self.set_count(v - clocks);
                    if clocks == v {
                        self.out = false; // the one strobe CLK
                    }
                    return 0;
                }
                self.count = 0;
                self.state = CounterState::Inactive;
                1
            }
            _ => 0,
        }
    }

    /// Emit this counter's OUT transitions over the next `clocks` input CLKs as
    /// 1-based tick numbers, WITHOUT advancing it.
    ///
    /// O(transitions), never O(clocks). The observer loop it replaces emits at
    /// tick `t` when `channel_out(channel)` differs from its value at `t - 1`,
    /// and a channel's OUT depends on nothing but that channel, so the emitted
    /// set is exactly this counter's own transitions. It must therefore be read
    /// off the PRE-advance state -- the caller enumerates before it advances.
    fn out_transitions_in<F: FnMut(u64, bool)>(&self, clocks: u64, emit: &mut F) {
        if clocks == 0 {
            return;
        }
        match self.state {
            CounterState::Inactive | CounterState::WaitGate => {}
            CounterState::LoadDelay => {
                // The load CLK moves the CE, never OUT; counting resumes on the
                // CLK after it, from the reload value.
                let reload = u64::from(self.effective_reload());
                self.counting_transitions(reload, clocks - 1, 1, emit);
            }
            CounterState::Counting => {
                self.counting_transitions(u64::from(self.count), clocks, 0, emit)
            }
        }
    }

    /// `out_transitions_in`'s Counting core. `offset` shifts every emitted tick,
    /// for the LoadDelay entry whose first CLK is the load.
    fn counting_transitions<F: FnMut(u64, bool)>(
        &self,
        v: u64,
        clocks: u64,
        offset: u64,
        emit: &mut F,
    ) {
        if clocks == 0 {
            return;
        }
        if !self.gate {
            // The lazy GATE-low force is an OUT change like any other, and the
            // observer loop would report it on the first paused CLK.
            if matches!(self.mode, 2 | 3) && !self.out {
                emit(offset + 1, true);
            }
            return;
        }
        let r = u64::from(self.effective_reload());
        match self.mode {
            0 | 1 => {
                if !self.out && v != 0 && v <= clocks {
                    emit(offset + v, true);
                }
            }
            2 => {
                // OUT is high except for the single CLK before each reload, when
                // the CE sits at 1. From v >= 2 that low CLK is v - 1 and the
                // reload is v; from v <= 1 the reload is CLK 1 with no low CLK
                // ahead of it. Both edges are emitted only when the level really
                // moves, so an entry already at the target level emits nothing.
                let mut level = self.out;
                let (mut low, mut high) = if v >= 2 { (v - 1, v) } else { (0, 1) };
                loop {
                    if low >= 1 {
                        if low > clocks {
                            break;
                        }
                        if level {
                            emit(offset + low, false);
                            level = false;
                        }
                    }
                    if high > clocks {
                        break;
                    }
                    if !level {
                        emit(offset + high, true);
                        level = true;
                    }
                    low = high + r - 1;
                    high += r;
                }
            }
            3 => {
                let mut level = self.out;
                let mut tick = Self::mode3_half(v, level);
                while tick <= clocks {
                    level = !level;
                    emit(offset + tick, level);
                    tick += Self::mode3_half(r, level);
                }
            }
            4 | 5 => {
                if !self.out {
                    emit(offset + 1, true);
                } else if v != 0 {
                    if v <= clocks {
                        emit(offset + v, false);
                    }
                    if v < clocks {
                        // i.e. the strobe-return CLK `v + 1` is within range.
                        emit(offset + v + 1, true);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Why one bulk advance fell back to the per-CLK loop. Not the knob: these three
/// are properties of the programmed chip (or of the span), and each takes the
/// loop on BOTH arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BulkDecline {
    Bcd,
    IllegalReload,
    SpanTooWide,
}

/// `IZARRAVM_PIT_BULK_ADVANCE`, the analytic PIT advance arm. **DEFAULT ON since
/// the 2026-08-25 flip.**
///
/// NULLING SEMANTICS, stated because this campaign has been bitten by them
/// twice (`env-null-empty-is-off-trap`, and `IZARRAVM_SEGMENT_RETIRE_GOVERNOR`
/// whose empty string is the ESCAPE while unset is the default):
///
/// * **unset** -> the default, which is now **ON** (the analytic bulk advance)
/// * **`""` (empty)** -> **the same as unset**, i.e. the default, i.e. now ON
/// * `"0"` / `"off"` -> OFF (the per-CLK loop: the ESCAPE and the A/B base)
/// * `"1"` / `"on"` -> ON, stated
/// * anything else, or non-UTF-8 -> **panic**, naming the accepted spellings
///
/// Empty means DEFAULT, not "the OFF arm on purpose". **They coincided before
/// this flip and they DO NOT COINCIDE NOW**: an empty variable selects ON. That
/// is precisely why the rule was written as "empty == unset" rather than
/// "empty == off" while the default was still OFF -- so the flip moved the
/// default without moving the rule. **Every OFF leg must EXPORT `0`; clearing
/// the variable runs the ON arm.** `[Environment]::SetEnvironmentVariable(
/// $name, $null, "Process")` -- how every harness in this tree clears a variable
/// -- leaves the variable PRESENT AND EMPTY on Windows, and a leg that cleared
/// the variable means "the default", never "the escape".
///
/// This follows `run::parse_ata_poll_skip_arm` (`run.rs`), which is the
/// well-behaved knob in this crate: same spellings, same panic-on-typo, same
/// "empty is unset" rule. It differs from it in the DEFAULT only, because this
/// is a new slice and flipped only after a ladder. It now matches that knob in
/// default as well: doom-486 min-wall ratio 1.0596 against a 1.03 bar with 12 of
/// 12 A/B pairs ON-faster, doom-586 1.0140 against 1.01 with the sign agreeing,
/// zero contaminated legs, and guest_seconds / perf.instructions /
/// raw_bus_clocks / realtics / gametics identical across arms over 48 legs.
///
/// The panic on a typo is the point: a mistyped ladder leg that fell through to
/// the default would be read as "the arm I named changed nothing".
///
/// Resolved ONCE into a `OnceLock` and read at machine construction, never
/// inside the advance and never per CLK (`default-off-instruments-tax-hot-path`).
/// Both arms are reachable in ONE binary through
/// `Machine::set_pit_bulk_advance_enabled`.
pub(crate) fn bulk_advance_default() -> bool {
    static ARM: OnceLock<bool> = OnceLock::new();
    *ARM.get_or_init(|| parse_bulk_advance_arm(std::env::var("IZARRAVM_PIT_BULK_ADVANCE")))
}

const BULK_ADVANCE_SPELLINGS: &str = "accepted spellings are unset or `` (both the default, \
     which is OFF: the per-input-CLK loop), `0` / `off` (the same OFF arm, stated), and `1` / \
     `on` (the analytic bulk advance)";

fn parse_bulk_advance_arm(value: Result<String, std::env::VarError>) -> bool {
    let raw = match value {
        Err(std::env::VarError::NotPresent) => return true,
        // Not-UTF-8 is not a spelling of either arm: someone set the variable
        // and meant something by it, so it reaches the typo panic rather than
        // the silence of "unset".
        Err(std::env::VarError::NotUnicode(_)) => panic!(
            "IZARRAVM_PIT_BULK_ADVANCE is set to a value that is not valid UTF-8; \
             {BULK_ADVANCE_SPELLINGS}"
        ),
        Ok(raw) => raw,
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        // Empty is the DEFAULT, which is now ON. It is deliberately NOT grouped
        // with the escape spellings: see the nulling note above.
        "" => true,
        "0" | "off" => false,
        "1" | "on" => true,
        other => panic!(
            "IZARRAVM_PIT_BULK_ADVANCE={other:?} names no arm; {BULK_ADVANCE_SPELLINGS}. \
             Refusing to guess: a mistyped ladder leg would silently run the DEFAULT and be \
             read as the arm it named doing nothing"
        ),
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

/// Borrowed, behaviorally effective PIT state for canonical comparison.
///
/// Counter order is fixed by the 8254 channel numbering. The projection keeps
/// live timing state while removing history that no later port read, counter
/// transition, or deadline can observe.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CanonicalPit<'a> {
    pit: &'a Pit,
}

const fn rw_mode_tag(rw: RwMode) -> u8 {
    match rw {
        RwMode::Lsb => 0,
        RwMode::Msb => 1,
        RwMode::LsbThenMsb => 2,
    }
}

const fn counter_state_tag(state: CounterState) -> u8 {
    match state {
        // Both states hold OUT and ignore input clocks. Every port, GATE, and
        // prediction path treats them identically until the next programming
        // operation or trigger.
        CounterState::Inactive | CounterState::WaitGate => 0,
        CounterState::LoadDelay => 1,
        CounterState::Counting => 2,
    }
}

fn canonical_count(counter: &Counter) -> u32 {
    let retain_full_count = counter.state == CounterState::Counting
        && match counter.mode {
            0 | 1 => !counter.out,
            2 | 3 => true,
            4 | 5 => counter.out,
            _ => false,
        };
    if retain_full_count {
        counter.count
    } else {
        counter.count & 0xffff
    }
}

fn canonical_latch(counter: &Counter) -> (bool, u16) {
    let Some(value) = counter.latch else {
        return (false, 0);
    };
    let value = match counter.rw {
        RwMode::Lsb => value & 0x00ff,
        RwMode::Msb => value & 0xff00,
        RwMode::LsbThenMsb if counter.read_msb_next => value & 0xff00,
        RwMode::LsbThenMsb => value,
    };
    (true, value)
}

fn write_canonical_counter(
    out: &mut CanonicalFieldWriter<'_>,
    counter: &Counter,
) -> Result<(), CanonicalStateError> {
    let (latch_present, latch) = canonical_latch(counter);
    let (status_present, status) = counter
        .status_latch
        .map_or((false, 0), |value| (true, value));
    let dual_byte = counter.rw == RwMode::LsbThenMsb;
    out.write_u8(counter.mode)?;
    out.write_u8(rw_mode_tag(counter.rw))?;
    out.write_bool(counter.bcd)?;
    out.write_u32(canonical_count(counter))?;
    out.write_u16(counter.reload)?;
    out.write_bool(counter.out)?;
    out.write_bool(counter.gate)?;
    out.write_u8(counter_state_tag(counter.state))?;
    out.write_bool(counter.null_count)?;
    out.write_bool(latch_present)?;
    out.write_u16(latch)?;
    out.write_bool(status_present)?;
    out.write_u8(status)?;
    out.write_bool(dual_byte && counter.write_msb_next)?;
    out.write_bool(dual_byte && counter.read_msb_next)
}

impl CanonicalPit<'_> {
    /// Writes version 1 of the fixed 60-byte three-counter PIT payload.
    pub(crate) fn write_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        for counter in &self.pit.counters {
            write_canonical_counter(out, counter)?;
        }
        Ok(())
    }
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
        pit.write_control_word(0x74, 0);
        pit.counters[1].write_count((REFRESH_DIVISOR & 0xff) as u8);
        pit.counters[1].write_count((REFRESH_DIVISOR >> 8) as u8);
        pit
    }
}

impl Pit {
    pub(crate) fn canonical_projection(&self) -> CanonicalPit<'_> {
        CanonicalPit { pit: self }
    }

    /// Port write at the in-batch PIT-clock offset `clocks`. Only the latch
    /// commands on 0x43 read device state, and they take the peek so a latch
    /// records the instant the guest asked for (see `Counter::count_after`).
    pub(crate) fn write_port_at(&mut self, port: u16, value: u8, clocks: u64) -> bool {
        match port {
            0x40..=0x42 => self.counters[(port - 0x40) as usize].write_count(value),
            0x43 => self.write_control_word(value, clocks),
            _ => return false,
        }
        true
    }

    /// Zero-offset convenience for tests that drive the chip directly with no
    /// batch around it. Production goes through `write_port_at`.
    #[cfg(test)]
    pub(crate) fn write_port(&mut self, port: u16, value: u8) -> bool {
        self.write_port_at(port, value, 0)
    }

    fn write_control_word(&mut self, value: u8, clocks: u64) {
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
                        counter.latch_count(clocks);
                    }
                    if latch_status {
                        counter.latch_status(clocks);
                    }
                }
            }
        } else {
            self.counters[sc as usize].write_control(value, clocks);
        }
    }

    /// Counter read at the in-batch PIT-clock offset `clocks`: an unlatched read
    /// reports the CE at that instant, a latched one returns the frozen snapshot.
    pub(crate) fn read_port_at(&mut self, port: u16, clocks: u64) -> Option<u8> {
        match port {
            0x40..=0x42 => Some(self.counters[(port - 0x40) as usize].read(clocks)),
            _ => None,
        }
    }

    /// Zero-offset convenience for tests (see `write_port`).
    #[cfg(test)]
    pub(crate) fn read_port(&mut self, port: u16) -> Option<u8> {
        self.read_port_at(port, 0)
    }

    /// Why the whole chip cannot take one analytic advance of `clocks` CLKs.
    ///
    /// Chip-wide, and checked before ANY counter moves: a per-counter decision
    /// would leave a half-advanced chip that the fallback then advanced again.
    fn bulk_decline(&self, clocks: u64) -> Option<BulkDecline> {
        if clocks >= 1u64 << 32 {
            // Past 2^32 CLKs the counting element's own 32-bit wrap stops being
            // one subtraction, and modes 0/1/4/5 would need the wrap-round
            // terminal count the peeks already decline to model. Unreachable in
            // practice: 2^32 PIT CLKs is an hour of guest time in one device
            // advance, and the per-CLK fallback would take four billion
            // iterations to serve it, so this changes nothing that could run.
            return Some(BulkDecline::SpanTooWide);
        }
        self.counters.iter().find_map(Counter::bulk_decline)
    }

    /// One analytic advance of the whole chip. Returns the channel-0 OUT rising
    /// edge count, exactly as the per-CLK loop would.
    fn bulk_advance<F>(
        &mut self,
        clocks: u64,
        watch_channel: Option<usize>,
        out_changed: &mut F,
        counters: &mut crate::PitBulkAdvanceCounters,
    ) -> u32
    where
        F: FnMut(u64, bool),
    {
        // BEFORE anything moves. The transition list is expressed relative to
        // the watched counter's PRE-advance state, and the watched counter is
        // always one of the three advanced below (production watches channel 2).
        if let Some(counter) = watch_channel.and_then(|channel| self.counters.get(channel)) {
            let mut emitted = 0u64;
            counter.out_transitions_in(clocks, &mut |tick, level| {
                emitted += 1;
                out_changed(tick, level);
            });
            counters.transitions += emitted;
        }
        let mut edges = 0u64;
        for (i, counter) in self.counters.iter_mut().enumerate() {
            let rises = counter.advance(clocks);
            if i == 0 {
                edges = rises;
            }
        }
        u32::try_from(edges).unwrap_or(u32::MAX)
    }

    fn tick_with_observer<F>(
        &mut self,
        clocks: u64,
        watch_channel: Option<usize>,
        mut out_changed: F,
        bulk: bool,
        counters: &mut crate::PitBulkAdvanceCounters,
    ) -> u32
    where
        F: FnMut(u64, bool),
    {
        if clocks == 0 {
            // Not an advance at all; the loop below would do nothing either.
            return 0;
        }
        // The arm is read ONCE per device advance, never inside the loop.
        if bulk {
            match self.bulk_decline(clocks) {
                None => {
                    counters.advances += 1;
                    counters.advance_clocks += clocks;
                    return self.bulk_advance(clocks, watch_channel, &mut out_changed, counters);
                }
                Some(BulkDecline::Bcd) => counters.declines_bcd += 1,
                Some(BulkDecline::IllegalReload) => counters.declines_illegal_reload += 1,
                Some(BulkDecline::SpanTooWide) => counters.declines_span_too_wide += 1,
            }
        } else {
            counters.declines_knob_off += 1;
        }
        counters.loop_advances += 1;
        counters.loop_clocks += clocks;
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

    /// Advance every counter by `clocks` input CLK pulses on the PER-CLK LOOP.
    /// Returns the number of channel-0 OUT rising edges, which the machine turns
    /// into IRQ0 requests.
    ///
    /// Deliberately pinned to the loop arm: this is the reference the analytic
    /// advance is differentiated against, and every pre-existing test that drives
    /// the chip through it keeps meaning what it meant.
    #[cfg(test)]
    pub(crate) fn tick(&mut self, clocks: u64) -> u32 {
        let mut counters = crate::PitBulkAdvanceCounters::default();
        self.tick_with_observer(clocks, None, |_, _| {}, false, &mut counters)
    }

    /// `tick` on a STATED arm, for the differential tests.
    #[cfg(test)]
    pub(crate) fn tick_arm(
        &mut self,
        clocks: u64,
        bulk: bool,
        counters: &mut crate::PitBulkAdvanceCounters,
    ) -> u32 {
        self.tick_with_observer(clocks, None, |_, _| {}, bulk, counters)
    }

    /// Advance every counter and append channel OUT transitions with the PIT input
    /// tick on which they occurred. Tick numbers are 1-based within this advance.
    pub(crate) fn tick_recording_out_transitions(
        &mut self,
        clocks: u64,
        channel: usize,
        transitions: &mut Vec<OutTransition>,
        bulk: bool,
        counters: &mut crate::PitBulkAdvanceCounters,
    ) -> u32 {
        self.tick_with_observer(
            clocks,
            Some(channel),
            |tick, level| {
                transitions.push(OutTransition { tick, level });
            },
            bulk,
            counters,
        )
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
    /// without stepping. The lazy port 0x61 bits 4/5 read uses this path.
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
