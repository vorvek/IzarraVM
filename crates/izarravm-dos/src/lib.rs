use izarravm_bus::{BusError, Memory};
use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DosError {
    #[error("C: drive root does not exist: {0}")]
    MissingDriveRoot(PathBuf),
    #[error("C: drive root is not a directory: {0}")]
    DriveRootIsNotDirectory(PathBuf),
    #[error("only C: drive paths are supported in the current scaffold: {0}")]
    UnsupportedDrive(String),
    #[error("DOS path attempts to leave the mounted drive: {0}")]
    PathTraversal(String),
    #[error("DOS memory access failed: {0}")]
    Memory(#[from] BusError),
    #[error("COM image is too large at {0} bytes (max 65280)")]
    ComTooLarge(usize),
    #[error("not an MZ executable (bad signature)")]
    BadExeSignature,
    #[error("MZ image is truncated: {0}")]
    ExeImageTruncated(&'static str),
    #[error("MZ relocation points outside the load module")]
    ExeRelocationOutOfRange,
    #[error("not enough memory to load the MZ image ({needed} paragraphs, {available} available)")]
    ExeNotEnoughMemory { needed: u32, available: u32 },
    #[error("not enough conventional memory for the environment segment")]
    EnvSegmentFull,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDrive {
    letter: char,
    root: PathBuf,
    read_only: bool,
}

impl HostDrive {
    pub fn mount_c(root: impl AsRef<Path>) -> Result<Self, DosError> {
        let root = root.as_ref();
        if !root.exists() {
            return Err(DosError::MissingDriveRoot(root.to_owned()));
        }
        if !root.is_dir() {
            return Err(DosError::DriveRootIsNotDirectory(root.to_owned()));
        }

        Ok(Self {
            letter: 'C',
            root: root.to_owned(),
            read_only: false,
        })
    }

    pub fn letter(&self) -> char {
        self.letter
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn resolve_dos_path(&self, dos_path: &str) -> Result<PathBuf, DosError> {
        let normalized = dos_path.trim();
        let Some(after_drive) = normalized
            .strip_prefix("C:")
            .or_else(|| normalized.strip_prefix("c:"))
        else {
            return Err(DosError::UnsupportedDrive(dos_path.to_owned()));
        };

        let mut resolved = self.root.clone();
        for component in after_drive
            .trim_start_matches(['\\', '/'])
            .split(['\\', '/'])
            .filter(|component| !component.is_empty() && *component != ".")
        {
            if component == ".." {
                return Err(DosError::PathTraversal(dos_path.to_owned()));
            }
            resolved.push(component);
        }

        Ok(resolved)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DosKernelServices {
    pub c_drive: HostDrive,
}

impl DosKernelServices {
    pub fn new(c_drive: HostDrive) -> Self {
        Self { c_drive }
    }
}

/// The registers a real-mode INT 21h handler reads and writes. The machine
/// marshals these to and from the CPU register file around a dispatch, so the
/// kernel stays free of any CPU-crate dependency.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DosRegs {
    pub ax: u16,
    pub bx: u16,
    pub cx: u16,
    pub dx: u16,
    pub si: u16,
    pub di: u16,
    pub ds: u16, // segment selector
    pub es: u16, // segment selector
    pub cf: bool,
    pub zf: bool,
}

/// What the caller should do after a handled software interrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DosAction {
    Continue, // results are in DosRegs; the IRET stub returns to the caller
    Exit(u8), // terminate the program with this code
    /// AH=4Bh AL=0: switch the CPU to the child. The kernel has saved the parent
    /// context and built the child PSP/environment; the machine snapshots the
    /// parent CPU, applies `entry`, and sets the child AX to `child_ax` (FCB
    /// drive-validity AL/AH per RBIL). The child runs until it exits.
    Exec {
        entry: ProgramEntry,
        child_ax: u16,
    },
}

/// Conventional memory modeled as one block ending at paragraph 0xA000. The
/// program owns [psp_seg, prog_top); AH=48h blocks stack upward from free_base.
/// Bump allocator with a single resizable program block and LIFO
/// reclaim; no MCB chain, no free-list coalescing, no UMB/HIMEM.
#[derive(Debug, Default)]
struct Arena {
    psp_seg: u16,
    prog_top: u16,
    free_base: u16,
    blocks: Vec<(u16, u16)>, // (segment, paragraphs) in allocation order
}

const ARENA_TOP: u16 = 0xa000; // matches CONVENTIONAL_TOP_PARAGRAPH in the loader

enum ResizeError {
    TooBig(u16), // largest paragraphs that would fit
    InvalidBlock,
}

impl Arena {
    /// AH=48h: allocate `paras` paragraphs. Ok(segment) or Err(largest-available).
    fn allocate(&mut self, paras: u16) -> Result<u16, u16> {
        let end = u32::from(self.free_base) + u32::from(paras);
        if end <= u32::from(ARENA_TOP) {
            let seg = self.free_base;
            self.blocks.push((seg, paras));
            self.free_base = end as u16;
            Ok(seg)
        } else {
            Err(ARENA_TOP - self.free_base)
        }
    }

    /// AH=4Ah: resize the block at `seg` to `paras` paragraphs.
    fn resize(&mut self, seg: u16, paras: u16) -> Result<(), ResizeError> {
        if seg == self.psp_seg {
            // The program block. Its ceiling is the lowest AH=48h block, or ARENA_TOP.
            let limit = self
                .blocks
                .iter()
                .map(|&(s, _)| s)
                .min()
                .unwrap_or(ARENA_TOP);
            let new_top = u32::from(self.psp_seg) + u32::from(paras);
            if new_top <= u32::from(limit) {
                self.prog_top = new_top as u16;
                if self.blocks.is_empty() {
                    self.free_base = self.prog_top;
                }
                Ok(())
            } else {
                Err(ResizeError::TooBig(limit - self.psp_seg))
            }
        } else if let Some(idx) = self.blocks.iter().position(|&(s, _)| s == seg) {
            if idx + 1 == self.blocks.len() {
                // Top block: grow/shrink against the ceiling, moving free_base.
                let new_end = u32::from(seg) + u32::from(paras);
                if new_end <= u32::from(ARENA_TOP) {
                    self.blocks[idx].1 = paras;
                    self.free_base = new_end as u16;
                    Ok(())
                } else {
                    Err(ResizeError::TooBig(ARENA_TOP - seg))
                }
            } else {
                // Non-top block: shrink updates the size (no reclaim); grow fails.
                let cur = self.blocks[idx].1;
                if paras <= cur {
                    self.blocks[idx].1 = paras;
                    Ok(())
                } else {
                    Err(ResizeError::TooBig(cur))
                }
            }
        } else {
            Err(ResizeError::InvalidBlock)
        }
    }

    /// AH=49h: free the block at `seg`. Ok(()) or Err(()) for an unknown block.
    fn free(&mut self, seg: u16) -> Result<(), ()> {
        if seg == self.psp_seg {
            return Ok(()); // freeing the program block (e.g. at termination)
        }
        if let Some(idx) = self.blocks.iter().position(|&(s, _)| s == seg) {
            let is_top = idx + 1 == self.blocks.len();
            let (_, paras) = self.blocks.remove(idx);
            if is_top {
                self.free_base -= paras; // LIFO reclaim
            }
            Ok(())
        } else {
            Err(())
        }
    }
}

/// Toka-DOS wall clock. Deterministic by default (a fixed 1997 instant) so unit
/// tests are stable; the machine/CLI may overwrite it via set_clock. Fields are
/// stored explicitly, including day_of_week, to avoid any calendar computation.
#[derive(Debug, Clone, Copy)]
struct DosDateTime {
    year: u16,
    month: u8,
    day: u8,
    day_of_week: u8, // 0 = Sunday
    hour: u8,
    minute: u8,
    second: u8,
    hundredths: u8,
}

impl Default for DosDateTime {
    fn default() -> Self {
        // 1997-06-17 (a Tuesday, day_of_week = 2), 12:00:00.00.
        Self {
            year: 1997,
            month: 6,
            day: 17,
            day_of_week: 2,
            hour: 12,
            minute: 0,
            second: 0,
            hundredths: 0,
        }
    }
}

/// A DOS file access mode, from AL's low 3 bits on open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessMode {
    Read,
    Write,
    ReadWrite,
}

impl AccessMode {
    /// AL's low 3 bits select the access mode (the high bits are sharing and
    /// inheritance, ignored). 0=read, 1=write, 2=read/write; 3-7 are unused by
    /// real programs and map to Read (marked).
    fn from_open_al(al: u8) -> Self {
        match al & 0x07 {
            1 => AccessMode::Write,
            2 => AccessMode::ReadWrite,
            _ => AccessMode::Read,
        }
    }

    fn can_read(self) -> bool {
        matches!(self, AccessMode::Read | AccessMode::ReadWrite)
    }

    fn can_write(self) -> bool {
        matches!(self, AccessMode::Write | AccessMode::ReadWrite)
    }
}

/// An open file handle: the host file plus the DOS access mode it was opened
/// with, which the kernel enforces on reads and writes.
#[derive(Debug)]
struct OpenFile {
    file: File,
    mode: AccessMode,
}

/// Open an existing host file for a DOS access mode (no create).
fn open_host_file(path: &Path, mode: AccessMode) -> std::io::Result<File> {
    match mode {
        AccessMode::Read => File::open(path),
        AccessMode::Write => OpenOptions::new().write(true).open(path),
        AccessMode::ReadWrite => OpenOptions::new().read(true).write(true).open(path),
    }
}

/// One entry of a FindFirst/FindNext result: the documented DTA fields plus the
/// uppercase 8.3 name to write into the 13-byte ASCIIZ slot.
#[derive(Debug, Clone)]
struct FindEntry {
    attr: u8,
    time: u16, // packed DOS time (RBIL #01665)
    date: u16, // packed DOS date (RBIL #01666)
    size: u32,
    name: String, // uppercase 8.3, e.g. "LEVEL1.DAT"
}

/// A live directory search: the snapshot of matching entries and the cursor into
/// it, keyed in the kernel by the DTA address. The whole match list is
/// taken once at FindFirst; host directory changes between calls are not seen
/// (real DOS re-walks and per RBIL may even return the same file twice, so
/// neither is "correct"; ours is stable). The cursor lives here, not in the DTA
/// reserved bytes, so relocating or copying the DTA mid-search is not honored.
#[derive(Debug)]
struct FindSearch {
    entries: Vec<FindEntry>,
    next: usize,
}

/// Saved per-program DOS state, pushed when a child is EXECed (AL=0) and
/// restored when the child exits. open_files is NOT saved; parent and child
/// share one handle table (real DOS refcounts handles 0-4 into the child's JFT
/// and closes inherited handles on exit, neither of which we model, marked).
#[derive(Debug)]
struct ProgramContext {
    arena: Arena,
    dta: (u16, u16),
    find_searches: HashMap<(u16, u16), FindSearch>,
}

/// The stateful DOS kernel. Owns the host-side state that must survive between
/// INT 21h calls: the open-file handle table and the mounted C: drive, plus the
/// standard input and output buffers (high-level emulated, HLE). The machine
/// holds one of these and calls `dispatch` from its INT 21h handler.
#[derive(Debug, Default)]
pub struct DosKernel {
    drive: Option<HostDrive>,
    // File handles 5 and up: AH=3Dh inserts, AH=3Fh/3Eh look up.
    open_files: HashMap<u16, OpenFile>,
    stdin: VecDeque<u8>,
    stdout: Vec<u8>,
    clock: DosDateTime,
    arena: Arena,
    dta: (u16, u16),
    find_searches: HashMap<(u16, u16), FindSearch>,
    // Parent program frames for nested EXEC (AL=0); restored on child exit.
    program_stack: Vec<ProgramContext>,
    last_exit_code: u8, // AH=4Dh AL; cleared after it is read
    last_exit_type: u8, // AH=4Dh AH; always 0x00 (normal termination), marked
}

impl DosKernel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount a host directory as the C: drive. File opens resolve against it.
    pub fn mount_c(&mut self, drive: HostDrive) {
        self.drive = Some(drive);
    }

    /// Replace the standard-input buffer, consumed front to back by the
    /// character-input calls.
    pub fn set_stdin(&mut self, bytes: &[u8]) {
        self.stdin = bytes.iter().copied().collect();
    }

    /// The bytes written to standard output so far.
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    /// Replace the wall clock (host-time wiring is a later option; default is fixed).
    #[allow(clippy::too_many_arguments)]
    pub fn set_clock(
        &mut self,
        year: u16,
        month: u8,
        day: u8,
        day_of_week: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) {
        self.clock = DosDateTime {
            year,
            month,
            day,
            day_of_week,
            hour,
            minute,
            second,
            hundredths: 0,
        };
    }

    /// Seed per-program state after a program loads: the memory arena spanning
    /// [psp_seg, ARENA_TOP) with the program owning [psp_seg, prog_top). The
    /// machine calls this from new_dos_program; prog_top is the PSP:0x02 value.
    pub fn init_program(&mut self, psp_seg: u16, prog_top: u16) {
        self.arena = Arena {
            psp_seg,
            prog_top,
            free_base: prog_top,
            blocks: Vec::new(),
        };
        self.dta = (psp_seg, 0x80);
        self.find_searches.clear();
        self.program_stack.clear();
        self.last_exit_code = 0;
        self.last_exit_type = 0;
    }

    /// Allocate the DOS environment segment, write the env block in the real DOS
    /// format, and record its segment in `PSP:0x2C`. Each entry becomes an ASCIIZ
    /// `KEY=VALUE` string; the block ends with the empty-string terminator. The
    /// segment is allocated in whole paragraphs above the program block via the
    /// arena, so it sits where real DOS places it and a guest `AH=49h`/`AH=4Ah`
    /// around it behaves as on real hardware. The machine calls this from
    /// `new_dos_program` after `init_program`. With no entries a valid (empty)
    /// environment is still allocated so `PSP:0x2C` is always a live pointer.
    pub fn install_environment(
        &mut self,
        mem: &mut Memory,
        entries: &[(&str, &str)],
    ) -> Result<(), DosError> {
        let block = build_env_block(entries);
        let paras = u16::try_from(block.len().div_ceil(16)).unwrap_or(u16::MAX);
        let psp_base = usize::from(self.arena.psp_seg) * 16;
        // The program block may have claimed all of conventional memory (an .EXE
        // with a large e_maxalloc sets PSP:0x02 = ARENA_TOP). Carve env room out
        // of the top of the program block, mirroring real DOS, which sizes the
        // program block AFTER reserving the environment; PSP:0x02 tracks the
        // reduced top. For a .COM (PSP:0x02 = segment + 0x1000) there is already
        // ample room above the program, so no shrink happens.
        let limit = ARENA_TOP.saturating_sub(paras);
        if self.arena.prog_top > limit {
            self.arena.prog_top = limit;
            if self.arena.blocks.is_empty() {
                self.arena.free_base = limit;
            }
            mem.write_u16(psp_base + 0x02, limit)?;
        }
        let env_seg = self
            .arena
            .allocate(paras)
            .map_err(|_| DosError::EnvSegmentFull)?;
        let env_base = usize::from(env_seg) * 16;
        for (offset, &byte) in block.iter().enumerate() {
            mem.write_u8(env_base + offset, byte)?;
        }
        mem.write_u16(psp_base + 0x2c, env_seg)?;
        Ok(())
    }

    /// Resolve the ASCIIZ filename at ds:dx to a host path. Ok(Ok(path)) on
    /// success; Ok(Err(code)) when a DOS error code should be returned (no NUL ->
    /// 0x03, no drive -> 0x02, bad path -> 0x03); Err(DosError) for a guest-memory
    /// fault.
    fn resolve_open_path(
        &self,
        mem: &Memory,
        ds: u16,
        dx: u16,
    ) -> Result<Result<PathBuf, u16>, DosError> {
        let Some(name) = read_asciiz(mem, ds, dx)? else {
            return Ok(Err(0x03));
        };
        let Some(drive) = self.drive.as_ref() else {
            return Ok(Err(0x02));
        };
        Ok(drive.resolve_dos_path(&name).map_err(|_| 0x03))
    }

    /// Resolve DS:DX to a host path and read the program image into an owned
    /// Vec. Ok(Ok(image)) on success; Ok(Err(code)) when a DOS error code should
    /// be returned (no drive -> 0x02, bad path -> 0x03, missing file -> 0x02,
    /// host error -> 0x05); Err(DosError::Memory) for a guest-memory fault.
    fn read_program_image(
        &self,
        mem: &Memory,
        regs: &DosRegs,
    ) -> Result<Result<Vec<u8>, u16>, DosError> {
        let path = match self.resolve_open_path(mem, regs.ds, regs.dx)? {
            Ok(path) => path,
            Err(code) => return Ok(Err(code)),
        };
        let mut file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(err) => return Ok(Err(dos_io_error_code(&err))),
        };
        let mut image = Vec::new();
        if let Err(err) = file.read_to_end(&mut image) {
            return Ok(Err(dos_io_error_code(&err)));
        }
        Ok(Ok(image))
    }

    /// AH=4Bh AL=3: load an overlay into the caller-allocated segment named in
    /// the EPB at ES:BX (#01591: load segment at 0x00, relocation factor at
    /// 0x02). CF=0 on success; CF=1 + AX on error. A malformed MZ image maps to
    /// 0x0B (bad format), inherited from the loader's error variants (marked).
    fn exec_load_overlay(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let epb_base = usize::from(regs.es) * 16 + usize::from(regs.bx);
        let load_seg = mem.read_u16(epb_base)?;
        let reloc_factor = mem.read_u16(epb_base + 2)?;
        let image = match self.read_program_image(mem, regs)? {
            Ok(image) => image,
            Err(code) => {
                set_dos_error(regs, code);
                return Ok(DosAction::Continue);
            }
        };
        match load_overlay(&image, mem, load_seg, reloc_factor) {
            Ok(()) => {
                regs.cf = false;
                Ok(DosAction::Continue)
            }
            Err(DosError::Memory(e)) => Err(DosError::Memory(e)),
            Err(_) => {
                set_dos_error(regs, 0x0b);
                Ok(DosAction::Continue)
            }
        }
    }

    /// Build the child environment block. env_source 0 -> an empty environment
    /// (a single terminating NUL). Non-zero -> copy the source block's string
    /// region (ASCIIZ strings up to the terminating empty string), capped at
    /// 32 KiB; no terminator within the cap -> Err(0x0A). Only the string region
    /// is copied, not the optional count + program-name suffix (marked).
    fn child_environment(
        &self,
        mem: &Memory,
        env_source: u16,
    ) -> Result<Result<Vec<u8>, u16>, DosError> {
        if env_source == 0 {
            return Ok(Ok(vec![0x00]));
        }
        let base = usize::from(env_source) * 16;
        let mut out = Vec::new();
        let cap = 32_768usize;
        loop {
            // Read one ASCIIZ string; its source position is out.len() (the next
            // unread byte), which advances as bytes are pushed.
            let string_start = out.len();
            loop {
                if out.len() >= cap {
                    return Ok(Err(0x0a));
                }
                let b = mem.read_u8(base + out.len())?;
                out.push(b);
                if b == 0 {
                    break;
                }
            }
            if out.len() - string_start == 1 {
                break; // just the lone terminating NUL ends the block
            }
        }
        Ok(Ok(out))
    }

    /// Write the child command tail at PSP offset 0x80 from the EPB command-tail
    /// pointer (a length byte followed by chars). A null (0:0) pointer writes an
    /// empty tail (length 0, a 0x0D terminator).
    fn write_command_tail(
        &self,
        mem: &mut Memory,
        psp: usize,
        seg: u16,
        off: u16,
    ) -> Result<(), DosError> {
        let null = seg == 0 && off == 0;
        let count = if null {
            0u8
        } else {
            let base = usize::from(seg) * 16 + usize::from(off);
            mem.read_u8(base)?.min(127)
        };
        mem.write_u8(psp + 0x80, count)?;
        if !null {
            let base = usize::from(seg) * 16 + usize::from(off);
            for i in 0..usize::from(count) {
                mem.write_u8(psp + 0x81 + i, mem.read_u8(base + 1 + i)?)?;
            }
        }
        mem.write_u8(psp + 0x81 + usize::from(count), 0x0d)?;
        Ok(())
    }

    /// AH=4Bh AL=0: load and execute. Reads the name and EPB #01590 at ES:BX,
    /// builds the child environment and PSP, loads the image, saves the parent
    /// context, switches to the child context, and returns Exec. Errors set
    /// CF/AX and return Continue (no child runs).
    fn exec_load_and_execute(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let image = match self.read_program_image(mem, regs)? {
            Ok(image) => image,
            Err(code) => {
                set_dos_error(regs, code);
                return Ok(DosAction::Continue);
            }
        };
        // EPB #01590: env word (0x00), command-tail far ptr (0x02), FCB1 (0x06),
        // FCB2 (0x0A).
        let epb_base = usize::from(regs.es) * 16 + usize::from(regs.bx);
        let env_source = mem.read_u16(epb_base)?;
        let cmdtail_off = mem.read_u16(epb_base + 2)?;
        let cmdtail_seg = mem.read_u16(epb_base + 4)?;
        let fcb1_off = mem.read_u16(epb_base + 6)?;
        let fcb1_seg = mem.read_u16(epb_base + 8)?;
        let fcb2_off = mem.read_u16(epb_base + 0x0a)?;
        let fcb2_seg = mem.read_u16(epb_base + 0x0c)?;

        let env_bytes = match self.child_environment(mem, env_source)? {
            Ok(bytes) => bytes,
            Err(code) => {
                set_dos_error(regs, code);
                return Ok(DosAction::Continue);
            }
        };
        let env_paras = (env_bytes.len() as u16).div_ceil(16).max(1);
        let env_seg = self.arena.free_base;
        let child_psp = match env_seg.checked_add(env_paras) {
            Some(s) if s < ARENA_TOP => s,
            _ => {
                set_dos_error(regs, 0x0a);
                return Ok(DosAction::Continue);
            }
        };
        // The child needs at least a 64 KiB segment: load_com sets SP=0xFFFE and
        // writes the return word there. A child whose PSP lands too high would
        // overflow past ARENA_TOP, so reject it as insufficient memory (0x08)
        // before writing the env block. An MZ child's finer fit is enforced by
        // load_program below (ExeNotEnoughMemory -> 0x08).
        if u32::from(child_psp) + 0x1000 > u32::from(ARENA_TOP) {
            set_dos_error(regs, 0x08);
            return Ok(DosAction::Continue);
        }
        let env_linear = usize::from(env_seg) * 16;
        for (i, &byte) in env_bytes.iter().enumerate() {
            mem.write_u8(env_linear + i, byte)?;
        }

        let parent_psp = self.arena.psp_seg;
        let entry = match load_program(&image, mem, child_psp) {
            Ok(entry) => entry,
            Err(DosError::ExeNotEnoughMemory { .. }) => {
                set_dos_error(regs, 0x08);
                return Ok(DosAction::Continue);
            }
            Err(DosError::Memory(e)) => return Err(DosError::Memory(e)),
            Err(_) => {
                set_dos_error(regs, 0x0b);
                return Ok(DosAction::Continue);
            }
        };

        // Patch the child PSP.
        let psp = usize::from(child_psp) * 16;
        mem.write_u16(psp + 0x02, ARENA_TOP)?; // child owns to the top (DOS default)
        mem.write_u16(psp + 0x16, parent_psp)?; // parent PSP link
        mem.write_u16(psp + 0x2c, env_seg)?; // environment segment
        self.write_command_tail(mem, psp, cmdtail_seg, cmdtail_off)?;
        // Default JFT at 0x18: stdin/stdout/stderr open, the rest closed.
        for off in 0x18u16..0x2cu16 {
            mem.write_u8(psp + usize::from(off), 0)?;
        }
        mem.write_u8(psp + 0x18, 0x01)?;
        mem.write_u8(psp + 0x19, 0x01)?;
        mem.write_u8(psp + 0x1a, 0x01)?;
        let fcb1_drive = copy_fcb(mem, psp + 0x5c, fcb1_seg, fcb1_off)?;
        let fcb2_drive = copy_fcb(mem, psp + 0x6c, fcb2_seg, fcb2_off)?;
        let child_ax = (u16::from(fcb_drive_validity(fcb2_drive)) << 8)
            | u16::from(fcb_drive_validity(fcb1_drive));

        // Save the parent context, then switch to the child.
        let parent = ProgramContext {
            arena: std::mem::take(&mut self.arena),
            dta: self.dta,
            find_searches: std::mem::take(&mut self.find_searches),
        };
        self.program_stack.push(parent);
        self.arena = Arena {
            psp_seg: child_psp,
            prog_top: ARENA_TOP,
            free_base: ARENA_TOP,
            blocks: Vec::new(),
        };
        self.dta = (child_psp, 0x80);
        // A fresh child has terminated no child of its own.
        self.last_exit_code = 0;
        self.last_exit_type = 0;

        Ok(DosAction::Exec { entry, child_ax })
    }

    /// Restore the parent program's DOS state after a child exits with `code`,
    /// and record the exit code/type for AH=4Dh. Called by the machine when it
    /// pops a parent frame.
    pub fn finish_exec(&mut self, code: u8) {
        if let Some(parent) = self.program_stack.pop() {
            self.arena = parent.arena;
            self.dta = parent.dta;
            self.find_searches = parent.find_searches;
        }
        self.last_exit_code = code;
        self.last_exit_type = 0x00; // only normal termination is modeled (marked).
    }

    /// Split a FindFirst filespec into (host directory, final-component pattern).
    /// Ok((dir, pattern)) on success; Err(code) is a DOS error code (0x02 no drive,
    /// 0x03 bad/non-C/traversal path). The pattern is the last path component (may
    /// hold wildcards); the directory defaults to the C: root when no path is given
    /// (no per-drive current directory is tracked, marked). The filespec is already
    /// read from guest memory, so this touches no memory and returns no DosError.
    fn split_find_spec(&self, filespec: &str) -> Result<(PathBuf, String), u16> {
        let drive = self.drive.as_ref().ok_or(0x02u16)?;
        let spec = filespec.trim();
        let after_drive =
            if let Some(rest) = spec.strip_prefix("C:").or_else(|| spec.strip_prefix("c:")) {
                rest
            } else if spec.as_bytes().get(1) == Some(&b':') {
                return Err(0x03); // a drive letter other than C: (we mount only C:)
            } else {
                spec
            };
        let normalized = after_drive.replace('/', "\\");
        let (dir_part, pattern) = match normalized.rfind('\\') {
            Some(i) => (normalized[..i].to_string(), normalized[i + 1..].to_string()),
            None => (String::new(), normalized.clone()),
        };
        let mut dir = drive.root().to_path_buf();
        for component in dir_part.split('\\').filter(|c| !c.is_empty() && *c != ".") {
            if component == ".." {
                return Err(0x03);
            }
            dir.push(component);
        }
        Ok((dir, pattern))
    }

    /// Service a software interrupt the DOS kernel handles. `vector` is the INT
    /// number (0x20 terminate, 0x21 the AH-dispatched set). Reads and writes
    /// `regs`, reads/writes guest memory through `mem`. DOS services are emulated
    /// host-side (HLE). Unimplemented INT 21h functions return Continue with no
    /// effect, so the caller's IRET stub returns cleanly.
    ///
    /// `mem` is `&mut` because the file-read call (AH=3Fh) writes the data it
    /// reads back into guest memory at DS:DX; most other calls only read it.
    pub fn dispatch(
        &mut self,
        vector: u8,
        regs: &mut DosRegs,
        mem: &mut Memory,
    ) -> Result<DosAction, DosError> {
        match vector {
            0x20 => Ok(DosAction::Exit(0)),
            0x21 => self.dispatch_int21(regs, mem),
            // The machine only records 0x10/0x20/0x21 and routes 0x10 elsewhere, so
            // this is unreachable today. Treat it as a no-op rather than panic.
            _ => Ok(DosAction::Continue),
        }
    }

    fn dispatch_int21(
        &mut self,
        regs: &mut DosRegs,
        mem: &mut Memory,
    ) -> Result<DosAction, DosError> {
        let ah = (regs.ax >> 8) as u8;
        match ah {
            // AH=01h: read one character with echo. A real keyboard blocks; with a
            // preloaded buffer an empty buffer yields the redirected-input EOF byte ^Z.
            0x01 => {
                let ch = self.stdin.pop_front().unwrap_or(0x1a);
                self.stdout.push(ch);
                regs.ax = (regs.ax & 0xff00) | u16::from(ch);
                Ok(DosAction::Continue)
            }
            // AH=02h: write the byte in DL to standard output. AL returns it (DOS 2+).
            0x02 => {
                let ch = regs.dx as u8;
                self.stdout.push(ch);
                regs.ax = (regs.ax & 0xff00) | u16::from(ch);
                Ok(DosAction::Continue)
            }
            // AH=06h: direct console I/O. DL=0xFF reads without waiting (ZF reports
            // whether a character was ready); any other DL writes DL.
            0x06 => {
                if regs.dx as u8 == 0xff {
                    match self.stdin.pop_front() {
                        Some(ch) => {
                            regs.ax = (regs.ax & 0xff00) | u16::from(ch);
                            regs.zf = false;
                        }
                        None => regs.zf = true,
                    }
                } else {
                    // Output form: ZF is undefined for AH=06h output, so leave regs.zf
                    // at the guest value rather than clobbering it.
                    let ch = regs.dx as u8;
                    self.stdout.push(ch);
                    regs.ax = (regs.ax & 0xff00) | u16::from(ch);
                }
                Ok(DosAction::Continue)
            }
            // AH=08h: read one character without echo. ^Z on an empty buffer, as AH=01h.
            0x08 => {
                let ch = self.stdin.pop_front().unwrap_or(0x1a);
                regs.ax = (regs.ax & 0xff00) | u16::from(ch);
                Ok(DosAction::Continue)
            }
            // AH=09h: print the '$'-terminated string at DS:DX to standard output.
            0x09 => {
                let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                let mut offset = 0usize;
                loop {
                    let byte = mem.read_u8(base + offset)?;
                    if byte == b'$' {
                        break;
                    }
                    self.stdout.push(byte);
                    offset += 1;
                }
                // DOS returns AL = '$' (0x24) from AH=09h.
                regs.ax = (regs.ax & 0xff00) | 0x24;
                Ok(DosAction::Continue)
            }
            // AH=3Dh: open an existing file at DS:DX (ASCIIZ). AL's low 3 bits are
            // the access mode (0=read, 1=write, 2=read/write), honored and enforced
            // per handle. CF=0 + AX=handle on success, CF=1 + AX=DOS code on error.
            0x3d => {
                let mode = AccessMode::from_open_al(regs.ax as u8);
                let path = match self.resolve_open_path(mem, regs.ds, regs.dx)? {
                    Ok(path) => path,
                    Err(code) => {
                        set_dos_error(regs, code);
                        return Ok(DosAction::Continue);
                    }
                };
                match open_host_file(&path, mode) {
                    Ok(file) => {
                        let handle = (5u16..)
                            .find(|h| !self.open_files.contains_key(h))
                            .expect("a free DOS handle exists at or below u16::MAX");
                        self.open_files.insert(handle, OpenFile { file, mode });
                        regs.ax = handle;
                        regs.cf = false;
                    }
                    Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                }
                Ok(DosAction::Continue)
            }
            0x3f => {
                let handle = regs.bx;
                let count = usize::from(regs.cx);
                let Some(of) = self.open_files.get_mut(&handle) else {
                    set_dos_error(regs, 0x06);
                    return Ok(DosAction::Continue);
                };
                if !of.mode.can_read() {
                    set_dos_error(regs, 0x05);
                    return Ok(DosAction::Continue);
                }
                let mut buffer = vec![0u8; count];
                let mut filled = 0usize;
                while filled < count {
                    match of.file.read(&mut buffer[filled..]) {
                        Ok(0) => break,
                        Ok(n) => filled += n,
                        Err(err) => {
                            set_dos_error(regs, dos_io_error_code(&err));
                            return Ok(DosAction::Continue);
                        }
                    }
                }
                let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                for (index, &byte) in buffer[..filled].iter().enumerate() {
                    mem.write_u8(base + index, byte)?;
                }
                regs.ax = filled as u16;
                regs.cf = false;
                Ok(DosAction::Continue)
            }
            // AH=3Eh: close the handle in BX. Dropping the File closes it (RAII).
            // CF=0 if the handle was open, CF=1 + AX=0x06 if it was not.
            0x3e => {
                if self.open_files.remove(&regs.bx).is_some() {
                    regs.cf = false;
                } else {
                    set_dos_error(regs, 0x06);
                }
                Ok(DosAction::Continue)
            }
            // AH=30h: get Toka-DOS version. AL=major, AH=minor, BH=OEM, BL:CX=serial (0).
            0x30 => {
                regs.ax =
                    u16::from(TOKA_DOS_VERSION_MAJOR) | (u16::from(TOKA_DOS_VERSION_MINOR) << 8);
                regs.bx = u16::from(TOKA_DOS_OEM) << 8;
                regs.cx = 0;
                Ok(DosAction::Continue)
            }
            // AH=19h: get current default drive. Only C: is mounted, so AL=2 (0=A).
            0x19 => {
                regs.ax = (regs.ax & 0xff00) | 0x02;
                Ok(DosAction::Continue)
            }
            // AH=25h: set interrupt vector AL to DS:DX. Writes the real guest IVT
            // (offset then segment, little-endian) at AL*4. Re-vectoring an
            // HLE'd INT (0x10/0x20/0x21) writes the IVT but host dispatch still
            // intercepts those by vector number.
            0x25 => {
                let addr = usize::from(regs.ax as u8) * 4;
                mem.write_u16(addr, regs.dx)?;
                mem.write_u16(addr + 2, regs.ds)?;
                Ok(DosAction::Continue)
            }
            // AH=35h: get interrupt vector AL into ES:BX.
            0x35 => {
                let addr = usize::from(regs.ax as u8) * 4;
                regs.bx = mem.read_u16(addr)?;
                regs.es = mem.read_u16(addr + 2)?;
                Ok(DosAction::Continue)
            }
            // AH=2Ah: get date. CX=year, DH=month, DL=day, AL=day-of-week (0=Sun).
            0x2a => {
                regs.cx = self.clock.year;
                regs.dx = (u16::from(self.clock.month) << 8) | u16::from(self.clock.day);
                regs.ax = (regs.ax & 0xff00) | u16::from(self.clock.day_of_week);
                Ok(DosAction::Continue)
            }
            // AH=2Bh: set date. CX=year(1980-2099), DH=month, DL=day. AL=0 ok, 0xFF
            // invalid. No calendar routine, so the day range is the coarse
            // 1..=31 (real DOS rejects e.g. Feb 31) and day_of_week is not recomputed;
            // no in-scope reader needs per-month validation or the post-set weekday.
            0x2b => {
                let year = regs.cx;
                let month = (regs.dx >> 8) as u8;
                let day = regs.dx as u8;
                if (1980..=2099).contains(&year)
                    && (1..=12).contains(&month)
                    && (1..=31).contains(&day)
                {
                    self.clock.year = year;
                    self.clock.month = month;
                    self.clock.day = day;
                    regs.ax &= 0xff00;
                } else {
                    regs.ax = (regs.ax & 0xff00) | 0xff;
                }
                Ok(DosAction::Continue)
            }
            // AH=2Ch: get time. CH=hour, CL=minute, DH=second, DL=hundredths.
            0x2c => {
                regs.cx = (u16::from(self.clock.hour) << 8) | u16::from(self.clock.minute);
                regs.dx = (u16::from(self.clock.second) << 8) | u16::from(self.clock.hundredths);
                Ok(DosAction::Continue)
            }
            // AH=2Dh: set time. CH=hour, CL=minute, DH=second, DL=hundredths. AL=0 ok,
            // 0xFF invalid.
            0x2d => {
                let hour = (regs.cx >> 8) as u8;
                let minute = regs.cx as u8;
                let second = (regs.dx >> 8) as u8;
                let hundredths = regs.dx as u8;
                if hour < 24 && minute < 60 && second < 60 && hundredths < 100 {
                    self.clock.hour = hour;
                    self.clock.minute = minute;
                    self.clock.second = second;
                    self.clock.hundredths = hundredths;
                    regs.ax &= 0xff00;
                } else {
                    regs.ax = (regs.ax & 0xff00) | 0xff;
                }
                Ok(DosAction::Continue)
            }
            // AH=48h: allocate BX paragraphs. CF=0 AX=segment, or CF=1 AX=0x08
            // BX=largest-available.
            0x48 => {
                match self.arena.allocate(regs.bx) {
                    Ok(seg) => {
                        regs.ax = seg;
                        regs.cf = false;
                    }
                    Err(largest) => {
                        regs.cf = true;
                        regs.ax = 0x08;
                        regs.bx = largest;
                    }
                }
                Ok(DosAction::Continue)
            }
            // AH=49h: free the block in ES. CF=0, or CF=1 AX=0x09 (invalid block).
            0x49 => {
                match self.arena.free(regs.es) {
                    Ok(()) => regs.cf = false,
                    Err(()) => {
                        regs.cf = true;
                        regs.ax = 0x09;
                    }
                }
                Ok(DosAction::Continue)
            }
            // AH=4Ah: resize the block in ES to BX paragraphs. CF=0, or CF=1 with
            // AX=0x08 BX=largest-available (too big) / AX=0x09 (invalid block).
            0x4a => {
                match self.arena.resize(regs.es, regs.bx) {
                    Ok(()) => regs.cf = false,
                    Err(ResizeError::TooBig(largest)) => {
                        regs.cf = true;
                        regs.ax = 0x08;
                        regs.bx = largest;
                    }
                    Err(ResizeError::InvalidBlock) => {
                        regs.cf = true;
                        regs.ax = 0x09;
                    }
                }
                Ok(DosAction::Continue)
            }
            // AH=1Ah: set the Disk Transfer Area to DS:DX.
            0x1a => {
                self.dta = (regs.ds, regs.dx);
                Ok(DosAction::Continue)
            }
            // AH=2Fh: get the Disk Transfer Area into ES:BX. Default is PSP:0x80.
            0x2f => {
                regs.es = self.dta.0;
                regs.bx = self.dta.1;
                Ok(DosAction::Continue)
            }
            // AH=4Ch: terminate with the return code in AL.
            0x4c => Ok(DosAction::Exit((regs.ax & 0x00ff) as u8)),
            // AH=33h: Ctrl-Break flag. Stub: AL=00 (get) returns DL=0 (off);
            // AL=01 (set) is accepted as a no-op. No INT 23h state is tracked yet.
            0x33 => {
                if regs.ax as u8 == 0x00 {
                    regs.dx &= 0xff00; // DL = 0 (off)
                }
                Ok(DosAction::Continue)
            }
            // AH=0Eh: select default drive. Stub: only C: exists, so report
            // AL=1 logical drive and do not change the current drive.
            0x0e => {
                regs.ax = (regs.ax & 0xff00) | 0x01;
                Ok(DosAction::Continue)
            }
            // AH=3Ch: create or truncate a file at DS:DX (ASCIIZ). CX = attributes
            // (ignored; the host has no DOS attribute bits, marked). Opens read/write,
            // truncating an existing file to zero. CF=0 + AX=handle, or CF=1 + AX=code.
            0x3c => {
                let path = match self.resolve_open_path(mem, regs.ds, regs.dx)? {
                    Ok(path) => path,
                    Err(code) => {
                        set_dos_error(regs, code);
                        return Ok(DosAction::Continue);
                    }
                };
                match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&path)
                {
                    Ok(file) => {
                        let handle = (5u16..)
                            .find(|h| !self.open_files.contains_key(h))
                            .expect("a free DOS handle exists at or below u16::MAX");
                        self.open_files.insert(
                            handle,
                            OpenFile {
                                file,
                                mode: AccessMode::ReadWrite,
                            },
                        );
                        regs.ax = handle;
                        regs.cf = false;
                    }
                    Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                }
                Ok(DosAction::Continue)
            }
            // AH=40h: write CX bytes from DS:DX to the handle in BX. BX=1/2 route to
            // stdout/stderr (the output buffer). For a file handle, CX=0 truncates the
            // file at the current position. CF=0 + AX=bytes-written, or CF=1 + AX=code.
            0x40 => {
                let handle = regs.bx;
                let count = usize::from(regs.cx);
                let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                // Predefined console handles: 1=stdout, 2=stderr -> the output buffer.
                if handle == 1 || handle == 2 {
                    for index in 0..count {
                        let byte = mem.read_u8(base + index)?;
                        self.stdout.push(byte);
                    }
                    regs.ax = regs.cx;
                    regs.cf = false;
                    return Ok(DosAction::Continue);
                }
                let Some(of) = self.open_files.get_mut(&handle) else {
                    set_dos_error(regs, 0x06);
                    return Ok(DosAction::Continue);
                };
                if !of.mode.can_write() {
                    set_dos_error(regs, 0x05);
                    return Ok(DosAction::Continue);
                }
                if count == 0 {
                    // CX=0 truncates (or extends) the file to the current position.
                    let pos = match of.file.stream_position() {
                        Ok(pos) => pos,
                        Err(err) => {
                            set_dos_error(regs, dos_io_error_code(&err));
                            return Ok(DosAction::Continue);
                        }
                    };
                    if let Err(err) = of.file.set_len(pos) {
                        set_dos_error(regs, dos_io_error_code(&err));
                        return Ok(DosAction::Continue);
                    }
                    regs.ax = 0;
                    regs.cf = false;
                    return Ok(DosAction::Continue);
                }
                let mut buffer = vec![0u8; count];
                for (index, slot) in buffer.iter_mut().enumerate() {
                    *slot = mem.read_u8(base + index)?;
                }
                match of.file.write_all(&buffer) {
                    Ok(()) => {
                        regs.ax = regs.cx;
                        regs.cf = false;
                    }
                    Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                }
                Ok(DosAction::Continue)
            }
            // AH=42h: seek the handle in BX. AL=whence (0=start, 1=current signed,
            // 2=end signed), CX:DX = 32-bit offset (CX high). CF=0 + DX:AX = new
            // absolute position; AL>2 -> CF=1 + AX=0x01 (invalid function).
            0x42 => {
                let handle = regs.bx;
                let Some(of) = self.open_files.get_mut(&handle) else {
                    set_dos_error(regs, 0x06);
                    return Ok(DosAction::Continue);
                };
                let offset = (u32::from(regs.cx) << 16) | u32::from(regs.dx);
                let seek = match regs.ax as u8 {
                    0 => SeekFrom::Start(u64::from(offset)),
                    1 => SeekFrom::Current(i64::from(offset as i32)),
                    2 => SeekFrom::End(i64::from(offset as i32)),
                    _ => {
                        set_dos_error(regs, 0x01);
                        return Ok(DosAction::Continue);
                    }
                };
                match of.file.seek(seek) {
                    Ok(pos) => {
                        let pos = pos as u32;
                        regs.ax = pos as u16;
                        regs.dx = (pos >> 16) as u16;
                        regs.cf = false;
                    }
                    Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                }
                Ok(DosAction::Continue)
            }
            // AH=4Eh: find first matching file. CX = attribute mask, DS:DX = ASCIIZ
            // filespec (path + 8.3 wildcards). On success the 43-byte FindFirst data
            // block is written to the current DTA and CF=0; on failure CF=1 with
            // AX = 0x02 (no drive), 0x03 (bad path), 0x05 (host error), or 0x12 (no
            // matching file).
            0x4e => {
                let Some(filespec) = read_asciiz(mem, regs.ds, regs.dx)? else {
                    set_dos_error(regs, 0x03);
                    return Ok(DosAction::Continue);
                };
                let (dir, pattern) = match self.split_find_spec(&filespec) {
                    Ok(split) => split,
                    Err(code) => {
                        set_dos_error(regs, code);
                        return Ok(DosAction::Continue);
                    }
                };
                let mask = regs.cx as u8;
                let pattern_template = pattern_to_8_3(&pattern);
                let read_dir = match std::fs::read_dir(&dir) {
                    Ok(read_dir) => read_dir,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        set_dos_error(regs, 0x03);
                        return Ok(DosAction::Continue);
                    }
                    Err(_) => {
                        set_dos_error(regs, 0x05);
                        return Ok(DosAction::Continue);
                    }
                };
                let mut entries = Vec::new();
                for dirent in read_dir.flatten() {
                    let raw = dirent.file_name();
                    let Some(name) = raw.to_str() else {
                        continue; // a non-UTF-8 host name cannot be an 8.3 DOS name
                    };
                    let Some(file_template) = host_name_to_8_3(name) else {
                        continue;
                    };
                    if !template_matches(&file_template, &pattern_template) {
                        continue;
                    }
                    let Ok(metadata) = dirent.metadata() else {
                        continue;
                    };
                    let attr = if metadata.is_dir() { 0x10 } else { 0x00 };
                    if !attr_matches(attr, mask) {
                        continue;
                    }
                    let (time, date) =
                        dos_time_date(metadata.modified().unwrap_or(std::time::UNIX_EPOCH));
                    entries.push(FindEntry {
                        attr,
                        time,
                        date,
                        // The DTA size field is a 32-bit dword, so a host
                        // file over 4 GiB truncates; DOS cannot represent more.
                        size: metadata.len() as u32,
                        name: name.to_ascii_uppercase(),
                    });
                }
                let Some(first) = entries.first().cloned() else {
                    set_dos_error(regs, 0x12);
                    return Ok(DosAction::Continue);
                };
                write_find_record(mem, self.dta, &first)?;
                // An abandoned search (FindFirst, take the first hit, never
                // run to 0x12) leaves its snapshot here until init_program clears the
                // map; bounded per program run, so no eviction policy.
                self.find_searches
                    .insert(self.dta, FindSearch { entries, next: 1 });
                regs.cf = false;
                Ok(DosAction::Continue)
            }
            // AH=4Fh: find next matching file. The active search is keyed by the
            // current DTA address. CF=0 with the next entry written to the DTA, or
            // CF=1 AX=0x12 (no more files) when the search is exhausted or there is
            // no active search at this DTA.
            0x4f => {
                let dta = self.dta;
                let Some(search) = self.find_searches.get_mut(&dta) else {
                    set_dos_error(regs, 0x12);
                    return Ok(DosAction::Continue);
                };
                let Some(entry) = search.entries.get(search.next).cloned() else {
                    self.find_searches.remove(&dta);
                    set_dos_error(regs, 0x12);
                    return Ok(DosAction::Continue);
                };
                search.next += 1;
                write_find_record(mem, dta, &entry)?;
                regs.cf = false;
                Ok(DosAction::Continue)
            }
            // AH=4Bh: EXEC. AL=0 (load and execute) and AL=3 (load overlay) are
            // handled; AL=1 (load without execute) and AL=4 (background) are not
            // implemented and return 0x01 (invalid function), marked.
            0x4b => match regs.ax as u8 {
                0x00 => self.exec_load_and_execute(mem, regs),
                0x03 => self.exec_load_overlay(mem, regs),
                _ => {
                    set_dos_error(regs, 0x01);
                    Ok(DosAction::Continue)
                }
            },
            // AH=4Dh: get the return code of the last child. AL=code, AH=type
            // (always 0x00 normal; Ctrl-C/critical/TSR are not modeled, marked).
            // CF is always clear; the stored code is cleared after the read
            // (one-shot, per RBIL).
            0x4d => {
                regs.ax = (u16::from(self.last_exit_type) << 8) | u16::from(self.last_exit_code);
                self.last_exit_code = 0;
                self.last_exit_type = 0;
                regs.cf = false;
                Ok(DosAction::Continue)
            }
            // Other file functions (find) and everything else are not yet
            // implemented; later slices fill them in. An unimplemented function
            // returns Continue so the IRET stub returns to the caller.
            _ => Ok(DosAction::Continue),
        }
    }
}

