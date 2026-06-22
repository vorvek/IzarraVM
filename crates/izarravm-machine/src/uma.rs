//! The UMA-hole reservation map: a paragraph-granular allocator over the
//! UMB-able upper-memory window (0xC0000-0xEFFFF, the `is_umb_window` band of
//! the region classifier). It is the single authority that hands out upper
//! memory blocks (P5 UMBs) and the LIM EMS 4.0 page frame (P6), so no two
//! consumers can ever claim the same paragraph. Option or system ROM mapped into
//! the window is reserved up front and is never handed out.
//!
//! This is pure bookkeeping: it tracks reserved spans and computes the free holes
//! between them. It does not touch the bus or guest RAM. A later slice wires real
//! UMBs and the EMS frame onto the addresses it returns.

use crate::memmap::{SYSTEM_ROM_BASE, UPPER_MEMORY_BASE};

/// The window the map governs: the UMB-able upper memory, 0xC0000-0xEFFFF.
const WINDOW_BASE: u32 = UPPER_MEMORY_BASE;
const WINDOW_END: u32 = SYSTEM_ROM_BASE;

/// A paragraph is 16 bytes; UMBs and MCBs are paragraph-granular.
const PARAGRAPH: u32 = 16;
/// The LIM EMS 4.0 page frame is four 16 KiB pages.
pub const EMS_FRAME_SIZE: u32 = 64 * 1024;
/// The page frame aligns to a 16 KiB page boundary within the window.
const EMS_FRAME_ALIGN: u32 = 16 * 1024;

/// Round `x` up to the next multiple of `align`, or None if that overflows u32.
fn align_up(x: u32, align: u32) -> Option<u32> {
    x.checked_next_multiple_of(align)
}

/// What a reserved span is used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UmaUse {
    /// Option or system ROM mapped into the window at boot; never handed out.
    Rom,
    /// An upper memory block handed to the DOS arena.
    Umb,
    /// The LIM EMS 4.0 64 KiB page frame.
    EmsFrame,
}

/// One reserved span: the half-open range [base, base + size), within the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UmaReservation {
    pub base: u32,
    pub size: u32,
    pub kind: UmaUse,
}

/// A paragraph-granular allocator over the UMB-able window. Reservations are kept
/// sorted by base and never overlap, so the free holes between them are exactly
/// the space still available to hand out.
#[derive(Debug, Default, Clone)]
pub struct UmaReservationMap {
    reservations: Vec<UmaReservation>,
}

impl UmaReservationMap {
    /// An empty map: the whole 0xC0000-0xEFFFF window is free.
    pub fn new() -> Self {
        Self {
            reservations: Vec::new(),
        }
    }

    /// The current reservations, sorted by base.
    pub fn reservations(&self) -> &[UmaReservation] {
        &self.reservations
    }

    /// Mark a fixed ROM span occupied so it is never handed out as a UMB or the
    /// EMS frame. Returns false (and reserves nothing) if the span falls outside
    /// the window or overlaps an existing reservation.
    pub fn reserve_rom(&mut self, base: u32, size: u32) -> bool {
        if size == 0 || base < WINDOW_BASE {
            return false;
        }
        // checked_add rejects a span whose end overflows u32, which would
        // otherwise wrap below WINDOW_END and slip an out-of-window ROM past the
        // bound check, corrupting the hole math.
        match base.checked_add(size) {
            Some(end) if end <= WINDOW_END => {}
            _ => return false,
        }
        if self.overlaps(base, size) {
            return false;
        }
        self.insert(UmaReservation {
            base,
            size,
            kind: UmaUse::Rom,
        });
        true
    }

    /// First-fit a free hole of `size` bytes (rounded up to a paragraph) for an
    /// upper memory block. Returns its paragraph-aligned base, or None when no
    /// hole is large enough.
    pub fn alloc_umb(&mut self, size: u32) -> Option<u32> {
        if size == 0 {
            return None;
        }
        let size = align_up(size, PARAGRAPH)?;
        self.alloc(size, PARAGRAPH, UmaUse::Umb)
    }

    /// Reserve the 64 KiB LIM EMS 4.0 page frame in the first 16 KiB-aligned hole
    /// that fits it. Returns its base, or None when no hole is large enough. The
    /// frame is placed here rather than picked independently, so it cannot land
    /// on top of a UMB or ROM.
    pub fn alloc_ems_frame(&mut self) -> Option<u32> {
        self.alloc(EMS_FRAME_SIZE, EMS_FRAME_ALIGN, UmaUse::EmsFrame)
    }

