// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_core::{CanonicalFieldWriter, CanonicalStateError, GswMode, MASTER_CLOCK_HZ};

use crate::timing::PIT_INPUT_HZ;

const MICROSECOND_HZ: u64 = 1_000_000;
const NANOSECOND_HZ: u64 = 1_000_000_000;
pub(crate) const MARGO_FRAME_HZ: u64 = 60;
const DISTIRA_LINE_HZ: u64 = 525 * 60;

const fn saturating_u64(value: u128) -> u64 {
    if value > u64::MAX as u128 {
        u64::MAX
    } else {
        value as u64
    }
}

/// Fractional progress for a device clock measured against the fixed machine
/// timeline. The remainder is always less than [`MASTER_CLOCK_HZ`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RatePhase {
    remainder: u64,
}

impl RatePhase {
    pub(crate) fn with_remainder(remainder: u64) -> Self {
        assert!(
            remainder < MASTER_CLOCK_HZ,
            "rate-phase remainder must be below the master clock"
        );
        Self { remainder }
    }

    pub(crate) const fn remainder(self) -> u64 {
        self.remainder
    }

    /// Advance an integer-Hz device clock by `master_ticks`, returning the whole
    /// device events now due. Splitting one advance into batches gives the same
    /// event total and final remainder.
    ///
    /// The narrow arm is a VALUE-PRESERVING rewrite of the wide one, not an
    /// approximation, and the guard is what makes that true: `checked_mul` +
    /// `checked_add` admit it only when the exact product-plus-remainder fits
    /// `u64`, and inside that domain `u64` and `u128` division agree digit for
    /// digit (the wide quotient is then also below `u64::MAX`, so `saturating_u64`
    /// was a no-op too). It exists because the wide form calls `__udivti3` /
    /// `__umodti3` — twelve device clocks per advance, measured at ~1.6% of gp2's
    /// wall — while the narrow form divides by the CONSTANT `MASTER_CLOCK_HZ` and
    /// the compiler strength-reduces it to a multiply-high with no divide at all.
    /// The guard's domain is not tight: a device clock at `NANOSECOND_HZ`, the
    /// fastest rate this module drives, stays narrow for any advance below
    /// ~18.4 G master ticks (3.3 seconds of machine time in ONE batch).
    pub(crate) fn advance(&mut self, master_ticks: u64, rate_hz: u64) -> u64 {
        if let Some(scaled) = master_ticks.checked_mul(rate_hz)
            && let Some(total) = scaled.checked_add(self.remainder)
        {
            self.remainder = total % MASTER_CLOCK_HZ;
            return total / MASTER_CLOCK_HZ;
        }
        let total = self.remainder as u128 + master_ticks as u128 * rate_hz as u128;
        self.remainder = (total % MASTER_CLOCK_HZ as u128) as u64;
        saturating_u64(total / MASTER_CLOCK_HZ as u128)
    }

