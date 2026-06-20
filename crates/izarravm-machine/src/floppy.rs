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
}

const SECTOR: usize = 512;

/// Map a raw image length to a CHS geometry, or None for an unrecognized size.
pub fn geometry_for(size: usize) -> Option<Geometry> {
    Some(match size {
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
        })
    }

    pub fn geometry(&self) -> Geometry {
        self.geom
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