    /// Release a previously handed-out UMB or EMS frame at `base`, returning its
    /// space to the free holes. ROM reservations are permanent: freeing one (or a
    /// base that is not reserved) returns false and changes nothing.
    pub fn free(&mut self, base: u32) -> bool {
        if let Some(i) = self
            .reservations
            .iter()
            .position(|r| r.base == base && r.kind != UmaUse::Rom)
        {
            self.reservations.remove(i);
            true
        } else {
            false
        }
    }

    /// Total free bytes left in the window.
    pub fn total_free(&self) -> u32 {
        self.free_holes().iter().map(|&(_, size)| size).sum()
    }

    /// The free holes between reservations, as (base, size), in address order.
    fn free_holes(&self) -> Vec<(u32, u32)> {
        let mut holes = Vec::new();
        let mut cursor = WINDOW_BASE;
        for r in &self.reservations {
            if r.base > cursor {
                holes.push((cursor, r.base - cursor));
            }
            cursor = cursor.max(r.base + r.size);
        }
        if cursor < WINDOW_END {
            holes.push((cursor, WINDOW_END - cursor));
        }
        holes
    }

    /// Allocate `size` bytes from the first hole that fits it at `align`.
    fn alloc(&mut self, size: u32, align: u32, kind: UmaUse) -> Option<u32> {
        for (hole_base, hole_size) in self.free_holes() {
            // Align the start up within the hole; skip the hole if that overflows.
            let Some(base) = align_up(hole_base, align) else {
                continue;
            };
            // hole_base + hole_size stays below 1 MiB (every hole is window-
            // clamped), so the right side cannot overflow; checked_add guards the
            // left side in case a caller asked for a near-u32::MAX size.
            if base
                .checked_add(size)
                .is_some_and(|end| end <= hole_base + hole_size)
            {
                self.insert(UmaReservation { base, size, kind });
                return Some(base);
            }
        }
        None
    }

    fn overlaps(&self, base: u32, size: u32) -> bool {
        let end = base + size;
        self.reservations
            .iter()
            .any(|r| base < r.base + r.size && r.base < end)
    }

