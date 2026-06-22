//! LIM EMS 4.0 expanded memory: the handle and page state behind INT 67h.
//!
//! Expanded memory is handed out in 16 KiB logical pages from a region carved out
//! of extended RAM, the way EMM386 simulates EMS from extended memory (so EMS and
//! XMS share one physical pool, partitioned at init). A handle owns a list of
//! logical pages; mapping a logical page into one of the four physical slots of the
//! 64 KiB page frame makes its backing bytes visible at the frame address. The
//! backing bytes live in the machine's RAM at `region_base + backing_page *
//! 16 KiB`; the bus aliases frame accesses onto them (the EMS arm in MachineBus
//! calls `resolve`), so a map moves guest-visible bytes with no copy.
//!
//! This module is pure state: handles, the free-page set, the four-slot frame map,
//! and the per-handle saved mapping context. It never touches the bus; the
//! dispatcher (`handle_int67`) marshals registers to these methods, and the bus
//! reads the frame map through `resolve`.

use std::collections::HashMap;

/// A logical/physical expanded-memory page is 16 KiB.
pub const EMS_PAGE_SIZE: u32 = 16 * 1024;
/// The page frame is four 16 KiB physical pages (slots 0-3).
pub const FRAME_SLOTS: usize = 4;
/// Reported EMM version, BCD: LIM EMS 4.0.
pub const EMS_VERSION_BCD: u8 = 0x40;

/// INT 67h status codes returned in AH; 0x00 is success.
pub mod status {
    pub const OK: u8 = 0x00;
    pub const INVALID_HANDLE: u8 = 0x83;
    pub const NO_MORE_HANDLES: u8 = 0x85;
    pub const TOTAL_EXCEEDED: u8 = 0x87;
    pub const FREE_EXCEEDED: u8 = 0x88;
    pub const ZERO_PAGES: u8 = 0x89;
    pub const INVALID_LOGICAL_PAGE: u8 = 0x8a;
    pub const INVALID_PHYSICAL_PAGE: u8 = 0x8b;
    pub const CONTEXT_ALREADY_SAVED: u8 = 0x8d;
    pub const NO_SAVED_CONTEXT: u8 = 0x8e;
    /// Move/exchange (57h) region is larger than the handle's allocated pages.
    pub const REGION_EXCEEDS_PAGES: u8 = 0x93;
    /// Move/exchange (57h) region length is more than 1 MiB.
    pub const LENGTH_EXCEEDS_1M: u8 = 0x96;
    /// Move/exchange (57h) conventional region crosses the 1 MiB boundary.
    pub const MEMORY_WRAP: u8 = 0xa2;
}

/// One physical-frame slot's mapping: the owning handle and the backing-page index
/// (within the EMS region) its current logical page resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SlotMap {
    handle: u16,
    backing: u32,
}

#[derive(Debug, Clone)]
struct EmsHandle {
    /// Backing-page indices this handle owns, in logical-page order.
    pages: Vec<u32>,
    /// The 8-byte handle name (functions 53h get/set); blank until set.
    name: [u8; 8],
}

/// The expanded-memory manager state for one machine.
#[derive(Debug)]
pub struct EmsState {
    frame_seg: u16,
    region_base: u32,
    total_pages: u16,
    /// Per backing-page free flag; the index is the backing-page number.
    free: Vec<bool>,
    /// Handle id -> handle (None for a free slot). Index 0 is the reserved OS
    /// handle and is never handed out, so a valid app handle is always >= 1.
    handles: Vec<Option<EmsHandle>>,
    /// The four physical frame slots; None is unmapped.
    frame_map: [Option<SlotMap>; FRAME_SLOTS],
    /// Per-handle saved frame-map context (functions 47h save / 48h restore).
    saved: HashMap<u16, [Option<SlotMap>; FRAME_SLOTS]>,
}

impl EmsState {
    /// EMM386's default handle ceiling.
    const MAX_HANDLES: usize = 64;

