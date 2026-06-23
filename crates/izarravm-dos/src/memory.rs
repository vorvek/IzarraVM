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

/// Kernel-reserved paragraph holding the AH=52h list-of-lists (SysVars) table.
/// 0x0050 = linear 0x500, just above the BIOS data area (0x400-0x4FF) and below
/// where programs load (psp_seg >= 0x0100), so it collides with neither.
const SYSVARS_SEG: u16 = 0x0050;

/// DOS default LASTDRIVE, reported in the list of lists: drives A: through E:.
const DEFAULT_LASTDRIVE: u8 = 5;

/// Conventional memory modeled as an authoritative in-RAM MCB chain ending at
/// paragraph 0xA000. The chain is the source of truth: allocate/free/resize walk
/// and mutate the real headers a guest reads through AH=52h, so a memory manager
/// that rewrites a header in place drives the allocator. The arena itself holds
/// only the current program's PSP and the resident flag; the program top and free
/// base are read back from the chain. LIFO reclaim, no free-list coalescing (a
/// freed non-top block leaks until the blocks above it are freed); no UMB/HIMEM.
#[derive(Debug, Default)]
pub(super) struct Arena {
    pub(super) psp_seg: u16,
    // The first MCB of this process's chain. For a directly-loaded program it is
    // psp_seg-1 (the program block heads the chain). An EXEC child's chain starts
    // one block lower, at its environment block's header, so a guest walking the
    // child chain sees env -> program -> free.
    pub(super) chain_first: u16,
    // AH=31h KEEP PROCESS: the program block stays allocated at termination. Set
    // once a TSR keeps itself resident; a later free of the program block is a
    // no-op the same as a normal exit, so the flag only records that the
    // paragraphs are reserved.
    pub(super) resident: bool,
}

impl Arena {
    /// The program block's MCB header sits one paragraph below the PSP; the chain
    /// is walked from here.
    pub(super) fn first_mcb(&self) -> u16 {
        self.chain_first
    }

    /// Walk the authoritative chain from the first MCB.
    fn chain(&self, mem: &Memory) -> Vec<RamMcb> {
        read_mcb_chain(mem, self.first_mcb())
    }

    /// Write the initial chain at program load: the program block, then the free
    /// remainder up to ARENA_TOP. When the program fills the arena (prog_top ==
    /// ARENA_TOP) it is itself the last block and there is no free tail. The chain
    /// is authoritative from here; allocate/free/resize mutate it in place.
    pub(super) fn write_initial_chain(
        &self,
        mem: &mut Memory,
        prog_top: u16,
    ) -> Result<(), DosError> {
        let prog_size = prog_top.wrapping_sub(self.psp_seg);
        if prog_top < ARENA_TOP {
            write_mcb_header(
                mem,
                self.first_mcb(),
                b'M',
                self.psp_seg,
                prog_size,
                PROG_NAME,
            )?;
            write_mcb_header(mem, prog_top, b'Z', 0, ARENA_TOP - prog_top - 1, NO_NAME)?;
        } else {
            write_mcb_header(
                mem,
                self.first_mcb(),
                b'Z',
                self.psp_seg,
                prog_size,
                PROG_NAME,
            )?;
        }
        // The chain must read back through the same walker the allocator uses.
        debug_assert!(
            self.chain(mem).last().is_some_and(|z| z.sig == b'Z'),
            "the initial MCB chain must end in a Z block"
        );
        Ok(())
    }

    /// The free tail: (header seg, data size) of the last block when it is free
    /// (owner 0). None when the arena is full (the last block is owned).
    fn free_region(&self, mem: &Memory) -> Option<(u16, u16)> {
        free_tail(self.first_mcb(), mem)
    }

    /// The program's top-of-memory paragraph (PSP:0x02), derived from the program
    /// block's size word in the chain. Falls back to psp_seg only if the chain is
    /// unwritten, which never happens after init_program.
    pub(super) fn prog_top(&self, mem: &Memory) -> u16 {
        // The program block is the one whose data segment is the PSP (an EXEC
        // child's chain starts at the env block, so it is not necessarily first).
        self.chain(mem)
            .iter()
            .find(|m| m.mcb_seg.wrapping_add(1) == self.psp_seg)
            .map(|m| m.mcb_seg.wrapping_add(1).wrapping_add(m.size))
            .unwrap_or(self.psp_seg)
    }

