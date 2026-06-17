use izarravm_bus::{BusError, Memory};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::Read;
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
}

/// The stateful DOS kernel. Owns the host-side state that must survive between
/// INT 21h calls: the open-file handle table and the mounted C: drive, plus the
/// standard input and output buffers (high-level emulated, HLE). The machine
/// holds one of these and calls `dispatch` from its INT 21h handler.
#[derive(Debug, Default)]
pub struct DosKernel {
    drive: Option<HostDrive>,
    // File handles 5 and up: AH=3Dh inserts, AH=3Fh/3Eh look up.
    open_files: HashMap<u16, File>,
    stdin: VecDeque<u8>,
    stdout: Vec<u8>,
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
            // AH=3Dh: open an existing file at DS:DX (ASCIIZ) for reading. AL is
            // the access mode but is not enforced: every open is read-only,
            // because writes (AH=40h) are a later slice. Returns CF=0 + AX=handle
            // on success, CF=1 + AX=DOS code on error.
            0x3d => {
                let name = match read_asciiz(mem, regs.ds, regs.dx)? {
                    Some(name) => name,
                    None => {
                        set_dos_error(regs, 0x03);
                        return Ok(DosAction::Continue);
                    }
                };
                let path = match &self.drive {
                    None => {
                        set_dos_error(regs, 0x02);
                        return Ok(DosAction::Continue);
                    }
                    Some(drive) => match drive.resolve_dos_path(&name) {
                        Ok(path) => path,
                        Err(_) => {
                            set_dos_error(regs, 0x03);
                            return Ok(DosAction::Continue);
                        }
                    },
                };
                match File::open(&path) {
                    Ok(file) => {
                        let handle = (5u16..)
                            .find(|h| !self.open_files.contains_key(h))
                            .expect("a free DOS handle exists at or below u16::MAX");
                        self.open_files.insert(handle, file);
                        regs.ax = handle;
                        regs.cf = false;
                    }
                    Err(err) => set_dos_error(regs, dos_io_error_code(&err)),
                }
                Ok(DosAction::Continue)
            }
            // AH=3Fh: read CX bytes from the handle in BX into the buffer at
            // DS:DX. Returns CF=0 + AX=bytes-read (0 = EOF) on success, CF=1 +
            // AX=0x06 for an unknown handle. A host read error maps to a DOS code;
            // a guest-memory write fault propagates as DosError::Memory.
            0x3f => {
                let handle = regs.bx;
                let count = usize::from(regs.cx);
                let Some(file) = self.open_files.get_mut(&handle) else {
                    set_dos_error(regs, 0x06);
                    return Ok(DosAction::Continue);
                };
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
            // AH=4Ch: terminate with the return code in AL.
            0x4c => Ok(DosAction::Exit((regs.ax & 0x00ff) as u8)),
            // Other file functions (write, seek, find) and everything else are not
            // yet implemented; later slices fill them in. An unimplemented function
            // returns Continue so the IRET stub returns to the caller.
            _ => Ok(DosAction::Continue),
        }
    }
}

/// The largest .COM image: a 64 KiB segment minus the 256-byte PSP.
const COM_MAX_LEN: usize = 0x10000 - 0x100;

/// Conventional memory is modeled as one block ending at the 640 KiB video
/// aperture (paragraph 0xA000). ponytail: single block, no MCB chain / EBDA /
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
/// command tail at 0x80. The environment segment, parent PSP, default FCBs, and
/// the DTA are left zero (later slices).
fn build_psp(mem: &mut Memory, psp_seg: u16, top_of_mem_paragraph: u16) -> Result<(), DosError> {
    let base = usize::from(psp_seg) * 16;
    mem.write_u8(base, 0xcd)?;
    mem.write_u8(base + 1, 0x20)?;
    mem.write_u16(base + 2, top_of_mem_paragraph)?;
    mem.write_u8(base + 0x80, 0x00)?;
    mem.write_u8(base + 0x81, 0x0d)?;
    Ok(())
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
    let module_len = module.len();
    let module_paras = module_len.div_ceil(16) as u32;

    let start_seg = psp_segment.wrapping_add(0x10);
    let needed = u32::from(start_seg) + module_paras + u32::from(e_minalloc);
    if needed > CONVENTIONAL_TOP_PARAGRAPH {
        return Err(DosError::ExeNotEnoughMemory {
            needed,
            available: CONVENTIONAL_TOP_PARAGRAPH,
        });
    }

    // Top of the program's block: honor e_maxalloc, clamp to conventional memory.
    let top_paragraph = (u32::from(start_seg) + module_paras + u32::from(e_maxalloc))
        .min(CONVENTIONAL_TOP_PARAGRAPH) as u16;
    build_psp(mem, psp_segment, top_paragraph)?;

    // Copy the load module to start_seg:0.
    let base = usize::from(start_seg) * 16;
    for (index, &byte) in module.iter().enumerate() {
        mem.write_u8(base + index, byte)?;
    }

    // Apply relocations: each (off, seg) names a word at module offset
    // seg*16+off; add start_seg so the segment reference points at the real load
    // address. ponytail: out-of-range relocations are rejected rather than
    // applied blindly as real DOS would (avoids corrupting arbitrary memory).
    let reloc_end = usize::from(e_lfarlc) + usize::from(e_crlc) * 4;
    if reloc_end > image.len() {
        return Err(DosError::ExeImageTruncated(
            "relocation table extends past the file",
        ));
    }
    for i in 0..usize::from(e_crlc) {
        let entry = usize::from(e_lfarlc) + i * 4;
        let off = u16::from_le_bytes([image[entry], image[entry + 1]]);
        let seg = u16::from_le_bytes([image[entry + 2], image[entry + 3]]);
        let module_offset = usize::from(seg) * 16 + usize::from(off);
        if module_offset + 2 > module_len {
            return Err(DosError::ExeRelocationOutOfRange);
        }
        let target = base + module_offset;
        let value = mem.read_u16(target)?;
        mem.write_u16(target, value.wrapping_add(start_seg))?;
    }

    Ok(ProgramEntry {
        cs: start_seg.wrapping_add(e_cs),
        ip: e_ip,
        ss: start_seg.wrapping_add(e_ss),
        sp: e_sp,
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
    // ponytail: detect "MZ" only; the rare "ZM" alternate signature is treated
    // as a .COM (no real DOS game ships "ZM").
    if image.len() >= 2 && image[0] == b'M' && image[1] == b'Z' {
        load_exe(image, mem, psp_segment)
    } else {
        load_com(image, mem, psp_segment)
    }
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
            ax: 0x3000, // AH=30h get DOS version, not implemented this slice
            ..DosRegs::default()
        };
        let mut kernel = DosKernel::new();
        kernel.set_stdin(b"");
        let action = kernel.dispatch(0x21, &mut regs, &mut mem).unwrap();
        assert_eq!(action, DosAction::Continue);
        assert!(kernel.stdout().is_empty());
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
}
