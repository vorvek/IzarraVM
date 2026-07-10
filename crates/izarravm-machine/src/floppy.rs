// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! In-memory floppy image with geometry derived from its size.
//!
//! The drive is a 1.44 MB high-density unit, but the media geometry is read off
//! the image length so a double-density 720 KB disk reads with the right
//! sectors-per-track. Wizardry III's booter is a 720 KB image, so hardcoding 18
//! sectors per track would misread it.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub cylinders: u16,
    pub heads: u8,
    pub sectors: u8,
    /// INT 13h AH=08 BL drive type: 0x03 = 720 KB, 0x04 = 1.44 MB.
    pub drive_type: u8,
}

#[derive(Debug)]
pub struct Floppy {
    bytes: Vec<u8>,
    geom: Geometry,
    pub dirty: bool,
    /// Tracked head position, so a seek's distance (and thus its time) is the
    /// real cylinder delta rather than a fixed cost.
    current_cylinder: u16,
}

const SECTOR: usize = 512;

/// One revolution at 300 RPM. Half of it is the average rotational latency: the
/// wait for the target sector to come under the head after a seek.
const REVOLUTION_SECS: f64 = 0.2;
/// Head step time per cylinder, clamped so a full stroke lands in the period
/// 3-100 ms seek envelope.
const SEEK_PER_TRACK_SECS: f64 = 0.003;
const SEEK_MAX_SECS: f64 = 0.100;
/// Sustained transfer rate. High-density media (1.2/1.44 MB, 500 kbit/s) moves
/// ~62.5 KB/s; double-density (360/720 KB, 250 kbit/s) is half that.
const HD_BYTES_PER_SEC: f64 = 62_500.0;
const DD_BYTES_PER_SEC: f64 = 31_250.0;

/// Map a raw image length to a CHS geometry, or None for an unrecognized size.
pub fn geometry_for(size: usize) -> Option<Geometry> {
    Some(match size {
        // The early 5.25" formats. All double-density (250 kbit/s), so they share
        // the 0x03 drive type the 360 KB disk uses; only the head and sector
        // counts differ. 160/180 KB are single-sided.
        163_840 => Geometry {
            cylinders: 40,
            heads: 1,
            sectors: 8,
            drive_type: 0x03,
        },
        184_320 => Geometry {
            cylinders: 40,
            heads: 1,
            sectors: 9,
            drive_type: 0x03,
        },
        327_680 => Geometry {
            cylinders: 40,
            heads: 2,
            sectors: 8,
            drive_type: 0x03,
        },
        368_640 => Geometry {
            cylinders: 40,
            heads: 2,
            sectors: 9,
            drive_type: 0x03,
        },
        737_280 => Geometry {
            cylinders: 80,
            heads: 2,
            sectors: 9,
            drive_type: 0x03,
        },
        1_228_800 => Geometry {
            cylinders: 80,
            heads: 2,
            sectors: 15,
            drive_type: 0x04,
        },
        1_474_560 => Geometry {
            cylinders: 80,
            heads: 2,
            sectors: 18,
            drive_type: 0x04,
        },
        _ => return None,
    })
}

impl Floppy {
    pub fn from_image(bytes: Vec<u8>) -> Result<Self, String> {
        let geom = geometry_for(bytes.len())
            .ok_or_else(|| format!("unsupported floppy image size {} bytes", bytes.len()))?;
        Ok(Self {
            bytes,
            geom,
            dirty: false,
            current_cylinder: 0,
        })
    }

    pub fn geometry(&self) -> Geometry {
        self.geom
    }

    /// Emulated seconds an access at `target_cyl` moving `bytes` of data takes on
    /// the real drive: seek from the tracked head position, plus the average
    /// rotational latency when the head moved, plus the transfer time. Updates the
    /// tracked position to `target_cyl`. `bytes` = 0 models a bare seek/recalibrate.
    pub fn access_duration_secs(&mut self, target_cyl: u16, bytes: usize) -> f64 {
        let delta = (i32::from(target_cyl) - i32::from(self.current_cylinder)).unsigned_abs();
        self.current_cylinder = target_cyl;
        let (seek, latency) = if delta == 0 {
            // Same track: no step, and sequential sectors arrive without a fresh
            // rotational wait.
            (0.0, 0.0)
        } else {
            let seek = (SEEK_PER_TRACK_SECS * f64::from(delta)).min(SEEK_MAX_SECS);
            (seek, REVOLUTION_SECS / 2.0)
        };
        let rate = if self.geom.drive_type == 0x04 {
            HD_BYTES_PER_SEC
        } else {
            DD_BYTES_PER_SEC
        };
        seek + latency + bytes as f64 / rate
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Linear byte offset for a 1-based sector at CHS, or None if out of range.
    pub fn chs_offset(&self, cyl: u16, head: u8, sector: u8) -> Option<usize> {
        if sector == 0
            || sector > self.geom.sectors
            || head >= self.geom.heads
            || cyl >= self.geom.cylinders
        {
            return None;
        }
        let lba = (u32::from(cyl) * u32::from(self.geom.heads) + u32::from(head))
            * u32::from(self.geom.sectors)
            + u32::from(sector - 1);
        Some(lba as usize * SECTOR)
    }

    pub fn read_sector(&self, cyl: u16, head: u8, sector: u8) -> Option<&[u8]> {
        let off = self.chs_offset(cyl, head, sector)?;
        self.bytes.get(off..off + SECTOR)
    }

    pub fn write_sector(&mut self, cyl: u16, head: u8, sector: u8, data: &[u8]) -> bool {
        let Some(off) = self.chs_offset(cyl, head, sector) else {
            return false;
        };
        if data.len() < SECTOR {
            return false;
        }
        self.bytes[off..off + SECTOR].copy_from_slice(&data[..SECTOR]);
        self.dirty = true;
        true
    }

    /// Write one byte during a timed FDC DMA transfer.
    pub fn write_sector_byte(
        &mut self,
        cyl: u16,
        head: u8,
        sector: u8,
        byte_offset: usize,
        value: u8,
    ) -> bool {
        if byte_offset >= SECTOR {
            return false;
        }
        let Some(offset) = self.chs_offset(cyl, head, sector) else {
            return false;
        };
        self.bytes[offset + byte_offset] = value;
        self.dirty = true;
        true
    }

    /// Fill every sector of the addressed track with `fill_byte`, the way INT 13h
    /// AH=05h formats a track. Returns false if the cylinder/head are off the
    /// mounted media. The standard DOS format filler is 0xF6.
    pub fn format_track(&mut self, cyl: u16, head: u8, fill_byte: u8) -> bool {
        if head >= self.geom.heads || cyl >= self.geom.cylinders {
            return false;
        }
        for sector in 1..=self.geom.sectors {
            if let Some(off) = self.chs_offset(cyl, head, sector) {
                self.bytes[off..off + SECTOR].fill(fill_byte);
            }
        }
        self.dirty = true;
        true
    }
}

#[cfg(test)]
#[path = "floppy_test.rs"]
mod tests;
