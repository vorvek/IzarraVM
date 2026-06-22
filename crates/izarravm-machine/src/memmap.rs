//! The named physical-memory regions of the first megabyte plus the HMA, as the
//! DOS world sees them: conventional RAM, the upper-memory area (video aperture,
//! option and system ROM, and the UMB-able holes between them), and extended
//! memory with its real-mode-reachable HMA window.
//!
//! This is the single place those boundaries are defined. The memory manager
//! (IEMM) and the BIOS memory services classify an address through here instead
//! of scattering the same magic constants across the crate. It is a pure address
//! model: no state, no A20 effect, no wraparound. Those ride a later slice; this
//! one only names where things live.

/// Top of conventional (DOS-usable low) RAM, exclusive: 640 KiB. The BIOS
/// reserves the top 1 KiB of this for the EBDA, but that is still conventional
/// RAM, so it classifies as Conventional.
pub const CONVENTIONAL_TOP: u32 = 0x0A_0000;
/// Base of the video RAM aperture (EGA/VGA graphics and the text pages).
pub const VIDEO_RAM_BASE: u32 = 0x0A_0000;
/// Base of upper memory: option and system ROM plus the UMB-able holes,
/// 0xC0000-0xEFFFF. This window is where load-high UMBs and the EMS page frame
/// are carved (a later slice owns that reservation map).
pub const UPPER_MEMORY_BASE: u32 = 0x0C_0000;
/// Base of the system BIOS ROM, the top 64 KiB of the first megabyte.
pub const SYSTEM_ROM_BASE: u32 = 0x0F_0000;
/// The 1 MiB real-mode boundary; extended memory and the HMA begin here.
pub const HMA_BASE: u32 = 0x10_0000;
/// One past the HMA. Real-mode FFFF:FFFF reaches 0x10FFEF, so the HMA spans
/// 0x100000..0x10FFF0 (65520 bytes: 64 KiB minus the 16-byte segment base).
pub const HMA_TOP: u32 = 0x10_FFF0;

/// A named region of the low physical address space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemRegion {
    /// DOS-usable low RAM, 0x00000-0x9FFFF (640 KiB). The BIOS-reserved EBDA in
    /// the top 1 KiB is still conventional RAM and classifies here.
    Conventional,
    /// Video RAM aperture, 0xA0000-0xBFFFF (128 KiB).
    VideoRam,
    /// Upper memory, 0xC0000-0xEFFFF: option/system ROM and the UMB-able holes.
    UpperMemory,
    /// System BIOS ROM, 0xF0000-0xFFFFF.
    SystemRom,
    /// Extended memory, 0x100000 and up. The first ~64 KiB is also the HMA (see
    /// `is_hma`); physically it is the same extended RAM.
    Extended,
}

/// Classify a physical address into its named region. Addresses at or above the
/// 1 MiB boundary are Extended (use `is_hma` to pick out the HMA window within).
pub fn classify(addr: u32) -> MemRegion {
    match addr {
        a if a < CONVENTIONAL_TOP => MemRegion::Conventional,
        a if a < UPPER_MEMORY_BASE => MemRegion::VideoRam,
        a if a < SYSTEM_ROM_BASE => MemRegion::UpperMemory,
        a if a < HMA_BASE => MemRegion::SystemRom,
        _ => MemRegion::Extended,
    }
}

/// True if `addr` falls in the High Memory Area (0x100000-0x10FFEF), the slice
/// of extended memory a real-mode program can reach through FFFF:offset when the
/// A20 gate is open. This is a label on extended RAM, not a separate region.
pub fn is_hma(addr: u32) -> bool {
    (HMA_BASE..HMA_TOP).contains(&addr)
}

/// True if `addr` lies in the UMB-able upper-memory window (0xC0000-0xEFFFF),
/// the only region where upper memory blocks and the EMS page frame may be
/// carved. The video aperture and the system ROM are deliberately excluded.
pub fn is_umb_window(addr: u32) -> bool {
    (UPPER_MEMORY_BASE..SYSTEM_ROM_BASE).contains(&addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries_classify_to_the_right_region() {
        assert_eq!(classify(0), MemRegion::Conventional);
        assert_eq!(classify(0x0_9FFFF), MemRegion::Conventional);
        // The EBDA (top 1 KiB of conventional) is still conventional RAM.
        assert_eq!(classify(0x0_9FC00), MemRegion::Conventional);
        assert_eq!(classify(CONVENTIONAL_TOP), MemRegion::VideoRam);
        assert_eq!(classify(0x0_BFFFF), MemRegion::VideoRam);
        assert_eq!(classify(UPPER_MEMORY_BASE), MemRegion::UpperMemory);
        assert_eq!(classify(0x0_EFFFF), MemRegion::UpperMemory);
        assert_eq!(classify(SYSTEM_ROM_BASE), MemRegion::SystemRom);
        assert_eq!(classify(0x0_FFFFF), MemRegion::SystemRom);
        assert_eq!(classify(HMA_BASE), MemRegion::Extended);
        assert_eq!(classify(0x80_0000), MemRegion::Extended);
    }

    #[test]
    fn hma_window_is_the_first_64k_minus_16() {
        assert!(
            !is_hma(0x0_FFFFF),
            "below the 1 MiB boundary is not the HMA"
        );
        assert!(is_hma(HMA_BASE), "the HMA starts at 1 MiB");
        assert!(is_hma(HMA_TOP - 1), "FFFF:FFEF, the last HMA byte");
        assert!(!is_hma(HMA_TOP), "0x10FFF0 is past the real-mode reach");
        // The HMA is 64 KiB minus the 16-byte segment base.
        assert_eq!(HMA_TOP - HMA_BASE, 0x1_0000 - 0x10);
    }

    #[test]
    fn umb_window_excludes_video_and_system_rom() {
        assert!(
            !is_umb_window(VIDEO_RAM_BASE),
            "video aperture is not UMB-able"
        );
        assert!(
            is_umb_window(UPPER_MEMORY_BASE),
            "0xC0000 opens the UMB window"
        );
        assert!(
            is_umb_window(0x0_EFFFF),
            "0xEFFFF is the last UMB-able byte"
        );
        assert!(
            !is_umb_window(SYSTEM_ROM_BASE),
            "system ROM is not UMB-able"
        );
    }

    #[test]
    fn the_named_regions_tile_the_first_megabyte_without_gaps() {
        // Walk every region boundary; each address belongs to exactly the region
        // whose half-open range contains it, with no gap or overlap.
        let edges = [
            (0u32, MemRegion::Conventional),
            (CONVENTIONAL_TOP, MemRegion::VideoRam),
            (UPPER_MEMORY_BASE, MemRegion::UpperMemory),
            (SYSTEM_ROM_BASE, MemRegion::SystemRom),
            (HMA_BASE, MemRegion::Extended),
        ];
        for (addr, want) in edges {
            assert_eq!(classify(addr), want, "addr {addr:#07x}");
        }
    }
}