    /// Build the manager over a region of `total_pages` 16 KiB pages based at
    /// `region_base` in physical RAM, with the page frame at segment `frame_seg`.
    pub fn new(frame_seg: u16, region_base: u32, total_pages: u16) -> Self {
        Self {
            frame_seg,
            region_base,
            total_pages,
            free: vec![true; usize::from(total_pages)],
            handles: vec![None], // index 0 reserved for the OS handle
            frame_map: [None; FRAME_SLOTS],
            saved: HashMap::new(),
        }
    }

    /// The page-frame segment (function 41h).
    pub fn frame_segment(&self) -> u16 {
        self.frame_seg
    }

    /// The reported EMM version, BCD (function 46h).
    pub fn version(&self) -> u8 {
        EMS_VERSION_BCD
    }

    /// (free pages, total pages) for function 42h.
    pub fn page_counts(&self) -> (u16, u16) {
        (self.free_pages() as u16, self.total_pages)
    }

    fn free_pages(&self) -> usize {
        self.free.iter().filter(|&&f| f).count()
    }

    fn handle(&self, id: u16) -> Result<&EmsHandle, u8> {
        self.handles
            .get(usize::from(id))
            .and_then(|h| h.as_ref())
            .ok_or(status::INVALID_HANDLE)
    }

    /// Function 43h: allocate `pages` logical pages and return a new handle.
    pub fn allocate(&mut self, pages: u16) -> Result<u16, u8> {
        if pages == 0 {
            return Err(status::ZERO_PAGES);
        }
        if pages > self.total_pages {
            return Err(status::TOTAL_EXCEEDED);
        }
        if usize::from(pages) > self.free_pages() {
            return Err(status::FREE_EXCEEDED);
        }
        // Find a free handle id (>= 1), or grow up to the ceiling.
        let id = match (1..self.handles.len()).find(|&i| self.handles[i].is_none()) {
            Some(i) => i,
            None => {
                if self.handles.len() > Self::MAX_HANDLES {
                    return Err(status::NO_MORE_HANDLES);
                }
                self.handles.push(None);
                self.handles.len() - 1
            }
        };
        let backing = self.take_free_pages(usize::from(pages));
        self.handles[id] = Some(EmsHandle {
            pages: backing,
            name: [0; 8],
        });
        Ok(id as u16)
    }

    /// Mark and collect `n` free backing pages (first-fit, individually mapped).
    fn take_free_pages(&mut self, n: usize) -> Vec<u32> {
        let mut out = Vec::with_capacity(n);
        for (i, slot) in self.free.iter_mut().enumerate() {
            if *slot {
                *slot = false;
                out.push(i as u32);
                if out.len() == n {
                    break;
                }
            }
        }
        out
    }

    /// Function 44h: map logical page `logical` of `handle` into physical frame
    /// slot `phys` (0-3). `logical` = 0xFFFF unmaps the slot (the EMS 4.0 / QEMM
    /// extension).
    pub fn map(&mut self, phys: u8, logical: u16, handle: u16) -> Result<(), u8> {
        if usize::from(phys) >= FRAME_SLOTS {
            return Err(status::INVALID_PHYSICAL_PAGE);
        }
        let slot = if logical == 0xFFFF {
            None
        } else {
            let h = self.handle(handle)?;
            let backing = *h
                .pages
                .get(usize::from(logical))
                .ok_or(status::INVALID_LOGICAL_PAGE)?;
            Some(SlotMap { handle, backing })
        };
        self.frame_map[usize::from(phys)] = slot;
        Ok(())
    }

    /// Function 45h: release a handle and its pages, clearing any live or saved
    /// frame slot that referenced one of its pages so a later restore cannot
    /// reinstate a slot aliasing a reassigned page.
    pub fn release(&mut self, handle: u16) -> Result<(), u8> {
        let idx = usize::from(handle);
        let h = self
            .handles
            .get_mut(idx)
            .and_then(|slot| slot.take())
            .ok_or(status::INVALID_HANDLE)?;
        let freed = h.pages;
        for page in &freed {
            self.free[*page as usize] = true;
        }
        self.invalidate_freed(&freed);
        self.saved.remove(&handle);
        Ok(())
    }

