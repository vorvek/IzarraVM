// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::{beyond_extent, exe_directory};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn narrow(w: &[u16]) -> String {
    String::from_utf16_lossy(w)
}

#[test]
fn directory_is_the_path_up_to_the_last_separator_nul_terminated() {
    let dir = exe_directory(&wide(r"D:\dev\IzarraVM\target\profiling\izarravm.exe"))
        .expect("a normal exe path has a directory");
    assert_eq!(*dir.last().expect("non-empty"), 0, "must be NUL-terminated");
    assert_eq!(
        narrow(&dir[..dir.len() - 1]),
        r"D:\dev\IzarraVM\target\profiling"
    );
}

#[test]
fn forward_slashes_are_accepted_too() {
    let dir = exe_directory(&wide("D:/dev/IzarraVM/target/release/izarravm.exe"))
        .expect("a forward-slash path still has a directory");
    assert_eq!(
        narrow(&dir[..dir.len() - 1]),
        "D:/dev/IzarraVM/target/release"
    );
}

#[test]
fn resolve_symbol_reports_a_nonzero_extent_for_a_live_function() {
    use windows_sys::Win32::System::Diagnostics::Debug::{SymCleanup, SymInitializeW};
    use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // The beyond-extent guard treats a zero recorded size as "no extent, proves
    // nothing", so if this toolchain's PDBs ever stop carrying function extents
    // the guard is silently disarmed. Resolve a function from this very test
    // binary and pin that the Size plumbing populates. This is the only dbghelp
    // user in the test process (dbghelp is not thread-safe across concurrent
    // sessions), and it cleans up its session.
    //
    // The search path must be the exe's own directory, same as
    // `stop_and_report`: dbghelp's NULL default (CWD + _NT_SYMBOL_PATH) does
    // not contain the test binary's PDB, and this test fails from a NULL path.
    let process = unsafe { GetCurrentProcess() };
    let mut path = [0u16; 1024];
    let len = unsafe { GetModuleFileNameW(std::ptr::null_mut(), path.as_mut_ptr(), 1024) };
    let exe_dir = exe_directory(&path[..len as usize]).expect("test exe has a directory");
    assert_ne!(
        unsafe { SymInitializeW(process, exe_dir.as_ptr(), 1) },
        0,
        "SymInitializeW failed"
    );
    let rip = super::set_phase as usize as u64;
    let resolved = super::resolve_symbol(process, rip);
    unsafe { SymCleanup(process) };
    let (name, displacement, size) = resolved.expect("the test binary's own PDB must resolve");
    assert!(name.contains("set_phase"), "resolved {name:?}");
    assert!(
        size > 0,
        "the PDB carried no extent for a function symbol; the beyond-extent guard is disarmed"
    );
    assert!(
        displacement < u64::from(size),
        "a function entry must sit inside its own recorded extent"
    );
}

#[test]
fn a_zero_recorded_size_is_not_evidence_of_misattribution() {
    // The PDB carried no extent for the symbol; a displacement past nothing
    // proves nothing.
    assert!(!beyond_extent(0x40, 0));
}

#[test]
fn beyond_extent_is_displacement_at_or_past_the_recorded_size() {
    // A function of size S occupies [start, start+S): the last in-extent byte
    // is at displacement S-1 and displacement S is already the gap after it.
    assert!(!beyond_extent(0x3f, 0x40));
    assert!(beyond_extent(0x40, 0x40));
    assert!(beyond_extent(0x1000, 0x40));
}

#[test]
fn a_path_with_no_directory_falls_back_to_dbghelps_default() {
    // No separator at all, and a separator at index 0 (a root-relative path whose
    // directory would be the empty string). Both must yield None so the caller
    // passes NULL rather than an empty search path, which dbghelp would treat as
    // a real, and wrong, search path.
    assert!(exe_directory(&wide("izarravm.exe")).is_none());
    assert!(exe_directory(&wide(r"\izarravm.exe")).is_none());
    assert!(exe_directory(&[]).is_none());
}