    /// The first free paragraph above the program block and any allocations: the
    /// free tail's header segment, where the next AH=48h block lands. ARENA_TOP
    /// when the arena is full.
    pub(super) fn free_base(&self, mem: &Memory) -> u16 {
        self.free_region(mem)
            .map(|(seg, _)| seg)
            .unwrap_or(ARENA_TOP)
    }

    /// AH=48h: allocate `paras` paragraphs from the free tail. Ok(Ok(data segment))
    /// on success, Ok(Err(largest-available data paragraphs)) when it does not fit,
    /// Err(DosError) on a guest memory fault. The block reserves a one-paragraph
    /// header at the free tail; the data segment handed out is one paragraph higher,
    /// and the tail shrinks past header and data. Allocation comes only from the
    /// tail (no first-fit scan of freed holes), preserving LIFO reclaim. The
    /// largest-available figure already subtracts the header paragraph.
    pub(super) fn allocate(
        &mut self,
        paras: u16,
        mem: &mut Memory,
    ) -> Result<Result<u16, u16>, DosError> {
        carve_from_tail(self.first_mcb(), ARENA_TOP, paras, mem)
    }

    /// AH=49h: free the block whose data segment is `seg`. Ok(Ok(())) on success,
    /// Ok(Err(())) for an unknown block, Err(DosError) on a guest memory fault. A
    /// block immediately below the free tail is merged back into it (LIFO reclaim);
    /// any other freed block becomes an owner-0 hole that leaks until the blocks
    /// above it are freed (the documented no-coalesce ceiling).
    pub(super) fn free(&mut self, seg: u16, mem: &mut Memory) -> Result<Result<(), ()>, DosError> {
        if seg == self.psp_seg {
            return Ok(Ok(())); // freeing the program block (e.g. at termination)
        }
        free_block(self.first_mcb(), ARENA_TOP, seg, mem)
    }

    /// AH=4Ah: resize the block whose segment is `seg` to `paras` paragraphs.
    /// Ok(Ok(())) on success, Ok(Err(ResizeError)) on a DOS error, Err(DosError) on
    /// a guest memory fault.
    pub(super) fn resize(
        &mut self,
        seg: u16,
        paras: u16,
        mem: &mut Memory,
    ) -> Result<Result<(), ResizeError>, DosError> {
        let chain = self.chain(mem);
        if seg == self.psp_seg {
            // The program block (data segment == PSP; for an EXEC child it follows
            // the env block, so it is not necessarily chain[0]). Its ceiling is the
            // lowest still-OWNED block ABOVE it; leaked owner-0 holes, the free tail,
            // and the env below it are not ceilings, the program grows through holes
            // up to that owned block (or ARENA_TOP when nothing above is owned).
            let Some(pos) = chain
                .iter()
                .position(|m| m.mcb_seg.wrapping_add(1) == self.psp_seg)
            else {
                return Ok(Err(ResizeError::InvalidBlock));
            };
            let prog = chain[pos];
            let owned_above = chain.iter().skip(pos + 1).find(|m| m.owner != 0).copied();
            let limit = owned_above.map(|m| m.mcb_seg).unwrap_or(ARENA_TOP);
            let new_top = u32::from(self.psp_seg) + u32::from(paras);
            if new_top > u32::from(limit) {
                return Ok(Err(ResizeError::TooBig(limit - self.psp_seg)));
            }
            let new_top = new_top as u16;
            // The program links to a successor (a hole, or the free tail) unless it
            // now reaches ARENA_TOP with nothing owned above.
            let prog_sig = if owned_above.is_some() || new_top < ARENA_TOP {
                b'M'
            } else {
                b'Z'
            };
            write_mcb_header(
                mem,
                prog.mcb_seg,
                prog_sig,
                self.psp_seg,
                new_top - self.psp_seg,
                PROG_NAME,
            )?;
            if let Some(owned) = owned_above {
                // The freed gap below the owned block leaks as an owner-0 hole.
                if new_top < owned.mcb_seg {
                    write_mcb_header(mem, new_top, b'M', 0, owned.mcb_seg - new_top - 1, NO_NAME)?;
                }
            } else if new_top < ARENA_TOP {
                // Nothing owned above: the remainder is the free tail.
                write_mcb_header(mem, new_top, b'Z', 0, ARENA_TOP - new_top - 1, NO_NAME)?;
            }
            return Ok(Ok(()));
        }
        // An AH=48h block: shares the free-tail resize engine with the upper-memory
        // arena, bounded by the conventional ceiling.
        resize_block(self.first_mcb(), ARENA_TOP, seg, paras, mem)
    }

