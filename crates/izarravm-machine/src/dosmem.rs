//! The DOS upper-memory-block (UMB) arena + MCB-chain primitives, extracted from
//! the retired `izarravm-dos` crate. This is the live memory-manager the IEMM
//! upper-memory setup (`furnish_dos_upper_memory`, which runs on every
//! `Machine::new`) depends on — it is NOT part of the HLE DOS kernel.
//!
//! The Request/Release/Reallocate UMB entry points (`request_umb`/`release_umb`/
//! `resize_umb` and their `DosMemory` wrappers) carve from the same MCB chain as
//! AH=48h-high. They lost their only caller (the host XMS driver's AH=10h/11h/12h
//! dispatch) when SP-4b M1 retired `xms.rs` in favor of a guest TOKAEMM.SYS driver
//! running in V86; they stay `#[allow(dead_code)]` because TOKAEMM's UMB support
//! (later in the same milestone) calls into this same engine through a V86 trap
//! instead of the old host dispatch.
//!
//! This is a byte-for-byte MOVE of the UMB-relevant closure out of
//! `izarravm-dos`'s `memory.rs`, not a redesign: the arena still walks and mutates
//! real MCB headers in guest RAM (`izarravm_bus::Memory`), so a debugger pointed at
//! the pool reads them like any other DOS arena. The only substantive change is the
//! error type — the DOS crate wrapped `BusError` in its own `DosError`; here the
//! failures are purely guest-memory reads/writes, so they surface `BusError`
//! directly.
//!
//! See dev_docs/2026-07-01-katea-sp3-hle-deletion-plan.md (Task 3a).

use izarravm_bus::{BusError, Memory};

/// The top of conventional memory in paragraphs: the 640 KiB line (0xA000
/// paragraphs = 0xA0000 bytes) where the video aperture begins. The UMB code needs
/// it only as the MCB-walk safety cap; the arena itself lives above it.
#[allow(dead_code)] // only feeds MCB_WALK_CAP, itself unused until TOKAEMM's UMB calls land.
const ARENA_TOP: u16 = 0xa000;

/// The 8-byte MCB owner name for a blank (free / manager-owned) block.
const NO_NAME: &[u8; 8] = b"\0\0\0\0\0\0\0\0";

/// AH=4Ah resize result details shared by conventional and upper-memory blocks.
#[allow(dead_code)] // only constructed by resize_umb, unused until TOKAEMM's UMB calls land.
enum ResizeError {
    TooBig(u16), // largest paragraphs that would fit
    InvalidBlock,
}

/// The AH=58h fit method the generic MCB allocator honours. The UMB entry points
/// (XMS Request/Reallocate UMB) only ever use first-fit today; `Best`/`Last` and
/// the `from_strategy` decoder are carried faithfully from the original allocator
/// so the AH=48h-high strategy path can reuse this engine unchanged once its
/// caller is extracted (the HLE seam that drove them dies in the next task).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Best/Last are unreachable until the AH=48h-high allocator moves here.
enum AllocFit {
    First,
    Best,
    Last,
}

impl AllocFit {
    #[allow(dead_code)] // AH=58h strategy decoder; no caller in the UMB-only closure yet.
    fn from_strategy(strategy: u16) -> Self {
        match strategy & 0x0003 {
            0x0001 => Self::Best,
            0x0002 => Self::Last,
            _ => Self::First,
        }
    }
}

/// The DOS upper-memory-block arena: a second authoritative MCB chain living in
/// the UMB-able window above conventional memory (0xC0000-0xEFFFF), in the holes
/// the memory manager leaves between option and system ROM. The machine's UMA
/// reservation map decides where it sits and how big it is and furnishes it
/// through `set_umb_region`; the chain itself is real MCB headers in guest RAM,
/// so a debugger pointed at the pool reads them like any other arena.
///
/// It is kept separate from the conventional chain rather than bridged across the
/// video aperture: a contiguous walk would have to plant an MCB header in the
/// 0xA0000 frame buffer. The link state (AH=5803h) gates whether allocation is
/// routed here, not whether the arena exists.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct UmbArena {
    /// Header paragraph of the first UMB MCB (the pool's base segment).
    first_mcb: u16,
    /// One paragraph past the pool: the ceiling the free tail reaches, the UMB
    /// analogue of `ARENA_TOP` for the conventional arena.
    top: u16,
}

#[allow(dead_code)] // only reached through request/release/resize_umb; see module docs.
impl UmbArena {
    fn contains_data(self, seg: u16) -> bool {
        (self.first_mcb..self.top).contains(&seg)
    }

