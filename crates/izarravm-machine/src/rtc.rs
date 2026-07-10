// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! MC146818 real-time clock and CMOS NVRAM.
//!
//! The chip exposes two I/O ports: 0x70 selects a register (low 7 bits) and
//! holds the NMI-disable flag in bit 7; 0x71 reads or writes the selected
//! register. Registers 0x00..0x0D are the clock and four status bytes; 0x0E..
//! 0x3F are general-purpose battery-backed RAM.
//!
//! The Izarra 3000 keeps the clock in binary, 24-hour format (Register B bits
//! DM=1 and 24/12=1) so the BIOS ASM does not have to unpack BCD. The host
//! seeds the time once at startup and the device self-advances on the machine
//! clock; there is no live host resync.

use crate::timeline::RatePhase;

/// Register index of the seconds byte; the rest follow the standard offsets.
const REG_SECONDS: u8 = 0x00;
const REG_MINUTES: u8 = 0x02;
const REG_HOURS: u8 = 0x04;
const REG_WEEKDAY: u8 = 0x06;
const REG_DAY: u8 = 0x07;
const REG_MONTH: u8 = 0x08;
const REG_YEAR: u8 = 0x09;
const REG_A: u8 = 0x0a;
const REG_B: u8 = 0x0b;
const REG_C: u8 = 0x0c;
const REG_D: u8 = 0x0d;

/// Register A power-on value: 32768 Hz time base, 1024 Hz rate (UIP clear).
const REG_A_DEFAULT: u8 = 0x26;
/// Register B power-on value: binary data mode (DM, bit 2) and 24-hour mode
/// (bit 1). All interrupt enables clear, no DST.
const REG_B_DEFAULT: u8 = 0x06;
/// Register D power-on value: VRT set (bit 7), meaning the battery is good.
const REG_D_DEFAULT: u8 = 0x80;

/// Register B interrupt-enable bits (MC146818 datasheet). PIE gates the
/// periodic interrupt at the Register A rate, AIE the once-per-day alarm, and
/// UIE the once-per-second update-ended interrupt.
const REG_B_PIE: u8 = 0x40; // bit 6: periodic interrupt enable
const REG_B_AIE: u8 = 0x20; // bit 5: alarm interrupt enable
const REG_B_UIE: u8 = 0x10; // bit 4: update-ended interrupt enable

/// Register C flag bits. IRQF is the wire-OR of the three sources gated by
/// their enables; PF/AF/UF are the raw sources. A read of Register C returns
/// these and clears all four (see `read_data`).
const REG_C_IRQF: u8 = 0x80; // bit 7: interrupt request (any enabled source)
const REG_C_PF: u8 = 0x40; // bit 6: periodic flag
const REG_C_AF: u8 = 0x20; // bit 5: alarm flag
const REG_C_UF: u8 = 0x10; // bit 4: update-ended flag

/// Alarm-match registers (the seconds/minutes/hours the AIE compares against).
const REG_SECONDS_ALARM: u8 = 0x01;
const REG_MINUTES_ALARM: u8 = 0x03;
const REG_HOURS_ALARM: u8 = 0x05;

/// First and last NVRAM byte the checksum covers (the Izarra general area).
const NVRAM_CHECKSUM_LO: usize = 0x10;
const NVRAM_CHECKSUM_HI: usize = 0x2d;
/// Where the 16-bit checksum is stored (high byte then low byte, AT order).
const NVRAM_CHECKSUM_HIGH: usize = 0x2e;
const NVRAM_CHECKSUM_LOW: usize = 0x2f;

/// CMOS diagnostic status byte (the AT's "shutdown reason / POST status" slot).
/// Bit 6 flags a bad NVRAM checksum, bit 7 flags lost RTC power.
const REG_DIAGNOSTIC: usize = 0x0e;
const DIAG_BAD_CHECKSUM: u8 = 0x40; // bit 6
const DIAG_POWER_LOST: u8 = 0x80; // bit 7

/// CMOS century byte (packed BCD), the AT/PS-2 convention. The Izarra defaults
/// to century 20 (the machine runs in the 2000s).
const REG_CENTURY: usize = 0x32;
/// PS/2 alternate century slot. Some BIOSes mirror 0x32 here, so keep them in
/// step. Both sit outside the 0x10..=0x2D checksum range.
const REG_CENTURY_ALT: usize = 0x37;
/// Default century in packed BCD (20 -> 0x20).
const CENTURY_DEFAULT: u8 = 0x20;