    /// Drop every live and saved frame slot that references one of `freed` backing
    /// pages. A saved mapping context (function 47h) snapshots resolved backing
    /// indices, so once a page is freed and reassigned, a stale snapshot would let
    /// a restore (48h) alias another handle's expanded memory. Scrubbing both the
    /// live map and the saved contexts keeps that from ever happening.
    fn invalidate_freed(&mut self, freed: &[u32]) {
        for slot in &mut self.frame_map {
            if matches!(slot, Some(s) if freed.contains(&s.backing)) {
                *slot = None;
            }
        }
        for saved in self.saved.values_mut() {
            for slot in saved.iter_mut() {
                if matches!(slot, Some(s) if freed.contains(&s.backing)) {
                    *slot = None;
                }
            }
        }
    }

    /// Function 4Ch: the number of pages owned by `handle`.
    pub fn pages_for_handle(&self, handle: u16) -> Result<u16, u8> {
        self.handle(handle).map(|h| h.pages.len() as u16)
    }

    /// Function 4Bh: the number of active handles (the reserved OS handle aside).
    pub fn handle_count(&self) -> u16 {
        self.handles.iter().filter(|h| h.is_some()).count() as u16
    }

    /// Function 4Dh: (handle, page count) for every active handle.
    pub fn all_handles(&self) -> Vec<(u16, u16)> {
        self.handles
            .iter()
            .enumerate()
            .filter_map(|(id, h)| h.as_ref().map(|h| (id as u16, h.pages.len() as u16)))
            .collect()
    }

    /// Function 47h: save the current frame mapping under `handle`.
    pub fn save_context(&mut self, handle: u16) -> Result<(), u8> {
        self.handle(handle)?;
        if self.saved.contains_key(&handle) {
            return Err(status::CONTEXT_ALREADY_SAVED);
        }
        self.saved.insert(handle, self.frame_map);
        Ok(())
    }

    /// Function 48h: restore the frame mapping saved under `handle`.
    pub fn restore_context(&mut self, handle: u16) -> Result<(), u8> {
        self.handle(handle)?;
        let map = self.saved.remove(&handle).ok_or(status::NO_SAVED_CONTEXT)?;
        self.frame_map = map;
        Ok(())
    }

    /// Function 51h: grow or shrink `handle` to `new_pages`. Growing draws more
    /// free pages (88h when too few); shrinking returns the trailing pages and
    /// unmaps any frame slot that pointed at one.
    pub fn reallocate(&mut self, handle: u16, new_pages: u16) -> Result<(), u8> {
        let cur = self.handle(handle)?.pages.len();
        let new = usize::from(new_pages);
        match new.cmp(&cur) {
            std::cmp::Ordering::Greater => {
                let need = new - cur;
                if need > self.free_pages() {
                    return Err(status::FREE_EXCEEDED);
                }
                let extra = self.take_free_pages(need);
                self.handles[usize::from(handle)]
                    .as_mut()
                    .unwrap()
                    .pages
                    .extend(extra);
            }
            std::cmp::Ordering::Less => {
                let freed = self.handles[usize::from(handle)]
                    .as_mut()
                    .unwrap()
                    .pages
                    .split_off(new);
                for page in &freed {
                    self.free[*page as usize] = true;
                }
                self.invalidate_freed(&freed);
            }
            std::cmp::Ordering::Equal => {}
        }
        Ok(())
    }

    /// Function 53h AL=01: set the 8-byte handle name.
    pub fn set_name(&mut self, handle: u16, name: [u8; 8]) -> Result<(), u8> {
        let idx = usize::from(handle);
        let h = self
            .handles
            .get_mut(idx)
            .and_then(|x| x.as_mut())
            .ok_or(status::INVALID_HANDLE)?;
        h.name = name;
        Ok(())
    }