    /// AH=31h KEEP PROCESS: trim the program block to `paras` paragraphs and mark it
    /// resident. Everything above the resident block becomes a single free tail (the
    /// AH=48h blocks are released, the common TSR pattern holds no separate
    /// allocations). `paras` is clamped so the block never grows past its current top.
    pub(super) fn keep_resident(&mut self, paras: u16, mem: &mut Memory) -> Result<(), DosError> {
        let cur_top = self.prog_top(mem);
        let want = u32::from(self.psp_seg) + u32::from(paras);
        let new_top = want.min(u32::from(cur_top)) as u16;
        // The program block's own header is psp_seg-1 (not first_mcb(), which for an
        // EXEC child is the env block below the program). Any AH=48h block above the
        // program is released into the free tail; an env block below stays owned.
        let prog_mcb = self.psp_seg.wrapping_sub(1);
        if new_top < ARENA_TOP {
            write_mcb_header(
                mem,
                prog_mcb,
                b'M',
                self.psp_seg,
                new_top - self.psp_seg,
                PROG_NAME,
            )?;
            write_mcb_header(mem, new_top, b'Z', 0, ARENA_TOP - new_top - 1, NO_NAME)?;
        } else {
            write_mcb_header(
                mem,
                prog_mcb,
                b'Z',
                self.psp_seg,
                new_top - self.psp_seg,
                PROG_NAME,
            )?;
        }
        self.resident = true;
        Ok(())
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
pub(super) struct UmbArena {
    /// Header paragraph of the first UMB MCB (the pool's base segment).
    pub(super) first_mcb: u16,
    /// One paragraph past the pool: the ceiling the free tail reaches, the UMB
    /// analogue of `ARENA_TOP` for the conventional arena.
    pub(super) top: u16,
}

impl UmbArena {
    pub(super) fn contains_data(self, seg: u16) -> bool {
        (self.first_mcb..self.top).contains(&seg)
    }

    #[cfg(test)]
    pub(super) fn chain(self, mem: &Memory) -> Vec<RamMcb> {
        read_mcb_chain(mem, self.first_mcb)
    }

    fn allocate(self, paras: u16, mem: &mut Memory) -> Result<Result<u16, u16>, DosError> {
        carve_from_tail(self.first_mcb, self.top, paras, mem)
    }

    fn free(self, seg: u16, mem: &mut Memory) -> Result<Result<(), ()>, DosError> {
        free_block(self.first_mcb, self.top, seg, mem)
    }

    fn resize(
        self,
        seg: u16,
        paras: u16,
        mem: &mut Memory,
    ) -> Result<Result<(), ResizeError>, DosError> {
        resize_block(self.first_mcb, self.top, seg, paras, mem)
    }
}

/// The nine valid AH=58h allocation strategies: the low two bits pick the fit
/// (first / best / last) and bits 6-7 pick the memory area (low, high, or high
/// then low). Any other value is rejected on a set-strategy call.
pub(super) fn is_valid_alloc_strategy(strategy: u16) -> bool {
    matches!(
        strategy,
        0x00 | 0x01 | 0x02 | 0x40 | 0x41 | 0x42 | 0x80 | 0x81 | 0x82
    )
}

/// Furnish or clear the upper-memory arena that DOS exposes to AH=48h high
/// allocations and XMS UMB calls. Clearing the arena also unlinks it, preserving
/// the invariant that a linked state always has a real pool behind it.
pub(super) fn set_umb_region(
    umb: &mut Option<UmbArena>,
    umb_link: &mut bool,
    seg: u16,
    paras: u16,
    mem: &mut Memory,
) -> Result<(), DosError> {
    if paras < 2 {
        *umb = None;
        *umb_link = false;
        return Ok(());
    }
    write_mcb_header(mem, seg, b'Z', 0, paras - 1, NO_NAME)?;
    *umb = Some(UmbArena {
        first_mcb: seg,
        top: seg.wrapping_add(paras),
    });
    Ok(())
}

fn linked_umb(umb: Option<UmbArena>, umb_link: bool) -> Option<UmbArena> {
    match (umb_link, umb) {
        (true, Some(arena)) => Some(arena),
        _ => None,
    }
}

/// AH=48h allocation honouring the AH=58h strategy and the UMB link state.
pub(super) fn allocate_strategy(
    arena: &mut Arena,
    umb: Option<UmbArena>,
    umb_link: bool,
    alloc_strategy: u16,
    paras: u16,
    mem: &mut Memory,
) -> Result<Result<u16, u16>, DosError> {
    let area = alloc_strategy & 0x00c0; // bits 6-7
    match (area, linked_umb(umb, umb_link)) {
        (0x40, Some(u)) => u.allocate(paras, mem),
        (0x80, Some(u)) => match u.allocate(paras, mem)? {
            Ok(seg) => Ok(Ok(seg)),
            // Upper memory could not satisfy it: fall back to conventional. On a
            // double failure report the larger of the two arenas' free tails, the
            // way DOS's single-chain walk reports the global largest block.
            Err(hi) => match arena.allocate(paras, mem)? {
                Ok(seg) => Ok(Ok(seg)),
                Err(lo) => Ok(Err(hi.max(lo))),
            },
        },
        _ => arena.allocate(paras, mem),
    }
}

/// AH=49h free routed to the arena that owns `seg`: the upper-memory arena when
/// the segment falls in its window, the conventional arena otherwise.
pub(super) fn free_routed(
    arena: &mut Arena,
    umb: Option<UmbArena>,
    seg: u16,
    mem: &mut Memory,
) -> Result<Result<(), ()>, DosError> {
    if let Some(u) = umb {
        if u.contains_data(seg) {
            return u.free(seg, mem);
        }
    }
    arena.free(seg, mem)
}

/// AH=4Ah resize routed to the arena that owns `seg` (see [`free_routed`]).
pub(super) fn resize_routed(
    arena: &mut Arena,
    umb: Option<UmbArena>,
    seg: u16,
    paras: u16,
    mem: &mut Memory,
) -> Result<Result<(), ResizeError>, DosError> {
    if let Some(u) = umb {
        if u.contains_data(seg) {
            return u.resize(seg, paras, mem);
        }
    }
    arena.resize(seg, paras, mem)
}

/// XMS function 10h Request UMB: carve `paras` paragraphs from the same upper
/// MCB chain used by AH=48h-high allocations.
pub(super) fn request_umb(
    umb: Option<UmbArena>,
    paras: u16,
    mem: &mut Memory,
) -> Result<Result<u16, u16>, DosError> {
    match umb {
        Some(u) => u.allocate(paras, mem),
        None => Ok(Err(0)),
    }
}

/// XMS function 11h Release UMB.
pub(super) fn release_umb(
    umb: Option<UmbArena>,
    seg: u16,
    mem: &mut Memory,
) -> Result<Result<(), ()>, DosError> {
    match umb {
        Some(u) if u.contains_data(seg) => u.free(seg, mem),
        _ => Ok(Err(())),
    }
}

/// XMS function 12h Reallocate UMB. `Err(Some(largest))` is the too-big case;
/// `Err(None)` means `seg` is not a live UMB block.
pub(super) fn resize_umb(
    umb: Option<UmbArena>,
    seg: u16,
    paras: u16,
    mem: &mut Memory,
) -> Result<Result<(), Option<u16>>, DosError> {
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

pub(super) fn write_env_mcb(
    mem: &mut Memory,
    env_mcb: u16,
    child_psp: u16,
    env_paras: u16,
) -> Result<(), DosError> {
    write_mcb_header(mem, env_mcb, b'M', child_psp, env_paras, NO_NAME)
}

pub(super) fn write_child_program_mcb(mem: &mut Memory, child_psp: u16) -> Result<(), DosError> {
    write_mcb_header(
        mem,
        child_psp.wrapping_sub(1),
        b'Z',
        child_psp,
        ARENA_TOP - child_psp,
        PROG_NAME,
    )
}

pub(super) fn write_free_mcb_to_cap(
    mem: &mut Memory,
    free_base: u16,
    cap: u16,
) -> Result<(), DosError> {
    // 'M' when a resident block follows (a link to it), else the tail.
    let sig = if cap < ARENA_TOP { b'M' } else { b'Z' };
    write_mcb_header(mem, free_base, sig, 0, cap - free_base - 1, NO_NAME)
}

/// AH=52h GET LIST OF LISTS (SysVars). Returns ES:BX -> the DOS internal
/// variable table. The first-MCB segment at [BX-2] points at the live in-RAM MCB
/// chain. The modeled scalar fields and device-driver headers are filled in;
/// unmodeled chain pointers stay zero.
pub(super) fn write_sysvars(
    mem: &mut Memory,
    first_mcb: u16,
    ems_present: bool,
    lastdrive: Option<u8>,
) -> Result<(u16, u16), DosError> {
    let base = usize::from(SYSVARS_SEG) * 16;
    // [BX-2] = first MCB segment (BX returns 0x0002, so this is offset 0).
    mem.write_u16(base, first_mcb)?;
    // Clear the documented field span, then fill the known fields over it.
    for off in 2..0x40usize {
        mem.write_u8(base + off, 0)?;
    }
    // [BX+0x10] WORD: the largest bytes-per-block of any block device, a 512-byte
    // sector here.
    mem.write_u16(base + 2 + 0x10, 512)?;
    // [BX+0x21] BYTE: LASTDRIVE.
    mem.write_u8(base + 2 + 0x21, lastdrive.unwrap_or(DEFAULT_LASTDRIVE))?;
    // [BX+0x22]: the NUL device header, the head of the device-driver chain.
    let nul_off = 0x22usize; // BX-relative offset of the NUL header
    let ems_off = nul_off + 0x12; // the EMMXXXX0 header right after NUL
    let nul = base + 2 + nul_off;
    if ems_present {
        mem.write_u16(nul, (2 + ems_off) as u16)?; // next offset
        mem.write_u16(nul + 2, SYSVARS_SEG)?; // next segment
    } else {
        mem.write_u16(nul, 0xffff)?;
        mem.write_u16(nul + 2, 0xffff)?; // FFFF:FFFF = end
    }
    mem.write_u16(nul + 4, 0x8004)?; // attribute: char device, NUL bit
    mem.write_u16(nul + 6, 0xffff)?; // strategy entry (none)
    mem.write_u16(nul + 8, 0xffff)?; // interrupt entry (none)
    for (i, &byte) in b"NUL     ".iter().enumerate() {
        mem.write_u8(nul + 0x0a + i, byte)?;
    }
    // The EMMXXXX0 device header terminates the chain when EMS is present, so a
    // guest walking the device list finds the manager by name.
    if ems_present {
        let ems = base + 2 + ems_off;
        mem.write_u16(ems, 0xffff)?; // next offset
        mem.write_u16(ems + 2, 0xffff)?; // next segment (end)
        mem.write_u16(ems + 4, 0xc000)?; // attribute: character device
        mem.write_u16(ems + 6, 0xffff)?; // strategy entry (none)
        mem.write_u16(ems + 8, 0xffff)?; // interrupt entry (none)
        for (i, &byte) in b"EMMXXXX0".iter().enumerate() {
            mem.write_u8(ems + 0x0a + i, byte)?;
        }
    }
    Ok((SYSVARS_SEG, 0x0002))
}

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