    #[cfg(test)]
    fn chain(self, mem: &Memory) -> Vec<RamMcb> {
        read_mcb_chain(mem, self.first_mcb)
    }

    fn allocate(self, paras: u16, mem: &mut Memory) -> Result<Result<u16, u16>, BusError> {
        self.allocate_fit(AllocFit::First, paras, mem)
    }

    fn allocate_fit(
        self,
        fit: AllocFit,
        paras: u16,
        mem: &mut Memory,
    ) -> Result<Result<u16, u16>, BusError> {
        allocate_block(self.first_mcb, fit, paras, mem)
    }

    fn free(self, seg: u16, mem: &mut Memory) -> Result<Result<(), ()>, BusError> {
        free_block(self.first_mcb, self.top, seg, mem)
    }

    fn resize(
        self,
        seg: u16,
        paras: u16,
        mem: &mut Memory,
    ) -> Result<Result<(), ResizeError>, BusError> {
        resize_block(self.first_mcb, self.top, seg, paras, mem)
    }
}

/// Furnish or clear the upper-memory arena that DOS exposes to AH=48h high
/// allocations and XMS UMB calls.
fn set_umb_region(
    umb: &mut Option<UmbArena>,
    seg: u16,
    paras: u16,
    mem: &mut Memory,
) -> Result<(), BusError> {
    if paras < 2 {
        *umb = None;
        return Ok(());
    }
    write_mcb_header(mem, seg, b'Z', 0, paras - 1, NO_NAME)?;
    *umb = Some(UmbArena {
        first_mcb: seg,
        top: seg.wrapping_add(paras),
    });
    Ok(())
}

/// XMS function 10h Request UMB: carve `paras` paragraphs from the same upper
/// MCB chain used by AH=48h-high allocations.
#[allow(dead_code)] // unused until TOKAEMM's UMB calls land; see module docs.
fn request_umb(
    umb: Option<UmbArena>,
    paras: u16,
    mem: &mut Memory,
) -> Result<Result<u16, u16>, BusError> {
    match umb {
        Some(u) => u.allocate(paras, mem),
        None => Ok(Err(0)),
    }
}

/// XMS function 11h Release UMB.
#[allow(dead_code)] // unused until TOKAEMM's UMB calls land; see module docs.
fn release_umb(
    umb: Option<UmbArena>,
    seg: u16,
    mem: &mut Memory,
) -> Result<Result<(), ()>, BusError> {
    match umb {
        Some(u) if u.contains_data(seg) => u.free(seg, mem),
        _ => Ok(Err(())),
    }
}

/// XMS function 12h Reallocate UMB. `Err(Some(largest))` is the too-big case;
/// `Err(None)` means `seg` is not a live UMB block.
#[allow(dead_code)] // unused until TOKAEMM's UMB calls land; see module docs.
fn resize_umb(
    umb: Option<UmbArena>,
    seg: u16,
    paras: u16,
    mem: &mut Memory,
) -> Result<Result<(), Option<u16>>, BusError> {
    let Some(u) = umb else {
        return Ok(Err(None));
    };
    if !u.contains_data(seg) {
        return Ok(Err(None));
    }
    match u.resize(seg, paras, mem)? {
        Ok(()) => Ok(Ok(())),
        Err(ResizeError::TooBig(largest)) => Ok(Err(Some(largest))),
        Err(ResizeError::InvalidBlock) => Ok(Err(None)),
    }
}

fn write_mcb_header(
    mem: &mut Memory,
    seg: u16,
    sig: u8,
    owner: u16,
    size: u16,
    name: &[u8; 8],
) -> Result<(), BusError> {
    let base = usize::from(seg) * 16;
    mem.write_u8(base, sig)?;
    mem.write_u16(base + 1, owner)?;
    mem.write_u16(base + 3, size)?;
    for off in 5..8 {
        mem.write_u8(base + off, 0)?;
    }
    for (off, &b) in name.iter().enumerate() {
        mem.write_u8(base + 8 + off, b)?;
    }
    Ok(())
}

/// Safety cap on an MCB walk: a valid chain cannot have more headers than there
/// are paragraphs in the arena (each header occupies at least its own paragraph),
/// so this bounds a corrupt or cyclic chain. The realistic chain is a handful of
/// blocks and stops at its 'Z' header long before this.
const MCB_WALK_CAP: usize = ARENA_TOP as usize;

