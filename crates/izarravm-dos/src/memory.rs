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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AllocFit {
    First,
    Best,
    Last,
}

impl AllocFit {
    fn from_strategy(strategy: u16) -> Self {
        match strategy & 0x0003 {
            0x0001 => Self::Best,
            0x0002 => Self::Last,
            _ => Self::First,
        }
    }
}

/// The 8-byte MCB owner name. The program block carries a fixed placeholder (the
/// loader does not thread the real loaded name down here); other blocks are blank.
pub(super) const PROG_NAME: &[u8; 8] = b"TOKAPROG";
pub(super) const NO_NAME: &[u8; 8] = b"\0\0\0\0\0\0\0\0";

/// Kernel-reserved paragraph holding the AH=52h list-of-lists (SysVars) table.
/// 0x0064 = linear 0x640, just above the BIOS RAM stub cluster at 0x600-0x63F
/// (the IRET, RTC, INT 18h halt, and INT 24h critical-error stubs). The SysVars
/// structure grows up from here to the first MCB, so it must clear those stubs.
const SYSVARS_SEG: u16 = 0x0064;

/// DOS default LASTDRIVE, reported in the list of lists: drives A: through E:.
const DEFAULT_LASTDRIVE: u8 = 5;

/// AH=52h publishes the SFT and Current Directory Structure (CDS) array from the
/// same reserved paragraph as SysVars. Keep as many SFT slots as fit before the
/// CDS array and below the first MCB header. The default FILES=40 table plus the
/// default CDS array is slightly too large for this low-memory scratch paragraph,
/// so entries that do not fit are left blank until the DOS data segment is given a
/// larger owned block.
const DEVICE_HEADER_LEN: usize = 0x12;
const NUL_DEVICE_OFF: usize = 0x22;
const EMM_DEVICE_OFF: usize = NUL_DEVICE_OFF + DEVICE_HEADER_LEN;
const CON_DEVICE_OFF: usize = EMM_DEVICE_OFF + DEVICE_HEADER_LEN;
const CLOCK_DEVICE_OFF: usize = CON_DEVICE_OFF + DEVICE_HEADER_LEN;
const SFT_TABLE_OFF: usize = 0x70;
pub(super) const DBCS_LEAD_BYTE_TABLE_PTR: (u16, u16) = (SYSVARS_SEG, SFT_TABLE_OFF as u16);
const SFT_HEADER_LEN: usize = 0x06;
const SFT_ENTRY_LEN: usize = 0x3b;
const STANDARD_SFT_SLOTS: usize = 5;
const DPB_LEN: usize = 0x21;
const BDS_ENTRY_LEN: usize = 0x64;
pub(super) const BLOCK_BPB_LEN: usize = 0x19;
const CDS_ENTRY_LEN: usize = 0x58;
const SDA_LIVE_PREFIX_LEN: usize = 0x1a;
const NLS_UPPER_TABLE_LEN: usize = 2 + 0x80;
const NLS_LOWER_TABLE_LEN: usize = 2 + 0x100;
const NLS_FILENAME_UPPER_TABLE_LEN: usize = 2 + 0x80;
const NLS_FILENAME_TERMINATORS: &[u8] = b".\"/\\[]:|<>+=;,";
const NLS_FILENAME_TERMINATOR_TABLE_LEN: usize = 2 + 8 + NLS_FILENAME_TERMINATORS.len();
const NLS_COLLATING_TABLE_LEN: usize = 2 + 0x100;
const NLS_TABLES_LEN: usize = NLS_UPPER_TABLE_LEN
    + NLS_LOWER_TABLE_LEN
    + NLS_FILENAME_UPPER_TABLE_LEN
    + NLS_FILENAME_TERMINATOR_TABLE_LEN
    + NLS_COLLATING_TABLE_LEN;
const SDA_LIST_SEG: u16 = 0x0052;
const SDA_LIST_OFF: u16 = 0x0000;
pub(super) const SDA_ALWAYS_SWAPPED_LEN: u16 = 0x001a;
pub(super) const SDA_IN_DOS_SWAPPED_LEN: u16 = 0x0000;

