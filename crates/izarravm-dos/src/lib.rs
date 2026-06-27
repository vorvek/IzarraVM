use izarravm_bus::{BusError, Memory};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use thiserror::Error;

mod driver;
mod memory;

pub use driver::{
    DEVICE_HEADER_LEN, DeviceHeaderInfo, DriverLoadError, InitResult, build_init_request,
    parse_device_header, read_init_result,
};

use memory::{
    ARENA_TOP, Arena, BLOCK_BPB_LEN, BlockDeviceBpb, BlockDeviceDpbEntry, DBCS_LEAD_BYTE_TABLE_PTR,
    ResizeError, SDA_ALWAYS_SWAPPED_LEN, SDA_IN_DOS_SWAPPED_LEN, SdaCriticalError, SdaSnapshot,
    SftHostFileEntry, SysvarsDevices, UmbArena, allocate_strategy, free_routed,
    free_umb_blocks_owned_by, is_valid_alloc_strategy, mcb_chain_is_complete, release_umb,
    request_umb, resize_routed, resize_umb, set_umb_owner, set_umb_region, stamp_mcb_owner,
    write_child_program_mcb, write_driver_bds, write_env_mcb, write_free_mcb_to_cap,
    write_nls_tables, write_sda, write_sda_list, write_sysvars,
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
const DEVICE_ATTR_OPEN_CLOSE: u16 = 0x0800;

fn kbd_ring_is_empty(mem: &Memory) -> Result<bool, DosError> {
    let head = mem.read_u16(KBD_BDA_BASE + KBD_HEAD)?;
    let tail = mem.read_u16(KBD_BDA_BASE + KBD_TAIL)?;
    Ok(head == tail)
}

fn kbd_ring_peek(mem: &Memory) -> Result<Option<(u8, u8)>, DosError> {
    let head = mem.read_u16(KBD_BDA_BASE + KBD_HEAD)?;
    let tail = mem.read_u16(KBD_BDA_BASE + KBD_TAIL)?;
    if head == tail {
        return Ok(None);
    }
    let word = mem.read_u16(KBD_BDA_BASE + head as usize)?;
    Ok(Some(((word >> 8) as u8, (word & 0xff) as u8)))
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

/// The default per-user C: root, `<home>/.izarravm/c_drive`, created if missing.
/// This is where `c_drive`, `cmos.bin`, and `izarravm.conf` live for a normal
/// launch, so they never land in whatever directory the binary was started from.
pub fn default_c_root_in(home: &Path) -> PathBuf {
    let chosen = home.join(".izarravm").join("c_drive");
    let _ = std::fs::create_dir_all(&chosen);
    chosen
}

/// The portable C: root, `<exe_dir>/c_drive`, created if missing. Selected only
/// when the user opts in with `--portable`, so a self-contained release keeps its
/// state beside the executable.
pub fn portable_c_root_in(exe_dir: &Path) -> PathBuf {
    let chosen = exe_dir.join("c_drive");
    let _ = std::fs::create_dir_all(&chosen);
    chosen
}

/// Resolve the C: root for a normal launch: the per-user `<home>/.izarravm` by
/// default, or a `c_drive` beside the executable when `portable` is set. Portable
/// mode keys off the executable's own directory, not the process working
/// directory. `home_dir` is un-deprecated on the project MSRV and behaves
/// correctly on Windows and Unix, so no `dirs` crate is pulled in.
pub fn resolve_c_root(portable: bool) -> PathBuf {
    if portable {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        portable_c_root_in(&exe_dir)
    } else {
        #[allow(deprecated)]
        let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
        default_c_root_in(&home)
    }
}

/// How `toka_dos_install` lays the OS down onto the C: drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    /// Install only if Toka-DOS is absent (first boot). The presence of
    /// `C:\DOS\ICOMMAND.COM` is the marker.
    EnsureIfMissing,
    /// Overwrite the system files from ROM, leaving any user files in place.
    Repair,
    /// Wipe the drive, then reinstall every system file.
    Format,
}

fn toka_dos_marker(c_root: &Path) -> PathBuf {
    c_root.join("DOS").join("ICOMMAND.COM")
}

/// The default AUTOEXEC.BAT: put the tools directory on the PATH, set a
/// path-showing prompt, and load the mouse driver. DOS line endings (CRLF).
const DEFAULT_AUTOEXEC_BAT: &str = "@ECHO OFF\r\nPATH=C:\\DOS\r\nPROMPT=$P$G\r\nMOUSE\r\n";

const DEFAULT_FILE_COUNT: u16 = 40;
const DEFAULT_BUFFER_COUNT: u16 = 20;
const DEFAULT_LASTDRIVE_COUNT: u8 = 5;
const FIRST_LOADED_BLOCK_DRIVE: u8 = 3;
const MAX_DOS_DRIVE: u8 = 25;
const C_DRIVE_SECTORS_PER_CLUSTER: u16 = 64;
const C_DRIVE_BYTES_PER_SECTOR: u16 = 512;
const C_DRIVE_TOTAL_CLUSTERS: u16 = 0xffff;
const C_DRIVE_FREE_CLUSTERS: u16 = 0xf000;
const C_DRIVE_VOLUME_LABEL: [u8; 11] = *b"NO NAME    ";
const C_DRIVE_FS_TYPE: [u8; 8] = *b"FAT16   ";

/// The default CONFIG.SYS: the directives a period DOS carries. The HIMEM.SYS
/// and IEMM.EXE RAM lines select the IEMM RAM mode (UMBs plus the EMS page
/// frame) at SYSINIT, the way a real DOS=HIGH,UMB box is configured; the machine
/// parses these to drive the memory layout. IEMM.EXE is the Toka-DOS memory
/// manager; the parser also accepts the real-DOS EMM386.EXE name so a pasted
/// real-DOS config still drives the mode. The CD-extension DEVICE= line is still
/// left out until that driver exists.
const DEFAULT_CONFIG_SYS: &str = "DEVICE=C:\\DOS\\HIMEM.SYS /TESTMEM:OFF\r\nDEVICE=C:\\DOS\\IEMM.EXE RAM\r\nDOS=HIGH,UMB\r\nFILES=40\r\nBUFFERS=20\r\nLASTDRIVE=E\r\n";

/// How the boot-config writer treats an existing CONFIG.SYS / AUTOEXEC.BAT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BootConfigPolicy {
    /// Write each default only when the file is absent (first boot). A user's
    /// edits are never touched.
    FillIfMissing,
    /// Repair: back an existing file up to its `.OLD` sibling (overwriting any
    /// stale `.OLD`), then write the default unconditionally. An absent file is
    /// just written, with no `.OLD` created.
    BackupAndReplace,
    /// Format: write each default unconditionally and never create a `.OLD`.
    Overwrite,
}

fn write_system_files(
    c_root: &Path,
    files: &[(String, Vec<u8>)],
    policy: BootConfigPolicy,
) -> std::io::Result<()> {
    std::fs::create_dir_all(c_root)?;
    let dos_dir = c_root.join("DOS");
    std::fs::create_dir_all(&dos_dir)?;
    for (name, bytes) in files {
        std::fs::write(dos_dir.join(name), bytes)?;
    }
    remove_root_system_file_copies(c_root, files)?;
    write_default_boot_config(c_root, policy)
}

fn remove_root_system_file_copies(
    c_root: &Path,
    files: &[(String, Vec<u8>)],
) -> std::io::Result<()> {
    for (name, _) in files {
        match std::fs::remove_file(c_root.join(name)) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

/// Write the default AUTOEXEC.BAT and CONFIG.SYS at the C: root according to
/// `policy`: leave a user's edits in place on first boot, hand back a known-good
/// pair on Repair (saving the prior file to `.OLD`), or overwrite on Format.
fn write_default_boot_config(c_root: &Path, policy: BootConfigPolicy) -> std::io::Result<()> {
    write_one_boot_file(&c_root.join("AUTOEXEC.BAT"), DEFAULT_AUTOEXEC_BAT, policy)?;
    write_one_boot_file(&c_root.join("CONFIG.SYS"), DEFAULT_CONFIG_SYS, policy)?;
    Ok(())
}

/// Write one boot file (CONFIG.SYS or AUTOEXEC.BAT) per the policy.
fn write_one_boot_file(
    path: &Path,
    default: &str,
    policy: BootConfigPolicy,
) -> std::io::Result<()> {
    match policy {
        BootConfigPolicy::FillIfMissing => {
            if !path.exists() {
                std::fs::write(path, default)?;
            }
        }
        BootConfigPolicy::BackupAndReplace => {
            if path.exists() {
                // Save the user's current file to its .OLD sibling, replacing any
                // stale backup, then hand back the known-good default.
                std::fs::copy(path, path.with_extension("OLD"))?;
            }
            std::fs::write(path, default)?;
        }
        BootConfigPolicy::Overwrite => {
            std::fs::write(path, default)?;
        }
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
            if toka_dos_marker(c_root).exists() {
                remove_root_system_file_copies(c_root, files)?;
                write_default_boot_config(c_root, BootConfigPolicy::FillIfMissing)?;
                return Ok(());
            }
            write_system_files(c_root, files, BootConfigPolicy::FillIfMissing)
        }
        InstallMode::Repair => {
            write_system_files(c_root, files, BootConfigPolicy::BackupAndReplace)
        }
        InstallMode::Format => {
            if c_root.exists() {
                clear_directory(c_root)?;
            }
            write_system_files(c_root, files, BootConfigPolicy::Overwrite)
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
    #[error("DOS system data layout does not fit below the first MCB")]
    SystemLayoutTooSmall,
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
    pub bp: u16,
    pub ds: u16, // segment selector
    pub es: u16, // segment selector
    pub cf: bool,
    pub zf: bool,
}

/// What the caller should do after a handled software interrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DosAction {
    Continue,            // results are in DosRegs; the IRET stub returns to the caller
    Exit(u8),            // terminate the program with this code
    InvokeInterrupt(u8), // route to a guest interrupt vector before returning
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
    /// Call a loaded character device. The kernel cannot far-call the driver
    /// itself, so the machine builds the DOS request packet, far-calls the driver's
    /// strategy then interrupt on the CPU, and writes either the transferred count
    /// or an explicit success AX back to the program along with CF.
    CallDevice {
        header: FarPtr,          // the driver header, for the strategy/interrupt entries
        command: u8,             // 4 = READ, 8 = WRITE, 0Dh = OPEN, 0Eh = CLOSE
        transfer: FarPtr,        // the caller's DS:DX buffer, if the command uses one
        count: u16,              // the requested byte count, if the command uses one
        success_ax: Option<u16>, // None => return transferred count in AX
        rollback_handle_on_error: Option<u16>, // rollback a just-opened handle if the driver fails
    },
}

/// A real-mode far pointer, stored and passed as segment:offset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FarPtr {
    pub segment: u16,
    pub offset: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExtendedError {
    code: u16,
    bx: u16,
    ch: u8,
    pointer: FarPtr,
}

impl ExtendedError {
    fn from_code(code: u16) -> Self {
        Self {
            code,
            ..Self::default()
        }
    }
}

impl Default for ExtendedError {
    fn default() -> Self {
        Self {
            code: 0,
            bx: (0x0d << 8) | 0x05,
            ch: 0x01,
            pointer: FarPtr::default(),
        }
    }
}

/// Disk area reported in the INT 24h AH flags for a block-device critical error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CriticalErrorArea {
    Dos,
    Fat,
    RootDirectory,
    Data,
}

impl CriticalErrorArea {
    fn bits(self) -> u8 {
        match self {
            CriticalErrorArea::Dos => 0,
            CriticalErrorArea::Fat => 1,
            CriticalErrorArea::RootDirectory => 2,
            CriticalErrorArea::Data => 3,
        }
    }
}

/// A DOS critical-error request waiting for the future host-to-guest trampoline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CriticalErrorRequest {
    pub drive: u8,
    pub error_code: u8,
    pub write: bool,
    pub area: CriticalErrorArea,
    pub device_header: FarPtr,
    pub ignore_allowed: bool,
    pub retry_allowed: bool,
    pub fail_allowed: bool,
}

impl CriticalErrorRequest {
    /// Build a DOS 3+ disk critical-error request with Ignore, Retry, and Fail all
    /// allowed. Abort is always available and is not represented by an AH flag.
    pub fn disk(
        drive: u8,
        error_code: u8,
        write: bool,
        area: CriticalErrorArea,
        device_header: FarPtr,
    ) -> Self {
        Self {
            drive,
            error_code,
            write,
            area,
            device_header,
            ignore_allowed: true,
            retry_allowed: true,
            fail_allowed: true,
        }
    }

    fn ah_flags(self) -> u8 {
        let mut flags = self.area.bits() << 1;
        if self.write {
            flags |= 0x01;
        }
        if self.fail_allowed {
            flags |= 0x08;
        }
        if self.retry_allowed {
            flags |= 0x10;
        }
        if self.ignore_allowed {
            flags |= 0x20;
        }
        flags
    }

    fn regs(self) -> DosRegs {
        DosRegs {
            ax: (u16::from(self.ah_flags()) << 8) | u16::from(self.drive),
            si: self.device_header.offset,
            di: u16::from(self.error_code),
            bp: self.device_header.segment,
            ..DosRegs::default()
        }
    }
}

/// The INT 24h callback frame the machine will trampoline into guest code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CriticalErrorCall {
    pub handler: FarPtr,
    pub regs: DosRegs,
}

/// A critical-error handler's return code, the value an INT 24h handler leaves in
/// AL for DOS to act on. DOS reads only the low two bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CriticalErrorResponse {
    Ignore,
    Retry,
    Abort,
    Fail,
}

impl CriticalErrorResponse {
    fn from_al(al: u8) -> Self {
        match al & 0x03 {
            0 => CriticalErrorResponse::Ignore,
            1 => CriticalErrorResponse::Retry,
            2 => CriticalErrorResponse::Abort,
            _ => CriticalErrorResponse::Fail,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveCriticalError {
    drive: u8,
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

    fn sft_open_mode(self) -> u16 {
        match self {
            AccessMode::Read => 0,
            AccessMode::Write => 1,
            AccessMode::ReadWrite => 2,
        }
    }
}

/// Where a handle's bytes flow: the emulated console (screen + keyboard) or a
/// host file. Standard handles 0/1/2 default to Console; AH=3Dh/3Ch opens and
/// DUP2 onto them install Host.
#[derive(Debug, Clone)]
enum OutputTarget {
    Console,
    Host(Rc<RefCell<File>>),
    Memory(Rc<RefCell<MemoryFile>>),
}

#[derive(Debug, Clone)]
struct MemoryFile {
    bytes: Vec<u8>,
    position: u32,
}

impl MemoryFile {
    fn size_u32(&self) -> u32 {
        self.bytes.len().min(u32::MAX as usize) as u32
    }

    fn read_to_guest(
        &mut self,
        mem: &mut Memory,
        base: usize,
        count: usize,
    ) -> Result<usize, DosError> {
        let start = usize::try_from(self.position).unwrap_or(usize::MAX);
        if start >= self.bytes.len() {
            return Ok(0);
        }
        let available = self.bytes.len().saturating_sub(start);
        let filled = available.min(count);
        for (index, &byte) in self.bytes[start..start + filled].iter().enumerate() {
            mem.write_u8(base + index, byte)?;
        }
        self.position = self.position.saturating_add(filled as u32);
        Ok(filled)
    }

    fn seek(&mut self, whence: u8, offset: u32) -> Option<u32> {
        let base = match whence {
            0 => 0i64,
            1 => i64::from(self.position),
            2 => i64::from(self.size_u32()),
            _ => return None,
        };
        let signed = if whence == 0 {
            i64::from(offset)
        } else {
            i64::from(offset as i32)
        };
        let pos = (base + signed) as u32;
        self.position = pos;
        Some(pos)
    }
}

/// An open file handle: its byte target plus the DOS access mode the kernel
/// enforces on host reads and writes. A Console handle carries no host file.
#[derive(Debug, Clone)]
struct OpenFile {
    target: OutputTarget,
    mode: AccessMode,
    sft_name: [u8; 11],
}

impl OpenFile {
    fn is_console(&self) -> bool {
        matches!(self.target, OutputTarget::Console)
    }

    /// The shared host file, or None for a Console handle.
    fn host_file(&self) -> Option<&Rc<RefCell<File>>> {
        match &self.target {
            OutputTarget::Host(f) => Some(f),
            OutputTarget::Console | OutputTarget::Memory(_) => None,
        }
    }

    fn memory_file(&self) -> Option<&Rc<RefCell<MemoryFile>>> {
        match &self.target {
            OutputTarget::Memory(f) => Some(f),
            OutputTarget::Console | OutputTarget::Host(_) => None,
        }
    }
}

/// An open handle on a loaded character device. The header far pointer locates
/// the driver's strategy and interrupt entries so a read or write can far-call
/// them; the access mode is enforced the same way a file handle's is.
#[derive(Debug, Clone, Copy)]
struct OpenDeviceHandle {
    header: FarPtr,
    mode: AccessMode,
}

/// A console handle. CON is bidirectional, so seed it ReadWrite; console writes
/// never consult the mode and console reads go through the keyboard path.
fn console_record() -> OpenFile {
    OpenFile {
        target: OutputTarget::Console,
        mode: AccessMode::ReadWrite,
        sft_name: *b"CON        ",
    }
}

fn open_file_record(file: File, mode: AccessMode, path: &Path) -> OpenFile {
    OpenFile {
        target: OutputTarget::Host(Rc::new(RefCell::new(file))),
        mode,
        sft_name: sft_name_from_path(path),
    }
}

fn open_memory_file_record(name: [u8; 11], bytes: Vec<u8>) -> OpenFile {
    OpenFile {
        target: OutputTarget::Memory(Rc::new(RefCell::new(MemoryFile { bytes, position: 0 }))),
        mode: AccessMode::Read,
        sft_name: name,
    }
}

/// Open an existing host file for a DOS access mode (no create).
fn open_host_file(path: &Path, mode: AccessMode) -> std::io::Result<File> {
    match mode {
        AccessMode::Read => File::open(path),
        AccessMode::Write => OpenOptions::new().write(true).open(path),
        AccessMode::ReadWrite => OpenOptions::new().read(true).write(true).open(path),
    }
}

/// Map the DOS create attribute bits that have a host equivalent. Hidden, system,
/// and archive are not represented in the host filesystem facade, but read-only
/// maps cleanly to permissions and is visible through AH=43h.
fn apply_create_attributes(path: &Path, attrs: u16) -> std::io::Result<()> {
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_readonly(attrs & 0x0001 != 0);
    std::fs::set_permissions(path, perms)
}

/// Strip a DOS filename down to its device base name: take the last path
/// component (after any drive or directory), drop a leading `X:` drive specifier,
/// then keep everything before the first `.` extension. DOS matches a device by
/// this base name regardless of drive, path, or extension, so TESTDEV,
/// C:\DEV\TESTDEV, C:TESTDEV, and TESTDEV.XYZ all reduce to "TESTDEV".
fn device_base_name(name: &str) -> &str {
    let mut last = name.rsplit(['\\', '/']).next().unwrap_or(name);
    // A bare drive specifier like "C:NAME" has no path separator, so strip the
    // leading two-char "X:" here (after the path split, before the extension).
    let bytes = last.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        last = &last[2..];
    }
    last.split('.').next().unwrap_or("")
}

/// Whether a DOS filename names the EMMXXXX0 character device. DOS matches a
/// device by its base name regardless of drive, path, or extension, so EMMXXXX0,
/// C:\EMMXXXX0, and EMMXXXX0.SYS all refer to the device.
fn is_ems_device_name(name: &str) -> bool {
    device_base_name(name).eq_ignore_ascii_case("EMMXXXX0")
}

/// The match key for a DOS device-open name: the device base name, upper-cased
/// and trimmed. DOS opens a device by its 1-8 char base name, so TESTDEV,
/// C:\DEV\TESTDEV, C:TESTDEV, and TESTDEV.XYZ all key to "TESTDEV".
fn device_name_key(name: &str) -> String {
    device_base_name(name).trim().to_ascii_uppercase()
}

/// One entry of a FindFirst/FindNext result: the documented DTA fields, the raw
/// 11-byte directory-entry name, and the uppercase name to write into the
/// 13-byte ASCIIZ slot.
#[derive(Debug, Clone)]
struct FindEntry {
    attr: u8,
    time: u16, // packed DOS time (RBIL #01665)
    date: u16, // packed DOS date (RBIL #01666)
    size: u32,
    raw_name: [u8; 11],
    name: String, // uppercase 8.3, e.g. "LEVEL1.DAT"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DosFindEntry {
    pub name_8_3: [u8; 11],
    pub attr: u8,
    pub time: u16,
    pub date: u16,
    pub size: u32,
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

#[derive(Debug)]
struct PendingLine {
    addr: u32,
    count: u8,
    template: Vec<u8>,
    template_index: usize,
}

/// Saved per-program DOS state, pushed when a child is EXECed (AL=0) and
/// restored when the child exits. Host-file handles are saved with shared file
/// objects, so inherited handles keep their seek position while child-only
/// handles are dropped on exit.
#[derive(Debug)]
struct ProgramContext {
    arena: Arena,
    dta: (u16, u16),
    find_searches: HashMap<(u16, u16), FindSearch>,
    open_files: HashMap<u16, OpenFile>,
    ems_handles: HashSet<u16>,
    device_handles: HashMap<u16, OpenDeviceHandle>,
    // The parent's free-tail segment at the moment of EXEC. The child's env and
    // program blocks are carved from here upward, so on the child's exit finish_exec
    // frees the parent's memory back from this segment, capped below any resident
    // (TSR) region the child or a descendant left above it.
    free_base: u16,
}

/// A raw `.SYS` image placed resident and ready to have INIT run on the CPU. The
/// machine far-calls strategy then interrupt with `request_ptr`, then finalizes or
/// aborts based on the request-header status.
#[derive(Debug, Clone, Copy)]
pub struct StagedDriver {
    /// Image base; the device header is at driver_seg:0.
    pub driver_seg: u16,
    /// Start segment of the staged allocation that owns the image and INIT scratch.
    pub allocation_seg: u16,
    /// Paragraphs allocated for the image plus INIT scratch before final trim.
    pub allocation_paras: u16,
    /// Whether the raw device header is a block driver rather than a character device.
    pub is_block_device: bool,
    /// DOS-assigned first drive byte written into the INIT request.
    pub first_drive: u8,
    /// (segment, offset) entry for the strategy routine.
    pub strategy: (u16, u16),
    /// (segment, offset) entry for the interrupt routine.
    pub interrupt: (u16, u16),
    /// Linear address of the INIT request header.
    pub request_linear: usize,
    /// (segment, offset) far pointer to the INIT request header.
    pub request_ptr: (u16, u16),
}

#[derive(Debug, Clone)]
struct LoadedBlockDevice {
    header: (u16, u16),
    first_drive: u8,
    installed_units: u8,
    bpbs_by_unit: Vec<Option<BlockDeviceBpb>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockDeviceIoTarget {
    pub header: FarPtr,
    pub unit: u8,
    pub media: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockDeviceFsTarget {
    pub io: BlockDeviceIoTarget,
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub first_fat_sector: u16,
    pub fat_count: u8,
    pub root_entries: u16,
    pub first_data_sector: u16,
    pub highest_cluster: u16,
    pub sectors_per_fat: u16,
    pub first_root_sector: u16,
}

/// Where SYSINIT should try to place a raw CONFIG.SYS driver image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverLoadPlacement {
    /// Plain DEVICE= loads in conventional memory.
    Low,
    /// DEVICEHIGH= tries linked upper memory first, then conventional memory.
    HighThenLow,
}

/// Why staging a `.SYS` driver failed. The SYSINIT loop reports and continues.
#[derive(Debug)]
pub enum DriverStageError {
    /// The header could not be parsed (MZ format, or malformed).
    Load(DriverLoadError),
    /// No resident block large enough for the image plus its scratch paragraph.
    OutOfMemory,
    /// A block driver requested a first drive after Z:.
    NoBlockDriveLetters,
    /// A guest memory fault while copying the image or building the request.
    Memory(DosError),
}

impl From<DosError> for DriverStageError {
    fn from(error: DosError) -> Self {
        DriverStageError::Memory(error)
    }
}

impl From<BusError> for DriverStageError {
    fn from(error: BusError) -> Self {
        DriverStageError::Memory(DosError::Memory(error))
    }
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
    last_exit_type: u8, // AH=4Dh AH; 0x00 normal, 0x03 TSR resident.
    // AH=0Ch flush-and-invoke: the flush runs once, not again on a WaitForKey re-entry.
    cooked_flush_done: bool,
    // AH=33h BREAK flag. The current HLE checks Ctrl-C on the DOS console calls
    // either way, but the flag itself is guest-visible and must round-trip.
    ctrl_break_enabled: bool,
    // INT 2Fh AX=122Fh can change the DOS version reported by AH=30h. None means
    // report the true Toka-DOS API version.
    reported_version_override: Option<u16>,
    // AH=37h Xenix compatibility switch character. None means the DOS default '/'.
    switch_char: Option<u8>,
    // AH=63h interim console flag for DBCS-aware callers. CP437 starts clear.
    interim_console_flag: bool,
    // Extended/function keys (arrows, F-keys) arrive on the ring as a (scancode, 0)
    // pair. DOS cooked input returns them as two reads: 0x00 first, then the scancode
    // on the next AH=01/06/07/08/0Ch call. This holds the scancode between the two.
    pending_scancode: Option<u8>,
    // AH=0Ah buffered input: the running line-edit state keyed by buffer address,
    // so it survives the per-character WaitForKey re-entries.
    pending_line: Option<PendingLine>,
    // Current directory on C:, as a path from the root with no leading or trailing
    // backslash ("" is the root, "DOS" is \DOS, "DOS\\NET" is \DOS\NET). This is
    // the format AH=47h returns. The current directory is global in DOS, so it is
    // not saved or restored across EXEC.
    cwd: String,
    // AH=0Eh/19h selected default drive, 0=A:. None means the boot default C:.
    current_drive: Option<u8>,
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
    // AH=59h extended error state. Held until the next error, or AX=5D0Ah, overwrites
    // it (DOS does not clear it on a successful call).
    extended_error: ExtendedError,
    // AH=69h volume serial/label stored in the C: extended BPB facade.
    volume_serial: u32,
    volume_label: Option<[u8; 11]>,
    // AX=440Dh/CX=0847h and 0867h disk access flag. This round-trips only;
    // enforce it when raw sector I/O exists.
    media_access_disabled: bool,
    // AX=5E00h/5E01h Microsoft Networks machine name.
    machine_name: Option<([u8; DOS_MACHINE_NAME_LEN], u8)>,
    // Active INT 24h critical-error callback state. None outside the callback.
    critical_error: Option<ActiveCriticalError>,
    // Far pointers (segment, offset) to CONFIG.SYS-loaded device-driver headers,
    // most-recently-loaded first (each new driver front-inserts so it links in right
    // after NUL). write_sysvars rebuilds the device-chain skeleton on every AH=52h
    // query, so these are re-spliced after NUL each time to survive the rebuild.
    loaded_devices: Vec<(u16, u16)>,
    // Loaded block devices in CONFIG.SYS load order. Drive-letter assignment uses
    // their installed unit spans even when a unit's BPB is invalid and unpublished.
    loaded_block_devices: Vec<LoadedBlockDevice>,
    // Handles a guest has opened on a loaded CHARACTER device by name (AH=3Dh), so
    // AH=3Fh read, AH=40h write, AH=44h IOCTL, and AH=3Eh close route to the driver
    // rather than a host file. Parallel to ems_handles: inherited by an EXEC child,
    // restored on the child's exit, skipped by alloc_handle, cleared on reboot.
    device_handles: HashMap<u16, OpenDeviceHandle>,
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

    /// Append a SYSINIT boot message to the DOS console buffer, CR/LF-terminated.
    /// The machine mirrors the console onto the VGA screen, so this is the same
    /// path DOS uses to print device-load feedback at boot.
    pub fn write_boot_message(&mut self, text: &str) {
        self.stdout.extend_from_slice(text.as_bytes());
        self.stdout.extend_from_slice(b"\r\n");
    }

    /// Set the apparent DOS version returned by INT 21h AH=30h. The word uses the
    /// AH=30h register layout: low byte major, high byte minor. Passing zero
    /// restores the true DOS version.
    pub fn set_reported_version_word(&mut self, version: u16) {
        self.reported_version_override = (version != 0).then_some(version);
    }

    fn reported_version_word(&self) -> u16 {
        self.reported_version_override
            .unwrap_or(TOKA_DOS_VERSION_WORD)
    }

    fn current_drive(&self) -> u8 {
        self.current_drive.unwrap_or(2)
    }

    fn logical_drive_count(&self) -> u8 {
        self.lastdrive
            .unwrap_or(DEFAULT_LASTDRIVE_COUNT)
            .clamp(5, 26)
    }

    fn dos_drive_index(&self, drive: u8) -> Option<u8> {
        if drive == 0 {
            Some(self.current_drive())
        } else if (1..=26).contains(&drive) {
            Some(drive - 1)
        } else {
            None
        }
    }

    fn drive_is_mounted_c(&self, drive: u8) -> bool {
        self.dos_drive_index(drive) == Some(2)
    }

    /// Update AH=59h class/action/locus fields from a DOS internal error table.
    /// Each four-byte record is code, class, action, locus; 0xFF in a field means
    /// "leave unchanged", and a code byte of 0xFF terminates the table.
    pub fn set_extended_error_from_table(
        &mut self,
        mem: &Memory,
        seg: u16,
        off: u16,
    ) -> Result<u16, DosError> {
        let wanted = self.extended_error.code as u8;
        let mut cursor = off;
        for _ in 0..=u16::MAX / 4 {
            let base = usize::from(seg) * 16 + usize::from(cursor);
            let code = mem.read_u8(base)?;
            if code == 0xff {
                cursor = cursor.wrapping_add(1);
                break;
            }
            let class = mem.read_u8(base + 1)?;
            let action = mem.read_u8(base + 2)?;
            let locus = mem.read_u8(base + 3)?;
            cursor = cursor.wrapping_add(4);
            if code == wanted {
                if class != 0xff {
                    self.extended_error.bx =
                        (u16::from(class) << 8) | (self.extended_error.bx & 0x00ff);
                }
                if action != 0xff {
                    self.extended_error.bx = (self.extended_error.bx & 0xff00) | u16::from(action);
                }
                if locus != 0xff {
                    self.extended_error.ch = locus;
                }
                break;
            }
        }
        Ok(cursor)
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
        self.critical_error = None;
        self.seed_standard_handles();
        Ok(())
    }

    /// Allocate a resident block, copy the raw `.SYS` image flat, stamp the owner
    /// to the system PSP, and build the INIT request header with the argument tail.
    /// The block carries one spare paragraph above the image for the request header
    /// and the CR-terminated tail. The caller runs INIT on the CPU, then calls
    /// finalize_sys_driver on success or abort_sys_driver on failure.
    pub fn stage_sys_driver(
        &mut self,
        image: &[u8],
        args: &str,
        placement: DriverLoadPlacement,
        mem: &mut Memory,
    ) -> Result<StagedDriver, DriverStageError> {
        let info = parse_device_header(image).map_err(DriverStageError::Load)?;
        let first_drive = if info.is_block_device() {
            self.next_block_device_drive()
                .ok_or(DriverStageError::NoBlockDriveLetters)?
        } else {
            0
        };
        let image_paras = u16::try_from((image.len() as u32).div_ceil(16))
            .map_err(|_| DriverStageError::OutOfMemory)?;
        // Spare paragraphs above the image for the request header (the arg tail
        // starts at offset 0x20) plus the CR-terminated tail itself.
        let arg_off = 0x20u16; // past the 0x17-byte request header, room to spare
        let scratch_bytes = u32::from(arg_off) + args.len() as u32 + 1;
        let scratch_paras =
            u16::try_from(scratch_bytes.div_ceil(16)).map_err(|_| DriverStageError::OutOfMemory)?;
        let total_paras = image_paras
            .checked_add(scratch_paras)
            .ok_or(DriverStageError::OutOfMemory)?;
        let allocation = match placement {
            DriverLoadPlacement::Low => self.arena.allocate(total_paras, mem)?,
            DriverLoadPlacement::HighThenLow => allocate_strategy(
                &mut self.arena,
                self.umb,
                self.umb_link,
                0x0080,
                total_paras,
                mem,
            )?,
        };
        let seg = match allocation {
            Ok(seg) => seg,
            Err(_) => return Err(DriverStageError::OutOfMemory),
        };
        stamp_mcb_owner(mem, seg, self.arena.psp_seg)?;
        let base = usize::from(seg) * 16;
        for (i, &b) in image.iter().enumerate() {
            mem.write_u8(base + i, b)?;
        }
        // Request header and arg tail in the spare trailing paragraphs.
        let request_seg = seg + image_paras;
        let request_linear = usize::from(request_seg) * 16;
        let arg_linear = request_linear + usize::from(arg_off);
        for (i, &b) in args.as_bytes().iter().enumerate() {
            mem.write_u8(arg_linear + i, b)?;
        }
        mem.write_u8(arg_linear + args.len(), 0x0d)?; // CR-terminate the tail
        let break_default = (seg, image_paras.wrapping_mul(16)); // end of image
        build_init_request(
            mem,
            request_linear,
            break_default,
            (request_seg, arg_off),
            first_drive,
        )?;
        Ok(StagedDriver {
            driver_seg: seg,
            allocation_seg: seg,
            allocation_paras: total_paras,
            is_block_device: info.is_block_device(),
            first_drive,
            strategy: (seg, info.strategy),
            interrupt: (seg, info.interrupt),
            request_linear,
            request_ptr: (request_seg, 0x00),
        })
    }

    /// INIT succeeded: trim the resident block to the returned break address (DOS
    /// reclaims the INIT-only tail) and record the header so AH=52h lists it.
    pub fn finalize_sys_driver(
        &mut self,
        staged: &StagedDriver,
        mem: &mut Memory,
    ) -> Result<(), DosError> {
        let result = read_init_result(mem, staged.request_linear)?;
        let loaded_block = if staged.is_block_device {
            Some(self.capture_loaded_block_device(staged, mem)?)
        } else {
            None
        };
        // Break address in paragraphs above the block base. The break offset rounds
        // up to a paragraph. The resident block must keep at least the device header
        // (0x12 bytes), so the free-tail MCB the trim writes never lands inside it.
        let header_paras = (DEVICE_HEADER_LEN as u16).div_ceil(16);
        let break_paras = result
            .break_seg
            .wrapping_sub(staged.driver_seg)
            .wrapping_add(result.break_off.div_ceil(16))
            .max(header_paras);
        let _ = self.resize_routed(staged.driver_seg, break_paras, mem)?; // best-effort trim
        // Front-insert so the most-recently-loaded driver sits nearest NUL, the way
        // real DOS links each new driver in right after the NUL header.
        self.loaded_devices.insert(0, (staged.driver_seg, 0x0000));
        if let Some(loaded_block) = loaded_block {
            if loaded_block.installed_units != 0 {
                self.loaded_block_devices.push(loaded_block);
            }
        }
        Ok(())
    }

    /// INIT failed or never returned: return the resident block to free memory.
    pub fn abort_sys_driver(
        &mut self,
        staged: &StagedDriver,
        mem: &mut Memory,
    ) -> Result<(), DosError> {
        let _ = self.free_routed(staged.driver_seg, mem)?;
        Ok(())
    }

    /// Install default Console entries for handles 0/1/2 if absent. `or_insert`
    /// means an inherited redirect (carried into a child by the deep-cloned
    /// open_files) is preserved. These never count toward alloc_handle (it scans
    /// 5..) nor appear in sft_host_file_entries (it skips Console).
    fn seed_standard_handles(&mut self) {
        self.open_files.entry(0).or_insert_with(console_record);
        self.open_files.entry(1).or_insert_with(console_record);
        self.open_files.entry(2).or_insert_with(console_record);
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

    /// Prepare a DOS INT 24h critical-error callback without running guest code.
    /// The future outbound trampoline will take the returned frame, call the live
    /// INT 24h vector, then feed the handler's AL back to finish_critical_error.
    pub fn begin_critical_error(
        &mut self,
        mem: &mut Memory,
        request: CriticalErrorRequest,
    ) -> Result<CriticalErrorCall, DosError> {
        self.extended_error =
            ExtendedError::from_code(critical_error_to_extended_error(request.error_code));
        self.critical_error = Some(ActiveCriticalError {
            drive: request.drive,
        });
        Ok(CriticalErrorCall {
            handler: interrupt_vector(mem, 0x24)?,
            regs: request.regs(),
        })
    }

    /// Finish a pending DOS INT 24h callback and decode the handler's AL action.
    pub fn finish_critical_error(&mut self, handler_al: u8) -> CriticalErrorResponse {
        self.critical_error = None;
        CriticalErrorResponse::from_al(handler_al)
    }

    /// The lowest free file handle (>= 5), skipping both host files and the open
    /// EMS-device handles so the two never collide. FILES= includes inherited
    /// handles 0-4, so dynamic handles stop before the configured count.
    fn alloc_handle(&self) -> Option<u16> {
        (5u16..self.file_count()).find(|h| {
            !self.open_files.contains_key(h)
                && !self.ems_handles.contains(h)
                && !self.device_handles.contains_key(h)
        })
    }

    fn has_live_handle_at_or_above(&self, limit: u16) -> bool {
        self.open_files.keys().any(|&handle| handle >= limit)
            || self.ems_handles.iter().any(|&handle| handle >= limit)
            || self.device_handles.keys().any(|&handle| handle >= limit)
    }

    /// Find a loaded CHARACTER device whose name matches `name`, the way DOS opens
    /// a device by name: case-insensitive, with any path and extension ignored.
    /// Returns the driver header far pointer, or None for no match (or a block
    /// device, which is not openable by name in 4b).
    fn find_loaded_character_device(&self, mem: &Memory, name: &str) -> Option<FarPtr> {
        let wanted = device_name_key(name);
        'devices: for &(seg, off) in &self.loaded_devices {
            let base = usize::from(seg) * 16 + usize::from(off);
            // A failed read is a bad header, not a function-level error: skip this
            // entry so a valid match on a later device is not hidden.
            let Ok(attr) = mem.read_u16(base + 4) else {
                continue;
            };
            if attr & 0x8000 == 0 {
                continue; // block device, not openable by name in 4b
            }
            let mut raw = [0u8; 8];
            for (i, b) in raw.iter_mut().enumerate() {
                let Ok(byte) = mem.read_u8(base + 0x0a + i) else {
                    continue 'devices;
                };
                *b = byte;
            }
            let have = String::from_utf8_lossy(&raw).trim().to_ascii_uppercase();
            if have == wanted {
                return Some(FarPtr {
                    segment: seg,
                    offset: off,
                });
            }
        }
        None
    }

    /// True when AH=3Dh on `name` should open a character device before any
    /// filesystem route. DOS applies this name precedence even with a drive or
    /// extension, so `D:\\TESTDEV.XYZ` still resolves to a loaded `TESTDEV` driver.
    pub fn open_name_would_resolve_to_character_device(&self, mem: &Memory, name: &str) -> bool {
        (is_ems_device_name(name) && self.ems_present)
            || self.find_loaded_character_device(mem, name).is_some()
    }

    /// Device attribute bit 11 asks DOS to send command 0Dh/0Eh on open/close.
    fn device_supports_open_close(&self, mem: &Memory, header: FarPtr) -> bool {
        let base = usize::from(header.segment) * 16 + usize::from(header.offset);
        mem.read_u16(base + 4)
            .is_ok_and(|attr| attr & DEVICE_ATTR_OPEN_CLOSE != 0)
    }

    /// Drop a loaded-device handle whose driver rejected its OPEN request.
    pub fn rollback_failed_device_open(&mut self, handle: u16) {
        self.device_handles.remove(&handle);
    }

    /// Test accessor: the header far pointer recorded for an open device handle.
    #[cfg(test)]
    fn device_handle_header_for_test(&self, handle: u16) -> Option<FarPtr> {
        self.device_handles.get(&handle).map(|d| d.header)
    }

    pub fn file_count(&self) -> u16 {
        self.file_count.unwrap_or(DEFAULT_FILE_COUNT)
    }

    pub fn next_block_device_drive(&self) -> Option<u8> {
        let used: u16 = self
            .loaded_block_devices
            .iter()
            .map(|device| u16::from(device.installed_units))
            .sum();
        let next = u16::from(FIRST_LOADED_BLOCK_DRIVE).checked_add(used)?;
        u8::try_from(next)
            .ok()
            .filter(|drive| *drive <= MAX_DOS_DRIVE)
    }

    pub fn block_device_io_target(&self, drive: u8) -> Option<BlockDeviceIoTarget> {
        for device in &self.loaded_block_devices {
            if drive < device.first_drive {
                continue;
            }
            let unit = drive - device.first_drive;
            if unit >= device.installed_units {
                continue;
            }
            let bpb = device.bpbs_by_unit.get(usize::from(unit))?.as_ref()?;
            return Some(BlockDeviceIoTarget {
                header: FarPtr {
                    segment: device.header.0,
                    offset: device.header.1,
                },
                unit,
                media: bpb.media,
            });
        }
        None
    }

    pub fn block_device_fs_target(&self, drive: u8) -> Option<BlockDeviceFsTarget> {
        for device in &self.loaded_block_devices {
            if drive < device.first_drive {
                continue;
            }
            let unit = drive - device.first_drive;
            if unit >= device.installed_units {
                continue;
            }
            let bpb = device.bpbs_by_unit.get(usize::from(unit))?.as_ref()?;
            let io = BlockDeviceIoTarget {
                header: FarPtr {
                    segment: device.header.0,
                    offset: device.header.1,
                },
                unit,
                media: bpb.media,
            };
            return Some(BlockDeviceFsTarget {
                io,
                bytes_per_sector: bpb.bytes_per_sector,
                sectors_per_cluster: bpb.cluster_mask + 1,
                first_fat_sector: bpb.first_fat_sector,
                fat_count: bpb.fat_count,
                root_entries: bpb.root_entries,
                first_data_sector: bpb.first_data_sector,
                highest_cluster: bpb.highest_cluster,
                sectors_per_fat: bpb.sectors_per_fat,
                first_root_sector: bpb.first_root_sector,
            });
        }
        None
    }

    fn published_block_dpbs(&self) -> Vec<BlockDeviceDpbEntry> {
        let mut entries = Vec::new();
        let published_drive_count = self.lastdrive.unwrap_or(DEFAULT_LASTDRIVE_COUNT).min(26);
        for device in &self.loaded_block_devices {
            for (unit, bpb) in device.bpbs_by_unit.iter().enumerate() {
                let Some(bpb) = bpb else { continue };
                let Some(drive) = device.first_drive.checked_add(unit as u8) else {
                    continue;
                };
                if drive >= published_drive_count {
                    continue;
                }
                entries.push(BlockDeviceDpbEntry {
                    drive,
                    unit: unit as u8,
                    header: device.header,
                    bpb: *bpb,
                });
            }
        }
        entries
    }

    fn publish_sysvars(&mut self, mem: &mut Memory) -> Result<(u16, u16), DosError> {
        let first_mcb = self.arena.first_mcb();
        let ems_present = self.ems_present;
        let lastdrive = self.lastdrive;
        let file_count = self.file_count();
        let host_files = self.sft_host_file_entries();
        let block_dpbs = self.published_block_dpbs();
        write_sysvars(
            mem,
            first_mcb,
            ems_present,
            lastdrive,
            file_count,
            SysvarsDevices {
                host_files: &host_files,
                block_dpbs: &block_dpbs,
                loaded_devices: &self.loaded_devices,
            },
        )
    }

    pub fn publish_driver_bds(&mut self, mem: &mut Memory) -> Result<(u16, u16), DosError> {
        let _ = self.publish_sysvars(mem)?;
        let block_dpbs = self.published_block_dpbs();
        write_driver_bds(
            mem,
            self.arena.first_mcb(),
            self.lastdrive,
            self.file_count(),
            &block_dpbs,
        )
    }

    fn publish_nls_tables(&self, mem: &mut Memory) -> Result<memory::NlsTablePointers, DosError> {
        let block_dpbs = self.published_block_dpbs();
        write_nls_tables(
            mem,
            self.arena.first_mcb(),
            self.lastdrive,
            self.file_count(),
            block_dpbs.len(),
        )
    }

    fn valid_media_id_drive(&self, drive: u8) -> bool {
        self.drive_is_mounted_c(drive)
    }

    fn write_media_id_packet(&self, mem: &mut Memory, base: usize) -> Result<(), DosError> {
        mem.write_u16(base, 0)?;
        mem.write_u32(base + 2, self.volume_serial)?;
        let label = self.volume_label.unwrap_or(C_DRIVE_VOLUME_LABEL);
        for (index, byte) in label.into_iter().enumerate() {
            mem.write_u8(base + 6 + index, byte)?;
        }
        for (index, byte) in C_DRIVE_FS_TYPE.into_iter().enumerate() {
            mem.write_u8(base + 17 + index, byte)?;
        }
        Ok(())
    }

    fn read_media_id_packet(&mut self, mem: &Memory, base: usize) -> Result<bool, DosError> {
        if mem.read_u16(base)? != 0 {
            return Ok(false);
        }
        self.volume_serial = mem.read_u32(base + 2)?;
        let mut label = [0; 11];
        for (index, slot) in label.iter_mut().enumerate() {
            *slot = mem.read_u8(base + 6 + index)?;
        }
        self.volume_label = Some(label);
        Ok(true)
    }

    fn write_access_flag_packet(&self, mem: &mut Memory, base: usize) -> Result<(), DosError> {
        mem.write_u8(base, 0)?;
        mem.write_u8(base + 1, u8::from(!self.media_access_disabled))?;
        Ok(())
    }

    fn read_access_flag_packet(&mut self, mem: &Memory, base: usize) -> Result<bool, DosError> {
        if mem.read_u8(base)? != 0 {
            return Ok(false);
        }
        self.media_access_disabled = mem.read_u8(base + 1)? == 0;
        Ok(true)
    }

    fn published_dpb_for_drive(
        &mut self,
        mem: &mut Memory,
        requested_drive: u8,
    ) -> Result<Option<(u16, u16)>, DosError> {
        let Some(drive) = self.dos_drive_index(requested_drive) else {
            return Ok(None);
        };
        let (sysvars_seg, sysvars_off) = self.publish_sysvars(mem)?;
        let sysvars = usize::from(sysvars_seg) * 16 + usize::from(sysvars_off);
        let mut dpb_off = mem.read_u16(sysvars)?;
        let mut dpb_seg = mem.read_u16(sysvars + 2)?;
        for _ in 0..26 {
            if dpb_seg == 0xffff && dpb_off == 0xffff {
                return Ok(None);
            }
            let dpb = usize::from(dpb_seg) * 16 + usize::from(dpb_off);
            if mem.read_u8(dpb)? == drive {
                return Ok(Some((dpb_seg, dpb_off)));
            }
            let next_off = mem.read_u16(dpb + 0x19)?;
            let next_seg = mem.read_u16(dpb + 0x1b)?;
            dpb_off = next_off;
            dpb_seg = next_seg;
        }
        Ok(None)
    }

    fn staged_allocation_contains(staged: &StagedDriver, linear: usize, len: usize) -> bool {
        let start = usize::from(staged.allocation_seg) * 16;
        let end = start + usize::from(staged.allocation_paras) * 16;
        linear >= start && linear.checked_add(len).is_some_and(|after| after <= end)
    }

    fn capture_loaded_block_device(
        &self,
        staged: &StagedDriver,
        mem: &Memory,
    ) -> Result<LoadedBlockDevice, DosError> {
        let max_units = 26u8.saturating_sub(staged.first_drive);
        let installed_units = mem.read_u8(staged.request_linear + 0x0d)?.min(max_units);
        let array_off = mem.read_u16(staged.request_linear + 0x12)?;
        let array_seg = mem.read_u16(staged.request_linear + 0x14)?;
        let array_linear = usize::from(array_seg) * 16 + usize::from(array_off);
        let array_len = usize::from(installed_units) * 2;
        let array_inside = Self::staged_allocation_contains(staged, array_linear, array_len);
        let mut bpbs_by_unit = Vec::with_capacity(usize::from(installed_units));
        for unit in 0..installed_units {
            if !array_inside {
                bpbs_by_unit.push(None);
                continue;
            }
            let offset_word = mem.read_u16(array_linear + usize::from(unit) * 2)?;
            let bpb_linear = usize::from(staged.driver_seg) * 16 + usize::from(offset_word);
            if !Self::staged_allocation_contains(staged, bpb_linear, BLOCK_BPB_LEN) {
                bpbs_by_unit.push(None);
                continue;
            }
            let mut raw = [0u8; BLOCK_BPB_LEN];
            for (i, byte) in raw.iter_mut().enumerate() {
                *byte = mem.read_u8(bpb_linear + i)?;
            }
            bpbs_by_unit.push(BlockDeviceBpb::from_bytes(&raw));
        }
        Ok(LoadedBlockDevice {
            header: (staged.driver_seg, 0),
            first_drive: staged.first_drive,
            installed_units,
            bpbs_by_unit,
        })
    }

    fn sft_host_file_entries(&mut self) -> Vec<SftHostFileEntry> {
        let mut entries = Vec::with_capacity(self.open_files.len());
        for (&slot, open) in &mut self.open_files {
            let (size, position) = match &open.target {
                OutputTarget::Console => continue,
                OutputTarget::Host(host) => {
                    let mut file = host.borrow_mut();
                    let size = file
                        .metadata()
                        .map(|meta| meta.len().min(u64::from(u32::MAX)) as u32)
                        .unwrap_or(0);
                    let position = file
                        .stream_position()
                        .map(|pos| pos.min(u64::from(u32::MAX)) as u32)
                        .unwrap_or(0);
                    (size, position)
                }
                OutputTarget::Memory(file) => {
                    let file = file.borrow();
                    (file.size_u32(), file.position)
                }
            };
            entries.push(SftHostFileEntry {
                slot,
                open_mode: open.mode.sft_open_mode(),
                size,
                position,
                name: open.sft_name,
            });
        }
        entries.sort_by_key(|entry| entry.slot);
        entries
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
        let result = allocate_strategy(
            &mut self.arena,
            self.umb,
            self.umb_link,
            self.alloc_strategy,
            paras,
            mem,
        )?;
        if let Ok(seg) = result {
            set_umb_owner(self.umb, seg, self.arena.psp_seg, mem)?;
        }
        Ok(result)
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
        // A warm reboot reloads CONFIG.SYS drivers from scratch; drop the prior
        // session's loaded-device list so the chain starts at the bare skeleton,
        // and the handles opened on them.
        self.loaded_devices.clear();
        self.loaded_block_devices.clear();
        self.device_handles.clear();
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

    fn name_targets_mounted_c(&self, name: &str) -> bool {
        let bytes = name.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' {
            bytes[0].eq_ignore_ascii_case(&b'C')
        } else {
            self.current_drive() == 2
        }
    }

    /// Resolve a DOS filename (drive-qualified, absolute, or relative to the
    /// current directory) to a host path under the mounted C: drive, or a DOS
    /// error code (0x02 no drive).
    fn resolve_name(&self, name: &str) -> Result<PathBuf, u16> {
        let Some(drive) = self.drive.as_ref() else {
            return Err(0x02);
        };
        if !self.name_targets_mounted_c(name) {
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
            Err(err) => return Ok(Err(dos_io_error_code_for_path(&err, &path))),
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
        // Default JFT at 0x18: stdin/stdout/stderr share CON slot 1, AUX is slot
        // 3, PRN is slot 4, and the rest are closed (0xFF).
        for off in 0x18u16..0x2cu16 {
            mem.write_u8(psp + usize::from(off), 0xff)?;
        }
        mem.write_u8(psp + 0x18, 0x01)?;
        mem.write_u8(psp + 0x19, 0x01)?;
        mem.write_u8(psp + 0x1a, 0x01)?;
        mem.write_u8(psp + 0x1b, 0x03)?;
        mem.write_u8(psp + 0x1c, 0x04)?;
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
            open_files: self.open_files.clone(),
            ems_handles: self.ems_handles.clone(),
            device_handles: self.device_handles.clone(),
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
        // Seed any standard handle the child did not inherit; an inherited
        // redirect (a Host target carried in the live open_files) is preserved.
        self.seed_standard_handles();

        Ok(DosAction::Exec { entry, child_ax })
    }

    /// AH=4Bh AL=1: load a program and return its initial SS:SP and CS:IP in the
    /// EXEC parameter block without transferring control.
    fn exec_load_no_execute(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let last_exit_code = self.last_exit_code;
        let last_exit_type = self.last_exit_type;
        let action = self.exec_load_and_execute(mem, regs)?;
        let DosAction::Exec { entry, child_ax } = action else {
            return Ok(action);
        };
        let Some(parent) = self.program_stack.pop() else {
            return Ok(DosAction::Continue);
        };
        self.arena = parent.arena;
        self.dta = parent.dta;
        self.find_searches = parent.find_searches;
        self.open_files = parent.open_files;
        self.ems_handles = parent.ems_handles;
        self.device_handles = parent.device_handles;
        self.last_exit_code = last_exit_code;
        self.last_exit_type = last_exit_type;

        let sp = entry.sp.wrapping_sub(2);
        mem.write_u16((usize::from(entry.ss) * 16) + usize::from(sp), child_ax)?;
        let epb = usize::from(regs.es) * 16 + usize::from(regs.bx);
        mem.write_u16(epb + 0x0e, sp)?;
        mem.write_u16(epb + 0x10, entry.ss)?;
        mem.write_u16(epb + 0x12, entry.ip)?;
        mem.write_u16(epb + 0x14, entry.cs)?;
        regs.cf = false;
        Ok(DosAction::Continue)
    }

    /// Restore the parent program's DOS state after a child exits with `code`,
    /// and record the exit code/type for AH=4Dh. Called by the machine when it
    /// pops a parent frame.
    pub fn finish_exec(&mut self, code: u8, mem: &mut Memory) -> Result<(), DosError> {
        // The exiting child's conventional blocks (env + program, above the parent
        // free base) are freed back to the parent, UNLESS the child itself kept
        // resident (a TSR), in which case keep_resident already left a correct free
        // tail above its block. AH=48h blocks allocated from the UMB arena are
        // owner-tagged with the child's PSP and are swept here on every terminating
        // exit (normal AH=4Ch/INT 20h and abnormal Ctrl-C/critical-error aborts all
        // funnel through finish_exec); TSRs keep their UMBs because the resident
        // program may still use them.
        let child_resident = self.arena.resident;
        let child_psp = self.arena.psp_seg;
        if let Some(parent) = self.program_stack.pop() {
            self.arena = parent.arena;
            self.dta = parent.dta;
            self.find_searches = parent.find_searches;
            self.open_files = parent.open_files;
            self.ems_handles = parent.ems_handles;
            self.device_handles = parent.device_handles;
            if !child_resident {
                free_umb_blocks_owned_by(self.umb, child_psp, mem)?;
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
        self.last_exit_type = if child_resident { 0x03 } else { 0x00 };
        Ok(())
    }

    fn restore_psp_saved_vectors(&self, mem: &mut Memory) -> Result<(), DosError> {
        let psp = usize::from(self.arena.psp_seg) * 16;
        for (psp_off, int_no) in [(0x0au16, 0x22u8), (0x0e, 0x23), (0x12, 0x24)] {
            let ivt = usize::from(int_no) * 4;
            mem.write_u16(ivt, mem.read_u16(psp + usize::from(psp_off))?)?;
            mem.write_u16(ivt + 2, mem.read_u16(psp + usize::from(psp_off) + 2)?)?;
        }
        Ok(())
    }

    fn keep_process_resident(&mut self, paras: u16, mem: &mut Memory) -> Result<(), DosError> {
        self.arena.keep_resident(paras, mem)?;
        // Record the resident region's base so an ancestor's exit will not reclaim
        // it as the EXEC chain unwinds past this resident block.
        self.resident_regions.push(self.arena.chain_first);
        Ok(())
    }

    /// Split a FindFirst filespec into (host directory, final-component pattern).
    /// Ok((dir, pattern)) on success; Err(code) is a DOS error code (0x02 no drive,
    /// 0x03 bad/non-C path). The pattern is the last path component and may hold
    /// wildcards. Relative specs use the current directory, matching the normal
    /// file calls.
    fn split_find_spec(&self, filespec: &str) -> Result<(PathBuf, String), u16> {
        let drive = self.drive.as_ref().ok_or(0x02u16)?;
        let spec = filespec.trim();
        let after_drive =
            if let Some(rest) = spec.strip_prefix("C:").or_else(|| spec.strip_prefix("c:")) {
                rest
            } else if spec.as_bytes().get(1) == Some(&b':') {
                return Err(0x03); // a drive letter other than C: (we mount only C:)
            } else if self.current_drive() != 2 {
                return Err(0x03);
            } else {
                spec
            };
        let normalized = after_drive.replace('/', "\\");
        let mut parts = normalized
            .split('\\')
            .filter(|c| !c.is_empty())
            .collect::<Vec<_>>();
        let pattern = parts.pop().unwrap_or_default().to_string();
        let mut components = Vec::new();
        if !normalized.starts_with('\\') {
            components.extend(
                self.cwd
                    .split('\\')
                    .filter(|c| !c.is_empty())
                    .map(str::to_string),
            );
        }
        for component in parts {
            match component {
                "." => {}
                ".." => {
                    components.pop();
                }
                other => components.push(other.to_string()),
            }
        }
        let mut dir = drive.root().to_path_buf();
        for component in components {
            dir.push(component);
        }
        Ok((dir, pattern))
    }

    pub fn find_first_from_entries(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
        pattern: &str,
        entries: &[DosFindEntry],
    ) -> Result<DosAction, DosError> {
        let mask = regs.cx as u8;
        let pattern_template = pattern_to_8_3(pattern);
        let entries = entries
            .iter()
            .filter(|entry| {
                attr_matches(entry.attr, mask)
                    && template_matches(&entry.name_8_3, &pattern_template)
            })
            .map(|entry| FindEntry {
                attr: entry.attr,
                time: entry.time,
                date: entry.date,
                size: entry.size,
                raw_name: entry.name_8_3,
                name: find_name_from_8_3(&entry.name_8_3),
            })
            .collect::<Vec<_>>();
        let Some(first) = entries.first().cloned() else {
            self.fail_find_first(regs, 0x12);
            return Ok(DosAction::Continue);
        };
        write_find_record(mem, self.dta, &first)?;
        self.find_searches
            .insert(self.dta, FindSearch { entries, next: 1 });
        regs.cf = false;
        Ok(DosAction::Continue)
    }

    /// Return the normal 37-byte FCB subrecord. An extended FCB starts with a 0xFF
    /// marker, five reserved bytes, an attribute byte, then the normal FCB at +7.
    fn fcb_body_base(&self, mem: &Memory, base: usize) -> Result<usize, DosError> {
        if mem.read_u8(base)? == 0xff {
            Ok(base + 7)
        } else {
            Ok(base)
        }
    }

    fn fcb_body_base_for_regs(&self, mem: &Memory, regs: &DosRegs) -> Result<usize, DosError> {
        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
        self.fcb_body_base(mem, base)
    }

    /// The HLE mounts only C:. FCB drive byte 0 means the selected default drive.
    fn fcb_drive_targets_mounted_c(&self, drive: u8) -> bool {
        if drive == 0 {
            self.current_drive() == 2
        } else {
            drive == 3
        }
    }

    /// Resolve the file named by an FCB subrecord to a host path. The FCB 8.3
    /// fields name the file relative to the selected drive's root (no path, like
    /// the FCB API). Ok(Ok(path)) on success; Ok(Err(())) when the FCB names no
    /// resolvable file or an unmounted drive.
    fn fcb_path_at(
        &self,
        mem: &Memory,
        base: usize,
        name_off: usize,
    ) -> Result<Result<PathBuf, ()>, DosError> {
        if !self.fcb_drive_targets_mounted_c(mem.read_u8(base)?) {
            return Ok(Err(()));
        }
        let name = fcb_name(mem, base, name_off)?;
        if name.is_empty() {
            return Ok(Err(()));
        }
        Ok(self.resolve_name(&name).map_err(|_| ()))
    }

    /// Resolve the file named by the FCB at DS:DX to a host path, accepting either
    /// a normal FCB or an extended FCB wrapper.
    fn fcb_path(&self, mem: &Memory, regs: &DosRegs) -> Result<Result<PathBuf, ()>, DosError> {
        let base = self.fcb_body_base_for_regs(mem, regs)?;
        self.fcb_path_at(mem, base, FCB_NAME)
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
        let raw_base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
        let base = self.fcb_body_base(mem, raw_base)?;
        let create_attrs = if create && base != raw_base {
            u16::from(mem.read_u8(raw_base + 6)?)
        } else {
            0
        };
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
        if create && base != raw_base && apply_create_attributes(&path, create_attrs).is_err() {
            regs.ax = (regs.ax & 0xff00) | 0xff;
            return Ok(DosAction::Continue);
        }
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
        let base = self.fcb_body_base_for_regs(mem, regs)?;
        let name = fcb_name(mem, base, FCB_NAME)?;
        let Some(drive) = self.drive.as_ref() else {
            regs.ax = (regs.ax & 0xff00) | 0xff;
            return Ok(DosAction::Continue);
        };
        if !self.fcb_drive_targets_mounted_c(mem.read_u8(base)?) {
            regs.ax = (regs.ax & 0xff00) | 0xff;
            return Ok(DosAction::Continue);
        }
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
    /// returned; a normal FCB returns normal files only. Extended FCB searches
    /// with attribute 0x08 see the stored volume label and, for a pure 0x08 mask,
    /// do not fall through to normal files.
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
        if search_attr & 0x08 != 0 {
            if let Some(label) = self.volume_label {
                if template_matches(&label, &pattern) {
                    entries.push(volume_label_find_entry(label));
                }
            }
        }
        if search_attr != 0x08 {
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
                        raw_name: template,
                        name: host.to_ascii_uppercase(),
                    });
                }
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
        let base = self.fcb_body_base_for_regs(mem, regs)?;
        let al = match (
            self.fcb_path_at(mem, base, FCB_NAME)?,
            self.fcb_path_at(mem, base, FCB_RENAME_NEW)?,
        ) {
            (Ok(old), Ok(new)) if std::fs::rename(&old, &new).is_ok() => 0x00,
            _ => 0xff,
        };
        regs.ax = (regs.ax & 0xff00) | al;
        Ok(DosAction::Continue)
    }

    /// AH=14h SEQUENTIAL READ. Read one record (FCB record size) from the file
    /// position the current block/record select into the DTA, then advance the
    /// record number. AL=00 read in full, 01 EOF/no data, 02 DTA segment wrap,
    /// 03 a partial record (the last record, zero-padded into the DTA).
    fn fcb_seq_read(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let base = self.fcb_body_base_for_regs(mem, regs)?;
        let path = match self.fcb_path(mem, regs)? {
            Ok(path) => path,
            Err(()) => {
                regs.ax = (regs.ax & 0xff00) | 0x01;
                return Ok(DosAction::Continue);
            }
        };
        let record_size = mem.read_u16(base + FCB_RECSIZE)?;
        let block = mem.read_u16(base + FCB_BLOCK)?;
        let current = mem.read_u8(base + FCB_CURREC)?;
        let pos = fcb_seq_position(block, current, record_size);
        let size = if record_size == 0 { 128 } else { record_size };
        if !fcb_dta_transfer_fits(self.dta, usize::from(size)) {
            regs.ax = (regs.ax & 0xff00) | 0x02;
            return Ok(DosAction::Continue);
        }
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(_) => {
                regs.ax = (regs.ax & 0xff00) | 0x01;
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
    /// number. AL=00 on success, 01 on a host write error, 02 on DTA segment wrap.
    fn fcb_seq_write(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let base = self.fcb_body_base_for_regs(mem, regs)?;
        let path = match self.fcb_path(mem, regs)? {
            Ok(path) => path,
            Err(()) => {
                regs.ax = (regs.ax & 0xff00) | 0x01;
                return Ok(DosAction::Continue);
            }
        };
        let record_size = mem.read_u16(base + FCB_RECSIZE)?;
        let block = mem.read_u16(base + FCB_BLOCK)?;
        let current = mem.read_u8(base + FCB_CURREC)?;
        let pos = fcb_seq_position(block, current, record_size);
        let size = if record_size == 0 { 128 } else { record_size };
        if !fcb_dta_transfer_fits(self.dta, usize::from(size)) {
            regs.ax = (regs.ax & 0xff00) | 0x02;
            return Ok(DosAction::Continue);
        }
        let dta = usize::from(self.dta.0) * 16 + usize::from(self.dta.1);
        let mut record = vec![0u8; usize::from(size)];
        for (i, slot) in record.iter_mut().enumerate() {
            *slot = mem.read_u8(dta + i)?;
        }
        let mut file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => f,
            Err(_) => {
                regs.ax = (regs.ax & 0xff00) | 0x01;
                return Ok(DosAction::Continue);
            }
        };
        if file.seek(SeekFrom::Start(pos)).is_err() || file.write_all(&record).is_err() {
            regs.ax = (regs.ax & 0xff00) | 0x01;
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
        let base = self.fcb_body_base_for_regs(mem, regs)?;
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
    /// untouched), 02 DTA segment wrap, 03 partial final record (zero-padded).
    fn fcb_random_read(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let base = self.fcb_body_base_for_regs(mem, regs)?;
        let path = match self.fcb_path(mem, regs)? {
            Ok(path) => path,
            Err(()) => {
                regs.ax = (regs.ax & 0xff00) | 0x01;
                return Ok(DosAction::Continue);
            }
        };
        let record_size = mem.read_u16(base + FCB_RECSIZE)?;
        let random = mem.read_u32(base + FCB_RANDREC)?;
        fcb_sync_block_record_from_random(mem, base, random)?;
        let size = if record_size == 0 { 128 } else { record_size };
        let pos = u64::from(random) * u64::from(size);
        if !fcb_dta_transfer_fits(self.dta, usize::from(size)) {
            regs.ax = (regs.ax & 0xff00) | 0x02;
            return Ok(DosAction::Continue);
        }
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(_) => {
                regs.ax = (regs.ax & 0xff00) | 0x01;
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
    /// record. AL=00 success, 01 on a host write error, 02 on DTA segment wrap.
    fn fcb_random_write(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let base = self.fcb_body_base_for_regs(mem, regs)?;
        let path = match self.fcb_path(mem, regs)? {
            Ok(path) => path,
            Err(()) => {
                regs.ax = (regs.ax & 0xff00) | 0x01;
                return Ok(DosAction::Continue);
            }
        };
        let record_size = mem.read_u16(base + FCB_RECSIZE)?;
        let random = mem.read_u32(base + FCB_RANDREC)?;
        fcb_sync_block_record_from_random(mem, base, random)?;
        let size = if record_size == 0 { 128 } else { record_size };
        let pos = u64::from(random) * u64::from(size);
        if !fcb_dta_transfer_fits(self.dta, usize::from(size)) {
            regs.ax = (regs.ax & 0xff00) | 0x02;
            return Ok(DosAction::Continue);
        }
        let dta = usize::from(self.dta.0) * 16 + usize::from(self.dta.1);
        let mut record = vec![0u8; usize::from(size)];
        for (i, slot) in record.iter_mut().enumerate() {
            *slot = mem.read_u8(dta + i)?;
        }
        let mut file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => f,
            Err(_) => {
                regs.ax = (regs.ax & 0xff00) | 0x01;
                return Ok(DosAction::Continue);
            }
        };
        if file.seek(SeekFrom::Start(pos)).is_err() || file.write_all(&record).is_err() {
            regs.ax = (regs.ax & 0xff00) | 0x01;
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
        let base = self.fcb_body_base_for_regs(mem, regs)?;
        let block = mem.read_u16(base + FCB_BLOCK)?;
        let current = mem.read_u8(base + FCB_CURREC)?;
        let random = u32::from(block) * 128 + u32::from(current);
        mem.write_u32(base + FCB_RANDREC, random)?;
        Ok(DosAction::Continue)
    }

    /// AH=27h RANDOM BLOCK READ. Read CX records starting at the random record into
    /// the DTA, packed back to back. CX returns the count actually read; the random
    /// record and the block/record cursor advance past the last record. AL=00 all
    /// records read, 01 EOF/no data, 02 DTA segment wrap, 03 a partial final
    /// record (zero-padded).
    fn fcb_random_block_read(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let base = self.fcb_body_base_for_regs(mem, regs)?;
        let path = match self.fcb_path(mem, regs)? {
            Ok(path) => path,
            Err(()) => {
                regs.cx = 0;
                regs.ax = (regs.ax & 0xff00) | 0x01;
                return Ok(DosAction::Continue);
            }
        };
        let record_size = mem.read_u16(base + FCB_RECSIZE)?;
        let size = if record_size == 0 { 128 } else { record_size };
        let start = mem.read_u32(base + FCB_RANDREC)?;
        let wanted = regs.cx;
        if !fcb_dta_transfer_fits(
            self.dta,
            usize::from(wanted).saturating_mul(usize::from(size)),
        ) {
            regs.cx = 0;
            regs.ax = (regs.ax & 0xff00) | 0x02;
            return Ok(DosAction::Continue);
        }
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(_) => {
                regs.cx = 0;
                regs.ax = (regs.ax & 0xff00) | 0x01;
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
    /// 01 on a host write error, 02 on DTA segment wrap.
    fn fcb_random_block_write(
        &mut self,
        mem: &mut Memory,
        regs: &mut DosRegs,
    ) -> Result<DosAction, DosError> {
        let base = self.fcb_body_base_for_regs(mem, regs)?;
        let path = match self.fcb_path(mem, regs)? {
            Ok(path) => path,
            Err(()) => {
                regs.cx = 0;
                regs.ax = (regs.ax & 0xff00) | 0x01;
                return Ok(DosAction::Continue);
            }
        };
        let record_size = mem.read_u16(base + FCB_RECSIZE)?;
        let size = if record_size == 0 { 128 } else { record_size };
        let start = mem.read_u32(base + FCB_RANDREC)?;
        let wanted = regs.cx;
        if wanted != 0
            && !fcb_dta_transfer_fits(
                self.dta,
                usize::from(wanted).saturating_mul(usize::from(size)),
            )
        {
            regs.cx = 0;
            regs.ax = (regs.ax & 0xff00) | 0x02;
            return Ok(DosAction::Continue);
        }
        let mut file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => f,
            Err(_) => {
                regs.cx = 0;
                regs.ax = (regs.ax & 0xff00) | 0x01;
                return Ok(DosAction::Continue);
            }
        };
        if wanted == 0 {
            // CX=0: set the file size to start*record-size, no record transfer.
            let len = u64::from(start) * u64::from(size);
            if file.set_len(len).is_err() {
                regs.ax = (regs.ax & 0xff00) | 0x01;
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
                regs.ax = (regs.ax & 0xff00) | 0x01;
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
            0x20 => {
                self.restore_psp_saved_vectors(mem)?;
                Ok(DosAction::Exit(0))
            }
            0x27 => {
                let bytes = regs.dx.clamp(DOS_INT27_MIN_RESIDENT_BYTES, 0xfff0);
                let paras = bytes.div_ceil(16);
                self.keep_process_resident(paras, mem)?;
                Ok(DosAction::Exit(0))
            }
            0x21 => {
                let action = self.dispatch_int21(regs, mem)?;
                // Any INT 21h call returning with carry set has placed its DOS
                // error code in AX. Record it here so a later AH=59h reports the
                // most recent failure, covering every set_dos_error site, not just
                // the handlers that route through fail().
                if regs.cf {
                    self.extended_error = ExtendedError::from_code(regs.ax);
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

    fn ctrl_c_action(&mut self) -> DosAction {
        DosAction::InvokeInterrupt(0x23)
    }

    fn consume_pending_ctrl_c(&mut self, mem: &mut Memory) -> Result<bool, DosError> {
        if self.pending_scancode.is_some() {
            return Ok(false);
        }
        if matches!(kbd_ring_peek(mem)?, Some((_, 0x03))) {
            let _ = kbd_ring_dequeue(mem)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Write one byte to a standard output handle (1 = stdout, 2 = stderr). A
    /// Console target, or an unseeded standard handle, reaches the screen buffer;
    /// a redirected Host target writes the byte to its file. A read-only memory
    /// file redirected onto stdout/stderr rejects character writes just like AH=40h.
    /// Returns a DOS error code on write fault.
    fn write_console_handle(&mut self, handle: u16, byte: u8) -> Result<(), u16> {
        match self.open_files.get(&handle).map(|of| &of.target) {
            Some(OutputTarget::Host(file)) => file
                .borrow_mut()
                .write_all(&[byte])
                .map_err(|e| dos_io_error_code(&e)),
            Some(OutputTarget::Memory(_)) => Err(0x05),
            _ => {
                self.stdout.push(byte);
                Ok(())
            }
        }
    }

    /// Read one character from the keyboard ring. Some -> set AL (and echo when
    /// asked) and Continue; None -> WaitForKey so the caller re-runs the INT.
    fn read_char(
        &mut self,
        regs: &mut DosRegs,
        mem: &mut Memory,
        echo: bool,
        check_ctrl_c: bool,
    ) -> Result<DosAction, DosError> {
        match self.next_cooked_char(mem)? {
            Some((ch, extended)) => {
                if check_ctrl_c && ch == 0x03 && !extended {
                    return Ok(self.ctrl_c_action());
                }
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
        let template_len = usize::from(mem.read_u8(buf + 1)?.min(max.saturating_sub(1)));
        let mut line = match self.pending_line.take() {
            Some(line) if line.addr == addr => line,
            _ => {
                let mut template = Vec::with_capacity(template_len);
                for i in 0..template_len {
                    template.push(mem.read_u8(buf + 2 + i)?);
                }
                PendingLine {
                    addr,
                    count: 0,
                    template,
                    template_index: 0,
                }
            }
        };
        loop {
            let Some((scancode, ascii)) = kbd_ring_dequeue(mem)? else {
                self.pending_line = Some(line);
                return Ok(DosAction::WaitForKey);
            };
            if ascii == 0x03 {
                self.pending_line = None;
                return Ok(self.ctrl_c_action());
            }
            let mut push_char = |ch: u8, line: &mut PendingLine| -> Result<bool, DosError> {
                if usize::from(line.count) + 1 < usize::from(max) {
                    mem.write_u8(buf + 2 + usize::from(line.count), ch)?;
                    line.count += 1;
                    self.stdout.push(ch);
                    Ok(true)
                } else {
                    self.stdout.push(0x07); // buffer full, bell
                    Ok(false)
                }
            };
            match ascii {
                0x0d => {
                    mem.write_u8(buf + 2 + usize::from(line.count), 0x0d)?;
                    self.stdout.push(0x0d);
                    mem.write_u8(buf + 1, line.count)?;
                    self.pending_line = None;
                    return Ok(DosAction::Continue);
                }
                0x08 => {
                    if line.count > 0 {
                        line.count -= 1;
                        self.stdout.extend_from_slice(&[0x08, 0x20, 0x08]);
                    }
                }
                0x00 => match scancode {
                    // F1 copies one character from the recall template.
                    0x3b => {
                        if let Some(&ch) = line.template.get(line.template_index) {
                            if push_char(ch, &mut line)? {
                                line.template_index += 1;
                            }
                        }
                    }
                    // F3 copies the rest of the recall template.
                    0x3d => {
                        while let Some(&ch) = line.template.get(line.template_index) {
                            if !push_char(ch, &mut line)? {
                                break;
                            }
                            line.template_index += 1;
                        }
                    }
                    // F5 makes the current input the new recall template.
                    0x3f => {
                        let mut next_template = Vec::with_capacity(usize::from(line.count));
                        for i in 0..usize::from(line.count) {
                            next_template.push(mem.read_u8(buf + 2 + i)?);
                        }
                        line.template = next_template;
                        line.template_index = 0;
                        line.count = 0;
                    }
                    // Del skips one character in the recall template.
                    0x53 if line.template_index < line.template.len() => {
                        line.template_index += 1;
                    }
                    _ => {}
                },
                _ => {
                    let _ = push_char(ascii, &mut line)?;
                }
            }
        }
    }

    /// Record a DOS error code for AH=59h, then set the standard CF/AX error
    /// return. The new (AH=59h-aware) handlers route their failures through this
    /// so the extended-error query has a value to report.
    fn fail(&mut self, regs: &mut DosRegs, code: u16) {
        self.extended_error = ExtendedError::from_code(code);
        set_dos_error(regs, code);
    }

    pub fn fail_regs(&mut self, regs: &mut DosRegs, code: u16) {
        self.fail(regs, code);
    }

    pub fn fail_find_first(&mut self, regs: &mut DosRegs, code: u16) {
        self.find_searches.remove(&self.dta);
        self.fail(regs, code);
    }

    /// Record a DOS error code that was discovered after the kernel yielded a
    /// follow-up action to the machine, such as a loaded-driver request failure.
    pub fn record_last_error(&mut self, code: u16) {
        self.extended_error = ExtendedError::from_code(code);
    }

    pub fn open_readonly_memory_file(
        &mut self,
        regs: &mut DosRegs,
        name: [u8; 11],
        bytes: Vec<u8>,
    ) {
        let Some(mode) = AccessMode::try_from_open_al(regs.ax as u8) else {
            self.fail(regs, 0x0c);
            return;
        };
        if mode != AccessMode::Read {
            self.fail(regs, 0x05);
            return;
        }
        let Some(handle) = self.alloc_handle() else {
            self.fail(regs, 0x04);
            return;
        };
        self.open_files
            .insert(handle, open_memory_file_record(name, bytes));
        regs.ax = handle;
        regs.cf = false;
    }

    pub fn alloc_internal_scratch(
        &mut self,
        paras: u16,
        mem: &mut Memory,
    ) -> Result<Result<u16, u16>, DosError> {
        self.arena.allocate(paras, mem)
    }

    pub fn free_internal_scratch(
        &mut self,
        seg: u16,
        mem: &mut Memory,
    ) -> Result<Result<(), ()>, DosError> {
        self.free_routed(seg, mem)
    }

    fn refresh_sda(&self, mem: &mut Memory) -> Result<(u16, u16), DosError> {
        let published_block_count = self.published_block_dpbs().len();
        write_sda(
            mem,
            self.arena.first_mcb(),
            self.lastdrive,
            self.file_count(),
            published_block_count,
            SdaSnapshot {
                last_error: self.extended_error.code,
                current_dta: self.dta,
                current_psp: self.arena.psp_seg,
                last_exit_code: self.last_exit_code,
                last_exit_type: self.last_exit_type,
                critical_error: self.critical_error.map(|active| SdaCriticalError {
                    drive: active.drive,
                }),
            },
        )
    }

    fn refresh_sda_list(&self, mem: &mut Memory) -> Result<(u16, u16), DosError> {
        let published_block_count = self.published_block_dpbs().len();
        write_sda_list(
            mem,
            self.arena.first_mcb(),
            self.lastdrive,
            self.file_count(),
            published_block_count,
            SdaSnapshot {
                last_error: self.extended_error.code,
                current_dta: self.dta,
                current_psp: self.arena.psp_seg,
                last_exit_code: self.last_exit_code,
                last_exit_type: self.last_exit_type,
                critical_error: self.critical_error.map(|active| SdaCriticalError {
                    drive: active.drive,
                }),
            },
        )
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
            0x01 => self.read_char(regs, mem, true, true),
            // AH=02h: write the byte in DL to standard output. AL returns it (DOS 2+).
            0x02 => {
                if self.consume_pending_ctrl_c(mem)? {
                    return Ok(self.ctrl_c_action());
                }
                let ch = regs.dx as u8;
                if let Err(code) = self.write_console_handle(1, ch) {
                    set_dos_error(regs, code);
                    return Ok(DosAction::Continue);
                }
                regs.ax = (regs.ax & 0xff00) | u16::from(ch);
                Ok(DosAction::Continue)
            }
            // AH=03h: read one byte from STDAUX. The current DOS facade has no
            // serial receive source, matching BIOS INT 14h's receive-timeout
            // limit, so return a deterministic NUL byte instead of an unwakeable
            // wait.
            0x03 => {
                if self.consume_pending_ctrl_c(mem)? {
                    return Ok(self.ctrl_c_action());
                }
                regs.ax &= 0xff00;
                regs.cf = false;
                Ok(DosAction::Continue)
            }
            // AH=04h/05h: write DL to STDAUX/STDPRN. The current HLE has no
            // serial or printer TX sink, so accept the byte and discard it.
            0x04 | 0x05 => {
                if self.consume_pending_ctrl_c(mem)? {
                    return Ok(self.ctrl_c_action());
                }
                let ch = regs.dx as u8;
                regs.ax = (regs.ax & 0xff00) | u16::from(ch);
                regs.cf = false;
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
                    if let Err(code) = self.write_console_handle(1, ch) {
                        set_dos_error(regs, code);
                        return Ok(DosAction::Continue);
                    }
                    regs.ax = (regs.ax & 0xff00) | u16::from(ch);
                }
                Ok(DosAction::Continue)
            }
            // AH=08h: read one character without echo from the keyboard ring. An empty
            // ring blocks via WaitForKey, the same as AH=01h.
            0x08 => self.read_char(regs, mem, false, true),
            // AH=07h: read one character, no echo, no Ctrl-C check. Blocks.
            0x07 => self.read_char(regs, mem, false, false),
            // AH=0Ah: buffered line input into DS:DX. Blocks until CR.
            0x0a => self.buffered_input(regs, mem),
            // AH=0Bh: get input status. ZF set and AL=0 when empty, ZF clear and
            // AL=0xFF when a character is waiting. Does not consume the character.
            0x0b => {
                if self.consume_pending_ctrl_c(mem)? {
                    return Ok(self.ctrl_c_action());
                }
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
                    0x01 => self.read_char(regs, mem, true, true)?,
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
                    0x07 => self.read_char(regs, mem, false, false)?,
                    0x08 => self.read_char(regs, mem, false, true)?,
                    0x0a => self.buffered_input(regs, mem)?,
                    _ => DosAction::Continue,
                };
                if !matches!(result, DosAction::WaitForKey) {
                    self.cooked_flush_done = false;
                }
                Ok(result)
            }
            // AH=09h: write '$'-terminated string at DS:DX to stdout.
            0x09 => {
                if self.consume_pending_ctrl_c(mem)? {
                    return Ok(self.ctrl_c_action());
                }
                let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                let mut offset = 0usize;
                loop {
                    let byte = mem.read_u8(base + offset)?;
                    if byte == b'$' {
                        break;
                    }
                    if let Err(code) = self.write_console_handle(1, byte) {
                        set_dos_error(regs, code);
                        return Ok(DosAction::Continue);
                    }
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
                    // A loaded CHARACTER device opened by name. Record the handle
                    // against the driver header so read/write/IOCTL route to it.
                    if let Some(header) = self.find_loaded_character_device(mem, &name) {
                        let Some(mode) = AccessMode::try_from_open_al(regs.ax as u8) else {
                            set_dos_error(regs, 0x0c); // invalid access mode
                            return Ok(DosAction::Continue);
                        };
                        let Some(handle) = self.alloc_handle() else {
                            set_dos_error(regs, 0x04); // too many open files
                            return Ok(DosAction::Continue);
                        };
                        self.device_handles
                            .insert(handle, OpenDeviceHandle { header, mode });
                        regs.ax = handle;
                        regs.cf = false;
                        if self.device_supports_open_close(mem, header) {
                            return Ok(DosAction::CallDevice {
                                header,
                                command: 0x0d,
                                transfer: FarPtr::default(),
                                count: 0,
                                success_ax: Some(handle),
                                rollback_handle_on_error: Some(handle),
                            });
                        }
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
                        self.open_files
                            .insert(handle, open_file_record(file, mode, &path));
                        regs.ax = handle;
                        regs.cf = false;
                    }
                    Err(err) => set_dos_error(regs, dos_io_error_code_for_path(&err, &path)),
                }
                Ok(DosAction::Continue)
            }
            0x3f => {
                let handle = regs.bx;
                let count = usize::from(regs.cx);
                // Predefined stdin (CON): read cooked keyboard bytes through the
                // caller's buffer. A console read blocks until at least one byte is
                // available, stops once CX bytes or CR is consumed, and echoes normal
                // characters like the BIOS/DOS console path. Extended-key bytes are
                // delivered by next_cooked_char's existing 00h+scancode state machine
                // but are not echoed.
                if handle == 0 {
                    if count == 0 {
                        regs.ax = 0;
                        regs.cf = false;
                        return Ok(DosAction::Continue);
                    }
                    // A redirected stdin reads file bytes synchronously and returns
                    // 0 at EOF with no WaitForKey. Console keeps the cooked keyboard
                    // path below.
                    match self.open_files.get(&0).map(|of| of.target.clone()) {
                        Some(OutputTarget::Host(file)) => {
                            let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                            let mut buffer = vec![0u8; count];
                            let mut filled = 0usize;
                            {
                                let mut f = file.borrow_mut();
                                while filled < count {
                                    match f.read(&mut buffer[filled..]) {
                                        Ok(0) => break,
                                        Ok(n) => filled += n,
                                        Err(err) => {
                                            set_dos_error(regs, dos_io_error_code(&err));
                                            return Ok(DosAction::Continue);
                                        }
                                    }
                                }
                            }
                            for (index, &byte) in buffer[..filled].iter().enumerate() {
                                mem.write_u8(base + index, byte)?;
                            }
                            regs.ax = filled as u16;
                            regs.cf = false;
                            return Ok(DosAction::Continue);
                        }
                        Some(OutputTarget::Memory(file)) => {
                            let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                            let filled = file.borrow_mut().read_to_guest(mem, base, count)?;
                            regs.ax = filled as u16;
                            regs.cf = false;
                            return Ok(DosAction::Continue);
                        }
                        _ => {}
                    }
                    let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                    let mut filled = 0usize;
                    while filled < count {
                        let Some((ch, extended)) = self.next_cooked_char(mem)? else {
                            if filled == 0 {
                                return Ok(DosAction::WaitForKey);
                            }
                            break;
                        };
                        mem.write_u8(base + filled, ch)?;
                        filled += 1;
                        if !extended {
                            self.stdout.push(ch);
                        }
                        if ch == 0x0d {
                            break;
                        }
                    }
                    regs.ax = filled as u16;
                    regs.cf = false;
                    return Ok(DosAction::Continue);
                }
                // A read from the EMMXXXX0 character device returns end-of-file (0
                // bytes), the way a real EMM driver answers a DOS read; its control
                // traffic goes through INT 67h, not the file handle.
                if self.ems_handles.contains(&handle) {
                    regs.ax = 0;
                    regs.cf = false;
                    return Ok(DosAction::Continue);
                }
                // A loaded character device: route the read to its driver. A
                // read-only check matches the file path; a zero count is a 0-byte
                // success with no driver call, like the handle-0 short-circuit.
                if let Some(dev) = self.device_handles.get(&handle).copied() {
                    if !dev.mode.can_read() {
                        set_dos_error(regs, 0x05); // access denied
                        return Ok(DosAction::Continue);
                    }
                    if regs.cx == 0 {
                        regs.ax = 0;
                        regs.cf = false;
                        return Ok(DosAction::Continue);
                    }
                    return Ok(DosAction::CallDevice {
                        header: dev.header,
                        command: 4,
                        transfer: FarPtr {
                            segment: regs.ds,
                            offset: regs.dx,
                        },
                        count: regs.cx,
                        success_ax: None,
                        rollback_handle_on_error: None,
                    });
                }
                let Some(of) = self.open_files.get(&handle) else {
                    // Predefined STDAUX exists as a character device, but the HLE has
                    // no serial RX buffer yet. Report EOF rather than invalid handle.
                    if handle == 3 {
                        regs.ax = 0;
                        regs.cf = false;
                        return Ok(DosAction::Continue);
                    }
                    set_dos_error(regs, 0x06);
                    return Ok(DosAction::Continue);
                };
                if !of.mode.can_read() {
                    set_dos_error(regs, 0x05);
                    return Ok(DosAction::Continue);
                }
                match of.target.clone() {
                    OutputTarget::Console => {
                        regs.ax = 0;
                        regs.cf = false; // a Console (non-stdin) handle has no input
                        Ok(DosAction::Continue)
                    }
                    OutputTarget::Memory(file) => {
                        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                        let filled = file.borrow_mut().read_to_guest(mem, base, count)?;
                        regs.ax = filled as u16;
                        regs.cf = false;
                        Ok(DosAction::Continue)
                    }
                    OutputTarget::Host(host) => {
                        let mut file = host.borrow_mut();
                        let mut buffer = vec![0u8; count];
                        let mut filled = 0usize;
                        while filled < count {
                            match file.read(&mut buffer[filled..]) {
                                Ok(0) => break,
                                Ok(n) => filled += n,
                                Err(err) => {
                                    set_dos_error(regs, dos_io_error_code(&err));
                                    return Ok(DosAction::Continue);
                                }
                            }
                        }
                        drop(file);
                        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                        for (index, &byte) in buffer[..filled].iter().enumerate() {
                            mem.write_u8(base + index, byte)?;
                        }
                        regs.ax = filled as u16;
                        regs.cf = false;
                        Ok(DosAction::Continue)
                    }
                }
            }
            // AH=3Eh: close the handle in BX. Dropping the File closes it (RAII).
            // CF=0 if it closes cleanly. CF=1 + AX=0x06 for an invalid handle, or a
            // device error if a loaded driver rejects CLOSE.
            0x3e => {
                if self.open_files.remove(&regs.bx).is_some() || self.ems_handles.remove(&regs.bx) {
                    regs.cf = false;
                } else if let Some(dev) = self.device_handles.remove(&regs.bx) {
                    regs.cf = false;
                    if self.device_supports_open_close(mem, dev.header) {
                        return Ok(DosAction::CallDevice {
                            header: dev.header,
                            command: 0x0e,
                            transfer: FarPtr::default(),
                            count: 0,
                            success_ax: Some(regs.ax),
                            rollback_handle_on_error: None,
                        });
                    }
                } else {
                    set_dos_error(regs, 0x06);
                }
                Ok(DosAction::Continue)
            }
            // AH=30h: get the apparent DOS version. AL=major, AH=minor, BH=OEM,
            // BL:CX=serial (0). INT 2Fh AX=122Fh can change the apparent version.
            0x30 => {
                regs.ax = self.reported_version_word();
                regs.bx = u16::from(TOKA_DOS_OEM) << 8;
                regs.cx = 0;
                Ok(DosAction::Continue)
            }
            // AH=19h: get current default drive. AL is 0=A:.
            0x19 => {
                regs.ax = (regs.ax & 0xff00) | u16::from(self.current_drive());
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
            // AH=26h CREATE NEW PSP. DOS copies the current PSP to DX, then
            // refreshes the memory-size word and saved INT 22h/23h/24h vectors.
            0x26 => {
                let top = psp_top_from_mcb_or_current(mem, self.arena.psp_seg, regs.dx)?;
                copy_psp_from_current(mem, self.arena.psp_seg, regs.dx, top, 0)?;
                Ok(DosAction::Continue)
            }
            // AH=35h: get interrupt vector AL into ES:BX.
            0x35 => {
                let addr = usize::from(regs.ax as u8) * 4;
                regs.bx = mem.read_u16(addr)?;
                regs.es = mem.read_u16(addr + 2)?;
                Ok(DosAction::Continue)
            }
            // AH=38h: get country-specific information. Toka-DOS exposes the
            // built-in US/CP437 tables and reports unsupported countries as absent.
            0x38 => {
                let requested = match regs.ax as u8 {
                    0x00 => DOS_COUNTRY_US,
                    0xff => regs.bx,
                    country => u16::from(country),
                };
                if regs.dx == 0xffff {
                    if requested == DOS_COUNTRY_US {
                        regs.ax = DOS_COUNTRY_US;
                        regs.bx = DOS_COUNTRY_US;
                        regs.cf = false;
                    } else {
                        set_dos_error(regs, 0x02);
                    }
                    return Ok(DosAction::Continue);
                }
                if requested == DOS_COUNTRY_US {
                    let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                    write_us_country_info(mem, base)?;
                    regs.ax = DOS_COUNTRY_US;
                    regs.bx = DOS_COUNTRY_US;
                    regs.cf = false;
                } else {
                    set_dos_error(regs, 0x02);
                }
                Ok(DosAction::Continue)
            }
            // AH=2Ah: get date. CX=year, DH=month, DL=day, AL=day-of-week (0=Sun).
            0x2a => {
                regs.cx = self.clock.year;
                regs.dx = (u16::from(self.clock.month) << 8) | u16::from(self.clock.day);
                self.clock.day_of_week =
                    dos_day_of_week(self.clock.year, self.clock.month, self.clock.day);
                regs.ax = (regs.ax & 0xff00) | u16::from(self.clock.day_of_week);
                Ok(DosAction::Continue)
            }
            // AH=2Bh: set date. CX=year(1980-2099), DH=month, DL=day. AL=0 ok, 0xFF
            // invalid. DOS validates month/day combinations and recomputes weekday.
            0x2b => {
                let year = regs.cx;
                let month = (regs.dx >> 8) as u8;
                let day = regs.dx as u8;
                if is_valid_dos_date(year, month, day) {
                    self.clock.year = year;
                    self.clock.month = month;
                    self.clock.day = day;
                    self.clock.day_of_week = dos_day_of_week(year, month, day);
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
            // BX=largest-available. A damaged MCB chain reports AX=0x07.
            0x48 => {
                if !mcb_chain_is_complete(mem, self.arena.first_mcb()) {
                    self.fail(regs, 0x07);
                    return Ok(DosAction::Continue);
                }
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
            // A damaged MCB chain reports AX=0x07.
            0x49 => {
                if !mcb_chain_is_complete(mem, self.arena.first_mcb()) {
                    self.fail(regs, 0x07);
                    return Ok(DosAction::Continue);
                }
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
            // A damaged MCB chain reports AX=0x07.
            0x4a => {
                if !mcb_chain_is_complete(mem, self.arena.first_mcb()) {
                    self.fail(regs, 0x07);
                    return Ok(DosAction::Continue);
                }
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
            // AH=1Bh/1Ch: old allocation-table information calls. They return
            // the same disk geometry as AH=36h, plus DS:BX pointing at the media
            // byte in the published DPB.
            0x1b | 0x1c => {
                let requested_drive = if ah == 0x1b { 0 } else { regs.dx as u8 };
                let Some((dpb_seg, dpb_off)) =
                    self.published_dpb_for_drive(mem, requested_drive)?
                else {
                    regs.ax = (regs.ax & 0xff00) | 0xff;
                    return Ok(DosAction::Continue);
                };
                let dpb = usize::from(dpb_seg) * 16 + usize::from(dpb_off);
                regs.ds = dpb_seg;
                regs.bx = dpb_off.wrapping_add(0x17);
                regs.dx = mem.read_u16(dpb + 0x0d)?;
                regs.cx = mem.read_u16(dpb + 0x02)?;
                regs.ax = (regs.ax & 0xff00) | u16::from(mem.read_u8(dpb + 0x04)?.wrapping_add(1));
                Ok(DosAction::Continue)
            }
            // AH=1Fh/32h: return a pointer to the Drive Parameter Block. AH=1Fh
            // uses the default drive; AH=32h takes DL (0=default, 1=A, ...).
            0x1f | 0x32 => {
                let requested_drive = if ah == 0x1f { 0 } else { regs.dx as u8 };
                let Some((dpb_seg, dpb_off)) =
                    self.published_dpb_for_drive(mem, requested_drive)?
                else {
                    regs.ax = (regs.ax & 0xff00) | 0xff;
                    return Ok(DosAction::Continue);
                };
                regs.ds = dpb_seg;
                regs.bx = dpb_off;
                regs.ax &= 0xff00;
                Ok(DosAction::Continue)
            }
            // AH=2Fh: get the Disk Transfer Area into ES:BX. Default is PSP:0x80.
            0x2f => {
                regs.es = self.dta.0;
                regs.bx = self.dta.1;
                Ok(DosAction::Continue)
            }
            // AH=4Ch: terminate with the return code in AL.
            0x4c => {
                self.restore_psp_saved_vectors(mem)?;
                Ok(DosAction::Exit((regs.ax & 0x00ff) as u8))
            }
            // AH=31h KEEP PROCESS (TSR): terminate with the AL return code but leave
            // the program resident. DX is the requested resident size in paragraphs;
            // DOS 3+ keeps at least six paragraphs. Restore the saved termination,
            // Ctrl-C, and critical-error vectors from the PSP before leaving.
            0x31 => {
                let paras = regs.dx.max(0x0006);
                self.restore_psp_saved_vectors(mem)?;
                self.keep_process_resident(paras, mem)?;
                Ok(DosAction::Exit((regs.ax & 0x00ff) as u8))
            }
            // AH=33h: Ctrl-Break flag and DOS 5+ true-version query. AL=00 gets
            // DL, AL=01 sets it from DL, AL=06 returns BL=major, BH=minor.
            0x33 => {
                match regs.ax as u8 {
                    0x00 => {
                        regs.dx = (regs.dx & 0xff00) | u16::from(self.ctrl_break_enabled);
                    }
                    0x01 => {
                        self.ctrl_break_enabled = regs.dx as u8 != 0;
                    }
                    0x02 => {
                        let old = self.ctrl_break_enabled;
                        self.ctrl_break_enabled = regs.dx as u8 != 0;
                        regs.dx = (regs.dx & 0xff00) | u16::from(old);
                    }
                    0x03 => {
                        regs.dx &= 0xff00;
                    }
                    0x04 => {}
                    0x05 => {
                        regs.dx = (regs.dx & 0xff00) | 3;
                    }
                    0x06 => {
                        regs.bx = TOKA_DOS_VERSION_WORD;
                        regs.dx = 0;
                        regs.ax &= 0xff00;
                    }
                    _ => regs.ax = (regs.ax & 0xff00) | 0xff,
                }
                Ok(DosAction::Continue)
            }
            // AH=34h GET ADDRESS OF INDOS FLAG. The byte lives at SDA+1; the
            // critical-error flag is the preceding byte, matching DOS 3+.
            0x34 => {
                let (seg, sda_off) = self.refresh_sda(mem)?;
                regs.es = seg;
                regs.bx = sda_off.wrapping_add(1);
                regs.cf = false;
                Ok(DosAction::Continue)
            }
            // AH=0Eh: select default drive. AL reports the DOS 3+ potential drive
            // count, at least LASTDRIVE= and at least five drives.
            0x0e => {
                let drive_count = self.logical_drive_count();
                let selected = regs.dx as u8;
                if selected < drive_count {
                    self.current_drive = Some(selected);
                }
                regs.ax = (regs.ax & 0xff00) | u16::from(drive_count);
                Ok(DosAction::Continue)
            }
            // AH=3Ch: create or truncate a file at DS:DX (ASCIIZ). CX = attributes;
            // the read-only bit maps to host permissions, other bits are not modeled.
            // Opens read/write, truncating an existing file to zero. CF=0 + AX=handle,
            // or CF=1 + AX=code.
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
                        if let Err(err) = apply_create_attributes(&path, regs.cx) {
                            set_dos_error(regs, dos_io_error_code_for_path(&err, &path));
                            return Ok(DosAction::Continue);
                        }
                        self.open_files
                            .insert(handle, open_file_record(file, AccessMode::ReadWrite, &path));
                        regs.ax = handle;
                        regs.cf = false;
                    }
                    Err(err) => set_dos_error(regs, dos_io_error_code_for_path(&err, &path)),
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
                    Err(err) => set_dos_error(regs, dos_io_error_code_for_path(&err, &path)),
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
                    Err(err) => set_dos_error(regs, dos_io_error_code_for_path(&err, &path)),
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
                        self.cwd = self.absolute_dos_path(&name).to_ascii_uppercase();
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
                    Err(err) => set_dos_error(regs, dos_io_error_code_for_path(&err, &path)),
                }
                Ok(DosAction::Continue)
            }
            // AH=47h: get the current directory for the drive in DL (0=default,
            // 3=C:) into the 64-byte buffer at DS:SI, with no leading backslash.
            0x47 => {
                if !self.drive_is_mounted_c(regs.dx as u8) {
                    self.fail(regs, 0x0f);
                    return Ok(DosAction::Continue);
                }
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
                    Err(err) => set_dos_error(regs, dos_rename_error_code(&err, &old, &new)),
                }
                Ok(DosAction::Continue)
            }
            // AH=36h: get free disk space for the drive in DL (0=default, 3=C:).
            // Cosmetic but plausible over the host-filesystem C:: 32 KiB clusters
            // on a ~2 GiB volume. AX=sectors/cluster, BX=free clusters,
            // CX=bytes/sector, DX=total clusters; AX=0xFFFF means an invalid drive.
            0x36 => {
                let drive = (regs.dx & 0xff) as u8;
                if !self.drive_is_mounted_c(drive) {
                    regs.ax = 0xffff;
                    return Ok(DosAction::Continue);
                }
                regs.ax = C_DRIVE_SECTORS_PER_CLUSTER; // 64 * 512 = 32 KiB
                regs.cx = C_DRIVE_BYTES_PER_SECTOR;
                regs.dx = C_DRIVE_TOTAL_CLUSTERS;
                regs.bx = C_DRIVE_FREE_CLUSTERS;
                Ok(DosAction::Continue)
            }
            // AH=37h SWITCHAR/AVAILDEV. DOS 4 keeps the switch character mutable,
            // reports devices always available for AL=02h, and makes AL=03h a no-op.
            0x37 => {
                match regs.ax as u8 {
                    0x00 => {
                        let ch = self.switch_char.unwrap_or(b'/');
                        regs.dx = (regs.dx & 0xff00) | u16::from(ch);
                    }
                    0x01 => {
                        self.switch_char = Some(regs.dx as u8);
                    }
                    0x02 => {
                        regs.dx |= 0x00ff;
                    }
                    0x03 => {}
                    _ => regs.ax = (regs.ax & 0xff00) | 0xff,
                }
                Ok(DosAction::Continue)
            }
            // AH=40h: write CX bytes from DS:DX to the handle in BX. CON handles
            // 0/1/2 route to the output buffer. For a file handle, CX=0 truncates
            // the file at the current position. CF=0 + AX=bytes-written, or CF=1
            // + AX=code.
            0x40 => {
                let handle = regs.bx;
                let count = usize::from(regs.cx);
                let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                // A write to the EMMXXXX0 character device is accepted and discarded,
                // reporting every byte written, the way a real EMM driver answers a
                // DOS write (its real traffic is INT 67h, not the file handle).
                if self.ems_handles.contains(&handle) {
                    regs.ax = regs.cx;
                    regs.cf = false;
                    return Ok(DosAction::Continue);
                }
                // A loaded character device: route the write to its driver. A
                // write-access check matches the file path; a zero count is a
                // 0-byte success with no driver call.
                if let Some(dev) = self.device_handles.get(&handle).copied() {
                    if !dev.mode.can_write() {
                        set_dos_error(regs, 0x05); // access denied
                        return Ok(DosAction::Continue);
                    }
                    if regs.cx == 0 {
                        regs.ax = 0;
                        regs.cf = false;
                        return Ok(DosAction::Continue);
                    }
                    return Ok(DosAction::CallDevice {
                        header: dev.header,
                        command: 8,
                        transfer: FarPtr {
                            segment: regs.ds,
                            offset: regs.dx,
                        },
                        count: regs.cx,
                        success_ax: None,
                        rollback_handle_on_error: None,
                    });
                }
                if self
                    .open_files
                    .get(&handle)
                    .is_some_and(|of| matches!(of.target, OutputTarget::Memory(_)))
                {
                    set_dos_error(regs, 0x05);
                    return Ok(DosAction::Continue);
                }
                // A Host file gets the bytes; a Console handle (or an unseeded
                // standard handle 0/1/2 in a bare-kernel test) reaches the screen.
                let host = match self.open_files.get(&handle) {
                    Some(of) if of.host_file().is_some() => {
                        if !of.mode.can_write() {
                            set_dos_error(regs, 0x05);
                            return Ok(DosAction::Continue);
                        }
                        of.host_file().cloned()
                    }
                    Some(_) => None,             // a Console handle
                    None if handle <= 2 => None, // unseeded standard handle -> console
                    // AUX (3, COM1) and PRN (4, LPT1): accept the write and report every
                    // byte written, but discard the data. The HLE has no serial or
                    // printer capture at the INT 21h layer (marked).
                    None if handle == 3 || handle == 4 => {
                        regs.ax = regs.cx;
                        regs.cf = false;
                        return Ok(DosAction::Continue);
                    }
                    None => {
                        set_dos_error(regs, 0x06);
                        return Ok(DosAction::Continue);
                    }
                };
                match host {
                    None => {
                        for index in 0..count {
                            let byte = mem.read_u8(base + index)?;
                            if let Err(code) = self.write_console_handle(handle, byte) {
                                set_dos_error(regs, code);
                                return Ok(DosAction::Continue);
                            }
                        }
                        regs.ax = regs.cx;
                        regs.cf = false;
                        Ok(DosAction::Continue)
                    }
                    Some(file) => {
                        let mut file = file.borrow_mut();
                        if count == 0 {
                            let pos = match file.stream_position() {
                                Ok(pos) => pos,
                                Err(err) => {
                                    set_dos_error(regs, dos_io_error_code(&err));
                                    return Ok(DosAction::Continue);
                                }
                            };
                            if let Err(err) = file.set_len(pos) {
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
                        match file.write_all(&buffer) {
                            Ok(()) => {
                                regs.ax = regs.cx;
                                regs.cf = false;
                            }
                            Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                        }
                        Ok(DosAction::Continue)
                    }
                }
            }
            // AH=42h: seek the handle in BX. AL=whence (0=start, 1=current signed,
            // 2=end signed), CX:DX = 32-bit offset (CX high). CF=0 + DX:AX = new
            // absolute position; AL>2 -> CF=1 + AX=0x01 (invalid function).
            0x42 => {
                let handle = regs.bx;
                let offset = (u32::from(regs.cx) << 16) | u32::from(regs.dx);
                let whence = regs.ax as u8;
                let Some(of) = self.open_files.get(&handle) else {
                    set_dos_error(regs, 0x06);
                    return Ok(DosAction::Continue);
                };
                let pos = match of.target.clone() {
                    OutputTarget::Console => {
                        regs.ax = 0;
                        regs.dx = 0;
                        regs.cf = false; // seeking CON is a no-op
                        return Ok(DosAction::Continue);
                    }
                    OutputTarget::Memory(file) => {
                        let Some(pos) = file.borrow_mut().seek(whence, offset) else {
                            set_dos_error(regs, 0x01);
                            return Ok(DosAction::Continue);
                        };
                        pos
                    }
                    OutputTarget::Host(host) => {
                        let mut file = host.borrow_mut();
                        // Resolve the base the offset applies to. whence 0 takes the offset
                        // from BOF; whence 1 from current; whence 2 from EOF. DOS keeps a
                        // 32-bit unsigned file pointer, so negative results wrap into the
                        // high 4 GiB range rather than failing.
                        let base = match whence {
                            0 => 0i64,
                            1 => match file.stream_position() {
                                Ok(p) => p as i64,
                                Err(err) => {
                                    set_dos_error(regs, dos_io_error_code(&err));
                                    return Ok(DosAction::Continue);
                                }
                            },
                            2 => match file.seek(SeekFrom::End(0)) {
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
                        // A before-start pointer wraps to its 32-bit two's complement.
                        // Seeking beyond EOF is allowed. A later read returns EOF; a
                        // write there extends the host file as DOS would.
                        let pos = (base + signed) as u32;
                        if let Err(err) = file.seek(SeekFrom::Start(u64::from(pos))) {
                            set_dos_error(regs, dos_io_error_code(&err));
                            return Ok(DosAction::Continue);
                        }
                        pos
                    }
                };
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
                    self.fail_find_first(regs, 0x03);
                    return Ok(DosAction::Continue);
                };
                let (dir, pattern) = match self.split_find_spec(&filespec) {
                    Ok(split) => split,
                    Err(code) => {
                        self.fail_find_first(regs, code);
                        return Ok(DosAction::Continue);
                    }
                };
                let mask = regs.cx as u8;
                let pattern_template = pattern_to_8_3(&pattern);
                let read_dir = match std::fs::read_dir(&dir) {
                    Ok(read_dir) => read_dir,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        self.fail_find_first(regs, 0x03);
                        return Ok(DosAction::Continue);
                    }
                    Err(_) => {
                        self.fail_find_first(regs, 0x05);
                        return Ok(DosAction::Continue);
                    }
                };
                let mut entries = Vec::new();
                let root_search = self.drive.as_ref().is_some_and(|drive| dir == drive.root());
                if mask & 0x08 != 0 && root_search {
                    if let Some(label) = self.volume_label {
                        if template_matches(&label, &pattern_template) {
                            entries.push(volume_label_find_entry(label));
                        }
                    }
                }
                if mask != 0x08 {
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
                            raw_name: file_template,
                            name: name.to_ascii_uppercase(),
                        });
                    }
                }
                let Some(first) = entries.first().cloned() else {
                    self.fail_find_first(regs, 0x12);
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
            // AH=4Bh: EXEC. AL=0 loads and executes, AL=1 loads without executing,
            // AL=3 loads an overlay, and AL=5 marks a prepared DOS 5+ execution
            // state. Other subfunctions fail invalid-function.
            0x4b => match regs.ax as u8 {
                0x00 => self.exec_load_and_execute(mem, regs),
                0x01 => self.exec_load_no_execute(mem, regs),
                0x03 => self.exec_load_overlay(mem, regs),
                0x05 => {
                    regs.ax = 0;
                    regs.cf = false;
                    Ok(DosAction::Continue)
                }
                _ => {
                    set_dos_error(regs, 0x01);
                    Ok(DosAction::Continue)
                }
            },
            // AH=4Dh: get the return code of the last child. AL=code, AH=type
            // (0x00 normal, 0x03 terminate-and-stay-resident; Ctrl-C/critical
            // aborts are not modeled, marked).
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
            0x00 => {
                self.restore_psp_saved_vectors(mem)?;
                Ok(DosAction::Exit(0))
            }
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
                let (es, bx) = self.publish_sysvars(mem)?;
                regs.es = es;
                regs.bx = bx;
                Ok(DosAction::Continue)
            }
            // AH=53h: translate a BIOS Parameter Block at DS:SI into the Drive
            // Parameter Block buffer at ES:BP. DOS preserves caller-owned DPB
            // fields outside the BPB-derived geometry.
            0x53 => {
                let bpb_base = usize::from(regs.ds) * 16 + usize::from(regs.si);
                let mut raw = [0u8; BLOCK_BPB_LEN];
                for (index, byte) in raw.iter_mut().enumerate() {
                    *byte = mem.read_u8(bpb_base + index)?;
                }
                let Some(bpb) = BlockDeviceBpb::from_bytes(&raw) else {
                    set_dos_error(regs, 0x01);
                    return Ok(DosAction::Continue);
                };
                let dpb = usize::from(regs.es) * 16 + usize::from(regs.bp);
                mem.write_u16(dpb + 0x02, bpb.bytes_per_sector)?;
                mem.write_u8(dpb + 0x04, bpb.cluster_mask)?;
                mem.write_u8(dpb + 0x05, bpb.cluster_shift)?;
                mem.write_u16(dpb + 0x06, bpb.first_fat_sector)?;
                mem.write_u8(dpb + 0x08, bpb.fat_count)?;
                mem.write_u16(dpb + 0x09, bpb.root_entries)?;
                mem.write_u16(dpb + 0x0b, bpb.first_data_sector)?;
                mem.write_u16(dpb + 0x0d, bpb.highest_cluster)?;
                mem.write_u16(dpb + 0x0f, bpb.sectors_per_fat)?;
                mem.write_u16(dpb + 0x11, bpb.first_root_sector)?;
                mem.write_u8(dpb + 0x17, bpb.media)?;
                mem.write_u16(dpb + 0x1d, 0)?;
                mem.write_u16(dpb + 0x1f, 0xffff)?;
                regs.cf = false;
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
            // AH=55h CREATE CHILD PSP. This shares AH=26h's PSP-copy path, but
            // uses SI as the memory-size word, links the child to the current PSP,
            // and makes the child the current PSP.
            0x55 => {
                let parent = self.arena.psp_seg;
                copy_psp_from_current(mem, parent, regs.dx, regs.si, parent)?;
                self.arena.psp_seg = regs.dx;
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
            // AH=67h SET HANDLE COUNT: resize the modeled per-process JFT limit.
            0x67 => {
                if self.has_live_handle_at_or_above(regs.bx) {
                    self.fail(regs, 0x04);
                    return Ok(DosAction::Continue);
                }
                self.file_count = Some(regs.bx);
                regs.cf = false;
                Ok(DosAction::Continue)
            }
            // AH=68h/6Ah COMMIT FILE (fflush): host writes are unbuffered at the DOS
            // layer, so there is nothing to flush. Succeed for a valid open handle.
            0x68 | 0x6a => {
                if self.open_files.contains_key(&regs.bx) {
                    if ah == 0x6a {
                        regs.ax = (regs.ax & 0x00ff) | 0x6800;
                    }
                    regs.cf = false;
                } else {
                    set_dos_error(regs, 0x06); // invalid handle
                }
                Ok(DosAction::Continue)
            }
            // AH=45h DUP: duplicate the handle in BX onto a new handle. The clone shares
            // the underlying open file and seek position.
            0x45 => {
                let cloned = match self.open_files.get(&regs.bx) {
                    Some(of) => of.clone(),
                    None => {
                        set_dos_error(regs, 0x06); // invalid handle
                        return Ok(DosAction::Continue);
                    }
                };
                let Some(handle) = self.alloc_handle() else {
                    set_dos_error(regs, 0x04); // too many open files
                    return Ok(DosAction::Continue);
                };
                self.open_files.insert(handle, cloned);
                regs.ax = handle;
                regs.cf = false;
                Ok(DosAction::Continue)
            }
            // AH=46h DUP2/FORCEDUP: force handle CX to refer to the same open file as BX,
            // closing whatever CX referred to first.
            0x46 => {
                if regs.cx >= self.file_count() {
                    set_dos_error(regs, 0x04); // too many open files
                    return Ok(DosAction::Continue);
                }
                let cloned = match self.open_files.get(&regs.bx) {
                    Some(of) => of.clone(),
                    None => {
                        set_dos_error(regs, 0x06);
                        return Ok(DosAction::Continue);
                    }
                };
                if regs.cx != regs.bx {
                    self.ems_handles.remove(&regs.cx);
                    self.device_handles.remove(&regs.cx);
                    self.open_files.insert(regs.cx, cloned);
                }
                regs.cf = false;
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
                        Err(err) => set_dos_error(regs, dos_io_error_code_for_path(&err, &path)),
                    },
                    0x01 => match std::fs::metadata(&path) {
                        Ok(meta) => {
                            let mut perms = meta.permissions();
                            perms.set_readonly(regs.cx & 0x01 != 0);
                            match std::fs::set_permissions(&path, perms) {
                                Ok(()) => regs.cf = false,
                                Err(err) => {
                                    set_dos_error(regs, dos_io_error_code_for_path(&err, &path));
                                }
                            }
                        }
                        Err(err) => set_dos_error(regs, dos_io_error_code_for_path(&err, &path)),
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
                        self.open_files
                            .insert(handle, open_file_record(file, AccessMode::ReadWrite, &path));
                        regs.ax = handle;
                        regs.cf = false;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                        set_dos_error(regs, 0x50) // file already exists
                    }
                    Err(err) => set_dos_error(regs, dos_io_error_code_for_path(&err, &path)),
                }
                Ok(DosAction::Continue)
            }
            // AH=5Ch LOCK/UNLOCK FILE ACCESS. The HLE is single-process and has no
            // SHARE table yet, so valid range locks are accepted as no-ops.
            0x5c => {
                match regs.ax as u8 {
                    0x00 | 0x01 => {
                        let handle = regs.bx;
                        let valid = handle <= 4
                            || self.open_files.contains_key(&handle)
                            || self.ems_handles.contains(&handle)
                            || self.device_handles.contains_key(&handle);
                        if valid {
                            regs.cf = false;
                        } else {
                            set_dos_error(regs, 0x06);
                        }
                    }
                    _ => set_dos_error(regs, 0x01),
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
                // A standard handle redirected to a host file is no longer a device.
                let host_redirected = self
                    .open_files
                    .get(&handle)
                    .map(|of| !of.is_console())
                    .unwrap_or(false);
                let device_handle = self.device_handles.get(&handle).copied();
                let is_device_handle = (handle <= 4 && !host_redirected)
                    || self.ems_handles.contains(&handle)
                    || device_handle.is_some();
                let valid = handle <= 4
                    || self.open_files.contains_key(&handle)
                    || self.ems_handles.contains(&handle)
                    || device_handle.is_some();
                let is_character = is_device_handle;
                let valid_drive = self.drive_is_mounted_c((regs.bx & 0x00ff) as u8);
                match regs.ax as u8 {
                    0x00 => {
                        if let Some(dev) = device_handle {
                            // A loaded character device. Report ISDEV (bit 7) plus
                            // the driver's IOCTL-supported capability (driver
                            // attribute bit 14 -> info-word bit 14), the way DOS
                            // reflects the driver's own attribute word.
                            let base = usize::from(dev.header.segment) * 16
                                + usize::from(dev.header.offset);
                            let attr = mem.read_u16(base + 4)?;
                            let mut info = 0x0080u16; // ISDEV
                            if attr & 0x4000 != 0 {
                                info |= 0x4000; // supports IOCTL
                            }
                            regs.dx = info;
                            regs.cf = false;
                        } else if is_device_handle {
                            // A character device. Bits 0/1 identify the STDIN/STDOUT
                            // aliases (not AUX/PRN capabilities), so handles 3 and 4
                            // keep only ISDEV set.
                            let io = match handle {
                                0 => 0x01,
                                1 | 2 => 0x02,
                                _ => 0x00,
                            };
                            if self.ems_handles.contains(&handle) {
                                // The EMMXXXX0 device: bit 7 ISDEV plus the
                                // IOCTL-supported bit, the way an EMM driver answers
                                // the open-then-IOCTL detection.
                                regs.dx = 0xc080;
                            } else {
                                regs.dx = 0x80 | io; // bit 7 ISDEV + standard alias bits
                                if builtin_char_ioctl_category(handle).is_some() {
                                    regs.dx |= DOS_DEV_ATTR_DEV320;
                                }
                            }
                            regs.cf = false;
                        } else if self.open_files.contains_key(&handle) {
                            // A regular file (or a redirected standard handle); bit 7
                            // clear means a file.
                            regs.dx = 0x0002;
                            regs.cf = false;
                        } else {
                            set_dos_error(regs, 0x06); // invalid handle
                        }
                    }
                    0x01 => {
                        // Set device info applies only to character devices. The HLE keeps
                        // the device attributes fixed, but validates the target like DOS.
                        if !valid {
                            set_dos_error(regs, 0x06);
                        } else if !is_character {
                            set_dos_error(regs, 0x05);
                        } else {
                            regs.cf = false;
                        }
                    }
                    0x02 | 0x03 => {
                        // Character-device control channels. The built-in console and EMS
                        // facades have no private control bytes, so a valid character device
                        // transfers zero bytes.
                        if !valid {
                            set_dos_error(regs, 0x06);
                        } else if !is_character {
                            set_dos_error(regs, 0x05);
                        } else {
                            regs.ax = 0;
                            regs.cf = false;
                        }
                    }
                    0x04 | 0x05 => {
                        // Block-device control channel for the single mounted fixed C: drive.
                        if valid_drive {
                            regs.ax = 0;
                            regs.cf = false;
                        } else {
                            set_dos_error(regs, 0x0f);
                        }
                    }
                    0x06 => {
                        // Get input status: AL=0xFF ready, 0x00 not. Console input (handle 0)
                        // is ready only when a key waits; disk files are ready until EOF;
                        // output devices are always ready.
                        if valid {
                            let ready = if handle == 0
                                && self
                                    .open_files
                                    .get(&0)
                                    .map(|of| of.is_console())
                                    .unwrap_or(true)
                            {
                                !kbd_ring_is_empty(mem)?
                            } else if let Some(host) = self
                                .open_files
                                .get(&handle)
                                .and_then(|of| of.host_file())
                                .cloned()
                            {
                                let mut file = host.borrow_mut();
                                match file.stream_position().and_then(|pos| {
                                    file.metadata().map(|metadata| pos < metadata.len())
                                }) {
                                    Ok(ready) => ready,
                                    Err(err) => {
                                        set_dos_error(regs, dos_io_error_code(&err));
                                        return Ok(DosAction::Continue);
                                    }
                                }
                            } else if let Some(memory) = self
                                .open_files
                                .get(&handle)
                                .and_then(|of| of.memory_file())
                                .cloned()
                            {
                                let file = memory.borrow();
                                file.position < file.size_u32()
                            } else {
                                true
                            };
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
                        if valid_drive {
                            regs.ax = 1;
                            regs.cf = false;
                        } else {
                            set_dos_error(regs, 0x0f);
                        }
                    }
                    0x09 => {
                        // Is drive remote? DX bit 12 clear: C: is local.
                        if valid_drive {
                            regs.dx = 0;
                            regs.cf = false;
                        } else {
                            set_dos_error(regs, 0x0f);
                        }
                    }
                    0x0a => {
                        // Is handle remote? DX bit 15 clear: local.
                        if valid {
                            regs.dx = 0;
                            regs.cf = false;
                        } else {
                            set_dos_error(regs, 0x06);
                        }
                    }
                    0x0b => {
                        // Set sharing retry count. No sharing subsystem exists yet, so this
                        // accepted DOS 3.1+ knob is a no-op success.
                        regs.cf = false;
                    }
                    0x0c => {
                        // Generic character-device IOCTL. Built-in CON and PRN expose
                        // the CP437 code-page calls; other functions still require a
                        // real character-device driver path.
                        let category = (regs.cx >> 8) as u8;
                        let function = regs.cx as u8;
                        if !valid {
                            set_dos_error(regs, 0x06);
                        } else if !is_character {
                            set_dos_error(regs, 0x05);
                        } else if !builtin_char_ioctl_supported(handle, category, function) {
                            set_dos_error(regs, 0x01);
                        } else {
                            let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                            match function {
                                0x4a | 0x4d => {
                                    if mem.read_u16(base + 2)? == DOS_CODE_PAGE_US {
                                        regs.ax = 0;
                                        regs.cf = false;
                                    } else {
                                        set_dos_error(regs, 0x02);
                                    }
                                }
                                0x4c => {
                                    if code_page_prepare_list_contains_active(mem, base)? {
                                        regs.ax = 0;
                                        regs.cf = false;
                                    } else {
                                        set_dos_error(regs, 0x02);
                                    }
                                }
                                0x6a => {
                                    write_selected_code_page_packet(mem, base)?;
                                    regs.ax = 0;
                                    regs.cf = false;
                                }
                                0x6b => {
                                    write_code_page_prepare_list_packet(mem, base)?;
                                    regs.ax = 0;
                                    regs.cf = false;
                                }
                                _ => unreachable!(),
                            }
                        }
                    }
                    0x0d => {
                        // Generic block-device IOCTL. DOS 4 uses category 08h minor
                        // 46h/66h as the lower-level path for AH=69h media ID.
                        if !valid_drive {
                            set_dos_error(regs, 0x0f);
                        } else {
                            let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                            match regs.cx {
                                0x0847 => {
                                    if self.read_access_flag_packet(mem, base)? {
                                        regs.ax = 0;
                                        regs.cf = false;
                                    } else {
                                        set_dos_error(regs, 0x01);
                                    }
                                }
                                0x0866 => {
                                    self.write_media_id_packet(mem, base)?;
                                    regs.ax = 0;
                                    regs.cf = false;
                                }
                                0x0867 => {
                                    self.write_access_flag_packet(mem, base)?;
                                    regs.ax = 0;
                                    regs.cf = false;
                                }
                                0x0846 => {
                                    if self.read_media_id_packet(mem, base)? {
                                        regs.ax = 0;
                                        regs.cf = false;
                                    } else {
                                        set_dos_error(regs, 0x01);
                                    }
                                }
                                _ => set_dos_error(regs, 0x01),
                            }
                        }
                    }
                    0x0e => {
                        // Logical drive map: the C: block device has one logical drive.
                        if valid_drive {
                            regs.ax &= 0xff00;
                            regs.cf = false;
                        } else {
                            set_dos_error(regs, 0x0f);
                        }
                    }
                    0x0f => {
                        // Set logical drive map. With one fixed C: drive there is nothing to
                        // remap, but a request for C: is harmless and succeeds.
                        if valid_drive {
                            regs.cf = false;
                        } else {
                            set_dos_error(regs, 0x0f);
                        }
                    }
                    0x10 => {
                        // Generic IOCTL capability by handle.
                        let category = (regs.cx >> 8) as u8;
                        let function = regs.cx as u8;
                        if !valid {
                            set_dos_error(regs, 0x06);
                        } else if builtin_char_ioctl_supported(handle, category, function) {
                            regs.ax = 0;
                            regs.cf = false;
                        } else {
                            set_dos_error(regs, 0x01);
                        }
                    }
                    0x11 => {
                        // Generic IOCTL capability by drive.
                        if !valid_drive {
                            set_dos_error(regs, 0x0f);
                        } else if matches!(regs.cx, 0x0846 | 0x0847 | 0x0866 | 0x0867) {
                            regs.ax = 0;
                            regs.cf = false;
                        } else {
                            set_dos_error(regs, 0x01);
                        }
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
                let Some(host) = of.host_file().cloned() else {
                    set_dos_error(regs, 0x06); // a character device has no file date
                    return Ok(DosAction::Continue);
                };
                match regs.ax as u8 {
                    0x00 => match host.borrow().metadata().and_then(|m| m.modified()) {
                        Ok(modified) => {
                            let (time, date) = dos_time_date(modified);
                            regs.cx = time;
                            regs.dx = date;
                            regs.cf = false;
                        }
                        Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                    },
                    0x01 => match host
                        .borrow()
                        .set_modified(systemtime_from_dos(regs.cx, regs.dx))
                    {
                        Ok(()) => regs.cf = false,
                        Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                    },
                    0x02 | 0x03 => {
                        let buffer_len = regs.cx;
                        regs.cx = 2;
                        if buffer_len >= 2 {
                            let base = usize::from(regs.es) * 16 + usize::from(regs.di);
                            mem.write_u16(base, 0)?;
                        }
                        regs.cf = false;
                    }
                    0x04 => {
                        regs.cf = false;
                    }
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
                regs.ax = self.extended_error.code;
                regs.bx = self.extended_error.bx;
                regs.cx = (regs.cx & 0x00ff) | (u16::from(self.extended_error.ch) << 8);
                regs.es = self.extended_error.pointer.segment;
                regs.di = self.extended_error.pointer.offset;
                regs.cf = false; // the query itself succeeds; do not overwrite the saved state
                Ok(DosAction::Continue)
            }
            // AH=5Dh internal server functions. AX=5D00h runs an INT 21h call from
            // a DOS parameter list. AX=5D06h returns the SDA, and AX=5D0Ah stores
            // the extended-error record that AH=59h returns.
            0x5d => match regs.ax as u8 {
                0x00 => {
                    let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                    let mut nested = DosRegs {
                        ax: mem.read_u16(base)?,
                        bx: mem.read_u16(base + 2)?,
                        cx: mem.read_u16(base + 4)?,
                        dx: mem.read_u16(base + 6)?,
                        si: mem.read_u16(base + 8)?,
                        di: mem.read_u16(base + 10)?,
                        bp: regs.bp,
                        ds: mem.read_u16(base + 12)?,
                        es: mem.read_u16(base + 14)?,
                        ..DosRegs::default()
                    };
                    if nested.ax == 0x5d00 {
                        set_dos_error(&mut nested, 0x01);
                        *regs = nested;
                        return Ok(DosAction::Continue);
                    }
                    let action = self.dispatch_int21(&mut nested, mem)?;
                    *regs = nested;
                    Ok(action)
                }
                0x01 => {
                    regs.cf = false;
                    Ok(DosAction::Continue)
                }
                0x02..=0x04 => {
                    self.fail(regs, 0x01);
                    Ok(DosAction::Continue)
                }
                0x05 => {
                    self.fail(regs, 0x12);
                    Ok(DosAction::Continue)
                }
                0x06 => {
                    let (seg, sda_off) = self.refresh_sda(mem)?;
                    regs.ds = seg;
                    regs.si = sda_off;
                    regs.cx = SDA_IN_DOS_SWAPPED_LEN;
                    regs.dx = SDA_ALWAYS_SWAPPED_LEN;
                    regs.cf = false;
                    Ok(DosAction::Continue)
                }
                0x0b => {
                    let (seg, list_off) = self.refresh_sda_list(mem)?;
                    regs.ds = seg;
                    regs.si = list_off;
                    regs.cf = false;
                    Ok(DosAction::Continue)
                }
                0x07..=0x09 => {
                    self.fail(regs, 0x01);
                    Ok(DosAction::Continue)
                }
                0x0a => {
                    let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                    self.extended_error = ExtendedError {
                        code: mem.read_u16(base)?,
                        bx: mem.read_u16(base + 2)?,
                        ch: (mem.read_u16(base + 4)? >> 8) as u8,
                        pointer: FarPtr {
                            offset: mem.read_u16(base + 10)?,
                            segment: mem.read_u16(base + 14)?,
                        },
                    };
                    regs.cf = false;
                    Ok(DosAction::Continue)
                }
                _ => {
                    self.fail(regs, 0x01);
                    Ok(DosAction::Continue)
                }
            },
            // AH=5Eh/5Fh Microsoft Networks services. Toka carries the local
            // machine-name calls; redirector and printer setup calls still fail
            // without a network redirector.
            0x5e => {
                match regs.ax as u8 {
                    0x00 => {
                        if let Some((name, number)) = &self.machine_name {
                            write_machine_name(mem, regs.ds, regs.dx, name)?;
                            regs.cx = (0x01 << 8) | u16::from(*number);
                        } else {
                            regs.cx &= 0x00ff; // CH=0: machine name is not valid.
                        }
                        regs.cf = false;
                    }
                    0x01 => {
                        if regs.cx & 0xff00 == 0 {
                            self.machine_name = None;
                        } else {
                            self.machine_name =
                                Some((read_machine_name(mem, regs.ds, regs.dx)?, regs.cx as u8));
                        }
                        regs.cf = false;
                    }
                    _ => self.fail(regs, 0x01),
                }
                Ok(DosAction::Continue)
            }
            0x5f => {
                match regs.ax as u8 {
                    0x07 | 0x08 => {
                        let drive = regs.dx as u8;
                        if drive < self.lastdrive.unwrap_or(DEFAULT_LASTDRIVE_COUNT) {
                            regs.cf = false;
                        } else {
                            self.fail(regs, 0x0f);
                        }
                    }
                    _ => self.fail(regs, 0x01),
                }
                Ok(DosAction::Continue)
            }
            // AH=5Ah CREATE TEMPORARY FILE: DS:DX points at an ASCIIZ directory path
            // plus 13 zero bytes. Generate a DOS 6-style 8-letter name, append it
            // (with its NUL) so the caller can read back the full path, then create
            // it create-exclusive.
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
                let prefix = temp_file_prefix(&dir);
                let seed = temp_file_seed();
                // Try a sequence of names until one does not yet exist. The host
                // create-exclusive open is the real guard; this loop just picks a
                // free candidate near the time-derived seed.
                let mut created = None;
                for offset in 0u32..=0xffff {
                    let generated = temp_file_name(seed.wrapping_add(offset));
                    let candidate = format!("{prefix}{generated}");
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
                            created = Some((file, candidate, path));
                            break;
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                        Err(err) => {
                            self.fail(regs, dos_io_error_code_for_path(&err, &path));
                            return Ok(DosAction::Continue);
                        }
                    }
                }
                let Some((file, name, path)) = created else {
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
                self.open_files
                    .insert(handle, open_file_record(file, AccessMode::ReadWrite, &path));
                regs.ax = handle;
                regs.cf = false;
                Ok(DosAction::Continue)
            }
            // AH=6Ch EXTENDED OPEN/CREATE: a superset of AH=3Dh open and AH=3Ch
            // create. BX = access/share mode (low 3 bits are the access mode), CX =
            // attributes for a created or replaced file, DX = action flags
            // (bit 0 open-if-exists, bit 1 replace/truncate-if-exists, bit 4
            // create-if-not-exists), DS:SI = ASCIIZ filename. On success CF=0,
            // AX=handle, CX=action taken (1 opened, 2 created, 3 truncated). On
            // failure CF=1 with the DOS code.
            0x6c => {
                if regs.ax as u8 != 0 {
                    self.fail(regs, 0x01);
                    return Ok(DosAction::Continue);
                }
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
                        if action_taken != 1 {
                            if let Err(err) = apply_create_attributes(&path, regs.cx) {
                                self.fail(regs, dos_io_error_code_for_path(&err, &path));
                                return Ok(DosAction::Continue);
                            }
                        }
                        self.open_files
                            .insert(handle, open_file_record(file, mode, &path));
                        regs.ax = handle;
                        regs.cx = action_taken;
                        regs.cf = false;
                    }
                    Err(err) => self.fail(regs, dos_io_error_code_for_path(&err, &path)),
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
                if !self.name_targets_mounted_c(&name) {
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
            // AH=61h UNUSED: documented to return AL=0.
            0x61 => {
                regs.ax &= 0xff00;
                Ok(DosAction::Continue)
            }
            // AH=63h DBCS lead-byte/interim-console services. CP437 has no DBCS
            // lead ranges, so the table is just the 00h,00h terminator.
            0x63 => {
                match regs.ax as u8 {
                    0x00 => {
                        let (seg, off) = DBCS_LEAD_BYTE_TABLE_PTR;
                        mem.write_u16(usize::from(seg) * 16 + usize::from(off), 0)?;
                        regs.ds = seg;
                        regs.si = off;
                        regs.cf = false;
                    }
                    0x01 => {
                        self.interim_console_flag = regs.dx as u8 & 1 != 0;
                        regs.cf = false;
                    }
                    0x02 => {
                        regs.dx = (regs.dx & 0xff00) | u16::from(self.interim_console_flag);
                        regs.cf = false;
                    }
                    _ => set_dos_error(regs, 0x01),
                }
                Ok(DosAction::Continue)
            }
            // AH=64h SET DEVICE DRIVER LOOKAHEAD FLAG. The HLE console does not call
            // character driver function 5, so the flag has no effect; MS-DOS returns
            // no value and treats the call as successful.
            0x64 => {
                regs.cf = false;
                Ok(DosAction::Continue)
            }
            // AH=65h: country/NLS services. The HLE provides a US/CP437 default
            // table and ASCII capitalization, enough for DOS tools that probe NLS.
            0x65 => {
                match regs.ax as u8 {
                    0x01 => {
                        let code_page_ok = regs.bx == 0xffff || regs.bx == DOS_CODE_PAGE_US;
                        let country_ok = regs.dx == 0xffff || regs.dx == DOS_COUNTRY_US;
                        if !code_page_ok || !country_ok || regs.cx < DOS_EXT_COUNTRY_INFO_LEN as u16
                        {
                            set_dos_error(regs, 0x02);
                        } else {
                            let base = usize::from(regs.es) * 16 + usize::from(regs.di);
                            mem.write_u8(base, 0x01)?;
                            mem.write_u16(base + 1, 38)?;
                            mem.write_u16(base + 3, DOS_COUNTRY_US)?;
                            mem.write_u16(base + 5, DOS_CODE_PAGE_US)?;
                            write_us_country_info(mem, base + 7)?;
                            regs.cx = DOS_EXT_COUNTRY_INFO_LEN as u16;
                            regs.cf = false;
                        }
                    }
                    0x02..=0x06 => {
                        let code_page_ok = regs.bx == 0xffff || regs.bx == DOS_CODE_PAGE_US;
                        let country_ok = regs.dx == 0xffff || regs.dx == DOS_COUNTRY_US;
                        if !code_page_ok || !country_ok || regs.cx < 5 {
                            set_dos_error(regs, 0x02);
                        } else {
                            let tables = self.publish_nls_tables(mem)?;
                            let ptr = match regs.ax as u8 {
                                0x02 => tables.uppercase,
                                0x03 => tables.lowercase,
                                0x04 => tables.filename_uppercase,
                                0x05 => tables.filename_terminators,
                                0x06 => tables.collating,
                                _ => unreachable!(),
                            };
                            let base = usize::from(regs.es) * 16 + usize::from(regs.di);
                            mem.write_u8(base, regs.ax as u8)?;
                            mem.write_u16(base + 1, ptr.1)?;
                            mem.write_u16(base + 3, ptr.0)?;
                            regs.cx = 5;
                            regs.cf = false;
                        }
                    }
                    0x07 => {
                        let code_page_ok = regs.bx == 0xffff || regs.bx == DOS_CODE_PAGE_US;
                        let country_ok = regs.dx == 0xffff || regs.dx == DOS_COUNTRY_US;
                        if !code_page_ok || !country_ok || regs.cx < 5 {
                            set_dos_error(regs, 0x02);
                        } else {
                            let base = usize::from(regs.es) * 16 + usize::from(regs.di);
                            let (seg, off) = DBCS_LEAD_BYTE_TABLE_PTR;
                            mem.write_u16(usize::from(seg) * 16 + usize::from(off), 0)?;
                            mem.write_u8(base, 0x07)?;
                            mem.write_u16(base + 1, off)?;
                            mem.write_u16(base + 3, seg)?;
                            regs.cx = 5;
                            regs.cf = false;
                        }
                    }
                    0x20 | 0xa0 => {
                        regs.dx = (regs.dx & 0xff00) | u16::from(nls_upper(regs.dx as u8));
                        regs.cf = false;
                    }
                    0x21 | 0xa1 => {
                        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                        for i in 0..usize::from(regs.cx) {
                            let byte = mem.read_u8(base + i)?;
                            mem.write_u8(base + i, nls_upper(byte))?;
                        }
                        regs.cf = false;
                    }
                    0x22 | 0xa2 => {
                        let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                        for i in 0..=0xffffusize {
                            let byte = mem.read_u8(base + i)?;
                            if byte == 0 {
                                break;
                            }
                            mem.write_u8(base + i, nls_upper(byte))?;
                        }
                        regs.cf = false;
                    }
                    0x23 => {
                        regs.ax = match regs.dx as u8 {
                            b'N' | b'n' => 0,
                            b'Y' | b'y' => 1,
                            _ => 2,
                        };
                        regs.cf = false;
                    }
                    _ => set_dos_error(regs, 0x01),
                }
                Ok(DosAction::Continue)
            }
            // AX=6601h/6602h: code-page table. Only CP437 is present.
            0x66 => {
                match regs.ax as u8 {
                    0x01 => {
                        regs.bx = DOS_CODE_PAGE_US;
                        regs.dx = DOS_CODE_PAGE_US;
                        regs.cf = false;
                    }
                    0x02 if regs.bx == DOS_CODE_PAGE_US && regs.dx == DOS_CODE_PAGE_US => {
                        regs.cf = false;
                    }
                    0x02 => set_dos_error(regs, 0x02),
                    _ => set_dos_error(regs, 0x01),
                }
                Ok(DosAction::Continue)
            }
            // AH=69h DOS 4+ GET/SET DISK SERIAL NUMBER. The host C: facade exposes
            // a FAT16-style extended BPB with serial, label, and filesystem type.
            0x69 => {
                let subfunction = regs.ax as u8;
                let drive = regs.bx as u8;
                let info_level = (regs.bx >> 8) as u8;
                if info_level != 0 {
                    self.fail(regs, 0x01);
                    return Ok(DosAction::Continue);
                }
                if !self.valid_media_id_drive(drive) {
                    self.fail(regs, 0x0f);
                    return Ok(DosAction::Continue);
                }
                let base = usize::from(regs.ds) * 16 + usize::from(regs.dx);
                match subfunction {
                    0x00 => {
                        self.write_media_id_packet(mem, base)?;
                        regs.ax = 0;
                        regs.cf = false;
                    }
                    0x01 => {
                        if !self.read_media_id_packet(mem, base)? {
                            self.fail(regs, 0x01);
                            return Ok(DosAction::Continue);
                        }
                        regs.ax = 0;
                        regs.cf = false;
                    }
                    _ => self.fail(regs, 0x01),
                }
                Ok(DosAction::Continue)
            }
            // AH=6Bh is a DOS 5+ null call. AH=6Dh/6Eh/6Fh are ROM-search calls;
            // on normal non-ROM MS-DOS they report unsupported by returning AL=0.
            0x6b | 0x6d | 0x6e | 0x6f => {
                regs.ax &= 0xff00;
                Ok(DosAction::Continue)
            }
            // AH=70h/71h are MS-DOS 7 internationalization/LFN families. Toka-DOS
            // presents as DOS 6.22, so callers get the documented fallback errors.
            0x70 => {
                self.fail(regs, 0x7000);
                Ok(DosAction::Continue)
            }
            0x71 => {
                self.fail(regs, 0x7100);
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
            // Unknown INT 21h functions fail with DOS error 1. The old CP/M null
            // calls and DOS 5+ null call are handled explicitly above.
            _ => {
                self.fail(regs, 0x01);
                Ok(DosAction::Continue)
            }
        }
    }
}

/// Toka-DOS (Toka Disk Operating System), the Izarra 3000's MS-DOS 6.22 clone,
/// is what this HLE kernel emulates. INT 21h AH=30h reports its version.
const TOKA_DOS_VERSION_MAJOR: u8 = 6;
const TOKA_DOS_VERSION_MINOR: u8 = 22; // 6.22, the .NN-hundredths convention (6.20 -> 20)
const TOKA_DOS_VERSION_WORD: u16 =
    (TOKA_DOS_VERSION_MAJOR as u16) | ((TOKA_DOS_VERSION_MINOR as u16) << 8);
const TOKA_DOS_OEM: u8 = 0xff;
const DOS_COUNTRY_US: u16 = 1;
const DOS_CODE_PAGE_US: u16 = 437;
const DOS_COUNTRY_INFO_LEN: usize = 34;
const DOS_EXT_COUNTRY_INFO_LEN: usize = 41;
const DOS_MACHINE_NAME_LEN: usize = 15;
const DOS_DEV_ATTR_DEV320: u16 = 0x0040;
const DOS_INT27_MIN_RESIDENT_BYTES: u16 = 0x0060;

fn write_us_country_info(mem: &mut Memory, base: usize) -> Result<(), DosError> {
    for offset in 0..DOS_COUNTRY_INFO_LEN {
        mem.write_u8(base + offset, 0)?;
    }
    mem.write_u16(base, 0)?; // USA date format, mm/dd/yy.
    mem.write_u8(base + 0x02, b'$')?; // currency symbol, ASCIZ.
    mem.write_u8(base + 0x07, b',')?; // thousands separator, ASCIZ.
    mem.write_u8(base + 0x09, b'.')?; // decimal separator, ASCIZ.
    mem.write_u8(base + 0x0b, b'/')?; // date separator, ASCIZ.
    mem.write_u8(base + 0x0d, b':')?; // time separator, ASCIZ.
    mem.write_u8(base + 0x10, 2)?; // currency decimal places.
    mem.write_u8(base + 0x16, b',')?; // data-list separator, ASCIZ.
    Ok(())
}

fn nls_upper(byte: u8) -> u8 {
    byte.to_ascii_uppercase()
}

fn builtin_char_ioctl_category(handle: u16) -> Option<u8> {
    match handle {
        0..=2 => Some(0x03), // CON
        4 => Some(0x05),     // PRN/LPT
        _ => None,
    }
}

fn builtin_char_ioctl_supported(handle: u16, category: u8, function: u8) -> bool {
    builtin_char_ioctl_category(handle) == Some(category)
        && matches!(function, 0x4a | 0x4c | 0x4d | 0x6a | 0x6b)
}

fn write_selected_code_page_packet(mem: &mut Memory, base: usize) -> Result<(), DosError> {
    mem.write_u16(base, 4)?;
    mem.write_u16(base + 2, DOS_CODE_PAGE_US)?;
    mem.write_u16(base + 4, 0)?;
    Ok(())
}

fn write_code_page_prepare_list_packet(mem: &mut Memory, base: usize) -> Result<(), DosError> {
    mem.write_u16(base, 8)?;
    mem.write_u16(base + 2, 1)?;
    mem.write_u16(base + 4, DOS_CODE_PAGE_US)?;
    mem.write_u16(base + 6, 1)?;
    mem.write_u16(base + 8, DOS_CODE_PAGE_US)?;
    Ok(())
}

fn code_page_prepare_list_contains_active(mem: &Memory, base: usize) -> Result<bool, DosError> {
    let count = usize::from(mem.read_u16(base + 4)?);
    for index in 0..count.min(64) {
        if mem.read_u16(base + 6 + index * 2)? == DOS_CODE_PAGE_US {
            return Ok(true);
        }
    }
    Ok(false)
}

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
/// RET to PSP:0 terminates; the top-of-memory paragraph at 0x02; the CP/M CALL
/// 5 far call at 0x05; the INT 21h/RETF helper at 0x50; an empty command tail at
/// 0x80. The documented vectors at 0x0A/0x0E/0x12 snapshot the current INT
/// 22h/23h/24h IVT entries; 0x16 (parent PSP) defaults to 0 and the EXEC path
/// overwrites it for a child; 0x32/0x34 hold the JFT count and far pointer, with
/// the 20-byte JFT at 0x18 wiring stdin/stdout/stderr to CON, handle 3 to AUX,
/// handle 4 to PRN, and the rest closed. The environment segment (0x2C) is
/// filled in by `DosKernel::install_environment`.
fn build_psp(mem: &mut Memory, psp_seg: u16, top_of_mem_paragraph: u16) -> Result<(), DosError> {
    let base = usize::from(psp_seg) * 16;
    mem.write_u8(base, 0xcd)?;
    mem.write_u8(base + 1, 0x20)?;
    mem.write_u16(base + 2, top_of_mem_paragraph)?;
    // PSP:0x05 is the ancient CP/M-compatible CALL 5 entry. MS-DOS points it at
    // the low-memory entry whose bytes overlap the nominal INT 30h/31h vectors.
    mem.write_u8(base + 0x05, 0x9a)?; // call far 0000:00C0
    mem.write_u16(base + 0x06, 0x00c0)?;
    mem.write_u16(base + 0x08, 0x0000)?;
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
    mem.write_u8(base + 0x18, 0x01)?; // stdin -> CON
    mem.write_u8(base + 0x19, 0x01)?; // stdout -> CON
    mem.write_u8(base + 0x1a, 0x01)?; // stderr -> CON
    mem.write_u8(base + 0x1b, 0x03)?; // stdaux -> AUX
    mem.write_u8(base + 0x1c, 0x04)?; // stdprn -> PRN
    // PSP:0x32 JFT entry count, PSP:0x34 far pointer to the JFT (PSP:0x18).
    mem.write_u16(base + 0x32, JFT_LEN as u16)?;
    mem.write_u16(base + 0x34, 0x0018)?;
    mem.write_u16(base + 0x36, psp_seg)?;
    // PSP:0x50 is the documented portable DOS-call helper: INT 21h then RETF.
    mem.write_u8(base + 0x50, 0xcd)?;
    mem.write_u8(base + 0x51, 0x21)?;
    mem.write_u8(base + 0x52, 0xcb)?;
    mem.write_u8(base + 0x53, 0)?;
    mem.write_u8(base + 0x54, 0)?;
    mem.write_u8(base + 0x80, 0x00)?;
    mem.write_u8(base + 0x81, 0x0d)?;
    Ok(())
}

fn psp_top_from_mcb_or_current(
    mem: &Memory,
    current_psp: u16,
    target_psp: u16,
) -> Result<u16, DosError> {
    if target_psp != 0 {
        let mcb = usize::from(target_psp.wrapping_sub(1)) * 16;
        let sig = mem.read_u8(mcb)?;
        if matches!(sig, b'M' | b'Z') {
            return Ok(target_psp.wrapping_add(mem.read_u16(mcb + 3)?));
        }
    }
    Ok(mem.read_u16(usize::from(current_psp) * 16 + 0x02)?)
}

fn copy_psp_from_current(
    mem: &mut Memory,
    current_psp: u16,
    target_psp: u16,
    top_of_mem_paragraph: u16,
    parent_psp: u16,
) -> Result<(), DosError> {
    let source = usize::from(current_psp) * 16;
    let target = usize::from(target_psp) * 16;
    let mut bytes = [0u8; 0x100];
    for (offset, byte) in bytes.iter_mut().enumerate() {
        *byte = mem.read_u8(source + offset)?;
    }
    for (offset, &byte) in bytes.iter().enumerate() {
        mem.write_u8(target + offset, byte)?;
    }
    mem.write_u16(target + 0x02, top_of_mem_paragraph)?;
    for (psp_off, int_no) in [(0x0au16, 0x22u8), (0x0e, 0x23), (0x12, 0x24)] {
        let ivt = usize::from(int_no) * 4;
        mem.write_u16(target + usize::from(psp_off), mem.read_u16(ivt)?)?;
        mem.write_u16(target + usize::from(psp_off) + 2, mem.read_u16(ivt + 2)?)?;
    }
    mem.write_u16(target + 0x16, parent_psp)?;
    if mem.read_u16(target + 0x36)? == current_psp {
        mem.write_u16(target + 0x36, target_psp)?;
    }
    Ok(())
}

/// The default Job File Table length DOS reports in PSP:0x32 (20 handles).
const JFT_LEN: usize = 20;

fn interrupt_vector(mem: &Memory, int_no: u8) -> Result<FarPtr, DosError> {
    let ivt = usize::from(int_no) * 4;
    Ok(FarPtr {
        offset: mem.read_u16(ivt)?,
        segment: mem.read_u16(ivt + 2)?,
    })
}

fn critical_error_to_extended_error(code: u8) -> u16 {
    match code {
        0x00..=0x0c => 0x13 + u16::from(code),
        0x0d..=0x11 => 0x20 + u16::from(code - 0x0d),
        _ => u16::from(code),
    }
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

/// Map a host file I/O error to a DOS error code. Callers with the target path
/// should prefer `dos_io_error_code_for_path`, which can split file-not-found
/// from path-not-found.
fn dos_io_error_code(err: &std::io::Error) -> u16 {
    if is_too_many_open_files_error(err) {
        return 0x04;
    }
    match err.kind() {
        std::io::ErrorKind::NotFound => 0x02,
        std::io::ErrorKind::InvalidInput => 0x0c,
        _ => 0x05,
    }
}

fn dos_io_error_code_for_path(err: &std::io::Error, path: &Path) -> u16 {
    if err.kind() == std::io::ErrorKind::NotFound && path_parent_is_missing(path) {
        0x03
    } else {
        dos_io_error_code(err)
    }
}

fn dos_rename_error_code(err: &std::io::Error, old: &Path, new: &Path) -> u16 {
    if path_parent_is_missing(new) {
        0x03
    } else {
        dos_io_error_code_for_path(err, old)
    }
}

fn path_parent_is_missing(path: &Path) -> bool {
    path.parent().is_some_and(|parent| !parent.exists())
}

fn is_too_many_open_files_error(err: &std::io::Error) -> bool {
    match err.raw_os_error() {
        #[cfg(windows)]
        Some(4) => true,
        #[cfg(unix)]
        Some(23 | 24) => true,
        _ => false,
    }
}

fn temp_file_prefix(dir: &str) -> String {
    if dir.is_empty() {
        r"C:\".to_string()
    } else if dir.ends_with('\\') || dir.ends_with('/') {
        dir.to_string()
    } else {
        format!(r"{dir}\")
    }
}

fn temp_file_seed() -> u32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (now.as_secs() as u32).rotate_left(16) ^ now.subsec_nanos()
}

fn temp_file_name(seed: u32) -> String {
    let mut name = String::with_capacity(8);
    for index in (0..8).rev() {
        let nibble = ((seed >> (index * 4)) & 0x0f) as u8;
        name.push(char::from(b'A' + nibble));
    }
    name
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

fn is_leap_year(year: u16) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_valid_dos_date(year: u16, month: u8, day: u8) -> bool {
    (1980..=2099).contains(&year) && day != 0 && day <= days_in_month(year, month)
}

fn dos_day_of_week(year: u16, month: u8, day: u8) -> u8 {
    let days = days_from_civil(i64::from(year), u32::from(month), u32::from(day));
    (days + 4).rem_euclid(7) as u8
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

fn sft_name_from_path(path: &Path) -> [u8; 11] {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(host_name_to_8_3)
        .unwrap_or([b' '; 11])
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

fn find_name_from_8_3(name: &[u8; 11]) -> String {
    let stem = String::from_utf8_lossy(&name[..8]).trim_end().to_string();
    let ext = String::from_utf8_lossy(&name[8..]).trim_end().to_string();
    if ext.is_empty() {
        stem
    } else {
        format!("{stem}.{ext}")
    }
}

fn volume_label_find_entry(label: [u8; 11]) -> FindEntry {
    FindEntry {
        attr: 0x08,
        time: 0,
        date: (1 << 5) | 1, // 1980-01-01
        size: 0,
        raw_name: label,
        name: String::from_utf8_lossy(&label).into_owned(),
    }
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
    for (index, byte) in entry.raw_name.into_iter().enumerate() {
        mem.write_u8(dirent + index, byte)?;
    }
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

/// FCB record operations cannot let the Disk Transfer Area wrap past offset
/// FFFFh. Real DOS reports AL=02h for this before copying bytes.
fn fcb_dta_transfer_fits(dta: (u16, u16), bytes: usize) -> bool {
    usize::from(dta.1).saturating_add(bytes) <= 0x1_0000
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

pub fn read_dos_asciiz(mem: &Memory, seg: u16, off: u16) -> Result<Option<String>, DosError> {
    read_asciiz(mem, seg, off)
}

fn read_machine_name(
    mem: &Memory,
    seg: u16,
    off: u16,
) -> Result<[u8; DOS_MACHINE_NAME_LEN], DosError> {
    let base = usize::from(seg) * 16 + usize::from(off);
    let mut name = [b' '; DOS_MACHINE_NAME_LEN];
    for (index, slot) in name.iter_mut().enumerate() {
        let byte = mem.read_u8(base + index)?;
        if byte == 0 {
            break;
        }
        *slot = byte;
    }
    Ok(name)
}

fn write_machine_name(
    mem: &mut Memory,
    seg: u16,
    off: u16,
    name: &[u8; DOS_MACHINE_NAME_LEN],
) -> Result<(), DosError> {
    let base = usize::from(seg) * 16 + usize::from(off);
    for (index, byte) in name.iter().enumerate() {
        mem.write_u8(base + index, *byte)?;
    }
    mem.write_u8(base + DOS_MACHINE_NAME_LEN, 0)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn redirected_stdout_ah02_and_ah40_write_to_the_host_file() {
        use std::io::{Read, Seek, SeekFrom};
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let shared = Rc::new(RefCell::new(tempfile::tempfile().unwrap()));
        kernel.open_files.insert(
            1,
            OpenFile {
                target: OutputTarget::Host(shared.clone()),
                mode: AccessMode::Write,
                sft_name: *b"OUT        ",
            },
        );
        let mut c = DosRegs {
            ax: 0x0200,
            dx: u16::from(b'A'),
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut c, &mut mem).unwrap();
        assert!(!c.cf);
        let src = 0x0100usize * 16 + 0x0300;
        mem.write_u8(src, b'B').unwrap();
        mem.write_u8(src + 1, b'C').unwrap();
        let mut w = DosRegs {
            ax: 0x4000,
            bx: 1,
            cx: 2,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut w, &mut mem).unwrap();
        assert!(!w.cf);
        assert_eq!(w.ax, 2);
        assert!(kernel.stdout().is_empty());
        let mut got = Vec::new();
        shared.borrow_mut().seek(SeekFrom::Start(0)).unwrap();
        shared.borrow_mut().read_to_end(&mut got).unwrap();
        assert_eq!(got, b"ABC");
    }

    #[test]
    fn default_stdout_still_reaches_the_screen_buffer() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        kernel.seed_standard_handles();
        let mut c = DosRegs {
            ax: 0x0200,
            dx: u16::from(b'X'),
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut c, &mut mem).unwrap();
        let src = 0x0100usize * 16 + 0x0300;
        mem.write_u8(src, b'Y').unwrap();
        let mut w = DosRegs {
            ax: 0x4000,
            bx: 1,
            cx: 1,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut w, &mut mem).unwrap();
        assert_eq!(kernel.stdout(), b"XY");
    }

    #[test]
    fn dup_of_handle_one_succeeds_and_dup2_redirects_then_restores() {
        use std::io::{Read, Seek, SeekFrom};
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        kernel.seed_standard_handles();
        let mut dup = DosRegs {
            ax: 0x4500,
            bx: 1,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut dup, &mut mem).unwrap();
        assert!(!dup.cf, "DUP of handle 1 now succeeds");
        let saved = dup.ax;
        assert!(saved >= 5);
        let shared = Rc::new(RefCell::new(tempfile::tempfile().unwrap()));
        kernel.open_files.insert(
            6,
            OpenFile {
                target: OutputTarget::Host(shared.clone()),
                mode: AccessMode::Write,
                sft_name: *b"OUT        ",
            },
        );
        let mut d2 = DosRegs {
            ax: 0x4600,
            bx: 6,
            cx: 1,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut d2, &mut mem).unwrap();
        assert!(!d2.cf);
        let mut c = DosRegs {
            ax: 0x0200,
            dx: u16::from(b'Z'),
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut c, &mut mem).unwrap();
        assert!(kernel.stdout().is_empty());
        let mut restore = DosRegs {
            ax: 0x4600,
            bx: saved,
            cx: 1,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut restore, &mut mem).unwrap();
        let mut c2 = DosRegs {
            ax: 0x0200,
            dx: u16::from(b'!'),
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut c2, &mut mem).unwrap();
        assert_eq!(kernel.stdout(), b"!");
        let mut got = Vec::new();
        shared.borrow_mut().seek(SeekFrom::Start(0)).unwrap();
        shared.borrow_mut().read_to_end(&mut got).unwrap();
        assert_eq!(got, b"Z");
    }

    #[test]
    fn ah3f_handle0_from_a_host_file_reads_then_eofs_with_no_waitforkey() {
        use std::io::{Seek, SeekFrom, Write as _};
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(b"hi").unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();
        kernel.open_files.insert(
            0,
            OpenFile {
                target: OutputTarget::Host(Rc::new(RefCell::new(f))),
                mode: AccessMode::Read,
                sft_name: *b"IN         ",
            },
        );
        let dst = 0x0100usize * 16 + 0x0400;
        let mut r = DosRegs {
            ax: 0x3f00,
            bx: 0,
            cx: 8,
            ds: 0x0100,
            dx: 0x0400,
            ..DosRegs::default()
        };
        let action = kernel.dispatch(0x21, &mut r, &mut mem).unwrap();
        assert!(
            matches!(action, DosAction::Continue),
            "a Host stdin never returns WaitForKey"
        );
        assert_eq!(r.ax, 2);
        assert_eq!(mem.read_u8(dst).unwrap(), b'h');
        assert_eq!(mem.read_u8(dst + 1).unwrap(), b'i');
        let mut r2 = DosRegs {
            ax: 0x3f00,
            bx: 0,
            cx: 8,
            ds: 0x0100,
            dx: 0x0400,
            ..DosRegs::default()
        };
        let a2 = kernel.dispatch(0x21, &mut r2, &mut mem).unwrap();
        assert!(matches!(a2, DosAction::Continue));
        assert_eq!(r2.ax, 0);
    }

    #[test]
    fn ah44_isdev_reports_device_for_console_and_file_for_host() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        kernel.seed_standard_handles();
        let mut q = DosRegs {
            ax: 0x4400,
            bx: 1,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut q, &mut mem).unwrap();
        assert!(!q.cf);
        assert_ne!(q.dx & 0x0080, 0, "console stdout is a device");
        kernel.open_files.insert(
            1,
            OpenFile {
                target: OutputTarget::Host(Rc::new(RefCell::new(tempfile::tempfile().unwrap()))),
                mode: AccessMode::Write,
                sft_name: *b"OUT        ",
            },
        );
        let mut q2 = DosRegs {
            ax: 0x4400,
            bx: 1,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut q2, &mut mem).unwrap();
        assert!(!q2.cf);
        assert_eq!(q2.dx & 0x0080, 0, "a redirected stdout is a file");
    }

    #[test]
    fn a_child_inherits_a_redirected_handle_one_across_the_exec_clone() {
        let mut kernel = DosKernel::new();
        kernel.seed_standard_handles();
        let shared = Rc::new(RefCell::new(tempfile::tempfile().unwrap()));
        kernel.open_files.insert(
            1,
            OpenFile {
                target: OutputTarget::Host(shared.clone()),
                mode: AccessMode::Write,
                sft_name: *b"OUT        ",
            },
        );
        let _parent_clone = kernel.open_files.clone();
        kernel.seed_standard_handles(); // child seed must NOT clobber the redirect
        match kernel.open_files.get(&1).map(|of| of.is_console()) {
            Some(false) => {}
            other => {
                panic!("child must inherit the Host redirect at handle 1, got is_console={other:?}")
            }
        }
    }

    #[cfg(windows)]
    fn raw_too_many_open_files_error() -> i32 {
        4
    }

    #[cfg(unix)]
    fn raw_too_many_open_files_error() -> i32 {
        24
    }

    #[cfg(not(any(windows, unix)))]
    fn raw_too_many_open_files_error() -> i32 {
        4
    }

    #[test]
    fn toka_install_ensure_repair_format() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let files = vec![
            ("ICOMMAND.COM".to_string(), vec![1u8, 2, 3]),
            ("VER.COM".to_string(), vec![4u8]),
        ];

        // Format lays everything down on a fresh drive under C:\DOS.
        toka_dos_install(root, &files, InstallMode::Format).unwrap();
        assert!(root.join("DOS").join("ICOMMAND.COM").exists());
        assert!(root.join("DOS").join("VER.COM").exists());
        assert!(!root.join("ICOMMAND.COM").exists());
        assert!(!root.join("VER.COM").exists());

        // EnsureIfMissing is a no-op once the marker is present: a hand-edited
        // system file is left untouched.
        std::fs::write(root.join("DOS").join("ICOMMAND.COM"), b"edited").unwrap();
        std::fs::write(root.join("ICOMMAND.COM"), b"stale").unwrap();
        toka_dos_install(root, &files, InstallMode::EnsureIfMissing).unwrap();
        assert_eq!(
            std::fs::read(root.join("DOS").join("ICOMMAND.COM")).unwrap(),
            b"edited"
        );
        assert!(
            !root.join("ICOMMAND.COM").exists(),
            "EnsureIfMissing removes stale root system files"
        );

        // Repair overwrites system files but keeps a stray user file.
        std::fs::write(root.join("USER.TXT"), b"x").unwrap();
        toka_dos_install(root, &files, InstallMode::Repair).unwrap();
        assert_eq!(
            std::fs::read(root.join("DOS").join("ICOMMAND.COM")).unwrap(),
            vec![1, 2, 3]
        );
        assert!(root.join("USER.TXT").exists());

        // Format wipes the stray user file, then reinstalls.
        toka_dos_install(root, &files, InstallMode::Format).unwrap();
        assert!(!root.join("USER.TXT").exists());
        assert!(root.join("DOS").join("ICOMMAND.COM").exists());
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

        // System files live under C:\DOS, including the shell and COMMAND alias.
        assert!(root.join("DOS").join("ICOMMAND.COM").exists());
        assert!(root.join("DOS").join("COMMAND.COM").exists());
        assert!(root.join("DOS").join("MEM.COM").exists());
        assert!(!root.join("ICOMMAND.COM").exists());
        assert!(!root.join("COMMAND.COM").exists());
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

        // Repair hands back the known-good default and saves the prior file to
        // its .OLD sibling, so a novice who broke CONFIG.SYS can recover and
        // still get their old text back.
        std::fs::write(root.join("AUTOEXEC.BAT"), b"REM mine").unwrap();
        std::fs::write(root.join("CONFIG.SYS"), b"REM cfg").unwrap();
        toka_dos_install(root, &files, InstallMode::Repair).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("AUTOEXEC.BAT")).unwrap(),
            DEFAULT_AUTOEXEC_BAT,
            "Repair writes the default AUTOEXEC.BAT"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("CONFIG.SYS")).unwrap(),
            DEFAULT_CONFIG_SYS,
            "Repair writes the default CONFIG.SYS"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("AUTOEXEC.OLD")).unwrap(),
            "REM mine",
            "Repair backs the prior AUTOEXEC.BAT up to AUTOEXEC.OLD"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("CONFIG.OLD")).unwrap(),
            "REM cfg",
            "Repair backs the prior CONFIG.SYS up to CONFIG.OLD"
        );
    }

    #[test]
    fn repair_backs_up_custom_config_sys() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let files = vec![("ICOMMAND.COM".to_string(), vec![1u8])];
        toka_dos_install(root, &files, InstallMode::Format).unwrap();

        std::fs::write(root.join("CONFIG.SYS"), b"REM broken").unwrap();
        toka_dos_install(root, &files, InstallMode::Repair).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("CONFIG.OLD")).unwrap(),
            "REM broken",
            "Repair saves the custom CONFIG.SYS to CONFIG.OLD"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("CONFIG.SYS")).unwrap(),
            DEFAULT_CONFIG_SYS,
            "Repair writes the default CONFIG.SYS"
        );
    }

    #[test]
    fn repair_backs_up_custom_autoexec_bat() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let files = vec![("ICOMMAND.COM".to_string(), vec![1u8])];
        toka_dos_install(root, &files, InstallMode::Format).unwrap();

        std::fs::write(root.join("AUTOEXEC.BAT"), b"REM mine").unwrap();
        toka_dos_install(root, &files, InstallMode::Repair).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("AUTOEXEC.OLD")).unwrap(),
            "REM mine",
            "Repair saves the custom AUTOEXEC.BAT to AUTOEXEC.OLD"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("AUTOEXEC.BAT")).unwrap(),
            DEFAULT_AUTOEXEC_BAT,
            "Repair writes the default AUTOEXEC.BAT"
        );
    }

    #[test]
    fn repair_writes_defaults_and_no_old_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let files = vec![("ICOMMAND.COM".to_string(), vec![1u8])];
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("ICOMMAND.COM"), vec![1u8]).unwrap();
        assert!(!root.join("CONFIG.SYS").exists());
        assert!(!root.join("AUTOEXEC.BAT").exists());

        toka_dos_install(root, &files, InstallMode::Repair).unwrap();

        assert!(root.join("DOS").join("ICOMMAND.COM").exists());
        assert!(
            !root.join("ICOMMAND.COM").exists(),
            "Repair removes stale root system files"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("CONFIG.SYS")).unwrap(),
            DEFAULT_CONFIG_SYS
        );
        assert_eq!(
            std::fs::read_to_string(root.join("AUTOEXEC.BAT")).unwrap(),
            DEFAULT_AUTOEXEC_BAT
        );
        assert!(
            !root.join("CONFIG.OLD").exists(),
            "no backup is created when CONFIG.SYS was absent"
        );
        assert!(
            !root.join("AUTOEXEC.OLD").exists(),
            "no backup is created when AUTOEXEC.BAT was absent"
        );
    }

    #[test]
    fn repair_leaves_unrelated_user_files_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let files = vec![("ICOMMAND.COM".to_string(), vec![1u8])];
        toka_dos_install(root, &files, InstallMode::Format).unwrap();

        let wolf_dir = root.join("GAMES").join("WOLF3D");
        std::fs::create_dir_all(&wolf_dir).unwrap();
        let wolf_exe = wolf_dir.join("WOLF3D.EXE");
        std::fs::write(&wolf_exe, b"GAME").unwrap();

        toka_dos_install(root, &files, InstallMode::Repair).unwrap();

        assert_eq!(
            std::fs::read(&wolf_exe).unwrap(),
            b"GAME",
            "Repair leaves an unrelated user file untouched"
        );
    }

    #[test]
    fn repair_overwrites_stale_config_old() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let files = vec![("ICOMMAND.COM".to_string(), vec![1u8])];
        toka_dos_install(root, &files, InstallMode::Format).unwrap();

        std::fs::write(root.join("CONFIG.OLD"), b"REM stale").unwrap();
        std::fs::write(root.join("CONFIG.SYS"), b"REM current").unwrap();

        toka_dos_install(root, &files, InstallMode::Repair).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("CONFIG.OLD")).unwrap(),
            "REM current",
            "Repair overwrites a stale CONFIG.OLD with the just-backed-up CONFIG.SYS"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("CONFIG.SYS")).unwrap(),
            DEFAULT_CONFIG_SYS
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
    fn ah0a_f1_and_f3_recall_the_input_template() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let buf = 0x2000usize;
        mem.write_u8(buf, 8).unwrap();
        mem.write_u8(buf + 1, 3).unwrap();
        mem.write_u8(buf + 2, b'D').unwrap();
        mem.write_u8(buf + 3, b'O').unwrap();
        mem.write_u8(buf + 4, b'S').unwrap();
        seed_ring_words(&mut mem, &[0x3b00, 0x3d00, 0x000d]); // F1, F3, CR
        let mut regs = DosRegs {
            ax: 0x0a00,
            dx: buf as u16,
            ds: 0,
            ..Default::default()
        };

        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();

        assert_eq!(action, DosAction::Continue);
        assert_eq!(mem.read_u8(buf + 1).unwrap(), 3);
        assert_eq!(mem.read_u8(buf + 2).unwrap(), b'D');
        assert_eq!(mem.read_u8(buf + 3).unwrap(), b'O');
        assert_eq!(mem.read_u8(buf + 4).unwrap(), b'S');
        assert_eq!(mem.read_u8(buf + 5).unwrap(), 0x0d);
    }

    #[test]
    fn ah0a_delete_skips_one_template_character() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let buf = 0x2000usize;
        mem.write_u8(buf, 8).unwrap();
        mem.write_u8(buf + 1, 3).unwrap();
        mem.write_u8(buf + 2, b'A').unwrap();
        mem.write_u8(buf + 3, b'B').unwrap();
        mem.write_u8(buf + 4, b'C').unwrap();
        seed_ring_words(&mut mem, &[0x5300, 0x3d00, 0x000d]); // Del, F3, CR
        let mut regs = DosRegs {
            ax: 0x0a00,
            dx: buf as u16,
            ds: 0,
            ..Default::default()
        };

        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();

        assert_eq!(action, DosAction::Continue);
        assert_eq!(mem.read_u8(buf + 1).unwrap(), 2);
        assert_eq!(mem.read_u8(buf + 2).unwrap(), b'B');
        assert_eq!(mem.read_u8(buf + 3).unwrap(), b'C');
        assert_eq!(mem.read_u8(buf + 4).unwrap(), 0x0d);
    }

    #[test]
    fn ah0a_f5_stores_current_line_as_the_new_template() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let buf = 0x2000usize;
        mem.write_u8(buf, 10).unwrap();
        mem.write_u8(buf + 1, 3).unwrap();
        mem.write_u8(buf + 2, b'O').unwrap();
        mem.write_u8(buf + 3, b'L').unwrap();
        mem.write_u8(buf + 4, b'D').unwrap();
        seed_ring_words(&mut mem, &[0x004e, 0x0045, 0x0057, 0x3f00, 0x3d00, 0x000d]); // "NEW", F5, F3, CR
        let mut regs = DosRegs {
            ax: 0x0a00,
            dx: buf as u16,
            ds: 0,
            ..Default::default()
        };

        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();

        assert_eq!(action, DosAction::Continue);
        assert_eq!(mem.read_u8(buf + 1).unwrap(), 3);
        assert_eq!(mem.read_u8(buf + 2).unwrap(), b'N');
        assert_eq!(mem.read_u8(buf + 3).unwrap(), b'E');
        assert_eq!(mem.read_u8(buf + 4).unwrap(), b'W');
        assert_eq!(mem.read_u8(buf + 5).unwrap(), 0x0d);
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
    fn ah33_ctrl_break_flag_round_trips() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();

        let mut set = DosRegs {
            ax: 0x3301,
            dx: 0x0001,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut set, &mut mem).unwrap();

        let mut get = DosRegs {
            ax: 0x3300,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut get, &mut mem).unwrap();

        assert_eq!(get.dx & 0x00ff, 1, "AH=33h reports the stored BREAK flag");

        let mut set_and_get = DosRegs {
            ax: 0x3302,
            dx: 0x0000,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut set_and_get, &mut mem).unwrap();
        assert_eq!(
            set_and_get.dx & 0x00ff,
            1,
            "AX=3302h returns the old BREAK flag"
        );

        let mut get_after = DosRegs {
            ax: 0x3300,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut get_after, &mut mem).unwrap();
        assert_eq!(get_after.dx & 0x00ff, 0, "AX=3302h updates the BREAK flag");

        let mut cpsw = DosRegs {
            ax: 0x3303,
            dx: 0x1201,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut cpsw, &mut mem).unwrap();
        assert_eq!(cpsw.dx, 0x1200, "CPSW is parked off");

        let mut boot = DosRegs {
            ax: 0x3305,
            dx: 0x1200,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut boot, &mut mem).unwrap();
        assert_eq!(boot.dx, 0x1203, "Toka-DOS boots from C:");

        let mut bad = DosRegs {
            ax: 0x3307,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut bad, &mut mem).unwrap();
        assert_eq!(bad.ax & 0x00ff, 0xff);
    }

    #[test]
    fn ah37_switch_char_round_trips_and_availdev_matches_dos4() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();

        let mut get_default = DosRegs {
            ax: 0x3700,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut get_default, &mut mem).unwrap();
        assert_eq!(get_default.dx & 0x00ff, u16::from(b'/'));

        let mut set = DosRegs {
            ax: 0x3701,
            dx: u16::from(b'-'),
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut set, &mut mem).unwrap();
        let mut get_changed = DosRegs {
            ax: 0x3700,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut get_changed, &mut mem).unwrap();
        assert_eq!(get_changed.dx & 0x00ff, u16::from(b'-'));

        let mut avail = DosRegs {
            ax: 0x3702,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut avail, &mut mem).unwrap();
        assert_eq!(avail.dx & 0x00ff, 0xff, "DOS 4 reports AVAILDEV on");

        let mut set_avail = DosRegs {
            ax: 0x3703,
            dx: 0,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut set_avail, &mut mem).unwrap();
        let mut still_avail = DosRegs {
            ax: 0x3702,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut still_avail, &mut mem).unwrap();
        assert_eq!(still_avail.dx & 0x00ff, 0xff, "AL=03h is a no-op in DOS 4");

        let mut invalid = DosRegs {
            ax: 0x3704,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut invalid, &mut mem).unwrap();
        assert_eq!(invalid.ax & 0x00ff, 0xff);
    }

    #[test]
    fn ah01_ctrl_c_invokes_int23_instead_of_returning_byte() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        seed_keyboard_ring(&mut mem, &[0x03]).unwrap();

        let mut regs = DosRegs {
            ax: 0x0100,
            ..Default::default()
        };
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();

        assert_eq!(
            action,
            DosAction::InvokeInterrupt(0x23),
            "Ctrl-C must dispatch INT 23h"
        );
        assert!(kernel.stdout().is_empty(), "Ctrl-C is consumed, not echoed");
    }

    #[test]
    fn ah3f_handle_zero_reads_a_cooked_console_line() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let buf = 0x2000usize;
        seed_keyboard_ring(&mut mem, b"go\r").unwrap();

        let mut regs = DosRegs {
            ax: 0x3f00,
            bx: 0,
            cx: 8,
            dx: buf as u16,
            ds: 0,
            ..Default::default()
        };
        assert_eq!(
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap(),
            DosAction::Continue
        );

        assert!(!regs.cf);
        assert_eq!(regs.ax, 3, "stdin read returns bytes through the CR");
        assert_eq!(mem.read_u8(buf).unwrap(), b'g');
        assert_eq!(mem.read_u8(buf + 1).unwrap(), b'o');
        assert_eq!(mem.read_u8(buf + 2).unwrap(), b'\r');
        assert_eq!(kernel.stdout(), b"go\r", "console stdin read echoes");
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
    fn ah00_restores_psp_saved_vectors() {
        let (mut kernel, mut mem, _prog_top) = env_kernel();
        seed_psp_saved_and_live_vectors(&mut mem);
        let mut regs = DosRegs {
            ax: 0x0000,
            ..Default::default()
        };

        assert_eq!(
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap(),
            DosAction::Exit(0)
        );

        assert_psp_saved_vectors_restored(&mem);
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

    /// Total free conventional paragraphs: the sum of every owner-0 block's data
    /// size in the program's MCB chain.
    fn conventional_free_paragraphs(kernel: &DosKernel, mem: &Memory) -> u32 {
        read_mcb_chain(mem, kernel.arena.first_mcb())
            .iter()
            .filter(|m| m.owner == 0)
            .map(|m| u32::from(m.size))
            .sum()
    }

    /// Walk the device chain from the SysVars first-device pointer (the NUL header)
    /// and return each 8-byte device name. The kernel publishes the chain through
    /// AH=52h; this reads the same structure NUL heads.
    fn device_chain_names(mem: &Memory) -> Vec<[u8; 8]> {
        // NUL sits at SYSVARS_SEG:0x24 (the 2-byte first-MCB field precedes the
        // device area). Follow next pointers until the FFFF:FFFF terminator.
        let mut names = Vec::new();
        let mut off = 0x0024u16;
        let mut seg = 0x0064u16;
        for _ in 0..32 {
            if seg == 0xffff && off == 0xffff {
                break;
            }
            let header = usize::from(seg) * 16 + usize::from(off);
            let mut name = [0u8; 8];
            for (i, slot) in name.iter_mut().enumerate() {
                *slot = mem.read_u8(header + 0x0a + i).unwrap_or(0);
            }
            names.push(name);
            let next_off = mem.read_u16(header).unwrap_or(0xffff);
            let next_seg = mem.read_u16(header + 2).unwrap_or(0xffff);
            off = next_off;
            seg = next_seg;
        }
        names
    }

    /// Lay the SysVars device-chain skeleton by issuing an AH=52h query, then walk
    /// it. Used to check a loaded driver survives the SysVars rebuild.
    fn published_device_chain(kernel: &mut DosKernel, mem: &mut Memory) -> Vec<[u8; 8]> {
        let mut regs = DosRegs {
            ax: 0x5200,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, mem).unwrap();
        device_chain_names(mem)
    }

    fn tests_block_image() -> Vec<u8> {
        let mut image = driver::tests_char_image();
        image[4..6].copy_from_slice(&0x0000u16.to_le_bytes());
        image
    }

    fn tests_bpb_1440k() -> [u8; 0x19] {
        tests_bpb_custom(512, 1, 2880, 0xf0)
    }

    fn tests_bpb_custom(
        bytes_per_sector: u16,
        sectors_per_cluster: u8,
        total_sectors: u16,
        media: u8,
    ) -> [u8; 0x19] {
        let mut bpb = [0u8; 0x19];
        bpb[0..2].copy_from_slice(&bytes_per_sector.to_le_bytes());
        bpb[2] = sectors_per_cluster;
        bpb[3..5].copy_from_slice(&1u16.to_le_bytes());
        bpb[5] = 2;
        bpb[6..8].copy_from_slice(&224u16.to_le_bytes());
        bpb[8..10].copy_from_slice(&total_sectors.to_le_bytes());
        bpb[10] = media;
        bpb[11..13].copy_from_slice(&9u16.to_le_bytes());
        bpb[13..15].copy_from_slice(&18u16.to_le_bytes());
        bpb[15..17].copy_from_slice(&2u16.to_le_bytes());
        bpb
    }

    fn tests_block_image_with_bpbs(bpbs: &[(u16, [u8; 0x19])]) -> Vec<u8> {
        let mut image = tests_block_image();
        for &(offset, bpb) in bpbs {
            let end = usize::from(offset) + bpb.len();
            if image.len() < end {
                image.resize(end, 0);
            }
            image[usize::from(offset)..end].copy_from_slice(&bpb);
        }
        image
    }

    fn tests_block_image_with_bpb(bpb_offset: u16) -> Vec<u8> {
        tests_block_image_with_bpbs(&[(bpb_offset, tests_bpb_1440k())])
    }

    fn complete_block_init_with_offsets(
        mem: &mut Memory,
        staged: &StagedDriver,
        units: u8,
        array_offset: u16,
        bpb_offsets: &[u16],
    ) {
        let base = usize::from(staged.driver_seg) * 16;
        for (unit, offset) in bpb_offsets.iter().copied().enumerate() {
            mem.write_u16(base + usize::from(array_offset) + unit * 2, offset)
                .unwrap();
        }
        mem.write_u16(staged.request_linear + 0x03, 0x0100).unwrap();
        mem.write_u8(staged.request_linear + 0x0d, units).unwrap();
        mem.write_u16(staged.request_linear + 0x0e, 0x0100).unwrap();
        mem.write_u16(staged.request_linear + 0x10, staged.driver_seg)
            .unwrap();
        mem.write_u16(staged.request_linear + 0x12, array_offset)
            .unwrap();
        mem.write_u16(staged.request_linear + 0x14, staged.driver_seg)
            .unwrap();
    }

    fn complete_block_init_with_bpb(mem: &mut Memory, staged: &StagedDriver, bpb_offset: u16) {
        let array_offset = 0x20;
        complete_block_init_with_offsets(mem, staged, 1, array_offset, &[bpb_offset]);
    }

    fn ah52_sysvars_base(kernel: &mut DosKernel, mem: &mut Memory) -> usize {
        let mut regs = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, mem).unwrap();
        usize::from(regs.es) * 16 + usize::from(regs.bx)
    }

    fn ah52_dpb_drives(kernel: &mut DosKernel, mem: &mut Memory) -> Vec<u8> {
        let base = ah52_sysvars_base(kernel, mem);
        let mut off = mem.read_u16(base).unwrap();
        let mut seg = mem.read_u16(base + 2).unwrap();
        let mut drives = Vec::new();
        for _ in 0..32 {
            if (off, seg) == (0xffff, 0xffff) {
                break;
            }
            let dpb = usize::from(seg) * 16 + usize::from(off);
            drives.push(mem.read_u8(dpb).unwrap());
            off = mem.read_u16(dpb + 0x19).unwrap();
            seg = mem.read_u16(dpb + 0x1b).unwrap();
        }
        drives
    }

    #[test]
    fn stage_sys_driver_sets_first_loaded_block_driver_to_drive_d() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut dos = DosKernel::new();
        dos.init_program(0x0100, 0x1100, &mut mem).unwrap();
        let image = tests_block_image();

        let staged = dos
            .stage_sys_driver(&image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();

        assert_eq!(
            mem.read_u8(staged.request_linear + 0x16).unwrap(),
            3,
            "first loaded block device should receive D: as first-drive byte"
        );
    }

    #[test]
    fn stage_sys_driver_keeps_finalize_metadata_for_block_and_character_drivers() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut dos = DosKernel::new();
        dos.init_program(0x0100, 0x1100, &mut mem).unwrap();
        let block_image = tests_block_image();
        let char_image = driver::tests_char_image();

        let block = dos
            .stage_sys_driver(&block_image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        let character = dos
            .stage_sys_driver(&char_image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();

        assert!(block.is_block_device);
        assert_eq!(block.first_drive, 3);
        assert_eq!(block.allocation_seg, block.driver_seg);
        assert_eq!(block.allocation_paras, 5);
        assert!(!character.is_block_device);
        assert_eq!(character.first_drive, 0);
        assert_eq!(character.allocation_seg, character.driver_seg);
        assert_eq!(character.allocation_paras, 5);
    }

    #[test]
    fn staging_allocates_copies_and_finalize_splices_after_nul() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut dos = DosKernel::new();
        dos.init_program(0x0100, 0x1100, &mut mem).unwrap();
        let image = driver::tests_char_image();

        let staged = dos
            .stage_sys_driver(&image, "RAM", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        // Image copied flat to the block; header readable at block:0.
        let base = usize::from(staged.driver_seg) * 16;
        assert_eq!(mem.read_u16(base + 4).unwrap(), 0x8000); // attribute survived copy
        // Request header built but INIT not run yet.
        let r = driver::read_init_result(&mem, staged.request_linear).unwrap();
        assert!(!r.done);

        // Simulate a successful INIT: DONE, break = one paragraph of resident code.
        mem.write_u16(staged.request_linear + 0x03, 0x0100).unwrap();
        mem.write_u16(staged.request_linear + 0x0e, 0x0010).unwrap();
        mem.write_u16(staged.request_linear + 0x10, staged.driver_seg)
            .unwrap();

        dos.finalize_sys_driver(&staged, &mut mem).unwrap();

        // The published chain lists the driver between NUL and the first built-in.
        let names = published_device_chain(&mut dos, &mut mem);
        assert_eq!(names.first(), Some(b"NUL     "));
        assert_eq!(names.get(1), Some(b"TESTDEV "));
        assert!(names.iter().any(|n| n == b"CON     "));
    }

    #[test]
    fn staging_stamps_the_resident_block_with_the_system_psp() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut dos = DosKernel::new();
        dos.init_program(0x0100, 0x1100, &mut mem).unwrap();
        let image = driver::tests_char_image();

        let staged = dos
            .stage_sys_driver(&image, "RAM", DriverLoadPlacement::Low, &mut mem)
            .unwrap();

        // The block's MCB owner word (header paragraph + 1) is the system PSP, so the
        // resident driver survives an EXEC child's exit sweep.
        let owner = mem
            .read_u16((usize::from(staged.driver_seg) - 1) * 16 + 1)
            .unwrap();
        assert_eq!(
            owner, dos.arena.psp_seg,
            "resident block owned by the system PSP"
        );
    }

    #[test]
    fn staging_a_driver_too_large_for_the_arena_reports_out_of_memory() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut dos = DosKernel::new();
        // The program owns nearly the whole arena up to ARENA_TOP (0xA000), leaving
        // only a few free paragraphs above it for a resident driver block.
        dos.init_program(0x0100, 0x9ff0, &mut mem).unwrap();
        // An image larger than the free tail but well inside the paragraph range: the
        // allocator finds no block that fits and reports OutOfMemory.
        let mut image = driver::tests_char_image();
        image.resize(64 * 1024, 0); // 0x1000 paragraphs, more than the free tail
        assert!(matches!(
            dos.stage_sys_driver(&image, "", DriverLoadPlacement::Low, &mut mem),
            Err(DriverStageError::OutOfMemory)
        ));
    }

    #[test]
    fn abort_frees_a_failed_driver_block() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut dos = DosKernel::new();
        dos.init_program(0x0100, 0x1100, &mut mem).unwrap();
        let image = driver::tests_char_image();
        let before = conventional_free_paragraphs(&dos, &mem);
        let staged = dos
            .stage_sys_driver(&image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        assert!(conventional_free_paragraphs(&dos, &mem) < before); // block taken
        dos.abort_sys_driver(&staged, &mut mem).unwrap();
        assert_eq!(conventional_free_paragraphs(&dos, &mem), before); // block returned
    }

    #[test]
    fn stage_sys_driver_high_then_low_uses_the_linked_upper_arena() {
        let (mut kernel, mut mem) = umb_test_kernel();
        link_umbs(&mut kernel, &mut mem);
        let image = driver::tests_char_image();
        let conventional_before = conventional_free_paragraphs(&kernel, &mem);

        let staged = kernel
            .stage_sys_driver(&image, "", DriverLoadPlacement::HighThenLow, &mut mem)
            .unwrap();

        assert!(
            (0xc800..0xf000).contains(&staged.driver_seg),
            "staged driver should land in the UMB window, got {:#06x}",
            staged.driver_seg
        );
        assert_eq!(
            conventional_free_paragraphs(&kernel, &mem),
            conventional_before,
            "high staging must not consume conventional memory"
        );
        let chain = kernel.umb_chain(&mem);
        let block = chain
            .iter()
            .find(|m| m.mcb_seg.wrapping_add(1) == staged.driver_seg)
            .expect("UMB MCB for staged driver");
        assert_eq!(block.owner, 0x0100);
    }

    #[test]
    fn stage_sys_driver_high_then_low_falls_back_when_upper_memory_is_full() {
        let (mut kernel, mut mem) = umb_test_kernel();
        link_umbs(&mut kernel, &mut mem);
        let drained = kernel.request_umb(0x27fb, &mut mem).unwrap().unwrap();
        assert!(
            (0xc800..0xf000).contains(&drained),
            "drain allocation lands in UMBs"
        );
        let image = driver::tests_char_image();

        let staged = kernel
            .stage_sys_driver(&image, "", DriverLoadPlacement::HighThenLow, &mut mem)
            .unwrap();

        assert!(
            (0x0100..0xa000).contains(&staged.driver_seg),
            "too-small UMB tail should fall back low, got {:#06x}",
            staged.driver_seg
        );
    }

    #[test]
    fn finalize_sys_driver_trims_a_high_loaded_driver_in_the_upper_arena() {
        let (mut kernel, mut mem) = umb_test_kernel();
        link_umbs(&mut kernel, &mut mem);
        let image = driver::tests_char_image();
        let conventional_before = conventional_free_paragraphs(&kernel, &mem);
        let staged = kernel
            .stage_sys_driver(&image, "", DriverLoadPlacement::HighThenLow, &mut mem)
            .unwrap();

        mem.write_u16(staged.request_linear + 0x03, 0x0100).unwrap();
        mem.write_u16(staged.request_linear + 0x0e, 0x0012).unwrap();
        mem.write_u16(staged.request_linear + 0x10, staged.driver_seg)
            .unwrap();

        kernel.finalize_sys_driver(&staged, &mut mem).unwrap();

        let chain = kernel.umb_chain(&mem);
        let resident = chain
            .iter()
            .find(|m| m.mcb_seg.wrapping_add(1) == staged.driver_seg)
            .expect("resident high driver block");
        assert_eq!(resident.owner, 0x0100);
        assert_eq!(resident.size, 2, "header-sized resident block kept");
        assert!(
            chain
                .iter()
                .any(|m| m.owner == 0 && m.mcb_seg > resident.mcb_seg),
            "trim leaves a reusable upper free tail"
        );
        assert_eq!(
            conventional_free_paragraphs(&kernel, &mem),
            conventional_before,
            "high finalize must not touch conventional memory"
        );
    }

    #[test]
    fn abort_sys_driver_frees_a_high_loaded_driver_from_the_upper_arena() {
        let (mut kernel, mut mem) = umb_test_kernel();
        link_umbs(&mut kernel, &mut mem);
        let image = driver::tests_char_image();
        let conventional_before = conventional_free_paragraphs(&kernel, &mem);
        let staged = kernel
            .stage_sys_driver(&image, "", DriverLoadPlacement::HighThenLow, &mut mem)
            .unwrap();

        kernel.abort_sys_driver(&staged, &mut mem).unwrap();

        let chain = kernel.umb_chain(&mem);
        assert_eq!(chain.len(), 1, "upper arena coalesced back to one block");
        assert_eq!(chain[0].mcb_seg, 0xc800);
        assert_eq!(chain[0].owner, 0);
        assert_eq!(chain[0].size, 0x27ff);
        assert_eq!(
            conventional_free_paragraphs(&kernel, &mem),
            conventional_before,
            "high abort must not touch conventional memory"
        );
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
        // AH=48h UMBs are owner-tagged with the current PSP so EXEC exit can sweep
        // a child's upper-memory blocks. The conventional free tail is intact.
        assert!(kernel.umb_chain(&mem).iter().any(|m| m.owner == 0x0100));
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
                .any(|m| m.owner == 0x0100 && m.size == 0x0400)
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
            kernel.init_program(0x0100, 0x1100, &mut mem).unwrap();
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
            b"CON     ",
            "no EMS links NUL to the standard CON device"
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
    fn ah44_generic_character_ioctl_reports_and_selects_cp437() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let base = 0x0100usize * 16 + 0x0200;

        let mut info = DosRegs {
            ax: 0x4400,
            bx: 1,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut info, &mut mem).unwrap();
        assert!(!info.cf);
        assert_ne!(info.dx & DOS_DEV_ATTR_DEV320, 0);

        let mut cap = DosRegs {
            ax: 0x4410,
            bx: 1,
            cx: 0x036a,
            cf: true,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut cap, &mut mem).unwrap();
        assert!(!cap.cf);
        assert_eq!(cap.ax, 0);

        let mut query = DosRegs {
            ax: 0x440c,
            bx: 1,
            cx: 0x036a,
            ds: 0x0100,
            dx: 0x0200,
            cf: true,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut query, &mut mem).unwrap();
        assert!(!query.cf);
        assert_eq!(mem.read_u16(base).unwrap(), 4);
        assert_eq!(mem.read_u16(base + 2).unwrap(), DOS_CODE_PAGE_US);
        assert_eq!(mem.read_u16(base + 4).unwrap(), 0);

        let mut select = DosRegs {
            ax: 0x440c,
            bx: 1,
            cx: 0x034a,
            ds: 0x0100,
            dx: 0x0200,
            cf: true,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut select, &mut mem).unwrap();
        assert!(!select.cf);

        mem.write_u16(base + 2, 850).unwrap();
        let mut bad_select = DosRegs {
            ax: 0x440c,
            bx: 1,
            cx: 0x034a,
            ds: 0x0100,
            dx: 0x0200,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut bad_select, &mut mem).unwrap();
        assert!(bad_select.cf);
        assert_eq!(bad_select.ax, 0x02);

        let mut list = DosRegs {
            ax: 0x440c,
            bx: 1,
            cx: 0x036b,
            ds: 0x0100,
            dx: 0x0200,
            cf: true,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut list, &mut mem).unwrap();
        assert!(!list.cf);
        assert_eq!(mem.read_u16(base).unwrap(), 8);
        assert_eq!(mem.read_u16(base + 2).unwrap(), 1);
        assert_eq!(mem.read_u16(base + 4).unwrap(), DOS_CODE_PAGE_US);
        assert_eq!(mem.read_u16(base + 6).unwrap(), 1);
        assert_eq!(mem.read_u16(base + 8).unwrap(), DOS_CODE_PAGE_US);

        let mut wrong_category = DosRegs {
            ax: 0x4410,
            bx: 1,
            cx: 0x016a,
            ..Default::default()
        };
        kernel
            .dispatch(0x21, &mut wrong_category, &mut mem)
            .unwrap();
        assert!(wrong_category.cf);
        assert_eq!(wrong_category.ax, 0x01);
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

        for handle in [3u16, 4] {
            let mut regs = DosRegs {
                ax: 0x4400,
                bx: handle,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
            assert_eq!(
                regs.dx & 0x03,
                0,
                "AUX/PRN are character devices, but not STDIN/STDOUT aliases"
            );
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
    fn ah44_input_status_reports_file_eof() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("DATA.TXT", b"hi")], r"C:\DATA.TXT");
        let handle = open_data(&mut kernel, &mut mem);

        let mut before = DosRegs {
            ax: 0x4406,
            bx: handle,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut before, &mut mem).unwrap();
        assert!(!before.cf);
        assert_eq!(before.ax & 0xff, 0xff, "unread file data is ready");

        let read = read(&mut kernel, &mut mem, handle, 8, 0x0400);
        assert_eq!(read.ax, 2);

        let mut after = DosRegs {
            ax: 0x4406,
            bx: handle,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut after, &mut mem).unwrap();
        assert!(!after.cf);
        assert_eq!(after.ax & 0xff, 0x00, "EOF file input is not ready");
    }

    #[test]
    fn ah44_set_device_info_rejects_disk_file_handles() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("DATA.TXT", b"hi")], r"C:\DATA.TXT");
        let handle = open_data(&mut kernel, &mut mem);
        let mut regs = DosRegs {
            ax: 0x4401,
            bx: handle,
            dx: 0x0080,
            ..DosRegs::default()
        };

        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();

        assert!(regs.cf);
        assert_eq!(regs.ax, 0x05);
    }

    #[test]
    fn ah44_handle_remote_rejects_invalid_handles() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut regs = DosRegs {
            ax: 0x440a,
            bx: 0x99,
            ..DosRegs::default()
        };

        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();

        assert!(regs.cf);
        assert_eq!(regs.ax, 0x06);
    }

    #[test]
    fn ah44_drive_queries_reject_unmounted_drives() {
        for ax in [0x4408, 0x4409, 0x440e, 0x440f, 0x4411] {
            let mut kernel = DosKernel::new();
            let mut mem = Memory::new(64 * 1024).unwrap();
            let mut regs = DosRegs {
                ax,
                bx: 0x0001, // A:, not mounted in the HLE
                ..DosRegs::default()
            };

            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();

            assert!(regs.cf, "AX={ax:#06x} should reject A:");
            assert_eq!(regs.ax, 0x0f, "AX={ax:#06x} should report invalid drive");
        }
    }

    #[test]
    fn ah44_single_drive_stub_subfunctions_succeed() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();

        let mut retry = DosRegs {
            ax: 0x440b,
            cx: 1,
            dx: 3,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut retry, &mut mem).unwrap();
        assert!(!retry.cf, "sharing retry stub succeeds");

        for ax in [0x440e, 0x440f] {
            let mut regs = DosRegs {
                ax,
                bx: 0x0003, // C:
                cf: true,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
            assert!(!regs.cf, "AX={ax:#06x} should accept C:");
        }

        let mut cap = DosRegs {
            ax: 0x4410,
            bx: 1,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut cap, &mut mem).unwrap();
        assert!(cap.cf, "handle capability reports unavailable");
        assert_eq!(cap.ax, 0x01);

        let mut drive_cap = DosRegs {
            ax: 0x4411,
            bx: 0x0003, // C:
            cf: false,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut drive_cap, &mut mem).unwrap();
        assert!(drive_cap.cf, "drive capability reports unavailable");
        assert_eq!(drive_cap.ax, 0x01);
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

        let ea_base = 0x0100usize * 16 + 0x0300;
        mem.write_u16(ea_base, 0xffff).unwrap();
        let mut get_eas = DosRegs {
            ax: 0x5702,
            bx: handle,
            cx: 2,
            es: 0x0100,
            di: 0x0300,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get_eas, &mut mem).unwrap();
        assert!(!get_eas.cf);
        assert_eq!(get_eas.cx, 2);
        assert_eq!(mem.read_u16(ea_base).unwrap(), 0, "no extended attributes");

        let mut get_ea_props = DosRegs {
            ax: 0x5703,
            bx: handle,
            cx: 0,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get_ea_props, &mut mem).unwrap();
        assert!(!get_ea_props.cf);
        assert_eq!(get_ea_props.cx, 2);

        let mut set_eas = DosRegs {
            ax: 0x5704,
            bx: handle,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set_eas, &mut mem).unwrap();
        assert!(!set_eas.cf);
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
    fn ah4c_restores_psp_saved_vectors() {
        let (mut kernel, mut mem, _prog_top) = env_kernel();
        seed_psp_saved_and_live_vectors(&mut mem);
        let mut regs = DosRegs {
            ax: 0x4c07,
            ..DosRegs::default()
        };

        assert_eq!(
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap(),
            DosAction::Exit(7)
        );

        assert_psp_saved_vectors_restored(&mem);
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
    fn int20_restores_psp_saved_vectors() {
        let (mut kernel, mut mem, _prog_top) = env_kernel();
        seed_psp_saved_and_live_vectors(&mut mem);
        let mut regs = DosRegs::default();

        assert_eq!(
            kernel.dispatch(0x20, &mut regs, &mut mem).unwrap(),
            DosAction::Exit(0)
        );

        assert_psp_saved_vectors_restored(&mem);
    }

    #[test]
    fn unknown_int21_function_fails_invalid_function() {
        let mut mem = Memory::new(4096).unwrap();
        let mut regs = DosRegs {
            ax: 0x7200,
            ..DosRegs::default()
        };
        let mut kernel = DosKernel::new();
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::Continue);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x0001);
        assert!(kernel.stdout().is_empty());

        let mut err = DosRegs {
            ax: 0x5900,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut err, &mut mem).unwrap();
        assert_eq!(err.ax, 0x0001);
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
        assert_eq!(regs.ax >> 8, 22); // AH = minor (6.22)
        assert_eq!(regs.bx >> 8, 0xff); // BH = OEM
    }

    #[test]
    fn ah33_06_reports_true_toka_dos_version() {
        let mut mem = Memory::new(4096).unwrap();
        let mut regs = DosRegs {
            ax: 0x3306,
            ..DosRegs::default()
        };
        let mut kernel = DosKernel::new();
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(regs.bx & 0x00ff, 6); // BL = major
        assert_eq!(regs.bx >> 8, 22); // BH = minor
        assert_eq!(regs.dx, 0); // revision and flags
        assert_ne!(regs.ax as u8, 0xff);
    }

    #[test]
    fn reported_version_override_affects_ah30_only() {
        let mut mem = Memory::new(4096).unwrap();
        let mut kernel = DosKernel::new();
        kernel.set_reported_version_word(0x0005); // 5.00

        let mut apparent = DosRegs {
            ax: 0x3000,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut apparent, &mut mem).unwrap();
        assert_eq!(apparent.ax, 0x0005);

        let mut true_version = DosRegs {
            ax: 0x3306,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut true_version, &mut mem).unwrap();
        assert_eq!(true_version.bx, 0x1606);

        kernel.set_reported_version_word(0);
        let mut restored = DosRegs {
            ax: 0x3000,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut restored, &mut mem).unwrap();
        assert_eq!(restored.ax, 0x1606);
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
    fn ah67_extends_the_dynamic_handle_count() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("DATA.TXT", b"hi")], r"C:\DATA.TXT");
        kernel.set_config_sys_counts(6, 20);
        assert_eq!(open(&mut kernel, &mut mem).ax, 5);
        let mut grow = DosRegs {
            ax: 0x6700,
            bx: 8,
            ..DosRegs::default()
        };

        kernel.dispatch(0x21, &mut grow, &mut mem).unwrap();

        assert!(!grow.cf);
        assert_eq!(kernel.file_count(), 8);
        assert_eq!(open(&mut kernel, &mut mem).ax, 6);
        assert_eq!(open(&mut kernel, &mut mem).ax, 7);
        let too_many = open(&mut kernel, &mut mem);
        assert!(too_many.cf);
        assert_eq!(too_many.ax, 0x04);

        let mut shrink = DosRegs {
            ax: 0x6700,
            bx: 7,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut shrink, &mut mem).unwrap();
        assert!(shrink.cf, "cannot shrink away live handle 7");
        assert_eq!(shrink.ax, 0x04);
        assert_eq!(
            kernel.file_count(),
            8,
            "failed shrink leaves count unchanged"
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
    fn open_missing_parent_sets_cf_and_ax03() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[], r"C:\NOPE\DATA.TXT");
        let regs = open(&mut kernel, &mut mem);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x03);
    }

    #[test]
    fn dos_io_error_code_splits_common_host_errors() {
        assert_eq!(
            dos_io_error_code(&std::io::Error::from(std::io::ErrorKind::InvalidInput)),
            0x0c
        );
        assert_eq!(
            dos_io_error_code(&std::io::Error::from_raw_os_error(
                raw_too_many_open_files_error()
            )),
            0x04
        );
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
    fn ah46_dup2_rejects_a_target_past_the_handle_count() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("DATA.TXT", b"hi")], r"C:\DATA.TXT");
        kernel.set_config_sys_counts(6, 20);
        assert_eq!(open(&mut kernel, &mut mem).ax, 5);
        let mut regs = DosRegs {
            ax: 0x4600,
            bx: 5,
            cx: 6,
            ..DosRegs::default()
        };

        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();

        assert!(regs.cf);
        assert_eq!(regs.ax, 0x04);
        let close_out_of_range = close(&mut kernel, &mut mem, 6);
        assert!(close_out_of_range.cf);
        assert_eq!(close_out_of_range.ax, 0x06);
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
    fn ah5c_lock_unlock_validates_handle_and_subfunction() {
        let (mut kernel, mut mem, _dir) =
            kernel_with_drive(&[("LOCK.TXT", b"data")], r"C:\LOCK.TXT");
        let opened = open(&mut kernel, &mut mem);
        assert!(!opened.cf);
        let handle = opened.ax;

        for al in [0x00u16, 0x01] {
            let mut regs = DosRegs {
                ax: 0x5c00 | al,
                bx: handle,
                dx: 4,
                di: 8,
                cf: true,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
            assert!(!regs.cf, "lock/unlock succeeds for a valid handle");
        }

        let mut bad_function = DosRegs {
            ax: 0x5c02,
            bx: handle,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut bad_function, &mut mem).unwrap();
        assert!(bad_function.cf);
        assert_eq!(bad_function.ax, 0x01);

        let mut bad_handle = DosRegs {
            ax: 0x5c00,
            bx: 0x7777,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut bad_handle, &mut mem).unwrap();
        assert!(bad_handle.cf);
        assert_eq!(bad_handle.ax, 0x06);
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

    fn open_memory_handle(kernel: &mut DosKernel, data: &[u8]) -> u16 {
        let mut regs = DosRegs {
            ax: 0x3d00,
            ..DosRegs::default()
        };
        kernel.open_readonly_memory_file(&mut regs, *b"MEM     TXT", data.to_vec());
        assert!(!regs.cf, "memory file open failed: ax={:#06x}", regs.ax);
        regs.ax
    }

    #[test]
    fn memory_file_handle_reads_then_eofs() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let handle = open_memory_handle(&mut kernel, b"abcdef");

        let first = read(&mut kernel, &mut mem, handle, 4, 0x0400);
        assert!(!first.cf);
        assert_eq!(first.ax, 4);
        let second = read(&mut kernel, &mut mem, handle, 4, 0x0410);
        assert!(!second.cf);
        assert_eq!(second.ax, 2);
        let third = read(&mut kernel, &mut mem, handle, 4, 0x0420);
        assert!(!third.cf);
        assert_eq!(third.ax, 0);

        let base0 = 0x0100usize * 16 + 0x0400;
        let chunk0: Vec<u8> = (0..4).map(|i| mem.read_u8(base0 + i).unwrap()).collect();
        assert_eq!(chunk0, b"abcd");
        let base1 = 0x0100usize * 16 + 0x0410;
        let chunk1: Vec<u8> = (0..2).map(|i| mem.read_u8(base1 + i).unwrap()).collect();
        assert_eq!(chunk1, b"ef");
    }

    #[test]
    fn memory_file_handle_seek_reads_from_new_position() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let handle = open_memory_handle(&mut kernel, b"abcdef");
        let mut seek = DosRegs {
            ax: 0x4200,
            bx: handle,
            dx: 2,
            ..DosRegs::default()
        };

        kernel.dispatch(0x21, &mut seek, &mut mem).unwrap();

        assert!(!seek.cf);
        assert_eq!(seek.ax, 2);
        assert_eq!(seek.dx, 0);
        let regs = read(&mut kernel, &mut mem, handle, 3, 0x0400);
        assert!(!regs.cf);
        assert_eq!(regs.ax, 3);
        let base = 0x0100usize * 16 + 0x0400;
        let got: Vec<u8> = (0..3).map(|i| mem.read_u8(base + i).unwrap()).collect();
        assert_eq!(got, b"cde");
    }

    #[test]
    fn memory_file_handle_write_and_zero_count_truncate_are_access_denied() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let handle = open_memory_handle(&mut kernel, b"abcdef");
        let src = 0x0100usize * 16 + 0x0500;
        mem.write_u8(src, b'X').unwrap();
        let mut write = DosRegs {
            ax: 0x4000,
            bx: handle,
            cx: 1,
            ds: 0x0100,
            dx: 0x0500,
            ..DosRegs::default()
        };

        kernel.dispatch(0x21, &mut write, &mut mem).unwrap();

        assert!(write.cf);
        assert_eq!(write.ax, 0x05);
        let mut truncate = DosRegs {
            ax: 0x4000,
            bx: handle,
            cx: 0,
            ds: 0x0100,
            dx: 0x0500,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut truncate, &mut mem).unwrap();
        assert!(truncate.cf);
        assert_eq!(truncate.ax, 0x05);
    }

    #[test]
    fn memory_file_handle_commit_and_close_are_valid() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let handle = open_memory_handle(&mut kernel, b"abcdef");

        for ax in [0x6800, 0x6a00] {
            let mut commit = DosRegs {
                ax,
                bx: handle,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut commit, &mut mem).unwrap();
            assert!(!commit.cf, "commit {ax:#06x} succeeds");
            if ax == 0x6a00 {
                assert_eq!(commit.ax >> 8, 0x68, "AH=6Ah returns AH=68h");
            }
        }

        let closed = close(&mut kernel, &mut mem, handle);
        assert!(!closed.cf);
    }

    #[test]
    fn memory_file_handle_sft_reports_size_and_position() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        kernel.init_program(0x0100, 0x1100, &mut mem).unwrap();
        let handle = open_memory_handle(&mut kernel, b"abcdef");
        let read = read(&mut kernel, &mut mem, handle, 2, 0x0400);
        assert!(!read.cf);

        let entry = ah52_sft_entry(&mut kernel, &mut mem, handle);

        assert_eq!(mem.read_u16(entry).unwrap(), 1);
        assert_eq!(mem.read_u16(entry + 0x02).unwrap(), 0);
        assert_eq!(mem.read_u16(entry + 0x05).unwrap() & 0x0080, 0);
        assert_eq!(mem.read_u32(entry + 0x11).unwrap(), 6);
        assert_eq!(mem.read_u32(entry + 0x15).unwrap(), 2);
        let name: Vec<u8> = (0..11)
            .map(|i| mem.read_u8(entry + 0x20 + i).unwrap())
            .collect();
        assert_eq!(&name, b"MEM     TXT");
    }

    #[test]
    fn memory_file_handle_dup_and_dup2_share_cursor() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let handle = open_memory_handle(&mut kernel, b"abcdef");
        let mut dup = DosRegs {
            ax: 0x4500,
            bx: handle,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut dup, &mut mem).unwrap();
        assert!(!dup.cf);
        let dup_handle = dup.ax;

        assert_eq!(read(&mut kernel, &mut mem, handle, 2, 0x0400).ax, 2);
        let through_dup = read(&mut kernel, &mut mem, dup_handle, 2, 0x0410);
        assert!(!through_dup.cf);
        assert_eq!(through_dup.ax, 2);
        let base = 0x0100usize * 16 + 0x0410;
        let got: Vec<u8> = (0..2).map(|i| mem.read_u8(base + i).unwrap()).collect();
        assert_eq!(got, b"cd");

        let mut dup2 = DosRegs {
            ax: 0x4600,
            bx: handle,
            cx: 9,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut dup2, &mut mem).unwrap();
        assert!(!dup2.cf);
        let through_dup2 = read(&mut kernel, &mut mem, 9, 1, 0x0420);
        assert!(!through_dup2.cf);
        assert_eq!(through_dup2.ax, 1);
        let got = mem.read_u8(0x0100usize * 16 + 0x0420).unwrap();
        assert_eq!(got, b'e');
    }

    #[test]
    fn memory_file_handle_dup2_to_stdout_rejects_char_output() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let handle = open_memory_handle(&mut kernel, b"abcdef");
        let mut dup2 = DosRegs {
            ax: 0x4600,
            bx: handle,
            cx: 1,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut dup2, &mut mem).unwrap();
        assert!(!dup2.cf);

        let mut char_out = DosRegs {
            ax: 0x0200,
            dx: u16::from(b'X'),
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut char_out, &mut mem).unwrap();

        let base = 0x0100usize * 16 + 0x0500;
        for (index, &byte) in b"YZ$".iter().enumerate() {
            mem.write_u8(base + index, byte).unwrap();
        }
        let mut string_out = DosRegs {
            ax: 0x0900,
            ds: 0x0100,
            dx: 0x0500,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut string_out, &mut mem).unwrap();

        assert!(char_out.cf);
        assert_eq!(char_out.ax, 0x05);
        assert!(string_out.cf);
        assert_eq!(string_out.ax, 0x05);
        assert!(kernel.stdout().is_empty());
    }

    #[test]
    fn memory_file_handle_dup2_to_aux_reads_file_bytes() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let handle = open_memory_handle(&mut kernel, b"abcdef");
        let mut dup2 = DosRegs {
            ax: 0x4600,
            bx: handle,
            cx: 3,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut dup2, &mut mem).unwrap();
        assert!(!dup2.cf);

        let through_aux = read(&mut kernel, &mut mem, 3, 2, 0x0520);

        assert!(!through_aux.cf);
        assert_eq!(through_aux.ax, 2);
        let base = 0x0100usize * 16 + 0x0520;
        let got: Vec<u8> = (0..2).map(|i| mem.read_u8(base + i).unwrap()).collect();
        assert_eq!(got, b"ab");
    }

    #[test]
    fn memory_file_handle_dup2_to_aux_prn_rejects_writes() {
        for target in [3, 4] {
            let mut kernel = DosKernel::new();
            let mut mem = Memory::new(1024 * 1024).unwrap();
            let handle = open_memory_handle(&mut kernel, b"abcdef");
            let mut dup2 = DosRegs {
                ax: 0x4600,
                bx: handle,
                cx: target,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut dup2, &mut mem).unwrap();
            assert!(!dup2.cf);

            let src = 0x0100usize * 16 + 0x0540;
            mem.write_u8(src, b'X').unwrap();
            let mut write = DosRegs {
                ax: 0x4000,
                bx: target,
                cx: 1,
                ds: 0x0100,
                dx: 0x0540,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut write, &mut mem).unwrap();

            assert!(write.cf, "handle {target} rejects write");
            assert_eq!(write.ax, 0x05, "handle {target} returns access denied");
        }
    }

    #[test]
    fn memory_file_handle_ioctl_reports_regular_file() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let handle = open_memory_handle(&mut kernel, b"abcdef");
        let mut regs = DosRegs {
            ax: 0x4400,
            bx: handle,
            ..DosRegs::default()
        };

        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();

        assert!(!regs.cf);
        assert_eq!(regs.dx & 0x0080, 0, "memory file is not a device");
        assert_eq!(regs.dx, 0x0002);
    }

    #[test]
    fn memory_file_handle_ioctl_input_status_tracks_eof() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let handle = open_memory_handle(&mut kernel, b"abc");

        let mut before = DosRegs {
            ax: 0x4406,
            bx: handle,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut before, &mut mem).unwrap();
        assert!(!before.cf);
        assert_eq!(before.ax & 0x00ff, 0x00ff, "memory file starts ready");

        let drained = read(&mut kernel, &mut mem, handle, 3, 0x0440);
        assert!(!drained.cf);
        assert_eq!(drained.ax, 3);

        let mut after = DosRegs {
            ax: 0x4406,
            bx: handle,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut after, &mut mem).unwrap();
        assert!(!after.cf);
        assert_eq!(after.ax & 0x00ff, 0x0000, "memory file is not ready at EOF");
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
        assert_eq!(get.ax & 0xff, 6); // Saturday
        // Leap day is valid in 2000.
        let mut leap = DosRegs {
            ax: 0x2b00,
            cx: 2000,
            dx: (2u16 << 8) | 29,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut leap, &mut mem).unwrap();
        assert_eq!(leap.ax & 0xff, 0x00);
        // Reject month 13.
        let mut bad = DosRegs {
            ax: 0x2b00,
            cx: 2001,
            dx: (13u16 << 8) | 3,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut bad, &mut mem).unwrap();
        assert_eq!(bad.ax & 0xff, 0xff); // failure, clock unchanged
        // Reject impossible month/day combinations and non-leap Feb 29.
        for &(month, day, year) in &[(2, 31, 2001), (4, 31, 2001), (2, 29, 2001)] {
            let mut invalid = DosRegs {
                ax: 0x2b00,
                cx: year,
                dx: (month << 8) | day,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut invalid, &mut mem).unwrap();
            assert_eq!(invalid.ax & 0xff, 0xff, "{year:04}-{month:02}-{day:02}");
        }
        let mut get2 = DosRegs {
            ax: 0x2a00,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get2, &mut mem).unwrap();
        assert_eq!(get2.cx, 2000);
        assert_eq!(get2.dx >> 8, 2);
        assert_eq!(get2.dx & 0xff, 29);
        assert_eq!(get2.ax & 0xff, 2); // Tuesday
    }

    #[test]
    fn ah38_returns_us_country_info() {
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut kernel = DosKernel::new();
        let mut regs = DosRegs {
            ax: 0x3800,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };

        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();

        assert!(!regs.cf);
        assert_eq!(regs.bx, 1);
        assert_eq!(regs.ax, 1);
        let base = 0x0100usize * 16 + 0x0200;
        assert_eq!(mem.read_u16(base).unwrap(), 0); // USA mm/dd/yy
        assert_eq!(mem.read_u8(base + 2).unwrap(), b'$');
        assert_eq!(mem.read_u8(base + 7).unwrap(), b',');
        assert_eq!(mem.read_u8(base + 9).unwrap(), b'.');
        assert_eq!(mem.read_u8(base + 0x0b).unwrap(), b'/');
        assert_eq!(mem.read_u8(base + 0x0d).unwrap(), b':');
        assert_eq!(mem.read_u8(base + 0x10).unwrap(), 2);
        assert_eq!(mem.read_u8(base + 0x16).unwrap(), b',');

        let sentinel = 0x0100usize * 16 + 0x00ff;
        mem.write_u8(sentinel, 0x5a).unwrap();
        let mut set_us = DosRegs {
            ax: 0x3801,
            bx: 0xffff,
            ds: 0x0100,
            dx: 0xffff,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set_us, &mut mem).unwrap();
        assert!(!set_us.cf);
        assert_eq!(set_us.ax, DOS_COUNTRY_US);
        assert_eq!(set_us.bx, DOS_COUNTRY_US);
        assert_eq!(
            mem.read_u8(sentinel).unwrap(),
            0x5a,
            "set-country does not write a country-info buffer"
        );

        let mut set_unknown = DosRegs {
            ax: 0x382c,
            dx: 0xffff,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set_unknown, &mut mem).unwrap();
        assert!(set_unknown.cf);
        assert_eq!(set_unknown.ax, 0x0002);
    }

    #[test]
    fn ah61_ah63_and_ah64_return_unused_dbcs_and_lookahead_state() {
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut kernel = DosKernel::new();

        let mut unused = DosRegs {
            ax: 0x61ff,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut unused, &mut mem).unwrap();
        assert_eq!(unused.ax & 0x00ff, 0);

        let mut table = DosRegs {
            ax: 0x6300,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut table, &mut mem).unwrap();
        assert!(!table.cf);
        let table_base = usize::from(table.ds) * 16 + usize::from(table.si);
        assert_eq!(
            mem.read_u16(table_base).unwrap(),
            0,
            "CP437 has no DBCS lead ranges"
        );

        let mut set = DosRegs {
            ax: 0x6301,
            dx: 1,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set, &mut mem).unwrap();
        let mut get = DosRegs {
            ax: 0x6302,
            dx: 0x1200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get, &mut mem).unwrap();
        assert_eq!(get.dx, 0x1201);

        let mut invalid = DosRegs {
            ax: 0x6303,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut invalid, &mut mem).unwrap();
        assert!(invalid.cf);
        assert_eq!(invalid.ax, 0x01);

        let mut lookahead = DosRegs {
            ax: 0x6401,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut lookahead, &mut mem).unwrap();
        assert!(!lookahead.cf);
    }

    #[test]
    fn ah65_returns_country_info_and_capitalizes_ascii() {
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut kernel = DosKernel::new();
        kernel.init_program(0x0100, 0x0200, &mut mem).unwrap();
        let mut info = DosRegs {
            ax: 0x6501,
            bx: 0xffff,
            cx: 64,
            dx: 0xffff,
            es: 0x0100,
            di: 0x0300,
            ..DosRegs::default()
        };

        kernel.dispatch(0x21, &mut info, &mut mem).unwrap();

        assert!(!info.cf);
        assert_eq!(info.cx, 41);
        let info_base = 0x0100usize * 16 + 0x0300;
        assert_eq!(mem.read_u8(info_base).unwrap(), 0x01);
        assert_eq!(mem.read_u16(info_base + 1).unwrap(), 38);
        assert_eq!(mem.read_u16(info_base + 3).unwrap(), 1);
        assert_eq!(mem.read_u16(info_base + 5).unwrap(), 437);
        assert_eq!(mem.read_u16(info_base + 7).unwrap(), 0);
        assert_eq!(mem.read_u8(info_base + 9).unwrap(), b'$');

        let mut dbcs = DosRegs {
            ax: 0x6507,
            bx: 0xffff,
            cx: 5,
            dx: 0xffff,
            es: 0x0100,
            di: 0x0340,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut dbcs, &mut mem).unwrap();
        assert!(!dbcs.cf);
        assert_eq!(dbcs.cx, 5);
        let dbcs_info = 0x0100usize * 16 + 0x0340;
        let (dbcs_seg, dbcs_off) = DBCS_LEAD_BYTE_TABLE_PTR;
        assert_eq!(mem.read_u8(dbcs_info).unwrap(), 0x07);
        assert_eq!(mem.read_u16(dbcs_info + 1).unwrap(), dbcs_off);
        assert_eq!(mem.read_u16(dbcs_info + 3).unwrap(), dbcs_seg);
        let dbcs_table = usize::from(dbcs_seg) * 16 + usize::from(dbcs_off);
        assert_eq!(mem.read_u16(dbcs_table).unwrap(), 0);

        let pointer_info = [
            (0x6502u16, 0x80u16, 0x02usize, 0x80u8),
            (0x6503, 0x100, 0x02 + usize::from(b'A'), b'a'),
            (0x6504, 0x80, 0x02, 0x80),
            (0x6506, 0x100, 0x02 + usize::from(b'A'), b'A'),
        ];
        for (index, (ax, table_len, sample_off, sample)) in pointer_info.iter().enumerate() {
            let mut regs = DosRegs {
                ax: *ax,
                bx: 0xffff,
                cx: 5,
                dx: 0xffff,
                es: 0x0100,
                di: 0x0360 + index as u16 * 8,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
            assert!(!regs.cf, "AX={ax:04X}h succeeds");
            assert_eq!(regs.cx, 5);
            let record = 0x0100usize * 16 + usize::from(regs.di);
            assert_eq!(mem.read_u8(record).unwrap(), *ax as u8);
            let table_off = mem.read_u16(record + 1).unwrap();
            let table_seg = mem.read_u16(record + 3).unwrap();
            let table = usize::from(table_seg) * 16 + usize::from(table_off);
            assert_eq!(mem.read_u16(table).unwrap(), *table_len);
            assert_eq!(mem.read_u8(table + sample_off).unwrap(), *sample);
        }

        let mut terminators = DosRegs {
            ax: 0x6505,
            bx: 0xffff,
            cx: 5,
            dx: 0xffff,
            es: 0x0100,
            di: 0x0388,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut terminators, &mut mem).unwrap();
        assert!(!terminators.cf);
        let term_record = 0x0100usize * 16 + 0x0388;
        let term_off = mem.read_u16(term_record + 1).unwrap();
        let term_seg = mem.read_u16(term_record + 3).unwrap();
        let term_table = usize::from(term_seg) * 16 + usize::from(term_off);
        assert_eq!(mem.read_u16(term_table).unwrap(), 22);
        assert_eq!(mem.read_u8(term_table + 0x09).unwrap(), 14);
        assert_eq!(mem.read_u8(term_table + 0x0a).unwrap(), b'.');
        assert_eq!(mem.read_u8(term_table + 0x0d).unwrap(), b'\\');

        let mut ch = DosRegs {
            ax: 0x6520,
            dx: 0x1200 | u16::from(b'a'),
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut ch, &mut mem).unwrap();
        assert!(!ch.cf);
        assert_eq!(ch.dx, 0x1200 | u16::from(b'A'));

        let counted = 0x0400usize;
        for (i, byte) in b"aZ9".iter().enumerate() {
            mem.write_u8(0x0100usize * 16 + counted + i, *byte).unwrap();
        }
        let mut string = DosRegs {
            ax: 0x6521,
            cx: 3,
            ds: 0x0100,
            dx: counted as u16,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut string, &mut mem).unwrap();
        assert!(!string.cf);
        assert_eq!(mem.read_u8(0x0100usize * 16 + counted).unwrap(), b'A');
        assert_eq!(mem.read_u8(0x0100usize * 16 + counted + 1).unwrap(), b'Z');
        assert_eq!(mem.read_u8(0x0100usize * 16 + counted + 2).unwrap(), b'9');

        let asciiz = 0x0410usize;
        for (i, byte) in b"dos\0".iter().enumerate() {
            mem.write_u8(0x0100usize * 16 + asciiz + i, *byte).unwrap();
        }
        let mut zstr = DosRegs {
            ax: 0x6522,
            ds: 0x0100,
            dx: asciiz as u16,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut zstr, &mut mem).unwrap();
        assert!(!zstr.cf);
        assert_eq!(mem.read_u8(0x0100usize * 16 + asciiz).unwrap(), b'D');
        assert_eq!(mem.read_u8(0x0100usize * 16 + asciiz + 1).unwrap(), b'O');
        assert_eq!(mem.read_u8(0x0100usize * 16 + asciiz + 2).unwrap(), b'S');
        assert_eq!(mem.read_u8(0x0100usize * 16 + asciiz + 3).unwrap(), 0);

        for (byte, class) in [(b'N', 0), (b'y', 1), (b'?', 2)] {
            let mut yes_no = DosRegs {
                ax: 0x6523,
                dx: u16::from(byte),
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut yes_no, &mut mem).unwrap();
            assert!(!yes_no.cf);
            assert_eq!(yes_no.ax, class);
        }

        let mut filename_ch = DosRegs {
            ax: 0x65a0,
            dx: u16::from(b'z'),
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut filename_ch, &mut mem).unwrap();
        assert!(!filename_ch.cf);
        assert_eq!(filename_ch.dx, u16::from(b'Z'));

        for (i, byte) in b"file\0".iter().enumerate() {
            mem.write_u8(0x0100usize * 16 + 0x0420 + i, *byte).unwrap();
        }
        let mut filename_zstr = DosRegs {
            ax: 0x65a2,
            ds: 0x0100,
            dx: 0x0420,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut filename_zstr, &mut mem).unwrap();
        assert!(!filename_zstr.cf);
        assert_eq!(mem.read_u8(0x0100usize * 16 + 0x0420).unwrap(), b'F');
        assert_eq!(mem.read_u8(0x0100usize * 16 + 0x0423).unwrap(), b'E');
    }

    #[test]
    fn ah66_gets_and_accepts_cp437() {
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut kernel = DosKernel::new();
        let mut get = DosRegs {
            ax: 0x6601,
            ..DosRegs::default()
        };

        kernel.dispatch(0x21, &mut get, &mut mem).unwrap();

        assert!(!get.cf);
        assert_eq!(get.bx, 437);
        assert_eq!(get.dx, 437);

        let mut set = DosRegs {
            ax: 0x6602,
            bx: 437,
            dx: 437,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set, &mut mem).unwrap();
        assert!(!set.cf);
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
    fn ah48_corrupt_mcb_chain_returns_arena_destroyed() {
        let (mut kernel, mut mem) = arena_kernel();
        // DOS distinguishes a damaged MCB chain from an ordinary allocation miss:
        // error 07h means memory control blocks destroyed, while 08h is merely not
        // enough memory. Corrupt the live first header signature the allocator walks.
        mem.write_u8(0x00ff * 16, b'X').unwrap();
        let mut regs = DosRegs {
            ax: 0x4800,
            bx: 0x0010,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x07);

        let mut err = DosRegs {
            ax: 0x5900,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut err, &mut mem).unwrap();
        assert_eq!(err.ax, 0x07);
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
    fn ah4a_corrupt_mcb_chain_returns_arena_destroyed() {
        let (mut kernel, mut mem) = arena_kernel();
        mem.write_u8(0x00ff * 16, b'X').unwrap();
        let mut regs = DosRegs {
            ax: 0x4a00,
            es: 0x0100,
            bx: 0x0800,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x07);
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
        // Shrinking a non-top AH=48h block creates a free MCB in the gap and must
        // leave the owned block above it a valid, freeable block.
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

        let c = dos_alloc(&mut kernel, &mut mem, 0x0008);
        assert!(!c.cf);
        assert_eq!(c.ax, 0x110a, "the shrink-created gap is allocatable");

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
    fn ah4a_grows_into_a_freed_successor() {
        let (mut kernel, mut mem) = arena_kernel();
        let a = dos_alloc(&mut kernel, &mut mem, 0x0010);
        let b = dos_alloc(&mut kernel, &mut mem, 0x0010);
        let c = dos_alloc(&mut kernel, &mut mem, 0x0010);
        assert_eq!((a.ax, b.ax, c.ax), (0x1101, 0x1112, 0x1123));

        let mut free_b = DosRegs {
            ax: 0x4900,
            es: b.ax,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut free_b, &mut mem).unwrap();
        assert!(!free_b.cf);

        let mut grow_a = DosRegs {
            ax: 0x4a00,
            es: a.ax,
            bx: 0x0021,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut grow_a, &mut mem).unwrap();
        assert!(!grow_a.cf, "A grows through B's freed MCB up to C");

        let mut q = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut q, &mut mem).unwrap();
        let first = mem
            .read_u16(usize::from(q.es) * 16 + usize::from(q.bx) - 2)
            .unwrap();
        let chain = read_mcb_chain(&mem, first);
        assert!(
            chain.iter().any(|m| m.owner == c.ax),
            "the owned block above the grown span survives"
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
    fn ah49_non_top_free_is_reused_by_first_fit() {
        let (mut kernel, mut mem) = arena_kernel();
        let a = dos_alloc(&mut kernel, &mut mem, 0x0010);
        assert_eq!(a.ax, 0x1101);
        let b = dos_alloc(&mut kernel, &mut mem, 0x0010);
        assert_eq!(b.ax, 0x1112);

        let mut free_a = DosRegs {
            ax: 0x4900,
            es: a.ax,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut free_a, &mut mem).unwrap();
        assert!(!free_a.cf);

        let c = dos_alloc(&mut kernel, &mut mem, 0x0008);
        assert!(!c.cf);
        assert_eq!(
            c.ax, a.ax,
            "first-fit should reuse the lower free MCB before the tail"
        );
    }

    #[test]
    fn ah49_adjacent_free_blocks_coalesce_before_allocation() {
        let (mut kernel, mut mem) = arena_kernel();
        let a = dos_alloc(&mut kernel, &mut mem, 0x0010);
        let b = dos_alloc(&mut kernel, &mut mem, 0x0010);
        let c = dos_alloc(&mut kernel, &mut mem, 0x0010);
        assert_eq!((a.ax, b.ax, c.ax), (0x1101, 0x1112, 0x1123));

        for seg in [a.ax, b.ax] {
            let mut free = DosRegs {
                ax: 0x4900,
                es: seg,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut free, &mut mem).unwrap();
            assert!(!free.cf);
        }

        let d = dos_alloc(&mut kernel, &mut mem, 0x0020);
        assert!(!d.cf);
        assert_eq!(
            d.ax, a.ax,
            "adjacent free blocks should coalesce into a reusable span"
        );
    }

    #[test]
    fn ah58_last_fit_allocates_from_the_high_end_of_a_free_block() {
        let (mut kernel, mut mem) = arena_kernel();
        set_alloc_strategy(&mut kernel, &mut mem, 0x0002);

        let regs = dos_alloc(&mut kernel, &mut mem, 0x0010);
        assert!(!regs.cf);
        assert_eq!(
            regs.ax, 0x9ff0,
            "last-fit splits the highest suitable free block from its high end"
        );
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
    fn ah49_corrupt_mcb_chain_returns_arena_destroyed() {
        let (mut kernel, mut mem) = arena_kernel();
        mem.write_u8(0x00ff * 16, b'X').unwrap();
        let mut regs = DosRegs {
            ax: 0x4900,
            es: 0x1101,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x07);
    }

    #[test]
    fn ah49_non_top_free_then_top_free_coalesces_and_reuses_from_low_end() {
        // Free a lower block, then the block above it. The free-list contract should
        // coalesce those adjacent MCBs and let the next first-fit allocation reuse
        // the low block instead of leaking it.
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
        // A fresh allocation reuses the coalesced A+B span from its low end.
        let mut c = DosRegs {
            ax: 0x4800,
            bx: 0x0008,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut c, &mut mem).unwrap();
        assert_eq!(c.ax, 0x1101);
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
        let clock_off = mem.read_u16(base + 0x08).unwrap();
        let clock_seg = mem.read_u16(base + 0x0a).unwrap();
        let con_off = mem.read_u16(base + 0x0c).unwrap();
        let con_seg = mem.read_u16(base + 0x0e).unwrap();
        assert_ne!((clock_seg, clock_off), (0, 0), "CLOCK$ pointer");
        assert_ne!((con_seg, con_off), (0, 0), "CON pointer");

        assert_eq!(mem.read_u16(nul).unwrap(), con_off, "NUL links to CON");
        assert_eq!(
            mem.read_u16(nul + 2).unwrap(),
            con_seg,
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

        let con = usize::from(con_seg) * 16 + usize::from(con_off);
        assert_eq!(mem.read_u16(con).unwrap(), clock_off, "CON links to CLOCK$");
        assert_eq!(
            mem.read_u16(con + 2).unwrap(),
            clock_seg,
            "CON next link segment"
        );
        assert_eq!(
            mem.read_u16(con + 4).unwrap(),
            0x8013,
            "CON attribute: char + special + stdin + stdout"
        );
        let con_name: Vec<u8> = (0..8)
            .map(|i| mem.read_u8(con + 0x0a + i).unwrap())
            .collect();
        assert_eq!(&con_name, b"CON     ", "CON device name");

        let clock = usize::from(clock_seg) * 16 + usize::from(clock_off);
        assert_eq!(
            mem.read_u16(clock).unwrap(),
            0xffff,
            "CLOCK$ terminates chain"
        );
        assert_eq!(
            mem.read_u16(clock + 2).unwrap(),
            0xffff,
            "CLOCK$ next link segment"
        );
        assert_eq!(
            mem.read_u16(clock + 4).unwrap(),
            0x8008,
            "CLOCK$ attribute: char + clock"
        );
        let clock_name: Vec<u8> = (0..8)
            .map(|i| mem.read_u8(clock + 0x0a + i).unwrap())
            .collect();
        assert_eq!(&clock_name, b"CLOCK$  ", "CLOCK$ device name");

        assert_eq!(
            mem.read_u8(base + 0x20).unwrap(),
            1,
            "one block device is installed"
        );
        assert_ne!(
            (mem.read_u16(base + 2).unwrap(), mem.read_u16(base).unwrap()),
            (0, 0),
            "[BX+0x00] points at the first DPB"
        );
    }

    #[test]
    fn ah52_keeps_emmxxxx0_between_nul_and_standard_devices() {
        let (mut kernel, mut mem) = arena_kernel();
        kernel.set_ems_present(true);
        let mut regs = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        let base = usize::from(regs.es) * 16 + usize::from(regs.bx);

        let nul = base + 0x22;
        let ems_off = mem.read_u16(nul).unwrap();
        let ems_seg = mem.read_u16(nul + 2).unwrap();
        let con_off = mem.read_u16(base + 0x0c).unwrap();
        let con_seg = mem.read_u16(base + 0x0e).unwrap();
        assert_ne!(
            (ems_seg, ems_off),
            (0xffff, 0xffff),
            "NUL links to EMMXXXX0"
        );

        let ems = usize::from(ems_seg) * 16 + usize::from(ems_off);
        let ems_name: Vec<u8> = (0..8)
            .map(|i| mem.read_u8(ems + 0x0a + i).unwrap())
            .collect();
        assert_eq!(&ems_name, b"EMMXXXX0", "EMMXXXX0 stays after NUL");
        assert_eq!(
            mem.read_u16(ems + 4).unwrap(),
            0xc000,
            "EMMXXXX0 attributes"
        );
        assert_eq!(mem.read_u16(ems).unwrap(), con_off, "EMMXXXX0 links to CON");
        assert_eq!(
            mem.read_u16(ems + 2).unwrap(),
            con_seg,
            "EMMXXXX0 next link segment"
        );
    }

    #[test]
    fn ah52_publishes_a_c_drive_dpb_chain() {
        let (mut kernel, mut mem) = arena_kernel();
        let mut regs = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        let base = usize::from(regs.es) * 16 + usize::from(regs.bx);

        let dpb_off = mem.read_u16(base).unwrap();
        let dpb_seg = mem.read_u16(base + 2).unwrap();
        assert_ne!(
            (dpb_seg, dpb_off),
            (0, 0),
            "[BX+0x00] points at the first DPB"
        );
        let dpb = usize::from(dpb_seg) * 16 + usize::from(dpb_off);
        assert_eq!(mem.read_u8(dpb).unwrap(), 2, "DPB drive number is C:");
        assert_eq!(
            mem.read_u8(dpb + 0x01).unwrap(),
            0,
            "first block-device unit"
        );
        assert_eq!(mem.read_u16(dpb + 0x02).unwrap(), 512, "bytes per sector");
        assert_eq!(
            mem.read_u8(dpb + 0x04).unwrap(),
            63,
            "64 sectors per cluster minus one"
        );
        assert_eq!(mem.read_u8(dpb + 0x05).unwrap(), 6, "cluster shift");
        assert_eq!(mem.read_u16(dpb + 0x0f).unwrap(), 256, "sectors per FAT");
        assert_eq!(
            mem.read_u16(dpb + 0x11).unwrap(),
            513,
            "first root directory sector"
        );
        assert_eq!(mem.read_u8(dpb + 0x17).unwrap(), 0xf8, "fixed disk media");
        assert_eq!(mem.read_u8(dpb + 0x18).unwrap(), 0, "disk accessed flag");
        assert_eq!(
            (
                mem.read_u16(dpb + 0x1b).unwrap(),
                mem.read_u16(dpb + 0x19).unwrap()
            ),
            (0xffff, 0xffff),
            "the single DPB terminates the chain"
        );
        assert_eq!(
            mem.read_u16(dpb + 0x1f).unwrap(),
            0xf000,
            "free clusters match AH=36h"
        );
    }

    #[test]
    fn ah1b_and_ah1c_return_allocation_table_info() {
        let (mut kernel, mut mem) = arena_kernel();

        for (ax, dx) in [(0x1b00, 0), (0x1c00, 3)] {
            let mut regs = DosRegs {
                ax,
                dx,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
            assert_eq!(regs.ax & 0x00ff, C_DRIVE_SECTORS_PER_CLUSTER);
            assert_eq!(regs.cx, C_DRIVE_BYTES_PER_SECTOR);
            assert_eq!(regs.dx, C_DRIVE_TOTAL_CLUSTERS);

            let media = usize::from(regs.ds) * 16 + usize::from(regs.bx);
            assert_eq!(
                mem.read_u8(media).unwrap(),
                0xf8,
                "DS:BX points at the FAT ID byte"
            );
        }

        let mut bad = DosRegs {
            ax: 0x1c00,
            dx: 1,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut bad, &mut mem).unwrap();
        assert_eq!(bad.ax & 0x00ff, 0xff, "A: has no DPB");
    }

    #[test]
    fn ah1f_and_ah32_return_drive_parameter_blocks() {
        let (mut kernel, mut mem) = arena_kernel();
        let mut default_dpb = DosRegs {
            ax: 0x1f00,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut default_dpb, &mut mem).unwrap();
        assert_eq!(default_dpb.ax & 0x00ff, 0);
        let dpb = usize::from(default_dpb.ds) * 16 + usize::from(default_dpb.bx);
        assert_eq!(mem.read_u8(dpb).unwrap(), 2, "default DPB is C:");
        assert_eq!(mem.read_u16(dpb + 0x02).unwrap(), C_DRIVE_BYTES_PER_SECTOR);

        let mut explicit_c = DosRegs {
            ax: 0x3200,
            dx: 3,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut explicit_c, &mut mem).unwrap();
        assert_eq!(explicit_c.ax & 0x00ff, 0);
        assert_eq!(
            (explicit_c.ds, explicit_c.bx),
            (default_dpb.ds, default_dpb.bx),
            "DL=3 returns the same C: DPB"
        );

        let mut bad = DosRegs {
            ax: 0x3200,
            dx: 1,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut bad, &mut mem).unwrap();
        assert_eq!(bad.ax & 0x00ff, 0xff, "A: has no DPB");
    }

    #[test]
    fn ah53_translates_bpb_to_dpb() {
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut kernel = DosKernel::new();
        let bpb = 0x0100usize * 16 + 0x0200;
        let dpb = 0x0200usize * 16 + 0x0100;
        for (index, byte) in tests_bpb_custom(512, 1, 2880, 0xf0).iter().enumerate() {
            mem.write_u8(bpb + index, *byte).unwrap();
        }
        mem.write_u8(dpb, 3).unwrap();
        mem.write_u8(dpb + 0x01, 7).unwrap();
        mem.write_u16(dpb + 0x13, 0xaaaa).unwrap();
        mem.write_u16(dpb + 0x15, 0xbbbb).unwrap();
        mem.write_u8(dpb + 0x18, 0xcc).unwrap();
        mem.write_u16(dpb + 0x19, 0xdddd).unwrap();
        mem.write_u16(dpb + 0x1b, 0xeeee).unwrap();

        let mut regs = DosRegs {
            ax: 0x5300,
            ds: 0x0100,
            si: 0x0200,
            es: 0x0200,
            bp: 0x0100,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();

        assert!(!regs.cf);
        assert_eq!(mem.read_u8(dpb).unwrap(), 3, "drive byte preserved");
        assert_eq!(mem.read_u8(dpb + 0x01).unwrap(), 7, "unit preserved");
        assert_eq!(mem.read_u16(dpb + 0x02).unwrap(), 512);
        assert_eq!(mem.read_u8(dpb + 0x04).unwrap(), 0);
        assert_eq!(mem.read_u8(dpb + 0x05).unwrap(), 0);
        assert_eq!(mem.read_u16(dpb + 0x06).unwrap(), 1);
        assert_eq!(mem.read_u8(dpb + 0x08).unwrap(), 2);
        assert_eq!(mem.read_u16(dpb + 0x09).unwrap(), 224);
        assert_eq!(mem.read_u16(dpb + 0x0b).unwrap(), 33);
        assert_eq!(mem.read_u16(dpb + 0x0d).unwrap(), 2848);
        assert_eq!(mem.read_u16(dpb + 0x0f).unwrap(), 9);
        assert_eq!(mem.read_u16(dpb + 0x11).unwrap(), 19);
        assert_eq!(mem.read_u16(dpb + 0x13).unwrap(), 0xaaaa);
        assert_eq!(mem.read_u16(dpb + 0x15).unwrap(), 0xbbbb);
        assert_eq!(mem.read_u8(dpb + 0x17).unwrap(), 0xf0);
        assert_eq!(mem.read_u8(dpb + 0x18).unwrap(), 0xcc);
        assert_eq!(mem.read_u16(dpb + 0x19).unwrap(), 0xdddd);
        assert_eq!(mem.read_u16(dpb + 0x1b).unwrap(), 0xeeee);
        assert_eq!(mem.read_u16(dpb + 0x1d).unwrap(), 0);
        assert_eq!(mem.read_u16(dpb + 0x1f).unwrap(), 0xffff);
    }

    #[test]
    fn ah69_gets_and_sets_c_drive_serial_info() {
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut kernel = DosKernel::new();
        let base = 0x0100usize * 16 + 0x0200;

        let mut get_default = DosRegs {
            ax: 0x6900,
            bx: 0x0000,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get_default, &mut mem).unwrap();
        assert!(!get_default.cf);
        assert_eq!(mem.read_u16(base).unwrap(), 0);
        assert_eq!(mem.read_u32(base + 2).unwrap(), 0);
        for (index, byte) in C_DRIVE_VOLUME_LABEL.into_iter().enumerate() {
            assert_eq!(mem.read_u8(base + 6 + index).unwrap(), byte);
        }
        for (index, byte) in C_DRIVE_FS_TYPE.into_iter().enumerate() {
            assert_eq!(mem.read_u8(base + 17 + index).unwrap(), byte);
        }

        mem.write_u16(base, 0).unwrap();
        mem.write_u32(base + 2, 0x1234_5678).unwrap();
        for (index, byte) in (*b"TOKADOS    ").into_iter().enumerate() {
            mem.write_u8(base + 6 + index, byte).unwrap();
        }
        let mut set = DosRegs {
            ax: 0x6901,
            bx: 0x0003,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set, &mut mem).unwrap();
        assert!(!set.cf);

        let mut get = DosRegs {
            ax: 0x6900,
            bx: 0x0003,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get, &mut mem).unwrap();
        let out = 0x0100usize * 16 + 0x0300;
        assert_eq!(mem.read_u32(out + 2).unwrap(), 0x1234_5678);
        for (index, byte) in (*b"TOKADOS    ").into_iter().enumerate() {
            assert_eq!(mem.read_u8(out + 6 + index).unwrap(), byte);
        }

        let mut bad_drive = DosRegs {
            ax: 0x6900,
            bx: 0x0001,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut bad_drive, &mut mem).unwrap();
        assert!(bad_drive.cf);
        assert_eq!(bad_drive.ax, 0x0f);
    }

    #[test]
    fn ioctl_440d_gets_and_sets_c_drive_media_id() {
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut kernel = DosKernel::new();
        let base = 0x0100usize * 16 + 0x0200;

        mem.write_u16(base, 0).unwrap();
        mem.write_u32(base + 2, 0x8765_4321).unwrap();
        for (index, byte) in (*b"GENIOCTL  ").into_iter().enumerate() {
            mem.write_u8(base + 6 + index, byte).unwrap();
        }

        let mut set = DosRegs {
            ax: 0x440d,
            bx: 0x0003,
            cx: 0x0846,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set, &mut mem).unwrap();
        assert!(!set.cf);

        let mut get = DosRegs {
            ax: 0x440d,
            bx: 0x0000,
            cx: 0x0866,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get, &mut mem).unwrap();
        let out = 0x0100usize * 16 + 0x0300;
        assert!(!get.cf);
        assert_eq!(mem.read_u16(out).unwrap(), 0);
        assert_eq!(mem.read_u32(out + 2).unwrap(), 0x8765_4321);
        for (index, byte) in (*b"GENIOCTL  ").into_iter().enumerate() {
            assert_eq!(mem.read_u8(out + 6 + index).unwrap(), byte);
        }
        for (index, byte) in C_DRIVE_FS_TYPE.into_iter().enumerate() {
            assert_eq!(mem.read_u8(out + 17 + index).unwrap(), byte);
        }

        mem.write_u8(base, 0).unwrap();
        mem.write_u8(base + 1, 0).unwrap();
        let mut set_access = DosRegs {
            ax: 0x440d,
            bx: 0x0003,
            cx: 0x0847,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set_access, &mut mem).unwrap();
        assert!(!set_access.cf);

        let mut get_access = DosRegs {
            ax: 0x440d,
            bx: 0x0003,
            cx: 0x0867,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get_access, &mut mem).unwrap();
        assert!(!get_access.cf);
        assert_eq!(mem.read_u8(out).unwrap(), 0);
        assert_eq!(mem.read_u8(out + 1).unwrap(), 0);

        let mut supported = DosRegs {
            ax: 0x4411,
            bx: 0x0003,
            cx: 0x0867,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut supported, &mut mem).unwrap();
        assert!(!supported.cf);
        assert_eq!(supported.ax, 0);

        let mut unsupported = DosRegs {
            ax: 0x4411,
            bx: 0x0003,
            cx: 0x0868,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut unsupported, &mut mem).unwrap();
        assert!(unsupported.cf);
        assert_eq!(unsupported.ax, 0x01);
    }

    #[test]
    fn late_dos_probe_calls_report_null_or_not_supported() {
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut kernel = DosKernel::new();

        for ax in [0x6bff, 0x6dff, 0x6eff, 0x6fff] {
            let mut regs = DosRegs {
                ax,
                cf: true,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
            assert_eq!(regs.ax & 0x00ff, 0, "AX={ax:04x} returns AL=0");
        }

        for (ax, error) in [(0x7000, 0x7000), (0x7160, 0x7100)] {
            let mut regs = DosRegs {
                ax,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
            assert!(regs.cf, "AX={ax:04x} must fail on DOS 6.22");
            assert_eq!(regs.ax, error);
        }
    }

    #[test]
    fn ah52_publishes_loaded_block_driver_dpb_and_cds_from_bpb() {
        let (mut kernel, mut mem) = arena_kernel();
        let image = tests_block_image_with_bpb(0x30);
        let staged = kernel
            .stage_sys_driver(&image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        complete_block_init_with_bpb(&mut mem, &staged, 0x30);
        kernel.finalize_sys_driver(&staged, &mut mem).unwrap();

        let mut regs = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        let base = usize::from(regs.es) * 16 + usize::from(regs.bx);

        assert_eq!(
            mem.read_u8(base + 0x20).unwrap(),
            2,
            "C: plus loaded D: block unit should be counted"
        );
        let c_dpb_off = mem.read_u16(base).unwrap();
        let c_dpb_seg = mem.read_u16(base + 2).unwrap();
        let c_dpb = usize::from(c_dpb_seg) * 16 + usize::from(c_dpb_off);
        let d_dpb_off = mem.read_u16(c_dpb + 0x19).unwrap();
        let d_dpb_seg = mem.read_u16(c_dpb + 0x1b).unwrap();
        let d_dpb = usize::from(d_dpb_seg) * 16 + usize::from(d_dpb_off);

        assert_eq!(mem.read_u8(d_dpb).unwrap(), 3, "D: DPB drive number");
        assert_eq!(mem.read_u8(d_dpb + 0x01).unwrap(), 0, "driver unit 0");
        assert_eq!(mem.read_u16(d_dpb + 0x02).unwrap(), 512, "bytes per sector");
        assert_eq!(mem.read_u8(d_dpb + 0x04).unwrap(), 0, "cluster mask");
        assert_eq!(mem.read_u8(d_dpb + 0x05).unwrap(), 0, "cluster shift");
        assert_eq!(mem.read_u16(d_dpb + 0x06).unwrap(), 1, "first FAT sector");
        assert_eq!(mem.read_u8(d_dpb + 0x08).unwrap(), 2, "FAT count");
        assert_eq!(mem.read_u16(d_dpb + 0x09).unwrap(), 224, "root entries");
        assert_eq!(mem.read_u16(d_dpb + 0x0b).unwrap(), 33, "first data sector");
        assert_eq!(mem.read_u16(d_dpb + 0x0d).unwrap(), 2848, "highest cluster");
        assert_eq!(mem.read_u16(d_dpb + 0x0f).unwrap(), 9, "sectors per FAT");
        assert_eq!(mem.read_u16(d_dpb + 0x11).unwrap(), 19, "first root sector");
        assert_eq!(
            mem.read_u16(d_dpb + 0x13).unwrap(),
            0,
            "driver header offset"
        );
        assert_eq!(
            mem.read_u16(d_dpb + 0x15).unwrap(),
            staged.driver_seg,
            "driver header segment"
        );
        assert_eq!(mem.read_u8(d_dpb + 0x17).unwrap(), 0xf0, "media byte");
        assert_eq!(
            (
                mem.read_u16(d_dpb + 0x1b).unwrap(),
                mem.read_u16(d_dpb + 0x19).unwrap()
            ),
            (0xffff, 0xffff),
            "D: terminates the DPB chain"
        );
        assert_eq!(
            mem.read_u16(d_dpb + 0x1f).unwrap(),
            0xffff,
            "free clusters unknown"
        );

        let cds_off = mem.read_u16(base + 0x16).unwrap();
        let cds_seg = mem.read_u16(base + 0x18).unwrap();
        let d_cds = usize::from(cds_seg) * 16 + usize::from(cds_off) + 3 * 0x58;
        assert_eq!(
            mem.read_u16(d_cds + 0x43).unwrap(),
            0x4000,
            "D: local physical CDS"
        );
        assert_eq!(
            mem.read_u16(d_cds + 0x45).unwrap(),
            d_dpb_off,
            "D: CDS DPB offset"
        );
        assert_eq!(
            mem.read_u16(d_cds + 0x47).unwrap(),
            d_dpb_seg,
            "D: CDS DPB segment"
        );
    }

    #[test]
    fn ah52_publishes_multi_unit_and_second_block_driver_letters() {
        let (mut kernel, mut mem) = arena_kernel();
        kernel.set_lastdrive(6); // A: through F:
        let first_image = tests_block_image_with_bpbs(&[
            (0x40, tests_bpb_custom(512, 1, 2880, 0xf0)),
            (0x70, tests_bpb_custom(512, 2, 2880, 0xf9)),
        ]);
        let first = kernel
            .stage_sys_driver(&first_image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        assert_eq!(first.first_drive, 3, "two-unit driver starts at D:");
        complete_block_init_with_offsets(&mut mem, &first, 2, 0x20, &[0x40, 0x70]);
        kernel.finalize_sys_driver(&first, &mut mem).unwrap();

        let second_image = tests_block_image_with_bpb(0x40);
        let second = kernel
            .stage_sys_driver(&second_image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        assert_eq!(second.first_drive, 5, "next driver skips D: and E:");
        complete_block_init_with_bpb(&mut mem, &second, 0x40);
        kernel.finalize_sys_driver(&second, &mut mem).unwrap();

        assert_eq!(
            ah52_dpb_drives(&mut kernel, &mut mem),
            vec![2, 3, 4, 5],
            "DPB chain follows C:, D:, E:, F:"
        );
    }

    #[test]
    fn bad_bpb_consumes_assigned_span_without_publishing_fake_dpb() {
        let (mut kernel, mut mem) = arena_kernel();
        let mut bad_image = tests_block_image();
        bad_image.resize(0x40, 0);
        let bad = kernel
            .stage_sys_driver(&bad_image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        let unrelated = usize::from(bad.driver_seg) * 16 + 0x9000;
        for (i, byte) in tests_bpb_1440k().iter().copied().enumerate() {
            mem.write_u8(unrelated + i, byte).unwrap();
        }
        complete_block_init_with_offsets(&mut mem, &bad, 1, 0x20, &[0x9000]);
        kernel.finalize_sys_driver(&bad, &mut mem).unwrap();
        assert_eq!(
            ah52_dpb_drives(&mut kernel, &mut mem),
            vec![2],
            "readable BPB bytes outside the staged allocation do not publish D:"
        );

        let good_image = tests_block_image_with_bpb(0x40);
        let good = kernel
            .stage_sys_driver(&good_image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        assert_eq!(good.first_drive, 4, "the bad D: unit still consumed D:");
        complete_block_init_with_bpb(&mut mem, &good, 0x40);
        kernel.finalize_sys_driver(&good, &mut mem).unwrap();
        assert_eq!(ah52_dpb_drives(&mut kernel, &mut mem), vec![2, 4]);
    }

    #[test]
    fn malformed_bpb_geometry_fails_closed() {
        for case in 0..5 {
            let (mut kernel, mut mem) = arena_kernel();
            let mut bpb = tests_bpb_1440k();
            match case {
                0 => bpb[0..2].copy_from_slice(&0u16.to_le_bytes()),
                1 => bpb[2] = 0,
                2 => bpb[2] = 3,
                3 => bpb[8..10].copy_from_slice(&0u16.to_le_bytes()),
                4 => bpb[8..10].copy_from_slice(&20u16.to_le_bytes()),
                _ => unreachable!(),
            }
            let image = tests_block_image_with_bpbs(&[(0x40, bpb)]);
            let bad = kernel
                .stage_sys_driver(&image, "", DriverLoadPlacement::Low, &mut mem)
                .unwrap();
            complete_block_init_with_bpb(&mut mem, &bad, 0x40);
            kernel.finalize_sys_driver(&bad, &mut mem).unwrap();
            assert_eq!(
                ah52_dpb_drives(&mut kernel, &mut mem),
                vec![2],
                "case {case} must not publish a fake D: DPB"
            );
            let good = kernel
                .stage_sys_driver(
                    &tests_block_image_with_bpb(0x40),
                    "",
                    DriverLoadPlacement::Low,
                    &mut mem,
                )
                .unwrap();
            assert_eq!(good.first_drive, 4, "case {case} still consumes D:");
        }
    }

    #[test]
    fn ah52_largest_bytes_per_block_tracks_published_bpbs() {
        let (mut kernel, mut mem) = arena_kernel();
        let base = ah52_sysvars_base(&mut kernel, &mut mem);
        assert_eq!(mem.read_u16(base + 0x10).unwrap(), 512);

        let big_image =
            tests_block_image_with_bpbs(&[(0x40, tests_bpb_custom(1024, 1, 1440, 0xf8))]);
        let big = kernel
            .stage_sys_driver(&big_image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        complete_block_init_with_bpb(&mut mem, &big, 0x40);
        kernel.finalize_sys_driver(&big, &mut mem).unwrap();
        let base = ah52_sysvars_base(&mut kernel, &mut mem);
        assert_eq!(mem.read_u16(base + 0x10).unwrap(), 1024);

        let mut invalid = tests_bpb_custom(2048, 1, 1440, 0xf8);
        invalid[8..10].copy_from_slice(&0u16.to_le_bytes());
        let invalid_image = tests_block_image_with_bpbs(&[(0x40, invalid)]);
        let staged = kernel
            .stage_sys_driver(&invalid_image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        complete_block_init_with_bpb(&mut mem, &staged, 0x40);
        kernel.finalize_sys_driver(&staged, &mut mem).unwrap();
        let base = ah52_sysvars_base(&mut kernel, &mut mem);
        assert_eq!(
            mem.read_u16(base + 0x10).unwrap(),
            1024,
            "invalid unpublished BPB does not raise the block size"
        );
    }

    #[test]
    fn ah52_clamps_loaded_block_driver_dpbs_to_lastdrive() {
        let (mut kernel, mut mem) = arena_kernel();
        let image = tests_block_image_with_bpbs(&[
            (0x40, tests_bpb_custom(512, 1, 2880, 0xf0)),
            (0x70, tests_bpb_custom(512, 1, 2880, 0xf9)),
        ]);
        let staged = kernel
            .stage_sys_driver(&image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        complete_block_init_with_offsets(&mut mem, &staged, 2, 0x20, &[0x40, 0x70]);
        kernel.finalize_sys_driver(&staged, &mut mem).unwrap();

        kernel.set_lastdrive(3); // C:
        assert_eq!(ah52_dpb_drives(&mut kernel, &mut mem), vec![2]);
        let base = ah52_sysvars_base(&mut kernel, &mut mem);
        assert_eq!(mem.read_u8(base + 0x20).unwrap(), 1);

        kernel.set_lastdrive(4); // D:
        assert_eq!(ah52_dpb_drives(&mut kernel, &mut mem), vec![2, 3]);
        kernel.set_lastdrive(5); // E:
        assert_eq!(ah52_dpb_drives(&mut kernel, &mut mem), vec![2, 3, 4]);
    }

    #[test]
    fn block_driver_unit_count_is_clipped_at_z_without_wrapping() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        kernel.init_program(0x0200, 0x1200, &mut mem).unwrap();
        kernel.set_lastdrive(26);
        let image = tests_block_image_with_bpbs(&[(0x60, tests_bpb_1440k())]);
        let staged = kernel
            .stage_sys_driver(&image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        let offsets = vec![0x60; 23];
        complete_block_init_with_offsets(&mut mem, &staged, 30, 0x20, &offsets);
        kernel.finalize_sys_driver(&staged, &mut mem).unwrap();
        let drives = ah52_dpb_drives(&mut kernel, &mut mem);
        assert_eq!(drives.first(), Some(&2));
        assert_eq!(drives.last(), Some(&25));
        assert_eq!(drives.len(), 24, "C: plus D: through Z:");

        let free_before = conventional_free_paragraphs(&kernel, &mem);
        let err = kernel
            .stage_sys_driver(&image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap_err();
        assert!(matches!(err, DriverStageError::NoBlockDriveLetters));
        assert_eq!(
            conventional_free_paragraphs(&kernel, &mem),
            free_before,
            "exhausted block-driver staging fails before allocation"
        );
    }

    #[test]
    fn zero_unit_block_driver_links_but_consumes_no_drive_letters() {
        let (mut kernel, mut mem) = arena_kernel();
        let image = tests_block_image_with_bpb(0x40);
        let staged = kernel
            .stage_sys_driver(&image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        complete_block_init_with_offsets(&mut mem, &staged, 0, 0x20, &[]);
        kernel.finalize_sys_driver(&staged, &mut mem).unwrap();

        assert!(
            published_device_chain(&mut kernel, &mut mem)
                .iter()
                .any(|name| name == b"TESTDEV "),
            "a zero-unit block driver is still linked into the device chain"
        );
        assert_eq!(ah52_dpb_drives(&mut kernel, &mut mem), vec![2]);
        let next = kernel
            .stage_sys_driver(&image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        assert_eq!(next.first_drive, 3, "zero units do not consume D:");
    }

    #[test]
    fn failed_block_driver_init_publishes_no_dpb_and_consumes_no_drive() {
        let (mut kernel, mut mem) = arena_kernel();
        let image = tests_block_image_with_bpb(0x40);
        let failed = kernel
            .stage_sys_driver(&image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        mem.write_u16(failed.request_linear + 0x03, 0x8100).unwrap();
        kernel.abort_sys_driver(&failed, &mut mem).unwrap();

        assert!(
            !published_device_chain(&mut kernel, &mut mem)
                .iter()
                .any(|name| name == b"TESTDEV "),
            "failed INIT must not link the block driver"
        );
        assert_eq!(ah52_dpb_drives(&mut kernel, &mut mem), vec![2]);
        let next = kernel
            .stage_sys_driver(&image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        assert_eq!(next.first_drive, 3, "failed INIT does not consume D:");
    }

    #[test]
    fn expanded_dpb_layout_keeps_sda_pointers_below_the_first_mcb() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        kernel.init_program(0x0200, 0x1200, &mut mem).unwrap();
        kernel.set_lastdrive(26);
        let image = tests_block_image_with_bpbs(&[(0x60, tests_bpb_1440k())]);
        let staged = kernel
            .stage_sys_driver(&image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        let offsets = vec![0x60; 23];
        complete_block_init_with_offsets(&mut mem, &staged, 23, 0x20, &offsets);
        kernel.finalize_sys_driver(&staged, &mut mem).unwrap();

        let base = ah52_sysvars_base(&mut kernel, &mut mem);
        let cds_off = mem.read_u16(base + 0x16).unwrap();
        let cds_seg = mem.read_u16(base + 0x18).unwrap();
        let cds = usize::from(cds_seg) * 16 + usize::from(cds_off);
        let last_cds_end = cds + 26 * 0x58;
        let mut ah34 = DosRegs {
            ax: 0x3400,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut ah34, &mut mem).unwrap();
        let sda = usize::from(ah34.es) * 16 + usize::from(ah34.bx) - 1;
        assert!(
            sda >= last_cds_end,
            "SDA starts after the expanded CDS array"
        );
        assert!(
            sda + usize::from(SDA_ALWAYS_SWAPPED_LEN) <= usize::from(kernel.arena.first_mcb()) * 16,
            "SDA live prefix fits below the first MCB"
        );

        let mut ax5d06 = DosRegs {
            ax: 0x5d06,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut ax5d06, &mut mem).unwrap();
        assert_eq!((ax5d06.ds, ax5d06.si), (ah34.es, ah34.bx - 1));
    }

    #[test]
    fn block_driver_name_is_not_opened_as_a_character_device() {
        let (mut kernel, mut mem) = arena_kernel();
        let image = tests_block_image_with_bpb(0x40);
        let staged = kernel
            .stage_sys_driver(&image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        complete_block_init_with_bpb(&mut mem, &staged, 0x40);
        kernel.finalize_sys_driver(&staged, &mut mem).unwrap();
        let (ds, dx) = put_asciiz(&mut mem, 0x2000, b"TESTDEV");
        let mut regs = DosRegs {
            ax: 0x3d00,
            ds,
            dx,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(
            regs.cf,
            "block devices are not opened through AH=3Dh name lookup"
        );
    }

    #[test]
    fn file_open_on_block_drive_letter_does_not_route_to_host_c() {
        let (mut kernel, mut mem) = arena_kernel();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("SAME.TXT"), b"host c").unwrap();
        kernel.mount_c(HostDrive::mount_c(root.path()).unwrap());
        let image = tests_block_image_with_bpb(0x40);
        let staged = kernel
            .stage_sys_driver(&image, "", DriverLoadPlacement::Low, &mut mem)
            .unwrap();
        complete_block_init_with_bpb(&mut mem, &staged, 0x40);
        kernel.finalize_sys_driver(&staged, &mut mem).unwrap();

        let (ds, dx) = put_asciiz(&mut mem, 0x2000, b"D:\\SAME.TXT");
        let mut regs = DosRegs {
            ax: 0x3d00,
            ds,
            dx,
            ..Default::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(regs.cf, "D: is not a host-backed file route in this slice");
        assert_eq!(regs.ax, 0x0003, "unbacked block-drive path is rejected");
    }

    #[test]
    fn ah52_publishes_a_cds_array_sized_by_lastdrive() {
        let (mut kernel, mut mem) = arena_kernel();
        kernel.set_lastdrive(8); // A: through H:

        let mut regs = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        let base = usize::from(regs.es) * 16 + usize::from(regs.bx);
        let dpb_off = mem.read_u16(base).unwrap();
        let dpb_seg = mem.read_u16(base + 2).unwrap();

        let cds_off = mem.read_u16(base + 0x16).unwrap();
        let cds_seg = mem.read_u16(base + 0x18).unwrap();
        assert_ne!(
            (cds_seg, cds_off),
            (0, 0),
            "[BX+0x16] points at the CDS array"
        );
        assert_eq!(
            mem.read_u8(base + 0x21).unwrap(),
            8,
            "LASTDRIVE also sizes the CDS array"
        );

        const CDS_LEN: usize = 0x58;
        let cds = usize::from(cds_seg) * 16 + usize::from(cds_off);
        for (index, letter) in (b'A'..=b'H').enumerate() {
            let entry = cds + index * CDS_LEN;
            assert_eq!(mem.read_u8(entry).unwrap(), letter, "drive letter");
            assert_eq!(mem.read_u8(entry + 1).unwrap(), b':', "drive colon");
            assert_eq!(mem.read_u8(entry + 2).unwrap(), b'\\', "root slash");
            assert_eq!(mem.read_u8(entry + 3).unwrap(), 0, "path terminator");
            assert_eq!(
                mem.read_u16(entry + 0x4f).unwrap(),
                2,
                "root backslash offset hides the drive letter and colon"
            );
        }

        let c_drive = cds + 2 * CDS_LEN;
        assert_eq!(
            mem.read_u16(c_drive + 0x43).unwrap(),
            0x4000,
            "C: is marked as a physical local drive"
        );
        assert_eq!(
            mem.read_u16(c_drive + 0x45).unwrap(),
            dpb_off,
            "C: CDS points at the C: DPB offset"
        );
        assert_eq!(
            mem.read_u16(c_drive + 0x47).unwrap(),
            dpb_seg,
            "C: CDS points at the C: DPB segment"
        );
    }

    #[test]
    fn ah34_returns_the_live_indos_flag_inside_the_sda() {
        let (mut kernel, mut mem) = arena_kernel();
        let mut set_dta = DosRegs {
            ax: 0x1a00,
            ds: 0x1234,
            dx: 0x0056,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set_dta, &mut mem).unwrap();

        let mut regs = DosRegs {
            ax: 0x3400,
            es: 0xabcd,
            bx: 0xdead,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();

        assert_ne!((regs.es, regs.bx), (0xabcd, 0xdead), "AH=34h returns ES:BX");
        let indos = usize::from(regs.es) * 16 + usize::from(regs.bx);
        let sda = indos - 1;
        assert_eq!(mem.read_u8(sda).unwrap(), 0, "critical-error flag is clear");
        assert_eq!(
            mem.read_u8(indos).unwrap(),
            0,
            "InDOS is clear between calls"
        );
        assert_eq!(
            mem.read_u16(sda + 0x0c).unwrap(),
            0x0056,
            "current DTA offset"
        );
        assert_eq!(
            mem.read_u16(sda + 0x0e).unwrap(),
            0x1234,
            "current DTA segment"
        );
        assert_eq!(mem.read_u16(sda + 0x10).unwrap(), 0x0100, "current PSP");
    }

    #[test]
    fn ax5d06_returns_minimal_live_sda_and_parks_the_dos_stacks() {
        let (mut kernel, mut mem) = arena_kernel();
        let mut bad_read = DosRegs {
            ax: 0x3f00,
            bx: 0x2222,
            cx: 1,
            ds: 0x2000,
            dx: 0x0000,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut bad_read, &mut mem).unwrap();
        assert!(bad_read.cf);
        assert_eq!(bad_read.ax, 0x0006, "bad handle seeds AH=59h state");

        let mut set_dta = DosRegs {
            ax: 0x1a00,
            ds: 0x3456,
            dx: 0x0789,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set_dta, &mut mem).unwrap();

        let mut regs = DosRegs {
            ax: 0x5d06,
            ds: 0xbeef,
            si: 0xface,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();

        assert!(!regs.cf, "AX=5D06h succeeds");
        assert_ne!(
            (regs.ds, regs.si),
            (0xbeef, 0xface),
            "DS:SI points at the SDA"
        );
        assert_eq!(regs.cx, 0, "large in-DOS stack swap area is parked");
        assert_eq!(
            regs.dx, 0x001a,
            "only the stable SDA prefix is always swapped"
        );

        let sda = usize::from(regs.ds) * 16 + usize::from(regs.si);
        assert_eq!(mem.read_u8(sda).unwrap(), 0, "critical-error flag");
        assert_eq!(mem.read_u8(sda + 1).unwrap(), 0, "InDOS flag");
        assert_eq!(
            mem.read_u8(sda + 2).unwrap(),
            0xff,
            "no current critical-error drive"
        );
        assert_eq!(mem.read_u8(sda + 3).unwrap(), 0x01, "last-error locus");
        assert_eq!(mem.read_u16(sda + 4).unwrap(), 0x0006, "last-error code");
        assert_eq!(mem.read_u8(sda + 6).unwrap(), 0x05, "last-error action");
        assert_eq!(mem.read_u8(sda + 7).unwrap(), 0x0d, "last-error class");
        assert_eq!(
            mem.read_u16(sda + 0x0c).unwrap(),
            0x0789,
            "current DTA offset"
        );
        assert_eq!(
            mem.read_u16(sda + 0x0e).unwrap(),
            0x3456,
            "current DTA segment"
        );
        assert_eq!(mem.read_u16(sda + 0x10).unwrap(), 0x0100, "current PSP");
        assert_eq!(
            mem.read_u16(sda + 0x14).unwrap(),
            0,
            "last process return code"
        );
        assert_eq!(mem.read_u8(sda + 0x16).unwrap(), 2, "current drive C:");
        assert_eq!(
            mem.read_u8(sda + 0x17).unwrap(),
            0,
            "extended break flag off"
        );
        assert_eq!(
            mem.read_u8(sda + 0x18).unwrap(),
            0,
            "code-page switch flag parked"
        );
        assert_eq!(
            mem.read_u8(sda + 0x19).unwrap(),
            0,
            "INT 24 abort code-page flag parked"
        );
    }

    #[test]
    fn ax5d0b_returns_dos4_sda_list() {
        let (mut kernel, mut mem) = arena_kernel();
        let mut set_dta = DosRegs {
            ax: 0x1a00,
            ds: 0x3456,
            dx: 0x0789,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set_dta, &mut mem).unwrap();

        let mut regs = DosRegs {
            ax: 0x5d0b,
            ds: 0xbeef,
            si: 0xface,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();

        assert!(!regs.cf, "AX=5D0Bh succeeds");
        assert_ne!((regs.ds, regs.si), (0xbeef, 0xface));
        let list = usize::from(regs.ds) * 16 + usize::from(regs.si);
        assert_eq!(mem.read_u16(list).unwrap(), 1, "one SDA area");
        let sda_off = mem.read_u16(list + 2).unwrap();
        let sda_seg = mem.read_u16(list + 4).unwrap();
        assert_ne!((sda_seg, sda_off), (regs.ds, regs.si));
        assert_eq!(
            mem.read_u16(list + 6).unwrap(),
            0x8000 | SDA_ALWAYS_SWAPPED_LEN,
            "stable prefix is swap-always"
        );
        let sda = usize::from(sda_seg) * 16 + usize::from(sda_off);
        assert_eq!(mem.read_u16(sda + 0x0c).unwrap(), 0x0789);
        assert_eq!(mem.read_u16(sda + 0x0e).unwrap(), 0x3456);
    }

    #[test]
    fn ax5d00_dispatches_int21_from_dpl() {
        let (mut kernel, mut mem) = arena_kernel();
        let dpl = 0x0100 * 16 + 0x0200;
        mem.write_u16(dpl, 0x1a00).unwrap();
        mem.write_u16(dpl + 6, 0x0789).unwrap();
        mem.write_u16(dpl + 12, 0x3456).unwrap();

        let mut regs = DosRegs {
            ax: 0x5d00,
            bp: 0xbabe,
            ds: 0x0100,
            dx: 0x0200,
            cf: true,
            ..DosRegs::default()
        };
        assert_eq!(
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap(),
            DosAction::Continue
        );
        assert!(!regs.cf);
        assert_eq!(regs.ax, 0x1a00);
        assert_eq!(regs.bp, 0xbabe);
        assert_eq!(regs.ds, 0x3456);
        assert_eq!(regs.dx, 0x0789);

        let mut get_dta = DosRegs {
            ax: 0x2f00,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get_dta, &mut mem).unwrap();
        assert_eq!((get_dta.es, get_dta.bx), (0x3456, 0x0789));
    }

    #[test]
    fn critical_error_scaffold_builds_int24_frame_and_marks_sda() {
        let (mut kernel, mut mem) = arena_kernel();
        let psp = 0x0100usize * 16;
        // PSP:12h is the saved previous handler. A live critical error must call
        // the current INT 24h vector instead, so seed different values.
        mem.write_u16(psp + 0x12, 0x2222).unwrap();
        mem.write_u16(psp + 0x14, 0x3333).unwrap();
        mem.write_u16(0x24 * 4, 0x4567).unwrap();
        mem.write_u16(0x24 * 4 + 2, 0x89ab).unwrap();

        let call = kernel
            .begin_critical_error(
                &mut mem,
                CriticalErrorRequest::disk(
                    2,
                    0x0b,
                    true,
                    CriticalErrorArea::Data,
                    FarPtr {
                        segment: 0x0050,
                        offset: 0x0012,
                    },
                ),
            )
            .unwrap();

        assert_eq!(
            call.handler,
            FarPtr {
                segment: 0x89ab,
                offset: 0x4567,
            },
            "critical error uses the live INT 24h vector"
        );
        assert_eq!(call.regs.ax, 0x3f02, "AH flags plus AL drive");
        assert_eq!(call.regs.di, 0x000b, "DI low byte carries the error code");
        assert_eq!(call.regs.bp, 0x0050, "BP:SI points at the driver header");
        assert_eq!(call.regs.si, 0x0012, "BP:SI points at the driver header");

        let mut sda_regs = DosRegs {
            ax: 0x5d06,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut sda_regs, &mut mem).unwrap();
        let sda = usize::from(sda_regs.ds) * 16 + usize::from(sda_regs.si);
        assert_eq!(mem.read_u8(sda).unwrap(), 1, "critical-error flag is set");
        assert_eq!(
            mem.read_u8(sda + 1).unwrap(),
            1,
            "DOS is busy during INT 24h"
        );
        assert_eq!(
            mem.read_u8(sda + 2).unwrap(),
            2,
            "current critical-error drive"
        );
        assert_eq!(mem.read_u16(sda + 4).unwrap(), 0x001e, "AH=59h error code");

        assert_eq!(
            kernel.finish_critical_error(0x07),
            CriticalErrorResponse::Fail
        );

        kernel.dispatch(0x21, &mut sda_regs, &mut mem).unwrap();
        let sda = usize::from(sda_regs.ds) * 16 + usize::from(sda_regs.si);
        assert_eq!(mem.read_u8(sda).unwrap(), 0, "critical-error flag clears");
        assert_eq!(
            mem.read_u8(sda + 2).unwrap(),
            0xff,
            "no current critical-error drive"
        );
    }

    #[test]
    fn ah52_publishes_an_sft_header_sized_by_files() {
        let (mut kernel, mut mem) = arena_kernel();
        kernel.set_config_sys_counts(7, 20);

        let mut regs = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        let base = usize::from(regs.es) * 16 + usize::from(regs.bx);

        let sft_off = mem.read_u16(base + 0x04).unwrap();
        let sft_seg = mem.read_u16(base + 0x06).unwrap();
        assert_ne!(
            (sft_seg, sft_off),
            (0, 0),
            "[BX+0x04] points at the first SFT header"
        );

        let sft = usize::from(sft_seg) * 16 + usize::from(sft_off);
        assert_eq!(
            mem.read_u16(sft).unwrap(),
            0xffff,
            "the first SFT header is the end of the SFT chain"
        );
        assert_eq!(
            mem.read_u16(sft + 2).unwrap(),
            0xffff,
            "the SFT next segment is FFFF for the end of the chain"
        );
        assert_eq!(
            mem.read_u16(sft + 4).unwrap(),
            7,
            "FILES= sizes the SFT table"
        );
    }

    #[test]
    fn ah52_seeds_the_con_sft_entry_used_by_the_default_jft() {
        let (mut kernel, mut mem) = arena_kernel();
        build_psp(&mut mem, 0x0100, 0x1100).unwrap();

        assert_eq!(mem.read_u8(0x0100 * 16 + 0x18).unwrap(), 1, "stdin JFT");
        assert_eq!(mem.read_u8(0x0100 * 16 + 0x19).unwrap(), 1, "stdout JFT");
        assert_eq!(mem.read_u8(0x0100 * 16 + 0x1a).unwrap(), 1, "stderr JFT");
        assert_eq!(mem.read_u8(0x0100 * 16 + 0x1b).unwrap(), 3, "AUX JFT");
        assert_eq!(mem.read_u8(0x0100 * 16 + 0x1c).unwrap(), 4, "PRN JFT");

        let mut regs = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        let base = usize::from(regs.es) * 16 + usize::from(regs.bx);
        let sft_off = mem.read_u16(base + 0x04).unwrap();
        let sft_seg = mem.read_u16(base + 0x06).unwrap();
        let sft = usize::from(sft_seg) * 16 + usize::from(sft_off);

        const SFT_ENTRY_LEN: usize = 0x3b;
        let con = sft + 0x06 + SFT_ENTRY_LEN;
        assert_eq!(
            mem.read_u16(con).unwrap(),
            3,
            "stdin/stdout/stderr all reference the CON SFT slot"
        );
        assert_eq!(
            mem.read_u16(con + 0x02).unwrap(),
            0x0002,
            "the shared CON SFT entry is read/write"
        );
        assert_ne!(
            mem.read_u16(con + 0x05).unwrap() & 0x0080,
            0,
            "CON is marked as a character device"
        );
        let name: Vec<u8> = (0..11)
            .map(|i| mem.read_u8(con + 0x20 + i).unwrap())
            .collect();
        assert_eq!(&name, b"CON        ", "CON SFT name");

        let aux = sft + 0x06 + 3 * SFT_ENTRY_LEN;
        let prn = sft + 0x06 + 4 * SFT_ENTRY_LEN;
        for (entry, expected_name) in [(aux, b"AUX        "), (prn, b"PRN        ")] {
            assert_eq!(
                mem.read_u16(entry).unwrap(),
                1,
                "one JFT entry references the device"
            );
            assert_ne!(
                mem.read_u16(entry + 0x05).unwrap() & 0x0080,
                0,
                "AUX/PRN SFT slots are character devices"
            );
            let name: Vec<u8> = (0..11)
                .map(|i| mem.read_u8(entry + 0x20 + i).unwrap())
                .collect();
            assert_eq!(&name, expected_name);
        }
    }

    fn ah52_sft_entry(kernel: &mut DosKernel, mem: &mut Memory, handle: u16) -> usize {
        let mut regs = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, mem).unwrap();
        let base = usize::from(regs.es) * 16 + usize::from(regs.bx);
        let sft_off = mem.read_u16(base + 0x04).unwrap();
        let sft_seg = mem.read_u16(base + 0x06).unwrap();
        let sft = usize::from(sft_seg) * 16 + usize::from(sft_off);
        const SFT_ENTRY_LEN: usize = 0x3b;
        sft + 0x06 + usize::from(handle) * SFT_ENTRY_LEN
    }

    fn ah52_sft_position(kernel: &mut DosKernel, mem: &mut Memory, handle: u16) -> u32 {
        let entry = ah52_sft_entry(kernel, mem, handle);
        mem.read_u32(entry + 0x15).unwrap()
    }

    #[test]
    fn ah52_publishes_an_open_host_file_sft_entry() {
        let (mut kernel, mut mem, _dir) =
            kernel_with_drive(&[("LEVEL1.DAT", b"abcdef")], r"C:\LEVEL1.DAT");
        kernel.init_program(0x0100, 0x1100, &mut mem).unwrap();

        let open = open(&mut kernel, &mut mem);
        assert!(!open.cf, "the host file opens");
        assert_eq!(open.ax, 5, "the first dynamic handle is 5");

        let mut regs = DosRegs {
            ax: 0x5200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        let base = usize::from(regs.es) * 16 + usize::from(regs.bx);
        let sft_off = mem.read_u16(base + 0x04).unwrap();
        let sft_seg = mem.read_u16(base + 0x06).unwrap();
        let sft = usize::from(sft_seg) * 16 + usize::from(sft_off);

        const SFT_ENTRY_LEN: usize = 0x3b;
        let entry = sft + 0x06 + usize::from(open.ax) * SFT_ENTRY_LEN;
        assert_eq!(mem.read_u16(entry).unwrap(), 1, "one handle references it");
        assert_eq!(
            mem.read_u16(entry + 0x02).unwrap(),
            0,
            "read-only open mode"
        );
        assert_eq!(
            mem.read_u8(entry + 0x04).unwrap(),
            0,
            "normal file attributes"
        );
        assert_eq!(
            mem.read_u16(entry + 0x05).unwrap() & 0x0080,
            0,
            "the SFT entry is a file, not a character device"
        );
        assert_eq!(mem.read_u32(entry + 0x11).unwrap(), 6, "file size");
        assert_eq!(mem.read_u32(entry + 0x15).unwrap(), 0, "current offset");
        let name: Vec<u8> = (0..11)
            .map(|i| mem.read_u8(entry + 0x20 + i).unwrap())
            .collect();
        assert_eq!(&name, b"LEVEL1  DAT", "FCB-style SFT name");
    }

    #[test]
    fn ah52_refreshes_host_file_sft_offset_after_read_write_and_seek() {
        let (mut kernel, mut mem, _dir) =
            kernel_with_drive(&[("LEVEL1.DAT", b"abcdef")], r"C:\LEVEL1.DAT");
        kernel.init_program(0x0100, 0x1100, &mut mem).unwrap();
        let mut open = DosRegs {
            ax: 0x3d02,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut open, &mut mem).unwrap();
        assert!(!open.cf, "the read/write host file opens");
        let handle = open.ax;
        assert_eq!(handle, 5, "the first dynamic handle is 5");

        let read = read(&mut kernel, &mut mem, handle, 2, 0x0400);
        assert!(!read.cf);
        assert_eq!(read.ax, 2);
        assert_eq!(ah52_sft_position(&mut kernel, &mut mem, handle), 2);

        let mut seek_end = DosRegs {
            ax: 0x4202,
            bx: handle,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut seek_end, &mut mem).unwrap();
        assert!(!seek_end.cf);
        assert_eq!(ah52_sft_position(&mut kernel, &mut mem, handle), 6);

        let src = 0x0100usize * 16 + 0x0500;
        mem.write_u8(src, b'X').unwrap();
        mem.write_u8(src + 1, b'Y').unwrap();
        let mut write = DosRegs {
            ax: 0x4000,
            bx: handle,
            cx: 2,
            ds: 0x0100,
            dx: 0x0500,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut write, &mut mem).unwrap();
        assert!(!write.cf);
        assert_eq!(write.ax, 2);
        let entry = ah52_sft_entry(&mut kernel, &mut mem, handle);
        assert_eq!(
            mem.read_u32(entry + 0x11).unwrap(),
            8,
            "file size after write"
        );
        assert_eq!(mem.read_u32(entry + 0x15).unwrap(), 8, "offset after write");

        let mut seek_abs = DosRegs {
            ax: 0x4200,
            bx: handle,
            dx: 3,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut seek_abs, &mut mem).unwrap();
        assert!(!seek_abs.cf);
        assert_eq!(ah52_sft_position(&mut kernel, &mut mem, handle), 3);
    }

    #[test]
    fn ah52_clears_host_file_sft_entry_after_close() {
        let (mut kernel, mut mem, _dir) =
            kernel_with_drive(&[("LEVEL1.DAT", b"abcdef")], r"C:\LEVEL1.DAT");
        kernel.init_program(0x0100, 0x1100, &mut mem).unwrap();
        let open = open(&mut kernel, &mut mem);
        assert!(!open.cf, "the host file opens");
        let handle = open.ax;
        let entry = ah52_sft_entry(&mut kernel, &mut mem, handle);
        assert_eq!(
            mem.read_u16(entry).unwrap(),
            1,
            "entry is live before close"
        );

        let close = close(&mut kernel, &mut mem, handle);
        assert!(!close.cf);
        let entry = ah52_sft_entry(&mut kernel, &mut mem, handle);
        assert_eq!(mem.read_u16(entry).unwrap(), 0, "refcount is cleared");
        assert_eq!(mem.read_u32(entry + 0x11).unwrap(), 0, "size is cleared");
        assert_eq!(mem.read_u32(entry + 0x15).unwrap(), 0, "offset is cleared");
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
        // mirrors it as offset then segment.
        let mut mem = Memory::new(64 * 1024).unwrap();
        // IVT entry 0x24 = offset 0xBEEF, segment 0xF000.
        mem.write_u16(0x24 * 4, 0xbeef).unwrap();
        mem.write_u16(0x24 * 4 + 2, 0xf000).unwrap();
        build_psp(&mut mem, 0x0100, 0x1100).unwrap();
        let psp = 0x0100usize * 16;
        assert_eq!(mem.read_u16(psp + 0x12).unwrap(), 0xbeef, "PSP offset");
        assert_eq!(mem.read_u16(psp + 0x14).unwrap(), 0xf000, "PSP segment");
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
    fn ah0e_selects_default_drive_and_reports_lastdrive_count() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[("DATA.TXT", b"hi")], "DATA.TXT");
        kernel.set_lastdrive(6); // A: through F:

        let mut select_d = DosRegs {
            ax: 0x0e00,
            dx: 0x0003,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut select_d, &mut mem).unwrap();
        assert_eq!(select_d.ax & 0xff, 0x06);

        let mut current = DosRegs {
            ax: 0x1900,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut current, &mut mem).unwrap();
        assert_eq!(current.ax & 0xff, 0x03);

        let mut open_default_d = DosRegs {
            ax: 0x3d00,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel
            .dispatch(0x21, &mut open_default_d, &mut mem)
            .unwrap();
        assert!(open_default_d.cf);
        assert_eq!(open_default_d.ax, 0x03);

        let mut select_c = DosRegs {
            ax: 0x0e00,
            dx: 0x0002,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut select_c, &mut mem).unwrap();

        let mut open_default_c = DosRegs {
            ax: 0x3d00,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel
            .dispatch(0x21, &mut open_default_c, &mut mem)
            .unwrap();
        assert!(!open_default_c.cf);
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
    fn ah3c_honors_the_readonly_create_attribute() {
        let (mut kernel, mut mem, dir) = kernel_with_drive(&[], r"C:\RO.TXT");
        let mut regs = DosRegs {
            ax: 0x3c00,
            cx: 0x0001,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf, "create failed: ax={:#06x}", regs.ax);

        let path = dir.path().join("RO.TXT");
        assert!(
            std::fs::metadata(path).unwrap().permissions().readonly(),
            "CX bit 0 creates a read-only host file"
        );
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
    fn ah40_to_stdin_handle_writes_to_the_console_device() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let src = 0x0100usize * 16 + 0x0300;
        mem.write_u8(src, b'!').unwrap();
        let mut regs = DosRegs {
            ax: 0x4000,
            bx: 0,
            cx: 1,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_eq!(regs.ax, 1);
        assert_eq!(kernel.stdout(), b"!");
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
    fn ah04_and_ah05_accept_and_discard_aux_prn_output() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();

        for (ah, ch) in [(0x04u16, b'A'), (0x05u16, b'P')] {
            let mut regs = DosRegs {
                ax: ah << 8,
                dx: u16::from(ch),
                cf: true,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
            assert!(!regs.cf);
            assert_eq!(regs.ax & 0x00ff, u16::from(ch));
        }

        assert!(
            kernel.stdout().is_empty(),
            "AUX/PRN character output is not echoed to the console"
        );
    }

    #[test]
    fn stdaux_reads_are_deterministic_without_serial_rx() {
        let mut kernel = DosKernel::new();
        let mut mem = Memory::new(64 * 1024).unwrap();
        let dst = 0x2000usize;

        // AH=03h has no status return. With no serial receive source wired, the
        // DOS facade returns a deterministic NUL byte instead of blocking forever.
        let mut single = DosRegs {
            ax: 0x0300,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut single, &mut mem).unwrap();
        assert!(!single.cf);
        assert_eq!(single.ax & 0x00ff, 0x00);

        // Handle 3 is the inherited STDAUX JFT entry. Reads see EOF, not an
        // invalid-handle error, because the character device exists even though
        // the current HLE has no RX buffer for it.
        let mut handle_read = DosRegs {
            ax: 0x3f00,
            bx: 3,
            cx: 8,
            ds: 0,
            dx: dst as u16,
            ..DosRegs::default()
        };
        mem.write_u8(dst, 0xa5).unwrap();
        kernel.dispatch(0x21, &mut handle_read, &mut mem).unwrap();
        assert!(!handle_read.cf);
        assert_eq!(handle_read.ax, 0, "AUX read reports EOF when RX is empty");
        assert_eq!(
            mem.read_u8(dst).unwrap(),
            0xa5,
            "EOF leaves buffer untouched"
        );
        assert!(kernel.stdout().is_empty(), "AUX input is not echoed");
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

    fn dta_raw_name(mem: &Memory) -> [u8; 11] {
        let mut name = [0; 11];
        for (index, slot) in name.iter_mut().enumerate() {
            *slot = mem.read_u8(0x1e + index).unwrap();
        }
        name
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
    fn ah4e_bare_pattern_uses_current_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("SUB")).unwrap();
        std::fs::write(dir.path().join("ROOT.TXT"), b"root").unwrap();
        std::fs::write(dir.path().join("SUB").join("LOCAL.TXT"), b"local").unwrap();
        let (mut kernel, mut mem) = find_kernel(dir.path());

        let path_base = 0x0100usize * 16 + 0x0200;
        for (i, byte) in b"SUB".iter().enumerate() {
            mem.write_u8(path_base + i, *byte).unwrap();
        }
        mem.write_u8(path_base + 3, 0).unwrap();
        let mut chdir = DosRegs {
            ax: 0x3b00,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut chdir, &mut mem).unwrap();
        assert!(!chdir.cf);

        let regs = find_first(&mut kernel, &mut mem, "*.TXT", 0);
        assert!(!regs.cf);
        assert_eq!(dta_name(&mem), "LOCAL.TXT");
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
    fn ah4e_volume_label_mask_returns_label_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("F.TXT"), b"a").unwrap();
        let (mut kernel, mut mem) = find_kernel(dir.path());
        kernel.volume_label = Some(*b"TOKA-DOS   ");

        let regs = find_first(&mut kernel, &mut mem, "C:\\*.*", 0x08);
        assert!(!regs.cf);
        assert_eq!(mem.read_u8(0x15).unwrap(), 0x08);
        assert_eq!(mem.read_u32(0x1a).unwrap(), 0);
        assert_eq!(&dta_raw_name(&mem), b"TOKA-DOS   ");

        let next = find_next(&mut kernel, &mut mem);
        assert!(next.cf);
        assert_eq!(next.ax, 0x12);
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
        let mut exec_state = DosRegs {
            ax: 0x4b05,
            ds: 0x1000,
            dx: 0x0100,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut exec_state, &mut mem).unwrap();
        assert!(!exec_state.cf);
        assert_eq!(exec_state.ax, 0);

        for al in [0x02u16, 0x04] {
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
        place_fcb_at(mem, 0x0100usize * 16 + 0x0200, drive, name);
    }

    fn place_fcb_at(mem: &mut Memory, base: usize, drive: u8, name: &str) {
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
    fn fcb_open_supports_extended_fcb_header() {
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[("DATA.BIN", &[0u8; 300])]);
        place_extended_fcb(&mut mem, 0x01, 3, "DATA.BIN");

        let regs = fcb_call(&mut kernel, &mut mem, 0x0f);

        assert_eq!(regs.ax & 0xff, 0x00, "extended FCB open succeeds");
        let base = 0x0100usize * 16 + 0x0200;
        let fcb = base + 7;
        assert_eq!(mem.read_u8(base).unwrap(), 0xff, "extended prefix kept");
        assert_eq!(mem.read_u8(base + 6).unwrap(), 0x01, "attribute byte kept");
        assert_eq!(
            mem.read_u8(fcb).unwrap(),
            3,
            "drive byte lives in the normal FCB subrecord"
        );
        assert_eq!(mem.read_u16(fcb + 0x0e).unwrap(), 128, "record size 128");
        assert_eq!(mem.read_u32(fcb + 0x10).unwrap(), 300, "file size");
        assert_eq!(mem.read_u16(fcb + 0x0c).unwrap(), 0, "current block 0");
        assert_eq!(mem.read_u8(fcb + 0x20).unwrap(), 0, "current record 0");
    }

    #[test]
    fn fcb_open_missing_file_returns_ff() {
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[]);
        place_fcb(&mut mem, 3, "NOPE.DAT");
        let regs = fcb_call(&mut kernel, &mut mem, 0x0f);
        assert_eq!(regs.ax & 0xff, 0xff);
    }

    #[test]
    fn fcb_open_rejects_unmounted_drive_byte() {
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[("DATA.BIN", b"x")]);
        place_fcb(&mut mem, 1, "DATA.BIN"); // A:, not mounted by the HLE

        let regs = fcb_call(&mut kernel, &mut mem, 0x0f);

        assert_eq!(regs.ax & 0xff, 0xff, "A: FCBs do not alias to mounted C:");
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
    fn fcb_sequential_read_uses_extended_fcb_body_fields() {
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[("FILE.BIN", &[0x5au8; 128])]);
        place_extended_fcb(&mut mem, 0, 3, "FILE.BIN");
        set_dta_0500(&mut kernel, &mut mem);
        assert_eq!(fcb_call(&mut kernel, &mut mem, 0x0f).ax & 0xff, 0x00);

        let read = fcb_call(&mut kernel, &mut mem, 0x14);

        assert_eq!(read.ax & 0xff, 0x00, "extended FCB read succeeds");
        let base = 0x0100usize * 16 + 0x0200;
        let fcb = base + 7;
        assert_eq!(mem.read_u8(base).unwrap(), 0xff, "extended prefix kept");
        assert_eq!(mem.read_u8(0x0500usize * 16).unwrap(), 0x5a, "DTA filled");
        assert_eq!(
            mem.read_u8(fcb + 0x20).unwrap(),
            1,
            "current record advanced inside the FCB body"
        );
    }

    #[test]
    fn fcb_sequential_read_returns_02_when_record_crosses_dta_segment() {
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[("DATA.BIN", &[0xacu8; 128])]);
        place_fcb(&mut mem, 3, "DATA.BIN");
        assert_eq!(fcb_call(&mut kernel, &mut mem, 0x0f).ax & 0xff, 0x00);
        let mut set_dta = DosRegs {
            ax: 0x1a00,
            ds: 0x0500,
            dx: 0xffc0,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set_dta, &mut mem).unwrap();

        let read = fcb_call(&mut kernel, &mut mem, 0x14);
        assert_eq!(
            read.ax & 0xff,
            0x02,
            "record transfer crossing a DTA segment boundary returns AL=02"
        );
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
    fn fcb_create_uses_extended_fcb_readonly_attribute() {
        let (mut kernel, mut mem, dir) = fcb_kernel(&[]);
        place_extended_fcb(&mut mem, 0x01, 3, "ROFCB.BIN");

        let regs = fcb_call(&mut kernel, &mut mem, 0x16);

        assert_eq!(regs.ax & 0xff, 0x00, "extended FCB create succeeds");
        assert!(
            std::fs::metadata(dir.path().join("ROFCB.BIN"))
                .unwrap()
                .permissions()
                .readonly(),
            "extended FCB attribute bit 0 creates a read-only host file"
        );
    }

    #[test]
    fn fcb_sequential_write_returns_02_when_record_crosses_dta_segment() {
        let (mut kernel, mut mem, dir) = fcb_kernel(&[]);
        place_fcb(&mut mem, 3, "WRAP.BIN");
        assert_eq!(fcb_call(&mut kernel, &mut mem, 0x16).ax & 0xff, 0x00);
        let mut set_dta = DosRegs {
            ax: 0x1a00,
            ds: 0x0500,
            dx: 0xffc0,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set_dta, &mut mem).unwrap();

        let write = fcb_call(&mut kernel, &mut mem, 0x15);
        assert_eq!(
            write.ax & 0xff,
            0x02,
            "record write crossing a DTA segment boundary returns AL=02"
        );
        assert_eq!(
            std::fs::metadata(dir.path().join("WRAP.BIN"))
                .unwrap()
                .len(),
            0,
            "no bytes are written after a segment-wrap failure"
        );
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
    fn fcb_extended_find_volume_label_uses_label_entry() {
        let (mut kernel, mut mem, _dir) = fcb_kernel(&[("F.TXT", b"x")]);
        kernel.volume_label = Some(*b"TOKA-DOS   ");
        place_extended_fcb(&mut mem, 0x08, 0, "????????.???");

        let r = fcb_call(&mut kernel, &mut mem, 0x11);
        assert_eq!(r.ax & 0xff, 0x00);
        assert_eq!(mem.read_u8(FCB_DTA).unwrap(), 0xff);
        assert_eq!(mem.read_u8(FCB_DTA + 6).unwrap(), 0x08);
        let raw: Vec<u8> = (0..11)
            .map(|i| mem.read_u8(FCB_DTA + 8 + i).unwrap())
            .collect();
        assert_eq!(&raw, b"TOKA-DOS   ");
        assert_eq!(mem.read_u32(FCB_DTA + 8 + 0x1c).unwrap(), 0);

        let next = fcb_call(&mut kernel, &mut mem, 0x12);
        assert_eq!(next.ax & 0xff, 0xff);
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

    fn fcb_record_call_with_cx(
        kernel: &mut DosKernel,
        mem: &mut Memory,
        ah: u16,
        cx: u16,
    ) -> DosRegs {
        let mut regs = DosRegs {
            ax: ah << 8,
            ds: 0x0100,
            dx: 0x0200,
            cx,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, mem).unwrap();
        regs
    }

    #[test]
    fn fcb_read_record_ops_map_missing_file_to_eof_status() {
        for (ah, cx) in [(0x14u16, 0), (0x21, 0), (0x27, 1)] {
            let (mut kernel, mut mem, _dir) = fcb_kernel(&[]);
            place_fcb(&mut mem, 3, "NOFILE.BIN");
            set_dta_0500(&mut kernel, &mut mem);
            let dta = 0x0500usize * 16;
            mem.write_u8(dta, 0x77).unwrap();

            let regs = fcb_record_call_with_cx(&mut kernel, &mut mem, ah, cx);
            assert_eq!(
                regs.ax & 0xff,
                0x01,
                "AH={ah:02x} maps a missing record source to AL=01, not AL=FF"
            );
            if ah == 0x27 {
                assert_eq!(regs.cx, 0, "block read transfers no records");
            }
            assert_eq!(mem.read_u8(dta).unwrap(), 0x77, "DTA left untouched");
        }
    }

    #[test]
    fn fcb_write_record_ops_map_missing_file_to_disk_full_status() {
        for (ah, cx) in [(0x15u16, 0), (0x22, 0), (0x28, 1)] {
            let (mut kernel, mut mem, _dir) = fcb_kernel(&[]);
            place_fcb(&mut mem, 3, "NOFILE.BIN");
            set_dta_0500(&mut kernel, &mut mem);
            let dta = 0x0500usize * 16;
            for i in 0..128usize {
                mem.write_u8(dta + i, i as u8).unwrap();
            }

            let regs = fcb_record_call_with_cx(&mut kernel, &mut mem, ah, cx);
            assert_eq!(
                regs.ax & 0xff,
                0x01,
                "AH={ah:02x} maps an unwritable record target to AL=01, not AL=FF"
            );
            if ah == 0x28 {
                assert_eq!(regs.cx, 0, "block write transfers no records");
            }
        }
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
    fn fcb_random_record_ops_return_02_when_dta_wraps() {
        for (ah, cx) in [(0x21u16, 0), (0x22, 0), (0x27, 1), (0x28, 1)] {
            let (mut kernel, mut mem, _dir) = fcb_kernel(&[("DATA.BIN", &[0x11u8; 128])]);
            place_fcb(&mut mem, 3, "DATA.BIN");
            assert_eq!(fcb_call(&mut kernel, &mut mem, 0x0f).ax & 0xff, 0x00);
            let base = 0x0100usize * 16 + 0x0200;
            mem.write_u32(base + 0x21, 0).unwrap();
            let mut set_dta = DosRegs {
                ax: 0x1a00,
                ds: 0x0500,
                dx: 0xffc0,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut set_dta, &mut mem).unwrap();

            let mut regs = DosRegs {
                ax: ah << 8,
                ds: 0x0100,
                dx: 0x0200,
                cx,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
            assert_eq!(
                regs.ax & 0xff,
                0x02,
                "AH={ah:02x} returns AL=02 when the DTA would wrap"
            );
            if ah == 0x27 || ah == 0x28 {
                assert_eq!(regs.cx, 0, "no block records transfer on DTA wrap");
            }
        }
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
    fn ah4b_al1_loads_child_and_returns_entry_without_exec() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CHILD.COM"), [0xcdu8, 0x20]).unwrap();
        let (mut kernel, mut mem) = exec_kernel(dir.path());
        kernel.last_exit_code = 7;
        kernel.last_exit_type = 3;
        place_exec_inputs(&mut mem, "C:\\CHILD.COM", 0);
        let mut regs = DosRegs {
            ax: 0x4b01,
            ds: 0x1000,
            dx: 0,
            es: 0x1000,
            bx: 0x40,
            ..DosRegs::default()
        };

        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();

        assert_eq!(action, DosAction::Continue);
        assert!(!regs.cf);
        assert_eq!(kernel.arena.psp_seg, 0x0100);
        assert!(kernel.program_stack.is_empty());
        assert_eq!(kernel.last_exit_code, 7);
        assert_eq!(kernel.last_exit_type, 3);
        assert_eq!(mem.read_u16(0x1004e).unwrap(), 0xfffc); // initial SP
        assert_eq!(mem.read_u16(0x10050).unwrap(), 0x0203); // initial SS
        assert_eq!(mem.read_u16(0x10052).unwrap(), 0x0100); // entry IP
        assert_eq!(mem.read_u16(0x10054).unwrap(), 0x0203); // entry CS
        assert_eq!(mem.read_u16(0x0203 * 16 + 0xfffc).unwrap(), 0x0000);
        assert_eq!(mem.read_u16(0x0203 * 16 + 0x16).unwrap(), 0x0100);
        assert_eq!(kernel.arena.free_base(&mem), ARENA_TOP);
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
    fn finish_exec_closes_child_only_handles_but_keeps_parent_handles() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CHILD.COM"), [0xcdu8, 0x20]).unwrap();
        std::fs::write(dir.path().join("DATA.TXT"), b"abcdef").unwrap();
        let (mut kernel, mut mem) = exec_kernel(dir.path());
        let path_base = 0x0100usize * 16 + 0x0200;
        for (index, byte) in r"C:\DATA.TXT".bytes().enumerate() {
            mem.write_u8(path_base + index, byte).unwrap();
        }
        mem.write_u8(path_base + r"C:\DATA.TXT".len(), 0).unwrap();
        let parent_handle = open_data(&mut kernel, &mut mem);
        assert_eq!(parent_handle, 5);
        let parent_first = read(&mut kernel, &mut mem, parent_handle, 1, 0x0400);
        assert!(!parent_first.cf);
        assert_eq!(mem.read_u8(0x0100usize * 16 + 0x0400).unwrap(), b'a');

        place_exec_inputs(&mut mem, "C:\\CHILD.COM", 0);
        let exec = exec_al0(&mut kernel, &mut mem);
        assert!(!exec.cf);
        assert_eq!(kernel.arena.psp_seg, 0x0203);
        let child_inherited = read(&mut kernel, &mut mem, parent_handle, 1, 0x0410);
        assert!(!child_inherited.cf);
        assert_eq!(mem.read_u8(0x0100usize * 16 + 0x0410).unwrap(), b'b');
        let child_only_handle = open_data(&mut kernel, &mut mem);
        assert_eq!(child_only_handle, 6);

        kernel.finish_exec(7, &mut mem).unwrap();

        let parent_after = read(&mut kernel, &mut mem, parent_handle, 1, 0x0420);
        assert!(!parent_after.cf);
        assert_eq!(mem.read_u8(0x0100usize * 16 + 0x0420).unwrap(), b'c');
        let child_only_after = read(&mut kernel, &mut mem, child_only_handle, 1, 0x0430);
        assert!(child_only_after.cf);
        assert_eq!(child_only_after.ax, 0x06);
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
    fn finish_exec_reclaims_child_upper_memory_blocks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CHILD.COM"), [0xcdu8, 0x20]).unwrap();
        let (mut kernel, mut mem) = exec_kernel(dir.path());
        kernel.set_umb_region(0xc800, 0x0040, &mut mem).unwrap();
        kernel.set_dos_umb(true);
        kernel.set_umb_link(true);
        set_alloc_strategy(&mut kernel, &mut mem, 0x0040);

        place_exec_inputs(&mut mem, "C:\\CHILD.COM", 0);
        let regs = exec_al0(&mut kernel, &mut mem);
        assert!(!regs.cf);
        assert_eq!(kernel.arena.psp_seg, 0x0203);

        let child_umb = dos_alloc(&mut kernel, &mut mem, 0x0010);
        assert!(!child_umb.cf);
        assert!(
            (0xc801..0xc840).contains(&child_umb.ax),
            "child allocation came from the UMB arena"
        );

        kernel.finish_exec(0, &mut mem).unwrap();

        let full_pool = match kernel.request_umb(0x003f, &mut mem).unwrap() {
            Ok(seg) => seg,
            Err(largest) => panic!("child UMB leaked; largest free UMB was {largest:#06x}"),
        };
        assert_eq!(full_pool, 0xc801);
    }

    #[test]
    fn abnormal_int20_exit_reclaims_child_upper_memory_blocks() {
        let dir = tempfile::tempdir().unwrap();
        // CHILD.COM is INT 20h (the legacy/abnormal terminate), not AH=4Ch.
        std::fs::write(dir.path().join("CHILD.COM"), [0xcdu8, 0x20]).unwrap();
        let (mut kernel, mut mem) = exec_kernel(dir.path());
        kernel.set_umb_region(0xc800, 0x0040, &mut mem).unwrap();
        kernel.set_dos_umb(true);
        kernel.set_umb_link(true);
        set_alloc_strategy(&mut kernel, &mut mem, 0x0040);

        place_exec_inputs(&mut mem, "C:\\CHILD.COM", 0);
        let regs = exec_al0(&mut kernel, &mut mem);
        assert!(!regs.cf);
        assert_eq!(kernel.arena.psp_seg, 0x0203);

        let child_umb = dos_alloc(&mut kernel, &mut mem, 0x0010);
        assert!(!child_umb.cf);
        assert!(
            (0xc801..0xc840).contains(&child_umb.ax),
            "child allocation came from the UMB arena"
        );

        // The child terminates abnormally via INT 20h. The kernel returns Exit(0);
        // the machine then pops the parent frame and completes teardown through
        // finish_exec. Mirror that sequence here.
        let mut term = DosRegs::default();
        let action = kernel.dispatch(0x20, &mut term, &mut mem).unwrap();
        assert_eq!(
            action,
            DosAction::Exit(0),
            "INT 20h must terminate the child"
        );
        kernel.finish_exec(0, &mut mem).unwrap();

        let full_pool = match kernel.request_umb(0x003f, &mut mem).unwrap() {
            Ok(seg) => seg,
            Err(largest) => {
                panic!("child UMB leaked after abnormal exit; largest free UMB was {largest:#06x}")
            }
        };
        assert_eq!(full_pool, 0xc801);
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
            open_files: kernel.open_files.clone(),
            ems_handles: kernel.ems_handles.clone(),
            device_handles: kernel.device_handles.clone(),
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
        // CP/M compatibility entry and portable DOS-call helper.
        assert_eq!(mem.read_u8(psp + 0x05).unwrap(), 0x9a);
        assert_eq!(mem.read_u16(psp + 0x06).unwrap(), 0x00c0);
        assert_eq!(mem.read_u16(psp + 0x08).unwrap(), 0x0000);
        assert_eq!(mem.read_u8(psp + 0x50).unwrap(), 0xcd);
        assert_eq!(mem.read_u8(psp + 0x51).unwrap(), 0x21);
        assert_eq!(mem.read_u8(psp + 0x52).unwrap(), 0xcb);
        // The INT 22h/23h/24h far vectors are snapshotted from the IVT.
        assert_eq!(mem.read_u16(psp + 0x0a).unwrap(), 0x1111);
        assert_eq!(mem.read_u16(psp + 0x0c).unwrap(), 0x2222);
        assert_eq!(mem.read_u16(psp + 0x0e).unwrap(), 0x3333);
        assert_eq!(mem.read_u16(psp + 0x10).unwrap(), 0x4444);
        assert_eq!(mem.read_u16(psp + 0x12).unwrap(), 0x5555);
        assert_eq!(mem.read_u16(psp + 0x14).unwrap(), 0x6666);
        // Parent PSP defaults to 0 (no parent for a directly loaded program).
        assert_eq!(mem.read_u16(psp + 0x16).unwrap(), 0);
        // The JFT: count 20 at 0x32, far pointer PSP:0x18 at 0x34, handles 0-2
        // map to CON, handle 3 maps to AUX, handle 4 maps to PRN, and the rest
        // stay closed (0xFF).
        assert_eq!(mem.read_u16(psp + 0x32).unwrap(), 20);
        assert_eq!(mem.read_u16(psp + 0x34).unwrap(), 0x0018);
        assert_eq!(mem.read_u16(psp + 0x36).unwrap(), 0x0100);
        assert_eq!(mem.read_u8(psp + 0x18).unwrap(), 0x01);
        assert_eq!(mem.read_u8(psp + 0x19).unwrap(), 0x01);
        assert_eq!(mem.read_u8(psp + 0x1a).unwrap(), 0x01);
        assert_eq!(mem.read_u8(psp + 0x1b).unwrap(), 0x03);
        assert_eq!(mem.read_u8(psp + 0x1c).unwrap(), 0x04);
        assert_eq!(mem.read_u8(psp + 0x18 + 19).unwrap(), 0xff); // last entry closed
    }

    #[test]
    fn ah26_create_psp_copies_current_psp_and_refreshes_vectors() {
        // RBIL: AH=26h creates a PSP at DX, copying the caller's PSP, while
        // refreshing the top-of-memory word, saved INT 22h/23h/24h vectors from
        // the IVT, and clearing the parent PSP field.
        let mut mem = Memory::new(1024 * 1024).unwrap();
        load_com(&[0xb8, 0x00, 0x4c, 0xcd, 0x21], &mut mem, 0x0100).unwrap();
        let prog_top = mem.read_u16(0x0100 * 16 + 0x02).unwrap();
        let mut kernel = DosKernel::new();
        kernel.init_program(0x0100, prog_top, &mut mem).unwrap();
        let parent = 0x0100usize * 16;
        mem.write_u8(parent + 0x80, 4).unwrap();
        for (i, &byte) in b"TAIL\r".iter().enumerate() {
            mem.write_u8(parent + 0x81 + i, byte).unwrap();
        }

        let mut alloc = DosRegs {
            ax: 0x4800,
            bx: 0x0020,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut alloc, &mut mem).unwrap();
        assert!(!alloc.cf, "allocation failed: ax={:#06x}", alloc.ax);
        let target_psp = alloc.ax;

        mem.write_u16(0x22 * 4, 0x1111).unwrap();
        mem.write_u16(0x22 * 4 + 2, 0x2222).unwrap();
        mem.write_u16(0x23 * 4, 0x3333).unwrap();
        mem.write_u16(0x23 * 4 + 2, 0x4444).unwrap();
        mem.write_u16(0x24 * 4, 0x5555).unwrap();
        mem.write_u16(0x24 * 4 + 2, 0x6666).unwrap();

        let mut create = DosRegs {
            ax: 0x2600,
            dx: target_psp,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut create, &mut mem).unwrap();

        let target = usize::from(target_psp) * 16;
        assert_eq!(mem.read_u16(target).unwrap(), 0x20cd);
        assert_eq!(mem.read_u16(target + 0x02).unwrap(), target_psp + 0x0020);
        assert_eq!(mem.read_u16(target + 0x0a).unwrap(), 0x1111);
        assert_eq!(mem.read_u16(target + 0x0c).unwrap(), 0x2222);
        assert_eq!(mem.read_u16(target + 0x0e).unwrap(), 0x3333);
        assert_eq!(mem.read_u16(target + 0x10).unwrap(), 0x4444);
        assert_eq!(mem.read_u16(target + 0x12).unwrap(), 0x5555);
        assert_eq!(mem.read_u16(target + 0x14).unwrap(), 0x6666);
        assert_eq!(mem.read_u16(target + 0x16).unwrap(), 0x0000);
        assert_eq!(mem.read_u8(target + 0x80).unwrap(), 4);
        for (i, &byte) in b"TAIL\r".iter().enumerate() {
            assert_eq!(mem.read_u8(target + 0x81 + i).unwrap(), byte);
        }
    }

    #[test]
    fn ah55_create_child_psp_sets_parent_and_current_psp() {
        // RBIL: AH=55h uses the AH=26h PSP creation path, but writes the parent
        // PSP link to the current PSP, stores SI in PSP:02h, and makes DX the
        // current PSP.
        let mut mem = Memory::new(1024 * 1024).unwrap();
        load_com(&[0xb8, 0x00, 0x4c, 0xcd, 0x21], &mut mem, 0x0100).unwrap();
        let prog_top = mem.read_u16(0x0100 * 16 + 0x02).unwrap();
        let mut kernel = DosKernel::new();
        kernel.init_program(0x0100, prog_top, &mut mem).unwrap();
        let parent = 0x0100usize * 16;
        mem.write_u8(parent + 0x80, 3).unwrap();
        for (i, &byte) in b"RUN\r".iter().enumerate() {
            mem.write_u8(parent + 0x81 + i, byte).unwrap();
        }

        let mut alloc = DosRegs {
            ax: 0x4800,
            bx: 0x0030,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut alloc, &mut mem).unwrap();
        assert!(!alloc.cf, "allocation failed: ax={:#06x}", alloc.ax);
        let child_psp = alloc.ax;

        let mut create = DosRegs {
            ax: 0x5500,
            dx: child_psp,
            si: 0x7777,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut create, &mut mem).unwrap();

        let child = usize::from(child_psp) * 16;
        assert_eq!(mem.read_u16(child).unwrap(), 0x20cd);
        assert_eq!(mem.read_u16(child + 0x02).unwrap(), 0x7777);
        assert_eq!(mem.read_u16(child + 0x16).unwrap(), 0x0100);
        assert_eq!(mem.read_u16(child + 0x36).unwrap(), child_psp);
        assert_eq!(mem.read_u8(child + 0x80).unwrap(), 3);
        for (i, &byte) in b"RUN\r".iter().enumerate() {
            assert_eq!(mem.read_u8(child + 0x81 + i).unwrap(), byte);
        }

        let mut get_current = DosRegs {
            ax: 0x5100,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get_current, &mut mem).unwrap();
        assert_eq!(get_current.bx, child_psp);
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
    fn ah31_enforces_dos3_minimum_and_keeps_psp_owner() {
        let (mut kernel, mut mem, _prog_top) = env_kernel();
        let mut regs = DosRegs {
            ax: 0x3100,
            dx: 0x0001,
            ..DosRegs::default()
        };

        assert_eq!(
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap(),
            DosAction::Exit(0)
        );

        assert!(kernel.arena.resident);
        assert_eq!(kernel.arena.prog_top(&mem), 0x0100 + 0x0006);
        let chain = read_mcb_chain(&mem, kernel.arena.first_mcb());
        assert_eq!(chain[0].owner, 0x0100);
        assert_eq!(chain[0].size, 0x0006);
    }

    fn seed_psp_saved_and_live_vectors(mem: &mut Memory) {
        let psp = 0x0100usize * 16;
        for (psp_off, int_no, saved_off, saved_seg, live_off, live_seg) in [
            (0x0a, 0x22, 0x1111, 0x2222, 0xaaaa, 0xbbbb),
            (0x0e, 0x23, 0x3333, 0x4444, 0xcccc, 0xdddd),
            (0x12, 0x24, 0x5555, 0x6666, 0xeeee, 0xffff),
        ] {
            mem.write_u16(psp + psp_off, saved_off).unwrap();
            mem.write_u16(psp + psp_off + 2, saved_seg).unwrap();
            let ivt = int_no * 4;
            mem.write_u16(ivt, live_off).unwrap();
            mem.write_u16(ivt + 2, live_seg).unwrap();
        }
    }

    fn assert_psp_saved_vectors_restored(mem: &Memory) {
        for (int_no, expected_off, expected_seg) in [
            (0x22, 0x1111, 0x2222),
            (0x23, 0x3333, 0x4444),
            (0x24, 0x5555, 0x6666),
        ] {
            let ivt = int_no * 4;
            assert_eq!(mem.read_u16(ivt).unwrap(), expected_off);
            assert_eq!(mem.read_u16(ivt + 2).unwrap(), expected_seg);
        }
    }

    #[test]
    fn ah31_restores_psp_saved_vectors() {
        let (mut kernel, mut mem, _prog_top) = env_kernel();
        seed_psp_saved_and_live_vectors(&mut mem);

        let mut regs = DosRegs {
            ax: 0x3100,
            dx: 0x0020,
            ..DosRegs::default()
        };
        assert_eq!(
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap(),
            DosAction::Exit(0)
        );

        assert_psp_saved_vectors_restored(&mem);
    }

    #[test]
    fn ah31_records_tsr_exit_type_for_ah4d() {
        let (mut kernel, mut mem, _prog_top) = env_kernel();
        let mut keep = DosRegs {
            ax: 0x3109,
            dx: 0x0020,
            ..DosRegs::default()
        };

        assert_eq!(
            kernel.dispatch(0x21, &mut keep, &mut mem).unwrap(),
            DosAction::Exit(9)
        );
        kernel.finish_exec(9, &mut mem).unwrap();

        let mut get = DosRegs {
            ax: 0x4d00,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get, &mut mem).unwrap();

        assert!(!get.cf);
        assert_eq!(get.ax, 0x0309);
    }

    #[test]
    fn int27_keeps_resident_with_dos3_minimum_and_tsr_exit_type() {
        let (mut kernel, mut mem, _prog_top) = env_kernel();
        let mut keep = DosRegs {
            dx: 0x0020, // bytes, below the DOS 3+ 0x60-byte minimum
            ..DosRegs::default()
        };

        assert_eq!(
            kernel.dispatch(0x27, &mut keep, &mut mem).unwrap(),
            DosAction::Exit(0)
        );
        assert!(kernel.arena.resident);
        assert_eq!(kernel.arena.prog_top(&mem), 0x0100 + 0x0006);
        kernel.finish_exec(0, &mut mem).unwrap();

        let mut get = DosRegs {
            ax: 0x4d00,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get, &mut mem).unwrap();

        assert!(!get.cf);
        assert_eq!(get.ax, 0x0300);
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
    fn portable_c_root_uses_exe_dir_and_creates() {
        let tmp = std::env::temp_dir().join(format!("izarra_croot_{}", std::process::id()));
        // --portable puts the C: drive beside the executable.
        let got = portable_c_root_in(&tmp);
        assert_eq!(got, tmp.join("c_drive"));
        assert!(got.is_dir());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn default_c_root_uses_home_and_creates() {
        let tmp = std::env::temp_dir().join(format!("izarra_chome_{}", std::process::id()));
        let home = tmp.join("home");
        let got = default_c_root_in(&home);
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
    fn ax5d0a_sets_extended_error_record_for_ah59() {
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut kernel = DosKernel::new();
        let base = 0x0100usize * 16 + 0x0200;
        mem.write_u16(base, 0x0053).unwrap(); // AX: fail on INT 24h
        mem.write_u16(base + 2, 0x0907).unwrap(); // BH class, BL action
        mem.write_u16(base + 4, 0x0400).unwrap(); // CH locus
        mem.write_u16(base + 10, 0x3456).unwrap(); // DI
        mem.write_u16(base + 14, 0xabcd).unwrap(); // ES

        let mut set = DosRegs {
            ax: 0x5d0a,
            ds: 0x0100,
            dx: 0x0200,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set, &mut mem).unwrap();
        assert!(!set.cf);

        let mut err = DosRegs {
            ax: 0x5900,
            cx: 0x00aa,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut err, &mut mem).unwrap();
        assert_eq!(err.ax, 0x0053);
        assert_eq!(err.bx, 0x0907);
        assert_eq!(err.cx, 0x04aa);
        assert_eq!(err.es, 0xabcd);
        assert_eq!(err.di, 0x3456);
        assert!(!err.cf);
    }

    #[test]
    fn ax5d_share_and_redirected_printer_helpers_report_absent_services() {
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut kernel = DosKernel::new();

        let mut commit = DosRegs {
            ax: 0x5d01,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut commit, &mut mem).unwrap();
        assert!(!commit.cf);

        for ax in [0x5d02, 0x5d03, 0x5d04, 0x5d07, 0x5d08, 0x5d09] {
            let mut regs = DosRegs {
                ax,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
            assert!(regs.cf, "AX={ax:04X}h fails without SHARE/redirector");
            assert_eq!(regs.ax, 0x0001);
        }

        let mut list = DosRegs {
            ax: 0x5d05,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut list, &mut mem).unwrap();
        assert!(list.cf);
        assert_eq!(list.ax, 0x0012, "no SHARE open-file list entries");

        let mut unknown = DosRegs {
            ax: 0x5dff,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut unknown, &mut mem).unwrap();
        assert!(unknown.cf);
        assert_eq!(unknown.ax, 0x0001);
    }

    #[test]
    fn network_services_report_no_redirector() {
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut kernel = DosKernel::new();

        let mut machine_name = DosRegs {
            ax: 0x5e00,
            cx: 0xff42,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut machine_name, &mut mem).unwrap();
        assert!(!machine_name.cf);
        assert_eq!(machine_name.cx, 0x0042, "CH=0 marks the name invalid");

        for ax in [0x5e02, 0x5e03, 0x5f02, 0x5f03, 0x5f04] {
            let mut regs = DosRegs {
                ax,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
            assert!(regs.cf, "AX={ax:04x} must fail without MS Networks");
            assert_eq!(regs.ax, 0x01);
        }

        let mut network_err = DosRegs {
            ax: 0x5900,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut network_err, &mut mem).unwrap();
        assert_eq!(network_err.ax, 0x01);

        for ax in [0x5f07, 0x5f08] {
            let mut c_drive = DosRegs {
                ax,
                dx: 2,
                cf: true,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut c_drive, &mut mem).unwrap();
            assert!(!c_drive.cf, "AX={ax:04x} accepts a LASTDRIVE-backed C:");

            let mut out_of_range = DosRegs {
                ax,
                dx: 25,
                ..DosRegs::default()
            };
            kernel.dispatch(0x21, &mut out_of_range, &mut mem).unwrap();
            assert!(out_of_range.cf, "AX={ax:04x} rejects drives past LASTDRIVE");
            assert_eq!(out_of_range.ax, 0x0f);
        }

        let mut err = DosRegs {
            ax: 0x5900,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut err, &mut mem).unwrap();
        assert_eq!(err.ax, 0x0f);
    }

    #[test]
    fn ax5e_gets_sets_and_undefines_machine_name() {
        let mut mem = Memory::new(64 * 1024).unwrap();
        let mut kernel = DosKernel::new();
        let base = 0x0100usize * 16 + 0x0200;
        for (index, byte) in b"TOKADOS".iter().enumerate() {
            mem.write_u8(base + index, *byte).unwrap();
        }
        mem.write_u8(base + 7, 0).unwrap();

        let mut set = DosRegs {
            ax: 0x5e01,
            cx: 0x0107,
            ds: 0x0100,
            dx: 0x0200,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut set, &mut mem).unwrap();
        assert!(!set.cf);

        let mut get = DosRegs {
            ax: 0x5e00,
            cx: 0xff00,
            ds: 0x0100,
            dx: 0x0300,
            cf: true,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get, &mut mem).unwrap();
        let out = 0x0100usize * 16 + 0x0300;
        assert!(!get.cf);
        assert_eq!(get.cx, 0x0107);
        let raw: Vec<u8> = (0..DOS_MACHINE_NAME_LEN)
            .map(|i| mem.read_u8(out + i).unwrap())
            .collect();
        assert_eq!(&raw, b"TOKADOS        ");
        assert_eq!(mem.read_u8(out + DOS_MACHINE_NAME_LEN).unwrap(), 0);

        let mut undefine = DosRegs {
            ax: 0x5e01,
            cx: 0x0007,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut undefine, &mut mem).unwrap();
        assert!(!undefine.cf);

        let mut get_invalid = DosRegs {
            ax: 0x5e00,
            cx: 0xff55,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut get_invalid, &mut mem).unwrap();
        assert!(!get_invalid.cf);
        assert_eq!(get_invalid.cx, 0x0055);
    }

    fn create_temp(kernel: &mut DosKernel, mem: &mut Memory) -> DosRegs {
        let mut regs = DosRegs {
            ax: 0x5a00,
            cx: 0,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, mem).unwrap();
        regs
    }

    fn read_guest_asciiz(mem: &Memory, seg: u16, off: u16) -> String {
        let base = usize::from(seg) * 16 + usize::from(off);
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
        out
    }

    fn assert_temp_leaf_shape(name: &str) {
        assert_eq!(name.len(), 8, "temp leaf was {name}");
        assert!(
            name.bytes().all(|byte| (b'A'..=b'P').contains(&byte)),
            "temp leaf was {name}"
        );
    }

    #[test]
    fn ah5a_creates_a_unique_temp_file_and_appends_the_name() {
        // DS:DX points at the directory path "C:\" (ending in '\'). The handler
        // appends a generated name and creates it create-exclusive.
        let (mut kernel, mut mem, dir) = kernel_with_drive(&[], "C:\\");
        let regs = create_temp(&mut kernel, &mut mem);
        assert!(!regs.cf, "create temp failed: ax={:#06x}", regs.ax);
        assert!(regs.ax >= 5);
        // Read the full ASCIIZ path back from DS:DX: it starts with "C:\" and the
        // appended name names a file that now exists on the host.
        let path = read_guest_asciiz(&mem, 0x0100, 0x0200);
        assert!(path.starts_with("C:\\"), "path was {path}");
        let host_name = &path[3..]; // strip "C:\"
        assert_temp_leaf_shape(host_name);
        assert!(dir.path().join(host_name).exists(), "missing {host_name}");
    }

    #[test]
    fn ah5a_inserts_a_missing_trailing_backslash() {
        let (mut kernel, mut mem, dir) = kernel_with_drive(&[], r"C:\TMP");
        std::fs::create_dir(dir.path().join("TMP")).unwrap();

        let regs = create_temp(&mut kernel, &mut mem);

        assert!(!regs.cf, "create temp failed: ax={:#06x}", regs.ax);
        let path = read_guest_asciiz(&mem, 0x0100, 0x0200);
        assert!(path.starts_with(r"C:\TMP\"), "path was {path}");
        let host_name = &path[r"C:\TMP\".len()..];
        assert_temp_leaf_shape(host_name);
        assert!(
            dir.path().join("TMP").join(host_name).exists(),
            "missing {host_name}"
        );
    }

    #[test]
    fn ah6c_opens_an_existing_file_and_creates_a_new_one() {
        let (mut kernel, mut mem, _dir) = kernel_with_drive(&[], r"C:\EA.TXT");
        let mut ea_open = DosRegs {
            ax: 0x6c01,
            ds: 0x0100,
            si: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut ea_open, &mut mem).unwrap();
        assert!(ea_open.cf);
        assert_eq!(ea_open.ax, 0x0001);

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
    fn ah6c_honors_the_readonly_create_attribute() {
        let (mut kernel, mut mem, dir) = kernel_with_drive(&[], r"C:\RO6C.TXT");
        let mut regs = DosRegs {
            ax: 0x6c00,
            bx: 0x0002,
            cx: 0x0001,
            dx: 0x0010,
            ds: 0x0100,
            si: 0x0200,
            ..DosRegs::default()
        };
        kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf, "extended create failed: ax={:#06x}", regs.ax);
        assert_eq!(regs.cx, 2);

        let path = dir.path().join("RO6C.TXT");
        assert!(
            std::fs::metadata(path).unwrap().permissions().readonly(),
            "CX bit 0 creates a read-only host file"
        );
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

    // --- Slice 4b: character-device I/O routing -----------------------------

    /// Stage a TESTDEV-style raw `.SYS` image with the given 8-byte name and
    /// attributes, simulate a successful INIT, and finalize it so loaded_devices
    /// lists its header. The header lands at driver_seg:0. Returns that header far
    /// pointer.
    fn stage_and_finalize_inline_device_with_attr(
        dos: &mut DosKernel,
        mem: &mut Memory,
        name: &[u8; 8],
        attributes: u16,
    ) -> FarPtr {
        let mut image = driver::tests_char_image();
        image[4..6].copy_from_slice(&attributes.to_le_bytes());
        image[10..18].copy_from_slice(name);
        let staged = dos
            .stage_sys_driver(&image, "", DriverLoadPlacement::Low, mem)
            .unwrap();
        // Simulate a successful INIT: status DONE, break one paragraph of resident.
        mem.write_u16(staged.request_linear + 0x03, 0x0100).unwrap();
        mem.write_u16(staged.request_linear + 0x0e, 0x0010).unwrap();
        mem.write_u16(staged.request_linear + 0x10, staged.driver_seg)
            .unwrap();
        dos.finalize_sys_driver(&staged, mem).unwrap();
        FarPtr {
            segment: staged.driver_seg,
            offset: 0x0000,
        }
    }

    fn stage_and_finalize_inline_device(
        dos: &mut DosKernel,
        mem: &mut Memory,
        name: &[u8; 8],
    ) -> FarPtr {
        stage_and_finalize_inline_device_with_attr(dos, mem, name, 0x8000)
    }

    /// Write an ASCIIZ string into guest memory at seg:off.
    fn write_asciiz(mem: &mut Memory, seg: u16, off: u16, text: &str) {
        let base = usize::from(seg) * 16 + usize::from(off);
        for (i, b) in text.bytes().enumerate() {
            mem.write_u8(base + i, b).unwrap();
        }
        mem.write_u8(base + text.len(), 0).unwrap();
    }

    /// AH=3Dh open registers: DS:DX = ASCIIZ name, AL = access mode.
    fn open_regs(ds: u16, dx: u16, al: u8) -> DosRegs {
        DosRegs {
            ax: 0x3d00 | u16::from(al),
            ds,
            dx,
            ..DosRegs::default()
        }
    }

    #[test]
    fn open_a_loaded_character_device_returns_a_tracked_handle() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut dos = DosKernel::new();
        dos.init_program(0x0100, 0x1100, &mut mem).unwrap();
        let header = stage_and_finalize_inline_device(&mut dos, &mut mem, b"MYDEV   ");
        write_asciiz(&mut mem, 0x0100, 0x0080, "MYDEV");
        let mut regs = open_regs(0x0100, 0x0080, 2);
        let action = dos.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::Continue);
        assert!(!regs.cf);
        let handle = regs.ax;
        assert_eq!(dos.device_handle_header_for_test(handle), Some(header));
    }

    #[test]
    fn open_a_device_with_open_close_bit_requests_driver_open() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut dos = DosKernel::new();
        dos.init_program(0x0100, 0x1100, &mut mem).unwrap();
        let header =
            stage_and_finalize_inline_device_with_attr(&mut dos, &mut mem, b"OPENCLOS", 0x8800);
        write_asciiz(&mut mem, 0x0100, 0x0080, "OPENCLOS");

        let mut regs = open_regs(0x0100, 0x0080, 2);
        let action = dos.dispatch(0x21, &mut regs, &mut mem).unwrap();

        assert!(!regs.cf);
        let handle = regs.ax;
        assert_eq!(dos.device_handle_header_for_test(handle), Some(header));
        match action {
            DosAction::CallDevice {
                header: h,
                command,
                transfer,
                count,
                success_ax,
                rollback_handle_on_error,
            } => {
                assert_eq!(h, header);
                assert_eq!(command, 0x0d, "command 0Dh is DEVICE OPEN");
                assert_eq!(transfer, FarPtr::default());
                assert_eq!(count, 0);
                assert_eq!(success_ax, Some(handle));
                assert_eq!(rollback_handle_on_error, Some(handle));
            }
            other => panic!("expected CallDevice for DEVICE OPEN, got {other:?}"),
        }
    }

    #[test]
    fn open_a_device_name_with_path_and_extension_still_opens_it() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut dos = DosKernel::new();
        dos.init_program(0x0100, 0x1100, &mut mem).unwrap();
        let header = stage_and_finalize_inline_device(&mut dos, &mut mem, b"MYDEV   ");
        // DOS opens a device by base name regardless of path or extension.
        write_asciiz(&mut mem, 0x0100, 0x0080, "C:\\DEV\\mydev.xyz");
        let mut regs = open_regs(0x0100, 0x0080, 0);
        dos.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        assert_eq!(dos.device_handle_header_for_test(regs.ax), Some(header));
    }

    #[test]
    fn close_removes_a_device_handle() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut dos = DosKernel::new();
        dos.init_program(0x0100, 0x1100, &mut mem).unwrap();
        stage_and_finalize_inline_device(&mut dos, &mut mem, b"MYDEV   ");
        write_asciiz(&mut mem, 0x0100, 0x0080, "MYDEV");
        let mut regs = open_regs(0x0100, 0x0080, 2);
        dos.dispatch(0x21, &mut regs, &mut mem).unwrap();
        let handle = regs.ax;
        assert!(dos.device_handle_header_for_test(handle).is_some());

        let mut close = DosRegs {
            ax: 0x3e00,
            bx: handle,
            ..DosRegs::default()
        };
        let action = dos.dispatch(0x21, &mut close, &mut mem).unwrap();
        assert!(!close.cf);
        assert!(dos.device_handle_header_for_test(handle).is_none());
        assert_eq!(action, DosAction::Continue);
    }

    #[test]
    fn close_a_device_with_open_close_bit_requests_driver_close() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut dos = DosKernel::new();
        dos.init_program(0x0100, 0x1100, &mut mem).unwrap();
        let header =
            stage_and_finalize_inline_device_with_attr(&mut dos, &mut mem, b"OPENCLOS", 0x8800);
        write_asciiz(&mut mem, 0x0100, 0x0080, "OPENCLOS");
        let mut open = open_regs(0x0100, 0x0080, 2);
        dos.dispatch(0x21, &mut open, &mut mem).unwrap();
        let handle = open.ax;

        let mut close = DosRegs {
            ax: 0x3e7b,
            bx: handle,
            ..DosRegs::default()
        };
        let action = dos.dispatch(0x21, &mut close, &mut mem).unwrap();

        assert!(!close.cf);
        assert!(dos.device_handle_header_for_test(handle).is_none());
        match action {
            DosAction::CallDevice {
                header: h,
                command,
                transfer,
                count,
                success_ax,
                rollback_handle_on_error,
            } => {
                assert_eq!(h, header);
                assert_eq!(command, 0x0e, "command 0Eh is DEVICE CLOSE");
                assert_eq!(transfer, FarPtr::default());
                assert_eq!(count, 0);
                assert_eq!(success_ax, Some(0x3e7b));
                assert_eq!(rollback_handle_on_error, None);
            }
            other => panic!("expected CallDevice for DEVICE CLOSE, got {other:?}"),
        }
    }

    #[test]
    fn alloc_handle_skips_device_handles() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut dos = DosKernel::new();
        dos.init_program(0x0100, 0x1100, &mut mem).unwrap();
        // Reserve handle 5 as a device handle, then assert alloc_handle never returns it.
        dos.device_handles.insert(
            5,
            OpenDeviceHandle {
                header: FarPtr {
                    segment: 0x2000,
                    offset: 0,
                },
                mode: AccessMode::ReadWrite,
            },
        );
        let next = dos.alloc_handle().unwrap();
        assert_ne!(next, 5, "alloc_handle must not reuse a live device handle");
    }

    /// Set up a kernel with MYDEV loaded and open it with the given access mode,
    /// returning the dos kernel, memory, the handle, and the device header.
    fn dos_with_open_device(al: u8) -> (DosKernel, Memory, u16, FarPtr) {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut dos = DosKernel::new();
        dos.init_program(0x0100, 0x1100, &mut mem).unwrap();
        let header = stage_and_finalize_inline_device(&mut dos, &mut mem, b"MYDEV   ");
        write_asciiz(&mut mem, 0x0100, 0x0080, "MYDEV");
        let mut regs = open_regs(0x0100, 0x0080, al);
        dos.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        (dos, mem, regs.ax, header)
    }

    #[test]
    fn read_on_a_device_handle_returns_a_call_device_action() {
        let (mut dos, mut mem, handle, header) = dos_with_open_device(2);
        let mut regs = DosRegs {
            ax: 0x3f00,
            bx: handle,
            cx: 4,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        let action = dos.dispatch(0x21, &mut regs, &mut mem).unwrap();
        match action {
            DosAction::CallDevice {
                header: h,
                command,
                transfer,
                count,
                success_ax,
                rollback_handle_on_error,
            } => {
                assert_eq!(command, 4);
                assert_eq!(count, 4);
                assert_eq!(success_ax, None);
                assert_eq!(rollback_handle_on_error, None);
                assert_eq!(
                    transfer,
                    FarPtr {
                        segment: 0x0100,
                        offset: 0x0200
                    }
                );
                assert_eq!(h, header);
            }
            other => panic!("expected CallDevice, got {other:?}"),
        }
    }

    #[test]
    fn write_on_a_device_handle_returns_a_call_device_action() {
        let (mut dos, mut mem, handle, header) = dos_with_open_device(2);
        let mut regs = DosRegs {
            ax: 0x4000,
            bx: handle,
            cx: 3,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        let action = dos.dispatch(0x21, &mut regs, &mut mem).unwrap();
        match action {
            DosAction::CallDevice {
                header: h,
                command,
                transfer,
                count,
                success_ax,
                rollback_handle_on_error,
            } => {
                assert_eq!(command, 8);
                assert_eq!(count, 3);
                assert_eq!(success_ax, None);
                assert_eq!(rollback_handle_on_error, None);
                assert_eq!(
                    transfer,
                    FarPtr {
                        segment: 0x0100,
                        offset: 0x0300
                    }
                );
                assert_eq!(h, header);
            }
            other => panic!("expected CallDevice, got {other:?}"),
        }
    }

    #[test]
    fn write_on_a_read_only_device_handle_is_access_denied() {
        let (mut dos, mut mem, handle, _) = dos_with_open_device(0); // read-only
        let mut regs = DosRegs {
            ax: 0x4000,
            bx: handle,
            cx: 3,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        let action = dos.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::Continue);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x05); // access denied
    }

    #[test]
    fn read_on_a_write_only_device_handle_is_access_denied() {
        let (mut dos, mut mem, handle, _) = dos_with_open_device(1); // write-only
        let mut regs = DosRegs {
            ax: 0x3f00,
            bx: handle,
            cx: 4,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        let action = dos.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::Continue);
        assert!(regs.cf);
        assert_eq!(regs.ax, 0x05);
    }

    #[test]
    fn zero_count_read_on_a_device_short_circuits() {
        let (mut dos, mut mem, handle, _) = dos_with_open_device(2);
        let mut regs = DosRegs {
            ax: 0x3f00,
            bx: handle,
            cx: 0,
            ds: 0x0100,
            dx: 0x0200,
            ..DosRegs::default()
        };
        let action = dos.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::Continue);
        assert!(!regs.cf);
        assert_eq!(regs.ax, 0);
    }

    #[test]
    fn ioctl_get_device_data_on_a_device_reports_a_character_device() {
        let (mut dos, mut mem, handle, _) = dos_with_open_device(2);
        let mut regs = DosRegs {
            ax: 0x4400, // AH=44h AL=00h get device data
            bx: handle,
            ..DosRegs::default()
        };
        let action = dos.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::Continue);
        assert!(!regs.cf);
        // ISDEV (bit 7) set, no other capability bit: the stage_and_finalize fixture
        // header has attribute 0x8000 (character, no IOCTL), so the info word is
        // exactly ISDEV.
        assert_eq!(
            regs.dx, 0x0080,
            "info word is exactly ISDEV; got {:#06x}",
            regs.dx
        );
    }

    #[test]
    fn ioctl_get_device_data_reflects_the_driver_ioctl_attribute_bit() {
        // A device whose header attribute sets bit 14 (0x4000, supports IOCTL) must
        // be reflected in the info word's bit 14, proving the attribute is read from
        // the header and not fabricated.
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut dos = DosKernel::new();
        dos.init_program(0x0100, 0x1100, &mut mem).unwrap();
        let header = stage_and_finalize_inline_device(&mut dos, &mut mem, b"IOCTLDEV");
        // Set the supports-IOCTL bit in the live driver header attribute word.
        let base = usize::from(header.segment) * 16 + usize::from(header.offset);
        let attr = mem.read_u16(base + 4).unwrap();
        mem.write_u16(base + 4, attr | 0x4000).unwrap();

        write_asciiz(&mut mem, 0x0100, 0x0080, "IOCTLDEV");
        let mut open = open_regs(0x0100, 0x0080, 2);
        dos.dispatch(0x21, &mut open, &mut mem).unwrap();
        assert!(!open.cf);
        let handle = open.ax;

        let mut regs = DosRegs {
            ax: 0x4400,
            bx: handle,
            ..DosRegs::default()
        };
        dos.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf);
        // ISDEV (bit 7 = 0x80) plus supports-IOCTL (bit 14 = 0x4000).
        assert_eq!(
            regs.dx, 0x4080,
            "ISDEV + supports-IOCTL; got {:#06x}",
            regs.dx
        );
    }

    #[test]
    fn device_name_key_strips_path_drive_and_extension() {
        // A bare name, a full path, a bare drive specifier, and a name with an
        // extension all key to the same device base name.
        assert_eq!(device_name_key("TESTDEV"), "TESTDEV");
        assert_eq!(device_name_key("\\DEV\\TESTDEV"), "TESTDEV");
        assert_eq!(device_name_key("C:\\DEV\\TESTDEV"), "TESTDEV");
        assert_eq!(device_name_key("TESTDEV.XYZ"), "TESTDEV");
        // The fix: a bare "X:NAME" drive specifier with no path separator.
        assert_eq!(device_name_key("C:TESTDEV"), "TESTDEV");
        assert_eq!(device_name_key("c:testdev.sys"), "TESTDEV");
    }

    #[test]
    fn open_a_device_with_a_bare_drive_prefix_still_opens_it() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut dos = DosKernel::new();
        dos.init_program(0x0100, 0x1100, &mut mem).unwrap();
        let header = stage_and_finalize_inline_device(&mut dos, &mut mem, b"MYDEV   ");
        // "C:MYDEV" has a drive specifier but no path separator.
        write_asciiz(&mut mem, 0x0100, 0x0080, "C:MYDEV");
        let mut regs = open_regs(0x0100, 0x0080, 2);
        dos.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert!(!regs.cf, "open failed: ax={:#06x}", regs.ax);
        assert_eq!(dos.device_handle_header_for_test(regs.ax), Some(header));
    }

    #[test]
    fn forcedup_onto_a_device_handle_drops_the_device_entry() {
        // AH=46h FORCEDUP of a file handle onto an open device handle must remove
        // the device entry, so later I/O on that number routes to the file, not the
        // dead driver. Without the fix the device entry shadows the file (read/write
        // check device_handles first) and the entry leaks on close.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("DATA.TXT"), b"abc").unwrap();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut dos = DosKernel::new();
        dos.mount_c(HostDrive::mount_c(dir.path()).unwrap());
        dos.init_program(0x0100, 0x1100, &mut mem).unwrap();
        stage_and_finalize_inline_device(&mut dos, &mut mem, b"MYDEV   ");

        // Open the device on some handle, then open a host file on another.
        write_asciiz(&mut mem, 0x0100, 0x0080, "MYDEV");
        let mut dev_open = open_regs(0x0100, 0x0080, 2);
        dos.dispatch(0x21, &mut dev_open, &mut mem).unwrap();
        let dev_handle = dev_open.ax;
        assert!(dos.device_handle_header_for_test(dev_handle).is_some());

        write_asciiz(&mut mem, 0x0100, 0x0090, "C:\\DATA.TXT");
        let mut file_open = open_regs(0x0100, 0x0090, 0);
        dos.dispatch(0x21, &mut file_open, &mut mem).unwrap();
        let file_handle = file_open.ax;

        // FORCEDUP the file handle onto the device handle number.
        let mut dup = DosRegs {
            ax: 0x4600,
            bx: file_handle,
            cx: dev_handle,
            ..DosRegs::default()
        };
        dos.dispatch(0x21, &mut dup, &mut mem).unwrap();
        assert!(!dup.cf);

        // The device entry is gone: I/O on that number now takes the file path.
        assert!(dos.device_handle_header_for_test(dev_handle).is_none());
        let mut read = DosRegs {
            ax: 0x3f00,
            bx: dev_handle,
            cx: 3,
            ds: 0x0100,
            dx: 0x0300,
            ..DosRegs::default()
        };
        let action = dos.dispatch(0x21, &mut read, &mut mem).unwrap();
        assert_eq!(
            action,
            DosAction::Continue,
            "read takes the file path, not CallDevice"
        );
        assert!(!read.cf);
        assert_eq!(read.ax, 3, "three bytes read from the file");
    }

    #[test]
    fn exec_inherits_a_parent_device_handle_and_drops_a_child_only_one() {
        // Mirror the open-file inheritance test for device handles: a device handle
        // opened in the parent survives the EXEC clone and restore, while a device
        // handle opened only in the child is gone after finish_exec.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CHILD.COM"), [0xcdu8, 0x20]).unwrap();
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut kernel = DosKernel::new();
        kernel.init_program(0x0100, 0x1100, &mut mem).unwrap();
        kernel.mount_c(HostDrive::mount_c(dir.path()).unwrap());
        let parent_header = stage_and_finalize_inline_device(&mut kernel, &mut mem, b"PARENT  ");
        write_asciiz(&mut mem, 0x0100, 0x0200, "PARENT");
        let mut popen = open_regs(0x0100, 0x0200, 2);
        kernel.dispatch(0x21, &mut popen, &mut mem).unwrap();
        let parent_handle = popen.ax;
        assert_eq!(
            kernel.device_handle_header_for_test(parent_handle),
            Some(parent_header)
        );

        place_exec_inputs(&mut mem, "C:\\CHILD.COM", 0);
        let exec = exec_al0(&mut kernel, &mut mem);
        assert!(!exec.cf);
        // The child inherits the parent's device handle through the clone.
        assert_eq!(
            kernel.device_handle_header_for_test(parent_handle),
            Some(parent_header),
            "child inherits the parent device handle"
        );
        // Register a second device handle only in the child. A child PSP fills
        // conventional memory, so staging a real second driver has no room; insert
        // the handle directly, which is what the open path would record.
        let child_header = FarPtr {
            segment: 0x2000,
            offset: 0,
        };
        let child_handle = kernel.alloc_handle().unwrap();
        assert_ne!(child_handle, parent_handle);
        kernel.device_handles.insert(
            child_handle,
            OpenDeviceHandle {
                header: child_header,
                mode: AccessMode::ReadWrite,
            },
        );
        assert_eq!(
            kernel.device_handle_header_for_test(child_handle),
            Some(child_header)
        );

        kernel.finish_exec(0, &mut mem).unwrap();

        // The parent's device handle survives; the child-only one is dropped.
        assert_eq!(
            kernel.device_handle_header_for_test(parent_handle),
            Some(parent_header),
            "parent device handle survives the child exit"
        );
        assert!(
            kernel.device_handle_header_for_test(child_handle).is_none(),
            "child-only device handle is gone after finish_exec"
        );
    }
}