/// Convert a binary value 0..=99 to packed BCD.
fn bin_to_bcd(n: u8) -> u8 {
    ((n / 10) << 4) | (n % 10)
}

/// Convert packed BCD back to binary.
fn bcd_to_bin(n: u8) -> u8 {
    (n >> 4) * 10 + (n & 0x0f)
}

/// Days in each month for a non-leap year, indexed 1..=12.
const DAYS_IN_MONTH: [u8; 13] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

fn is_leap_year(year: u16) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: u16, month: u8) -> u8 {
    if month == 2 && is_leap_year(year) {
        29
    } else {
        DAYS_IN_MONTH[usize::from(month)]
    }
}

/// MC146818 RTC plus 64 bytes of CMOS RAM.
#[derive(Debug, Clone)]
pub struct Rtc {
    /// The 64 register/RAM bytes. Indices 0x00..0x0D mirror the clock fields
    /// kept in `time`; the rest are battery-backed RAM.
    ram: [u8; 64],
    /// Selected register, latched by a write to port 0x70 (low 7 bits).
    index: u8,
    /// NMI-disable flag, the high bit of the last write to port 0x70. Tracked
    /// so a read of 0x70 round-trips it; the device takes no action on it.
    nmi_disabled: bool,
    /// Broken-down local time the clock advances.
    time: Time,
    /// Whether the clock has been seeded from the host yet.
    seeded: bool,
    /// Set when the guest writes an NVRAM byte (index 0x0E or above), so the
    /// host can flush cmos.bin. Cleared by `take_nvram_dirty`.
    nvram_dirty: bool,
    /// Phase of the programmable periodic divider against the master timeline.
    periodic_phase: RatePhase,
}

#[derive(Debug, Clone, Copy, Default)]
struct Time {
    year: u16,   // full year, e.g. 2026
    month: u8,   // 1..=12
    day: u8,     // 1..=31
    weekday: u8, // 1..=7, 1 = Sunday (AT convention)
    hour: u8,    // 0..=23
    minute: u8,  // 0..=59
    second: u8,  // 0..=59
}

impl Default for Rtc {
    fn default() -> Self {
        Self::new()
    }
}

impl Rtc {
    /// A fresh device: clock at the epoch start until seeded, status registers
    /// at their power-on values, and a defaulted NVRAM area with a valid
    /// checksum.
    pub fn new() -> Self {
        let mut ram = [0u8; 64];
        ram[usize::from(REG_A)] = REG_A_DEFAULT;
        ram[usize::from(REG_B)] = REG_B_DEFAULT;
        ram[usize::from(REG_C)] = 0x00;
        ram[usize::from(REG_D)] = REG_D_DEFAULT;
        ram[REG_CENTURY] = CENTURY_DEFAULT;
        ram[REG_CENTURY_ALT] = CENTURY_DEFAULT;
        let mut rtc = Self {
            ram,
            index: 0,
            nmi_disabled: false,
            time: Time {
                year: 2026,
                month: 1,
                day: 1,
                weekday: 1,
                hour: 0,
                minute: 0,
                second: 0,
            },
            seeded: false,
            nvram_dirty: false,
            periodic_phase: RatePhase::default(),
        };
        rtc.write_time_registers();
        rtc.refresh_checksum();
        rtc
    }

    /// Seed the clock from host-provided fields. `weekday` is 1..=7 with
    /// 1 = Sunday; values outside the valid ranges are clamped so a bad host
    /// reading cannot poison the registers.
    #[allow(clippy::too_many_arguments)]
    pub fn seed(
        &mut self,
        year: u16,
        month: u8,
        day: u8,
        weekday: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) {
        self.time = Time {
            year,
            month: month.clamp(1, 12),
            day: day.clamp(1, 31),
            weekday: weekday.clamp(1, 7),
            hour: hour.min(23),
            minute: minute.min(59),
            second: second.min(59),
        };
        self.seeded = true;
        self.write_time_registers();
    }