/// Toka-DOS (Toka Disk Operating System), the Izarra 3000's MS-DOS 6.1 clone,
/// is what this HLE kernel emulates. INT 21h AH=30h reports its version.
const TOKA_DOS_VERSION_MAJOR: u8 = 6;
const TOKA_DOS_VERSION_MINOR: u8 = 10; // 6.10, the .NN-hundredths convention (6.20 -> 20)
const TOKA_DOS_OEM: u8 = 0xff;

/// The largest .COM image: a 64 KiB segment minus the 256-byte PSP.
const COM_MAX_LEN: usize = 0x10000 - 0x100;

/// Conventional memory is modeled as one block ending at the 640 KiB video
/// aperture (paragraph 0xA000). Single block, no MCB chain / EBDA /
/// UMB; an .EXE that walks or resizes the memory arena is out of scope.
const CONVENTIONAL_TOP_PARAGRAPH: u32 = 0xa000;

/// Where to start executing a loaded program. A .COM sets all six to its single
/// load segment; an .EXE sets a distinct CS:IP and SS:SP with DS=ES=PSP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramEntry {
    pub cs: u16,
    pub ip: u16,
    pub ss: u16,
    pub sp: u16,
    pub ds: u16,
    pub es: u16,
}

/// Build the 256-byte PSP at psp_seg:0. INT 20h (CD 20) at offset 0 so a near
/// RET to PSP:0 terminates; the top-of-memory paragraph at 0x02; an empty
/// command tail at 0x80. The environment segment (0x2C) is filled in by
/// `DosKernel::install_environment`; the parent PSP, default FCBs, and the DTA
/// are left zero (later slices).
fn build_psp(mem: &mut Memory, psp_seg: u16, top_of_mem_paragraph: u16) -> Result<(), DosError> {
    let base = usize::from(psp_seg) * 16;
    mem.write_u8(base, 0xcd)?;
    mem.write_u8(base + 1, 0x20)?;
    mem.write_u16(base + 2, top_of_mem_paragraph)?;
    mem.write_u8(base + 0x80, 0x00)?;
    mem.write_u8(base + 0x81, 0x0d)?;
    Ok(())
}