    /// Earliest master-tick delta at which `events` device events are due.
    /// Returns `None` for a stopped device clock.
    pub(crate) fn ticks_until(self, events: u64, rate_hz: u64) -> Option<u64> {
        if events == 0 {
            return Some(0);
        }
        if rate_hz == 0 {
            return None;
        }
        // Same value-preserving narrowing as `advance`. The subtraction cannot
        // underflow on this arm: `events >= 1` is established above and the
        // remainder is below `MASTER_CLOCK_HZ` by this type's invariant, so
        // `scaled >= MASTER_CLOCK_HZ > self.remainder`. `u64::div_ceil` of a
        // value that already fits cannot exceed it, so the wide form's
        // saturation had nothing to do here either.
        if let Some(scaled) = events.checked_mul(MASTER_CLOCK_HZ) {
            return Some((scaled - self.remainder).div_ceil(rate_hz));
        }
        let needed = events as u128 * MASTER_CLOCK_HZ as u128 - self.remainder as u128;
        Some(saturating_u64(needed.div_ceil(rate_hz as u128)))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DeviceRates {
    pub dsp_hz: u64,
    pub wss_hz: u64,
    pub cd_playing: bool,
    pub vga_dot_hz: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DeviceAdvance {
    pub master_ticks: u64,
    pub pit_remainder_before: u64,
    pub microseconds: u64,
    pub pit_clocks: u64,
    pub dsp_frames: u64,
    pub wss_frames: u64,
    pub cd_frames: u64,
    pub rtc_seconds: u64,
    pub margo_nanoseconds: u64,
    pub margo_frames: u64,
    pub distira_lines: u64,
    pub vga_dots: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceClock {
    Pit,
    Dsp,
    Wss,
    MargoFrame,
    Rtc,
    Vga,
}

/// The fixed machine timeline and every fractional device-clock phase. Device
/// implementations receive only whole events from this module, so changing CPU
/// mode changes future CPU quanta without changing elapsed time or device phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Timeline {
    now_ticks: u64,
    io_stall_ticks: u64,
    ticks_per_cpu_clock: u64,
    tsc_clocks: u64,
    tsc_phase_ticks: u64,
    microseconds: RatePhase,
    pit: RatePhase,
    dsp: RatePhase,
    wss: RatePhase,
    cd: RatePhase,
    rtc: RatePhase,
    margo: RatePhase,
    margo_frame: RatePhase,
    distira: RatePhase,
    vga: RatePhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CanonicalTimelineError {
    CpuQuantum {
        expected: u64,
        actual: u64,
    },
    PhaseRemainder {
        phase: &'static str,
        remainder: u64,
        limit: u64,
    },
    Totals {
        now_ticks: u64,
        io_stall_ticks: u64,
    },
}

/// Validated timing state whose absolute TSC origin has been projected out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CanonicalTimelineProjection {
    words: [u64; 13],
}

impl CanonicalTimelineProjection {
    pub(crate) fn write_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        for value in self.words {
            out.write_u64(value)?;
        }
        Ok(())
    }
}

impl Timeline {
    pub(crate) fn new(mode: GswMode) -> Self {
        Self {
            now_ticks: 0,
            io_stall_ticks: 0,
            ticks_per_cpu_clock: exact_cpu_quantum(mode),
            tsc_clocks: 0,
            tsc_phase_ticks: 0,
            microseconds: RatePhase::default(),
            pit: RatePhase::default(),
            dsp: RatePhase::default(),
            wss: RatePhase::default(),
            cd: RatePhase::default(),
            rtc: RatePhase::default(),
            margo: RatePhase::default(),
            margo_frame: RatePhase::default(),
            distira: RatePhase::default(),
            vga: RatePhase::default(),
        }
    }

    pub(crate) const fn now_ticks(self) -> u64 {
        self.now_ticks
    }

    pub(crate) const fn io_stall_ticks(self) -> u64 {
        self.io_stall_ticks
    }

    pub(crate) const fn tsc_clocks(self) -> u64 {
        self.tsc_clocks
    }

    pub(crate) fn canonical_projection(
        self,
        mode: GswMode,
    ) -> Result<CanonicalTimelineProjection, CanonicalTimelineError> {
        let expected_quantum = exact_cpu_quantum(mode);
        if self.ticks_per_cpu_clock != expected_quantum {
            return Err(CanonicalTimelineError::CpuQuantum {
                expected: expected_quantum,
                actual: self.ticks_per_cpu_clock,
            });
        }
        if self.tsc_phase_ticks >= self.ticks_per_cpu_clock {
            return Err(CanonicalTimelineError::PhaseRemainder {
                phase: "tsc",
                remainder: self.tsc_phase_ticks,
                limit: self.ticks_per_cpu_clock,
            });
        }
        for (phase, remainder) in [
            ("microseconds", self.microseconds.remainder()),
            ("pit", self.pit.remainder()),
            ("dsp", self.dsp.remainder()),
            ("wss", self.wss.remainder()),
            ("cd", self.cd.remainder()),
            ("rtc", self.rtc.remainder()),
            ("margo", self.margo.remainder()),
            ("margo_frame", self.margo_frame.remainder()),
            ("distira", self.distira.remainder()),
            ("vga", self.vga.remainder()),
        ] {
            if remainder >= MASTER_CLOCK_HZ {
                return Err(CanonicalTimelineError::PhaseRemainder {
                    phase,
                    remainder,
                    limit: MASTER_CLOCK_HZ,
                });
            }
        }
        if self.io_stall_ticks > self.now_ticks {
            return Err(CanonicalTimelineError::Totals {
                now_ticks: self.now_ticks,
                io_stall_ticks: self.io_stall_ticks,
            });
        }
        Ok(CanonicalTimelineProjection {
            words: [
                self.now_ticks,
                self.io_stall_ticks,
                self.tsc_phase_ticks,
                self.microseconds.remainder(),
                self.pit.remainder(),
                self.dsp.remainder(),
                self.wss.remainder(),
                self.cd.remainder(),
                self.rtc.remainder(),
                self.margo.remainder(),
                self.margo_frame.remainder(),
                self.distira.remainder(),
                self.vga.remainder(),
            ],
        })
    }

    #[cfg(test)]
    pub(crate) fn excluding_tsc(mut self) -> Self {
        self.tsc_clocks = 0;
        self.tsc_phase_ticks = 0;
        self
    }

    #[cfg(test)]
    pub(crate) const fn ticks_per_cpu_clock(self) -> u64 {
        self.ticks_per_cpu_clock
    }

    pub(crate) fn set_mode(&mut self, mode: GswMode) {
        self.ticks_per_cpu_clock = exact_cpu_quantum(mode);
        self.tsc_phase_ticks = 0;
    }

    pub(crate) fn master_ticks_for_cpu_clocks(self, cpu_clocks: u64) -> u64 {
        // `saturating_mul` IS the wide form: `saturating_u64(a as u128 * b as u128)`
        // clamps at `u64::MAX` for exactly the products that overflow `u64`, and
        // returns the exact product otherwise.
        cpu_clocks.saturating_mul(self.ticks_per_cpu_clock)
    }

    #[cfg(test)]
    pub(crate) fn cpu_clocks_for_master_ticks_floor(self, master_ticks: u64) -> u64 {
        master_ticks / self.ticks_per_cpu_clock
    }

    /// The batch-cap path calls this several times per batch (`event_batch_cap`
    /// consults every armed device deadline), and the wide form burned a
    /// `__udivti3` per call for no reason: the NUMERATOR here is a plain `u64`,
    /// never a product, so nothing was ever wide. `u64::div_ceil` cannot overflow
    /// for unsigned operands — the quotient is at most `master_ticks` — which is
    /// why the dropped `saturating_u64` clamped nothing.
    pub(crate) fn cpu_clocks_for_master_ticks_ceil(self, master_ticks: u64) -> u64 {
        master_ticks.div_ceil(self.ticks_per_cpu_clock)
    }

    pub(crate) fn advance_cpu_clocks(
        &mut self,
        cpu_clocks: u64,
        rates: DeviceRates,
    ) -> DeviceAdvance {
        self.advance_master_ticks(self.master_ticks_for_cpu_clocks(cpu_clocks), rates)
    }

    pub(crate) fn advance_master_ticks(
        &mut self,
        requested_ticks: u64,
        rates: DeviceRates,
    ) -> DeviceAdvance {
        let master_ticks = requested_ticks.min(u64::MAX - self.now_ticks);
        self.now_ticks += master_ticks;
        let quantum = self.ticks_per_cpu_clock;
        // Narrow TSC accounting, exact on its own domain and wide otherwise. The
        // guard states the two facts the `u64` arithmetic below needs:
        //
        //   * `tsc_phase_ticks < quantum` is this type's phase invariant. Its
        //     writers: `new`, `set_mode` and the test-only `excluding_tsc` store 0,
        //     and this function stores a remainder of `quantum`. Nothing outside the module
        //     can write it, and `canonical_projection` refuses a capture that
        //     violates it (`CanonicalTimelineError::PhaseRemainder`). So
        //     `master_ticks % quantum + tsc_phase_ticks < 2 * quantum`, and one
        //     conditional carry finishes the division;
        //   * `quantum <= MASTER_CLOCK_HZ` bounds that sum to under 2 *
        //     `MASTER_CLOCK_HZ` (~1.1e10), decisively inside `u64`. Every shipped
        //     quantum is 33, 83, 249 or 747 ticks, so this is slack of nine orders
        //     of magnitude, not a near miss.
        //
        // Neither can fail today; the wide arm is the honest way to say so without
        // making an unreachable state silently wrong.
        let (tsc_clocks, phase) = if self.tsc_phase_ticks < quantum && quantum <= MASTER_CLOCK_HZ {
            let clocks = master_ticks / quantum;
            let phase = master_ticks % quantum + self.tsc_phase_ticks;
            if phase >= quantum {
                (clocks + 1, phase - quantum)
            } else {
                (clocks, phase)
            }
        } else {
            let tsc_ticks = self.tsc_phase_ticks as u128 + master_ticks as u128;
            (
                (tsc_ticks / quantum as u128) as u64,
                (tsc_ticks % quantum as u128) as u64,
            )
        };
        self.tsc_phase_ticks = phase;
        self.tsc_clocks = self.tsc_clocks.wrapping_add(tsc_clocks);
        let pit_remainder_before = self.pit.remainder();
        DeviceAdvance {
            master_ticks,
            pit_remainder_before,
            microseconds: self.microseconds.advance(master_ticks, MICROSECOND_HZ),
            pit_clocks: self.pit.advance(master_ticks, u64::from(PIT_INPUT_HZ)),
            dsp_frames: self.dsp.advance(master_ticks, rates.dsp_hz),
            wss_frames: self.wss.advance(master_ticks, rates.wss_hz),
            cd_frames: self
                .cd
                .advance(master_ticks, if rates.cd_playing { 75 } else { 0 }),
            rtc_seconds: self.rtc.advance(master_ticks, 1),
            margo_nanoseconds: self.margo.advance(master_ticks, NANOSECOND_HZ),
            margo_frames: self.margo_frame.advance(master_ticks, MARGO_FRAME_HZ),
            distira_lines: self.distira.advance(master_ticks, DISTIRA_LINE_HZ),
            vga_dots: self.vga.advance(master_ticks, rates.vga_dot_hz),
        }
    }

    pub(crate) fn advance_io_stall_ticks(
        &mut self,
        master_ticks: u64,
        rates: DeviceRates,
    ) -> DeviceAdvance {
        let advance = self.advance_master_ticks(master_ticks, rates);
        // `saturating_add` is the same clamp the wide sum plus `saturating_u64` was.
        self.io_stall_ticks = self.io_stall_ticks.saturating_add(advance.master_ticks);
        advance
    }

    pub(crate) fn preview_cpu_clocks(self, cpu_clocks: u64, vga_dot_hz: u64) -> (u64, u64) {
        let ticks = self.master_ticks_for_cpu_clocks(cpu_clocks);
        let mut pit = self.pit;
        let mut vga = self.vga;
        (
            pit.advance(ticks, u64::from(PIT_INPUT_HZ)),
            vga.advance(ticks, vga_dot_hz),
        )
    }

    /// Microseconds a `cpu_clocks` advance WOULD produce, without performing it.
    ///
    /// Same arithmetic as the `microseconds` field of `advance_master_ticks`,
    /// run on a copy of the phase accumulator, so a peek and the real advance
    /// that follows it cannot disagree. `preview_cpu_clocks` is the precedent.
    pub(crate) fn preview_microseconds(self, cpu_clocks: u64) -> u64 {
        let ticks = self.master_ticks_for_cpu_clocks(cpu_clocks);
        let mut microseconds = self.microseconds;
        microseconds.advance(ticks, MICROSECOND_HZ)
    }

    pub(crate) fn cpu_clocks_until(
        self,
        clock: DeviceClock,
        events: u64,
        rate_hz: u64,
    ) -> Option<u64> {
        self.master_ticks_until(clock, events, rate_hz)
            .map(|ticks| self.cpu_clocks_for_master_ticks_ceil(ticks).max(1))
    }

    pub(crate) fn master_ticks_until(
        self,
        clock: DeviceClock,
        events: u64,
        rate_hz: u64,
    ) -> Option<u64> {
        let phase = match clock {
            DeviceClock::Pit => self.pit,
            DeviceClock::Dsp => self.dsp,
            DeviceClock::Wss => self.wss,
            DeviceClock::MargoFrame => self.margo_frame,
            DeviceClock::Rtc => self.rtc,
            DeviceClock::Vga => self.vga,
        };
        phase.ticks_until(events, rate_hz)
    }
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new(GswMode::default())
    }
}

fn exact_cpu_quantum(mode: GswMode) -> u64 {
    mode.clock_rate()
        .master_ticks_per_clock()
        .expect("every GSW CPU rate must divide the master clock exactly")
}

#[cfg(test)]
#[path = "timeline_test.rs"]
mod tests;