    /// Advance the clock by `n` whole seconds, carrying into minutes, hours,
    /// days, months, and years as needed. The status and NVRAM registers are
    /// untouched.
    pub fn tick_seconds(&mut self, n: u64) {
        if n == 0 {
            return;
        }
        // Carry seconds into minutes, hours, and days in bulk. Callers pass
        // small counts per machine step, but bulk arithmetic also handles a
        // large jump (a paused VM resuming) without a per-second loop.
        let second_total = u64::from(self.time.second) + n;
        self.time.second = (second_total % 60) as u8;

        let minute_total = u64::from(self.time.minute) + second_total / 60;
        self.time.minute = (minute_total % 60) as u8;

        let hour_total = u64::from(self.time.hour) + minute_total / 60;
        self.time.hour = (hour_total % 24) as u8;

        let day_carry = hour_total / 24;
        // Weekday advances by the whole-day delta, mod 7 (1 = Sunday).
        if day_carry > 0 {
            let steps = (day_carry % 7) as u8;
            self.time.weekday =
                ((u16::from(self.time.weekday) - 1 + u16::from(steps)) % 7 + 1) as u8;
        }
        // Roll the calendar date one month at a time until the carried days fit
        // in the current month.
        let mut remaining_days = day_carry;
        while remaining_days > 0 {
            let dim = u64::from(days_in_month(self.time.year, self.time.month));
            let day_total = u64::from(self.time.day) + remaining_days;
            if day_total <= dim {
                self.time.day = day_total as u8;
                remaining_days = 0;
            } else {
                // Consume the rest of this month, then roll to the next.
                remaining_days -= dim - u64::from(self.time.day) + 1;
                self.time.day = 1;
                if self.time.month == 12 {
                    self.time.month = 1;
                    self.time.year = self.time.year.wrapping_add(1);
                } else {
                    self.time.month += 1;
                }
            }
        }
        self.write_time_registers();
    }

    /// Advance the clock and programmable divider on the master timeline.
    /// Returns true only when IRQF changes from clear to set.
    pub fn advance_master_ticks(&mut self, master_ticks: u64, elapsed_seconds: u64) -> bool {
        let alarm_due = elapsed_seconds > 0
            && self
                .seconds_until_alarm()
                .is_some_and(|seconds| seconds <= elapsed_seconds);
        self.tick_seconds(elapsed_seconds);

        let mut flags = 0;
        if self
            .periodic_phase
            .advance(master_ticks, self.periodic_rate_hz())
            > 0
        {
            flags |= REG_C_PF;
        }
        if elapsed_seconds > 0 {
            flags |= REG_C_UF;
        }
        if alarm_due {
            flags |= REG_C_AF;
        }
        self.latch_interrupt_flags(flags)
    }

    fn latch_interrupt_flags(&mut self, flags: u8) -> bool {
        if flags == 0 {
            return false;
        }
        let was_pending = self.ram[usize::from(REG_C)] & REG_C_IRQF != 0;
        self.ram[usize::from(REG_C)] |= flags;
        let enables = self.ram[usize::from(REG_B)];
        let enabled = (flags & REG_C_PF != 0 && enables & REG_B_PIE != 0)
            || (flags & REG_C_AF != 0 && enables & REG_B_AIE != 0)
            || (flags & REG_C_UF != 0 && enables & REG_B_UIE != 0);
        if enabled {
            self.ram[usize::from(REG_C)] |= REG_C_IRQF;
        }
        enabled && !was_pending
    }

    /// Programmed periodic rate. Divider value 010 selects the normal 32.768 kHz
    /// time base; the two legacy aliases and standard rates match MC146818.
    pub(crate) fn periodic_rate_hz(&self) -> u64 {
        if self.ram[usize::from(REG_A)] & 0x70 != 0x20 {
            return 0;
        }
        match self.ram[usize::from(REG_A)] & 0x0f {
            0 => 0,
            1 | 8 => 256,
            2 | 9 => 128,
            rate => 32_768 >> (rate - 1),
        }
    }

    pub(crate) fn ticks_until_periodic_irq(&self) -> Option<u64> {
        if self.ram[usize::from(REG_C)] & REG_C_IRQF != 0
            || self.ram[usize::from(REG_B)] & REG_B_PIE == 0
        {
            return None;
        }
        self.periodic_phase.ticks_until(1, self.periodic_rate_hz())
    }