    fn insert(&mut self, r: UmaReservation) {
        self.reservations.push(r);
        self.reservations.sort_by_key(|r| r.base);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW_SIZE: u32 = WINDOW_END - WINDOW_BASE; // 192 KiB

    /// Assert no two reservations share a paragraph (the P6 no-double-owned gate).
    fn assert_no_overlap(map: &UmaReservationMap) {
        let rs = map.reservations();
        for (i, a) in rs.iter().enumerate() {
            for b in &rs[i + 1..] {
                let disjoint = a.base + a.size <= b.base || b.base + b.size <= a.base;
                assert!(disjoint, "reservations overlap: {a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn an_empty_map_is_all_free() {
        let map = UmaReservationMap::new();
        assert_eq!(map.total_free(), WINDOW_SIZE);
        assert!(map.reservations().is_empty());
    }

    #[test]
    fn reserve_rom_validates_bounds_and_overlap() {
        let mut map = UmaReservationMap::new();
        // Below the window.
        assert!(!map.reserve_rom(WINDOW_BASE - PARAGRAPH, PARAGRAPH));
        // Past the window end.
        assert!(!map.reserve_rom(WINDOW_END - PARAGRAPH, PARAGRAPH * 2));
        // Zero size.
        assert!(!map.reserve_rom(WINDOW_BASE, 0));
        // A valid 32 KiB option ROM at the window base.
        assert!(map.reserve_rom(WINDOW_BASE, 0x8000));
        assert_eq!(map.total_free(), WINDOW_SIZE - 0x8000);
        // An overlapping ROM is rejected.
        assert!(!map.reserve_rom(WINDOW_BASE + 0x4000, 0x1000));
    }

    #[test]
    fn alloc_umb_first_fits_after_a_rom_hole() {
        let mut map = UmaReservationMap::new();
        // A 32 KiB ROM occupies the start of the window.
        assert!(map.reserve_rom(WINDOW_BASE, 0x8000));
        // The next UMB lands right after the ROM, paragraph-aligned.
        let umb = map.alloc_umb(0x1000).expect("a 4 KiB UMB fits");
        assert_eq!(umb, WINDOW_BASE + 0x8000);
        assert_eq!(umb % PARAGRAPH, 0);
        assert_no_overlap(&map);
    }

    #[test]
    fn umb_size_rounds_up_to_a_paragraph() {
        let mut map = UmaReservationMap::new();
        let a = map.alloc_umb(1).expect("a 1-byte request");
        let b = map.alloc_umb(1).expect("the next request");
        // The first UMB occupies a whole paragraph, so the second starts 16 bytes on.
        assert_eq!(b - a, PARAGRAPH);
        assert_no_overlap(&map);
    }

    #[test]
    fn ems_frame_is_64k_and_16k_aligned_and_disjoint_from_umbs() {
        let mut map = UmaReservationMap::new();
        // Interleave a UMB, the EMS frame, and another UMB: the P6 gate scenario.
        let _u1 = map.alloc_umb(0x2000).expect("first UMB");
        let frame = map.alloc_ems_frame().expect("the 64 KiB frame fits");
        let _u2 = map.alloc_umb(0x2000).expect("second UMB");
        assert_eq!(frame % EMS_FRAME_ALIGN, 0, "frame is 16 KiB-aligned");
        let placed = map
            .reservations()
            .iter()
            .find(|r| r.kind == UmaUse::EmsFrame)
            .unwrap();
        assert_eq!(placed.size, EMS_FRAME_SIZE);
        assert_no_overlap(&map);
    }

    #[test]
    fn free_returns_a_umb_but_not_a_rom() {
        let mut map = UmaReservationMap::new();
        assert!(map.reserve_rom(WINDOW_BASE, 0x4000));
        let umb = map.alloc_umb(0x4000).unwrap();
        let before = map.total_free();
        // Freeing the UMB returns its space.
        assert!(map.free(umb));
        assert_eq!(map.total_free(), before + 0x4000);
        // ROM cannot be freed, and an unknown base is a no-op.
        assert!(!map.free(WINDOW_BASE));
        assert!(!map.free(WINDOW_BASE + 0x12345));
    }

    #[test]
    fn the_window_runs_out_of_space() {
        let mut map = UmaReservationMap::new();
        // Three 64 KiB frames tile the 192 KiB window exactly (0xC0000-0xEFFFF).
        assert!(map.alloc_ems_frame().is_some());
        assert!(map.alloc_ems_frame().is_some());
        assert!(map.alloc_ems_frame().is_some());
        // The window is now full.
        assert_eq!(map.total_free(), 0);
        assert!(map.alloc_ems_frame().is_none(), "no fourth frame");
        assert!(map.alloc_umb(PARAGRAPH).is_none(), "no room for a UMB");
        assert_no_overlap(&map);
    }

    #[test]
    fn reserve_rom_rejects_an_overflowing_span() {
        let mut map = UmaReservationMap::new();
        // base + size wraps below WINDOW_END; it must be rejected, not accepted.
        assert!(!map.reserve_rom(u32::MAX, 1));
        assert!(!map.reserve_rom(0xFFFF_FFF0, 0x20));
        assert!(
            map.reservations().is_empty(),
            "no out-of-window ROM slipped in"
        );
        assert_eq!(map.total_free(), WINDOW_SIZE);
    }

    #[test]
    fn a_freed_hole_is_reused_by_the_next_alloc() {
        let mut map = UmaReservationMap::new();
        assert!(map.reserve_rom(WINDOW_BASE, 0x4000));
        let first = map.alloc_umb(0x1000).expect("a UMB after the ROM");
        assert!(map.free(first));
        // First-fit reclaims the freed hole: the re-alloc lands at the same base.
        let second = map
            .alloc_umb(0x1000)
            .expect("re-alloc into the reclaimed hole");
        assert_eq!(second, first);
        assert_no_overlap(&map);
    }

    #[test]
    fn ems_frame_skips_to_the_next_16k_boundary_past_an_unaligned_rom() {
        let mut map = UmaReservationMap::new();
        // A 0x5000 ROM (not 16 KiB-aligned) ends at 0xC5000; the frame must skip
        // up to the next 16 KiB boundary, 0xC8000.
        assert!(map.reserve_rom(WINDOW_BASE, 0x5000));
        let frame = map.alloc_ems_frame().expect("the frame fits past the ROM");
        assert!(frame >= WINDOW_BASE + 0x5000, "frame starts past the ROM");
        assert_eq!(frame % EMS_FRAME_ALIGN, 0, "frame is 16 KiB-aligned");
        assert_eq!(frame, WINDOW_BASE + 0x8000);
        assert_no_overlap(&map);
    }
}
