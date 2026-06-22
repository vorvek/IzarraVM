//! XMS (eXtended Memory Specification 3.0) driver state and the Extended Memory
//! Block (EMB) allocator.
//!
//! This is the host side of a HIMEM.SYS-style driver. The guest gets the driver
//! entry point from INT 2Fh AX=4310h (an `INT 66h; RETF` stub in ROM), then
//! FAR-CALLs it with a function selector in AH. The trap lands in the machine's
//! soft-INT dispatch, which reads the guest registers and calls
//! [`XmsState::function`] and the A20/move/lock helpers here.
//!
//! Functions return AX=1 on success or AX=0 with an error code in BL on failure,
//! the standard XMS convention. Error codes are the documented HIMEM values.
//!
//! The A20 gate is deliberately NOT tracked here: the machine's single source of
//! truth is the 8042 output-port bit 1 (shared with fast-A20 port 0x92), so the
//! caller routes the global/local A20 functions to that. This module only keeps
//! the local-enable nesting counter the spec requires.

/// XMS error codes (BL on failure), from the XMS 3.0 specification.
pub mod err {
    /// Function not implemented.
    pub const NOT_IMPLEMENTED: u8 = 0x80;
    /// HMA does not exist.
    pub const HMA_DOES_NOT_EXIST: u8 = 0x90;
    /// HMA is already in use.
    pub const HMA_IN_USE: u8 = 0x91;
    /// DX (space requested) is less than the /HMAMIN= parameter.
    pub const HMA_MIN_NOT_MET: u8 = 0x92;
    /// HMA is not allocated.
    pub const HMA_NOT_ALLOCATED: u8 = 0x93;
    /// All extended memory is allocated.
    pub const OUT_OF_MEMORY: u8 = 0xa0;
    /// All available handles are in use.
    pub const OUT_OF_HANDLES: u8 = 0xa1;
    /// Invalid handle.
    pub const INVALID_HANDLE: u8 = 0xa2;
    /// Source handle is invalid (move).
    pub const INVALID_SOURCE_HANDLE: u8 = 0xa3;
    /// Source offset is invalid (move).
    pub const INVALID_SOURCE_OFFSET: u8 = 0xa4;
    /// Destination handle is invalid (move).
    pub const INVALID_DEST_HANDLE: u8 = 0xa5;
    /// Destination offset is invalid (move).
    pub const INVALID_DEST_OFFSET: u8 = 0xa6;
    /// Length is invalid (move).
    pub const INVALID_LENGTH: u8 = 0xa7;
    /// Block is not locked (unlock of an unlocked block).
    pub const BLOCK_NOT_LOCKED: u8 = 0xaa;
    /// Block is locked (free/resize of a locked block).
    pub const BLOCK_LOCKED: u8 = 0xab;
    /// Lock count overflowed.
    pub const LOCK_COUNT_OVERFLOW: u8 = 0xac;
    /// Request UMB (10h) / Reallocate UMB (12h): only a smaller block is available;
    /// DX holds the largest available size.
    pub const UMB_SMALLER_AVAILABLE: u8 = 0xb0;
    /// Request UMB (10h): no upper memory blocks are available.
    pub const NO_UMB_AVAILABLE: u8 = 0xb1;
    /// Release/Reallocate UMB (11h/12h): the UMB segment number is invalid.
    pub const INVALID_UMB_SEGMENT: u8 = 0xb2;
}

/// Linear base address of extended memory: the first byte above the 1 MB line.
pub const EXTENDED_MEMORY_BASE: u32 = 0x10_0000;

/// Size of the High Memory Area in KB (just under 64 KB: 0xFFFF bytes from the
/// 1 MB line, the classic 64 KB minus 16 bytes).
pub const HMA_SIZE_KB: u16 = 64;

/// The driver revision reported in BX by function 00h (get version). 0x0300 ==
/// "3.00", matching the XMS 3.0 spec level the function table implements.
pub const DRIVER_REVISION: u16 = 0x0300;

