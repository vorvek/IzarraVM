// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

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
