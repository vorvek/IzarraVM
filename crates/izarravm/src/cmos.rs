//! Host side of the RTC/CMOS: seed the clock from host local time at startup
//! and persist the 64-byte NVRAM image to `cmos.bin` next to `izarravm.conf`.
//!
//! The local-time read uses the `time` crate's `now_local()`, which is sound
//! only when called before any extra threads are spawned. Startup runs this on
//! the main thread before the emulation thread starts, so the read is safe; if
//! the host refuses a local offset, we fall back to UTC and log it.

use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use tracing::warn;

/// Broken-down local time fields for seeding the RTC. `weekday` is 1..=7 with
/// 1 = Sunday, matching the AT convention the device expects.
#[derive(Debug, Clone, Copy)]
pub struct SeedTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// Read host local time once. Falls back to UTC if the platform refuses a local
/// UTC offset (the `time` crate guards this when other threads may exist). Call
/// this on the main thread at startup, before spawning the emulation thread.
pub fn read_host_time() -> SeedTime {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| {
        warn!("host local time unavailable; seeding RTC from UTC");
        OffsetDateTime::now_utc()
    });
    from_offset(now)
}

/// Convert an `OffsetDateTime` into the device seed fields. `time`'s weekday is
/// Monday..=Sunday; the device wants 1 = Sunday..7 = Saturday.
fn from_offset(now: OffsetDateTime) -> SeedTime {
    // time::Weekday::number_days_from_sunday() returns 0 for Sunday..6 for
    // Saturday; the device weekday is that plus one.
    let weekday = now.weekday().number_days_from_sunday() + 1;
    SeedTime {
        year: now.year().max(0) as u16,
        month: now.month() as u8,
        day: now.day(),
        weekday,
        hour: now.hour(),
        minute: now.minute(),
        second: now.second(),
    }
}

/// Path to the persisted CMOS image, beside `izarravm.conf`.
pub fn cmos_path(c_root: &Path) -> PathBuf {
    c_root.parent().unwrap_or(c_root).join("cmos.bin")
}

/// Everything the emulation thread needs to bring the RTC online: the host
/// seed time and where to load/persist `cmos.bin`. Read once on the main thread
/// at startup and handed to the thread that builds the Machine.
#[derive(Debug, Clone)]
pub struct RtcSetup {
    pub seed: SeedTime,
    pub cmos_path: PathBuf,
}

impl RtcSetup {
    /// Read host local time and resolve the cmos.bin path beside the C: root.
    pub fn from_c_root(c_root: &Path) -> Self {
        Self {
            seed: read_host_time(),
            cmos_path: cmos_path(c_root),
        }
    }

    /// Apply the setup to a freshly built Machine: load cmos.bin if present
    /// (else keep defaults and write a fresh image), then seed the clock.
    pub fn apply(&self, machine: &mut izarravm_machine::Machine) {
        match load_cmos_file(&self.cmos_path) {
            Some(image) => {
                if !machine.load_cmos(&image) {
                    warn!(
                        path = %self.cmos_path.display(),
                        "cmos.bin had a bad checksum; repaired and reusing the bytes"
                    );
                    // The device repaired the checksum; persist the fixed image.
                    save_cmos_file(&self.cmos_path, &machine.cmos_bytes());
                }
            }
            None => {
                // No saved CMOS: persist the defaulted image (with its fresh
                // checksum) so the file exists for next run.
                save_cmos_file(&self.cmos_path, &machine.cmos_bytes());
            }
        }
        let s = self.seed;
        machine.seed_rtc(
            s.year, s.month, s.day, s.weekday, s.hour, s.minute, s.second,
        );
    }
}

/// Load a 64-byte CMOS image from `path`, or None if the file is missing or not
/// exactly 64 bytes. A wrong-sized or unreadable file is treated as absent so
/// the device falls back to defaults plus a fresh checksum.
pub fn load_cmos_file(path: &Path) -> Option<[u8; 64]> {
    let bytes = std::fs::read(path).ok()?;
    let array: [u8; 64] = bytes.try_into().ok()?;
    Some(array)
}

/// Write a 64-byte CMOS image to `path`, logging on failure rather than
/// aborting the run.
pub fn save_cmos_file(path: &Path, bytes: &[u8; 64]) {
    if let Err(err) = std::fs::write(path, bytes) {
        warn!(%err, path = %path.display(), "failed to persist cmos.bin");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Month;

    #[test]
    fn weekday_maps_sunday_to_one() {
        // 2026-06-21 is a Sunday.
        let dt = OffsetDateTime::new_utc(
            time::Date::from_calendar_date(2026, Month::June, 21).unwrap(),
            time::Time::from_hms(12, 0, 0).unwrap(),
        );
        let seed = from_offset(dt);
        assert_eq!(seed.weekday, 1);
        assert_eq!((seed.year, seed.month, seed.day), (2026, 6, 21));
    }

    #[test]
    fn weekday_maps_saturday_to_seven() {
        // 2026-06-20 is a Saturday.
        let dt = OffsetDateTime::new_utc(
            time::Date::from_calendar_date(2026, Month::June, 20).unwrap(),
            time::Time::from_hms(8, 30, 15).unwrap(),
        );
        let seed = from_offset(dt);
        assert_eq!(seed.weekday, 7);
        assert_eq!((seed.hour, seed.minute, seed.second), (8, 30, 15));
    }

    #[test]
    fn load_round_trips_a_saved_image() {
        let dir = std::env::temp_dir().join(format!("izarra_cmos_{}", std::process::id()));
        let c_root = dir.join("c_drive");
        std::fs::create_dir_all(&c_root).unwrap();
        let path = cmos_path(&c_root);
        let mut image = [0u8; 64];
        image[0x10] = 3;
        image[0x2f] = 0xab;
        save_cmos_file(&path, &image);
        let loaded = load_cmos_file(&path).unwrap();
        assert_eq!(loaded, image);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wrong_size_file_is_treated_as_absent() {
        let dir = std::env::temp_dir().join(format!("izarra_cmos_bad_{}", std::process::id()));
        let c_root = dir.join("c_drive");
        std::fs::create_dir_all(&c_root).unwrap();
        let path = cmos_path(&c_root);
        std::fs::write(&path, [0u8; 32]).unwrap();
        assert!(load_cmos_file(&path).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cmos_path_sits_beside_c_root() {
        let c_root = PathBuf::from("/home/user/.izarravm/c_drive");
        assert_eq!(
            cmos_path(&c_root),
            PathBuf::from("/home/user/.izarravm/cmos.bin")
        );
    }
}
