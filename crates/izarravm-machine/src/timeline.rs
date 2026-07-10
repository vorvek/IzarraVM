// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_core::{GswMode, MASTER_CLOCK_HZ};

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
    pub(crate) fn advance(&mut self, master_ticks: u64, rate_hz: u64) -> u64 {
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

impl Timeline {
    pub(crate) fn new(mode: GswMode) -> Self {
        Self {
            now_ticks: 0,
            io_stall_ticks: 0,
            ticks_per_cpu_clock: exact_cpu_quantum(mode),
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

    #[cfg(test)]
    pub(crate) const fn ticks_per_cpu_clock(self) -> u64 {
        self.ticks_per_cpu_clock
    }

    pub(crate) fn set_mode(&mut self, mode: GswMode) {
        self.ticks_per_cpu_clock = exact_cpu_quantum(mode);
    }

    pub(crate) fn master_ticks_for_cpu_clocks(self, cpu_clocks: u64) -> u64 {
        saturating_u64(cpu_clocks as u128 * self.ticks_per_cpu_clock as u128)
    }

    #[cfg(test)]
    pub(crate) fn cpu_clocks_for_master_ticks_floor(self, master_ticks: u64) -> u64 {
        master_ticks / self.ticks_per_cpu_clock
    }

    pub(crate) fn cpu_clocks_for_master_ticks_ceil(self, master_ticks: u64) -> u64 {
        saturating_u64((master_ticks as u128).div_ceil(self.ticks_per_cpu_clock as u128))
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
        self.io_stall_ticks =
            saturating_u64(self.io_stall_ticks as u128 + advance.master_ticks as u128);
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