/// One Extended Memory Block: its size, how many times the guest has locked it,
/// and where it lives in the flat address space. `base_linear` is the 32-bit
/// physical address a lock (function 0Ch) hands back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Emb {
    pub size_kb: u16,
    pub lock_count: u8,
    pub base_linear: u32,
}

/// The allocation seam. A backend owns the extended-memory region above the HMA
/// and hands out [`Emb`] blocks by handle. The default [`FlatEmbAllocator`] is a
/// bump allocator with a free list; the planned memory mapper (the other half of
/// the memory equation) will implement this trait to back EMBs with real mapped
/// pages instead of just reserving linear ranges over flat RAM.
///
/// Handles are 1-based; handle 0 is reserved by the XMS move call to mean "the
/// offset is a real-mode seg:off linear address", so it is never a valid block.
pub trait EmbBackend: std::fmt::Debug {
    /// Allocate `size_kb` KB. Returns a nonzero handle on success, or an error
    /// code (`err::OUT_OF_MEMORY` / `err::OUT_OF_HANDLES`) on failure.
    fn allocate(&mut self, size_kb: u16) -> Result<u16, u8>;
    /// Free the block. Errors with `err::INVALID_HANDLE` or `err::BLOCK_LOCKED`.
    fn free(&mut self, handle: u16) -> Result<(), u8>;
    /// Look up a block by handle.
    fn get(&self, handle: u16) -> Option<&Emb>;
    /// Look up a block mutably (for lock-count changes).
    fn get_mut(&mut self, handle: u16) -> Option<&mut Emb>;
    /// Largest free run and total free memory, both in KB. Function 08h reports
    /// these.
    fn free_kb(&self) -> (u32, u32);
    /// Count of handles still available for allocation. Function 0Eh reports it.
    fn free_handles(&self) -> u8;
    /// Resize a block in place to `size_kb`. Errors as allocate plus
    /// `err::BLOCK_LOCKED`.
    fn resize(&mut self, handle: u16, size_kb: u16) -> Result<(), u8>;
}

/// Number of EMB handles the flat allocator exposes. Real HIMEM defaults to 32;
/// this matches it so a guest's handle table sizing assumptions hold.
const MAX_HANDLES: usize = 32;

/// A first-fit allocator over a fixed linear window above the HMA.
///
/// ponytail: this reserves linear ranges over already-flat RAM and never touches
/// a page table, so an EMB at base B is just the bytes at physical B. It uses a
/// fixed handle table and a coalescing first-fit free list. Good enough to run a
/// game that allocates a few large blocks. The upgrade path is the memory mapper
/// (a separate session): implement [`EmbBackend`] there to map discontiguous
/// physical pages, grow past the flat window, and decouple handle from linear
/// address. Swap it in by changing the [`Default`] for [`XmsState`].
#[derive(Debug, Clone)]
pub struct FlatEmbAllocator {
    /// First linear byte the allocator may hand out (above the HMA).
    base: u32,
    /// One past the last linear byte (base + window size).
    end: u32,
    /// Live blocks, indexed by handle-1. `None` is a free slot.
    blocks: Vec<Option<Emb>>,
}

impl FlatEmbAllocator {
    /// Build an allocator over `[base, base + size_kb*1024)`. `base` is the linear
    /// address of the first managed byte (the machine passes the first byte above
    /// the HMA), and `size_kb` is how much extended memory is free for EMBs.
    pub fn new(base: u32, size_kb: u32) -> Self {
        let end = base.saturating_add(size_kb.saturating_mul(1024));
        Self {
            base,
            end,
            blocks: vec![None; MAX_HANDLES],
        }
    }

    /// Total managed window in KB.
    fn window_kb(&self) -> u32 {
        (self.end - self.base) / 1024
    }

    /// Sum of every live block's size in KB.
    fn used_kb(&self) -> u32 {
        self.blocks
            .iter()
            .flatten()
            .map(|b| u32::from(b.size_kb))
            .sum()
    }

