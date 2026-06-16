use izarravm_bus::{BusError, Memory};
use std::collections::VecDeque;
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

/// Service a software interrupt the DOS kernel handles. `vector` is the INT number
/// (0x20 terminate, 0x21 the AH-dispatched function set). Reads and writes `regs`,
/// reads guest memory through `mem`, and appends console output to `stdout`.
///
/// DOS services are emulated host-side (HLE), the standard approach for a machine
/// with no resident DOS. Unimplemented INT 21h functions return Continue with no
/// effect, so the caller's IRET stub returns cleanly; later slices fill them in.
///
/// `mem` is `&mut` because the file-read call (AH=3Fh, a later slice) writes the
/// data it reads back into guest memory at DS:DX; the print call only reads.
pub fn dispatch(
    vector: u8,
    regs: &mut DosRegs,
    mem: &mut Memory,
    stdin: &mut VecDeque<u8>,
    stdout: &mut Vec<u8>,
) -> Result<DosAction, DosError> {
    match vector {
        0x20 => Ok(DosAction::Exit(0)),
        0x21 => dispatch_int21(regs, mem, stdin, stdout),
        // The machine only records 0x10/0x20/0x21 and routes 0x10 elsewhere, so
        // this is unreachable today. Treat it as a no-op rather than panic.
        _ => Ok(DosAction::Continue),
    }
}

fn dispatch_int21(
    regs: &mut DosRegs,
    mem: &mut Memory,
    stdin: &mut VecDeque<u8>,
    stdout: &mut Vec<u8>,
) -> Result<DosAction, DosError> {
    let ah = (regs.ax >> 8) as u8;
    match ah {
        // AH=01h: read one character with echo. A real keyboard blocks; with a
        // preloaded buffer an empty buffer yields the redirected-input EOF byte ^Z.
        0x01 => {
            let ch = stdin.pop_front().unwrap_or(0x1a);
            stdout.push(ch);
            regs.ax = (regs.ax & 0xff00) | u16::from(ch);
            Ok(DosAction::Continue)
        }
        // AH=02h: write the byte in DL to standard output. AL returns it (DOS 2+).
        0x02 => {
            let ch = regs.dx as u8;
            stdout.push(ch);
            regs.ax = (regs.ax & 0xff00) | u16::from(ch);
            Ok(DosAction::Continue)
        }
        // AH=06h: direct console I/O. DL=0xFF reads without waiting (ZF reports
        // whether a character was ready); any other DL writes DL.
        0x06 => {
            if regs.dx as u8 == 0xff {
                match stdin.pop_front() {
                    Some(ch) => {
                        regs.ax = (regs.ax & 0xff00) | u16::from(ch);
                        regs.zf = false;
                    }
                    None => regs.zf = true,
                }
            } else {
                let ch = regs.dx as u8;
                stdout.push(ch);
                regs.ax = (regs.ax & 0xff00) | u16::from(ch);
            }
            Ok(DosAction::Continue)
        }
        // AH=08h: read one character without echo. ^Z on an empty buffer, as AH=01h.
        0x08 => {
            let ch = stdin.pop_front().unwrap_or(0x1a);
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
                stdout.push(byte);
                offset += 1;
            }
            // DOS returns AL = '$' (0x24) from AH=09h.
            regs.ax = (regs.ax & 0xff00) | 0x24;
            Ok(DosAction::Continue)
        }
        // AH=4Ch: terminate with the return code in AL.
        0x4c => Ok(DosAction::Exit((regs.ax & 0x00ff) as u8)),
        // Character I/O (01h/02h/06h/08h) and file I/O (3Dh/3Fh/3Eh) are later
        // slices. An unimplemented function is a no-op so the IRET stub returns.
        _ => Ok(DosAction::Continue),
    }
}

/// The largest .COM image: a 64 KiB segment minus the 256-byte PSP.
const COM_MAX_LEN: usize = 0x10000 - 0x100;

/// Where to start executing a loaded .COM. All four segment registers point at the
/// load segment; the entry is at offset 0x100, the stack at 0xFFFE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramEntry {
    pub segment: u16, // CS = DS = ES = SS
    pub ip: u16,      // 0x0100
    pub sp: u16,      // 0xfffe
}