/// One MCB read back from guest RAM: the header paragraph, the link/last
/// signature, the owner PSP (0 = free), and the data size in paragraphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RamMcb {
    mcb_seg: u16,
    sig: u8,
    owner: u16,
    size: u16,
}

/// Walk the in-RAM MCB chain from `first_mcb`, the inverse of the initial arena
/// writer: read each header's signature, owner, and size, then follow the
/// data-plus-size step to the next, stopping at a 'Z' last-block header or an
/// unreadable / invalid signature. This reads the chain as the guest sees it in
/// memory, so edits a guest or the allocator makes to a header are observed here.
fn read_mcb_chain(mem: &Memory, first_mcb: u16) -> Vec<RamMcb> {
    let mut out = Vec::new();
    let mut seg = first_mcb;
    for _ in 0..MCB_WALK_CAP {
        let base = usize::from(seg) * 16;
        let (Ok(sig), Ok(owner), Ok(size)) = (
            mem.read_u8(base),
            mem.read_u16(base + 1),
            mem.read_u16(base + 3),
        ) else {
            break; // ran off mapped memory
        };
        if sig != b'M' && sig != b'Z' {
            break; // not a valid MCB header
        }
        out.push(RamMcb {
            mcb_seg: seg,
            sig,
            owner,
            size,
        });
        if sig == b'Z' {
            break; // last block in the chain
        }
        seg = seg.wrapping_add(1).wrapping_add(size);
    }
    out
}

/// The free tail of the chain rooted at `first_mcb`: (header seg, data size) of
/// the last block when it is free (owner 0), else None when the region is full.
#[cfg(test)]
fn free_tail(first_mcb: u16, mem: &Memory) -> Option<(u16, u16)> {
    match read_mcb_chain(mem, first_mcb).last() {
        Some(last) if last.owner == 0 => Some((last.mcb_seg, last.size)),
        _ => None,
    }
}

fn coalesce_free_blocks(first_mcb: u16, mem: &mut Memory) -> Result<(), BusError> {
    loop {
        let chain = read_mcb_chain(mem, first_mcb);
        let Some((left, right)) = chain
            .windows(2)
            .find(|pair| pair[0].owner == 0 && pair[1].owner == 0)
            .map(|pair| (pair[0], pair[1]))
        else {
            return Ok(());
        };
        write_mcb_header(
            mem,
            left.mcb_seg,
            right.sig,
            0,
            left.size.wrapping_add(1).wrapping_add(right.size),
            NO_NAME,
        )?;
    }
}

fn largest_free_block(chain: &[RamMcb]) -> u16 {
    chain
        .iter()
        .filter(|m| m.owner == 0)
        .map(|m| m.size)
        .max()
        .unwrap_or(0)
}

fn select_free_block(chain: &[RamMcb], fit: AllocFit, paras: u16) -> Option<RamMcb> {
    let mut blocks = chain
        .iter()
        .copied()
        .filter(|m| m.owner == 0 && m.size >= paras);
    match fit {
        AllocFit::First => blocks.next(),
        AllocFit::Best => blocks.min_by_key(|m| m.size),
        AllocFit::Last => blocks.next_back(),
    }
}

fn split_free_block(
    block: RamMcb,
    fit: AllocFit,
    paras: u16,
    mem: &mut Memory,
) -> Result<u16, BusError> {
    if block.size == paras {
        let data_seg = block.mcb_seg.wrapping_add(1);
        write_mcb_header(mem, block.mcb_seg, block.sig, data_seg, paras, NO_NAME)?;
        return Ok(data_seg);
    }

    match fit {
        AllocFit::Last => {
            let data_seg = block
                .mcb_seg
                .wrapping_add(1)
                .wrapping_add(block.size)
                .wrapping_sub(paras);
            let alloc_mcb = data_seg.wrapping_sub(1);
            write_mcb_header(mem, block.mcb_seg, b'M', 0, block.size - paras - 1, NO_NAME)?;
            write_mcb_header(mem, alloc_mcb, block.sig, data_seg, paras, NO_NAME)?;
            Ok(data_seg)
        }
        AllocFit::First | AllocFit::Best => {
            let data_seg = block.mcb_seg.wrapping_add(1);
            let new_free = data_seg.wrapping_add(paras);
            write_mcb_header(mem, block.mcb_seg, b'M', data_seg, paras, NO_NAME)?;
            write_mcb_header(mem, new_free, block.sig, 0, block.size - paras - 1, NO_NAME)?;
            Ok(data_seg)
        }
    }
}

