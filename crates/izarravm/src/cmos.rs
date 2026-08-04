// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Host side of the RTC/CMOS: seed the clock from host local time at startup
//! and persist the 64-byte NVRAM image to `cmos.bin` next to `izarravm.conf`.
//!
//! The local-time read uses the `time` crate's `now_local()`, which is sound
//! only when called before any extra threads are spawned. Startup runs this on
//! the main thread before the emulation thread starts, so the read is safe; if
//! the host refuses a local offset, we fall back to UTC and log it.

use izarravm_core::{GswMode, SbDma8, SbDma16, SbIrq};
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

/// The hardware settings a command-line flag asked for THIS run, for the
/// settings CMOS also owns.
///
/// These flags set power-on values, and a saved `cmos.bin` overrides them --
/// which is correct (NVRAM is what the machine boots from) but invisible. A
/// user who types `--sb-irq 5` and hears the card answer on 7 has no way to
/// tell that a saved assignment beat them, so `apply` says so. Only flags that
/// were actually typed are tracked; a default the user never asked for being
/// overridden is not news.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestedHardware {
    pub cpu: Option<GswMode>,
    pub sb_irq: Option<SbIrq>,
    pub sb_dma: Option<SbDma8>,
    pub sb_high_dma: Option<SbDma16>,
}

impl RequestedHardware {
    fn is_empty(self) -> bool {
        self == Self::default()
    }

    /// Name every flag whose value the loaded CMOS did not end up honouring.
    fn overridden_by(self, machine: &izarravm_machine::Machine) -> Vec<String> {
        let mut names = Vec::new();
        if let Some(cpu) = self.cpu
            && machine.active_mode() != cpu
        {
            names.push(format!("--cpu {cpu}"));
        }
        // A machine built without the SB16 has no routing to disagree with, so
        // an absent card is silence rather than a warning about every flag.
        if let Some((irq, dma, high_dma)) = machine.sound_blaster_routing() {
            if let Some(want) = self.sb_irq
                && irq != want.line()
            {
                names.push(format!("--sb-irq {want}"));
            }
            if let Some(want) = self.sb_dma
                && dma != want.channel()
            {
                names.push(format!("--sb-dma {want}"));
            }
            if let Some(want) = self.sb_high_dma
                && high_dma != want.channel()
            {
                names.push(format!("--sb-high-dma {want}"));
            }
        }
        names
    }
}

/// Everything the emulation thread needs to bring the RTC online: the host
/// seed time, where to load/persist `cmos.bin`, and which hardware settings a
/// flag asked for. Read once on the main thread at startup and handed to the
/// thread that builds the Machine.
#[derive(Debug, Clone)]
pub struct RtcSetup {
    pub seed: SeedTime,
    pub cmos_path: PathBuf,
    pub requested: RequestedHardware,
}

impl RtcSetup {
    /// Read host local time and resolve the cmos.bin path beside the C: root.
    pub fn from_c_root(c_root: &Path) -> Self {
        Self {
            seed: read_host_time(),
            cmos_path: cmos_path(c_root),
            requested: RequestedHardware::default(),
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
                        "cmos.bin had a bad checksum; restored Izarra defaults"
                    );
                    // Persist the defaulted, checksummed replacement.
                    save_cmos_file(&self.cmos_path, &machine.cmos_bytes());
                }
                self.warn_about_overridden_flags(machine);
            }
            None => {
                // No saved CMOS: persist the defaulted image (with its fresh
                // checksum) so the file exists for next run. Nothing overrode
                // the flags here -- this is the run where they set the machine
                // up, and the image just written is what they set it to.
                save_cmos_file(&self.cmos_path, &machine.cmos_bytes());
            }
        }
        let s = self.seed;
        machine.seed_rtc(
            s.year, s.month, s.day, s.weekday, s.hour, s.minute, s.second,
        );
    }

    fn warn_about_overridden_flags(&self, machine: &izarravm_machine::Machine) {
        if self.requested.is_empty() {
            return;
        }
        let ignored = self.requested.overridden_by(machine);
        if ignored.is_empty() {
            return;
        }
        warn!(
            path = %self.cmos_path.display(),
            flags = %ignored.join(", "),
            "the saved CMOS overrode these flags; it is what the machine boots \
             from. Change the CPU speed with GSWMODE or the BIOS setup panel \
             (Del), and the sound card with SNDCTRL, both inside DOS -- or \
             delete cmos.bin to start from the flags again"
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
#[path = "cmos_test.rs"]
mod tests;
