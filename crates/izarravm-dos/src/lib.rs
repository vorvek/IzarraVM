use izarravm_bus::{BusError, Memory};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

mod memory;

use memory::{
    ARENA_TOP, Arena, ResizeError, UmbArena, allocate_strategy, free_routed,
    is_valid_alloc_strategy, release_umb, request_umb, resize_routed, resize_umb, set_umb_region,
    write_child_program_mcb, write_env_mcb, write_free_mcb_to_cap, write_sysvars,
};
#[cfg(test)]
use memory::{NO_NAME, RamMcb, read_mcb_chain, write_mcb_header};

// BDA keyboard ring (the buffer the resident keyboard BIOS fills). Segment 0x40,
// head at 0x1a, tail at 0x1c, a 16-entry ring at 0x1e..0x3e. Each entry is a word
// scancode<<8 | ascii. Empty when head == tail.
const KBD_BDA_BASE: usize = 0x400;
const KBD_HEAD: usize = 0x1a;
const KBD_TAIL: usize = 0x1c;
const KBD_RING_START: u16 = 0x1e;
const KBD_RING_END: u16 = 0x3e; // exclusive

fn kbd_ring_is_empty(mem: &Memory) -> Result<bool, DosError> {
    let head = mem.read_u16(KBD_BDA_BASE + KBD_HEAD)?;
    let tail = mem.read_u16(KBD_BDA_BASE + KBD_TAIL)?;
    Ok(head == tail)
}

/// Dequeue the next (scancode, ascii) pair, advancing the head with wrap. None
/// when the ring is empty.
fn kbd_ring_dequeue(mem: &mut Memory) -> Result<Option<(u8, u8)>, DosError> {
    let head = mem.read_u16(KBD_BDA_BASE + KBD_HEAD)?;
    let tail = mem.read_u16(KBD_BDA_BASE + KBD_TAIL)?;
    if head == tail {
        return Ok(None);
    }
    let word = mem.read_u16(KBD_BDA_BASE + head as usize)?;
    let mut next = head + 2;
    if next >= KBD_RING_END {
        next = KBD_RING_START;
    }
    mem.write_u16(KBD_BDA_BASE + KBD_HEAD, next)?;
    Ok(Some(((word >> 8) as u8, (word & 0xff) as u8)))
}

fn kbd_ring_flush(mem: &mut Memory) -> Result<(), DosError> {
    let tail = mem.read_u16(KBD_BDA_BASE + KBD_TAIL)?;
    mem.write_u16(KBD_BDA_BASE + KBD_HEAD, tail)?;
    Ok(())
}

/// Seed the ring with ASCII bytes (scancode byte left zero), for tests and the
/// machine-level convenience seeder. Holds up to 15 bytes; longer inputs should
/// drive the real INT 09h path instead.
pub fn seed_keyboard_ring(mem: &mut Memory, ascii: &[u8]) -> Result<(), DosError> {
    debug_assert!(ascii.len() < 16, "keyboard ring holds 15 entries");
    mem.write_u16(KBD_BDA_BASE + KBD_HEAD, KBD_RING_START)?;
    let mut off = KBD_RING_START;
    for &b in ascii {
        mem.write_u16(KBD_BDA_BASE + off as usize, u16::from(b))?;
        off += 2;
        if off >= KBD_RING_END {
            off = KBD_RING_START;
        }
    }
    mem.write_u16(KBD_BDA_BASE + KBD_TAIL, off)?;
    Ok(())
}

/// Resolve the C: root: `<base>/c_drive` if it exists (portable mode), else
/// `<home>/.izarravm/c_drive`. The chosen path is created if missing.
pub fn resolve_c_root_in(base: &Path, home: &Path) -> PathBuf {
    let local = base.join("c_drive");
    let chosen = if local.is_dir() {
        local
    } else {
        home.join(".izarravm").join("c_drive")
    };
    let _ = std::fs::create_dir_all(&chosen);
    chosen
}

/// Resolve the C: root against the process working directory and the host home
/// directory. `home_dir` is un-deprecated on the project MSRV and behaves
/// correctly on Windows and Unix, so no `dirs` crate is pulled in.
pub fn resolve_c_root() -> PathBuf {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    #[allow(deprecated)]
    let home = std::env::home_dir().unwrap_or_else(|| base.clone());
    resolve_c_root_in(&base, &home)
}

/// How `toka_dos_install` lays the OS down onto the C: drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    /// Install only if Toka-DOS is absent (first boot). The presence of
    /// `ICOMMAND.COM` is the marker.
    EnsureIfMissing,
    /// Overwrite the system files from ROM, leaving any user files in place.
    Repair,
    /// Wipe the drive, then reinstall every system file.
    Format,
}

/// The marker file whose presence means Toka-DOS is already installed.
const TOKA_DOS_MARKER: &str = "ICOMMAND.COM";

/// System files that stay at the C: root: the shell and its COMMAND.COM alias,
/// the COMSPEC the boot path launches. Every other system file is a tool and
/// installs under C:\DOS, where the shell's PATH search finds it.
const ROOT_SYSTEM_FILES: &[&str] = &["ICOMMAND.COM", "COMMAND.COM"];

fn is_root_system_file(name: &str) -> bool {
    ROOT_SYSTEM_FILES
        .iter()
        .any(|root| root.eq_ignore_ascii_case(name))
}

/// The default AUTOEXEC.BAT: put the tools directory on the PATH and set a
/// path-showing prompt. DOS line endings (CRLF).
const DEFAULT_AUTOEXEC_BAT: &str = "@ECHO OFF\r\nPATH=C:\\DOS\r\nPROMPT=$P$G\r\n";

const DEFAULT_FILE_COUNT: u16 = 40;
const DEFAULT_BUFFER_COUNT: u16 = 20;

/// The default CONFIG.SYS: the directives a period DOS carries. The HIMEM.SYS
/// and IEMM.EXE RAM lines select the IEMM RAM mode (UMBs plus the EMS page
/// frame) at SYSINIT, the way a real DOS=HIGH,UMB box is configured; the machine
/// parses these to drive the memory layout. IEMM.EXE is the Toka-DOS memory
/// manager; the parser also accepts the real-DOS EMM386.EXE name so a pasted
/// real-DOS config still drives the mode. The CD-extension DEVICE= line is still
/// left out until that driver exists.
const DEFAULT_CONFIG_SYS: &str = "DEVICE=C:\\DOS\\HIMEM.SYS /TESTMEM:OFF\r\nDEVICE=C:\\DOS\\IEMM.EXE RAM\r\nDOS=HIGH,UMB\r\nFILES=40\r\nBUFFERS=20\r\nLASTDRIVE=E\r\n";

fn write_system_files(c_root: &Path, files: &[(String, Vec<u8>)]) -> std::io::Result<()> {
    std::fs::create_dir_all(c_root)?;
    let dos_dir = c_root.join("DOS");
    std::fs::create_dir_all(&dos_dir)?;
    for (name, bytes) in files {
        let dir = if is_root_system_file(name) {
            c_root
        } else {
            dos_dir.as_path()
        };
        std::fs::write(dir.join(name), bytes)?;
    }
    write_default_boot_config(c_root)
}

/// Write the default AUTOEXEC.BAT and CONFIG.SYS at the C: root, but only when
/// each is absent, so a reinstall or repair never clobbers a user's edits.
fn write_default_boot_config(c_root: &Path) -> std::io::Result<()> {
    let autoexec = c_root.join("AUTOEXEC.BAT");
    if !autoexec.exists() {
        std::fs::write(autoexec, DEFAULT_AUTOEXEC_BAT)?;
    }
    let config = c_root.join("CONFIG.SYS");
    if !config.exists() {
        std::fs::write(config, DEFAULT_CONFIG_SYS)?;
    }
    Ok(())
}

