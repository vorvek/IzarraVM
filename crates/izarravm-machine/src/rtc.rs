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

/// First and last NVRAM byte the checksum covers (the Izarra general area).
const NVRAM_CHECKSUM_LO: usize = 0x10;
const NVRAM_CHECKSUM_HI: usize = 0x2d;
/// Where the 16-bit checksum is stored (high byte then low byte, AT order).
const NVRAM_CHECKSUM_HIGH: usize = 0x2e;
const NVRAM_CHECKSUM_LOW: usize = 0x2f;

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
            REG_C | REG_D => { /* status C and D are read-only */ }
            _ => {
                self.ram[usize::from(reg)] = value;
                self.nvram_dirty = true;
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
    /// the saved time. A bad NVRAM checksum is repaired in place (the bytes are
    /// kept but the stored checksum is refreshed) and `false` is returned, so the
    /// caller can log that the file was inconsistent.
    pub fn load_nvram(&mut self, bytes: &[u8; 64]) -> bool {
        self.ram = *bytes;
        // Keep the status registers sane regardless of the file: force binary
        // 24-hour mode and VRT so the BIOS reads a known format.
        self.ram[usize::from(REG_B)] |= 0x06;
        self.ram[usize::from(REG_D)] |= 0x80;
        self.read_time_registers();
        let valid = self.checksum_valid();
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

    /// Return whether the guest wrote NVRAM since the last call, clearing the
    /// flag. The host polls this to flush cmos.bin only when something changed.
    pub fn take_nvram_dirty(&mut self) -> bool {
        std::mem::take(&mut self.nvram_dirty)
    }

    /// Copy the broken-down time into the register bytes (binary, 24-hour).
    fn write_time_registers(&mut self) {
        self.ram[usize::from(REG_SECONDS)] = self.time.second;
        self.ram[usize::from(REG_MINUTES)] = self.time.minute;
        self.ram[usize::from(REG_HOURS)] = self.time.hour;
        self.ram[usize::from(REG_WEEKDAY)] = self.time.weekday;
        self.ram[usize::from(REG_DAY)] = self.time.day;
        self.ram[usize::from(REG_MONTH)] = self.time.month;
        self.ram[usize::from(REG_YEAR)] = (self.time.year % 100) as u8;
    }

    /// Re-derive the broken-down time from the register bytes after a reload.
    /// The century is assumed to be 2000 since this is a 1990s-era fantasy
    /// machine running in the 2000s; a two-digit year of 90+ maps to 19xx so a
    /// game that writes 95 still reads back as 1995.
    fn read_time_registers(&mut self) {
        let yy = u16::from(self.ram[usize::from(REG_YEAR)] % 100);
        let year = if yy >= 90 { 1900 + yy } else { 2000 + yy };
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
mod tests {
    use super::*;

    #[test]
    fn rtc_register_round_trip() {
        let mut r = Rtc::new();
        r.write_port(0x70, 0x00); // select seconds
        r.write_port(0x71, 30);
        r.write_port(0x70, 0x00);
        assert_eq!(r.read_port(0x71), Some(30));
    }

    #[test]
    fn rtc_seconds_advance_and_carry() {
        let mut r = Rtc::new();
        r.seed(2026, 6, 20, 6, 23, 59, 58);
        r.tick_seconds(3);
        r.write_port(0x70, 0x00);
        assert_eq!(r.read_port(0x71), Some(1)); // 58 -> 01
        r.write_port(0x70, 0x02);
        assert_eq!(r.read_port(0x71), Some(0)); // minutes 59 -> 00
        r.write_port(0x70, 0x04);
        assert_eq!(r.read_port(0x71), Some(0)); // hours 23 -> 00
    }

    #[test]
    fn day_carries_across_month_boundary() {
        let mut r = Rtc::new();
        // 30 June 23:59:59, plus 2 seconds -> 1 July 00:00:01.
        r.seed(2026, 6, 30, 3, 23, 59, 59);
        r.tick_seconds(2);
        r.write_port(0x70, REG_DAY);
        assert_eq!(r.read_port(0x71), Some(1));
        r.write_port(0x70, REG_MONTH);
        assert_eq!(r.read_port(0x71), Some(7));
        r.write_port(0x70, REG_HOURS);
        assert_eq!(r.read_port(0x71), Some(0));
    }

    #[test]
    fn leap_day_is_honored() {
        let mut r = Rtc::new();
        // 28 Feb 2024 (leap) 23:59:59 + 1s -> 29 Feb.
        r.seed(2024, 2, 28, 4, 23, 59, 59);
        r.tick_seconds(1);
        r.write_port(0x70, REG_DAY);
        assert_eq!(r.read_port(0x71), Some(29));
        r.write_port(0x70, REG_MONTH);
        assert_eq!(r.read_port(0x71), Some(2));
    }

    #[test]
    fn cmos_checksum_round_trips_via_bytes() {
        let mut r = Rtc::new();
        r.set_nvram(0x10, 3); // FR layout
        r.refresh_checksum();
        let saved = r.nvram();
        let mut r2 = Rtc::new();
        r2.load_nvram(&saved);
        assert_eq!(r2.nvram_byte(0x10), 3);
        assert!(r2.checksum_valid());
    }

    #[test]
    fn bad_checksum_is_detected() {
        let mut r = Rtc::new();
        r.set_nvram(0x11, 1);
        // No refresh: the stored checksum is now stale.
        assert!(!r.checksum_valid());
    }

    #[test]
    fn register_b_reports_binary_24h() {
        let mut r = Rtc::new();
        r.write_port(0x70, REG_B);
        let b = r.read_port(0x71).unwrap();
        assert_ne!(b & 0x04, 0); // DM = 1 (binary)
        assert_ne!(b & 0x02, 0); // 24/12 = 1 (24-hour)
    }

    #[test]
    fn register_d_reports_vrt() {
        let mut r = Rtc::new();
        r.write_port(0x70, REG_D);
        assert_eq!(r.read_port(0x71).unwrap() & 0x80, 0x80);
    }

    #[test]
    fn index_port_round_trips_nmi_bit() {
        let mut r = Rtc::new();
        r.write_port(0x70, 0x80 | 0x0a); // NMI disabled, index = Reg A
        assert_eq!(r.read_port(0x70), Some(0x8a));
    }

    #[test]
    fn year_write_keeps_century() {
        let mut r = Rtc::new();
        r.seed(2026, 6, 20, 6, 12, 0, 0);
        r.write_port(0x70, REG_YEAR);
        r.write_port(0x71, 30); // guest writes "30"
        r.write_port(0x70, REG_YEAR);
        assert_eq!(r.read_port(0x71), Some(30));
    }
}