/// Load a .COM image into `mem` at `segment` and build its PSP. Returns the entry
/// state for the caller to apply to the CPU.
pub fn load_com(image: &[u8], mem: &mut Memory, segment: u16) -> Result<ProgramEntry, DosError> {
    if image.len() > COM_MAX_LEN {
        return Err(DosError::ComTooLarge(image.len()));
    }
    let base = usize::from(segment) * 16;
    // Minimal PSP. INT 20h (CD 20) at offset 0 so a near RET to PSP:0 terminates.
    mem.write_u8(base, 0xcd)?;
    mem.write_u8(base + 1, 0x20)?;
    // Offset 0x02: segment of the first byte past the program's 64 KiB block.
    mem.write_u16(base + 2, segment.wrapping_add(0x1000))?;
    // Offset 0x80: command tail. Empty here: length 0, then a CR. The environment
    // segment, parent PSP, default FCBs, and DTA are left zero (later slices).
    mem.write_u8(base + 0x80, 0x00)?;
    mem.write_u8(base + 0x81, 0x0d)?;
    // Program image at offset 0x100.
    for (index, &byte) in image.iter().enumerate() {
        mem.write_u8(base + 0x100 + index, byte)?;
    }
    // .COM stack: SP=0xFFFE with a 0x0000 return word, so a bare RET lands at
    // PSP:0 and hits the INT 20h. Written after the image, so a maximum-size image
    // has its last two bytes overwritten by this word, which is what real DOS does.
    mem.write_u16(base + 0xfffe, 0x0000)?;
    Ok(ProgramEntry {
        segment,
        ip: 0x0100,
        sp: 0xfffe,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut stdin = VecDeque::new();
        let mut out = Vec::new();
        let action = dispatch(0x21, &mut regs, &mut mem, &mut stdin, &mut out).unwrap();
        assert_eq!(action, DosAction::Continue);
        assert_eq!(out, b"Hello");
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
        let mut stdin = VecDeque::new();
        let mut out = Vec::new();
        assert!(matches!(
            dispatch(0x21, &mut regs, &mut mem, &mut stdin, &mut out),
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
        let mut stdin = VecDeque::new();
        let mut out = Vec::new();
        dispatch(0x21, &mut regs, &mut mem, &mut stdin, &mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn ah4c_exits_with_al_code() {
        let mut mem = Memory::new(4096).unwrap();
        let mut regs = DosRegs {
            ax: 0x4c07,
            ..DosRegs::default()
        };
        let mut stdin = VecDeque::new();
        let mut out = Vec::new();
        assert_eq!(
            dispatch(0x21, &mut regs, &mut mem, &mut stdin, &mut out).unwrap(),
            DosAction::Exit(7)
        );
    }

    #[test]
    fn int20_exits_with_zero() {
        let mut mem = Memory::new(4096).unwrap();
        let mut regs = DosRegs::default();
        let mut stdin = VecDeque::new();
        let mut out = Vec::new();
        assert_eq!(
            dispatch(0x20, &mut regs, &mut mem, &mut stdin, &mut out).unwrap(),
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
        let mut stdin = VecDeque::new();
        let mut out = Vec::new();
        let action = dispatch(0x21, &mut regs, &mut mem, &mut stdin, &mut out).unwrap();
        assert_eq!(action, DosAction::Continue);
        assert!(out.is_empty());
    }

    #[test]
    fn load_com_builds_psp_and_entry() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let image = [0xb8, 0x00, 0x4c, 0xcd, 0x21]; // mov ax,4c00; int 21
        let entry = load_com(&image, &mut mem, 0x0100).unwrap();
        assert_eq!(
            entry,
            ProgramEntry {
                segment: 0x0100,
                ip: 0x0100,
                sp: 0xfffe
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
        let mut stdin: VecDeque<u8> = input.iter().copied().collect();
        let mut stdout = Vec::new();
        let mut regs = DosRegs {
            ax,
            dx,
            ..DosRegs::default()
        };
        let action = dispatch(0x21, &mut regs, &mut mem, &mut stdin, &mut stdout).unwrap();
        assert_eq!(action, DosAction::Continue);
        (regs, stdout) // DosRegs is Copy and holds the post-dispatch AX/ZF
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
    fn ah06_output_writes_dl() {
        let (regs, out) = char_io(0x0600, 0x0042, b""); // AH=06h, DL='B' (not 0xFF)
        assert_eq!(out, b"B");
        assert_eq!(regs.ax & 0x00ff, 0x42);
    }

    #[test]
    fn ah06_input_available_clears_zf() {
        let (regs, out) = char_io(0x0600, 0x00ff, b"X"); // AH=06h, DL=0xFF
        assert_eq!(regs.ax & 0x00ff, 0x58); // AL = 'X'
        assert!(!regs.zf); // character available
        assert!(out.is_empty()); // no echo
    }

    #[test]
    fn ah06_input_empty_sets_zf() {
        let (regs, out) = char_io(0x0600, 0x00ff, b""); // AH=06h, DL=0xFF, empty
        assert!(regs.zf); // no character ready
        assert!(out.is_empty());
    }
}
