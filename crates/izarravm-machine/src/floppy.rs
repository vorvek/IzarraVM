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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wizardry_720k_geometry() {
        let g = geometry_for(737_280).unwrap();
        assert_eq!((g.cylinders, g.heads, g.sectors), (80, 2, 9));
        assert_eq!(g.drive_type, 0x03);
    }

    #[test]
    fn supported_sizes_map_to_geometry() {
        assert_eq!(geometry_for(368_640).unwrap().sectors, 9);
        assert_eq!(geometry_for(1_228_800).unwrap().sectors, 15);
        assert_eq!(geometry_for(1_474_560).unwrap().sectors, 18);
    }

    #[test]
    fn early_525_formats_map_to_geometry() {
        // 160 KB and 180 KB are single-sided; 320 KB and 360 KB are double-sided.
        let g160 = geometry_for(163_840).unwrap();
        assert_eq!((g160.cylinders, g160.heads, g160.sectors), (40, 1, 8));
        let g180 = geometry_for(184_320).unwrap();
        assert_eq!((g180.cylinders, g180.heads, g180.sectors), (40, 1, 9));
        let g320 = geometry_for(327_680).unwrap();
        assert_eq!((g320.cylinders, g320.heads, g320.sectors), (40, 2, 8));
        // Each maps to a full disk: cyl * heads * sectors * 512 == file size.
        for size in [163_840, 184_320, 327_680] {
            let g = geometry_for(size).unwrap();
            let bytes =
                usize::from(g.cylinders) * usize::from(g.heads) * usize::from(g.sectors) * 512;
            assert_eq!(
                bytes, size,
                "geometry for {size} must cover the whole image"
            );
        }
    }

    #[test]
    fn chs_offset_matches_lba() {
        let f = Floppy::from_image(vec![0u8; 737_280]).unwrap();
        // CHS(0,0,1) is LBA 0.
        assert_eq!(f.chs_offset(0, 0, 1), Some(0));
        // CHS(0,1,1) is LBA 9 on a 9-spt disk.
        assert_eq!(f.chs_offset(0, 1, 1), Some(9 * 512));
        // Sector 10 does not exist on a 9-spt disk.
        assert_eq!(f.chs_offset(0, 0, 10), None);
        // Sector 0 is not a valid 1-based sector.
        assert_eq!(f.chs_offset(0, 0, 0), None);
    }

    #[test]
    fn access_duration_models_seek_latency_and_transfer() {
        let mut f = Floppy::from_image(vec![0u8; 1_474_560]).unwrap(); // 1.44M, HD
        // First read at track 0 (head starts there): no seek, no latency, just
        // the transfer of one sector at 62.5 KB/s.
        let one_sector = f.access_duration_secs(0, 512);
        assert!((one_sector - 512.0 / 62_500.0).abs() < 1e-9);
        // A read on the same track is transfer-only again (no fresh latency).
        assert!((f.access_duration_secs(0, 512) - 512.0 / 62_500.0).abs() < 1e-9);
        // Seeking to track 10 costs 10 steps of seek plus half a revolution of
        // rotational latency, on top of the transfer.
        let seek_read = f.access_duration_secs(10, 512);
        let expect = 0.003 * 10.0 + 0.2 / 2.0 + 512.0 / 62_500.0;
        assert!((seek_read - expect).abs() < 1e-9, "{seek_read} vs {expect}");
        // A full-stroke seek is clamped to 100 ms.
        f.access_duration_secs(0, 0);
        let full = f.access_duration_secs(79, 0);
        assert!((full - (0.100 + 0.2 / 2.0)).abs() < 1e-9);
    }

    #[test]
    fn double_density_transfers_at_half_the_rate() {
        let mut hd = Floppy::from_image(vec![0u8; 1_474_560]).unwrap();
        let mut dd = Floppy::from_image(vec![0u8; 737_280]).unwrap();
        // Same bytes, same track: DD takes twice as long to transfer as HD.
        let hd_t = hd.access_duration_secs(0, 4096);
        let dd_t = dd.access_duration_secs(0, 4096);
        assert!((dd_t - 2.0 * hd_t).abs() < 1e-9);
    }

    #[test]
    fn round_trip_sector() {
        let mut f = Floppy::from_image(vec![0u8; 737_280]).unwrap();
        let mut buf = [0u8; 512];
        buf[0] = 0xAB;
        assert!(f.write_sector(1, 1, 5, &buf));
        assert_eq!(f.read_sector(1, 1, 5).unwrap()[0], 0xAB);
        assert!(f.dirty);
    }

    #[test]
    fn out_of_range_write_is_rejected() {
        let mut f = Floppy::from_image(vec![0u8; 737_280]).unwrap();
        let buf = [0u8; 512];
        assert!(!f.write_sector(0, 0, 10, &buf));
        assert!(!f.dirty);
    }

    #[test]
    fn unknown_size_rejected() {
        assert!(Floppy::from_image(vec![0u8; 123]).is_err());
    }
}