    /// Function 53h AL=00: get the 8-byte handle name.
    pub fn name(&self, handle: u16) -> Result<[u8; 8], u8> {
        self.handle(handle).map(|h| h.name)
    }

    /// The physical RAM address of byte `offset` within logical page `logical` of
    /// `handle`, for the move/exchange copy (function 57h). `offset` may run past a
    /// page; the page step is followed through the handle's (not necessarily
    /// contiguous) backing pages. Errors on a bad handle or an out-of-range page.
    pub fn backing_addr(&self, handle: u16, logical: u16, offset: u32) -> Result<u32, u8> {
        let h = self.handle(handle)?;
        let page = usize::from(logical) + (offset / EMS_PAGE_SIZE) as usize;
        let within = offset % EMS_PAGE_SIZE;
        let backing = *h.pages.get(page).ok_or(status::INVALID_LOGICAL_PAGE)?;
        Ok(self.region_base + backing * EMS_PAGE_SIZE + within)
    }

    /// The page-frame segment of physical slot `phys` (0-3), for function 58h's
    /// mappable-address array. Each slot is 16 KiB (0x400 paragraphs) on from the
    /// frame base.
    pub fn slot_segment(&self, phys: u8) -> u16 {
        self.frame_seg + u16::from(phys) * 0x400
    }

    /// Whether `addr` lies in the 64 KiB page-frame window. The bus uses this to
    /// keep the non-EMS hot path off the per-byte resolve.
    pub fn in_frame(&self, addr: u32) -> bool {
        let base = u32::from(self.frame_seg) << 4;
        (base..base + (FRAME_SLOTS as u32) * EMS_PAGE_SIZE).contains(&addr)
    }