    /// Whole one-second update events until an enabled update or alarm source
    /// can assert IRQ8. Periodic timing is returned separately in master ticks.
    pub(crate) fn seconds_until_irq(&self) -> Option<u64> {
        if self.ram[usize::from(REG_C)] & REG_C_IRQF != 0 {
            return None;
        }
        let enables = self.ram[usize::from(REG_B)];
        let update = (enables & REG_B_UIE != 0).then_some(1);
        let alarm = (enables & REG_B_AIE != 0)
            .then(|| self.seconds_until_alarm())
            .flatten();
        update.into_iter().chain(alarm).min()
    }

    fn seconds_until_alarm(&self) -> Option<u64> {
        let now = u64::from(self.time.hour) * 3600
            + u64::from(self.time.minute) * 60
            + u64::from(self.time.second);
        (1..=86_400).find(|delta| {
            let then = (now + delta) % 86_400;
            self.alarm_matches_time(
                (then / 3600) as u8,
                ((then / 60) % 60) as u8,
                (then % 60) as u8,
            )
        })
    }

    fn alarm_matches_time(&self, hour: u8, minute: u8, second: u8) -> bool {
        let matches = |alarm: u8, now: u8| alarm >= 0xc0 || alarm == now;
        matches(self.ram[usize::from(REG_SECONDS_ALARM)], second)
            && matches(self.ram[usize::from(REG_MINUTES_ALARM)], minute)
            && matches(self.ram[usize::from(REG_HOURS_ALARM)], hour)
    }

    /// Read the byte the index port currently selects. Status and clock reads
    /// return the live values; reading Register C clears its interrupt flags.
    pub fn read_data(&mut self) -> u8 {
        let idx = usize::from(self.index & 0x3f);
        let value = self.ram[idx];
        if self.index & 0x7f == REG_C {
            // Reading Register C clears the interrupt-request flags.
            self.ram[usize::from(REG_C)] = 0;
        }
        value
    }

    /// Write the byte the index port currently selects. Writes to the clock
    /// fields update the broken-down time; writes to NVRAM land in RAM.
    pub fn write_data(&mut self, value: u8) {
        let reg = self.index & 0x7f;
        match reg {
            REG_SECONDS => self.time.second = value.min(59),
            REG_MINUTES => self.time.minute = value.min(59),
            REG_HOURS => self.time.hour = value.min(23),
            REG_WEEKDAY => self.time.weekday = value.clamp(1, 7),
            REG_DAY => self.time.day = value.clamp(1, 31),
            REG_MONTH => self.time.month = value.clamp(1, 12),
            REG_YEAR => {
                // Two-digit year register: keep the century from the current
                // clock so a guest writing 26 means 2026, not 0026.
                let century = (self.time.year / 100) * 100;
                self.time.year = century + u16::from(value % 100);
            }
            // Alarm-match registers: stored but not part of the broken-down clock
            // and not battery-backed NVRAM, so they bypass the dirty flag.
            REG_SECONDS_ALARM | REG_MINUTES_ALARM | REG_HOURS_ALARM => {
                self.ram[usize::from(reg)] = value;
            }
            // Register A: the OS programs the rate-select and time-base bits here.
            // UIP (bit 7) is read-only and always reads 0 on this device (no
            // update cycle is modeled), so mask it off on write.
            REG_A => {
                let value = value & 0x7f;
                if self.ram[usize::from(REG_A)] != value {
                    self.periodic_phase = RatePhase::default();
                }
                self.ram[usize::from(REG_A)] = value;
            }
            // Register B: interrupt enables and format bits. The format bits stay
            // forced (binary, 24-hour) so the BIOS format never changes underfoot,
            // but the enable bits the guest sets drive interrupt generation.
            REG_B => self.ram[usize::from(REG_B)] = value | REG_B_DEFAULT,
            REG_C | REG_D => { /* status C and D are read-only */ }
            _ => {
                self.ram[usize::from(reg)] = value;
                self.nvram_dirty = true;
                if usize::from(reg) == REG_CENTURY {
                    // A direct century write moves the clock with it: keep the
                    // alternate slot in step and re-derive the full year.
                    self.ram[REG_CENTURY_ALT] = value;
                    self.read_time_registers();
                }
            }
        }
        if reg <= REG_YEAR {
            self.write_time_registers();
        }
    }