fn clear_directory(c_root: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(c_root)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Lay Toka-DOS down onto the C: drive. `files` are the OS system files from the
/// motherboard ROM, as (DOS 8.3 name, bytes) pairs. This is what the first-boot
/// install, the BIOS "Repair Toka-DOS" option, and the BIOS "Format Drive"
/// option all call.
pub fn toka_dos_install(
    c_root: &Path,
    files: &[(String, Vec<u8>)],
    mode: InstallMode,
) -> std::io::Result<()> {
    match mode {
        InstallMode::EnsureIfMissing => {
            if c_root.join(TOKA_DOS_MARKER).exists() {
                return Ok(());
            }
            write_system_files(c_root, files)
        }
        InstallMode::Repair => write_system_files(c_root, files),
        InstallMode::Format => {
            if c_root.exists() {
                clear_directory(c_root)?;
            }
            write_system_files(c_root, files)
        }
    }
}

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
    /// A blocking console read (AH=01/07/08, and AH=0Ah once added) found the
    /// keyboard ring empty. The machine rewinds the INT 21h so it re-executes
    /// after the ISR refills the ring. The kernel leaves the registers unchanged.
    WaitForKey,
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
    /// Validate the access mode in the low nibble of an open byte: 0=read,
    /// 1=write, 2=read/write. Any other value (3-15, including a set reserved
    /// bit 3) is invalid, and a real DOS open rejects it with error 0x0C rather
    /// than silently treating it as read. The MS-DOS source masks the full nibble
    /// (access_mask = 0x0F). The sharing and inheritance bits in the high nibble
    /// are not validated here.
    fn try_from_open_al(al: u8) -> Option<Self> {
        match al & 0x0f {
            0 => Some(AccessMode::Read),
            1 => Some(AccessMode::Write),
            2 => Some(AccessMode::ReadWrite),
            _ => None,
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

/// Whether a DOS filename names the EMMXXXX0 character device. DOS matches a
/// device by its base name regardless of drive, path, or extension, so EMMXXXX0,
/// C:\EMMXXXX0, and EMMXXXX0.SYS all refer to the device.
fn is_ems_device_name(name: &str) -> bool {
    name.rsplit(['\\', '/'])
        .next()
        .unwrap_or(name)
        .split('.')
        .next()
        .unwrap_or("")
        .eq_ignore_ascii_case("EMMXXXX0")
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
    // The parent's free-tail segment at the moment of EXEC. The child's env and
    // program blocks are carved from here upward, so on the child's exit finish_exec
    // frees the parent's memory back from this segment, capped below any resident
    // (TSR) region the child or a descendant left above it.
    free_base: u16,
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
    stdout: Vec<u8>,
    clock: DosDateTime,
    arena: Arena,
    dta: (u16, u16),
    find_searches: HashMap<(u16, u16), FindSearch>,
    // Parent program frames for nested EXEC (AL=0); restored on child exit.
    program_stack: Vec<ProgramContext>,
    // Base segments of resident (AH=31h TSR) MCB regions. An ancestor exiting must
    // not reclaim memory at or above a resident region, so a free-tail restore is
    // capped below the lowest resident base above it. Never pruned (TSRs persist).
    resident_regions: Vec<u16>,
    last_exit_code: u8, // AH=4Dh AL; cleared after it is read
    last_exit_type: u8, // AH=4Dh AH; always 0x00 (normal termination), marked
    // AH=0Ch flush-and-invoke: the flush runs once, not again on a WaitForKey re-entry.
    cooked_flush_done: bool,
    // Extended/function keys (arrows, F-keys) arrive on the ring as a (scancode, 0)
    // pair. DOS cooked input returns them as two reads: 0x00 first, then the scancode
    // on the next AH=01/06/07/08/0Ch call. This holds the scancode between the two.
    pending_scancode: Option<u8>,
    // AH=0Ah buffered input: the running character count keyed by buffer address,
    // so it survives the per-character WaitForKey re-entries.
    pending_line: Option<(u32, u8)>,
    // Current directory on C:, as a path from the root with no leading or trailing
    // backslash ("" is the root, "DOS" is \DOS, "DOS\\NET" is \DOS\NET). This is
    // the format AH=47h returns. The current directory is global in DOS, so it is
    // not saved or restored across EXEC.
    cwd: String,
    // AH=2Eh/54h write-verify flag. The HLE writes host files directly, so this has
    // no datapath effect; it only round-trips for guests that read it back.
    verify_flag: bool,
    // AH=58h memory-allocation strategy. Bits 6-7 route allocations between low
    // conventional memory and the linked upper-memory arena.
    alloc_strategy: u16,
    // AH=58h AL=03 UMB link state. Gates whether a high or high-then-low allocation
    // strategy is routed into the upper-memory arena; the arena itself exists in RAM
    // regardless. DOS defaults to unlinked (false).
    umb_link: bool,
    // The upper-memory-block arena, when the machine has furnished one (see
    // `set_umb_region`). None on a machine with no UMB-able memory.
    umb: Option<UmbArena>,
    // Whether the EMS manager (the EMMXXXX0 character device) is present. The
    // machine sets it from the EMM386 mode; it gates opening the EMMXXXX0 device
    // and listing it in the device chain, the way a guest detects expanded memory.
    ems_present: bool,
    // Whether CONFIG.SYS carried DOS=UMB. Real DOS lets a program link the upper
    // area through AH=5803h only when the box was loaded with DOS=UMB; without it,
    // upper memory is reachable only through the XMS Request UMB primitive. The
    // machine sets this from the parsed CONFIG.SYS.
    dos_umb: bool,
    // Highest configured DOS drive index from CONFIG.SYS LASTDRIVE=, A: = 1.
    // None means the shipped default E: value is published.
    lastdrive: Option<u8>,
    // CONFIG.SYS FILES= count. DOS counts the five inherited standard handles in
    // this total, so dynamic handle allocation starts at 5 and stops before it.
    file_count: Option<u16>,
    // CONFIG.SYS BUFFERS= count. Stored for the later disk-buffer-chain model.
    buffer_count: Option<u16>,
    // Handles a guest has opened on the EMMXXXX0 device (AH=3Dh), so AH=44h IOCTL
    // and AH=3Eh close treat them as the device rather than a host file.
    ems_handles: HashSet<u16>,
    // AH=59h extended error: the last DOS error code reported to a guest. Held until
    // the next error overwrites it (DOS does not clear it on a successful call).
    last_error: u16,
}

impl DosKernel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount a host directory as the C: drive. File opens resolve against it.
    pub fn mount_c(&mut self, drive: HostDrive) {
        self.drive = Some(drive);
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
    pub fn init_program(
        &mut self,
        psp_seg: u16,
        prog_top: u16,
        mem: &mut Memory,
    ) -> Result<(), DosError> {
        self.arena = Arena {
            psp_seg,
            chain_first: psp_seg.wrapping_sub(1),
            resident: false,
        };
        self.arena.write_initial_chain(mem, prog_top)?;
        self.dta = (psp_seg, 0x80);
        self.find_searches.clear();
        self.program_stack.clear();
        self.last_exit_code = 0;
        self.last_exit_type = 0;
        Ok(())
    }

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
    ) -> Result<(), DosError> {
        set_umb_region(&mut self.umb, &mut self.umb_link, seg, paras, mem)
    }

    /// Set whether the EMS manager (the EMMXXXX0 device) is present. The machine
    /// calls this from the EMM386 mode at DOS init: a guest then detects expanded
    /// memory by opening EMMXXXX0 or walking the device chain.
    pub fn set_ems_present(&mut self, present: bool) {
        self.ems_present = present;
    }

    /// Whether the EMS manager (the EMMXXXX0 device) is present. The machine sets
    /// this from the built `ems` Option at DOS init, so it is true under both RAM
    /// and NOEMS (the frameless manager) and false only for HIMEM-only.
    pub fn ems_present(&self) -> bool {
        self.ems_present
    }

    /// The lowest free file handle (>= 5), skipping both host files and the open
    /// EMS-device handles so the two never collide. FILES= includes inherited
    /// handles 0-4, so dynamic handles stop before the configured count.
    fn alloc_handle(&self) -> Option<u16> {
        (5u16..self.file_count())
            .find(|h| !self.open_files.contains_key(h) && !self.ems_handles.contains(h))
    }

    pub fn file_count(&self) -> u16 {
        self.file_count.unwrap_or(DEFAULT_FILE_COUNT)
    }

    pub fn buffer_count(&self) -> u16 {
        self.buffer_count.unwrap_or(DEFAULT_BUFFER_COUNT)
    }

    /// Walk the upper-memory arena's MCB chain, empty when no UMB pool is furnished.
    #[cfg(test)]
    fn umb_chain(&self, mem: &Memory) -> Vec<RamMcb> {
        match self.umb {
            Some(arena) => arena.chain(mem),
            None => Vec::new(),
        }
    }

    /// AH=48h allocation honouring the AH=58h strategy and the UMB link state.
    /// Bits 6-7 of the strategy pick the area: 01 high (upper memory only), 10
    /// high-then-low (upper first, then conventional). Low (00), an unlinked arena,
    /// or no upper memory all allocate from conventional memory.
    fn allocate_strategy(
        &mut self,
        paras: u16,
        mem: &mut Memory,
    ) -> Result<Result<u16, u16>, DosError> {
        allocate_strategy(
            &mut self.arena,
            self.umb,
            self.umb_link,
            self.alloc_strategy,
            paras,
            mem,
        )
    }

    /// AH=49h free routed to the arena that owns `seg`: the upper-memory arena when
    /// the segment falls in its window, the conventional arena otherwise.
    fn free_routed(&mut self, seg: u16, mem: &mut Memory) -> Result<Result<(), ()>, DosError> {
        free_routed(&mut self.arena, self.umb, seg, mem)
    }

    /// AH=4Ah resize routed to the arena that owns `seg` (see `free_routed`).
    fn resize_routed(
        &mut self,
        seg: u16,
        paras: u16,
        mem: &mut Memory,
    ) -> Result<Result<(), ResizeError>, DosError> {
        resize_routed(&mut self.arena, self.umb, seg, paras, mem)
    }

    /// Whether an upper-memory-block arena is furnished (the EMM386 manager is
    /// loaded with UMBs). The XMS UMB calls and the AH=5803h link gate on this.
    pub fn has_umb_arena(&self) -> bool {
        self.umb.is_some()
    }

    /// Set the AH=5803h UMB link state. The machine calls this at SYSINIT to link
    /// the arena when CONFIG.SYS carries DOS=UMB, so a default box comes up linked.
    pub fn set_umb_link(&mut self, linked: bool) {
        self.umb_link = linked;
    }

    /// Record whether CONFIG.SYS carried DOS=UMB, which gates whether AH=5803h may
    /// link the upper area at all. The machine sets this at SYSINIT.
    pub fn set_dos_umb(&mut self, configured: bool) {
        self.dos_umb = configured;
    }

    /// Record CONFIG.SYS LASTDRIVE=. AH=52h publishes this in the SysVars table
    /// as a count, with A: = 1 and Z: = 26.
    pub fn set_lastdrive(&mut self, lastdrive: u8) {
        self.lastdrive = Some(lastdrive);
    }

    /// Record CONFIG.SYS FILES= and BUFFERS=. FILES= gates dynamic handle
    /// allocation immediately; BUFFERS= is stored for the future disk-buffer chain.
    pub fn set_config_sys_counts(&mut self, files: u16, buffers: u16) {
        self.file_count = Some(files);
        self.buffer_count = Some(buffers);
    }

    /// XMS function 10h Request UMB: carve `paras` paragraphs from the SAME
    /// upper-memory MCB chain the AH=48h-high path uses, so the two never hand out
    /// the same paragraph. Ok(Ok(segment)), Ok(Err(largest data paras / 0 when the
    /// pool is full)), Err(DosError) on a memory fault. Independent of the AH=5803h
    /// link: XMS Request UMB is the manager primitive, available whenever the pool
    /// exists.
    pub fn request_umb(
        &mut self,
        paras: u16,
        mem: &mut Memory,
    ) -> Result<Result<u16, u16>, DosError> {
        request_umb(self.umb, paras, mem)
    }

    /// XMS function 11h Release UMB: free the upper-memory block whose segment is
    /// `seg`. Ok(Ok(())), or Ok(Err(())) when `seg` is not a UMB block.
    pub fn release_umb(&mut self, seg: u16, mem: &mut Memory) -> Result<Result<(), ()>, DosError> {
        release_umb(self.umb, seg, mem)
    }

    /// XMS function 12h Reallocate UMB: resize the upper-memory block at `seg` to
    /// `paras`. Ok(Ok(())); Ok(Err(Some(largest))) when a grow does not fit (the
    /// caller maps it to B0h with the largest size); Ok(Err(None)) when `seg` is not
    /// a UMB block (B2h).
    pub fn resize_umb(
        &mut self,
        seg: u16,
        paras: u16,
        mem: &mut Memory,
    ) -> Result<Result<(), Option<u16>>, DosError> {
        resize_umb(self.umb, seg, paras, mem)
    }

    /// Stand up a system PSP, arena, and base environment with no running
    /// program, so a boot stub can EXEC the shell as the first process. This is
    /// the SYSINIT-equivalent: it gives the first `AH=4Bh` a valid parent context
    /// (a PSP at `psp_seg` owning just its own paragraphs, an arena up to
    /// `ARENA_TOP`, and an environment block named by `PSP:0x2C`).
    pub fn init_shell_base(
        &mut self,
        mem: &mut Memory,
        psp_seg: u16,
        env: &[(&str, &str)],
    ) -> Result<(), DosError> {
        // SYSINIT resets the allocation-manager state: a warm reboot comes back
        // with UMBs unlinked and the low-first allocation strategy, not the prior
        // session's AH=58h settings. The upper arena's chain is re-laid separately
        // by the machine's furnish step (set_umb_region) on the same boot.
        self.umb_link = false;
        self.alloc_strategy = 0;
        build_psp(mem, psp_seg, ARENA_TOP)?;
        let prog_top = psp_seg.saturating_add(0x10); // the system PSP is its 256 bytes
        self.init_program(psp_seg, prog_top, mem)?;
        self.install_environment(mem, env)?;
        Ok(())
    }

    /// Allocate the DOS environment segment, write the env block in the real DOS
    /// format, and record its segment in `PSP:0x2C`. Each entry becomes an ASCIIZ
    /// `KEY=VALUE` string; the block ends with the empty-string terminator, then
    /// the DOS 3.0+ argv0 trailer (a WORD count of 0x0001 and the program's
    /// ASCIIZ full path). The segment is allocated in whole paragraphs above the
    /// program block via the arena, so it sits where real DOS places it and a
    /// guest `AH=49h`/`AH=4Ah` around it behaves as on real hardware. The machine
    /// calls this from `new_dos_program` after `init_program`. With no entries a
    /// valid (empty) environment is still allocated so `PSP:0x2C` is always a
    /// live pointer.
    pub fn install_environment(
        &mut self,
        mem: &mut Memory,
        entries: &[(&str, &str)],
    ) -> Result<(), DosError> {
        let block = build_env_block_with_argv0(entries, DEFAULT_ARGV0);
        let paras = u16::try_from(block.len().div_ceil(16)).unwrap_or(u16::MAX);
        let psp = self.arena.psp_seg;
        let psp_base = usize::from(psp) * 16;
        // The program block may have claimed all of conventional memory (an .EXE
        // with a large e_maxalloc sets PSP:0x02 = ARENA_TOP). Carve env room out
        // of the top of the program block, mirroring real DOS, which sizes the
        // program block AFTER reserving the environment; PSP:0x02 tracks the
        // reduced top. For a .COM (PSP:0x02 = segment + 0x1000) there is already
        // ample room above the program, so no shrink happens. The env allocation
        // also reserves a one-paragraph MCB header, so leave room for paras+1.
        let limit = ARENA_TOP.saturating_sub(paras.saturating_add(1));
        if self.arena.prog_top(mem) > limit {
            // saturating: a pathological env larger than conventional memory would
            // drive limit below psp; the resize then fails cleanly as EnvSegmentFull.
            self.arena
                .resize(psp, limit.saturating_sub(psp), mem)?
                .map_err(|_| DosError::EnvSegmentFull)?;
            mem.write_u16(psp_base + 0x02, limit)?;
        }
        let env_seg = match self.arena.allocate(paras, mem)? {
            Ok(seg) => seg,
            Err(_) => return Err(DosError::EnvSegmentFull),
        };
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
        Ok(self.resolve_name(&name))
    }

    /// Resolve a DOS filename (drive-qualified, absolute, or relative to the
    /// current directory) to a host path under the mounted C: drive, or a DOS
    /// error code (0x02 no drive).
    fn resolve_name(&self, name: &str) -> Result<PathBuf, u16> {
        let Some(drive) = self.drive.as_ref() else {
            return Err(0x02);
        };
        // A drive letter other than C: names a drive that is not mounted.
        let bytes = name.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' && !bytes[0].eq_ignore_ascii_case(&b'C') {
            return Err(0x03);
        }
        let absolute = self.absolute_dos_path(name);
        let mut host = drive.root().to_path_buf();
        for component in absolute.split('\\').filter(|c| !c.is_empty()) {
            host.push(component);
        }
        Ok(host)
    }

    /// Fold a DOS filename and the current directory into an absolute path from
    /// the root, resolving `.` and `..`. The result has no leading backslash
    /// ("" is the root), the same format the current directory is stored in. A
    /// `..` at the root is ignored, so a guest cannot escape the mounted drive.
    fn absolute_dos_path(&self, name: &str) -> String {
        let after_drive = name
            .strip_prefix("C:")
            .or_else(|| name.strip_prefix("c:"))
            .unwrap_or(name);
        let mut components: Vec<&str> = Vec::new();
        if !after_drive.starts_with(['\\', '/']) {
            components.extend(self.cwd.split('\\').filter(|c| !c.is_empty()));
        }
        for part in after_drive.split(['\\', '/']).filter(|c| !c.is_empty()) {
            match part {
                "." => {}
                ".." => {
                    components.pop();
                }
                other => components.push(other),
            }
        }
        components.join("\\")
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

    /// Build the child environment block. env_source 0 -> inherit the caller's
    /// environment: copy the string region of the block named by the current
    /// PSP's 0x2C (RBIL INT 21/AH=4Bh EPB word 0: 0 = copy the calling
    /// process's environment); a caller with no env (0x2C == 0) yields an empty
    /// block (a single terminating NUL). Non-zero -> copy that source block's
    /// string region (ASCIIZ strings up to the terminating empty string), capped
    /// at 32 KiB; no terminator within the cap -> Err(0x0A). Only the string
    /// region is copied, not the optional count + program-name suffix (marked).
    fn child_environment(
        &self,
        mem: &Memory,
        mut env_source: u16,
    ) -> Result<Result<Vec<u8>, u16>, DosError> {
        if env_source == 0 {
            // Inherit: the env block of the current (during EXEC: parent) PSP.
            env_source = mem.read_u16(usize::from(self.arena.psp_seg) * 16 + 0x2c)?;
            if env_source == 0 {
                return Ok(Ok(vec![0x00]));
            }
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
        // Carve the child's blocks from the parent's free tail as real owned MCBs.
        // The environment block heads the child's chain: its MCB header sits at the
        // parent free base, env data one paragraph up. The child program block's
        // header sits just above the env data, its PSP one paragraph higher again.
        let env_mcb = self.arena.free_base(mem);
        // The child needs at least a 64 KiB program segment (load_com sets SP=0xFFFE
        // and writes the return word there). Too little conventional memory left is
        // insufficient memory (0x08), reported before any child memory is written.
        let child_psp = match env_mcb.checked_add(env_paras + 2) {
            Some(s) if u32::from(s) + 0x1000 <= u32::from(ARENA_TOP) => s,
            _ => {
                set_dos_error(regs, 0x08);
                return Ok(DosAction::Continue);
            }
        };
        let env_seg = env_mcb + 1; // PSP:0x2C points at the env data, above its header

        // Load the child image FIRST: a failed load must leave the parent's chain
        // untouched (no env header written over the parent's free tail to roll back).
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

        // The load succeeded: now write the environment MCB block (owner = the child
        // PSP) over the parent's old free tail, and the env bytes into its data area.
        write_env_mcb(mem, env_mcb, child_psp, env_paras)?;
        let env_linear = usize::from(env_seg) * 16;
        for (i, &byte) in env_bytes.iter().enumerate() {
            mem.write_u8(env_linear + i, byte)?;
        }

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

        // Save the parent context (its free tail, to restore when the child's blocks
        // are freed on exit), then switch to the child.
        let parent = ProgramContext {
            arena: std::mem::take(&mut self.arena),
            dta: self.dta,
            find_searches: std::mem::take(&mut self.find_searches),
            free_base: env_mcb,
        };
        self.program_stack.push(parent);
        self.arena = Arena {
            psp_seg: child_psp,
            chain_first: env_mcb, // the chain starts at the env block
            resident: false,
        };
        // Write the child program block: owned by the child PSP, the last block,
        // filling conventional memory to the top. Its header sits just above the env
        // data, so the child chain reads env -> program -> (free tail once shrunk).
        write_child_program_mcb(mem, child_psp)?;
        self.dta = (child_psp, 0x80);
        // A fresh child has terminated no child of its own.
        self.last_exit_code = 0;
        self.last_exit_type = 0;

        Ok(DosAction::Exec { entry, child_ax })
    }

    /// Restore the parent program's DOS state after a child exits with `code`,
    /// and record the exit code/type for AH=4Dh. Called by the machine when it
    /// pops a parent frame.
    pub fn finish_exec(&mut self, code: u8, mem: &mut Memory) -> Result<(), DosError> {
        // The exiting child's blocks (env + program, above the parent free base) are
        // freed back to the parent, UNLESS the child itself kept resident (a TSR), in
        // which case keep_resident already left a correct free tail above its block.
        //
        // Upper-memory (UMB) blocks a child allocated with a high strategy are NOT
        // reclaimed here, so they leak past the child (marked): the conventional
        // reclaim is positional (it resets the parent free tail), and AH=48h blocks
        // carry their own segment as the owner rather than the PSP, so there is no
        // owner key to sweep the upper arena by. Real DOS frees a process's upper
        // memory on exit; an owner-keyed sweep waits on the owner=PSP convention.
        // In practice high allocators are TSRs that stay resident and keep theirs.
        let child_resident = self.arena.resident;
        if let Some(parent) = self.program_stack.pop() {
            self.arena = parent.arena;
            self.dta = parent.dta;
            self.find_searches = parent.find_searches;
            if !child_resident {
                // A resident TSR carved by this child or a descendant sits above the
                // freed region; cap the restored free block below the lowest such
                // resident base so the TSR is preserved (the EXEC chain unwinds past
                // it). With nothing resident above, the region reaches ARENA_TOP.
                let cap = self
                    .resident_regions
                    .iter()
                    .copied()
                    .filter(|&base| base > parent.free_base)
                    .min()
                    .unwrap_or(ARENA_TOP);
                if parent.free_base < cap {
                    write_free_mcb_to_cap(mem, parent.free_base, cap)?;
                }
            }
        }
        self.last_exit_code = code;
        self.last_exit_type = 0x00; // only normal termination is modeled (marked).
        Ok(())
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

    /// Resolve the file named by the FCB at DS:DX to a host path. The FCB 8.3
    /// fields name the file relative to the C: root (no path, like the FCB API).
    /// Ok(Ok(path)) on success; Ok(Err(())) when the FCB names no resolvable file
    /// (every FCB error is the single 0xFF return, so a code is not needed).
    fn fcb_path(&self, mem: &Memory, regs: &DosRegs) -> Result<Result<PathBuf, ()>, DosError> {
        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
        let name = fcb_name(mem, base, FCB_NAME)?;
        if name.is_empty() {
            return Ok(Err(()));
        }
        Ok(self.resolve_name(&name).map_err(|_| ()))
    }

    /// AH=0Fh OPEN FCB / AH=16h CREATE FCB shared body. `create` truncates or
    /// makes the file; otherwise the file must already exist. On success the FCB
    /// record-size (128), current block, current record, file size, and date/time
    /// fields are filled and AL=00; on any failure AL=0xFF.
    fn fcb_open_or_create(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
        create: bool,
    ) -> Result<DosAction, DosError> {
        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
        let path = match self.fcb_path(mem, regs)? {
            Ok(path) => path,
            Err(()) => {
                regs.ax = (regs.ax & 0xff00) | 0xff;
                return Ok(DosAction::Continue);
            }
        };
        let open = if create {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)
        } else {
            OpenOptions::new().read(true).write(true).open(&path)
        };
        let Ok(file) = open else {
            regs.ax = (regs.ax & 0xff00) | 0xff;
            return Ok(DosAction::Continue);
        };
        // Echo the opened 8.3 name back and seed the documented fields.
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            fcb_set_name(mem, base, name)?;
        }
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let (time, date) = file
            .metadata()
            .and_then(|m| m.modified())
            .map(dos_time_date)
            .unwrap_or((0, (1 << 5) | 1));
        mem.write_u16(base + FCB_BLOCK, 0)?;
        mem.write_u16(base + FCB_RECSIZE, 128)?;
        mem.write_u32(base + FCB_FILESIZE, size as u32)?;
        mem.write_u16(base + FCB_DATE, date)?;
        mem.write_u16(base + FCB_TIME, time)?;
        mem.write_u8(base + FCB_CURREC, 0)?;
        mem.write_u32(base + FCB_RANDREC, 0)?;
        regs.ax &= 0xff00; // AL = 00 success
        Ok(DosAction::Continue)
    }

    /// AH=10h CLOSE FCB. The HLE opens the host file per record op, so there is no
    /// buffered handle to flush; the FCB is left as is. AL=00 when the FCB names a
    /// resolvable file, 0xFF otherwise.
    fn fcb_close(&self, mem: &Memory, regs: &mut DosRegs) -> Result<DosAction, DosError> {
        let al = match self.fcb_path(mem, regs)? {
            Ok(_) => 0x00,
            Err(()) => 0xff,
        };
        regs.ax = (regs.ax & 0xff00) | al;
        Ok(DosAction::Continue)
    }

    /// AH=13h DELETE FCB by name, wildcards allowed. Deletes every matching file
    /// in the C: root. AL=00 if at least one file was deleted, 0xFF otherwise.
    fn fcb_delete(&self, mem: &Memory, regs: &mut DosRegs) -> Result<DosAction, DosError> {
        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
        let name = fcb_name(mem, base, FCB_NAME)?;
        let Some(drive) = self.drive.as_ref() else {
            regs.ax = (regs.ax & 0xff00) | 0xff;
            return Ok(DosAction::Continue);
        };
        let pattern = pattern_to_8_3(&name.to_ascii_uppercase());
        let mut deleted = false;
        if let Ok(read_dir) = std::fs::read_dir(drive.root()) {
            for dirent in read_dir.flatten() {
                let raw = dirent.file_name();
                let Some(host) = raw.to_str() else { continue };
                let Some(template) = host_name_to_8_3(host) else {
                    continue;
                };
                if template_matches(&template, &pattern)
                    && std::fs::remove_file(dirent.path()).is_ok()
                {
                    deleted = true;
                }
            }
        }
        regs.ax = (regs.ax & 0xff00) | if deleted { 0x00 } else { 0xff };
        Ok(DosAction::Continue)
    }

    /// AH=11h FIND FIRST using an FCB. The search FCB at DS:DX holds an 8.3 name
    /// with '?'/'*' wildcards. Enumerate C:, snapshot the matches, write the first
    /// as a directory entry into the DTA, and keep the cursor for AH=12h. AL=00h
    /// found, 0xFFh on no match or no drive. An extended FCB (0xFF prefix) carries
    /// a search attribute so directories, hidden, and system entries can be
    /// returned; a normal FCB returns normal files only. Volume-label search
    /// (attribute 0x08) is not modeled, the HLE having no volume label.
    fn fcb_find_first(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
        let (name_base, search_attr, extended) = fcb_search_header(mem, base)?;
        let name = fcb_name(mem, name_base, FCB_NAME)?;
        let Some(drive) = self.drive.as_ref() else {
            regs.ax = (regs.ax & 0xff00) | 0xff;
            return Ok(DosAction::Continue);
        };
        let pattern = pattern_to_8_3(&name.to_ascii_uppercase());
        let mut entries = Vec::new();
        if let Ok(read_dir) = std::fs::read_dir(drive.root()) {
            for dirent in read_dir.flatten() {
                let raw = dirent.file_name();
                let Some(host) = raw.to_str() else { continue };
                let Some(template) = host_name_to_8_3(host) else {
                    continue;
                };
                if !template_matches(&template, &pattern) {
                    continue;
                }
                let Ok(metadata) = dirent.metadata() else {
                    continue;
                };
                let attr = if metadata.is_dir() { 0x10 } else { 0x00 };
                // A normal FCB (search attribute 0) returns only normal files; an
                // extended FCB's attribute also admits directories, hidden, etc.
                if !attr_matches(attr, search_attr) {
                    continue;
                }
                let (time, date) =
                    dos_time_date(metadata.modified().unwrap_or(std::time::UNIX_EPOCH));
                entries.push(FindEntry {
                    attr,
                    time,
                    date,
                    size: metadata.len() as u32,
                    name: host.to_ascii_uppercase(),
                });
            }
        }
        let Some(first) = entries.first().cloned() else {
            regs.ax = (regs.ax & 0xff00) | 0xff;
            return Ok(DosAction::Continue);
        };
        write_fcb_find_record(mem, self.dta, &first, extended)?;
        // The cursor is keyed by the DTA, the same map the handle find (AH=4Eh/4Fh)
        // uses. Real DOS keeps an FCB search's cursor in the search FCB at DS:DX, so
        // a guest that moves the DTA between AH=11h and AH=12h, or interleaves a
        // handle find at the same DTA, is not honored (a documented HLE limitation).
        self.find_searches
            .insert(self.dta, FindSearch { entries, next: 1 });
        regs.ax &= 0xff00; // AL=00h found
        Ok(DosAction::Continue)
    }

    /// AH=12h FIND NEXT using an FCB. Continue the search keyed by the current DTA,
    /// writing the next directory entry in the normal or extended result format
    /// (re-read from the unchanged search FCB's 0xFF prefix). AL=00h, or 0xFFh
    /// when exhausted.
    fn fcb_find_next(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let dta = self.dta;
        // The search FCB is unchanged across the find (RBIL), so re-read whether it
        // is extended to choose the result format.
        let extended = mem.read_u8(usize::from(regs.ds) * 16 + usize::from(regs.dx))? == 0xff;
        let Some(search) = self.find_searches.get_mut(&dta) else {
            regs.ax = (regs.ax & 0xff00) | 0xff;
            return Ok(DosAction::Continue);
        };
        let Some(entry) = search.entries.get(search.next).cloned() else {
            self.find_searches.remove(&dta);
            regs.ax = (regs.ax & 0xff00) | 0xff;
            return Ok(DosAction::Continue);
        };
        search.next += 1;
        write_fcb_find_record(mem, dta, &entry, extended)?;
        regs.ax &= 0xff00; // AL=00h
        Ok(DosAction::Continue)
    }

    /// AH=17h RENAME FCB. The FCB at DS:DX holds the old 8.3 name at 0x01 and the
    /// new 8.3 name at 0x11. AL=00 on success, 0xFF on a missing source or host
    /// error. Wildcards in the names are not expanded (marked); the common case is
    /// a literal rename.
    fn fcb_rename(&self, mem: &Memory, regs: &mut DosRegs) -> Result<DosAction, DosError> {
        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
        let old_name = fcb_name(mem, base, FCB_NAME)?;
        let new_name = fcb_name(mem, base, FCB_RENAME_NEW)?;
        let al = match (self.resolve_name(&old_name), self.resolve_name(&new_name)) {
            (Ok(old), Ok(new)) if std::fs::rename(&old, &new).is_ok() => 0x00,
            _ => 0xff,
        };
        regs.ax = (regs.ax & 0xff00) | al;
        Ok(DosAction::Continue)
    }

    /// AH=14h SEQUENTIAL READ. Read one record (FCB record size) from the file
    /// position the current block/record select into the DTA, then advance the
    /// record number. AL=00 read in full, 01 EOF (no data), 03 a partial record
    /// (the last record, zero-padded into the DTA).
    fn fcb_seq_read(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
        let path = match self.fcb_path(mem, regs)? {
            Ok(path) => path,
            Err(()) => {
                regs.ax = (regs.ax & 0xff00) | 0xff;
                return Ok(DosAction::Continue);
            }
        };
        let record_size = mem.read_u16(base + FCB_RECSIZE)?;
        let block = mem.read_u16(base + FCB_BLOCK)?;
        let current = mem.read_u8(base + FCB_CURREC)?;
        let pos = fcb_seq_position(block, current, record_size);
        let size = if record_size == 0 { 128 } else { record_size };
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(_) => {
                regs.ax = (regs.ax & 0xff00) | 0xff;
                return Ok(DosAction::Continue);
            }
        };
        let mut buffer = vec![0u8; usize::from(size)];
        let filled = read_at(&mut file, pos, &mut buffer)?;
        let al = if filled == 0 {
            // At or past EOF: no data, and DOS leaves the DTA untouched.
            0x01
        } else {
            let dta = usize::from(self.dta.0) * 16 + usize::from(self.dta.1);
            for (i, &byte) in buffer.iter().enumerate() {
                mem.write_u8(dta + i, byte)?;
            }
            fcb_advance_record(mem, base, block, current)?;
            if filled < usize::from(size) {
                0x03 // a partial final record
            } else {
                0x00 // a full record
            }
        };
        regs.ax = (regs.ax & 0xff00) | al;
        Ok(DosAction::Continue)
    }

    /// AH=15h SEQUENTIAL WRITE. Write one record (FCB record size) from the DTA to
    /// the file position the current block/record select, then advance the record
    /// number. AL=00 on success, 0xFF on a host error.
    fn fcb_seq_write(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
        let path = match self.fcb_path(mem, regs)? {
            Ok(path) => path,
            Err(()) => {
                regs.ax = (regs.ax & 0xff00) | 0xff;
                return Ok(DosAction::Continue);
            }
        };
        let record_size = mem.read_u16(base + FCB_RECSIZE)?;
        let block = mem.read_u16(base + FCB_BLOCK)?;
        let current = mem.read_u8(base + FCB_CURREC)?;
        let pos = fcb_seq_position(block, current, record_size);
        let size = if record_size == 0 { 128 } else { record_size };
        let dta = usize::from(self.dta.0) * 16 + usize::from(self.dta.1);
        let mut record = vec![0u8; usize::from(size)];
        for (i, slot) in record.iter_mut().enumerate() {
            *slot = mem.read_u8(dta + i)?;
        }
        let mut file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => f,
            Err(_) => {
                regs.ax = (regs.ax & 0xff00) | 0xff;
                return Ok(DosAction::Continue);
            }
        };
        if file.seek(SeekFrom::Start(pos)).is_err() || file.write_all(&record).is_err() {
            regs.ax = (regs.ax & 0xff00) | 0xff;
            return Ok(DosAction::Continue);
        }
        // Keep the FCB file-size field current so a following AH=23h is accurate.
        if let Ok(meta) = file.metadata() {
            mem.write_u32(base + FCB_FILESIZE, meta.len() as u32)?;
        }
        fcb_advance_record(mem, base, block, current)?;
        regs.ax &= 0xff00; // AL = 00
        Ok(DosAction::Continue)
    }

    /// AH=23h GET FILE SIZE. Fill the FCB random-record field with the file size in
    /// records (rounded up by the FCB record size, defaulting to 128). AL=00 on
    /// success, 0xFF when the file does not resolve or exist.
    fn fcb_file_size(&self, mem: &mut Memory, regs: &mut DosRegs) -> Result<DosAction, DosError> {
        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
        let path = match self.fcb_path(mem, regs)? {
            Ok(path) => path,
            Err(()) => {
                regs.ax = (regs.ax & 0xff00) | 0xff;
                return Ok(DosAction::Continue);
            }
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            regs.ax = (regs.ax & 0xff00) | 0xff;
            return Ok(DosAction::Continue);
        };
        let record_size = match mem.read_u16(base + FCB_RECSIZE)? {
            0 => 128,
            n => n,
        };
        let records = meta.len().div_ceil(u64::from(record_size)) as u32;
        mem.write_u32(base + FCB_RANDREC, records)?;
        regs.ax &= 0xff00; // AL = 00
        Ok(DosAction::Continue)
    }

    /// AH=21h RANDOM READ. Read the single record the FCB random-record field at
    /// 0x21 selects into the DTA, leaving the random-record field unchanged but
    /// syncing the current block/record to it (RBIL: AH=21h sets the block/record
    /// from the random record). AL=00 read in full, 01 EOF (no data, DTA left
    /// untouched), 03 partial final record (zero-padded). 0xFF if the FCB names no
    /// resolvable file.
    fn fcb_random_read(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
        let path = match self.fcb_path(mem, regs)? {
            Ok(path) => path,
            Err(()) => {
                regs.ax = (regs.ax & 0xff00) | 0xff;
                return Ok(DosAction::Continue);
            }
        };
        let record_size = mem.read_u16(base + FCB_RECSIZE)?;
        let random = mem.read_u32(base + FCB_RANDREC)?;
        fcb_sync_block_record_from_random(mem, base, random)?;
        let size = if record_size == 0 { 128 } else { record_size };
        let pos = u64::from(random) * u64::from(size);
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(_) => {
                regs.ax = (regs.ax & 0xff00) | 0xff;
                return Ok(DosAction::Continue);
            }
        };
        let mut buffer = vec![0u8; usize::from(size)];
        let filled = read_at(&mut file, pos, &mut buffer)?;
        let al = if filled == 0 {
            0x01 // at or past EOF: no data, DTA left as is (matches AH=14h)
        } else {
            let dta = usize::from(self.dta.0) * 16 + usize::from(self.dta.1);
            for (i, &byte) in buffer.iter().enumerate() {
                mem.write_u8(dta + i, byte)?;
            }
            if filled < usize::from(size) {
                0x03
            } else {
                0x00
            }
        };
        regs.ax = (regs.ax & 0xff00) | al;
        Ok(DosAction::Continue)
    }

    /// AH=22h RANDOM WRITE. Write the single record at the DTA to the position the
    /// FCB random-record field selects. The current block/record sync to the random
    /// record. AL=00 success, 0xFF on a host error or an unresolvable FCB.
    fn fcb_random_write(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
        let path = match self.fcb_path(mem, regs)? {
            Ok(path) => path,
            Err(()) => {
                regs.ax = (regs.ax & 0xff00) | 0xff;
                return Ok(DosAction::Continue);
            }
        };
        let record_size = mem.read_u16(base + FCB_RECSIZE)?;
        let random = mem.read_u32(base + FCB_RANDREC)?;
        fcb_sync_block_record_from_random(mem, base, random)?;
        let size = if record_size == 0 { 128 } else { record_size };
        let pos = u64::from(random) * u64::from(size);
        let dta = usize::from(self.dta.0) * 16 + usize::from(self.dta.1);
        let mut record = vec![0u8; usize::from(size)];
        for (i, slot) in record.iter_mut().enumerate() {
            *slot = mem.read_u8(dta + i)?;
        }
        let mut file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => f,
            Err(_) => {
                regs.ax = (regs.ax & 0xff00) | 0xff;
                return Ok(DosAction::Continue);
            }
        };
        if file.seek(SeekFrom::Start(pos)).is_err() || file.write_all(&record).is_err() {
            regs.ax = (regs.ax & 0xff00) | 0xff;
            return Ok(DosAction::Continue);
        }
        if let Ok(meta) = file.metadata() {
            mem.write_u32(base + FCB_FILESIZE, meta.len() as u32)?;
        }
        regs.ax &= 0xff00; // AL = 00
        Ok(DosAction::Continue)
    }

    /// AH=24h SET RANDOM RECORD. Compute the FCB random-record field from the
    /// current block and record: random = block * 128 + current-record. No file
    /// access; this is pure FCB field math. AL is undocumented and left as is.
    fn fcb_set_random(&self, mem: &mut Memory, regs: &mut DosRegs) -> Result<DosAction, DosError> {
        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
        let block = mem.read_u16(base + FCB_BLOCK)?;
        let current = mem.read_u8(base + FCB_CURREC)?;
        let random = u32::from(block) * 128 + u32::from(current);
        mem.write_u32(base + FCB_RANDREC, random)?;
        Ok(DosAction::Continue)
    }

    /// AH=27h RANDOM BLOCK READ. Read CX records starting at the random record into
    /// the DTA, packed back to back. CX returns the count actually read; the random
    /// record and the block/record cursor advance past the last record. AL=00 all
    /// records read, 01 EOF reached mid-block (a clean stop on a record boundary),
    /// 03 a partial final record (zero-padded). 0xFF if the FCB does not resolve.
    fn fcb_random_block_read(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
        let path = match self.fcb_path(mem, regs)? {
            Ok(path) => path,
            Err(()) => {
                regs.ax = (regs.ax & 0xff00) | 0xff;
                return Ok(DosAction::Continue);
            }
        };
        let record_size = mem.read_u16(base + FCB_RECSIZE)?;
        let size = if record_size == 0 { 128 } else { record_size };
        let start = mem.read_u32(base + FCB_RANDREC)?;
        let wanted = regs.cx;
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(_) => {
                regs.ax = (regs.ax & 0xff00) | 0xff;
                return Ok(DosAction::Continue);
            }
        };
        let dta = usize::from(self.dta.0) * 16 + usize::from(self.dta.1);
        let mut read = 0u16;
        let mut al = 0x00u8;
        for index in 0..wanted {
            let record = u64::from(start) + u64::from(index);
            let pos = record * u64::from(size);
            let mut buffer = vec![0u8; usize::from(size)];
            let filled = read_at(&mut file, pos, &mut buffer)?;
            if filled == 0 {
                al = 0x01; // EOF on a record boundary, no partial record
                break;
            }
            let target = dta + usize::from(read) * usize::from(size);
            for (i, &byte) in buffer.iter().enumerate() {
                mem.write_u8(target + i, byte)?;
            }
            read += 1;
            if filled < usize::from(size) {
                al = 0x03; // partial final record, counted in CX
                break;
            }
        }
        regs.cx = read;
        // Advance the random record and the block/record cursor past what was read.
        let next = start + u32::from(read);
        mem.write_u32(base + FCB_RANDREC, next)?;
        fcb_sync_block_record_from_random(mem, base, next)?;
        regs.ax = (regs.ax & 0xff00) | u16::from(al);
        Ok(DosAction::Continue)
    }

    /// AH=28h RANDOM BLOCK WRITE. Write CX records from the DTA starting at the
    /// random record. The documented quirk: CX=0 sets the file size (truncates or
    /// extends) to the random record without writing data. CX returns the count
    /// written; the random record and block/record cursor advance. AL=00 success,
    /// 0xFF on a host error or unresolvable FCB.
    fn fcb_random_block_write(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
        let path = match self.fcb_path(mem, regs)? {
            Ok(path) => path,
            Err(()) => {
                regs.ax = (regs.ax & 0xff00) | 0xff;
                return Ok(DosAction::Continue);
            }
        };
        let record_size = mem.read_u16(base + FCB_RECSIZE)?;
        let size = if record_size == 0 { 128 } else { record_size };
        let start = mem.read_u32(base + FCB_RANDREC)?;
        let wanted = regs.cx;
        let mut file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => f,
            Err(_) => {
                regs.ax = (regs.ax & 0xff00) | 0xff;
                return Ok(DosAction::Continue);
            }
        };
        if wanted == 0 {
            // CX=0: set the file size to start*record-size, no record transfer.
            let len = u64::from(start) * u64::from(size);
            if file.set_len(len).is_err() {
                regs.ax = (regs.ax & 0xff00) | 0xff;
                return Ok(DosAction::Continue);
            }
            mem.write_u32(base + FCB_FILESIZE, len as u32)?;
            regs.ax &= 0xff00; // AL = 00
            return Ok(DosAction::Continue);
        }
        let dta = usize::from(self.dta.0) * 16 + usize::from(self.dta.1);
        for index in 0..wanted {
            let record = u64::from(start) + u64::from(index);
            let pos = record * u64::from(size);
            let mut buffer = vec![0u8; usize::from(size)];
            let source = dta + usize::from(index) * usize::from(size);
            for (i, slot) in buffer.iter_mut().enumerate() {
                *slot = mem.read_u8(source + i)?;
            }
            if file.seek(SeekFrom::Start(pos)).is_err() || file.write_all(&buffer).is_err() {
                regs.cx = index;
                regs.ax = (regs.ax & 0xff00) | 0xff;
                return Ok(DosAction::Continue);
            }
        }
        regs.cx = wanted;
        if let Ok(meta) = file.metadata() {
            mem.write_u32(base + FCB_FILESIZE, meta.len() as u32)?;
        }
        let next = start + u32::from(wanted);
        mem.write_u32(base + FCB_RANDREC, next)?;
        fcb_sync_block_record_from_random(mem, base, next)?;
        regs.ax &= 0xff00; // AL = 00
        Ok(DosAction::Continue)
    }

    /// AH=29h PARSE FILENAME. Parse the command-line filename at DS:SI into the FCB
    /// at ES:DI, honoring the AL option bits, then return AL: 0 no wildcards, 1 the
    /// name held a '*' or '?', 0xFF the parsed drive was invalid. SI advances past
    /// the parsed text. The DOS option bits (RBIL #01172):
    ///   bit0: scan past leading separators before the name
    ///   bit1: keep the FCB drive byte unless a drive is given (else set it)
    ///   bit2: keep the FCB name unless a name is given (else set it)
    ///   bit3: keep the FCB ext unless an ext is given (else set it)
    fn fcb_parse_filename(
        &self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let opts = regs.ax as u8;
        let src = usize::from(regs.ds) * 16 + usize::from(regs.si);
        let fcb = usize::from(regs.es) * 16 + usize::from(regs.di);
        // Read a bounded run of the source so the parse cannot wander off; a real
        // filename plus separators is far under this. Limit: a fixed 64-byte
        // window, not a true scan-to-CR; command tails parsed here are short.
        let mut text = Vec::with_capacity(64);
        for i in 0..64 {
            text.push(mem.read_u8(src + i)?);
        }
        let parsed = parse_fcb_filename(&text, opts);
        // Advance SI past the bytes the parser consumed.
        regs.si = regs.si.wrapping_add(parsed.consumed as u16);
        if parsed.invalid_drive {
            regs.ax = (regs.ax & 0xff00) | 0xff;
            return Ok(DosAction::Continue);
        }
        if let Some(drive) = parsed.drive {
            mem.write_u8(fcb, drive)?;
        } else if opts & 0x02 == 0 {
            mem.write_u8(fcb, 0)?; // default drive
        }
        if let Some(name) = parsed.name {
            for (i, &b) in name.iter().enumerate() {
                mem.write_u8(fcb + FCB_NAME + i, b)?;
            }
        } else if opts & 0x04 == 0 {
            for i in 0..8 {
                mem.write_u8(fcb + FCB_NAME + i, b' ')?;
            }
        }
        if let Some(ext) = parsed.ext {
            for (i, &b) in ext.iter().enumerate() {
                mem.write_u8(fcb + FCB_EXT + i, b)?;
            }
        } else if opts & 0x08 == 0 {
            for i in 0..3 {
                mem.write_u8(fcb + FCB_EXT + i, b' ')?;
            }
        }
        let al = if parsed.wildcards { 0x01 } else { 0x00 };
        regs.ax = (regs.ax & 0xff00) | al;
        Ok(DosAction::Continue)
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
            0x21 => {
                let action = self.dispatch_int21(regs, mem)?;
                // Any INT 21h call returning with carry set has placed its DOS
                // error code in AX. Record it here so a later AH=59h reports the
                // most recent failure, covering every set_dos_error site, not just
                // the handlers that route through fail().
                if regs.cf {
                    self.last_error = regs.ax;
                }
                Ok(action)
            }
            // The machine only records 0x10/0x20/0x21 and routes 0x10 elsewhere, so
            // this is unreachable today. Treat it as a no-op rather than panic.
            _ => Ok(DosAction::Continue),
        }
    }

    /// Pull the next byte for cooked single-character input (AH=01/06/07/08 and the
    /// AH=0Ch forms). Returns the byte to place in AL and whether it is half of an
    /// extended-key sequence (the 0x00 lead byte or the trailing scancode), which
    /// callers must not echo. Extended/function keys arrive on the ring as a
    /// (scancode, 0) pair and are delivered as 0x00 then the scancode on the next
    /// call, the INT 16h two-byte convention DOS forwards through INT 21h. None
    /// means the ring is empty.
    fn next_cooked_char(&mut self, mem: &mut Memory) -> Result<Option<(u8, bool)>, DosError> {
        if let Some(scancode) = self.pending_scancode.take() {
            return Ok(Some((scancode, true)));
        }
        match kbd_ring_dequeue(mem)? {
            // A real key with a zero ascii is an extended/function key. The keyboard
            // BIOS never enqueues an all-zero word, so a non-zero scancode is implied.
            Some((scancode, 0)) => {
                self.pending_scancode = Some(scancode);
                Ok(Some((0, true)))
            }
            Some((_, ascii)) => Ok(Some((ascii, false))),
            None => Ok(None),
        }
    }

    /// Read one character from the keyboard ring. Some -> set AL (and echo when
    /// asked) and Continue; None -> WaitForKey so the caller re-runs the INT.
    fn read_char(
        &mut self,
        regs: &mut DosRegs,
        mem: &mut Memory,
        echo: bool,
    ) -> Result<DosAction, DosError> {
        match self.next_cooked_char(mem)? {
            Some((ch, extended)) => {
                // Divergence (marked): real DOS echoes the 0x00 lead byte of an
                // extended key and suppresses only the scancode. We suppress both,
                // so neither a NUL nor a raw scancode is ever pushed to stdout.
                if echo && !extended {
                    self.stdout.push(ch);
                }
                regs.ax = (regs.ax & 0xff00) | u16::from(ch);
                Ok(DosAction::Continue)
            }
            None => Ok(DosAction::WaitForKey),
        }
    }

    /// AH=0Ah buffered line input into the guest buffer at DS:DX. The buffer is
    /// [max_len, actual_len, chars...]. Blocks per character; the running count
    /// is held in `pending_line` keyed by the buffer address so it survives the
    /// WaitForKey re-entries.
    fn buffered_input(
        &mut self,
        regs: &mut DosRegs,
        mem: &mut Memory,
    ) -> Result<DosAction, DosError> {
        // A half-read extended key from a prior single-char call does not carry into
        // line input; drop the held scancode so it cannot leak into a later read.
        self.pending_scancode = None;
        let buf = usize::from(regs.ds) * 16 + usize::from(regs.dx);
        let addr = buf as u32;
        let max = mem.read_u8(buf)?;
        if max == 0 {
            mem.write_u8(buf + 1, 0)?;
            return Ok(DosAction::Continue);
        }
        let mut count = match self.pending_line {
            Some((a, c)) if a == addr => c,
            _ => 0,
        };
        loop {
            let Some((_, ascii)) = kbd_ring_dequeue(mem)? else {
                self.pending_line = Some((addr, count));
                return Ok(DosAction::WaitForKey);
            };
            match ascii {
                0x0d => {
                    mem.write_u8(buf + 2 + usize::from(count), 0x0d)?;
                    self.stdout.push(0x0d);
                    mem.write_u8(buf + 1, count)?;
                    self.pending_line = None;
                    return Ok(DosAction::Continue);
                }
                0x08 => {
                    if count > 0 {
                        count -= 1;
                        self.stdout.extend_from_slice(&[0x08, 0x20, 0x08]);
                    }
                }
                _ => {
                    if usize::from(count) + 1 < usize::from(max) {
                        mem.write_u8(buf + 2 + usize::from(count), ascii)?;
                        count += 1;
                        self.stdout.push(ascii);
                    } else {
                        self.stdout.push(0x07); // buffer full, bell
                    }
                }
            }
        }
    }

    /// Record a DOS error code for AH=59h, then set the standard CF/AX error
    /// return. The new (AH=59h-aware) handlers route their failures through this
    /// so the extended-error query has a value to report.
    fn fail(&mut self, regs: &mut DosRegs, code: u16) {
        self.last_error = code;
        set_dos_error(regs, code);
    }

    fn dispatch_int21(
        &mut self,
        regs: &mut DosRegs,
        mem: &mut Memory,
    ) -> Result<DosAction, DosError> {
        let ah = (regs.ax >> 8) as u8;
        match ah {
            // AH=01h: read one character with echo from the keyboard ring. An empty
            // ring blocks: the kernel returns WaitForKey and the machine re-runs the INT.
            0x01 => self.read_char(regs, mem, true),
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
                    match self.next_cooked_char(mem)? {
                        Some((ch, _)) => {
                            regs.ax = (regs.ax & 0xff00) | u16::from(ch);
                            regs.zf = false;
                        }
                        None => regs.zf = true,
                    }
                } else {
                    let ch = regs.dx as u8;
                    self.stdout.push(ch);
                    regs.ax = (regs.ax & 0xff00) | u16::from(ch);
                }
                Ok(DosAction::Continue)
            }
            // AH=08h: read one character without echo from the keyboard ring. An empty
            // ring blocks via WaitForKey, the same as AH=01h.
            0x08 => self.read_char(regs, mem, false),
            // AH=07h: read one character, no echo, no Ctrl-C check. Blocks.
            0x07 => self.read_char(regs, mem, false),
            // AH=0Ah: buffered line input into DS:DX. Blocks until CR.
            0x0a => self.buffered_input(regs, mem),
            // AH=0Bh: get input status. ZF set and AL=0 when empty, ZF clear and
            // AL=0xFF when a character is waiting. Does not consume the character.
            0x0b => {
                if self.pending_scancode.is_none() && kbd_ring_is_empty(mem)? {
                    regs.ax &= 0xff00;
                    regs.zf = true;
                } else {
                    regs.ax = (regs.ax & 0xff00) | 0xff;
                    regs.zf = false;
                }
                Ok(DosAction::Continue)
            }
            // AH=0Ch: flush the input buffer, then invoke the input function named
            // in AL (01/06/07/08). The flush happens once even across WaitForKey
            // re-entries, so a key that arrives while blocking is not discarded.
            0x0c => {
                if !self.cooked_flush_done {
                    kbd_ring_flush(mem)?;
                    self.pending_scancode = None;
                    self.cooked_flush_done = true;
                }
                let al = regs.ax as u8;
                let result = match al {
                    0x01 => self.read_char(regs, mem, true)?,
                    0x06 => {
                        if regs.dx as u8 == 0xff {
                            match self.next_cooked_char(mem)? {
                                Some((ch, _)) => {
                                    regs.ax = (regs.ax & 0xff00) | u16::from(ch);
                                    regs.zf = false;
                                }
                                None => regs.zf = true,
                            }
                        }
                        DosAction::Continue
                    }
                    0x07 | 0x08 => self.read_char(regs, mem, false)?,
                    0x0a => self.buffered_input(regs, mem)?,
                    _ => DosAction::Continue,
                };
                if !matches!(result, DosAction::WaitForKey) {
                    self.cooked_flush_done = false;
                }
                Ok(result)
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
                // A character-device name opens the device, not a file. The EMMXXXX0
                // EMS manager is the one we model: open it when present so a guest's
                // detection (open + IOCTL) succeeds; when absent let it fall through
                // to a host-file open that fails, so the guest reads "no EMS".
                if let Some(name) = read_asciiz(mem, regs.ds, regs.dx)? {
                    if is_ems_device_name(&name) && self.ems_present {
                        let Some(handle) = self.alloc_handle() else {
                            set_dos_error(regs, 0x04); // too many open files
                            return Ok(DosAction::Continue);
                        };
                        self.ems_handles.insert(handle);
                        regs.ax = handle;
                        regs.cf = false;
                        return Ok(DosAction::Continue);
                    }
                }
                let path = match self.resolve_open_path(mem, regs.ds, regs.dx)? {
                    Ok(path) => path,
                    Err(code) => {
                        set_dos_error(regs, code);
                        return Ok(DosAction::Continue);
                    }
                };
                // Validate the access mode after the path, matching DOS order: a
                // bad path reports its own error before the invalid-access code.
                let Some(mode) = AccessMode::try_from_open_al(regs.ax as u8) else {
                    set_dos_error(regs, 0x0c);
                    return Ok(DosAction::Continue);
                };
                let Some(handle) = self.alloc_handle() else {
                    set_dos_error(regs, 0x04); // too many open files
                    return Ok(DosAction::Continue);
                };
                match open_host_file(&path, mode) {
                    Ok(file) => {
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
                // A read from the EMMXXXX0 character device returns end-of-file (0
                // bytes), the way a real EMM driver answers a DOS read; its control
                // traffic goes through INT 67h, not the file handle.
                if self.ems_handles.contains(&handle) {
                    regs.ax = 0;
                    regs.cf = false;
                    return Ok(DosAction::Continue);
                }
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
                if self.open_files.remove(&regs.bx).is_some() || self.ems_handles.remove(&regs.bx) {
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
                match self.allocate_strategy(regs.bx, mem)? {
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
                match self.free_routed(regs.es, mem)? {
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
                match self.resize_routed(regs.es, regs.bx, mem)? {
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
            // AH=31h KEEP PROCESS (TSR): terminate with the AL return code but leave
            // the program resident. DX is the resident size in paragraphs; the arena
            // trims the program block to it and flags the block resident so its
            // paragraphs are not reclaimed. Limit: the resident image is recorded
            // in the arena only (no MCB chain, no INT 21h vector hand-off); the exit
            // path is otherwise identical to AH=4Ch, so the machine frees nothing.
            0x31 => {
                self.arena.keep_resident(regs.dx, mem)?;
                // Record the resident region's base so an ancestor's exit will not
                // reclaim it (the EXEC chain can unwind past this resident block).
                self.resident_regions.push(self.arena.chain_first);
                Ok(DosAction::Exit((regs.ax & 0x00ff) as u8))
            }
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
                let Some(handle) = self.alloc_handle() else {
                    set_dos_error(regs, 0x04); // too many open files
                    return Ok(DosAction::Continue);
                };
                match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&path)
                {
                    Ok(file) => {
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
            // AH=39h: create the directory named at DS:DX. CF=0 on success; CF=1 +
            // AX=0x03 (path) or 0x05 (access) on failure.
            0x39 => {
                let path = match self.resolve_open_path(mem, regs.ds, regs.dx)? {
                    Ok(path) => path,
                    Err(code) => {
                        set_dos_error(regs, code);
                        return Ok(DosAction::Continue);
                    }
                };
                match std::fs::create_dir(&path) {
                    Ok(()) => regs.cf = false,
                    Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                }
                Ok(DosAction::Continue)
            }
            // AH=3Ah: remove the directory named at DS:DX.
            0x3a => {
                let path = match self.resolve_open_path(mem, regs.ds, regs.dx)? {
                    Ok(path) => path,
                    Err(code) => {
                        set_dos_error(regs, code);
                        return Ok(DosAction::Continue);
                    }
                };
                match std::fs::remove_dir(&path) {
                    Ok(()) => regs.cf = false,
                    Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                }
                Ok(DosAction::Continue)
            }
            // AH=3Bh: set the current directory to DS:DX. The path must name an
            // existing directory; the current directory is global in DOS.
            0x3b => {
                let Some(name) = read_asciiz(mem, regs.ds, regs.dx)? else {
                    set_dos_error(regs, 0x03);
                    return Ok(DosAction::Continue);
                };
                match self.resolve_name(&name) {
                    Ok(path) if path.is_dir() => {
                        self.cwd = self.absolute_dos_path(&name);
                        regs.cf = false;
                    }
                    Ok(_) => set_dos_error(regs, 0x03),
                    Err(code) => set_dos_error(regs, code),
                }
                Ok(DosAction::Continue)
            }
            // AH=41h: delete the file named at DS:DX.
            0x41 => {
                let path = match self.resolve_open_path(mem, regs.ds, regs.dx)? {
                    Ok(path) => path,
                    Err(code) => {
                        set_dos_error(regs, code);
                        return Ok(DosAction::Continue);
                    }
                };
                match std::fs::remove_file(&path) {
                    Ok(()) => regs.cf = false,
                    Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                }
                Ok(DosAction::Continue)
            }
            // AH=47h: get the current directory for the drive in DL (0=default,
            // 3=C:) into the 64-byte buffer at DS:SI, with no leading backslash.
            0x47 => {
                let base = usize::from(regs.ds) * 16 + usize::from(regs.si);
                let bytes = self.cwd.as_bytes();
                let written = bytes.len().min(63);
                for (index, &byte) in bytes.iter().take(written).enumerate() {
                    mem.write_u8(base + index, byte)?;
                }
                mem.write_u8(base + written, 0)?;
                regs.ax = 0x0100; // AX is undocumented; some callers expect 0x0100
                regs.cf = false;
                Ok(DosAction::Continue)
            }
            // AH=56h: rename DS:DX to ES:DI (both ASCIIZ). CF=0 on success.
            0x56 => {
                let old = match self.resolve_open_path(mem, regs.ds, regs.dx)? {
                    Ok(path) => path,
                    Err(code) => {
                        set_dos_error(regs, code);
                        return Ok(DosAction::Continue);
                    }
                };
                let Some(new_name) = read_asciiz(mem, regs.es, regs.di)? else {
                    set_dos_error(regs, 0x03);
                    return Ok(DosAction::Continue);
                };
                let new = match self.resolve_name(&new_name) {
                    Ok(path) => path,
                    Err(code) => {
                        set_dos_error(regs, code);
                        return Ok(DosAction::Continue);
                    }
                };
                match std::fs::rename(&old, &new) {
                    Ok(()) => regs.cf = false,
                    Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                }
                Ok(DosAction::Continue)
            }
            // AH=36h: get free disk space for the drive in DL (0=default, 3=C:).
            // Cosmetic but plausible over the host-filesystem C:: 32 KiB clusters
            // on a ~2 GiB volume. AX=sectors/cluster, BX=free clusters,
            // CX=bytes/sector, DX=total clusters; AX=0xFFFF means an invalid drive.
            0x36 => {
                let drive = (regs.dx & 0xff) as u8;
                if drive != 0 && drive != 3 {
                    regs.ax = 0xffff;
                    return Ok(DosAction::Continue);
                }
                regs.ax = 64; // sectors per cluster (64 * 512 = 32 KiB)
                regs.cx = 512; // bytes per sector
                regs.dx = 0xffff; // total clusters (~2 GiB)
                regs.bx = 0xf000; // free clusters
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
                // A write to the EMMXXXX0 character device is accepted and discarded,
                // reporting every byte written, the way a real EMM driver answers a
                // DOS write (its real traffic is INT 67h, not the file handle).
                if self.ems_handles.contains(&handle) {
                    regs.ax = regs.cx;
                    regs.cf = false;
                    return Ok(DosAction::Continue);
                }
                // AUX (3, COM1) and PRN (4, LPT1): accept the write and report every
                // byte written, but discard the data. The HLE has no serial or
                // printer capture at the INT 21h layer (marked). Handle 0 (stdin) is
                // left returning 0x06: whether a stdin write should route to CON is
                // ambiguous, and changing it would alter the pinned invalid-handle
                // test, so it is deferred to a human-reviewed slice.
                if handle == 3 || handle == 4 {
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
                let offset = (u32::from(regs.cx) << 16) | u32::from(regs.dx);
                let whence = regs.ax as u8;
                let Some(of) = self.open_files.get_mut(&handle) else {
                    set_dos_error(regs, 0x06);
                    return Ok(DosAction::Continue);
                };
                // Resolve the base the offset applies to. whence 0 takes the offset
                // unsigned; 1 (current) and 2 (end) take it signed. DOS lets the
                // resulting pointer fall before the start of the file with no error:
                // the 32-bit pointer wraps, and a later read/write at that spot fails.
                let base = match whence {
                    0 => 0i64,
                    1 => match of.file.stream_position() {
                        Ok(p) => p as i64,
                        Err(err) => {
                            set_dos_error(regs, dos_io_error_code(&err));
                            return Ok(DosAction::Continue);
                        }
                    },
                    2 => match of.file.seek(SeekFrom::End(0)) {
                        Ok(p) => p as i64,
                        Err(err) => {
                            set_dos_error(regs, dos_io_error_code(&err));
                            return Ok(DosAction::Continue);
                        }
                    },
                    _ => {
                        set_dos_error(regs, 0x01);
                        return Ok(DosAction::Continue);
                    }
                };
                let signed = if whence == 0 {
                    i64::from(offset)
                } else {
                    i64::from(offset as i32)
                };
                // A before-start pointer wraps to its 32-bit two's complement, the
                // value DOS reports. Seeking the host past EOF is harmless; a read
                // there returns 0 bytes, the HLE's stand-in for DOS's failed I/O.
                let pos = (base + signed) as u32;
                if let Err(err) = of.file.seek(SeekFrom::Start(u64::from(pos))) {
                    set_dos_error(regs, dos_io_error_code(&err));
                    return Ok(DosAction::Continue);
                }
                regs.ax = pos as u16;
                regs.dx = (pos >> 16) as u16;
                regs.cf = false;
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
            // AH=00h: terminate the program, the old .COM exit path. Equivalent to
            // INT 20h: return with exit code 0.
            0x00 => Ok(DosAction::Exit(0)),
            // AH=50h: set the current PSP segment (SET PID). AH=51h/62h get it. The
            // kernel tracks the active PSP as the arena's program segment.
            0x50 => {
                self.arena.psp_seg = regs.bx;
                Ok(DosAction::Continue)
            }
            0x51 | 0x62 => {
                regs.bx = self.arena.psp_seg;
                Ok(DosAction::Continue)
            }
            0x52 => {
                let (es, bx) = write_sysvars(
                    mem,
                    self.arena.first_mcb(),
                    self.ems_present,
                    self.lastdrive,
                )?;
                regs.es = es;
                regs.bx = bx;
                Ok(DosAction::Continue)
            }
            // AH=0Dh DISK RESET: the HLE writes host files directly, so there are no
            // DOS buffers to flush. Succeed with CF clear.
            0x0d => {
                regs.cf = false;
                Ok(DosAction::Continue)
            }
            // AH=18h/1Dh/1Eh/20h: CP/M-compatibility null functions. Real DOS returns
            // AL=00h and does nothing else.
            0x18 | 0x1d | 0x1e | 0x20 => {
                regs.ax &= 0xff00;
                Ok(DosAction::Continue)
            }
            // AH=2Eh SET VERIFY FLAG (from AL) / AH=54h GET VERIFY FLAG (into AL). The
            // flag has no effect on the direct host writes; it only round-trips.
            0x2e => {
                self.verify_flag = (regs.ax & 0xff) != 0;
                Ok(DosAction::Continue)
            }
            0x54 => {
                regs.ax = (regs.ax & 0xff00) | u16::from(self.verify_flag);
                Ok(DosAction::Continue)
            }
            // AH=58h memory-allocation strategy and UMB link state. AL=00/01 get/set
            // the strategy, AL=02/03 get/set the UMB link state. The strategy bits
            // route AH=48h through conventional memory, upper memory, or high-then-low.
            0x58 => match regs.ax as u8 {
                0x00 => {
                    regs.ax = self.alloc_strategy;
                    regs.cf = false;
                    Ok(DosAction::Continue)
                }
                0x01 => {
                    // DOS 5+ keys off BL (BH is expected 0 and ignored). The nine
                    // valid strategies: low 2 bits select the fit, bits 6-7 the
                    // memory area. DOS rejects anything else.
                    let strategy = regs.bx & 0x00ff;
                    if is_valid_alloc_strategy(strategy) {
                        self.alloc_strategy = strategy;
                        regs.cf = false;
                    } else {
                        set_dos_error(regs, 0x01); // invalid strategy
                    }
                    Ok(DosAction::Continue)
                }
                0x02 => {
                    regs.ax = u16::from(self.umb_link); // AL = current link state
                    regs.cf = false;
                    Ok(DosAction::Continue)
                }
                0x03 => {
                    // Linking the upper area is allowed only when the box was loaded
                    // with DOS=UMB and an arena exists. Without either, the call fails
                    // with AX=0001h, the way real DOS reports a machine loaded without
                    // DOS=UMB (a program must then use the XMS Request UMB primitive).
                    if !self.dos_umb || self.umb.is_none() {
                        set_dos_error(regs, 0x01);
                        return Ok(DosAction::Continue);
                    }
                    // BX = 0 unlink UMBs, 1 link them. Anything else is invalid.
                    match regs.bx {
                        0x0000 => {
                            self.umb_link = false;
                            regs.cf = false;
                        }
                        0x0001 => {
                            self.umb_link = true;
                            regs.cf = false;
                        }
                        _ => set_dos_error(regs, 0x01), // invalid link state
                    }
                    Ok(DosAction::Continue)
                }
                _ => {
                    set_dos_error(regs, 0x01); // invalid subfunction
                    Ok(DosAction::Continue)
                }
            },
            // AH=67h SET HANDLE COUNT: the handle table is an unbounded map, so the
            // requested count always fits. Succeed.
            0x67 => {
                regs.cf = false;
                Ok(DosAction::Continue)
            }
            // AH=68h/6Ah COMMIT FILE (fflush): host writes are unbuffered at the DOS
            // layer, so there is nothing to flush. Succeed for a valid open handle.
            0x68 | 0x6a => {
                if self.open_files.contains_key(&regs.bx) {
                    regs.cf = false;
                } else {
                    set_dos_error(regs, 0x06); // invalid handle
                }
                Ok(DosAction::Continue)
            }
            // AH=45h DUP: duplicate the handle in BX onto a new handle. The clone shares
            // the underlying open file (and its position) via File::try_clone.
            0x45 => {
                let cloned = match self.open_files.get(&regs.bx) {
                    Some(of) => of.file.try_clone().map(|file| OpenFile {
                        file,
                        mode: of.mode,
                    }),
                    None => {
                        set_dos_error(regs, 0x06); // invalid handle
                        return Ok(DosAction::Continue);
                    }
                };
                match cloned {
                    Ok(open) => {
                        let Some(handle) = self.alloc_handle() else {
                            set_dos_error(regs, 0x04); // too many open files
                            return Ok(DosAction::Continue);
                        };
                        self.open_files.insert(handle, open);
                        regs.ax = handle;
                        regs.cf = false;
                    }
                    Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                }
                Ok(DosAction::Continue)
            }
            // AH=46h DUP2/FORCEDUP: force handle CX to refer to the same open file as BX,
            // closing whatever CX referred to first (insert drops the old File via RAII).
            0x46 => {
                let cloned = match self.open_files.get(&regs.bx) {
                    Some(of) => of.file.try_clone().map(|file| OpenFile {
                        file,
                        mode: of.mode,
                    }),
                    None => {
                        set_dos_error(regs, 0x06);
                        return Ok(DosAction::Continue);
                    }
                };
                match cloned {
                    Ok(open) => {
                        self.open_files.insert(regs.cx, open);
                        regs.cf = false;
                    }
                    Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                }
                Ok(DosAction::Continue)
            }
            // AH=43h CHMOD: AL=00 get attributes (CX), AL=01 set. Limit: only the
            // read-only bit (0x01) maps to a host permission; hidden/system/archive are
            // not represented, so get reports archive (0x20) for files and 0x10 for dirs.
            0x43 => {
                let path = match self.resolve_open_path(mem, regs.ds, regs.dx)? {
                    Ok(path) => path,
                    Err(code) => {
                        set_dos_error(regs, code);
                        return Ok(DosAction::Continue);
                    }
                };
                match regs.ax as u8 {
                    0x00 => match std::fs::metadata(&path) {
                        Ok(meta) => {
                            let mut attr = if meta.is_dir() { 0x10u16 } else { 0x20 };
                            if meta.permissions().readonly() {
                                attr |= 0x01;
                            }
                            regs.cx = attr;
                            regs.cf = false;
                        }
                        Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                    },
                    0x01 => match std::fs::metadata(&path) {
                        Ok(meta) => {
                            let mut perms = meta.permissions();
                            perms.set_readonly(regs.cx & 0x01 != 0);
                            match std::fs::set_permissions(&path, perms) {
                                Ok(()) => regs.cf = false,
                                Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                            }
                        }
                        Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                    },
                    _ => set_dos_error(regs, 0x01),
                }
                Ok(DosAction::Continue)
            }
            // AH=5Bh CREATE NEW FILE: like AH=3Ch but fails with 0x50 (file exists) when
            // the file is already present, the create-exclusive semantic.
            0x5b => {
                let path = match self.resolve_open_path(mem, regs.ds, regs.dx)? {
                    Ok(path) => path,
                    Err(code) => {
                        set_dos_error(regs, code);
                        return Ok(DosAction::Continue);
                    }
                };
                let Some(handle) = self.alloc_handle() else {
                    set_dos_error(regs, 0x04); // too many open files
                    return Ok(DosAction::Continue);
                };
                match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(file) => {
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
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                        set_dos_error(regs, 0x50) // file already exists
                    }
                    Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                }
                Ok(DosAction::Continue)
            }
            // AH=44h IOCTL. The subfunction is in AL. The load-bearing one is AL=00h get
            // device info, which programs use to tell a console (bit 7 ISDEV set) from a
            // redirected file (clear) so they can decide whether to buffer output.
            // Limit: the character-device info word populates only the console bits
            // that matter (ISDEV + stdin/stdout); NUL/clock/binary/special are not set.
            0x44 => {
                let handle = regs.bx;
                let valid = handle <= 4
                    || self.open_files.contains_key(&handle)
                    || self.ems_handles.contains(&handle);
                match regs.ax as u8 {
                    0x00 => {
                        if handle <= 4 {
                            // The five standard handles are all character devices: stdin(0)
                            // input, stdout(1)/stderr(2)/stdprn(4) output, stdaux(3) both.
                            let io = match handle {
                                0 => 0x01,
                                3 => 0x03,
                                _ => 0x02,
                            };
                            regs.dx = 0x80 | io; // bit 7 ISDEV + the console direction bits
                            regs.cf = false;
                        } else if self.ems_handles.contains(&handle) {
                            // The EMMXXXX0 device: bit 7 ISDEV (so a guest knows it is a
                            // device, not a file) plus the IOCTL-supported bit, the way an
                            // EMM driver answers the open-then-IOCTL detection.
                            regs.dx = 0xc080;
                            regs.cf = false;
                        } else if self.open_files.contains_key(&handle) {
                            // A regular file on C: (drive index 2); bit 7 clear means a file.
                            regs.dx = 0x0002;
                            regs.cf = false;
                        } else {
                            set_dos_error(regs, 0x06); // invalid handle
                        }
                    }
                    0x01 => {
                        // Set device info: the attribute bits have no host effect; accept.
                        if valid {
                            regs.cf = false;
                        } else {
                            set_dos_error(regs, 0x06);
                        }
                    }
                    0x06 => {
                        // Get input status: AL=0xFF ready, 0x00 not. Console input (handle 0)
                        // is ready only when a key waits; files and outputs are always ready.
                        if valid {
                            let ready = handle != 0 || !kbd_ring_is_empty(mem)?;
                            regs.ax = (regs.ax & 0xff00) | if ready { 0xff } else { 0x00 };
                            regs.cf = false;
                        } else {
                            set_dos_error(regs, 0x06);
                        }
                    }
                    0x07 => {
                        // Get output status: host writes never block, so always ready.
                        if valid {
                            regs.ax = (regs.ax & 0xff00) | 0xff;
                            regs.cf = false;
                        } else {
                            set_dos_error(regs, 0x06);
                        }
                    }
                    0x08 => {
                        // Block device removable? AX=1 fixed (C: is a fixed disk).
                        regs.ax = 1;
                        regs.cf = false;
                    }
                    0x09 => {
                        // Is drive remote? DX bit 12 clear: C: is local.
                        regs.dx = 0;
                        regs.cf = false;
                    }
                    0x0a => {
                        // Is handle remote? DX bit 15 clear: local.
                        regs.dx = 0;
                        regs.cf = false;
                    }
                    _ => set_dos_error(regs, 0x01), // unsupported IOCTL subfunction
                }
                Ok(DosAction::Continue)
            }
            // AH=57h GET/SET a file's last-written date and time on the open handle in BX.
            // AL=00 returns the packed time/date in CX/DX; AL=01 sets them from CX/DX.
            // Archivers and compilers use this to preserve timestamps across a copy.
            0x57 => {
                let Some(of) = self.open_files.get(&regs.bx) else {
                    set_dos_error(regs, 0x06); // invalid handle
                    return Ok(DosAction::Continue);
                };
                match regs.ax as u8 {
                    0x00 => match of.file.metadata().and_then(|m| m.modified()) {
                        Ok(modified) => {
                            let (time, date) = dos_time_date(modified);
                            regs.cx = time;
                            regs.dx = date;
                            regs.cf = false;
                        }
                        Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                    },
                    0x01 => match of.file.set_modified(systemtime_from_dos(regs.cx, regs.dx)) {
                        Ok(()) => regs.cf = false,
                        Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                    },
                    _ => set_dos_error(regs, 0x01),
                }
                Ok(DosAction::Continue)
            }
            // AH=59h GET EXTENDED ERROR: report the last DOS error. AX = the saved
            // code, BH = error class, BL = suggested action, CH = locus. We use one
            // fixed mapping for every code: class 0x0D (unknown/other), action 0x05
            // (immediate abort), locus 0x01 (unknown). Limit: real DOS derives
            // class/action/locus per code from a table; in-scope callers only read
            // AX, so the coarse mapping suffices.
            0x59 => {
                regs.ax = self.last_error;
                regs.bx = (0x0d << 8) | 0x05; // BH = class, BL = action
                regs.cx = (regs.cx & 0x00ff) | (0x01 << 8); // CH = locus, CL preserved
                regs.cf = false; // the query itself succeeds; do not overwrite last_error
                Ok(DosAction::Continue)
            }
            // AH=5Ah CREATE TEMPORARY FILE: DS:DX points at an ASCIIZ directory path
            // ending in '\'. Generate a unique 8.3 name, append it (with its NUL) so
            // the caller can read back the full path, then create it create-exclusive.
            // CF=0 + AX=handle on success; on a name collision after a bounded number
            // of tries, or a host error, CF=1 with the DOS code.
            0x5a => {
                let Some(dir) = read_asciiz(mem, regs.ds, regs.dx)? else {
                    self.fail(regs, 0x03);
                    return Ok(DosAction::Continue);
                };
                let Some(handle) = self.alloc_handle() else {
                    set_dos_error(regs, 0x04); // too many open files
                    return Ok(DosAction::Continue);
                };
                // Try a sequence of names until one does not yet exist. The host
                // create-exclusive open is the real guard; this loop just picks a
                // free candidate. Limit: a fixed 0..4096 sweep, not DOS's clock
                // seed; ample for the temp files a single program run creates.
                let mut created = None;
                for n in 0u16..4096 {
                    let candidate = format!("{dir}{n:04X}.$$$");
                    let path = match self.resolve_name(&candidate) {
                        Ok(path) => path,
                        Err(code) => {
                            self.fail(regs, code);
                            return Ok(DosAction::Continue);
                        }
                    };
                    match OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create_new(true)
                        .open(&path)
                    {
                        Ok(file) => {
                            created = Some((file, candidate));
                            break;
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                        Err(err) => {
                            self.fail(regs, dos_io_error_code(&err));
                            return Ok(DosAction::Continue);
                        }
                    }
                }
                let Some((file, name)) = created else {
                    self.fail(regs, 0x05); // every candidate was taken
                    return Ok(DosAction::Continue);
                };
                // Append the generated name after the directory path at DS:DX so the
                // caller can read the full path back.
                let suffix = &name[dir.len()..];
                let tail = usize::from(regs.ds) * 16 + usize::from(regs.dx) + dir.len();
                for (i, &byte) in suffix.as_bytes().iter().enumerate() {
                    mem.write_u8(tail + i, byte)?;
                }
                mem.write_u8(tail + suffix.len(), 0)?;
                self.open_files.insert(
                    handle,
                    OpenFile {
                        file,
                        mode: AccessMode::ReadWrite,
                    },
                );
                regs.ax = handle;
                regs.cf = false;
                Ok(DosAction::Continue)
            }
            // AH=6Ch EXTENDED OPEN/CREATE: a superset of AH=3Dh open and AH=3Ch
            // create. BX = access/share mode (low 3 bits are the access mode), CX =
            // attributes for a created file (ignored, as in 3Ch), DX = action flags
            // (bit 0 open-if-exists, bit 1 replace/truncate-if-exists, bit 4
            // create-if-not-exists), DS:SI = ASCIIZ filename. On success CF=0,
            // AX=handle, CX=action taken (1 opened, 2 created, 3 truncated). On
            // failure CF=1 with the DOS code.
            0x6c => {
                let path = match self.resolve_open_path(mem, regs.ds, regs.si)? {
                    Ok(path) => path,
                    Err(code) => {
                        self.fail(regs, code);
                        return Ok(DosAction::Continue);
                    }
                };
                // Validate the access mode after the path, matching DOS order.
                let Some(mode) = AccessMode::try_from_open_al(regs.bx as u8) else {
                    self.fail(regs, 0x0c);
                    return Ok(DosAction::Continue);
                };
                let exists = path.exists();
                let open_if = regs.dx & 0x0001 != 0;
                let truncate_if = regs.dx & 0x0002 != 0;
                let create_if = regs.dx & 0x0010 != 0;
                // Pick the host action from the flags and whether the file exists.
                // action_taken: 1 opened, 2 created, 3 truncated (replaced).
                let action_taken = if exists {
                    if truncate_if {
                        3u16
                    } else if open_if {
                        1u16
                    } else {
                        // The file is there but neither open nor replace is allowed.
                        self.fail(regs, 0x50); // file already exists
                        return Ok(DosAction::Continue);
                    }
                } else if create_if {
                    2u16
                } else {
                    self.fail(regs, 0x02); // file not found and create not allowed
                    return Ok(DosAction::Continue);
                };
                let Some(handle) = self.alloc_handle() else {
                    set_dos_error(regs, 0x04); // too many open files
                    return Ok(DosAction::Continue);
                };
                let result = match action_taken {
                    1 => open_host_file(&path, mode),
                    2 => OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create_new(true)
                        .open(&path),
                    3 => OpenOptions::new()
                        .read(true)
                        .write(true)
                        .truncate(true)
                        .open(&path),
                    _ => unreachable!("known extended-open action"),
                };
                match result {
                    Ok(file) => {
                        self.open_files.insert(handle, OpenFile { file, mode });
                        regs.ax = handle;
                        regs.cx = action_taken;
                        regs.cf = false;
                    }
                    Err(err) => self.fail(regs, dos_io_error_code(&err)),
                }
                Ok(DosAction::Continue)
            }
            // AH=60h TRUENAME: canonicalize the ASCIIZ path at DS:SI into the
            // 128-byte buffer at ES:DI as a fully qualified, drive-letter-prefixed,
            // uppercase path (C:\...). Folds '.'/'..' and the current directory the
            // same way the file calls resolve a name. CF=0 on success; CF=1 with the
            // DOS code on a bad path.
            0x60 => {
                let Some(name) = read_asciiz(mem, regs.ds, regs.si)? else {
                    self.fail(regs, 0x03);
                    return Ok(DosAction::Continue);
                };
                // A drive letter other than C: names an unmounted drive.
                let bytes = name.as_bytes();
                if bytes.len() >= 2 && bytes[1] == b':' && !bytes[0].eq_ignore_ascii_case(&b'C') {
                    self.fail(regs, 0x03);
                    return Ok(DosAction::Continue);
                }
                let canonical =
                    format!("C:\\{}", self.absolute_dos_path(&name).to_ascii_uppercase());
                let base = usize::from(regs.es) * 16 + usize::from(regs.di);
                // The output buffer is 128 bytes including the terminator.
                let bytes = canonical.as_bytes();
                let written = bytes.len().min(127);
                for (i, &byte) in bytes.iter().take(written).enumerate() {
                    mem.write_u8(base + i, byte)?;
                }
                mem.write_u8(base + written, 0)?;
                regs.cf = false;
                Ok(DosAction::Continue)
            }
            // The FCB (File Control Block) file API: handle-free file ops keyed by
            // the FCB at DS:DX. AL=00 success, AL=0xFF failure (no CF). The
            // sequential ops transfer one record through the DTA; the random-access
            // ops (AH=21h/22h/24h/27h/28h) use the random-record field at 0x21.
            // AH=29h parses a filename into an FCB.
            0x0f => self.fcb_open_or_create(mem, regs, false),
            0x16 => self.fcb_open_or_create(mem, regs, true),
            0x10 => self.fcb_close(mem, regs),
            0x13 => self.fcb_delete(mem, regs),
            0x17 => self.fcb_rename(mem, regs),
            0x14 => self.fcb_seq_read(mem, regs),
            0x15 => self.fcb_seq_write(mem, regs),
            0x21 => self.fcb_random_read(mem, regs),
            0x22 => self.fcb_random_write(mem, regs),
            0x23 => self.fcb_file_size(mem, regs),
            0x24 => self.fcb_set_random(mem, regs),
            0x27 => self.fcb_random_block_read(mem, regs),
            0x28 => self.fcb_random_block_write(mem, regs),
            0x29 => self.fcb_parse_filename(mem, regs),
            0x11 => self.fcb_find_first(mem, regs),
            0x12 => self.fcb_find_next(mem, regs),
            // Everything else is not yet implemented; later slices fill it in. An
            // unimplemented function returns Continue so the IRET stub returns to
            // the caller.
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
/// command tail at 0x80. The documented vectors at 0x0A/0x0E/0x12 snapshot the
/// current INT 22h/23h/24h IVT entries; 0x16 (parent PSP) defaults to 0 and the
/// EXEC path overwrites it for a child; 0x32/0x34 hold the JFT count and far
/// pointer, with the 20-byte JFT at 0x18 wired stdin/stdout/stderr open and the
/// rest closed. The environment segment (0x2C) is filled in by
/// `DosKernel::install_environment`.
fn build_psp(mem: &mut Memory, psp_seg: u16, top_of_mem_paragraph: u16) -> Result<(), DosError> {
    let base = usize::from(psp_seg) * 16;
    mem.write_u8(base, 0xcd)?;
    mem.write_u8(base + 1, 0x20)?;
    mem.write_u16(base + 2, top_of_mem_paragraph)?;
    // PSP:0x0A/0x0E/0x12 are the terminate (INT 22h), Ctrl-C (INT 23h), and
    // critical-error (INT 24h) far vectors DOS saves so a child can restore them
    // on exit. Snapshot the live IVT entries (offset then segment) at AL*4. The
    // PSP copy and the IVT entry stay consistent because the PSP mirrors the IVT;
    // a guest installing its own INT 24h handler writes the IVT, and the next
    // build_psp captures it here.
    for (psp_off, int_no) in [(0x0au16, 0x22u8), (0x0e, 0x23), (0x12, 0x24)] {
        let ivt = usize::from(int_no) * 4;
        mem.write_u16(base + usize::from(psp_off), mem.read_u16(ivt)?)?;
        mem.write_u16(base + usize::from(psp_off) + 2, mem.read_u16(ivt + 2)?)?;
    }
    // PSP:0x16 parent PSP segment. A program loaded directly has no parent (0);
    // the EXEC path patches it to the parent PSP for a child.
    mem.write_u16(base + 0x16, 0)?;
    // PSP:0x18 the 20-byte Job File Table. 0xFF is a closed handle; entries 0/1/2
    // open onto stdin/stdout/stderr (handle 1 -> the device the SFT slot names).
    for off in 0..JFT_LEN {
        mem.write_u8(base + 0x18 + off, 0xff)?;
    }
    mem.write_u8(base + 0x18, 0x01)?; // stdin
    mem.write_u8(base + 0x19, 0x01)?; // stdout
    mem.write_u8(base + 0x1a, 0x01)?; // stderr
    // PSP:0x32 JFT entry count, PSP:0x34 far pointer to the JFT (PSP:0x18).
    mem.write_u16(base + 0x32, JFT_LEN as u16)?;
    mem.write_u16(base + 0x34, 0x0018)?;
    mem.write_u16(base + 0x36, psp_seg)?;
    mem.write_u8(base + 0x80, 0x00)?;
    mem.write_u8(base + 0x81, 0x0d)?;
    Ok(())
}

/// The default Job File Table length DOS reports in PSP:0x32 (20 handles).
const JFT_LEN: usize = 20;

/// A critical-error handler's return code, the value an INT 24h handler leaves in
/// AL for DOS to act on (RBIL INT 24h "Return:"). DOS reads only the low two bits,
/// so 4..255 alias back into this set; we mask the same way.
// Scaffolding for the deferred INT 24h far-call (see psp_saved_vector); exercised
// by tests but not yet on a live code path, so allow dead_code until the machine
// crate's host->guest call seam invokes a handler.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CriticalErrorResponse {
    Ignore, // 0: ignore the error and return success to the caller
    Retry,  // 1: retry the failing operation
    Abort,  // 2: abort the program through INT 23h
    Fail,   // 3: fail the system call (DOS 3.1+)
}

impl CriticalErrorResponse {
    /// Decode the AL a critical-error handler returns. Only AL bits 0-1 are
    /// significant, so a handler that returns 0x07 (a common "leave AL untouched"
    /// accident) decodes the same as 0x03 Fail, matching DOS.
    #[allow(dead_code)] // scaffolding for the deferred INT 24h far-call; exercised by tests
    fn from_al(al: u8) -> Self {
        match al & 0x03 {
            0 => CriticalErrorResponse::Ignore,
            1 => CriticalErrorResponse::Retry,
            2 => CriticalErrorResponse::Abort,
            _ => CriticalErrorResponse::Fail,
        }
    }
}

/// The far pointer (segment, offset) a PSP holds for one of the saved INT 22h/23h/
/// 24h vectors. `psp_off` is 0x0A terminate, 0x0E Ctrl-C, 0x12 critical-error. The
/// vector is stored offset-then-segment, the IVT layout DOS copies it from.
// Limit: the INT 24h vector is stored in the PSP and the IVT, and
// CriticalErrorResponse decodes a handler's reply, but nothing calls the handler:
// the HLE file path goes straight to the host filesystem, so no failing block
// device exists to raise a critical error. The far-call into the guest handler is
// deferred until a block device can fault; that needs the outbound host->guest
// call seam from the machine crate (which would push a fake IRET frame and run the
// vector here at PSP:0x12), at which point this reader supplies the address.
#[allow(dead_code)]
fn psp_saved_vector(mem: &Memory, psp_seg: u16, psp_off: u16) -> Result<(u16, u16), DosError> {
    let base = usize::from(psp_seg) * 16 + usize::from(psp_off);
    let offset = mem.read_u16(base)?;
    let segment = mem.read_u16(base + 2)?;
    Ok((segment, offset))
}

/// Format a DOS environment block: a sequence of ASCIIZ `KEY=VALUE` strings
/// followed by an extra NUL (the empty string that terminates the list). Keys
/// are stored verbatim, so callers pass uppercase DOS-style keys. With no
/// entries the block is a single NUL, a valid empty environment. The DOS 3.0+
/// argv0 trailer is added by `build_env_block_with_argv0`, not here.
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

/// The env block plus the DOS 3.0+ argv0 trailer: the double-NUL-terminated env
/// strings, then a WORD count of 0x0001, then the program's full ASCIIZ path.
/// Real DOS writes the path a program reads back to learn its own name; the
/// loader does not track the guest path, so a fixed `argv0` stands in (marked).
fn build_env_block_with_argv0(entries: &[(&str, &str)], argv0: &str) -> Vec<u8> {
    let mut block = build_env_block(entries);
    block.extend_from_slice(&1u16.to_le_bytes()); // string count following
    block.extend_from_slice(argv0.as_bytes());
    block.push(0); // the argv0 ASCIIZ terminator
    block
}

/// The argv0 path placed in the environment trailer. Limit: the loader does
/// not know the guest program's path, so a single plausible default stands in;
/// in-scope callers read the env strings, not this trailer.
pub const DEFAULT_ARGV0: &str = "C:\\PROGRAM.EXE";

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
    if needed > u32::from(ARENA_TOP) {
        return Err(DosError::ExeNotEnoughMemory {
            needed,
            available: u32::from(ARENA_TOP),
        });
    }
    // Top of the program's block: honor e_maxalloc, clamp to conventional memory.
    let top_paragraph = (u32::from(start_seg) + module_paras + u32::from(exe.e_maxalloc))
        .min(u32::from(ARENA_TOP)) as u16;
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

/// Days from the Unix epoch (1970-01-01) to a civil date, the inverse of
/// `civil_from_days` (Howard Hinnant's days_from_civil). Used to turn a packed DOS
/// date back into a host timestamp for AH=57h AL=01.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * i64::from(mp) + 2) / 5 + i64::from(day) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Convert a packed DOS (time, date) pair to a host SystemTime, treating the fields as
/// UTC the same way `dos_time_date` reads them back. Out-of-range fields are clamped.
fn systemtime_from_dos(time: u16, date: u16) -> std::time::SystemTime {
    let year = 1980 + i64::from(date >> 9);
    let month = u32::from((date >> 5) & 0x0f).clamp(1, 12);
    let day = u32::from(date & 0x1f).clamp(1, 31);
    let hour = u32::from(time >> 11).min(23);
    let minute = u32::from((time >> 5) & 0x3f).min(59);
    let second = u32::from(time & 0x1f) * 2; // DOS stores seconds/2
    let days = days_from_civil(year, month, day).max(0) as u64;
    let secs = days * 86_400 + u64::from(hour) * 3600 + u64::from(minute) * 60 + u64::from(second);
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)
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

/// Drive number (1-based) the FCB find result reports. C: is the only mounted
/// FCB drive, so a normal-FCB find always reports drive 3.
const FCB_FIND_DRIVE: u8 = 3;

/// Inspect a search FCB at `base`. A normal FCB has its name at +1 and searches
/// for normal files only. An extended FCB starts with 0xFF: five reserved bytes,
/// a search-attribute byte at +6, a drive byte at +7, then the normal FCB fields
/// at +7 onward, so the name is at +8. Returns the base `fcb_name` reads the name
/// from, the search attribute (0 for a normal FCB), and whether it is extended.
fn fcb_search_header(mem: &Memory, base: usize) -> Result<(usize, u8, bool), DosError> {
    if mem.read_u8(base)? == 0xff {
        Ok((base + 7, mem.read_u8(base + 6)?, true))
    } else {
        Ok((base, 0, false))
    }
}

/// Write an FCB find result (AH=11h/12h) into the DTA. A normal result is the
/// drive number at +0 followed by the 32-byte directory entry at +1. An extended
/// result prepends the extended header: 0xFF at +0, five reserved bytes, the
/// found file's attribute at +6, the drive at +7, then the directory entry at +8.
/// fcb_set_name lands the name/ext exactly on the entry's name field, and the
/// whole thing doubles as an unopened FCB.
fn write_fcb_find_record(
    mem: &mut Memory,
    dta: (u16, u16),
    entry: &FindEntry,
    extended: bool,
) -> Result<(), DosError> {
    let base = usize::from(dta.0) * 16 + usize::from(dta.1);
    let dirent = if extended { base + 8 } else { base + 1 };
    for i in base..dirent + 0x20 {
        mem.write_u8(i, 0)?;
    }
    if extended {
        mem.write_u8(base, 0xff)?;
        mem.write_u8(base + 6, entry.attr)?; // extended-header attribute byte
        mem.write_u8(base + 7, FCB_FIND_DRIVE)?;
    } else {
        mem.write_u8(base, FCB_FIND_DRIVE)?;
    }
    fcb_set_name(mem, dirent - 1, &entry.name)?; // name at the entry start, ext +8
    mem.write_u8(dirent + 0x0b, entry.attr)?; // entry+0x0B attribute
    // entry+0x0C reserved (10 bytes) stays zero
    mem.write_u16(dirent + 0x16, entry.time)?; // entry+0x16 time
    mem.write_u16(dirent + 0x18, entry.date)?; // entry+0x18 date
    // entry+0x1A starting cluster: the HLE has no FAT, so this is a placeholder
    // until the FAT32 facade lands. FAT-walking copy protection is out of scope.
    mem.write_u16(dirent + 0x1a, 0)?;
    mem.write_u32(dirent + 0x1c, entry.size)?; // entry+0x1C file size (truncated past 4 GiB)
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

// The documented unopened-FCB field offsets (RBIL FCB layout). The block is
// 37 bytes; the kernel touches only the fields the sequential ops need. The
// drive byte at 0x00 is not consulted: fcb_path folds the drive in through the
// 8.3 name, matching resolve_name's default-drive handling.
const FCB_NAME: usize = 0x01; // 8-byte blank-padded file name
const FCB_EXT: usize = 0x09; // 3-byte blank-padded extension
const FCB_BLOCK: usize = 0x0c; // current block number (word)
const FCB_RECSIZE: usize = 0x0e; // logical record size (word)
const FCB_FILESIZE: usize = 0x10; // file size in bytes (dword)
const FCB_DATE: usize = 0x14; // packed date of last write (word)
const FCB_TIME: usize = 0x16; // packed time of last write (word)
const FCB_CURREC: usize = 0x20; // current record within the block (byte)
const FCB_RANDREC: usize = 0x21; // random record number (dword)
const FCB_RENAME_NEW: usize = 0x11; // AH=17h: the new name 8.3 starts here

/// The 8.3 name held in an FCB at `base`, as a DOS path string ("NAME.EXT", or
/// "NAME" with no extension). Trailing blanks in each field are trimmed; a '?'
/// is preserved so AH=13h delete can pass wildcards on to find. The drive byte
/// is folded into the returned name only when it names C: (drive 3) explicitly,
/// otherwise the default drive is used the same as resolve_name does.
fn fcb_name(mem: &Memory, base: usize, name_off: usize) -> Result<String, DosError> {
    let mut name = String::new();
    for i in 0..8 {
        let b = mem.read_u8(base + name_off + i)?;
        if b == b' ' || b == 0 {
            break;
        }
        name.push(b as char);
    }
    let mut ext = String::new();
    for i in 0..3 {
        let b = mem.read_u8(base + FCB_EXT - FCB_NAME + name_off + i)?;
        if b == b' ' || b == 0 {
            break;
        }
        ext.push(b as char);
    }
    if ext.is_empty() {
        Ok(name)
    } else {
        Ok(format!("{name}.{ext}"))
    }
}

/// Write the 8.3 name from a DOS host file name into an FCB at `base`, blank
/// padding each field. Used by AH=0Fh open / AH=16h create to echo the opened
/// name back into the canonical FCB fields. A name that does not split 8.3 is
/// padded as far as it fits.
fn fcb_set_name(mem: &mut Memory, base: usize, name: &str) -> Result<(), DosError> {
    let upper = name.to_ascii_uppercase();
    let (stem, ext) = match upper.rsplit_once('.') {
        Some((s, e)) => (s, e),
        None => (upper.as_str(), ""),
    };
    for i in 0..8 {
        let b = stem.as_bytes().get(i).copied().unwrap_or(b' ');
        mem.write_u8(base + FCB_NAME + i, b)?;
    }
    for i in 0..3 {
        let b = ext.as_bytes().get(i).copied().unwrap_or(b' ');
        mem.write_u8(base + FCB_EXT + i, b)?;
    }
    Ok(())
}

/// Seek `file` to `pos` and read up to `buffer.len()` bytes, returning the count
/// filled (0 at or past EOF). A seek past the end leaves the read returning 0. A
/// host io error maps to 0 filled the same as EOF; FCB ops only signal success or
/// the lone 0xFF, so finer io codes are not surfaced (marked).
fn read_at(file: &mut File, pos: u64, buffer: &mut [u8]) -> Result<usize, DosError> {
    if file.seek(SeekFrom::Start(pos)).is_err() {
        return Ok(0);
    }
    let mut filled = 0usize;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => break,
        }
    }
    Ok(filled)
}

/// The byte file position the FCB's current block and record select, given the
/// record size: (block * 128 + current-record) * record-size. A record size of
/// 0 (an unopened FCB DOS never saw) is treated as 128 so the math is defined.
fn fcb_seq_position(block: u16, current_record: u8, record_size: u16) -> u64 {
    let record_index = u64::from(block) * 128 + u64::from(current_record);
    let size = if record_size == 0 { 128 } else { record_size };
    record_index * u64::from(size)
}

/// Advance an FCB's current-record cursor by one record, carrying into the block
/// number after 128 records, and write both fields back. This is what AH=14h read
/// and AH=15h write do after each transfer.
fn fcb_advance_record(
    mem: &mut Memory,
    base: usize,
    block: u16,
    current_record: u8,
) -> Result<(), DosError> {
    let (next_block, next_record) = if current_record >= 127 {
        (block.wrapping_add(1), 0)
    } else {
        (block, current_record + 1)
    };
    mem.write_u16(base + FCB_BLOCK, next_block)?;
    mem.write_u8(base + FCB_CURREC, next_record)?;
    Ok(())
}

/// Sync an FCB's current block/record fields to a random-record number: block =
/// random / 128, current-record = random % 128. The random-access ops set the
/// sequential cursor this way so a following sequential read picks up where the
/// random op left off (RBIL: AH=21h/22h set the block and record from the random
/// field).
fn fcb_sync_block_record_from_random(
    mem: &mut Memory,
    base: usize,
    random: u32,
) -> Result<(), DosError> {
    let block = (random / 128) as u16;
    let record = (random % 128) as u8;
    mem.write_u16(base + FCB_BLOCK, block)?;
    mem.write_u8(base + FCB_CURREC, record)?;
    Ok(())
}

/// The result of parsing a command-line filename into FCB fields (AH=29h). Each
/// of drive/name/ext is Some only when the source supplied it, so the caller can
/// honor the "keep the existing field" option bits. `consumed` is how many source
/// bytes the parse advanced over, for updating SI.
struct ParsedFcbName {
    drive: Option<u8>,     // 1-based drive number (A=1), or None for none given
    name: Option<[u8; 8]>, // blank-padded 8-char name, or None
    ext: Option<[u8; 3]>,  // blank-padded 3-char ext, or None
    wildcards: bool,       // a '*' or '?' appeared (AL=1)
    invalid_drive: bool,   // a drive letter outside A-Z (AL=0xFF)
    consumed: usize,
}

/// Parse a DOS command-line filename (AH=29h). `opts` is AL: bit0 scans past
/// leading separators first. A '*' fills the rest of its field with '?'. The
/// separator set is the DOS filename terminators (whitespace and the shell
/// punctuation); a name component stops at the first such byte, '.', or ':'.
fn parse_fcb_filename(text: &[u8], opts: u8) -> ParsedFcbName {
    // DOS treats these as filename separators/terminators (RBIL AH=29h notes).
    const SEPARATORS: &[u8] = b":.;,=+ \t/\"[]<>|";
    let mut i = 0usize;
    if opts & 0x01 != 0 {
        // Skip leading separators (but not '.' or ':', which start fields).
        while i < text.len() && matches!(text[i], b' ' | b'\t' | b';' | b',' | b'=' | b'+') {
            i += 1;
        }
    }
    let mut wildcards = false;
    // Optional drive: a letter followed by ':'.
    let mut drive = None;
    let mut invalid_drive = false;
    if i + 1 < text.len() && text[i + 1] == b':' {
        let letter = text[i];
        if letter.is_ascii_alphabetic() {
            drive = Some(letter.to_ascii_uppercase() - b'A' + 1);
            i += 2;
        } else {
            invalid_drive = true;
            i += 2;
        }
    }
    // Name field: up to 8 chars, stopping at a separator. '*' pads with '?'.
    let parse_field = |text: &[u8], i: &mut usize, width: usize, wildcards: &mut bool| {
        let mut field = vec![b' '; width];
        let mut wrote = false;
        let mut pos = 0usize;
        while *i < text.len() && pos < width {
            let b = text[*i];
            if b == 0 || SEPARATORS.contains(&b) {
                break;
            }
            wrote = true;
            if b == b'*' {
                *wildcards = true;
                for slot in field.iter_mut().skip(pos) {
                    *slot = b'?';
                }
                *i += 1;
                // A '*' consumes the rest of the field; skip remaining name chars.
                while *i < text.len() && text[*i] != 0 && !SEPARATORS.contains(&text[*i]) {
                    *i += 1;
                }
                break;
            }
            if b == b'?' {
                *wildcards = true;
            }
            field[pos] = b.to_ascii_uppercase();
            pos += 1;
            *i += 1;
        }
        // Drop any name chars past the field width that did not fit.
        while *i < text.len() && text[*i] != 0 && !SEPARATORS.contains(&text[*i]) {
            *i += 1;
        }
        if wrote { Some(field) } else { None }
    };
    let name = parse_field(text, &mut i, 8, &mut wildcards).map(|v| {
        let mut a = [b' '; 8];
        a.copy_from_slice(&v);
        a
    });
    // Extension: only if a '.' follows.
    let mut ext = None;
    if i < text.len() && text[i] == b'.' {
        i += 1;
        ext = parse_field(text, &mut i, 3, &mut wildcards).map(|v| {
            let mut a = [b' '; 3];
            a.copy_from_slice(&v);
            a
        });
    }
    ParsedFcbName {
        drive,
        name,
        ext,
        wildcards,
        invalid_drive,
        consumed: i,
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
    fn toka_install_ensure_repair_format() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let files = vec![
            ("ICOMMAND.COM".to_string(), vec![1u8, 2, 3]),
            ("VER.COM".to_string(), vec![4u8]),
        ];

        // Format lays everything down on a fresh drive: the shell stays at the
        // root, tools install under C:\DOS (the user-requested layout).
        toka_dos_install(root, &files, InstallMode::Format).unwrap();
        assert!(root.join("ICOMMAND.COM").exists());
        assert!(root.join("DOS").join("VER.COM").exists());
        assert!(!root.join("VER.COM").exists());

        // EnsureIfMissing is a no-op once the marker is present: a hand-edited
        // system file is left untouched.
        std::fs::write(root.join("ICOMMAND.COM"), b"edited").unwrap();
        toka_dos_install(root, &files, InstallMode::EnsureIfMissing).unwrap();
        assert_eq!(std::fs::read(root.join("ICOMMAND.COM")).unwrap(), b"edited");

        // Repair overwrites system files but keeps a stray user file.
        std::fs::write(root.join("USER.TXT"), b"x").unwrap();
        toka_dos_install(root, &files, InstallMode::Repair).unwrap();
        assert_eq!(
            std::fs::read(root.join("ICOMMAND.COM")).unwrap(),
            vec![1, 2, 3]
        );
        assert!(root.join("USER.TXT").exists());

        // Format wipes the stray user file, then reinstalls.
        toka_dos_install(root, &files, InstallMode::Format).unwrap();
        assert!(!root.join("USER.TXT").exists());
        assert!(root.join("ICOMMAND.COM").exists());
    }

    #[test]
    fn install_relocates_tools_under_dos_and_generates_boot_config() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let files = vec![
            ("ICOMMAND.COM".to_string(), vec![1u8]),
            ("COMMAND.COM".to_string(), vec![1u8]),
            ("MEM.COM".to_string(), vec![2u8]),
        ];
        toka_dos_install(root, &files, InstallMode::Format).unwrap();

        // The shell and its COMMAND.COM alias stay at the root (the boot COMSPEC).
        assert!(root.join("ICOMMAND.COM").exists());
        assert!(root.join("COMMAND.COM").exists());
        // A tool installs under C:\DOS, not the root.
        assert!(root.join("DOS").join("MEM.COM").exists());
        assert!(!root.join("MEM.COM").exists());

        // The default boot config is generated.
        let autoexec = std::fs::read_to_string(root.join("AUTOEXEC.BAT")).unwrap();
        assert!(
            autoexec.contains("PATH=C:\\DOS"),
            "AUTOEXEC sets the DOS path"
        );
        let config = std::fs::read_to_string(root.join("CONFIG.SYS")).unwrap();
        assert!(config.contains("DOS=HIGH,UMB"));
        assert!(config.contains("LASTDRIVE=E"));
        // The shipped default names the Toka-DOS memory manager IEMM.EXE (the
        // parser also accepts the real-DOS EMM386.EXE alias).
        assert!(
            config.contains("DEVICE=C:\\DOS\\IEMM.EXE RAM"),
            "the default CONFIG.SYS loads the IEMM manager"
        );

        // A user-edited config survives a reinstall (generated only when absent).
        std::fs::write(root.join("AUTOEXEC.BAT"), b"REM mine").unwrap();
        std::fs::write(root.join("CONFIG.SYS"), b"REM cfg").unwrap();
        toka_dos_install(root, &files, InstallMode::Repair).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("AUTOEXEC.BAT")).unwrap(),
            "REM mine",
            "Repair keeps the user's AUTOEXEC.BAT"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("CONFIG.SYS")).unwrap(),
            "REM cfg",
            "Repair keeps the user's CONFIG.SYS"
        );
    }

    #[test]
    fn ah0a_reads_a_line_until_cr_with_backspace() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let buf = 0x2000usize;
        mem.write_u8(buf, 8).unwrap(); // max length 8
        // Type "ab", backspace, "c", CR.
        seed_keyboard_ring(&mut mem, &[b'a', b'b', 0x08, b'c', 0x0d]).unwrap();
        let mut regs = DosRegs {
            ax: 0x0a00,
            dx: buf as u16,
            ds: 0,
            ..Default::default()
        };
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::Continue);
        assert_eq!(mem.read_u8(buf + 1).unwrap(), 2, "two chars: a, c");
        assert_eq!(mem.read_u8(buf + 2).unwrap(), b'a');
        assert_eq!(mem.read_u8(buf + 3).unwrap(), b'c');
        assert_eq!(
            mem.read_u8(buf + 4).unwrap(),
            0x0d,
            "CR stored after the chars"
        );
    }

    // Enqueue one raw (scancode, ascii) word into the BDA keyboard ring. Extended
    // keys (arrows, F-keys) carry a non-zero scancode and a zero ascii.
    fn seed_ring_word(mem: &mut Memory, word: u16) {
        mem.write_u16(KBD_BDA_BASE + KBD_HEAD, KBD_RING_START)
            .unwrap();
        mem.write_u16(KBD_BDA_BASE + KBD_RING_START as usize, word)
            .unwrap();
        mem.write_u16(KBD_BDA_BASE + KBD_TAIL, KBD_RING_START + 2)
            .unwrap();
    }

    // Seed several raw (scancode<<8 | ascii) words into the ring in order.
    fn seed_ring_words(mem: &mut Memory, words: &[u16]) {
        mem.write_u16(KBD_BDA_BASE + KBD_HEAD, KBD_RING_START)
            .unwrap();
        let mut off = KBD_RING_START;
        for &w in words {
            mem.write_u16(KBD_BDA_BASE + off as usize, w).unwrap();
            off += 2;
        }
        mem.write_u16(KBD_BDA_BASE + KBD_TAIL, off).unwrap();
    }

    #[test]
    fn extended_key_returns_zero_then_scancode_across_two_reads() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        seed_ring_word(&mut mem, 0x3b00); // F1: scancode 0x3B, ascii 0x00

        // AH=07h read (no echo): the first call yields the 0x00 lead byte.
        let mut r1 = DosRegs {
            ax: 0x0700,
            ..Default::default()
        };
        assert_eq!(
            kernel.dispatch(0x21, &mut r1, &mut mem).unwrap(),
            DosAction::Continue
        );
        assert_eq!(r1.ax & 0xff, 0x00, "extended key leads with a 0x00 byte");

        // AH=0Bh still reports input ready even though the ring is drained, because
        // the scancode is pending.
        let mut st = DosRegs {
            ax: 0x0b00,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut st, &mut mem).unwrap();
        assert_eq!(
            st.ax & 0xff,
            0xff,
            "a pending scancode counts as input ready"
        );
        assert!(!st.zf);

        // The second read yields the scancode itself.
        let mut r2 = DosRegs {
            ax: 0x0700,
            ..Default::default()
        };
        assert_eq!(
            kernel.dispatch(0x21, &mut r2, &mut mem).unwrap(),
            DosAction::Continue
        );
        assert_eq!(r2.ax & 0xff, 0x3b, "the second read returns the scancode");

        // The ring is genuinely empty now; a third read blocks.
        let mut r3 = DosRegs {
            ax: 0x0700,
            ..Default::default()
        };
        assert_eq!(
            kernel.dispatch(0x21, &mut r3, &mut mem).unwrap(),
            DosAction::WaitForKey
        );
    }

    #[test]
    fn extended_key_scancode_is_not_echoed() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        seed_ring_word(&mut mem, 0x3b00); // F1

        // AH=01h read-with-echo, twice (0x00 then the scancode).
        for _ in 0..2 {
            let mut regs = DosRegs {
                ax: 0x0100,
                ..Default::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        }
        assert!(
            kernel.stdout().is_empty(),
            "neither the 0x00 lead nor the scancode is echoed"
        );
    }

    #[test]
    fn extended_key_lead_does_not_leak_into_later_line_then_read() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let buf = 0x2000usize;
        mem.write_u8(buf, 8).unwrap();
        // F1 lead, then a complete line "hi" + CR.
        seed_ring_words(
            &mut mem,
            &[0x3b00, u16::from(b'h'), u16::from(b'i'), 0x000d],
        );

        // Read the F1 lead via AH=08h; the scancode is now pending.
        let mut r1 = DosRegs {
            ax: 0x0800,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut r1, &mut mem).unwrap();
        assert_eq!(r1.ax & 0xff, 0x00);

        // Switch to AH=0Ah line input; the pending scancode must not derail it.
        let mut rl = DosRegs {
            ax: 0x0a00,
            dx: buf as u16,
            ds: 0,
            ..Default::default()
        };
        assert_eq!(
            kernel.dispatch(0x21, &mut rl, &mut mem).unwrap(),
            DosAction::Continue
        );
        assert_eq!(mem.read_u8(buf + 1).unwrap(), 2, "the line is 'hi'");

        // A following single-char read finds an empty ring and blocks; the orphaned
        // scancode must not surface as a bare byte.
        let mut r2 = DosRegs {
            ax: 0x0800,
            ..Default::default()
        };
        assert_eq!(
            kernel.dispatch(0x21, &mut r2, &mut mem).unwrap(),
            DosAction::WaitForKey
        );
    }

    #[test]
    fn ah0c_flush_clears_a_pending_scancode() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        seed_ring_word(&mut mem, 0x3b00); // F1

        // AH=08h reads the lead byte; the scancode is pending.
        let mut r1 = DosRegs {
            ax: 0x0800,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut r1, &mut mem).unwrap();
        assert_eq!(r1.ax & 0xff, 0x00);

        // AH=0Ch AL=06h DL=0xFF: flush, then a no-wait read. The flush drops the
        // pending scancode, so the read reports nothing ready.
        let mut r2 = DosRegs {
            ax: 0x0c06,
            dx: 0x00ff,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut r2, &mut mem).unwrap();
        assert!(r2.zf, "the flush cleared the pending scancode");
    }

    #[test]
    fn ah00_terminates_the_program() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut regs = DosRegs {
            ax: 0x0000,
            ..Default::default()
        };
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::Exit(0));
    }

    #[test]
    fn ah50_51_62_get_and_set_current_psp() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut regs = DosRegs {
            ax: 0x5000,
            bx: 0x1234,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap(); // set PSP
        for ah in [0x5100u16, 0x6200] {
            let mut regs = DosRegs {
                ax: ah,
                ..Default::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
            assert_eq!(regs.bx, 0x1234);
        }
    }

    #[test]
    fn ah2e_54_verify_flag_round_trips() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut regs = DosRegs {
            ax: 0x2e01,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap(); // set verify on
        let mut regs = DosRegs {
            ax: 0x5400,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.ax & 0xff, 1);
    }

    #[test]
    fn ah58_alloc_strategy_round_trips() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut regs = DosRegs {
            ax: 0x5801,
            bx: 0x0002,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap(); // set strategy
        assert!(!regs.cf);
        let mut regs = DosRegs {
            ax: 0x5800,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_eq!(regs.ax, 2);
    }

    #[test]
    fn ah58_invalid_strategy_is_rejected() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        // Set a valid strategy first.
        let mut regs = DosRegs {
            ax: 0x5801,
            bx: 0x0001,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        // 0x03 is not a valid strategy: rejected, and the stored one is unchanged.
        let mut regs = DosRegs {
            ax: 0x5801,
            bx: 0x0003,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf, "an invalid strategy sets CF");
        assert_eq!(regs.ax, 0x01, "AX = invalid-function error");
        let mut regs = DosRegs {
            ax: 0x5800,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf, "the get after a rejected set still clears CF");
        assert_eq!(
            regs.ax, 0x0001,
            "the valid strategy survived the rejected set"
        );

        // A high-memory strategy (0x40 last-fit area bits) round-trips too, so the
        // full nine-value set is honored, not just the low-memory three.
        let mut regs = DosRegs {
            ax: 0x5801,
            bx: 0x0042,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf, "a high-memory strategy is accepted");
        let mut regs = DosRegs {
            ax: 0x5800,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.ax, 0x0042, "the high-memory strategy reads back");
    }

    #[test]
    fn ah58_umb_link_state_round_trips() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        // A DOS=UMB box with a UMB area, so the link is allowed and meaningful.
        kernel.set_umb_region(0xc800, 0x2800, &mut mem).unwrap();
        kernel.set_dos_umb(true);
        // UMBs are unlinked by default.
        let mut regs = DosRegs {
            ax: 0x5802,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_eq!(regs.ax & 0xff, 0x00, "UMBs unlinked by default");
        // Link them, then read the state back.
        let mut regs = DosRegs {
            ax: 0x5803,
            bx: 0x0001,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        let mut regs = DosRegs {
            ax: 0x5802,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.ax & 0xff, 0x01, "UMBs report linked after the set");
        // An invalid link state is rejected.
        let mut regs = DosRegs {
            ax: 0x5803,
            bx: 0x0002,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf, "an invalid link state sets CF");
        assert_eq!(regs.ax, 0x01);
    }

    #[test]
    fn ah5803_link_fails_without_a_umb_arena() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        // No UMB area furnished: linking fails with AX=0001h, the way real DOS
        // reports a machine loaded without DOS=UMB.
        let mut regs = DosRegs {
            ax: 0x5803,
            bx: 0x0001,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf, "linking with no UMB area fails");
        assert_eq!(regs.ax, 0x01);
    }

    #[test]
    fn ah5803_link_fails_when_dos_umb_was_not_configured() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        // EMM386 furnished a UMB arena, but CONFIG.SYS had no DOS=UMB, so the DOS
        // link path is unavailable (a program must use XMS Request UMB instead).
        kernel.set_umb_region(0xc800, 0x2800, &mut mem).unwrap();
        kernel.set_dos_umb(false);
        let mut regs = DosRegs {
            ax: 0x5803,
            bx: 0x0001,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf, "linking without DOS=UMB fails even with an arena");
        assert_eq!(regs.ax, 0x01);
        // The XMS Request UMB primitive still works without DOS=UMB.
        assert!(matches!(kernel.request_umb(0x10, &mut mem), Ok(Ok(_))));
    }

    #[test]
    fn resize_umb_distinguishes_too_big_from_an_invalid_segment() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        kernel.set_umb_region(0xc800, 0x2800, &mut mem).unwrap();
        let a = match kernel.request_umb(0x10, &mut mem).unwrap() {
            Ok(seg) => seg,
            other => panic!("expected a UMB, got {other:?}"),
        };
        // Carve a second block so A is no longer the top block.
        let _b = kernel.request_umb(0x10, &mut mem).unwrap();
        // Growing the now-non-top block A has nowhere to go: Some(largest) -> B0h.
        match kernel.resize_umb(a, 0x100, &mut mem).unwrap() {
            Err(Some(largest)) => assert_eq!(largest, 0x10, "B0h reports the current size"),
            other => panic!("expected Err(Some(_)), got {other:?}"),
        }
        // A segment that is not a UMB is None -> B2h.
        assert!(matches!(
            kernel.resize_umb(0x0050, 0x10, &mut mem),
            Ok(Err(None))
        ));
    }

    #[test]
    fn set_umb_region_lays_a_free_block_a_guest_can_read() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        // A 160 KiB pool at 0xC800, the hole above a 32 KiB VGA BIOS at 0xC0000.
        kernel.set_umb_region(0xc800, 0x2800, &mut mem).unwrap();
        // A guest or debugger reads a single free 'Z' MCB spanning the pool.
        let chain = kernel.umb_chain(&mem);
        assert_eq!(chain.len(), 1, "the pool starts as one free block");
        let block = chain[0];
        assert_eq!(block.mcb_seg, 0xc800, "the chain heads at the pool base");
        assert_eq!(block.sig, b'Z', "a single block is the last block");
        assert_eq!(block.owner, 0, "the pool starts free");
        assert_eq!(block.size, 0x2800 - 1, "the header takes one paragraph");
    }

    #[test]
    fn the_umb_arena_exists_independent_of_the_link_state() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        kernel.set_umb_region(0xc800, 0x2800, &mut mem).unwrap();
        // The default link state is unlinked, yet the arena is already in RAM: the
        // manager builds it at load, the link only gates allocation routing.
        assert!(!kernel.umb_link);
        assert_eq!(kernel.umb_chain(&mem).len(), 1);
        // A guest edit to the pool header survives, the chain being authoritative.
        mem.write_u16(0xc800 * 16 + 1, 0x0123).unwrap(); // claim it for PSP 0x0123
        assert_eq!(kernel.umb_chain(&mem)[0].owner, 0x0123);
    }

    #[test]
    fn set_umb_region_with_no_room_clears_the_arena() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        kernel.set_umb_region(0xc800, 0x2800, &mut mem).unwrap();
        assert!(kernel.umb.is_some());
        // A degenerate pool (no room for a header plus data) leaves no arena.
        kernel.set_umb_region(0xc800, 1, &mut mem).unwrap();
        assert!(kernel.umb.is_none());
        assert!(kernel.umb_chain(&mem).is_empty());
    }

    /// Set the AH=58h allocation strategy through the dispatch path.
    fn set_alloc_strategy(kernel: &mut DosKernel, mem: &mut Memory, strategy: u16) {
        let mut regs = DosRegs {
            ax: 0x5801,
            bx: strategy,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, mem).unwrap();
        assert!(!regs.cf, "the strategy {strategy:#06x} is valid");
    }

    /// Link the upper-memory arena through AH=5803h.
    fn link_umbs(kernel: &mut DosKernel, mem: &mut Memory) {
        let mut regs = DosRegs {
            ax: 0x5803,
            bx: 0x0001,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, mem).unwrap();
        assert!(!regs.cf);
    }

    /// AH=48h allocate `paras`, returning the dispatch result registers.
    fn dos_alloc(kernel: &mut DosKernel, mem: &mut Memory, paras: u16) -> DosRegs {
        let mut regs = DosRegs {
            ax: 0x4800,
            bx: paras,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, mem).unwrap();
        regs
    }

    fn umb_test_kernel() -> (DosKernel, Memory) {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        // Conventional program 0x0100-0x1100, then a 160 KiB UMB pool at 0xC800,
        // on a DOS=UMB box so AH=5803h may link it.
        kernel.init_program(0x0100, 0x1100, &mut mem).unwrap();
        kernel.set_umb_region(0xc800, 0x2800, &mut mem).unwrap();
        kernel.set_dos_umb(true);
        (kernel, mem)
    }

    #[test]
    fn ah48_high_strategy_allocates_from_the_upper_arena_when_linked() {
        let (mut kernel, mut mem) = umb_test_kernel();
        link_umbs(&mut kernel, &mut mem);
        set_alloc_strategy(&mut kernel, &mut mem, 0x0040); // high memory only
        let regs = dos_alloc(&mut kernel, &mut mem, 0x0100);
        assert!(!regs.cf, "the upper allocation succeeds");
        assert!(
            (0xc800..0xf000).contains(&regs.ax),
            "the block lands in the UMB window, got {:#06x}",
            regs.ax
        );
        // The upper arena now owns the block; the conventional free tail is intact.
        assert!(kernel.umb_chain(&mem).iter().any(|m| m.owner == regs.ax));
        assert_eq!(kernel.arena.free_base(&mem), 0x1100);
    }

    #[test]
    fn ah48_high_strategy_falls_back_to_conventional_when_unlinked() {
        let (mut kernel, mut mem) = umb_test_kernel();
        // Strategy is high, but UMBs are not linked: DOS allocates conventional.
        set_alloc_strategy(&mut kernel, &mut mem, 0x0040);
        let regs = dos_alloc(&mut kernel, &mut mem, 0x0100);
        assert!(!regs.cf);
        assert!(
            (0x0100..0xa000).contains(&regs.ax),
            "an unlinked high request stays in conventional memory"
        );
        assert!(kernel.umb_chain(&mem).iter().all(|m| m.owner == 0));
    }

    #[test]
    fn ah48_high_then_low_falls_back_when_upper_memory_is_full() {
        let (mut kernel, mut mem) = umb_test_kernel();
        link_umbs(&mut kernel, &mut mem);
        set_alloc_strategy(&mut kernel, &mut mem, 0x0080); // high then low
        // Drain the upper arena: ask for more than its ~160 KiB holds.
        let big = dos_alloc(&mut kernel, &mut mem, 0x2000);
        assert!(
            (0xc800..0xf000).contains(&big.ax),
            "first lands in upper memory"
        );
        // The next high-then-low request no longer fits up high and falls to low.
        let low = dos_alloc(&mut kernel, &mut mem, 0x1000);
        assert!(!low.cf);
        assert!(
            (0x0100..0xa000).contains(&low.ax),
            "the fallback allocation is conventional, got {:#06x}",
            low.ax
        );
    }

    #[test]
    fn ah49_and_ah4a_route_to_the_upper_arena_by_address() {
        let (mut kernel, mut mem) = umb_test_kernel();
        link_umbs(&mut kernel, &mut mem);
        set_alloc_strategy(&mut kernel, &mut mem, 0x0040);
        let seg = dos_alloc(&mut kernel, &mut mem, 0x0100).ax;
        // Resize the upper block up against its free tail.
        let mut regs = DosRegs {
            ax: 0x4a00,
            bx: 0x0400,
            es: seg,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf, "the upper resize succeeds");
        assert!(
            kernel
                .umb_chain(&mem)
                .iter()
                .any(|m| m.owner == seg && m.size == 0x0400)
        );
        // Free it: LIFO reclaim folds it back into the upper free tail.
        let mut regs = DosRegs {
            ax: 0x4900,
            es: seg,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf, "the upper free succeeds");
        // Back to a single free block spanning the whole pool.
        let chain = kernel.umb_chain(&mem);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].owner, 0);
        assert_eq!(chain[0].mcb_seg, 0xc800);
    }

    #[test]
    fn init_shell_base_resets_the_allocation_manager_state() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        kernel.init_program(0x0100, 0x1100, &mut mem).unwrap();
        kernel.set_umb_region(0xc800, 0x2800, &mut mem).unwrap();
        kernel.set_dos_umb(true);
        // A guest links UMBs and picks a high strategy.
        link_umbs(&mut kernel, &mut mem);
        set_alloc_strategy(&mut kernel, &mut mem, 0x0040);
        assert!(kernel.umb_link);
        // SYSINIT (a warm reboot's boot-base setup) clears both to the defaults.
        kernel.init_shell_base(&mut mem, 0x0100, &[]).unwrap();
        assert!(!kernel.umb_link, "a reboot unlinks UMBs");
        assert_eq!(kernel.alloc_strategy, 0, "a reboot resets the strategy");
    }

    #[test]
    fn ah48_high_then_low_double_failure_reports_the_global_largest() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        // A program filling almost all of conventional memory: a tiny free tail.
        kernel.init_program(0x0100, 0x9f00, &mut mem).unwrap();
        kernel.set_umb_region(0xc800, 0x2800, &mut mem).unwrap();
        kernel.set_dos_umb(true);
        link_umbs(&mut kernel, &mut mem);
        set_alloc_strategy(&mut kernel, &mut mem, 0x0080); // high then low
        // Request more than either arena holds: both fail. The larger free tail is
        // the upper arena's, so BX must report it, not the tiny conventional one.
        let regs = dos_alloc(&mut kernel, &mut mem, 0x3000);
        assert!(regs.cf, "neither arena can satisfy it");
        assert_eq!(regs.ax, 0x08);
        assert_eq!(regs.bx, 0x27ff, "BX is the upper arena's largest, not 0xff");
    }

    /// Write an ASCIIZ string at 0000:`off` and return DS/DX for it.
    fn put_asciiz(mem: &mut Memory, off: u16, text: &[u8]) -> (u16, u16) {
        for (i, &b) in text.iter().enumerate() {
            mem.write_u8(usize::from(off) + i, b).unwrap();
        }
        mem.write_u8(usize::from(off) + text.len(), 0).unwrap();
        (0, off)
    }

    #[test]
    fn emmxxxx0_opens_as_a_character_device_when_ems_is_present() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let (ds, dx) = put_asciiz(&mut mem, 0x2000, b"EMMXXXX0");

        // With no EMS the device is not openable (it falls through to a host-file
        // open, which fails), so a guest reads "no EMS".
        let mut regs = DosRegs {
            ax: 0x3d00,
            ds,
            dx,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf, "EMMXXXX0 does not open without EMS");

        // With EMS present the open succeeds, IOCTL reports a character device that
        // is ready, and the handle closes.
        kernel.set_ems_present(true);
        let mut regs = DosRegs {
            ax: 0x3d00,
            ds,
            dx,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf, "EMMXXXX0 opens when EMS is present");
        let handle = regs.ax;

        let mut regs = DosRegs {
            ax: 0x4400,
            bx: handle,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_ne!(
            regs.dx & 0x0080,
            0,
            "bit 7 ISDEV marks it a device, not a file"
        );

        let mut regs = DosRegs {
            ax: 0x4407,
            bx: handle,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.ax & 0xff, 0xff, "the device reports ready");

        let mut regs = DosRegs {
            ax: 0x3e00,
            bx: handle,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf, "the device handle closes");
    }

    #[test]
    fn emmxxxx0_heads_the_device_chain_only_when_present() {
        let read_chain_name = |present: bool| -> [u8; 8] {
            let mut kernel = DosKernel::new();
            let mut mem = Memory::new(1024 * 1024).unwrap();
            kernel.set_ems_present(present);
            let mut regs = DosRegs {
                ax: 0x5200,
                ..Default::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
            let sysvars = usize::from(regs.es) * 16 + usize::from(regs.bx);
            let nul = sysvars + 0x22; // NUL header at [BX+0x22]
            let next_off = mem.read_u16(nul).unwrap();
            let next_seg = mem.read_u16(nul + 2).unwrap();
            if (next_off, next_seg) == (0xffff, 0xffff) {
                return *b"\0\0\0\0\0\0\0\0"; // chain ends at NUL
            }
            let next = usize::from(next_seg) * 16 + usize::from(next_off);
            let mut name = [0u8; 8];
            for (i, slot) in name.iter_mut().enumerate() {
                *slot = mem.read_u8(next + 0x0a + i).unwrap();
            }
            name
        };
        assert_eq!(&read_chain_name(true), b"EMMXXXX0", "EMS chains after NUL");
        assert_eq!(
            &read_chain_name(false),
            b"\0\0\0\0\0\0\0\0",
            "no EMS leaves NUL ending the chain"
        );
    }

    #[test]
    fn an_open_emmxxxx0_handle_does_not_collide_with_a_created_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = DosKernel::new();
        kernel.mount_c(HostDrive::mount_c(dir.path()).unwrap());
        let mut mem = Memory::new(1024 * 1024).unwrap();
        kernel.set_ems_present(true);
        // Open the EMS device.
        let (ds, dx) = put_asciiz(&mut mem, 0x2000, b"EMMXXXX0");
        let mut regs = DosRegs {
            ax: 0x3d00,
            ds,
            dx,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        let ems_handle = regs.ax;
        // Create a file through a different handle-minting path (AH=3Ch).
        let (ds, dx) = put_asciiz(&mut mem, 0x3000, b"DATA.TXT");
        let mut regs = DosRegs {
            ax: 0x3c00,
            ds,
            dx,
            cx: 0,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf, "the file is created");
        let file_handle = regs.ax;
        assert_ne!(
            ems_handle, file_handle,
            "the EMS device and the file get distinct handles"
        );
        // IOCTL 4400h on the file must report a file (bit 7 clear), not the device.
        let mut regs = DosRegs {
            ax: 0x4400,
            bx: file_handle,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(
            regs.dx & 0x0080,
            0,
            "the file is not misreported as a character device"
        );
    }

    #[test]
    fn ah18_null_function_returns_al_zero() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut regs = DosRegs {
            ax: 0x18ff,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.ax & 0xff, 0);
    }

    #[test]
    fn ah0d_disk_reset_succeeds() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut regs = DosRegs {
            ax: 0x0d00,
            cf: true,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
    }

    #[test]
    fn ah68_commit_invalid_handle_errors() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut regs = DosRegs {
            ax: 0x6800,
            bx: 0x0099,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x06);
    }

    #[test]
    fn ah44_get_device_info_distinguishes_console_from_file() {
        // Console handle 1 (stdout): the ISDEV bit (7) is set.
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut regs = DosRegs {
            ax: 0x4400,
            bx: 1,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_ne!(regs.dx & 0x80, 0, "console is a character device");
        // A regular file handle: the ISDEV bit is clear.
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("DATA.TXT", b"hi")], r"C:\DATA.TXT");
        let handle = open(&mut kernel, &mut mem).ax;
        let mut regs = DosRegs {
            ax: 0x4400,
            bx: handle,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_eq!(regs.dx & 0x80, 0, "a file is not a character device");
    }

    #[test]
    fn ah44_get_device_info_invalid_handle_errors() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut regs = DosRegs {
            ax: 0x4400,
            bx: 50,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x06);
    }

    #[test]
    fn ah44_standard_handles_are_character_devices() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        for handle in 0u16..=4 {
            let mut regs = DosRegs {
                ax: 0x4400,
                bx: handle,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
            assert!(!regs.cf, "standard handle {handle} is valid");
            assert_ne!(regs.dx & 0x80, 0, "handle {handle} is a character device");
        }
    }

    #[test]
    fn ah44_output_status_reports_ready() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut regs = DosRegs {
            ax: 0x4407,
            bx: 1,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_eq!(regs.ax & 0xff, 0xff);
    }

    #[test]
    fn ah44_input_status_empty_console_is_not_ready() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut regs = DosRegs {
            ax: 0x4406,
            bx: 0,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_eq!(regs.ax & 0xff, 0x00);
    }

    #[test]
    fn ah57_gets_and_sets_a_file_timestamp() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("T.TXT", b"x")], r"C:\T.TXT");
        // Open read-write so the host permits set_modified.
        let mut regs = DosRegs {
            ax: 0x3d02,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        let handle = regs.ax;
        // 2021-07-15 13:45:30 (DOS packs seconds/2, so 30 -> 15).
        let date = ((2021u16 - 1980) << 9) | (7 << 5) | 15;
        let time = (13u16 << 11) | (45 << 5) | 15;
        let mut regs = DosRegs {
            ax: 0x5701,
            bx: handle,
            cx: time,
            dx: date,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        let mut regs = DosRegs {
            ax: 0x5700,
            bx: handle,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_eq!(regs.dx, date);
        assert_eq!(regs.cx, time);
    }

    #[test]
    fn days_from_civil_inverts_civil_from_days() {
        for &days in &[0i64, 3652, 10_000, 20_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn ah0a_blocks_when_the_line_is_incomplete() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let buf = 0x2000usize;
        mem.write_u8(buf, 8).unwrap();
        seed_keyboard_ring(&mut mem, b"ab").unwrap(); // no CR yet
        let mut regs = DosRegs {
            ax: 0x0a00,
            dx: buf as u16,
            ds: 0,
            ..Default::default()
        };
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::WaitForKey, "no CR -> block");
        // Supply the rest and re-enter: the count resumes from where it paused.
        seed_keyboard_ring(&mut mem, &[b'c', 0x0d]).unwrap();
        let mut regs = DosRegs {
            ax: 0x0a00,
            dx: buf as u16,
            ds: 0,
            ..Default::default()
        };
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::Continue);
        assert_eq!(mem.read_u8(buf + 1).unwrap(), 3, "abc");
    }

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
    fn ring_seed_then_dequeue_returns_bytes_in_order() {
        let mut mem = Memory::new(64 * 1024).unwrap();
        seed_keyboard_ring(&mut mem, b"hi").unwrap();
        assert!(!kbd_ring_is_empty(&mem).unwrap());
        assert_eq!(kbd_ring_dequeue(&mut mem).unwrap(), Some((0, b'h')));
        assert_eq!(kbd_ring_dequeue(&mut mem).unwrap(), Some((0, b'i')));
        assert_eq!(kbd_ring_dequeue(&mut mem).unwrap(), None);
        assert!(kbd_ring_is_empty(&mem).unwrap());
    }

    #[test]
    fn ah07_reads_without_echo() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        seed_keyboard_ring(&mut mem, b"x").unwrap();
        let mut regs = DosRegs {
            ax: 0x0700,
            ..Default::default()
        };
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::Continue);
        assert_eq!(regs.ax & 0xff, u16::from(b'x'));
        assert!(kernel.stdout().is_empty(), "AH=07h does not echo");
    }

    #[test]
    fn ah0b_reports_status_without_consuming() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut regs = DosRegs {
            ax: 0x0b00,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.zf, "empty ring -> ZF set, AL=0");
        assert_eq!(regs.ax & 0xff, 0);
        seed_keyboard_ring(&mut mem, b"y").unwrap();
        let mut regs = DosRegs {
            ax: 0x0b00,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.zf, "key waiting -> ZF clear, AL=0xFF");
        assert_eq!(regs.ax & 0xff, 0xff);
        assert!(!kbd_ring_is_empty(&mem).unwrap(), "AH=0Bh does not consume");
    }

    #[test]
    fn ah0c_flushes_then_reads_with_subfunction() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        seed_keyboard_ring(&mut mem, b"old").unwrap();
        let mut regs = DosRegs {
            ax: 0x0c01,
            ..Default::default()
        };
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(
            action,
            DosAction::WaitForKey,
            "flush emptied the ring, AL=01 blocks"
        );
        assert!(kbd_ring_is_empty(&mem).unwrap());
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
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut kernel = DosKernel::new();
        seed_keyboard_ring(&mut mem, input).unwrap();
        let mut regs = DosRegs {
            ax,
            dx,
            ..DosRegs::default()
        };
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::Continue);
        (regs, kernel.stdout().to_vec()) // DosRegs is Copy and holds the post-dispatch AX/ZF
    }

    /// Like char_io but returns the action so blocking-read tests can assert it.
    fn char_io_action(ax: u16, dx: u16, input: &[u8]) -> (DosAction, DosRegs, Vec<u8>) {
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut kernel = DosKernel::new();
        seed_keyboard_ring(&mut mem, input).unwrap();
        let mut regs = DosRegs {
            ax,
            dx,
            ..DosRegs::default()
        };
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        (action, regs, kernel.stdout().to_vec())
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
    fn ah01_on_empty_ring_blocks() {
        let (action, regs, out) = char_io_action(0x0100, 0, b""); // empty ring
        assert_eq!(action, DosAction::WaitForKey);
        assert_eq!(regs.ax, 0x0100); // AX unchanged so the retry re-reads AH=01h
        assert!(out.is_empty());
    }

    #[test]
    fn ah08_on_empty_ring_blocks() {
        let (action, regs, out) = char_io_action(0x0800, 0, b""); // empty ring, no echo
        assert_eq!(action, DosAction::WaitForKey);
        assert_eq!(regs.ax, 0x0800); // AX unchanged
        assert!(out.is_empty());
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
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut kernel = DosKernel::new();
        seed_keyboard_ring(&mut mem, b"X").unwrap();
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
    fn config_sys_file_and_buffer_counts_are_recorded() {
        let mut kernel = DosKernel::new();

        kernel.set_config_sys_counts(37, 12);

        assert_eq!(kernel.file_count(), 37);
        assert_eq!(kernel.buffer_count(), 12);
    }

    #[test]
    fn files_count_caps_dynamic_file_handles() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("DATA.TXT", b"hi")], r"C:\DATA.TXT");
        kernel.set_config_sys_counts(6, 20);

        let first = open(&mut kernel, &mut mem);
        assert!(!first.cf);
        assert_eq!(first.ax, 5);

        let second = open(&mut kernel, &mut mem);
        assert!(second.cf);
        assert_eq!(
            second.ax, 0x04,
            "FILES=6 leaves room for one dynamic handle"
        );
    }

    #[test]
    fn open_missing_file_sets_cf_and_ax02() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[], r"C:\NOPE.TXT");
        let regs = open(&mut kernel, &mut mem);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x02);
    }

    #[test]
    fn open_with_invalid_access_mode_returns_0x0c() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("DATA.TXT", b"hi")], r"C:\DATA.TXT");
        // AH=3Dh with the access bits = 3 (reserved). The open is rejected with the
        // invalid-access-code error even though the file exists.
        let mut regs = DosRegs {
            ax: 0x3df3, // AL=0xF3: reserved access nibble 3, sharing/inherit bits set
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x0c);
    }

    #[test]
    fn extended_open_with_invalid_access_mode_returns_0x0c() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("DATA.TXT", b"hi")], r"C:\DATA.TXT");
        // AH=6Ch takes the access mode in BL; bits = 5 are reserved. The path is at
        // DS:SI and open-if-exists is set, but the bad mode is rejected first.
        let mut regs = DosRegs {
            ax: 0x6c00,
            bx: 0x00c5, // BL=0xC5: reserved access nibble 5, high bits set
            dx: 0x0001,
            ds: 0x0100,
            si: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x0c);
    }

    #[test]
    fn ah45_dup_returns_the_next_free_handle() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("DATA.TXT", b"hi")], r"C:\DATA.TXT");
        assert_eq!(open(&mut kernel, &mut mem).ax, 5);
        let mut regs = DosRegs {
            ax: 0x4500,
            bx: 5,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_eq!(regs.ax, 6);
    }

    #[test]
    fn ah45_dup_invalid_handle_errors() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[], r"C:\X.TXT");
        let mut regs = DosRegs {
            ax: 0x4500,
            bx: 99,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x06);
    }

    #[test]
    fn ah46_dup2_forces_a_target_handle() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("DATA.TXT", b"hi")], r"C:\DATA.TXT");
        assert_eq!(open(&mut kernel, &mut mem).ax, 5);
        let mut regs = DosRegs {
            ax: 0x4600,
            bx: 5,
            cx: 9,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        // Handle 9 now aliases the file: closing it succeeds.
        let mut regs = DosRegs {
            ax: 0x3e00,
            bx: 9,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
    }

    #[test]
    fn ah5b_create_new_fails_if_the_file_exists() {
        let (mut kernel, mut mem, _dir) =
            kernel_with_drive(&[("EXISTS.TXT", b"x")], r"C:\EXISTS.TXT");
        let mut regs = DosRegs {
            ax: 0x5b00,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x50);
    }

    #[test]
    fn ah5b_create_new_succeeds_for_a_fresh_name() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[], r"C:\FRESH.TXT");
        let mut regs = DosRegs {
            ax: 0x5b00,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_eq!(regs.ax, 5);
    }

    #[test]
    fn ah43_chmod_reports_and_sets_readonly() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("F.TXT", b"x")], r"C:\F.TXT");
        let mut regs = DosRegs {
            ax: 0x4300,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_eq!(regs.cx & 0x20, 0x20); // archive
        assert_eq!(regs.cx & 0x01, 0x00); // not read-only yet
        let mut regs = DosRegs {
            ax: 0x4301,
            ds: 0x0100,
            dx: 0x0200,
            cx: 0x01,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        let mut regs = DosRegs {
            ax: 0x4300,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.cx & 0x01, 0x01); // read-only now set
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

    fn arena_kernel() -> (DosKernel, Memory) {
        // Conventional memory must span the whole arena (up to ARENA_TOP) now that
        // the MCB chain lives in guest RAM, not a shadow Vec.
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut kernel = DosKernel::new();
        // Program PSP at 0x0100, block top at 0x1100 (a .COM-style 64 KiB block).
        kernel.init_program(0x0100, 0x1100, &mut mem).unwrap();
        (kernel, mem)
    }

    #[test]
    fn ah48_allocates_above_the_program_block() {
        let (mut kernel, mut mem) = arena_kernel();
        let mut regs = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        // The header paragraph sits at prog_top (0x1100); the data segment is one higher.
        assert_eq!(regs.ax, 0x1101);
    }

    #[test]
    fn ah4a_shrink_program_block_then_ah48_allocates_the_tail() {
        let (mut kernel, mut mem) = arena_kernel();
        // Shrink the program block (ES = PSP) to 0x0800 paragraphs.
        let mut resize = DosRegs {
            ax: 0x4a00,
            es: 0x0100,
            bx: 0x0800,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut resize, &mut mem).unwrap();
        assert!(!resize.cf);
        // Now AH=48h allocates from the freed tail: free_base = 0x0100 + 0x0800 = 0x0900,
        // its MCB header lands there, and the data segment is one paragraph higher.
        let mut alloc = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut alloc, &mut mem).unwrap();
        assert!(!alloc.cf);
        assert_eq!(alloc.ax, 0x0901);
    }

    #[test]
    fn ah48_past_the_ceiling_returns_largest_available() {
        let (mut kernel, mut mem) = arena_kernel();
        // Request more than fits: free_base=0x1100, header at 0x1100, data starts at
        // 0x1101, ceiling 0xA000 -> largest data that fits is 0xA000 - 0x1101 = 0x8EFF.
        let mut regs = DosRegs {
            ax: 0x4800,
            bx: 0x9000,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x08);
        assert_eq!(regs.bx, 0x8eff); // 0xA000 - 0x1101 (header paragraph excluded)
    }

    #[test]
    fn ah4a_grow_program_block_too_big_fails_with_largest() {
        let (mut kernel, mut mem) = arena_kernel();
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
    fn ah4a_grow_program_past_a_leaked_hole_is_capped_by_the_owned_block() {
        // A leaked owner-0 hole sitting directly above the program must not let a
        // program grow run over a still-owned block further up the chain. The
        // ceiling is the lowest OWNED header, skipping holes, not the immediate
        // successor.
        let (mut kernel, mut mem) = arena_kernel(); // psp 0x100, prog_top 0x1100
        let mut a = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut a, &mut mem).unwrap();
        assert_eq!(a.ax, 0x1101);
        let mut b = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut b, &mut mem).unwrap();
        assert_eq!(b.ax, 0x1112);
        // Free A (non-top): it becomes a leaked owner-0 hole directly above the program.
        let mut free_a = DosRegs {
            ax: 0x4900,
            es: 0x1101,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut free_a, &mut mem).unwrap();
        assert!(!free_a.cf);
        // Grow the program: it is capped at B's header (0x1111), not ARENA_TOP.
        let mut grow = DosRegs {
            ax: 0x4a00,
            es: 0x0100,
            bx: 0x2000,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut grow, &mut mem).unwrap();
        assert!(grow.cf, "a grow over the owned block B must fail");
        assert_eq!(grow.ax, 0x08);
        assert_eq!(grow.bx, 0x1011, "largest = B header 0x1111 - psp 0x0100");
        // B (owner 0x1112) is still a live block in the chain, not clobbered.
        let mut q = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut q, &mut mem).unwrap();
        let first = mem
            .read_u16(usize::from(q.es) * 16 + usize::from(q.bx) - 2)
            .unwrap();
        assert!(
            read_mcb_chain(&mem, first)
                .iter()
                .any(|m| m.owner == 0x1112),
            "B survives the rejected grow"
        );
    }

    #[test]
    fn ah4a_shrink_an_exact_fill_block_opens_a_free_tail() {
        // An AH=48h allocation that exactly fills the arena becomes the last 'Z'
        // block (owned). Shrinking it must open a free tail, not panic on a
        // missing successor.
        let (mut kernel, mut mem) = arena_kernel(); // psp 0x100, prog_top 0x1100
        // Largest data that fits is 0xA000 - 0x1101 = 0x8EFF; this consumes the tail.
        let mut a = DosRegs {
            ax: 0x4800,
            bx: 0x8eff,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut a, &mut mem).unwrap();
        assert!(!a.cf);
        assert_eq!(a.ax, 0x1101);
        // Shrink it: succeeds and opens a free tail above it.
        let mut shrink = DosRegs {
            ax: 0x4a00,
            es: 0x1101,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut shrink, &mut mem).unwrap();
        assert!(!shrink.cf);
        // A fresh allocation lands in the freed tail: header 0x1111, data 0x1112.
        let mut b = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut b, &mut mem).unwrap();
        assert!(!b.cf);
        assert_eq!(b.ax, 0x1112);
    }

    #[test]
    fn ah4a_shrink_a_non_top_block_keeps_the_block_above_intact() {
        // Shrinking a non-top AH=48h block leaks a hole in the gap (no reclaim) and
        // must leave the owned block above it a valid, freeable block.
        let (mut kernel, mut mem) = arena_kernel();
        let mut a = DosRegs {
            ax: 0x4800,
            bx: 0x0020,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut a, &mut mem).unwrap();
        assert_eq!(a.ax, 0x1101);
        let mut b = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut b, &mut mem).unwrap();
        let b_seg = b.ax;
        // Shrink A (non-top) in place.
        let mut shrink = DosRegs {
            ax: 0x4a00,
            es: 0x1101,
            bx: 0x0008,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut shrink, &mut mem).unwrap();
        assert!(!shrink.cf);
        // B is still a valid owned block (freeing it succeeds).
        let mut free_b = DosRegs {
            ax: 0x4900,
            es: b_seg,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut free_b, &mut mem).unwrap();
        assert!(
            !free_b.cf,
            "the block above the shrunk non-top block survives"
        );
    }

    #[test]
    fn ah31_keep_resident_releases_a_block_above_the_program() {
        // The TSR pattern: allocate a block, then keep-resident trimming the program.
        // Everything above the resident block, including the AH=48h block, is
        // released into a single free tail at the trimmed program top.
        let (mut kernel, mut mem) = arena_kernel(); // psp 0x100, prog_top 0x1100
        let mut a = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut a, &mut mem).unwrap();
        assert_eq!(a.ax, 0x1101);
        let mut tsr = DosRegs {
            ax: 0x3100,
            dx: 0x0020,
            ..DosRegs::default()
        };
        let action = kernel.dispatch(0x21, &mut tsr, &mut mem).unwrap();
        assert!(matches!(action, DosAction::Exit(_)));
        // The free tail begins at the resident top 0x120; the released block's space
        // is reused.
        let mut next = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut next, &mut mem).unwrap();
        assert_eq!(
            next.ax, 0x0121,
            "free tail begins at the trimmed program top"
        );
    }

    #[test]
    fn ah49_frees_top_block_lifo_then_reuses() {
        let (mut kernel, mut mem) = arena_kernel();
        let mut a = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut a, &mut mem).unwrap();
        let seg = a.ax; // data segment 0x1101 (header at 0x1100)
        let mut free = DosRegs {
            ax: 0x4900,
            es: seg,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut free, &mut mem).unwrap();
        assert!(!free.cf);
        // The next allocation reuses the reclaimed header and data paragraphs.
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
        let (mut kernel, mut mem) = arena_kernel();
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
        let (mut kernel, mut mem) = arena_kernel(); // psp 0x100, prog_top 0x1100
        let mut a = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut a, &mut mem).unwrap();
        // Header at 0x1100, data at 0x1101; free_base ends at 0x1111.
        assert_eq!(a.ax, 0x1101);
        let mut b = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut b, &mut mem).unwrap();
        // B's header at 0x1111, data at 0x1112; free_base ends at 0x1122.
        assert_eq!(b.ax, 0x1112);
        // Free the lower block A (non-top): the hole is not reclaimed.
        let mut free_a = DosRegs {
            ax: 0x4900,
            es: 0x1101,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut free_a, &mut mem).unwrap();
        assert!(!free_a.cf);
        // Free the top block B: reclaims its data plus header, no underflow.
        let mut free_b = DosRegs {
            ax: 0x4900,
            es: 0x1112,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut free_b, &mut mem).unwrap();
        assert!(!free_b.cf);
        // A fresh allocation reuses B's reclaimed span: header at 0x1111, data at
        // 0x1112; A's hole at 0x1101 is leaked, as documented.
        let mut c = DosRegs {
            ax: 0x4800,
            bx: 0x0008,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut c, &mut mem).unwrap();
        assert_eq!(c.ax, 0x1112);
    }

    #[test]
    fn ah48_zero_paragraphs_reserves_only_its_header() {
        // A zero-paragraph allocation is a legal DOS request: it still carries an
        // MCB header, so it returns a data segment one paragraph above free_base and
        // advances free_base past that single header paragraph.
        let (mut kernel, mut mem) = arena_kernel();
        let mut z = DosRegs {
            ax: 0x4800,
            bx: 0x0000,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut z, &mut mem).unwrap();
        assert!(!z.cf);
        assert_eq!(z.ax, 0x1101); // header at 0x1100, data at 0x1101
        let mut a = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut a, &mut mem).unwrap();
        // free_base advanced by the zero block's header, so the next data segment is
        // one higher again: header at 0x1101, data at 0x1102.
        assert_eq!(a.ax, 0x1102);
    }

    /// Walk the materialized MCB chain from a first-MCB segment, returning the
    /// (sig, owner, size) of each block until a 'Z' header (or a stop after a
    /// generous bound so a corrupt chain cannot loop forever).
    fn walk_mcb_chain(mem: &Memory, first: u16) -> Vec<(u8, u16, u16)> {
        let mut out = Vec::new();
        let mut seg = first;
        for _ in 0..64 {
            let base = usize::from(seg) * 16;
            let sig = mem.read_u8(base).unwrap();
            let owner = mem.read_u16(base + 1).unwrap();
            let size = mem.read_u16(base + 3).unwrap();
            out.push((sig, owner, size));
            if sig == b'Z' {
                break;
            }
            // Next MCB is at this header's data + size; data is seg+1.
            seg = seg.wrapping_add(1).wrapping_add(size);
        }
        out
    }

    #[test]
    fn ah52_mcb_chain_walk_sums_to_arena_and_ends_in_z() {
        // arena_kernel: psp 0x0100, prog_top 0x1100, free_base 0x1100. The chain is
        // the program block, then the free remainder; sigs M..Z, sizes cover the
        // arena from psp_seg to ARENA_TOP.
        let (mut kernel, mut mem) = arena_kernel();
        let mut regs = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        // ES:BX-2 holds the first MCB segment.
        let ptr = usize::from(regs.es) * 16 + usize::from(regs.bx);
        let first = mem.read_u16(ptr - 2).unwrap();
        assert_eq!(first, 0x0100 - 1, "first MCB is psp_seg-1");
        let chain = walk_mcb_chain(&mem, first);
        // Two blocks: program (M, owner 0x0100) then free (Z, owner 0).
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].0, b'M');
        assert_eq!(chain[0].1, 0x0100, "program block owner = PSP");
        assert_eq!(chain[1].0, b'Z', "last block is Z");
        assert_eq!(chain[1].1, 0, "free block owner 0");
        // The two data blocks plus their two header paragraphs span psp_seg-1
        // (0xFF) up to ARENA_TOP (0xA000): 2 headers + sum of sizes.
        let total: u32 = chain.iter().map(|&(_, _, s)| u32::from(s)).sum();
        assert_eq!(
            total + chain.len() as u32,
            u32::from(ARENA_TOP) - (0x0100 - 1),
            "headers + data fill the arena"
        );
    }

    #[test]
    fn ah52_mcb_chain_reflects_an_allocation() {
        // After an AH=48h allocation, the chain has three blocks: program, the new
        // block (owner = its own segment), and the free remainder.
        let (mut kernel, mut mem) = arena_kernel();
        let mut alloc = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut alloc, &mut mem).unwrap();
        let new_seg = alloc.ax; // data segment 0x1101 (header at 0x1100)
        // Drop a sentinel into the allocated block's data. Materializing the chain
        // must not overwrite it: the block's MCB header lives one paragraph below.
        let sentinel_addr = usize::from(new_seg) * 16;
        mem.write_u8(sentinel_addr, 0xa5).unwrap();
        let mut regs = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        let ptr = usize::from(regs.es) * 16 + usize::from(regs.bx);
        let first = mem.read_u16(ptr - 2).unwrap();
        let chain = walk_mcb_chain(&mem, first);
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[1].1, new_seg, "AH=48h block owned by its segment");
        assert_eq!(chain[1].2, 0x0010, "block size in paragraphs");
        assert_eq!(chain[2].0, b'Z');
        // The chain reports the block's data at mcb_seg+1. The block MCB sits just
        // past the program MCB (first + 1 + program size), so the data segment the
        // chain advertises must equal what AH=48h handed the guest.
        let block_mcb = first.wrapping_add(1).wrapping_add(chain[0].2);
        assert_eq!(
            block_mcb.wrapping_add(1),
            new_seg,
            "chain reports the AH=48h data segment, not one paragraph above it"
        );
        // The sentinel survives: no 'M'/'Z' header was written over the guest data.
        assert_eq!(
            mem.read_u8(sentinel_addr).unwrap(),
            0xa5,
            "materialize_mcb_chain must not clobber the allocated block's data"
        );
    }

    #[test]
    fn mcb_chain_is_authoritative_a_guest_edit_drives_the_allocator() {
        // THE GATE for the authoritative flip. The in-RAM MCB chain is the source
        // of truth, not a shadow Vec. A guest (a memory manager) rewrites the chain
        // in place: it shrinks the program block and lays a fresh free-tail header
        // at the new boundary. Two things must then hold: a re-query does not
        // clobber the edit, and the NEXT AH=48h carves from the guest's boundary.
        // That second check is what forces a real flip: freezing the materialize
        // alone would survive the re-query but still allocate from the stale shadow.
        let (mut kernel, mut mem) = arena_kernel(); // psp 0x100, prog_top 0x1100

        // First AH=52h: the chain lands in RAM; ES:BX-2 holds the first MCB segment.
        let mut q1 = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut q1, &mut mem).unwrap();
        let first = mem
            .read_u16(usize::from(q1.es) * 16 + usize::from(q1.bx) - 2)
            .unwrap();
        assert_eq!(first, 0x00ff, "first MCB is psp_seg-1");

        // A guest shrinks the program block to 0x0800 paragraphs and writes a new
        // free-tail header at the new boundary 0x0900, a consistent in-place edit.
        mem.write_u16(usize::from(first) * 16 + 3, 0x0800).unwrap();
        let new_tail = 0x0900usize;
        mem.write_u8(new_tail * 16, b'Z').unwrap();
        mem.write_u16(new_tail * 16 + 1, 0).unwrap(); // owner 0 (free)
        mem.write_u16(new_tail * 16 + 3, ARENA_TOP - 0x0900 - 1)
            .unwrap();

        // Re-query: the edit survives, the chain is not re-materialized over.
        let mut q2 = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut q2, &mut mem).unwrap();
        assert_eq!(
            mem.read_u16(usize::from(first) * 16 + 3).unwrap(),
            0x0800,
            "AH=52h must not clobber a guest edit to the MCB header"
        );

        // The allocator reads the guest's chain: AH=48h carves from the free tail
        // the guest placed at 0x0900, so the data segment is 0x0901, not 0x1101.
        let mut alloc = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut alloc, &mut mem).unwrap();
        assert!(!alloc.cf);
        assert_eq!(
            alloc.ax, 0x0901,
            "AH=48h carves from the guest-edited free tail, not the shadow"
        );
    }

    #[test]
    fn read_mcb_chain_reconstructs_the_chain_and_reflects_a_guest_edit() {
        let (mut kernel, mut mem) = arena_kernel();
        // AH=52h materializes the MCB chain in guest RAM and returns the first MCB.
        let mut regs = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        let ptr = usize::from(regs.es) * 16 + usize::from(regs.bx);
        let first = mem.read_u16(ptr - 2).unwrap();

        // Reading the chain back reconstructs the program block then the free
        // remainder, ending in a 'Z' header with 'M' links before it.
        let chain = read_mcb_chain(&mem, first);
        assert!(chain.len() >= 2, "program block + free remainder");
        assert_eq!(chain[0].mcb_seg, first, "the walk starts at the first MCB");
        assert_eq!(chain[0].owner, 0x0100, "program block owner = PSP");
        assert_eq!(chain.last().unwrap().sig, b'Z', "the chain ends in Z");
        assert!(
            chain[..chain.len() - 1].iter().all(|m| m.sig == b'M'),
            "every block before the last is an M link"
        );

        // Hand-edit the owner word of the first header directly in guest RAM. The
        // reader reflects it: the chain in memory is the source of truth, not a
        // shadow copy. This is the property the authoritative allocator relies on.
        mem.write_u16(usize::from(first) * 16 + 1, 0x1234).unwrap();
        let edited = read_mcb_chain(&mem, first);
        assert_eq!(
            edited[0].owner, 0x1234,
            "the reader observes the edited owner word"
        );
    }

    #[test]
    fn read_mcb_chain_steps_over_an_intermediate_block() {
        let (mut kernel, mut mem) = arena_kernel();
        // An AH=48h allocation puts a middle 'M' link between the program block
        // and the free remainder, exercising the reader's next-MCB stepping.
        let mut alloc = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut alloc, &mut mem).unwrap();
        let block_seg = alloc.ax;

        let mut regs = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        let ptr = usize::from(regs.es) * 16 + usize::from(regs.bx);
        let first = mem.read_u16(ptr - 2).unwrap();

        let chain = read_mcb_chain(&mem, first);
        assert_eq!(chain.len(), 3, "program + AH=48h block + free remainder");
        assert_eq!(chain[1].sig, b'M', "the middle block is a link");
        assert_eq!(
            chain[1].owner, block_seg,
            "the AH=48h block is owned by its own data segment"
        );
        assert_eq!(chain[1].size, 0x0010, "the block's size in paragraphs");
        assert_eq!(chain[2].sig, b'Z', "the free remainder ends the chain");
    }

    #[test]
    fn ah52_publishes_sysvars_scalar_fields_and_the_nul_device() {
        let (mut kernel, mut mem) = arena_kernel();
        let mut regs = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        let bx = usize::from(regs.bx);
        let base = usize::from(regs.es) * 16 + bx;

        // The first-MCB pointer at [BX-2] is still published (the existing contract).
        assert_eq!(
            mem.read_u16(base - 2).unwrap(),
            0x0100 - 1,
            "first MCB at [BX-2]"
        );
        // [BX+0x10] max bytes per block = a 512-byte sector.
        assert_eq!(
            mem.read_u16(base + 0x10).unwrap(),
            512,
            "max bytes per block"
        );
        // [BX+0x21] LASTDRIVE = E:.
        assert_eq!(mem.read_u8(base + 0x21).unwrap(), 5, "LASTDRIVE");

        // [BX+0x22] the NUL device header heads the driver chain.
        let nul = base + 0x22;
        assert_eq!(
            mem.read_u16(nul).unwrap(),
            0xffff,
            "NUL next link offset is FFFF (end of chain)"
        );
        assert_eq!(
            mem.read_u16(nul + 2).unwrap(),
            0xffff,
            "NUL next link segment"
        );
        assert_eq!(
            mem.read_u16(nul + 4).unwrap(),
            0x8004,
            "NUL attribute: char device + NUL bit"
        );
        assert_eq!(
            mem.read_u16(nul + 6).unwrap(),
            0xffff,
            "NUL strategy entry (none)"
        );
        assert_eq!(
            mem.read_u16(nul + 8).unwrap(),
            0xffff,
            "NUL interrupt entry (none)"
        );
        let name: Vec<u8> = (0..8)
            .map(|i| mem.read_u8(nul + 0x0a + i).unwrap())
            .collect();
        assert_eq!(&name, b"NUL     ", "NUL device name");

        // A parked chain pointer (the first DPB at [BX+0]) stays zero for now.
        assert_eq!(
            mem.read_u16(base).unwrap(),
            0,
            "the unmodeled DPB pointer is left zero"
        );
    }

    #[test]
    fn critical_error_response_decodes_low_two_bits() {
        assert_eq!(
            CriticalErrorResponse::from_al(0),
            CriticalErrorResponse::Ignore
        );
        assert_eq!(
            CriticalErrorResponse::from_al(1),
            CriticalErrorResponse::Retry
        );
        assert_eq!(
            CriticalErrorResponse::from_al(2),
            CriticalErrorResponse::Abort
        );
        assert_eq!(
            CriticalErrorResponse::from_al(3),
            CriticalErrorResponse::Fail
        );
        // High bits ignored: 0x07 aliases to Fail, 0x05 to Retry.
        assert_eq!(
            CriticalErrorResponse::from_al(0x07),
            CriticalErrorResponse::Fail
        );
        assert_eq!(
            CriticalErrorResponse::from_al(0x05),
            CriticalErrorResponse::Retry
        );
    }

    #[test]
    fn psp_saves_int24_vector_consistent_with_ivt() {
        // Install an INT 24h vector in the IVT, build a PSP, and confirm PSP:0x12
        // mirrors it (segment,offset) and psp_saved_vector reads it back.
        let mut mem = Memory::new(64 * 1024).unwrap();
        // IVT entry 0x24 = offset 0xBEEF, segment 0xF000.
        mem.write_u16(0x24 * 4, 0xbeef).unwrap();
        mem.write_u16(0x24 * 4 + 2, 0xf000).unwrap();
        build_psp(&mut mem, 0x0100, 0x1100).unwrap();
        let psp = 0x0100usize * 16;
        assert_eq!(mem.read_u16(psp + 0x12).unwrap(), 0xbeef, "PSP offset");
        assert_eq!(mem.read_u16(psp + 0x14).unwrap(), 0xf000, "PSP segment");
        let (seg, off) = psp_saved_vector(&mem, 0x0100, 0x12).unwrap();
        assert_eq!((seg, off), (0xf000, 0xbeef));
    }

    #[test]
    fn ah1a_2f_dta_round_trips_with_default_at_psp_0x80() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut kernel = DosKernel::new();
        kernel.init_program(0x0100, 0x1100, &mut mem).unwrap();
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
    fn ah40_to_aux_and_prn_accept_and_discard() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let buf = 0x2000usize;
        for (i, b) in b"data".iter().enumerate() {
            mem.write_u8(buf + i, *b).unwrap();
        }
        for handle in [3u16, 4] {
            let mut regs = DosRegs {
                ax: 0x4000,
                bx: handle,
                cx: 4,
                ds: 0,
                dx: buf as u16,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
            assert!(!regs.cf, "AUX/PRN write does not error");
            assert_eq!(regs.ax, 4, "all bytes reported written");
        }
        // CX=0 to a device reports zero bytes and does not error (no truncation,
        // which is the file-handle behavior).
        let mut zero = DosRegs {
            ax: 0x4000,
            bx: 3,
            cx: 0,
            ds: 0,
            dx: buf as u16,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut zero, &mut mem).unwrap();
        assert!(!zero.cf);
        assert_eq!(zero.ax, 0, "CX=0 device write reports zero bytes");
        assert!(
            kernel.stdout().is_empty(),
            "AUX/PRN output is not echoed to the console"
        );
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
    fn ah42_seek_before_start_wraps_without_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("S.TXT"), b"01234").unwrap(); // 5 bytes
        let mut kernel = DosKernel::new();
        kernel.mount_c(HostDrive::mount_c(dir.path()).unwrap());
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let name_base = 0x0100usize * 16 + 0x0200;
        for (i, b) in r"C:\S.TXT".bytes().enumerate() {
            mem.write_u8(name_base + i, b).unwrap();
        }
        mem.write_u8(name_base + r"C:\S.TXT".len(), 0).unwrap();
        let mut open = DosRegs {
            ax: 0x3d00,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut open, &mut mem).unwrap();
        let handle = open.ax;

        // END - 10 on a 5-byte file is position -5. DOS reports it as 0xFFFFFFFB
        // with no error rather than failing the seek.
        let neg = (-10i32) as u32;
        let mut s = DosRegs {
            ax: 0x4202,
            bx: handle,
            cx: (neg >> 16) as u16,
            dx: neg as u16,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut s, &mut mem).unwrap();
        assert!(!s.cf, "a before-start seek does not error");
        assert_eq!(
            (u32::from(s.dx) << 16) | u32::from(s.ax),
            0xFFFF_FFFB,
            "5 - 10 = -5 wraps to the 32-bit pointer"
        );

        // A read at that wrapped (past-EOF) position returns no bytes, the HLE's
        // stand-in for DOS's failed I/O before the start of the file.
        let mut rd = DosRegs {
            ax: 0x3f00,
            bx: handle,
            cx: 4,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut rd, &mut mem).unwrap();
        assert!(!rd.cf);
        assert_eq!(
            rd.ax, 0,
            "a read at a before-start position returns 0 bytes"
        );
    }

    #[test]
    fn dir_ops_mkdir_chdir_rename_delete_with_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = DosKernel::new();
        kernel.mount_c(HostDrive::mount_c(dir.path()).unwrap());
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let seg = 0x0100u16;
        let p1 = 0x0200usize;
        let p2 = 0x0300usize;
        let buf = 0x0400usize;
        let put = |mem: &mut Memory, off: usize, s: &str| {
            for (i, b) in s.bytes().enumerate() {
                mem.write_u8(off + i, b).unwrap();
            }
            mem.write_u8(off + s.len(), 0).unwrap();
        };
        let base = |off: usize| usize::from(seg) * 16 + off;
        let call = |kernel: &mut DosKernel, mem: &mut Memory, regs: DosRegs| {
            let mut regs = regs;
            kernel.dispatch(0x21, &mut regs, mem).unwrap();
            regs
        };

        // MKDIR C:\DOS (drive-qualified, absolute).
        put(&mut mem, base(p1), r"C:\DOS");
        let r = call(
            &mut kernel,
            &mut mem,
            DosRegs {
                ax: 0x3900,
                ds: seg,
                dx: p1 as u16,
                ..DosRegs::default()
            },
        );
        assert!(!r.cf, "mkdir");
        assert!(dir.path().join("DOS").is_dir());

        // CHDIR DOS (relative), then get-cwd returns "DOS" with no leading slash.
        put(&mut mem, base(p1), "DOS");
        let r = call(
            &mut kernel,
            &mut mem,
            DosRegs {
                ax: 0x3b00,
                ds: seg,
                dx: p1 as u16,
                ..DosRegs::default()
            },
        );
        assert!(!r.cf, "chdir");
        call(
            &mut kernel,
            &mut mem,
            DosRegs {
                ax: 0x4700,
                ds: seg,
                si: buf as u16,
                ..DosRegs::default()
            },
        );
        let mut cwd = String::new();
        let mut i = 0;
        loop {
            let byte = mem.read_u8(base(buf) + i).unwrap();
            if byte == 0 {
                break;
            }
            cwd.push(byte as char);
            i += 1;
        }
        assert_eq!(cwd, "DOS");

        // Create a file with a relative name: it lands inside C:\DOS.
        put(&mut mem, base(p1), "A.TXT");
        let r = call(
            &mut kernel,
            &mut mem,
            DosRegs {
                ax: 0x3c00,
                ds: seg,
                dx: p1 as u16,
                ..DosRegs::default()
            },
        );
        assert!(!r.cf);
        assert!(dir.path().join("DOS").join("A.TXT").exists());

        // RENAME A.TXT -> B.TXT (both relative to the current directory).
        put(&mut mem, base(p1), "A.TXT");
        put(&mut mem, base(p2), "B.TXT");
        let r = call(
            &mut kernel,
            &mut mem,
            DosRegs {
                ax: 0x5600,
                ds: seg,
                dx: p1 as u16,
                es: seg,
                di: p2 as u16,
                ..DosRegs::default()
            },
        );
        assert!(!r.cf, "rename");
        assert!(dir.path().join("DOS").join("B.TXT").exists());
        assert!(!dir.path().join("DOS").join("A.TXT").exists());

        // DELETE B.TXT.
        put(&mut mem, base(p1), "B.TXT");
        let r = call(
            &mut kernel,
            &mut mem,
            DosRegs {
                ax: 0x4100,
                ds: seg,
                dx: p1 as u16,
                ..DosRegs::default()
            },
        );
        assert!(!r.cf, "delete");
        assert!(!dir.path().join("DOS").join("B.TXT").exists());

        // CHDIR .. back to the root, then RMDIR the now-empty C:\DOS.
        put(&mut mem, base(p1), "..");
        call(
            &mut kernel,
            &mut mem,
            DosRegs {
                ax: 0x3b00,
                ds: seg,
                dx: p1 as u16,
                ..DosRegs::default()
            },
        );
        put(&mut mem, base(p1), r"C:\DOS");
        let r = call(
            &mut kernel,
            &mut mem,
            DosRegs {
                ax: 0x3a00,
                ds: seg,
                dx: p1 as u16,
                ..DosRegs::default()
            },
        );
        assert!(!r.cf, "rmdir");
        assert!(!dir.path().join("DOS").exists());

        // AH=36h reports a valid C: drive with plausible geometry.
        let r = call(
            &mut kernel,
            &mut mem,
            DosRegs {
                ax: 0x3600,
                dx: 3,
                ..DosRegs::default()
            },
        );
        assert_ne!(r.ax, 0xffff);
        assert_eq!(r.cx, 512);
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
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut kernel = DosKernel::new();
        kernel.mount_c(HostDrive::mount_c(dir).unwrap());
        kernel.init_program(0x0100, 0x0200, &mut mem).unwrap();
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

    // --- FCB API ---

    /// A kernel mounted on a temp C: holding `files`, plus a 1 MiB memory and the
    /// kept-alive tempdir. The default DTA is PSP:0x80; tests set it explicitly.
    fn fcb_kernel(files: &[(&str, &[u8])]) -> (DosKernel, Memory, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        for (name, contents) in files {
            std::fs::write(dir.path().join(name), contents).unwrap();
        }
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut kernel = DosKernel::new();
        kernel.mount_c(HostDrive::mount_c(dir.path()).unwrap());
        kernel.init_program(0x0100, 0x1100, &mut mem).unwrap(); // arena + DTA seed
        (kernel, mem, dir)
    }

    /// Write a drive byte and a blank-padded 8.3 name into the FCB at 0x0100:0x0200.
    /// `name` is "STEM.EXT" or "STEM"; the fields beyond the name are left as the
    /// caller seeded them.
    fn place_fcb(mem: &mut Memory, drive: u8, name: &str) {
        let base = 0x0100usize * 16 + 0x0200;
        mem.write_u8(base, drive).unwrap();
        let (stem, ext) = match name.split_once('.') {
            Some((s, e)) => (s, e),
            None => (name, ""),
        };
        for i in 0..8 {
            let b = stem.as_bytes().get(i).copied().unwrap_or(b' ');
            mem.write_u8(base + 0x01 + i, b).unwrap();
        }
        for i in 0..3 {
            let b = ext.as_bytes().get(i).copied().unwrap_or(b' ');
            mem.write_u8(base + 0x09 + i, b).unwrap();
        }
    }

    fn fcb_call(kernel: &mut DosKernel, mem: &mut Memory, ah: u16) -> DosRegs {
        let mut regs = DosRegs {
            ax: ah << 8,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, mem).unwrap();
        regs
    }

    #[test]
    fn fcb_open_fills_the_record_fields_and_succeeds() {
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[("DATA.BIN", &[0u8; 300])]);
        place_fcb(&mut mem, 3, "DATA.BIN");
        let regs = fcb_call(&mut kernel, &mut mem, 0x0f);
        assert_eq!(regs.ax & 0xff, 0x00, "open succeeds");
        let base = 0x0100usize * 16 + 0x0200;
        assert_eq!(mem.read_u16(base + 0x0e).unwrap(), 128, "record size 128");
        assert_eq!(mem.read_u32(base + 0x10).unwrap(), 300, "file size");
        assert_eq!(mem.read_u16(base + 0x0c).unwrap(), 0, "current block 0");
        assert_eq!(mem.read_u8(base + 0x20).unwrap(), 0, "current record 0");
    }

    #[test]
    fn fcb_open_missing_file_returns_ff() {
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[]);
        place_fcb(&mut mem, 3, "NOPE.DAT");
        let regs = fcb_call(&mut kernel, &mut mem, 0x0f);
        assert_eq!(regs.ax & 0xff, 0xff);
    }

    #[test]
    fn fcb_sequential_read_walks_records_to_eof() {
        // A 200-byte file: record 0 is full (128 bytes), record 1 is a 72-byte
        // partial, record 2 is EOF. Read into the DTA at 0x0500:0x0000.
        let mut data = vec![0u8; 200];
        for (i, b) in data.iter_mut().enumerate() {
            *b = i as u8;
        }
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[("FILE.BIN", &data)]);
        place_fcb(&mut mem, 3, "FILE.BIN");
        // Point the DTA somewhere clear of the FCB.
        let mut set_dta = DosRegs {
            ax: 0x1a00,
            ds: 0x0500,
            dx: 0x0000,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set_dta, &mut mem).unwrap();
        // Open, then read the first record.
        assert_eq!(fcb_call(&mut kernel, &mut mem, 0x0f).ax & 0xff, 0x00);
        let dta = 0x0500usize * 16;
        let read1 = fcb_call(&mut kernel, &mut mem, 0x14);
        assert_eq!(read1.ax & 0xff, 0x00, "first record is full");
        assert_eq!(mem.read_u8(dta).unwrap(), 0);
        assert_eq!(mem.read_u8(dta + 127).unwrap(), 127);
        // The current record advanced to 1.
        let base = 0x0100usize * 16 + 0x0200;
        assert_eq!(mem.read_u8(base + 0x20).unwrap(), 1);
        // Second record: a 72-byte partial (AL=03).
        let read2 = fcb_call(&mut kernel, &mut mem, 0x14);
        assert_eq!(read2.ax & 0xff, 0x03, "partial final record");
        assert_eq!(mem.read_u8(dta).unwrap(), 128); // byte 128 of the file
        assert_eq!(mem.read_u8(dta + 71).unwrap(), 199); // last byte
        // Third read: EOF (AL=01).
        let read3 = fcb_call(&mut kernel, &mut mem, 0x14);
        assert_eq!(read3.ax & 0xff, 0x01, "EOF");
    }

    #[test]
    fn fcb_create_then_sequential_write_persists_a_record() {
        let (mut kernel, mut mem, dir) = fcb_kernel(&[]);
        place_fcb(&mut mem, 3, "OUT.BIN");
        // Create the file (AH=16h): AL=00 and the FCB is set up for writes.
        assert_eq!(fcb_call(&mut kernel, &mut mem, 0x16).ax & 0xff, 0x00);
        assert!(dir.path().join("OUT.BIN").exists());
        // Stage a 128-byte record in the DTA at 0x0500:0x0000, then write it.
        let mut set_dta = DosRegs {
            ax: 0x1a00,
            ds: 0x0500,
            dx: 0x0000,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set_dta, &mut mem).unwrap();
        let dta = 0x0500usize * 16;
        for i in 0..128usize {
            mem.write_u8(dta + i, (i as u8) ^ 0x5a).unwrap();
        }
        let write = fcb_call(&mut kernel, &mut mem, 0x15);
        assert_eq!(write.ax & 0xff, 0x00, "write succeeds");
        // The host file now holds the 128-byte record.
        let written = std::fs::read(dir.path().join("OUT.BIN")).unwrap();
        assert_eq!(written.len(), 128);
        assert_eq!(written[0], 0x5a);
        assert_eq!(written[127], 127u8 ^ 0x5a);
        // The current record advanced to 1.
        let base = 0x0100usize * 16 + 0x0200;
        assert_eq!(mem.read_u8(base + 0x20).unwrap(), 1);
    }

    #[test]
    fn fcb_delete_removes_matching_files_with_wildcards() {
        let (mut kernel, mut mem, dir) =
            fcb_kernel(&[("A.DAT", b"x"), ("B.DAT", b"y"), ("KEEP.TXT", b"z")]);
        place_fcb(&mut mem, 3, "????????.DAT");
        let regs = fcb_call(&mut kernel, &mut mem, 0x13);
        assert_eq!(regs.ax & 0xff, 0x00, "at least one deleted");
        assert!(!dir.path().join("A.DAT").exists());
        assert!(!dir.path().join("B.DAT").exists());
        assert!(dir.path().join("KEEP.TXT").exists(), "non-match kept");
    }

    // The DTA the FCB helpers use: init_program sets it to PSP:0x80 = 0x1080.
    const FCB_DTA: usize = 0x0100 * 16 + 0x80;

    #[test]
    fn fcb_find_first_and_next_enumerate_txt_files() {
        let (mut kernel, mut mem, _dir) =
            fcb_kernel(&[("A.TXT", b"aa"), ("B.TXT", b"bbb"), ("C.DAT", b"c")]);
        place_fcb(&mut mem, 0, "????????.TXT");

        // Read-dir order is filesystem-dependent, so collect both names as a set.
        let stem = |mem: &Memory| -> String {
            (0..8)
                .map(|i| mem.read_u8(FCB_DTA + 0x01 + i).unwrap() as char)
                .collect::<String>()
                .trim_end()
                .to_string()
        };

        let r1 = fcb_call(&mut kernel, &mut mem, 0x11);
        assert_eq!(r1.ax & 0xff, 0x00, "first .TXT match found");
        assert_eq!(mem.read_u8(FCB_DTA).unwrap(), 3, "drive C: = 3");
        let ext: Vec<u8> = (0..3)
            .map(|i| mem.read_u8(FCB_DTA + 0x09 + i).unwrap())
            .collect();
        assert_eq!(&ext, b"TXT", "the extension field is TXT");
        let first = stem(&mem);

        let r2 = fcb_call(&mut kernel, &mut mem, 0x12);
        assert_eq!(r2.ax & 0xff, 0x00, "second .TXT match found");
        let second = stem(&mem);

        let mut got = [first, second];
        got.sort();
        assert_eq!(
            got,
            ["A".to_string(), "B".to_string()],
            "both .TXT files, distinct records"
        );

        let r3 = fcb_call(&mut kernel, &mut mem, 0x12);
        assert_eq!(r3.ax & 0xff, 0xff, "only two .TXT files, then exhausted");
    }

    #[test]
    fn fcb_find_first_fills_name_attribute_and_size() {
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[("DATA.BIN", &[0u8; 100])]);
        place_fcb(&mut mem, 0, "DATA.BIN");

        let r = fcb_call(&mut kernel, &mut mem, 0x11);
        assert_eq!(r.ax & 0xff, 0x00);
        let name: Vec<u8> = (0..8)
            .map(|i| mem.read_u8(FCB_DTA + 0x01 + i).unwrap())
            .collect();
        assert_eq!(&name, b"DATA    ", "8-char blank-padded name");
        assert_eq!(
            mem.read_u8(FCB_DTA + 0x0c).unwrap(),
            0x00,
            "normal file attribute"
        );
        assert_eq!(
            mem.read_u32(FCB_DTA + 0x1d).unwrap(),
            100,
            "file size dword"
        );
        assert_eq!(
            mem.read_u16(FCB_DTA + 0x1b).unwrap(),
            0,
            "starting cluster is the no-FAT placeholder"
        );
        // The 10 reserved bytes between the attribute and the time stay zero, which
        // also pins that the attribute, time, date, cluster, and size offsets do not
        // overlap.
        for off in 0x0d..0x17 {
            assert_eq!(
                mem.read_u8(FCB_DTA + off).unwrap(),
                0,
                "reserved byte cleared"
            );
        }
    }

    #[test]
    fn fcb_find_first_no_match_returns_ff() {
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[("A.TXT", b"x")]);
        place_fcb(&mut mem, 0, "NOPE.ZZZ");
        let r = fcb_call(&mut kernel, &mut mem, 0x11);
        assert_eq!(r.ax & 0xff, 0xff);
    }

    #[test]
    fn fcb_find_excludes_directories() {
        let (mut kernel, mut mem, dir) = fcb_kernel(&[("FILE.TXT", b"x")]);
        std::fs::create_dir(dir.path().join("SUBDIR")).unwrap();
        place_fcb(&mut mem, 0, "????????.???");

        let r1 = fcb_call(&mut kernel, &mut mem, 0x11);
        assert_eq!(r1.ax & 0xff, 0x00);
        let name: Vec<u8> = (0..8)
            .map(|i| mem.read_u8(FCB_DTA + 0x01 + i).unwrap())
            .collect();
        assert_eq!(&name, b"FILE    ", "the file, not the directory");
        // The directory was filtered by attr_matches(attr, 0), so only the file
        // matched and find-next is immediately exhausted.
        let r2 = fcb_call(&mut kernel, &mut mem, 0x12);
        assert_eq!(r2.ax & 0xff, 0xff, "the SUBDIR directory was excluded");
    }

    // Place an extended search FCB at DS:DX=0100:0200: 0xFF header, five reserved
    // bytes, the search attribute at +6, the drive at +7, and the 8.3 name at +8.
    fn place_extended_fcb(mem: &mut Memory, search_attr: u8, drive: u8, name: &str) {
        let base = 0x0100usize * 16 + 0x0200;
        mem.write_u8(base, 0xff).unwrap();
        for i in 1..6 {
            mem.write_u8(base + i, 0).unwrap();
        }
        mem.write_u8(base + 6, search_attr).unwrap();
        mem.write_u8(base + 7, drive).unwrap();
        let (stem, ext) = name.split_once('.').unwrap_or((name, ""));
        for i in 0..8 {
            mem.write_u8(
                base + 8 + i,
                stem.as_bytes().get(i).copied().unwrap_or(b' '),
            )
            .unwrap();
        }
        for i in 0..3 {
            mem.write_u8(
                base + 16 + i,
                ext.as_bytes().get(i).copied().unwrap_or(b' '),
            )
            .unwrap();
        }
    }

    #[test]
    fn fcb_extended_find_returns_a_directory_a_normal_fcb_excludes() {
        let (mut kernel, mut mem, dir) = fcb_kernel(&[]);
        std::fs::create_dir(dir.path().join("SUBDIR")).unwrap();

        // A normal FCB find excludes the directory.
        place_fcb(&mut mem, 0, "????????.???");
        let n = fcb_call(&mut kernel, &mut mem, 0x11);
        assert_eq!(
            n.ax & 0xff,
            0xff,
            "a normal FCB find does not return a directory"
        );

        // An extended FCB carrying the directory search attribute returns it, in the
        // extended result format (0xFF header, attribute at +6, drive at +7, the
        // directory entry at +8).
        place_extended_fcb(&mut mem, 0x10, 0, "????????.???");
        let e = fcb_call(&mut kernel, &mut mem, 0x11);
        assert_eq!(e.ax & 0xff, 0x00, "the extended search finds the directory");
        assert_eq!(mem.read_u8(FCB_DTA).unwrap(), 0xff, "extended FCB header");
        assert_eq!(mem.read_u8(FCB_DTA + 7).unwrap(), 3, "drive C: = 3");
        let name: Vec<u8> = (0..8)
            .map(|i| mem.read_u8(FCB_DTA + 8 + i).unwrap())
            .collect();
        assert_eq!(&name, b"SUBDIR  ", "the directory name in the entry");
        assert_eq!(
            mem.read_u8(FCB_DTA + 8 + 0x0b).unwrap(),
            0x10,
            "directory attribute in the entry"
        );
    }

    #[test]
    fn fcb_extended_find_normal_file_uses_extended_result_format() {
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[("DATA.BIN", &[0u8; 50])]);
        place_extended_fcb(&mut mem, 0x00, 0, "DATA.BIN");

        let r = fcb_call(&mut kernel, &mut mem, 0x11);
        assert_eq!(r.ax & 0xff, 0x00);
        assert_eq!(mem.read_u8(FCB_DTA).unwrap(), 0xff, "extended header");
        assert_eq!(
            mem.read_u8(FCB_DTA + 6).unwrap(),
            0x00,
            "the file attribute in the header"
        );
        assert_eq!(mem.read_u8(FCB_DTA + 7).unwrap(), 3, "drive");
        let name: Vec<u8> = (0..8)
            .map(|i| mem.read_u8(FCB_DTA + 8 + i).unwrap())
            .collect();
        assert_eq!(&name, b"DATA    ");
        assert_eq!(
            mem.read_u32(FCB_DTA + 8 + 0x1c).unwrap(),
            50,
            "size in the entry"
        );
    }

    #[test]
    fn fcb_extended_find_next_keeps_the_extended_format() {
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[("A.TXT", b"a"), ("B.TXT", b"b")]);
        place_extended_fcb(&mut mem, 0x00, 0, "????????.TXT");

        let r1 = fcb_call(&mut kernel, &mut mem, 0x11);
        assert_eq!(r1.ax & 0xff, 0x00, "first .TXT match");
        assert_eq!(
            mem.read_u8(FCB_DTA).unwrap(),
            0xff,
            "find-first extended header"
        );

        // Find-next re-reads the unchanged FCB and keeps the extended format.
        let r2 = fcb_call(&mut kernel, &mut mem, 0x12);
        assert_eq!(r2.ax & 0xff, 0x00, "second .TXT match");
        assert_eq!(
            mem.read_u8(FCB_DTA).unwrap(),
            0xff,
            "find-next keeps the extended header"
        );
        assert_eq!(
            mem.read_u8(FCB_DTA + 7).unwrap(),
            3,
            "drive in the extended header"
        );

        let r3 = fcb_call(&mut kernel, &mut mem, 0x12);
        assert_eq!(r3.ax & 0xff, 0xff, "exhausted");
    }

    #[test]
    fn fcb_rename_moves_a_file() {
        let (mut kernel, mut mem, dir) = fcb_kernel(&[("OLD.TXT", b"data")]);
        place_fcb(&mut mem, 3, "OLD.TXT");
        // The new 8.3 name goes at FCB offset 0x11 (stem) / 0x19 (ext).
        let base = 0x0100usize * 16 + 0x0200;
        for (i, b) in b"NEW     ".iter().enumerate() {
            mem.write_u8(base + 0x11 + i, *b).unwrap();
        }
        for (i, b) in b"TXT".iter().enumerate() {
            mem.write_u8(base + 0x19 + i, *b).unwrap();
        }
        let regs = fcb_call(&mut kernel, &mut mem, 0x17);
        assert_eq!(regs.ax & 0xff, 0x00);
        assert!(!dir.path().join("OLD.TXT").exists());
        assert_eq!(std::fs::read(dir.path().join("NEW.TXT")).unwrap(), b"data");
    }

    #[test]
    fn fcb_close_succeeds_for_a_resolvable_file() {
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[("DATA.BIN", b"hi")]);
        place_fcb(&mut mem, 3, "DATA.BIN");
        assert_eq!(fcb_call(&mut kernel, &mut mem, 0x10).ax & 0xff, 0x00);
    }

    #[test]
    fn fcb_get_file_size_reports_records() {
        // 300 bytes at 128-byte records = ceil(300/128) = 3 records.
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[("BIG.BIN", &[0u8; 300])]);
        place_fcb(&mut mem, 3, "BIG.BIN");
        // Seed the record size the FCB carries (AH=23h reads it; default to 128).
        let base = 0x0100usize * 16 + 0x0200;
        mem.write_u16(base + 0x0e, 128).unwrap();
        let regs = fcb_call(&mut kernel, &mut mem, 0x23);
        assert_eq!(regs.ax & 0xff, 0x00);
        assert_eq!(mem.read_u32(base + 0x21).unwrap(), 3, "3 records");
    }

    /// Point the DTA at 0x0500:0x0000 (clear of the FCB at 0x0100:0x0200).
    fn set_dta_0500(kernel: &mut DosKernel, mem: &mut Memory) {
        let mut regs = DosRegs {
            ax: 0x1a00,
            ds: 0x0500,
            dx: 0x0000,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, mem).unwrap();
    }

    #[test]
    fn fcb_random_write_then_read_round_trips_a_record() {
        // Create a fresh file, set the random record to 2, write a record there,
        // then read it back and confirm the bytes match.
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[]);
        place_fcb(&mut mem, 3, "RAND.BIN");
        assert_eq!(fcb_call(&mut kernel, &mut mem, 0x16).ax & 0xff, 0x00); // create
        set_dta_0500(&mut kernel, &mut mem);
        let base = 0x0100usize * 16 + 0x0200;
        let dta = 0x0500usize * 16;
        // Random record 2 with the default 128-byte record size.
        mem.write_u32(base + 0x21, 2).unwrap();
        for i in 0..128usize {
            mem.write_u8(dta + i, (i as u8).wrapping_mul(3)).unwrap();
        }
        let write = fcb_call(&mut kernel, &mut mem, 0x22);
        assert_eq!(write.ax & 0xff, 0x00, "random write succeeds");
        // The block/record cursor synced to random 2: block 0, record 2.
        assert_eq!(mem.read_u16(base + 0x0c).unwrap(), 0);
        assert_eq!(mem.read_u8(base + 0x20).unwrap(), 2);
        // Clear the DTA, then random-read the same record back.
        for i in 0..128usize {
            mem.write_u8(dta + i, 0).unwrap();
        }
        mem.write_u32(base + 0x21, 2).unwrap();
        let read = fcb_call(&mut kernel, &mut mem, 0x21);
        assert_eq!(read.ax & 0xff, 0x00, "random read full record");
        for i in 0..128usize {
            assert_eq!(mem.read_u8(dta + i).unwrap(), (i as u8).wrapping_mul(3));
        }
    }

    #[test]
    fn fcb_random_read_past_eof_returns_01_and_leaves_dta() {
        // A one-record file; random read of record 5 is EOF and must not clobber
        // the DTA (the consistency fix carried from the sequential path).
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[("ONE.BIN", &[0xabu8; 128])]);
        place_fcb(&mut mem, 3, "ONE.BIN");
        assert_eq!(fcb_call(&mut kernel, &mut mem, 0x0f).ax & 0xff, 0x00); // open
        set_dta_0500(&mut kernel, &mut mem);
        let base = 0x0100usize * 16 + 0x0200;
        let dta = 0x0500usize * 16;
        mem.write_u8(dta, 0x77).unwrap(); // sentinel
        mem.write_u32(base + 0x21, 5).unwrap();
        let read = fcb_call(&mut kernel, &mut mem, 0x21);
        assert_eq!(read.ax & 0xff, 0x01, "EOF");
        assert_eq!(mem.read_u8(dta).unwrap(), 0x77, "DTA left untouched");
    }

    #[test]
    fn fcb_set_random_record_computes_from_block_and_record() {
        // AH=24h: random = block * 128 + current-record.
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[("X.BIN", b"x")]);
        place_fcb(&mut mem, 3, "X.BIN");
        let base = 0x0100usize * 16 + 0x0200;
        mem.write_u16(base + 0x0c, 3).unwrap(); // block 3
        mem.write_u8(base + 0x20, 7).unwrap(); // record 7
        fcb_call(&mut kernel, &mut mem, 0x24);
        assert_eq!(mem.read_u32(base + 0x21).unwrap(), 3 * 128 + 7);
    }

    #[test]
    fn fcb_random_block_read_reads_cx_records_and_advances() {
        // A 3-record file (384 bytes). Read 2 records from random 0; CX returns 2,
        // the random record and block/record advance to 2.
        let mut data = vec![0u8; 384];
        for (i, b) in data.iter_mut().enumerate() {
            *b = i as u8;
        }
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[("BLK.BIN", &data)]);
        place_fcb(&mut mem, 3, "BLK.BIN");
        assert_eq!(fcb_call(&mut kernel, &mut mem, 0x0f).ax & 0xff, 0x00);
        set_dta_0500(&mut kernel, &mut mem);
        let base = 0x0100usize * 16 + 0x0200;
        let dta = 0x0500usize * 16;
        mem.write_u32(base + 0x21, 0).unwrap();
        let mut regs = DosRegs {
            ax: 0x2700,
            ds: 0x0100,
            dx: 0x0200,
            cx: 2,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.ax & 0xff, 0x00, "all records read");
        assert_eq!(regs.cx, 2, "2 records read");
        assert_eq!(mem.read_u8(dta).unwrap(), 0); // record 0 byte 0
        assert_eq!(mem.read_u8(dta + 128).unwrap(), 128u8); // record 1 byte 0
        assert_eq!(
            mem.read_u32(base + 0x21).unwrap(),
            2,
            "random advanced to 2"
        );
        assert_eq!(mem.read_u16(base + 0x0c).unwrap(), 0);
        assert_eq!(mem.read_u8(base + 0x20).unwrap(), 2);
    }

    #[test]
    fn fcb_random_block_write_cx0_sets_file_size() {
        // CX=0 truncates/extends the file to random * record-size without writing.
        let (mut kernel, mut mem, dir) = fcb_kernel(&[("SZ.BIN", &[0u8; 512])]);
        place_fcb(&mut mem, 3, "SZ.BIN");
        assert_eq!(fcb_call(&mut kernel, &mut mem, 0x0f).ax & 0xff, 0x00);
        let base = 0x0100usize * 16 + 0x0200;
        mem.write_u32(base + 0x21, 2).unwrap(); // 2 records * 128 = 256 bytes
        let mut regs = DosRegs {
            ax: 0x2800,
            ds: 0x0100,
            dx: 0x0200,
            cx: 0,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.ax & 0xff, 0x00);
        let len = std::fs::metadata(dir.path().join("SZ.BIN")).unwrap().len();
        assert_eq!(len, 256, "file truncated to 2 records");
        assert_eq!(
            mem.read_u32(base + 0x10).unwrap(),
            256,
            "FCB file-size updated"
        );
    }

    #[test]
    fn fcb_parse_filename_wildcard_sets_al1_and_fields() {
        // AH=29h parse of "B:FILE*.TX" with no option bits. The '*' fills the name
        // tail with '?', so AL=1 and the FCB name/ext carry the parsed bytes.
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[]);
        // Source at DS:SI = 0x0100:0x0300, FCB at ES:DI = 0x0100:0x0200.
        let src = 0x0100usize * 16 + 0x0300;
        for (i, &b) in b"B:FILE*.TX\0".iter().enumerate() {
            mem.write_u8(src + i, b).unwrap();
        }
        let mut regs = DosRegs {
            ax: 0x2900, // AL = 0 (no option bits)
            ds: 0x0100,
            si: 0x0300,
            es: 0x0100,
            di: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.ax & 0xff, 0x01, "wildcards present -> AL=1");
        let fcb = 0x0100usize * 16 + 0x0200;
        assert_eq!(mem.read_u8(fcb).unwrap(), 2, "drive B: -> 2");
        // Name "FILE" then '?'-padded to 8 from the '*'.
        assert_eq!(&read_fcb_field(&mem, fcb + 0x01, 8), b"FILE????");
        // Ext "TX" blank-padded to 3.
        assert_eq!(&read_fcb_field(&mem, fcb + 0x09, 3), b"TX ");
    }

    #[test]
    fn fcb_parse_filename_invalid_drive_returns_ff() {
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[]);
        let src = 0x0100usize * 16 + 0x0300;
        for (i, &b) in b"5:NAME.EXT\0".iter().enumerate() {
            mem.write_u8(src + i, b).unwrap();
        }
        let mut regs = DosRegs {
            ax: 0x2900,
            ds: 0x0100,
            si: 0x0300,
            es: 0x0100,
            di: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.ax & 0xff, 0xff, "non-letter drive -> AL=0xFF");
    }

    fn read_fcb_field(mem: &Memory, base: usize, len: usize) -> Vec<u8> {
        (0..len).map(|i| mem.read_u8(base + i).unwrap()).collect()
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
        // env MCB header at the parent free_base 0x0200, env data at 0x0201, the
        // child program MCB header at 0x0202, so the child PSP is 0x0203.
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        let child_psp = 0x0203usize * 16;
        match action {
            DosAction::Exec { child_ax, .. } => {
                assert_eq!(child_ax, 0x0000); // null FCBs -> valid drives
                assert_eq!(mem.read_u16(child_psp + 0x02).unwrap(), 0xa000);
                assert_eq!(mem.read_u16(child_psp + 0x16).unwrap(), 0x0100); // parent
                assert_eq!(mem.read_u16(child_psp + 0x2c).unwrap(), 0x0201); // env data seg
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
        // env data at 0x0201:0 (above its MCB header at 0x0200) is a terminating NUL.
        assert_eq!(mem.read_u8(0x0201 * 16).unwrap(), 0x00);
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
        // Copied env data at 0x0201:0 (above its MCB header) holds the same bytes.
        for (i, &b) in b"A=1\0B=2\0\0".iter().enumerate() {
            assert_eq!(mem.read_u8(0x0201 * 16 + i).unwrap(), b);
        }
    }

    #[test]
    fn ah4b_al0_inherits_the_parent_environment() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CHILD.COM"), [0xcdu8, 0x20]).unwrap();
        let (mut kernel, mut mem) = exec_kernel(dir.path());
        // Seed the parent's environment exactly as new_dos_program does, so the
        // parent PSP:0x2C names a BLASTER block the child must inherit.
        kernel
            .install_environment(&mut mem, &[("BLASTER", "A220 I5 D1 H5 T6")])
            .unwrap();
        place_exec_inputs(&mut mem, "C:\\CHILD.COM", 0); // env_source 0 = inherit
        let _ = exec_al0(&mut kernel, &mut mem);
        // EXEC switched in the child; its PSP:0x2C names the inherited env block.
        let child_env = mem
            .read_u16(usize::from(kernel.arena.psp_seg) * 16 + 0x2c)
            .unwrap();
        assert_eq!(
            parse_env_block(&mem, child_env),
            vec![("BLASTER".to_string(), "A220 I5 D1 H5 T6".to_string())]
        );
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
        // Now in the child context (psp_seg = 0x0203, above the env block + header).
        assert_eq!(kernel.arena.psp_seg, 0x0203);
        kernel.finish_exec(7, &mut mem).unwrap();
        assert_eq!(kernel.arena.psp_seg, 0x0100); // parent restored
        // The child's memory (env + program) was freed back to the parent: its free
        // tail is restored at the old free base.
        assert_eq!(kernel.arena.free_base(&mem), 0x0200);
        assert_eq!(kernel.last_exit_code, 7);
    }

    #[test]
    fn exec_child_chain_shows_the_env_block_then_the_program() {
        // Commit 2 fidelity: a child's MCB chain (via AH=52h) starts at its
        // environment block (owned by the child PSP), then the program block, so a
        // guest walking the chain sees env -> program.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CHILD.COM"), [0xcdu8, 0x20]).unwrap();
        let (mut kernel, mut mem) = exec_kernel(dir.path());
        place_exec_inputs(&mut mem, "C:\\CHILD.COM", 0);
        let _ = exec_al0(&mut kernel, &mut mem);
        let child_psp = kernel.arena.psp_seg;
        let mut q = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut q, &mut mem).unwrap();
        let first = mem
            .read_u16(usize::from(q.es) * 16 + usize::from(q.bx) - 2)
            .unwrap();
        let chain = read_mcb_chain(&mem, first);
        assert!(chain.len() >= 2);
        // First block is the env (data segment = PSP:0x2C), owned by the child.
        assert_eq!(chain[0].owner, child_psp, "env block owned by the child");
        let env_seg = mem.read_u16(usize::from(child_psp) * 16 + 0x2c).unwrap();
        assert_eq!(
            chain[0].mcb_seg.wrapping_add(1),
            env_seg,
            "env data = PSP:0x2C"
        );
        // Then the program block (data segment = child PSP), also owned by the child.
        assert_eq!(
            chain[1].owner, child_psp,
            "program block owned by the child"
        );
        assert_eq!(
            chain[1].mcb_seg.wrapping_add(1),
            child_psp,
            "program data = PSP"
        );
    }

    #[test]
    fn finish_exec_keeps_a_resident_child_block() {
        // A child that keeps itself resident (AH=31h TSR) is NOT reclaimed on exit:
        // its program block stays owned and the parent's free tail sits above it.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CHILD.COM"), [0xcdu8, 0x20]).unwrap();
        let (mut kernel, mut mem) = exec_kernel(dir.path());
        place_exec_inputs(&mut mem, "C:\\CHILD.COM", 0);
        let _ = exec_al0(&mut kernel, &mut mem);
        let child_psp = kernel.arena.psp_seg;
        let mut tsr = DosRegs {
            ax: 0x3100,
            dx: 0x0040,
            ..DosRegs::default()
        };
        assert!(matches!(
            kernel.dispatch(0x21, &mut tsr, &mut mem).unwrap(),
            DosAction::Exit(_)
        ));
        kernel.finish_exec(0, &mut mem).unwrap();
        // The resident block survived: the parent's free base is above it, not back
        // at the old 0x0200.
        assert!(
            kernel.arena.free_base(&mem) > child_psp,
            "the resident child block was not reclaimed"
        );
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
    fn exec_of_a_bad_program_leaves_the_parent_arena_intact() {
        // A failed child load must not corrupt the parent's chain or lose its free
        // memory: the env block is written only after the load succeeds.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CHILD.COM"), [0x4du8, 0x5a]).unwrap(); // truncated MZ
        let (mut kernel, mut mem) = exec_kernel(dir.path());
        let before = kernel.arena.free_base(&mem);
        place_exec_inputs(&mut mem, "C:\\CHILD.COM", 0);
        let regs = exec_al0(&mut kernel, &mut mem);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x0b);
        // The parent's free base is unchanged: no env header clobbered its free tail.
        assert_eq!(
            kernel.arena.free_base(&mem),
            before,
            "parent free memory preserved across a failed EXEC"
        );
        assert!(kernel.program_stack.is_empty(), "no child frame pushed");
    }

    #[test]
    fn finish_exec_preserves_a_resident_region_above_the_freed_child() {
        // An ancestor exiting must not reclaim a deeper resident TSR: a resident
        // region recorded above the parent's free base caps the restored free region.
        let (mut kernel, mut mem) = arena_kernel(); // parent psp 0x100, free tail 0x1100
        // A resident TSR: a small owned program block at 0x4000 with a free tail above.
        write_mcb_header(&mut mem, 0x4000, b'M', 0x4001, 0x40, NO_NAME).unwrap();
        write_mcb_header(&mut mem, 0x4041, b'Z', 0, ARENA_TOP - 0x4041 - 1, NO_NAME).unwrap();
        kernel.resident_regions.push(0x4000);
        // A child frame whose free base is below the resident region.
        let dta = kernel.dta;
        kernel.program_stack.push(ProgramContext {
            arena: std::mem::take(&mut kernel.arena),
            dta,
            find_searches: HashMap::new(),
            free_base: 0x1100,
        });
        kernel.arena = Arena {
            psp_seg: 0x1200,
            chain_first: 0x1100,
            resident: false,
        };
        kernel.finish_exec(0, &mut mem).unwrap();
        // The TSR block survives and the freed child region below it is capped.
        let chain = read_mcb_chain(&mem, kernel.arena.first_mcb());
        assert!(
            chain
                .iter()
                .any(|m| m.mcb_seg == 0x4000 && m.owner == 0x4001),
            "the resident TSR above the freed child survives"
        );
        assert!(
            chain.iter().any(|m| m.mcb_seg == 0x1100 && m.owner == 0),
            "the child's memory is freed below the TSR"
        );
        assert_eq!(
            kernel.arena.free_base(&mem),
            0x4041,
            "free tail above the TSR"
        );
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
        kernel.init_program(0x0100, prog_top, &mut mem).unwrap();
        (kernel, mem, prog_top)
    }

    #[test]
    fn install_environment_seeds_psp_env_pointer_and_parses_back() {
        let (mut kernel, mut mem, prog_top) = env_kernel();
        kernel
            .install_environment(&mut mem, &[("BLASTER", "A220 I5 D1 H5 T6")])
            .unwrap();

        // PSP:0x2C names the env segment. Its MCB header sits at prog_top, so the env
        // data segment is one paragraph above the program block.
        let env_seg = mem.read_u16(0x0100 * 16 + 0x2c).unwrap();
        assert_eq!(env_seg, prog_top + 1);
        // The block at env_seg:0 scans back to the single BLASTER entry.
        assert_eq!(
            parse_env_block(&mem, env_seg),
            vec![("BLASTER".to_string(), "A220 I5 D1 H5 T6".to_string())]
        );
    }

    #[test]
    fn build_psp_fills_the_documented_fields() {
        // Seed the IVT entries for INT 22h/23h/24h so the snapshot has bytes to
        // copy, then build a PSP and read the fields back.
        let mut mem = Memory::new(1024 * 1024).unwrap();
        mem.write_u16(0x22 * 4, 0x1111).unwrap(); // INT 22h offset
        mem.write_u16(0x22 * 4 + 2, 0x2222).unwrap(); // INT 22h segment
        mem.write_u16(0x23 * 4, 0x3333).unwrap();
        mem.write_u16(0x23 * 4 + 2, 0x4444).unwrap();
        mem.write_u16(0x24 * 4, 0x5555).unwrap();
        mem.write_u16(0x24 * 4 + 2, 0x6666).unwrap();
        build_psp(&mut mem, 0x0100, 0x9000).unwrap();
        let psp = 0x0100usize * 16;
        // The INT 22h/23h/24h far vectors are snapshotted from the IVT.
        assert_eq!(mem.read_u16(psp + 0x0a).unwrap(), 0x1111);
        assert_eq!(mem.read_u16(psp + 0x0c).unwrap(), 0x2222);
        assert_eq!(mem.read_u16(psp + 0x0e).unwrap(), 0x3333);
        assert_eq!(mem.read_u16(psp + 0x10).unwrap(), 0x4444);
        assert_eq!(mem.read_u16(psp + 0x12).unwrap(), 0x5555);
        assert_eq!(mem.read_u16(psp + 0x14).unwrap(), 0x6666);
        // Parent PSP defaults to 0 (no parent for a directly loaded program).
        assert_eq!(mem.read_u16(psp + 0x16).unwrap(), 0);
        // The JFT: count 20 at 0x32, far pointer PSP:0x18 at 0x34, the 20 handles
        // at 0x18 with stdin/stdout/stderr open and the rest closed (0xFF).
        assert_eq!(mem.read_u16(psp + 0x32).unwrap(), 20);
        assert_eq!(mem.read_u16(psp + 0x34).unwrap(), 0x0018);
        assert_eq!(mem.read_u16(psp + 0x36).unwrap(), 0x0100);
        assert_eq!(mem.read_u8(psp + 0x18).unwrap(), 0x01);
        assert_eq!(mem.read_u8(psp + 0x19).unwrap(), 0x01);
        assert_eq!(mem.read_u8(psp + 0x1a).unwrap(), 0x01);
        assert_eq!(mem.read_u8(psp + 0x1b).unwrap(), 0xff); // handle 3 closed
        assert_eq!(mem.read_u8(psp + 0x18 + 19).unwrap(), 0xff); // last entry closed
    }

    #[test]
    fn install_environment_appends_the_argv0_trailer() {
        // After the double-NUL that ends the env strings, DOS 3.0+ writes a WORD
        // count of 0x0001 and the program's ASCIIZ full path.
        let (mut kernel, mut mem, _prog_top) = env_kernel();
        kernel
            .install_environment(&mut mem, &[("PATH", "C:\\")])
            .unwrap();
        let env_seg = mem.read_u16(0x0100 * 16 + 0x2c).unwrap();
        let base = usize::from(env_seg) * 16;
        // "PATH=C:\\\0" then the terminating empty-string NUL, then the trailer.
        let strings = b"PATH=C:\\\0\0";
        for (i, &b) in strings.iter().enumerate() {
            assert_eq!(mem.read_u8(base + i).unwrap(), b);
        }
        let trailer = base + strings.len();
        assert_eq!(mem.read_u16(trailer).unwrap(), 0x0001); // string count
        // The argv0 ASCIIZ path follows the count.
        let mut path = Vec::new();
        let mut i = trailer + 2;
        loop {
            let byte = mem.read_u8(i).unwrap();
            if byte == 0 {
                break;
            }
            path.push(byte);
            i += 1;
        }
        assert_eq!(path, DEFAULT_ARGV0.as_bytes());
    }

    #[test]
    fn ah31_keeps_the_process_resident() {
        // AH=31h exits with the AL code but trims the program block to DX
        // paragraphs and flags it resident. A program at 0x0100 keeps 0x20
        // paragraphs.
        let (mut kernel, mut mem, _prog_top) = env_kernel();
        let mut regs = DosRegs {
            ax: 0x3107, // AL=07 return code
            dx: 0x0020, // resident size in paragraphs
            ..DosRegs::default()
        };
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::Exit(7));
        // The arena trimmed the program block to psp_seg + DX and flagged it.
        assert!(kernel.arena.resident);
        assert_eq!(kernel.arena.prog_top(&mem), 0x0100 + 0x0020);
        // The freed tail is available: the next allocation puts its MCB header at the
        // trimmed top (0x0120) and hands back the data segment one paragraph higher.
        let mut alloc = DosRegs {
            ax: 0x4800,
            bx: 0x0001,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut alloc, &mut mem).unwrap();
        assert!(!alloc.cf);
        assert_eq!(alloc.ax, 0x0100 + 0x0020 + 1);
    }

    #[test]
    fn install_environment_advances_the_arena_above_the_block() {
        let (mut kernel, mut mem, prog_top) = env_kernel();
        kernel
            .install_environment(&mut mem, &[("BLASTER", "A220 I5 D1 H5 T6")])
            .unwrap();
        // The next AH=48h allocation must land above the env block (env strings
        // plus the argv0 trailer), proving the arena's free base advanced by the
        // rounded-up paragraph count.
        let env_paras = u16::try_from(
            build_env_block_with_argv0(&[("BLASTER", "A220 I5 D1 H5 T6")], DEFAULT_ARGV0)
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
        // Two header paragraphs sit in the way: the env block's and this allocation's.
        assert_eq!(regs.ax, prog_top + env_paras + 2);
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
        // env data sits one paragraph above the program block, past its MCB header.
        assert_eq!(mem.read_u16(psp + 0x2c).unwrap(), prog_top + 1); // env seg
    }

    #[test]
    fn install_environment_with_no_entries_still_allocates_a_segment() {
        // An empty env (sound disabled) is still a valid block: PSP:0x2C names a
        // readable segment whose first byte is the terminator NUL.
        let (mut kernel, mut mem, prog_top) = env_kernel();
        kernel.install_environment(&mut mem, &[]).unwrap();
        let env_seg = mem.read_u16(0x0100 * 16 + 0x2c).unwrap();
        assert_eq!(env_seg, prog_top + 1); // data above the env block's MCB header
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
        kernel.init_program(0x0100, ARENA_TOP, &mut mem).unwrap();
        kernel
            .install_environment(&mut mem, &[("BLASTER", "A220 I5 D1 H5 T6")])
            .unwrap();
        let paras = u16::try_from(
            build_env_block_with_argv0(&[("BLASTER", "A220 I5 D1 H5 T6")], DEFAULT_ARGV0)
                .len()
                .div_ceil(16),
        )
        .unwrap();
        let psp = 0x0100usize * 16;
        // PSP:0x02 drops by the env paragraphs plus the env block's one MCB header,
        // so the program data stops just below that header ...
        assert_eq!(mem.read_u16(psp + 2).unwrap(), ARENA_TOP - paras - 1);
        // ... and PSP:0x2C names the env data segment carved from the top, one
        // paragraph above its header.
        let env_seg = mem.read_u16(psp + 0x2c).unwrap();
        assert_eq!(env_seg, ARENA_TOP - paras);
        assert_eq!(
            parse_env_block(&mem, env_seg),
            vec![("BLASTER".to_string(), "A220 I5 D1 H5 T6".to_string())]
        );
    }

    #[test]
    fn resolve_c_root_prefers_local_then_creates() {
        let tmp = std::env::temp_dir().join(format!("izarra_croot_{}", std::process::id()));
        let local = tmp.join("c_drive");
        std::fs::create_dir_all(&local).unwrap();
        // When ./c_drive exists relative to `base`, it wins.
        let got = resolve_c_root_in(&tmp, &tmp.join("home"));
        assert_eq!(got, local);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn resolve_c_root_falls_back_to_home_and_creates() {
        let tmp = std::env::temp_dir().join(format!("izarra_chome_{}", std::process::id()));
        let home = tmp.join("home");
        let got = resolve_c_root_in(&tmp.join("nowhere"), &home);
        assert_eq!(got, home.join(".izarravm").join("c_drive"));
        assert!(got.is_dir());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn ah59_reports_the_last_dos_error() {
        // Drive a failing extended open (AH=6Ch open-only on a missing file) to
        // record an error, then read it back with AH=59h.
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[], r"C:\GONE.TXT");
        let mut open = DosRegs {
            ax: 0x6c00,
            bx: 0x0000, // read access
            dx: 0x0001, // open-if-exists only (no create)
            ds: 0x0100,
            si: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut open, &mut mem).unwrap();
        assert!(open.cf);
        assert_eq!(open.ax, 0x02); // file not found
        let mut err = DosRegs {
            ax: 0x5900,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut err, &mut mem).unwrap();
        assert_eq!(err.ax, 0x02); // the saved code
        assert_eq!(err.bx >> 8, 0x0d); // BH = class
        assert_eq!(err.bx & 0xff, 0x05); // BL = action
        assert_eq!(err.cx >> 8, 0x01); // CH = locus
    }

    #[test]
    fn ah59_tracks_errors_from_ordinary_handlers() {
        // A plain AH=3Dh open of a missing file fails through set_dos_error, not
        // the new fail() helper. The dispatcher must still record it so AH=59h
        // reports the true error, the classic recover-the-error-after-a-failed-call
        // idiom.
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[], r"C:\GONE.TXT");
        let open = open(&mut kernel, &mut mem);
        assert!(open.cf);
        assert_eq!(open.ax, 0x02); // file not found

        let mut err = DosRegs {
            ax: 0x5900,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut err, &mut mem).unwrap();
        assert_eq!(err.ax, 0x02, "AH=59h reports the open's error code");
        assert!(!err.cf, "the query itself clears carry");
    }

    #[test]
    fn ah5a_creates_a_unique_temp_file_and_appends_the_name() {
        // DS:DX points at the directory path "C:\" (ending in '\'). The handler
        // appends a generated name and creates it create-exclusive.
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[], "C:\\");
        let mut regs = DosRegs {
            ax: 0x5a00,
            cx: 0,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf, "create temp failed: ax={:#06x}", regs.ax);
        assert!(regs.ax >= 5);
        // Read the full ASCIIZ path back from DS:DX: it starts with "C:\" and the
        // appended name names a file that now exists on the host.
        let base = 0x0100usize * 16 + 0x0200;
        let mut path = String::new();
        let mut i = 0;
        loop {
            let byte = mem.read_u8(base + i).unwrap();
            if byte == 0 {
                break;
            }
            path.push(byte as char);
            i += 1;
        }
        assert!(path.starts_with("C:\\"), "path was {path}");
        let host_name = &path[3..]; // strip "C:\"
        assert!(_dir.path().join(host_name).exists(), "missing {host_name}");
    }

    #[test]
    fn ah6c_opens_an_existing_file_and_creates_a_new_one() {
        // Open-existing: bit 0 set (open-if-exists). CX reports 1 (opened).
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("HAVE.TXT", b"hi")], r"C:\HAVE.TXT");
        let mut open = DosRegs {
            ax: 0x6c00,
            bx: 0x0000,
            dx: 0x0001,
            ds: 0x0100,
            si: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut open, &mut mem).unwrap();
        assert!(!open.cf, "open failed: ax={:#06x}", open.ax);
        assert_eq!(open.ax, 5);
        assert_eq!(open.cx, 1); // opened

        // Create-new: bit 4 set (create-if-not-exists), file absent. CX reports 2.
        let (mut kernel, mut mem, dir) = kernel_with_drive(&[], r"C:\MADE.TXT");
        let mut create = DosRegs {
            ax: 0x6c00,
            bx: 0x0002, // write access
            cx: 0,
            dx: 0x0010, // create-if-not-exists
            ds: 0x0100,
            si: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut create, &mut mem).unwrap();
        assert!(!create.cf, "create failed: ax={:#06x}", create.ax);
        assert_eq!(create.cx, 2); // created
        assert!(dir.path().join("MADE.TXT").exists());
    }

    #[test]
    fn ah60_truename_canonicalizes_to_a_drive_qualified_uppercase_path() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[], r"sub\..\game.exe");
        // Input ASCIIZ at DS:SI = 0x0100:0x0200; output buffer at ES:DI = 0x0100:0x0600.
        let mut regs = DosRegs {
            ax: 0x6000,
            ds: 0x0100,
            si: 0x0200,
            es: 0x0100,
            di: 0x0600,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        let base = 0x0100usize * 16 + 0x0600;
        let mut out = String::new();
        let mut i = 0;
        loop {
            let byte = mem.read_u8(base + i).unwrap();
            if byte == 0 {
                break;
            }
            out.push(byte as char);
            i += 1;
        }
        // "sub\..\game.exe" folds the "sub\.." away and uppercases to C:\GAME.EXE.
        assert_eq!(out, r"C:\GAME.EXE");
    }
}