/// Format a DOS environment block: a sequence of ASCIIZ `KEY=VALUE` strings
/// followed by an extra NUL (the empty string that terminates the list). Keys
/// are stored verbatim, so callers pass uppercase DOS-style keys. With no
/// entries the block is a single NUL, a valid empty environment. Real DOS then
/// appends a `0x0001` word and an ASCIIZ argv0 (the program path); that trailer
/// follows the terminator and is invisible to env scanners, so it is omitted
/// here (the loader does not track the guest program path today).
fn build_env_block(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut block = Vec::new();
    for (key, value) in entries {
        block.extend_from_slice(key.as_bytes());
        block.push(b'=');
        block.extend_from_slice(value.as_bytes());
        block.push(0);
    }
    block.push(0); // the terminating empty string
    block
}

/// Load a .COM image into `mem` at `segment` and build its PSP. Returns the entry
/// state for the caller to apply to the CPU.
pub fn load_com(image: &[u8], mem: &mut Memory, segment: u16) -> Result<ProgramEntry, DosError> {
    if image.len() > COM_MAX_LEN {
        return Err(DosError::ComTooLarge(image.len()));
    }
    build_psp(mem, segment, segment.wrapping_add(0x1000))?;
    let base = usize::from(segment) * 16;
    // Program image at offset 0x100.
    for (index, &byte) in image.iter().enumerate() {
        mem.write_u8(base + 0x100 + index, byte)?;
    }
    // .COM stack: SP=0xFFFE with a 0x0000 return word, so a bare RET lands at
    // PSP:0 and hits the INT 20h. Written after the image, so a maximum-size
    // image has its last two bytes overwritten, which is what real DOS does.
    mem.write_u16(base + 0xfffe, 0x0000)?;
    Ok(ProgramEntry {
        cs: segment,
        ip: 0x0100,
        ss: segment,
        sp: 0xfffe,
        ds: segment,
        es: segment,
    })
}

/// A parsed MZ/.EXE image: the entry fields and the load-module slice plus the
/// raw relocation table. Validates the signature, the page/last-page sizes, and
/// that the module and the relocation table fit the image; rejects the same
/// malformations load_exe checked inline.
struct ParsedExe<'a> {
    e_ss: u16,
    e_sp: u16,
    e_ip: u16,
    e_cs: u16,
    e_minalloc: u16,
    e_maxalloc: u16,
    module: &'a [u8],
    relocs: &'a [u8],
}

fn parse_exe(image: &[u8]) -> Result<ParsedExe<'_>, DosError> {
    if image.len() < 0x1c {
        return Err(DosError::ExeImageTruncated(
            "header shorter than 0x1C bytes",
        ));
    }
    let word = |off: usize| u16::from_le_bytes([image[off], image[off + 1]]);
    if word(0x00) != 0x5a4d {
        return Err(DosError::BadExeSignature);
    }
    let e_cblp = word(0x02);
    let e_cp = word(0x04);
    let e_crlc = word(0x06);
    let e_cparhdr = word(0x08);
    let e_minalloc = word(0x0a);
    let e_maxalloc = word(0x0c);
    let e_ss = word(0x0e);
    let e_sp = word(0x10);
    let e_ip = word(0x14);
    let e_cs = word(0x16);
    let e_lfarlc = word(0x18);

    if e_cp == 0 {
        return Err(DosError::ExeImageTruncated("page count e_cp is zero"));
    }
    // e_cblp is bytes used on the last page, legal range 0..=512 (0 and 512 both
    // mean a full page). A larger value is a malformed header that would
    // underflow the last-page computation below, so reject it up front.
    if e_cblp > 512 {
        return Err(DosError::ExeImageTruncated(
            "bytes-on-last-page e_cblp exceeds 512",
        ));
    }
    let header_bytes = usize::from(e_cparhdr) * 16;
    let last_page = if e_cblp != 0 {
        512 - usize::from(e_cblp)
    } else {
        0
    };
    let image_size = usize::from(e_cp) * 512 - last_page;
    if header_bytes > image_size || image_size > image.len() {
        return Err(DosError::ExeImageTruncated(
            "load module extends past the file",
        ));
    }
    let module = &image[header_bytes..image_size];
    let reloc_end = usize::from(e_lfarlc) + usize::from(e_crlc) * 4;
    if reloc_end > image.len() {
        return Err(DosError::ExeImageTruncated(
            "relocation table extends past the file",
        ));
    }
    let relocs = &image[usize::from(e_lfarlc)..reloc_end];
    Ok(ParsedExe {
        e_ss,
        e_sp,
        e_ip,
        e_cs,
        e_minalloc,
        e_maxalloc,
        module,
        relocs,
    })
}