/// Allocate `paras` paragraphs from the live MCB free-list. Adjacent free MCBs are
/// coalesced first, then the AH=58h fit method chooses the block. First/best-fit
/// split from the low end; last-fit splits from the high end like DOS.
fn allocate_block(
    first_mcb: u16,
    fit: AllocFit,
    paras: u16,
    mem: &mut Memory,
) -> Result<Result<u16, u16>, BusError> {
    coalesce_free_blocks(first_mcb, mem)?;
    let chain = read_mcb_chain(mem, first_mcb);
    match select_free_block(&chain, fit, paras) {
        Some(block) => Ok(Ok(split_free_block(block, fit, paras, mem)?)),
        None => Ok(Err(largest_free_block(&chain))),
    }
}

/// Free the owned block whose data segment is `seg` in the region rooted at
/// `first_mcb`. The live MCB chain is authoritative: a valid owned MCB at ES-1 is
/// marked owner-0 and adjacent free blocks are folded together.
fn free_block(
    first_mcb: u16,
    _top: u16,
    seg: u16,
    mem: &mut Memory,
) -> Result<Result<(), ()>, BusError> {
    let chain = read_mcb_chain(mem, first_mcb);
    let Some(block) = chain
        .iter()
        .copied()
        .find(|m| m.owner != 0 && m.mcb_seg == seg.wrapping_sub(1))
    else {
        return Ok(Err(()));
    };
    write_mcb_header(mem, block.mcb_seg, block.sig, 0, block.size, NO_NAME)?;
    coalesce_free_blocks(first_mcb, mem)?;
    Ok(Ok(()))
}

/// Resize the owned AH=48h-style block whose data segment is `seg` in the region
/// `[first_mcb, top)` to `paras` paragraphs. A free successor is coalesced into the
/// grow ceiling, and shrink-created gaps become reusable free MCBs.
fn resize_block(
    first_mcb: u16,
    top: u16,
    seg: u16,
    paras: u16,
    mem: &mut Memory,
) -> Result<Result<(), ResizeError>, BusError> {
    coalesce_free_blocks(first_mcb, mem)?;
    let chain = read_mcb_chain(mem, first_mcb);
    let Some(pos) = chain
        .iter()
        .position(|m| m.owner != 0 && m.mcb_seg == seg.wrapping_sub(1))
    else {
        return Ok(Err(ResizeError::InvalidBlock));
    };
    let block = chain[pos];
    let next = chain.get(pos + 1).copied();
    let limit = match next {
        Some(n) if n.owner == 0 => n.mcb_seg.wrapping_add(1).wrapping_add(n.size),
        Some(n) => n.mcb_seg,
        None => top,
    };
    let new_end = u32::from(seg) + u32::from(paras);
    if new_end > u32::from(limit) {
        return Ok(Err(ResizeError::TooBig(limit - seg)));
    }
    let new_end = new_end as u16;
    if new_end == limit {
        let sig = match next {
            Some(n) if n.owner == 0 => n.sig,
            Some(_) => b'M',
            None => b'Z',
        };
        write_mcb_header(mem, block.mcb_seg, sig, block.owner, paras, NO_NAME)?;
    } else {
        let free_sig = match next {
            Some(n) if n.owner == 0 => n.sig,
            Some(_) => b'M',
            None => b'Z',
        };
        write_mcb_header(mem, block.mcb_seg, b'M', block.owner, paras, NO_NAME)?;
        write_mcb_header(mem, new_end, free_sig, 0, limit - new_end - 1, NO_NAME)?;
    }
    Ok(Ok(()))
}

/// Owns the UMB arena state that used to live in the retired Rust DOS kernel: the
/// upper-memory MCB chain descriptor. This is the live DOS memory-manager the XMS
/// UMB services and `furnish_dos_upper_memory` drive.
#[derive(Debug, Default)]
pub struct DosMemory {
    umb: Option<UmbArena>,
}

impl DosMemory {
    /// Furnish the upper-memory-block arena: the machine's UMA reservation map
    /// reserves the ROM in the 0xC0000-0xEFFFF window and hands the remaining hole
    /// here as `[seg, seg + paras)`. This lays a single free MCB spanning the pool,
    /// so a guest (or a debugger) reads a real upper-memory arena from the start.
    /// `paras` below 2 leaves no room for a header plus data and clears the arena.
    pub fn set_umb_region(
        &mut self,
        seg: u16,
        paras: u16,
        mem: &mut Memory,
    ) -> Result<(), BusError> {
        set_umb_region(&mut self.umb, seg, paras, mem)
    }