#[derive(Debug, Clone, Copy)]
pub(super) struct SdaCriticalError {
    pub(super) drive: u8,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SdaSnapshot {
    pub(super) last_error: u16,
    pub(super) current_dta: (u16, u16),
    pub(super) current_psp: u16,
    pub(super) last_exit_code: u8,
    pub(super) last_exit_type: u8,
    pub(super) critical_error: Option<SdaCriticalError>,
}

#[derive(Debug, Clone, Copy)]
struct SysvarsLayout {
    drive_count: u8,
    sft_slots: usize,
    dpb_off: usize,
    cds_array_off: usize,
    bds_off: usize,
    sda_off: usize,
    nls_off: usize,
}

fn sysvars_layout(
    first_mcb: u16,
    lastdrive: Option<u8>,
    file_count: u16,
    published_block_units: usize,
) -> Result<SysvarsLayout, DosError> {
    let base = usize::from(SYSVARS_SEG) * 16;
    let sysvars_limit = usize::from(first_mcb) * 16;
    let drive_count = lastdrive.unwrap_or(DEFAULT_LASTDRIVE).min(26);
    let sft_linear = base + 2 + SFT_TABLE_OFF;
    let cds_bytes = usize::from(drive_count) * CDS_ENTRY_LEN;
    let dpb_bytes = DPB_LEN * (1 + published_block_units);
    let bds_bytes = BDS_ENTRY_LEN * (1 + published_block_units);
    let max_sft_slots = (sysvars_limit.saturating_sub(
        sft_linear
            + SFT_HEADER_LEN
            + dpb_bytes
            + cds_bytes
            + bds_bytes
            + SDA_LIVE_PREFIX_LEN
            + NLS_TABLES_LEN,
    ) / SFT_ENTRY_LEN)
        .max(2);
    let sft_slots = usize::from(file_count)
        .max(STANDARD_SFT_SLOTS)
        .min(max_sft_slots);
    let dpb_off = SFT_TABLE_OFF + SFT_HEADER_LEN + sft_slots * SFT_ENTRY_LEN;
    let cds_array_off = dpb_off + dpb_bytes;
    let bds_off = cds_array_off + cds_bytes;
    let sda_off = bds_off + bds_bytes;
    let nls_off = sda_off + SDA_LIVE_PREFIX_LEN;
    let layout = SysvarsLayout {
        drive_count,
        sft_slots,
        dpb_off,
        cds_array_off,
        bds_off,
        sda_off,
        nls_off,
    };
    let layout_end = base + 2 + layout.nls_off + NLS_TABLES_LEN;
    if layout_end > sysvars_limit {
        return Err(DosError::SystemLayoutTooSmall);
    }
    Ok(layout)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct NlsTablePointers {
    pub(super) uppercase: (u16, u16),
    pub(super) lowercase: (u16, u16),
    pub(super) filename_uppercase: (u16, u16),
    pub(super) filename_terminators: (u16, u16),
    pub(super) collating: (u16, u16),
}

/// A live host-file handle projected into the DOS System File Table.
#[derive(Debug, Clone, Copy)]
pub(super) struct SftHostFileEntry {
    pub(super) slot: u16,
    pub(super) open_mode: u16,
    pub(super) size: u32,
    pub(super) position: u32,
    pub(super) name: [u8; 11],
}

#[derive(Debug, Clone, Copy)]
pub(super) struct BlockDeviceBpb {
    pub(super) bytes_per_sector: u16,
    pub(super) cluster_mask: u8,
    pub(super) cluster_shift: u8,
    pub(super) first_fat_sector: u16,
    pub(super) fat_count: u8,
    pub(super) root_entries: u16,
    pub(super) total_sectors: u32,
    pub(super) first_data_sector: u16,
    pub(super) highest_cluster: u16,
    pub(super) sectors_per_fat: u16,
    pub(super) sectors_per_track: u16,
    pub(super) heads: u16,
    pub(super) hidden_sectors: u32,
    pub(super) first_root_sector: u16,
    pub(super) media: u8,
}

impl BlockDeviceBpb {
    pub(super) fn from_bytes(bytes: &[u8; BLOCK_BPB_LEN]) -> Option<Self> {
        let bytes_per_sector = u16::from_le_bytes([bytes[0], bytes[1]]);
        let sectors_per_cluster = bytes[2];
        let reserved_sectors = u16::from_le_bytes([bytes[3], bytes[4]]);
        let fat_count = bytes[5];
        let root_entries = u16::from_le_bytes([bytes[6], bytes[7]]);
        let total_sectors_small = u16::from_le_bytes([bytes[8], bytes[9]]);
        let media = bytes[10];
        let sectors_per_fat = u16::from_le_bytes([bytes[11], bytes[12]]);
        let sectors_per_track = u16::from_le_bytes([bytes[13], bytes[14]]);
        let heads = u16::from_le_bytes([bytes[15], bytes[16]]);
        let hidden_sectors = u32::from_le_bytes([bytes[17], bytes[18], bytes[19], bytes[20]]);
        let huge_sectors = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
        if bytes_per_sector == 0
            || sectors_per_cluster == 0
            || !sectors_per_cluster.is_power_of_two()
        {
            return None;
        }
        let first_root_sector = u32::from(reserved_sectors)
            .checked_add(u32::from(fat_count).checked_mul(u32::from(sectors_per_fat))?)?;
        let root_dir_bytes = u32::from(root_entries).checked_mul(32)?;
        let root_dir_sectors = root_dir_bytes.div_ceil(u32::from(bytes_per_sector));
        let first_data_sector = first_root_sector.checked_add(root_dir_sectors)?;
        let total_sectors = if total_sectors_small != 0 {
            u32::from(total_sectors_small)
        } else {
            huge_sectors
        };
        if total_sectors <= first_data_sector {
            return None;
        }
        let highest_cluster = total_sectors
            .checked_sub(first_data_sector)?
            .checked_div(u32::from(sectors_per_cluster))?
            .checked_add(1)?;
        Some(Self {
            bytes_per_sector,
            cluster_mask: sectors_per_cluster - 1,
            cluster_shift: sectors_per_cluster.trailing_zeros() as u8,
            first_fat_sector: reserved_sectors,
            fat_count,
            root_entries,
            total_sectors,
            first_data_sector: u16::try_from(first_data_sector).ok()?,
            highest_cluster: u16::try_from(highest_cluster).ok()?,
            sectors_per_fat,
            sectors_per_track,
            heads,
            hidden_sectors,
            first_root_sector: u16::try_from(first_root_sector).ok()?,
            media,
        })
    }

    fn to_bytes(self) -> [u8; BLOCK_BPB_LEN] {
        let mut bytes = [0u8; BLOCK_BPB_LEN];
        bytes[0..2].copy_from_slice(&self.bytes_per_sector.to_le_bytes());
        bytes[2] = self.cluster_mask + 1;
        bytes[3..5].copy_from_slice(&self.first_fat_sector.to_le_bytes());
        bytes[5] = self.fat_count;
        bytes[6..8].copy_from_slice(&self.root_entries.to_le_bytes());
        bytes[8..10].copy_from_slice(
            &(if self.total_sectors <= u32::from(u16::MAX) {
                self.total_sectors as u16
            } else {
                0
            })
            .to_le_bytes(),
        );
        bytes[10] = self.media;
        bytes[11..13].copy_from_slice(&self.sectors_per_fat.to_le_bytes());
        bytes[13..15].copy_from_slice(&self.sectors_per_track.to_le_bytes());
        bytes[15..17].copy_from_slice(&self.heads.to_le_bytes());
        bytes[17..21].copy_from_slice(&self.hidden_sectors.to_le_bytes());
        bytes[21..25].copy_from_slice(&self.total_sectors.to_le_bytes());
        bytes
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct BlockDeviceDpbEntry {
    pub(super) drive: u8,
    pub(super) unit: u8,
    pub(super) header: (u16, u16),
    pub(super) bpb: BlockDeviceBpb,
}

pub(super) struct SysvarsDevices<'a> {
    pub(super) host_files: &'a [SftHostFileEntry],
    pub(super) block_dpbs: &'a [BlockDeviceDpbEntry],
    pub(super) loaded_devices: &'a [(u16, u16)],
}

fn write_character_device_header(
    mem: &mut Memory,
    linear: usize,
    next_off: u16,
    next_seg: u16,
    attributes: u16,
    name: &[u8; 8],
) -> Result<(), DosError> {
    mem.write_u16(linear, next_off)?;
    mem.write_u16(linear + 2, next_seg)?;
    mem.write_u16(linear + 4, attributes)?;
    mem.write_u16(linear + 6, 0xffff)?;
    mem.write_u16(linear + 8, 0xffff)?;
    for (i, &byte) in name.iter().enumerate() {
        mem.write_u8(linear + 0x0a + i, byte)?;
    }
    Ok(())
}

fn write_sft_character_device(
    mem: &mut Memory,
    sft: usize,
    slot: usize,
    ref_count: u16,
    open_mode: u16,
    device_info: u16,
    name: &[u8; 11],
) -> Result<(), DosError> {
    let entry = sft + SFT_HEADER_LEN + slot * SFT_ENTRY_LEN;
    mem.write_u16(entry, ref_count)?;
    mem.write_u16(entry + 0x02, open_mode)?;
    mem.write_u8(entry + 0x04, 0)?;
    mem.write_u16(entry + 0x05, device_info)?;
    for (i, &byte) in name.iter().enumerate() {
        mem.write_u8(entry + 0x20 + i, byte)?;
    }
    Ok(())
}

/// Conventional memory modeled as an authoritative in-RAM MCB chain ending at
/// paragraph 0xA000. The chain is the source of truth: allocate/free/resize walk
/// and mutate the real headers a guest reads through AH=52h, so a memory manager
/// that rewrites a header in place drives the allocator. The arena itself holds
/// only the current program's PSP and the resident flag; the program top and free
/// base are read back from the chain. Allocation coalesces adjacent free MCBs and
/// honors the AH=58h first/best/last-fit method bits.
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

    /// AH=48h-style allocation using DOS's default first-fit method.
    pub(super) fn allocate(
        &mut self,
        paras: u16,
        mem: &mut Memory,
    ) -> Result<Result<u16, u16>, DosError> {
        self.allocate_fit(AllocFit::First, paras, mem)
    }

    fn allocate_fit(
        &mut self,
        fit: AllocFit,
        paras: u16,
        mem: &mut Memory,
    ) -> Result<Result<u16, u16>, DosError> {
        allocate_block(self.first_mcb(), fit, paras, mem)
    }

    /// AH=49h: free the block whose data segment is `seg`. Ok(Ok(())) on success,
    /// Ok(Err(())) for an unknown block, Err(DosError) on a guest memory fault. The
    /// block is marked owner-0 in the live MCB chain and adjacent free blocks are
    /// coalesced so the next allocation can reuse it regardless of free order.
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
        self.allocate_fit(AllocFit::First, paras, mem)
    }

    fn allocate_fit(
        self,
        fit: AllocFit,
        paras: u16,
        mem: &mut Memory,
    ) -> Result<Result<u16, u16>, DosError> {
        allocate_block(self.first_mcb, fit, paras, mem)
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
    let fit = AllocFit::from_strategy(alloc_strategy);
    match (area, linked_umb(umb, umb_link)) {
        (0x40, Some(u)) => u.allocate_fit(fit, paras, mem),
        (0x80, Some(u)) => match u.allocate_fit(fit, paras, mem)? {
            Ok(seg) => Ok(Ok(seg)),
            // Upper memory could not satisfy it: fall back to conventional. On a
            // double failure report the larger of the two arenas' free blocks, the
            // way DOS's single-chain walk reports the global largest block.
            Err(hi) => match arena.allocate_fit(fit, paras, mem)? {
                Ok(seg) => Ok(Ok(seg)),
                Err(lo) => Ok(Err(hi.max(lo))),
            },
        },
        _ => arena.allocate_fit(fit, paras, mem),
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

/// Retag an AH=48h allocation that landed in the UMB arena to the owning PSP.
/// XMS Request UMB calls do not use this path, so their manager-owned blocks keep
/// the allocator's default owner value.
pub(super) fn set_umb_owner(
    umb: Option<UmbArena>,
    seg: u16,
    owner: u16,
    mem: &mut Memory,
) -> Result<(), DosError> {
    if let Some(u) = umb {
        if u.contains_data(seg) {
            mem.write_u16(usize::from(seg.wrapping_sub(1)) * 16 + 1, owner)?;
        }
    }
    Ok(())
}

/// Free all UMB blocks owned by `owner`. Called from the shared `finish_exec`
/// teardown for every terminating child that did not keep resident, so it covers
/// the normal AH=4Ch / INT 20h exits and the abnormal Ctrl-C and critical-error
/// aborts alike; TSRs (AH=31h, INT 27h) are excluded and keep their UMBs.
pub(super) fn free_umb_blocks_owned_by(
    umb: Option<UmbArena>,
    owner: u16,
    mem: &mut Memory,
) -> Result<(), DosError> {
    let Some(u) = umb else {
        return Ok(());
    };
    let owned: Vec<u16> = read_mcb_chain(mem, u.first_mcb)
        .into_iter()
        .filter(|m| m.owner == owner)
        .map(|m| m.mcb_seg.wrapping_add(1))
        .collect();
    for seg in owned {
        let _ = u.free(seg, mem)?;
    }
    Ok(())
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
/// chain. The modeled scalar fields, SFT header, CDS array, and device-driver
/// headers are filled in; unmodeled chain pointers stay zero.
pub(super) fn write_sysvars(
    mem: &mut Memory,
    first_mcb: u16,
    ems_present: bool,
    lastdrive: Option<u8>,
    file_count: u16,
    devices: SysvarsDevices<'_>,
) -> Result<(u16, u16), DosError> {
    let base = usize::from(SYSVARS_SEG) * 16;
    let host_files = devices.host_files;
    let block_dpbs = devices.block_dpbs;
    let loaded_devices = devices.loaded_devices;
    let layout = sysvars_layout(first_mcb, lastdrive, file_count, block_dpbs.len())?;
    let drive_count = layout.drive_count;
    let sft_slots = layout.sft_slots;
    let dpb_off = layout.dpb_off;
    let cds_array_off = layout.cds_array_off;
    let cds_bytes = usize::from(drive_count) * CDS_ENTRY_LEN;
    let dpb_linear = base + 2 + dpb_off;
    let cds_linear = base + 2 + cds_array_off;
    let clear_end = 2 + cds_array_off + cds_bytes;
    // [BX-2] = first MCB segment (BX returns 0x0002, so this is offset 0).
    mem.write_u16(base, first_mcb)?;
    // Clear the documented field span plus the sized CDS array, then fill the
    // known fields over it.
    for off in 2..clear_end {
        mem.write_u8(base + off, 0)?;
    }
    // [BX+0x00] DWORD: pointer to the first Drive Parameter Block. This model has
    // one backed block device, C:, so the DPB chain contains a single fixed-disk
    // entry and terminates at FFFF:FFFF.
    mem.write_u16(base + 2, (2 + dpb_off) as u16)?;
    mem.write_u16(base + 2 + 2, SYSVARS_SEG)?;
    let largest_bytes_per_block = block_dpbs
        .iter()
        .map(|entry| entry.bpb.bytes_per_sector)
        .max()
        .unwrap_or(512)
        .max(512);
    mem.write_u16(base + 2 + 0x10, largest_bytes_per_block)?;
    // [BX+0x04] DWORD: pointer to the first System File Table.
    mem.write_u16(base + 2 + 0x04, (2 + SFT_TABLE_OFF) as u16)?;
    mem.write_u16(base + 2 + 0x06, SYSVARS_SEG)?;
    let sft = base + 2 + SFT_TABLE_OFF;
    mem.write_u16(sft, 0xffff)?; // next offset, FFFF:FFFF = last SFT table
    mem.write_u16(sft + 2, 0xffff)?; // next segment
    mem.write_u16(sft + 4, file_count)?; // number of SFT slots in this table
    // The PSP's default JFT maps stdin/stdout/stderr to SFT slot 1 (CON), AUX to
    // slot 3, and PRN to slot 4. Seed those character-device entries; live host-file
    // slots are filled below from the kernel's open-handle table.
    if sft_slots > 1 {
        write_sft_character_device(mem, sft, 1, 3, 0x0002, 0x0083, b"CON        ")?;
    }
    if sft_slots > 3 {
        write_sft_character_device(mem, sft, 3, 1, 0x0002, 0x0080, b"AUX        ")?;
    }
    if sft_slots > 4 {
        write_sft_character_device(mem, sft, 4, 1, 0x0001, 0x0080, b"PRN        ")?;
    }
    for host in host_files {
        let slot = usize::from(host.slot);
        if slot >= sft_slots {
            continue;
        }
        let entry = sft + SFT_HEADER_LEN + slot * SFT_ENTRY_LEN;
        mem.write_u16(entry, 1)?; // one JFT handle references this SFT slot
        mem.write_u16(entry + 0x02, host.open_mode)?;
        mem.write_u8(entry + 0x04, 0)?; // normal host file attributes
        mem.write_u16(entry + 0x05, 0x0002)?; // drive C:, bit 7 clear means file
        mem.write_u32(entry + 0x11, host.size)?;
        mem.write_u32(entry + 0x15, host.position)?;
        for (i, &byte) in host.name.iter().enumerate() {
            mem.write_u8(entry + 0x20 + i, byte)?;
        }
    }
    // DOS 4.x DPB for C:. Keep the values coherent with AH=36h's fixed-disk
    // facade: 512-byte sectors, 64 sectors per cluster, and an unknown-but-large
    // FAT16-style volume.
    mem.write_u8(dpb_linear, 2)?; // drive number: C:
    mem.write_u8(dpb_linear + 0x01, 0)?; // first unit within the block driver
    mem.write_u16(dpb_linear + 0x02, 512)?;
    mem.write_u8(dpb_linear + 0x04, 63)?; // sectors per cluster - 1
    mem.write_u8(dpb_linear + 0x05, 6)?; // 2^6 sectors per cluster
    mem.write_u16(dpb_linear + 0x06, 1)?; // reserved sectors
    mem.write_u8(dpb_linear + 0x08, 2)?; // FAT copies
    mem.write_u16(dpb_linear + 0x09, 512)?; // root directory entries
    mem.write_u16(dpb_linear + 0x0b, 545)?; // first data sector
    mem.write_u16(dpb_linear + 0x0d, 0xffff)?; // highest cluster number
    mem.write_u16(dpb_linear + 0x0f, 256)?; // sectors per FAT, DOS 4.x WORD
    mem.write_u16(dpb_linear + 0x11, 513)?; // first root directory sector
    mem.write_u16(dpb_linear + 0x13, 0xffff)?; // block device header not modeled yet
    mem.write_u16(dpb_linear + 0x15, 0xffff)?;
    mem.write_u8(dpb_linear + 0x17, 0xf8)?; // fixed disk media descriptor
    mem.write_u8(dpb_linear + 0x18, 0)?; // disk has been accessed
    if block_dpbs.is_empty() {
        mem.write_u16(dpb_linear + 0x19, 0xffff)?; // next DPB pointer, end of chain
        mem.write_u16(dpb_linear + 0x1b, 0xffff)?;
    } else {
        mem.write_u16(dpb_linear + 0x19, (2 + dpb_off + DPB_LEN) as u16)?;
        mem.write_u16(dpb_linear + 0x1b, SYSVARS_SEG)?;
    }
    mem.write_u16(dpb_linear + 0x1d, 2)?; // start free-space search at cluster 2
    mem.write_u16(dpb_linear + 0x1f, 0xf000)?; // free clusters, matching AH=36h
    for (index, entry) in block_dpbs.iter().enumerate() {
        let dpb = dpb_linear + (index + 1) * DPB_LEN;
        mem.write_u8(dpb, entry.drive)?;
        mem.write_u8(dpb + 0x01, entry.unit)?;
        mem.write_u16(dpb + 0x02, entry.bpb.bytes_per_sector)?;
        mem.write_u8(dpb + 0x04, entry.bpb.cluster_mask)?;
        mem.write_u8(dpb + 0x05, entry.bpb.cluster_shift)?;
        mem.write_u16(dpb + 0x06, entry.bpb.first_fat_sector)?;
        mem.write_u8(dpb + 0x08, entry.bpb.fat_count)?;
        mem.write_u16(dpb + 0x09, entry.bpb.root_entries)?;
        mem.write_u16(dpb + 0x0b, entry.bpb.first_data_sector)?;
        mem.write_u16(dpb + 0x0d, entry.bpb.highest_cluster)?;
        mem.write_u16(dpb + 0x0f, entry.bpb.sectors_per_fat)?;
        mem.write_u16(dpb + 0x11, entry.bpb.first_root_sector)?;
        mem.write_u16(dpb + 0x13, entry.header.1)?;
        mem.write_u16(dpb + 0x15, entry.header.0)?;
        mem.write_u8(dpb + 0x17, entry.bpb.media)?;
        mem.write_u8(dpb + 0x18, 0)?;
        if index + 1 == block_dpbs.len() {
            mem.write_u16(dpb + 0x19, 0xffff)?;
            mem.write_u16(dpb + 0x1b, 0xffff)?;
        } else {
            mem.write_u16(dpb + 0x19, (2 + dpb_off + (index + 2) * DPB_LEN) as u16)?;
            mem.write_u16(dpb + 0x1b, SYSVARS_SEG)?;
        }
        mem.write_u16(dpb + 0x1d, 2)?;
        mem.write_u16(dpb + 0x1f, 0xffff)?;
    }
    // [BX+0x16] DWORD: pointer to the Current Directory Structure array. Each
    // entry is 0x58 bytes and the count is published at [BX+0x21].
    mem.write_u16(base + 2 + 0x16, (2 + cds_array_off) as u16)?;
    mem.write_u16(base + 2 + 0x18, SYSVARS_SEG)?;
    mem.write_u8(
        base + 2 + 0x20,
        1 + u8::try_from(block_dpbs.len()).unwrap_or(u8::MAX),
    )?;
    // [BX+0x21] BYTE: LASTDRIVE.
    mem.write_u8(base + 2 + 0x21, drive_count)?;
    for index in 0..drive_count {
        let entry = cds_linear + usize::from(index) * CDS_ENTRY_LEN;
        let letter = b'A' + index;
        mem.write_u8(entry, letter)?;
        mem.write_u8(entry + 1, b':')?;
        mem.write_u8(entry + 2, b'\\')?;
        mem.write_u8(entry + 3, 0)?;
        // Mark C: and CONFIG.SYS-published block drivers as mounted local physical
        // drives. Other letters are reserved by LASTDRIVE but not backed here.
        let is_c = letter == b'C';
        let loaded = block_dpbs
            .iter()
            .position(|block| block.drive == index)
            .map(|pos| (2 + dpb_off + (pos + 1) * DPB_LEN) as u16);
        let dpb_ptr = if is_c {
            Some((2 + dpb_off) as u16)
        } else {
            loaded
        };
        mem.write_u16(entry + 0x43, if dpb_ptr.is_some() { 0x4000 } else { 0 })?;
        mem.write_u16(entry + 0x45, dpb_ptr.unwrap_or(0xffff))?;
        mem.write_u16(
            entry + 0x47,
            if dpb_ptr.is_some() {
                SYSVARS_SEG
            } else {
                0xffff
            },
        )?;
        mem.write_u16(entry + 0x49, 0)?; // current directory starts at root
        mem.write_u16(entry + 0x4b, 0xffff)?;
        mem.write_u16(entry + 0x4d, 0xffff)?;
        mem.write_u16(entry + 0x4f, 2)?; // hide "X:" from GETDIR-style views
    }
    // [BX+0x08] and [BX+0x0c] point at the active CLOCK$ and CON headers. NUL
    // remains the first linked device; EMMXXXX0, when present, stays directly
    // after NUL before the standard devices.
    let clock_ptr = (2 + CLOCK_DEVICE_OFF) as u16;
    let con_ptr = (2 + CON_DEVICE_OFF) as u16;
    mem.write_u16(base + 2 + 0x08, clock_ptr)?;
    mem.write_u16(base + 2 + 0x0a, SYSVARS_SEG)?;
    mem.write_u16(base + 2 + 0x0c, con_ptr)?;
    mem.write_u16(base + 2 + 0x0e, SYSVARS_SEG)?;

    let nul = base + 2 + NUL_DEVICE_OFF;
    let ems = base + 2 + EMM_DEVICE_OFF;
    let con = base + 2 + CON_DEVICE_OFF;
    let clock = base + 2 + CLOCK_DEVICE_OFF;
    if ems_present {
        write_character_device_header(
            mem,
            nul,
            (2 + EMM_DEVICE_OFF) as u16,
            SYSVARS_SEG,
            0x8004,
            b"NUL     ",
        )?;
        write_character_device_header(mem, ems, con_ptr, SYSVARS_SEG, 0xc000, b"EMMXXXX0")?;
    } else {
        write_character_device_header(mem, nul, con_ptr, SYSVARS_SEG, 0x8004, b"NUL     ")?;
    }
    write_character_device_header(mem, con, clock_ptr, SYSVARS_SEG, 0x8013, b"CON     ")?;
    write_character_device_header(mem, clock, 0xffff, 0xffff, 0x8008, b"CLOCK$  ")?;
    // Re-link any CONFIG.SYS-loaded drivers between NUL and the first built-in
    // device. write_sysvars rebuilds the skeleton on every AH=52h query, so the
    // loaded list is the source of truth and is spliced back in each time. The head
    // NUL points at when no drivers are loaded is the first built-in (EMM or CON).
    splice_loaded_devices(mem, nul, ems_present, con_ptr, loaded_devices)?;
    Ok((SYSVARS_SEG, 0x0002))
}

fn write_bds_bpb(
    mem: &mut Memory,
    linear: usize,
    bytes: &[u8; BLOCK_BPB_LEN],
) -> Result<(), DosError> {
    for (index, &byte) in bytes.iter().enumerate() {
        mem.write_u8(linear + index, byte)?;
    }
    Ok(())
}

fn default_c_bds_bpb() -> [u8; BLOCK_BPB_LEN] {
    let mut bytes = [0u8; BLOCK_BPB_LEN];
    let total_sectors = 1024u32 * 16 * 63;
    bytes[0..2].copy_from_slice(&512u16.to_le_bytes());
    bytes[2] = 64;
    bytes[3..5].copy_from_slice(&1u16.to_le_bytes());
    bytes[5] = 2;
    bytes[6..8].copy_from_slice(&512u16.to_le_bytes());
    bytes[8..10].copy_from_slice(&0u16.to_le_bytes());
    bytes[10] = 0xf8;
    bytes[11..13].copy_from_slice(&256u16.to_le_bytes());
    bytes[13..15].copy_from_slice(&63u16.to_le_bytes());
    bytes[15..17].copy_from_slice(&16u16.to_le_bytes());
    bytes[17..21].copy_from_slice(&63u32.to_le_bytes());
    bytes[21..25].copy_from_slice(&total_sectors.to_le_bytes());
    bytes
}

fn write_bds_link(
    mem: &mut Memory,
    linear: usize,
    next: Option<(u16, u16)>,
) -> Result<(), DosError> {
    let (off, seg) = next.unwrap_or((0xffff, 0xffff));
    mem.write_u16(linear, off)?;
    mem.write_u16(linear + 0x02, seg)?;
    Ok(())
}

fn write_bds_label(mem: &mut Memory, linear: usize, fs_type: &[u8; 8]) -> Result<(), DosError> {
    for (index, &byte) in b"NO NAME    ".iter().enumerate() {
        mem.write_u8(linear + 0x4b + index, byte)?;
    }
    mem.write_u8(linear + 0x56, 0)?;
    mem.write_u32(linear + 0x57, 0)?;
    for (index, &byte) in fs_type.iter().enumerate() {
        mem.write_u8(linear + 0x5b + index, byte)?;
    }
    mem.write_u8(linear + 0x63, 0)?;
    Ok(())
}

fn write_default_c_bds_entry(
    mem: &mut Memory,
    linear: usize,
    next: Option<(u16, u16)>,
) -> Result<(), DosError> {
    let bpb = default_c_bds_bpb();
    write_bds_link(mem, linear, next)?;
    mem.write_u8(linear + 0x04, 0x80)?; // INT 13h fixed disk 0
    mem.write_u8(linear + 0x05, 2)?; // C:
    write_bds_bpb(mem, linear + 0x06, &bpb)?;
    mem.write_u8(linear + 0x1f, 0x40)?; // FAT16
    mem.write_u16(linear + 0x20, 0)?;
    mem.write_u8(linear + 0x22, 0)?;
    mem.write_u16(linear + 0x23, 0x0001)?; // fixed media
    mem.write_u16(linear + 0x25, 1024)?;
    write_bds_bpb(mem, linear + 0x27, &bpb)?;
    for off in 0x40..0x46 {
        mem.write_u8(linear + off, 0)?;
    }
    mem.write_u8(linear + 0x46, 0xff)?;
    mem.write_u16(linear + 0x47, 0xffff)?;
    mem.write_u16(linear + 0x49, 0xffff)?;
    write_bds_label(mem, linear, b"FAT16   ")
}

fn write_loaded_bds_entry(
    mem: &mut Memory,
    linear: usize,
    next: Option<(u16, u16)>,
    entry: &BlockDeviceDpbEntry,
) -> Result<(), DosError> {
    let bpb = entry.bpb;
    let bpb_bytes = bpb.to_bytes();
    let fat16 = bpb.highest_cluster >= 0x0ff6;
    let cylinders = if bpb.sectors_per_track != 0 && bpb.heads != 0 {
        (bpb.total_sectors / u32::from(bpb.sectors_per_track) / u32::from(bpb.heads))
            .min(u32::from(u16::MAX)) as u16
    } else {
        0
    };
    write_bds_link(mem, linear, next)?;
    mem.write_u8(linear + 0x04, entry.unit)?;
    mem.write_u8(linear + 0x05, entry.drive)?;
    write_bds_bpb(mem, linear + 0x06, &bpb_bytes)?;
    mem.write_u8(linear + 0x1f, if fat16 { 0x40 } else { 0x00 })?;
    mem.write_u16(linear + 0x20, 0)?;
    mem.write_u8(linear + 0x22, 0)?;
    mem.write_u16(
        linear + 0x23,
        if bpb.media == 0xf8 { 0x0001 } else { 0x0000 },
    )?;
    mem.write_u16(linear + 0x25, cylinders)?;
    write_bds_bpb(mem, linear + 0x27, &bpb_bytes)?;
    for off in 0x40..0x46 {
        mem.write_u8(linear + off, 0)?;
    }
    mem.write_u8(linear + 0x46, 0xff)?;
    mem.write_u32(linear + 0x47, 0xffff_ffff)?;
    write_bds_label(mem, linear, if fat16 { b"FAT16   " } else { b"FAT12   " })
}

/// INT 2Fh AX=0803h exposes DOS 4.x drive data tables for DRIVER.SYS support.
/// The table lives inside the same reserved SysVars paragraph as the DPB/CDS/SDA
/// data, so the returned far pointer stays stable until the next DOS data rebuild.
pub(super) fn write_driver_bds(
    mem: &mut Memory,
    first_mcb: u16,
    lastdrive: Option<u8>,
    file_count: u16,
    block_dpbs: &[BlockDeviceDpbEntry],
) -> Result<(u16, u16), DosError> {
    let layout = sysvars_layout(first_mcb, lastdrive, file_count, block_dpbs.len())?;
    let first_off = (2 + layout.bds_off) as u16;
    let base = usize::from(SYSVARS_SEG) * 16 + usize::from(first_off);
    let total = 1 + block_dpbs.len();
    for index in 0..total * BDS_ENTRY_LEN {
        mem.write_u8(base + index, 0)?;
    }

    let next_for = |index: usize| {
        if index + 1 == total {
            None
        } else {
            Some((
                (2 + layout.bds_off + (index + 1) * BDS_ENTRY_LEN) as u16,
                SYSVARS_SEG,
            ))
        }
    };
    write_default_c_bds_entry(mem, base, next_for(0))?;
    for (index, entry) in block_dpbs.iter().enumerate() {
        write_loaded_bds_entry(
            mem,
            base + (index + 1) * BDS_ENTRY_LEN,
            next_for(index + 1),
            entry,
        )?;
    }
    Ok((SYSVARS_SEG, first_off))
}

fn write_case_table(
    mem: &mut Memory,
    linear: usize,
    start: u8,
    len: usize,
    lowercase: bool,
) -> Result<(), DosError> {
    mem.write_u16(linear, len as u16)?;
    for index in 0..len {
        let byte = start.wrapping_add(index as u8);
        let mapped = if lowercase {
            byte.to_ascii_lowercase()
        } else {
            byte.to_ascii_uppercase()
        };
        mem.write_u8(linear + 2 + index, mapped)?;
    }
    Ok(())
}

fn write_collating_table(mem: &mut Memory, linear: usize) -> Result<(), DosError> {
    mem.write_u16(linear, 0x0100)?;
    for byte in 0u16..=0xff {
        mem.write_u8(linear + 2 + usize::from(byte), byte as u8)?;
    }
    Ok(())
}

fn write_filename_terminator_table(mem: &mut Memory, linear: usize) -> Result<(), DosError> {
    mem.write_u16(linear, (NLS_FILENAME_TERMINATOR_TABLE_LEN - 2) as u16)?;
    mem.write_u8(linear + 0x02, 0x01)?;
    mem.write_u8(linear + 0x03, 0x00)?;
    mem.write_u8(linear + 0x04, 0xff)?;
    mem.write_u8(linear + 0x05, 0x00)?;
    mem.write_u8(linear + 0x06, b'\\')?;
    mem.write_u8(linear + 0x07, b'/')?;
    mem.write_u8(linear + 0x08, 0x02)?;
    mem.write_u8(linear + 0x09, NLS_FILENAME_TERMINATORS.len() as u8)?;
    for (index, &byte) in NLS_FILENAME_TERMINATORS.iter().enumerate() {
        mem.write_u8(linear + 0x0a + index, byte)?;
    }
    Ok(())
}

/// INT 21h AH=65h subfunctions 02h-06h return pointers to NLS tables. Toka-DOS
/// currently ships only the US/CP437 defaults, so extended-byte maps are identity
/// maps and ASCII is folded for the full lowercase table.
pub(super) fn write_nls_tables(
    mem: &mut Memory,
    first_mcb: u16,
    lastdrive: Option<u8>,
    file_count: u16,
    published_block_units: usize,
) -> Result<NlsTablePointers, DosError> {
    let layout = sysvars_layout(first_mcb, lastdrive, file_count, published_block_units)?;
    let mut off = (2 + layout.nls_off) as u16;
    let mut linear = usize::from(SYSVARS_SEG) * 16 + usize::from(off);

    let uppercase = (SYSVARS_SEG, off);
    write_case_table(mem, linear, 0x80, 0x80, false)?;
    off = off.wrapping_add(NLS_UPPER_TABLE_LEN as u16);
    linear += NLS_UPPER_TABLE_LEN;

    let lowercase = (SYSVARS_SEG, off);
    write_case_table(mem, linear, 0x00, 0x100, true)?;
    off = off.wrapping_add(NLS_LOWER_TABLE_LEN as u16);
    linear += NLS_LOWER_TABLE_LEN;

    let filename_uppercase = (SYSVARS_SEG, off);
    write_case_table(mem, linear, 0x80, 0x80, false)?;
    off = off.wrapping_add(NLS_FILENAME_UPPER_TABLE_LEN as u16);
    linear += NLS_FILENAME_UPPER_TABLE_LEN;

    let filename_terminators = (SYSVARS_SEG, off);
    write_filename_terminator_table(mem, linear)?;
    off = off.wrapping_add(NLS_FILENAME_TERMINATOR_TABLE_LEN as u16);
    linear += NLS_FILENAME_TERMINATOR_TABLE_LEN;

    let collating = (SYSVARS_SEG, off);
    write_collating_table(mem, linear)?;

    Ok(NlsTablePointers {
        uppercase,
        lowercase,
        filename_uppercase,
        filename_terminators,
        collating,
    })
}

/// Insert the loaded-driver headers into the chain after NUL: NUL -> driver[0] ->
/// ... -> driver[n] -> the first built-in device. The list is most-recently-loaded
/// first, so driver[0] is the last `.SYS` CONFIG.SYS loaded and sits nearest NUL.
/// Called on every SysVars rebuild so the loaded list survives the rebuild. Each
/// far pointer is (segment, offset) of a loaded device header.
fn splice_loaded_devices(
    mem: &mut Memory,
    nul: usize,
    ems_present: bool,
    con_ptr: u16,
    loaded_devices: &[(u16, u16)],
) -> Result<(), DosError> {
    if loaded_devices.is_empty() {
        return Ok(());
    }
    let builtin_head = if ems_present {
        ((2 + EMM_DEVICE_OFF) as u16, SYSVARS_SEG)
    } else {
        (con_ptr, SYSVARS_SEG)
    };
    // NUL points at the first loaded driver.
    let (first_seg, first_off) = loaded_devices[0];
    mem.write_u16(nul, first_off)?;
    mem.write_u16(nul + 2, first_seg)?;
    // Each loaded driver points at the next; the last points at the first built-in.
    for (i, &(seg, off)) in loaded_devices.iter().enumerate() {
        let header = usize::from(seg) * 16 + usize::from(off);
        let (next_off, next_seg) = match loaded_devices.get(i + 1) {
            Some(&(ns, no)) => (no, ns),
            None => builtin_head,
        };
        mem.write_u16(header, next_off)?;
        mem.write_u16(header + 2, next_seg)?;
    }
    Ok(())
}

/// Refresh the DOS 4.x-style live prefix of the Swappable Data Area and return
/// its far pointer. The large DOS internal stacks and file-operation scratch that
/// follow the prefix are deliberately parked for now, so AX=5D06h reports no
/// in-DOS-only swap area and only this 0x1A-byte always-swapped prefix.
pub(super) fn write_sda(
    mem: &mut Memory,
    first_mcb: u16,
    lastdrive: Option<u8>,
    file_count: u16,
    published_block_units: usize,
    snapshot: SdaSnapshot,
) -> Result<(u16, u16), DosError> {
    let layout = sysvars_layout(first_mcb, lastdrive, file_count, published_block_units)?;
    let sda_off = (2 + layout.sda_off) as u16;
    let sda = usize::from(SYSVARS_SEG) * 16 + usize::from(sda_off);
    for off in 0..SDA_LIVE_PREFIX_LEN {
        mem.write_u8(sda + off, 0)?;
    }

    if let Some(critical_error) = snapshot.critical_error {
        mem.write_u8(sda, 1)?; // critical-error flag, an INT 24h path is active
        mem.write_u8(sda + 0x01, 1)?; // DOS is busy while the handler is pending
        mem.write_u8(sda + 0x02, critical_error.drive)?;
    } else {
        mem.write_u8(sda, 0)?; // critical-error flag, no INT 24h path is active
        mem.write_u8(sda + 0x01, 0)?; // InDOS count is clear between HLE calls
        mem.write_u8(sda + 0x02, 0xff)?; // no current critical-error drive
    }
    mem.write_u8(sda + 0x03, 0x01)?; // AH=59h locus: unknown/not appropriate
    mem.write_u16(sda + 0x04, snapshot.last_error)?;
    mem.write_u8(sda + 0x06, 0x05)?; // AH=59h action: immediate abort
    mem.write_u8(sda + 0x07, 0x0d)?; // AH=59h class: unknown/other
    // 0x08 ES:DI media-ID pointer is only meaningful for disk-change errors, which
    // the HLE does not generate, so the zero filled pointer remains parked.
    mem.write_u16(sda + 0x0c, snapshot.current_dta.1)?;
    mem.write_u16(sda + 0x0e, snapshot.current_dta.0)?;
    mem.write_u16(sda + 0x10, snapshot.current_psp)?;
    // 0x12 SP across INT 23h is parked until Ctrl-C far calls exist.
    mem.write_u16(
        sda + 0x14,
        u16::from(snapshot.last_exit_code) | (u16::from(snapshot.last_exit_type) << 8),
    )?;
    mem.write_u8(sda + 0x16, 2)?; // current drive C: (0 = A:)
    mem.write_u8(sda + 0x17, 0)?; // extended break flag off
    mem.write_u8(sda + 0x18, 0)?; // code page switching flag parked
    mem.write_u8(sda + 0x19, 0)?; // INT 24 abort code-page flag parked
    Ok((SYSVARS_SEG, sda_off))
}

pub(super) fn write_sda_list(
    mem: &mut Memory,
    first_mcb: u16,
    lastdrive: Option<u8>,
    file_count: u16,
    published_block_units: usize,
    snapshot: SdaSnapshot,
) -> Result<(u16, u16), DosError> {
    let (seg, sda_off) = write_sda(
        mem,
        first_mcb,
        lastdrive,
        file_count,
        published_block_units,
        snapshot,
    )?;
    let list = usize::from(SDA_LIST_SEG) * 16 + usize::from(SDA_LIST_OFF);
    mem.write_u16(list, 1)?;
    mem.write_u16(list + 2, sda_off)?;
    mem.write_u16(list + 4, seg)?;
    mem.write_u16(list + 6, 0x8000 | SDA_ALWAYS_SWAPPED_LEN)?;
    Ok((SDA_LIST_SEG, SDA_LIST_OFF))
}

/// Write one MCB header into guest RAM: the signature ('M' link or 'Z' last), the
/// owner PSP word (0 = free), the data size in paragraphs, three reserved bytes,
/// and the 8-byte owner name. The header occupies the paragraph at `seg`; the
/// block's data starts at `seg + 1`.
/// Stamp the owner field of the MCB whose data segment is `seg`, so a resident
/// driver block is owned by the system PSP and never reclaimed at program exit.
/// The MCB header is the paragraph below the data segment; owner is at +1.
pub(super) fn stamp_mcb_owner(mem: &mut Memory, seg: u16, owner: u16) -> Result<(), DosError> {
    let mcb = usize::from(seg.wrapping_sub(1)) * 16;
    mem.write_u16(mcb + 1, owner)?;
    Ok(())
}

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

pub(super) fn mcb_chain_is_complete(mem: &Memory, first_mcb: u16) -> bool {
    let mut seg = first_mcb;
    for _ in 0..MCB_WALK_CAP {
        let base = usize::from(seg) * 16;
        let (Ok(sig), Ok(size)) = (mem.read_u8(base), mem.read_u16(base + 3)) else {
            return false;
        };
        if sig != b'M' && sig != b'Z' {
            return false;
        }
        if sig == b'Z' {
            return true;
        }
        let next = seg.wrapping_add(1).wrapping_add(size);
        if next <= seg {
            return false;
        }
        seg = next;
    }
    false
}

/// The free tail of the chain rooted at `first_mcb`: (header seg, data size) of
/// the last block when it is free (owner 0), else None when the region is full.
pub(super) fn free_tail(first_mcb: u16, mem: &Memory) -> Option<(u16, u16)> {
    match read_mcb_chain(mem, first_mcb).last() {
        Some(last) if last.owner == 0 => Some((last.mcb_seg, last.size)),
        _ => None,
    }
}

fn coalesce_free_blocks(first_mcb: u16, mem: &mut Memory) -> Result<(), DosError> {
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
) -> Result<u16, DosError> {
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
) -> Result<Result<u16, u16>, DosError> {
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
pub(super) fn free_block(
    first_mcb: u16,
    _top: u16,
    seg: u16,
    mem: &mut Memory,
) -> Result<Result<(), ()>, DosError> {
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
pub(super) fn resize_block(
    first_mcb: u16,
    top: u16,
    seg: u16,
    paras: u16,
    mem: &mut Memory,
) -> Result<Result<(), ResizeError>, DosError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_sysvars_keeps_the_bios_ram_stub_cluster() {
        // The BIOS keeps its RAM IRET/RTC/halt/critical-error stubs at linear
        // 0x600..0x63F. SysVars must sit above them, or a guest AH=52h would zero
        // the IRET stub it returns through. Seed marker bytes across the cluster.
        let mut mem = Memory::new(1024 * 1024).unwrap();
        for addr in 0x600..0x640 {
            mem.write_u8(addr, 0xcf).unwrap();
        }
        // A low first_mcb forces a large SysVars structure (the worst case for
        // overrunning low memory toward the stub cluster).
        write_sysvars(
            &mut mem,
            0x0100,
            false,
            Some(5),
            40,
            SysvarsDevices {
                host_files: &[],
                block_dpbs: &[],
                loaded_devices: &[],
            },
        )
        .unwrap();
        for addr in 0x600..0x640 {
            assert_eq!(
                mem.read_u8(addr).unwrap(),
                0xcf,
                "SysVars must not write into the BIOS RAM stub cluster at {addr:#x}"
            );
        }
    }

    #[test]
    fn write_sysvars_rejects_worst_case_layout_below_old_0100_psp() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut raw = [0u8; BLOCK_BPB_LEN];
        raw[0..2].copy_from_slice(&512u16.to_le_bytes());
        raw[2] = 1;
        raw[3..5].copy_from_slice(&1u16.to_le_bytes());
        raw[5] = 2;
        raw[6..8].copy_from_slice(&224u16.to_le_bytes());
        raw[8..10].copy_from_slice(&2880u16.to_le_bytes());
        raw[10] = 0xf0;
        raw[11..13].copy_from_slice(&9u16.to_le_bytes());
        raw[13..15].copy_from_slice(&18u16.to_le_bytes());
        raw[15..17].copy_from_slice(&2u16.to_le_bytes());
        let bpb = BlockDeviceBpb::from_bytes(&raw).unwrap();
        let block_dpbs: Vec<_> = (0..23)
            .map(|unit| BlockDeviceDpbEntry {
                drive: 3 + unit,
                unit,
                header: (0x2000 + u16::from(unit), 0),
                bpb,
            })
            .collect();

        let err = write_sysvars(
            &mut mem,
            0x00ff,
            false,
            Some(b'Z' - b'A' + 1),
            40,
            SysvarsDevices {
                host_files: &[],
                block_dpbs: &block_dpbs,
                loaded_devices: &[],
            },
        )
        .unwrap_err();
        assert!(matches!(err, DosError::SystemLayoutTooSmall));
        write_sysvars(
            &mut mem,
            0x01ff,
            false,
            Some(b'Z' - b'A' + 1),
            40,
            SysvarsDevices {
                host_files: &[],
                block_dpbs: &block_dpbs,
                loaded_devices: &[],
            },
        )
        .unwrap();
    }
}