    /// First-fit search for a `size_kb` gap, returning its linear base. Walks the
    /// live blocks sorted by base and looks for a hole big enough, including the
    /// gap after the last block. A zero-size request is legal (HIMEM allows a
    /// 0 KB handle) and lands at the first free byte.
    fn find_gap(&self, size_kb: u16) -> Option<u32> {
        let need = u32::from(size_kb) * 1024;
        let mut bases: Vec<(u32, u32)> = self
            .blocks
            .iter()
            .flatten()
            .map(|b| (b.base_linear, u32::from(b.size_kb) * 1024))
            .collect();
        bases.sort_unstable_by_key(|&(base, _)| base);
        let mut cursor = self.base;
        for (base, len) in bases {
            if base.saturating_sub(cursor) >= need {
                return Some(cursor);
            }
            cursor = base + len;
        }
        if self.end.saturating_sub(cursor) >= need {
            Some(cursor)
        } else {
            None
        }
    }
}

impl EmbBackend for FlatEmbAllocator {
    fn allocate(&mut self, size_kb: u16) -> Result<u16, u8> {
        let base = self.find_gap(size_kb).ok_or(err::OUT_OF_MEMORY)?;
        let slot = self
            .blocks
            .iter()
            .position(Option::is_none)
            .ok_or(err::OUT_OF_HANDLES)?;
        self.blocks[slot] = Some(Emb {
            size_kb,
            lock_count: 0,
            base_linear: base,
        });
        // Handles are 1-based so handle 0 stays the move call's "real-mode address"
        // sentinel.
        Ok(slot as u16 + 1)
    }

    fn free(&mut self, handle: u16) -> Result<(), u8> {
        let slot = self.slot(handle)?;
        let block = self.blocks[slot].as_ref().ok_or(err::INVALID_HANDLE)?;
        if block.lock_count != 0 {
            return Err(err::BLOCK_LOCKED);
        }
        self.blocks[slot] = None;
        Ok(())
    }

    fn get(&self, handle: u16) -> Option<&Emb> {
        let slot = self.slot(handle).ok()?;
        self.blocks[slot].as_ref()
    }

    fn get_mut(&mut self, handle: u16) -> Option<&mut Emb> {
        let slot = self.slot(handle).ok()?;
        self.blocks[slot].as_mut()
    }

    fn free_kb(&self) -> (u32, u32) {
        let total = self.window_kb().saturating_sub(self.used_kb());
        // Largest free run: walk the gaps the way find_gap does, but track the max.
        let mut bases: Vec<(u32, u32)> = self
            .blocks
            .iter()
            .flatten()
            .map(|b| (b.base_linear, u32::from(b.size_kb) * 1024))
            .collect();
        bases.sort_unstable_by_key(|&(base, _)| base);
        let mut cursor = self.base;
        let mut largest = 0u32;
        for (base, len) in bases {
            largest = largest.max(base.saturating_sub(cursor));
            cursor = base + len;
        }
        largest = largest.max(self.end.saturating_sub(cursor));
        (largest / 1024, total)
    }

    fn free_handles(&self) -> u8 {
        self.blocks.iter().filter(|b| b.is_none()).count() as u8
    }

    fn resize(&mut self, handle: u16, size_kb: u16) -> Result<(), u8> {
        let slot = self.slot(handle)?;
        let block = self.blocks[slot].ok_or(err::INVALID_HANDLE)?;
        if block.lock_count != 0 {
            return Err(err::BLOCK_LOCKED);
        }
        if size_kb == block.size_kb {
            return Ok(());
        }
        // Free the old block, then place the new size. If the new size does not
        // fit, restore the original so resize is all-or-nothing.
        self.blocks[slot] = None;
        match self.find_gap(size_kb) {
            Some(base) => {
                self.blocks[slot] = Some(Emb {
                    size_kb,
                    lock_count: 0,
                    base_linear: base,
                });
                Ok(())
            }
            None => {
                self.blocks[slot] = Some(block);
                Err(err::OUT_OF_MEMORY)
            }
        }
    }
}

impl FlatEmbAllocator {
    /// Map a 1-based handle to a table slot, validating the range.
    fn slot(&self, handle: u16) -> Result<usize, u8> {
        if handle == 0 || usize::from(handle) > self.blocks.len() {
            return Err(err::INVALID_HANDLE);
        }
        Ok(usize::from(handle) - 1)
    }
}