    /// XMS function 10h Request UMB: carve `paras` paragraphs from the SAME
    /// upper-memory MCB chain the AH=48h-high path uses, so the two never hand out
    /// the same paragraph. Ok(Ok(segment)), Ok(Err(largest data paras / 0 when the
    /// pool is full)), Err(BusError) on a memory fault. Independent of the AH=5803h
    /// link: XMS Request UMB is the manager primitive, available whenever the pool
    /// exists.
    #[allow(dead_code)] // unused until TOKAEMM's UMB calls land; see module docs.
    pub fn request_umb(
        &mut self,
        paras: u16,
        mem: &mut Memory,
    ) -> Result<Result<u16, u16>, BusError> {
        request_umb(self.umb, paras, mem)
    }

    /// XMS function 11h Release UMB: free the upper-memory block whose segment is
    /// `seg`. Ok(Ok(())), or Ok(Err(())) when `seg` is not a UMB block.
    #[allow(dead_code)] // unused until TOKAEMM's UMB calls land; see module docs.
    pub fn release_umb(&mut self, seg: u16, mem: &mut Memory) -> Result<Result<(), ()>, BusError> {
        release_umb(self.umb, seg, mem)
    }

    /// XMS function 12h Reallocate UMB: resize the upper-memory block at `seg` to
    /// `paras`. Ok(Ok(())); Ok(Err(Some(largest))) when a grow does not fit (the
    /// caller maps it to B0h with the largest size); Ok(Err(None)) when `seg` is not
    /// a UMB block (B2h).
    #[allow(dead_code)] // unused until TOKAEMM's UMB calls land; see module docs.
    pub fn resize_umb(
        &mut self,
        seg: u16,
        paras: u16,
        mem: &mut Memory,
    ) -> Result<Result<(), Option<u16>>, BusError> {
        resize_umb(self.umb, seg, paras, mem)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_1mib() -> Memory {
        Memory::new(1024 * 1024).unwrap()
    }

    #[test]
    fn set_umb_region_lays_a_single_free_z_block_spanning_the_pool() {
        let mut mem = mem_1mib();
        let mut dm = DosMemory::default();
        dm.set_umb_region(0xC800, 0x0800, &mut mem).unwrap();
        assert!(dm.umb.is_some());
        let arena = dm.umb.unwrap();
        let chain = arena.chain(&mem);
        assert_eq!(chain.len(), 1, "one free block spans the pool");
        assert_eq!(chain[0].sig, b'Z');
        assert_eq!(chain[0].owner, 0);
        assert_eq!(chain[0].size, 0x0800 - 1);
        assert_eq!(free_tail(arena.first_mcb, &mem), Some((0xC800, 0x0800 - 1)));
    }

    #[test]
    fn set_umb_region_clears_a_too_small_pool() {
        let mut mem = mem_1mib();
        let mut dm = DosMemory::default();
        dm.set_umb_region(0xC800, 0x0800, &mut mem).unwrap();
        dm.set_umb_region(0xC800, 1, &mut mem).unwrap();
        assert!(dm.umb.is_none(), "a <2-para pool clears the arena");
    }

    #[test]
    fn request_release_resize_umb_round_trip() {
        let mut mem = mem_1mib();
        let mut dm = DosMemory::default();
        dm.set_umb_region(0xC800, 0x0800, &mut mem).unwrap();

        let seg = match dm.request_umb(0x0100, &mut mem).unwrap() {
            Ok(seg) => seg,
            Err(_) => panic!("request should succeed"),
        };
        assert!(dm.umb.unwrap().contains_data(seg));

        // Grow it — the free successor is available, so it fits.
        assert_eq!(dm.resize_umb(seg, 0x0200, &mut mem).unwrap(), Ok(()));
        // Shrink it.
        assert_eq!(dm.resize_umb(seg, 0x0080, &mut mem).unwrap(), Ok(()));

        // Release it.
        assert_eq!(dm.release_umb(seg, &mut mem).unwrap(), Ok(()));
        // A second release of the same segment is now invalid.
        assert_eq!(dm.release_umb(seg, &mut mem).unwrap(), Err(()));
    }

    #[test]
    fn request_umb_reports_no_room_when_the_pool_is_absent() {
        let mut mem = mem_1mib();
        let mut dm = DosMemory::default();
        assert_eq!(dm.request_umb(0x10, &mut mem).unwrap(), Err(0));
        assert_eq!(dm.release_umb(0xC800, &mut mem).unwrap(), Err(()));
        assert_eq!(dm.resize_umb(0xC800, 0x10, &mut mem).unwrap(), Err(None));
    }
}
