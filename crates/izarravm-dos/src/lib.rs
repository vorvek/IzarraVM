use izarravm_bus::{BusError, Memory};
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
pub fn dispatch(
    vector: u8,
    regs: &mut DosRegs,
    mem: &mut Memory,
    stdout: &mut Vec<u8>,
) -> Result<DosAction, DosError> {
    match vector {
        0x20 => Ok(DosAction::Exit(0)),
        0x21 => dispatch_int21(regs, mem, stdout),
        // The machine only records 0x10/0x20/0x21 and routes 0x10 elsewhere, so
        // this is unreachable today. Treat it as a no-op rather than panic.
        _ => Ok(DosAction::Continue),
    }
}

fn dispatch_int21(
    regs: &mut DosRegs,
    mem: &mut Memory,
    stdout: &mut Vec<u8>,
) -> Result<DosAction, DosError> {
    let ah = (regs.ax >> 8) as u8;
    match ah {
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
        let mut out = Vec::new();
        let action = dispatch(0x21, &mut regs, &mut mem, &mut out).unwrap();
        assert_eq!(action, DosAction::Continue);
        assert_eq!(out, b"Hello");
        assert_eq!(regs.ax & 0x00ff, 0x24); // AH=09h returns AL = '$'
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
        let mut out = Vec::new();
        dispatch(0x21, &mut regs, &mut mem, &mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn ah4c_exits_with_al_code() {
        let mut mem = Memory::new(4096).unwrap();
        let mut regs = DosRegs {
            ax: 0x4c07,
            ..DosRegs::default()
        };
        let mut out = Vec::new();
        assert_eq!(
            dispatch(0x21, &mut regs, &mut mem, &mut out).unwrap(),
            DosAction::Exit(7)
        );
    }

    #[test]
    fn int20_exits_with_zero() {
        let mut mem = Memory::new(4096).unwrap();
        let mut regs = DosRegs::default();
        let mut out = Vec::new();
        assert_eq!(
            dispatch(0x20, &mut regs, &mut mem, &mut out).unwrap(),
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
        let mut out = Vec::new();
        let action = dispatch(0x21, &mut regs, &mut mem, &mut out).unwrap();
        assert_eq!(action, DosAction::Continue);
        assert!(out.is_empty());
    }
}