/// The backing region reserved for EMS at the top of extended RAM: its physical
/// base and how many 16 KiB pages it holds. The XMS/EMS partition returns this so
/// the machine builds the EMS manager over the same physical pool XMS draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmsRegion {
    pub base: u32,
    pub total_pages: u16,
}

/// Driver-level XMS state: the EMB allocator behind the [`EmbBackend`] seam, the
/// HMA-allocation flag, and the local-A20 nesting counter.
#[derive(Debug)]
pub struct XmsState {
    backend: Box<dyn EmbBackend + Send>,
    /// True once a guest has claimed the HMA (function 01h) and not released it.
    hma_allocated: bool,
    /// The `/HMAMIN=` threshold in bytes: function 01h rejects a request whose DX
    /// (space needed) is below this, so the HMA stays free for a larger claimant.
    /// Default 0 grants any request, matching HIMEM with no /HMAMIN given.
    hma_min_bytes: u32,
    /// Local-enable nesting count for A20 functions 05h/06h. The spec lets a
    /// program enable A20 locally several times; A20 only drops when the count
    /// reaches zero again. The actual gate lives in the 8042, driven by the caller.
    local_a20_count: u32,
}

impl XmsState {
    /// Build the state for a machine with `memory_mib` of RAM, with no EMS
    /// reservation (XMS owns all of extended memory above the HMA).
    pub fn new(memory_mib: u16) -> Self {
        Self::new_with_ems(memory_mib, 0).0
    }

    /// Partition extended RAM between XMS and EMS. EMS takes `ems_want_kb` (clamped
    /// to whole 16 KiB pages and to what extended memory can spare) at the TOP of
    /// the pool; XMS gets the rest. EMS is simulated from extended memory the way
    /// EMM386 does, so the two share one physical pool rather than double-counting.
    /// Returns the XMS state and, when the EMS share is non-zero, its backing region.
    pub fn new_with_ems(memory_mib: u16, ems_want_kb: u32) -> (Self, Option<EmsRegion>) {
        let total_kb = u32::from(memory_mib) * 1024;
        // Extended memory is RAM above 1 MB; the top 64 KB of it is the HMA.
        let extended_kb = total_kb.saturating_sub(1024);
        let usable_kb = extended_kb.saturating_sub(u32::from(HMA_SIZE_KB));
        // EMS is carved in whole 16 KiB pages and never takes more than is there.
        let ems_kb = (ems_want_kb.min(usable_kb) / 16) * 16;
        let xms_pool_kb = usable_kb - ems_kb;
        let pool_base = EXTENDED_MEMORY_BASE + u32::from(HMA_SIZE_KB) * 1024;
        let state = Self {
            backend: Box::new(FlatEmbAllocator::new(pool_base, xms_pool_kb)),
            hma_allocated: false,
            hma_min_bytes: 0,
            local_a20_count: 0,
        };
        let region = (ems_kb > 0).then(|| EmsRegion {
            base: pool_base + xms_pool_kb * 1024,
            total_pages: (ems_kb / 16) as u16,
        });
        (state, region)
    }

    /// Set the `/HMAMIN=` threshold in KB. Function 01h then rejects a request
    /// below this size with [`err::HMA_MIN_NOT_MET`]. HIMEM accepts 0-63; the
    /// value is clamped to 63 so the threshold can never exceed the HMA itself
    /// (a threshold of 64 KB or more would reject even the 0xFFFF whole-HMA
    /// request and leave the HMA permanently unclaimable).
    pub fn set_hma_min_kb(&mut self, kb: u16) {
        self.hma_min_bytes = u32::from(kb.min(63)) * 1024;
    }

    /// Function 00h reports DX=1 when the HMA exists. The Izarra 3000 always has
    /// memory above 1 MB, so the HMA region is always present.
    pub fn hma_exists(&self) -> bool {
        true
    }