    /// Resolve a physical address inside the page frame to the backing RAM address
    /// of the mapped page, or None when the address is outside the frame or its
    /// slot is unmapped (the bus then leaves the access on flat RAM). This is the
    /// read path the MachineBus EMS alias uses.
    pub fn resolve(&self, addr: u32) -> Option<u32> {
        let frame_base = u32::from(self.frame_seg) << 4;
        let frame_end = frame_base + (FRAME_SLOTS as u32) * EMS_PAGE_SIZE;
        if addr < frame_base || addr >= frame_end {
            return None;
        }
        let offset = addr - frame_base;
        let slot = (offset / EMS_PAGE_SIZE) as usize;
        let in_page = offset % EMS_PAGE_SIZE;
        let mapped = self.frame_map[slot]?;
        Some(self.region_base + mapped.backing * EMS_PAGE_SIZE + in_page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manager with a 64-page (1 MiB) region based at 0x110000, frame at 0xE000.
    fn manager() -> EmsState {
        EmsState::new(0xE000, 0x11_0000, 64)
    }

    #[test]
    fn allocate_consumes_free_pages_and_returns_a_handle() {
        let mut ems = manager();
        assert_eq!(ems.page_counts(), (64, 64));
        let h = ems.allocate(10).unwrap();
        assert!(h >= 1, "app handles start at 1");
        assert_eq!(ems.page_counts(), (54, 64));
        assert_eq!(ems.pages_for_handle(h).unwrap(), 10);
        assert_eq!(ems.handle_count(), 1);
    }

    #[test]
    fn allocate_rejects_zero_too_many_and_no_free() {
        let mut ems = manager();
        assert_eq!(ems.allocate(0), Err(status::ZERO_PAGES));
        assert_eq!(ems.allocate(65), Err(status::TOTAL_EXCEEDED));
        ems.allocate(64).unwrap(); // drains the pool
        assert_eq!(ems.allocate(1), Err(status::FREE_EXCEEDED));
    }

    #[test]
    fn map_resolves_a_frame_address_to_its_backing_page() {
        let mut ems = manager();
        let h = ems.allocate(4).unwrap();
        // Map logical page 2 into physical slot 1 (frame 0xE000, slot 1 = 0xE4000).
        ems.map(1, 2, h).unwrap();
        let backing = ems.resolve(0xE_4000 + 0x123).unwrap();
        // The handle's third page (index 2) backs the slot; with a fresh pool the
        // handle owns backing pages 0..4, so logical page 2 is backing page 2.
        assert_eq!(backing, 0x11_0000 + 2 * EMS_PAGE_SIZE + 0x123);
        // An unmapped slot resolves to nothing.
        assert_eq!(ems.resolve(0xE_0000), None); // slot 0 never mapped
        // Outside the frame resolves to nothing.
        assert_eq!(ems.resolve(0xF_0000), None);
    }

    #[test]
    fn map_validates_handle_physical_and_logical() {
        let mut ems = manager();
        let h = ems.allocate(2).unwrap();
        assert_eq!(ems.map(0, 0, 999), Err(status::INVALID_HANDLE));
        assert_eq!(ems.map(4, 0, h), Err(status::INVALID_PHYSICAL_PAGE));
        assert_eq!(ems.map(0, 2, h), Err(status::INVALID_LOGICAL_PAGE));
        // 0xFFFF unmaps without needing a valid logical page.
        ems.map(0, 0, h).unwrap();
        ems.map(0, 0xFFFF, h).unwrap();
        assert_eq!(ems.resolve(0xE_0000), None);
    }

    #[test]
    fn release_frees_pages_and_clears_slots() {
        let mut ems = manager();
        let h = ems.allocate(4).unwrap();
        ems.map(0, 0, h).unwrap();
        assert!(ems.resolve(0xE_0000).is_some());
        ems.release(h).unwrap();
        assert_eq!(ems.page_counts(), (64, 64), "pages returned to the pool");
        assert_eq!(ems.resolve(0xE_0000), None, "the frame slot is cleared");
        assert_eq!(ems.pages_for_handle(h), Err(status::INVALID_HANDLE));
        // The freed handle id is reused by the next allocation.
        assert_eq!(ems.allocate(1).unwrap(), h);
    }

    #[test]
    fn save_and_restore_round_trip_the_frame_map() {
        let mut ems = manager();
        let h = ems.allocate(2).unwrap();
        ems.map(0, 0, h).unwrap();
        let mapped = ems.resolve(0xE_0000).unwrap();
        ems.save_context(h).unwrap();
        assert_eq!(ems.save_context(h), Err(status::CONTEXT_ALREADY_SAVED));
        // Remap the slot, then restore brings the saved mapping back.
        ems.map(0, 1, h).unwrap();
        assert_ne!(ems.resolve(0xE_0000).unwrap(), mapped);
        ems.restore_context(h).unwrap();
        assert_eq!(ems.resolve(0xE_0000).unwrap(), mapped);
        assert_eq!(ems.restore_context(h), Err(status::NO_SAVED_CONTEXT));
    }

    #[test]
    fn reallocate_grows_then_shrinks_and_clears_a_freed_slot() {
        let mut ems = manager();
        let h = ems.allocate(4).unwrap();
        ems.reallocate(h, 10).unwrap();
        assert_eq!(ems.pages_for_handle(h).unwrap(), 10);
        assert_eq!(ems.page_counts().0, 64 - 10);
        // Map a high logical page, then shrink below it: its frame slot must clear.
        ems.map(0, 8, h).unwrap();
        assert!(ems.resolve(0xE_0000).is_some());
        ems.reallocate(h, 4).unwrap();
        assert_eq!(ems.pages_for_handle(h).unwrap(), 4);
        assert_eq!(ems.page_counts().0, 64 - 4, "the trailing pages are freed");
        assert_eq!(
            ems.resolve(0xE_0000),
            None,
            "the slot on a freed page is cleared"
        );
    }

    #[test]
    fn restore_cannot_alias_a_page_reassigned_after_a_shrink() {
        let mut ems = manager(); // 64 pages
        let a = ems.allocate(4).unwrap(); // backing 0,1,2,3
        ems.map(0, 3, a).unwrap(); // slot 0 -> backing 3
        ems.save_context(a).unwrap(); // snapshot slot 0 = backing 3
        ems.map(0, 0, a).unwrap(); // remap slot 0 -> backing 0 (survives the shrink)
        ems.reallocate(a, 1).unwrap(); // frees backing 1,2,3 and scrubs the saved slot
        let b = ems.allocate(3).unwrap(); // first-fit reclaims backing 1,2,3
        ems.map(0, 2, b).unwrap(); // B owns backing 3 now (its logical page 2)
        ems.restore_context(a).unwrap();
        // A's saved slot referenced a freed page, so the restore leaves it unmapped
        // rather than aliasing B's reassigned page.
        assert_eq!(
            ems.resolve(0xE_0000),
            None,
            "the restore must not alias B's page"
        );
    }

    #[test]
    fn release_scrubs_other_handles_saved_contexts_for_the_freed_pages() {
        let mut ems = manager();
        let a = ems.allocate(2).unwrap(); // backing 0,1
        ems.map(0, 0, a).unwrap(); // slot 0 -> A's backing 0
        let c = ems.allocate(1).unwrap(); // backing 2
        ems.save_context(c).unwrap(); // C snapshots slot 0 = {A, backing 0}
        ems.release(a).unwrap(); // frees A's pages 0,1 and scrubs C's saved slot
        let _b = ems.allocate(2).unwrap(); // reclaims backing 0,1
        ems.restore_context(c).unwrap();
        assert_eq!(
            ems.resolve(0xE_0000),
            None,
            "C's restore must not alias the reassigned page"
        );
    }

    #[test]
    fn handle_name_round_trips() {
        let mut ems = manager();
        let h = ems.allocate(1).unwrap();
        assert_eq!(ems.name(h).unwrap(), [0u8; 8]);
        ems.set_name(h, *b"MYHANDLE").unwrap();
        assert_eq!(&ems.name(h).unwrap(), b"MYHANDLE");
        assert_eq!(ems.set_name(99, [0u8; 8]), Err(status::INVALID_HANDLE));
    }

    #[test]
    fn backing_addr_steps_through_a_handles_pages() {
        let mut ems = manager();
        let h = ems.allocate(3).unwrap();
        assert_eq!(ems.backing_addr(h, 0, 0x100).unwrap(), 0x11_0000 + 0x100);
        // An offset past a page boundary steps to the next logical (backing) page.
        assert_eq!(
            ems.backing_addr(h, 0, EMS_PAGE_SIZE + 5).unwrap(),
            0x11_0000 + EMS_PAGE_SIZE + 5
        );
        // Past the handle's last page is an invalid logical page.
        assert_eq!(
            ems.backing_addr(h, 0, 3 * EMS_PAGE_SIZE),
            Err(status::INVALID_LOGICAL_PAGE)
        );
    }

    #[test]
    fn slot_segment_steps_by_16k() {
        let ems = manager(); // frame at 0xE000
        assert_eq!(ems.slot_segment(0), 0xE000);
        assert_eq!(ems.slot_segment(1), 0xE400);
        assert_eq!(ems.slot_segment(3), 0xEC00);
    }

    #[test]
    fn handles_run_out_at_the_ceiling() {
        let mut ems = EmsState::new(0xE000, 0x11_0000, 256);
        for _ in 0..EmsState::MAX_HANDLES {
            ems.allocate(1).unwrap();
        }
        assert_eq!(ems.handle_count(), EmsState::MAX_HANDLES as u16);
        assert_eq!(ems.allocate(1), Err(status::NO_MORE_HANDLES));
    }
}
