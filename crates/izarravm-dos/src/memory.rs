use izarravm_bus::Memory;

use crate::DosError;

/// The top of conventional memory in paragraphs: the 640 KiB line (0xA000
/// paragraphs = 0xA0000 bytes) where the video aperture begins. This is the one
/// source for that boundary across the DOS layer: the allocation arena ends
/// here, the MCB chain spans up to it, and the .EXE loader clamps a program's
/// block to it. It is the same 640 KiB conventional ceiling the hardware side
/// models at 0xA0000; the dos crate does not depend on the machine crate, so the
/// value lives here independently rather than being shared across the two.
pub(super) const ARENA_TOP: u16 = 0xa000;

/// AH=4Ah resize result details shared by conventional and upper-memory blocks.
pub(super) enum ResizeError {
    TooBig(u16), // largest paragraphs that would fit
    InvalidBlock,
}

/// The 8-byte MCB owner name. The program block carries a fixed placeholder (the
/// loader does not thread the real loaded name down here); other blocks are blank.
pub(super) const PROG_NAME: &[u8; 8] = b"TOKAPROG";
pub(super) const NO_NAME: &[u8; 8] = b"\0\0\0\0\0\0\0\0";

/// Write one MCB header into guest RAM: the signature ('M' link or 'Z' last), the
/// owner PSP word (0 = free), the data size in paragraphs, three reserved bytes,
/// and the 8-byte owner name. The header occupies the paragraph at `seg`; the
/// block's data starts at `seg + 1`.
pub(super) fn write_mcb_header(
    mem: &mut Memory,
    seg: u16,
    sig: u8,
    owner: u16,
    size: u16,
    name: &[u8; 8],
) -> Result<(), DosError> {
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
pub(super) struct RamMcb {
    pub(super) mcb_seg: u16,
    pub(super) sig: u8,
    pub(super) owner: u16,
    pub(super) size: u16,
}

/// Walk the in-RAM MCB chain from `first_mcb`, the inverse of the initial arena
/// writer: read each header's signature, owner, and size, then follow the
/// data-plus-size step to the next, stopping at a 'Z' last-block header or an
/// unreadable / invalid signature. This reads the chain as the guest sees it in
/// memory, so edits a guest or the allocator makes to a header are observed here.
pub(super) fn read_mcb_chain(mem: &Memory, first_mcb: u16) -> Vec<RamMcb> {
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
pub(super) fn free_tail(first_mcb: u16, mem: &Memory) -> Option<(u16, u16)> {
    match read_mcb_chain(mem, first_mcb).last() {
        Some(last) if last.owner == 0 => Some((last.mcb_seg, last.size)),
        _ => None,
    }
}

/// Carve `paras` paragraphs from the free tail of the region `[first_mcb, top)`.
/// Reserves a one-paragraph header at the tail; the data segment handed out is one
/// paragraph higher. Ok(Ok(data seg)), or Ok(Err(largest data paras)) when it does
/// not fit (already net of the header).
pub(super) fn carve_from_tail(
    first_mcb: u16,
    top: u16,
    paras: u16,
    mem: &mut Memory,
) -> Result<Result<u16, u16>, DosError> {
    let Some((free_seg, _)) = free_tail(first_mcb, mem) else {
        return Ok(Err(0)); // region full: no free tail to carve
    };
    let data_seg = u32::from(free_seg) + 1; // header sits at free_seg
    let end = data_seg + u32::from(paras); // first free paragraph after the data
    if end <= u32::from(top) {
        let seg = data_seg as u16;
        if end < u32::from(top) {
            // A free tail remains above the new block.
            write_mcb_header(mem, free_seg, b'M', seg, paras, NO_NAME)?;
            let new_free = end as u16;
            write_mcb_header(mem, new_free, b'Z', 0, top - new_free - 1, NO_NAME)?;
        } else {
            // The carve consumes the tail exactly: the new block is the last.
            write_mcb_header(mem, free_seg, b'Z', seg, paras, NO_NAME)?;
        }
        Ok(Ok(seg))
    } else {
        Ok(Err((u32::from(top).saturating_sub(data_seg)) as u16))
    }
}

/// Free the owned block whose data segment is `seg` in the region `[first_mcb,
/// top)`. A block directly below the free tail merges into it (LIFO reclaim); any
/// other freed block becomes an owner-0 hole that leaks until the blocks above it
/// are freed. Ok(Ok(())) on success, Ok(Err(())) for an unknown block.
pub(super) fn free_block(
    first_mcb: u16,
    top: u16,
    seg: u16,
    mem: &mut Memory,
) -> Result<Result<(), ()>, DosError> {
    let chain = read_mcb_chain(mem, first_mcb);
    let Some(pos) = chain
        .iter()
        .position(|m| m.owner != 0 && m.mcb_seg == seg.wrapping_sub(1))
    else {
        return Ok(Err(()));
    };
    let block = chain[pos];
    let block_end = block.mcb_seg.wrapping_add(1).wrapping_add(block.size);
    match chain.last() {
        Some(tail) if tail.owner == 0 && block_end == tail.mcb_seg => {
            // LIFO reclaim: extend the free tail down over this block.
            write_mcb_header(
                mem,
                block.mcb_seg,
                b'Z',
                0,
                top - block.mcb_seg - 1,
                NO_NAME,
            )?;
        }
        _ => {
            // Non-top free: mark the block free in place; the hole leaks.
            mem.write_u16(usize::from(block.mcb_seg) * 16 + 1, 0)?;
        }
    }
    Ok(Ok(()))
}

/// Resize the owned AH=48h-style block whose data segment is `seg` in the region
/// `[first_mcb, top)` to `paras` paragraphs. Same `ResizeError` codes as the
/// conventional path: a block at (or whose free tail sits directly above) the top
/// moves the tail; a non-top block shrinks in place and leaks the gap, and a
/// non-top grow has nowhere to go.
pub(super) fn resize_block(
    first_mcb: u16,
    top: u16,
    seg: u16,
    paras: u16,
    mem: &mut Memory,
) -> Result<Result<(), ResizeError>, DosError> {
    let chain = read_mcb_chain(mem, first_mcb);
    let Some(pos) = chain
        .iter()
        .position(|m| m.owner != 0 && m.mcb_seg == seg.wrapping_sub(1))
    else {
        return Ok(Err(ResizeError::InvalidBlock));
    };
    let block = chain[pos];
    let block_end = block.mcb_seg.wrapping_add(1).wrapping_add(block.size);
    let tail_above = matches!(chain.last(), Some(t) if t.owner == 0 && block_end == t.mcb_seg);
    let is_last = pos + 1 == chain.len();
    if tail_above || is_last {
        let new_end = u32::from(seg) + u32::from(paras);
        if new_end > u32::from(top) {
            return Ok(Err(ResizeError::TooBig(top - seg)));
        }
        let new_end = new_end as u16;
        if new_end < top {
            write_mcb_header(mem, block.mcb_seg, b'M', seg, paras, NO_NAME)?;
            write_mcb_header(mem, new_end, b'Z', 0, top - new_end - 1, NO_NAME)?;
        } else {
            write_mcb_header(mem, block.mcb_seg, b'Z', seg, paras, NO_NAME)?;
        }
        Ok(Ok(()))
    } else if paras <= block.size {
        // Non-top block with a successor: shrink in place; the gap leaks.
        write_mcb_header(mem, block.mcb_seg, b'M', seg, paras, NO_NAME)?;
        let gap_seg = seg.wrapping_add(paras);
        let next = chain[pos + 1].mcb_seg;
        if gap_seg < next {
            write_mcb_header(mem, gap_seg, b'M', 0, next - gap_seg - 1, NO_NAME)?;
        }
        Ok(Ok(()))
    } else {
        Ok(Err(ResizeError::TooBig(block.size)))
    }
}