    /// Try to claim the HMA (function 01h). `space_needed` is DX, the bytes the
    /// caller wants (an application passes 0xFFFF for the whole HMA). Per the XMS
    /// spec the order is: exists, not already in use, then the /HMAMIN size gate.
    /// Returns the error code on failure.
    pub fn request_hma(&mut self, space_needed: u16) -> Result<(), u8> {
        if !self.hma_exists() {
            return Err(err::HMA_DOES_NOT_EXIST);
        }
        if self.hma_allocated {
            return Err(err::HMA_IN_USE);
        }
        if u32::from(space_needed) < self.hma_min_bytes {
            return Err(err::HMA_MIN_NOT_MET);
        }
        self.hma_allocated = true;
        Ok(())
    }

    /// Release the HMA (function 02h).
    pub fn release_hma(&mut self) -> Result<(), u8> {
        if !self.hma_allocated {
            return Err(err::HMA_NOT_ALLOCATED);
        }
        self.hma_allocated = false;
        Ok(())
    }

    /// Note a local A20 enable (function 05h) and report whether this is the
    /// nesting transition that should actually drive the gate on (count 0 -> 1).
    pub fn local_enable_a20(&mut self) -> bool {
        let was_zero = self.local_a20_count == 0;
        self.local_a20_count = self.local_a20_count.saturating_add(1);
        was_zero
    }

    /// Note a local A20 disable (function 06h) and report whether this is the
    /// transition that should drive the gate off (count 1 -> 0). A disable with
    /// the count already at zero is a no-op that still drives off.
    pub fn local_disable_a20(&mut self) -> bool {
        self.local_a20_count = self.local_a20_count.saturating_sub(1);
        self.local_a20_count == 0
    }

    /// Free extended memory (function 08h): (largest free KB, total free KB).
    pub fn query_free(&self) -> (u32, u32) {
        self.backend.free_kb()
    }

    /// Allocate an EMB of `size_kb` (function 09h). Returns the handle.
    pub fn allocate(&mut self, size_kb: u16) -> Result<u16, u8> {
        self.backend.allocate(size_kb)
    }

    /// Free an EMB (function 0Ah).
    pub fn free(&mut self, handle: u16) -> Result<(), u8> {
        self.backend.free(handle)
    }

    /// Lock an EMB (function 0Ch): bump its lock count and return its 32-bit
    /// linear base. The base is what the guest puts in DX:BX.
    pub fn lock(&mut self, handle: u16) -> Result<u32, u8> {
        let block = self.backend.get_mut(handle).ok_or(err::INVALID_HANDLE)?;
        block.lock_count = block
            .lock_count
            .checked_add(1)
            .ok_or(err::LOCK_COUNT_OVERFLOW)?;
        Ok(block.base_linear)
    }

    /// Unlock an EMB (function 0Dh): drop its lock count by one.
    pub fn unlock(&mut self, handle: u16) -> Result<(), u8> {
        let block = self.backend.get_mut(handle).ok_or(err::INVALID_HANDLE)?;
        if block.lock_count == 0 {
            return Err(err::BLOCK_NOT_LOCKED);
        }
        block.lock_count -= 1;
        Ok(())
    }

    /// Handle info (function 0Eh): (lock count, free handles, size KB).
    pub fn handle_info(&self, handle: u16) -> Result<(u8, u8, u16), u8> {
        let block = self.backend.get(handle).ok_or(err::INVALID_HANDLE)?;
        Ok((block.lock_count, self.backend.free_handles(), block.size_kb))
    }

    /// Resize an EMB (function 0Fh).
    pub fn resize(&mut self, handle: u16, size_kb: u16) -> Result<(), u8> {
        self.backend.resize(handle, size_kb)
    }