/// Walk `relocs` (4-byte little-endian (offset, segment) entries) and apply each
/// to the module loaded at linear `base`: read the word at `base + seg*16 + off`,
/// add `addend`, write it back. `addend` is the load segment for an EXE and the
/// caller's relocation factor for an overlay. Out-of-range relocations are
/// rejected rather than applied blindly as real DOS would, to avoid corrupting
/// arbitrary memory (marked).
fn apply_relocs(
    mem: &mut Memory,
    base: usize,
    module_len: usize,
    relocs: &[u8],
    addend: u16,
) -> Result<(), DosError> {
    for entry in relocs.chunks_exact(4) {
        let off = u16::from_le_bytes([entry[0], entry[1]]);
        let seg = u16::from_le_bytes([entry[2], entry[3]]);
        let module_offset = usize::from(seg) * 16 + usize::from(off);
        if module_offset + 2 > module_len {
            return Err(DosError::ExeRelocationOutOfRange);
        }
        let target = base + module_offset;
        let value = mem.read_u16(target)?;
        mem.write_u16(target, value.wrapping_add(addend))?;
    }
    Ok(())
}

/// Load a DOS MZ/.EXE into `mem`: parse the 0x1C-byte header, copy the load
/// module to start_seg:0 (start_seg = psp_segment + 0x10), apply each relocation
/// (add start_seg to the flagged word), build the PSP, and return the entry
/// state (CS:IP and SS:SP from the header, DS=ES=PSP). Conventional memory is
/// one block ending at paragraph 0xA000; e_minalloc is enforced and e_maxalloc
/// clamps the PSP top-of-memory word.
pub fn load_exe(
    image: &[u8],
    mem: &mut Memory,
    psp_segment: u16,
) -> Result<ProgramEntry, DosError> {
    let exe = parse_exe(image)?;
    let module_len = exe.module.len();
    let module_paras = module_len.div_ceil(16) as u32;

    let start_seg = psp_segment.wrapping_add(0x10);
    let needed = u32::from(start_seg) + module_paras + u32::from(exe.e_minalloc);
    if needed > CONVENTIONAL_TOP_PARAGRAPH {
        return Err(DosError::ExeNotEnoughMemory {
            needed,
            available: CONVENTIONAL_TOP_PARAGRAPH,
        });
    }
    // Top of the program's block: honor e_maxalloc, clamp to conventional memory.
    let top_paragraph = (u32::from(start_seg) + module_paras + u32::from(exe.e_maxalloc))
        .min(CONVENTIONAL_TOP_PARAGRAPH) as u16;
    build_psp(mem, psp_segment, top_paragraph)?;

    // Copy the load module to start_seg:0.
    let base = usize::from(start_seg) * 16;
    for (index, &byte) in exe.module.iter().enumerate() {
        mem.write_u8(base + index, byte)?;
    }
    apply_relocs(mem, base, module_len, exe.relocs, start_seg)?;

    Ok(ProgramEntry {
        cs: start_seg.wrapping_add(exe.e_cs),
        ip: exe.e_ip,
        ss: start_seg.wrapping_add(exe.e_ss),
        sp: exe.e_sp,
        ds: psp_segment,
        es: psp_segment,
    })
}

/// Load a .COM or .EXE by signature and build its PSP. Real DOS detects the
/// format by the "MZ" word at the start of the image, not the file extension.
/// `psp_segment` is where the PSP goes; a .COM loads its code at psp_segment:0x100,
/// an .EXE loads its module at (psp_segment + 0x10):0.
pub fn load_program(
    image: &[u8],
    mem: &mut Memory,
    psp_segment: u16,
) -> Result<ProgramEntry, DosError> {
    // Detect "MZ" only; the rare "ZM" alternate signature is treated
    // as a .COM (no real DOS game ships "ZM").
    if image.len() >= 2 && image[0] == b'M' && image[1] == b'Z' {
        load_exe(image, mem, psp_segment)
    } else {
        load_com(image, mem, psp_segment)
    }
}

/// AH=4Bh AL=3: load an overlay at the caller-allocated `load_seg`, applying
/// `reloc_factor` to an MZ image's relocations (0 for a raw .COM-format file).
/// No PSP, no environment, no execution: the caller owns the memory and decides
/// how to call the overlay.
pub fn load_overlay(
    image: &[u8],
    mem: &mut Memory,
    load_seg: u16,
    reloc_factor: u16,
) -> Result<(), DosError> {
    let base = usize::from(load_seg) * 16;
    if image.len() >= 2 && image[0] == b'M' && image[1] == b'Z' {
        let exe = parse_exe(image)?;
        for (i, &byte) in exe.module.iter().enumerate() {
            mem.write_u8(base + i, byte)?;
        }
        apply_relocs(mem, base, exe.module.len(), exe.relocs, reloc_factor)?;
    } else {
        for (i, &byte) in image.iter().enumerate() {
            mem.write_u8(base + i, byte)?;
        }
    }
    Ok(())
}

/// Set the DOS error return convention: CF=1 and AX = the DOS error code.
fn set_dos_error(regs: &mut DosRegs, code: u16) {
    regs.cf = true;
    regs.ax = code;
}

/// Map a host file io error to a DOS error code. NotFound is 0x02 (file not
/// found); everything else, including permission failures, maps to 0x05 (access
/// denied), the closest in-scope code. The host filesystem does not separate
/// file-not-found from path-not-found, so 0x03 is reserved for path resolution.
fn dos_io_error_code(err: &std::io::Error) -> u16 {
    match err.kind() {
        std::io::ErrorKind::NotFound => 0x02,
        _ => 0x05,
    }
}

/// Whether a byte is legal in a DOS 8.3 filename component: letters, digits, and a
/// fixed punctuation set. Space, '.', and the DOS-reserved characters are not
/// legal. Extended bytes (>= 0x80) fall through to false; the caller separately
/// skips non-ASCII host names (marked).
fn is_8_3_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'()-@^_`{}~".contains(&byte)
}

/// Convert a host modified-time to a packed DOS (time, date) pair. The result is
/// UTC, not local time (marked); a timestamp before 1980-01-01 clamps to it, and a
/// year past 2107 (the 7-bit DOS year ceiling) is not representable and clamps.
/// Uses Howard Hinnant's days-to-civil algorithm so no date dependency is added.
fn dos_time_date(modified: std::time::SystemTime) -> (u16, u16) {
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let seconds_of_day = (secs % 86_400) as u32;
    let (mut year, month, day) = civil_from_days(days);
    if year < 1980 {
        return (0, (1 << 5) | 1); // 1980-01-01 00:00:00
    }
    if year > 2107 {
        year = 2107;
    }
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    let date = (((year - 1980) as u16) << 9) | ((month as u16) << 5) | day as u16;
    let time = ((hour as u16) << 11) | ((minute as u16) << 5) | ((second / 2) as u16);
    (time, date)
}

/// Howard Hinnant's civil-from-days: days since the Unix epoch (1970-01-01) to a
/// proleptic Gregorian (year, month [1..12], day [1..31]).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Build the blank-padded 11-byte 8.3 template for a host file name, uppercased.
/// None if the name does not fit 8.3 (empty or dotted base such as a leading-dot
/// ".cfg", base > 8, ext > 3, or non-ASCII): such host files are invisible to DOS
/// find. No NAME~1 long-name mangling; the corpus is 8.3-named.
fn host_name_to_8_3(name: &str) -> Option<[u8; 11]> {
    if !name.is_ascii() {
        return None;
    }
    let (base, ext) = match name.rsplit_once('.') {
        Some((base, ext)) => (base, ext),
        None => (name, ""),
    };
    if base.is_empty()
        || base.len() > 8
        || ext.len() > 3
        || !base.bytes().all(is_8_3_char)
        || !ext.bytes().all(is_8_3_char)
    {
        return None;
    }
    let mut template = [b' '; 11];
    for (i, byte) in base.bytes().enumerate() {
        template[i] = byte.to_ascii_uppercase();
    }
    for (i, byte) in ext.bytes().enumerate() {
        template[8 + i] = byte.to_ascii_uppercase();
    }
    Some(template)
}

/// Build the 11-byte search template from a DOS wildcard pattern. '*' fills the
/// rest of its field with '?'; other characters are uppercased; short fields pad
/// with blanks. The COMMAND.COM habit of rewriting a bare name to
/// "name.*" is NOT applied here (we are the kernel, not the shell), so "*" matches
/// only extensionless files while "*.*" matches every name.
fn pattern_to_8_3(pattern: &str) -> [u8; 11] {
    let (base, ext) = match pattern.split_once('.') {
        Some((base, ext)) => (base, ext),
        None => (pattern, ""),
    };
    let mut template = [b' '; 11];
    fill_field(&mut template[..8], base);
    fill_field(&mut template[8..], ext);
    template
}

/// Copy a pattern field into a blank slice: '*' fills the remainder with '?',
/// other characters are uppercased and copied until the field or pattern ends.
fn fill_field(field: &mut [u8], pattern: &str) {
    for (i, byte) in pattern.bytes().enumerate() {
        if i >= field.len() {
            break;
        }
        if byte == b'*' {
            for slot in &mut field[i..] {
                *slot = b'?';
            }
            return;
        }
        field[i] = byte.to_ascii_uppercase();
    }
}

/// Match a file's 8.3 template against a pattern template: at each of the 11
/// positions a '?' in the pattern matches any byte (including the blank pad, so
/// "LEVEL?.DAT" matches both "LEVEL1.DAT" and "LEVEL.DAT"); any other pattern byte
/// must equal the file byte.
fn template_matches(file: &[u8; 11], pattern: &[u8; 11]) -> bool {
    file.iter()
        .zip(pattern.iter())
        .all(|(&f, &p)| p == b'?' || p == f)
}

/// RBIL: for masks other than the volume-label bit, a file matches if it has at
/// most the masked special attributes. Host files carry only "normal" (0x00) or
/// "directory" (0x10): a normal file always matches; a directory matches only when
/// the mask includes the directory bit. Read-only (bit 0) and archive (bit 5) do
/// not restrict and are ignored, per the spec.
fn attr_matches(file_attr: u8, mask: u8) -> bool {
    const SPECIAL: u8 = 0x02 | 0x04 | 0x10; // hidden | system | directory
    file_attr & !mask & SPECIAL == 0
}

/// Write a 43-byte FindFirst data block at the DTA `(segment, offset)`. The
/// DOS-internal area 0x00..0x15 is zeroed (the search cursor is kept kernel-side,
/// so nothing readable lives there); the documented fields follow. A guest-memory
/// fault propagates as DosError::Memory.
fn write_find_record(mem: &mut Memory, dta: (u16, u16), entry: &FindEntry) -> Result<(), DosError> {
    let base = usize::from(dta.0) * 16 + usize::from(dta.1);
    for offset in 0..0x15 {
        mem.write_u8(base + offset, 0)?;
    }
    mem.write_u8(base + 0x15, entry.attr)?;
    mem.write_u16(base + 0x16, entry.time)?;
    mem.write_u16(base + 0x18, entry.date)?;
    mem.write_u32(base + 0x1a, entry.size)?;
    let name = entry.name.as_bytes();
    for i in 0..13 {
        mem.write_u8(base + 0x1e + i, name.get(i).copied().unwrap_or(0))?;
    }
    Ok(())
}

/// Copy 16 bytes for an FCB from (seg:off) into the child PSP at `dst`, or zero
/// it for a null (0:0) pointer. Returns the FCB's drive byte (0 for a null FCB).
fn copy_fcb(mem: &mut Memory, dst: usize, seg: u16, off: u16) -> Result<u8, DosError> {
    if seg == 0 && off == 0 {
        for i in 0..16 {
            mem.write_u8(dst + i, 0)?;
        }
        Ok(0)
    } else {
        let src = usize::from(seg) * 16 + usize::from(off);
        let mut drive = 0u8;
        for i in 0..16 {
            let b = mem.read_u8(src + i)?;
            if i == 0 {
                drive = b;
            }
            mem.write_u8(dst + i, b)?;
        }
        Ok(drive)
    }
}

/// RBIL EXEC note: AL/AH = 00h if the corresponding FCB has a valid drive letter,
/// FFh if not. Drive 0 (default) and 1..=26 are valid.
fn fcb_drive_validity(drive: u8) -> u8 {
    if drive == 0 || (1..=26).contains(&drive) {
        0
    } else {
        0xff
    }
}