    /// Port read: 0x70 returns the index plus NMI flag, 0x71 returns the
    /// selected register. Returns None for any other port so the bus dispatch
    /// can fall through.
    pub fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            0x70 => Some((self.index & 0x7f) | (u8::from(self.nmi_disabled) << 7)),
            0x71 => Some(self.read_data()),
            _ => None,
        }
    }

    /// Port write: 0x70 latches the index and NMI flag, 0x71 writes the
    /// selected register. Returns true when the port was handled.
    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        match port {
            0x70 => {
                self.index = value & 0x7f;
                self.nmi_disabled = value & 0x80 != 0;
                true
            }
            0x71 => {
                self.write_data(value);
                true
            }
            _ => false,
        }
    }

    /// The full 64-byte CMOS image (clock registers plus NVRAM) for persistence.
    pub fn nvram(&self) -> [u8; 64] {
        self.ram
    }

    /// One NVRAM byte by index.
    pub fn nvram_byte(&self, index: usize) -> u8 {
        self.ram.get(index).copied().unwrap_or(0)
    }

    /// Set one NVRAM byte by index. Out-of-range indices are ignored.
    pub fn set_nvram(&mut self, index: usize, value: u8) {
        if let Some(slot) = self.ram.get_mut(index) {
            *slot = value;
        }
    }

    /// Replace the whole CMOS image from a persisted file. The clock fields are
    /// re-derived from the loaded registers so a reload restores both NVRAM and
    /// the saved time.
    ///
    /// A bad NVRAM checksum is recorded before it is repaired: diagnostic byte
    /// 0x0E gets bit 6 (incorrect-checksum) set so a guest can detect a tampered
    /// or corrupt image. Bit 7 (RTC power lost) follows Register D's VRT bit: a
    /// cleared VRT in the file means the battery died. The stored checksum is
    /// then refreshed in place (the data bytes are kept) and `false` is
    /// returned, so the caller can log that the file was inconsistent.
    pub fn load_nvram(&mut self, bytes: &[u8; 64]) -> bool {
        self.ram = *bytes;
        self.periodic_phase = RatePhase::default();
        // A century byte of 0 means an older image without one; fall back to the
        // default so the year does not resolve to year 0.
        if self.ram[REG_CENTURY] == 0 {
            self.ram[REG_CENTURY] = CENTURY_DEFAULT;
            self.ram[REG_CENTURY_ALT] = CENTURY_DEFAULT;
        }
        // Record power-loss from the loaded Register D before we force VRT on.
        let power_lost = self.ram[usize::from(REG_D)] & 0x80 == 0;
        // Keep the status registers sane regardless of the file: force binary
        // 24-hour mode and VRT so the BIOS reads a known format.
        self.ram[usize::from(REG_B)] |= 0x06;
        self.ram[usize::from(REG_D)] |= 0x80;
        self.read_time_registers();
        let valid = self.checksum_valid();
        // Stamp the diagnostic byte before repairing so a guest can still see
        // that the image was inconsistent.
        let mut diag = 0u8;
        if !valid {
            diag |= DIAG_BAD_CHECKSUM;
        }
        if power_lost {
            diag |= DIAG_POWER_LOST;
        }
        self.ram[REG_DIAGNOSTIC] = diag;
        if !valid {
            self.refresh_checksum();
        }
        valid
    }

    /// 16-bit checksum of NVRAM bytes 0x10..=0x2D, as stored at 0x2E/0x2F.
    pub fn checksum(&self) -> u16 {
        let mut sum: u16 = 0;
        for byte in &self.ram[NVRAM_CHECKSUM_LO..=NVRAM_CHECKSUM_HI] {
            sum = sum.wrapping_add(u16::from(*byte));
        }
        sum
    }

    /// Recompute and store the NVRAM checksum at 0x2E (high) and 0x2F (low).
    pub fn refresh_checksum(&mut self) {
        let sum = self.checksum();
        self.ram[NVRAM_CHECKSUM_HIGH] = (sum >> 8) as u8;
        self.ram[NVRAM_CHECKSUM_LOW] = (sum & 0xff) as u8;
    }

    /// Whether the stored checksum matches the current NVRAM contents.
    pub fn checksum_valid(&self) -> bool {
        let stored = (u16::from(self.ram[NVRAM_CHECKSUM_HIGH]) << 8)
            | u16::from(self.ram[NVRAM_CHECKSUM_LOW]);
        stored == self.checksum()
    }

    /// Whether the clock has been seeded from the host.
    pub fn is_seeded(&self) -> bool {
        self.seeded
    }

    /// Broken-down local time as (year, month, day, weekday, hour, minute,
    /// second). The values are binary; INT 1Ah converts them to BCD.
    pub fn clock(&self) -> (u16, u8, u8, u8, u8, u8, u8) {
        let t = self.time;
        (
            t.year, t.month, t.day, t.weekday, t.hour, t.minute, t.second,
        )
    }

    /// The century stored in CMOS byte 0x32, as a binary number (e.g. 20). The
    /// INT 1Ah AH=04h handler reads this to report the full date in BCD.
    pub fn century(&self) -> u8 {
        bcd_to_bin(self.ram[REG_CENTURY])
    }

    /// Set the century (binary, e.g. 19 or 20) into CMOS byte 0x32 as packed
    /// BCD, mirror it to the PS/2 alternate slot 0x37, and roll the clock's full
    /// year to match. Both slots sit outside the checksum range, so this does
    /// not disturb the NVRAM checksum.
    pub fn set_century(&mut self, century: u8) {
        let bcd = bin_to_bcd(century);
        self.ram[REG_CENTURY] = bcd;
        self.ram[REG_CENTURY_ALT] = bcd;
        self.read_time_registers();
    }

    /// Return whether the guest wrote NVRAM since the last call, clearing the
    /// flag. The host polls this to flush cmos.bin only when something changed.
    pub fn take_nvram_dirty(&mut self) -> bool {
        std::mem::take(&mut self.nvram_dirty)
    }

    /// Copy the broken-down time into the register bytes (binary, 24-hour).
    /// The two-digit year register tracks `year % 100`; the century byte at
    /// 0x32 (and its PS/2 mirror at 0x37) carries the rest in packed BCD so a
    /// reload reconstructs the full year.
    fn write_time_registers(&mut self) {
        self.ram[usize::from(REG_SECONDS)] = self.time.second;
        self.ram[usize::from(REG_MINUTES)] = self.time.minute;
        self.ram[usize::from(REG_HOURS)] = self.time.hour;
        self.ram[usize::from(REG_WEEKDAY)] = self.time.weekday;
        self.ram[usize::from(REG_DAY)] = self.time.day;
        self.ram[usize::from(REG_MONTH)] = self.time.month;
        self.ram[usize::from(REG_YEAR)] = (self.time.year % 100) as u8;
        let century = bin_to_bcd((self.time.year / 100) as u8);
        self.ram[REG_CENTURY] = century;
        self.ram[REG_CENTURY_ALT] = century;
    }

    /// Re-derive the broken-down time from the register bytes after a reload.
    /// The full year is the century byte at 0x32 (packed BCD) times 100 plus the
    /// two-digit year register, so a saved 0x19 century reads back as 19xx and
    /// the default 0x20 as 20xx.
    fn read_time_registers(&mut self) {
        let yy = u16::from(self.ram[usize::from(REG_YEAR)] % 100);
        let century = u16::from(bcd_to_bin(self.ram[REG_CENTURY]));
        let year = century * 100 + yy;
        self.time = Time {
            year,
            month: self.ram[usize::from(REG_MONTH)].clamp(1, 12),
            day: self.ram[usize::from(REG_DAY)].clamp(1, 31),
            weekday: self.ram[usize::from(REG_WEEKDAY)].clamp(1, 7),
            hour: self.ram[usize::from(REG_HOURS)].min(23),
            minute: self.ram[usize::from(REG_MINUTES)].min(59),
            second: self.ram[usize::from(REG_SECONDS)].min(59),
        };
    }
}

#[cfg(test)]
#[path = "rtc_test.rs"]
mod tests;