    /// Resolve a move endpoint (handle, offset) into a linear address. Handle 0
    /// means the offset is a real-mode seg:off pair (segment in the high word,
    /// offset in the low word), per the XMS move descriptor. A nonzero handle
    /// adds the offset to the block's linear base, validating the handle and that
    /// the range stays inside the block. Used by function 0Bh.
    pub fn move_endpoint(
        &self,
        handle: u16,
        offset: u32,
        length: u32,
        bad_handle: u8,
        bad_offset: u8,
    ) -> Result<u32, u8> {
        if handle == 0 {
            // Real-mode pointer: high word segment, low word offset -> linear.
            let seg = u32::from((offset >> 16) as u16);
            let off = u32::from(offset as u16);
            return Ok(seg * 16 + off);
        }
        let block = self.backend.get(handle).ok_or(bad_handle)?;
        let size = u32::from(block.size_kb) * 1024;
        // Both fields come from a guest descriptor, so compare without overflowing:
        // offset <= size is checked first, then length against the remaining space.
        if offset > size || length > size - offset {
            return Err(bad_offset);
        }
        Ok(block.base_linear + offset)
    }
}

impl Default for XmsState {
    /// A machine with no profile yet defaults to the Izarra 3000's 16 MB so tests
    /// and the boot path get a usable EMB pool before `new` runs.
    fn default() -> Self {
        Self::new(16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_revision_is_three_oh_oh() {
        assert_eq!(DRIVER_REVISION, 0x0300);
    }

    #[test]
    fn allocate_lock_unlock_free_round_trip() {
        let mut xms = XmsState::new(16);
        let (_, total_before) = xms.query_free();
        let handle = xms.allocate(64).expect("allocate 64 KB");
        assert_ne!(handle, 0, "handle is 1-based, never 0");

        // A locked block hands back a linear address at or above 1 MB.
        let linear = xms.lock(handle).expect("lock");
        assert!(
            linear >= EXTENDED_MEMORY_BASE,
            "locked EMB lives above 1 MB, got {linear:#x}"
        );

        // It is above the HMA, too (the first 64 KB of extended memory).
        assert!(linear >= EXTENDED_MEMORY_BASE + u32::from(HMA_SIZE_KB) * 1024);

        // A locked block cannot be freed.
        assert_eq!(xms.free(handle), Err(err::BLOCK_LOCKED));

        xms.unlock(handle).expect("unlock");
        xms.free(handle).expect("free after unlock");

        // The KB return to the pool.
        let (_, total_after) = xms.query_free();
        assert_eq!(total_after, total_before, "freeing returns the KB");
    }

    #[test]
    fn query_free_reflects_an_allocation() {
        let mut xms = XmsState::new(16);
        let (_, total_before) = xms.query_free();
        let handle = xms.allocate(128).expect("allocate 128 KB");
        let (_, total_after) = xms.query_free();
        assert_eq!(
            total_before - total_after,
            128,
            "free total drops by the allocation"
        );
        // The handle info reports the size back.
        let (lock, _free, size) = xms.handle_info(handle).expect("handle info");
        assert_eq!(lock, 0);
        assert_eq!(size, 128);
    }

    #[test]
    fn unlock_an_unlocked_block_errors() {
        let mut xms = XmsState::new(16);
        let handle = xms.allocate(16).unwrap();
        assert_eq!(xms.unlock(handle), Err(err::BLOCK_NOT_LOCKED));
    }

    #[test]
    fn invalid_handle_is_rejected() {
        let mut xms = XmsState::new(16);
        assert_eq!(xms.lock(0), Err(err::INVALID_HANDLE));
        assert_eq!(xms.free(99), Err(err::INVALID_HANDLE));
        assert_eq!(xms.handle_info(1), Err(err::INVALID_HANDLE));
    }

    #[test]
    fn hma_claim_is_exclusive() {
        let mut xms = XmsState::new(16);
        // 0xFFFF is the "application wants the whole HMA" request.
        xms.request_hma(0xFFFF).expect("first claim");
        assert_eq!(xms.request_hma(0xFFFF), Err(err::HMA_IN_USE));
        xms.release_hma().expect("release");
        assert_eq!(xms.release_hma(), Err(err::HMA_NOT_ALLOCATED));
    }

    #[test]
    fn hma_request_below_hmamin_is_rejected() {
        let mut xms = XmsState::new(16);
        xms.set_hma_min_kb(16); // /HMAMIN=16 -> 16384 bytes
        // A small TSR-sized request is refused so the HMA stays free for DOS.
        assert_eq!(xms.request_hma(8 * 1024), Err(err::HMA_MIN_NOT_MET));
        // The HMA is still free after a rejected request.
        assert!(!xms.hma_allocated);
        // A request at or above the threshold (or the whole-HMA sentinel) succeeds.
        xms.request_hma(0xFFFF)
            .expect("a full-HMA claim clears /HMAMIN");
    }

    #[test]
    fn default_hmamin_grants_any_size() {
        let mut xms = XmsState::new(16);
        // With no /HMAMIN, even a zero-size request is granted (HIMEM default).
        xms.request_hma(0)
            .expect("default /HMAMIN=0 grants any request");
    }

    #[test]
    fn hmamin_is_clamped_so_the_hma_stays_claimable() {
        let mut xms = XmsState::new(16);
        // An out-of-range /HMAMIN must not exceed the HMA: the whole-HMA request
        // still succeeds rather than the HMA becoming permanently unclaimable.
        xms.set_hma_min_kb(64);
        xms.request_hma(0xFFFF)
            .expect("a clamped /HMAMIN still admits the whole-HMA request");
    }

    #[test]
    fn local_a20_count_nests() {
        let mut xms = XmsState::new(16);
        assert!(xms.local_enable_a20(), "0 -> 1 drives the gate on");
        assert!(!xms.local_enable_a20(), "1 -> 2 does not re-drive");
        assert!(!xms.local_disable_a20(), "2 -> 1 keeps it on");
        assert!(xms.local_disable_a20(), "1 -> 0 drives it off");
    }

    #[test]
    fn move_endpoint_real_mode_pointer() {
        let xms = XmsState::new(16);
        // Handle 0: B800:0010 -> linear 0xB8010.
        let seg_off = (0xB800u32 << 16) | 0x0010;
        let linear = xms
            .move_endpoint(
                0,
                seg_off,
                4,
                err::INVALID_SOURCE_HANDLE,
                err::INVALID_SOURCE_OFFSET,
            )
            .unwrap();
        assert_eq!(linear, 0xB8010);
    }

    #[test]
    fn move_endpoint_block_bounds_checked() {
        let mut xms = XmsState::new(16);
        let handle = xms.allocate(2).unwrap(); // 2 KB block
        let base = xms.lock(handle).unwrap();
        // Offset inside the block resolves to base + offset.
        let inside = xms
            .move_endpoint(
                handle,
                100,
                4,
                err::INVALID_DEST_HANDLE,
                err::INVALID_DEST_OFFSET,
            )
            .unwrap();
        assert_eq!(inside, base + 100);
        // A run past the end is rejected.
        assert_eq!(
            xms.move_endpoint(
                handle,
                2047,
                4,
                err::INVALID_DEST_HANDLE,
                err::INVALID_DEST_OFFSET
            ),
            Err(err::INVALID_DEST_OFFSET)
        );
    }

    #[test]
    fn resize_grows_and_shrinks() {
        let mut xms = XmsState::new(16);
        let handle = xms.allocate(64).unwrap();
        xms.resize(handle, 256).expect("grow");
        let (_, _, size) = xms.handle_info(handle).unwrap();
        assert_eq!(size, 256);
        xms.resize(handle, 32).expect("shrink");
        let (_, _, size) = xms.handle_info(handle).unwrap();
        assert_eq!(size, 32);
    }

    #[test]
    fn first_fit_reuses_a_freed_gap() {
        let mut xms = XmsState::new(16);
        let a = xms.allocate(64).unwrap();
        let _b = xms.allocate(64).unwrap();
        let base_a = xms.lock(a).unwrap();
        xms.unlock(a).unwrap();
        xms.free(a).unwrap();
        // The next allocation that fits takes the freed gap (lowest address).
        let c = xms.allocate(64).unwrap();
        let base_c = xms.lock(c).unwrap();
        assert_eq!(base_c, base_a, "first-fit reuses the freed low gap");
    }
}