/// Read an ASCIIZ string from guest memory at seg:off, scanning for a NUL with a
/// 128-byte cap (a DOS path is well under this). Returns None if no terminator
/// appears within the cap (a malformed caller). A memory read fault propagates as
/// DosError::Memory.
fn read_asciiz(mem: &Memory, seg: u16, off: u16) -> Result<Option<String>, DosError> {
    const MAX: usize = 128;
    let base = usize::from(seg) * 16 + usize::from(off);
    let mut bytes = Vec::new();
    for index in 0..MAX {
        let byte = mem.read_u8(base + index)?;
        if byte == 0 {
            return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
        }
        bytes.push(byte);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn mounts_existing_directory_as_c_drive() {
        let directory = tempfile::tempdir().unwrap();
        let drive = HostDrive::mount_c(directory.path()).unwrap();

        assert_eq!(drive.letter(), 'C');
        assert_eq!(drive.root(), directory.path());
    }

    #[test]
    fn resolves_dos_paths_under_host_root() {
        let directory = tempfile::tempdir().unwrap();
        let drive = HostDrive::mount_c(directory.path()).unwrap();

        assert_eq!(
            drive.resolve_dos_path(r"C:\GAMES\WOLF3D").unwrap(),
            directory.path().join("GAMES").join("WOLF3D")
        );
    }

    #[test]
    fn rejects_path_traversal() {
        let directory = tempfile::tempdir().unwrap();
        let drive = HostDrive::mount_c(directory.path()).unwrap();

        assert!(matches!(
            drive.resolve_dos_path(r"C:\..\WINDOWS"),
            Err(DosError::PathTraversal(_))
        ));
    }

    fn mem_with(at: usize, bytes: &[u8]) -> Memory {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        for (index, &byte) in bytes.iter().enumerate() {
            mem.write_u8(at + index, byte).unwrap();
        }
        mem
    }

    #[test]
    fn ah09_prints_string_up_to_terminator() {
        // DS:DX = 0x0100:0x0010 -> linear 0x1010.
        let mut mem = mem_with(0x1010, b"Hello$");
        let mut regs = DosRegs {
            ax: 0x0900,
            ds: 0x0100,
            dx: 0x0010,
            ..DosRegs::default()
        };
        let mut kernel = DosKernel::new();
        kernel.set_stdin(b"");
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::Continue);
        assert_eq!(kernel.stdout(), b"Hello");
        assert_eq!(regs.ax & 0x00ff, 0x24); // AH=09h returns AL = '$'
        assert_eq!(regs.ax >> 8, 0x09); // AH is preserved
    }

    #[test]
    fn ah09_without_terminator_propagates_a_memory_error() {
        // A string with no '$' runs off the end of memory; the out-of-bounds read
        // surfaces as a DosError rather than looping forever.
        let mut mem = Memory::new(4096).unwrap();
        for offset in 0..4096 {
            mem.write_u8(offset, b'A').unwrap();
        }
        let mut regs = DosRegs {
            ax: 0x0900,
            ds: 0x0000,
            dx: 0x0000,
            ..DosRegs::default()
        };
        let mut kernel = DosKernel::new();
        kernel.set_stdin(b"");
        assert!(matches!(
            kernel.dispatch(0x21, &mut regs, &mut mem),
            Err(DosError::Memory(_))
        ));
    }

    #[test]
    fn ah09_empty_string_writes_nothing() {
        let mut mem = mem_with(0x1010, b"$");
        let mut regs = DosRegs {
            ax: 0x0900,
            ds: 0x0100,
            dx: 0x0010,
            ..DosRegs::default()
        };
        let mut kernel = DosKernel::new();
        kernel.set_stdin(b"");
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(kernel.stdout().is_empty());
    }

    #[test]
    fn ah4c_exits_with_al_code() {
        let mut mem = Memory::new(4096).unwrap();
        let mut regs = DosRegs {
            ax: 0x4c07,
            ..DosRegs::default()
        };
        let mut kernel = DosKernel::new();
        kernel.set_stdin(b"");
        assert_eq!(
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap(),
            DosAction::Exit(7)
        );
    }

    #[test]
    fn int20_exits_with_zero() {
        let mut mem = Memory::new(4096).unwrap();
        let mut regs = DosRegs::default();
        let mut kernel = DosKernel::new();
        kernel.set_stdin(b"");
        assert_eq!(
            kernel.dispatch(0x20, &mut regs, &mut mem).unwrap(),
            DosAction::Exit(0)
        );
    }

    #[test]
    fn unimplemented_int21_function_is_noop() {
        let mut mem = Memory::new(4096).unwrap();
        let mut regs = DosRegs {
            ax: 0x4400, // AH=44h IOCTL, not implemented
            ..DosRegs::default()
        };
        let mut kernel = DosKernel::new();
        kernel.set_stdin(b"");
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::Continue);
        assert!(kernel.stdout().is_empty());
    }

    #[test]
    fn ah30_reports_toka_dos_version() {
        let mut mem = Memory::new(4096).unwrap();
        let mut regs = DosRegs {
            ax: 0x3000,
            ..DosRegs::default()
        };
        let mut kernel = DosKernel::new();
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.ax & 0x00ff, 6); // AL = major
        assert_eq!(regs.ax >> 8, 10); // AH = minor (6.10)
        assert_eq!(regs.bx >> 8, 0xff); // BH = OEM
    }

    #[test]
    fn ah19_reports_c_drive() {
        let mut mem = Memory::new(4096).unwrap();
        let mut regs = DosRegs {
            ax: 0x1900,
            ..DosRegs::default()
        };
        let mut kernel = DosKernel::new();
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.ax & 0x00ff, 0x02); // AL = 2 (C:)
    }

    #[test]
    fn load_com_builds_psp_and_entry() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let image = [0xb8, 0x00, 0x4c, 0xcd, 0x21]; // mov ax,4c00; int 21
        let entry = load_com(&image, &mut mem, 0x0100).unwrap();
        assert_eq!(
            entry,
            ProgramEntry {
                cs: 0x0100,
                ip: 0x0100,
                ss: 0x0100,
                sp: 0xfffe,
                ds: 0x0100,
                es: 0x0100,
            }
        );
        let base = 0x0100usize * 16;
        assert_eq!(mem.read_u8(base).unwrap(), 0xcd); // INT 20h opcode at PSP:0
        assert_eq!(mem.read_u8(base + 1).unwrap(), 0x20);
        assert_eq!(mem.read_u16(base + 0x02).unwrap(), 0x1100); // top-of-memory paragraph
        assert_eq!(mem.read_u8(base + 0x80).unwrap(), 0x00); // empty command tail length
        assert_eq!(mem.read_u8(base + 0x81).unwrap(), 0x0d);
        assert_eq!(mem.read_u8(base + 0x100).unwrap(), 0xb8); // image lands at 0x100
        assert_eq!(mem.read_u8(base + 0x104).unwrap(), 0x21);
        assert_eq!(mem.read_u16(base + 0xfffe).unwrap(), 0x0000); // .COM return word
    }

    #[test]
    fn load_com_rejects_oversize_image() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let len = 0x10000 - 0x100 + 1; // one byte over the .COM limit
        let image = vec![0x90; len];
        match load_com(&image, &mut mem, 0x0100) {
            Err(DosError::ComTooLarge(reported)) => assert_eq!(reported, len),
            other => panic!("expected ComTooLarge, got {other:?}"),
        }
    }

    fn char_io(ax: u16, dx: u16, input: &[u8]) -> (DosRegs, Vec<u8>) {
        let mut mem = Memory::new(4096).unwrap();
        let mut kernel = DosKernel::new();
        kernel.set_stdin(input);
        let mut regs = DosRegs {
            ax,
            dx,
            ..DosRegs::default()
        };
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::Continue);
        (regs, kernel.stdout().to_vec()) // DosRegs is Copy and holds the post-dispatch AX/ZF
    }

    #[test]
    fn ah02_writes_dl_to_stdout() {
        let (regs, out) = char_io(0x0200, 0x0041, b""); // AH=02h, DL='A'
        assert_eq!(out, b"A");
        assert_eq!(regs.ax & 0x00ff, 0x41); // AL = written byte
    }

    #[test]
    fn ah01_reads_with_echo() {
        let (regs, out) = char_io(0x0100, 0, b"A"); // AH=01h
        assert_eq!(regs.ax & 0x00ff, 0x41); // AL = 'A'
        assert_eq!(out, b"A"); // echoed
    }

    #[test]
    fn ah08_reads_without_echo() {
        let (regs, out) = char_io(0x0800, 0, b"A"); // AH=08h
        assert_eq!(regs.ax & 0x00ff, 0x41);
        assert!(out.is_empty()); // no echo
    }

    #[test]
    fn ah01_on_empty_returns_eof_ctrl_z() {
        let (regs, _out) = char_io(0x0100, 0, b""); // empty buffer
        assert_eq!(regs.ax & 0x00ff, 0x1a); // ^Z
    }

    #[test]
    fn ah08_on_empty_returns_eof_ctrl_z() {
        let (regs, _out) = char_io(0x0800, 0, b""); // empty buffer, no echo
        assert_eq!(regs.ax & 0x00ff, 0x1a); // ^Z
    }

    #[test]
    fn ah06_output_writes_dl() {
        let (regs, out) = char_io(0x0600, 0x0042, b""); // AH=06h, DL='B' (not 0xFF)
        assert_eq!(out, b"B");
        assert_eq!(regs.ax & 0x00ff, 0x42);
    }

    #[test]
    fn ah06_input_available_clears_zf() {
        // ZF starts set so the assertion proves the available path clears it.
        let mut mem = Memory::new(4096).unwrap();
        let mut kernel = DosKernel::new();
        kernel.set_stdin(b"X");
        let mut regs = DosRegs {
            ax: 0x0600,
            dx: 0x00ff,
            zf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.ax & 0x00ff, 0x58); // AL = 'X'
        assert!(!regs.zf); // cleared because a character was available
        assert!(kernel.stdout().is_empty()); // no echo
    }

    #[test]
    fn ah06_input_empty_sets_zf() {
        let (regs, out) = char_io(0x0600, 0x00ff, b""); // AH=06h, DL=0xFF, empty
        assert!(regs.zf); // no character ready
        assert!(out.is_empty());
    }

    /// Build (kernel mounted on a temp C:, memory with `name` ASCIIZ at DS:DX
    /// = 0x0100:0x0200, the tempdir kept alive). Write `files` into the drive
    /// first as (name, contents).
    fn kernel_with_drive(
        files: &[(&str, &[u8])],
        name: &str,
    ) -> (DosKernel, Memory, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        for (file_name, contents) in files {
            let mut f = std::fs::File::create(dir.path().join(file_name)).unwrap();
            f.write_all(contents).unwrap();
        }
        let mut kernel = DosKernel::new();
        kernel.mount_c(HostDrive::mount_c(dir.path()).unwrap());
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let base = 0x0100usize * 16 + 0x0200;
        for (index, byte) in name.bytes().enumerate() {
            mem.write_u8(base + index, byte).unwrap();
        }
        mem.write_u8(base + name.len(), 0).unwrap(); // NUL terminator
        (kernel, mem, dir)
    }

    fn open(kernel: &mut DosKernel, mem: &mut Memory) -> DosRegs {
        let mut regs = DosRegs {
            ax: 0x3d00, // AH=3Dh, AL=00 read
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, mem).unwrap();
        regs
    }

    #[test]
    fn open_existing_file_returns_handle_5() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("DATA.TXT", b"hi")], r"C:\DATA.TXT");
        let regs = open(&mut kernel, &mut mem);
        assert!(!regs.cf);
        assert_eq!(regs.ax, 5);
    }

    #[test]
    fn open_missing_file_sets_cf_and_ax02() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[], r"C:\NOPE.TXT");
        let regs = open(&mut kernel, &mut mem);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x02);
    }

    #[test]
    fn open_with_no_drive_mounted_sets_ax02() {
        let mut kernel = DosKernel::new(); // no drive
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let base = 0x0100usize * 16 + 0x0200;
        for (index, byte) in r"C:\DATA.TXT".bytes().enumerate() {
            mem.write_u8(base + index, byte).unwrap();
        }
        mem.write_u8(base + r"C:\DATA.TXT".len(), 0).unwrap();
        let regs = open(&mut kernel, &mut mem);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x02);
    }

    #[test]
    fn open_bad_drive_letter_sets_ax03() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[], r"D:\DATA.TXT");
        let regs = open(&mut kernel, &mut mem);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x03); // resolve_dos_path UnsupportedDrive -> path not found
    }

    #[test]
    fn open_unterminated_name_sets_ax03() {
        // A filename with no NUL within the 128-byte cap is a malformed caller:
        // read_asciiz returns None and the arm reports path not found (0x03).
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = DosKernel::new();
        kernel.mount_c(HostDrive::mount_c(dir.path()).unwrap());
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let base = 0x0100usize * 16 + 0x0200;
        for index in 0..200usize {
            mem.write_u8(base + index, b'A').unwrap(); // no NUL within the 128-byte cap
        }
        let regs = open(&mut kernel, &mut mem);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x03);
    }

    /// Open DATA.TXT and return the handle (asserts the open succeeded).
    fn open_data(kernel: &mut DosKernel, mem: &mut Memory) -> u16 {
        let regs = open(kernel, mem);
        assert!(!regs.cf, "open failed: ax={:#06x}", regs.ax);
        regs.ax
    }

    fn read(
        kernel: &mut DosKernel,
        mem: &mut Memory,
        handle: u16,
        count: u16,
        dst: u16,
    ) -> DosRegs {
        let mut regs = DosRegs {
            ax: 0x3f00,
            bx: handle,
            cx: count,
            ds: 0x0100,
            dx: dst,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, mem).unwrap();
        regs
    }

    #[test]
    fn reads_file_bytes_into_guest_memory() {
        let (mut kernel, mut mem, _dir) =
            kernel_with_drive(&[("DATA.TXT", b"hello")], r"C:\DATA.TXT");
        let handle = open_data(&mut kernel, &mut mem);
        let regs = read(&mut kernel, &mut mem, handle, 16, 0x0400);
        assert!(!regs.cf);
        assert_eq!(regs.ax, 5); // 5 bytes read
        let base = 0x0100usize * 16 + 0x0400;
        let got: Vec<u8> = (0..5).map(|i| mem.read_u8(base + i).unwrap()).collect();
        assert_eq!(got, b"hello");
    }

    #[test]
    fn reads_in_chunks_then_eof() {
        let (mut kernel, mut mem, _dir) =
            kernel_with_drive(&[("DATA.TXT", b"abcdef")], r"C:\DATA.TXT");
        let handle = open_data(&mut kernel, &mut mem);
        let first = read(&mut kernel, &mut mem, handle, 4, 0x0400);
        assert_eq!(first.ax, 4);
        let second = read(&mut kernel, &mut mem, handle, 4, 0x0410);
        assert_eq!(second.ax, 2); // only 2 left
        let third = read(&mut kernel, &mut mem, handle, 4, 0x0420);
        assert_eq!(third.ax, 0); // EOF
        assert!(!third.cf);
        let base = 0x0100usize * 16 + 0x0400;
        let chunk0: Vec<u8> = (0..4).map(|i| mem.read_u8(base + i).unwrap()).collect();
        assert_eq!(chunk0, b"abcd");
        let base1 = 0x0100usize * 16 + 0x0410;
        let chunk1: Vec<u8> = (0..2).map(|i| mem.read_u8(base1 + i).unwrap()).collect();
        assert_eq!(chunk1, b"ef");
    }

    #[test]
    fn read_invalid_handle_sets_ax06() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("DATA.TXT", b"hi")], r"C:\DATA.TXT");
        let regs = read(&mut kernel, &mut mem, 99, 16, 0x0400);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x06);
    }

    fn close(kernel: &mut DosKernel, mem: &mut Memory, handle: u16) -> DosRegs {
        let mut regs = DosRegs {
            ax: 0x3e00,
            bx: handle,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, mem).unwrap();
        regs
    }

    #[test]
    fn closes_an_open_handle() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("DATA.TXT", b"hi")], r"C:\DATA.TXT");
        let handle = open_data(&mut kernel, &mut mem);
        let regs = close(&mut kernel, &mut mem, handle);
        assert!(!regs.cf);
        assert_eq!(regs.ax, 0x3e00); // AX untouched on success, so AH is preserved
        // Reading the closed handle now fails as invalid.
        let after = read(&mut kernel, &mut mem, handle, 4, 0x0400);
        assert!(after.cf);
        assert_eq!(after.ax, 0x06);
        // Closing the same handle again fails as invalid (no idempotent success).
        let again = close(&mut kernel, &mut mem, handle);
        assert!(again.cf);
        assert_eq!(again.ax, 0x06);
    }

    #[test]
    fn close_invalid_handle_sets_ax06() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("DATA.TXT", b"hi")], r"C:\DATA.TXT");
        let regs = close(&mut kernel, &mut mem, 99);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x06);
    }

    #[test]
    fn handles_start_at_5_and_reuse_lowest_free() {
        // One file opened twice: each open is an independent File and handle.
        let (mut kernel, mut mem, _dir) =
            kernel_with_drive(&[("DATA.TXT", b"one")], r"C:\DATA.TXT");
        let h1 = open_data(&mut kernel, &mut mem);
        let h2 = open_data(&mut kernel, &mut mem);
        assert_eq!(h1, 5);
        assert_eq!(h2, 6);
        close(&mut kernel, &mut mem, h1);
        let h3 = open_data(&mut kernel, &mut mem);
        assert_eq!(h3, 5); // lowest free handle reused
    }

    /// Build a minimal MZ image: a 32-byte (2-paragraph) header so the relocation
    /// table at 0x1C fits, then the load module. e_cp/e_cblp are chosen so
    /// image_size == header + module == the returned file length.
    #[allow(clippy::too_many_arguments)]
    fn build_mz(
        module: &[u8],
        relocs: &[(u16, u16)],
        e_cs: u16,
        e_ip: u16,
        e_ss: u16,
        e_sp: u16,
        e_minalloc: u16,
        e_maxalloc: u16,
    ) -> Vec<u8> {
        let e_cparhdr: u16 = 2;
        let header_bytes = usize::from(e_cparhdr) * 16;
        assert!(
            0x1c + relocs.len() * 4 <= header_bytes,
            "relocs overflow header"
        );
        let total = header_bytes + module.len();
        let e_cp = total.div_ceil(512) as u16;
        let e_cblp = (total % 512) as u16;
        let e_lfarlc: u16 = 0x1c;
        let mut img = vec![0u8; total];
        img[0..2].copy_from_slice(b"MZ");
        img[2..4].copy_from_slice(&e_cblp.to_le_bytes());
        img[4..6].copy_from_slice(&e_cp.to_le_bytes());
        img[6..8].copy_from_slice(&(relocs.len() as u16).to_le_bytes());
        img[8..10].copy_from_slice(&e_cparhdr.to_le_bytes());
        img[10..12].copy_from_slice(&e_minalloc.to_le_bytes());
        img[12..14].copy_from_slice(&e_maxalloc.to_le_bytes());
        img[14..16].copy_from_slice(&e_ss.to_le_bytes());
        img[16..18].copy_from_slice(&e_sp.to_le_bytes());
        img[20..22].copy_from_slice(&e_ip.to_le_bytes());
        img[22..24].copy_from_slice(&e_cs.to_le_bytes());
        img[24..26].copy_from_slice(&e_lfarlc.to_le_bytes());
        for (i, (off, seg)) in relocs.iter().enumerate() {
            let p = 0x1c + i * 4;
            img[p..p + 2].copy_from_slice(&off.to_le_bytes());
            img[p + 2..p + 4].copy_from_slice(&seg.to_le_bytes());
        }
        img[header_bytes..].copy_from_slice(module);
        img
    }

    #[test]
    fn load_exe_parses_entry_and_places_module() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let module = [0xaa, 0xbb, 0xcc, 0xdd];
        let image = build_mz(&module, &[], 0x0002, 0x0010, 0x0001, 0x0200, 0x10, 0xffff);
        let psp = 0x0100u16;
        let start_seg = psp + 0x10;
        let entry = load_exe(&image, &mut mem, psp).unwrap();
        assert_eq!(entry.cs, start_seg + 0x0002);
        assert_eq!(entry.ip, 0x0010);
        assert_eq!(entry.ss, start_seg + 0x0001);
        assert_eq!(entry.sp, 0x0200);
        assert_eq!(entry.ds, psp);
        assert_eq!(entry.es, psp);
        let base = usize::from(start_seg) * 16;
        assert_eq!(mem.read_u8(base).unwrap(), 0xaa);
        assert_eq!(mem.read_u8(base + 3).unwrap(), 0xdd);
    }

    #[test]
    fn load_exe_applies_relocation() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        // 8-byte module; the word at offset 4 holds 0x0000 (the link-time segment).
        let module = [0u8; 8];
        let image = build_mz(&module, &[(4u16, 0u16)], 0, 0, 0, 0x100, 0x10, 0xffff);
        let psp = 0x0100u16;
        let start_seg = psp + 0x10;
        load_exe(&image, &mut mem, psp).unwrap();
        let target = usize::from(start_seg) * 16 + 4;
        // The relocation added start_seg to the original 0x0000.
        assert_eq!(mem.read_u16(target).unwrap(), start_seg);
    }

    #[test]
    fn load_exe_builds_psp() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let module = [0x90u8; 16];
        let image = build_mz(&module, &[], 0, 0, 0, 0x100, 0x10, 0x20);
        let psp = 0x0100u16;
        load_exe(&image, &mut mem, psp).unwrap();
        let base = usize::from(psp) * 16;
        assert_eq!(mem.read_u8(base).unwrap(), 0xcd);
        assert_eq!(mem.read_u8(base + 1).unwrap(), 0x20);
        // top = min(0xA000, start_seg(0x110) + module_paras(1) + maxalloc(0x20)) = 0x131
        assert_eq!(mem.read_u16(base + 2).unwrap(), 0x0131);
        assert_eq!(mem.read_u8(base + 0x80).unwrap(), 0x00);
        assert_eq!(mem.read_u8(base + 0x81).unwrap(), 0x0d);
    }

    #[test]
    fn load_exe_rejects_bad_signature() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut image = build_mz(&[0u8; 16], &[], 0, 0, 0, 0x100, 0x10, 0xffff);
        image[0] = b'X';
        assert!(matches!(
            load_exe(&image, &mut mem, 0x100),
            Err(DosError::BadExeSignature)
        ));
    }

    #[test]
    fn load_exe_rejects_truncated_header() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let image = [0x4d, 0x5a, 0x00];
        assert!(matches!(
            load_exe(&image, &mut mem, 0x100),
            Err(DosError::ExeImageTruncated(_))
        ));
    }

    #[test]
    fn load_exe_rejects_truncated_module() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut image = build_mz(&[0u8; 16], &[], 0, 0, 0, 0x100, 0x10, 0xffff);
        // Inflate e_cp and clear e_cblp so image_size far exceeds the file length.
        image[4..6].copy_from_slice(&9u16.to_le_bytes());
        image[2..4].copy_from_slice(&0u16.to_le_bytes());
        assert!(matches!(
            load_exe(&image, &mut mem, 0x100),
            Err(DosError::ExeImageTruncated(_))
        ));
    }

    #[test]
    fn load_exe_rejects_out_of_bounds_relocation() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        // module is 8 bytes; the reloc points at module offset 100 (seg=0, off=100).
        let image = build_mz(&[0u8; 8], &[(100u16, 0u16)], 0, 0, 0, 0x100, 0x10, 0xffff);
        assert!(matches!(
            load_exe(&image, &mut mem, 0x100),
            Err(DosError::ExeRelocationOutOfRange)
        ));
    }

    #[test]
    fn load_exe_rejects_insufficient_memory() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        // e_minalloc overruns paragraph 0xA000.
        let image = build_mz(&[0u8; 16], &[], 0, 0, 0, 0x100, 0xffff, 0xffff);
        assert!(matches!(
            load_exe(&image, &mut mem, 0x100),
            Err(DosError::ExeNotEnoughMemory { .. })
        ));
    }

    #[test]
    fn load_exe_rejects_oversized_e_cblp() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut image = build_mz(&[0u8; 16], &[], 0, 0, 0, 0x100, 0x10, 0xffff);
        // e_cblp > 512 is a malformed header; it must be rejected, not underflow
        // the last-page computation and panic in debug builds.
        image[2..4].copy_from_slice(&0x0201u16.to_le_bytes());
        assert!(matches!(
            load_exe(&image, &mut mem, 0x100),
            Err(DosError::ExeImageTruncated(_))
        ));
    }

    #[test]
    fn load_program_routes_exe_by_signature() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let image = build_mz(
            &[0u8; 16],
            &[],
            0x0002,
            0x0010,
            0x0001,
            0x0200,
            0x10,
            0xffff,
        );
        let entry = load_program(&image, &mut mem, 0x0100).unwrap();
        // EXE: CS and DS are distinct segments; DS is the PSP.
        assert_ne!(entry.cs, entry.ds);
        assert_eq!(entry.ds, 0x0100);
    }

    #[test]
    fn ah2a_2c_read_the_default_clock() {
        let mut mem = Memory::new(4096).unwrap();
        let mut kernel = DosKernel::new();
        let mut date = DosRegs {
            ax: 0x2a00,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut date, &mut mem).unwrap();
        assert_eq!(date.cx, 1997); // year
        assert_eq!(date.dx >> 8, 6); // month
        assert_eq!(date.dx & 0xff, 17); // day
        assert_eq!(date.ax & 0xff, 2); // day_of_week (Tuesday)
        let mut time = DosRegs {
            ax: 0x2c00,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut time, &mut mem).unwrap();
        assert_eq!(time.cx >> 8, 12); // hour
        assert_eq!(time.cx & 0xff, 0); // minute
    }

    #[test]
    fn ah2b_2d_set_then_get_and_reject_out_of_range() {
        let mut mem = Memory::new(4096).unwrap();
        let mut kernel = DosKernel::new();
        // Set date 2001-02-03.
        let mut set = DosRegs {
            ax: 0x2b00,
            cx: 2001,
            dx: (2u16 << 8) | 3,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set, &mut mem).unwrap();
        assert_eq!(set.ax & 0xff, 0x00); // success
        let mut get = DosRegs {
            ax: 0x2a00,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get, &mut mem).unwrap();
        assert_eq!(get.cx, 2001);
        assert_eq!(get.dx >> 8, 2);
        assert_eq!(get.dx & 0xff, 3);
        // Reject month 13.
        let mut bad = DosRegs {
            ax: 0x2b00,
            cx: 2001,
            dx: (13u16 << 8) | 3,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut bad, &mut mem).unwrap();
        assert_eq!(bad.ax & 0xff, 0xff); // failure, clock unchanged
        let mut get2 = DosRegs {
            ax: 0x2a00,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get2, &mut mem).unwrap();
        assert_eq!(get2.dx >> 8, 2); // still February
    }

    #[test]
    fn ah25_then_ah35_round_trip_vector() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        // AH=25h: set INT 0x1C to DS:DX = 0xBEEF:0x1234.
        let mut set = DosRegs {
            ax: 0x251c,
            ds: 0xbeef,
            dx: 0x1234,
            ..DosRegs::default()
        };
        let mut kernel = DosKernel::new();
        kernel.dispatch(0x21, &mut set, &mut mem).unwrap();
        // The IVT entry at 0x1C*4 holds offset then segment, little-endian.
        assert_eq!(mem.read_u16(0x1c * 4).unwrap(), 0x1234);
        assert_eq!(mem.read_u16(0x1c * 4 + 2).unwrap(), 0xbeef);
        // AH=35h: get INT 0x1C back into ES:BX.
        let mut get = DosRegs {
            ax: 0x351c,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get, &mut mem).unwrap();
        assert_eq!(get.es, 0xbeef);
        assert_eq!(get.bx, 0x1234);
    }

    #[test]
    fn load_program_routes_com_when_no_mz() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let image = [0xb8, 0x00, 0x4c, 0xcd, 0x21]; // mov ax,4c00; int 21 (no MZ)
        let entry = load_program(&image, &mut mem, 0x0100).unwrap();
        // COM: all six entry fields equal the one load segment.
        assert_eq!(entry.cs, 0x0100);
        assert_eq!(entry.ds, 0x0100);
        assert_eq!(entry.ss, 0x0100);
        assert_eq!(entry.es, 0x0100);
        assert_eq!(entry.ip, 0x0100);
        assert_eq!(entry.sp, 0xfffe);
    }

    fn arena_kernel() -> DosKernel {
        let mut kernel = DosKernel::new();
        // Program PSP at 0x0100, block top at 0x1100 (a .COM-style 64 KiB block).
        kernel.init_program(0x0100, 0x1100);
        kernel
    }

    #[test]
    fn ah48_allocates_above_the_program_block() {
        let mut mem = Memory::new(4096).unwrap();
        let mut kernel = arena_kernel();
        let mut regs = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_eq!(regs.ax, 0x1100); // first free paragraph = prog_top
    }

    #[test]
    fn ah4a_shrink_program_block_then_ah48_allocates_the_tail() {
        let mut mem = Memory::new(4096).unwrap();
        let mut kernel = arena_kernel();
        // Shrink the program block (ES = PSP) to 0x0800 paragraphs.
        let mut resize = DosRegs {
            ax: 0x4a00,
            es: 0x0100,
            bx: 0x0800,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut resize, &mut mem).unwrap();
        assert!(!resize.cf);
        // Now AH=48h allocates from the freed tail: new free_base = 0x0100 + 0x0800 = 0x0900.
        let mut alloc = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut alloc, &mut mem).unwrap();
        assert!(!alloc.cf);
        assert_eq!(alloc.ax, 0x0900);
    }

    #[test]
    fn ah48_past_the_ceiling_returns_largest_available() {
        let mut mem = Memory::new(4096).unwrap();
        let mut kernel = arena_kernel();
        // Request more than fits: free_base=0x1100, ceiling 0xA000 -> available 0x8F00.
        let mut regs = DosRegs {
            ax: 0x4800,
            bx: 0x9000,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x08);
        assert_eq!(regs.bx, 0x8f00); // 0xA000 - 0x1100
    }

    #[test]
    fn ah4a_grow_program_block_too_big_fails_with_largest() {
        let mut mem = Memory::new(4096).unwrap();
        let mut kernel = arena_kernel();
        // No allocations yet, so limit is ARENA_TOP. Ask for more than fits.
        let mut regs = DosRegs {
            ax: 0x4a00,
            es: 0x0100,
            bx: 0xa000,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x08);
        assert_eq!(regs.bx, 0x9f00); // 0xA000 - 0x0100
    }

    #[test]
    fn ah49_frees_top_block_lifo_then_reuses() {
        let mut mem = Memory::new(4096).unwrap();
        let mut kernel = arena_kernel();
        let mut a = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut a, &mut mem).unwrap();
        let seg = a.ax; // 0x1100
        let mut free = DosRegs {
            ax: 0x4900,
            es: seg,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut free, &mut mem).unwrap();
        assert!(!free.cf);
        // The next allocation reuses the reclaimed paragraph.
        let mut b = DosRegs {
            ax: 0x4800,
            bx: 0x0008,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut b, &mut mem).unwrap();
        assert_eq!(b.ax, seg);
    }

    #[test]
    fn ah49_unknown_block_returns_ax09() {
        let mut mem = Memory::new(4096).unwrap();
        let mut kernel = arena_kernel();
        let mut regs = DosRegs {
            ax: 0x4900,
            es: 0x5555,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x09);
    }

    #[test]
    fn ah49_non_top_free_leaves_a_hole_without_underflow() {
        // Free a lower (non-top) block, then the top block: free_base must not
        // underflow, and the lower hole stays leaked (the documented LIFO ceiling).
        let mut mem = Memory::new(4096).unwrap();
        let mut kernel = arena_kernel(); // psp 0x100, prog_top 0x1100
        let mut a = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut a, &mut mem).unwrap();
        assert_eq!(a.ax, 0x1100);
        let mut b = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut b, &mut mem).unwrap();
        assert_eq!(b.ax, 0x1110);
        // Free the lower block A (non-top): the hole is not reclaimed.
        let mut free_a = DosRegs {
            ax: 0x4900,
            es: 0x1100,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut free_a, &mut mem).unwrap();
        assert!(!free_a.cf);
        // Free the top block B: reclaims its paragraphs, no underflow.
        let mut free_b = DosRegs {
            ax: 0x4900,
            es: 0x1110,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut free_b, &mut mem).unwrap();
        assert!(!free_b.cf);
        // A fresh allocation starts at 0x1110 (B's reclaimed top); A's hole at
        // 0x1100 is leaked, as documented.
        let mut c = DosRegs {
            ax: 0x4800,
            bx: 0x0008,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut c, &mut mem).unwrap();
        assert_eq!(c.ax, 0x1110);
    }

    #[test]
    fn ah48_zero_paragraphs_returns_a_segment_without_advancing() {
        // A zero-paragraph allocation is a legal DOS request: it returns the
        // current free_base and does not advance it.
        let mut mem = Memory::new(4096).unwrap();
        let mut kernel = arena_kernel();
        let mut z = DosRegs {
            ax: 0x4800,
            bx: 0x0000,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut z, &mut mem).unwrap();
        assert!(!z.cf);
        assert_eq!(z.ax, 0x1100); // free_base, unchanged
        let mut a = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut a, &mut mem).unwrap();
        assert_eq!(a.ax, 0x1100); // next allocation still at free_base
    }

    #[test]
    fn ah1a_2f_dta_round_trips_with_default_at_psp_0x80() {
        let mut mem = Memory::new(4096).unwrap();
        let mut kernel = DosKernel::new();
        kernel.init_program(0x0100, 0x1100);
        // Default DTA = PSP:0x80.
        let mut get = DosRegs {
            ax: 0x2f00,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get, &mut mem).unwrap();
        assert_eq!(get.es, 0x0100);
        assert_eq!(get.bx, 0x0080);
        // Set DTA to 0x1234:0x5678, read it back.
        let mut set = DosRegs {
            ax: 0x1a00,
            ds: 0x1234,
            dx: 0x5678,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set, &mut mem).unwrap();
        let mut get2 = DosRegs {
            ax: 0x2f00,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get2, &mut mem).unwrap();
        assert_eq!(get2.es, 0x1234);
        assert_eq!(get2.bx, 0x5678);
    }

    #[test]
    fn ah33_get_ctrl_break_returns_off() {
        let mut mem = Memory::new(4096).unwrap();
        let mut kernel = DosKernel::new();
        let mut regs = DosRegs {
            ax: 0x3300,
            dx: 0xffff,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.dx & 0xff, 0x00); // DL = 0 (Ctrl-Break checking off)
    }

    #[test]
    fn ah0e_reports_one_drive() {
        let mut mem = Memory::new(4096).unwrap();
        let mut kernel = DosKernel::new();
        let mut regs = DosRegs {
            ax: 0x0e00,
            dx: 0x0002,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.ax & 0xff, 0x01); // AL = number of logical drives
    }

    #[test]
    fn read_on_a_write_only_handle_returns_ax05() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("DATA.TXT", b"hi")], r"C:\DATA.TXT");
        // Open AL=1 (write-only) on the existing file.
        let mut open = DosRegs {
            ax: 0x3d01,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut open, &mut mem).unwrap();
        assert!(!open.cf, "open failed: ax={:#06x}", open.ax);
        let handle = open.ax;
        // Reading a write-only handle is access-denied.
        let mut read = DosRegs {
            ax: 0x3f00,
            bx: handle,
            cx: 16,
            ds: 0x0100,
            dx: 0x0400,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut read, &mut mem).unwrap();
        assert!(read.cf);
        assert_eq!(read.ax, 0x05);
    }

    #[test]
    fn ah3c_creates_a_file_and_returns_a_handle() {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = DosKernel::new();
        kernel.mount_c(HostDrive::mount_c(dir.path()).unwrap());
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let base = 0x0100usize * 16 + 0x0200;
        for (i, b) in r"C:\NEW.TXT".bytes().enumerate() {
            mem.write_u8(base + i, b).unwrap();
        }
        mem.write_u8(base + r"C:\NEW.TXT".len(), 0).unwrap();
        let mut regs = DosRegs {
            ax: 0x3c00,
            cx: 0,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf, "create failed: ax={:#06x}", regs.ax);
        assert!(regs.ax >= 5);
        assert!(dir.path().join("NEW.TXT").exists());
    }

    #[test]
    fn ah3c_truncates_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("OLD.TXT"), b"previous contents").unwrap();
        let mut kernel = DosKernel::new();
        kernel.mount_c(HostDrive::mount_c(dir.path()).unwrap());
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let base = 0x0100usize * 16 + 0x0200;
        for (i, b) in r"C:\OLD.TXT".bytes().enumerate() {
            mem.write_u8(base + i, b).unwrap();
        }
        mem.write_u8(base + r"C:\OLD.TXT".len(), 0).unwrap();
        let mut regs = DosRegs {
            ax: 0x3c00,
            cx: 0,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_eq!(
            std::fs::metadata(dir.path().join("OLD.TXT")).unwrap().len(),
            0
        );
    }

    #[test]
    fn ah40_on_a_read_only_handle_returns_ax05() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("R.TXT", b"hi")], r"C:\R.TXT");
        let mut open = DosRegs {
            ax: 0x3d00,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut open, &mut mem).unwrap();
        let handle = open.ax;
        let mut write = DosRegs {
            ax: 0x4000,
            bx: handle,
            cx: 2,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut write, &mut mem).unwrap();
        assert!(write.cf);
        assert_eq!(write.ax, 0x05);
    }

    #[test]
    fn ah40_to_stdout_handle_writes_to_the_output_buffer() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let src = 0x0100usize * 16 + 0x0300;
        for (i, b) in b"hello".iter().enumerate() {
            mem.write_u8(src + i, *b).unwrap();
        }
        let mut regs = DosRegs {
            ax: 0x4000,
            bx: 1,
            cx: 5,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_eq!(regs.ax, 5);
        assert_eq!(kernel.stdout(), b"hello");
    }

    #[test]
    fn ah40_to_an_invalid_handle_returns_ax06() {
        // BX=0 is not a routed console handle (1/2) and not an open file, so it
        // falls through to the table lookup and returns invalid handle.
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut regs = DosRegs {
            ax: 0x4000,
            bx: 0,
            cx: 1,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x06);
    }

    #[test]
    fn ah42_seek_set_cur_end_return_position() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("S.TXT"), b"0123456789").unwrap();
        let mut kernel = DosKernel::new();
        kernel.mount_c(HostDrive::mount_c(dir.path()).unwrap());
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let name_base = 0x0100usize * 16 + 0x0200;
        for (i, b) in r"C:\S.TXT".bytes().enumerate() {
            mem.write_u8(name_base + i, b).unwrap();
        }
        mem.write_u8(name_base + r"C:\S.TXT".len(), 0).unwrap();
        let mut open = DosRegs {
            ax: 0x3d02,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut open, &mut mem).unwrap();
        let handle = open.ax;
        // SET to 3.
        let mut s = DosRegs {
            ax: 0x4200,
            bx: handle,
            cx: 0,
            dx: 3,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut s, &mut mem).unwrap();
        assert!(!s.cf);
        assert_eq!(((u32::from(s.dx) << 16) | u32::from(s.ax)), 3);
        // CUR +2 -> 5.
        let mut c = DosRegs {
            ax: 0x4201,
            bx: handle,
            cx: 0,
            dx: 2,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut c, &mut mem).unwrap();
        assert_eq!(((u32::from(c.dx) << 16) | u32::from(c.ax)), 5);
        // END +0 -> 10 (file length).
        let mut e = DosRegs {
            ax: 0x4202,
            bx: handle,
            cx: 0,
            dx: 0,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut e, &mut mem).unwrap();
        assert_eq!(((u32::from(e.dx) << 16) | u32::from(e.ax)), 10);
    }

    #[test]
    fn ah42_bad_whence_returns_ax01() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("S.TXT", b"hi")], r"C:\S.TXT");
        let mut open = DosRegs {
            ax: 0x3d00,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut open, &mut mem).unwrap();
        let handle = open.ax;
        let mut bad = DosRegs {
            ax: 0x4203,
            bx: handle,
            cx: 0,
            dx: 0,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut bad, &mut mem).unwrap();
        assert!(bad.cf);
        assert_eq!(bad.ax, 0x01);
    }

    #[test]
    fn ah40_writes_bytes_and_ah3f_reads_them_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = DosKernel::new();
        kernel.mount_c(HostDrive::mount_c(dir.path()).unwrap());
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let name_base = 0x0100usize * 16 + 0x0200;
        for (i, b) in r"C:\W.TXT".bytes().enumerate() {
            mem.write_u8(name_base + i, b).unwrap();
        }
        mem.write_u8(name_base + r"C:\W.TXT".len(), 0).unwrap();
        let mut create = DosRegs {
            ax: 0x3c00,
            cx: 0,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut create, &mut mem).unwrap();
        let handle = create.ax;
        let src = 0x0100usize * 16 + 0x0300;
        for (i, b) in b"ABCD".iter().enumerate() {
            mem.write_u8(src + i, *b).unwrap();
        }
        let mut write = DosRegs {
            ax: 0x4000,
            bx: handle,
            cx: 4,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut write, &mut mem).unwrap();
        assert!(!write.cf);
        assert_eq!(write.ax, 4);
        assert_eq!(std::fs::read(dir.path().join("W.TXT")).unwrap(), b"ABCD");
        let mut seek = DosRegs {
            ax: 0x4200,
            bx: handle,
            cx: 0,
            dx: 0,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut seek, &mut mem).unwrap();
        let mut read = DosRegs {
            ax: 0x3f00,
            bx: handle,
            cx: 4,
            ds: 0x0100,
            dx: 0x0400,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut read, &mut mem).unwrap();
        assert_eq!(read.ax, 4);
        let dst = 0x0100usize * 16 + 0x0400;
        let got: Vec<u8> = (0..4).map(|i| mem.read_u8(dst + i).unwrap()).collect();
        assert_eq!(got, b"ABCD");
    }

    fn find_first(kernel: &mut DosKernel, mem: &mut Memory, filespec: &str, mask: u16) -> DosRegs {
        // Place the ASCIIZ filespec at DS:DX = 0x0010:0x0000 (linear 0x100), clear
        // of the DTA record written at linear 0 (the default DTA is (0,0)).
        let base = 0x100;
        for (i, byte) in filespec.bytes().enumerate() {
            mem.write_u8(base + i, byte).unwrap();
        }
        mem.write_u8(base + filespec.len(), 0).unwrap();
        let mut regs = DosRegs {
            ax: 0x4e00,
            cx: mask,
            ds: 0x0010,
            dx: 0x0000,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, mem).unwrap();
        regs
    }

    fn find_next(kernel: &mut DosKernel, mem: &mut Memory) -> DosRegs {
        let mut regs = DosRegs {
            ax: 0x4f00,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, mem).unwrap();
        regs
    }

    /// Read the ASCIIZ 8.3 name from the DTA record at linear 0, offset 0x1E.
    fn dta_name(mem: &Memory) -> String {
        let mut bytes = Vec::new();
        for i in 0..13 {
            let byte = mem.read_u8(0x1e + i).unwrap();
            if byte == 0 {
                break;
            }
            bytes.push(byte);
        }
        String::from_utf8(bytes).unwrap()
    }

    /// A kernel with `dir` mounted as C: and a 1 MiB memory for DTA writes.
    fn find_kernel(dir: &Path) -> (DosKernel, Memory) {
        let mut kernel = DosKernel::new();
        kernel.mount_c(HostDrive::mount_c(dir).unwrap());
        let mem = Memory::new(1024 * 1024).unwrap();
        (kernel, mem)
    }

    #[test]
    fn host_name_to_8_3_accepts_and_rejects() {
        assert_eq!(host_name_to_8_3("HELLO.TXT"), Some(*b"HELLO   TXT"));
        assert_eq!(host_name_to_8_3("readme"), Some(*b"README     "));
        assert_eq!(host_name_to_8_3("a.b.c"), None); // two dots
        assert_eq!(host_name_to_8_3("report.text"), None); // ext > 3
        assert_eq!(host_name_to_8_3("toolongname.do"), None); // base > 8
        assert_eq!(host_name_to_8_3(".cfg"), None); // empty base
        assert_eq!(host_name_to_8_3("MY FILE.TXT"), None); // space is not an 8.3 char
        assert_eq!(host_name_to_8_3("A+B.TXT"), None); // '+' is reserved
        assert_eq!(host_name_to_8_3("CONFIG_1.SYS"), Some(*b"CONFIG_1SYS")); // '_' is legal
    }

    #[test]
    fn template_matches_wildcards() {
        let star_dot_star = pattern_to_8_3("*.*");
        assert!(template_matches(
            &host_name_to_8_3("GAME.EXE").unwrap(),
            &star_dot_star
        ));
        assert!(template_matches(
            &host_name_to_8_3("README").unwrap(),
            &star_dot_star
        ));

        let star_exe = pattern_to_8_3("*.EXE");
        assert!(template_matches(
            &host_name_to_8_3("GAME.EXE").unwrap(),
            &star_exe
        ));
        assert!(!template_matches(
            &host_name_to_8_3("GAME.COM").unwrap(),
            &star_exe
        ));

        let level_q = pattern_to_8_3("LEVEL?.DAT");
        assert!(template_matches(
            &host_name_to_8_3("LEVEL1.DAT").unwrap(),
            &level_q
        ));
        assert!(template_matches(
            &host_name_to_8_3("LEVEL.DAT").unwrap(),
            &level_q
        ));
    }

    #[test]
    fn attr_matches_normal_and_directory() {
        assert!(attr_matches(0x00, 0x00)); // a normal file always matches
        assert!(!attr_matches(0x10, 0x00)); // a directory needs the mask bit
        assert!(attr_matches(0x10, 0x10));
        assert!(attr_matches(0x00, 0x10));
    }

    #[test]
    fn ah4e_finds_a_matching_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("HELLO.TXT"), b"hi there").unwrap();
        let (mut kernel, mut mem) = find_kernel(dir.path());
        let regs = find_first(&mut kernel, &mut mem, "C:\\*.TXT", 0);
        assert!(!regs.cf);
        assert_eq!(dta_name(&mem), "HELLO.TXT");
        assert_eq!(mem.read_u8(0x15).unwrap(), 0x00); // attr: normal file
        assert_eq!(mem.read_u32(0x1a).unwrap(), 8); // size
    }

    #[test]
    fn ah4e_no_match_returns_0x12() {
        let dir = tempfile::tempdir().unwrap();
        let (mut kernel, mut mem) = find_kernel(dir.path());
        let regs = find_first(&mut kernel, &mut mem, "C:\\*.TXT", 0);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x12);
    }

    #[test]
    fn ah4e_bad_directory_returns_0x03() {
        let dir = tempfile::tempdir().unwrap();
        let (mut kernel, mut mem) = find_kernel(dir.path());
        let regs = find_first(&mut kernel, &mut mem, "C:\\NOPE\\*.*", 0);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x03);
    }

    #[test]
    fn ah4e_skips_non_8_3_host_names_and_uppercases() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.txt"), b"a").unwrap();
        std::fs::write(dir.path().join("report.text"), b"b").unwrap(); // ext > 3
        std::fs::write(dir.path().join("a.b.c"), b"c").unwrap(); // two dots
        let (mut kernel, mut mem) = find_kernel(dir.path());
        let regs = find_first(&mut kernel, &mut mem, "C:\\*.*", 0);
        assert!(!regs.cf);
        assert_eq!(dta_name(&mem), "OK.TXT");
        let regs = find_next(&mut kernel, &mut mem);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x12);
    }

    #[test]
    fn ah4e_directory_attr_filtering() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("F.TXT"), b"a").unwrap();
        std::fs::create_dir(dir.path().join("SUB")).unwrap();

        let (mut kernel, mut mem) = find_kernel(dir.path());
        // Mask 0x00: directories excluded, only the file is found.
        let regs = find_first(&mut kernel, &mut mem, "C:\\*.*", 0x00);
        assert!(!regs.cf);
        assert_eq!(dta_name(&mem), "F.TXT");
        assert!(find_next(&mut kernel, &mut mem).cf);

        // Mask 0x10: the directory is included.
        let mut names = Vec::new();
        let regs = find_first(&mut kernel, &mut mem, "C:\\*.*", 0x10);
        assert!(!regs.cf);
        names.push(dta_name(&mem));
        loop {
            let regs = find_next(&mut kernel, &mut mem);
            if regs.cf {
                break;
            }
            names.push(dta_name(&mem));
        }
        names.sort();
        assert_eq!(names, vec!["F.TXT", "SUB"]);
        // The directory entry carries attr 0x10 in its record.
        let regs = find_first(&mut kernel, &mut mem, "C:\\SUB", 0x10);
        assert!(!regs.cf);
        assert_eq!(mem.read_u8(0x15).unwrap(), 0x10);
    }

    #[test]
    fn ah4f_iterates_all_matches_then_0x12() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["A.DAT", "B.DAT", "C.DAT"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let (mut kernel, mut mem) = find_kernel(dir.path());
        let mut names = Vec::new();
        let regs = find_first(&mut kernel, &mut mem, "C:\\*.DAT", 0);
        assert!(!regs.cf);
        names.push(dta_name(&mem));
        loop {
            let regs = find_next(&mut kernel, &mut mem);
            if regs.cf {
                assert_eq!(regs.ax, 0x12);
                break;
            }
            names.push(dta_name(&mem));
        }
        names.sort();
        assert_eq!(names, vec!["A.DAT", "B.DAT", "C.DAT"]);
    }

    #[test]
    fn ah4f_without_find_first_returns_0x12() {
        let dir = tempfile::tempdir().unwrap();
        let (mut kernel, mut mem) = find_kernel(dir.path());
        let regs = find_next(&mut kernel, &mut mem);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x12);
    }

    #[test]
    fn ah4e_record_layout_zeroes_reserved_area() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("F.TXT"), b"abcd").unwrap();
        let (mut kernel, mut mem) = find_kernel(dir.path());
        // Dirty the reserved area first to prove FindFirst zeroes it.
        for offset in 0..0x15 {
            mem.write_u8(offset, 0xff).unwrap();
        }
        let regs = find_first(&mut kernel, &mut mem, "C:\\*.*", 0);
        assert!(!regs.cf);
        for offset in 0..0x15 {
            assert_eq!(mem.read_u8(offset).unwrap(), 0, "reserved byte {offset:#x}");
        }
        assert_eq!(mem.read_u8(0x15).unwrap(), 0x00); // attr
        assert_ne!(mem.read_u16(0x18).unwrap(), 0); // date is the real host mtime
        assert_eq!(mem.read_u32(0x1a).unwrap(), 4); // size
        assert_eq!(dta_name(&mem), "F.TXT");
    }

    #[test]
    fn ah4e_packs_the_host_modified_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("STAMP.TXT");
        std::fs::write(&path, b"x").unwrap();
        // 2000-01-01 12:34:56 UTC.
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(946_730_096);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(when)
            .unwrap();
        let (mut kernel, mut mem) = find_kernel(dir.path());
        let regs = find_first(&mut kernel, &mut mem, "C:\\*.TXT", 0);
        assert!(!regs.cf);
        // date = ((2000-1980)<<9)|(1<<5)|1 = 10273; time = (12<<11)|(34<<5)|(56/2) = 25692.
        assert_eq!(mem.read_u16(0x18).unwrap(), 10_273);
        assert_eq!(mem.read_u16(0x16).unwrap(), 25_692);
    }

    #[test]
    fn ah40_cx0_truncates_at_the_current_position() {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = DosKernel::new();
        kernel.mount_c(HostDrive::mount_c(dir.path()).unwrap());
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let name_base = 0x0100usize * 16 + 0x0200;
        for (i, b) in r"C:\T.TXT".bytes().enumerate() {
            mem.write_u8(name_base + i, b).unwrap();
        }
        mem.write_u8(name_base + r"C:\T.TXT".len(), 0).unwrap();
        let mut create = DosRegs {
            ax: 0x3c00,
            cx: 0,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut create, &mut mem).unwrap();
        let handle = create.ax;
        let src = 0x0100usize * 16 + 0x0300;
        for i in 0..10u8 {
            mem.write_u8(src + usize::from(i), b'x').unwrap();
        }
        let mut write = DosRegs {
            ax: 0x4000,
            bx: handle,
            cx: 10,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut write, &mut mem).unwrap();
        let mut seek = DosRegs {
            ax: 0x4200,
            bx: handle,
            cx: 0,
            dx: 4,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut seek, &mut mem).unwrap();
        let mut trunc = DosRegs {
            ax: 0x4000,
            bx: handle,
            cx: 0,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut trunc, &mut mem).unwrap();
        assert!(!trunc.cf);
        assert_eq!(
            std::fs::metadata(dir.path().join("T.TXT")).unwrap().len(),
            4
        );
    }

    #[test]
    fn load_overlay_copies_raw_bytes() {
        let mut mem = Memory::new(0x10000).unwrap();
        let image = [0x12u8, 0x34, 0x56];
        load_overlay(&image, &mut mem, 0x0100, 0).unwrap();
        let base = 0x0100 * 16;
        assert_eq!(mem.read_u8(base).unwrap(), 0x12);
        assert_eq!(mem.read_u8(base + 2).unwrap(), 0x56);
    }

    #[test]
    fn load_overlay_applies_relocations() {
        // A 1-page MZ image: a 0x20-byte header (fixed 0x1c bytes + a 4-byte
        // relocation table at 0x1c), then one paragraph module holding a word
        // 0x1000. Load at 0x0200 with a relocation factor of 0x0030; the word
        // becomes 0x1030.
        let mut image = vec![0u8; 512];
        image[0..2].copy_from_slice(&0x5a4du16.to_le_bytes()); // "MZ"
        image[2..4].copy_from_slice(&512u16.to_le_bytes()); // e_cblp = full page
        image[4..6].copy_from_slice(&1u16.to_le_bytes()); // e_cp = 1 page
        image[6..8].copy_from_slice(&1u16.to_le_bytes()); // e_crlc = 1
        image[8..10].copy_from_slice(&2u16.to_le_bytes()); // e_cparhdr = 0x20 bytes
        image[0x0a..0x0c].copy_from_slice(&0u16.to_le_bytes()); // e_minalloc
        image[0x0c..0x0e].copy_from_slice(&0u16.to_le_bytes()); // e_maxalloc
        image[0x18..0x1a].copy_from_slice(&0x1cu16.to_le_bytes()); // e_lfarlc = 0x1c
        // relocation entry at 0x1c: (off=0, seg=0) -> module offset 0
        image[0x1c..0x20].copy_from_slice(&[0, 0, 0, 0]);
        // module at 0x20..: the word at module (0,0) = 0x1000
        image[0x20..0x22].copy_from_slice(&0x1000u16.to_le_bytes());

        let mut mem = Memory::new(0x10000).unwrap();
        load_overlay(&image, &mut mem, 0x0200, 0x0030).unwrap();
        assert_eq!(mem.read_u16(0x0200 * 16).unwrap(), 0x1030);
    }

    #[test]
    fn load_overlay_rejects_truncated_mz() {
        let mut mem = Memory::new(4096).unwrap();
        let bad = [0x4du8, 0x5a]; // claims MZ but header shorter than 0x1c
        assert!(load_overlay(&bad, &mut mem, 0x0100, 0).is_err());
    }

    #[test]
    fn load_overlay_rejects_out_of_range_reloc() {
        let mut image = vec![0u8; 512];
        image[0..2].copy_from_slice(&0x5a4du16.to_le_bytes());
        image[2..4].copy_from_slice(&512u16.to_le_bytes());
        image[4..6].copy_from_slice(&1u16.to_le_bytes());
        image[6..8].copy_from_slice(&1u16.to_le_bytes()); // one reloc
        image[8..10].copy_from_slice(&2u16.to_le_bytes()); // header 0x20
        image[0x18..0x1a].copy_from_slice(&0x1cu16.to_le_bytes()); // reloc table at 0x1c
        image[0x1c..0x20].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]); // far outside module
        let mut mem = Memory::new(0x10000).unwrap();
        assert!(matches!(
            load_overlay(&image, &mut mem, 0x0200, 0),
            Err(DosError::ExeRelocationOutOfRange)
        ));
    }

    /// A kernel with `dir` as C: and a 1 MiB memory for overlay writes.
    fn overlay_kernel(dir: &Path) -> (DosKernel, Memory) {
        let mut kernel = DosKernel::new();
        kernel.mount_c(HostDrive::mount_c(dir).unwrap());
        let mem = Memory::new(1024 * 1024).unwrap();
        (kernel, mem)
    }

    #[test]
    fn ah4b_al3_loads_overlay_via_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("OV.BIN"), [0x5au8]).unwrap();
        let (mut kernel, mut mem) = overlay_kernel(dir.path());
        // ASCIZ name "C:\OV.BIN" at seg 0x1000:0 (linear 0x10000).
        let name = b"C:\\OV.BIN";
        for (i, &b) in name.iter().enumerate() {
            mem.write_u8(0x10000 + i, b).unwrap();
        }
        mem.write_u8(0x10000 + name.len(), 0).unwrap();
        // EPB #01591 at seg 0x1000:0x40: load_seg=0x0500, reloc=0.
        mem.write_u16(0x10040, 0x0500).unwrap();
        mem.write_u16(0x10042, 0).unwrap();
        let mut regs = DosRegs {
            ax: 0x4b03,
            ds: 0x1000,
            dx: 0x0000,
            es: 0x1000,
            bx: 0x0040,
            ..DosRegs::default()
        };
        assert_eq!(
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap(),
            DosAction::Continue
        );
        assert!(!regs.cf);
        assert_eq!(mem.read_u8(0x0500 * 16).unwrap(), 0x5a);
    }

    #[test]
    fn ah4b_bad_subfunction_returns_0x01() {
        let dir = tempfile::tempdir().unwrap();
        let (mut kernel, mut mem) = overlay_kernel(dir.path());
        for al in [0x01u16, 0x04] {
            let mut regs = DosRegs {
                ax: 0x4b00 | al,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
            assert!(regs.cf);
            assert_eq!(regs.ax, 0x01);
        }
    }

    #[test]
    fn ah4b_missing_overlay_file_returns_0x02() {
        let dir = tempfile::tempdir().unwrap();
        let (mut kernel, mut mem) = overlay_kernel(dir.path());
        let name = b"C:\\NOPE.BIN";
        for (i, &b) in name.iter().enumerate() {
            mem.write_u8(0x10000 + i, b).unwrap();
        }
        mem.write_u8(0x10000 + name.len(), 0).unwrap();
        mem.write_u16(0x10040, 0x0500).unwrap();
        mem.write_u16(0x10042, 0).unwrap();
        let mut regs = DosRegs {
            ax: 0x4b03,
            ds: 0x1000,
            dx: 0x0000,
            es: 0x1000,
            bx: 0x0040,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x02);
    }

    /// A kernel with `dir` as C:, a parent program at PSP 0x0100 owning
    /// [0x0100, 0x0200), and a 1 MiB memory for the child PSP and inputs.
    fn exec_kernel(dir: &Path) -> (DosKernel, Memory) {
        let mut kernel = DosKernel::new();
        kernel.mount_c(HostDrive::mount_c(dir).unwrap());
        kernel.init_program(0x0100, 0x0200);
        let mem = Memory::new(1024 * 1024).unwrap();
        (kernel, mem)
    }

    /// Write `name` as ASCIIZ at linear 0x10000 and a 14-byte EPB at 0x10040
    /// (env word, then null cmdtail/fcb1/fcb2 pointers). The caller sets
    /// ds=0x1000 dx=0 es=0x1000 bx=0x40.
    fn place_exec_inputs(mem: &mut Memory, name: &str, env: u16) {
        let nb = 0x10000usize;
        for (i, &b) in name.as_bytes().iter().enumerate() {
            mem.write_u8(nb + i, b).unwrap();
        }
        mem.write_u8(nb + name.len(), 0).unwrap();
        mem.write_u16(0x10040, env).unwrap();
        mem.write_u16(0x10042, 0).unwrap(); // cmdtail off
        mem.write_u16(0x10044, 0).unwrap(); // cmdtail seg (0:0 = null)
        mem.write_u16(0x10046, 0).unwrap(); // fcb1 off
        mem.write_u16(0x10048, 0).unwrap(); // fcb1 seg
        mem.write_u16(0x1004a, 0).unwrap(); // fcb2 off
        mem.write_u16(0x1004c, 0).unwrap(); // fcb2 seg
    }

    fn exec_al0(kernel: &mut DosKernel, mem: &mut Memory) -> DosRegs {
        let mut regs = DosRegs {
            ax: 0x4b00,
            ds: 0x1000,
            dx: 0x0000,
            es: 0x1000,
            bx: 0x0040,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, mem).unwrap();
        regs
    }

    #[test]
    fn fcb_drive_validity_table() {
        assert_eq!(fcb_drive_validity(0), 0); // default
        assert_eq!(fcb_drive_validity(3), 0); // C:
        assert_eq!(fcb_drive_validity(26), 0);
        assert_eq!(fcb_drive_validity(27), 0xff);
    }

    #[test]
    fn ah4b_al0_builds_child_psp_and_returns_exec() {
        let dir = tempfile::tempdir().unwrap();
        // A minimal child .COM: INT 20h (terminate). It never runs in this test.
        std::fs::write(dir.path().join("CHILD.COM"), [0xcdu8, 0x20]).unwrap();
        let (mut kernel, mut mem) = exec_kernel(dir.path());
        place_exec_inputs(&mut mem, "C:\\CHILD.COM", 0);
        let mut regs = DosRegs {
            ax: 0x4b00,
            ds: 0x1000,
            dx: 0,
            es: 0x1000,
            bx: 0x40,
            ..DosRegs::default()
        };
        // env is 1 paragraph at the parent free_base 0x0200, so child PSP = 0x0201.
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        let child_psp = 0x0201usize * 16;
        match action {
            DosAction::Exec { child_ax, .. } => {
                assert_eq!(child_ax, 0x0000); // null FCBs -> valid drives
                assert_eq!(mem.read_u16(child_psp + 0x02).unwrap(), 0xa000);
                assert_eq!(mem.read_u16(child_psp + 0x16).unwrap(), 0x0100); // parent
                assert_eq!(mem.read_u16(child_psp + 0x2c).unwrap(), 0x0200); // env seg
                assert_eq!(mem.read_u8(child_psp + 0x80).unwrap(), 0); // empty tail
                assert_eq!(mem.read_u8(child_psp + 0x81).unwrap(), 0x0d);
                assert_eq!(mem.read_u8(child_psp + 0x18).unwrap(), 0x01); // JFT stdin
            }
            other => panic!("expected Exec, got {other:?}"),
        }
    }

    #[test]
    fn ah4b_al0_inherits_empty_environment() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CHILD.COM"), [0xcdu8, 0x20]).unwrap();
        let (mut kernel, mut mem) = exec_kernel(dir.path());
        place_exec_inputs(&mut mem, "C:\\CHILD.COM", 0);
        let _ = exec_al0(&mut kernel, &mut mem);
        // env block at 0x0200:0 is a single terminating NUL.
        assert_eq!(mem.read_u8(0x0200 * 16).unwrap(), 0x00);
    }

    #[test]
    fn ah4b_al0_copies_explicit_environment() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CHILD.COM"), [0xcdu8, 0x20]).unwrap();
        let (mut kernel, mut mem) = exec_kernel(dir.path());
        // Source env at seg 0x3000: "A=1\0B=2\0\0".
        let src = 0x3000usize * 16;
        for (i, &b) in b"A=1\0B=2\0\0".iter().enumerate() {
            mem.write_u8(src + i, b).unwrap();
        }
        place_exec_inputs(&mut mem, "C:\\CHILD.COM", 0x3000);
        let _ = exec_al0(&mut kernel, &mut mem);
        // Copied env at 0x0200:0 holds the same bytes.
        for (i, &b) in b"A=1\0B=2\0\0".iter().enumerate() {
            assert_eq!(mem.read_u8(0x0200 * 16 + i).unwrap(), b);
        }
    }

    #[test]
    fn ah4b_missing_program_returns_0x02() {
        let dir = tempfile::tempdir().unwrap();
        let (mut kernel, mut mem) = exec_kernel(dir.path());
        place_exec_inputs(&mut mem, "C:\\NOPE.COM", 0);
        let regs = exec_al0(&mut kernel, &mut mem);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x02);
        assert!(kernel.program_stack.is_empty()); // no child context pushed
    }

    #[test]
    fn finish_exec_restores_parent_context_and_records_code() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CHILD.COM"), [0xcdu8, 0x20]).unwrap();
        let (mut kernel, mut mem) = exec_kernel(dir.path());
        place_exec_inputs(&mut mem, "C:\\CHILD.COM", 0);
        let mut regs = DosRegs {
            ax: 0x4b00,
            ds: 0x1000,
            dx: 0,
            es: 0x1000,
            bx: 0x40,
            ..DosRegs::default()
        };
        assert!(matches!(
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap(),
            DosAction::Exec { .. }
        ));
        // Now in the child context (psp_seg = 0x0201).
        assert_eq!(kernel.arena.psp_seg, 0x0201);
        kernel.finish_exec(7);
        assert_eq!(kernel.arena.psp_seg, 0x0100); // parent restored
        assert_eq!(kernel.arena.free_base, 0x0200);
        assert_eq!(kernel.last_exit_code, 7);
    }

    #[test]
    fn ah4d_returns_then_clears_the_exit_code() {
        let mut kernel = DosKernel::new();
        kernel.last_exit_code = 7;
        let mut mem = Memory::new(4096).unwrap();
        let mut regs = DosRegs {
            ax: 0x4d00,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_eq!(regs.ax, 0x0007);
        // Second read returns 0 (one-shot).
        regs.ax = 0x4d00;
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.ax, 0x0000);
    }

    #[test]
    fn ah4d_in_a_fresh_child_reads_zero() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CHILD.COM"), [0xcdu8, 0x20]).unwrap();
        let (mut kernel, mut mem) = exec_kernel(dir.path());
        kernel.last_exit_code = 5; // a prior child's code, must not leak in
        place_exec_inputs(&mut mem, "C:\\CHILD.COM", 0);
        let mut regs = DosRegs {
            ax: 0x4b00,
            ds: 0x1000,
            dx: 0,
            es: 0x1000,
            bx: 0x40,
            ..DosRegs::default()
        };
        assert!(matches!(
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap(),
            DosAction::Exec { .. }
        ));
        // In the child context, AH=4Dh reads 0 (reset on entry).
        let mut regs = DosRegs {
            ax: 0x4d00,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.ax, 0x0000);
    }

    #[test]
    fn ah4b_al0_bad_format_returns_0x0b() {
        let dir = tempfile::tempdir().unwrap();
        // A truncated MZ image: claims "MZ" but the header is shorter than 0x1c.
        std::fs::write(dir.path().join("CHILD.COM"), [0x4du8, 0x5a]).unwrap();
        let (mut kernel, mut mem) = exec_kernel(dir.path());
        place_exec_inputs(&mut mem, "C:\\CHILD.COM", 0);
        let regs = exec_al0(&mut kernel, &mut mem);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x0b);
    }

    #[test]
    fn ah4b_al0_invalid_fcb_drives_yield_ffff_child_ax() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CHILD.COM"), [0xcdu8, 0x20]).unwrap();
        let (mut kernel, mut mem) = exec_kernel(dir.path());
        // Name at 0x10000.
        let name = b"C:\\CHILD.COM";
        for (i, &b) in name.iter().enumerate() {
            mem.write_u8(0x10000 + i, b).unwrap();
        }
        mem.write_u8(0x10000 + name.len(), 0).unwrap();
        // A 16-byte FCB at 0x1000:0x80 with an invalid drive byte (27).
        for i in 0..16 {
            mem.write_u8(0x10080 + i, if i == 0 { 27 } else { 0 })
                .unwrap();
        }
        // EPB: env=0, null cmdtail, FCB1 and FCB2 both -> 0x1000:0x80.
        mem.write_u16(0x10040, 0).unwrap();
        mem.write_u16(0x10042, 0).unwrap();
        mem.write_u16(0x10044, 0).unwrap();
        mem.write_u16(0x10046, 0x0080).unwrap();
        mem.write_u16(0x10048, 0x1000).unwrap();
        mem.write_u16(0x1004a, 0x0080).unwrap();
        mem.write_u16(0x1004c, 0x1000).unwrap();
        let mut regs = DosRegs {
            ax: 0x4b00,
            ds: 0x1000,
            dx: 0,
            es: 0x1000,
            bx: 0x40,
            ..DosRegs::default()
        };
        match kernel.dispatch(0x21, &mut regs, &mut mem).unwrap() {
            DosAction::Exec { child_ax, .. } => assert_eq!(child_ax, 0xffff),
            other => panic!("expected Exec, got {other:?}"),
        }
    }

    // --- environment-segment seeding ---

    #[test]
    fn build_env_block_formats_entries_as_asciiz_key_value() {
        // One entry: "FOO=bar" + its NUL, then the empty-string terminator NUL.
        assert_eq!(build_env_block(&[("FOO", "bar")]), b"FOO=bar\0\0");
        // Two entries chain, each NUL-terminated, then the terminator.
        assert_eq!(
            build_env_block(&[("A", "1"), ("B", "two")]),
            b"A=1\0B=two\0\0"
        );
    }

    #[test]
    fn build_env_block_with_no_entries_is_just_the_terminator() {
        // A valid empty environment is a single NUL (the terminating empty string).
        assert_eq!(build_env_block(&[]), b"\0");
    }

    /// Walk a written env block back into (KEY, VALUE) pairs, mirroring what a
    /// DOS game does when it scans the segment named by PSP:0x2C.
    fn parse_env_block(mem: &Memory, seg: u16) -> Vec<(String, String)> {
        let base = usize::from(seg) * 16;
        let mut entries = Vec::new();
        let mut offset = 0usize;
        loop {
            let mut bytes = Vec::new();
            loop {
                let byte = mem.read_u8(base + offset).unwrap();
                offset += 1;
                if byte == 0 {
                    break;
                }
                bytes.push(byte);
            }
            if bytes.is_empty() {
                break; // the terminating empty string
            }
            let entry = String::from_utf8(bytes).unwrap();
            let (key, value) = entry.split_once('=').expect("KEY=VALUE");
            entries.push((key.to_string(), value.to_string()));
        }
        entries
    }

    /// Build a PSP at 0x0100, seed the arena, and return (kernel, prog_top). The
    /// loader sets PSP:0x02 to segment + 0x1000 = 0x1100; the install tests build
    /// on that realistic prog_top.
    fn env_kernel() -> (DosKernel, Memory, u16) {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        load_com(&[0xb8, 0x00, 0x4c, 0xcd, 0x21], &mut mem, 0x0100).unwrap();
        let prog_top = mem.read_u16(0x0100 * 16 + 2).unwrap();
        let mut kernel = DosKernel::new();
        kernel.init_program(0x0100, prog_top);
        (kernel, mem, prog_top)
    }

    #[test]
    fn install_environment_seeds_psp_env_pointer_and_parses_back() {
        let (mut kernel, mut mem, prog_top) = env_kernel();
        kernel
            .install_environment(&mut mem, &[("BLASTER", "A220 I5 D1 H5 T6")])
            .unwrap();

        // PSP:0x2C names the env segment, which sits directly above the program.
        let env_seg = mem.read_u16(0x0100 * 16 + 0x2c).unwrap();
        assert_eq!(env_seg, prog_top);
        // The block at env_seg:0 scans back to the single BLASTER entry.
        assert_eq!(
            parse_env_block(&mem, env_seg),
            vec![("BLASTER".to_string(), "A220 I5 D1 H5 T6".to_string())]
        );
    }

    #[test]
    fn install_environment_advances_the_arena_above_the_block() {
        let (mut kernel, mut mem, prog_top) = env_kernel();
        kernel
            .install_environment(&mut mem, &[("BLASTER", "A220 I5 D1 H5 T6")])
            .unwrap();
        // The next AH=48h allocation must land above the env block, proving the
        // arena's free base advanced by the rounded-up paragraph count.
        let env_paras = u16::try_from(
            build_env_block(&[("BLASTER", "A220 I5 D1 H5 T6")])
                .len()
                .div_ceil(16),
        )
        .unwrap();
        let mut regs = DosRegs {
            ax: 0x4800,
            bx: 0x0001,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_eq!(regs.ax, prog_top + env_paras);
    }

    #[test]
    fn install_environment_leaves_the_psp_load_fields_intact() {
        let (mut kernel, mut mem, prog_top) = env_kernel();
        let psp = 0x0100usize * 16;
        // Snapshot the fields load_com built, then install the env.
        assert_eq!(mem.read_u8(psp).unwrap(), 0xcd); // INT 20h at PSP:0
        assert_eq!(mem.read_u8(psp + 1).unwrap(), 0x20);
        assert_eq!(mem.read_u8(psp + 0x80).unwrap(), 0x00); // empty command tail
        assert_eq!(mem.read_u8(psp + 0x81).unwrap(), 0x0d);
        kernel
            .install_environment(&mut mem, &[("BLASTER", "A220 I5 D1 H5 T6")])
            .unwrap();
        // The install writes only PSP:0x2C; the loader's fields are untouched.
        assert_eq!(mem.read_u8(psp).unwrap(), 0xcd);
        assert_eq!(mem.read_u8(psp + 1).unwrap(), 0x20);
        assert_eq!(mem.read_u16(psp + 2).unwrap(), prog_top); // top-of-memory
        assert_eq!(mem.read_u8(psp + 0x80).unwrap(), 0x00);
        assert_eq!(mem.read_u8(psp + 0x81).unwrap(), 0x0d);
        assert_eq!(mem.read_u16(psp + 0x2c).unwrap(), prog_top); // env seg
    }

    #[test]
    fn install_environment_with_no_entries_still_allocates_a_segment() {
        // An empty env (sound disabled) is still a valid block: PSP:0x2C names a
        // readable segment whose first byte is the terminator NUL.
        let (mut kernel, mut mem, prog_top) = env_kernel();
        kernel.install_environment(&mut mem, &[]).unwrap();
        let env_seg = mem.read_u16(0x0100 * 16 + 0x2c).unwrap();
        assert_eq!(env_seg, prog_top);
        assert_eq!(mem.read_u8(usize::from(env_seg) * 16).unwrap(), 0);
        assert!(parse_env_block(&mem, env_seg).is_empty());
    }

    #[test]
    fn ah49_frees_the_seeded_environment_segment() {
        let (mut kernel, mut mem, _prog_top) = env_kernel();
        kernel
            .install_environment(&mut mem, &[("BLASTER", "A220 I5 D1 H5 T6")])
            .unwrap();
        let env_seg = mem.read_u16(0x0100 * 16 + 0x2c).unwrap();
        // AH=49h on the env segment frees it (the arena treats it as a normal block).
        let mut regs = DosRegs {
            ax: 0x4900,
            es: env_seg,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
    }

    #[test]
    fn install_environment_carves_room_from_a_maxed_program_block() {
        // An .EXE whose e_maxalloc claims all of conventional memory leaves
        // PSP:0x02 at ARENA_TOP. The env must still be carved out of the program
        // block, and PSP:0x02 reduced to match — real DOS sizes the program block
        // after reserving the environment.
        let mut mem = Memory::new(1024 * 1024).unwrap();
        build_psp(&mut mem, 0x0100, ARENA_TOP).unwrap();
        let mut kernel = DosKernel::new();
        kernel.init_program(0x0100, ARENA_TOP);
        kernel
            .install_environment(&mut mem, &[("BLASTER", "A220 I5 D1 H5 T6")])
            .unwrap();
        let paras = u16::try_from(
            build_env_block(&[("BLASTER", "A220 I5 D1 H5 T6")])
                .len()
                .div_ceil(16),
        )
        .unwrap();
        let psp = 0x0100usize * 16;
        // PSP:0x02 is reduced by the env paragraphs ...
        assert_eq!(mem.read_u16(psp + 2).unwrap(), ARENA_TOP - paras);
        // ... and PSP:0x2C names the env segment carved from the top.
        let env_seg = mem.read_u16(psp + 0x2c).unwrap();
        assert_eq!(env_seg, ARENA_TOP - paras);
        assert_eq!(
            parse_env_block(&mem, env_seg),
            vec![("BLASTER".to_string(), "A220 I5 D1 H5 T6".to_string())]
        );
    }
}
