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
    // The beyond-extent guard treats a zero recorded size as "no extent, proves
    // nothing", so if this toolchain's PDBs ever stop carrying function extents
    // the guard is silently disarmed. Resolve a function from this very test
    // binary and pin that the Size plumbing populates.
    //
    // In a CHILD PROCESS with one test thread: `SymInitializeW(fInvadeProcess
    // = 1)` walks every loaded module under the loader lock while the parent
    // harness's other threads load and unload DLLs (the audio-backend tests
    // do), and dbghelp is not thread-safe — CI took a STATUS_ACCESS_VIOLATION
    // with this test in flight beside the gui::session battery. The profiler
    // itself never has this problem: it initializes dbghelp at report time,
    // after the emulation thread has stopped.
    if std::env::var_os("IZARRAVM_RIPROFILE_EXTENT_CHILD").is_some() {
        resolve_extent_in_this_process();
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    let output = std::process::Command::new(exe)
        .args([
            "riprofile::tests::resolve_symbol_reports_a_nonzero_extent_for_a_live_function",
            "--exact",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("IZARRAVM_RIPROFILE_EXTENT_CHILD", "1")
        .output()
        .expect("spawn the extent child");
    assert!(
        output.status.success(),
        "extent child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// The function the child resolves. `set_phase` was the original probe and
/// release codegen destroyed it two ways at once: nothing in the test binary
/// reads `ACTIVE_PHASE`, so its store optimized down to a bare `ret`, and the
/// linker's identical-code folding then merged that `ret` with every other
/// empty function — `SymFromAddrW` answered with a ThinLTO-promoted
/// tracing_core symbol at displacement 0. The anchor mixes constants nothing
/// else in the binary uses, so no build can make it byte-identical to another
/// function, and `#[inline(never)]` pins a standalone body to resolve.
#[inline(never)]
fn extent_anchor(x: u32) -> u32 {
    x.rotate_left(9).wrapping_mul(0x9E37_79B1) ^ 0x495A_4152
}

fn resolve_extent_in_this_process() {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SYMOPT_LOAD_LINES, SYMOPT_UNDNAME, SymCleanup, SymInitializeW, SymSetOptions,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // The search path must be the exe's own directory, same as
    // `stop_and_report`: dbghelp's NULL default (CWD + _NT_SYMBOL_PATH) does
    // not contain the test binary's PDB, and this test fails from a NULL path.
    let process = unsafe { GetCurrentProcess() };
    // The profiler's options, and the debug-info probe below needs the lines.
    unsafe { SymSetOptions(SYMOPT_LOAD_LINES | SYMOPT_UNDNAME) };
    let mut path = [0u16; 1024];
    let len = unsafe { GetModuleFileNameW(std::ptr::null_mut(), path.as_mut_ptr(), 1024) };
    let exe_dir = exe_directory(&path[..len as usize]).expect("test exe has a directory");
    assert_ne!(
        unsafe { SymInitializeW(process, exe_dir.as_ptr(), 1) },
        0,
        "SymInitializeW failed"
    );
    let rip = std::hint::black_box(extent_anchor as *const () as usize as u64);
    // A `--release` test binary carries no CodeView for this crate (`debug` is
    // unset in that profile): its PDB holds only linker publics, which have no
    // sizes and omit LTO-internalized functions entirely, so the extent
    // invariant has nothing to attach to. Detect that build per-ADDRESS —
    // module-level flags like `IMAGEHLP_MODULEW64.LineNumbers` read true even
    // then, because the CRT's /Z7 objects contribute debug info for their own
    // ranges.
    if super::resolve_line(process, rip).is_none() {
        unsafe { SymCleanup(process) };
        if cfg!(debug_assertions) {
            panic!(
                "a debug build must carry line info for its own code; without it \
                 the extent guard below would be silently skipped in CI"
            );
        }
        eprintln!(
            "riprofile extent test: publics-only PDB (optimized build without debug \
             info); function extents do not exist in this binary, skipping"
        );
        return;
    }
    let resolved = super::resolve_symbol(process, rip);
    unsafe { SymCleanup(process) };
    let (name, displacement, size) = resolved.expect("the test binary's own PDB must resolve");
    assert!(name.contains("extent_anchor"), "resolved {name:?}");
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
